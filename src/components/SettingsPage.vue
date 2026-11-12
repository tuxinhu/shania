<script setup lang="ts">
/**
 * 设置页（整页，不是弹窗）。
 *
 * 布局：左边模块导航、右边配置项。分会话页与设置页两屏，是因为设置项已经多到
 * 一个 560px 的弹窗装不下——七段内容摞在一起，用户想改「语速」得先滚过三个
 * 下载进度条。
 *
 * 模块是按「什么时候会来改它」分的，不是按代码结构：
 *   通用   装完基本不动（外观、快捷键、启动行为）
 *   模型   连接参数和生成参数绑在一起改，测试按钮就在旁边
 *   人设   要反复改的长文本，单独一页免得每次滚
 *   语音   输入/输出都是「下载 + 状态 + 试听」的重操作，不能被别的项抢注意力
 *   工具   现在只有开关，但它是 MCP 的落点，先占住位置
 *   数据   低频、都涉及不可恢复操作与版本信息
 */
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import Icon from './Icon.vue'
import WindowControls from './WindowControls.vue'
import {
  applyGlobalShortcut,
  checkForUpdate,
  installUpdate,
  isAutoStartEnabled,
  isTauri,
  openLogsDir,
  setAutoStart,
  setCloseAction,
  sttStreamInstall,
  sttStreamStatus,
  sttStreamUnload,
  testConnection,
  ttsInstall,
  ttsSay,
  ttsStatus,
  ttsUnload,
  type StreamStatus,
  type TtsStatus,
} from '@/lib/api'
import { acceleratorFromEvent, formatShortcut } from '@/lib/shortcut'
import { useChatStore } from '@/stores/chat'
import { useSettingsStore } from '@/stores/settings'

const emit = defineEmits<{ (e: 'close'): void }>()

const settingsStore = useSettingsStore()
const chat = useChatStore()
const showKey = ref(false)

const APP_VERSION = '0.1.0'

/* ------------------------------ 模块导航 ------------------------------ */

type ModuleId = 'general' | 'model' | 'persona' | 'voice' | 'tools' | 'data'

const modules: { id: ModuleId; label: string; icon: string; desc: string }[] = [
  { id: 'general', label: '通用', icon: 'settings', desc: '外观、唤起方式与启动行为' },
  { id: 'model', label: '模型', icon: 'brain', desc: '接口地址、生成参数与多套配置' },
  { id: 'persona', label: '人设', icon: 'sparkles', desc: '决定它怎么说话、扮演什么角色' },
  { id: 'voice', label: '语音', icon: 'mic', desc: '语音输入与语音合成的模型和参数' },
  { id: 'tools', label: '工具', icon: 'tool', desc: '允许它调用哪些本机能力' },
  { id: 'data', label: '数据与关于', icon: 'info', desc: '会话存储、日志与版本更新' },
]

const TAB_KEY = 'shania.settings.tab'

function initialTab(): ModuleId {
  const saved = localStorage.getItem(TAB_KEY)
  // 上次停在哪页就回哪页：改语音参数的人不会想每次从「通用」翻起
  if (saved && modules.some((m) => m.id === saved)) return saved as ModuleId
  return 'general'
}

const active = ref<ModuleId>(initialTab())
const current = computed(() => modules.find((m) => m.id === active.value) ?? modules[0])

watch(active, (v) => localStorage.setItem(TAB_KEY, v))

/** 导航上的状态点：这页有需要处理的事才亮，不拿「没装模型」当常态噪音 */
const dots = computed<Record<string, boolean>>(() => ({
  voice: voiceNeedsAttention.value,
  data: updatePhase.value === 'available',
}))

function close() {
  emit('close')
}

/* ---------- 快捷键录制 ---------- */
const recording = ref(false)
const shortcutError = ref('')
const shortcutText = computed(() => formatShortcut(settingsStore.settings.shortcut))

const themes = [
  { value: 'system', label: '跟随系统', icon: 'monitor' },
  { value: 'light', label: '浅色', icon: 'sun' },
  { value: 'dark', label: '深色', icon: 'moon' },
] as const

/* ---------- 模型配置 ---------- */
const profileName = ref('')
const activeProfileId = computed(() => settingsStore.settings.activeProfileId)

function saveProfile() {
  const name = profileName.value.trim()
  if (!name) return
  settingsStore.saveCurrentAsProfile(name)
  profileName.value = ''
}

/* ---------- 连通性测试 ---------- */
const testing = ref(false)
const testOk = ref(false)
const testText = ref('')

async function runTest() {
  if (testing.value) return
  testing.value = true
  testOk.value = false
  testText.value = '正在请求…'
  try {
    const r = await testConnection({
      baseUrl: settingsStore.settings.baseUrl,
      apiKey: settingsStore.settings.apiKey,
      model: settingsStore.settings.model,
      temperature: settingsStore.settings.temperature,
      maxTokens: 8,
    })
    testOk.value = true
    testText.value = `连接正常 · ${r.latencyMs} ms · ${r.model}`
  } catch (err) {
    testOk.value = false
    testText.value = String(err instanceof Error ? err.message : err)
  } finally {
    testing.value = false
  }
}

/* ---------- 开机自启 ---------- */
const autoStart = ref(false)
const autoStartBusy = ref(false)

async function toggleAutoStart() {
  if (autoStartBusy.value) return
  autoStartBusy.value = true
  const next = !autoStart.value
  try {
    await setAutoStart(next)
    autoStart.value = next
    settingsStore.settings.autoStart = next
  } catch (err) {
    shortcutError.value = `开机自启设置失败：${String(err)}`
  } finally {
    autoStartBusy.value = false
  }
}

/* ---------- 关闭行为 ---------- */
async function changeCloseAction(action: 'tray' | 'quit') {
  settingsStore.settings.closeAction = action
  try {
    await setCloseAction(action)
  } catch {
    /* 预览环境无后端，忽略 */
  }
}

/* ---------- 更新 ---------- */
type UpdatePhase = 'idle' | 'checking' | 'none' | 'available' | 'unsupported' | 'error' | 'installing'
const updatePhase = ref<UpdatePhase>('idle')
const updateMessage = ref('')
const updateProgress = ref(0)

