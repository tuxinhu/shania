/**
 * 全局快捷键的展示与录制。
 * 存储格式与 Rust 侧解析器（global-hotkey）保持一致，例如 CmdOrCtrl+Shift+Space。
 */

export const DEFAULT_SHORTCUT = 'CmdOrCtrl+Shift+Space'

const isMac =
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.userAgent)

/** 修饰键的显示名：mac 用符号，Windows 用文字 */
const MOD_LABELS: Record<string, string> = {
  cmdorctrl: isMac ? '⌘' : 'Ctrl',
  commandorcontrol: isMac ? '⌘' : 'Ctrl',
  commandorctrl: isMac ? '⌘' : 'Ctrl',
  cmdorcontrol: isMac ? '⌘' : 'Ctrl',
  ctrl: isMac ? '⌃' : 'Ctrl',
  control: isMac ? '⌃' : 'Ctrl',
  shift: isMac ? '⇧' : 'Shift',
  alt: isMac ? '⌥' : 'Alt',
  option: isMac ? '⌥' : 'Alt',
  super: isMac ? '⌘' : 'Win',
  cmd: '⌘',
  command: '⌘',
  meta: '⌘',
}

const KEY_LABELS: Record<string, string> = {
  space: 'Space',
  enter: 'Enter',
  tab: 'Tab',
  backspace: 'Backspace',
  delete: 'Delete',
  escape: 'Esc',
  esc: 'Esc',
  arrowup: '↑',
  arrowdown: '↓',
  arrowleft: '←',
  arrowright: '→',
  up: '↑',
  down: '↓',
  left: '←',
  right: '→',
  home: 'Home',
  end: 'End',
  pageup: 'PageUp',
  pagedown: 'PageDown',
  insert: 'Insert',
  minus: '-',
  equal: '=',
  comma: ',',
  period: '.',
  slash: '/',
  semicolon: ';',
  quote: "'",
  bracketleft: '[',
  bracketright: ']',
  backslash: '\\',
  backquote: '`',
}

function keyLabel(token: string): string {
  const t = token.toLowerCase()
  if (/^key[a-z]$/.test(t)) return t.slice(3).toUpperCase()
  if (/^digit\d$/.test(t)) return t.slice(5)
  if (/^f\d{1,2}$/.test(t)) return t.toUpperCase()
  return KEY_LABELS[t] ?? token
}

/** 把存储用的组合键转成可读文本：CmdOrCtrl+Shift+Space → Ctrl + Shift + Space */
export function formatShortcut(accelerator: string): string {
  if (!accelerator) return '未设置'
  const parts = accelerator
    .split('+')
    .map((raw) => raw.trim())
    .filter(Boolean)
    .map((token) => MOD_LABELS[token.toLowerCase()] ?? keyLabel(token))
  return parts.join(isMac ? '' : ' + ')
}

/** 键盘事件里可以直接录制的键（其余键返回不支持） */
const CODE_TOKENS: Record<string, string> = {
  Space: 'Space',
  Enter: 'Enter',
  NumpadEnter: 'Enter',
  Tab: 'Tab',
  Backspace: 'Backspace',
  Delete: 'Delete',
  Insert: 'Insert',
  Home: 'Home',
  End: 'End',
  PageUp: 'PageUp',
  PageDown: 'PageDown',
  ArrowUp: 'ArrowUp',
  ArrowDown: 'ArrowDown',
  ArrowLeft: 'ArrowLeft',
  ArrowRight: 'ArrowRight',
  Minus: 'Minus',
  Equal: 'Equal',
  Comma: 'Comma',
  Period: 'Period',
  Slash: 'Slash',
  Semicolon: 'Semicolon',
  Quote: 'Quote',
  BracketLeft: 'BracketLeft',
  BracketRight: 'BracketRight',
  Backslash: 'Backslash',
  Backquote: 'Backquote',
}

export type CaptureResult =
  | { ok: true; accelerator: string }
  | { ok: false; reason: string }

/**
 * 从键盘事件生成组合键。
 * reason 为空串表示「只按了修饰键」，属于正常录制过程，不当作错误提示。
 */
export function acceleratorFromEvent(event: KeyboardEvent): CaptureResult {
  const code = event.code || ''

  // 只按了修饰键：继续等待主键
  if (/^(Control|Shift|Alt|Meta|OS)(Left|Right)?$/.test(code)) {
    return { ok: false, reason: '' }
  }

  let token = ''
  if (/^Key[A-Z]$/.test(code)) token = code.slice(3)
  else if (/^Digit[0-9]$/.test(code)) token = code.slice(5)
  else if (/^F[0-9]{1,2}$/.test(code)) token = code
  else if (CODE_TOKENS[code]) token = CODE_TOKENS[code]

  if (!token) return { ok: false, reason: '这个键暂不支持，换一个试试' }

  const mods: string[] = []
  if (isMac ? event.metaKey : event.ctrlKey) mods.push('CmdOrCtrl')
  if (isMac && event.ctrlKey) mods.push('Ctrl')
  if (!isMac && event.metaKey) mods.push('Super')
  if (event.altKey) mods.push('Alt')
  if (event.shiftKey) mods.push('Shift')

  if (!mods.length) {
    return { ok: false, reason: '需要至少搭配一个修饰键（Ctrl / Alt / Shift）' }
  }

  return { ok: true, accelerator: [...mods, token].join('+') }
}
