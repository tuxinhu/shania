import type { Attachment, ContentPart, Message, WireMessage } from './types'

/** 粗略 token 估算：中日韩字符约 1 token/字，其余按 3.5 字符/token */
export function estimateTextTokens(text: string): number {
  let wide = 0
  let narrow = 0
  for (const ch of text) {
    const code = ch.codePointAt(0) ?? 0
    if (
      (code >= 0x3000 && code <= 0x303f) ||
      (code >= 0x4e00 && code <= 0x9fff) ||
      (code >= 0xff00 && code <= 0xffef)
    ) {
      wide += 1
    } else {
      narrow += 1
    }
  }
  return Math.ceil(wide * 1.05 + narrow / 3.5)
}

/** 一张图片按视觉 token 粗估；与具体模型有关，取一个偏保守的常量 */
export const IMAGE_TOKEN_COST = 1200

export function estimateMessageTokens(m: Message): number {
  let cost = estimateTextTokens(m.content) + 4
  for (const a of m.attachments ?? []) {
    // 文本附件按正文字数算，图片按固定视觉 token 估
    cost += a.kind === 'text' ? estimateTextTokens(a.text ?? '') : IMAGE_TOKEN_COST
  }
  return cost
}

/** 内容里已有三反引号时加长围栏，避免把 Markdown 结构写坏 */
function fence(text: string): string {
  const ticks = text.includes('```') ? '````' : '```'
  return `${ticks}\n${text}\n${ticks}`
}

/** 文本附件在上下文里的呈现：文件名 + 正文围栏 */
function textBlock(a: Attachment): string {
  return `【附件：${a.name}】\n${fence(a.text ?? '')}`
}

function isImage(a: Attachment): boolean {
  // 老数据没有 kind 字段，一律当图片（那时只支持图片）
  return (a.kind ?? 'image') === 'image' && !!a.dataUrl
}

/** 把本地消息转成 OpenAI 兼容格式；带图时 content 变成多模态数组 */
export function toWire(m: Message): WireMessage {
  const role = m.role as 'user' | 'assistant'
  const attachments = m.attachments ?? []
  const images = attachments.filter(isImage)
  const texts = attachments.filter((a) => a.kind === 'text')

  // 文本附件直接并入正文，不占多模态数组
  const body = [m.content.trim(), ...texts.map(textBlock)].filter(Boolean).join('\n\n')

  if (!images.length) {
    return { role, content: body }
  }

  const parts: ContentPart[] = [{ type: 'text', text: body || '（用户发送了图片）' }]
  for (const a of images) {
    parts.push({ type: 'image_url', image_url: { url: a.dataUrl } })
  }
  return { role, content: parts }
}

export interface WireBuildResult {
  messages: WireMessage[]
  /** 被裁掉的消息条数，用于界面提示 */
  omitted: number
  /** 估算 token 用量 */
  tokens: number
}

/**
 * 把完整历史压到预算内：从最新往回累加，超预算就停，
 * 并在最前面插入一条提示，让模型知道上下文被裁剪过。
 */
export function buildWireDetailed(messages: Message[], limit: number): WireBuildResult {
  const candidates = messages.filter(
    (m) =>
      (m.role === 'user' || m.role === 'assistant') &&
      (m.content.trim().length > 0 || (m.attachments?.length ?? 0) > 0),
  )
  if (!candidates.length) return { messages: [], omitted: 0, tokens: 0 }

  const budget = Math.max(1000, limit)
  let used = 0
  let start = 0
  let omitted = 0

  for (let i = candidates.length - 1; i >= 0; i -= 1) {
    const cost = estimateMessageTokens(candidates[i])
    // 无论如何至少保留最后一条，否则会发出空请求
    if (used + cost > budget && i < candidates.length - 1) {
      start = i + 1
      omitted = i + 1
      break
    }
    used += cost
    start = i
  }

  const wire = candidates.slice(start).map(toWire)
  if (omitted > 0) {
    wire.unshift({
      role: 'assistant',
      content: `（为控制上下文长度，已省略更早的 ${omitted} 条消息）`,
    })
  }
  return { messages: wire, omitted, tokens: used }
}

export function buildWire(messages: Message[], limit: number): WireMessage[] {
  return buildWireDetailed(messages, limit).messages
}
