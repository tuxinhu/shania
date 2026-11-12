//! 本地语音合成：进程内 ONNX 推理（sherpa-onnx `OfflineTts`），不需要外部进程。
//!
//! # 为什么是 MeloTTS
//!
//! 用的是 `vits-melo-tts-zh_en`：170MB、44.1kHz、单音色、实测 RTF 0.32-0.43。
//!
//! 前后试过的另外两个（代码已删，模型文件还在 `~/.shania/models/` 下）：
//!
//! | 模型 | 实测 | 为什么不用 |
//! |---|---|---|
//! | matcha-icefall-zh-baker | RTF 0.04，最快 | lexicon 里**一个英文词都没有**，送英文进去返回 0 采样——不是念错，是完全不出声，中英混排的句子中间会空一截 |
//! | kokoro-multi-lang-v1_1 | RTF 0.50，103 音色 | 中文按词查注音表，表外的「好的」「我看了下」念成英文音；首声 1.8 秒 |
//!
//! melo 是唯一「中文准、英文也念得出来」的：lexicon 11.2 万条含中英音素，
//! 表外的缩写（API / JSON / GPU）按字母念。代价是比 matcha 慢（首声 1.3 秒
//! vs 0.9 秒），通话语境里「内容完整」比「快 0.4 秒」重要。
//!
//! # 为什么整句合成而不是流式回调
//!
//! matcha 合成一句 15 字只要 60-100ms，分块回调省下的等待还不如「一块 PCM
//! 直接排进播放队列」可靠——回调的块边界语义不明确，接错了就是重复播放或漏音。
//! 句间无缝靠前端的播放队列保证。
//!
//! # 换后端
//!
//! 合成入口是 [`Engine::say`]：接云 TTS 时在这里分流即可，`Loaded` 那层
//! 只管本地 ONNX 的细节。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsModelConfig};

use crate::stream_stt::{pick, scan};

pub const MELO_DIR: &str = "vits-melo-tts-zh_en";

/// 压缩包大小（163MB，解压出 170MB 的 fp32 声学模型）
///
/// 包里还有一个 `model.int8.onnx`，只有 **133 字节**——那是 git-lfs 的指针
/// 文件，不是模型。挑文件时必须点名 `model.onnx`，否则加载的是它，报「模型
/// 与 sherpa-onnx 版本不匹配」，几乎查不出来。
pub const MELO_BYTES: u64 = 167_006_755;

const MELO_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-melo-tts-zh_en.tar.bz2";

/// 允许用镜像覆盖（国内直连 GitHub 经常只有几十 KB/s）
pub fn melo_url() -> String {
    std::env::var("SHANIA_TTS_MELO_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| MELO_URL.to_string())
}

/// 合成时开多少线程。
///
/// 声学模型是自回归结构，单步慢，核给少了就是在干等——实测 4 线程 RTF 0.50、
/// 8 线程 0.40，再多收益就平了。开满会跟 X-ASR 抢 CPU，而识别卡顿比合成慢
/// 更影响通话。
fn threads() -> i32 {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4);
    (cores / 2).clamp(4, 10)
}

/// 输出归一化的目标峰值。
///
/// melo 的原始峰值只有 0.07-0.25，matcha 是 0.60——同样把系统音量开满，听感
/// 差一个数量级，用户第一反应是「这个模型坏了」。按句归一化到 0.85，而不是
/// 乘固定倍数：乘 4 会让本来就有 0.25 的句子撞到 1.0 削波（实测长句和带
/// markdown 的那句都满了），削波听起来是「滋」的一声。
const TARGET_PEAK: f32 = 0.85;

/// 超过这个耗时就在日志里记一条（正常 20 字约 1.3 秒）
const SLOW_MS: u128 = 2500;

pub fn dir(root: &Path) -> PathBuf {
    root.join("models").join(MELO_DIR)
}

/// 解压后真正要喂给合成器的文件
#[derive(Debug, Clone)]
pub struct Files {
    pub model: PathBuf,
    pub tokens: PathBuf,
    pub lexicon: PathBuf,
    pub rule_fsts: Option<String>,
    /// jieba 词典，缺了中文分词会退化
    pub dict_dir: Option<PathBuf>,
}

