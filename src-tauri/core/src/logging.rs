//! 极简文件日志。
//!
//! 桌面端在 release 下没有控制台，`eprintln!` 等于把线索直接扔掉；这里统一落到
//! `~/.shania/logs/shania.log`，出问题时让用户把文件发过来就能定位。
//!
//! 没有引 `tracing` / `log`：需要的只是「一行时间 + 级别 + 文本」，
//! 而且不希望在启动早期就依赖复杂的初始化顺序。
//!
//! 除了主日志，各外部服务的原始输出单独成文件（都在同一个目录下）：
//!
//! - `stt.log` —— whisper-server 的 stdout/stderr（每次启动覆盖）
//! - `mcp-<server>.log` —— 每个 MCP server 的 stderr（每次连接覆盖）

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// 主日志超过这个大小，就在下次启动时轮转成 `shania.log.1`（只留一份旧文件）
const MAX_BYTES: u64 = 5 * 1024 * 1024;

struct Sink {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

static SINK: OnceLock<Sink> = OnceLock::new();

/// 初始化日志目录。重复调用只有第一次生效，返回主日志路径。
///
/// 打不开文件不算致命错误：退回 stderr，程序照常跑。
pub fn init(dir: &Path) -> Option<PathBuf> {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("shania.log");
    rotate(&path);

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();

    let _ = SINK.set(Sink {
        path: path.clone(),
        file: Mutex::new(file),
    });

    SINK.get().map(|sink| sink.path.clone())
}

/// 默认日志目录 `~/.shania/logs`。
///
/// 给不依赖桌面端 `settings` 模块的调用方用（CLI 就是这么用的），
/// 和桌面端落在同一个位置。
pub fn default_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".shania").join("logs"))
}

/// 主日志的路径（未初始化时为 None）
pub fn path() -> Option<PathBuf> {
    SINK.get().map(|sink| sink.path.clone())
}

/// 日志目录（未初始化时为 None）。外部服务的日志也放在这里。
pub fn dir() -> Option<PathBuf> {
    path().and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// 写一行。级别固定宽度，方便肉眼对齐。
pub fn write(level: &str, msg: &str) {
    let line = format_line(level, msg);

    if let Some(sink) = SINK.get() {
        if let Ok(mut guard) = sink.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.write_all(line.as_bytes());
                return;
            }
        }
    }

    // 还没初始化、或者文件写不了：至少 dev 下能在终端看到
    eprint!("{line}");
}

/// 一行日志的格式：`2026-11-11 21:34:28.123 INFO  内容`
fn format_line(level: &str, msg: &str) -> String {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    format!("{stamp} {level:<5} {msg}\n")
}

fn rotate(path: &Path) {
    let too_big = std::fs::metadata(path)
        .map(|meta| meta.len() > MAX_BYTES)
        .unwrap_or(false);
    if !too_big {
        return;
    }
    let old = PathBuf::from(format!("{}.1", path.to_string_lossy()));
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::rename(path, &old);
}

/// 各个外部服务的日志文件。`name` 用固定短名，避免路径穿越。
///
/// 每次启动覆盖：排查问题时关心的是「这一次为什么失败」，
/// 而不是半年前的输出；主日志那边才需要保留历史。
pub fn service_log(dir: &Path, name: &str) -> PathBuf {
    let _ = std::fs::create_dir_all(dir);
    dir.join(format!("{name}.log"))
}

/// 主日志的 `INFO` 档
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::logging::write("INFO", &format!($($arg)*))
    };
}

/// 主日志的 `WARN` 档：能用但有异常，用户可能会来问
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::logging::write("WARN", &format!($($arg)*))
    };
}

/// 主日志的 `ERROR` 档
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::logging::write("ERROR", &format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_one_line_with_level() {
        let line = format_line("WARN", "端口被占用");
        assert!(line.ends_with("WARN  端口被占用\n"), "实际：{line}");
        assert_eq!(line.lines().count(), 1);
    }

    #[test]
    fn rotates_when_too_big() {
        let dir = std::env::temp_dir().join(format!("shania-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let path = dir.join("shania.log");
        std::fs::write(&path, vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();

        rotate(&path);
        assert!(!path.exists());
        assert!(PathBuf::from(format!("{}.1", path.to_string_lossy())).is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_small_file_untouched() {
        let dir = std::env::temp_dir().join(format!("shania-log-small-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let path = dir.join("shania.log");
        std::fs::write(&path, b"keep me").unwrap();
        rotate(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn service_logs_share_the_directory() {
        let dir = std::env::temp_dir().join(format!("shania-log-svc-{}", std::process::id()));
        let path = service_log(&dir, "mcp-filesystem");
        assert_eq!(path.file_name().unwrap(), "mcp-filesystem.log");
        assert!(path.parent().unwrap().is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
