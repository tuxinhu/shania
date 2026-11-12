//! whisper 引擎的进程管理与安装入口（whisper.cpp 的 `whisper-server`）。
//!
//! **这条链路才是唯一需要「服务地址 + 端口」的那个**：它在独立子进程里跑 HTTP 服务，
//! 前端（`lib/api.ts::transcribeAudio`）按 OpenAI 兼容的 `/audio/transcriptions`
//! 把整段音频 POST 过去。另一条引擎 X-ASR（`stream_stt.rs`）在进程内推理，
//! 不监听任何端口，所以「服务端口」「识别服务地址」这两项设置对它无效。
//!
//! 两套引擎由 `settings.sttEngine` 指定：指定的那个先试，起不来就换另一个兜底
//! （`auto` 表示「X-ASR 装了就用它」）。这里只负责 whisper 这一半能力。
//!
//! 生命周期很简单：**用到才起、起一次常驻、退出时杀**。
//! 模型加载要一两秒，每次转写都重启会让「说完等半天」雪上加霜，所以进程复用。
//!
//! 端口默认 28080（和 `scripts/stt-server.ps1` 一致，方便用户 curl 调试），被占时自动顺延，
//! 真的全都占满才交给系统随机。设置里可以改这个首选值，填 0 就是每次随机。
//! 没用 8080 是因为它太容易被别的开发工具占用。

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use shania_core::stt_setup;

/// 模型加载慢的机器上（机械盘）可能要十几秒，给足余量
const READY_TIMEOUT: Duration = Duration::from_secs(120);

/// 服务端或模型没装好时返回的错误码，前端据此弹「去设置里安装」
pub const NOT_INSTALLED: &str = "STT_NOT_INSTALLED";

