//! 本地语音合成：进程内 ONNX 推理，三种模型按可用性自动选择。
//!
//! # 三个模型，用途不同
//!
//! | 模型 | 大小 | 实测 RTF | 音色 | 特点 |
//! |---|---|---|---|---|
//! | vits-melo-tts-zh_en | 170MB | 0.32 | 1 个女声 | **默认**，中英混读，44.1kHz |
//! | matcha-icefall-zh-baker | 72+54MB | **0.04** | 1 个女声 | 最快，但一个英文都不认得 |
//! | kokoro-multi-lang-v1_1 | 348MB | 0.50 | 103 个 | 音色多，中文词表残缺 |
//!
//! 默认走 melo：它是这三个里唯一「中文准、英文也念得出来」的。matcha 快 8 倍，
//! 但送英文进去返回 0 采样——不是念错，是完全不出声，中英混排的句子会中间空
//! 一截。通话语境里「内容完整」比「快 0.4 秒」重要。
//!
//! Kokoro 的 103 个音色留着给朗读长文用，通话不推荐（首声 1.8 秒）。
//!
//! 探测顺序 melo 优先。用户在设置里指定了模型时按用户的来（[`choose`]），
//! 指定的那个没装就退回自动，不会变成「没声音」。
//!
//! # 为什么整句合成而不是流式回调
//!
//! matcha 合成一句 15 字只要 60-100ms，分块回调省下的等待还不如「一块 PCM
//! 直接排进播放队列」可靠——回调的块边界语义不明确，接错了就是重复播放或漏音。
//! 句间无缝靠前端的播放队列保证。
//!
//! # 换后端
//!
//! [`Speech`] 是留给以后接云 TTS 的扩展点：调用方只依赖 trait，换实现不用改
//! 通话状态机。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsMatchaModelConfig, OfflineTtsModelConfig,
};

use crate::stream_stt::{pick, scan};

pub const MELO_DIR: &str = "vits-melo-tts-zh_en";
pub const MATCHA_DIR: &str = "matcha-icefall-zh-baker";
pub const KOKORO_DIR: &str = "kokoro-multi-lang-v1_1";

/// melo 的压缩包（163MB，解压 170MB 的 fp32 模型）
///
/// 包里还有一个 `model.int8.onnx`，只有 **133 字节**——那是 git-lfs 的指针
/// 文件，不是模型。挑文件时必须点名 `model.onnx`，否则加载的是它，报「模型
/// 与 sherpa-onnx 版本不匹配」。
pub const MELO_BYTES: u64 = 167_006_755;
/// matcha 的声学模型包
pub const MATCHA_BYTES: u64 = 75_455_508;
/// matcha 的声码器（单独发布，不在压缩包里）
pub const VOCODER_BYTES: u64 = 53_884_024;
/// kokoro 的压缩包
pub const KOKORO_BYTES: u64 = 364_855_331;

const MATCHA_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/matcha-icefall-zh-baker.tar.bz2";
const VOCODER_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/vocoder-models/vocos-22khz-univ.onnx";
const VOCODER_NAME: &str = "vocos-22khz-univ.onnx";

/// 声码器在模型目录里的文件名（下载和查找共用这一个名字）
pub fn vocoder_name() -> &'static str {
    VOCODER_NAME
}

const KOKORO_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_1.tar.bz2";
const MELO_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-melo-tts-zh_en.tar.bz2";

