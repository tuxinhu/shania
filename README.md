# Shania Agent · 项目文档

基于 **Tauri 2 + Vue 3 + TypeScript** 的桌面端 Agent 对话系统。苹果风格 UI，Rust 侧代理请求与 Agent 工具循环，前端只消费事件流。

> 本文档是项目的"续作指南"：在任何新会话里让 AI 先读本文件，即可接着开发。

---

## 1. 技术栈与架构

| 层 | 技术 | 说明 |
|---|---|---|
| 桌面壳 | Tauri 2.12（Rust 主进程 + WebView2） | 所有网络请求、Agent 循环在 Rust 侧完成 |
| 前端 | Vue 3.5 `<script setup>` + Pinia 4 | 状态管理：`stores/chat.ts`（会话）、`stores/settings.ts`（配置） |
| 构建 | Vite 7.3 | 开发时 1420 端口热更新；发布时产出 `dist/` 内嵌进 exe |
| 类型 | TypeScript 5.9 | `pnpm build` 会先跑 `vue-tsc --noEmit` 检查 |
| 渲染 | markdown-it + highlight.js | 流式 Markdown、代码高亮 |

**核心架构**（三层，依赖单向）：

```
src-tauri/core/          shania-core —— Agent 逻辑，Cargo.toml 里没有 tauri
  ├─ chat.rs               多轮工具循环 / SSE 解析 / 连通性测试 / 取消开关表
  ├─ tools.rs              内置工具（时间 / 系统信息 / 读文件 / 列目录）
  └─ events.rs             StreamEvent + EventSink（谁消费由调用方决定）

src-tauri/src/           桌面端 —— 窗口 + 命令层
  └─ chat.rs               实现 EventSink（塞进 IPC Channel），其余全部转发 core

src-tauri/cli/           命令行 —— bin 名 shania
  └─ main.rs               实现 EventSink（打终端），单次问答 + REPL
```

```
        ┌──────────────┐
        │ shania-core  │  ← 没有任何 UI 依赖
        └──────┬───────┘
      ┌────────┴────────┐
 桌面端(Tauri)       CLI(终端)
```

拆开的核心动机：CLI 若与 Tauri 同处一个 crate，为了问一句话就得背上整套
WebView2 与托盘。拆开后 CLI 二进制只含 reqwest + serde，Agent 逻辑也只有一份。

- API 协议：**OpenAI 兼容**（baseURL + apiKey + 模型名），支持多模型配置切换
- 事件流：`#[serde(tag = "type")]` 枚举 → content / toolStart / toolResult / error
- localhost 请求强制 `no_proxy()`（防代理改写 absolute-form URI 导致 404）

## 2. 已实现功能（全部验证通过）

### 对话核心
- 流式对话 + Markdown 渲染 + 代码高亮
- 图片上传（>1.2MB 自动重编码，长边 1568，PNG 保透明），支持视觉模型
- **文本附件**：md / json / csv / 代码等文本文件读成正文注入上下文（单文件 ≤512KB、注入 ≤20 万字，超出截断并注明）；二进制文件明确拒绝，不假装能理解
- **语音输入**：X-ASR 流式识别（进程内推理、边说边出字、不占端口），文字实时填入光标处（见下方「语音与 MCP」）
- 失败重试、编辑重发、删除消息
- 上下文自动裁剪（token 估算：中文 1.05 token/字，图片 1200 token，超限时首部插「已省略 N 条」）
- Agent 工具调用：Rust 侧多轮循环，前端显示工具卡片 + 结果折叠
- 工具结果截断：回灌模型的那份限 12000 字符，UI 显示完整

### 会话管理
- 多会话 + 本地持久化（`storage.rs`，`~/.shania/sessions/<id>.json` 一个会话一个文件，原子写入：temp + rename）
- 全局内容搜索、归档、清空、草稿保存
- **会话导入**：选 JSON 文件恢复会话，兼容导出格式 / 存储快照 / 单会话三种结构，按 id 去重，重复导入不产生副本

