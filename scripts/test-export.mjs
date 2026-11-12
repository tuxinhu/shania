// 导出格式验证：把 src/lib/export.ts 打成 CJS 后直接跑，绕过原生保存对话框
import { mkdirSync, writeFileSync, readFileSync, readdirSync } from 'node:fs'
import { resolve } from 'node:path'

const out = resolve('.preview/export-test.cjs')
mkdirSync('.preview', { recursive: true })

// esbuild 是 vite 的间接依赖，用 pnpm 严格模式时不在顶层，这里按目录名定位
const pnpmDir = resolve('node_modules/.pnpm')
const esbuildPkg = readdirSync(pnpmDir).find((d) => d.startsWith('esbuild@'))
if (!esbuildPkg) throw new Error('未找到 esbuild 依赖')
const esbuild = await import(
  'file://' + resolve(pnpmDir, esbuildPkg, 'node_modules/esbuild/lib/main.js')
)

await esbuild.build({
  entryPoints: ['src/lib/export.ts'],
  bundle: true,
  format: 'cjs',
  platform: 'node',
  outfile: out,
  logLevel: 'silent',
  external: ['@tauri-apps/plugin-dialog'],
})

const { sessionToMarkdown, sessionToJson } = await import('file://' + out)

const session = {
  id: 's1',
  title: '导出/测试: 标题带非法字符',
  createdAt: Date.parse('2026-11-09T10:00:00+08:00'),
  updatedAt: Date.parse('2026-11-09T10:05:00+08:00'),
  pinned: false,
  messages: [
    { id: 'm1', role: 'user', content: '帮我看下当前目录', createdAt: 1 },
    {
      id: 'm2',
      role: 'assistant',
      content: '目录里有 3 个文件。\n\n```json\n{"a":1}\n```',
      reasoning: '先列目录再回答',
      createdAt: 2,
      model: 'qwen3-vl-8b',
      usage: { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 },
      toolCalls: [
        {
          id: 't1',
          name: 'list_directory',
          arguments: '{"path":"."}',
          result: 'a.txt\nb.txt\nc.txt',
          status: 'done',
          elapsedMs: 12,
        },
      ],
    },
    { id: 'm3', role: 'assistant', content: '', createdAt: 3, error: '模型返回 500' },
  ],
}

const md = sessionToMarkdown(session, 'Shania')
const json = sessionToJson(session)

writeFileSync('.preview/export-sample.md', md, 'utf8')
writeFileSync('.preview/export-sample.json', json, 'utf8')

const checks = [
  ['标题行', md.startsWith('# 导出/测试: 标题带非法字符')],
  ['元信息引用行', md.includes('> 创建于 2026-11-09 10:00')],
  ['用户段', md.includes('## 我\n\n帮我看下当前目录')],
  ['助手段带模型与 token', md.includes('*qwen3-vl-8b · 30 tokens*')],
  ['思考过程折叠', md.includes('<summary>思考过程（7 字）</summary>')],
  ['工具调用折叠含耗时', md.includes('<summary>**list_directory**　完成 · 12 ms</summary>')],
  ['工具参数为 json 代码块', md.includes('```json\n{"path":"."}\n```')],
  ['错误标注', md.includes('> 错误：模型返回 500')],
  ['正文代码块未被破坏', md.includes('```json\n{"a":1}\n```')],
  ['消息间分隔符', md.includes('\n---\n')],
  ['JSON 顶层字段', (() => {
    const j = JSON.parse(json)
    return j.app === 'Shania Agent' && !!j.exportedAt && j.session.id === 's1'
  })()],
]

let bad = 0
for (const [name, ok] of checks) {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}`)
  if (!ok) bad++
}
console.log(`\n${checks.length - bad}/${checks.length} 通过，Markdown ${md.length} 字符，JSON ${json.length} 字符`)

// 展示前 24 行便于肉眼检查结构
console.log('\n--- Markdown 前 24 行 ---')
console.log(readFileSync('.preview/export-sample.md', 'utf8').split('\n').slice(0, 24).join('\n'))
process.exit(bad ? 1 : 0)