/// 允许用镜像覆盖（国内直连 GitHub 经常只有几十 KB/s）
pub fn matcha_url() -> String {
    std::env::var("SHANIA_TTS_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| MATCHA_URL.to_string())
}

pub fn melo_url() -> String {
    std::env::var("SHANIA_TTS_MELO_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| MELO_URL.to_string())
}

pub fn vocoder_url() -> String {
    std::env::var("SHANIA_TTS_VOCODER_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| VOCODER_URL.to_string())
}

pub fn kokoro_url() -> String {
    std::env::var("SHANIA_TTS_KOKORO_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| KOKORO_URL.to_string())
}

/// 用哪种模型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Melo,
    Matcha,
    Kokoro,
}

impl Kind {
    pub fn label(&self) -> &'static str {
        match self {
            Kind::Melo => "MeloTTS（中英混，单音色）",
            Kind::Matcha => "Matcha（最快，单音色，不会英文）",
            Kind::Kokoro => "Kokoro（慢，103 音色）",
        }
    }

    pub fn dir_name(&self) -> &'static str {
        match self {
            Kind::Melo => MELO_DIR,
            Kind::Matcha => MATCHA_DIR,
            Kind::Kokoro => KOKORO_DIR,
        }
    }

    /// 存进 `settings.json` 的标识
    pub fn id(&self) -> &'static str {
        match self {
            Kind::Melo => "melo",
            Kind::Matcha => "matcha",
            Kind::Kokoro => "kokoro",
        }
    }

    pub fn from_id(s: &str) -> Option<Kind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "melo" | "melotts" => Some(Kind::Melo),
            "matcha" => Some(Kind::Matcha),
            "kokoro" => Some(Kind::Kokoro),
            _ => None,
        }
    }

    /// 压缩包大小，下载完拿来校验，防止存到半截文件或错误页
    pub fn archive_bytes(&self) -> u64 {
        match self {
            Kind::Melo => MELO_BYTES,
            Kind::Matcha => MATCHA_BYTES,
            Kind::Kokoro => KOKORO_BYTES,
        }
    }

    /// 合成时开多少线程。
    ///
    /// melo 的声学模型是自回归结构，单步慢，核给少了就是在干等——实测 4 线程
    /// RTF 0.50、8 线程 0.40，再多收益就平了。另外两个是前馈模型，2-4 线程
    /// 已经够，开满只会跟 X-ASR 抢 CPU（识别卡顿比合成慢更影响通话）。
    fn threads(&self) -> i32 {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4);
        match self {
            Kind::Melo => (cores / 2).clamp(4, 10),
            _ => cores.clamp(2, 4),
        }
    }

    /// 目标峰值：`None` 表示原样输出。
    ///
    /// melo 的峰值只有 0.07-0.25，matcha 是 0.60——同样把系统音量开满，听感
    /// 差一个数量级，用户第一反应是「这个模型坏了」。按句归一化到 0.85，
    /// 而不是乘一个固定倍数：固定 4 倍会让本来就有 0.25 的句子撞到 1.0 削波
    /// （实测「长句」和带 markdown 的那句都满了），削波听起来是「滋」的一声，
    /// 比声音小难听得多。
    fn target_peak(&self) -> Option<f32> {
        match self {
            Kind::Melo => Some(0.85),
            _ => None,
        }
    }

    /// 超过这个耗时就在日志里记一条（超过正常水平＝机器在忙或选错了模型）
    fn slow_ms(&self) -> u128 {
        match self {
            Kind::Melo => 2500,
            Kind::Matcha => 800,
            Kind::Kokoro => 3000,
        }
    }
}

pub fn dir(root: &Path, kind: Kind) -> PathBuf {
    root.join("models").join(kind.dir_name())
}

/// 解压后真正要喂给合成器的文件
#[derive(Debug, Clone)]
pub struct Files {
    pub kind: Kind,
    /// 声学模型（matcha 是 model-steps-3.onnx，kokoro 是 model.onnx）
    pub model: PathBuf,
    pub tokens: PathBuf,
    /// matcha 用单个 lexicon.txt；kokoro 是中英双语，逗号拼两本
    pub lexicon: String,
    /// 英文词表（只有 kokoro 有）。合成前拿它判断哪些英文词念得出来。
    pub english: Option<PathBuf>,
    pub rule_fsts: Option<String>,
    /// kokoro 独有：音色向量表
    pub voices: Option<PathBuf>,
    /// matcha 独有：声码器、jieba 词典
    pub vocoder: Option<PathBuf>,
    pub dict_dir: Option<PathBuf>,
}

/// 找可用的模型，matcha 优先。
pub fn files(root: &Path) -> Option<Files> {
    files_for(root, choose(root, None)?)
}

/// 指定模型的可用文件，没装完整则返回 None。
pub fn files_for(root: &Path, kind: Kind) -> Option<Files> {
    match kind {
        Kind::Melo => melo_files(root),
        Kind::Matcha => matcha_files(root),
        Kind::Kokoro => kokoro_files(root),
    }
}