### 桌面集成
- 窗口状态记忆（大小/位置/最大化，仅记忆 SIZE|POSITION|MAXIMIZED，不记 visible）
- 关闭行为可配置：隐藏到托盘 或 直接退出
- 全局快捷键唤起（可录制、校验、冲突回退，托盘菜单文案动态跟随）
- 单实例：重复启动唤回已有窗口
- 系统托盘常驻（显示主窗口〔带当前快捷键〕/ 新建对话 / 设置 / 打开配置目录 / 退出）
- 开机自启（autostart 插件）

### 设置与交付
- 多模型配置 profiles：baseURL / apiKey / 模型名，一键切换 + 连接测试
- **深色主题**：浅色 / 深色 / 跟随系统，首屏前由内联脚本定主题（不闪白）
- 语音识别配置：模型状态 + 启动预加载（不需要端口、地址、语言这些参数）
- 会话导出 Markdown / JSON
- 自动更新骨架（updater 插件已接入，见"未完成"）

### 验证情况
| 测试 | 命令 | 结果 |
|---|---|---|
| 上下文裁剪单测 | `pnpm test:context` | 22/22 |
| 导出序列化单测 | `pnpm test:export` | 11/11 |
| 文本对话端到端 | `pnpm e2e:chat` | 通过 |
| 图片对话端到端 | `pnpm e2e:image` | 通过（mock 日志 images=1） |
| UI 自动化（CDP 驱动真实窗口） | `pnpm e2e:ui` | 22/22 |
| 全局快捷键 | `scripts/test-shortcut.mjs` | 二次注册返回 already registered 即证明成功 |
| 托盘/自启/窗口状态 | taskkill 无 /F、注册表、重启窗口尺寸 | 全部通过 |
| vue-tsc + cargo build | — | 零错误零警告 |

## 3. 如何运行 / 打包

所有命令都在**项目根目录**（本文件所在目录）执行。

```bash
pnpm install        # 首次（或换机后）
pnpm mock           # 起 mock LLM 服务（127.0.0.1:4000，协议为 OpenAI 兼容；PORT=xxxx 可改端口）
pnpm dev            # 起 Vite 开发服务器（1420 端口）
pnpm app:dev        # Tauri 开发模式（自动带起 pnpm dev，等价一条命令）
pnpm stt            # 起旧的 whisper-server（桌面端已不再调用，仅手动调试用）
```

### 语音与 MCP

**语音输入**：桌面端自带模型的**下载 + 加载**，不需要装任何服务、起任何进程。
**边说边转**——不用等点「结束」，说的时候字就跟着往外冒。

只有一套引擎：**X-ASR 160ms**（`stream-x-asr-160ms-zh-en-punct-int8`）。

| 项 | 值 |
|---|---|
| 跑在哪 | **本进程内**，sherpa-onnx 的 `OnlineRecognizer` 直接调模型 |
| 端口 / 地址 | **不需要**，不监听端口、不发 HTTP |
| 出字时机 | 每 160ms 一块，边说边出；停约 1 秒自动定稿 |
| 首字延迟 | 约 0.4s（实测） |
| 模型 | int8 量化，中英双语，自带标点，134MB |
| 实测 RTF | 0.19–0.24（4 线程） |

设置面板 →「语音输入」只有两块：**模型状态**（未安装 → 下载 134MB → 已就绪/已加载）
和 **启动时预加载**开关。

#### 实现要点

- 引擎在 `core/src/stream_stt.rs`：sherpa-onnx 的 `OnlineRecognizer`，进程内常驻，
  一个会话一个 stream。音频 100ms 一块从 `pcm-recorder.ts` 的 `onFrame` 流出，
  经 base64 → Rust 解码成 `f32` → `accept_waveform`
- **句末判定交给模型**（`rule1/rule2/rule3`）：`rule2 = 0.9s` 尾部静音算一句说完。
  前端那套能量 VAD 仍然在跑，但只用来驱动麦克风的呼吸效果，不参与切句
- **`finish()` 必须补 0.66 秒静音**：模型按 160ms 分块编码，最后不足一块的音频解不出字。
  实测「改成次要样式」不补零只会吐「改成次要样」
- **标点由模型自带**（punct 版），Rust 侧只做繁转简 + 半角逗号转全角；
  `lib/punctuate.ts` 只在句末兜一个断句标点（`closeSegment` 见到已有标点会跳过）
