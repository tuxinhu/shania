//! 流式语音识别：X-ASR 160ms 模型，走 sherpa-onnx 的 `OnlineRecognizer`，**进程内**推理。
//!
//! 它取代了早期那套 whisper-server 方案（独立子进程 + HTTP + 前端能量切句 + 900ms 重解码预览）。
//! 那种做法的问题很实在：说完一整句才出字、静音和提示音会编出字幕、还要占一个端口和一套进程管理。
//! 这里换成模型自己断句，音频块进来就喂，喂着就出字。
//!
//! 所以这里不做能量切句、也不做 partial 重解码：一个 session 一个 stream，音频进来就喂，
//! endpoint 判定交给模型自己（`rule1/rule2/rule3`），断了就 reset 开始下一句。
//!
//! 模型是 X-ASR 160ms 的 int8 量化包（134MB），解压后四个文件。刻意用「按文件名前缀扫目录」
//! 而不是写死 `encoder-epoch-99-avg-1.int8.onnx`：换个模型版本就得改代码，扫一遍更省事。

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sherpa_onnx::{
    OnlineModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream,
    OnlineTransducerModelConfig,
};

/// 模型包大小。下载完对不上就当失败——中断的下载会留下一个能读但跑不起来的半个文件。
pub const MODEL_BYTES: u64 = 133_898_007;

/// 解压目录名带上模型版本：以后换模型不会和旧目录混在一起
pub const DIR_NAME: &str = "stream-x-asr-160ms-zh-en-punct-int8";

/// 固定 tag 的直链，不跟 latest 走：模型换了要重新实测延迟和准确率，不该悄悄变
const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05.tar.bz2";

/// 允许用镜像覆盖（国内直连 GitHub 经常只有几十 KB/s）
pub fn model_url() -> String {
    std::env::var("SHANIA_STREAM_STT_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| MODEL_URL.to_string())
}

/// 流式下载模型包，每收到一块回调一次进度。
///
/// 回调按值传泛型而不是 `&mut dyn FnMut`：后者不满足 Send，
/// Tauri 的 async command 要求整条 future 是 Send 的。
///
/// 先写 `.part`、完整了再改名：中断只会留下半个临时文件，不会被误认为装好。
pub async fn download<F>(url: &str, dest: &Path, mut on_progress: F) -> Result<u64, String>
where
    F: FnMut(u64, Option<u64>) + Send,
{
    use futures_util::StreamExt;

    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
    }

    let client = reqwest::Client::builder()
        .user_agent("shania-agent")
        // 134MB 在国内慢网下可能要十几分钟，超时给足
        .timeout(Duration::from_secs(30 * 60))
        .build()
        .map_err(|e| format!("创建下载客户端失败：{e}"))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("下载失败：{e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("下载失败：服务返回 {status}"));
    }
    let total = resp.content_length();

    let tmp = dest.with_extension("part");
    let mut file = File::create(&tmp).map_err(|e| format!("创建临时文件失败：{e}"))?;
    let mut got: u64 = 0;
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("下载中断：{e}"))?;
        file.write_all(&chunk)
            .map_err(|e| format!("写入失败：{e}"))?;
        got += chunk.len() as u64;
        on_progress(got, total);
    }

    file.flush().map_err(|e| format!("刷新文件失败：{e}"))?;
    drop(file);

    fs::rename(&tmp, dest).map_err(|e| format!("重命名失败：{e}"))?;
    Ok(got)
}

pub fn dir(root: &Path) -> PathBuf {
    root.join("models").join(DIR_NAME)
}

/// 解压后真正要喂给识别器的四个文件
pub struct Files {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
}

/// 递归找文件。包里固定只有一层目录（`<name>/xxx.onnx`），限深 3 足够。
pub(crate) fn scan(dir: &Path, out: &mut Vec<PathBuf>, depth: u8) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan(&path, out, depth + 1);
        } else {
            out.push(path);
        }
    }
}