async function checkUpdate() {
  if (updatePhase.value === 'checking' || updatePhase.value === 'installing') return
  updatePhase.value = 'checking'
  updateMessage.value = '正在检查…'
  const result = await checkForUpdate()
  if (result.status === 'available') {
    updatePhase.value = 'available'
    updateMessage.value = `发现新版本 ${result.version}`
  } else if (result.status === 'none') {
    updatePhase.value = 'none'
    updateMessage.value = '已是最新版本'
  } else if (result.status === 'unsupported') {
    updatePhase.value = 'unsupported'
    updateMessage.value = result.message ?? '当前环境不支持自动更新'
  } else {
    updatePhase.value = 'error'
    updateMessage.value = result.message ?? '检查失败'
  }
}

async function doInstall() {
  if (updatePhase.value !== 'available') return
  updatePhase.value = 'installing'
  updateProgress.value = 0
  updateMessage.value = '正在下载更新…'
  try {
    await installUpdate((p) => {
      updateProgress.value = p
      updateMessage.value = `正在下载更新… ${p}%`
    })
  } catch (err) {
    updatePhase.value = 'error'
    updateMessage.value = `安装失败：${err instanceof Error ? err.message : String(err)}`
  }
}

/* ---------- 清空数据 / 恢复默认 ---------- */
const confirmingClear = ref(false)
const confirmingReset = ref(false)
let clearTimer: ReturnType<typeof setTimeout> | null = null
let resetTimer: ReturnType<typeof setTimeout> | null = null

function clearAllSessions() {
  if (chat.streaming) return
  if (!confirmingClear.value) {
    confirmingClear.value = true
    clearTimer = setTimeout(() => (confirmingClear.value = false), 4000)
    return
  }
  if (clearTimer) clearTimeout(clearTimer)
  confirmingClear.value = false
  chat.clearAll()
}

async function resetAll() {
  if (!confirmingReset.value) {
    confirmingReset.value = true
    resetTimer = setTimeout(() => (confirmingReset.value = false), 4000)
    return
  }
  if (resetTimer) clearTimeout(resetTimer)
  confirmingReset.value = false
  settingsStore.reset()
  shortcutError.value = ''
  try {
    await applyGlobalShortcut(settingsStore.settings.shortcut)
    await setCloseAction(settingsStore.settings.closeAction)
  } catch (err) {
    shortcutError.value = String(err)
  }
}

/* ---------- Esc 关闭 ---------- */
function onKeydown(e: KeyboardEvent) {
  // 正在录快捷键时 Esc 是「取消录制」，不能顺带把整页关掉
  if (recording.value) return
  if (e.key === 'Escape') close()
}

/** 开始录制：先注销当前热键，否则按下旧组合会直接把窗口收起来 */
async function startCapture() {
  if (recording.value) return
  shortcutError.value = ''
  recording.value = true
  try {
    await applyGlobalShortcut('')
  } catch {
    /* 注销失败不影响录制，最多是旧键仍然生效 */
  }
}

/** 取消录制：把原来的键重新挂回去 */
async function cancelCapture() {
  if (!recording.value) return
  recording.value = false
  try {
    await applyGlobalShortcut(settingsStore.settings.shortcut)
  } catch {
    /* ignore */
  }
}

async function onHotkeyKeydown(e: KeyboardEvent) {
  if (!recording.value) return
  e.preventDefault()
  e.stopPropagation()

  // 事件派发结束后 currentTarget 会置空，提前持有引用
  const button = e.currentTarget as HTMLElement | null

  if (e.key === 'Escape') {
    void cancelCapture()
    return
  }
  if (e.repeat) return

  const result = acceleratorFromEvent(e)
  if (!result.ok) {
    // reason 为空表示只按了修饰键，继续等待主键
    if (result.reason) shortcutError.value = result.reason
    return
  }

  const previous = settingsStore.settings.shortcut
  try {
    await applyGlobalShortcut(result.accelerator)
    settingsStore.settings.shortcut = result.accelerator
    shortcutError.value = ''
    recording.value = false
    // 交还焦点，避免随后误按空格又把按钮激活成录制态
    button?.blur()
  } catch (err) {
    shortcutError.value = String(err)
    recording.value = false
    try {
      await applyGlobalShortcut(previous)
    } catch {
      /* 旧键也回不来时，只能等用户重新设置 */
    }
  }
}

/* ---------- 语音输入（X-ASR） ---------- */
const stt = ref<StreamStatus | null>(null)
const sttInstalling = ref(false)
const sttError = ref('')
const sttProgress = ref<{ stage: string; done: number; total: number | null }>({
  stage: '',
  done: 0,
  total: null,
})
let unlistenStt: UnlistenFn | null = null

/** 模型在不在盘上；不在就点麦克风也白搭 */
const streamReady = computed(() => !!stt.value?.ready)

const sttPercent = computed(() => {
  const { done, total } = sttProgress.value
  if (!total) return done > 0 ? 100 : 0
  return Math.min(100, Math.round((done / total) * 100))
})

const sttStageText = computed(() => {
  const stage = sttProgress.value.stage
  if (!stage) return ''
  return stage === 'done' ? '收尾' : '下载模型'
})

/** 模型状态：装没装、多大、有没有加载进内存 */
const streamStateText = computed(() => {
  if (!stt.value) return '浏览器预览环境不可用'
  const mb = Math.round(stt.value.bytes / 1024 / 1024)
  if (!stt.value.ready) {
    return mb > 0 ? `下载未完成（${mb}MB）` : '未安装'
  }
  return stt.value.loaded ? `已就绪（${mb}MB，已加载）` : `已安装（${mb}MB），用到时加载`
})

const preloadHint = computed(() =>
  settingsStore.settings.sttPreload
    ? '开：启动后在后台把模型读进内存（约 500MB），点麦克风立刻出字。下次启动生效'
    : '关：点麦克风时才加载模型，第一次会等 1-2 秒。下次启动生效',
)

async function refreshStt() {
  stt.value = await sttStreamStatus().catch(() => null)
}

