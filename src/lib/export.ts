import { invoke } from '@tauri-apps/api/core'
import { isTauri } from './api'
import type { Message, Session } from './types'

export type ExportResult = 'saved' | 'cancelled' | 'failed'

function pad(n: number) {
  return String(n).padStart(2, '0')
}

function stamp(ts: number) {
  const d = new Date(ts)
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}

/** 去掉文件名里不允许的字符 */
function safeFileName(title: string) {
  const cleaned = title
    .replace(/[\\/:*?"<>|]/g, '_')
    .replace(/\s+/g, ' ')
    .trim()
  return (cleaned || '会话').slice(0, 60)
}

/** 内容里已有三反引号时改用四反引号包裹，避免 Markdown 结构被破坏 */
function fence(text: string, lang = '') {
  const ticks = text.includes('```') ? '````' : '```'
  return `${ticks}${lang}\n${text}\n${ticks}`
}

function toolBlock(m: Message): string {
  if (!m.toolCalls?.length) return ''
  const blocks = m.toolCalls.map((c) => {
    const status = c.status === 'error' ? '失败' : c.status === 'running' ? '运行中' : '完成'
    const cost = c.elapsedMs != null ? ` · ${c.elapsedMs} ms` : ''
    const head = `**${c.name}**　${status}${cost}`
    const args = c.arguments?.trim() || '{}'
    const result = c.result?.trim()
    const body = [
      '参数：',
      fence(args, 'json'),
      result ? '\n返回：' : '',
      result ? fence(result) : '',
    ]
      .filter(Boolean)
      .join('\n')
    return `<details>\n<summary>${head}</summary>\n\n${body}\n\n</details>`
  })
  return blocks.join('\n\n')
}

export function sessionToMarkdown(s: Session, displayName = 'Shania'): string {
  const head = [
    `# ${s.title}`,
    '',
    `> 创建于 ${stamp(s.createdAt)}　·　最后更新 ${stamp(s.updatedAt)}　·　共 ${s.messages.length} 条消息`,
    '',
  ]

  const body = s.messages.map((m) => {
    if (m.role === 'user') {
      // 图片是 data URL，直接写进 Markdown 会让文件暴涨，这里只注明数量
      const images = (m.attachments ?? []).filter((a) => (a.kind ?? 'image') === 'image')
      const texts = (m.attachments ?? []).filter((a) => a.kind === 'text')
      const notes: string[] = []
      if (images.length) {
        notes.push(`含 ${images.length} 张图片：${images.map((a) => a.name).join('、')}（Markdown 导出不含图片内容）`)
      }
      if (texts.length) {
        notes.push(`含 ${texts.length} 个文本附件：${texts.map((a) => a.name).join('、')}（正文已随消息发送，此处不重复）`)
      }
      const extra = notes.length ? `\n\n> ${notes.join('；')}` : ''
      return `## 我\n\n${m.content}${extra}\n`
    }
    const parts = [`## ${displayName}`]
    if (m.model) parts.push(`\n*${m.model}${m.usage?.total_tokens ? ` · ${m.usage.total_tokens} tokens` : ''}*`)
    if (m.reasoning) {
      parts.push(`\n<details>\n<summary>思考过程（${m.reasoning.length} 字）</summary>\n\n${m.reasoning}\n\n</details>`)
    }
    const tools = toolBlock(m)
    if (tools) parts.push(`\n${tools}`)
    if (m.content.trim()) parts.push(`\n${m.content}`)
    if (m.error) parts.push(`\n> 错误：${m.error}`)
    return parts.join('\n') + '\n'
  })

  return [...head, body.join('\n---\n\n'), ''].join('\n')
}

export function sessionToJson(s: Session): string {
  return JSON.stringify(
    {
      exportedAt: new Date().toISOString(),
      app: 'Shania Agent',
      session: s,
    },
    null,
    2,
  )
}

function browserDownload(name: string, content: string, mime: string) {
  const blob = new Blob([content], { type: `${mime};charset=utf-8` })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = name
  a.click()
  URL.revokeObjectURL(url)
}

/**
 * 导出会话。桌面端弹原生保存对话框，按用户选择的后缀决定格式；
 * 浏览器预览环境退化为直接下载 Markdown。
 */
export async function exportSession(session: Session, displayName: string): Promise<ExportResult> {
  const base = safeFileName(session.title)

  if (!isTauri) {
    browserDownload(`${base}.md`, sessionToMarkdown(session, displayName), 'text/markdown')
    return 'saved'
  }

  try {
    const { save } = await import('@tauri-apps/plugin-dialog')
    const path = await save({
      title: '导出会话',
      defaultPath: `${base}.md`,
      filters: [
        { name: 'Markdown', extensions: ['md'] },
        { name: 'JSON', extensions: ['json'] },
      ],
    })
    if (!path) return 'cancelled'

    const content = path.toLowerCase().endsWith('.json')
      ? sessionToJson(session)
      : sessionToMarkdown(session, displayName)

    await invoke('write_text_file', { path, content })
    return 'saved'
  } catch (err) {
    console.error('导出失败', err)
    return 'failed'
  }
}
