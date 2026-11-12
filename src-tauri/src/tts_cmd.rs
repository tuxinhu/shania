//! 本地语音合成的命令层。
//!
//! 前端把要念的句子发过来，这里返回 base64 编码的小端 i16 PCM 和采样率，
//! 播放交给前端的 `AudioContext`——和麦克风采集共用一个上下文，Chromium 的
//! 回声消除才能拿到参考信号（全双工的前提）。
//!
//! 为什么不在这里直接放音：Rust 侧要引入 `cpal`/`rodio`，等于在应用里再开一条
//! 音频输出链路，和 WebView 的播放设备不一定是同一个。分开之后 AEC 会失效，
//! 而且「打断」要跨进程发信号，比前端 `source.stop()` 麻烦得多。

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use shania_core::tts::{self, Engine};

/// 模型没装好：前端据此提示「去设置里下载语音模型」
pub const TTS_NOT_INSTALLED: &str = "TTS_NOT_INSTALLED";

pub struct TtsState {
    /// 用 Arc 持有：加载和推理都是同步阻塞的，必须丢进阻塞线程池跑
    engine: Arc<Engine>,
}

impl Default for TtsState {
    fn default() -> Self {
        Self {
            engine: Arc::new(Engine::new()),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress<'a> {
    stage: &'a str,
    done: u64,
    total: Option<u64>,
}

fn root_dir() -> Result<std::path::PathBuf, String> {
    crate::settings::settings_dir()
}

/// 设置里指定的模型。`auto`（或字段缺失）交给 Rust 侧自动挑。
///
/// 只作为**兜底**：前端每次调用都会显式带 `model`，因为设置写盘是防抖的，
/// 刚点完切换就来查状态时文件里还是旧值——那会导致「两个都装好了却互相说
/// 对方没装好」的死循环提示。
fn preferred() -> Option<tts::Kind> {
    let Ok(Some(raw)) = crate::settings::load_settings() else {
        return None;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("ttsModel").and_then(|s| s.as_str()).map(String::from))
        .and_then(|s| tts::Kind::from_id(&s))
}

/// 调用方显式传的模型优先，缺失才去读设置文件。
fn resolve(model: Option<String>) -> Option<tts::Kind> {
    model.as_deref().and_then(tts::Kind::from_id).or_else(preferred)
}

/// 一段合成好的语音
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Speech {
    /// base64 编码的小端 i16 单声道 PCM
    pcm: String,
    rate: u32,
    duration_ms: u32,
}

/// 合成一句话。
///
/// `sid` 是音色编号，`speed` 是语速（0.5-2.0）。两个都可省略，走默认值。
///
/// 必须是 async + spawn_blocking：同步命令在 Tauri 里跑在**主线程**上，
/// 首次加载 348MB 模型要 1-2 秒，直接写同步会把整个界面冻住。
#[tauri::command]
pub async fn tts_say(
    text: String,
    sid: Option<i32>,
    speed: Option<f32>,
    model: Option<String>,
    state: State<'_, TtsState>,
) -> Result<Speech, String> {
    if text.trim().is_empty() {
        return Err("没有可合成的内容".to_string());
    }
    let root = root_dir()?;
    if !tts::ready(&root) {
        return Err(TTS_NOT_INSTALLED.to_string());
    }

    let engine = state.engine.clone();
    let sid = sid.unwrap_or(0);
    let speed = speed.unwrap_or(1.0);
    let prefer = resolve(model);

    tauri::async_runtime::spawn_blocking(move || {
        engine.start(&root, prefer)?;
        let audio = engine.say(&text, sid, speed)?;
        Ok::<Speech, String>(Speech {
            pcm: audio.to_pcm_base64(),
            rate: audio.rate as u32,
            duration_ms: audio.duration_ms(),
        })
    })
    .await
    .map_err(|e| format!("合成任务失败：{e}"))?
}

/// 模型状态：装没装、占多少磁盘、有没有加载进内存、有哪些音色。
///
/// `model` 是前端当前的设置值（`auto` / `matcha` / `kokoro`）。必须显式传：
/// 设置写盘有 250ms 防抖，刚切换完就来查时文件里还是旧值。
#[tauri::command]
pub fn tts_status(model: Option<String>, state: State<'_, TtsState>) -> TtsStatus {
    let Ok(root) = root_dir() else {
        return TtsStatus::default();
    };
    let info = state.engine.info();
    let prefer = resolve(model);

    // 已加载的模型就是当前生效的；没加载则给出「下次会挑哪个」，让设置面板
    // 能把「你选的这个其实没生效」直接显示出来
    let active = info.map(|(_, _, k)| k).or_else(|| tts::choose(&root, prefer));

    let models: Vec<ModelStatus> = [tts::Kind::Matcha, tts::Kind::Kokoro]
        .into_iter()
        .map(|kind| ModelStatus {
            id: kind.id().to_string(),
            label: kind.label().to_string(),
            ready: tts::files_for(&root, kind).is_some(),
            bytes: tts::size(&root, kind),
            active: active == Some(kind),
        })
        .collect();

    TtsStatus {
        ready: active.is_some(),
        bytes: models.iter().map(|m| m.bytes).sum(),
        loaded: info.is_some(),
        dir: active
            .map(|k| tts::dir(&root, k).to_string_lossy().to_string())
            .unwrap_or_default(),
        speakers: info.map(|(_, n, _)| n).unwrap_or(0),
        rate: info.map(|(r, _, _)| r).unwrap_or(0),
        kind: info.map(|(_, _, k)| k.label().to_string()).unwrap_or_default(),
        active_kind: active.map(|k| k.id().to_string()).unwrap_or_default(),
        models,
    }
}

#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TtsStatus {
    ready: bool,
    bytes: u64,
    loaded: bool,
    dir: String,
    /// 音色总数。没加载模型时为 0，前端据此决定音色选择器的范围。
    speakers: i32,
    rate: i32,
    /// 当前用的模型（Matcha 单音色 / Kokoro 103 音色），没加载时为空
    kind: String,
    /// 当前生效模型的 id，`matcha` / `kokoro`
    active_kind: String,
    /// 两个模型各自装没装。设置面板按这个决定显示哪个下载按钮
    models: Vec<ModelStatus>,
}

#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    id: String,
    label: String,
    ready: bool,
    bytes: u64,
    /// 当前是否正在用
    active: bool,
}