- 后处理刻意**不做**按句丢幻听：流式吐的是半句话，按句删会把正文一起清掉
- 实测 RTF **0.19–0.24**（4 线程），20 秒长音频不退化；线程数上限卡在 4
  ——实测 8 线程反而降到 0.354，内存带宽先饱和
- **IPC 三个命令**（`stt_stream_start` / `feed` / `stop`），数据全程走 Tauri IPC，
  不开 HTTP 端口——这也是它不需要「服务地址」的原因
- 模型加载 2–5 秒，所以设置里的「启动时预加载」默认打开：启动 2 秒后由独立线程读盘，
  点麦克风时已经就绪。关掉则退化为点麦克风时才加载（首次会等 1–2 秒）

#### 发版前必须处理：DLL 分发（重要）

`sherpa-onnx` 当前用 `shared` 特性（DLL 模式），不是默认的 `static`。原因是：

> static 包是拿 **MSVC 14.40+** 的 STL 编译的，在 VS 2022 17.7（MSVC 14.37）上链接会缺
> `__std_remove_2` / `__std_find_end_1` 这类新符号，报一堆 `LNK2001/2019`。

DLL 内部已经把 STL 静态链好了，我们只调 C API，所以不受本机 STL 版本影响。
`build.rs` 会自动把 4 个 DLL 拷到 `target/{debug,release}/`，**开发期无需任何操作**。

但发版时这 4 个 DLL 必须和 `shania-agent.exe` **放在同一目录**——Windows 的 DLL 搜索顺序里
`system32` 优先于 PATH，而 Windows 自带的 `C://Windows//system32//onnxruntime.dll` 是个旧版本，
被加载到会直接崩（`The given version [28] is not supported` + `STATUS_ACCESS_VIOLATION`，
实测踩过）。两条路：

1. **推荐**：把 VS 2022 更新到 17.10+（MSVC 14.44），改回 `static`，回到单 exe 分发
2. 留在 `shared`：用 NSIS `installerHooks` 把 4 个 DLL 装到 `$INSTDIR` 根目录

安装位置：

```
~/.shania/models/stream-x-asr-160ms-zh-en-punct-int8/    # X-ASR 模型，解压后约 230MB
                                                          # （encoder.int8 / decoder.onnx / joiner.int8 / tokens.txt）
```

下载地址可用 `SHANIA_STREAM_STT_URL` 指向镜像（默认走 GitHub release，国内可能很慢）。
模型按字节数校验，中断的下载会留下 `.part` 而不被误认为装好。

#### 旧的 whisper 方案（已下线）

早期版本还接过 whisper-server（独立子进程 + HTTP + 前端能量切句），以及 SenseVoice 做
「定稿后再纠错」。两者都实测不如 X-ASR：

- whisper 要占一个端口、管一个子进程，服务拉起要 2–3 秒，说完一整句才出字，
  静音和提示音还会编字幕
- SenseVoice 纠错在中文短句上是净损失（8 句样本 CER 1.36% vs 流式 0.00%）

相关代码已整体删除。**模型和脚本保留在磁盘上**，将来要接回来可以直接用：

```
~/.shania/tools/whisper/Release/whisper-server.exe        # CPU 版（8.6MB）
~/.shania/tools/whisper-cuda/Release/whisper-server.exe   # GPU 版（1.2GB，自带 cudart/cublas）
~/.shania/models/ggml-small-q5_1.bin                      # 182MB
~/.shania/models/ggml-silero-v6.2.0.bin                   # 885KB，VAD
~/.shania/models/sense-voice-zh-en-ja-ko-yue-2024-07-17/  # 229MB
scripts/stt-server.ps1                                    # 手动起服务的脚本
```

`pnpm stt` / `pnpm stt:gpu` 仍然可用（内部就是 `scripts/stt-server.ps1`），
只是桌面端不会再调用它——启动起来也没有客户端。

#### 实测延迟（i7-11800H / RTX 3050 Laptop 4GB）

