<script setup lang="ts">
import { computed, nextTick, ref } from 'vue'
import Icon from './Icon.vue'
import MarkdownView from './MarkdownView.vue'
import ToolCallCard from './ToolCallCard.vue'
import { copyText } from '@/lib/clipboard'
import { formatTime } from '@/lib/format'
import type { Message } from '@/lib/types'
import { useChatStore } from '@/stores/chat'
import { useSettingsStore } from '@/stores/settings'

const props = defineProps<{ message: Message; isLast: boolean }>()

const chat = useChatStore()
const settingsStore = useSettingsStore()

const imageAtts = computed(() =>
  (props.message.attachments ?? []).filter((a) => (a.kind ?? 'image') === 'image' && a.dataUrl),
)
const textAtts = computed(() => (props.message.attachments ?? []).filter((a) => a.kind === 'text'))

const showReasoning = ref(false)
const copied = ref(false)
const editing = ref(false)
const editText = ref('')
const editBox = ref<HTMLTextAreaElement | null>(null)

const name = computed(() => settingsStore.settings.displayName || 'Shania')
const time = computed(() => formatTime(props.message.createdAt))
const streaming = computed(() => props.message.status === 'streaming')
const isUser = computed(() => props.message.role === 'user')

const tokenText = computed(() => {
  const u = props.message.usage
  if (!u?.completion_tokens) return ''
  const parts = [`${u.completion_tokens} tokens`]
  if (u.prompt_tokens) parts.unshift(`${u.prompt_tokens} in`)
  return parts.join(' · ')
})

async function copy() {
  const ok = await copyText(props.message.content)
  if (!ok) return
  copied.value = true
  window.setTimeout(() => (copied.value = false), 1400)
}

