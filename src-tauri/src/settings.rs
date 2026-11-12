use std::fs;
use std::path::PathBuf;

/// 用户级配置目录：桌面端与 CLI 共用同一个 `~/.shania/`。
///
/// 这里刻意不放在 `app_data_dir()` 下——那个目录带 bundle 标识
/// （`%APPDATA%\com.shania.agent`），CLI 拿不到，两边就会各存一份设置。
pub fn settings_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| "无法定位用户主目录".to_string())?;
    Ok(PathBuf::from(home).join(".shania"))
}

pub fn settings_path() -> Result<PathBuf, String> {
    Ok(settings_dir()?.join("settings.json"))
}

/// 读取共享设置。文件不存在时返回 `None`，由前端决定用默认值还是迁移旧数据。
#[tauri::command]
pub fn load_settings() -> Result<Option<String>, String> {
    let path = settings_path()?;
    if !path.exists() {
        return Ok(None);
    }
    fs::read_to_string(&path)
        .map(Some)
        .map_err(|e| format!("读取设置失败：{e}"))
}

/// 写入共享设置。先写临时文件再改名，避免写一半崩溃留下半个 JSON。
#[tauri::command]
pub fn save_settings(json: String) -> Result<(), String> {
    // 前端传来的必须先是合法 JSON，否则报错而不是写入坏文件
    serde_json::from_str::<serde_json::Value>(&json)
        .map_err(|e| format!("设置数据格式错误：{e}"))?;

    let path = settings_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败：{e}"))?;
    }

    let tmp = path.with_file_name("settings.json.tmp");
    fs::write(&tmp, json.as_bytes()).map_err(|e| format!("写入临时文件失败：{e}"))?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("替换旧设置文件失败：{e}"))?;
    }
    fs::rename(&tmp, &path).map_err(|e| format!("保存设置失败：{e}"))?;
    Ok(())
}

/// 供前端展示「设置文件在哪」，省得用户自己翻目录
#[tauri::command]
pub fn settings_location() -> Result<String, String> {
    Ok(settings_path()?.to_string_lossy().to_string())
}