| 场景 | 4 秒音频 | 8 秒音频 |
|---|---|---|
| X-ASR 流式（CPU，4 线程） | RTF 0.19–0.24，首字约 0.4s | 同左，不退化 |
| whisper CPU + `-ac 450`（已下线） | 1.4 s | 1.8 s |
| whisper GPU（已下线） | 0.5 s | 0.3–0.5 s |

#### 语音输出（本地 TTS）

两个模型都是进程内 ONNX（和 X-ASR 共用同一个 `onnxruntime.dll`），不联网、不占显存、
不需要单独的服务进程：

| 模型 | 磁盘 | 实测合成耗时 | 音色 | 定位 |
|---|---|---|---|---|
| `matcha-icefall-zh-baker` | 72MB + 54MB 声码器 | 12 字 132ms、20 字 243ms | 1 个女声 | 默认，中文通话用 |
| `kokoro-multi-lang-v1_1` | 348MB | 12 字 1290ms、20 字 2420ms | 103 个 | 音质更自然，但中文词表覆盖不足 |

**两个模型都没有英文能力**，差别只在于「哪些词念得出来」：

| | 拉丁字母 | 阿拉伯数字 | 中文 |
|---|---|---|---|
| Matcha | lexicon 里 0 个英文词，全部删掉 | 靠 `number.fst` 转中文读法 | 逐字查拼音，覆盖完整 |
| Kokoro | 保留 17.8 万词英文表里有的（`settings`/`base` 能念），其余的删掉 | 靠 `number-zh.fst` | 按词查注音表，**表里没有的词会退回拼音、念成英文音** |

kokoro 那行是它最要命的缺陷：「好的」「我看了下」这类常用词不在 68036 行词表里，
会被念成 “how did”、“well, can less”。所以**中文通话一律用 Matcha**，Kokoro 留给
英文内容或「不在乎个别词念错、只要音色好听」的朗读场景。

英文词的处理逻辑在 `tts.rs` 的 `Latin`：`Drop`（Matcha，整词删）和
`Allow(HashSet)`（Kokoro，查表保留）。删而不是留着，是因为留着会念出不相干的音——
实测 “API” 和 “GPT” 都念成 “total”，两个不同的词撞成同一个音，听感上就是「同一个字
被重读了两遍」。

模型吐出来的 markdown 结构也在两处拦掉（`Splitter` 和 `sanitize`）：**``` 代码块整段
不念**（不然 Kokoro 会把 `for line in buf` 里的 for/in/line 当真英文念出来），表格分隔行
跳过，路径和代码剩下的符号串（`://`、`+`、`() {`）整段删掉——中文合成器会把它们念成
「冒号斜杠斜杠」，或者干脆卡住不吐音。单个英文句读符号（`1: 打开设置`）翻成中文标点，
停顿保留。

切换在「设置 → 语音输出 → 合成模型」，三选一：**自动 / Matcha / Kokoro**。
存进 `settings.json` 的 `ttsModel`，取值 `auto` | `matcha` | `kokoro`：

- `auto`：装了哪个用哪个，Matcha 优先（快）
- 指定的模型没装 → 自动回退到已装的那个，并且设置页会用红字说明「现在实际在用谁」，
  不会出现「选了却没声音」
- 切换时会把内存里的旧模型丢掉，下次合成按新选择重新加载（两个模型同时驻留要多占 350MB）

需要镜像时用 `SHANIA_TTS_URL`（Matcha 包）/ `SHANIA_TTS_VOCODER_URL`（声码器）/
`SHANIA_TTS_KOKORO_URL`（Kokoro 包）覆盖下载地址。


**MCP 工具**：配置文件 `~/.shania/mcp.json`（不存在视为没有 MCP）。

```json
{
  "servers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "D:/tmp"],
      "env": {},
      "enabled": true
    }
  }
}
```

- 工具名统一加前缀 `mcp__<server>__<tool>`，模型看到的描述是 `[MCP·server] 原描述`
- 每轮对话开始时启动、结束时关闭；单个 server 连不上只打印警告，不影响对话
- 启动失败的原因打到 stderr（桌面端为进程输出，CLI 直接可见）
- `--no-tools` 会同时关掉内置工具与 MCP

### 调试 vs 正式包（重要！）

