/**
 * 开发辅助：用 Chromium DevTools Protocol 截取界面状态，便于检查 UI。
 *
 * 用法：
 *   1. 启动 Chrome：chrome --headless=new --remote-debugging-port=9222 http://localhost:1420/
 *   2. node scripts/dev-shot.mjs .preview/chat.png chat
 */
import fs from 'node:fs'
import path from 'node:path'

const PORT = 9222
const APP_URL = 'http://localhost:1420'
const outFile = process.argv[2] || '.preview/shot.png'
const mode = process.argv[3] || 'chat'
const theme = process.argv[4] || 'light'

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function findTarget() {
  for (let i = 0; i < 60; i++) {
    try {
      const res = await fetch(`http://127.0.0.1:${PORT}/json`)
      const list = await res.json()
      const page = list.find((t) => t.type === 'page' && t.url.includes('1420'))
      if (page) return page
    } catch {
      /* 等待 Chrome 就绪 */
    }
    await sleep(300)
  }
  throw new Error('未找到调试目标，请确认 Chrome 已带 --remote-debugging-port 启动')
}

const target = await findTarget()
const ws = new WebSocket(target.webSocketDebuggerUrl)
await new Promise((resolve, reject) => {
  ws.onopen = resolve
  ws.onerror = reject
})

let seq = 0
const pending = new Map()
ws.onmessage = (ev) => {
  const msg = JSON.parse(ev.data)
  if (msg.id && pending.has(msg.id)) {
    pending.get(msg.id)(msg)
    pending.delete(msg.id)
  }
}

function send(method, params = {}) {
  const id = ++seq
  ws.send(JSON.stringify({ id, method, params }))
  return new Promise((resolve) => pending.set(id, resolve))
}

async function evaluate(expression) {
  const res = await send('Runtime.evaluate', {
    expression,
    returnByValue: true,
    awaitPromise: true,
  })
  if (res.result?.exceptionDetails) {
    throw new Error(res.result.exceptionDetails.text)
  }
  return res.result?.result?.value
}

await send('Page.enable')
// 视口尺寸走环境变量，方便在大窗口下检查留白（如 SHOT_W=1706 SHOT_H=1066 模拟
// 2560x1600 屏 150% 缩放后最大化时的 CSS 尺寸）
await send('Emulation.setDeviceMetricsOverride', {
  width: Number(process.env.SHOT_W || 1200),
  height: Number(process.env.SHOT_H || 800),
  deviceScaleFactor: 1,
  mobile: false,
})
// 每次从干净会话开始，避免历史数据干扰截图；顺手把拖出来的栏宽复位，
// 否则同一个 profile 里连拍几张，宽度会跟着上次拖拽漂移
await evaluate(
  "['shania.sessions','shania.sidebarW','shania.railPanelW'].forEach((k)=>localStorage.removeItem(k))",
)
await send('Page.reload', { ignoreCache: false })
await sleep(1600)

if (theme === 'dark') {
  await evaluate("document.documentElement.dataset.theme = 'dark'")
  await sleep(200)
}

if (mode === 'chat' || mode === 'chat-dark') {
  await evaluate("document.querySelector('.card')?.click()")
  await sleep(300)
  await evaluate("document.querySelector('.send:not(.stop)')?.click()")
  await sleep(9000)
}

if (mode === 'settings' || mode.startsWith('settings:')) {
  await evaluate("document.querySelector('.foot-btn')?.click()")
  await sleep(600)
  // settings:语音 —— 冒号后面是导航项文字（前缀匹配），用来逐页验收
  const tab = mode.split(':')[1]
  if (tab) {
    await evaluate(
      `[...document.querySelectorAll('.nav-item')]` +
        `.find((b) => b.textContent.trim().startsWith(${JSON.stringify(tab)}))?.click()`,
    )
    await sleep(500)
  }
}

// rail:浏览器 / rail:工作空间 / rail:变更 —— 按按钮 title 匹配右侧工具条
if (mode.startsWith('rail:')) {
  const label = mode.split(':')[1]
  await evaluate(
    `[...document.querySelectorAll('.rail-btn')]` +
      `.find((b) => b.title.includes(${JSON.stringify(label)}))?.click()`,
  )
  await sleep(900)
}

fs.mkdirSync(path.dirname(outFile), { recursive: true })
const shot = await send('Page.captureScreenshot', { format: 'png' })
fs.writeFileSync(outFile, Buffer.from(shot.result.data, 'base64'))
console.log(`saved ${outFile}`)
ws.close()
process.exit(0)