/// 模型完整可用时返回文件清单
pub fn files(root: &Path) -> Option<Files> {
    let base = dir(root);
    let mut all = Vec::new();
    scan(&base, &mut all, 0);
    if all.is_empty() {
        return None;
    }

    // 点名 model.onnx：包里同时有一个 133 字节的 model.int8.onnx（git-lfs 指针），
    // 按关键词模糊匹配可能挑到它
    let model = all
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some("model.onnx"))
        .cloned()
        .or_else(|| pick(&all, "model", ".onnx"))?;
    let tokens = pick(&all, "tokens", ".txt")?;
    let lexicon = pick(&all, "lexicon", ".txt")?;

    // 四个 FST 各有分工：数字、日期、电话号码，以及 new_heteronym（多音字，
    // 「银行/行走」这类）。少一个不会崩，但那类词会念错
    let fsts: Vec<String> = ["phone", "date", "number", "new_heteronym"]
        .into_iter()
        .filter_map(|name| pick(&all, name, ".fst"))
        .map(|p| p.display().to_string())
        .collect();

    let dict = base.join("dict");

    Some(Files {
        model,
        tokens,
        lexicon,
        rule_fsts: if fsts.is_empty() {
            None
        } else {
            Some(fsts.join(","))
        },
        dict_dir: if dict.is_dir() { Some(dict) } else { None },
    })
}

pub fn ready(root: &Path) -> bool {
    files(root).is_some()
}

/// 模型目录占用
pub fn size(root: &Path) -> u64 {
    let mut all = Vec::new();
    scan(&dir(root), &mut all, 0);
    all.iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// 一段合成好的音频
pub struct Audio {
    pub samples: Vec<f32>,
    pub rate: i32,
}

impl Audio {
    /// 换算成毫秒，前端用来估算播放时长
    pub fn duration_ms(&self) -> u32 {
        if self.rate <= 0 {
            return 0;
        }
        (self.samples.len() as u64 * 1000 / self.rate as u64) as u32
    }

    /// 编成 base64 的小端 i16 PCM。
    ///
    /// 用 i16 而不是 f32：体积直接减半，而合成器内部本来产出就是 16bit 精度，
    /// 转 f32 只是让 API 统一，没必要把这层膨胀传给前端。
    pub fn to_pcm_base64(&self) -> String {
        use base64::Engine;

        let mut bytes = Vec::with_capacity(self.samples.len() * 2);
        for sample in &self.samples {
            let v = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }
}

struct Loaded {
    tts: OfflineTts,
    rate: i32,
    speakers: i32,
}

impl Loaded {
    /// 合成一次。失败返回 None（调用方决定重试还是跳过）。
    fn generate(&self, text: &str, config: &GenerationConfig) -> Option<sherpa_onnx::GeneratedAudio> {
        self.tts
            .generate_with_config(text, config, None::<fn(&[f32], f32) -> bool>)
    }

    fn say(&self, text: &str, sid: i32, speed: f32) -> Result<Audio, String> {
        let clean = sanitize(text);
        // 纯标点（「……」「——」「。。。」）没有可念的字。这不是故障：
        // 分句器和模型都会偶尔吐这种东西，报错会让通话中断在半句话上。
        if !speakable(&clean) {
            return Ok(Audio {
                samples: Vec::new(),
                rate: self.rate,
            });
        }

        let sid = if self.speakers > 0 {
            sid.clamp(0, self.speakers - 1)
        } else {
            0
        };
        let config = GenerationConfig {
            sid,
            // 语速超出这个范围就会变调、发闷
            speed: speed.clamp(0.5, 2.0),
            // 句尾静音比例，0.2 是官方默认值
            silence_scale: 0.2,
            ..Default::default()
        };

        let began = Instant::now();
        let mut audio = self.generate(&clean, &config);

        // 第一次失败不代表模型坏了：多半是某个标点让 rule_fst 出了岔子。
        // 去掉符号再试一次，比直接报错让整通电话断掉划算。
        if audio.is_none() {
            let bare = strip_symbols(&clean);
            if speakable(&bare) && bare != clean {
                crate::log_warn!("TTS 合成失败，去掉符号后重试：「{}」", brief(&clean));
                audio = self.generate(&bare, &config);
            }
        }

        let Some(audio) = audio else {
            crate::log_warn!("TTS 合成失败，跳过这一句：「{}」", brief(&clean));
            return Ok(Audio {
                samples: Vec::new(),
                rate: self.rate,
            });
        };

        let mut samples = audio.samples().to_vec();
        let peak = samples.iter().fold(0.0f32, |a, x| a.max(x.abs()));
        if peak > 0.001 {
            // 上限 10 倍：极安静的句子放大到 0.85 就够，再往上只会把底噪也拉起来
            let gain = (TARGET_PEAK / peak).clamp(1.0, 10.0);
            for s in &mut samples {
                *s = (*s * gain).clamp(-1.0, 1.0);
            }
        }

        let elapsed = began.elapsed();
        if elapsed.as_millis() > SLOW_MS {
            crate::log_warn!(
                "TTS 合成偏慢：{} 字用了 {}ms（{} 采样）",
                clean.chars().count(),
                elapsed.as_millis(),
                samples.len()
            );
        }

        Ok(Audio {
            samples,
            rate: self.rate,
        })
    }

}

/// 合成器常驻容器。
///
/// 加载模型要 0.3-2.5 秒，每次通话都重建太浪费；`Mutex<Option<_>>` 而不是
/// `OnceLock`，因为要支持「卸载释放内存」。
pub struct Engine {
    inner: Mutex<Option<Loaded>>,
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

    /// 加载后的 (采样率, 音色数)。没加载时返回 None。
    pub fn info(&self) -> Option<(i32, i32)> {
        let guard = self.inner.lock().ok()?;
        guard.as_ref().map(|k| (k.rate, k.speakers))
    }

    /// 加载模型。已经加载过就直接返回，不读盘。
    pub fn start(&self, root: &Path) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "合成器状态锁失效".to_string())?;
        if guard.is_some() {
            return Ok(());
        }

        let files = files(root).ok_or_else(|| "语音合成模型还没装好".to_string())?;
        let began = Instant::now();

        let config = OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                vits: sherpa_onnx::OfflineTtsVitsModelConfig {
                    model: Some(files.model.to_string_lossy().to_string()),
                    lexicon: Some(files.lexicon.to_string_lossy().to_string()),
                    tokens: Some(files.tokens.to_string_lossy().to_string()),
                    dict_dir: files
                        .dict_dir
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string()),
                    // 官方示例值：0.667 是发音模型的随机度，调大更「有感情」也更
                    // 容易吞字；noise_scale_w 控制时长抖动
                    noise_scale: 0.667,
                    noise_scale_w: 0.8,
                    length_scale: 1.0,
                    ..Default::default()
                },
                num_threads: threads(),
                debug: false,
                ..Default::default()
            },
            rule_fsts: files.rule_fsts.clone(),
            ..Default::default()
        };

        let tts = OfflineTts::create(&config).ok_or_else(|| {
            "创建语音合成器失败：模型文件和 sherpa-onnx 版本不匹配（日志里通常有 onnx 报错）"
                .to_string()
        })?;

        let rate = tts.sample_rate();
        let speakers = tts.num_speakers();
        *guard = Some(Loaded {
            tts,
            rate,
            speakers,
        });

        crate::log_info!(
            "语音合成器就绪：{}ms，{} 音色，{}Hz",
            began.elapsed().as_millis(),
            speakers,
            rate
        );
        Ok(())
    }

    pub fn say(&self, text: &str, sid: i32, speed: f32) -> Result<Audio, String> {
        let guard = self.inner.lock().map_err(|_| "合成器状态锁失效".to_string())?;
        let k = guard
            .as_ref()
            .ok_or_else(|| "语音合成器还没启动".to_string())?;
        k.say(text, sid, speed)
    }

    /// 丢掉合成器（排查内存、换模型时用）
    pub fn shutdown(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = None;
        }
    }
}