/// 主动丢掉合成器，释放约 350MB 内存（下次合成会重新加载）
#[tauri::command]
pub fn tts_unload(state: State<'_, TtsState>) {
    state.engine.shutdown();
}

/// 提前把合成器加载好（通话开始前调用，避免第一句话等 1-2 秒）
#[tauri::command]
pub async fn tts_warmup(model: Option<String>, state: State<'_, TtsState>) -> Result<(), String> {
    let root = root_dir()?;
    if !tts::ready(&root) {
        return Err(TTS_NOT_INSTALLED.to_string());
    }
    let engine = state.engine.clone();
    let prefer = resolve(model);
    tauri::async_runtime::spawn_blocking(move || engine.start(&root, prefer))
        .await
        .map_err(|e| format!("加载失败：{e}"))?
}

/* ------------------------------------------------------------------ */
/* 安装                                                                */
/* ------------------------------------------------------------------ */

/// 下载并解压语音合成模型。
///
/// `kind` 省略时按设置里的选择，再退到 matcha。matcha 要下两个文件（72MB 模型包
/// + 54MB 声码器，声码器单独发布）；kokoro 是一个 348MB 的包。
/// 进度走 `stt-progress`，`stage` 是 `tts`。
#[tauri::command]
pub async fn tts_install(app: AppHandle, kind: Option<String>) -> Result<(), String> {
    let root = root_dir()?;
    let kind = kind
        .as_deref()
        .and_then(tts::Kind::from_id)
        .or_else(preferred)
        .unwrap_or(tts::Kind::Matcha);

    // 已经装好就别重下：348MB 不是小数目
    if tts::files_for(&root, kind).is_some() {
        return Ok(());
    }

    let dest = tts::dir(&root, kind);
    std::fs::create_dir_all(&dest).map_err(|e| format!("创建目录失败：{e}"))?;

    match kind {
        tts::Kind::Matcha => install_matcha(&app, &root, &dest).await?,
        tts::Kind::Kokoro => install_kokoro(&app, &root, &dest).await?,
    }

    if tts::files_for(&root, kind).is_none() {
        return Err("解压后没有找到完整的模型文件（model/voices/tokens/lexicon）".to_string());
    }

    let _ = app.emit(
        "stt-progress",
        Progress {
            stage: "done",
            done: 0,
            total: None,
        },
    );
    shania_core::log_info!(
        "语音合成模型安装完成：{}（{} MB，{}）",
        dest.display(),
        tts::size(&root, kind) / 1024 / 1024,
        kind.label()
    );
    Ok(())
}

