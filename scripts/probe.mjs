/**
 * 通用探针：在运行中的应用里执行一段 JS 并打印结果。
 * 用法：node scripts/probe.mjs "document.title"
 */
const expr = process.argv[2]
if (!expr) {
  console.error('用法：node scripts/probe.mjs "<javascript表达式>"')
  process.exit(1)
}

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

const res = await send('Runtime.evaluate', {
  expression: expr,
  returnByValue: true,
  awaitPromise: true,
})
if (res.result?.exceptionDetails) {
  console.error('异常：', res.result.exceptionDetails.text)
  process.exit(2)
}
console.log(JSON.stringify(res.result?.result?.value, null, 2))

ws.close()
process.exit(0)
