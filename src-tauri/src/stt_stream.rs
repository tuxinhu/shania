//! 流式语音识别（X-ASR 160ms）的命令层。
//!
//! 前端把麦克风的 16k float32 采样按 100ms 一块编码成 base64 发过来，
//! 这里解成采样喂给 [`shania_core::stream_stt::Engine`]，返回「当前整句」和
//! 「这一轮结束的句子」。识别器进程内常驻，模型只读一次盘。
//!
//! 注意这条链路**不监听端口、不发 HTTP**：麦克风数据经 Tauri IPC 直接进进程。
//! 这也是它取代 whisper-server 的主要原因之一——不用管端口占用，不用管子进程退出。
//!
//! 为什么不用 event 往前推：一次 feed 就是一次请求-响应，前端天然知道
//! 「这批音频处理完了、文本是这个」，不用维护事件生命周期，也不用担心
//! 上一轮的迟到事件盖住新会话。代价是一次 IPC 的往返（实测 0.1ms 级）。

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, State};

use shania_core::stream_stt::{self, Engine, Feed};

/// 模型没装好：前端据此提示「去设置里下载模型」
pub const STREAM_NOT_INSTALLED: &str = "STREAM_NOT_INSTALLED";

pub struct StreamState {
    /// 用 Arc 持有：加载模型和推理都是同步阻塞的几毫秒到几秒，
    /// 必须丢进阻塞线程池跑（见各命令里的 spawn_blocking），而阻塞任务要求 'static。
    engine: Arc<Engine>,
    /// 当前有效会话 id。前端每次开始录音生成一个新的；换会话之后，
    /// 上一轮还在路上的 feed 会因为 id 对不上被直接丢掉，不会把字串进去。
    session: Mutex<String>,
}

impl Default for StreamState {
    fn default() -> Self {
        Self {
            engine: Arc::new(Engine::new()),
            session: Mutex::new(String::new()),
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

/// 开始一次识别会话。模型已加载时只是重置 stream，不读盘。
///
/// `silence` 是断句阈值（秒），省略时用聊天输入的 0.9；通话传 0.5。
/// 阈值变了会重建识别器（1-2 秒），所以同一个功能内别来回改。
///
/// 必须是 async + spawn_blocking：同步命令在 Tauri 里跑在**主线程**上，
/// 首次加载模型要 2-5 秒，直接写同步会把整个界面冻住。
#[tauri::command]
pub async fn stt_stream_start(
    session_id: String,
    silence: Option<f32>,
    state: State<'_, StreamState>,
) -> Result<(), String> {
    let root = root_dir()?;
    if !stream_stt::ready(&root) {
        return Err(STREAM_NOT_INSTALLED.to_string());
    }

    // 会话 id 先清掉再启动：启动失败时不该留下一个「能被 feed」的旧 id
    if let Ok(mut guard) = state.session.lock() {
        guard.clear();
    }

    let engine = state.engine.clone();
    let silence = silence.unwrap_or(stream_stt::CHAT_SILENCE);
    tauri::async_runtime::spawn_blocking(move || engine.start_with(&root, silence))
        .await
        .map_err(|e| format!("启动识别器失败：{e}"))??;

    if let Ok(mut guard) = state.session.lock() {
        *guard = session_id;
    }
    Ok(())
}

/// 喂一段音频。`pcm` 是小端 float32 的 base64。
///
/// 同样走 spawn_blocking：一次 100ms 音频推理约 20ms，留在主线程会明显掉帧
/// （每秒十次、每次 20ms，等于主线程 20% 的时间在算语音）。
#[tauri::command]
pub async fn stt_stream_feed(
    session_id: String,
    pcm: String,
    state: State<'_, StreamState>,
) -> Result<Feed, String> {
    if !is_current(&state, &session_id) {
        return Ok(Feed::default());
    }
    // 一段 15 秒的音频也只有 960KB，超过 2MB 基本是传错了数据
    if pcm.len() > 2 * 1024 * 1024 {
        return Err(format!("单次音频过大（{} 字节）", pcm.len()));
    }
    let samples = stream_stt::decode_pcm(&pcm)?;
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.feed(&samples))
        .await
        .map_err(|e| format!("推理失败：{e}"))?
}

/// 结束会话：把尾部没定稿的部分冲出来，并作废这个 session id。
#[tauri::command]
pub async fn stt_stream_stop(
    session_id: String,
    state: State<'_, StreamState>,
) -> Result<Feed, String> {
    if !is_current(&state, &session_id) {
        return Ok(Feed::default());
    }
    if let Ok(mut guard) = state.session.lock() {
        guard.clear();
    }
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.finish())
        .await
        .map_err(|e| format!("收尾失败：{e}"))?
}

/// 主动丢掉识别器（排查内存占用时用；下次录音会重新加载）
#[tauri::command]
pub fn stt_stream_unload(state: State<'_, StreamState>) {
    state.engine.shutdown();
}

fn is_current(state: &StreamState, session_id: &str) -> bool {
    state
        .session
        .lock()
        .map(|g| !g.is_empty() && g.as_str() == session_id)
        .unwrap_or(false)
}

/* ------------------------------------------------------------------ */
/* 安装                                                                */
/* ------------------------------------------------------------------ */

/// 下载并解压流式模型（134MB）。进度走 `stt-progress`，`stage` 是 `stream`。
#[tauri::command]
pub async fn stt_stream_install(app: AppHandle) -> Result<(), String> {
    use tauri::Emitter;

    let root = root_dir()?;
    if stream_stt::ready(&root) {
        return Ok(());
    }

    let dest = stream_stt::dir(&root);
    std::fs::create_dir_all(&dest).map_err(|e| format!("创建目录失败：{e}"))?;

    let url = stream_stt::model_url();
    let archive = root.join("models").join("stream-model.tar.bz2");
    shania_core::log_info!("下载流式识别模型：{url}");

    let app_progress = app.clone();
    let got = stream_stt::download(&url, &archive, |done, total| {
        let _ = app_progress.emit(
            "stt-progress",
            Progress {
                stage: "stream",
                done,
                total,
            },
        );
    })
    .await?;

    // 官方包的大小是固定的；对不上说明下到了错误页或者半截文件，
    // 这种情况下解压出来的模型能读但跑不起来，排查起来非常费劲，不如直接删掉重来
    if got != stream_stt::MODEL_BYTES {
        let _ = std::fs::remove_file(&archive);
        return Err(format!(
            "模型包大小不对（得到 {got} 字节，应为 {}），已删除，请重试",
            stream_stt::MODEL_BYTES
        ));
    }

    // 解压 134MB 压缩包 → 约 200MB 文件，同步 IO，别卡 async 线程
    let archive_for_task = archive.clone();
    let dest_for_task = dest.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let n = stream_stt::extract_tar_bz2(&archive_for_task, &dest_for_task)?;
        let _ = std::fs::remove_file(&archive_for_task);
        Ok::<usize, String>(n)
    })
    .await
    .map_err(|e| format!("解压任务失败：{e}"))??;

    if !stream_stt::ready(&root) {
        return Err("解压后没有找到完整的模型文件（encoder/decoder/joiner/tokens）".to_string());
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
        "流式识别模型安装完成：{}（{} MB）",
        dest.display(),
        stream_stt::size(&root) / 1024 / 1024
    );
    Ok(())
}