/// 按名字里的关键词找文件。不写死 epoch 号——换模型版本时这里不用改。
pub(crate) fn pick(files: &[PathBuf], keyword: &str, suffix: &str) -> Option<PathBuf> {
    files
        .iter()
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.contains(keyword) && n.ends_with(suffix))
                .unwrap_or(false)
        })
        .cloned()
}

pub fn files(root: &Path) -> Option<Files> {
    let base = dir(root);
    let mut all = Vec::new();
    scan(&base, &mut all, 0);
    if all.is_empty() {
        return None;
    }

    Some(Files {
        encoder: pick(&all, "encoder", ".onnx")?,
        decoder: pick(&all, "decoder", ".onnx")?,
        joiner: pick(&all, "joiner", ".onnx")?,
        tokens: pick(&all, "tokens", ".txt")?,
    })
}

pub fn ready(root: &Path) -> bool {
    files(root).is_some()
}

/// 已占用的磁盘空间，给设置面板显示「装了多少」
pub fn size(root: &Path) -> u64 {
    let mut all = Vec::new();
    scan(&dir(root), &mut all, 0);
    all.iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// 解压 `.tar.bz2` 到 `dest`，保留包内的目录层级。
///
/// 用 crate 而不是调系统的 `tar.exe`：Windows 家庭版/精简系统上不一定有，
/// 而这条路径是「用户点一下就能用」的关键环节，不能赌外部工具。
pub fn extract_tar_bz2(archive: &Path, dest: &Path) -> Result<usize, String> {
    let file = File::open(archive).map_err(|e| format!("打开模型包失败：{e}"))?;
    let decoder = bzip2::read::BzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);

    fs::create_dir_all(dest).map_err(|e| format!("创建目录失败：{e}"))?;

    let mut count = 0usize;
    let entries = tar
        .entries()
        .map_err(|e| format!("解析模型包失败：{e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("读取模型包条目失败：{e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("条目路径无效：{e}"))?
            .to_path_buf();
        // 拒绝绝对路径和 `..`：压缩包是网络下载的，不能让它写到目标目录外
        if path.is_absolute() || path.components().any(|c| c.as_os_str() == "..") {
            continue;
        }
        let target = dest.join(&path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
        }
        if entry.header().entry_type().is_dir() {
            continue;
        }
        entry
            .unpack(&target)
            .map_err(|e| format!("解压 {} 失败：{e}", target.display()))?;
        count += 1;
    }
    Ok(count)
}

/* ------------------------------------------------------------------ */
/* 识别会话                                                             */
/* ------------------------------------------------------------------ */

/// 一次 feed 的产出。
#[derive(Debug, Default, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Feed {
    /// 当前这一句的**完整**文本（不是增量）。前端直接覆盖预览，别往后追加。
    pub partial: String,
    /// 这一轮里有几句因为 endpoint 判定为「说完了」，按顺序排好。
    pub finals: Vec<String>,
}

struct Session {
    recognizer: OnlineRecognizer,
    stream: OnlineStream,
    /// 这句已经说多久了，超过 `MAX_UTTERANCE_MS` 强制断——model 的 rule3 也是
    /// 干这个的，但它是按解码帧算的，我们这边再兜一层，免得一口气念稿子时
    /// partial 无限长、前端每次都要重绘一大段。
    utterance_ms: u32,
    last_partial: String,
}

/// 一句最长多久。超过就硬断，把当前 partial 当句末交出去。
const MAX_UTTERANCE_MS: u32 = 15_000;

/// 识别器常驻容器。
///
/// 模型加载约 1-2 秒（134MB 权重 + onnxruntime 初始化），每次录音都重建太浪费，
/// 所以进程内只建一次。`Mutex<Option<_>>` 而不是 `OnceLock`：卸载/换模型时能重建。
pub struct Engine {
    inner: Mutex<Option<Session>>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    pub fn loaded(&self) -> bool {
        self.inner.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    /// 创建识别器 + 一个新 stream。已经加载过就直接重置，不再读盘。
    pub fn start(&self, root: &Path) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "识别器状态锁失效".to_string())?;

