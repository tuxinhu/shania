/**
 * 开发辅助：量界面的真实几何尺寸，别靠肉眼看截图猜。
 *
 * 用法：
 *   1. 起 dev server（pnpm dev）
 *   2. 起 Chrome：chrome --headless=new --remote-debugging-port=9222 http://localhost:1420/
 *   3. SHOT_W=1706 SHOT_H=1066 node scripts/dev-measure.mjs
 *
 * 输出三段：布局矩形、CSS 变量、以及「消息列是否真的视觉居中」的左右留白。
 * 额外表达式可以走 argv[2]，会一起求值打印，方便量别的组件。
 */
import path from 'node:path'

const PORT = Number(process.env.CDP_PORT || 9222)
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const extra = process.argv[2] || ''

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
  throw new Error('未找到调试目标')
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
    throw new Error(res.result.exceptionDetails.text + ' ' + (res.result.exceptionDetails.exception?.description ?? ''))
  }
  return res.result?.result?.value
}

await send('Page.enable')
await send('Emulation.setDeviceMetricsOverride', {
  width: Number(process.env.SHOT_W || 1200),
  height: Number(process.env.SHOT_H || 800),
  deviceScaleFactor: 1,
  mobile: false,
})
await send('Page.reload', { ignoreCache: false })
// 视口尺寸变更后要等布局和 ResizeObserver 回调都跑完
await sleep(1800)

const report = await evaluate(`
(() => {
  const r = (sel) => {
    const el = document.querySelector(sel)
    if (!el) return null
    const b = el.getBoundingClientRect()
    return { x: Math.round(b.x), w: Math.round(b.width), right: Math.round(b.right) }
  }
  const cs = getComputedStyle(document.documentElement)
  const v = (n) => cs.getPropertyValue(n).trim()

  const winW = window.innerWidth
  const side = r('.sidebar')
  const rail = r('.rail')
  const panel = r('.panel')
  const scroll = r('.scroll')
  const inner = r('.inner')

  // 视觉留白：消息列左缘到窗口左边、右缘到窗口右边
  const gutter = inner
    ? { left: inner.x, right: winW - inner.right }
    : null

  return {
    viewport: winW,
    sidebar: side,
    panel,
    rail,
    scroll,
    inner,
    gutter,
    gutterDelta: gutter ? gutter.left - gutter.right : null,
    innerLeftProp: inner ? getComputedStyle(document.querySelector('.inner')).left : null,
    vars: {
      sidebarW: v('--sidebar-w'),
      railW: v('--rail-w'),
      railPanelW: v('--rail-panel-w'),
      dialogW: v('--dialog-w'),
      dialogCol: v('--dialog-col'),
    },
  }
})()
`)

console.log(JSON.stringify(report, null, 2))

if (extra) {
  console.log('\n--- extra ---')
  const out = await evaluate(extra)
  console.log(typeof out === 'string' ? out : JSON.stringify(out, null, 2))
}

ws.close()
process.exit(0)
