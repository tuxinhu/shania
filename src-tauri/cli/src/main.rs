//! Shania CLI —— 把同一个 Agent 核心搬到终端。
//!
//! 复用 `shania-core`，所以工具调用、多轮循环、SSE 解析与桌面端完全一致。
//! 这里只负责三件事：解析参数、拼配置、把事件渲染成终端输出。

use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
mod session;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{json, Value};
use shania_core::events::{EventSink, StreamEvent};
use shania_core::{test_connection, ChatConfig};

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:4000";
const DEFAULT_MODEL: &str = "qwen3-vl-8b";

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let opts = match Options::parse(std::env::args().skip(1).collect()) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("{msg}\n\n{}", help_text());
            return 2;
        }
    };

    if opts.help {
        print!("{}", help_text());
        return 0;
    }
    if opts.version {
        println!("shania {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }

    // 与桌面端共用一份日志：CLI 出的问题也能事后翻，MCP 子进程的 stderr 也落到同一目录
    if let Some(dir) = shania_core::logging::default_dir() {
        shania_core::logging::init(&dir);
    }
    shania_core::log_info!("CLI 启动：{}", std::env::args().skip(1).collect::<Vec<_>>().join(" "));

    // 与桌面端一致：首次运行把 mcp.json 模板落下来，用户想加 server 时有格式可抄
    if let Some(path) = shania_core::mcp::ensure_config() {
        eprintln!("已生成 MCP 配置模板：{}", path.display());
    }

    let config = resolve(&opts);

    // 核心层是 async 的，这里自己起一个运行时，不引入 #[tokio::main] 宏
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("无法初始化运行时：{e}");
            return 1;
        }
    };

    runtime.block_on(entry(opts, config))
}

async fn entry(opts: Options, config: ChatConfig) -> i32 {
    if opts.check {
        return match test_connection(&config).await {
            Ok(info) => {
                let latency = info["latencyMs"].as_u64().unwrap_or(0);
                let model = info["model"].as_str().unwrap_or(&config.model);
                let endpoint = info["endpoint"].as_str().unwrap_or(&config.base_url);
                println!("连通正常 · {latency}ms · 模型 {model}");
                println!("{DIM}{endpoint}{RESET}");
                0
            }
            Err(msg) => {
                eprintln!("{RED}{msg}{RESET}");
                1
            }
        };
    }

    // 没给提示词但标准输入被管道接管时，把 stdin 当作问题
    let prompt = opts.prompt.clone().or_else(read_stdin_if_piped);

    match prompt {
        Some(text) => {
            single_shot(text, &config, opts.show_reasoning, opts.continue_session).await
        }
        None => repl(&config, opts.show_reasoning, opts.continue_session).await,
    }
}

async fn single_shot(
    text: String,
    config: &ChatConfig,
    show_reasoning: bool,
    continue_session: bool,
) -> i32 {
    let mut session = if continue_session {
        session::load_latest()
    } else {
        None
    };

    let mut messages = match session.as_ref() {
        Some(s) => {
            eprintln!(
                "{DIM}接续会话：{}（{} 条消息）{RESET}",
                s.title,
                s.messages.len()
            );
            s.history()
        }
        None => Vec::new(),
    };

    let sink = TerminalSink::new(show_reasoning);
    let cancel = Arc::new(AtomicBool::new(false));
    messages.push(json!({ "role": "user", "content": text }));

    let result = shania_core::run_chat(config, messages, &sink, &cancel).await;
    let ok = result.is_ok();

    // 成功与否都把这一轮写进会话：失败的那轮至少留下用户提问，方便下次接着改
    let entry = session.get_or_insert_with(session::CliSession::new);
    entry.push_user(&text);
    if ok {
        entry.push_assistant(&sink.captured());
    }
    if let Err(err) = entry.save() {
        eprintln!("{RED}保存会话失败：{err}{RESET}");
    }

    if ok {
        0
    } else {
        1
    }
}