async function installStream() {
  if (sttInstalling.value) return
  sttInstalling.value = true
  sttError.value = ''
  try {
    await sttStreamInstall()
  } catch (err) {
    sttError.value = err instanceof Error ? err.message : String(err)
  } finally {
    sttInstalling.value = false
    sttProgress.value = { stage: '', done: 0, total: null }
    await refreshStt()
  }
}

async function unloadStream() {
  await sttStreamUnload().catch(() => undefined)
  await refreshStt()
}

async function openLogs() {
  // 打不开目录没什么可补救的，记一笔即可；界面上不用弹错
  await openLogsDir().catch((err) => console.error('打开日志目录失败', err))
}

function toggleSttPreload() {
  settingsStore.settings.sttPreload = !settingsStore.settings.sttPreload
}

/* ---------- 语音输出（MeloTTS） ---------- */
const tts = ref<TtsStatus | null>(null)
const ttsInstalling = ref(false)
const ttsTesting = ref(false)
const ttsError = ref('')
/** 试听后的自检信息：合成出来的时长/采样率/上下文状态 */
const ttsDiag = ref('')
const ttsProgress = ref<{ stage: string; done: number; total: number | null }>({
  stage: '',
  done: 0,
  total: null,
})

const ttsReady = computed(() => !!tts.value?.ready)

const ttsPercent = computed(() => {
  const { done, total } = ttsProgress.value
  if (!total) return done > 0 ? 100 : 0
  return Math.min(100, Math.round((done / total) * 100))
})

const ttsStateText = computed(() => {
  if (!tts.value) return '浏览器预览环境不可用'
  const mb = Math.round(tts.value.bytes / 1024 / 1024)
  if (!tts.value.ready) {
    // 目录里有东西但不完整：通常是下载断在半路
    return mb > 0 ? `下载未完成（${mb}MB）` : '未安装'
  }
  return tts.value.loaded
    ? `运行中（${mb}MB，${Math.round(tts.value.rate / 1000)}kHz）`
    : `已安装（${mb}MB），用到时加载`
})

/** 语音这页有没有需要处理的事：下载断了、或者刚报过错 */
const voiceNeedsAttention = computed(() => {
  const broken = (s: { ready: boolean; bytes: number } | null) => !!s && !s.ready && s.bytes > 0
  return !!sttError.value || !!ttsError.value || broken(stt.value) || broken(tts.value)
})

async function refreshTts() {
  tts.value = await ttsStatus().catch(() => null)
}

async function installTts() {
  if (ttsInstalling.value) return
  ttsInstalling.value = true
  ttsError.value = ''
  try {
    await ttsInstall()
  } catch (err) {
    ttsError.value = err instanceof Error ? err.message : String(err)
  } finally {
    ttsInstalling.value = false
    ttsProgress.value = { stage: '', done: 0, total: null }
    await refreshTts()
  }
}

async function unloadTts() {
  await ttsUnload().catch(() => undefined)
  await refreshTts()
}

/** 试听用的独立上下文：不开麦克风、不碰通话引擎，用完让浏览器回收 */
let previewCtx: AudioContext | null = null

/**
 * 试听文本。
 *
 * 挑了一段同时包含数字、日期、中英混排的话——这三样是最容易读错的地方，
 * 只念「你好」听不出模型好坏。
 */
const VOICE_SAMPLE = '你好，我是 Shania。今天是 11 月 12 号，下午三点提醒你开会。'

async function previewVoice() {
  if (ttsTesting.value) return
  ttsTesting.value = true
  ttsError.value = ''
  ttsDiag.value = ''

  // 必须在用户手势里建上下文：`new AudioContext()` 建在手势外会被浏览器判成
  // 自动播放，状态停在 suspended，后面 resume() 也叫不醒——症状就是「点了试听，
  // 一句合成完成了但完全没声音，也不报错」。所以先建再 await 合成。
  const ctx = previewCtx ?? new AudioContext()
  previewCtx = ctx
  if (ctx.state === 'suspended') await ctx.resume().catch(() => undefined)

  try {
    const speech = await ttsSay(VOICE_SAMPLE, settingsStore.settings.ttsSpeed)

    const binary = atob(speech.pcm)
    const bytes = new Uint8Array(binary.length)
    for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i)
    const pcm = new Int16Array(bytes.buffer, 0, bytes.length >> 1)
    const samples = new Float32Array(pcm.length)
    let peak = 0
    for (let i = 0; i < pcm.length; i += 1) {
      samples[i] = pcm[i] / 32768
      peak = Math.max(peak, Math.abs(samples[i]))
    }

    const buffer = ctx.createBuffer(1, samples.length, speech.rate)
    buffer.getChannelData(0).set(samples)
    const source = ctx.createBufferSource()
    source.buffer = buffer
    source.connect(ctx.destination)
    source.start()

    // 一行自检信息：声音不对时不用猜是「没合成出来」还是「没放出来」
    ttsDiag.value =
      `${(speech.durationMs / 1000).toFixed(1)}s @${speech.rate}Hz，峰值 ${peak.toFixed(2)}，` +
      `上下文 ${ctx.state} ${ctx.sampleRate}Hz`

    // 模型刚加载完，采样率这时才拿得到
    await refreshTts()
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err)
    ttsError.value = msg.includes('TTS_NOT_INSTALLED')
      ? '语音合成模型还没下载，先点上面的「下载」'
      : `试听失败：${msg}`
  } finally {
    ttsTesting.value = false
  }
}

onMounted(async () => {
  window.addEventListener('keydown', onKeydown)
  autoStart.value = await isAutoStartEnabled().catch(() => false)
  await refreshStt()
  await refreshTts()
  if (isTauri) {
    unlistenStt = await listen<{ stage: string; done: number; total: number | null }>(
      'stt-progress',
      (event) => {
        // 两个模型共用一个事件通道，靠 stage 分派，别让 TTS 的进度刷掉 STT 的
        if (event.payload.stage === 'tts') {
          ttsProgress.value = event.payload
        } else {
          sttProgress.value = event.payload
        }
      },
    )
  }
})

