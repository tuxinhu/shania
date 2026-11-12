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
    speed: Option<f32>,
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
    let speed = speed.unwrap_or(1.0);

    tauri::async_runtime::spawn_blocking(move || {
        engine.start(&root)?;
        let audio = engine.say(&text, 0, speed)?;
        Ok::<Speech, String>(Speech {
            pcm: audio.to_pcm_base64(),
            rate: audio.rate as u32,
            duration_ms: audio.duration_ms(),
        })
    })
    .await
    .map_err(|e| format!("合成任务失败：{e}"))?
}

/// 模型状态：装没装、占多少磁盘、有没有加载进内存。
#[tauri::command]
pub fn tts_status(state: State<'_, TtsState>) -> TtsStatus {
    let Ok(root) = root_dir() else {
        return TtsStatus::default();
    };
    let info = state.engine.info();

    TtsStatus {
        ready: tts::ready(&root),
        bytes: tts::size(&root),
        loaded: info.is_some(),
        dir: tts::dir(&root).to_string_lossy().to_string(),
        speakers: info.map(|(_, n)| n).unwrap_or(0),
        rate: info.map(|(r, _)| r).unwrap_or(0),
    }
}

#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TtsStatus {
    ready: bool,
    bytes: u64,
    loaded: bool,
    dir: String,
    /// 音色总数。没加载模型时为 0。
    speakers: i32,
    rate: i32,
}

/// 主动丢掉合成器，释放约 350MB 内存（下次合成会重新加载）
#[tauri::command]
pub fn tts_unload(state: State<'_, TtsState>) {
    state.engine.shutdown();
}

/// 提前把合成器加载好（通话开始前调用，避免第一句话等 1-2 秒）
#[tauri::command]
pub async fn tts_warmup(state: State<'_, TtsState>) -> Result<(), String> {
    let root = root_dir()?;
    if !tts::ready(&root) {
        return Err(TTS_NOT_INSTALLED.to_string());
    }
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.start(&root))
        .await
        .map_err(|e| format!("加载失败：{e}"))?
}

/* ------------------------------------------------------------------ */
/* 安装                                                                */
/* ------------------------------------------------------------------ */

/// 下载并解压语音合成模型（163MB 的包，含声学模型 + 词表 + FST）。
///
/// 已经装好就直接返回，不重下。进度走 `stt-progress`，`stage` 是 `tts`。
#[tauri::command]
pub async fn tts_install(app: AppHandle) -> Result<(), String> {
    let root = root_dir()?;

    // 已经装好就别重下：163MB 不是小数目
    if tts::files(&root).is_some() {
        return Ok(());
    }

    let dest = tts::dir(&root);
    std::fs::create_dir_all(&dest).map_err(|e| format!("创建目录失败：{e}"))?;

    let url = tts::melo_url();
    let archive = root.join("models").join("tts-melo.tar.bz2");
    shania_core::log_info!("下载语音合成模型：{url}");

    // 校验字节数：GitHub 的连接经常在 120-160MB 之间断一次，而下载器不支持
    // 续传，断了就是半包——不校验会解压失败，报一个看不出原因的错
    let got = download_with_progress(&app, &url, &archive).await?;
    if got != tts::MELO_BYTES {
        let _ = std::fs::remove_file(&archive);
        return Err(format!(
            "模型包大小不对（得到 {got} 字节，应为 {}），已删除，请重试",
            tts::MELO_BYTES
        ));
    }

    extract(archive, dest.clone()).await?;

    if tts::files(&root).is_none() {
        return Err("解压后没有找到完整的模型文件（model/lexicon/tokens）".to_string());
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
        "语音合成模型安装完成：{}（{} MB）",
        dest.display(),
        tts::size(&root) / 1024 / 1024
    );
    Ok(())
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
    std::thread::spawn(move || {
        // 比识别模型再晚一点：启动瞬间抢 CPU 读 348MB 会让冷启动很难看
        std::thread::sleep(std::time::Duration::from_secs(4));
        let state = app.state::<TtsState>();
        if !state.engine.loaded() {
            match state.engine.start(&root) {
                Ok(()) => shania_core::log_info!("语音合成器已预加载"),
                // 预热失败不影响可用性：第一次合成时还会再试
                Err(err) => shania_core::log_warn!("语音合成器预加载失败：{err}"),
            }
        }
    });
}
