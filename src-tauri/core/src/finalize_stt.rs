//! 定稿纠错：SenseVoice-Small（非流式，整句重听）。
//!
//! 和 [`crate::stream_stt`] 的分工：
//!
//! | | 流式 X-ASR（stream_stt） | 这里（SenseVoice） |
//! |---|---|---|
//! | 干什么 | 说话过程中逐字出结果 | 每句结束时把整句重听一遍，替换掉草稿 |
//! | 延迟 | 首字约 0.2s | 一次 6 秒音频约 250ms（RTF 0.034） |
//! | 输入 | 160ms 一块，持续喂 | 一整句，一次解完 |
//! | 标点 | 自带（`punct` 版） | 自带，但**挂在 `use_itn` 上**，见下 |
//!
//! 两个 2026-11 的实测结论决定了这里的写法：
//!
//! 1. **int8 和 fp32 输出逐字相同。** 三段中文样本、同一套 tokens，只换 onnx 精度，
//!    结果一个字都没差，而 int8 还快 20-40%、加载快 0.8 秒。所以只认 `*.int8.onnx`，
//!    fp32 那 894MB 是纯浪费（下载包要选 `-int8-` 那个，155MB，不是含 fp32 的 999MB 包）。
//! 2. **标点靠 `use_itn`，不是无条件自带。** 同一个模型：
//!    - `use_itn = true` → 有全角标点，但「三点」会写成「3点」
//!    - `use_itn = false` → 完全没有标点，数字保持中文
//!    标点对可用性的影响远大于「三 / 3」的差别（聊天框里后者甚至更自然），所以取 `true`。
//!    万一某类句子漏标点，前端还有 `lib/punctuate.ts` 那层启发式兜底。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig};

/// int8 专用包的大小。下载完对不上就当失败——和流式模型那边同一个理由：
/// 中断的下载会留下一个能读但跑不起来的半个文件，排查起来非常费劲。
pub const MODEL_BYTES: u64 = 163_002_883;

/// 目录名带模型版本，换模型时新旧目录不会混
pub const DIR_NAME: &str = "sense-voice-zh-en-ja-ko-yue-2024-07-17";

/// 刻意用 int8 专用包：资源包里还有一个含 fp32 + int8 的 999MB 版本，
/// 多下 845MB 换来一个实测输出完全相同的文件，没有意义。
const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2";