async function startEdit() {
  if (chat.streaming) return
  editText.value = props.message.content
  editing.value = true
  await nextTick()
  const el = editBox.value
  if (el) {
    el.focus()
    el.setSelectionRange(el.value.length, el.value.length)
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, 320)}px`
  }
}

function cancelEdit() {
  editing.value = false
  editText.value = ''
}

function saveEdit() {
  const text = editText.value.trim()
  if (!text) return
  editing.value = false
  void chat.editAndResend(props.message.id, text)
}

function onEditKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    e.preventDefault()
    cancelEdit()
    return
  }
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
    e.preventDefault()
    saveEdit()
  }
}

function remove() {
  chat.removeMessage(props.message.id)
}

function retry() {
  void chat.retry(props.message.id)
}
</script>

<template>
  <div v-if="isUser" class="msg user" :data-mid="message.id">
    <div class="user-wrap">
      <div v-if="editing" class="edit-box">
        <textarea
          ref="editBox"
          v-model="editText"
          rows="3"
          spellcheck="false"
          @keydown="onEditKeydown"
        />
        <div class="edit-bar">
          <span class="edit-tip">Ctrl + Enter 保存 · Esc 取消</span>
          <button class="mini ghost" @click="cancelEdit">取消</button>
          <button class="mini primary" :disabled="!editText.trim()" @click="saveEdit">保存并重发</button>
        </div>
      </div>

      <template v-else>
        <div v-if="imageAtts.length || textAtts.length" class="user-atts">
          <img v-for="a in imageAtts" :key="a.id" :src="a.dataUrl" :alt="a.name" :title="a.name" />
          <span v-for="a in textAtts" :key="a.id" class="att-file" :title="a.name">
            <Icon name="file" :size="12" />
            <span class="att-file-name">{{ a.name }}</span>
          </span>
        </div>
        <div v-if="message.content" class="user-bubble">{{ message.content }}</div>
        <div class="user-acts">
          <button title="编辑并重新发送" :disabled="chat.streaming" @click="startEdit">
            <Icon name="edit" :size="12" />
          </button>
          <button title="复制" @click="copy">
            <Icon :name="copied ? 'check' : 'copy'" :size="12" />
          </button>
          <button title="删除这条消息" :disabled="chat.streaming" @click="remove">
            <Icon name="trash" :size="12" />
          </button>
        </div>
      </template>
    </div>
  </div>

  <div v-else class="msg assistant" :data-mid="message.id">
    <div class="body">
      <div class="msg-head">
        <span class="avatar">
          <Icon name="sparkles" :size="13" />
        </span>
        <span class="name">{{ name }}</span>
        <span class="meta">{{ time }}</span>
        <span v-if="message.model" class="meta dot-sep">{{ message.model }}</span>
      </div>

      <div v-if="message.reasoning" class="reasoning">
        <button class="reasoning-toggle" @click="showReasoning = !showReasoning">
          <Icon name="brain" :size="12" />
          <span>思考过程</span>
          <span class="r-meta">{{ message.reasoning.length }} 字</span>
          <Icon class="chev" :class="{ open: showReasoning }" name="chevron" :size="12" />
        </button>
        <div v-if="showReasoning" class="reasoning-body">{{ message.reasoning }}</div>
      </div>

      <div v-if="message.toolCalls?.length" class="tools">
        <ToolCallCard v-for="call in message.toolCalls" :key="call.id" :call="call" />
      </div>

      <MarkdownView v-if="message.content" :content="message.content" />

      <div v-if="streaming && !message.content" class="thinking">
        <span class="dots"><i /><i /><i /></span>
        <span>{{ message.toolCalls?.length ? '正在处理工具结果…' : '正在思考…' }}</span>
      </div>

      <div v-if="message.error" class="error-box">
        <div class="error-text">
          <Icon name="info" :size="13" />
          <span>{{ message.error }}</span>
        </div>
        <button class="retry-btn" :disabled="chat.streaming" @click="retry">
          <Icon name="refresh" :size="12" />
          <span>重试</span>
        </button>
      </div>

      <div v-if="message.status === 'stopped'" class="stopped-note">已停止生成</div>

      <div v-if="!streaming && (message.content || message.toolCalls?.length)" class="msg-foot">
        <div class="foot-actions">
          <button title="复制回答" @click="copy">
            <Icon :name="copied ? 'check' : 'copy'" :size="12" />
            <span>{{ copied ? '已复制' : '复制' }}</span>
          </button>
          <button v-if="isLast" title="重新生成" :disabled="chat.streaming" @click="chat.regenerate()">
            <Icon name="refresh" :size="12" />
            <span>重新生成</span>
          </button>
          <button title="删除这条回复" :disabled="chat.streaming" @click="remove">
            <Icon name="trash" :size="12" />
            <span>删除</span>
          </button>
        </div>
        <span v-if="tokenText" class="usage">{{ tokenText }}</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.msg {
  animation: fade-up 0.28s var(--ease) both;
}

/* ---------- 用户 ---------- */
.msg.user {
  display: flex;
  justify-content: flex-end;
}

.user-wrap {
  max-width: 78%;
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 5px;
}

.user-atts {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
  justify-content: flex-end;
}
.user-atts img {
  width: 96px;
  height: 96px;
  object-fit: cover;
  border-radius: 12px;
  border: 1px solid var(--line);
  background: var(--surface-2);
  display: block;
}

/* 文本附件：显示成文件名标签，正文不在这里铺开 */
.att-file {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 260px;
  height: 30px;
  padding: 0 12px;
  border-radius: 9px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text-2);
  font-size: 12px;
}
.att-file-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.user-bubble {
  padding: 9px 14px;
  border-radius: 18px 18px 6px 18px;
  background: var(--bubble-user);
  color: var(--bubble-user-text);
  font-size: 14px;
  line-height: 1.62;
  white-space: pre-wrap;
  word-break: break-word;
  box-shadow: 0 1px 2px rgba(10, 100, 220, 0.24);
}

.user-acts {
  display: flex;
  gap: 1px;
  opacity: 0;
  transition: opacity 0.16s var(--ease);
}
.msg:hover .user-acts {
  opacity: 1;
}
.user-acts button {
  width: 22px;
  height: 20px;
  border-radius: 5px;
  display: grid;
  place-items: center;
  color: var(--text-3);
}
.user-acts button:hover:not(:disabled) {
  background: var(--hover);
  color: var(--text-2);
}
.user-acts button:disabled {
  opacity: 0.35;
  cursor: default;
}

/* ---------- 编辑态 ---------- */
.edit-box {
  width: 620px;
  max-width: 100%;
  padding: 9px;
  border-radius: 14px;
  background: var(--surface);
  border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--line));
  box-shadow: 0 0 0 3px var(--accent-soft), var(--shadow-md);
}
.edit-box textarea {
  display: block;
  width: 100%;
  min-height: 62px;
  max-height: 320px;
  padding: 4px 5px 6px;
  border: none;
  background: transparent;
  outline: none;
  resize: none;
  font-size: 13.5px;
  line-height: 1.6;
  color: var(--text);
  font-family: inherit;
}
.edit-bar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding-top: 5px;
  border-top: 1px solid var(--line-soft);
}
.edit-tip {
  margin-right: auto;
  font-size: 10.5px;
  color: var(--text-3);
  padding-left: 3px;
}
.mini {
  height: 25px;
  padding: 0 10px;
  border-radius: 7px;
  font-size: 11.5px;
  font-weight: 500;
}
.mini.ghost {
  color: var(--text-2);
}
.mini.ghost:hover {
  background: var(--hover);
}
.mini.primary {
  background: var(--accent);
  color: #fff;
}
.mini.primary:hover:not(:disabled) {
  background: var(--accent-hover);
}
.mini.primary:disabled {
  opacity: 0.45;
  cursor: default;
}

/* ---------- 助手 ---------- */
/* 头像跟着名字走，不单独占左侧一列：否则正文会被挤到图标右边 37px，
   跟图标不在同一个左边界上。现在正文、消息头、工具卡片全是同一条竖线 */
.msg.assistant {
  display: block;
}

.avatar {
  width: 22px;
  height: 22px;
  flex: none;
  border-radius: 7px;
  display: grid;
  place-items: center;
  color: #fff;
  background: linear-gradient(145deg, #59b0ff, #0a6ff0 62%, #7b5cff);
  box-shadow: 0 1px 3px rgba(10, 111, 240, 0.3), inset 0 0.5px 0 rgba(255, 255, 255, 0.5);
}

.body {
  min-width: 0;
}

.msg-head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 5px;
}

/* 头像比文字高，跟第一行垂直居中对齐；字号不同的 name / meta 也走 center，
   基线对齐会让 10.5px 的 meta 明显下沉 */
.msg-head .avatar {
  margin-right: 1px;
}

.name {
  font-size: 12.5px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.meta {
  font-size: 10.5px;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}
.meta.dot-sep::before {
  content: '·';
  margin-right: 6px;
}

/* ---------- 思考过程 ---------- */
.reasoning {
  margin: 2px 0 10px;
}

.reasoning-toggle {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 3px 8px 3px 6px;
  border-radius: 7px;
  background: var(--fill);
  color: var(--text-2);
  font-size: 11.5px;
  transition: background 0.14s var(--ease);
}
.reasoning-toggle:hover {
  background: var(--fill-strong);
}
.r-meta {
  color: var(--text-3);
  font-size: 10.5px;
}
.chev {
  transition: transform 0.18s var(--ease);
  color: var(--text-3);
}
.chev.open {
  transform: rotate(180deg);
}

.reasoning-body {
  margin-top: 6px;
  padding: 9px 12px;
  border-left: 2px solid var(--fill-strong);
  background: var(--surface-2);
  border-radius: 0 8px 8px 0;
  font-size: 12.5px;
  line-height: 1.68;
  color: var(--text-2);
  white-space: pre-wrap;
  max-height: 260px;
  overflow-y: auto;
  animation: fade-up 0.18s var(--ease);
}

/* ---------- 工具 ---------- */
.tools {
  margin-bottom: 4px;
}

/* ---------- 思考中 ---------- */
.thinking {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 0 2px;
  font-size: 12.5px;
  color: var(--text-3);
}

.dots {
  display: inline-flex;
  gap: 3.5px;
}
.dots i {
  width: 4.5px;
  height: 4.5px;
  border-radius: 50%;
  background: var(--text-3);
  animation: blink 1.3s infinite ease-in-out;
}
.dots i:nth-child(2) {
  animation-delay: 0.18s;
}
.dots i:nth-child(3) {
  animation-delay: 0.36s;
}

@keyframes blink {
  0%,
  70%,
  100% {
    opacity: 0.25;
    transform: scale(0.85);
  }
  35% {
    opacity: 1;
    transform: scale(1);
  }
}

/* ---------- 错误 / 停止 ---------- */
.error-box {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  margin-top: 8px;
  padding: 9px 11px;
  border-radius: 9px;
  background: color-mix(in srgb, var(--red) 9%, transparent);
  border: 1px solid color-mix(in srgb, var(--red) 24%, transparent);
}

.error-text {
  flex: 1;
  display: flex;
  gap: 7px;
  align-items: flex-start;
  color: color-mix(in srgb, var(--red) 82%, var(--text));
  font-size: 12.5px;
  line-height: 1.6;
  white-space: pre-wrap;
  min-width: 0;
}

.retry-btn {
  flex: none;
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 23px;
  padding: 0 8px;
  border-radius: 6px;
  font-size: 11.5px;
  color: color-mix(in srgb, var(--red) 82%, var(--text));
  border: 1px solid color-mix(in srgb, var(--red) 30%, transparent);
  background: color-mix(in srgb, var(--red) 6%, transparent);
}
.retry-btn:hover:not(:disabled) {
  background: color-mix(in srgb, var(--red) 14%, transparent);
}
.retry-btn:disabled {
  opacity: 0.45;
  cursor: default;
}

.stopped-note {
  margin-top: 8px;
  font-size: 11.5px;
  color: var(--text-3);
}

/* ---------- 底部操作 ---------- */
.msg-foot {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 7px;
  min-height: 20px;
}

.foot-actions {
  display: flex;
  gap: 2px;
  opacity: 0;
  transition: opacity 0.16s var(--ease);
}
.msg:hover .foot-actions {
  opacity: 1;
}

.foot-actions button {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 22px;
  padding: 0 7px;
  border-radius: 6px;
  font-size: 11px;
  color: var(--text-3);
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.foot-actions button:hover:not(:disabled) {
  background: var(--hover);
  color: var(--text-2);
}
.foot-actions button:disabled {
  opacity: 0.4;
  cursor: default;
}

.usage {
  margin-left: auto;
  font-size: 10.5px;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}
</style>