onUnmounted(() => {
  window.removeEventListener('keydown', onKeydown)
  if (clearTimer) clearTimeout(clearTimer)
  if (resetTimer) clearTimeout(resetTimer)
  unlistenStt?.()
  // 试听上下文挂在模块级，页面销毁时要收掉，否则音频设备一直被占着
  void previewCtx?.close().catch(() => undefined)
  previewCtx = null
})
</script>

<template>
  <div class="page">
    <aside class="nav">
      <div class="nav-top" data-tauri-drag-region>
        <WindowControls />
      </div>

      <div class="nav-title">设置</div>

      <nav class="nav-list">
        <button
          v-for="m in modules"
          :key="m.id"
          class="nav-item"
          :class="{ on: active === m.id }"
          @click="active = m.id"
        >
          <Icon :name="m.icon" :size="14" />
          <span>{{ m.label }}</span>
          <i v-if="dots[m.id]" class="dot" />
        </button>
      </nav>

      <div class="nav-foot">版本 {{ APP_VERSION }}</div>
    </aside>

    <main class="body">
      <header class="head" data-tauri-drag-region>
        <!-- 和下面的 .wrap 用同一组宽度限制，标题才能和表单左边界对齐 -->
        <div class="head-inner">
          <div class="head-text">
            <h2>{{ current.label }}</h2>
            <p>{{ current.desc }}</p>
          </div>
          <button class="done" title="返回会话（Esc）" @click="close">
            完成
            <span class="kbd">Esc</span>
          </button>
        </div>
      </header>

      <div class="pane">
        <div class="wrap">
          <!-- ============================ 通用 ============================ -->
          <template v-if="active === 'general'">
            <div class="group">
              <div class="row">
                <span class="row-label">外观</span>
                <div class="row-control">
                  <div class="segmented">
                    <button
                      v-for="t in themes"
                      :key="t.value"
                      :class="{ on: settingsStore.settings.theme === t.value }"
                      @click="settingsStore.settings.theme = t.value"
                    >
                      <Icon :name="t.icon" :size="12" />
                      <span>{{ t.label }}</span>
                    </button>
                  </div>
                </div>
              </div>

              <div class="row last">
                <span class="row-label">显示名称</span>
                <div class="row-control">
                  <input
                    v-model.trim="settingsStore.settings.displayName"
                    class="text-input"
                    type="text"
                    placeholder="Shania"
                  />
                </div>
              </div>
            </div>

            <div class="group-title">启动与窗口</div>
            <div class="group">
              <div class="row">
                <div class="row-text">
                  <span class="row-label">唤起快捷键</span>
                  <span class="row-sub" :class="{ warn: !!shortcutError }">
                    {{
                      shortcutError ||
                      (recording ? '按下组合键，Esc 取消' : '在任意窗口按下可唤出/收起')
                    }}
                  </span>
                </div>
                <div class="row-control">
                  <button
                    class="hotkey"
                    :class="{ recording, error: !!shortcutError }"
                    @click="startCapture"
                    @keydown="onHotkeyKeydown"
                    @blur="cancelCapture"
                  >
                    {{ recording ? '请按下组合键…' : shortcutText }}
                  </button>
                </div>
              </div>

              <div class="row">
                <div class="row-text">
                  <span class="row-label">关闭按钮</span>
                  <span class="row-sub">
                    {{
                      settingsStore.settings.closeAction === 'tray'
                        ? '收起到托盘，进程继续驻留'
                        : '直接退出应用'
                    }}
                  </span>
                </div>
                <div class="row-control">
                  <div class="segmented">
                    <button
                      :class="{ on: settingsStore.settings.closeAction === 'tray' }"
                      @click="changeCloseAction('tray')"
                    >
                      收起到托盘
                    </button>
                    <button
                      :class="{ on: settingsStore.settings.closeAction === 'quit' }"
                      @click="changeCloseAction('quit')"
                    >
                      退出
                    </button>
                  </div>
                </div>
              </div>

              <div class="row last">
                <div class="row-text">
                  <span class="row-label">开机自启</span>
                  <span class="row-sub">登录系统后在后台启动</span>
                </div>
                <button
                  class="switch"
                  :class="{ on: autoStart }"
                  role="switch"
                  :aria-checked="autoStart"
                  @click="toggleAutoStart"
                >
                  <span class="knob" />
                </button>
              </div>
            </div>

            <p class="foot-note">
              设置保存在 <code>{{ settingsStore.filePath || '~/.shania/settings.json' }}</code
              >，命令行工具 <code>shania</code> 读同一份文件。
            </p>
          </template>

          <!-- ============================ 模型 ============================ -->
          <template v-else-if="active === 'model'">
            <div class="group">
              <div class="field">
                <label for="baseUrl">接口地址</label>
                <input
                  id="baseUrl"
                  v-model.trim="settingsStore.settings.baseUrl"
                  type="text"
                  spellcheck="false"
                  placeholder="http://127.0.0.1:4000"
                />
                <p class="help">OpenAI 兼容端点，例如 LiteLLM、vLLM、DeepSeek、通义千问。</p>
              </div>

              <div class="field">
                <label for="apiKey">API Key</label>
                <div class="secret">
                  <input
                    id="apiKey"
                    v-model.trim="settingsStore.settings.apiKey"
                    :type="showKey ? 'text' : 'password'"
                    spellcheck="false"
                    placeholder="sk-…（本地服务可留空）"
                  />
                  <button class="eye" type="button" @click="showKey = !showKey">
                    <Icon :name="showKey ? 'eyeOff' : 'eye'" :size="14" />
                  </button>
                </div>
                <p class="help">仅保存在本机，不会上传到任何第三方。</p>
              </div>

              <div class="field last">
                <label for="model">模型名称</label>
                <input
                  id="model"
                  v-model.trim="settingsStore.settings.model"
                  type="text"
                  spellcheck="false"
                  placeholder="qwen3-vl-8b"
                />
              </div>
            </div>

            <div class="group-title">快速配置</div>
            <div class="group">
              <div v-for="p in settingsStore.settings.profiles" :key="p.id" class="row">
                <div class="row-text">
                  <span class="row-label">{{ p.name }}</span>
                  <span class="row-sub">{{ p.model }} · {{ p.baseUrl }}</span>
                </div>
                <div class="row-control">
                  <button
                    class="mini-btn"
                    :class="{ primary: p.id === activeProfileId }"
                    :disabled="p.id === activeProfileId"
                    @click="settingsStore.applyProfile(p.id)"
                  >
                    {{ p.id === activeProfileId ? '使用中' : '启用' }}
                  </button>
                  <button
                    class="mini-btn"
                    title="删除这套配置"
                    @click="settingsStore.removeProfile(p.id)"
                  >
                    <Icon name="trash" :size="11" />
                  </button>
                </div>
              </div>

              <div class="row last">
                <input
                  v-model="profileName"
                  class="text-input grow"
                  type="text"
                  spellcheck="false"
                  placeholder="给当前参数起个名字，如「本地 Qwen」"
                  @keydown.enter="saveProfile"
                />
                <button class="mini-btn primary" :disabled="!profileName.trim()" @click="saveProfile">
                  保存当前参数
                </button>
              </div>
            </div>

            <div class="group-title">连通性</div>
            <div class="group">
              <div class="row last">
                <div class="row-text">
                  <span class="row-label">测试连接</span>
                  <span
                    class="row-sub"
                    :class="{ ok: testOk, warn: !!testText && !testOk && !testing }"
                    :title="testText"
                  >
                    {{ testText || '保存前先测一下，能省掉很多排查时间' }}
                  </span>
                </div>
                <button class="mini-btn" :disabled="testing" @click="runTest">
                  {{ testing ? '测试中…' : '测试连接' }}
                </button>
              </div>
            </div>

            <div class="group-title">生成参数</div>
            <div class="group">
              <div class="row">
                <span class="row-label">温度</span>
                <div class="row-control slider">
                  <input
                    v-model.number="settingsStore.settings.temperature"
                    type="range"
                    min="0"
                    max="1.5"
                    step="0.05"
                  />
                  <span class="num">{{ settingsStore.settings.temperature.toFixed(2) }}</span>
                </div>
              </div>
              <div class="row">
                <span class="row-label">最大输出 Token</span>
                <div class="row-control">
                  <input
                    v-model.number="settingsStore.settings.maxTokens"
                    class="num-input"
                    type="number"
                    min="128"
                    max="32768"
                    step="128"
                  />
                </div>
              </div>
              <div class="row last">
                <div class="row-text">
                  <span class="row-label">历史预算</span>
                  <span class="row-sub">超出后自动丢弃较早消息，避免上下文超限</span>
                </div>
                <div class="row-control">
                  <input
                    v-model.number="settingsStore.settings.contextLimit"
                    class="num-input"
                    type="number"
                    min="2000"
                    max="200000"
                    step="1000"
                  />
                </div>
              </div>
            </div>
          </template>

          <!-- ============================ 人设 ============================ -->
          <template v-else-if="active === 'persona'">
            <div class="group">
              <div class="field last">
                <div class="field-head">
                  <label for="sysPrompt">系统提示词</label>
                  <span class="counter">{{ settingsStore.settings.systemPrompt.length }} 字</span>
                </div>
                <textarea
                  id="sysPrompt"
                  v-model="settingsStore.settings.systemPrompt"
                  class="prompt"
                  rows="12"
                  spellcheck="false"
                  placeholder="描述你希望它扮演的角色与行为方式…"
                />
                <p class="help">
                  每轮对话都会带上这段文字。写得具体比写得长有用：说清「回答多长、用什么语气、
                  遇到不确定的事怎么办」，比堆一堆形容词有效。
                </p>
              </div>
            </div>
          </template>

          <!-- ============================ 语音 ============================ -->
          <template v-else-if="active === 'voice'">
            <div class="group-title">语音输入</div>
            <div class="group">
              <div class="field">
                <div class="stt-head">
                  <span>识别模型</span>
                  <span class="stt-state" :class="{ ok: streamReady, warn: !!stt && !streamReady }">
                    {{ streamStateText }}
                  </span>
                </div>
                <div class="stt-actions">
                  <button
                    v-if="!streamReady"
                    class="mini-btn primary"
                    :disabled="sttInstalling"
                    @click="installStream"
                  >
                    {{ sttInstalling ? '下载中…' : '下载模型（约 134MB）' }}
                  </button>
                  <button
                    v-else-if="stt?.loaded"
                    class="mini-btn"
                    title="释放内存，下次录音时再重新加载"
                    @click="unloadStream"
                  >
                    卸载模型
                  </button>
                  <span v-if="stt?.dir" class="stt-dir" :title="stt.dir">~/.shania/models/…</span>
                </div>
                <div v-if="sttInstalling" class="stt-progress">
                  <div class="stt-bar"><i :style="{ width: `${sttPercent}%` }" /></div>
                  <span>{{ sttStageText }} {{ sttPercent }}%</span>
                </div>
                <p v-if="sttError" class="stt-error">{{ sttError }}</p>
                <p class="help">
                  用 <code>X-ASR 160ms</code>（中英双语，模型自带标点，int8 量化 134MB）。
                  识别在<strong>本进程内</strong>完成：<strong>不监听端口、不需要服务地址、没有独立进程要管</strong>。
                  说话时字跟着往外冒，首字约 0.4 秒，停 1 秒左右自动定稿。
                </p>
              </div>

              <div class="field last">
                <div class="stt-preload">
                  <div class="row-text">
                    <span class="row-label">启动时预加载</span>
                    <span class="row-sub">{{ preloadHint }}</span>
                  </div>
                  <button
                    class="switch"
                    :class="{ on: settingsStore.settings.sttPreload }"
                    role="switch"
                    :aria-checked="settingsStore.settings.sttPreload"
                    @click="toggleSttPreload"
                  >
                    <span class="knob" />
                  </button>
                </div>
              </div>
            </div>

            <div class="group-title">语音输出</div>
            <div class="group">
              <div class="field">
                <div class="stt-head">
                  <span>MeloTTS zh_en</span>
                  <span class="stt-state" :class="{ ok: ttsReady, warn: !!tts && !ttsReady }">
                    {{ ttsStateText }}
                  </span>
                </div>

                <div class="stt-actions">
                  <button
                    v-if="!ttsReady"
                    class="mini-btn primary"
                    :disabled="ttsInstalling"
                    @click="installTts"
                  >
                    {{ ttsInstalling ? '下载中…' : '下载（约 163MB）' }}
                  </button>
                  <button v-else class="mini-btn" :disabled="ttsTesting" @click="previewVoice">
                    {{ ttsTesting ? '合成中…' : '试听' }}
                  </button>
                  <button
                    v-if="tts?.loaded"
                    class="mini-btn"
                    title="释放约 400MB 内存，下次合成时再重新加载"
                    @click="unloadTts"
                  >
                    卸载内存中的模型
                  </button>
                  <span v-if="tts?.dir" class="stt-dir" :title="tts.dir">~/.shania/models/…</span>
                </div>

                <div v-if="ttsInstalling" class="stt-progress">
                  <div class="stt-bar"><i :style="{ width: `${ttsPercent}%` }" /></div>
                  <span>下载中 {{ ttsPercent }}%</span>
                </div>

                <p v-if="ttsDiag" class="help">{{ ttsDiag }}</p>
                <p v-if="ttsError" class="stt-error">{{ ttsError }}</p>
                <p class="help">
                  进程内 ONNX 合成：不联网、不占显存、不需要单独的服务进程。
                  中英混读，44.1kHz——词表外的英文缩写按字母念（API → A-P-I），
                  不会像纯中文模型那样整词删掉。
                  <br />
                  实测 20 字中英混排约 1.3 秒合成，通话首声约 1.3 秒。
                </p>
              </div>

              <div class="field last">
                <div class="stt-head">
                  <span>语速</span>
                  <span class="stt-state">{{ settingsStore.settings.ttsSpeed.toFixed(2) }}×</span>
                </div>
                <input
                  v-model.number="settingsStore.settings.ttsSpeed"
                  class="wide-slider"
                  type="range"
                  min="0.6"
                  max="1.6"
                  step="0.05"
                />
                <p class="help">通话建议 1.0。超过 1.3 会明显变调、发闷。</p>
              </div>
            </div>
          </template>

          <!-- ============================ 工具 ============================ -->
          <template v-else-if="active === 'tools'">
            <div class="group">
              <div class="row last">
                <div class="row-text">
                  <span class="row-label">工具调用</span>
                  <span class="row-sub">允许它读取时间、系统信息与本机文件；关掉后只能凭已有知识回答</span>
                </div>
                <button
                  class="switch"
                  :class="{ on: settingsStore.settings.toolsEnabled }"
                  role="switch"
                  :aria-checked="settingsStore.settings.toolsEnabled"
                  @click="settingsStore.settings.toolsEnabled = !settingsStore.settings.toolsEnabled"
                >
                  <span class="knob" />
                </button>
              </div>
            </div>

            <div class="group-title">内置工具</div>
            <div class="group">
              <div class="row">
                <div class="row-text">
                  <span class="row-label">get_current_time</span>
                  <span class="row-sub">当前时间与时区——模型自己不知道「今天」是几号</span>
                </div>
              </div>
              <div class="row">
                <div class="row-text">
                  <span class="row-label">get_system_info</span>
                  <span class="row-sub">操作系统、CPU、内存、磁盘占用</span>
                </div>
              </div>
              <div class="row">
                <div class="row-text">
                  <span class="row-label">read_text_file</span>
                  <span class="row-sub">读文本文件正文，单次有长度上限</span>
                </div>
              </div>
              <div class="row last">
                <div class="row-text">
                  <span class="row-label">list_directory</span>
                  <span class="row-sub">列目录，用来先看结构再决定读哪个文件</span>
                </div>
              </div>
            </div>

            <p class="foot-note">
              外部工具（MCP）按需配置 <code>~/.shania/mcp.json</code>，随每轮对话启停；
              管理界面还在做，暂时只能手改文件。
            </p>
          </template>

          <!-- ========================== 数据与关于 ========================== -->
          <template v-else>
            <div class="group-title">数据</div>
            <div class="group">
              <div class="row">
                <div class="row-text">
                  <span class="row-label">清空全部会话</span>
                  <span class="row-sub">删除本机保存的所有对话，不可恢复</span>
                </div>
                <button
                  class="mini-btn"
                  :class="{ danger: confirmingClear }"
                  :disabled="chat.streaming"
                  @click="clearAllSessions"
                >
                  {{ confirmingClear ? '再点一次确认' : '清空' }}
                </button>
              </div>

              <div class="row last">
                <div class="row-text">
                  <span class="row-label">日志目录</span>
                  <span class="row-sub">出问题先看这里：shania.log 是主日志</span>
                </div>
                <div class="row-control">
                  <button class="mini-btn" @click="openLogs">打开目录</button>
                </div>
              </div>
            </div>

            <p class="foot-note">
              设置保存在 <code>{{ settingsStore.filePath || '~/.shania/settings.json' }}</code
              >，对话历史在 <code>~/.shania/sessions/</code>，卸载或清理后不可恢复。
            </p>

            <div class="group-title">关于</div>
            <div class="group">
              <div class="row">
                <div class="row-text">
                  <span class="row-label">版本</span>
                  <span class="row-sub">{{ APP_VERSION }} · Shania Agent</span>
                </div>
                <div class="row-control">
                  <button
                    class="mini-btn"
                    :disabled="updatePhase === 'checking' || updatePhase === 'installing'"
                    @click="checkUpdate"
                  >
                    {{ updatePhase === 'checking' ? '检查中…' : '检查更新' }}
                  </button>
                </div>
              </div>

              <div v-if="updateMessage" class="row last update-row">
                <span class="row-sub" :class="{ warn: updatePhase === 'error' }">
                  {{ updateMessage }}
                </span>
                <div class="row-control">
                  <button
                    v-if="updatePhase === 'available'"
                    class="mini-btn primary"
                    @click="doInstall"
                  >
                    下载并安装
                  </button>
                </div>
              </div>

              <div v-if="updatePhase === 'installing'" class="row last">
                <div class="stt-bar"><i :style="{ width: `${updateProgress}%` }" /></div>
              </div>
            </div>

            <div class="group-title">危险操作</div>
            <div class="group">
              <div class="row last">
                <div class="row-text">
                  <span class="row-label">恢复默认设置</span>
                  <span class="row-sub">接口、模型、提示词、快捷键全部回到出厂值；会话不受影响</span>
                </div>
                <button
                  class="mini-btn"
                  :class="{ danger: confirmingReset }"
                  @click="resetAll"
                >
                  {{ confirmingReset ? '再点一次确认' : '恢复默认' }}
                </button>
              </div>
            </div>
          </template>
        </div>
      </div>
    </main>
  </div>
