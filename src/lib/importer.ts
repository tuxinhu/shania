import { invoke } from '@tauri-apps/api/core'
import { isTauri } from './api'
import type { Attachment, Message, Session } from './types'

/**
 * 会话导入。
 *
 * 兼容三种输入形态（导出文件就是第一种）：
 *   1. `{ exportedAt, app, session }` —— `sessionToJson` 的产物
 *   2. `{ sessions: [...] }` 或裸数组 —— 存储层的整份快照
 *   3. 单个裸 session 对象
 *
 * 只保留已知字段，其余丢弃：导出的 JSON 可能来自旧版本或别处，直接塞进
 * store 会带进一堆不可预期的属性。
 */

function asRecord(v: unknown): Record<string, unknown> | null {
  return v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null
}

function normalizeAttachments(raw: unknown): Attachment[] | undefined {
  if (!Array.isArray(raw)) return undefined
  const list: Attachment[] = []
  for (const item of raw) {
    const a = asRecord(item)
    if (!a) continue
    const dataUrl = typeof a.dataUrl === 'string' ? a.dataUrl : ''
    if (!dataUrl) continue
    list.push({
      id: typeof a.id === 'string' && a.id ? a.id : `att_${list.length}_${Math.random().toString(36).slice(2, 8)}`,
      name: typeof a.name === 'string' && a.name ? a.name : '附件',
      mime: typeof a.mime === 'string' ? a.mime : 'application/octet-stream',
      dataUrl,
      size: typeof a.size === 'number' ? a.size : 0,
      kind: a.kind === 'text' ? 'text' : 'image',
      text: typeof a.text === 'string' ? a.text : undefined,
    })
  }
  return list.length ? list : undefined
}

function normalizeMessages(raw: unknown): Message[] | null {
  if (!Array.isArray(raw)) return null
  const out: Message[] = []
  for (const item of raw) {
    const m = asRecord(item)
    if (!m) continue
    const role = m.role
    if (role !== 'user' && role !== 'assistant' && role !== 'system') continue
    const content = typeof m.content === 'string' ? m.content : ''
    const attachments = normalizeAttachments(m.attachments)
    if (!content.trim() && !attachments) continue
    out.push({
      id: typeof m.id === 'string' && m.id ? m.id : `msg_${out.length}_${Math.random().toString(36).slice(2, 8)}`,
      role,
      content,
      reasoning: typeof m.reasoning === 'string' ? m.reasoning : undefined,
      attachments,
      // 导入的历史不该停留在「正在输出」状态
      status: 'done',
      createdAt: typeof m.createdAt === 'number' ? m.createdAt : Date.now(),
      model: typeof m.model === 'string' ? m.model : undefined,
    })
  }
  return out
}

function normalizeSession(raw: unknown): Session | null {
  const s = asRecord(raw)
  if (!s) return null
  const messages = normalizeMessages(s.messages)
  if (!messages) return null

  const now = Date.now()
  const createdAt = typeof s.createdAt === 'number' ? s.createdAt : now
  return {
    id: typeof s.id === 'string' && s.id ? s.id : `imported_${now.toString(36)}_${Math.random().toString(36).slice(2, 8)}`,
    title: typeof s.title === 'string' && s.title.trim() ? s.title.trim().slice(0, 80) : '导入的对话',
    messages,
    createdAt,
    updatedAt: typeof s.updatedAt === 'number' ? s.updatedAt : createdAt,
    pinned: s.pinned === true ? true : undefined,
    archived: s.archived === true ? true : undefined,
  }
}

/** 解析 JSON 文本；结构不认识时抛错，错误信息直接给用户看 */
export function parseSessionsJson(text: string): Session[] {
  let data: unknown
  try {
    data = JSON.parse(text)
  } catch (err) {
    throw new Error(`不是合法的 JSON：${err instanceof Error ? err.message : String(err)}`)
  }

  const candidates: unknown[] = []
  if (Array.isArray(data)) {
    candidates.push(...data)
  } else {
    const obj = asRecord(data)
    if (!obj) throw new Error('文件内容不是会话数据')
    if (Array.isArray(obj.sessions)) candidates.push(...obj.sessions)
    else if (obj.session) candidates.push(obj.session)
    else candidates.push(obj)
  }

  const sessions = candidates.map(normalizeSession).filter((s): s is Session => s !== null)
  if (!sessions.length) throw new Error('文件里没有可导入的会话（缺少 messages 或结构不匹配）')
  return sessions
}

export type ImportPickResult =
  | { status: 'ok'; sessions: Session[] }
  | { status: 'cancelled' }
  | { status: 'failed'; message: string }

/** 弹原生文件选择框 → 读文件 → 解析。浏览器预览环境不支持导入。 */
export async function pickSessionsToImport(): Promise<ImportPickResult> {
  if (!isTauri) {
    return { status: 'failed', message: '浏览器预览环境不支持导入会话' }
  }

  try {
    const { open } = await import('@tauri-apps/plugin-dialog')
    const path = await open({
      title: '导入会话',
      multiple: false,
      directory: false,
      filters: [{ name: '会话 JSON', extensions: ['json'] }],
    })
    if (!path || typeof path !== 'string') return { status: 'cancelled' }

    const text = await invoke<string>('read_import_file', { path })
    return { status: 'ok', sessions: parseSessionsJson(text) }
  } catch (err) {
    return { status: 'failed', message: err instanceof Error ? err.message : String(err) }
  }
}
