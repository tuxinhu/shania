//! 流式语音识别：sherpa-onnx 的 `OnlineRecognizer`。
//!
//! 和 [`crate::transcribe`] 那条路（whisper-server，一次一整段）本质区别是**什么时候出结果**：
//!
//! | | whisper（`transcribe.rs`） | 这里 |
//! |---|---|---|
//! | 出字 | 说完一句才开始编码 | 每 160ms 一块，边说边出 |
//! | 首字延迟 | 0.3s（GPU）～1.4s（CPU `-ac 450`） | 约 200ms |
//! | 标点 | 靠前端补（`lib/punctuate.ts`） | 模型自带标点 token |
//! | 幻觉 | 静音/提示音会编字幕，要 VAD + 文本清洗两头堵 | 实测静音/白噪声/提示音全空 |
//!
//! 所以这里不做能量切句、不做 partial 重解码：一个 session 一个 stream，音频进来就喂，
//! endpoint 判定交给模型自己（`rule1/rule2/rule3`），断了就 reset 开始下一句。
//!
//! 模型是 X-ASR 160ms 的 int8 量化包（134MB），解压后四个文件。刻意用「按文件名前缀扫目录」
//! 而不是写死 `encoder-epoch-99-avg-1.int8.onnx`：换个模型版本就得改代码，扫一遍更省事。

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

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

