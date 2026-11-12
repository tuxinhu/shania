//! MCP（Model Context Protocol）客户端。
//!
//! 通过 stdio 启动配置里的 MCP server，完成 `initialize` 握手、拉取 `tools/list`，
//! 把外部工具并进多轮工具循环；模型调用时再转发 `tools/call`。
//!
//! 设计取舍：
//! - **每次对话启动、结束关闭**。MCP server 常驻会带来进程泄漏与状态不一致
//!   （比如文件系统 server 的根目录变了），按需启动最省心，代价是首轮多几百毫秒。
//! - **工具名统一加 `mcp__` 前缀**，避免和内置工具重名，也让模型能从名字看出工具来源。
//! - **单个 server 连不上不影响对话**，失败信息收集在 `warnings` 里，由调用方打印。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// MCP 协议版本。2024-11-05 是目前兼容面最广的一版。
const PROTOCOL_VERSION: &str = "2024-11-05";
/// 单次请求超时。工具调用偶尔很慢，但卡住超过这个时间基本是 server 死了。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct McpFile {
    #[serde(default)]
    pub servers: BTreeMap<String, ServerConfig>,
}

/// `~/.shania/mcp.json`
pub fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".shania").join("mcp.json"))
}

/// 首次运行时落一份带示例的模板，省得用户去猜格式。
///
/// 三条约束：
/// - **已存在就一个字都不动**，绝不覆盖用户配置；
/// - 示例默认 `enabled: false`，否则每轮对话都会去 npx 拉包，第一次启动慢得莫名其妙；
/// - 写失败只返回 `None`，不影响启动。
pub fn ensure_config() -> Option<PathBuf> {
    let path = config_path()?;
    if path.exists() {
        return None;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }

    let template = json!({
        "servers": {
            "example": {
                "command": "npx",
                "args": [
                    "-y",
                    "@modelcontextprotocol/server-filesystem",
                    "D:/tmp"
                ],
                "env": {},
                "enabled": false
            }
        }
    });

    let text = serde_json::to_string_pretty(&template).ok()?;
    std::fs::write(&path, text).ok()?;
    Some(path)
}

/// 配置文件不存在、或写坏了，都当作「没有 MCP」处理——不该因此起不来。
pub fn load_config() -> McpFile {
    let Some(path) = config_path() else {
        return McpFile::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return McpFile::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// 工具名只保留 `[A-Za-z0-9_-]`，并限制在 64 字符内：
/// OpenAI 的函数名规范如此，超长或含点号的会被接口直接拒掉。
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cleaned.chars().take(64).collect()
}

struct McpServer {
    name: String,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpServer {
    /// Windows 上 `npx` 这类是 `.cmd`，CreateProcess 起不来，必须套一层 cmd /C。
    fn spawn(name: &str, cfg: &ServerConfig) -> Result<Self, String> {
        let mut command = if cfg!(windows) && needs_cmd_wrapper(&cfg.command) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&cfg.command).args(&cfg.args);
            c
        } else {
            let mut c = Command::new(&cfg.command);
            c.args(&cfg.args);
            c
        };

        command
            .envs(&cfg.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true);

        // stderr 单独落盘（`logs/mcp-<server>.log`）：服务器起不来、缺依赖、
        // 参数写错这些信息全在这一路，丢了就只能看到一个「工具列表是空的」。
        // stdout 不能碰，那是 JSON-RPC 的通信通道。
        if let Some(dir) = crate::logging::dir() {
            let log = crate::logging::service_log(&dir, &format!("mcp-{}", sanitize(name)));
            if let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log)
            {
                command.stderr(Stdio::from(file));
            }
        }

        let mut child = command
            .spawn()
            .map_err(|e| format!("启动失败（{}）：{e}", cfg.command))?;

        let stdin = child.stdin.take().ok_or("无法获取 stdin")?;
        let stdout = child.stdout.take().ok_or("无法获取 stdout")?;

        Ok(Self {
            name: name.to_string(),
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    async fn send(&mut self, message: &Value) -> Result<(), String> {
        let mut line = serde_json::to_string(message).map_err(|e| format!("序列化失败：{e}"))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("写入失败：{e}"))?;
        self.stdin.flush().await.map_err(|e| format!("刷新失败：{e}"))
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    /// 发一个请求并等 id 匹配的响应；中间夹的通知（或日志行）跳过。
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;

        let read = async {
            loop {
                let mut line = String::new();
                let n = self
                    .stdout
                    .read_line(&mut line)
                    .await
                    .map_err(|e| format!("读取失败：{e}"))?;
                if n == 0 {
                    return Err(format!("{} 提前退出", self.name));
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // 有些 server 会往 stdout 打日志，解析不了就跳过
                let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                if value.get("id").and_then(|v| v.as_u64()) != Some(id) {
                    continue;
                }
                if let Some(err) = value.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("未知错误");
                    return Err(msg.to_string());
                }
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
        };

        tokio::time::timeout(REQUEST_TIMEOUT, read)
            .await
            .map_err(|_| format!("{} 响应超时（{}s）", self.name, REQUEST_TIMEOUT.as_secs()))?
    }

    async fn initialize(&mut self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "shania", "version": env!("CARGO_PKG_VERSION") },
            }),
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await
    }

    async fn list_tools(&mut self) -> Result<Vec<Value>, String> {
        let result = self.request("tools/list", json!({})).await?;
        Ok(result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default())
    }
}

