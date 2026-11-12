// 本地 Mock LLM（OpenAI 兼容），用于端到端联调：
// 第一轮返回 tool_call，第二轮返回 Markdown 正文，覆盖「流式 + 工具循环」全链路。
// 同时支持非流式（连通性测试）与图片消息，便于验证多模态链路。
import http from 'node:http'

const PORT = Number(process.env.PORT || 4000)

function sse(res, obj) {
  res.write(`data: ${JSON.stringify(obj)}\n\n`)
}

const FINAL_TEXT = `## 目录扫描完成

我看了 \`I:/project/agent-ui\` 的结构，这是一个 **Tauri + Vite + Vue 3** 项目，主要分三层：

| 层 | 位置 | 职责 |
| --- | --- | --- |
| 视图层 | \`src/components\` | 侧边栏、消息流、输入框、设置面板 |
| 状态层 | \`src/stores\` | Pinia：会话与设置 |
| 内核层 | \`src-tauri/src\` | SSE 代理、工具循环、本地持久化 |

### 关键实现

流式代理在 Rust 侧按行切 SSE，逐条通过 channel 推给前端：

\`\`\`rust
for line in buf.split_inclusive('\\n') {
    if let Some(payload) = line.strip_prefix("data: ") {
        if payload.trim() == "[DONE]" { break; }
        channel.send(StreamEvent::Delta { text: ... })?;
    }
}
\`\`\`

要不要我继续把 **工具结果折叠面板** 和 **会话导出** 也补上？`

function imageReply(count, text) {
  return `## 收到 ${count} 张图片

${text ? `你的描述是：**${text}**\n` : ''}
这是 Mock 模型，不具备真正的视觉能力；但这条回复证明**图片确实通过 \`content\` 数组传到了模型侧**。

传输结构是这样的：

\`\`\`json
{
  "role": "user",
  "content": [
    { "type": "text", "text": "看图" },
    { "type": "image_url", "image_url": { "url": "data:image/jpeg;base64,…" } }
  ]
}
\`\`\`

换成真实的 VL 模型（如 \`qwen3-vl\`）后，这里就会返回真正的图片理解结果。`
}

function chunkOf(text, model) {
  return {
    id: 'chatcmpl-mock',
    object: 'chat.completion.chunk',
    created: Math.floor(Date.now() / 1000),
    model,
    choices: [{ index: 0, delta: { content: text }, finish_reason: null }],
  }
}

function streamText(res, text, model) {
  // 按 6 个字符切片，模拟真实 token 节奏
  const parts = text.match(/[\s\S]{1,6}/g) || []
  let i = 0
  const tick = () => {
    if (i >= parts.length) {
      sse(res, {
        id: 'chatcmpl-mock',
        object: 'chat.completion.chunk',
        created: Math.floor(Date.now() / 1000),
        model,
        choices: [{ index: 0, delta: {}, finish_reason: 'stop' }],
      })
      res.write('data: [DONE]\n\n')
      res.end()
      return
    }
    sse(res, chunkOf(parts[i++], model))
    setTimeout(tick, 28)
  }
  tick()
}

/** 从多模态 content 中取出文本与图片数量 */
function readUserMessage(msg) {
  if (!msg) return { text: '', images: 0 }
  if (typeof msg.content === 'string') return { text: msg.content, images: 0 }
  if (!Array.isArray(msg.content)) return { text: '', images: 0 }

  let text = ''
  let images = 0
  for (const part of msg.content) {
    if (part?.type === 'text') text += part.text ?? ''
    else if (part?.type === 'image_url') images += 1
  }
  return { text, images }
}

