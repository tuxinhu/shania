/**
 * 端到端验证：图片附件链路
 * 在页面里合成一张图 → 粘贴 → 检查预览 → 发送 → 校验模型侧收到 image_url。
 */
import fs from 'node:fs'
import path from 'node:path'

const PORT = Number(process.env.CDP_PORT || 9223)
const outFile = process.argv[2] || '.preview/e2e-image.png'
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function findTarget() {
  for (let i = 0; i < 100; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json()
      const page = list.find((t) => t.type === 'page' && !t.url.startsWith('devtools'))
      if (page) return page
    } catch {
      /* 等 WebView2 */
    }
    await sleep(500)
  }
  throw new Error('未找到调试目标')
}

const target = await findTarget()
const ws = new WebSocket(target.webSocketDebuggerUrl)
await new Promise((res, rej) => {
  ws.onopen = res
  ws.onerror = rej
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
  return new Promise((r) => pending.set(id, r))
}
async function evaluate(expression) {
  const res = await send('Runtime.evaluate', {
    expression,
    returnByValue: true,
    awaitPromise: true,
  })
  if (res.result?.exceptionDetails) {
    throw new Error(res.result.exceptionDetails.text + ' :: ' + expression.slice(0, 100))
  }
  return res.result?.result?.value
}

await send('Runtime.enable')
await send('Page.enable')

// 指向 mock，清空会话，使用浅色主题便于截图
await evaluate(`
  localStorage.setItem('shania.settings.v1', JSON.stringify({
    baseUrl: 'http://127.0.0.1:4000/v1', apiKey: 'mock-key', model: 'mock-vl',
    temperature: 0.7, maxTokens: 2048, systemPrompt: '测试用。',
    toolsEnabled: true, theme: 'light', displayName: 'Shania',
    shortcut: 'CmdOrCtrl+Shift+Space', closeAction: 'tray', autoStart: false,
    contextLimit: 32000, profiles: [], activeProfileId: ''
  }));
  localStorage.removeItem('shania.sessions');
  true
`)
await send('Page.reload', { ignoreCache: false })
await sleep(2200)

// 合成一张 320x200 的图并粘贴进输入框
const pasted = await evaluate(`
  (async () => {
    const canvas = document.createElement('canvas');
    canvas.width = 320; canvas.height = 200;
    const ctx = canvas.getContext('2d');
    const g = ctx.createLinearGradient(0, 0, 320, 200);
    g.addColorStop(0, '#4ba7ff'); g.addColorStop(1, '#7b5cff');
    ctx.fillStyle = g; ctx.fillRect(0, 0, 320, 200);
    ctx.fillStyle = '#fff'; ctx.font = 'bold 26px sans-serif';
    ctx.fillText('TEST IMAGE', 62, 108);

    const blob = await new Promise((r) => canvas.toBlob(r, 'image/png'));
    const file = new File([blob], 'gradient.png', { type: 'image/png' });
    const dt = new DataTransfer();
    dt.items.add(file);

    const ta = document.querySelector('textarea');
    if (!ta) return 'no-textarea';
    ta.focus();
    ta.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
    return 'dispatched:' + file.size;
  })()
`)
console.log('paste:', pasted)

// 等压图与预览渲染
let attState = null
for (let i = 0; i < 40; i++) {
  await sleep(250)
  attState = await evaluate(`
    (() => {
      const img = document.querySelector('.attach-area .att img');
      return {
        preview: !!img,
        srcPrefix: img ? img.getAttribute('src').slice(0, 22) : null,
        error: document.querySelector('.attach-error')?.textContent?.trim() || null
      };
    })()
  `)
  if (attState.preview) break
}
console.log('attachment:', JSON.stringify(attState))

// 补一句文字再发送
await evaluate(`
  (() => {
    const ta = document.querySelector('textarea');
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    setter.call(ta, '看看这张图');
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    return ta.value;
  })()
`)
await sleep(300)

const clicked = await evaluate(`
  (() => {
    const btn = document.querySelector('.send:not(.stop)');
    if (!btn) return 'no-btn';
    if (btn.disabled) return 'disabled';
    btn.click();
    return 'clicked';
  })()
`)
console.log('send:', clicked)

let state = null
for (let i = 0; i < 60; i++) {
  await sleep(600)
  state = await evaluate(`
    (() => {
      const text = document.body.innerText;
      return {
        hasReply: text.includes('收到 1 张图片'),
        userImage: !!document.querySelector('.user-atts img'),
        streaming: !!document.querySelector('.send.stop'),
        hasErr: text.includes('模型服务返回') || text.includes('无法连接')
      };
    })()
  `)
  if (state.hasReply && !state.streaming) break
}
console.log('result:', JSON.stringify(state))

fs.mkdirSync(path.dirname(outFile), { recursive: true })
const shot = await send('Page.captureScreenshot', { format: 'png' })
fs.writeFileSync(outFile, Buffer.from(shot.result.data, 'base64'))
console.log(`saved ${outFile}`)

ws.close()
process.exit(state?.hasReply && state?.userImage ? 0 : 1)
