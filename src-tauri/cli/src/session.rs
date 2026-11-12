//! CLI 的会话接续。
//!
//! 直接读写桌面端那份 `~/.shania/sessions/<id>.json`，格式与 `src-tauri/src/storage.rs`
//! 保持一致：`{ id, title, messages, createdAt, updatedAt }`。
//! 这样 `shania --continue` 接的是桌面上刚才那轮对话，而不是另开一份。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

pub fn sessions_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".shania").join("sessions"))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn stamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 文件名只允许安全字符，防止会话 id 里出现路径分隔符
fn safe_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if cleaned.is_empty() {
        "session".to_string()
    } else {
        cleaned
    }
}

pub struct CliSession {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// 纯文本视图：只有 user / assistant 正文，工具轨迹不入会话文件
    pub messages: Vec<Value>,
}

impl CliSession {
    pub fn new() -> Self {
        let now = now_ms();
        Self {
            id: format!("cli-{}", stamp()),
            title: "新对话".to_string(),
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// 转成 core 需要的 history 格式
    pub fn history(&self) -> Vec<Value> {
        self.messages
            .iter()
            .filter_map(|m| {
                let role = m.get("role")?.as_str()?;
                if role != "user" && role != "assistant" {
                    return None;
                }
                let content = m.get("content")?.as_str().unwrap_or("").trim();
                if content.is_empty() {
                    return None;
                }
                Some(json!({ "role": role, "content": content }))
            })
            .collect()
    }

    fn push(&mut self, role: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let now = now_ms();
        self.messages.push(json!({
            "id": format!("{role}-{}", stamp()),
            "role": role,
            "content": text,
            "createdAt": now,
            "status": "done",
        }));
        self.updated_at = now;

        // 首条用户消息顺手当作标题，和桌面端行为一致
        if role == "user" && (self.title.is_empty() || self.title == "新对话") {
            let flat = text.replace(['\n', '\r'], " ");
            let title: String = flat.chars().take(28).collect();
            self.title = if title.trim().is_empty() {
                "新对话".to_string()
            } else {
                title
            };
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.push("user", text);
    }

    pub fn push_assistant(&mut self, text: &str) {
        self.push("assistant", text);
    }

    /// 落盘：先写临时文件再改名，和桌面端一致，避免半个 JSON
    pub fn save(&self) -> Result<PathBuf, String> {
        let dir = sessions_dir().ok_or_else(|| "无法定位用户主目录".to_string())?;
        fs::create_dir_all(&dir).map_err(|e| format!("创建会话目录失败：{e}"))?;

        let path = dir.join(format!("{}.json", safe_id(&self.id)));
        let payload = json!({
            "id": self.id,
            "title": self.title,
            "messages": self.messages,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        });
        let text = serde_json::to_string_pretty(&payload)
            .map_err(|e| format!("序列化会话失败：{e}"))?;

        let tmp = dir.join(format!("{}.json.tmp", safe_id(&self.id)));
        fs::write(&tmp, text.as_bytes()).map_err(|e| format!("写入会话失败：{e}"))?;
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        fs::rename(&tmp, &path).map_err(|e| format!("保存会话失败：{e}"))?;
        Ok(path)
    }
}

/// 从一条消息里抽出可读正文：文本附件并入正文，图片只留占位说明
fn message_text(m: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(content) = m.get("content").and_then(|v| v.as_str()) {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }

    if let Some(list) = m.get("attachments").and_then(|v| v.as_array()) {
        let mut texts = 0;
        let mut images = 0;
        for a in list {
            let is_text = a.get("kind").and_then(|v| v.as_str()) == Some("text");
            if is_text {
                let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("附件");
                if let Some(body) = a.get("text").and_then(|v| v.as_str()) {
                    let ticks = if body.contains("```") { "````" } else { "```" };
                    parts.push(format!("【附件：{name}】\n{ticks}\n{body}\n{ticks}"));
                    texts += 1;
                }
            } else {
                images += 1;
            }
        }
        if images > 0 {
            parts.push(format!("（含 {images} 张图片，命令行模式不加载图片内容）"));
        }
        let _ = texts;
    }

    parts.join("\n\n")
}

fn parse_session(raw: &Value, path: &Path) -> Option<CliSession> {
    let id = raw
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "session".to_string())
        });

    let list = raw.get("messages")?.as_array()?;
    let mut messages = Vec::new();
    for m in list {
        let role = m.get("role").and_then(|v| v.as_str())?;
        if role != "user" && role != "assistant" {
            continue;
        }
        let text = message_text(m);
        if text.is_empty() {
            continue;
        }
        messages.push(json!({
            "id": m.get("id").and_then(|v| v.as_str()).unwrap_or("msg"),
            "role": role,
            "content": text,
            "createdAt": m.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0),
            "status": "done",
        }));
    }

    let created_at = raw.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0);
    let updated_at = raw.get("updatedAt").and_then(|v| v.as_i64()).unwrap_or(created_at);

    Some(CliSession {
        id,
        title: raw
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("未命名对话")
            .to_string(),
        created_at,
        updated_at,
        messages,
    })
}

/// 找出最近更新的一条会话。读不懂的文件直接跳过——宁可少接一条，
/// 也不要因为某个坏文件让 `--continue` 整个失败。
pub fn load_latest() -> Option<CliSession> {
    let dir = sessions_dir()?;
    let entries = fs::read_dir(&dir).ok()?;

    let mut best: Option<CliSession> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // 跳过 sessions.json.bak 之类的备份
        if path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.contains('.') || s == "sessions")
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(session) = parse_session(&value, &path) else {
            continue;
        };
        if session.is_empty() {
            continue;
        }
        let newer = best
            .as_ref()
            .map(|b| session.updated_at > b.updated_at)
            .unwrap_or(true);
        if newer {
            best = Some(session);
        }
    }

    best
}
