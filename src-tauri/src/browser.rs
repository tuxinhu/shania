//! 内置浏览器：在主窗口里挂一个**子 webview**（Tauri 多 webview），
//! 而不是拿 iframe 硬凑——iframe 会被绝大多数站点的 X-Frame-Options 挡掉。
//!
//! 两个必须知道的约束：
//! 1. 子 webview 是操作系统原生控件，**永远盖在 HTML 之上**，所以它所在的区域
//!    不能有下拉菜单、tooltip 这类浮层，否则会被挡住。
//! 2. 它不参与 CSS 布局，位置和尺寸都要前端算好再发过来（browser_bounds），
//!    窗口缩放、面板拖宽、侧栏开合都得重新上报一次。

use tauri::{
    webview::WebviewBuilder, AppHandle, LogicalPosition, LogicalSize, Manager, WebviewUrl,
};

const LABEL: &str = "browser";

/// 把地址栏里的输入补全成合法 URL：
/// `example.com` → https://example.com，`rust tauri` → 必应搜索
fn normalize(input: &str) -> Result<tauri::Url, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("地址为空".to_string());
    }

    let full = if raw.contains("://") || raw.starts_with("about:") || raw.starts_with("data:") {
        raw.to_string()
    } else if raw.contains('.') && !raw.contains(' ') {
        // 看着像域名（有点、没空格）就补 https
        format!("https://{raw}")
    } else {
        format!("https://www.bing.com/search?q={}", encode_query(raw))
    };

    full.parse::<tauri::Url>()
        .map_err(|e| format!("地址无效：{e}"))
}

/// 搜索词要 percent-encode；手写比多引一个 crate 划算
fn encode_query(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn place(webview: &tauri::Webview, x: f64, y: f64, width: f64, height: f64) {
    // 尺寸可能为 0（面板正在收起），给个下限免得平台层报错
    let _ = webview.set_position(LogicalPosition::new(x, y));
    let _ = webview.set_size(LogicalSize::new(width.max(1.0), height.max(1.0)));
}

/// 打开/复用浏览器视图并定位。url 为空表示「只调位置，别导航」。
#[tauri::command]
pub fn browser_open(
    app: AppHandle,
    url: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let existing = app.get_webview(LABEL);

    if let Some(webview) = existing {
        if !url.trim().is_empty() {
            webview
                .navigate(normalize(&url)?)
                .map_err(|e| format!("导航失败：{e}"))?;
        }
        place(&webview, x, y, width, height);
        return Ok(());
    }

    let window = app.get_window("main").ok_or("主窗口不存在")?;
    let target = normalize(if url.trim().is_empty() { "about:blank" } else { &url })?;
    window
        .add_child(
            WebviewBuilder::new(LABEL, WebviewUrl::External(target)),
            LogicalPosition::new(x, y),
            LogicalSize::new(width.max(1.0), height.max(1.0)),
        )
        .map_err(|e| format!("创建浏览器视图失败：{e}"))?;
    Ok(())
}

/// 面板尺寸/位置变了就调它（前端的 ResizeObserver 驱动）
#[tauri::command]
pub fn browser_bounds(app: AppHandle, x: f64, y: f64, width: f64, height: f64) -> Result<(), String> {
    if let Some(webview) = app.get_webview(LABEL) {
        place(&webview, x, y, width, height);
    }
    Ok(())
}

#[tauri::command]
pub fn browser_navigate(app: AppHandle, url: String) -> Result<(), String> {
    let webview = app.get_webview(LABEL).ok_or("浏览器还没打开")?;
    webview
        .navigate(normalize(&url)?)
        .map_err(|e| format!("导航失败：{e}"))
}

#[tauri::command]
pub fn browser_script(app: AppHandle, script: String) -> Result<(), String> {
    let webview = app.get_webview(LABEL).ok_or("浏览器还没打开")?;
    webview.eval(&script).map_err(|e| e.to_string())
}

/// 地址栏同步用：拿到子 webview 当前真实地址（用户点了链接之后也会变）
#[tauri::command]
pub fn browser_url(app: AppHandle) -> Option<String> {
    let webview = app.get_webview(LABEL)?;
    webview.url().ok().map(|u| u.to_string())
}

#[tauri::command]
pub fn browser_close(app: AppHandle) -> Result<(), String> {
    if let Some(webview) = app.get_webview(LABEL) {
        webview.close().map_err(|e| format!("关闭失败：{e}"))?;
    }
    Ok(())
}
