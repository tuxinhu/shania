//! Tauri 命令层：只负责「把 core 的事件塞进 Channel」，不持有任何 Agent 逻辑。
//!
//! 真正的 SSE 解析与多轮工具循环在 `shania-core` 里，命令行程序用的是同一份。

use serde_json::Value;
use shania_core::chat::{run_chat, test_connection as core_test_connection};
use shania_core::events::{EventSink, StreamEvent};
use shania_core::ChatConfig;
use tauri::ipc::Channel;
use tauri::State;

use crate::ChatState;

/// 把核心层的抽象 sink 落到 Tauri 的 IPC 通道上
struct ChannelSink(Channel<StreamEvent>);

impl EventSink for ChannelSink {
    fn emit(&self, event: StreamEvent) {
        // 前端已关闭时发送失败属于正常情况，忽略即可
        let _ = self.0.send(event);
    }
}

#[tauri::command]
pub async fn chat_stream(
    state: State<'_, ChatState>,
    request_id: String,
    config: ChatConfig,
    messages: Vec<Value>,
    channel: Channel<StreamEvent>,
) -> Result<(), String> {
    let cancel = state.cancels.register(&request_id);
    let sink = ChannelSink(channel);

    // 前端不关心更新后的上下文（会话由前端自己维护），所以只取成败
    let result = run_chat(&config, messages, &sink, &cancel)
        .await
        .map(|_history| ());

    // 收尾：成功失败都要清理，否则这张表会随请求数一直涨
    state.cancels.release(&request_id);
    result
}

#[tauri::command]
pub fn stop_chat(state: State<'_, ChatState>, request_id: String) -> Result<(), String> {
    state.cancels.cancel(&request_id);
    Ok(())
}

#[tauri::command]
pub async fn test_connection(config: ChatConfig) -> Result<Value, String> {
    core_test_connection(&config).await
}