</template>

<style scoped>
.page {
  display: flex;
  /* 作为 .shell 的 flex 子项：不能用 width:100%。父容器里还有被 v-show 隐藏的
     会话栏，100% 会按容器宽度撑出来，把页面顶出视口、右边露出一条会话页 */
  flex: 1;
  min-width: 0;
  height: 100vh;
  overflow: hidden;
  background: var(--bg);

  /* 内容列宽：窄窗口按 720 走（再宽行内标签和控件就隔太远），窗口放开后
     分档放宽，否则最大化时 720 会变成一条窄带飘在大片空白中间 */
  --col: 720px;
}
@media (min-width: 1280px) {
  .page {
    --col: 880px;
  }
}
@media (min-width: 1600px) {
  .page {
    --col: 1000px;
  }
}

/* ---------- 左侧导航 ---------- */
.nav {
  width: 208px;
  flex: none;
  display: flex;
  flex-direction: column;
  background: var(--sidebar);
  border-right: 1px solid var(--line-soft);
  user-select: none;
}

.nav-top {
  height: 52px;
  flex: none;
  display: flex;
  align-items: center;
  padding: 0 18px;
}

.nav-title {
  padding: 0 20px 10px;
  font-size: 15px;
  font-weight: 600;
  letter-spacing: -0.01em;
  color: var(--text);
}

