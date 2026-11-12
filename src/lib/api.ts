import { Channel, invoke } from '@tauri-apps/api/core'
import type { StreamEvent, WireMessage } from './types'

export const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

export interface ChatConfigPayload {
  baseUrl: string
  apiKey: string
  model: string
  temperature?: number
  maxTokens?: number
  systemPrompt?: string
  toolsEnabled?: boolean
}

export interface ChatStreamPayload {
  requestId: string
  config: ChatConfigPayload
  messages: WireMessage[]
}

/** 走 Tauri 时用 Rust 侧 SSE 代理；浏览器预览时用本地模拟流 */
export async function streamChat(
  payload: ChatStreamPayload,
  onEvent: (event: StreamEvent) => void,
): Promise<void> {
  if (isTauri) {
    const channel = new Channel<StreamEvent>()
    channel.onmessage = onEvent
    await invoke('chat_stream', {
      requestId: payload.requestId,
      config: payload.config,
      messages: payload.messages,
      channel,
    })
    return
  }
  return mockStream(payload, onEvent)
}

export async function stopChat(requestId: string): Promise<void> {
  if (isTauri) {
    await invoke('stop_chat', { requestId })
    return
  }
  mockCancelled.add(requestId)
}

export async function loadSessionsRaw(): Promise<string> {
  if (isTauri) {
    return invoke<string>('load_sessions')
  }
  return localStorage.getItem('shania.sessions') || '[]'
}

export async function saveSessionsRaw(json: string): Promise<void> {
  if (isTauri) {
    await invoke('save_sessions', { json })
    return
  }
  localStorage.setItem('shania.sessions', json)
}

/* ------------------------------------------------------------------ */
/* 共享设置文件（~/.shania/settings.json，桌面端与 CLI 共用）              */
/* ------------------------------------------------------------------ */

/** 读共享设置；文件还不存在时返回 null（首次启动由前端决定是否迁移旧数据） */
export async function loadSettingsRaw(): Promise<string | null> {
  if (isTauri) {
    return invoke<string | null>('load_settings')
  }
  return localStorage.getItem('shania.settings.v1')
}

export async function saveSettingsRaw(json: string): Promise<void> {
  if (isTauri) {
    await invoke('save_settings', { json })
    return
  }
  localStorage.setItem('shania.settings.v1', json)
}

export interface SttProgress {
  stage: 'stream' | 'tts' | 'done'
  done: number
  total: number | null
}

/** X-ASR 模型状态：装没装、占多少磁盘、有没有加载进内存 */
export interface StreamStatus {
  ready: boolean
  bytes: number
  loaded: boolean
  dir: string
}

export async function sttStreamStatus(): Promise<StreamStatus | null> {
  if (!isTauri) return null
  return invoke<StreamStatus>('stt_stream_status')
}

/** 下载 X-ASR 流式模型（约 134MB），进度走 stt-progress，stage 为 stream */
export async function sttStreamInstall(): Promise<void> {
  if (!isTauri) throw new Error('浏览器预览环境不支持安装语音模型')
  await invoke('stt_stream_install')
}

/** 卸载 X-ASR 识别器释放内存（下次录音会重新加载） */
export async function sttStreamUnload(): Promise<void> {
  if (!isTauri) return
  await invoke('stt_stream_unload')
}

/* ------------------------------------------------------------------ */
/* 本地语音合成（MeloTTS）                                              */
/* ------------------------------------------------------------------ */

/** 模型没装好：调用方据此提示去设置里下载 */
export const TTS_NOT_INSTALLED = 'TTS_NOT_INSTALLED'

/** 语音合成模型状态 */
export interface TtsStatus {
  ready: boolean
  bytes: number
  loaded: boolean
  dir: string
  /** 音色总数，0 表示模型未加载 */
  speakers: number
  /** 输出采样率，Hz */
  rate: number
}

/** 一段合成好的语音：base64 的小端 i16 单声道 PCM */
export interface Speech {
  pcm: string
  rate: number
  durationMs: number
}

/** 模型状态：装没装、占多少磁盘、有没有加载进内存 */
export async function ttsStatus(): Promise<TtsStatus | null> {
  if (!isTauri) return null
  return invoke<TtsStatus>('tts_status')
}

/** 合成一句话。第一次调用会按需加载模型（约 5 秒） */
export async function ttsSay(text: string, speed: number): Promise<Speech> {
  if (!isTauri) throw new Error('浏览器预览环境没有本地语音合成')
  return invoke<Speech>('tts_say', { text, speed })
}