fn needs_cmd_wrapper(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if lower.ends_with(".exe") {
        return false;
    }
    // 没有扩展名的命令（npx、uvx、node…）和批处理都要过 cmd
    !lower.ends_with(".com")
}

struct McpTool {
    /// 暴露给模型的名字，如 mcp__filesystem__read_file
    full_name: String,
    /// 在 server 端的原始名字
    remote_name: String,
    server_index: usize,
}

/// 一轮对话期间持有的所有 MCP 连接。
#[derive(Default)]
pub struct McpHub {
    servers: Vec<McpServer>,
    tools: Vec<McpTool>,
    defs: Vec<Value>,
    /// 连接失败的 server，供调用方打印诊断信息
    pub warnings: Vec<String>,
}

impl McpHub {
    /// 按配置启动所有 server。单个失败只记录警告，不影响其他 server。
    pub async fn connect() -> Self {
        Self::connect_with(load_config()).await
    }

    /// 用给定配置启动，便于测试时不依赖真实 `~/.shania/mcp.json`
    pub async fn connect_with(cfg: McpFile) -> Self {
        let mut hub = McpHub::default();

        for (name, server_cfg) in cfg.servers.iter() {
            if !server_cfg.enabled {
                continue;
            }
            if server_cfg.command.trim().is_empty() {
                hub.warnings.push(format!("[{name}] 没有配置 command，已跳过"));
                continue;
            }

            match McpServer::spawn(name, server_cfg) {
                Ok(mut server) => match server.initialize().await {
                    Ok(()) => match server.list_tools().await {
                        Ok(list) => {
                            let index = hub.servers.len();
                            for tool in list {
                                let Some(remote) = tool.get("name").and_then(|v| v.as_str()) else {
                                    continue;
                                };
                                let full = sanitize(&format!("mcp__{name}__{remote}"));
                                let description = tool
                                    .get("description")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("外部 MCP 工具");
                                let parameters = tool
                                    .get("inputSchema")
                                    .cloned()
                                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));

                                hub.defs.push(json!({
                                    "type": "function",
                                    "function": {
                                        "name": full,
                                        "description": format!("[MCP·{name}] {description}"),
                                        "parameters": parameters,
                                    }
                                }));
                                hub.tools.push(McpTool {
                                    full_name: full,
                                    remote_name: remote.to_string(),
                                    server_index: index,
                                });
                            }
                            hub.servers.push(server);
                        }
                        Err(err) => hub.warnings.push(format!("[{name}] 取工具列表失败：{err}")),
                    },
                    Err(err) => hub.warnings.push(format!("[{name}] 握手失败：{err}")),
                },
                Err(err) => hub.warnings.push(format!("[{name}] {err}")),
            }
        }

        hub
    }

    /// 给请求体用的工具定义
    pub fn definitions(&self) -> &[Value] {
        &self.defs
    }

    pub fn tool_count(&self) -> usize {
        self.defs.len()
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.full_name == name)
    }

    /// 调用某个 MCP 工具；返回 None 表示名字不认识
    pub async fn call(&mut self, name: &str, args_raw: &str) -> Option<(bool, String)> {
        let (index, remote) = {
            let tool = self.tools.iter().find(|t| t.full_name == name)?;
            (tool.server_index, tool.remote_name.clone())
        };

        let args: Value = serde_json::from_str(args_raw).unwrap_or_else(|_| json!({}));
        let Some(server) = self.servers.get_mut(index) else {
            return Some((false, "MCP server 已断开".to_string()));
        };

        match server
            .request("tools/call", json!({ "name": remote, "arguments": args }))
            .await
        {
            Ok(result) => Some(flatten_result(&result)),
            Err(err) => Some((false, format!("调用失败：{err}"))),
        }
    }

    /// 结束本轮对话，主动关闭子进程
    pub async fn shutdown(&mut self) {
        for server in self.servers.iter_mut() {
            let _ = server.child.kill().await;
        }
        self.servers.clear();
        self.tools.clear();
        self.defs.clear();
    }
}