        if let Some(session) = guard.as_mut() {
            // 复用：把上一轮没说完的残句清掉，换个干净的 stream 状态
            session.recognizer.reset(&session.stream);
            session.last_partial.clear();
            session.utterance_ms = 0;
            return Ok(());
        }

        let files = files(root).ok_or_else(|| "流式识别模型还没装好".to_string())?;
        let began = Instant::now();

        let config = OnlineRecognizerConfig {
            model_config: OnlineModelConfig {
                transducer: OnlineTransducerModelConfig {
                    encoder: Some(files.encoder.to_string_lossy().to_string()),
                    decoder: Some(files.decoder.to_string_lossy().to_string()),
                    joiner: Some(files.joiner.to_string_lossy().to_string()),
                },
                tokens: Some(files.tokens.to_string_lossy().to_string()),
                num_threads: thread_count(),
                provider: Some("cpu".to_string()),
                ..Default::default()
            },
            // greedy 比 modified_beam_search 快一倍多，准确率差别在中文听写上看不出来；
            // 实时场景里延迟比最后一两个百分点重要。
            decoding_method: Some("greedy_search".to_string()),
            enable_endpoint: true,
            // rule1：**还没解出任何字**时，连续静音多久算一句结束。这里其实很少命中
            // （没字的时候多半是根本没开口），给个偏大的值，避免用户刚张嘴就被切。
            rule1_min_trailing_silence: 2.4,
            // rule2：已经解出字之后，尾部静音多久算说完。中文说话的换气停顿一般
            // 0.3-0.5 秒，0.9 秒能吃掉换气、又不会让一句说完等太久。
            rule2_min_trailing_silence: 0.9,
            // rule3：单句硬上限（秒）
            rule3_min_utterance_length: 20.0,
            ..Default::default()
        };

        let recognizer =
            OnlineRecognizer::create(&config).ok_or_else(|| {
                "创建流式识别器失败：模型文件和版本不匹配（日志里通常有 encoder 元信息）"
                    .to_string()
            })?;
        let stream = recognizer.create_stream();

        crate::log_info!(
            "流式识别器就绪：{}ms，{} 线程，模型 {}",
            began.elapsed().as_millis(),
            thread_count(),
            dir(root).display()
        );

        *guard = Some(Session {
            recognizer,
            stream,
            utterance_ms: 0,
            last_partial: String::new(),
        });
        Ok(())
    }

    /// 喂一段 16k 单声道 float32。
    ///
    /// 返回值里的 `partial` 是**整句**到目前为止的文本；`finals` 是这一轮里
    /// 被 endpoint 判定结束的句子。endpoint 之后会立刻 reset，所以下一句从零开始。
    pub fn feed(&self, samples: &[f32]) -> Result<Feed, String> {
        if samples.is_empty() {
            return Ok(Feed::default());
        }
        let mut guard = self.inner.lock().map_err(|_| "识别器状态锁失效".to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "识别器还没启动".to_string())?;

        session.stream.accept_waveform(16_000, samples);
        // 音频是 16k，每处理一块至少 10ms 的分帧；这里循环到模型说「不够再喂」
        // 为止——一次性喂 150ms 时通常能连解好几步。
        while session.recognizer.is_ready(&session.stream) {
            session.recognizer.decode(&session.stream);
        }
        session.utterance_ms += (samples.len() as u32 * 1000) / 16_000;

        let text = session
            .recognizer
            .get_result(&session.stream)
            .map(|r| clean(&r.text))
            .unwrap_or_default();

        let ended = session.recognizer.is_endpoint(&session.stream)
            || session.utterance_ms >= MAX_UTTERANCE_MS;

        if ended {
            session.recognizer.reset(&session.stream);
            session.utterance_ms = 0;
            session.last_partial.clear();
            return Ok(Feed {
                partial: String::new(),
                finals: if text.is_empty() { Vec::new() } else { vec![text] },
            });
        }

        // 内容没变就别重复发，省一次 IPC 和一次前端重绘
        if text == session.last_partial {
            return Ok(Feed::default());
        }
        session.last_partial = text.clone();
        Ok(Feed {
            partial: text,
            finals: Vec::new(),
        })
    }

    /// 录音停止：把尾部还没定稿的部分冲出来。
    ///
    /// 两件事缺一不可：
    ///   1. **补 0.66 秒静音**。这个模型按 160ms 分块编码，最后不足一块的音频
    ///      解不出字——实测「改成次要样式」只会吐出「改成次要样」，最后两个字
    ///      要靠补零才带得出来（sherpa 官方示例也是这个固定值）。
    ///   2. `input_finished` 之后再跑几轮 decode，让卷积的右侧上下文补满。
    pub fn finish(&self) -> Result<Feed, String> {
        let mut guard = self.inner.lock().map_err(|_| "识别器状态锁失效".to_string())?;
        let Some(session) = guard.as_mut() else {
            return Ok(Feed::default());
        };

        let pad = vec![0.0f32; 10_560];
        session.stream.accept_waveform(16_000, &pad);
        session.stream.input_finished();
        while session.recognizer.is_ready(&session.stream) {
            session.recognizer.decode(&session.stream);
        }

        let text = session
            .recognizer
            .get_result(&session.stream)
            .map(|r| clean(&r.text))
            .unwrap_or_default();

        session.recognizer.reset(&session.stream);
        session.utterance_ms = 0;
        session.last_partial.clear();

        Ok(Feed {
            partial: String::new(),
            finals: if text.is_empty() { Vec::new() } else { vec![text] },
        })
    }

    /// 丢掉整个识别器（换模型、卸载时用）
    pub fn shutdown(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = None;
        }
    }
}

