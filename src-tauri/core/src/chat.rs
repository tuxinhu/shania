use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::events::{EventSink, StreamEvent};
use crate::mcp::McpHub;
use crate::tools;

/// 单条工具结果回灌给模型时的字符上限。
/// 目录遍历、文件读取等工具可能返回极长内容，原样回灌会直接挤爆上下文。
/// 前端仍能拿到完整结果用于展示，只有送给模型的那一份被截断。
const MAX_TOOL_RESULT_CHARS: usize = 12_000;

#[derive(Debug, Deserialize, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatConfig {
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub tools_enabled: Option<bool>,
    #[serde(default)]
    pub max_rounds: Option<u32>,
    /// 是否启用 ~/.shania/mcp.json 里的 MCP server（缺省启用）
    #[serde(default)]
    pub mcp_enabled: Option<bool>,
}

impl ChatConfig {
    /// 只用地址和模型名构造，其余走默认值。CLI 的配置合并逻辑会用到。
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: String::new(),
            model: model.into(),
            temperature: None,
            max_tokens: None,
            system_prompt: None,
            tools_enabled: None,
            max_rounds: None,
            mcp_enabled: None,
        }
    }
}

/// 进行中请求的取消开关表。
/// 从 Tauri 的 `State` 里剥出来的纯结构，桌面端和 CLI 都能用。
#[derive(Default)]
pub struct CancelRegistry {
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl CancelRegistry {
    /// 登记一个请求，返回它的取消开关
    pub fn register(&self, request_id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut map) = self.cancels.lock() {
            map.insert(request_id.to_string(), flag.clone());
        }
        flag
    }

    /// 请求结束后清理，避免 map 无限增长
    pub fn release(&self, request_id: &str) {
        if let Ok(mut map) = self.cancels.lock() {
            map.remove(request_id);
        }
    }

    /// 请求停止；返回是否命中
    pub fn cancel(&self, request_id: &str) -> bool {
        if let Ok(map) = self.cancels.lock() {
            if let Some(flag) = map.get(request_id) {
                flag.store(true, Ordering::Relaxed);
                return true;
            }
        }
        false
    }
}

enum RoundError {
    Cancelled,
    Failed(String),
}

#[derive(Default, Clone)]
struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct RoundOutcome {
    content: String,
    reasoning: String,
    tool_calls: Vec<ToolCallAcc>,
    usage: Option<Value>,
}

/// 把用户填的 base url 规范成 chat/completions 完整地址
fn endpoint(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/chat/completions") {
        b.to_string()
    } else if b.ends_with("/v1") || b.ends_with("/openai") {
        format!("{b}/chat/completions")
    } else {
        format!("{b}/v1/chat/completions")
    }
}

fn build_body(
    config: &ChatConfig,
    messages: &[Value],
    with_tools: bool,
    extra_tools: &[Value],
) -> Value {
    let mut body = json!({
        "model": config.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });

    if let Some(t) = config.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = config.max_tokens {
        if m > 0 {
            body["max_tokens"] = json!(m);
        }
    }
    if with_tools {
        // 内置工具 + MCP 工具合并成一份清单
        let mut defs: Vec<Value> = tools::definitions()
            .as_array()
            .cloned()
            .unwrap_or_default();
        defs.extend(extra_tools.iter().cloned());
        if !defs.is_empty() {
            body["tools"] = json!(defs);
            body["tool_choice"] = json!("auto");
        }
    }
    body
}

/// 把流式累积的工具调用落到结果里。
/// 注意：[DONE] 是正常终止路径，必须在这里收尾，否则工具调用会丢失。
fn flush_tool_calls(outcome: &mut RoundOutcome, acc: &mut Vec<ToolCallAcc>) {
    if outcome.tool_calls.is_empty() {
        outcome.tool_calls = std::mem::take(acc)
            .into_iter()
            .filter(|c| !c.name.is_empty())
            .collect();
    }
}

/// 从 base url 里取出主机名（用于判断是否为本地地址）
fn host_of(base: &str) -> String {
    let rest = base.split("://").nth(1).unwrap_or(base);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    // 去掉端口，保留 IPv6 方括号形式
    if let Some(stripped) = host.strip_prefix('[') {
        return stripped.split(']').next().unwrap_or("").to_lowercase();
    }
    host.split(':').next().unwrap_or("").to_lowercase()
}

fn is_local_host(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "0.0.0.0"
}

/// 构建 HTTP 客户端。
/// 本地模型服务必须绕开系统代理：否则请求会被改写成 absolute-form URI，
/// 导致服务端按路径匹配时返回 404。
pub(crate) fn build_client(base_url: &str) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(20));
    if is_local_host(&host_of(base_url)) {
        builder = builder.no_proxy();
    }
    builder.build()
}