async fn repl(config: &ChatConfig, show_reasoning: bool, continue_session: bool) -> i32 {
    let mut session = if continue_session {
        session::load_latest()
    } else {
        None
    };
    let mut history: Vec<Value> = match session.as_ref() {
        Some(s) => s.history(),
        None => Vec::new(),
    };

    println!("{BOLD}Shania CLI{RESET} {DIM}· {}{RESET}", config.model);
    println!("{DIM}{}{RESET}", config.base_url);
    if let Some(s) = session.as_ref() {
        println!(
            "{DIM}接续：{}（{} 条消息）{RESET}",
            s.title,
            s.messages.len()
        );
    }
    println!("{DIM}/exit 退出 · /clear 清空上下文 · /help 帮助{RESET}");

    let stdin = io::stdin();
    let mut line = String::new();

    loop {
        print!("\n{CYAN}›{RESET} ");
        let _ = io::stdout().flush();

        line.clear();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => {
                println!();
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("读取输入失败：{e}");
                break;
            }
        }

        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        match input {
            "/exit" | "/quit" => break,
            "/clear" => {
                history.clear();
                if let Some(s) = session.as_mut() {
                    s.messages.clear();
                }
                println!("{DIM}上下文已清空{RESET}");
                continue;
            }
            "/help" => {
                print!("{}", repl_help());
                continue;
            }
            _ => {}
        }

        let sink = TerminalSink::new(show_reasoning);
        let cancel = Arc::new(AtomicBool::new(false));
        history.push(json!({ "role": "user", "content": input }));

        match shania_core::run_chat(config, history.clone(), &sink, &cancel).await {
            // 核心层会把工具调用等中间消息写回 history，直接采用，
            // 这样下一轮模型能看到完整的调用轨迹
            Ok(updated) => {
                history = updated;
                let reply = sink.captured();
                let entry = session.get_or_insert_with(session::CliSession::new);
                entry.push_user(input);
                entry.push_assistant(&reply);
                if let Err(err) = entry.save() {
                    eprintln!("{DIM}保存会话失败：{err}{RESET}");
                }
            }
            Err(_) => {
                // 这一轮失败了，撤掉刚压入的用户消息，避免污染后续上下文
                history.pop();
            }
        }
    }

    0
}

/* ------------------------------------------------------------------ */
/* 终端渲染                                                             */
/* ------------------------------------------------------------------ */

/// 把流式事件画到终端。
/// 正文走 stdout（方便重定向到文件），提示类信息走 stderr，
/// 这样 `shania "..." > out.md` 依然只拿到模型输出。
struct TerminalSink {
    color: bool,
    show_reasoning: bool,
    wrote_content: AtomicBool,
    reasoning_open: AtomicBool,
    /// 累积本次正文，供会话接续写回文件（终端已经打过一遍，这里只留一份）
    capture: Mutex<String>,
}

impl TerminalSink {
    fn new(show_reasoning: bool) -> Self {
        Self {
            color: io::stdout().is_terminal(),
            show_reasoning,
            wrote_content: AtomicBool::new(false),
            reasoning_open: AtomicBool::new(false),
            capture: Mutex::new(String::new()),
        }
    }

    /// 取出本次累积的正文
    fn captured(&self) -> String {
        self.capture
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| String::new())
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }
}

impl EventSink for TerminalSink {
    fn emit(&self, event: StreamEvent) {
        match event {
            StreamEvent::Reasoning { delta } => {
                if self.show_reasoning {
                    eprint!("{}", self.paint(DIM, &delta));
                    self.reasoning_open.store(true, Ordering::Relaxed);
                }
            }
            StreamEvent::Content { delta } => {
                // 思维链和正文是两个流，切换时补一个换行，否则会黏在一行
                if self.reasoning_open.swap(false, Ordering::Relaxed) {
                    eprintln!();
                }
                if !self.wrote_content.swap(true, Ordering::Relaxed) {
                    // 新的一轮正文开始，与上面工具日志隔开
                    if self.color {
                        eprintln!();
                    }
                }
                print!("{delta}");
                let _ = io::stdout().flush();
                if let Ok(mut buf) = self.capture.lock() {
                    buf.push_str(&delta);
                }
            }
            StreamEvent::Round { index } => {
                if index > 0 && self.wrote_content.swap(false, Ordering::Relaxed) {
                    // 上一轮正文结束，换个行的位置给下面的工具日志
                    println!();
                }
            }
            StreamEvent::ToolStart { name, arguments, .. } => {
                if self.wrote_content.swap(false, Ordering::Relaxed) {
                    println!();
                }
                let args = squeeze(&arguments, 160);
                eprintln!(
                    "{}",
                    self.paint(DIM, &format!("  [工具] {name} {args}"))
                );
            }
            StreamEvent::ToolResult {
                ok,
                elapsed_ms,
                result,
                ..
            } => {
                let head = result.lines().next().unwrap_or("").trim();
                let head = squeeze(head, 120);
                let mark = if ok { "完成" } else { "失败" };
                let line = format!("  [{mark} {elapsed_ms}ms] {head}");
                eprintln!(
                    "{}",
                    self.paint(if ok { DIM } else { YELLOW }, &line)
                );
            }
            StreamEvent::Done { .. } => {
                if self.wrote_content.swap(false, Ordering::Relaxed) {
                    println!();
                }
            }
            StreamEvent::Error { message } => {
                if self.wrote_content.swap(false, Ordering::Relaxed) {
                    println!();
                }
                eprintln!("{}", self.paint(RED, &message));
            }
        }
    }
}