#[derive(Default)]
pub struct SttState {
    inner: Mutex<Inner>,
    /// 串行化「拉起服务」这个动作。
    ///
    /// 启动预热（setup 里的 `preload`）和前端第一次点麦克风（`stt_ensure`）
    /// 是两条独立的异步路径，会同时跑到 `ensure_running` 里。两边都看到
    /// 「没在跑」就各自拉起一份，抢同一个端口：输的那个 bind 失败却不退，
    /// 白占 300MB 内存，还和赢的那个抢 CPU —— 实测 VAD 处理 1.3 秒音频
    /// 从 120ms 涨到 180ms，就是这么来的。
    gate: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Inner {
    child: Option<Child>,
    port: u16,
    /// 启动时**请求**的首选端口。实际端口可能因冲突顺延，所以要分开记，
    /// 否则「设置 8080 → 实际落到 8081」会被误判成「用户改了端口」而反复重启
    desired: u16,
    /// 当前进程是用哪个后端起的：切换 CPU/GPU 时要重启才能生效
    backend: stt_setup::Backend,
}

impl SttState {
    fn record(&self, child: Child, port: u16, desired: u16, backend: stt_setup::Backend) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.child = Some(child);
            guard.port = port;
            guard.desired = desired;
            guard.backend = backend;
        }
    }

    fn port(&self) -> u16 {
        self.inner.lock().map(|g| g.port).unwrap_or(0)
    }

    /// 当前进程是按哪个首选端口拉起来的
    fn desired(&self) -> u16 {
        self.inner.lock().map(|g| g.desired).unwrap_or(0)
    }

    fn backend(&self) -> stt_setup::Backend {
        self.inner
            .lock()
            .map(|g| g.backend)
            .unwrap_or_default()
    }

    fn running(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.child.is_some() && g.port != 0)
            .unwrap_or(false)
    }

    /// 子进程还活着吗（`try_wait` 返回 `Ok(None)` 表示在跑）。
    fn alive(&self) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        guard
            .child
            .as_mut()
            .map(|c| matches!(c.try_wait(), Ok(None)))
            .unwrap_or(false)
    }

    /// 杀掉子进程。app 退出时调用——Windows 上父进程退出不会自动带走子进程。
    pub fn stop(&self) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        if let Some(mut child) = guard.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        guard.port = 0;
        guard.desired = 0;
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SttStatus {
    server_ready: bool,
    model_ready: bool,
    running: bool,
    port: u16,
    /// 已下载的模型字节数，用于前端显示「下载了一半」
    model_bytes: u64,
    model_total: u64,
    /// ffmpeg 在不在：没有它就只能收 wav（前端会自己转 wav）
    ffmpeg_ready: bool,
    /// VAD 模型在不在：没有它也能跑，只是噪声段更容易编出内容
    vad_ready: bool,
    dir: String,
    /// 当前设置偏好的后端：cpu / cuda
    backend: &'static str,
    /// 当前正在跑的进程实际用的后端（可能还没启动，为空）
    running_backend: &'static str,
    /// 机器上有没有 NVIDIA 显卡，决定要不要给用户看「启用 GPU」
    gpu_available: bool,
    /// GPU 版二进制装好没有
    cuda_ready: bool,
    /// X-ASR 流式模型装好没有（进程内推理，不需要端口）。
    /// 装好了才有资格当「主引擎」；没装就只能靠 whisper。
    stream_ready: bool,
    /// X-ASR 模型占用多少磁盘
    stream_bytes: u64,
    /// X-ASR 识别器是否已经加载进内存（第一次录音或预加载后为 true）
    stream_loaded: bool,
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

/// 语音服务日志：`~/.shania/logs/stt.log`，每次启动覆盖
fn log_file(root: &std::path::Path) -> std::path::PathBuf {
    let dir = root.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("stt.log")
}

fn emit_progress(app: &AppHandle, stage: &str, done: u64, total: Option<u64>) {
    let _ = app.emit(
        "stt-progress",
        Progress {
            stage,
            done,
            total,
        },
    );
}

/// 查一次 nvidia-smi 要几十毫秒，而状态面板会反复刷新——缓存住
static GPU_AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn gpu_available() -> bool {
    *GPU_AVAILABLE.get_or_init(stt_setup::detect_nvidia)
}

#[tauri::command]
pub fn stt_status(
    state: State<'_, SttState>,
    stream: State<'_, crate::stt_stream::StreamState>,
) -> Result<SttStatus, String> {
    let root = root_dir()?;
    let (_, _, backend) = startup_prefs();
    let (stream_ready, stream_bytes, stream_loaded) =
        crate::stt_stream::status_bits(&root, &stream);
    Ok(SttStatus {
        server_ready: stt_setup::server_ready_for(&root, backend),
        model_ready: stt_setup::model_ready(&root),
        running: state.running(),
        port: state.port(),
        model_bytes: stt_setup::model_size(&root),
        model_total: stt_setup::MODEL_BYTES,
        ffmpeg_ready: stt_setup::ffmpeg_exe(&root).is_file(),
        vad_ready: stt_setup::vad_ready(&root),
        dir: stt_setup::tools_dir_for(&root, backend)
            .to_string_lossy()
            .to_string(),
        backend: backend.as_str(),
        running_backend: if state.running() {
            state.backend().as_str()
        } else {
            ""
        },
        gpu_available: gpu_available(),
        cuda_ready: stt_setup::server_ready_for(&root, stt_setup::Backend::Cuda),
        stream_ready,
        stream_bytes,
        stream_loaded,
    })
}

/// 下载并解压服务端 + 模型。两个 stage 分别推进度，前端按 stage 显示文案。
///
/// `backend` 不传就按设置里的偏好来；GPU 版包大 70 倍，所以只在明确要求时下载。
#[tauri::command]
pub async fn stt_install(app: AppHandle, backend: Option<String>) -> Result<(), String> {
    let root = root_dir()?;
    let backend = backend
        .as_deref()
        .and_then(stt_setup::Backend::parse)
        .unwrap_or_else(|| startup_prefs().2);

    if backend == stt_setup::Backend::Cuda && !gpu_available() {
        return Err("这台机器没有检测到可用的 NVIDIA 显卡，GPU 加速用不了".to_string());
    }

    std::fs::create_dir_all(stt_setup::tools_dir_for(&root, backend))
        .map_err(|e| format!("创建目录失败：{e}"))?;
    shania_core::log_info!(
        "开始安装语音服务：后端={}，服务端={}，模型={}",
        backend.as_str(),
        stt_setup::server_ready_for(&root, backend),
        stt_setup::model_ready(&root)
    );

    // 1) 服务端
    if !stt_setup::server_ready_for(&root, backend) {
        emit_progress(&app, "server", 0, None);
        let zip_path = stt_setup::tools_dir_for(&root, backend).join("whisper-bin.zip");
        let app_progress = app.clone();
        stt_setup::download(backend.zip_url(), &zip_path, |done, total| {
            emit_progress(&app_progress, "server", done, total);
        })
        .await?;

        let tools = stt_setup::tools_dir_for(&root, backend);
        let zip_for_task = zip_path.clone();
        // 解压是同步 IO（CUDA 包解压后有 1.2GB），别卡住 async 线程
        tauri::async_runtime::spawn_blocking(move || {
            let n = stt_setup::extract_zip(&zip_for_task, &tools)?;
            let _ = std::fs::remove_file(&zip_for_task);
            Ok::<usize, String>(n)
        })
        .await
        .map_err(|e| format!("解压任务失败：{e}"))??;

        if !stt_setup::server_ready_for(&root, backend) {
            return Err(match backend {
                stt_setup::Backend::Cpu => {
                    "压缩包里没有找到 whisper-server.exe".to_string()
                }
                stt_setup::Backend::Cuda => {
                    "GPU 版解压后缺少 whisper-server.exe 或 CUDA 运行库".to_string()
                }
            });
        }
    }

    // 2) 模型
    if !stt_setup::model_ready(&root) {
        let dest = stt_setup::model_path(&root);
        emit_progress(&app, "model", 0, Some(stt_setup::MODEL_BYTES));
        let app_progress = app.clone();
        let model_url = stt_setup::model_url();
        // 下载慢/失败最常见的原因是镜像地址，记下来省得来回问
        shania_core::log_info!("下载语音模型：{model_url}");
        let got = stt_setup::download(&model_url, &dest, |done, total| {
            emit_progress(&app_progress, "model", done, total);
        })
        .await?;

        if got != stt_setup::MODEL_BYTES {
            let _ = std::fs::remove_file(&dest);
            return Err(format!(
                "模型大小不对（得到 {got} 字节，应为 {}），已删除，请重试",
                stt_setup::MODEL_BYTES
            ));
        }
    }

    // 3) VAD 模型（885KB）。它只是「增强」——没有它服务照样能跑，只是噪声里
    //    更容易编出字幕。所以下载失败不阻断安装，记一笔日志了事。
    if !stt_setup::vad_ready(&root) {
        let dest = stt_setup::vad_path(&root);
        emit_progress(&app, "vad", 0, Some(stt_setup::VAD_BYTES));
        let app_progress = app.clone();
        let vad_url = stt_setup::vad_url();
        shania_core::log_info!("下载语音活动检测模型：{vad_url}");
        let result = stt_setup::download(&vad_url, &dest, |done, total| {
            emit_progress(&app_progress, "vad", done, total);
        })
        .await;
        match result {
            Ok(got) if got == stt_setup::VAD_BYTES => {}
            Ok(got) => {
                let _ = std::fs::remove_file(&dest);
                shania_core::log_warn!(
                    "VAD 模型大小不对（得到 {got} 字节，应为 {}），已删除",
                    stt_setup::VAD_BYTES
                );
            }
            Err(err) => shania_core::log_warn!("VAD 模型下载失败，噪声段更容易编出内容：{err}"),
        }
    }

    emit_progress(&app, "done", 0, None);
    shania_core::log_info!("语音服务安装完成");
    Ok(())
}
/// 确保服务在跑，返回端口。前端转写前调它，程序启动时也会通过 [`preload`] 提前调一次。
///
/// `preferred_port` 来自设置的「服务端口」：0 或不传表示交给系统随机。
/// 端口被占时不会报错，只会自动往后顺延一个。
#[tauri::command]
pub async fn stt_ensure(
    preferred_port: Option<u16>,
    backend: Option<String>,
    state: State<'_, SttState>,
) -> Result<u16, String> {
    let wanted = preferred_port.unwrap_or(stt_setup::DEFAULT_PORT);
    let backend = backend
        .as_deref()
        .and_then(stt_setup::Backend::parse)
        .unwrap_or_else(|| startup_prefs().2);
    ensure_running(state.inner(), wanted, backend).await
}

/// 启动时预热：模型加载要几秒（机械盘上十几秒），等用户点麦克风才起就太慢了。
///
/// 只在设置的「启动时预加载」开着时才动手，没装就安静跳过，不打扰用户。
/// 失败只打日志——预热失败不该影响程序可用性，用户点麦克风时还会再试一次。
///
/// **只预热这次真正会用到的那个引擎**：X-ASR 约 500MB、whisper 约 380MB，
/// 两边都读一遍纯属浪费。引擎按 `settings.sttEngine` 取：
///   * `stream`  → 只读 X-ASR 模型（whisper 只作兜底，真要兜底时再起进程）
///   * `whisper` → 只拉 whisper 进程
///   * `auto`    → X-ASR 装了就只读它，没装才拉 whisper
pub fn preload(app: &AppHandle) {
    let (enabled, port, backend) = startup_prefs();
    if !enabled {
        return;
    }

    let engine = engine_pref();
    if engine != "whisper" {
        crate::stt_stream::preload(app);
    }

    // auto 且 X-ASR 已经在盘上：这会儿点麦克风就会走流式，whisper 只在它起不来时
    // 才会被用到，没必要让它先占着 380MB
    let stream_on_disk = root_dir()
        .map(|root| shania_core::stream_stt::ready(&root))
        .unwrap_or(false);
    if engine != "whisper" && stream_on_disk {
        return;
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<SttState>();
        match ensure_running(state.inner(), port, backend).await {
            // 成功的那条由 ensure_running 记，这里只关心失败
            Ok(_) => {}
            // 没装是最常见的情况，不算错误
            Err(err) if err == NOT_INSTALLED => {}
            Err(err) => shania_core::log_error!("whisper 服务预加载失败：{err}"),
        }
    });
}

