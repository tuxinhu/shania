import type { Attachment } from './types'

/**
 * 文本类附件：读取文件正文，随消息一起送给模型。
 *
 * 这里不做「上传」——模型接口只接受纯文本与图片，所以文本文件是把内容
 * 转成文字拼进上下文（见 `context.ts` 的 `toWire`）。二进制文件没有通用
 * 处理方式，直接拒绝，避免用户以为它能被模型理解。
 */

/** 常见纯文本后缀。`text/*` 的 mime 已经能覆盖大部分，这里是兜底。 */
const TEXT_EXTENSIONS = new Set([
  'txt', 'md', 'markdown', 'rst', 'log', 'ini', 'conf', 'cfg', 'env', 'properties',
  'json', 'jsonc', 'json5', 'yaml', 'yml', 'toml', 'xml', 'csv', 'tsv', 'srt', 'vtt',
  'html', 'htm', 'css', 'scss', 'sass', 'less', 'vue', 'svelte', 'astro',
  'js', 'mjs', 'cjs', 'ts', 'mts', 'cts', 'jsx', 'tsx', 'dart', 'swift', 'kt', 'kts',
  'rs', 'go', 'py', 'rb', 'php', 'java', 'scala', 'cs', 'fs', 'c', 'h', 'cc', 'cpp',
  'hpp', 'm', 'mm', 'lua', 'pl', 'r', 'jl', 'ex', 'exs', 'erl', 'hs', 'clj', 'zig',
  'sh', 'bash', 'zsh', 'ps1', 'bat', 'cmd', 'sql', 'graphql', 'gql', 'proto', 'dockerfile',
])

/** 单个文本附件上限：512KB，超过基本是数据文件，塞进上下文会直接撑爆预算 */
const MAX_TEXT_BYTES = 512 * 1024
/** 实际注入上下文的最大字符数，超出部分截断并注明 */
const MAX_TEXT_CHARS = 200_000

function extensionOf(name: string): string {
  const dot = name.lastIndexOf('.')
  return dot === -1 ? '' : name.slice(dot + 1).toLowerCase()
}

/** 判断是否当作文本处理：mime 优先，后缀兜底（有些系统给空 mime） */
export function isTextFile(file: File): boolean {
  if (file.type.startsWith('text/')) return true
  if (file.type === 'application/json' || file.type === 'application/xml') return true
  const ext = extensionOf(file.name || '')
  if (ext && TEXT_EXTENSIONS.has(ext)) return true
  // 没有后缀但名字本身就是常见文件名
  return /^(dockerfile|makefile|readme|license)$/i.test(file.name || '')
}

function localId(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) return crypto.randomUUID()
  return `txt_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 8)}`
}

/**
 * 把文本文件读成附件。
 * 超过上限时截断而不是拒绝——用户往往就是想问「这段报错什么意思」，
 * 前 20 万字通常已经包含关键信息。
 */
export async function fileToTextAttachment(file: File, index: number): Promise<Attachment> {
  const name = file.name || `粘贴的文本 ${index + 1}`
  if (file.size > MAX_TEXT_BYTES) {
    throw new Error(`${name} 超过 512KB，请改用文本编辑器截取片段`)
  }

  const raw = await file.text()
  const truncated = raw.length > MAX_TEXT_CHARS
  const text = truncated
    ? `${raw.slice(0, MAX_TEXT_CHARS)}\n\n…（文件过长，已截断，剩余 ${raw.length - MAX_TEXT_CHARS} 字符未包含）`
    : raw

  return {
    id: localId(),
    name,
    mime: file.type || 'text/plain',
    dataUrl: '',
    size: file.size,
    kind: 'text',
    text,
  }
}