/// MCP 的返回是 content 数组，常见是 `[{type:"text", text:"..."}]`。
/// 图片/音频等非文本块这里只留占位说明，避免把 base64 灌进上下文。
fn flatten_result(result: &Value) -> (bool, String) {
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut parts: Vec<String> = Vec::new();
    if let Some(list) = result.get("content").and_then(|v| v.as_array()) {
        for item in list {
            match item.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                        parts.push(text.to_string());
                    }
                }
                Some(other) => parts.push(format!("（{other} 类型内容，已省略）")),
                None => {}
            }
        }
    }

    if parts.is_empty() {
        // 有些 server 直接把结果放在 structuredContent 里
        if let Some(v) = result.get("structuredContent") {
            parts.push(v.to_string());
        }
    }

    if parts.is_empty() {
        parts.push("（工具没有返回内容）".to_string());
    }

    (!is_error, parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个最小的 MCP server：只实现握手、tools/list 和一个 echo 工具。
    /// 用 node 写是因为它在本机一定有，省去额外依赖。
    const SERVER_JS: &str = r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
const send = (obj) => process.stdout.write(JSON.stringify(obj) + '\n');

rl.on('line', (line) => {
  let msg;
  try { msg = JSON.parse(line); } catch { return; }
  if (msg.method === 'initialize') {
    send({ jsonrpc: '2.0', id: msg.id, result: {
      protocolVersion: '2024-11-05',
      capabilities: { tools: {} },
      serverInfo: { name: 'test-server', version: '0.0.1' },
    }});
    return;
  }
  if (msg.method === 'tools/list') {
    send({ jsonrpc: '2.0', id: msg.id, result: { tools: [{
      name: 'echo',
      description: '把输入原样返回',
      inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] },
    }]}});
    return;
  }
  if (msg.method === 'tools/call') {
    const text = (msg.params.arguments || {}).text || '';
    send({ jsonrpc: '2.0', id: msg.id, result: {
      content: [{ type: 'text', text: 'echo: ' + text }],
      isError: false,
    }});
    return;
  }
  if (msg.id !== undefined) {
    send({ jsonrpc: '2.0', id: msg.id, error: { code: -32601, message: 'unknown method' } });
  }
});
"#;

    fn node_available() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn write_script() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("shania-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("server.js");
        std::fs::write(&path, SERVER_JS).unwrap();
        path
    }

    fn config_for(script: &std::path::Path) -> McpFile {
        let mut servers = BTreeMap::new();
        servers.insert(
            "test".to_string(),
            ServerConfig {
                command: "node".to_string(),
                args: vec![script.to_string_lossy().to_string()],
                env: BTreeMap::new(),
                enabled: true,
            },
        );
        McpFile { servers }
    }

    #[tokio::test]
    async fn handshake_lists_and_calls_tool() {
        if !node_available() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let script = write_script();
        let mut hub = McpHub::connect_with(config_for(&script)).await;

        assert!(hub.warnings.is_empty(), "连接告警：{:?}", hub.warnings);
        assert_eq!(hub.tool_count(), 1, "应该发现一个工具");
        assert!(hub.has_tool("mcp__test__echo"));

        let (ok, text) = hub
            .call("mcp__test__echo", r#"{"text":"你好"}"#)
            .await
            .expect("工具调用应返回结果");
        assert!(ok, "工具应执行成功");
        assert!(text.contains("echo: 你好"), "实际返回：{text}");

        hub.shutdown().await;
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[tokio::test]
    async fn unknown_tool_returns_none() {
        let hub = McpHub::default();
        assert!(!hub.has_tool("mcp__nope__nope"));
    }

    #[test]
    fn sanitizes_tool_names() {
        assert_eq!(sanitize("mcp__fs__read.file"), "mcp__fs__read_file");
        assert!(sanitize(&"x".repeat(100)).len() <= 64);
    }
}