/// 模型输出过一遍后处理。
///
/// 只做三件事（繁转简 + 空白归一 + 标点规范化），**不**做按句丢幻听那套：
/// 流式模型本来就很少在静音上编内容（实测静音/白噪声/提示音全空），而且它吐的是
/// 半句话——按句删的规则套上去，一句里出现「谢谢观看」就会连正文一起清空。
fn clean(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // 这个模型偶尔吐繁体（「謝謝」），统一成简体
    let simplified = crate::simplify::to_simplified(trimmed);
    tidy(&simplified)
}

/// 空白归一 + 中文标点规范化。
fn tidy(text: &str) -> String {
    normalize_punctuation(&squeeze_spaces(text)).trim().to_string()
}

/// 中文文本里的标点规范化。
///
/// 模型主要三个毛病：半角逗号（「改一下,按钮太大了」）、叠在一起的标点（`，。`）、
/// 标点后面多一个空格。英文句子完全不动——半角标点在英文里本来就是正确写法。
fn normalize_punctuation(text: &str) -> String {
    if !text.chars().any(is_cjk) {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(text.len());
    for (i, &raw) in chars.iter().enumerate() {
        // 只有紧跟在中文后面的半角标点才转全角。`So, my fellow` 里的逗号
        // 是英文的正确写法，跟着一起改就成了 `So， my fellow`。
        let c = if matches!(raw, ',' | '?' | '!' | ';')
            && out
                .iter()
                .rev()
                .find(|p| **p != ' ')
                .is_some_and(|p| is_cjk(*p))
        {
            match raw {
                ',' => '，',
                '?' => '？',
                '!' => '！',
                _ => '；',
            }
        } else {
            raw
        };

        if c == ' ' {
            // 中文标点前后不该有空格：「改一下， 按钮」读起来是断的
            if out.last().is_some_and(|p| is_cjk_punct(*p)) {
                continue;
            }
            // 中文之间不留空格（中文不靠空格分词），中英之间要留（「我要用 VSCode」）
            let prev_zh = out.last().is_some_and(|p| is_cjk(*p));
            let next_zh = chars[i + 1..]
                .iter()
                .find(|x| **x != ' ')
                .is_some_and(|x| is_cjk(*x));
            if prev_zh && next_zh {
                continue;
            }
            out.push(' ');
            continue;
        }

        if is_cjk_punct(c) {
            // 标点前刚压进来的空格要去掉
            while out.last() == Some(&' ') {
                out.pop();
            }
            // 相邻标点只留语气最强的一个：`，。` → `。`
            if let Some(prev) = out.last().copied() {
                if let Some(rank) = punct_rank(prev) {
                    if punct_rank(c).unwrap_or(0) <= rank {
                        continue;
                    }
                    out.pop();
                }
            }
        }

        out.push(c);
    }

    // 中文句子以半角句号收尾时换成全角。只在**文末、且前面是中文**时换：
    // 句中的 `.` 可能是小数点（3.5）或省略号（`...`），英文句子的 `.` 更该保持半角。
    while out.last() == Some(&' ') {
        out.pop();
    }
    let n = out.len();
    if n > 1 && out[n - 1] == '.' && out[n - 2] != '.' && is_cjk(out[n - 2]) {
        out[n - 1] = '。';
    }

    out.into_iter().collect()
}

/// 语气强度，用来在叠在一起的标点里挑一个
fn punct_rank(c: char) -> Option<u8> {
    match c {
        '。' => Some(3),
        '！' | '？' => Some(2),
        '，' | '；' | '：' => Some(1),
        '、' => Some(0),
        _ => None,
    }
}

fn is_cjk_punct(c: char) -> bool {
    punct_rank(c).is_some()
        || matches!(
            c,
            '（' | '）' | '【' | '】' | '「' | '」' | '『' | '』' | '…' | '—'
        )
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

/// 连续空白压成一个空格，顺便丢掉控制字符（模型偶尔吐 `\u{0}`）
fn squeeze_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for c in text.chars() {
        if c.is_control() && c != '\n' {
            continue;
        }
        if c.is_whitespace() {
            if last_space {
                continue;
            }
            last_space = true;
            out.push(' ');
            continue;
        }
        last_space = false;
        out.push(c);
    }
    out.trim().to_string()
}

/// 线程数：流式这个模型很小，实测 4 线程最优，8 线程反而更慢（0.205 → 0.354），
/// 大概是内存带宽先饱和了。上限卡在 4，别再往上堆。
pub(crate) fn thread_count() -> i32 {
    let logical = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (logical / 2).clamp(2, 4) as i32
}

/// base64 → float32 采样。
///
/// 前端把 `Float32Array` 的字节按小端编成 base64 传过来（比传 JSON 数字数组小 6 倍，
/// 一次 150ms 音频约 12KB 文本）。
pub fn decode_pcm(base64: &str) -> Result<Vec<f32>, String> {
    let bytes = decode_audio(base64)?;
    if bytes.len() % 4 != 0 {
        return Err(format!(
            "PCM 数据长度 {} 不是 4 的倍数，可能传进来的是 16bit 音频",
            bytes.len()
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// base64 → 字节，顺手兼容前端直接传 data URL 的情况
fn decode_audio(data: &str) -> Result<Vec<u8>, String> {
    let payload = data
        .split_once("base64,")
        .map(|(_, rest)| rest)
        .unwrap_or(data);
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| format!("音频数据解码失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shania-stream-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn finds_model_files_by_keyword() {
        let root = tmp("files");
        let sub = dir(&root).join("pkg");
        fs::create_dir_all(&sub).unwrap();
        for name in [
            "encoder-epoch-99-avg-1.int8.onnx",
            "decoder-epoch-99-avg-1.onnx",
            "joiner-epoch-99-avg-1.int8.onnx",
            "tokens.txt",
        ] {
            fs::write(sub.join(name), b"x").unwrap();
        }

        let files = files(&root).expect("四个文件都在，应该能找到");
        assert!(files.encoder.ends_with("encoder-epoch-99-avg-1.int8.onnx"));
        assert!(files.decoder.ends_with("decoder-epoch-99-avg-1.onnx"));
        assert!(files.joiner.ends_with("joiner-epoch-99-avg-1.int8.onnx"));
        assert!(files.tokens.ends_with("tokens.txt"));
        assert!(ready(&root));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_piece_is_not_ready() {
        let root = tmp("partial");
        let sub = dir(&root);
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("encoder.onnx"), b"x").unwrap();
        fs::write(sub.join("tokens.txt"), b"x").unwrap();

        // 少了 decoder 和 joiner，不算装好
        assert!(!ready(&root));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_root_is_not_ready() {
        let root = tmp("empty");
        assert!(!ready(&root));
        assert_eq!(size(&root), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn decodes_little_endian_f32() {
        let samples = vec![0.5f32, -0.25f32];
        let bytes: Vec<u8> = samples.iter().flat_map(|f| f.to_le_bytes()).collect();
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let got = decode_pcm(&b64).unwrap();
        assert_eq!(got, samples);
    }

    #[test]
    fn rejects_16bit_payload() {
        use base64::Engine;
        // 3 字节：不是 4 的倍数，说明前端发的是 16bit 而不是 float32
        let b64 = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3]);
        assert!(decode_pcm(&b64).is_err());
    }

    /// 读 16k/16bit/单声道 wav（`lib/pcm-recorder.ts` 的 `pcmToWav` 就是这个格式）
    fn read_wav(path: &Path) -> Vec<f32> {
        let bytes = fs::read(path).expect("读不到 wav");
        let mut at = 12usize;
        while at + 8 <= bytes.len() {
            let size = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
                as usize;
            if &bytes[at..at + 4] == b"data" {
                let start = at + 8;
                let end = (start + size).min(bytes.len());
                return bytes[start..end]
                    .chunks_exact(2)
                    .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                    .collect();
            }
            at += 8 + size + (size & 1);
        }
        panic!("wav 里没有 data 块");
    }

    fn home_root() -> PathBuf {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .expect("拿不到用户目录");
        PathBuf::from(home).join(".shania")
    }

    /// 真跑一遍推理，打印逐块结果和延迟。
    ///
    /// 需要模型已装好，并用 `SHANIA_STREAM_TEST_WAV` 指定 16k/16bit/单声道 wav：
    /// ```text
    /// SHANIA_STREAM_TEST_WAV=C:/x/a1.wav cargo test -p shania-core \
    ///   --features stream-stt -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "需要模型和语音样本"]
    fn recognizes_real_audio() {
        let wav = std::env::var("SHANIA_STREAM_TEST_WAV").expect("没给 SHANIA_STREAM_TEST_WAV");
        let root = home_root();
        let samples = read_wav(Path::new(&wav));
        let audio_ms = samples.len() as f32 / 16.0;
        println!("音频：{} 采样，{audio_ms:.0}ms", samples.len());

        let engine = Engine::new();
        let began = Instant::now();
        engine.start(&root).expect("加载模型失败");
        println!("模型加载：{}ms", began.elapsed().as_millis());

        let mut finals: Vec<String> = Vec::new();
        let mut last_partial = String::new();
        let mut decode_time = std::time::Duration::ZERO;

        // 100ms 一块，和前端 `FRAME_CHUNK` 一致
        for (i, chunk) in samples.chunks(1600).enumerate() {
            let t = Instant::now();
            let feed = engine.feed(chunk).expect("推理失败");
            decode_time += t.elapsed();
            if !feed.partial.is_empty() && feed.partial != last_partial {
                println!("  [{:>5}ms] {}", i * 100, feed.partial);
                last_partial = feed.partial;
            }
            for f in feed.finals {
                println!("  [{:>5}ms] = 定稿：{f}", i * 100);
                finals.push(f);
                last_partial.clear();
            }
        }

        let tail = engine.finish().expect("收尾失败");
        finals.extend(tail.finals);

        let wall = began.elapsed().as_millis();
        println!("\n解码总耗时：{}ms", decode_time.as_millis());
        println!("RTF：{:.3}", decode_time.as_secs_f32() / (audio_ms / 1000.0));
        println!("端到端（含加载）：{wall}ms");
        println!("定稿 {} 句：{:?}", finals.len(), finals);

        assert!(!finals.is_empty(), "一个字都没识别出来");
    }
}
