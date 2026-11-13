<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import ChatComposer from './ChatComposer.vue'
import EmptyState from './EmptyState.vue'
import Icon from './Icon.vue'
import MessageItem from './MessageItem.vue'
import VoiceCall from './VoiceCall.vue'
import { useChatStore } from '@/stores/chat'
import { useSettingsStore } from '@/stores/settings'

defineEmits<{ (e: 'open-settings'): void }>()

const chat = useChatStore()
const settingsStore = useSettingsStore()

const composer = ref<InstanceType<typeof ChatComposer> | null>(null)
const scroller = ref<HTMLElement | null>(null)
const stick = ref(true)
/** 通话浮层开关。组件挂载即接通、卸载即挂断，没有第三种状态 */
const callOpen = ref(false)

const title = computed(() => chat.current?.title || '新对话')
const isEmpty = computed(() => chat.messages.length === 0)

const lastSignature = computed(() => {
  const list = chat.messages
  const last = list[list.length - 1]
  if (!last) return 0
  const toolLen =
    last.toolCalls?.reduce((n, c) => n + (c.result?.length ?? 0) + c.arguments.length, 0) ?? 0
  return last.content.length + (last.reasoning?.length ?? 0) + toolLen
})

function onScroll() {
  const el = scroller.value
  if (!el) return
  stick.value = el.scrollHeight - el.scrollTop - el.clientHeight < 140
}

async function toBottom() {
  await nextTick()
  const el = scroller.value
  if (el) el.scrollTop = el.scrollHeight
}

watch(lastSignature, () => {
  if (stick.value) void toBottom()
})

watch(
  () => chat.messages.length,
  () => {
    stick.value = true
    void toBottom()
  },
)

watch(
  () => chat.currentId,
  () => {
    stick.value = true
    void toBottom()
  },
)

// 搜索命中后跳转：等会话切换完成再滚动，避免和「滚到底部」抢位置
watch(
  () => chat.focusMessageId,
  async (id) => {
    if (!id) return
    await nextTick()
    const node = scroller.value?.querySelector<HTMLElement>(`[data-mid="${id}"]`)
    if (node) {
      node.scrollIntoView({ block: 'center', behavior: 'smooth' })
      node.animate(
        [
          { boxShadow: '0 0 0 3px rgba(10, 111, 240, 0.22)', borderRadius: '10px' },
          { boxShadow: '0 0 0 3px rgba(10, 111, 240, 0)' },
        ],
        { duration: 1600, easing: 'ease-out' },
      )
    }
    chat.focusMessageId = ''
  },
)

onMounted(() => {
  void toBottom()
  composer.value?.focus()
})

function pick(text: string) {
  composer.value?.setDraft(text)
}
</script>

<template>
  <main class="main">
    <header class="head" data-tauri-drag-region>
      <div class="head-left">
        <h1 class="title" :title="title">{{ title }}</h1>
        <span v-if="chat.streaming" class="live-dot" title="生成中" />
        <span class="badge">{{ settingsStore.settings.model || '未配置模型' }}</span>
      </div>

      <div class="head-actions">
        <button title="清空当前对话" :disabled="isEmpty" @click="chat.clearCurrent()">
          <Icon name="trash" :size="15" />
        </button>
        <button
          title="重新生成"
          :disabled="isEmpty || chat.streaming"
          @click="chat.regenerate()"
        >
          <Icon name="refresh" :size="15" />
        </button>
        <button title="设置" @click="$emit('open-settings')">
          <Icon name="settings" :size="15" />
        </button>
      </div>
    </header>

    <div ref="scroller" class="scroll" @scroll="onScroll">
      <div class="inner" :class="{ 'is-empty': isEmpty }">
        <EmptyState v-if="isEmpty" @pick="pick" />
        <template v-else>
          <MessageItem
            v-for="(m, i) in chat.messages"
            :key="m.id"
            :message="m"
            :is-last="i === chat.messages.length - 1 && m.role === 'assistant'"
          />
        </template>
      </div>
    </div>

    <ChatComposer
      ref="composer"
      @open-settings="$emit('open-settings')"
      @start-call="callOpen = true"
    />

    <VoiceCall v-if="callOpen" @close="callOpen = false" />
  </main>
</template>

<style scoped>
.main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  background: var(--content);
}

.head {
  height: 52px;
  flex: none;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 0 16px 0 20px;
  border-bottom: 1px solid var(--line-soft);
  user-select: none;
}

.head-left {
  display: flex;
  align-items: center;
  gap: 9px;
  min-width: 0;
}

.title {
  margin: 0;
  font-size: 13px;
  font-weight: 600;
  letter-spacing: -0.01em;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 420px;
}

.live-dot {
  width: 6px;
  height: 6px;
  flex: none;
  border-radius: 50%;
  background: var(--accent);
  animation: pulse 1.4s ease-in-out infinite;
}

@keyframes pulse {
  0%,
  100% {
    opacity: 0.35;
    transform: scale(0.85);
  }
  50% {
    opacity: 1;
    transform: scale(1);
  }
}

.badge {
  flex: none;
  padding: 2px 7px;
  border-radius: 6px;
  background: var(--fill);
  color: var(--text-3);
  font-family: var(--font-mono);
  font-size: 10.5px;
  max-width: 200px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.head-actions {
  margin-left: auto;
  display: flex;
  gap: 1px;
}

.head-actions button {
  width: 28px;
  height: 28px;
  border-radius: 7px;
  display: grid;
  place-items: center;
  color: var(--text-2);
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.head-actions button:hover:not(:disabled) {
  background: var(--hover);
  color: var(--text);
}
.head-actions button:disabled {
  opacity: 0.32;
  cursor: default;
}

.scroll {
  flex: 1;
  overflow-y: auto;
  overflow-x: hidden;
  min-height: 0;
  scroll-behavior: auto;
  /* 两侧都预留滚动条的位置：否则滚动条一出现，内容区左移 5px，
     而下面的输入框没有滚动条不动，两者的中轴就错开了 */
  scrollbar-gutter: stable both-edges;
}

.inner {
  /* 在 .main 里居中：两侧留白自然相等，跟左侧栏、右侧工具条的宽度无关 */
  width: 100%;
  max-width: 800px;
  margin: 0 auto;
  min-height: 100%;
  padding: 26px 28px 10px;
  display: flex;
  flex-direction: column;
  gap: 22px;
}
.inner.is-empty {
  justify-content: center;
  padding-top: 0;
  padding-bottom: 0;
  gap: 0;
}

@media (max-width: 760px) {
  .inner {
    padding: 20px 16px 8px;
  }
  .badge {
    display: none;
  }
}
</style>