/// 单行化 + 截断，工具参数和结果可能带换行或极长
fn squeeze(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let head: String = flat.chars().take(max).collect();
        format!("{head}…")
    }
}

/* ------------------------------------------------------------------ */
/* 配置                                                                 */
/* ------------------------------------------------------------------ */

/// 配置文件的字段子集。桌面端的 `settings.json` 含更多字段（主题、模型预设等），
/// 多出来的字段会被忽略，只取连接相关的几项。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct FileConfig {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    system_prompt: Option<String>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    tools_enabled: Option<bool>,
}

impl FileConfig {
    /// 后者覆盖前者中已填写的字段
    fn merge(base: FileConfig, over: FileConfig) -> FileConfig {
        FileConfig {
            base_url: over.base_url.or(base.base_url),
            api_key: over.api_key.or(base.api_key),
            model: over.model.or(base.model),
            system_prompt: over.system_prompt.or(base.system_prompt),
            temperature: over.temperature.or(base.temperature),
            max_tokens: over.max_tokens.or(base.max_tokens),
            tools_enabled: over.tools_enabled.or(base.tools_enabled),
        }
    }
}

fn shania_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".shania"))
}

fn read_config(path: Option<PathBuf>) -> FileConfig {
    let Some(path) = path else {
        return FileConfig::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return FileConfig::default();
    };
    // 配置写坏了不该让程序起不来，退回默认值即可
    serde_json::from_str(&text).unwrap_or_default()
}

/// 桌面端写入的共享设置：`~/.shania/settings.json`
fn load_shared_config() -> FileConfig {
    read_config(shania_dir().map(|d| d.join("settings.json")))
}

/// CLI 专用覆盖：`~/.shania/config.json`，优先级高于共享设置
fn load_cli_config() -> FileConfig {
    read_config(shania_dir().map(|d| d.join("config.json")))
}

fn load_file_config() -> FileConfig {
    FileConfig::merge(load_shared_config(), load_cli_config())
}