/// 允许用镜像覆盖（国内直连 GitHub 经常只有几十 KB/s），和 whisper 那几个保持一致的做法
pub fn model_url() -> String {
    std::env::var("SHANIA_STREAM_STT_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| MODEL_URL.to_string())
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
    /// 模型目录。定稿纠错要拿它去读 SenseVoice 的路径，存在 session 里省得每次传。
    root: PathBuf,
    /// 每句定稿后是否用 SenseVoice 重听一遍。模型没装好时这里恒为 false。
    correct: bool,
    /// 这一句到目前为止的音频。定稿纠错必须整句重听，所以得留着；一句最长
    /// 15 秒（`MAX_UTTERANCE_MS`），240k 个 f32 不到 1MB，直接堆内存里没有负担。
    utter: Vec<f32>,
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
    ///
    /// `correct` 是「每句定稿后是否跑 SenseVoice 重听」。模型没装好时会被静默降级成
    /// false——纠错是可选增强，不能因为它让整个语音输入不可用。
    pub fn start(&self, root: &Path, correct: bool) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "识别器状态锁失效".to_string())?;

        // 每次开始都重新算一遍：用户可能刚在设置里拨了开关、或者刚下完模型
        let correct = correct && crate::finalize_stt::ready(root);

        if let Some(session) = guard.as_mut() {
            // 复用：把上一轮没说完的残句清掉，换个干净的 stream 状态
            session.recognizer.reset(&session.stream);
            session.last_partial.clear();
            session.utterance_ms = 0;
            session.utter.clear();
            session.correct = correct;
            session.root = root.to_path_buf();
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
        if correct {
            crate::log_info!("定稿纠错已启用：{}", crate::finalize_stt::dir(root).display());
        }

        *guard = Some(Session {
            recognizer,
            stream,
            root: root.to_path_buf(),
            correct,
            utter: Vec::new(),
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
        // 定稿纠错要整句重听，先把这块攒起来；断句时一次取走并清空
        session.utter.extend_from_slice(samples);

        let text = session
            .recognizer
            .get_result(&session.stream)
            .map(|r| clean(&r.text))
            .unwrap_or_default();

        let ended = session.recognizer.is_endpoint(&session.stream)
            || session.utterance_ms >= MAX_UTTERANCE_MS;

        if ended {
            let audio = std::mem::take(&mut session.utter);
            let mut finals = Vec::new();
            if !text.is_empty() {
                finals.push(correct_text(session, &audio, text));
            }
            session.recognizer.reset(&session.stream);
            session.utterance_ms = 0;
            session.last_partial.clear();
            return Ok(Feed {
                partial: String::new(),
                finals,
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

        // 取音频要在判断空文本之前：这一句哪怕流式没解出字，音频也已经攒着了
        let audio = std::mem::take(&mut session.utter);
        session.recognizer.reset(&session.stream);
        session.utterance_ms = 0;
        session.last_partial.clear();

        let text = if text.is_empty() {
            text
        } else {
            correct_text(session, &audio, text)
        };

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

/// 纠错结果和流式文本的最大差异率。
///
/// 这个功能的定位是「改几个同音字」，不是「重写一句话」。实测里 SenseVoice 偶尔会把
/// 整句听成另一句话——短句上下文少、英文术语、口音重的时候最容易出现。那种情况下
/// 它一定比已经跑通整句上下文的流式结果差，**保留流式文本**才是对的。
const MAX_EDIT_RATIO: f32 = 0.4;

/// 差异的绝对值上限：只要改动不超过这么多字，就算差异率偏高也接受。
///
/// 差异率是按句长归一化的，「你好啊」这种 3 字短句改一个字就是 33%，光看比例会误判。
const MAX_EDIT_CHARS: usize = 3;

/// 太短的音频不做纠错。
///
/// 0.8 秒以内基本是「好的」「嗯」「继续」这类应答：重听一次的收益接近于零，而
/// SenseVoice 在这种极短音频上最容易跑偏（上下文不足），换错一句话的代价却很实在。
const MIN_CORRECT_SAMPLES: usize = 12_800; // 0.8s @16k

/// 定稿文本：开了纠错就用 SenseVoice 整句重听的结果替换流式结果。
///
/// 任何一步失败（模型没装、推理报错、返回空串）都**退回**流式文本——纠错是锦上添花，
/// 不能因为它让一整句话消失。失败只记日志，不弹给用户。
///
/// 另外三条保守规则，见上面几个常量：太短的音频不纠、差异过大不接受、
/// 接受时把「流式原文 → 纠错后」一起写进日志，方便对账。
fn correct_text(session: &Session, audio: &[f32], fallback: String) -> String {
    if !session.correct || audio.len() < MIN_CORRECT_SAMPLES {
        return fallback;
    }
    let began = std::time::Instant::now();
    match crate::finalize_stt::finalize(&session.root, audio) {
        Ok(text) if text.is_empty() => fallback,
        Ok(text) => {
            let cost = began.elapsed().as_millis();
            let (edits, ratio) = diff_stats(&fallback, &text);
            if edits > MAX_EDIT_CHARS && ratio > MAX_EDIT_RATIO {
                // 听岔了：两家差得太多，说明 SenseVoice 走了另一条路。留流式的。
                crate::log_info!(
                    "定稿纠错差异过大（{} 字/{:.0}%），保留流式文本 {}ms｜流式「{}」→ 纠错「{}」",
                    edits,
                    ratio * 100.0,
                    cost,
                    fallback,
                    text
                );
                return fallback;
            }
            if text != fallback {
                crate::log_info!(
                    "定稿纠错 {}ms（改 {} 字/{:.0}%）｜流式「{}」→「{}」",
                    cost,
                    edits,
                    ratio * 100.0,
                    fallback,
                    text
                );
            }
            text
        }
        Err(err) => {
            crate::log_warn!("定稿纠错失败，沿用流式文本：{err}");
            fallback
        }
    }
}

/// 两段文本的字符级差异：返回（改动字数, 差异率 0..1）。
///
/// 只比「内容字」，标点和空白先剔掉——否则流式那边多一个「，」就被算成一次改动，
/// 比例会被无意义地稀释。差异率以两者较长的一条归一化。
fn diff_stats(a: &str, b: &str) -> (usize, f32) {
    let left = content_chars(a);
    let right = content_chars(b);
    let longest = left.len().max(right.len());
    if longest == 0 {
        return (0, 0.0);
    }
    let dist = levenshtein(&left, &right);
    (dist, dist as f32 / longest as f32)
}

/// 只留内容字：去掉空白和中英标点
fn content_chars(text: &str) -> Vec<char> {
    text.chars()
        .filter(|c| !c.is_whitespace() && !is_punct(*c))
        .collect()
}

fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation()
        || "，。、！？；：（）【】《》〈〉「」『』“”‘’…—～·".contains(c)
}

/// 字符级编辑距离，两行滚动数组
fn levenshtein(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// 模型输出过一遍后处理。
///
/// 只做三件事，**不**跑 [`crate::transcribe::clean_transcript`] 那一整套：那套是按
/// 整段 whisper 输出设计的（按句丢幻听、丢掉超过一半就整段清空），套在流式的
/// 半句话上会误伤——「谢谢观看」这种词出现在半句里时，一删就把整句清空了。
/// 流式模型本来也不会吐 `(字幕:xxx)`，没有剥括号的必要。
fn clean(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // 这个模型偶尔吐繁体（「謝謝」），和 whisper 那边一样统一成简体
    let simplified = crate::simplify::to_simplified(trimmed);
    crate::transcribe::tidy(&simplified)
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
    let bytes = crate::transcribe::decode_audio(base64)?;
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

    /// 只差一个字：典型同音字纠正，必须接受
    #[test]
    fn accepts_small_homophone_fix() {
        let (edits, ratio) = diff_stats("帮我把灯露液的按钮改一下", "帮我把登录页的按钮改一下");
        assert_eq!(edits, 3); // 灯露液 → 登录页，三个字都换了
        assert!(ratio < MAX_EDIT_RATIO, "25% 的改动属于纠错范围，实际 {ratio}");
    }

    /// 整句听成另一句话：必须拒绝（这正是「越纠越错」的那种情况）
    #[test]
    fn rejects_full_rewrite() {
        let (edits, ratio) = diff_stats(
            "帮我把登录页的按钮改成次要样式",
            "方我把东陆的按牛改掉四要格式",
        );
        assert!(edits > MAX_EDIT_CHARS);
        assert!(ratio > MAX_EDIT_RATIO, "整句改写应该被判为差异过大，实际 {ratio}");
    }

    /// 标点差异不算改动——流式少一个逗号不该触发「改写」判断
    #[test]
    fn ignores_punctuation_difference() {
        let (edits, ratio) = diff_stats("你好，你是谁？", "你好你是谁！");
        assert_eq!(edits, 0);
        assert_eq!(ratio, 0.0);
    }

    /// 短句里改两个字：比例很高但绝对改动小，仍然接受
    #[test]
    fn short_sentence_single_fix_is_accepted() {
        let (edits, ratio) = diff_stats("改一下", "该一夏");
        assert_eq!(edits, 2);
        assert!(ratio > MAX_EDIT_RATIO, "3 字句改 2 字比例是 67%，应超过阈值");
        assert!(edits <= MAX_EDIT_CHARS, "但绝对改动只有 2 字，判据要放行");
    }

    /// 空串不崩，也不被当成「完全一致」
    #[test]
    fn empty_strings_do_not_panic() {
        assert_eq!(diff_stats("", ""), (0, 0.0));
        assert_eq!(diff_stats("你好", "").1, 1.0);
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
        // 纠错开关从环境变量拿，方便同一个用例分别验「只流式」和「流式 + 定稿纠错」
        let correct = std::env::var("SHANIA_STREAM_TEST_CORRECT")
            .map(|v| v == "1")
            .unwrap_or(false);
        engine.start(&root, correct).expect("加载模型失败");
        println!("模型加载：{}ms（定稿纠错：{}）", began.elapsed().as_millis(), correct);

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