/// 设置里指定的识别引擎：`auto` / `stream`（X-ASR）/ `whisper`。
///
/// 缺字段或拼错一律当 `auto`——老版本设置文件里没有这一项，而 auto 的行为
/// 就是「装了 X-ASR 就用它，否则 whisper」，和以前的默认表现一致。
fn engine_pref() -> String {
    let raw = crate::settings::load_settings()
        .ok()
        .flatten()
        .unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| {
            v.get("sttEngine")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "auto".to_string())
}

/// 从设置文件里读「是否预加载」「首选端口」「用哪个后端」。
///
/// 设置是前端存的原始 JSON，Rust 侧没有强类型结构，所以这里按缺省值兜底：
/// 字段缺失一律当「预加载 + 默认端口 + CPU」。
fn startup_prefs() -> (bool, u16, stt_setup::Backend) {
    let raw = crate::settings::load_settings()
        .ok()
        .flatten()
        .unwrap_or_default();
    parse_prefs(&raw)
}

/// 纯解析，便于单测：坏 JSON 或多个字段缺失都按缺省值来。
fn parse_prefs(raw: &str) -> (bool, u16, stt_setup::Backend) {
    let default = (true, stt_setup::DEFAULT_PORT, stt_setup::Backend::default());
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return default;
    };

    let enabled = value
        .get("sttPreload")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let port = value
        .get("sttPort")
        .and_then(|v| v.as_u64())
        .map(|p| p as u16)
        .unwrap_or(stt_setup::DEFAULT_PORT);
    // 设了 GPU 但机器换了（或者显卡驱动没了）时不要死循环在起不来的进程上，
    // 这里只读偏好，实际能不能起来由 ensure_running 的启动检查兜底
    let backend = value
        .get("sttBackend")
        .and_then(|v| v.as_str())
        .and_then(stt_setup::Backend::parse)
        .unwrap_or_default();

    (enabled, port, backend)
}