/// 消费一次 SSE 响应流，把增量推给 sink
async fn consume_stream(
    resp: reqwest::Response,
    sink: &dyn EventSink,
    cancel: &Arc<AtomicBool>,
) -> Result<RoundOutcome, RoundError> {
    let mut outcome = RoundOutcome::default();
    let mut acc: Vec<ToolCallAcc> = Vec::new();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            return Err(RoundError::Cancelled);
        }

        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => return Err(RoundError::Failed(format!("读取流失败：{e}"))),
        };

        // 统一换行：CRLF 可能被切在两个 chunk 之间，必须对缓冲区整体规范化，
        // 否则 "\n\n" 事件分隔符会因中间的 \r 而匹配失败，整条流被一直缓冲。
        buf.push_str(&String::from_utf8_lossy(&chunk));
        if buf.contains('\r') {
            buf = buf.replace("\r\n", "\n");
        }

        // SSE 以空行分隔事件
        loop {
            let Some(pos) = buf.find("\n\n") else { break };
            let raw: String = buf.drain(..pos + 2).collect();
            let payload = raw
                .lines()
                .filter_map(|l| l.trim_end_matches('\r').strip_prefix("data:"))
                .map(|l| l.trim())
                .collect::<Vec<_>>()
                .join("\n");

            if payload.is_empty() {
                continue;
            }
            if payload == "[DONE]" {
                flush_tool_calls(&mut outcome, &mut acc);
                return Ok(outcome);
            }

            let value: Value = match serde_json::from_str(&payload) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // 服务端错误体
            if let Some(err) = value.get("error") {
                if !err.is_null() {
                    let msg = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("模型服务返回错误")
                        .to_string();
                    return Err(RoundError::Failed(msg));
                }
            }

            if let Some(usage) = value.get("usage") {
                if !usage.is_null() {
                    outcome.usage = Some(usage.clone());
                }
            }

            let delta = &value["choices"][0]["delta"];
            if !delta.is_object() {
                continue;
            }

            // 思维链（DeepSeek / Qwen 等）
            let reasoning = delta
                .get("reasoning_content")
                .and_then(|v| v.as_str())
                .or_else(|| delta.get("reasoning").and_then(|v| v.as_str()));
            if let Some(r) = reasoning {
                if !r.is_empty() {
                    outcome.reasoning.push_str(r);
                    sink.emit(StreamEvent::Reasoning { delta: r.to_string() });
                }
            }

            if let Some(c) = delta.get("content").and_then(|v| v.as_str()) {
                if !c.is_empty() {
                    outcome.content.push_str(c);
                    sink.emit(StreamEvent::Content { delta: c.to_string() });
                }
            }

            if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    while acc.len() <= idx {
                        acc.push(ToolCallAcc::default());
                    }
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            acc[idx].id = id.to_string();
                        }
                    }
                    if let Some(name) = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                    {
                        if !name.is_empty() {
                            acc[idx].name = name.to_string();
                        }
                    }
                    if let Some(args) = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                    {
                        acc[idx].arguments.push_str(args);
                    }
                }
            }
        }
    }

    flush_tool_calls(&mut outcome, &mut acc);
    Ok(outcome)
}

/// 发起一次模型请求并消费流
async fn run_round(
    client: &reqwest::Client,
    config: &ChatConfig,
    messages: &[Value],
    sink: &dyn EventSink,
    cancel: &Arc<AtomicBool>,
    mcp_defs: &[Value],
) -> Result<RoundOutcome, RoundError> {
    let body = build_body(
        config,
        messages,
        config.tools_enabled.unwrap_or(true),
        mcp_defs,
    );
    let url = endpoint(&config.base_url);

    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .json(&body);

    if !config.api_key.trim().is_empty() {
        req = req.bearer_auth(config.api_key.trim());
    }

    let resp = req.send().await.map_err(|e| {
        RoundError::Failed(format!(
            "无法连接模型服务（{url}）：{e}\n请检查接口地址、网络或代理设置。"
        ))
    })?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| {
                let t = text.trim();
                if t.is_empty() {
                    "无响应内容".to_string()
                } else if t.chars().count() > 400 {
                    let preview: String = t.chars().take(400).collect();
                    format!("{preview}…")
                } else {
                    t.to_string()
                }
            });
        return Err(RoundError::Failed(format!(
            "模型服务返回 {status}：{detail}"
        )));
    }

    // 少数服务不支持 SSE，直接返回整段 JSON
    let is_sse = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("event-stream"))
        .unwrap_or(true);

    if !is_sse {
        let text = resp
            .text()
            .await
            .map_err(|e| RoundError::Failed(format!("读取响应失败：{e}")))?;
        let value: Value = serde_json::from_str(&text).map_err(|_| {
            let preview: String = text.chars().take(300).collect();
            RoundError::Failed(format!("模型返回了无法解析的内容：{preview}"))
        })?;
        return outcome_from_completion(value, sink);
    }

    consume_stream(resp, sink, cancel).await
}

