import { defineStore } from 'pinia'
import { computed, reactive, ref, watch } from 'vue'
import { loadSessionsRaw, saveSessionsRaw, stopChat, streamChat } from '@/lib/api'
import { buildWire } from '@/lib/context'
import type { Attachment, Message, Session, StreamEvent, ToolCallRecord } from '@/lib/types'
import { useSettingsStore } from './settings'

export function uid(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) return crypto.randomUUID()
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`
}

function createSession(): Session {
  const now = Date.now()
  return {
    id: uid(),
    title: '新对话',
    messages: [],
    createdAt: now,
    updatedAt: now,
  }
}

export const useChatStore = defineStore('chat', () => {
  const settingsStore = useSettingsStore()

  const sessions = ref<Session[]>([])
  const currentId = ref('')
  const streaming = ref(false)
  const activeRequestId = ref('')
  const ready = ref(false)
  /** 搜索跳转目标：切换会话后由 ChatPanel 滚动到这条消息 */
  const focusMessageId = ref('')

  const current = computed(() => sessions.value.find((s) => s.id === currentId.value) ?? null)
  const messages = computed(() => current.value?.messages ?? [])

  const sortedSessions = computed(() =>
    [...sessions.value].sort((a, b) => {
      if (!!a.pinned !== !!b.pinned) return a.pinned ? -1 : 1
      return b.updatedAt - a.updatedAt
    }),
  )

  const activeSessions = computed(() => sortedSessions.value.filter((s) => !s.archived))
  const archivedSessions = computed(() => sortedSessions.value.filter((s) => s.archived))

  /* ------------------------------ 持久化 ------------------------------ */

  let saveTimer: ReturnType<typeof setTimeout> | null = null
  let saving = false

  async function persist(immediate = false) {
    if (saveTimer) {
      clearTimeout(saveTimer)
      saveTimer = null
    }
    if (immediate) {
      await doSave()
      return
    }
    saveTimer = setTimeout(() => {
      saveTimer = null
      void doSave()
    }, 700)
  }

  async function doSave() {
    if (saving) return
    saving = true
    try {
      await saveSessionsRaw(JSON.stringify(sessions.value))
    } catch (err) {
      console.error('保存会话失败', err)
    } finally {
      saving = false
    }
  }

  watch(sessions, () => void persist(), { deep: true })

  function normalize(list: Session[]): Session[] {
    return list
      .filter((s) => s && typeof s.id === 'string')
      .map((s) => ({
        ...s,
        messages: (s.messages ?? []).map((m) => ({
          ...m,
          status: m.status === 'streaming' ? 'stopped' : m.status,
        })),
      }))
  }

  async function init() {
    if (ready.value) return
    try {
      const raw = await loadSessionsRaw()
      const parsed = JSON.parse(raw)
      sessions.value = Array.isArray(parsed) ? normalize(parsed) : []
    } catch (err) {
      console.error('读取会话失败', err)
      sessions.value = []
    }
    if (!sessions.value.length) {
      const s = createSession()
      sessions.value = [s]
      currentId.value = s.id
    } else {
      currentId.value = (activeSessions.value[0] ?? sortedSessions.value[0]).id
    }
    ready.value = true
  }

  /* ------------------------------ 会话操作 ------------------------------ */

  function newSession() {
    const empty = sessions.value.find((s) => s.messages.length === 0 && !s.archived)
    if (empty) {
      currentId.value = empty.id
      return empty
    }
    const s = createSession()
    sessions.value.unshift(s)
    currentId.value = s.id
    return s
  }

  function selectSession(id: string) {
    currentId.value = id
  }

  function ensureSession(): Session {
    return current.value ?? newSession()
  }

  function removeSession(id: string) {
    const idx = sessions.value.findIndex((s) => s.id === id)
    if (idx === -1) return
    sessions.value.splice(idx, 1)
    if (!sessions.value.length) {
      const s = createSession()
      sessions.value.push(s)
      currentId.value = s.id
    } else if (currentId.value === id) {
      currentId.value = (activeSessions.value[0] ?? sortedSessions.value[0]).id
    }
    void persist(true)
  }

  function renameSession(id: string, title: string) {
    const s = sessions.value.find((v) => v.id === id)
    if (!s) return
    s.title = title.trim() || '新对话'
    s.updatedAt = Date.now()
    void persist(true)
  }

  function togglePin(id: string) {
    const s = sessions.value.find((v) => v.id === id)
    if (!s) return
    s.pinned = !s.pinned
    void persist(true)
  }

  function toggleArchive(id: string) {
    const s = sessions.value.find((v) => v.id === id)
    if (!s) return
    s.archived = !s.archived
    // 归档当前会话时，自动跳到下一个可用会话
    if (s.archived && currentId.value === id) {
      const next = activeSessions.value[0]
      if (next) currentId.value = next.id
    }
    void persist(true)
  }

  function clearCurrent() {
    const s = current.value
    if (!s || streaming.value) return
    s.messages = []
    s.title = '新对话'
    s.updatedAt = Date.now()
    void persist(true)
  }

  /** 清空全部会话，只留下一个新的空会话 */
  function clearAll() {
    if (streaming.value) return
    const s = createSession()
    sessions.value = [s]
    currentId.value = s.id
    void persist(true)
  }

  function jumpToMessage(sessionId: string, messageId: string) {
    currentId.value = sessionId
    focusMessageId.value = messageId
  }

  /* ------------------------------ 流式对话 ------------------------------ */

  async function send(text: string, attachments: Attachment[] = []) {
    const content = text.trim()
    if ((!content && !attachments.length) || streaming.value) return

    const s = ensureSession()
    const now = Date.now()

    const userMsg: Message = {
      id: uid(),
      role: 'user',
      content,
      attachments: attachments.length ? attachments : undefined,
      createdAt: now,
      status: 'done',
    }
    s.messages.push(userMsg)

    if (s.title === '新对话') {
      const base = content || attachments[0]?.name || '图片对话'
      s.title = base.replace(/\s+/g, ' ').slice(0, 28)
    }

    await runCompletion(s)
  }

  async function regenerate() {
    const s = current.value
    if (!s || streaming.value) return
    const lastUser = [...s.messages].reverse().find((m) => m.role === 'user')
    if (!lastUser) return
    while (s.messages.length && s.messages[s.messages.length - 1].role === 'assistant') {
      s.messages.pop()
    }
    await runCompletion(s)
  }

  /** 重试：删掉出错的助手消息，用同样的上文重新请求 */
  async function retry(messageId: string) {
    const s = current.value
    if (!s || streaming.value) return
    const idx = s.messages.findIndex((m) => m.id === messageId)
    if (idx === -1) return
    if (s.messages[idx].role !== 'assistant') return
    s.messages.splice(idx)
    const last = s.messages[s.messages.length - 1]
    if (!last || last.role !== 'user') return
    await runCompletion(s)
  }

  /** 编辑用户消息：改写后丢弃其后的所有回复并重新生成 */
  async function editAndResend(messageId: string, text: string) {
    const s = current.value
    if (!s || streaming.value) return
    const idx = s.messages.findIndex((m) => m.id === messageId)
    if (idx === -1) return
    const target = s.messages[idx]
    if (target.role !== 'user') return

    const next = text.trim()
    if (!next) return

    target.content = next
    target.createdAt = Date.now()
    s.messages.splice(idx + 1)
    s.updatedAt = Date.now()
    await runCompletion(s)
  }

  function removeMessage(messageId: string) {
    const s = current.value
    if (!s || streaming.value) return
    const idx = s.messages.findIndex((m) => m.id === messageId)
    if (idx === -1) return
    s.messages.splice(idx, 1)
    s.updatedAt = Date.now()
    void persist(true)
  }

  async function runCompletion(s: Session) {
    // 必须是响应式对象：流式增量直接写它才能触发视图更新
    const bot = reactive<Message>({
      id: uid(),
      role: 'assistant',
      content: '',
      reasoning: '',
      toolCalls: [],
      createdAt: Date.now(),
      model: settingsStore.settings.model,
      status: 'streaming',
    })
    s.messages.push(bot)
    s.updatedAt = Date.now()

    const history = s.messages.filter((m) => m.id !== bot.id)
    const wire = buildWire(history, settingsStore.settings.contextLimit)

    const requestId = uid()
    activeRequestId.value = requestId
    streaming.value = true

    let contentBuf = ''
    let reasoningBuf = ''
    let flushTimer: ReturnType<typeof setTimeout> | null = null

    const applyBuffers = () => {
      if (reasoningBuf) {
        bot.reasoning = (bot.reasoning ?? '') + reasoningBuf
        reasoningBuf = ''
      }
      if (contentBuf) {
        bot.content += contentBuf
        contentBuf = ''
      }
    }
    const schedule = () => {
      if (flushTimer) return
      flushTimer = setTimeout(() => {
        flushTimer = null
        applyBuffers()
      }, 32)
    }
    const flushNow = () => {
      if (flushTimer) {
        clearTimeout(flushTimer)
        flushTimer = null
      }
      applyBuffers()
    }

    const onEvent = (event: StreamEvent) => {
      switch (event.type) {
        case 'reasoning':
          reasoningBuf += event.delta
          schedule()
          break
        case 'content':
          contentBuf += event.delta
          schedule()
          break
        case 'round':
          if (event.index > 0) flushNow()
          break
        case 'toolStart': {
          flushNow()
          const call: ToolCallRecord = {
            id: event.id,
            name: event.name,
            arguments: event.arguments,
            status: 'running',
          }
          bot.toolCalls = [...(bot.toolCalls ?? []), call]
          break
        }
        case 'toolResult': {
          const list = bot.toolCalls ?? []
          const target = list.find((c) => c.id === event.id)
          if (target) {
            target.status = event.ok ? 'done' : 'error'
            target.ok = event.ok
            target.result = event.result
            target.elapsedMs = event.elapsedMs
          }
          break
        }
        case 'done':
          flushNow()
          if (bot.status !== 'stopped') bot.status = 'done'
          if (event.usage) bot.usage = event.usage
          break
        case 'error':
          flushNow()
          bot.status = 'error'
          bot.error = event.message
          break
      }
    }

    try {
      await streamChat(
        {
          requestId,
          config: {
            baseUrl: settingsStore.settings.baseUrl,
            apiKey: settingsStore.settings.apiKey,
            model: settingsStore.settings.model,
            temperature: settingsStore.settings.temperature,
            maxTokens: settingsStore.settings.maxTokens,
            systemPrompt: settingsStore.settings.systemPrompt,
            toolsEnabled: settingsStore.settings.toolsEnabled,
          },
          messages: wire,
        },
        onEvent,
      )
    } catch (err) {
      flushNow()
      if (bot.status !== 'error') {
        bot.status = 'error'
        bot.error = err instanceof Error ? err.message : String(err)
      }
    } finally {
      flushNow()
      if (bot.status === 'streaming') bot.status = 'done'
      if (!bot.content && !bot.error && !(bot.toolCalls && bot.toolCalls.length)) {
        bot.error = '模型没有返回任何内容，请检查接口地址与模型名是否正确。'
        bot.status = 'error'
      }
      streaming.value = false
      activeRequestId.value = ''
      s.updatedAt = Date.now()
      void persist(true)
    }
  }

  /**
   * 导入会话：按 id 去重，已存在的直接跳过（重复导入同一份文件不会产生副本）。
   * 返回新增与跳过的条数，供界面提示。
   */
  async function importSessions(list: Session[]): Promise<{ added: number; skipped: number }> {
    const existing = new Set(sessions.value.map((s) => s.id))
    let added = 0
    let skipped = 0

    for (const item of list) {
      if (existing.has(item.id)) {
        skipped += 1
        continue
      }
      existing.add(item.id)
      sessions.value.push(item)
      added += 1
    }

    if (added > 0) {
      await persist(true)
      // 当前没有选中会话时，直接跳到导入的第一条，省得用户自己找
      if (!currentId.value) selectSession(list[0].id)
    }

    return { added, skipped }
  }

  /**
   * 通话结束一轮：把用户说的话和模型**实际说出口**的回答落库。
   *
   * 刻意不复用 `send()`：那边会调 `runCompletion` 重新请求一遍模型，
   * 而通话的回答在播放时就已经生成好了。`assistant` 传的是截断后的内容
   * （被打断时用户没听到的部分不该进上下文，否则下一轮模型以为自己说过了）。
   */
  function appendTurn(user: string, assistant: string) {
    const u = user.trim()
    if (!u) return

    const s = ensureSession()
    const now = Date.now()
    s.messages.push({
      id: uid(),
      role: 'user',
      content: u,
      createdAt: now,
      status: 'done',
    })

    const a = assistant.trim()
    if (a) {
      s.messages.push({
        id: uid(),
        role: 'assistant',
        content: a,
        createdAt: now + 1,
        status: 'done',
        model: settingsStore.settings.model,
      })
    }

    s.updatedAt = now
    if (s.title === '新对话') {
      s.title = u.replace(/\s+/g, ' ').slice(0, 28)
    }
  }

  async function stop() {    if (!streaming.value || !activeRequestId.value) return
    const bot = [...messages.value].reverse().find((m) => m.status === 'streaming')
    if (bot) bot.status = 'stopped'
    try {
      await stopChat(activeRequestId.value)
    } catch (err) {
      console.error('停止失败', err)
    }
  }

  return {
    sessions,
    sortedSessions,
    activeSessions,
    archivedSessions,
    currentId,
    current,
    messages,
    streaming,
    ready,
    focusMessageId,
    init,
    newSession,
    selectSession,
    removeSession,
    renameSession,
    togglePin,
    toggleArchive,
    clearCurrent,
    clearAll,
    jumpToMessage,
    send,
    stop,
    appendTurn,
    regenerate,
    retry,
    editAndResend,
    removeMessage,
    importSessions,
  }
})