/** 提前加载合成器，避免通话第一句等几秒 */
export async function ttsWarmup(): Promise<void> {
  if (!isTauri) return
  await invoke('tts_warmup')
}

/** 卸载合成器，释放约 400MB 内存 */
export async function ttsUnload(): Promise<void> {
  if (!isTauri) return
  await invoke('tts_unload')
}

/** 下载语音合成模型（约 163MB）。进度走 stt-progress，stage 为 tts */
export async function ttsInstall(): Promise<void> {
  if (!isTauri) throw new Error('浏览器预览环境不支持安装语音模型')
  await invoke('tts_install')
}

/** 打开 `~/.shania/logs/`，排查问题时用 */
export async function openLogsDir(): Promise<void> {
  if (!isTauri) throw new Error('浏览器预览环境没有本地日志目录')
  await invoke('open_logs_dir')
}

/** 设置文件的绝对路径，用于在界面上告知用户「配置存在哪」 */
export async function settingsLocation(): Promise<string | null> {
  if (!isTauri) return null
  return invoke<string>('settings_location')
}

/**
 * 应用全局唤起快捷键。传空串表示暂时注销（录制新键时用）。
 * 浏览器预览环境没有系统级热键，直接忽略。
 */
export async function applyGlobalShortcut(accelerator: string): Promise<void> {
  if (!isTauri) return
  await invoke<string>('set_global_shortcut', { accelerator })
}

/* ------------------------------------------------------------------ */
/* 连通性测试 / 系统集成                                                 */
/* ------------------------------------------------------------------ */

export interface TestResult {
  latencyMs: number
  model: string
  endpoint: string
}

/** 发一个最小请求，验证地址、密钥与模型名 */
export async function testConnection(config: ChatConfigPayload): Promise<TestResult> {
  if (isTauri) {
    return invoke<TestResult>('test_connection', { config })
  }
  // 浏览器预览：没有后端，给一个可辨识的模拟结果
  await new Promise((r) => setTimeout(r, 420))
  return { latencyMs: 420, model: config.model, endpoint: '（浏览器预览，未实际请求）' }
}

/** 同步「关闭按钮行为」到 Rust 侧 */
export async function setCloseAction(action: 'tray' | 'quit'): Promise<void> {
  if (!isTauri) return
  await invoke('set_close_action', { action })
}

/* ------------------------------------------------------------------ */
/* 命令行参数入口                                                       */
/* ------------------------------------------------------------------ */

/** 后端解析出的启动意图，字段名与 Rust 侧 CliAction 的 serde 标签一致 */
export type CliAction =
  | { action: 'newChat' }
  | { action: 'settings' }
  | { action: 'prompt'; text: string }

/**
 * 取走首启时暂存的参数。
 * 后端在窗口加载完成前无法把事件送达前端，所以先存一份，这里主动来取。
 */
export async function takeCliArgs(): Promise<CliAction | null> {
  if (!isTauri) return null
  return invoke<CliAction | null>('take_cli_args')
}

/** 应用已在运行时，第二次启动带的参数会通过事件送过来 */
export async function onCliArgs(handler: (action: CliAction) => void): Promise<() => void> {
  if (!isTauri) return () => {}
  const { listen } = await import('@tauri-apps/api/event')
  return listen<CliAction>('cli-args', (event) => handler(event.payload))
}

export async function isAutoStartEnabled(): Promise<boolean> {
  if (!isTauri) return false
  const { isEnabled } = await import('@tauri-apps/plugin-autostart')
  return isEnabled()
}

export async function setAutoStart(enabled: boolean): Promise<void> {
  if (!isTauri) return
  const { enable, disable } = await import('@tauri-apps/plugin-autostart')
  if (enabled) await enable()
  else await disable()
}

export type UpdateStatus = 'available' | 'none' | 'unsupported' | 'error'

export interface UpdateCheckResult {
  status: UpdateStatus
  version?: string
  notes?: string
  message?: string
}

/**
 * 检查更新。
 * 未配置更新源时不当作失败——这是开发期的正常状态，给出明确提示即可。
 */
export async function checkForUpdate(): Promise<UpdateCheckResult> {
  if (!isTauri) {
    return { status: 'unsupported', message: '浏览器预览环境不支持自动更新' }
  }

  try {
    const { check } = await import('@tauri-apps/plugin-updater')
    const update = await check()
    if (!update) return { status: 'none' }
    return {
      status: 'available',
      version: update.version,
      notes: update.body ?? '',
    }
  } catch (err) {
    const text = err instanceof Error ? err.message : String(err)
    if (/endpoint|configured|pubkey|public key/i.test(text)) {
      return {
        status: 'unsupported',
        message: '尚未配置更新服务器，当前版本不会自动检查更新。',
      }
    }
    return { status: 'error', message: text.slice(0, 200) }
  }
}

