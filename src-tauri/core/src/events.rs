use serde::Serialize;
use serde_json::Value;

/// 流式事件。
///
/// 这是核心层对外输出的唯一通道：桌面端把它逐个塞进 Tauri 的
/// `Channel<StreamEvent>`，CLI 把它渲染成终端输出。字段全部 camelCase，
/// 前端 `lib/types.ts` 里的同名联合类型与之对应。
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum StreamEvent {
    /// 思维链增量
    Reasoning { delta: String },
    /// 正文增量
    Content { delta: String },
    /// 一轮新的模型请求开始（agent 循环）
    Round { index: u32 },
    /// 工具开始执行
    ToolStart {
        id: String,
        name: String,
        arguments: String,
    },
    /// 工具执行完成
    ToolResult {
        id: String,
        name: String,
        ok: bool,
        result: String,
        elapsed_ms: u64,
    },
    /// 全部结束
    Done { usage: Option<Value> },
    /// 出错
    Error { message: String },
}

/// 事件消费者。核心层只认这个抽象，不关心对面是 WebView 还是终端。
pub trait EventSink: Send + Sync {
    fn emit(&self, event: StreamEvent);
}

/// 用一个闭包当 sink，省去为每种前端写一个 struct。
pub struct FnSink<F>(pub F)
where
    F: Fn(StreamEvent) + Send + Sync;

impl<F> EventSink for FnSink<F>
where
    F: Fn(StreamEvent) + Send + Sync,
{
    fn emit(&self, event: StreamEvent) {
        (self.0)(event)
    }
}

/// 丢弃所有事件。用于 `--check` 这类只关心结果的场景。
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: StreamEvent) {}
}
