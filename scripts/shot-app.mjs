/**
 * 从运行中的应用抓一张截图，并顺带打印页面状态。
 * 用法：node scripts/shot-app.mjs <输出路径> [宽度] [高度]
 */
import { writeFileSync, mkdirSync } from 'node:fs'
import { dirname } from 'node:path'

const out = process.argv[2] || '.preview/app.png'
const width = Number(process.argv[3] || 1200)
const height = Number(process.argv[4] || 800)

mkdirSync(dirname(out), { recursive: true })

const list = await (await fetch('http://127.0.0.1:9223/json')).json()
const page = list.find((t) => t.type === 'page')
if (!page) throw new Error('未找到应用页面，确认应用已带 --remote-debugging-port=9223 启动')

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

await send('Emulation.setDeviceMetricsOverride', {
  width,
  height,
  deviceScaleFactor: 1,
  mobile: false,
})

const state = await send('Runtime.evaluate', {
  expression: `JSON.stringify({
    sessions: document.querySelectorAll('.session-item').length,
    hasEmpty: !!document.querySelector('.empty'),
    toolCards: document.querySelectorAll('.tool-card').length,
    theme: document.documentElement.getAttribute('data-theme'),
    title: document.title,
  })`,
  returnByValue: true,
})
console.log('state:', state.result?.result?.value)

await new Promise((r) => setTimeout(r, 800))
const shot = await send('Page.captureScreenshot', { format: 'png' })
writeFileSync(out, Buffer.from(shot.result.data, 'base64'))
console.log('saved', out)

ws.close()
process.exit(0)
