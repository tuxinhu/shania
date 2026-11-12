//! 桌面端的命令行参数入口。
//!
//! 支持三种用法：
//!   shania-agent.exe                      正常启动
//!   shania-agent.exe --new-chat           唤出窗口并新建对话
//!   shania-agent.exe --settings           唤出窗口并打开设置
//!   shania-agent.exe "帮我看看这个文件"     唤出窗口，新建对话并直接发送
//!
//! 注意两种入口的时序不同：
//!   - 应用已在运行（单实例回调）→ 前端一定已挂载，直接 emit 事件
//!   - 首次启动（setup 阶段）→ 前端还没起来，先存进 state，等它挂载后主动来取

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// 前端监听的事件名
pub const CLI_ARGS_EVENT: &str = "cli-args";

/// 参数解析结果，前端按 `action` 分派
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CliAction {
    NewChat,
    Settings,
    Prompt { text: String },
}

/// 首启参数暂存箱
#[derive(Default)]
pub struct CliArgsState {
    pending: Mutex<Option<CliAction>>,
}

impl CliArgsState {
    fn store(&self, action: CliAction) {
        if let Ok(mut slot) = self.pending.lock() {
            *slot = Some(action);
        }
    }

    fn take(&self) -> Option<CliAction> {
        self.pending.lock().ok().and_then(|mut slot| slot.take())
    }
}

/// 解析参数。
/// 裸文本（不带 `-` 前缀的）拼成一句话；`--prompt=xxx` 也算文本。
/// 未知的 `--flag` 直接忽略，避免打包器或系统塞进来的参数搅乱解析。
pub fn parse(args: &[String]) -> Option<CliAction> {
    const PROMPT_PREFIX: &str = "--prompt=";

    let mut texts: Vec<String> = Vec::new();
    let mut new_chat = false;
    let mut settings = false;

    for raw in args {
        let arg = raw.trim();
        if arg.is_empty() {
            continue;
        }
        match arg {
            "--new-chat" | "-n" => new_chat = true,
            "--settings" => settings = true,
            other if other.starts_with(PROMPT_PREFIX) => {
                texts.push(other[PROMPT_PREFIX.len()..].to_string())
            }
            other if other.starts_with('-') => {}
            other => texts.push(other.to_string()),
        }
    }

    // 带了文本就以文本为准：发消息本身就会新开对话
    if !texts.is_empty() {
        return Some(CliAction::Prompt {
            text: texts.join(" "),
        });
    }
    if settings {
        return Some(CliAction::Settings);
    }
    if new_chat {
        return Some(CliAction::NewChat);
    }
    None
}

/// 应用已在运行时调用：唤出窗口 + 通知前端
pub fn dispatch(app: &AppHandle, action: CliAction) {
    crate::show_main_window(app);
    let _ = app.emit(CLI_ARGS_EVENT, action);
}

/// 首次启动时调用：只存不发，等前端挂载后来取
pub fn store_initial(app: &AppHandle, action: CliAction) {
    if let Some(state) = app.try_state::<CliArgsState>() {
        state.store(action);
    }
}

#[tauri::command]
pub fn take_cli_args(state: State<'_, CliArgsState>) -> Option<CliAction> {
    state.take()
}
