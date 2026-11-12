/** 上下文裁剪与多模态转换的回归测试（离线，不依赖应用） */
import { execSync } from 'node:child_process'
import { existsSync, readdirSync } from 'node:fs'
import { createRequire } from 'node:module'
import { resolve } from 'node:path'

/** pnpm 严格隔离下 esbuild 不在 .bin 里，直接定位到包内可执行文件 */
function findEsbuild() {
  const base = 'node_modules/.pnpm'
  for (const dir of readdirSync(base)) {
    if (!dir.startsWith('esbuild@')) continue
    const candidate = resolve(base, dir, 'node_modules/esbuild/bin/esbuild')
    if (existsSync(candidate)) return candidate
  }
  throw new Error('找不到 esbuild')
}

const out = resolve('.preview/context-test.cjs')
execSync(
  `node "${findEsbuild()}" src/lib/context.ts --bundle --format=cjs --platform=node --log-level=error --outfile="${out}"`,
  { stdio: 'inherit' },
)

const require = createRequire(import.meta.url)
const ctx = require(out)
const { buildWireDetailed, estimateTextTokens, toWire } = ctx

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

function msg(id, role, content, attachments) {
  return {
    id,
    role,
    content,
    createdAt: Date.now(),
    status: 'done',
    ...(attachments ? { attachments } : {}),
  }
}

function image(id) {
  return {
    id,
    name: 'x.png',
    mime: 'image/png',
    dataUrl: 'data:image/png;base64,AAAA',
    size: 100,
  }
}

console.log('\n[1] token 估算')
check('纯中文按字数计', estimateTextTokens('你好世界') === 5, String(estimateTextTokens('你好世界')))
check('英文按 3.5 字符/token', estimateTextTokens('abcdefghij') === 3, String(estimateTextTokens('abcdefghij')))
check('空串为 0', estimateTextTokens('') === 0)

console.log('\n[2] 短历史完整保留')
{
  const list = [msg('1', 'user', '你好'), msg('2', 'assistant', '你好，有什么可以帮你？')]
  const r = buildWireDetailed(list, 32000)
  check('保留 2 条', r.messages.length === 2)
  check('无裁剪提示', r.omitted === 0)
  check('顺序不变', r.messages[0].content === '你好' && r.messages[1].role === 'assistant')
}

console.log('\n[3] 超预算时裁剪较早消息')
{
  const list = []
  for (let i = 0; i < 40; i += 1) {
    list.push(msg(`u${i}`, 'user', '这是一段用于把上下文撑大的中文内容，重复很多次。'.repeat(6)))
    list.push(msg(`a${i}`, 'assistant', '收到，我来回答这个问题。'.repeat(6)))
  }
  const r = buildWireDetailed(list, 1200)
  check('消息数量被压缩', r.messages.length < 80, `实际 ${r.messages.length}`)
  check('有裁剪提示', r.omitted > 0, `omitted=${r.omitted}`)
  check(
    '首条是裁剪提示',
    String(r.messages[0].content).includes('已省略更早的'),
    String(r.messages[0].content).slice(0, 40),
  )
  check('末尾保留最新一轮', String(r.messages.at(-1).content).includes('收到，我来回答'))
  check('估算 token 未超太多', r.tokens <= 1200 + 120, `tokens=${r.tokens}`)
}

console.log('\n[4] 单条超长消息仍然保留')
{
  const huge = msg('1', 'user', '啊'.repeat(9000))
  const r = buildWireDetailed([huge], 1000)
  check('不会发出空请求', r.messages.length === 1)
  check('内容未被丢弃', String(r.messages[0].content).length === 9000)
}

console.log('\n[5] 空会话')
{
  const r = buildWireDetailed([], 32000)
  check('返回空数组', r.messages.length === 0 && r.omitted === 0)
}

console.log('\n[6] 图片转多模态')
{
  const m = msg('1', 'user', '看图', [image('i1'), image('i2')])
  const wire = toWire(m)
  check('content 为数组', Array.isArray(wire.content))
  check('含文本片段', wire.content[0].type === 'text' && wire.content[0].text === '看图')
  check('含 2 个图片片段', wire.content.filter((p) => p.type === 'image_url').length === 2)
  check('图片是 data URL', wire.content[1].image_url.url.startsWith('data:image/'))
}

console.log('\n[7] 只有图片没有文字')
{
  const m = msg('1', 'user', '', [image('i1')])
  const wire = toWire(m)
  check('补了占位文本', wire.content[0].type === 'text' && wire.content[0].text.length > 0)
}

console.log('\n[8] 图片计费远高于文本')
{
  const textOnly = buildWireDetailed([msg('1', 'user', '看图')], 32000)
  const withImg = buildWireDetailed([msg('1', 'user', '看图', [image('i1')])], 32000)
  check('图片使 token 明显上升', withImg.tokens - textOnly.tokens >= 1000, `${textOnly.tokens} → ${withImg.tokens}`)
}

console.log('\n[9] 纯图片会话在小预算下仍保留最后一条')
{
  const list = []
  for (let i = 0; i < 5; i += 1) list.push(msg(`m${i}`, i % 2 ? 'assistant' : 'user', '', [image(`i${i}`)]))
  const r = buildWireDetailed(list, 2000)
  check('至少保留一条', r.messages.length >= 1)
  check('保留的是最后一条', r.messages.at(-1).content[0].type === 'text')
}

console.log(`\n结果：${pass} 通过 / ${fail} 失败\n`)
process.exit(fail ? 1 : 0)
