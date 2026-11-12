//! whisper 引擎（whisper.cpp 的 whisper-server）的安装辅助。
//!
//! 只管 HTTP 那套引擎：下载预编译包、下载 ggml 模型、等端口就绪。
//! X-ASR 那套的资源（`.tar.bz2` 解压、模型目录、镜像 URL）在 `stream_stt.rs`；
//! 两边共用的只有本文件里的 `download`。
//!
//! 分发时安装包不带 190MB 的模型，也不强制用户自己编译：这里负责把官方
//! Windows 预编译包和模型下载到 `~/.shania` 下，之后由桌面端自己拉起进程。
//!
//! 刻意只放**不依赖 Tauri** 的东西（路径推导、下载、解压、等健康检查），
//! 进程管理留在 `src-tauri/src/stt.rs`——这样 CLI 将来也能复用同一套安装逻辑。

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::StreamExt;

/// whisper.cpp 的构建 tag。v1.9.x 的 release 不附二进制，但每个 build 都有预编译包。
pub const SERVER_BUILD: &str = "b5454";
pub const SERVER_ZIP_URL: &str =
    "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip";

/// CUDA 构建包：自带 cudart/cublas 的 DLL，用户不用装 CUDA Toolkit。
///
/// 代价是 653MB 压缩 / 1.2GB 解压——比 CPU 包大 70 倍，所以只在用户主动
/// 勾选「GPU 加速」且机器上确实有 NVIDIA 显卡时才下。实测 8 秒音频从
/// 1433ms 降到约 300ms，值得为一台带独显的机器付这个体积。
pub const CUDA_ZIP_URL: &str =
    "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-win-cuda-12.4.0-x64.zip";

/// 推理后端：同一份模型，换不同的 whisper-server 可执行文件
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    #[default]
    Cpu,
    Cuda,
}

impl Backend {
    pub fn parse(value: &str) -> Option<Backend> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cpu" => Some(Backend::Cpu),
            "cuda" | "gpu" => Some(Backend::Cuda),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Cpu => "cpu",
            Backend::Cuda => "cuda",
        }
    }

    /// 两种后端各占一个目录，切回 CPU 时不用重装
    pub fn dir_name(self) -> &'static str {
        match self {
            Backend::Cpu => "whisper",
            Backend::Cuda => "whisper-cuda",
        }
    }

    pub fn zip_url(self) -> &'static str {
        match self {
            Backend::Cpu => SERVER_ZIP_URL,
            Backend::Cuda => CUDA_ZIP_URL,
        }
    }

    /// 下载体积提示用（前端也有一份，这里给 CLI 和日志用）
    pub fn approx_bytes(self) -> u64 {
        match self {
            Backend::Cpu => 8_900_000,
            Backend::Cuda => 684_913_404,
        }
    }
}

pub const MODEL_NAME: &str = "ggml-small-q5_1.bin";
/// 允许用环境变量指向镜像，HuggingFace 直连在国内经常只有几十 KB/s
pub const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin";