/// 解析非流式的完整响应
fn outcome_from_completion(
    value: Value,
    sink: &dyn EventSink,
) -> Result<RoundOutcome, RoundError> {
    if let Some(err) = value.get("error") {
        if !err.is_null() {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("模型服务返回错误");
            return Err(RoundError::Failed(msg.to_string()));
        }
    }

    let msg = &value["choices"][0]["message"];
    let mut outcome = RoundOutcome::default();

    if let Some(r) = msg.get("reasoning_content").and_then(|v| v.as_str()) {
        if !r.is_empty() {
            outcome.reasoning = r.to_string();
            sink.emit(StreamEvent::Reasoning {
                delta: r.to_string(),
            });
        }
    }

    if let Some(c) = msg.get("content").and_then(|v| v.as_str()) {
        if !c.is_empty() {
            outcome.content = c.to_string();
            sink.emit(StreamEvent::Content {
                delta: c.to_string(),
            });
        }
    }

    if let Some(calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
        for c in calls {
            outcome.tool_calls.push(ToolCallAcc {
                id: c.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                name: c["function"]["name"].as_str().unwrap_or("").to_string(),
                arguments: c["function"]["arguments"]
                    .as_str()
                    .unwrap_or("{}")
                    .to_string(),
            });
        }
    }

    if let Some(u) = value.get("usage") {
        if !u.is_null() {
            outcome.usage = Some(u.clone());
        }
    }

    Ok(outcome)
}

/// 跑一次完整对话：拼系统提示 → 多轮 agent 循环 → 收尾事件。
///
/// 这是桌面端与 CLI 唯一的公共入口。`Done` / `Error` 事件也由这里发出，
/// 保证两个前端看到的事件序列完全一致。
///
/// 成功时返回**更新后的上下文**（含工具调用与工具结果），
/// 多轮交互的前端（CLI 的 REPL）拿它当下一次请求的输入即可。
pub async fn run_chat(
    config: &ChatConfig,
    messages: Vec<Value>,
    sink: &dyn EventSink,
    cancel: &Arc<AtomicBool>,
) -> Result<Vec<Value>, String> {
    let client = build_client(&config.base_url).map_err(|e| e.to_string())?;

    let mut history: Vec<Value> = Vec::new();
    if let Some(sys) = config.system_prompt.as_ref() {
        if !sys.trim().is_empty() {
            history.push(json!({ "role": "system", "content": sys }));
        }
    }
    history.extend(messages);

    let max_rounds = config.max_rounds.unwrap_or(6).clamp(1, 12);

    // MCP server 按「一轮对话」为生命周期：进来时启动，结束时关掉。
    // 连不上不会中断对话，只把原因写进日志，避免界面报错却看不出所以然。
    let mut mcp = if config.mcp_enabled.unwrap_or(true) {
        let hub = McpHub::connect().await;
        for warning in hub.warnings.iter() {
            crate::log_warn!("MCP：{warning}");
        }
        hub
    } else {
        McpHub::default()
    };

    let outcome = run_agent_loop(
        &client,
        config,
        &mut history,
        sink,
        cancel,
        max_rounds,
        &mut mcp,
    )
    .await;

    mcp.shutdown().await;

    match outcome {
        Ok(()) => Ok(history),
        Err(RoundError::Cancelled) => {
            sink.emit(StreamEvent::Done { usage: None });
            Ok(history)
        }
        Err(RoundError::Failed(msg)) => {
            sink.emit(StreamEvent::Error {
                message: msg.clone(),
            });
            Err(msg)
        }
    }
}

/// 按字符（而非字节）截断，避免把中文切坏。
fn truncate_for_model(text: &str) -> String {
    let count = text.chars().count();
    if count <= MAX_TOOL_RESULT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_TOOL_RESULT_CHARS).collect();
    format!("{head}\n\n…（工具输出过长，已截断，原文共 {count} 字符）")
}