/* ------------------------------------------------------------------ */
/* 文本处理                                                             */
/* ------------------------------------------------------------------ */

/// 合成前的文本清洗。
///
/// 主要是防 markdown：模型偶尔会不听招呼吐 `**强调**` 或反引号，直接喂进
/// 合成器会被念成「星号星号」。通话模式虽然另外下了 prompt 约束，但这一层
/// 不能省——prompt 是概率性的，清洗是确定的。
///
/// 英文单词**原样保留**：这个模型本身就是中英混的，lexicon 有 11.2 万条含
/// 中英音素，表外的缩写（API / JSON / GPU）也念得出来——实测是逐字母念的，
/// 不是吞掉。所以这里不查白名单，查表反而会误删新词和专有名词。
fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    let mut prev: Option<char> = None;
    let mut it = text.chars().peekable();

    while let Some(raw) = it.next() {
        // markdown 标记直接丢掉
        if matches!(raw, '*' | '#' | '`' | '~' | '_' | '|' | '>' | '[' | ']') {
            continue;
        }
        if is_emoji(raw) {
            continue;
        }

        // 路径/域名整段删掉：`I:/project/agent-ui`、`src/stores` 念出来是
        // 一串英文单词，对听的人没有任何信息量，还会把句子切得七零八落
        if raw.is_ascii_alphanumeric() || matches!(raw, '.' | '/' | '\\' | '~') {
            let mut probe = it.clone();
            let mut token = String::from(raw);
            // 冒号也算 token 的一部分：`I:/project`、`https://…` 的斜杠紧跟在
            // 冒号后面。只是探测用——没有斜杠时不算路径，`3:20` 原样留给后面处理
            while let Some(&c) = probe.peek() {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '\\' | '-' | '_' | ':') {
                    token.push(c);
                    probe.next();
                } else {
                    break;
                }
            }
            let slashes = token.matches('/').count() + token.matches('\\').count();
            if slashes > 0 && token.chars().count() >= 4 {
                it = probe;
                continue;
            }
        }

        // 英文单词：整串取出来再决定留不留，逐个字符判断会把词拆碎
        if raw.is_ascii_alphabetic() {
            let mut word = String::new();
            word.push(raw);
            while let Some(&next) = it.peek() {
                if next.is_ascii_alphabetic() || next == '\'' || next == '-' {
                    word.push(next);
                    it.next();
                } else {
                    break;
                }
            }
            // 和前面的中文隔开，否则「用settings改」会粘成一个词
            if !out.is_empty() && !prev_space {
                out.push(' ');
            }
            prev = word.chars().last();
            prev_space = false;
            out.push_str(&word);
            continue;
        }

        // 一串 ASCII 符号（`://`、`+`、`/`、代码里的 `() {`）基本只会来自路径、
        // 代码或 markdown 结构。中文合成器会把它们念成「冒号斜杠斜杠」，或者
        // 直接卡住不吐音——听感就是句子中间空了一截。
        // 夹在数字中间的例外：`3.14`、`3:20` 是有意义的读数，交给后面的逻辑。
        if is_speech_noise(raw) {
            let prev_digit = prev.is_some_and(|c| c.is_ascii_digit());
            let next_digit = it.peek().is_some_and(|c| c.is_ascii_digit());
            if !(prev_digit && next_digit) {
                let mut len = 1;
                while it.peek().is_some_and(|c| is_speech_noise(*c)) {
                    it.next();
                    len += 1;
                }
                // 单个句读符号换成中文标点，停顿还在（`1: 打开设置`）；
                // 成串的是路径/代码残留，整段丢（`://`、`+`）
                if let (1, Some(pause)) = (len, ascii_pause(raw)) {
                    if prev != Some(pause) {
                        out.push(pause);
                        prev = Some(pause);
                        prev_space = false;
                    }
                }
                continue;
            }
        }

        // 换行在朗读里通常该是个停顿，但合成器自己有停顿模型，给空格更安全
        let ch = match raw {
            '\n' | '\r' | '\t' => ' ',
            _ => raw,
        };

        // 千分位逗号必须吃掉。`1,234` 留着会被 number.fst 拆成两段数字，
        // 实测读成「一二百三十四」；分句器还可能把它当软断点切开。
        if (ch == ',' || ch == '，')
            && prev.is_some_and(|c| c.is_ascii_digit())
            && it.peek().is_some_and(|c| c.is_ascii_digit())
        {
            continue;
        }

        if ch == ' ' {
            if prev_space || out.is_empty() {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        prev = Some(ch);
        out.push(ch);
    }

    out.trim().to_string()
}