/// 这一轮该用哪个模型。
///
/// `prefer` 来自设置里的 `ttsModel`（`auto` / `melo` / `matcha` / `kokoro`）。
/// 指定的那个没装就退回自动——否则用户点「只用 Melo」却没下模型，会从
/// 「音质差一点」变成「完全不出声」。
pub fn choose(root: &Path, prefer: Option<Kind>) -> Option<Kind> {
    if let Some(kind) = prefer {
        if files_for(root, kind).is_some() {
            return Some(kind);
        }
    }
    [Kind::Melo, Kind::Matcha, Kind::Kokoro]
        .into_iter()
        .find(|k| files_for(root, *k).is_some())
}

pub fn ready(root: &Path) -> bool {
    files(root).is_some()
}

/// 指定模型的目录占用
pub fn size(root: &Path, kind: Kind) -> u64 {
    let mut all = Vec::new();
    scan(&dir(root, kind), &mut all, 0);
    all.iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

fn melo_files(root: &Path) -> Option<Files> {
    let base = dir(root, Kind::Melo);
    let mut all = Vec::new();
    scan(&base, &mut all, 0);
    if all.is_empty() {
        return None;
    }

    // 点名 model.onnx：包里同时有一个 133 字节的 model.int8.onnx（git-lfs 指针），
    // 按关键词模糊匹配可能挑到它，加载时只报「版本不匹配」，很难查
    let model = all
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some("model.onnx"))
        .cloned()
        .or_else(|| pick(&all, "model", ".onnx"))?;
    let tokens = pick(&all, "tokens", ".txt")?;
    let lexicon = pick(&all, "lexicon", ".txt")?.display().to_string();

    // 四个 FST 各有分工：数字、日期、电话号码，以及 new_heteronym（多音字，
    // 「银行/行走」这类）。少一个不会崩，但那类词会念错
    let fsts: Vec<String> = ["phone", "date", "number", "new_heteronym"]
        .into_iter()
        .filter_map(|name| pick(&all, name, ".fst"))
        .map(|p| p.display().to_string())
        .collect();

    let dict = base.join("dict");

    Some(Files {
        kind: Kind::Melo,
        model,
        tokens,
        lexicon,
        // melo 不走白名单：见 Latin::Keep 的说明
        english: None,
        rule_fsts: if fsts.is_empty() {
            None
        } else {
            Some(fsts.join(","))
        },
        voices: None,
        vocoder: None,
        dict_dir: if dict.is_dir() { Some(dict) } else { None },
    })
}

fn matcha_files(root: &Path) -> Option<Files> {
    let base = dir(root, Kind::Matcha);
    let mut all = Vec::new();
    scan(&base, &mut all, 0);
    if all.is_empty() {
        return None;
    }

    let model = pick(&all, "model", ".onnx")?;
    let vocoder = pick(&all, "vocos", ".onnx")?;
    let tokens = pick(&all, "tokens", ".txt")?;
    let lexicon = pick(&all, "lexicon", ".txt")?.display().to_string();

    // 三个 FST 都在包里：数字、日期、电话号码的读法全靠它们
    let fsts: Vec<String> = ["phone", "date", "number"]
        .into_iter()
        .filter_map(|name| pick(&all, name, ".fst"))
        .map(|p| p.display().to_string())
        .collect();

    // jieba 词典，缺了中文分词会退化但还能跑
    let dict = base.join("dict");

    Some(Files {
        kind: Kind::Matcha,
        model,
        tokens,
        lexicon,
        english: None,
        rule_fsts: if fsts.is_empty() {
            None
        } else {
            Some(fsts.join(","))
        },
        voices: None,
        vocoder: Some(vocoder),
        dict_dir: if dict.is_dir() { Some(dict) } else { None },
    })
}