.nav-list {
  flex: 1;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 1px;
  padding: 0 10px 10px;
}

.nav-item {
  display: flex;
  align-items: center;
  gap: 9px;
  height: 34px;
  padding: 0 10px;
  border-radius: 8px;
  font-size: 12.5px;
  color: var(--text-2);
  text-align: left;
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.nav-item:hover {
  background: var(--hover);
  color: var(--text);
}
.nav-item.on {
  background: var(--active);
  color: var(--text);
  font-weight: 500;
}

.dot {
  margin-left: auto;
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--amber);
  flex: none;
}

.nav-foot {
  flex: none;
  padding: 10px 20px 14px;
  font-size: 10.5px;
  color: var(--text-3);
}

/* ---------- 右侧内容 ---------- */
.body {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  background: var(--content);
}

.head {
  flex: none;
  padding: 20px 30px 14px;
  border-bottom: 1px solid var(--line-soft);
  user-select: none;
}

.head-inner {
  display: flex;
  align-items: flex-start;
  gap: 16px;
  /* 与 .wrap 同宽：标题、「完成」按钮、下面的表单落在同一条中轴上 */
  max-width: var(--col);
  margin: 0 auto;
}

.head-text {
  min-width: 0;
  flex: 1;
}

.head h2 {
  margin: 0;
  font-size: 16px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.head p {
  margin: 3px 0 0;
  font-size: 11.5px;
  color: var(--text-3);
}

.done {
  flex: none;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 28px;
  padding: 0 12px;
  border-radius: 8px;
  background: var(--accent);
  color: #fff;
  font-size: 12px;
  font-weight: 500;
  transition: background 0.14s var(--ease);
}
.done:hover {
  background: var(--accent-hover);
}
.kbd {
  font-size: 9.5px;
  padding: 1px 4px;
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.22);
  letter-spacing: 0.02em;
}