/// 模型下载地址：`SHANIA_STT_MODEL_URL` 可以覆盖成镜像或内网地址
pub fn model_url() -> String {
    std::env::var("SHANIA_STT_MODEL_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| MODEL_URL.to_string())
}
/// 用来校验下载完整性，防止半截文件被当成可用模型
pub const MODEL_BYTES: u64 = 190_085_487;

/// silero VAD 模型，只有 885KB，但决定了「会不会编内容」。
///
/// whisper 遇到噪声、静音、纯音调时会「回忆」出训练语料里的东西——
/// 3 秒静音喂进去会输出「你不要再說了，我會把你送到你家裡的房間」，
/// 440Hz 提示音会输出「(字幕製作:貝爾)」。服务端参数压不住（`-sns`、
/// `-nth`、`-mc` 全试过，无效），唯一有效的是让 VAD 在推理**之前**
/// 把非语音段切掉：实测静音/白噪声/提示音全部输出空，真实语音一个字不差。
pub const VAD_NAME: &str = "ggml-silero-v6.2.0.bin";
pub const VAD_URL: &str =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin";
pub const VAD_BYTES: u64 = 885_098;

/// VAD 模型地址：`SHANIA_STT_VAD_URL` 可以覆盖成镜像
pub fn vad_url() -> String {
    std::env::var("SHANIA_STT_VAD_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| VAD_URL.to_string())
}

pub fn tools_dir(root: &Path) -> PathBuf {
    root.join("tools").join("whisper")
}

/// 指定后端放二进制的目录
pub fn tools_dir_for(root: &Path, backend: Backend) -> PathBuf {
    root.join("tools").join(backend.dir_name())
}

pub fn models_dir(root: &Path) -> PathBuf {
    root.join("models")
}

/// 解压时保留 `Release/` 这一层，exe 必须和它那堆 ggml-*.dll 待在一起
pub fn server_exe(root: &Path) -> PathBuf {
    server_exe_for(root, Backend::Cpu)
}

pub fn server_exe_for(root: &Path, backend: Backend) -> PathBuf {
    tools_dir_for(root, backend)
        .join("Release")
        .join("whisper-server.exe")
}

pub fn model_path(root: &Path) -> PathBuf {
    models_dir(root).join(MODEL_NAME)
}

/// VAD 和主模型放一起，跟后端无关（CPU/GPU 共用一份）
pub fn vad_path(root: &Path) -> PathBuf {
    models_dir(root).join(VAD_NAME)
}

pub fn ffmpeg_exe(root: &Path) -> PathBuf {
    root.join("tools").join("ffmpeg").join("bin").join("ffmpeg.exe")
}

pub fn server_ready(root: &Path) -> bool {
    server_ready_for(root, Backend::Cpu)
}

/// CUDA 版还要求 cudart 在：只拷贝 exe 不拷 DLL 的装法会让它启动即退出
pub fn server_ready_for(root: &Path, backend: Backend) -> bool {
    let exe = server_exe_for(root, backend);
    if !exe.is_file() {
        return false;
    }
    match backend {
        Backend::Cpu => true,
        Backend::Cuda => exe
            .parent()
            .map(|dir| dir.join("cudart64_12.dll").is_file() && dir.join("ggml-cuda.dll").is_file())
            .unwrap_or(false),
    }
}

/// 机器上有没有可用的 NVIDIA 显卡。
///
/// 用 `nvidia-smi` 而不是查注册表：驱动装好了它才会有，没显卡/没驱动就直接失败，
/// 正好是我们要判断的东西。新版驱动把它放在 System32，旧的在 NVSMI 目录，两个都试。
pub fn detect_nvidia() -> bool {
    let candidates = [
        "nvidia-smi",
        r"C:\Windows\System32\nvidia-smi.exe",
        r"C:\Program Files\NVIDIA Corporation\NVSMI\nvidia-smi.exe",
    ];
    candidates.iter().any(|exe| probe_nvidia_smi(exe))
}

#[cfg(windows)]
fn probe_nvidia_smi(exe: &str) -> bool {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW：避免在 GUI 程序里闪一个黑色控制台窗口
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new(exe)
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|out| out.status.success() && !out.stdout.is_empty())
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn probe_nvidia_smi(_exe: &str) -> bool {
    false
}

/// 只有大小对得上才算装好——中断的下载会留下一个能读但跑不起来的半个模型
pub fn model_ready(root: &Path) -> bool {
    match fs::metadata(model_path(root)) {
        Ok(meta) => meta.len() == MODEL_BYTES,
        Err(_) => false,
    }
}

pub fn model_size(root: &Path) -> u64 {
    fs::metadata(model_path(root)).map(|m| m.len()).unwrap_or(0)
}

/// VAD 是可选增强：没装就退回「只靠前端 VAD 和文本清洗」的老路子，
/// 所以判断宽松些，大小对得上即可。
pub fn vad_ready(root: &Path) -> bool {
    match fs::metadata(vad_path(root)) {
        Ok(meta) => meta.len() == VAD_BYTES,
        Err(_) => false,
    }
}

/// 流式下载，每收到一块就回调一次，供前端画进度条。
///
/// 回调按值传泛型而不是 `&mut dyn FnMut`：后者不满足 Send，
/// tauri 的 async command 要求整条 future 是 Send 的。
pub async fn download<F>(url: &str, dest: &Path, mut on_progress: F) -> Result<u64, String>
where
    F: FnMut(u64, Option<u64>) + Send,
{
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
    }

    let client = reqwest::Client::builder()
        .user_agent("shania-agent")
        .timeout(Duration::from_secs(60 * 30))
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

    // 先写 .part，完整了再改名：这样中断只会留下半个 .part，不会被误认为装好
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

/// 解压到 `dest_dir`，保留 zip 内的相对路径。
///
/// 只处理普通文件；`..` 开头的条目直接跳过，避免被恶意压缩包写到目标目录外。
pub fn extract_zip(zip_path: &Path, dest_dir: &Path) -> Result<usize, String> {
    let file = File::open(zip_path).map_err(|e| format!("打开压缩包失败：{e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解析压缩包失败：{e}"))?;

    let mut count = 0usize;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取压缩包条目失败：{e}"))?;

        let Some(raw_name) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            // 绝对路径或含 .. 的条目：跳过
            continue;
        };
        if entry.is_dir() {
            continue;
        }

        let target = dest_dir.join(&raw_name);
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
        }
        let mut out = File::create(&target).map_err(|e| format!("写入 {} 失败：{e}", target.display()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("解压失败：{e}"))?;
        count += 1;
    }

    Ok(count)
}