fn kokoro_files(root: &Path) -> Option<Files> {
    let base = dir(root, Kind::Kokoro);
    let mut all = Vec::new();
    scan(&base, &mut all, 0);
    if all.is_empty() {
        return None;
    }

    let model = all
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some("model.onnx"))
        .cloned()
        .or_else(|| pick(&all, "model", ".onnx"))?;
    let voices = pick(&all, "voices", ".bin")?;
    let tokens = pick(&all, "tokens", ".txt")?;

    // 中英混排模型，两个 lexicon 都要给：只给中文时英文单词会被逐字母念
    let en = pick(&all, "lexicon-us-en", ".txt").or_else(|| pick(&all, "lexicon-en", ".txt"));
    let zh = pick(&all, "lexicon-zh", ".txt");
    let lexicon = match (&en, &zh) {
        (Some(a), Some(b)) => format!("{},{}", a.display(), b.display()),
        (Some(a), None) => a.display().to_string(),
        (None, Some(b)) => b.display().to_string(),
        (None, None) => return None,
    };

    // 三个都要：漏了 number-zh 时阿拉伯数字会被 lexicon 判成 OOV 直接吞掉，
    // 听感是「成功率比上个月提高了 个百分点」——中间那个数整个消失。
    let fsts: Vec<String> = [
        pick(&all, "phone-zh", ".fst"),
        pick(&all, "date-zh", ".fst"),
        pick(&all, "number-zh", ".fst"),
    ]
    .into_iter()
    .flatten()
    .map(|p| p.display().to_string())
    .collect();

    // 词表和分词词典：dict 里有 jieba 的用户词典，缺了会把整句当成一个词，
    // 于是 lexicon 全查不到、退回按拼音念——「我看了下」念成「Well, can less」
    let dict = base.join("dict");

    // voices.bin 是 kokoro 的核心资产，加载时单独取
    Some(Files {
        kind: Kind::Kokoro,
        model,
        tokens,
        lexicon,
        english: en,
        rule_fsts: if fsts.is_empty() {
            None
        } else {
            Some(fsts.join(","))
        },
        voices: Some(voices),
        vocoder: None,
        dict_dir: if dict.is_dir() { Some(dict) } else { None },
    })
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

/// 语音合成后端。换云 TTS 时实现这个 trait 即可，调用方不用动。
pub trait Speech: Send + Sync {
    fn say(&self, text: &str, sid: i32, speed: f32) -> Result<Audio, String>;
    fn num_speakers(&self) -> i32;
    fn sample_rate(&self) -> i32;
}

struct Loaded {
    tts: OfflineTts,
    rate: i32,
    speakers: i32,
    kind: Kind,
    /// 念得出哪些英文词
    latin: Latin,
    /// 归一化目标峰值（melo 的原始输出太小），`None` 表示原样输出
    target_peak: Option<f32>,
}

impl Loaded {
    /// 合成一次。失败返回 None（调用方决定重试还是跳过）。
    fn generate(&self, text: &str, config: &GenerationConfig) -> Option<sherpa_onnx::GeneratedAudio> {
        self.tts
            .generate_with_config(text, config, None::<fn(&[f32], f32) -> bool>)
    }
}

impl Speech for Loaded {
    fn say(&self, text: &str, sid: i32, speed: f32) -> Result<Audio, String> {
        let clean = sanitize_with(text, &self.latin);
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
        if let Some(target) = self.target_peak {
            let peak = samples.iter().fold(0.0f32, |a, x| a.max(x.abs()));
            if peak > 0.001 {
                // 上限 10 倍：极安静的句子放大到 0.85 就够，再往上只会把底噪也拉起来
                let gain = (target / peak).clamp(1.0, 10.0);
                for s in &mut samples {
                    *s = (*s * gain).clamp(-1.0, 1.0);
                }
            }
        }

        // 慢到这个程度说明机器在忙，或者模型选错了
        let elapsed = began.elapsed();
        if elapsed.as_millis() > self.kind.slow_ms() as u128 {
            crate::log_warn!(
                "TTS 合成偏慢：{} 字用了 {}ms（{}，{} 采样）",
                clean.chars().count(),
                elapsed.as_millis(),
                self.kind.label(),
                samples.len()
            );
        }

        Ok(Audio {
            samples,
            rate: self.rate,
        })
    }

    fn num_speakers(&self) -> i32 {
        self.speakers
    }

    fn sample_rate(&self) -> i32 {
        self.rate
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

    /// 加载后的 (采样率, 音色数, 模型种类)。没加载时返回 None。
    pub fn info(&self) -> Option<(i32, i32, Kind)> {
        let guard = self.inner.lock().ok()?;
        guard.as_ref().map(|k| (k.rate, k.speakers, k.kind))
    }

    /// 加载模型。已经加载过就直接返回，不读盘。
    ///
    /// `prefer` 换了模型时会重载：把旧的丢掉再按新的建。切换不可能高频，
    /// 而两个模型同时在内存里要多占 350MB，不值得留。
    pub fn start(&self, root: &Path, prefer: Option<Kind>) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "合成器状态锁失效".to_string())?;

