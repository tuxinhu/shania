<script setup lang="ts">
/**
 * 通话浮层。
 *
 * 只在挂载期间持有 `VoiceConversation`，卸载即挂断——这样「关掉浮层」和
 * 「结束通话」不会走成两条不一样的路径（麦克风忘了关是最难查的那类 bug）。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import Icon from './Icon.vue'
import { CALL_SYSTEM_PROMPT, VoiceConversation } from '@/lib/conversation'
import type { CallState } from '@/lib/conversation'
import { buildWire } from '@/lib/context'
import { useChatStore } from '@/stores/chat'
import { useSettingsStore } from '@/stores/settings'

const emit = defineEmits<{ (e: 'close'): void }>()

const chat = useChatStore()
const settingsStore = useSettingsStore()

const state = ref<CallState>({
  phase: 'idle',
  hint: '',
  reply: '',
  heard: '',
  level: 0,
  muted: false,
  error: '',
})
/** 正在打开麦克风/加载模型：界面要有明确的「连接中」 */
const connecting = ref(true)

let conv: VoiceConversation | null = null
/** 模板要能感知「接通了没有」，所以单独一个 ref，而不是直接看 conv */
const connected = ref(false)

const phaseText = computed(() => {
  if (connecting.value) return '正在接通…'
  switch (state.value.phase) {
    case 'listening':
      return state.value.hint ? '听着…' : '在听…'
    case 'thinking':
      return '思考中…'
    case 'speaking':
      return '说话中…'
    default:
      return '未接通'
  }
})

const transcript = computed(() => state.value.hint || state.value.reply)

/** RMS 大致落在 0.01-0.15，乘 9 再截断，安静时也能看出一点点动静 */
const meterWidth = computed(() => Math.min(1, state.value.level * 9).toFixed(3))

onMounted(async () => {
  const base = settingsStore.settings.systemPrompt?.trim()
  const sysPrompt = base ? `${base}\n\n${CALL_SYSTEM_PROMPT}` : CALL_SYSTEM_PROMPT

  conv = new VoiceConversation({
    buildConfig: () => ({
      baseUrl: settingsStore.settings.baseUrl,
      apiKey: settingsStore.settings.apiKey,
      model: settingsStore.settings.model,
      temperature: settingsStore.settings.temperature,
      maxTokens: settingsStore.settings.maxTokens,
      systemPrompt: sysPrompt,
      // 通话里不调工具：工具要几秒才有结果，用户听到的是漫长的沉默
      toolsEnabled: false,
    }),
    buildMessages: (userText) => {
      const wire = buildWire(chat.messages, settingsStore.settings.contextLimit)
      return [...wire, { role: 'user', content: userText }]
    },
    speed: settingsStore.settings.ttsSpeed,
    onState: (s) => {
      state.value = s
    },
    onTurn: (turn) => chat.appendTurn(turn.user, turn.assistant),
  })

  try {
    await conv.start()
    connected.value = true
  } catch (err) {
    state.value = {
      ...state.value,
      error: err instanceof Error ? err.message : String(err),
    }
  } finally {
    connecting.value = false
  }

  window.addEventListener('keydown', onKey)
})

function onKey(event: KeyboardEvent) {
  if (event.key === 'Escape') void hangup()
}

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKey)
  void conv?.stop()
  conv = null
})

async function hangup() {
  await conv?.stop()
  conv = null
  emit('close')
}

function interrupt() {
  conv?.interrupt()
}

function toggleMute() {
  conv?.setMuted(!state.value.muted)
}
</script>

<template>
  <Teleport to="body">
    <div class="overlay">
      <div class="card" :class="state.phase">
        <div class="head">
          <Icon name="phone" :size="13" />
          <span>语音通话</span>
          <span class="model">{{ settingsStore.settings.model || '未配置模型' }}</span>
          <button class="close" title="挂断（Esc）" @click="hangup">
            <Icon name="x" :size="13" />
          </button>
        </div>

        <div class="stage">
          <div class="halo" :class="{ active: state.phase !== 'idle' && !state.error }">
            <span class="ring" />
            <span class="ring r2" />
            <span class="core">
              <Icon :name="state.phase === 'speaking' ? 'volume' : 'mic'" :size="20" />
            </span>
          </div>
          <p class="phase">{{ state.error ? '没有接通' : phaseText }}</p>
          <!-- 麦克风电平：能看见自己的声音确实进去了，才知道「打断不了」不是麦的问题 -->
          <span class="meter">
            <span class="fill" :style="{ transform: `scaleX(${meterWidth})` }" />
          </span>
        </div>

        <div class="transcript">
          <p v-if="state.error" class="error">{{ state.error }}</p>
          <template v-else>
            <p v-if="transcript" class="line" :class="{ hint: !!state.hint }">
              {{ transcript }}
            </p>
            <p v-else class="placeholder">直接说话就行，不用点击</p>
          </template>
        </div>

        <div class="actions">
          <button
            class="ghost"
            :class="{ on: state.muted }"
            :title="state.muted ? '取消静音' : '静音'"
            :disabled="!connected"
            @click="toggleMute"
          >
            <Icon :name="state.muted ? 'volumeOff' : 'volume'" :size="17" />
          </button>

          <button class="hangup" title="挂断" @click="hangup">
            <Icon name="phone" :size="20" />
          </button>

          <button
            class="ghost"
            title="打断它"
            :disabled="!connected"
            @click="interrupt"
          >
            <Icon name="stop" :size="16" />
          </button>
        </div>

        <p class="tip">
          说话时直接开口即可打断它；建议戴耳机，外放时回声消除有残留。
        </p>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  z-index: 60;
  display: grid;
  place-items: center;
  background: color-mix(in srgb, var(--content) 72%, transparent);
  backdrop-filter: blur(10px);
}

