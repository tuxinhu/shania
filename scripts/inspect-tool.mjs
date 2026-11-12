/** 检查工具卡片的三态：收起 / 展开（结果折叠）/ 结果展开 */
import fs from 'node:fs'

const PORT = Number(process.env.CDP_PORT || 9223)
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

const list = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json()
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
const evaluate = async (expression) => {
  const res = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })
  if (res.result?.exceptionDetails) throw new Error(res.result.exceptionDetails.text)
  return res.result?.result?.value
}
const shot = async (file) => {
  const s = await send('Page.captureScreenshot', { format: 'png' })
  fs.writeFileSync(file, Buffer.from(s.result.data, 'base64'))
  console.log('saved', file)
}

await evaluate(`
  (() => {
    const card = document.querySelector('.tool-card');
    if (!card) return 'no-card';
    card.scrollIntoView({ block: 'center' });
    return 'ok';
  })()
`)
await sleep(500)
await shot('.preview/tool-collapsed.png')

const opened = await evaluate(`
  (() => {
    const head = document.querySelector('.tool-card .tool-head');
    if (!head) return 'no-head';
    head.click();
    return 'clicked';
  })()
`)
console.log('expand card:', opened)
await sleep(450)
await shot('.preview/tool-expanded.png')

const resultState = await evaluate(`
  (() => {
    const bar = document.querySelector('.tool-card .result-toggle');
    if (!bar) return 'no-result-bar';
    bar.click();
    return document.querySelector('.tool-card .result-meta')?.textContent || 'clicked';
  })()
`)
console.log('result meta:', resultState)
await sleep(450)
await shot('.preview/tool-result.png')

const info = await evaluate(`
  (() => {
    const body = document.querySelector('.tool-card .result-body');
    const more = document.querySelector('.tool-card .result-more');
    return {
      hasBody: !!body,
      scrollH: body?.scrollHeight ?? 0,
      clientH: body?.clientHeight ?? 0,
      hasMoreBtn: !!more,
      moreText: more?.textContent?.trim() ?? ''
    };
  })()
`)
console.log('body:', JSON.stringify(info))

ws.close()
process.exit(0)