| | debug exe（target/debug） | 正式包（app:build 产物） |
|---|---|---|
| 界面来源 | 依赖 `devUrl`，**必须 Vite 在跑**才能显示 | 内嵌 `dist/`，双击即用 |
| 路径 | `src-tauri/target/debug/shania-agent.exe` | 裸 exe：`src-tauri/target/release/shania-agent.exe`<br>安装包：`src-tauri/target/release/bundle/nsis/*.exe` |

```bash
pnpm app:build      # = pnpm build（Vite 出 dist）→ cargo build release → NSIS 安装包
```

注意：项目从 `I:\project\agent-ui` 迁移到 `I:\project\shania` 后，首次 cargo build 会因缓存路径失效而大范围重编（一次性，几分钟）；之后恢复增量。

### 命令行（CLI）

和桌面端共用 `shania-core`，工具调用、多轮循环、SSE 解析是同一份代码。

```bash
pnpm cli --check              # 只测连通性
pnpm cli "现在几点"            # 单次问答，流式输出
pnpm cli                      # 交互模式：多轮对话，/clear 清空上下文，/exit 退出
echo "总结这段话" | pnpm cli    # 从标准输入读问题
pnpm cli -c                   # 接续最近一条会话（与桌面端共用 ~/.shania/sessions）
pnpm cli:build                # 出 release 二进制 → src-tauri/target/release/shania.exe
```

配置优先级（后者覆盖前者）：
**命令行参数 > 环境变量 > `~/.shania/config.json`（CLI 专用覆盖） > `~/.shania/settings.json`（与桌面端共用） > 内置默认（`http://127.0.0.1:4000` + `qwen3-vl-8b`）**

```bash
export SHANIA_BASE_URL=http://127.0.0.1:8000/v1
export SHANIA_API_KEY=sk-xxx
export SHANIA_MODEL=qwen3-vl-8b
shania --show-reasoning "解释一下这段代码"
```

参数：`-b/--base-url`、`-m/--model`、`-k/--api-key`、`-s/--system`、`-t/--temperature`、
`--max-tokens`、`--max-rounds`、`--no-tools`、`-c/--continue`、`-r/--show-reasoning`、`--check`。
完整清单见 `shania --help`。

会话接续：`-c` 会挑 `~/.shania/sessions/` 里 `updatedAt` 最新的一条接上，每轮结束自动写回，
所以「桌面端聊一半 → 命令行接着问」是同一个会话。CLI 建的新会话 id 形如 `cli-<时间戳>`，
桌面端照样能读；注意两边同时写同一条会话时**后写覆盖先写**，没有做文件锁。

输出分工：**正文走 stdout，工具日志/思维链/报错走 stderr**。所以 `shania "..." > answer.md` 只会写下模型正文，日志不会污染文件。

### 配置文件：`~/.shania/`

**桌面端与 CLI 读同一份设置**，两个前端不再各存一套。

| 文件 | 谁写 | 作用 |
|---|---|---|
| `~/.shania/settings.json` | 桌面端（设置面板改动后自动写） | **共享设置**：baseUrl / apiKey / model / temperature / maxTokens / systemPrompt / toolsEnabled，外加主题、模型预设、快捷键等桌面端专属项 |
| `~/.shania/sessions/<id>.json` | 桌面端（会话变化时写） | 会话历史，**一个会话一个文件**，temp + rename 原子写入 |
| `~/.shania/config.json` | 手写（可选） | **CLI 专用覆盖**：只想让命令行指向别的服务时用，不影响桌面端 |
| `~/.shania/logs/` | 桌面端 + CLI | 运行日志，见下节 |

实现要点：

- 桌面端设置存在普通 JSON 文件里，不再放 WebView 的 localStorage。localStorage 只留作首屏缓存（读文件前先把界面画出来，避免闪一下）。
- **旧数据会自动迁移**：首次启动若 `settings.json` 不存在、而 localStorage 里有旧设置，会原样写成文件；会话若是老的聚合数组（`~/.shania/sessions/sessions.json`、`~/.shania/sessions.json`、`%APPDATA%\com.shania.agent\sessions.json` 按新→旧找），会拆成单会话文件，原文件改名 `.bak`。
- 会话保存的取舍：**内容没变不重写**；数组里没有的会话会删掉对应文件；**解析不出来的文件一律不删**（宁可留垃圾，也不误删历史）；会话 id 落到文件名前会过滤非法字符，防止路径穿越。
- 读文件失败时**不落盘**，避免用默认值把用户配置冲掉。
- CLI 只取连接相关的几个字段，`settings.json` 里多出来的字段（主题、profiles…）会被忽略，不会报错。

