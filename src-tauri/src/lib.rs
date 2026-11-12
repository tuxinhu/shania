mod chat;
mod cli_args;
mod export;
mod settings;
mod stt_stream;
mod storage;
mod tools;
mod tts_cmd;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use shania_core::CancelRegistry;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_opener::OpenerExt;

/// 全局会话态：记录每个请求的取消开关。
/// 表本身在 core 里（命令行程序用的是同一份实现），这里只是挂成 Tauri 的托管状态。
#[derive(Default)]
pub struct ChatState {
    pub cancels: CancelRegistry,
}

/// 托盘常驻相关状态：区分「关闭窗口」与「真正退出」
#[derive(Default)]
pub struct TrayState {
    pub quitting: AtomicBool,
}

/// 界面偏好：关闭按钮的行为由前端设置决定
#[derive(Default)]
pub struct UiPrefs {
    /// "tray" = 收起到托盘（默认）；"quit" = 直接退出
    pub close_action: Mutex<String>,
}

/// 托盘菜单项句柄：改键后需要更新「显示主窗口」右侧的快捷键提示
#[derive(Default)]
pub struct TrayMenuState {
    pub show_item: Mutex<Option<MenuItem<tauri::Wry>>>,
}

/// 全局快捷键状态：记录当前生效的组合键，便于切换时注销旧键
#[derive(Default)]
pub struct ShortcutState {
    pub current: Mutex<Option<String>>,
}

/// 默认唤起快捷键（CmdOrCtrl 在 Windows 上即 Ctrl，macOS 上是 Command）
const DEFAULT_SHORTCUT: &str = "CmdOrCtrl+Shift+Space";

/// 应用一个快捷键：传空串表示只注销当前键（录制新键时用）
fn apply_shortcut(app: &AppHandle, accelerator: &str) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

    let acc = accelerator.trim();
    let state = app
        .try_state::<ShortcutState>()
        .ok_or_else(|| "快捷键状态未初始化".to_string())?;
    let mut current = state.current.lock().map_err(|_| "快捷键状态异常".to_string())?;

    // 空串：仅注销，不注册
    if acc.is_empty() {
        if let Some(old) = current.take() {
            if let Ok(old) = old.parse::<Shortcut>() {
                let _ = app.global_shortcut().unregister(old);
            }
        }
        return Ok(());
    }

    if current.as_deref() == Some(acc) {
        return Ok(());
    }

    let shortcut: Shortcut = acc
        .parse()
        .map_err(|e| format!("无法识别的组合键「{acc}」：{e}"))?;

    // 先注册新键，成功后再注销旧键。
    // 这样注册失败时旧键仍然可用，用户不会突然失去唤起入口。
    app.global_shortcut()
        .register(shortcut)
        .map_err(|e| format!("该组合键不可用（可能被其他程序占用）：{e}"))?;

    if let Some(old) = current.take() {
        if let Ok(old) = old.parse::<Shortcut>() {
            let _ = app.global_shortcut().unregister(old);
        }
    }

    *current = Some(acc.to_string());
    update_tray_shortcut(app, acc);
    Ok(())
}

/// 把当前快捷键写进托盘菜单项，让用户随时能看到「按什么键能唤出来」
fn update_tray_shortcut(app: &AppHandle, accelerator: &str) {
    let Some(state) = app.try_state::<TrayMenuState>() else {
        return;
    };
    let Ok(guard) = state.show_item.lock() else {
        return;
    };
    if let Some(item) = guard.as_ref() {
        let text = if accelerator.trim().is_empty() {
            "显示主窗口".to_string()
        } else {
            format!("显示主窗口　{}", pretty_accelerator(accelerator))
        };
        let _ = item.set_text(text);
    }
}

/// 把配置里的写法转成用户看得懂的按键名
fn pretty_accelerator(acc: &str) -> String {
    let is_mac = cfg!(target_os = "macos");
    acc.split('+')
        .map(|part| match part.trim() {
            "CmdOrCtrl" | "CommandOrControl" => {
                if is_mac {
                    "⌘"
                } else {
                    "Ctrl"
                }
            }
            "Ctrl" | "Control" => {
                if is_mac {
                    "⌃"
                } else {
                    "Ctrl"
                }
            }
            "Shift" => {
                if is_mac {
                    "⇧"
                } else {
                    "Shift"
                }
            }
            "Alt" | "Option" => {
                if is_mac {
                    "⌥"
                } else {
                    "Alt"
                }
            }
            other => other,
        })
        .collect::<Vec<_>>()
        .join(if is_mac { "" } else { "+" })
}