.card {
  width: 340px;
  padding: 16px 18px 14px;
  border-radius: var(--radius-lg);
  background: var(--surface);
  border: 1px solid var(--line);
  box-shadow: var(--shadow-lg);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 14px;
}

.head {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 7px;
  color: var(--text-3);
  font-size: 12px;
  user-select: none;
}

.head .model {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: var(--font-mono);
  font-size: 10.5px;
  opacity: 0.75;
}

.close {
  width: 22px;
  height: 22px;
  border-radius: 6px;
  display: grid;
  place-items: center;
  color: var(--text-3);
}
.close:hover {
  background: var(--hover);
  color: var(--text);
}

.stage {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
  padding: 6px 0 2px;
}

.halo {
  position: relative;
  width: 92px;
  height: 92px;
  display: grid;
  place-items: center;
}

.core {
  width: 56px;
  height: 56px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  background: var(--accent);
  color: #fff;
  transition: transform 0.24s var(--ease), opacity 0.24s var(--ease);
}

.halo:not(.active) .core {
  opacity: 0.35;
  transform: scale(0.9);
}

.ring {
  position: absolute;
  inset: 0;
  border-radius: 50%;
  border: 1.5px solid var(--accent);
  opacity: 0;
}

.halo.active .ring {
  animation: breathe 2.4s ease-out infinite;
}
.halo.active .ring.r2 {
  animation-delay: 1.2s;
}

/* 说话中脉动更快更明显，让用户一眼看出「它在说」 */
.card.speaking .halo.active .ring {
  animation-duration: 1.4s;
}

@keyframes breathe {
  0% {
    opacity: 0.42;
    transform: scale(0.72);
  }
  100% {
    opacity: 0;
    transform: scale(1.15);
  }
}

.phase {
  margin: 0;
  font-size: 12.5px;
  color: var(--text-2);
  letter-spacing: 0.02em;
}

.meter {
  width: 132px;
  height: 3px;
  border-radius: 2px;
  background: var(--line);
  overflow: hidden;
}

.meter .fill {
  display: block;
  width: 100%;
  height: 100%;
  border-radius: 2px;
  background: var(--accent);
  transform-origin: left center;
  transition: transform 0.12s linear;
}

.transcript {
  width: 100%;
  min-height: 62px;
  max-height: 132px;
  overflow-y: auto;
  padding: 9px 11px;
  border-radius: 10px;
  background: var(--fill);
  font-size: 12.5px;
  line-height: 1.65;
  color: var(--text-2);
}

.line {
  margin: 0;
  white-space: pre-wrap;
  word-break: break-word;
}
.line.hint {
  color: var(--text-3);
  font-style: italic;
}

.placeholder {
  margin: 0;
  color: var(--text-3);
  opacity: 0.7;
}

.error {
  margin: 0;
  color: var(--red);
  line-height: 1.6;
}

.actions {
  display: flex;
  align-items: center;
  gap: 18px;
}

.ghost {
  width: 40px;
  height: 40px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  background: var(--fill);
  color: var(--text-2);
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.ghost:hover:not(:disabled) {
  background: var(--hover);
  color: var(--text);
}
.ghost.on {
  background: var(--accent-soft);
  color: var(--accent);
}
.ghost:disabled {
  opacity: 0.3;
  cursor: default;
}

.hangup {
  width: 52px;
  height: 52px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  background: var(--red);
  color: #fff;
  transition: transform 0.14s var(--ease), filter 0.14s var(--ease);
}
.hangup:hover {
  filter: brightness(0.9);
  transform: scale(1.04);
}
/* 电话图标转 135° 就是挂断 */
.hangup :deep(svg) {
  transform: rotate(135deg);
}

.tip {
  margin: 0;
  font-size: 10.5px;
  line-height: 1.5;
  color: var(--text-3);
  opacity: 0.65;
  text-align: center;
}
</style>