const server = http.createServer((req, res) => {
  console.log(`[mock] ${req.method} ${req.url}`)
  // 兼容 absolute-form URI（代理转发场景）
  const pathname = new URL(req.url, 'http://localhost').pathname
  if (req.method === 'OPTIONS') {
    res.writeHead(204, {
      'Access-Control-Allow-Origin': '*',
      'Access-Control-Allow-Headers': '*',
      'Access-Control-Allow-Methods': '*',
    })
    return res.end()
  }

  if (pathname === '/v1/models') {
    res.writeHead(200, { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*' })
    return res.end(JSON.stringify({ object: 'list', data: [{ id: 'mock-agent', object: 'model' }] }))
  }

  if (pathname === '/v1/chat/completions') {
    let body = ''
    req.on('data', (c) => (body += c))
    req.on('end', () => {
      let payload = {}
      try {
        payload = JSON.parse(body)
      } catch {}
      const model = payload.model || 'mock-agent'
      const msgs = payload.messages || []
      const hasToolMsg = msgs.some((m) => m.role === 'tool')
      const wantsTool = !!payload.tools?.length

      const lastUser = [...msgs].reverse().find((m) => m.role === 'user')
      const { text: userText, images } = readUserMessage(lastUser)
      if (images > 0) console.log(`[mock] images=${images} text="${userText.slice(0, 40)}"`)

      // 连通性测试走非流式，直接返回一个最小的完整响应
      if (payload.stream === false) {
        res.writeHead(200, {
          'Content-Type': 'application/json',
          'Access-Control-Allow-Origin': '*',
        })
        return res.end(
          JSON.stringify({
            id: 'chatcmpl-mock',
            object: 'chat.completion',
            created: Math.floor(Date.now() / 1000),
            model,
            choices: [
              { index: 0, message: { role: 'assistant', content: 'pong' }, finish_reason: 'stop' },
            ],
            usage: { prompt_tokens: 3, completion_tokens: 1, total_tokens: 4 },
          }),
        )
      }

      res.writeHead(200, {
        'Content-Type': 'text/event-stream; charset=utf-8',
        'Cache-Control': 'no-cache',
        Connection: 'keep-alive',
        'Access-Control-Allow-Origin': '*',
      })

      // 带图的消息优先走图片回执，方便验证多模态链路
      if (images > 0) {
        return streamText(res, imageReply(images, userText.trim()), model)
      }

      if (wantsTool && !hasToolMsg) {
        // 第一轮：先吐一点"思考"，再发工具调用
        const toolName = payload.tools.some((t) => t.function?.name === 'list_directory')
          ? 'list_directory'
          : payload.tools[0].function.name
        const args = JSON.stringify({ path: 'I:/project/agent-ui' })
        sse(res, chunkOf('我先看一下项目目录结构。\n\n', model))
        setTimeout(() => {
          sse(res, {
            id: 'chatcmpl-mock',
            object: 'chat.completion.chunk',
            created: Math.floor(Date.now() / 1000),
            model,
            choices: [
              {
                index: 0,
                delta: {
                  tool_calls: [
                    {
                      index: 0,
                      id: 'call_mock_1',
                      type: 'function',
                      function: { name: toolName, arguments: args },
                    },
                  ],
                },
                finish_reason: null,
              },
            ],
          })
          sse(res, {
            id: 'chatcmpl-mock',
            object: 'chat.completion.chunk',
            created: Math.floor(Date.now() / 1000),
            model,
            choices: [{ index: 0, delta: {}, finish_reason: 'tool_calls' }],
          })
          res.write('data: [DONE]\n\n')
          res.end()
        }, 500)
        return
      }

      streamText(res, FINAL_TEXT, model)
    })
    return
  }

  res.writeHead(404, { 'Access-Control-Allow-Origin': '*' })
  res.end('not found')
})

// 端口被占基本都是上一次 mock 没关，给一句人话提示比丢一坨栈有用
server.on('error', (err) => {
  if (err.code === 'EADDRINUSE') {
    console.error(
      `[mock] 端口 ${PORT} 已被占用，多半是上一次 pnpm mock 还在后台跑。\n` +
        `[mock] 直接复用即可（curl http://127.0.0.1:${PORT}/v1/models 验证）；` +
        `要重启的话先结束占用进程：\n` +
        `[mock]   netstat -ano | findstr :${PORT}      # 取 PID\n` +
        `[mock]   taskkill /PID <PID> /F\n` +
        `[mock] 或换端口：PORT=4001 pnpm mock`,
    )
    process.exit(0)
  }
  console.error('[mock] 启动失败：', err)
  process.exit(1)
})

server.listen(PORT, '127.0.0.1', () => {
  console.log(`mock llm ready at http://127.0.0.1:${PORT}/v1 (pid ${process.pid})`)
})