### 日志：`~/.shania/logs/`

GUI 程序在 release 下没有控制台，`eprintln!` 等于把线索扔掉。所有日志统一落盘，托盘菜单有「打开日志目录」。

| 文件 | 内容 | 写入方式 |
|---|---|---|
| `shania.log` | 主日志：启动/退出、模型预加载、安装进度、MCP 警告、快捷键注册失败、坏的会话文件等 | 追加，**超过 5MB 下次启动轮转为 `.1`**（只留一份） |
| `stt.log` | 旧的 whisper-server 输出（已下线，只有手动跑 `pnpm stt` 时才会写） | **每次启动覆盖** |
| `mcp-<server>.log` | 每个 MCP server 的 stderr | **每次连接覆盖** |

```bash
tail -f ~/.shania/logs/shania.log     # 主日志
cat ~/.shania/logs/shania.log         # 语音识别问题看这里（搜「X-ASR」）
```

一行一条，格式 `2026-11-11 21:40:42.935 INFO  内容`。

实现（`core/src/logging.rs`，纯 std + chrono，没引 `tracing`）：

- `logging::init(dir)` 在 `run()` 最开头调用，桌面端和 CLI 都初始化；写不进去就退回 stderr，不影响程序
- 宏 `log_info!` / `log_warn!` / `log_error!`，跨 crate 用 `shania_core::log_info!(...)`
- 外部服务的原始输出由 `logging::service_log(dir, name)` 定位，**故意每次覆盖**：排查关心的是「这一次为什么失败」，历史由主日志保留

### 桌面端命令行参数

```bash
shania-agent.exe                          # 正常启动（收起到托盘）
shania-agent.exe --new-chat               # 唤出窗口并新建对话
shania-agent.exe --settings               # 唤出窗口并打开设置
shania-agent.exe "帮我总结 D:/notes.md"     # 唤出、新建对话、直接发送
```

应用已在运行时，第二次启动的参数由单实例插件转给已有窗口，不会开第二个实例。
实现上分两条通道（`src-tauri/src/cli_args.rs`）：**已在运行 → 发 `cli-args` 事件；首次启动 → 先存进 state，等前端挂载后调 `take_cli_args` 来取**。原因是首启时 WebView 里的 JS 还不存在，事件发出去会直接丢掉。

### 端到端测试套路（窗口自动化）

`tauri dev` 的 GUI 流程在自动化下会卡，现行做法：

```bash
pnpm mock &                                   # 1. mock 服务
npx vite --port 1420 --strictPort &           # 2. Vite
WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9223" \
  ./src-tauri/target/debug/shania-agent.exe & # 3. 裸跑 exe 开 CDP
node scripts/probe.mjs                        # 4. CDP 驱动真实窗口做 e2e
```

`scripts/` 下其余脚本：`e2e-*.mjs`（端到端）、`test-*.mjs`（单测）、`reset-settings.mjs`（重置配置到默认 mock）、`shot-app.mjs`/`dev-shot.mjs`（截图）。

## 4. 关键文件地图