/// 中文朗读里没有任何意义的 ASCII 符号。
///
/// `%` 与 `‰` 留着：它们参与读数（「98.5%」读成「百分之九十八点五」）。
fn is_speech_noise(ch: char) -> bool {
    ch.is_ascii_punctuation() && !matches!(ch, '%' | '‰')
}

/// ASCII 句读符号对应的中文标点。翻成中文标点是为了留住那个停顿：
/// 「步骤 1: 打开设置」直接删掉冒号会读成「打开设置」前面毫无停顿。
fn ascii_pause(ch: char) -> Option<char> {
    match ch {
        ',' => Some('，'),
        ':' => Some('：'),
        ';' => Some('；'),
        '!' => Some('！'),
        '?' => Some('？'),
        _ => None,
    }
}

/// 这段文本里有没有合成器能念出来的东西。
///
/// 分句器偶尔会切出「……」「——」这种纯标点片段（模型自己也会吐），
/// 直接喂给合成器会返回 null。那是正常输入，不是故障，别当错误上报——
/// 一次无关紧要的报错会把整通电话打断。
fn speakable(text: &str) -> bool {
    text.chars().any(|c| c.is_alphanumeric())
}

/// 日志里截一段文本，避免把整段回答刷进日志
fn brief(text: &str) -> String {
    let n = text.chars().count();
    if n <= 40 {
        return text.to_string();
    }
    let head: String = text.chars().take(40).collect();
    format!("{head}…（共 {n} 字）")
}

/// 去掉所有非字母数字（保留空格）。合成失败时的最后一次尝试：
/// 有时候是某个标点在 rule_fst 里出了岔子，删掉它就能念出来。
fn strip_symbols(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect()
}

/// emoji 与各类图形符号。留着会被念成「表情符号」或者直接吞掉一整个词。
fn is_emoji(ch: char) -> bool {
    let c = ch as u32;
    matches!(c,
        0x1F300..=0x1FAFF
        | 0x2600..=0x27BF
        | 0xFE00..=0xFE0F
        | 0x1F000..=0x1F0FF
        | 0x2B00..=0x2BFF
    )
}

/// 流式分句器：把 LLM 吐出来的增量文本切成适合合成的句子。
///
/// 三个约束同时成立才有好的听感：
///   1. **首句要短**——用户等的是第一声，不是整段。首句在第一个句末标点就切，
///      哪怕只有 6 个字。
///   2. **后续句要长**——句子切太碎，语调每次都重新起头，像机器人报站。
///      所以非首句一直攒到一个句末标点为止。
///   3. **必须有上限**——模型可能一口气吐 200 字不带标点（比如列清单），
///      不强制切会让第一段音频等到说完才出现。
pub struct Splitter {
    buf: String,
    first: bool,
    /// 正在一段 ``` 代码块里。代码块整段不念——见 `push` 里的说明。
    in_fence: bool,
    /// 正在一行以 `|` 开头的表格行里（流式到达时一行会被拆到多次 delta）
    in_table_row: bool,
}

