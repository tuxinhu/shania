//! Shania Agent 的 Agent 核心。
//!
//! 这里只做「跟模型说话」这件事：拼请求、解析 SSE、跑多轮工具循环、执行内置工具。
//! 刻意不依赖 Tauri，好让桌面端和命令行共用同一份逻辑——改一处，两边同时生效。
//!
//! 前端如何呈现不在本 crate 的职责范围内：调用方实现 [`EventSink`] 即可。

pub mod chat;
pub mod events;
/// 定稿纠错（SenseVoice 整句重听）。和 `stream_stt` 一样挂在同一个 feature 下：
/// 它俩共用一份 sherpa-onnx 运行时，拆开没有意义。
#[cfg(feature = "stream-stt")]
pub mod finalize_stt;
pub mod logging;
pub mod mcp;
pub mod simplify;
mod simplify_table;
pub mod stt_setup;
/// 流式识别。用 feature 隔开是因为它把 sherpa-onnx 和 onnxruntime 静态链接进来
/// （命令行二进制会从几 MB 涨到几十 MB），而 CLI 根本用不到语音输入。
#[cfg(feature = "stream-stt")]
pub mod stream_stt;
pub mod tools;
pub mod transcribe;

pub use chat::{run_chat, test_connection, CancelRegistry, ChatConfig};
pub use events::{EventSink, FnSink, NullSink, StreamEvent};
pub use mcp::McpHub;
pub use simplify::to_simplified;
pub use transcribe::{decode_audio, transcribe, SttConfig};
