/**
 * 端到端联调：连上 Tauri WebView2 的调试端口，真实走一遍
 * 输入 → Rust SSE 代理 → Mock 模型 → 工具调用 → 工具循环 → 渲染。
 *
 * 前置：
 *   WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9223" npx tauri dev
 *   node scripts/mock-llm.mjs
 */
import fs from 'node:fs'
import path from 'node:path'

const PORT = Number(process.env.CDP_PORT || 9223)
const outFile = process.argv[2] || '.preview/e2e.png'
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function findTarget() {
  for (let i = 0; i < 100; i++) {
    try {
      const res = await fetch(`http://127.0.0.1:${PORT}/json`)
      const list = await res.json()
      const page = list.find((t) => t.type === 'page' && !t.url.startsWith('devtools'))
      if (page) return page
    } catch {
      /* 等待 WebView2 就绪 */
    }
    await sleep(500)
  }
  throw new Error(`未找到调试目标（端口 ${PORT}）`)
}

const target = await findTarget()
console.log('target:', target.url)
const ws = new WebSocket(target.webSocketDebuggerUrl)
await new Promise((resolve, reject) => {
  ws.onopen = resolve
  ws.onerror = reject
})

let seq = 0
const pending = new Map()
const logs = []
ws.onmessage = (ev) => {
  const msg = JSON.parse(ev.data)
  if (msg.id && pending.has(msg.id)) {
    pending.get(msg.id)(msg)
    pending.delete(msg.id)
  } else if (msg.method === 'Runtime.consoleAPICalled') {
    logs.push(msg.params.args.map((a) => a.value ?? a.description ?? '').join(' '))
  } else if (msg.method === 'Runtime.exceptionThrown') {
    logs.push('EXCEPTION: ' + (msg.params.exceptionDetails?.text || ''))
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
    throw new Error(res.result.exceptionDetails.text + ' :: ' + expression.slice(0, 80))
  }
  return res.result?.result?.value
}

await send('Runtime.enable')
await send('Page.enable')

// 指向 Mock 模型，清空历史，保证一次干净运行
await evaluate(`
  localStorage.setItem('shania.settings.v1', JSON.stringify({
    baseUrl: 'http://127.0.0.1:4000/v1',
    apiKey: 'mock-key',
    model: 'mock-agent',
    temperature: 0.7,
    maxTokens: 2048,
    systemPrompt: '你是 Shania，测试用。',
    toolsEnabled: true,
    theme: 'light',
    displayName: 'Shania'
  }));
  localStorage.removeItem('shania.sessions');
  true
`)
await send('Page.reload', { ignoreCache: false })
await sleep(2000)

// 输入并发送
const typed = await evaluate(`
  (() => {
    const ta = document.querySelector('textarea');
    if (!ta) return 'no-textarea';
    ta.focus();
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    setter.call(ta, '看一下这个项目的目录结构');
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    return ta.value;
  })()
`)
console.log('typed:', typed)
await sleep(400)

const clicked = await evaluate(`
  (() => {
    const btn = document.querySelector('.send:not(.stop)');
    if (!btn) return 'no-send-btn';
    if (btn.disabled) return 'send-disabled';
    btn.click();
    return 'clicked';
  })()
`)
console.log('send:', clicked)

// 轮询等待流式结束：出现 Markdown 正文且不再处于 streaming 状态
let state = null
for (let i = 0; i < 90; i++) {
  await sleep(700)
  state = await evaluate(`
    (() => {
      const text = document.body.innerText;
      return {
        hasToolCard: text.includes('list_directory') || !!document.querySelector('.tool-card'),
        hasFinal: text.includes('目录扫描完成'),
        streaming: !!document.querySelector('.send.stop'),
        chars: text.length,
        snippet: text.slice(0, 240)
      };
    })()
  `)
  if (state.hasFinal && !state.streaming) break
}

console.log('state:', JSON.stringify(state))
if (logs.length) console.log('console:', logs.slice(-12).join('\n'))

fs.mkdirSync(path.dirname(outFile), { recursive: true })
const shot = await send('Page.captureScreenshot', { format: 'png' })
fs.writeFileSync(outFile, Buffer.from(shot.result.data, 'base64'))
console.log(`saved ${outFile}`)

ws.close()
process.exit(0)