```
src-tauri/core/src/          ← Agent 核心，改这里会同时影响桌面端与 CLI
  chat.rs         run_chat / run_agent_loop / consume_stream / test_connection / CancelRegistry
  tools.rs        内置工具定义与执行（4 个：时间/系统信息/读文件/列目录）+ is_builtin
  mcp.rs          MCP 客户端：stdio 启动 server、initialize / tools/list / tools/call
  stream_stt.rs   X-ASR 引擎：sherpa-onnx OnlineRecognizer，进程内推理（feature `stream-stt`）
                  含模型下载/解压/路径推导
  events.rs       StreamEvent + EventSink trait

src-tauri/cli/src/
  main.rs         ← CLI 入口：参数解析、配置合并、终端渲染 sink、REPL、--continue
  session.rs      会话接续：读写 ~/.shania/sessions/<id>.json（与桌面端同一份）

src-tauri/src/               ← 桌面端命令层，Agent 逻辑一律不在这一层
  chat.rs         实现 EventSink（ChannelSink），三个命令：chat_stream / stop_chat / test_connection
  cli_args.rs     命令行参数解析与分派（--new-chat / --settings / 裸文本）
  lib.rs          托盘 TrayState、快捷键 ShortcutState、close_action 关闭行为、single-instance 唤起
  tools.rs        只是把 core 的工具清单交给前端
  storage.rs      ~/.shania/sessions/<id>.json 按会话分文件读写（含聚合文件拆分迁移）
  settings.rs     ~/.shania/settings.json 原子读写（与 CLI 共用的设置文件）
  export.rs       导出落盘（write_text_file）/ 导入读取（read_import_file）
  stt_stream.rs   X-ASR 命令层：start / feed / stop / install / unload / status

src/
  lib/types.ts        ← StreamEvent 联合类型、Message/Attachment/WireMessage/AppSettings
  lib/context.ts      ← 上下文裁剪（纯函数）
  lib/image.ts        ← 图片压缩
  lib/files.ts        ← 文本附件判定与读取（含截断）
  lib/importer.ts     ← 会话导入：解析 / 校验 / 结构兼容
  lib/api.ts          ← IPC 封装 + 浏览器降级（mock 流式）
  lib/export.ts       ← Markdown/JSON 序列化
  lib/shortcut.ts     ← 快捷键格式化 + acceleratorFromEvent
  stores/chat.ts      ← 会话/流式状态机（重试/编辑/删除/归档/搜索/草稿）
  stores/settings.ts  ← 设置状态，落盘到 ~/.shania/settings.json（localStorage 仅作首屏缓存）
  components/         ← ChatPanel / MessageItem / ChatComposer / ToolCallCard /
                        MarkdownView / SidebarPanel / SettingsDialog / EmptyState / Icon
```

## 5. 未完成事项 / 下一步方向

### 待收尾（发版前必做）
1. **updater 配置**：`tauri.conf.json` 里 endpoint 还是占位符 `your-update-server.example.com`；发布时需 `createUpdaterArtifacts: true` 并配置真实更新服务器（签名密钥已生成过，pubkey 已在配置里）。
2. **真实模型连通性**：全程用 mock 验证协议层，还没连过真实 Qwen3-VL 服务，需人工填 baseURL 发一句验证。
3. **托盘菜单肉眼确认**：菜单项文案（含快捷键后缀）已由 `update_tray_shortcut` 动态刷新，但程序读不到托盘文字，需实际看一眼。
4. 打包跨平台：Windows 的 NSIS 可本机出包；macOS（.dmg/.app）与 Linux（.deb/.AppImage）需在对应系统上打包。
5. **语音输入需真机验证**：AudioWorklet 采集出帧、模型加载、定稿时机这三条路径都只验证到「Rust 侧能解出正确文本」，实际麦克风 → 文字全链路要在有麦的机器上点一遍。

### 功能扩展方向（按优先级建议）
1. **扩充工具集**：目前仅 4 个内置玩具工具。框架已通（Rust 多轮循环 + 前端卡片），可加：文件读写、网页抓取、执行命令、调本地 API 等。建议每加一个工具补一条 e2e。
2. **MCP 接入**：让 Agent 挂载外部 MCP server 的工具，与工具循环框架打通。
3. **消息内引用/复制优化**：代码块一键复制按钮等细节。
4. **工具集继续扩充**：内置仍只有 4 个，文件写入 / 网页抓取 / 执行命令还没做（MCP 补上了一部分外部能力）。
5. **多模态继续扩展**：视频、音频文件的理解；语音目前只做「说 → 文字」。