/// 允许用镜像覆盖（国内直连 GitHub 经常只有几十 KB/s）
pub fn model_url() -> String {
    std::env::var("SHANIA_SENSE_STT_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| MODEL_URL.to_string())
}

pub fn dir(root: &Path) -> PathBuf {
    root.join("models").join(DIR_NAME)
}

/// 真正要喂给识别器的两个文件
pub struct Files {
    pub model: PathBuf,
    pub tokens: PathBuf,
}

/// 按关键词扫目录找文件。
///
/// 优先 `int8`：包里同时有 `model.onnx` 和 `model.int8.onnx` 时（有些版本会两个都放），
/// 挑 int8 那个——实测输出一样、速度快 20-40%、体积小 4 倍。
pub fn files(root: &Path) -> Option<Files> {
    let mut all = Vec::new();
    crate::stream_stt::scan(&dir(root), &mut all, 0);
    if all.is_empty() {
        return None;
    }
    let model = crate::stream_stt::pick(&all, "int8", ".onnx")
        .or_else(|| crate::stream_stt::pick(&all, "model", ".onnx"))?;
    Some(Files {
        model,
        tokens: crate::stream_stt::pick(&all, "tokens", ".txt")?,
    })
}

pub fn ready(root: &Path) -> bool {
    files(root).is_some()
}

/// 已占用的磁盘空间，给设置面板显示
pub fn size(root: &Path) -> u64 {
    let mut all = Vec::new();
    crate::stream_stt::scan(&dir(root), &mut all, 0);
    all.iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// 识别器常驻容器。
///
/// 加载一次约 1.6 秒（228MB 权重），每句定稿都重建完全不能接受，所以进程内只建一次。
/// `Mutex<Option<_>>` 而不是 `OnceLock`：卸载、换模型时能重建。
static RECOGNIZER: Mutex<Option<OfflineRecognizer>> = Mutex::new(None);

pub fn loaded() -> bool {
    RECOGNIZER.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// 丢掉识别器，把内存还给系统
pub fn unload() {
    if let Ok(mut guard) = RECOGNIZER.lock() {
        *guard = None;
    }
}

/// 拿识别器，没建过就建一个。
///
/// 单独拆出来是为了让「预加载」和「真正用」走同一条路：启动后预加载一次，
/// 用户说第一句话时就不用再等那 1.6 秒（否则第一次定稿会明显卡一下）。
fn ensure(root: &Path) -> Result<std::sync::MutexGuard<'static, Option<OfflineRecognizer>>, String> {
    let mut guard = RECOGNIZER
        .lock()
        .map_err(|_| "定稿识别器状态锁失效".to_string())?;
    if guard.is_none() {
        let files = files(root).ok_or_else(|| "定稿纠错模型还没装好".to_string())?;
        let began = Instant::now();
        let recognizer = build(&files).ok_or_else(|| {
            "创建定稿识别器失败：模型文件和版本不匹配（日志里通常有 model 元信息）".to_string()
        })?;
        crate::log_info!(
            "定稿纠错模型就绪：{}ms，{} 线程，{}",
            began.elapsed().as_millis(),
            crate::stream_stt::thread_count(),
            files.model.display()
        );
        *guard = Some(recognizer);
    }
    Ok(guard)
}

/// 只加载不推理，给启动预加载用
pub fn preload(root: &Path) -> Result<(), String> {
    ensure(root).map(|_| ())
}

fn build(files: &Files) -> Option<OfflineRecognizer> {
    let config = OfflineRecognizerConfig {
        model_config: sherpa_onnx::OfflineModelConfig {
            sense_voice: OfflineSenseVoiceModelConfig {
                model: Some(files.model.to_string_lossy().to_string()),
                language: Some("zh".to_string()),
                // 标点就是这个开关带出来的，关掉之后标点会全部消失（见模块注释）
                use_itn: true,
            },
            tokens: Some(files.tokens.to_string_lossy().to_string()),
            // 非流式整句解码，线程数不用像流式那样抠：实测 4 线程 RTF 0.034，
            // 已经比实时快 30 倍，再往上堆收益很小
            num_threads: crate::stream_stt::thread_count(),
            provider: Some("cpu".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    OfflineRecognizer::create(&config)
}

/// 尾部补的静音。
///
/// SenseVoice 虽然是一次解完整段，但对「最后一个字刚好压在音频末尾」同样不友好
/// （实测脚本里一直保留这 0.3 秒），补一点静音让最后一块卷积的右侧上下文有东西可看。
const TAIL_PAD: usize = 4_800; // 0.3s @16k

/// 把一整句音频重听一遍，返回定稿文本。空音频返回空串，不报错。
///
/// 调用方要保证 `samples` 是 16k 单声道 float32，且长度不超过一句
/// （流式那边 `MAX_UTTERANCE_MS` 硬断在 15 秒）。
pub fn finalize(root: &Path, samples: &[f32]) -> Result<String, String> {
    if samples.is_empty() {
        return Ok(String::new());
    }

    let guard = ensure(root)?;
    let Some(recognizer) = guard.as_ref() else {
        return Ok(String::new());
    };

    let stream = recognizer.create_stream();
    stream.accept_waveform(16_000, samples);
    stream.accept_waveform(16_000, &vec![0.0f32; TAIL_PAD]);
    recognizer.decode(&stream);

    let text = stream
        .get_result()
        .map(|r| clean(&r.text))
        .unwrap_or_default();
    Ok(text)
}

/// 输出过一遍后处理：繁转简 + 标点规范（与流式那边共用 [`crate::stream_stt`] 的口径）。
///
/// 刻意**不**跑 `transcribe::clean_transcript` 那套按句丢幻听的规则：SenseVoice
/// 不会编 `(字幕:xxx)`，按句删反而会把正常内容删掉。
fn clean(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let simplified = crate::simplify::to_simplified(trimmed);
    crate::transcribe::tidy(&simplified)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shania-sense-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn prefers_int8_over_fp32() {
        let root = tmp("int8");
        let sub = dir(&root).join("pkg");
        fs::create_dir_all(&sub).unwrap();
        for name in ["model.onnx", "model.int8.onnx", "tokens.txt"] {
            fs::write(sub.join(name), b"x").unwrap();
        }

        let files = files(&root).expect("应该找到模型");
        assert!(files.model.ends_with("model.int8.onnx"), "必须优先 int8");
        assert!(files.tokens.ends_with("tokens.txt"));
        assert!(ready(&root));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn falls_back_to_fp32_when_int8_missing() {
        let root = tmp("fp32");
        let sub = dir(&root);
        fs::create_dir_all(&sub).unwrap();
        for name in ["model.onnx", "tokens.txt"] {
            fs::write(sub.join(name), b"x").unwrap();
        }

        let files = files(&root).expect("没有 int8 时应该退回 fp32");
        assert!(files.model.ends_with("model.onnx"));

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
    fn empty_audio_is_empty_text() {
        let root = tmp("silence");
        // 没装模型也不该因为空音频而报错：录音一句没说时走的就是这条路
        assert_eq!(finalize(&root, &[]).unwrap(), "");
        let _ = fs::remove_dir_all(&root);
    }
}