/// 首句允许的最短长度（字）。比这更短的句末标点会被跳过，避免「好的。」单独合成。
const MIN_FIRST: usize = 6;
/// 首句没有标点时的软切阈值（在逗号处切）。首声延迟主要看这个值。
const FIRST_TARGET: usize = 16;
/// 首句硬上限。
///
/// 首句短一点是有意的：用户等的是第一声，不是完整的一段。melo 的 RTF 是 0.35，
/// 16 字约 3 秒音频、1 秒合成。
const FIRST_HARD: usize = 24;

/// 后续句的目标长度。
///
/// **这个值决定听感有没有「分句」**。原来后续句取「最后一个句号，只要总长 ≤70」，
/// 于是模型吐一段 50 字、含三个句号的回答，会被整段切走、一口气念十几秒——中间
/// 的逗号全都不停，听起来就是「一段话没有断句」。而句子超过 70 字时又退回取
/// 「第一个句号」，可能只有两三个字，于是变成「好的。」「对的。」这种碎句，
/// 播放时一句一顿，就是「卡一下」。
///
/// 26 字约 5 秒音频，是「一口气说得完、又听得出层次」的区间。
const NEXT_TARGET: usize = 26;
/// 后续句最短长度。比这更短的片段并到下一句里，不单独合成。
const MIN_NEXT: usize = 8;
/// 后续句硬上限（整段没有可切标点时兜底）
const NEXT_HARD: usize = 34;

impl Default for Splitter {
    fn default() -> Self {
        Self::new()
    }
}