/** 下载并安装更新，完成后重启应用 */
export async function installUpdate(onProgress?: (percent: number) => void): Promise<void> {
  if (!isTauri) return
  const { check } = await import('@tauri-apps/plugin-updater')
  const update = await check()
  if (!update) return

  let downloaded = 0
  let total = 0
  await update.downloadAndInstall((event) => {
    if (event.event === 'Started') {
      total = event.data.contentLength ?? 0
    } else if (event.event === 'Progress') {
      downloaded += event.data.chunkLength
      if (total > 0) onProgress?.(Math.min(99, Math.round((downloaded / total) * 100)))
    } else if (event.event === 'Finished') {
      onProgress?.(100)
    }
  })

  const { relaunch } = await import('@tauri-apps/plugin-process')
  await relaunch()
}

/* ------------------------------------------------------------------ */
/* 浏览器预览用的模拟流（仅当不在 Tauri 容器内时生效）                    */
/* ------------------------------------------------------------------ */

const mockCancelled = new Set<string>()
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

async function typeOut(
  text: string,
  requestId: string,
  emit: (piece: string) => void,
  delay = 14,
) {
  let buf = ''
  for (const ch of text) {
    if (mockCancelled.has(requestId)) {
      mockCancelled.delete(requestId)
      return false
    }
    buf += ch
    // 按词/字符块推送，避免过于细碎
    if (buf.length >= 2 || ch === '\n' || /[，。！？、；：]/.test(ch)) {
      emit(buf)
      buf = ''
      await sleep(delay)
    }
  }
  if (buf) emit(buf)
  return true
}

async function mockStream(
  payload: ChatStreamPayload,
  onEvent: (event: StreamEvent) => void,
): Promise<void> {
  const { requestId, messages, config } = payload
  const lastUser = [...messages].reverse().find((m) => m.role === 'user')
  const rawContent = lastUser?.content
  const questionText =
    typeof rawContent === 'string'
      ? rawContent
      : (rawContent?.find((p) => p.type === 'text')?.text ?? '')
  const question = questionText.trim() || '你好'

  await sleep(220)
  if (mockCancelled.has(requestId)) {
    mockCancelled.delete(requestId)
    onEvent({ type: 'done', usage: null })
    return
  }

  const reasoning =
    '用户在浏览器预览环境下发来消息。当前没有连接真实的模型接口，我先返回一段演示内容，' +
    '同时模拟一次工具调用，方便检查流式渲染与工具卡片的展示效果。'

  onEvent({ type: 'round', index: 0 })
  if (!(await typeOut(reasoning, requestId, (d) => onEvent({ type: 'reasoning', delta: d }), 10))) {
    onEvent({ type: 'done', usage: null })
    return
  }

  const toolId = `mock_${Date.now()}`
  await sleep(180)
  onEvent({ type: 'toolStart', id: toolId, name: 'get_current_time', arguments: '{}' })
  await sleep(520)
  const now = new Date()
  onEvent({
    type: 'toolResult',
    id: toolId,
    name: 'get_current_time',
    ok: true,
    result: `本地时间：${now.toLocaleString('zh-CN')}\n时区：${Intl.DateTimeFormat().resolvedOptions().timeZone}`,
    elapsedMs: 520,
  })

  const answer = [
    `收到：**${question}**`,
    '',
    '目前运行在**浏览器预览模式**，还没有连上真实的模型服务。在桌面端（`pnpm app:dev`）里打开设置页，填入接口地址和模型名，就能获得真正的流式对话与工具调用。',
    '',
    '顺便验证一下渲染效果：',
    '',
    '```ts',
    "const reply = await streamChat({",
    "  requestId: 'req-1',",
    "  config: { baseUrl, apiKey, model },",
    '  messages,',
    '})',
    '```',
    '',
    '- 流式输出逐字渲染',
    '- Markdown 与代码高亮可用',
    '- 工具调用有独立卡片',
    '',
    `当前配置：\`${config.model || '未设置模型'}\``,
  ].join('\n')

  if (!(await typeOut(answer, requestId, (d) => onEvent({ type: 'content', delta: d }), 12))) {
    onEvent({ type: 'done', usage: null })
    return
  }

  onEvent({
    type: 'done',
    usage: { prompt_tokens: 128, completion_tokens: 256, total_tokens: 384 },
  })
}