/// 命令和预热共用的主体逻辑。
async fn ensure_running(
    state: &SttState,
    wanted: u16,
    backend: stt_setup::Backend,
) -> Result<u16, String> {
    // 拿不到锁说明另一个调用正在拉起服务，等它完事再判断——不能并发 spawn
    let _gate = state.gate.lock().await;

    if state.running() {
        let port = state.port();
        // 设置里的端口或后端改过就重启，让改动立刻生效；
        // wanted 为 0 是「每次随机」，不该因为实际端口不同就反复重启
        let port_changed = wanted != 0 && wanted != state.desired();
        let backend_changed = backend != state.backend();
        if !port_changed
            && !backend_changed
            && stt_setup::wait_ready(port, Duration::from_secs(2)).await.is_ok()
        {
            return Ok(port);
        }
        // 端口/后端改了，或者进程在但已经不响应（被外部杀了/崩了），重来一次
        state.stop();
    }

    let root = root_dir()?;
    if !stt_setup::server_ready_for(&root, backend) || !stt_setup::model_ready(&root) {
        return Err(NOT_INSTALLED.to_string());
    }

    // 首选端口上可能已经有用户自己起的服务（`pnpm stt` 默认也是 28080），能连上就直接复用，
    // 不再拉起第二份进程（每份模型常驻约占 380MB）
    if wanted != 0 && backend == state.backend() && stt_setup::probe(wanted).await {
        shania_core::log_info!("复用已在运行的语音服务：端口 {wanted}");
        return Ok(wanted);
    }

    let port = stt_setup::pick_port(wanted)?;
    let exe = stt_setup::server_exe_for(&root, backend);
    let log_path = log_file(&root);

    let child = spawn_server(&exe, &root, port, backend, &log_path)?;
    state.record(child, port, wanted, backend);

    if let Err(err) = stt_setup::wait_ready_with(port, READY_TIMEOUT, || state.alive()).await {
        state.stop();
        return Err(format!("{err}（日志：{}）", log_path.display()));
    }

    shania_core::log_info!(
        "语音服务已启动：http://127.0.0.1:{port}/v1（后端 {}，模型 {}，{} 线程，输出见 {}）",
        match backend {
            stt_setup::Backend::Cpu => "CPU",
            stt_setup::Backend::Cuda => "CUDA",
        },
        stt_setup::model_path(&root).display(),
        thread_count(),
        log_path.display()
    );
    Ok(port)
}

