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

/** 语音合成模型的选择（`auto` 由后端按可用性挑，matcha 优先） */
export type TtsModel = 'auto' | 'melo' | 'matcha' | 'kokoro'

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
  /** 程序启动时就把语音模型读进内存（加载要几秒）；关掉则退化为点麦克风时才加载 */
  sttPreload: boolean
  /** 语音合成的音色编号（只有 Kokoro 有多个音色，编号随模型版本变化） */
  ttsSid: number
  /** 语音合成的语速，0.5-2.0，1 为原速 */
  ttsSpeed: number
  /**
   * 用哪个合成模型。
   * - `auto`：装了哪个用哪个，melo 优先
   * - `melo`：中英混读、44.1kHz、单音色，RTF 0.32——中英混排的通话首选
   * - `matcha`：最快（RTF 0.06）、中文最稳，但一个英文都不认得
   * - `kokoro`：103 音色、音质更好，但 RTF 0.5，通话首声会慢到 1.8 秒
   *
   * 指定的模型没装时自动回退，不会变成没声音。
   */
  ttsModel: TtsModel
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
