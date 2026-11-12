/** 把界面设置重置为默认值（用于本地联调后清理） */
const list = await (await fetch('http://127.0.0.1:9223/json')).json()
const page = list.find((t) => t.type === 'page')
const ws = new WebSocket(page.webSocketDebuggerUrl)
await new Promise((r, j) => {
  ws.onopen = r
  ws.onerror = j
})

let id = 0
const pending = new Map()
ws.onmessage = (e) => {
  const m = JSON.parse(e.data)
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)(m)
    pending.delete(m.id)
  }
}
const send = (method, params = {}) => {
  const i = ++id
  ws.send(JSON.stringify({ id: i, method, params }))
  return new Promise((r) => pending.set(i, r))
}

const defaults = {
  baseUrl: 'http://127.0.0.1:4000',
  apiKey: '',
  model: 'qwen3-vl-8b',
  temperature: 0.7,
  maxTokens: 2048,
  systemPrompt:
    '你是 Shania，一个运行在用户桌面上的 AI 智能体。回答要准确、简洁、有条理；' +
    '需要实时信息或本机信息时，主动调用可用工具，不要凭空猜测。' +
    '默认使用简体中文回答，代码与技术术语保持原样。',
  toolsEnabled: true,
  theme: 'system',
  displayName: 'Shania',
  shortcut: 'CmdOrCtrl+Shift+Space',
  closeAction: 'tray',
  autoStart: false,
  contextLimit: 32000,
  profiles: [],
  activeProfileId: '',
}

await send('Runtime.evaluate', {
  expression: `localStorage.setItem('shania.settings.v1', ${JSON.stringify(JSON.stringify(defaults))}); true`,
})

// 把 Rust 侧的关闭行为也同步回默认值，避免下次点关闭直接退出
await send('Runtime.evaluate', {
  expression: `window.__TAURI_INTERNALS__?.invoke('set_close_action', { action: 'tray' })`,
  awaitPromise: true,
})

console.log('settings restored')
ws.close()
process.exit(0)
