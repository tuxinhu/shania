/**
 * 端到端验证：会话归档、内容搜索、消息编辑重发、单条删除、设置面板。
 * 前置：应用已启动并开放 9223 调试端口，mock 服务在 4000。
 */
import fs from 'node:fs'
import path from 'node:path'

const PORT = Number(process.env.CDP_PORT || 9223)
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

let pass = 0
let fail = 0
function check(name, cond, extra = '') {
  if (cond) {
    pass += 1
    console.log(`  PASS  ${name}`)
  } else {
    fail += 1
    console.log(`  FAIL  ${name}${extra ? `  → ${extra}` : ''}`)
  }
}

async function findTarget() {
  for (let i = 0; i < 100; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json()
      const page = list.find((t) => t.type === 'page' && !t.url.startsWith('devtools'))
      if (page) return page
    } catch {
      /* 等就绪 */
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
async function shot(file, clip) {
  fs.mkdirSync(path.dirname(file), { recursive: true })
  const r = await send('Page.captureScreenshot', clip ? { format: 'png', clip } : { format: 'png' })
  fs.writeFileSync(file, Buffer.from(r.result.data, 'base64'))
  console.log('  截图 →', file)
}

await send('Runtime.enable')
await send('Page.enable')
await send('Emulation.setDeviceMetricsOverride', {
  width: 1200,
  height: 800,
  deviceScaleFactor: 1,
  mobile: false,
})

// 清空真实存储（Tauri 下会话在文件里，不在 localStorage）
await evaluate(`
  localStorage.setItem('shania.settings.v1', JSON.stringify({
    baseUrl: 'http://127.0.0.1:4000/v1', apiKey: 'mock-key', model: 'mock-vl',
    temperature: 0.7, maxTokens: 2048, systemPrompt: '测试用。',
    toolsEnabled: true, theme: 'light', displayName: 'Shania',
    shortcut: 'CmdOrCtrl+Shift+Space', closeAction: 'tray', autoStart: false,
    contextLimit: 32000, profiles: [
      { id: 'p1', name: '本地 Mock', baseUrl: 'http://127.0.0.1:4000/v1', apiKey: 'mock-key', model: 'mock-vl', temperature: 0.7, maxTokens: 2048 },
      { id: 'p2', name: '远程 Qwen', baseUrl: 'https://api.example.com/v1', apiKey: 'sk-test', model: 'qwen3-vl-8b', temperature: 0.5, maxTokens: 4096 }
    ],
    activeProfileId: 'p1'
  }));
  true
`)
await evaluate(`window.__TAURI_INTERNALS__.invoke('save_sessions', { json: '[]' })`)
await send('Page.reload', { ignoreCache: false })
await sleep(2200)

async function sendMessage(text) {
  await evaluate(`
    (() => {
      const ta = document.querySelector('textarea');
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
      setter.call(ta, ${JSON.stringify(text)});
      ta.dispatchEvent(new Event('input', { bubbles: true }));
      return ta.value;
    })()
  `)
  await sleep(250)
  await evaluate(`document.querySelector('.send:not(.stop)').click(); true`)
  // 等到流式结束且出现正文
  for (let i = 0; i < 80; i++) {
    await sleep(600)
    const done = await evaluate(`!document.querySelector('.send.stop') && !!document.querySelector('.markdown')`)
    if (done) break
  }
}

console.log('\n[1] 造两个会话')
await sendMessage('第一个会话：看看项目结构')
let count = await evaluate(`document.querySelectorAll('.session-item').length`)
check('已有 1 个会话', count === 1, String(count))

await evaluate(`document.querySelector('.new-btn').click(); true`)
await sleep(600)
await sendMessage('第二个会话：再列一次目录')
count = await evaluate(`document.querySelectorAll('.session-item').length`)
check('已有 2 个会话', count === 2, String(count))

console.log('\n[2] 右键归档')
await evaluate(`
  (() => {
    const items = document.querySelectorAll('.session-item');
    const el = items[0];
    el.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 120, clientY: 220 }));
    return true;
  })()
`)
await sleep(400)
const ctxOpen = await evaluate(`!!document.querySelector('.ctx-menu')`)
check('右键菜单弹出', ctxOpen)
const ctxItems = await evaluate(
  `[...document.querySelectorAll('.ctx-menu button')].map(b => b.textContent.trim())`,
)
console.log('  菜单项:', JSON.stringify(ctxItems))
check('含归档项', ctxItems.includes('归档'), JSON.stringify(ctxItems))

await evaluate(`
  (() => {
    const btn = [...document.querySelectorAll('.ctx-menu button')].find(b => b.textContent.includes('归档'));
    if (!btn) return 'no-btn';
    btn.click();
    return 'ok';
  })()
`)
await sleep(900)
const archiveState = await evaluate(`
  (() => ({
    toggle: !!document.querySelector('.archive-toggle'),
    count: document.querySelectorAll('.archive-toggle')[0]?.textContent?.trim() || '',
    visibleItems: document.querySelectorAll('.session-item:not(.archived)').length
  }))()
`)
check('出现已归档分组', archiveState.toggle, JSON.stringify(archiveState))
check('归档项移出主列表', archiveState.visibleItems === 1, `visible=${archiveState.visibleItems}`)

console.log('\n[3] 展开归档')
await evaluate(`document.querySelector('.archive-toggle').click(); true`)
await sleep(400)
const expanded = await evaluate(`document.querySelectorAll('.session-item.archived').length`)
check('展开后可见归档会话', expanded >= 1, String(expanded))
await evaluate(`document.querySelector('.archive-toggle').click(); true`)
await sleep(300)

console.log('\n[4] 内容搜索')
await evaluate(`
  (() => {
    const input = document.querySelector('.search input');
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    setter.call(input, '目录扫描');
    input.dispatchEvent(new Event('input', { bubbles: true }));
    return true;
  })()
`)
await sleep(700)
const searchState = await evaluate(`
  (() => {
    const hit = document.querySelector('.session-hit');
    return {
      hitShown: !!hit,
      marks: document.querySelectorAll('.session-hit mark').length,
      text: hit?.textContent?.trim().slice(0, 60) || ''
    };
  })()
`)
check('显示命中片段', searchState.hitShown, JSON.stringify(searchState))
check('片段内有关键词高亮', searchState.marks >= 1, `marks=${searchState.marks}`)
await shot('.preview/ui-search.png')

console.log('\n[5] 清空搜索 + 编辑用户消息')
await evaluate(`
  (() => {
    const input = document.querySelector('.search input');
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    setter.call(input, '');
    input.dispatchEvent(new Event('input', { bubbles: true }));
    return true;
  })()
`)
await sleep(500)
await evaluate(`
  (() => {
    const btn = document.querySelector('.user-acts button');
    if (!btn) return 'no-btn';
    btn.click();
    return 'ok';
  })()
`)
await sleep(500)
const editOpen = await evaluate(`!!document.querySelector('.edit-box')`)
check('进入编辑态', editOpen)

await evaluate(`
  (() => {
    const ta = document.querySelector('.edit-box textarea');
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    setter.call(ta, '改写后的问题：请再检查一次目录');
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    return ta.value;
  })()
`)
await sleep(300)
await evaluate(`
  (() => {
    const btn = [...document.querySelectorAll('.edit-bar .mini')].find(b => b.textContent.includes('保存'));
    btn.click();
    return 'ok';
  })()
`)
await sleep(1500)
const editResult = await evaluate(`
  (() => {
    const bubbles = [...document.querySelectorAll('.user-bubble')];
    return {
      last: bubbles.at(-1)?.textContent?.trim() || '',
      total: bubbles.length
    };
  })()
`)
check('用户消息已改写', editResult.last.includes('改写后的问题'), editResult.last)
check('该消息之后的回复被丢弃', editResult.total === 1, `user msgs=${editResult.total}`)

// 等重新生成完成
for (let i = 0; i < 60; i++) {
  await sleep(600)
  const done = await evaluate(`!document.querySelector('.send.stop')`)
  if (done) break
}

console.log('\n[6] 删除单条消息')
const before = await evaluate(`document.querySelectorAll('.msg').length`)
await evaluate(`
  (() => {
    const btns = [...document.querySelectorAll('.user-acts button')];
    const del = btns.at(-1);
    if (!del) return 'no-btn';
    del.click();
    return 'ok';
  })()
`)
await sleep(600)
const after = await evaluate(`document.querySelectorAll('.msg').length`)
check('消息条数减少', after === before - 1, `${before} → ${after}`)

console.log('\n[7] 设置面板')
await evaluate(`
  window.dispatchEvent(new KeyboardEvent('keydown', { key: ',', ctrlKey: true, bubbles: true }));
  true
`)
await sleep(700)
const sheet = await evaluate(`
  (() => {
    const s = document.querySelector('.sheet');
    if (!s) return { open: false };
    const rows = [...s.querySelectorAll('.row-label')].map(e => e.textContent.trim());
    const selects = s.querySelectorAll('select').length;
    return { open: true, rows, selects };
  })()
`)
check('设置面板打开', sheet.open)
console.log('  设置项:', JSON.stringify(sheet.rows))
check('含快速配置', sheet.rows.includes('快速配置'))
check('含连通性', sheet.rows.includes('连通性'))
check('含历史预算', sheet.rows.includes('历史预算'))
check('含关闭按钮', sheet.rows.includes('关闭按钮'))
check('含开机自启', sheet.rows.includes('开机自启'))
check('含清空全部会话', sheet.rows.includes('清空全部会话'))
check('配置下拉存在', sheet.selects >= 1, `selects=${sheet.selects}`)

// 测试连接
await evaluate(`
  (() => {
    const btn = [...document.querySelectorAll('.mini-btn')].find(b => b.textContent.includes('测试连接'));
    if (!btn) return 'no-btn';
    btn.click();
    return 'ok';
  })()
`)
let testResult = ''
for (let i = 0; i < 40; i++) {
  await sleep(400)
  testResult = await evaluate(`
    (() => {
      const row = [...document.querySelectorAll('.row')].find(r => r.textContent.includes('连通性'));
      return row?.querySelector('.row-sub')?.textContent?.trim() || '';
    })()
  `)
  if (testResult.includes('连接正常') || testResult.includes('失败') || testResult.includes('返回')) break
}
check('连通性测试返回结果', testResult.includes('连接正常'), testResult)

await shot('.preview/ui-settings.png')

await evaluate(`window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); true`)
await sleep(500)

console.log(`\n结果：${pass} 通过 / ${fail} 失败\n`)
ws.close()
process.exit(fail ? 1 : 0)