/// 默认端口：和 `scripts/stt-server.ps1` 保持一致，方便用户直接 curl 调试。
///
/// 用 28080 而不是 8080——8080 太容易被其他开发工具占用（Tomcat、代理、各类网关），
/// 撞上就得顺延，端口反而不可预期。28080 也不在 Windows 的动态端口范围（49152+）内。
pub const DEFAULT_PORT: u16 = 28080;

/// 首选端口往后最多再试几个，全被占就交给系统随机
const PORT_SCAN: u16 = 20;

/// 取一个可用端口。
///
/// `preferred` 传 0 表示「随便给一个」。否则先试它，再依次往后扫 `PORT_SCAN` 个，
/// 这样桌面端每次启动的端口是稳定的（用户能预知 `http://127.0.0.1:28080/v1`），
/// 真有冲突也不会失败，只是自动顺延。
pub fn pick_port(preferred: u16) -> Result<u16, String> {
    if preferred != 0 {
        for offset in 0..=PORT_SCAN {
            let Some(port) = preferred.checked_add(offset) else {
                break;
            };
            if let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port)) {
                drop(listener);
                return Ok(port);
            }
        }
    }

    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("找不到可用端口：{e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("读取端口失败：{e}"))?
        .port();
    drop(listener);
    Ok(port)
}

/// 探测某个端口上是不是已经有一个 whisper-server 在跑。
///
/// 只认 `/health` 返回里同时含 `status` 和 `ok` 的响应，避免把别的本地 web 服务
/// 误当成语音服务——用户自己用 `pnpm stt` 起的服务（默认也是 28080）能被直接复用，
/// 省掉再拉起一份模型的内存。
pub async fn probe(port: u16) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    let url = format!("http://127.0.0.1:{port}/health");
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => resp
            .text()
            .await
            .map(|body| body.contains("status") && body.contains("ok"))
            .unwrap_or(false),
        _ => false,
    }
}

/// 轮询 `/health` 直到模型加载完。
///
/// 刚 spawn 时端口还没监听、或者返回 503（模型在加载），都算「还没好」。
pub async fn wait_ready(port: u16, timeout: Duration) -> Result<(), String> {
    wait_ready_with(port, timeout, || true).await
}