/// 优先级：命令行参数 > 环境变量 > ~/.shania/config.json > ~/.shania/settings.json > 内置默认
fn resolve(opts: &Options) -> ChatConfig {
    let file = load_file_config();

    let env = |key: &str| {
        std::env::var(key)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let base_url = opts
        .base_url
        .clone()
        .or_else(|| env("SHANIA_BASE_URL"))
        .or(file.base_url)
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    let model = opts
        .model
        .clone()
        .or_else(|| env("SHANIA_MODEL"))
        .or(file.model)
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    let api_key = opts
        .api_key
        .clone()
        .or_else(|| env("SHANIA_API_KEY"))
        .or(file.api_key)
        .unwrap_or_default();

    ChatConfig {
        base_url,
        api_key,
        model,
        temperature: opts.temperature.or(file.temperature),
        max_tokens: opts.max_tokens.or(file.max_tokens),
        system_prompt: opts.system.clone().or(file.system_prompt),
        tools_enabled: if opts.no_tools {
            Some(false)
        } else {
            file.tools_enabled
        },
        max_rounds: opts.max_rounds,
        // --no-tools 同时关掉 MCP：都是「给模型挂工具」，关就一起关
        mcp_enabled: if opts.no_tools { Some(false) } else { None },
    }
}

/* ------------------------------------------------------------------ */
/* 参数解析                                                             */
/* ------------------------------------------------------------------ */

#[derive(Default)]
struct Options {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    system: Option<String>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    max_rounds: Option<u32>,
    no_tools: bool,
    show_reasoning: bool,
    /// 接续最近一条会话（与桌面端共用 ~/.shania/sessions）
    continue_session: bool,
    check: bool,
    help: bool,
    version: bool,
    prompt: Option<String>,
}

/// 拆出 `--key=value` 形式，其余原样返回
fn split_flag(raw: &str) -> (&str, Option<String>) {
    if raw.starts_with("--") {
        if let Some((k, v)) = raw.split_once('=') {
            return (k, Some(v.to_string()));
        }
    }
    (raw, None)
}

/// 取参数值：`--model=x` 用等号里的，否则吃掉下一个 token
fn value_of(
    it: &mut std::vec::IntoIter<String>,
    inline: Option<String>,
    name: &str,
) -> Result<String, String> {
    match inline {
        Some(v) => Ok(v),
        None => it
            .next()
            .ok_or_else(|| format!("{name} 后面缺少取值")),
    }
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut o = Options::default();
        let mut texts: Vec<String> = Vec::new();
        let mut it = args.into_iter();

        while let Some(raw) = it.next() {
            let (key, inline) = split_flag(&raw);
            match key {
                "--base-url" | "-b" => {
                    o.base_url = Some(value_of(&mut it, inline, "--base-url")?)
                }
                "--api-key" | "-k" => o.api_key = Some(value_of(&mut it, inline, "--api-key")?),
                "--model" | "-m" => o.model = Some(value_of(&mut it, inline, "--model")?),
                "--system" | "-s" => o.system = Some(value_of(&mut it, inline, "--system")?),
                "--temperature" | "-t" => {
                    let v = value_of(&mut it, inline, "--temperature")?;
                    o.temperature = Some(
                        v.parse()
                            .map_err(|_| format!("--temperature 需要数字，收到「{v}」"))?,
                    );
                }
                "--max-tokens" => {
                    let v = value_of(&mut it, inline, "--max-tokens")?;
                    o.max_tokens = Some(
                        v.parse()
                            .map_err(|_| format!("--max-tokens 需要整数，收到「{v}」"))?,
                    );
                }
                "--max-rounds" => {
                    let v = value_of(&mut it, inline, "--max-rounds")?;
                    o.max_rounds = Some(
                        v.parse()
                            .map_err(|_| format!("--max-rounds 需要整数，收到「{v}」"))?,
                    );
                }
                "--continue" | "-c" => o.continue_session = true,
                "--no-tools" => o.no_tools = true,
                "--show-reasoning" | "-r" => o.show_reasoning = true,
                "--check" => o.check = true,
                "--help" | "-h" => o.help = true,
                "--version" | "-V" => o.version = true,
                "--" => {
                    texts.extend(it.by_ref());
                    break;
                }
                other if other.starts_with('-') => {
                    return Err(format!("未知参数：{other}"));
                }
                other => texts.push(other.to_string()),
            }
        }

        if !texts.is_empty() {
            o.prompt = Some(texts.join(" "));
        }
        Ok(o)
    }
}

/// 标准输入被管道接走时（`echo hi | shania`）读整段当问题
fn read_stdin_if_piped() -> Option<String> {
    if io::stdin().is_terminal() {
        return None;
    }
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).ok()?;
    let text = buf.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn help_text() -> String {
    format!(
        "{BOLD}shania{RESET} · Shania Agent 命令行客户端

{RESET}用法：
  shania [选项] [提示词...]      给了提示词就单次问答，不给就进交互模式
  echo \"问题\" | shania          从标准输入读取问题
  shania --check                只测连通性，不发对话
  shania -c                     接续最近一条会话（与桌面端共用）

{RESET}选项：
  -b, --base-url <URL>         接口地址，默认 {DEFAULT_BASE_URL}
  -m, --model <名字>           模型名，默认 {DEFAULT_MODEL}
  -k, --api-key <KEY>          接口密钥
  -s, --system <提示词>        系统提示
  -t, --temperature <小数>     采样温度
      --max-tokens <整数>      单次回复上限
      --max-rounds <整数>      工具循环上限，默认 6
      --no-tools               关闭工具调用
  -c, --continue               接续最近一条会话，上下文与桌面端共享
  -r, --show-reasoning         把思维链打到 stderr
      --check                  连通性测试
  -h, --help                   显示本帮助
  -V, --version                显示版本

{RESET}配置来源（后者覆盖前者）：
  命令行参数 > 环境变量 SHANIA_BASE_URL / SHANIA_API_KEY / SHANIA_MODEL
  > ~/.shania/config.json（CLI 专用覆盖）
  > ~/.shania/settings.json（与桌面端共用）
"
    )
}

fn repl_help() -> String {
    format!(
        "{DIM}可用命令：
  /clear    清空本轮上下文（不影响配置）
  /help     显示这段帮助
  /exit     退出（Ctrl+D 也可以）
{RESET}"
    )
}