async fn install_matcha(
    app: &AppHandle,
    root: &std::path::Path,
    dest: &std::path::Path,
) -> Result<(), String> {
    // 1. 模型包
    let url = tts::matcha_url();
    let archive = root.join("models").join("tts-matcha.tar.bz2");
    shania_core::log_info!("下载语音合成模型：{url}");

    let got = download_with_progress(app, &url, &archive).await?;
    if got != tts::MATCHA_BYTES {
        let _ = std::fs::remove_file(&archive);
        return Err(format!(
            "模型包大小不对（得到 {got} 字节，应为 {}），已删除，请重试",
            tts::MATCHA_BYTES
        ));
    }

    extract(archive, dest.to_path_buf()).await?;

    // 2. 声码器。不在模型包里，官方单独发布；缺了 matcha 根本出不了声
    let vocoder_path = dest.join(tts::vocoder_name());
    if !vocoder_path.is_file() {
        let vurl = tts::vocoder_url();
        shania_core::log_info!("下载声码器：{vurl}");
        let got = download_with_progress(app, &vurl, &vocoder_path).await?;
        if got != tts::VOCODER_BYTES {
            let _ = std::fs::remove_file(&vocoder_path);
            return Err(format!(
                "声码器大小不对（得到 {got} 字节，应为 {}），已删除，请重试",
                tts::VOCODER_BYTES
            ));
        }
    }
    Ok(())
}

async fn install_kokoro(
    app: &AppHandle,
    root: &std::path::Path,
    dest: &std::path::Path,
) -> Result<(), String> {
    let url = tts::kokoro_url();
    let archive = root.join("models").join("tts-kokoro.tar.bz2");
    shania_core::log_info!("下载语音合成模型：{url}");

    let got = download_with_progress(app, &url, &archive).await?;
    if got != tts::KOKORO_BYTES {
        let _ = std::fs::remove_file(&archive);
        return Err(format!(
            "模型包大小不对（得到 {got} 字节，应为 {}），已删除，请重试",
            tts::KOKORO_BYTES
        ));
    }

    extract(archive, dest.to_path_buf()).await
}

async fn download_with_progress(
    app: &AppHandle,
    url: &str,
    dest: &std::path::Path,
) -> Result<u64, String> {
    let app = app.clone();
    shania_core::stream_stt::download(url, dest, move |done, total| {
        let _ = app.emit(
            "stt-progress",
            Progress {
                stage: "tts",
                done,
                total,
            },
        );
    })
    .await
}

/// 解压放阻塞线程：348MB 的 bz2 解压要十几秒，占着 async 线程会被别的命令排队
async fn extract(archive: std::path::PathBuf, dest: std::path::PathBuf) -> Result<(), String> {
    let archive_for_task = archive.clone();
    tauri::async_runtime::spawn_blocking(move || {
        shania_core::stream_stt::extract_tar_bz2(&archive_for_task, &dest)?;
        let _ = std::fs::remove_file(&archive_for_task);
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("解压任务失败：{e}"))?
}

/// 启动后预热合成器。
///
/// 和识别模型共用「启动时预加载」开关：对用户来说都是「把语音模型读进内存」，
/// 拆成两个开关只会让人困惑。没装模型就什么都不做。
pub fn preload(app: &AppHandle) {
    use tauri::Manager;

    if !crate::stt_stream::preload_enabled() {
        return;
    }
    let Ok(root) = root_dir() else {
        return;
    };
    if !tts::ready(&root) {
        return;
    }

    let app = app.clone();
    let prefer = preferred();
    std::thread::spawn(move || {
        // 比识别模型再晚一点：启动瞬间抢 CPU 读 348MB 会让冷启动很难看
        std::thread::sleep(std::time::Duration::from_secs(4));
        let state = app.state::<TtsState>();
        if !state.engine.loaded() {
            match state.engine.start(&root, prefer) {
                Ok(()) => shania_core::log_info!("语音合成器已预加载"),
                // 预热失败不影响可用性：第一次合成时还会再试
                Err(err) => shania_core::log_warn!("语音合成器预加载失败：{err}"),
            }
        }
    });
}
