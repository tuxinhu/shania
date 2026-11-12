//! 工具清单的 IPC 出口。
//!
//! 工具本身（定义 + 执行）都在 `shania-core::tools` 里，桌面端只需要把它读出来给前端展示。

use serde_json::Value;

#[tauri::command]
pub fn list_tools() -> Value {
    shania_core::tools::definitions()
}
