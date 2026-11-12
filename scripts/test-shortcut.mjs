/**
 * 验证快捷键录制：打开设置 → 点录制 → 合成按键 → 检查是否落库并真正注册。
 * 需要应用以 --remote-debugging-port=9223 启动。
 */
import { writeFileSync } from 'node:fs'

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
const ev = async (expression) => {
  const r = await send('Runtime.evaluate', {
    expression,
    returnByValue: true,
    awaitPromise: true,
  })
  if (r.result?.exceptionDetails) {
    throw new Error(r.result.exceptionDetails.text + ' ' + (r.result.exceptionDetails.exception?.description || ''))
  }
  return r.result?.result?.value
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const shot = async (file) => {
  const r = await send('Page.captureScreenshot', { format: 'png' })
  writeFileSync(file, Buffer.from(r.result.data, 'base64'))
}

// 1. 打开设置面板
await ev(`window.dispatchEvent(new KeyboardEvent('keydown',{key:',',ctrlKey:true,bubbles:true})); true`)
await sleep(600)
console.log('设置面板:', await ev(`!!document.querySelector('.sheet')`))
console.log('初始显示:', await ev(`document.querySelector('.hotkey')?.textContent?.trim()`))
await shot('.preview/shortcut-1-idle.png')

// 2. 进入录制态
await ev(`document.querySelector('.hotkey').click(); true`)
await sleep(400)
console.log('录制态显示:', await ev(`document.querySelector('.hotkey')?.textContent?.trim()`))
console.log('录制态 class:', await ev(`document.querySelector('.hotkey')?.className`))
await shot('.preview/shortcut-2-recording.png')

// 3. 合成 Ctrl+Alt+K
await ev(`(() => {
  const b = document.querySelector('.hotkey');
  b.dispatchEvent(new KeyboardEvent('keydown',{code:'KeyK',key:'k',ctrlKey:true,altKey:true,bubbles:true,cancelable:true}));
  return true;
})()`)
await sleep(700)
console.log('改键后显示:', await ev(`document.querySelector('.hotkey')?.textContent?.trim()`))
console.log('保存值:', await ev(`JSON.parse(localStorage.getItem('shania.settings.v1')).shortcut`))
await shot('.preview/shortcut-3-applied.png')

// 4. 后端是否真的接受了这个键（再设一次，命中「已经是当前键」分支应直接成功）
console.log(
  '后端确认:',
  await ev(`(async()=>{
    try { return 'OK ' + await window.__TAURI_INTERNALS__.invoke('set_global_shortcut',{accelerator:'Ctrl+Alt+K'}) }
    catch(e){ return 'ERR ' + e }
  })()`),
)

// 5. 非法组合键应报错且不改动设置
console.log(
  '非法键:',
  await ev(`(async()=>{
    try { return 'OK ' + await window.__TAURI_INTERNALS__.invoke('set_global_shortcut',{accelerator:'Foo+Bar'}) }
    catch(e){ return 'ERR ' + String(e).slice(0,60) }
  })()`),
)
console.log('设置未被污染:', await ev(`JSON.parse(localStorage.getItem('shania.settings.v1')).shortcut`))

ws.close()
process.exit(0)