impl Splitter {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            first: true,
            in_fence: false,
            in_table_row: false,
        }
    }

    /// 喂增量文本，返回所有已经可以合成的句子。
    ///
    /// 代码块整段丢掉，不只是丢掉「```」这两个符号：块里的 `for line in buf`
    /// 会被中英混模型当成英文念出来，纯中文模型则删光英文只剩一串括号，
    /// 两种都是白跑一次合成、还留一段静音。
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        if delta.is_empty() {
            return Vec::new();
        }
        self.buf.push_str(delta);
        self.strip_tables();

        let mut out = Vec::new();
        // 一次最多切 8 句：防止异常输入（比如模型把整个文档吐进来）把这里卡死
        for _ in 0..8 {
            let Some(sentence) = self.take_ready() else {
                break;
            };
            let text = sentence.trim().to_string();
            self.first = false;
            // 围栏行翻转状态并丢掉；块内的一律不念。
            // 必须在 speakable 之前判断：收尾的 ``` 本身没有可念的字，
            // 先被当成「纯标点」丢掉的话，状态就永远关不上。
            let fences = text.matches("```").count();
            if fences > 0 {
                if fences % 2 == 1 {
                    self.in_fence = !self.in_fence;
                }
                continue;
            }
            if self.in_fence
                || text.is_empty()
                || !speakable(&text)
                || is_table_rule(&text)
                || text.starts_with('|')
            {
                continue;
            }
            out.push(text);
        }
        out
    }

    /// 把表格行整行剔掉。
    ///
    /// `| 视图层 | src/components | 侧边栏 |` 念出来是「视图层 src components
    /// 侧边栏」——没有句子结构，还会把后面的正文切碎。在切分之前删掉，比让
    /// 切点从行中间穿过去干净（那样会留下「消息流、输入框、设置面板 |」的尾巴）。
    ///
    /// 必须在分句前做，所以维护 `in_table_row`：流式到达时一行会跨好几个
    /// delta，只删当前缓冲里那段会漏掉后面的半个单元格。
    fn strip_tables(&mut self) {
        let mut out = String::with_capacity(self.buf.len());
        for line in self.buf.split_inclusive('\n') {
            if self.in_table_row {
                // 上一段 delta 结尾停在半行表格里：这行剩下的部分接着丢
                if line.ends_with('\n') {
                    self.in_table_row = false;
                }
                continue;
            }
            if line.trim_start().starts_with('|') {
                if !line.ends_with('\n') {
                    self.in_table_row = true;
                }
                continue;
            }
            out.push_str(line);
        }
        self.buf = out;
    }

    /// 本轮结束：把剩下的半句也交出去
    pub fn flush(&mut self) -> Option<String> {
        let rest = self.buf.trim().to_string();
        self.buf.clear();
        self.first = true;
        self.in_table_row = false;
        // 代码块没闭合就到结尾了：剩下的同样不念
        let fenced = std::mem::take(&mut self.in_fence);
        if fenced || rest.is_empty() || !speakable(&rest) {
            None
        } else {
            Some(rest)
        }
    }

    /// 清空但不交出内容（用户打断、开始新一轮时用）
    pub fn reset(&mut self) {
        self.buf.clear();
        self.first = true;
        self.in_fence = false;
        self.in_table_row = false;
    }

    /// 切下 `cut` 之前的文本，剩下的留在缓冲里。
    ///
    /// `forced` 表示这是断在字中间的硬切：末尾补个逗号，让合成器知道这里是
    /// 句内停顿。不补的话最后一个字经常被吞——实测「把现象记」只剩「把现象」。
    fn take(&mut self, cut: usize, forced: bool) -> String {
        let mut sentence = self.buf[..cut].to_string();
        if forced {
            sentence.push('，');
        }
        self.buf = self.buf[cut..].to_string();
        sentence
    }

    fn take_ready(&mut self) -> Option<String> {
        // 围栏必须单独成段。它只有 4 个字符（"```\n"），按正常规则会被最短长度
        // 并进后面那段正文——而围栏一翻转状态，跟着它的正文就被当成块内容丢掉了。
        if self.buf.starts_with("```") {
            return Some(self.take(3, false));
        }


        let chars: Vec<(usize, char)> = self.buf.char_indices().collect();
        // 前一句切走后会留下前导空白（空行、缩进）。不排除的话它们会吃掉长度
        // 预算：「\n\n主要分三层：」里的冒号会被当成第 8 个字，一句 6 个字的
        // 短句就被切出来了。
        let lead = chars
            .iter()
            .position(|&(_, c)| !c.is_whitespace())
            .unwrap_or(chars.len());
        let count = chars.len().saturating_sub(lead);
        if count == 0 {
            return None;
        }

        let (min_len, target, hard) = if self.first {
            (MIN_FIRST, FIRST_TARGET, FIRST_HARD)
        } else {
            (MIN_NEXT, NEXT_TARGET, NEXT_HARD)
        };

        // 切点不能越过围栏，否则围栏会和正文一起被切成一段，判定成代码块后
        // 整段（包括正文）都被丢掉
        let fence_pos = self.buf.find("```");
        let scan_end = fence_pos.unwrap_or(usize::MAX);

        // 窗口内（≤ target）的最后一个句末标点、第一个句末标点、
        // 窗口内最后一个逗号，以及「越过 target 后的第一个逗号」
        let mut win_end: Option<(usize, usize)> = None;
        let mut first_end: Option<(usize, usize)> = None;
        let mut win_soft: Option<(usize, usize)> = None;
        let mut soft_after: Option<(usize, usize)> = None;

        for (idx, &(byte, ch)) in chars.iter().enumerate() {
            if byte >= scan_end {
                break;
            }
            let len = (idx + 1).saturating_sub(lead);
            if len < min_len {
                continue;
            }
            let cut = byte + ch.len_utf8();
            if is_sentence_end(ch) {
                if first_end.is_none() {
                    first_end = Some((cut, len));
                }
                if len <= target {
                    win_end = Some((cut, len));
                }
            } else if is_soft_break(ch) && can_cut_at(&chars, idx) {
                if len <= target {
                    win_soft = Some((cut, len));
                } else if soft_after.is_none() {
                    soft_after = Some((cut, len));
                }
            }
        }

        let hard_cut = || {
            chars
                .get(lead + hard.saturating_sub(1))
                .map(|&(b, c)| b + c.len_utf8())
                .unwrap_or(usize::MAX)
                .min(scan_end)
        };

        if self.first {
            // 首句：第一个句号就走，最快出声
            if let Some((cut, len)) = first_end {
                if len <= hard {
                    return Some(self.take(cut, false));
                }
            }
            if count >= target {
                if let Some((cut, _)) = win_soft {
                    return Some(self.take(cut, false));
                }
            }
            if count >= hard {
                let cut = hard_cut();
                return Some(self.take(cut, true));
            }
            return None;
        }

        // 后续句：优先切在窗口内的句号上（一层意思说完）
        if let Some((cut, _)) = win_end {
            return Some(self.take(cut, false));
        }
        // 句号太远：攒够 target 就退到逗号，别等一整段
        if count >= target {
            if let Some((cut, _)) = win_soft {
                return Some(self.take(cut, false));
            }
            // 句号在 target 和 hard 之间：等它，比断在词中间自然
            if let Some((cut, len)) = first_end {
                if len <= hard {
                    return Some(self.take(cut, false));
                }
            }
            // 一个标点都没落在窗口里（句子带长路径或英文词时很常见）：
            // 退到越过 target 后的第一个逗号，总比硬切在「这是一｜个」中间好
            if let Some((cut, len)) = soft_after {
                if len <= hard {
                    return Some(self.take(cut, false));
                }
            }
        }
        if count >= hard {
            let cut = hard_cut();
            return Some(self.take(cut, true));
        }
        // 围栏就在后面，前面的正文又没有任何标点：先把正文整段交出去，
        // 围栏留到下一次 push 再单独切
        if let Some(pos) = fence_pos {
            if pos > 0 {
                return Some(self.take(pos, true));
            }
        }
        None
    }
}

