export type Role = 'user' | 'assistant' | 'system'

export type MessageStatus = 'streaming' | 'done' | 'stopped' | 'error'

export interface ToolCallRecord {
  id: string
  name: string
  arguments: string
  result?: string
  ok?: boolean
  status: 'running' | 'done' | 'error'
  elapsedMs?: number
}

export interface Usage {
  prompt_tokens?: number
  completion_tokens?: number
  total_tokens?: number
}

/** 附件类型：图片走多模态；文本类文件读成文字注入上下文 */
export type AttachmentKind = 'image' | 'text'

/** 图片等二进制附件，以 data URL 形式随消息保存在本地 */
export interface Attachment {
  id: string
  name: string
  mime: string
  /** data:image/png;base64,...；文本附件为空串 */
  dataUrl: string
  /** 字节数，用于展示与限额判断 */
  size: number
  /** 缺省视为 image —— 兼容加这个字段之前存下的会话 */
  kind?: AttachmentKind
  /** 文本附件的正文（已截断），仅在 kind === 'text' 时有值 */
  text?: string
}

export interface Message {
  id: string
  role: Role
  content: string
  reasoning?: string
  toolCalls?: ToolCallRecord[]
  attachments?: Attachment[]
  createdAt: number
  model?: string
  usage?: Usage
  error?: string
  status?: MessageStatus
}

export interface Session {
  id: string
  title: string
  messages: Message[]
  createdAt: number
  updatedAt: number
  pinned?: boolean
  archived?: boolean
}

export type ThemeMode = 'system' | 'light' | 'dark'

/** 关闭主窗口时的行为：收起到托盘，或直接退出应用 */
export type CloseAction = 'tray' | 'quit'

/** 一套可保存的模型接入配置 */
export interface ModelProfile {
  id: string
  name: string
  baseUrl: string
  apiKey: string
  model: string
  temperature: number
  maxTokens: number
}

export interface AppSettings {
  baseUrl: string
  apiKey: string
  model: string
  temperature: number
  maxTokens: number
  systemPrompt: string
  toolsEnabled: boolean
  theme: ThemeMode
  displayName: string
  /** 全局唤起快捷键，形如 CmdOrCtrl+Shift+Space */
  shortcut: string
  /** 关闭主窗口时的行为 */
  closeAction: CloseAction
  /** 开机自动启动 */
  autoStart: boolean
  /** 单次请求的历史预算（按估算 token 计），超出后自动丢弃较早的消息 */
  contextLimit: number
  /** 语音识别模型（走语音接口地址的 /audio/transcriptions） */
  sttModel: string
  /** 语音识别服务地址；留空表示用程序自己拉起的本地服务 */
  sttBaseUrl: string
  /** 本地语音服务的首选端口，0 表示每次随机（被占时会自动顺延） */
  sttPort: number
  /** 程序启动时就把语音服务拉起来（模型加载慢，按需启动要等几秒）；关掉则退化为用到才起 */
  sttPreload: boolean
  /**
   * 推理后端：`cpu` 兼容性最好；`cuda` 走 NVIDIA 显卡，实测快 3-8 倍。
   * 选 cuda 但没有可用显卡时，启动会退回 cpu。
   */
  sttBackend: 'cpu' | 'cuda'
  /** 语音识别语言提示，留空表示自动判断 */
  sttLanguage: string
  /**
   * 用哪套识别引擎。
   *   auto    模型装了就用流式（边说边出字），没装自动退回 whisper
   *   stream  强制流式；模型没装时语音按钮会提示去安装
   *   whisper 强制走 whisper-server（慢，但老机器上更稳）
   */
  sttEngine: 'auto' | 'stream' | 'whisper'
  /**
   * 是否在流式定稿之后再跑一遍「纠错模型」把结果重写一遍。
   *
   * 目前只在设置里占位：纠错层（SenseVoice）还没接，打开也不会有行为差异。
   * 保留这个字段是为了让老设置文件和新版本互相兼容——缺字段时按 false 处理。
   */
  sttCorrect: boolean
  /** 已保存的模型配置 */
  profiles: ModelProfile[]
  /** 当前生效配置对应的 profile id（手动改过参数则为空） */
  activeProfileId: string
}

/** 与 Rust 端 StreamEvent 一一对应 */
export type StreamEvent =
  | { type: 'reasoning'; delta: string }
  | { type: 'content'; delta: string }
  | { type: 'round'; index: number }
  | { type: 'toolStart'; id: string; name: string; arguments: string }
  | { type: 'toolResult'; id: string; name: string; ok: boolean; result: string; elapsedMs: number }
  | { type: 'done'; usage?: Usage | null }
  | { type: 'error'; message: string }

/** OpenAI 兼容的多模态内容片段 */
export type ContentPart =
  | { type: 'text'; text: string }
  | { type: 'image_url'; image_url: { url: string } }

export interface WireMessage {
  role: 'user' | 'assistant'
  content: string | ContentPart[]
}