/// 前端启动时同步「关闭按钮行为」，默认收起到托盘
#[tauri::command]
fn set_close_action(app: AppHandle, action: String) -> Result<(), String> {
    let state = app
        .try_state::<UiPrefs>()
        .ok_or_else(|| "界面偏好未初始化".to_string())?;
    let mut cur = state.close_action.lock().map_err(|_| "状态异常".to_string())?;
    *cur = if action == "quit" { "quit" } else { "tray" }.to_string();
    Ok(())
}

/// 前端改键时调用，返回实际生效的组合键
#[tauri::command]
fn set_global_shortcut(app: AppHandle, accelerator: String) -> Result<String, String> {
    apply_shortcut(&app, &accelerator)?;
    Ok(accelerator.trim().to_string())
}

pub(crate) fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// 已可见且未最小化 → 收起；否则唤出并聚焦
fn toggle_main_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let visible = win.is_visible().unwrap_or(false);
    let minimized = win.is_minimized().unwrap_or(false);

    if visible && !minimized {
        let _ = win.hide();
    } else {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// `~/.shania/logs/`
fn logs_dir() -> Result<std::path::PathBuf, String> {
    settings::settings_dir().map(|root| root.join("logs"))
}

/// 给设置面板用：打开日志目录（托盘里也有同一个入口）
#[tauri::command]
fn open_logs_dir(app: AppHandle) -> Result<(), String> {
    let dir = logs_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建日志目录失败：{e}"))?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| format!("打开日志目录失败：{e}"))
}

/// 在资源管理器里打开目录。失败只记日志——托盘上的点击不该弹窗打断用户。
fn open_in_explorer(app: &AppHandle, dir: Result<std::path::PathBuf, String>) {
    match dir {
        Ok(dir) => {
            let _ = std::fs::create_dir_all(&dir);
            if let Err(err) = app.opener().open_path(dir.to_string_lossy(), None::<&str>) {
                shania_core::log_error!("打开目录 {} 失败：{err}", dir.display());
            }
        }
        Err(err) => shania_core::log_error!("定位目录失败：{err}"),
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let new_chat = MenuItem::with_id(app, "new-chat", "新建对话", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
    let open_dir = MenuItem::with_id(app, "open-config-dir", "打开配置目录", true, None::<&str>)?;
    let open_logs = MenuItem::with_id(app, "open-logs-dir", "打开日志目录", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Shania", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &new_chat,
            &settings,
            &open_dir,
            &open_logs,
            &separator,
            &quit,
        ],
    )?;

    // 保存句柄，供改键后刷新菜单文案
    if let Some(state) = app.try_state::<TrayMenuState>() {
        if let Ok(mut slot) = state.show_item.lock() {
            *slot = Some(show.clone());
        }
    }

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("Shania Agent")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main_window(app),
            "new-chat" => {
                show_main_window(app);
                let _ = app.emit("new-chat", ());
            }
            "settings" => {
                show_main_window(app);
                let _ = app.emit("open-settings", ());
            }
            "open-config-dir" => {
                // 配置、会话、MCP 全在 ~/.shania/，直接打开省得用户自己翻
                open_in_explorer(app, settings::settings_dir());
            }
            "open-logs-dir" => {
                // 排查问题时用户第一件事就是找日志，别让他自己摸路径
                open_in_explorer(app, logs_dir());
            }
            "quit" => {
                if let Some(state) = app.try_state::<TrayState>() {
                    state.quitting.store(true, Ordering::Relaxed);
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.build(app)?;
    Ok(())
}

fn register_global_shortcut(app: &AppHandle) {
    // 先注册默认键，保证页面加载前就有唤起入口；
    // 前端读取到用户自定义的键之后会立刻覆盖它。
    if let Err(err) = apply_shortcut(app, DEFAULT_SHORTCUT) {
        shania_core::log_warn!("全局快捷键注册失败（可能被其他程序占用）：{err}");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 日志要在所有东西之前就位：启动早期出的问题（配置目录不可写、插件初始化失败）
    // 恰恰是最需要留痕的。桌面端 release 没有控制台，eprintln 等于扔掉。
    if let Ok(root) = settings::settings_dir() {
        shania_core::logging::init(&root.join("logs"));
    }

    tauri::Builder::default()
        // 单实例插件必须第一个注册：后续插件都依赖唯一的进程实例
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // 再次启动时唤出已有窗口，而不是起第二个实例。
            // 带了参数就交给前端处理（args[0] 是 exe 路径，必须跳过，
            // 否则它会被当成用户输入的一句话）
            match cli_args::parse(args.get(1..).unwrap_or(&[])) {
                Some(action) => cli_args::dispatch(app, action),
                None => show_main_window(app),
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // 只记忆尺寸/位置/最大化，不记忆可见性：
        // 否则「收起到托盘后退出」会把「隐藏」存下来，下次启动窗口是隐形的
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        toggle_main_window(app);
                    }
                })
                .build(),
        )
        .manage(ChatState::default())
        .manage(cli_args::CliArgsState::default())
        .manage(TrayState::default())
        .manage(ShortcutState::default())
        .manage(UiPrefs::default())
        .manage(TrayMenuState::default())
        .manage(stt_stream::StreamState::default())
        .manage(tts_cmd::TtsState::default())
        .invoke_handler(tauri::generate_handler![
            chat::chat_stream,
            chat::stop_chat,
            chat::test_connection,
            storage::load_sessions,
            storage::save_sessions,
            settings::load_settings,
            settings::save_settings,
            settings::settings_location,
            export::write_text_file,
            export::read_import_file,
            stt_stream::stt_stream_start,
            stt_stream::stt_stream_feed,
            stt_stream::stt_stream_stop,
            stt_stream::stt_stream_install,
            stt_stream::stt_stream_unload,
            stt_stream::stt_stream_status,
            tts_cmd::tts_say,
            tts_cmd::tts_status,
            tts_cmd::tts_install,
            tts_cmd::tts_unload,
            tts_cmd::tts_warmup,
            set_global_shortcut,
            set_close_action,
            cli_args::take_cli_args,
            tools::list_tools,
            open_logs_dir,
        ])
        .setup(|app| {
            let handle = app.handle();

            // 每次启动写一条「谁、在哪、哪个版本」：多份日志混在一起时靠这行区分
            shania_core::log_info!(
                "Shania {} 启动（{}），日志：{}",
                env!("CARGO_PKG_VERSION"),
                std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "未知路径".into()),
                shania_core::logging::path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "不可用（目录不可写）".into()),
            );

            // 首次运行落一份 mcp.json 模板，用户从托盘「打开配置目录」就能直接看到并改
            if let Some(path) = shania_core::mcp::ensure_config() {
                shania_core::log_info!("已生成 MCP 配置模板：{}", path.display());
            }

            if let Err(err) = build_tray(handle) {
                shania_core::log_error!("托盘创建失败：{err}");
            }
            register_global_shortcut(handle);

            // 识别模型在后台预热（加载要几秒），不阻塞窗口显示。
            // 设置里关掉「启动时预加载」就退回按需加载。
            stt_stream::preload(handle);
            // 合成器同理：348MB 模型读盘更慢，晚一点起，别跟窗口绘制抢 CPU
            tts_cmd::preload(handle);

            // 首启带参数时前端还没加载，先存着，等它挂载后主动来取。
            // 这里不能 emit——WebView 里的 JS 还不存在，事件会直接丢掉。
            let startup_args: Vec<String> = std::env::args().skip(1).collect();
            if let Some(action) = cli_args::parse(&startup_args) {
                cli_args::store_initial(handle, action);
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();

                let quitting = app
                    .try_state::<TrayState>()
                    .map(|s| s.quitting.load(Ordering::Relaxed))
                    .unwrap_or(false);

                if quitting {
                    return;
                }

                // 前端可配置：收起到托盘（默认）或直接退出
                let action = app
                    .try_state::<UiPrefs>()
                    .and_then(|s| s.close_action.lock().ok().map(|g| g.clone()))
                    .unwrap_or_else(|| "tray".to_string());

                if action == "quit" {
                    if let Some(state) = app.try_state::<TrayState>() {
                        state.quitting.store(true, Ordering::Relaxed);
                    }
                    // 不拦截关闭：跟随系统退出流程，让 window-state 正常落盘
                } else {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // 识别器是进程内常驻对象，进程退出即释放，没有子进程要收尾
            if let tauri::RunEvent::Exit = event {
                shania_core::log_info!("Shania 退出");
            }
        });
}