/// `| --- | --- |` 这种表格分隔行。念不出来，还会白占一次合成。
fn is_table_rule(text: &str) -> bool {
    text.chars()
        .all(|c| matches!(c, '|' | ':' | '-' | '—' | ' ' | '\t'))
}

fn is_sentence_end(ch: char) -> bool {
    matches!(ch, '。' | '！' | '？' | '；' | '!' | '?' | ';' | '\n' | '…')
}

fn is_soft_break(ch: char) -> bool {
    matches!(ch, '，' | '、' | '：' | ',' | ':' | '—')
}

/// `1,234` 里那个逗号：两边都是数字，切开会把数字拆成两段
fn can_cut_at(chars: &[(usize, char)], idx: usize) -> bool {
    let ch = chars[idx].1;
    let prev = idx
        .checked_sub(1)
        .and_then(|i| chars.get(i))
        .map(|&(_, c)| c);
    let next = chars.get(idx + 1).map(|&(_, c)| c);
    match ch {
        // 千分位：`1,234` 从这里断会读成「一 二百三十四」
        ',' | '，' => {
            let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
            !(digit(prev) && digit(next))
        }
        // 路径和 URL 的冒号：`I:/project`、`https://…` 从这里断，前半句是
        // 「我看了 I：」、后半句从 `/project` 开始，两边都念不成
        ':' | '：' => !matches!(next, Some('/') | Some('\\')),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_full_stop() {
        let mut s = Splitter::new();
        assert_eq!(s.push("今天天气不错。"), vec!["今天天气不错。"]);
    }

    #[test]
    fn first_sentence_is_long_enough() {
        let mut s = Splitter::new();
        // 「好的。」太短，不该单独成句——等下一句凑够再切
        let out = s.push("好的。我来帮你查一下今天的天气。");
        assert_eq!(out, vec!["好的。我来帮你查一下今天的天气。"]);
    }

    #[test]
    fn short_fragments_merge_into_next() {
        let mut s = Splitter::new();
        assert_eq!(s.push("我们来看第一句。"), vec!["我们来看第一句。"]);
        // 短于 MIN_NEXT 的片段不单独合成——「好。行。」两句各 2 字，
        // 单独合成就是两次 IPC、两段静音，听感是一顿一顿的
        assert!(s.push("好。行。").is_empty());
        assert_eq!(
            s.push("我知道了，马上处理。"),
            vec!["好。行。我知道了，马上处理。"]
        );
    }

    #[test]
    fn long_paragraph_is_split_by_target() {
        let mut s = Splitter::new();
        // 一段 50 多字、含三个句号的话不能整段念（旧规则会切 50 字，一口气
        // 念十几秒，中间逗号全不停）。现在每句都不超过 NEXT_TARGET
        let out = s.push(
            "这是一个 Tauri 项目，前端代码在 src 目录下。后端逻辑在 src-tauri 里。数据库用 sqlite 存。",
        );
        assert!(out.len() >= 2, "应当切出多句，实际：{out:?}");
        for sentence in &out {
            assert!(
                sentence.chars().count() <= NEXT_TARGET + 1,
                "句子超长（{} 字）：{sentence}",
                sentence.chars().count()
            );
        }
    }

    #[test]
    fn no_tiny_fragments() {
        let mut s = Splitter::new();
        let mut out = s.push("好的。我看了一下这个项目，结构挺清楚的，主要分三层。");
        if let Some(rest) = s.flush() {
            out.push(rest);
        }
        assert!(
            out.iter().all(|x| x.chars().count() >= MIN_FIRST),
            "不该出现碎句：{out:?}"
        );
    }

    #[test]
    fn hard_cut_without_punctuation() {
        let mut s = Splitter::new();
        let long = "啊".repeat(200);
        let out = s.push(&long);
        assert!(!out.is_empty());
        // 硬切后补一个逗号，所以是 FIRST_HARD + 1
        assert_eq!(out[0].chars().count(), FIRST_HARD + 1);
        assert!(out[0].ends_with('，'), "硬切要补逗号，否则末尾字会被吞");
    }

    #[test]
    fn flush_returns_remainder() {
        let mut s = Splitter::new();
        assert!(s.push("还没说完的一半").is_empty());
        assert_eq!(s.flush().as_deref(), Some("还没说完的一半"));
        // flush 之后回到首句状态
        assert!(s.flush().is_none());
    }

    #[test]
    fn reset_drops_buffer() {
        let mut s = Splitter::new();
        let _ = s.push("被打断的内容");
        s.reset();
        assert!(s.flush().is_none());
    }

    #[test]
    fn sanitize_strips_markdown() {
        assert_eq!(sanitize("**加粗**的`代码`"), "加粗的代码");
        assert_eq!(sanitize("第一行\n第二行"), "第一行 第二行");
        assert_eq!(sanitize(" 多个   空格 "), "多个 空格");
    }

    #[test]
    fn sanitize_drops_emoji() {
        assert_eq!(sanitize("好的👍"), "好的");
    }

    #[test]
    fn sanitize_removes_thousands_separator() {
        // 留着会被 number.fst 拆成两段，读成「一 二百三十四」
        assert_eq!(sanitize("费用是1,234元"), "费用是1234元");
        assert_eq!(sanitize("1，234"), "1234");
        assert_eq!(sanitize("1,2,3"), "123");
        // 不是数字之间的逗号要保留：它是正常停顿
        assert_eq!(sanitize("好的，走吧"), "好的，走吧");
    }

    #[test]
    fn speakable_rejects_punctuation_only() {
        assert!(!speakable("……"));
        assert!(!speakable("——！"));
        assert!(!speakable(""));
        assert!(speakable("好。"));
        assert!(speakable("2026"));
        assert!(speakable("API"));
    }

    #[test]
    fn splitter_skips_punctuation_only() {
        let mut s = Splitter::new();
        // 非首句时「。。。」能凑够 min_len，但它没有可念的字
        assert!(s.push("好的我明白了。").len() == 1);
        assert!(s.push("。。。。。。").is_empty());
        assert!(s.flush().is_none());
    }

    #[test]
    fn english_words_are_kept() {
        // 这个模型本身就是中英混的：lexicon 11.2 万条含中英音素，表外的缩写
        // （API / JSON / GPU）也念得出来——实测是逐字母念的，不是吞掉。
        // 所以清洗层不查白名单，原文照留
        let out = sanitize("用 API 调 JSON，再传给 GPU");
        assert!(out.contains("API"), "API 被删掉了：{out}");
        assert!(out.contains("JSON"), "JSON 被删掉了：{out}");
        assert!(out.contains("GPU"), "GPU 被删掉了：{out}");
    }

    #[test]
    fn sanitize_drops_symbol_islands() {
        // 路径、代码、markdown 结构留下的符号串：念出来是「冒号斜杠斜杠」
        assert_eq!(sanitize("我看了 I:/project/agent-ui 的结构"), "我看了 的结构");
        assert_eq!(
            sanitize("这是一个 Tauri + Vite + Vue 3 项目"),
            "这是一个 Tauri Vite Vue 3 项目"
        );
        assert_eq!(sanitize("模块 A | 模块 B"), "模块 A 模块 B");
        // 单个句读符号翻成中文标点，停顿不能丢
        assert_eq!(sanitize("步骤 1: 打开设置"), "步骤 1： 打开设置");
        assert_eq!(sanitize("先这样,再那样"), "先这样，再那样");
        // 数字里的符号是读数的一部分，不能删
        assert_eq!(sanitize("成功率 98.5%"), "成功率 98.5%");
        assert_eq!(sanitize("3:20 开会"), "3:20 开会");
    }

    #[test]
    fn splitter_skips_code_fences() {
        let mut s = Splitter::new();
        assert_eq!(s.push("我看一下实现。"), vec!["我看一下实现。"]);
        // 围栏行翻转状态并丢掉，块里的内容一句都不念
        assert!(s.push("```rust\n").is_empty());
        assert!(s.push("for line in buf {\n").is_empty());
        assert!(s.push("    let a = 1;\n").is_empty());
        assert!(s.push("```\n").is_empty());
        assert_eq!(s.push("看完了，没问题。"), vec!["看完了，没问题。"]);
    }

    #[test]
    fn splitter_skips_unclosed_fence_at_end() {
        let mut s = Splitter::new();
        assert!(s.push("```python\nprint(1)").is_empty());
        // 块没闭合就到结尾：剩下的同样不念，不然「print(1)」会被念出来
        assert!(s.flush().is_none());
    }

    #[test]
    fn splitter_skips_table_rule() {
        let mut s = Splitter::new();
        assert!(s.push("| --- | --- | --- |\n").is_empty());
        assert!(s.flush().is_none());
        // 下一句照常念，状态没被带坏
        assert_eq!(s.push("表格完了，接着说。"), vec!["表格完了，接着说。"]);
    }

    #[test]
    fn splitter_keeps_thousands_separator_together() {
        let mut s = Splitter::new();
        // 首句没有句末标点，只能靠软断点；数字中间的逗号不能当断点
        let out = s.push("这个数字是1,234,567");
        assert!(out.is_empty(), "不该在数字之间切开，得到 {out:?}");
        let whole = s.flush().unwrap();
        assert!(whole.contains("1,234,567"), "整段数字要完整：{whole}");
    }
}
