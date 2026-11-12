import hljs from 'highlight.js/lib/common'
import MarkdownIt from 'markdown-it'

const md = new MarkdownIt({
  html: false,
  linkify: true,
  breaks: true,
  typographer: false,
})

const escapeHtml = (s: string) => md.utils.escapeHtml(s)

// 代码块：语言标签 + 复制按钮
md.renderer.rules.fence = (tokens, idx) => {
  const token = tokens[idx]
  const raw = token.content
  const info = (token.info || '').trim().split(/\s+/)[0]
  const lang = info && hljs.getLanguage(info) ? info : ''

  let highlighted: string
  if (lang) {
    try {
      highlighted = hljs.highlight(raw, { language: lang, ignoreIllegals: true }).value
    } catch {
      highlighted = escapeHtml(raw)
    }
  } else {
    highlighted = escapeHtml(raw)
  }

  const label = lang || 'text'
  return (
    `<div class="code-block">` +
    `<div class="code-head"><span class="code-lang">${escapeHtml(label)}</span>` +
    `<button class="code-copy" type="button" data-code="${encodeURIComponent(raw)}">复制</button></div>` +
    `<pre class="hljs"><code>${highlighted}</code></pre>` +
    `</div>`
  )
}

// 外链统一新窗口打开（点击在组件层拦截，交给系统浏览器）
const defaultLinkOpen =
  md.renderer.rules.link_open ||
  ((tokens, idx, options, _env, self) => self.renderToken(tokens, idx, options))
md.renderer.rules.link_open = (tokens, idx, options, env, self) => {
  const token = tokens[idx]
  token.attrSet('target', '_blank')
  token.attrSet('rel', 'noopener noreferrer')
  return defaultLinkOpen(tokens, idx, options, env, self)
}

// 表格外层包一层，便于横向滚动
md.renderer.rules.table_open = () => '<div class="table-wrap"><table>'
md.renderer.rules.table_close = () => '</table></div>'

export function renderMarkdown(text: string): string {
  if (!text) return ''
  return md.render(text)
}