/// 拼参数并拉起服务进程。
///
/// 这里刻意**没有**传 `-sns` / `-nth`：实测下来（3 秒静音、白噪声、440Hz 提示音各跑三轮）
/// 这两个开关对中文幻觉毫无作用，纯静音照样输出「你不要再說了，我會把你送到你家裡的房間」，
/// 而 `-nth 0.7` 反而会误杀真实语音（真实一句的 no_speech_prob 也能到 0.695）。
/// 幻觉只能靠「不把非语音送进来」（前端 VAD）和「输出后过一遍清洗」两头堵。
fn spawn_server(
    exe: &std::path::Path,
    root: &std::path::Path,
    port: u16,
    backend: stt_setup::Backend,
    log_path: &std::path::Path,
) -> Result<Child, String> {
    let mut cmd = Command::new(exe);
    cmd.current_dir(exe.parent().unwrap_or(root))
        .arg("--model")
        .arg(stt_setup::model_path(root))
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        // 默认端点是 /inference，改成 OpenAI 兼容路径前端才能直接用
        .arg("--inference-path")
        .arg("/v1/audio/transcriptions")
        .arg("--language")
        .arg(server_language())
        .arg("--threads")
        .arg(thread_count().to_string());

    // CPU 上最大的开销是 encoder：默认音频会被补零到 30 秒再编码，一句 4 秒的话
    // 也要按 30 秒算。`-ac` 把注意力上下文缩到 9 秒，实测 4.5s → 1.4s。
    // 前端已经把每句切在 8 秒内（pcm-recorder 的 maxSegmentMs），所以不会截断。
    // GPU 上编码只要 0.3 秒，没必要冒这个风险，保持完整上下文更准。
    if backend == stt_setup::Backend::Cpu {
        cmd.arg("-ac").arg("450");
    }

    // VAD：在推理**之前**把非语音段切掉。这是治「噪声里编出字幕」的那一刀——
    // 实测 3 秒静音、白噪声、440Hz 提示音三者开 VAD 后全部输出空，
    // 而真实语音的识别内容一个字都不差。模型只有 885KB，没装就跳过。
    if stt_setup::vad_ready(root) {
        cmd.arg("--vad").arg("-vm").arg(stt_setup::vad_path(root));
    }

    // 前端已经把录音转成 wav 了，--convert 只是兜底（别人直接发 webm 时能用）。
    // 关键是：加了 --convert，whisper-server 启动时就要在 PATH 里找 ffmpeg，
    // 找不到会打印 "ffmpeg is not found" 然后当场退出——所以必须同时把目录塞进 PATH。
    if stt_setup::ffmpeg_exe(root).is_file() {
        cmd.arg("--convert");
        if let Some(dir) = stt_setup::ffmpeg_exe(root).parent() {
            let mut dirs = vec![dir.to_path_buf()];
            if let Some(old) = std::env::var_os("PATH") {
                dirs.extend(std::env::split_paths(&old));
            }
            if let Ok(joined) = std::env::join_paths(dirs) {
                cmd.env("PATH", joined);
            }
        }
    }

    // 输出落盘而不是丢进 null：起不来的时候（缺 DLL、模型坏了、ffmpeg 找不到）
    // 只有这份日志能说明原因，否则界面上只会看到「没就绪」
    let log = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)
        .ok();
    let err_log = log.as_ref().and_then(|f| f.try_clone().ok());

    cmd.stdin(Stdio::null());
    if let Some(file) = log {
        cmd.stdout(Stdio::from(file));
    } else {
        cmd.stdout(Stdio::null());
    }
    if let Some(file) = err_log {
        cmd.stderr(Stdio::from(file));
    } else {
        cmd.stderr(Stdio::null());
    }

    cmd.spawn()
        .map_err(|e| format!("启动语音服务失败（{}）：{e}", exe.display()))
}