.pane {
  flex: 1;
  overflow-y: auto;
  padding: 18px 30px 40px;
}

/* 内容不铺满：一行拉到 1200px 宽时，标签和控件会隔得老远，扫视很累。
   限宽之后居中而不是靠左——窗口一宽，靠左会让右侧空出一大片死白 */
.wrap {
  max-width: var(--col);
  margin: 0 auto;
}

/* ---------- 分组 ---------- */
.group {
  background: var(--surface-2);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius-md);
  overflow: hidden;
}

.group + .group {
  margin-top: 8px;
}

.group-title {
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--text-3);
  padding: 18px 2px 7px;
}
.group-title:first-child {
  padding-top: 0;
}

/* ---------- 字段 ---------- */
.field {
  padding: 10px 13px;
  border-bottom: 1px solid var(--line-soft);
}
.field.last,
.row.last {
  border-bottom: none;
}

.field-head {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin-bottom: 5px;
}
.field-head label {
  margin: 0;
}
.counter {
  margin-left: auto;
  font-size: 10.5px;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}

.field label {
  display: block;
  font-size: 12px;
  font-weight: 500;
  color: var(--text-2);
  margin-bottom: 5px;
}

.field input,
.field textarea {
  display: block;
  width: 100%;
  padding: 7px 10px;
  border-radius: 8px;
  border: 1px solid var(--line);
  background: var(--surface);
  font-size: 12.5px;
  outline: none;
  transition: border-color 0.16s var(--ease), box-shadow 0.16s var(--ease);
}
.field textarea {
  resize: vertical;
  min-height: 78px;
  line-height: 1.6;
  font-family: inherit;
}
.field textarea.prompt {
  min-height: 240px;
  line-height: 1.7;
}
.field input:focus,
.field textarea:focus {
  border-color: color-mix(in srgb, var(--accent) 55%, var(--line));
  box-shadow: 0 0 0 3px var(--accent-soft);
}

.field input::placeholder,
.field textarea::placeholder {
  color: var(--text-3);
}

.help {
  margin: 5px 0 0;
  font-size: 10.5px;
  line-height: 1.55;
  color: var(--text-3);
}

.secret {
  position: relative;
}
.secret input {
  padding-right: 34px;
}
.eye {
  position: absolute;
  right: 5px;
  top: 50%;
  transform: translateY(-50%);
  width: 24px;
  height: 24px;
  border-radius: 6px;
  display: grid;
  place-items: center;
  color: var(--text-3);
}
.eye:hover {
  background: var(--hover);
  color: var(--text-2);
}

