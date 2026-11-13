//! 工作空间：一个可配置的根目录，外加围绕它的三件事——看文件、看 git 变更、开终端。
//!
//! 这里是**用户可见的入口**，不是 agent 的工具。agent 的 `read_text_file` /
//! `list_directory` 仍按传进来的绝对路径走（见 core/src/tools.rs），两者互不影响：
//! 工作空间只是「我现在在看哪个目录」，不改 agent 的解析规则。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

/// 文件树里不展开的目录。不是安全措施，纯粹是这些目录一动就是几万个文件，
/// 展开一次能卡住界面好几秒，而用户几乎不会在这儿找文件。
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    ".venv",
    "__pycache__",
    ".idea",
    ".next",
    ".nuxt",
];

/// 一次最多列这么多条，防止误入超大目录把前端撑爆
const MAX_ENTRIES: usize = 500;

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// settings.json 里的 workspacePath。桌面端和 CLI 共用一份文件，
/// 所以在命令行里跑 `shania` 也能拿到同一个工作目录。
fn configured_root() -> Option<PathBuf> {
    let path = crate::settings::settings_path().ok()?;
    let text = fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("workspacePath")?.as_str().map(PathBuf::from)
}

/// 当前工作空间。配过且目录还在就用它，否则退回用户主目录——
/// 返回一个不存在的路径会让文件树直接空白，用户还以为坏了。
fn resolve_root() -> PathBuf {
    if let Some(p) = configured_root() {
        if p.is_dir() {
            return p;
        }
    }
    home_dir().unwrap_or_else(|| PathBuf::from("."))
}

#[derive(Serialize)]
pub struct Entry {
    name: String,
    path: String,
    dir: bool,
    size: u64,
}

/// 隐藏 git 拉起的控制台窗口（否则每次刷状态都会闪一下黑框）
#[cfg(windows)]
fn quiet(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn quiet(_cmd: &mut Command) {}

fn run_git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).args(args);
    quiet(&mut cmd);

    let out = cmd
        .output()
        .map_err(|e| format!("调用 git 失败（是否已安装 Git？）：{e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            "git 命令执行失败".to_string()
        } else {
            err
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[tauri::command]
pub fn workspace_root() -> String {
    resolve_root().to_string_lossy().to_string()
}

/// 切换工作空间，顺手写回 settings.json（保留其它字段，不能整包覆盖）
#[tauri::command]
pub fn workspace_set(path: String) -> Result<String, String> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(format!("不是有效目录：{path}"));
    }
    let canonical = dir
        .canonicalize()
        .unwrap_or(dir)
        .to_string_lossy()
        .to_string();

    let file = crate::settings::settings_path()?;
    let mut json: serde_json::Value = fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !json.is_object() {
        json = serde_json::json!({});
    }
    json["workspacePath"] = serde_json::Value::String(canonical.clone());
    crate::settings::save_settings(serde_json::to_string(&json).map_err(|e| e.to_string())?)?;

    Ok(canonical)
}

/// 列一层目录（懒加载：展开哪个调哪个，不做递归扫描）
#[tauri::command]
pub fn workspace_list(path: String) -> Result<Vec<Entry>, String> {
    let dir = PathBuf::from(&path);
    let read = fs::read_dir(&dir).map_err(|e| format!("无法读取目录：{e}"))?;

    let mut dirs: Vec<Entry> = Vec::new();
    let mut files: Vec<Entry> = Vec::new();

    for item in read.flatten() {
        if dirs.len() + files.len() >= MAX_ENTRIES {
            break;
        }
        let name = item.file_name().to_string_lossy().to_string();
        let meta = match item.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = meta.is_dir();
        if is_dir && SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let entry = Entry {
            name,
            path: item.path().to_string_lossy().to_string(),
            dir: is_dir,
            size: if is_dir { 0 } else { meta.len() },
        };
        if is_dir {
            dirs.push(entry);
        } else {
            files.push(entry);
        }
    }

    // 目录在前、各自按名字不区分大小写排：和资源管理器一致
    dirs.sort_by_key(|e| e.name.to_lowercase());
    files.sort_by_key(|e| e.name.to_lowercase());
    dirs.append(&mut files);
    Ok(dirs)
}

/// 用系统默认程序打开：文件夹交给资源管理器，文件交给关联程序
#[tauri::command]
pub fn workspace_open(path: String) -> Result<(), String> {
    let target = PathBuf::from(&path);
    if !target.exists() {
        return Err(format!("路径不存在：{path}"));
    }

    #[cfg(windows)]
    {
        // 打开目录用 explorer；打开文件用 start（走 ShellExecute 的关联程序）
        let mut cmd = if target.is_dir() {
            let mut c = Command::new("explorer");
            c.arg(&target);
            c
        } else {
            let mut c = Command::new("cmd");
            c.args(["/c", "start", ""]).arg(&target);
            c
        };
        quiet(&mut cmd);
        cmd.spawn().map_err(|e| format!("启动失败：{e}"))?;
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&target)
            .spawn()
            .map_err(|e| format!("启动失败：{e}"))?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(&target)
            .spawn()
            .map_err(|e| format!("启动失败：{e}"))?;
    }

    Ok(())
}