### 已知实现细节（改代码前须知）
- 上下文裁剪、导出序列化都是**纯函数**，改动必须同步跑 `test:context` / `test:export`。
- 流式渲染必须写**响应式代理对象**（`s[s.messages.length-1]`），直接改原始对象不触发渲染。
- SSE 事件按 `\n\n` 切分，跨 chunk 的 CRLF 已在缓冲区整体规范化。
- updater 插件未配置 endpoint 时启动会 panic（`invalid type: null`），别删掉 `plugins.updater` 配置。
- window-state 别把 `visible` 加回 state flags，会导致启动隐形。
- **Agent 逻辑一律改 `src-tauri/core`**，桌面端 `src/chat.rs` 只负责把事件转进 IPC。往 core 里塞窗口/托盘相关代码会让 CLI 编不过。
- `StreamEvent` 字段名与 Tauri 命令名都是**前后端契约**（`chat_stream` / `stop_chat` / `test_connection` / `take_cli_args`），改名必须同步改 `src/lib/types.ts` 与 `src/lib/api.ts`。
- 新增工具只需改 `core/src/tools.rs` 两处（`definitions()` + `execute()`），顺便在 `is_builtin()` 里登记名字——否则工具循环会把它当 MCP 工具去转发。
- 附件 `kind` 字段缺省视为 `image`：加这个字段之前存下的会话里只有图片，这样能直接兼容，不用写迁移。
- 语音识别放在 Rust 侧（`core/stream_stt.rs`）：麦克风 PCM 经 Tauri IPC 直接进进程，不经 WebView 发 HTTP，绕开 CORS 也省掉一次序列化。
- MCP server 的生命周期是「一轮对话」：`run_chat` 进来启动、出去 `kill`。想改成常驻要处理进程泄漏与状态失效，别随手改。
- **构建前必须关掉正在运行的 release exe**。Windows 不允许覆盖运行中的文件，cargo 会报 `failed to remove file ... shania-agent.exe / 拒绝访问 (os error 5)`。同理，`tauri dev` 运行期间持有 `target/debug/.cargo-build-lock`，任何 cargo 命令都会「拒绝访问」——两者是不同的锁，都得先关进程。
- **切换安装包类型会触发重新编译**。`tauri build` 会把 bundle 类型 patch 进 exe（日志里 `Patching ... with bundle type information: msi`），所以先打 nsis 再打 msi 时，即使源码没变也会重编一次（约 2 分钟）。只想要裸 exe 用 `--no-bundle`，不受影响。

## 6. 当前状态快照

- 最后更新：2026-11-11
- 项目位置：`I:\project\shania`（已从 agent-ui 迁移，旧目录为空壳可删）
- 应用状态：基础功能完成并验证，并补齐了 MCP / 语音输入 / 文本附件 / 会话导入 / 深色主题 / CLI 会话接续；**正式包已 build**（2026-11-11 19:46）
  - 裸 exe：`src-tauri/target/release/shania-agent.exe`（6.28 MB，内嵌 dist，双击即用）
  - 安装包 NSIS：`src-tauri/target/release/bundle/nsis/Shania Agent_0.1.0_x64-setup.exe`（2.37 MB，当前用户安装，不弹 UAC）
  - 安装包 MSI：`src-tauri/target/release/bundle/msi/Shania Agent_0.1.0_x64_en-US.msi`（3.23 MB，perMachine，装进 Program Files，需管理员）
  - 打包工具链缓存：`%LOCALAPPDATA%\tauri\`，NSIS 7.9 MB + WixTools314 117 MB，只在首次或缺文件时下载
- 命令行：`shania-cli` 已完成（单次问答 / REPL / `-c` 接续会话 / 连通性测试 / 配置三级合并）
- 测试：`cargo test --workspace` 全绿（storage 6 + mcp 3）；`npx vue-tsc --noEmit` 与 `pnpm build` 通过
- 外部能力：MCP（`~/.shania/mcp.json`，已用最小 node 服务端到端验证握手/列举/调用）、语音识别（`/audio/transcriptions`）
- 架构：已拆成 workspace 三成员 —— `shania-agent`（桌面端）、`shania-core`（无 tauri 依赖）、`shania-cli`
- 桌面端命令行参数：`--new-chat` / `--settings` / 裸文本三条路径已接通
- 默认配置：`http://127.0.0.1:4000` + `qwen3-vl-8b`（mock）