/// 模型状态：装没装、占多少磁盘、有没有加载进内存。
///
/// 设置面板只关心这几件事，所以单独开一个命令。以前这些字段混在一个
/// 包含 whisper 服务端/端口/后端的大状态结构里，那半边已经全删了。
#[tauri::command]
pub fn stt_stream_status(state: State<'_, StreamState>) -> StreamStatus {
    let Ok(root) = root_dir() else {
        return StreamStatus::default();
    };
    StreamStatus {
        ready: stream_stt::ready(&root),
        bytes: stream_stt::size(&root),
        loaded: state.engine.loaded(),
        dir: stream_stt::dir(&root).to_string_lossy().to_string(),
    }
}

#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StreamStatus {
    /// 四个模型文件是否齐全
    ready: bool,
    /// 磁盘占用
    bytes: u64,
    /// 识别器是否已加载进内存
    loaded: bool,
    /// 模型目录，设置面板上显示给用户看
    dir: String,
}

/// 启动后把 X-ASR 模型读进内存。
///
/// 模型加载实测 2-5 秒（debug 5 秒、release 约 2 秒），等用户点麦克风才开始读，
/// 就又变回「点了半天没反应」——那正是换 X-ASR 要解决的问题本身。
///
/// 刻意延后 2 秒、并且用独立线程：启动瞬间前端在渲染、窗口在画，这时候抢 CPU
/// 读 134MB 只会让冷启动更难看。用 std::thread 而不是 tokio 任务，是因为加载
/// 本身是阻塞 IO + 同步推理初始化，放进 async 池会占掉一个 worker。
pub fn preload(app: &AppHandle) {
    use tauri::Manager;

    // 设置里关掉了「启动时预加载」就不预热，等用户点麦克风时再读盘
    if !preload_enabled() {
        return;
    }

    let Ok(root) = root_dir() else {
        return;
    };
    // 没装就什么都不做，不能因为语音模型没下载就影响启动
    if !stream_stt::ready(&root) {
        return;
    }

    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let state = app.state::<StreamState>();
        // 用户手快，这期间已经点过麦克风并加载完了，别重复读一遍盘
        if !state.engine.loaded() {
            match state.engine.start(&root) {
                Ok(()) => shania_core::log_info!("X-ASR 流式识别模型已预加载"),
                // 预加载失败不影响可用性：点麦克风时还会再试一次
                Err(err) => shania_core::log_warn!("X-ASR 流式识别模型预加载失败：{err}"),
            }
        }
    });
}

/// 「启动时预加载」开关。缺字段按开处理——老设置文件里没有这一项，
/// 而它的默认行为一直是开机就加载。
///
/// 语音合成也读这个开关：对用户来说都是「把语音模型读进内存」，
/// 拆成两个只会让人困惑。
pub(crate) fn preload_enabled() -> bool {
    let Ok(Some(raw)) = crate::settings::load_settings() else {
        return true;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("sttPreload").and_then(|b| b.as_bool()))
        .unwrap_or(true)
}