        let want = choose(root, prefer).ok_or_else(|| "语音合成模型还没装好".to_string())?;
        if let Some(current) = guard.as_ref() {
            if current.kind == want {
                return Ok(());
            }
            crate::log_info!(
                "语音合成模型切换：{} → {}",
                current.kind.label(),
                want.label()
            );
            *guard = None;
        }

        let files = files_for(root, want).ok_or_else(|| "语音合成模型还没装好".to_string())?;
        let began = Instant::now();

        // 英文处理：kokoro 有 17.8 万词的英文 lexicon，只有表里的词念得对；
        // matcha 一行英文都没有，全删；melo 自己就是中英混，全留
        let latin = match files.kind {
            Kind::Melo => Latin::Keep,
            Kind::Matcha => Latin::Drop,
            Kind::Kokoro => match &files.english {
                Some(path) => Latin::Allow(load_words(path)),
                None => Latin::Drop,
            },
        };

        let threads = files.kind.threads();
        let model_config = match files.kind {
            Kind::Melo => OfflineTtsModelConfig {
                vits: sherpa_onnx::OfflineTtsVitsModelConfig {
                    model: Some(files.model.to_string_lossy().to_string()),
                    lexicon: Some(files.lexicon.clone()),
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
                num_threads: threads,
                debug: false,
                ..Default::default()
            },
            Kind::Matcha => {
                let vocoder = files
                    .vocoder
                    .as_ref()
                    .ok_or_else(|| "matcha 缺少声码器 vocos-22khz-univ.onnx".to_string())?;
                OfflineTtsModelConfig {
                    matcha: OfflineTtsMatchaModelConfig {
                        acoustic_model: Some(files.model.to_string_lossy().to_string()),
                        vocoder: Some(vocoder.to_string_lossy().to_string()),
                        tokens: Some(files.tokens.to_string_lossy().to_string()),
                        lexicon: Some(files.lexicon.clone()),
                        dict_dir: files
                            .dict_dir
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string()),
                        noise_scale: 0.667,
                        length_scale: 1.0,
                        ..Default::default()
                    },
                    num_threads: threads,
                    debug: false,
                    ..Default::default()
                }
            }
            Kind::Kokoro => {
                let voices = files
                    .voices
                    .as_ref()
                    .ok_or_else(|| "kokoro 缺少 voices.bin".to_string())?;
                OfflineTtsModelConfig {
                    kokoro: OfflineTtsKokoroModelConfig {
                        model: Some(files.model.to_string_lossy().to_string()),
                        voices: Some(voices.to_string_lossy().to_string()),
                        tokens: Some(files.tokens.to_string_lossy().to_string()),
                        lexicon: Some(files.lexicon.clone()),
                        // lang 在 v2.0.0 起已废弃（espeak 不再参与），语言由
                        // lexicon 决定，这里不再传。
                        dict_dir: files
                            .dict_dir
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string()),
                        length_scale: 1.0,
                        ..Default::default()
                    },
                    num_threads: threads,
                    debug: false,
                    ..Default::default()
                }
            }
        };

        let config = OfflineTtsConfig {
            model: model_config,
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
            kind: files.kind,
            latin,
            target_peak: files.kind.target_peak(),
        });

        crate::log_info!(
            "语音合成器就绪：{}ms，{} 音色，{}Hz，{}",
            began.elapsed().as_millis(),
            speakers,
            rate,
            files.kind.label()
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
/// 拉丁字母的处理策略。
///
/// 中文模型念不出英文：matcha 的 lexicon 有 66375 行、**一个英文词都没有**，
/// kokoro 有 17 万词的英文表。念不出的时候不是安静跳过，而是被映射成别的音素——
/// 实测 "API" 和 "GPT" 都念成 "total"，"settings" 念成「操操」。两个不同的词撞成
/// 同一个音，听感上就是「同一个字被重读了两遍」。
///
/// 所以超出能力的词一律删掉：宁可少念一个词，也不要念出不相干的音。
#[derive(Debug)]
pub enum Latin {
    /// 一个都不留（matcha）
    Drop,
    /// 只留词典里有的，顺便统一小写——`Settings` 转成 `settings` 才查得到（kokoro）
    Allow(std::collections::HashSet<String>),
    /// 全留（melo）
    ///
    /// melo 本身就是中英混模型，lexicon 11.2 万条含中英音素；表外的缩写
    /// （API / JSON / GPU）它也出声——实测是逐字母念出来的，不是吞掉。
    /// 所以这里不需要白名单，查表反而会误删新词和专有名词。
    Keep,
}

impl Latin {
    fn keep(&self, word: &str) -> Option<String> {
        let lower = word.to_ascii_lowercase();
        match self {
            Latin::Drop => None,
            Latin::Allow(set) => set.contains(&lower).then_some(lower),
            Latin::Keep => Some(word.to_string()),
        }
    }
}

/// 读 lexicon 的英文词表。每行是 `词 音素...`，只要第一列。
///
/// 读失败时返回空集合，等于「所有英文都删」——宁可少念，也不念错。
fn load_words(path: &Path) -> std::collections::HashSet<String> {
    let Ok(body) = std::fs::read_to_string(path) else {
        crate::log_warn!("读不到英文词表：{}", path.display());
        return std::collections::HashSet::new();
    };
    body.lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
fn sanitize(text: &str) -> String {
    sanitize_with(text, &Latin::Drop)
}

fn sanitize_with(text: &str, latin: &Latin) -> String {
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
            if let Some(kept) = latin.keep(&word) {
                // 和前面的中文隔开，否则「用settings改」会粘成一个词
                if !out.is_empty() && !prev_space {
                    out.push(' ');
                }
                prev = kept.chars().last();
                prev_space = false;
                out.push_str(&kept);
            }
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
}

/// 首句允许的最短长度（字）。比这更短的句末标点会被跳过，避免「好的。」单独合成。
const MIN_FIRST: usize = 6;
/// 首句没有标点时的软切阈值（在逗号/顿号处切）
const FIRST_SOFT: usize = 16;
/// 首句硬上限。
///
/// 这个值直接决定首声延迟：matcha 的 RTF 是 0.04，22 字约 2.6 秒音频、100ms 合成；
/// 而用 Kokoro（RTF 0.5）时同样的句子要 1.3 秒，首声会明显变慢。
const FIRST_HARD: usize = 28;
/// 后续句的软切阈值
const SOFT_MAX: usize = 40;
/// 后续句硬上限
const HARD_MAX: usize = 70;

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
        }
    }

    /// 喂增量文本，返回所有已经可以合成的句子。
    ///
    /// 代码块整段丢掉，不只是丢掉「```」这两个符号：块里的
    /// `for line in buf.split_inclusive` 在 Kokoro 的英文词表里能查到 for/in/line，
    /// 于是被一本正经念成英文；matcha 则删光英文只剩一串括号，白跑一次合成还
    /// 留一段静音。两种都不该出声。
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        if delta.is_empty() {
            return Vec::new();
        }
        self.buf.push_str(delta);

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
            if self.in_fence || text.is_empty() || !speakable(&text) || is_table_rule(&text) {
                continue;
            }
            out.push(text);
        }
        out
    }

    /// 本轮结束：把剩下的半句也交出去
    pub fn flush(&mut self) -> Option<String> {
        let rest = self.buf.trim().to_string();
        self.buf.clear();
        self.first = true;
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
    }

    fn take_ready(&mut self) -> Option<String> {
        let chars: Vec<(usize, char)> = self.buf.char_indices().collect();
        let count = chars.len();
        if count == 0 {
            return None;
        }

        let (soft_limit, hard_limit) = if self.first {
            (FIRST_SOFT, FIRST_HARD)
        } else {
            (SOFT_MAX, HARD_MAX)
        };
        let min_len = if self.first { MIN_FIRST } else { 2 };

        let mut first_end: Option<usize> = None;
        let mut last_end: Option<usize> = None;
        let mut soft: Option<usize> = None;

        for (idx, &(byte, ch)) in chars.iter().enumerate() {
            let len = idx + 1;
            if is_sentence_end(ch) {
                if len >= min_len {
                    if first_end.is_none() {
                        first_end = Some(byte + ch.len_utf8());
                    }
                    last_end = Some(byte + ch.len_utf8());
                }
            } else if is_soft_break(ch) && len >= min_len && !is_thousands_separator(&chars, idx) {
                soft = Some(byte + ch.len_utf8());
            }
        }

        // 首句取第一个句末标点（最快出声），后续句取最后一个（合并得更多、语调更连贯）
        let cut = if self.first {
            first_end
        } else {
            last_end.or_else(|| if count >= soft_limit { soft } else { None })
        };

        let forced = cut.is_none();
        let cut = cut.or_else(|| {
            if count >= hard_limit {
                // 没有任何标点可切：按硬上限截断，末端宁可断在字中间也不能无限等
                Some(
                    chars
                        .get(hard_limit - 1)
                        .map(|&(b, c)| b + c.len_utf8())
                        .unwrap_or(self.buf.len()),
                )
            } else {
                None
            }
        })?;

        let mut sentence = self.buf[..cut].to_string();
        if forced {
            // 硬切是人为断的：末尾补个逗号，让合成器知道这里是句内停顿。
            // 不补的话最后一个字经常被吞——实测「把现象记」只剩「把现象」。
            sentence.push('，');
        }
        self.buf = self.buf[cut..].to_string();
        Some(sentence)
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
fn is_thousands_separator(chars: &[(usize, char)], idx: usize) -> bool {
    if !matches!(chars[idx].1, ',' | '，') {
        return false;
    }
    let digit = |c: Option<&(usize, char)>| c.is_some_and(|&(_, c)| c.is_ascii_digit());
    digit(idx.checked_sub(1).and_then(|i| chars.get(i))) && digit(chars.get(idx + 1))
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
    fn later_sentences_merge() {
        let mut s = Splitter::new();
        assert_eq!(s.push("我们来看第一句。"), vec!["我们来看第一句。"]);
        // 第二句起合并到最后一个句末标点，一次切出两小句
        assert_eq!(s.push("好。行。"), vec!["好。行。"]);
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
    fn latin_words_follow_lexicon() {
        // matcha 的 lexicon 一行英文都没有：整词删掉，不留半个残字
        assert_eq!(sanitize_with("用 API 模型", &Latin::Drop), "用 模型");
        assert_eq!(sanitize_with("total 和 API", &Latin::Drop), "和");

        // kokoro 只留英文词表里有的，并统一小写（词典是小写的）
        let set: std::collections::HashSet<String> = ["settings", "base"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let allow = Latin::Allow(set);
        assert_eq!(
            sanitize_with("在 Settings 里改 base URL", &allow),
            "在 settings 里改 base"
        );
        // 一个词都不留时，不能留下孤零零的空格
        assert_eq!(sanitize_with("API GPT", &allow), "");
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
    fn melo_keeps_english_words() {
        // melo 自己就是中英混模型：表外缩写也念得出来（实测 API/JSON/GPU 都出声），
        // 所以清洗层不查白名单，原文照留
        let out = sanitize_with("用 API 调 JSON，再传给 GPU", &Latin::Keep);
        assert!(out.contains("API"), "API 被删掉了：{out}");
        assert!(out.contains("JSON"), "JSON 被删掉了：{out}");
        assert!(out.contains("GPU"), "GPU 被删掉了：{out}");
    }

    #[test]
    fn matcha_still_drops_english() {
        // 对照：matcha 一个英文都不认，留着会念成不相干的音
        assert_eq!(sanitize("用 API 调 JSON"), "用 调");
    }

    #[test]
    fn only_melo_normalizes_volume() {
        assert_eq!(Kind::Melo.target_peak(), Some(0.85));
        assert_eq!(Kind::Matcha.target_peak(), None);
        assert_eq!(Kind::Kokoro.target_peak(), None);
    }

    #[test]
    fn sanitize_drops_symbol_islands() {
        // 路径、代码、markdown 结构留下的符号串：念出来是「冒号斜杠斜杠」
        assert_eq!(sanitize("我看了 I:/project/agent-ui 的结构"), "我看了 的结构");
        assert_eq!(sanitize("这是一个 Tauri + Vite + Vue 3 项目"), "这是一个 3 项目");
        assert_eq!(sanitize("模块 A | 模块 B"), "模块 模块");
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
