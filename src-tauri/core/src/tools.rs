use std::fs;

use serde_json::{json, Value};

/// 内置工具清单（OpenAI function calling 格式）
pub fn definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "get_current_time",
                "description": "获取用户电脑当前的本地日期、时间、时区和 Unix 时间戳。当用户询问现在几点、今天几号、星期几时必须调用。",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_system_info",
                "description": "获取当前运行环境的系统信息：操作系统、CPU 架构、主机名、逻辑核心数。",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_text_file",
                "description": "读取本机上一个文本文件的全部内容（最大 200KB）。仅在用户明确要求查看某个文件内容时调用。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "文件的绝对路径，例如 D:/project/notes.md 或 /Users/me/a.txt"
                        }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_directory",
                "description": "列出本机某个目录下的文件与子目录（最多 200 项），返回名称、类型和大小。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "目录的绝对路径"
                        }
                    },
                    "required": ["path"]
                }
            }
        }
    ])
}

/// 是不是内置工具。工具循环靠它决定要不要交给 MCP 处理。
pub fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "get_current_time" | "get_system_info" | "read_text_file" | "list_directory"
    )
}

/// 执行工具，返回 (是否成功, 文本结果)
pub async fn execute(name: &str, args_raw: &str) -> (bool, String) {
    let args: Value = serde_json::from_str(args_raw).unwrap_or_else(|_| json!({}));

    match name {
        "get_current_time" => {
            let now = chrono::Local::now();
            let weekday = match chrono::Datelike::weekday(&now) {
                chrono::Weekday::Mon => "星期一",
                chrono::Weekday::Tue => "星期二",
                chrono::Weekday::Wed => "星期三",
                chrono::Weekday::Thu => "星期四",
                chrono::Weekday::Fri => "星期五",
                chrono::Weekday::Sat => "星期六",
                chrono::Weekday::Sun => "星期日",
            };
            (
                true,
                format!(
                    "本地时间：{}（{}）\n时区：{}\nUnix 时间戳：{}",
                    now.format("%Y-%m-%d %H:%M:%S"),
                    weekday,
                    now.format("%:z"),
                    chrono::Utc::now().timestamp()
                ),
            )
        }
        "get_system_info" => {
            let host = std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "unknown".to_string());
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(0);
            (
                true,
                format!(
                    "操作系统：{}\nCPU 架构：{}\n主机名：{}\n逻辑核心数：{}",
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    host,
                    cores
                ),
            )
        }
        "read_text_file" => read_text_file(&args),
        "list_directory" => list_directory(&args),
        other => (false, format!("未知工具：{other}")),
    }
}

fn read_text_file(args: &Value) -> (bool, String) {
    let Some(path) = args.get("path").and_then(|v| v.as_str()).map(str::trim) else {
        return (false, "缺少参数 path".to_string());
    };
    if path.is_empty() {
        return (false, "路径为空".to_string());
    }

    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return (false, format!("无法访问文件：{e}")),
    };
    if meta.is_dir() {
        return (false, "这是一个目录，请改用 list_directory".to_string());
    }
    if meta.len() > 200 * 1024 {
        return (
            false,
            format!("文件过大（{} 字节），超过 200KB 限制", meta.len()),
        );
    }

    match fs::read_to_string(path) {
        Ok(text) => (true, text),
        Err(e) => (false, format!("读取失败（可能不是 UTF-8 文本文件）：{e}")),
    }
}

fn list_directory(args: &Value) -> (bool, String) {
    let Some(path) = args.get("path").and_then(|v| v.as_str()).map(str::trim) else {
        return (false, "缺少参数 path".to_string());
    };

    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => return (false, format!("无法读取目录：{e}")),
    };

    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut count = 0usize;

    for entry in entries.flatten() {
        if count >= 200 {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        if is_dir {
            dirs.push(format!("{name}/"));
        } else {
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            files.push(format!("{name}  ({} B)", human_size(size)));
        }
        count += 1;
    }

    dirs.sort();
    files.sort();

    let mut out = format!(
        "目录：{path}\n共 {} 个子目录，{} 个文件\n",
        dirs.len(),
        files.len()
    );
    if !dirs.is_empty() {
        out.push_str("\n[目录]\n");
        out.push_str(&dirs.join("\n"));
        out.push('\n');
    }
    if !files.is_empty() {
        out.push_str("\n[文件]\n");
        out.push_str(&files.join("\n"));
        out.push('\n');
    }
    (true, out)
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}