/// 同上，但每轮都问一次 `alive()`。
///
/// 子进程启动即崩（比如缺 ffmpeg 时 `--convert` 会直接退出）时，
/// 不传这个判断就得傻等满 120 秒才报「没就绪」，日志里也看不出真正原因。
pub async fn wait_ready_with<F: Fn() -> bool>(
    port: u16,
    timeout: Duration,
    alive: F,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| format!("创建探测客户端失败：{e}"))?;
    let url = format!("http://127.0.0.1:{port}/health");
    let deadline = Instant::now() + timeout;
    let mut last = String::from("尚未响应");

    while Instant::now() < deadline {
        if !alive() {
            return Err("语音服务进程已退出，没能开始监听端口".to_string());
        }
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            Ok(resp) => last = format!("返回 {}", resp.status()),
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    if !alive() {
        return Err("语音服务进程已退出，没能开始监听端口".to_string());
    }

    Err(format!(
        "语音服务在 {} 秒内没有就绪（{last}）",
        timeout.as_secs()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_live_under_root() {
        let root = Path::new("C:/x/.shania");
        assert!(server_exe(root).ends_with("Release/whisper-server.exe"));
        assert_eq!(model_path(root), root.join("models").join(MODEL_NAME));
    }

    #[test]
    fn missing_model_is_not_ready() {
        let root = std::env::temp_dir().join("shania-stt-test-empty");
        let _ = fs::create_dir_all(&root);
        assert!(!model_ready(&root));
        assert_eq!(model_size(&root), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn extracts_files_and_skips_escaping_entries() {
        let dir = std::env::temp_dir().join(format!("shania-zip-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("t.zip");

        {
            let mut w = zip::ZipWriter::new(File::create(&zip_path).unwrap());
            let opts: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            w.start_file("Release/whisper-server.exe", opts).unwrap();
            w.write_all(b"fake-exe").unwrap();
            w.start_file("Release/ggml.dll", opts).unwrap();
            w.write_all(b"fake-dll").unwrap();
            w.finish().unwrap();
        }

        let out = dir.join("out");
        let n = extract_zip(&zip_path, &out).unwrap();
        assert_eq!(n, 2);
        assert!(out.join("Release/whisper-server.exe").is_file());
        assert_eq!(
            fs::read(out.join("Release/ggml.dll")).unwrap(),
            b"fake-dll"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn picks_the_preferred_port_when_free() {
        // 先占一个端口再请求它，验证会顺延而不是失败
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy_port = busy.local_addr().unwrap().port();
        let got = pick_port(busy_port).unwrap();
        assert_ne!(got, busy_port);
        assert!(got > busy_port && got <= busy_port + PORT_SCAN);
        drop(busy);
    }

    #[test]
    fn picks_a_usable_port_when_unspecified() {
        let port = pick_port(0).unwrap();
        assert!(port > 1024);
    }

    /// 真联网跑一遍下载 + 解压（只下 8.6MB 的服务端包，不碰 182MB 的模型）。
    /// 默认忽略，手动验证时：`cargo test -p shania-core -- --ignored`
    #[tokio::test]
    #[ignore = "需要联网"]
    async fn downloads_and_extracts_server_package() {
        let root = std::env::temp_dir().join(format!("shania-dl-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);

        let zip_path = tools_dir(&root).join("whisper-bin.zip");
        let got = download(SERVER_ZIP_URL, &zip_path, |_, _| {})
            .await
            .expect("下载应该成功");
        assert!(got > 1_000_000, "包太小，可能下到了错误页：{got}");

        let n = extract_zip(&zip_path, &tools_dir(&root)).expect("解压应该成功");
        assert!(n > 10, "条目数量异常：{n}");
        assert!(server_ready(&root), "解压后应该有 whisper-server.exe");

        let _ = fs::remove_dir_all(&root);
    }
}
