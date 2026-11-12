use std::fs;
use std::path::PathBuf;

/// 把导出内容写到用户选定的路径。
/// 路径来自前端调起的原生保存对话框，这里只负责落盘，不做路径猜测。
#[tauri::command]
pub fn write_text_file(path: String, content: String) -> Result<(), String> {
    let target = PathBuf::from(path.trim());
    if target.as_os_str().is_empty() {
        return Err("导出路径为空".to_string());
    }

    if let Some(dir) = target.parent() {
        if !dir.as_os_str().is_empty() && !dir.exists() {
            fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
        }
    }

    fs::write(&target, content.as_bytes()).map_err(|e| format!("写入文件失败：{e}"))
}

/// 读取用户选定的导入文件（JSON）。
/// 与「导出」对称：路径来自原生打开对话框，这里只做尺寸校验和读取。
#[tauri::command]
pub fn read_import_file(path: String) -> Result<String, String> {
    let target = PathBuf::from(path.trim());
    if target.as_os_str().is_empty() {
        return Err("导入路径为空".to_string());
    }

    let meta = fs::metadata(&target).map_err(|e| format!("无法访问文件：{e}"))?;
    if meta.is_dir() {
        return Err("选中的是一个目录，请选择 JSON 文件".to_string());
    }
    // 会话导出文件通常几十 KB；32MB 足够，超过基本是选错文件了
    if meta.len() > 32 * 1024 * 1024 {
        return Err(format!("文件过大（{} 字节），超过 32MB 限制", meta.len()));
    }

    fs::read_to_string(&target).map_err(|e| format!("读取失败（需要 UTF-8 文本）：{e}"))
}