/// 连通性测试：发一个最小的非流式请求，同时验证地址、密钥、模型名。
/// 比只请求 /models 更通用——不少自建服务并不实现 /models。
pub async fn test_connection(config: &ChatConfig) -> Result<Value, String> {
    let client = build_client(&config.base_url).map_err(|e| e.to_string())?;
    let url = endpoint(&config.base_url);

    let body = json!({
        "model": config.model,
        "messages": [{ "role": "user", "content": "ping" }],
        "max_tokens": 1,
        "stream": false,
    });

    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(Duration::from_secs(25));

    if !config.api_key.trim().is_empty() {
        req = req.bearer_auth(config.api_key.trim());
    }

    let started = Instant::now();
    let resp = req.send().await.map_err(|e| {
        let mut hint = format!("连接失败：{e}");
        if e.is_connect() || e.is_timeout() {
            hint.push_str("\n请确认服务已启动、端口正确，以及地址是否被本机代理拦截。");
        }
        hint
    })?;

    let status = resp.status();
    let latency = started.elapsed().as_millis() as u64;
    let text = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| {
                let t = text.trim();
                if t.chars().count() > 300 {
                    let p: String = t.chars().take(300).collect();
                    format!("{p}…")
                } else {
                    t.to_string()
                }
            });

        let hint = match status.as_u16() {
            401 | 403 => "API Key 无效或没有访问该模型的权限。",
            404 => "地址可能少了或多了 /v1，也可能模型名不存在。",
            422 | 400 => "请求被拒绝，通常是模型名写错或参数不被支持。",
            429 => "触发限流，稍后再试或检查配额。",
            500..=599 => "模型服务内部错误，请查看服务端日志。",
            _ => "",
        };

        let mut msg = format!("服务返回 {status}：{detail}");
        if !hint.is_empty() {
            msg.push_str(&format!("\n{hint}"));
        }
        return Err(msg);
    }

    // 解析出实际返回的模型名，便于确认请求真的落到目标模型上
    let actual_model = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["model"].as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| config.model.clone());

    Ok(json!({
        "latencyMs": latency,
        "model": actual_model,
        "endpoint": url,
    }))
}

async fn run_agent_loop(
    client: &reqwest::Client,
    config: &ChatConfig,
    history: &mut Vec<Value>,
    sink: &dyn EventSink,
    cancel: &Arc<AtomicBool>,
    max_rounds: u32,
    mcp: &mut McpHub,
) -> Result<(), RoundError> {
    let mut last_usage: Option<Value> = None;

    for round in 0..max_rounds {
        if cancel.load(Ordering::Relaxed) {
            return Err(RoundError::Cancelled);
        }
        sink.emit(StreamEvent::Round { index: round });

        let defs = mcp.definitions().to_vec();
        let outcome = run_round(client, config, history, sink, cancel, &defs).await?;
        if let Some(u) = outcome.usage {
            last_usage = Some(u);
        }

        if outcome.tool_calls.is_empty() {
            sink.emit(StreamEvent::Done {
                usage: last_usage.clone(),
            });
            return Ok(());
        }

        // 把 assistant 的工具调用意图写回上下文
        let calls: Vec<Value> = outcome
            .tool_calls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let id = if c.id.is_empty() {
                    format!("call_{round}_{i}")
                } else {
                    c.id.clone()
                };
                json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": c.name, "arguments": if c.arguments.is_empty() { "{}" } else { c.arguments.as_str() } }
                })
            })
            .collect();

        let mut assistant = json!({
            "role": "assistant",
            "content": outcome.content,
            "tool_calls": calls,
        });
        if !outcome.reasoning.is_empty() {
            assistant["reasoning_content"] = json!(outcome.reasoning);
        }
        history.push(assistant);

        // 依次执行工具
        for (i, call) in outcome.tool_calls.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(RoundError::Cancelled);
            }
            let id = calls[i]["id"].as_str().unwrap_or("call").to_string();
            let args = if call.arguments.trim().is_empty() {
                "{}".to_string()
            } else {
                call.arguments.clone()
            };

            sink.emit(StreamEvent::ToolStart {
                id: id.clone(),
                name: call.name.clone(),
                arguments: args.clone(),
            });

            let started = Instant::now();
            let (ok, result) = if tools::is_builtin(&call.name) {
                tools::execute(&call.name, &args).await
            } else if let Some(res) = mcp.call(&call.name, &args).await {
                res
            } else {
                (false, format!("未知工具：{}", call.name))
            };
            let elapsed = started.elapsed().as_millis() as u64;

            sink.emit(StreamEvent::ToolResult {
                id: id.clone(),
                name: call.name.clone(),
                ok,
                result: result.clone(),
                elapsed_ms: elapsed,
            });

            history.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": truncate_for_model(&result),
            }));
        }
    }

    sink.emit(StreamEvent::Done { usage: last_usage });
    Ok(())
}