/// 在工作空间里拉起终端。
///
/// 按 Windows Terminal → PowerShell 7 → Windows PowerShell → cmd 依次尝试，
/// 返回实际用上的那个名字，前端据此给一句确认（拉起外部窗口没有回调，
/// 不给反馈用户根本分不清是「开了没反应」还是「压根没执行」）。
///
/// 注意：这几步都**不能**加 CREATE_NO_WINDOW。那个标志会被子进程继承，
/// 终端窗口会整个起不来——表现出来就是「点了终端没反应」。git 那类控制台
/// 程序的 `quiet()` 是另一回事，那里要的恰恰是别闪黑框。
#[tauri::command]
pub fn workspace_terminal(path: String) -> Result<String, String> {
    let dir = {
        let p = PathBuf::from(&path);
        if p.is_dir() {
            p
        } else {
            resolve_root()
        }
    };

    #[cfg(windows)]
    {
        // -d 指定起始目录。wt 有标签页，会复用已开的窗口
        let mut wt = Command::new("wt");
        wt.arg("-d").arg(&dir);
        if wt.spawn().is_ok() {
            return Ok("Windows Terminal".into());
        }

        // PowerShell 7（pwsh）没装是常态，装了的话箭头/编码体验比 cmd 好
        let mut pwsh = Command::new("pwsh");
        pwsh.args(["-NoExit", "-Command"]).arg(format!(
            "Set-Location -LiteralPath \"{}\"",
            dir.display()
        ));
        if pwsh.spawn().is_ok() {
            return Ok("PowerShell 7".into());
        }

        let mut ps = Command::new("powershell");
        ps.args(["-NoExit", "-Command"]).arg(format!(
            "Set-Location -LiteralPath \"{}\"",
            dir.display()
        ));
        if ps.spawn().is_ok() {
            return Ok("Windows PowerShell".into());
        }

        // 最后兜底：cmd 自己就是控制台程序，直接起窗口，/k 让窗口留着别闪退。
        // 之前套了一层 `cmd /c start "" cmd /k "cd /d ..."`，多出来的引号会被
        // 拆得七零八落，目录进不去——直接 spawn 反而干净。
        let mut cmd = Command::new("cmd");
        cmd.arg("/k").arg("cd").arg("/d").arg(&dir);
        cmd.spawn()
            .map_err(|e| format!("启动终端失败：{e}"))?;
        Ok("命令提示符".into())
    }

    #[cfg(not(windows))]
    {
        Command::new("open")
            .arg("-a")
            .arg("Terminal")
            .arg(&dir)
            .spawn()
            .map_err(|e| format!("启动终端失败：{e}"))?;
        Ok("Terminal".into())
    }
}

#[derive(Serialize)]
pub struct Change {
    /// 相对仓库根，用 / 分隔（前端展示用）
    path: String,
    /// porcelain 的两位状态码，如 "M "、" M"、"??"
    code: String,
    staged: bool,
    untracked: bool,
}

#[tauri::command]
pub fn git_changes(path: String) -> Result<Vec<Change>, String> {
    let dir = PathBuf::from(&path);
    // -uall：把未跟踪目录展开到文件，否则只看到一行 `dir/`
    let text = run_git(&dir, &["status", "--porcelain=v1", "-uall"])?;

    let mut list = Vec::new();
    for line in text.lines() {
        if line.len() < 4 {
            continue;
        }
        let x = &line[0..1];
        let y = &line[1..2];
        let raw = line[3..].trim();
        // 重命名是 `old -> new`，取新名字
        let name = raw.rsplit(" -> ").next().unwrap_or(raw).trim_matches('"');
        let untracked = x == "?" && y == "?";
        list.push(Change {
            path: name.replace('\\', "/"),
            code: format!("{x}{y}"),
            staged: x != " " && x != "?",
            untracked,
        });
    }
    Ok(list)
}

#[tauri::command]
pub fn git_branch(path: String) -> Result<String, String> {
    let dir = PathBuf::from(&path);
    let name = run_git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    Ok(name)
}

/// 单文件 diff。未跟踪文件没有 HEAD 版本，直接把内容当「新增」返回。
#[tauri::command]
pub fn git_diff(path: String, file: String) -> Result<String, String> {
    let dir = PathBuf::from(&path);
    let text = run_git(&dir, &["diff", "--no-color", "HEAD", "--", &file])?;
    if !text.trim().is_empty() {
        return Ok(text);
    }

    // HEAD 里没有（新文件）——读原文，前端按整段新增渲染
    let full = dir.join(&file);
    match fs::read_to_string(&full) {
        Ok(body) => Ok(body),
        Err(e) => Err(format!("无法读取文件：{e}")),
    }
}