#[tauri::command]
pub fn stt_stop(state: State<'_, SttState>) {
    if state.running() {
        shania_core::log_info!("手动停止语音服务（端口 {}）", state.port());
    }
    state.stop();
}

/// whisper 的线程数按**物理核**给最划算，超线程反而更慢
fn thread_count() -> usize {
    let logical = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (logical / 2).max(2)
}

/// 服务进程默认语言：跟设置里的「识别语言」走，留空才交给自动判断。
///
/// 别看请求里每次都带 language，`--language` 是 `curl` 直接打服务、
/// 或者前端没填时的兜底；两处不一致时按请求为准。
fn server_language() -> String {
    let raw = crate::settings::load_settings()
        .ok()
        .flatten()
        .unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| {
            v.get("sttLanguage")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "auto".to_string())
}

/// app 退出时统一清理
pub fn shutdown(app: &AppHandle) {
    if let Some(state) = app.try_state::<SttState>() {
        state.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_preload_and_port_from_settings() {
        let (enabled, port, backend) =
            parse_prefs(r#"{"sttPreload":false,"sttPort":28081,"sttBackend":"cuda"}"#);
        assert!(!enabled);
        assert_eq!(port, 28081);
        assert_eq!(backend, stt_setup::Backend::Cuda);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // 老版本设置文件里没有这些字段：应当预加载 + 默认端口 + CPU
        let (enabled, port, backend) = parse_prefs(r#"{"baseUrl":"http://127.0.0.1:4000"}"#);
        assert!(enabled);
        assert_eq!(port, stt_setup::DEFAULT_PORT);
        assert_eq!(backend, stt_setup::Backend::Cpu);
    }

    #[test]
    fn broken_json_is_treated_as_defaults() {
        let (enabled, port, backend) = parse_prefs("{ 这不是 JSON");
        assert!(enabled);
        assert_eq!(port, stt_setup::DEFAULT_PORT);
        assert_eq!(backend, stt_setup::Backend::Cpu);
    }

    #[test]
    fn unknown_backend_string_falls_back_to_cpu() {
        // 前端写了拼错的值（或者是个我们不认识的新后端）时不能起不来
        let (_, _, backend) = parse_prefs(r#"{"sttBackend":"vulkan"}"#);
        assert_eq!(backend, stt_setup::Backend::Cpu);
    }
}