/* ---------- 行 ---------- */
.row {
  display: flex;
  align-items: center;
  gap: 12px;
  min-height: 46px;
  padding: 9px 13px;
  border-bottom: 1px solid var(--line-soft);
}

.row-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
  flex: 1;
}

.row-label {
  font-size: 12.5px;
  color: var(--text);
}

.row-sub {
  font-size: 10.5px;
  color: var(--text-3);
  line-height: 1.4;
  overflow: hidden;
  text-overflow: ellipsis;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
}
.row-sub.ok {
  color: var(--green);
}
.row-sub.warn {
  color: var(--red);
}

.row-control {
  margin-left: auto;
  display: flex;
  align-items: center;
  gap: 7px;
  flex: none;
}

/* ---------- 语音相关 ---------- */
.stt-head {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 12.5px;
  color: var(--text);
  margin-bottom: 8px;
}
.stt-preload {
  display: flex;
  align-items: center;
  gap: 12px;
}
.stt-state {
  font-size: 11.5px;
  color: var(--text-3);
}
.stt-state.ok {
  color: var(--green);
}
.stt-state.warn {
  color: var(--amber);
}
.stt-actions {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.stt-dir {
  font-size: 11px;
  color: var(--text-3);
  font-family: ui-monospace, monospace;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 260px;
}
.stt-progress {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 8px;
  font-size: 11.5px;
  color: var(--text-2);
  font-variant-numeric: tabular-nums;
}
.stt-bar {
  flex: 1;
  height: 4px;
  border-radius: 2px;
  background: var(--line);
  overflow: hidden;
}
.stt-bar i {
  display: block;
  height: 100%;
  background: var(--accent);
  transition: width 0.2s linear;
}
.stt-error {
  margin: 8px 0 0;
  font-size: 11.5px;
  color: var(--red);
  word-break: break-all;
}

.wide-slider {
  width: 100%;
  accent-color: var(--accent);
}

.slider input[type='range'] {
  width: 132px;
  accent-color: var(--accent);
}

.num {
  width: 34px;
  font-size: 11.5px;
  color: var(--text-2);
  font-variant-numeric: tabular-nums;
  text-align: right;
}

.num-input,
.text-input {
  width: 94px;
  height: 26px;
  padding: 0 8px;
  border-radius: 7px;
  border: 1px solid var(--line);
  background: var(--surface);
  font-size: 12px;
  text-align: right;
  outline: none;
}
.text-input {
  width: 120px;
  text-align: left;
}
.text-input.grow {
  width: auto;
  flex: 1;
  text-align: left;
}
.num-input:focus,
.text-input:focus {
  border-color: color-mix(in srgb, var(--accent) 55%, var(--line));
  box-shadow: 0 0 0 3px var(--accent-soft);
}

.mini-btn {
  height: 26px;
  flex: none;
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 0 10px;
  border-radius: 7px;
  border: 1px solid var(--line);
  background: var(--surface);
  font-size: 11.5px;
  color: var(--text-2);
  transition: background 0.14s var(--ease), color 0.14s var(--ease),
    border-color 0.14s var(--ease);
}
.mini-btn:hover:not(:disabled) {
  background: var(--surface-2);
  color: var(--text);
  border-color: color-mix(in srgb, var(--accent) 32%, var(--line));
}
.mini-btn:disabled {
  opacity: 0.45;
  cursor: default;
}
.mini-btn.primary {
  background: var(--accent);
  border-color: transparent;
  color: #fff;
}
.mini-btn.primary:hover:not(:disabled) {
  background: var(--accent-hover);
  color: #fff;
}
.mini-btn.danger {
  background: var(--red);
  border-color: transparent;
  color: #fff;
}
.mini-btn.danger:hover {
  background: var(--red);
  color: #fff;
  filter: brightness(1.06);
}

.update-row {
  background: var(--fill);
}

/* ---------- 快捷键录制 ---------- */
.hotkey {
  min-width: 120px;
  height: 26px;
  padding: 0 10px;
  border-radius: 7px;
  border: 1px solid var(--line);
  background: var(--surface);
  font-size: 12px;
  color: var(--text);
  white-space: nowrap;
  font-variant-numeric: tabular-nums;
  transition: border-color 0.16s var(--ease), box-shadow 0.16s var(--ease),
    color 0.16s var(--ease);
}
.hotkey:hover {
  border-color: color-mix(in srgb, var(--accent) 40%, var(--line));
}
.hotkey:focus {
  outline: none;
  border-color: color-mix(in srgb, var(--accent) 55%, var(--line));
  box-shadow: 0 0 0 3px var(--accent-soft);
}
.hotkey.recording {
  color: var(--accent);
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-soft);
}
.hotkey.error {
  border-color: var(--red);
}

/* ---------- 分段控件 ---------- */
.segmented {
  display: flex;
  gap: 2px;
  padding: 2px;
  border-radius: 8px;
  background: var(--fill);
}

.segmented button {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 24px;
  padding: 0 9px;
  border-radius: 6px;
  font-size: 11.5px;
  color: var(--text-2);
  transition: background 0.16s var(--ease), color 0.16s var(--ease), box-shadow 0.16s var(--ease);
}
.segmented button.on {
  background: var(--surface);
  color: var(--text);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.12);
}

/* ---------- 开关 ---------- */
.switch {
  margin-left: auto;
  width: 38px;
  height: 22px;
  flex: none;
  border-radius: 11px;
  background: var(--fill-strong);
  padding: 2px;
  transition: background 0.2s var(--ease);
}
.switch.on {
  background: var(--green);
}
.knob {
  display: block;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: #fff;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.25), 0 0 0 0.5px rgba(0, 0, 0, 0.05);
  transition: transform 0.2s var(--ease);
}
.switch.on .knob {
  transform: translateX(16px);
}

.foot-note {
  margin: 10px 2px 0;
  font-size: 10.5px;
  color: var(--text-3);
  line-height: 1.6;
}

.foot-note code,
.help code {
  font-family: ui-monospace, monospace;
  font-size: 10px;
  padding: 1px 4px;
  border-radius: 4px;
  background: var(--fill);
}
</style>
