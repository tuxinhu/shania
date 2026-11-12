<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import Icon from './Icon.vue'
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
  ttsWarmup,
  type StreamStatus,
  type TtsStatus,
} from '@/lib/api'
import { acceleratorFromEvent, formatShortcut } from '@/lib/shortcut'
import { useChatStore } from '@/stores/chat'
import { useSettingsStore } from '@/stores/settings'

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ (e: 'update:open', value: boolean): void }>()

const settingsStore = useSettingsStore()
const chat = useChatStore()
const showKey = ref(false)

const APP_VERSION = '0.1.0'

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

function onProfileChange(event: Event) {
  const id = (event.target as HTMLSelectElement).value
  if (id) settingsStore.applyProfile(id)
  else settingsStore.settings.activeProfileId = ''
}

function saveProfile() {
  const name = profileName.value.trim()
  if (!name) return
  settingsStore.saveCurrentAsProfile(name)
  profileName.value = ''
}

function removeActiveProfile() {
  if (!activeProfileId.value) return
  settingsStore.removeProfile(activeProfileId.value)
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
    testText.value = `开机自启设置失败：${String(err)}`
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
const updateInfo = ref<{ version: string; notes: string } | null>(null)

async function checkUpdate() {
  if (updatePhase.value === 'checking' || updatePhase.value === 'installing') return
  updatePhase.value = 'checking'
  updateMessage.value = '正在检查…'
  const result = await checkForUpdate()
  if (result.status === 'available') {
    updatePhase.value = 'available'
    updateInfo.value = { version: result.version ?? '', notes: result.notes ?? '' }
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

/* ---------- 清空数据 ---------- */
const confirmingClear = ref(false)
let clearTimer: ReturnType<typeof setTimeout> | null = null

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

/* ---------- 面板开关 ---------- */
function close() {
  emit('update:open', false)
}

function onKeydown(e: KeyboardEvent) {
  if (!props.open || recording.value) return
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

async function resetAll() {
  settingsStore.reset()
  shortcutError.value = ''
  try {
    await applyGlobalShortcut(settingsStore.settings.shortcut)
    await setCloseAction(settingsStore.settings.closeAction)
  } catch (err) {
    shortcutError.value = String(err)
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

async function toggleSttPreload() {
  settingsStore.settings.sttPreload = !settingsStore.settings.sttPreload
}

/* ---------- 语音输出 ---------- */

const tts = ref<TtsStatus | null>(null)
/** 正在下载的模型 id，空表示没有下载任务 */
const ttsInstalling = ref('')
/** 正在切换模型（Kokoro 要读 348MB，2-3 秒） */
const ttsSwitching = ref(false)
const ttsTesting = ref(false)
const ttsError = ref('')
const ttsProgress = ref<{ stage: string; done: number; total: number | null }>({
  stage: '',
  done: 0,
  total: null,
})

/** 设置里选的那个模型：auto 时显示后端实际挑中的 */
const chosenModel = computed(() => settingsStore.settings.ttsModel)
const activeKind = computed(() => tts.value?.activeKind ?? '')
const ttsReady = computed(() => !!tts.value?.ready)

const modelRow = (id: string) => tts.value?.models.find((m) => m.id === id)
const modelReady = (id: string) => !!modelRow(id)?.ready
const modelBytes = (id: string) => Math.round((modelRow(id)?.bytes ?? 0) / 1024 / 1024)

/** 音色总数只有加载模型之后才知道；matcha 只有 1 个 */
const speakerCount = computed(() => (activeKind.value === 'kokoro' ? (tts.value?.speakers ?? 0) : 0))

/**
 * 选了某个模型但它没装好——只看「装没装」，不看后端当前加载的是谁。
 *
 * 之前拿 `activeKind !== 设置值` 判断是错的：刚点完切换时后端还加载着旧模型
 * （或者设置还没落盘），两边永远不一致，于是不管选哪个都提示「没装好」。
 */
const fellBack = computed(() => {
  const want = chosenModel.value
  if (want === 'auto') return false
  return !modelReady(want)
})

const ttsPercent = computed(() => {
  const { done, total } = ttsProgress.value
  if (!total) return done > 0 ? 100 : 0
  return Math.min(100, Math.round((done / total) * 100))
})

const ttsStateText = computed(() => {
  if (!tts.value) return '浏览器预览环境不可用'
  if (!tts.value.ready) {
    // 一个模型都不完整：这时只能报总占用，用来提示「下了一半」
    const total = Math.round(tts.value.bytes / 1024 / 1024)
    return total > 0 ? `下载未完成（${total}MB）` : '未安装'
  }
  // 只报当前生效那个模型的体积：两个加起来（138+406MB）会让人以为程序很占地方
  const active = tts.value.models.find((m) => m.active)
  const mb = Math.round((active?.bytes ?? 0) / 1024 / 1024)
  const name = activeKind.value === 'kokoro' ? 'Kokoro' : 'Matcha'
  return tts.value.loaded
    ? `运行中：${name}（${mb}MB，${tts.value.speakers} 音色）`
    : `将使用：${name}（${mb}MB），用到时加载`
})

async function refreshTts() {
  // 显式带当前设置：后端读 settings.json 有 250ms 防抖窗口，刚切换过来会读到旧值
  tts.value = await ttsStatus(settingsStore.settings.ttsModel).catch(() => null)
}

/**
 * 换模型。
 *
 * 已经加载进内存的旧模型必须丢掉：否则 `tts_say` 里那句「已加载且种类相同
 * 就直接用」会让切换看起来没生效，用户得重启程序才发现。
 */
// 已经加载进内存的旧模型必须丢掉：否则 `tts_say` 里那句「已加载且种类相同
// 就直接用」会让切换看起来没生效，用户得重启程序才发现。
async function switchModel(id: 'auto' | 'matcha' | 'kokoro') {
  if (settingsStore.settings.ttsModel === id || ttsSwitching.value) return
  settingsStore.settings.ttsModel = id
  ttsError.value = ''
  ttsSwitching.value = true
  try {
    await ttsUnload().catch(() => undefined)
    // Kokoro 有 103 个音色，只有加载完才知道数量——切过去时顺手读进来，
    // 否则下面的音色网格要等第一次合成完才出现
    if (id === 'kokoro' && modelReady('kokoro')) {
      await ttsWarmup('kokoro').catch(() => undefined)
    }
    await refreshTts()
  } finally {
    ttsSwitching.value = false
  }
}

async function installTts(kind: 'matcha' | 'kokoro') {
  if (ttsInstalling.value) return
  const firstOne = !tts.value?.ready
  ttsInstalling.value = kind
  ttsError.value = ''
  try {
    await ttsInstall(kind)
    // 之前一个模型都没有：下完直接指过去，免得「下完了还是没声音」
    if (firstOne) await switchModel(kind)
  } catch (err) {
    ttsError.value = err instanceof Error ? err.message : String(err)
  } finally {
    ttsInstalling.value = ''
    ttsProgress.value = { stage: '', done: 0, total: null }
    await refreshTts()
  }
}

async function unloadTts() {
  await ttsUnload().catch(() => undefined)
  await refreshTts()
}

/** 试听用的独立上下文：不开麦克风、不碰通话引擎，用完就让浏览器回收 */
let previewCtx: AudioContext | null = null

/**
 * 试听文本。
 *
 * 挑了一段同时包含数字、日期、中英混排的话——这三样是最容易读错的地方，
 * 只念「你好」听不出模型好坏。
 */
const VOICE_SAMPLE = '你好，我是 Shania。今天是 11 月 12 号，下午三点提醒你开会。'

/**
 * 试听某个音色（不传则用当前设置的编号）。
 *
 * 顺带承担「加载模型」的作用：`tts_say` 内部会先 start 再合成，
 * 所以第一次点试听会等 1-2 秒（Kokoro 更久），之后视模型而定。
 */
async function previewVoice(sid?: number) {
  // 编号先落：合成 1 秒多的过程中点了别的编号，选中高亮至少要跟着走，
  // 否则用户会以为点击没被接收
  if (sid !== undefined) settingsStore.settings.ttsSid = sid
  if (ttsTesting.value) return
  ttsTesting.value = true
  ttsError.value = ''

  // 「重听」按钮不传 sid，用当前选中的编号
  const voice = sid ?? settingsStore.settings.ttsSid

  try {
    const speech = await ttsSay(
      VOICE_SAMPLE,
      voice,
      settingsStore.settings.ttsSpeed,
      settingsStore.settings.ttsModel,
    )

    const binary = atob(speech.pcm)
    const bytes = new Uint8Array(binary.length)
    for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i)
    const pcm = new Int16Array(bytes.buffer, 0, bytes.length >> 1)
    const samples = new Float32Array(pcm.length)
    for (let i = 0; i < pcm.length; i += 1) samples[i] = pcm[i] / 32768

    const ctx = previewCtx ?? new AudioContext()
    previewCtx = ctx
    if (ctx.state === 'suspended') await ctx.resume()

    const buffer = ctx.createBuffer(1, samples.length, speech.rate)
    buffer.getChannelData(0).set(samples)
    const source = ctx.createBufferSource()
    source.buffer = buffer
    source.connect(ctx.destination)
    source.start()

    // 模型刚加载完，音色总数这时才拿得到
    await refreshTts()
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err)
    ttsError.value = msg.includes('TTS_NOT_INSTALLED')
      ? '语音合成模型还没下载，先点上面的「下载语音模型」'
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
  unlistenStt?.()
})
</script>

<template>
  <Teleport to="body">
    <Transition name="sheet">
      <div v-if="open" class="overlay" @click.self="close">
        <div class="sheet">
          <header class="sheet-head">
            <h2>设置</h2>
            <button class="close" title="关闭" @click="close">
              <Icon name="x" :size="14" />
            </button>
          </header>

          <div class="sheet-body">
            <!-- 模型服务 -->
            <section>
              <div class="section-title">模型服务</div>
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

              <!-- 多套配置 -->
              <div class="group sub-group">
                <div class="row">
                  <div class="row-text">
                    <span class="row-label">快速配置</span>
                    <span class="row-sub">保存多套接口，随时切换</span>
                  </div>
                  <div class="row-control">
                    <select
                      class="select"
                      :value="activeProfileId"
                      @change="onProfileChange"
                    >
                      <option value="">手动配置</option>
                      <option v-for="p in settingsStore.settings.profiles" :key="p.id" :value="p.id">
                        {{ p.name }}
                      </option>
                    </select>
                    <button
                      v-if="activeProfileId"
                      class="mini-btn"
                      title="删除这套配置"
                      @click="removeActiveProfile"
                    >
                      <Icon name="trash" :size="12" />
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
                    保存
                  </button>
                </div>
              </div>

              <!-- 连通性 -->
              <div class="group sub-group">
                <div class="row last">
                  <div class="row-text">
                    <span class="row-label">连通性</span>
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
            </section>

            <!-- 生成参数 -->
            <section>
              <div class="section-title">生成参数</div>
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
            </section>

            <!-- 语音输入 -->
            <section>
              <div class="section-title">语音输入</div>
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
            </section>

            <!-- 语音输出 -->
            <section>
              <div class="section-title">语音输出</div>
              <div class="group">
                <div class="field">
                  <div class="stt-head">
                    <span>合成模型</span>
                    <span class="stt-state" :class="{ ok: ttsReady, warn: !!tts && !ttsReady }">
                      {{ ttsStateText }}
                    </span>
                  </div>

                  <div class="tts-pick">
                    <button
                      class="tts-opt"
                      :class="{ on: chosenModel === 'auto' }"
                      :disabled="ttsSwitching"
                      @click="switchModel('auto')"
                    >
                      <span class="tts-opt-name">自动</span>
                      <span class="tts-opt-desc">装了哪个用哪个，Matcha 优先</span>
                    </button>
                    <button
                      class="tts-opt"
                      :class="{ on: chosenModel === 'matcha' }"
                      :disabled="ttsSwitching"
                      @click="switchModel('matcha')"
                    >
                      <span class="tts-opt-name">Matcha</span>
                      <span class="tts-opt-desc">快，单音色 · 中文最稳 · RTF 0.06</span>
                    </button>
                    <button
                      class="tts-opt"
                      :class="{ on: chosenModel === 'kokoro' }"
                      :disabled="ttsSwitching"
                      @click="switchModel('kokoro')"
                    >
                      <span class="tts-opt-name">Kokoro</span>
                      <span class="tts-opt-desc">慢，103 音色 · 中文有个别词会念错 · RTF 0.5</span>
                    </button>
                  </div>

                  <p v-if="chosenModel === 'kokoro'" class="tts-warn">
                    Kokoro 的中文词表覆盖不如 Matcha：像「好的」「我看了下」这类常用词不在词表里，
                    会被按拼音念成英文音（听起来像重复或念错）。要念中文通话，建议用 Matcha。
                  </p>

                  <p v-if="ttsSwitching" class="help">正在切模型，读模型文件要 2-3 秒…</p>

                  <p v-if="fellBack" class="tts-error">
                    选的是 {{ chosenModel === 'kokoro' ? 'Kokoro' : 'Matcha' }}，但它的模型文件不完整，
                    现在{{ ttsReady ? `用 ${activeKind === 'kokoro' ? 'Kokoro' : 'Matcha'} 顶着` : '完全没法合成' }}。
                    点下面那一行的「下载」补全后会自动切过去。
                  </p>
                </div>

                <!-- 两个模型的下载/卸载各自独立：换模型不该逼用户重下已有的 -->
                <div
                  v-for="m in [
                    { id: 'matcha', name: 'Matcha zh-baker', size: '约 126MB', note: '模型包 72MB + 声码器 54MB' },
                    { id: 'kokoro', name: 'Kokoro multi-lang v1.1', size: '约 348MB', note: '中英混排，103 个音色' },
                  ]"
                  :key="m.id"
                  class="field"
                >
                  <div class="stt-head">
                    <span>{{ m.name }}</span>
                    <span class="stt-state" :class="{ ok: modelReady(m.id) }">
                      {{
                        modelReady(m.id)
                          ? `${modelBytes(m.id)}MB · 已安装`
                          : ttsInstalling === m.id
                            ? '下载中…'
                            : '未安装'
                      }}
                    </span>
                  </div>

                  <div class="stt-actions">
                    <button
                      v-if="!modelReady(m.id)"
                      class="mini-btn primary"
                      :disabled="!!ttsInstalling"
                      @click="installTts(m.id as 'matcha' | 'kokoro')"
                    >
                      {{ ttsInstalling === m.id ? '下载中…' : `下载（${m.size}）` }}
                    </button>
                    <span class="row-sub" style="flex: 1">{{ m.note }}</span>
                  </div>

                  <div v-if="ttsInstalling === m.id" class="stt-progress">
                    <div class="stt-bar"><i :style="{ width: `${ttsPercent}%` }" /></div>
                    <span>下载中 {{ ttsPercent }}%</span>
                  </div>
                </div>

                <div class="field">
                  <div class="stt-actions">
                    <button
                      v-if="tts?.loaded"
                      class="mini-btn"
                      title="释放内存，下次合成时再重新加载"
                      @click="unloadTts"
                    >
                      卸载内存中的模型
                    </button>
                    <span v-if="tts?.dir" class="stt-dir" :title="tts.dir">~/.shania/models/…</span>
                  </div>
                  <p v-if="ttsError" class="stt-error">{{ ttsError }}</p>
                  <p class="help">
                    两个模型都在<strong>本进程内</strong>合成：不联网、不占显存、不需要单独的服务进程。
                    <br />
                    <strong>Matcha</strong> 实测 8 字 109ms，通话延迟预算里最省的一环，但它只有一个女声。
                    <br />
                    <strong>Kokoro</strong> 音色明显更自然，可是 RTF 是 0.5——一句 8 字要 960ms，
                    通话首声会拖到 1.8 秒。想听它念长文可以选，做通话不推荐。
                  </p>
                </div>

                <!-- Kokoro 有 103 个音色，滑块挨个拖 + 点试听要几十分钟，
                     这里直接铺成编号网格：点一下即选定并朗读 -->
                <div v-if="speakerCount > 0" class="field">
                  <div class="stt-head">
                    <span>音色</span>
                    <span class="stt-state">
                      {{ settingsStore.settings.ttsSid }} 号 · 共 {{ speakerCount }}
                    </span>
                  </div>
                  <div class="voice-grid">
                    <button
                      v-for="i in speakerCount"
                      :key="i - 1"
                      class="voice-dot"
                      :class="{ on: settingsStore.settings.ttsSid === i - 1 }"
                      :title="`${i - 1} 号`"
                      @click="previewVoice(i - 1)"
                    >
                      {{ i - 1 }}
                    </button>
                  </div>
                  <div class="tts-row">
                    <button
                      class="mini-btn"
                      :disabled="ttsTesting"
                      @click="previewVoice(settingsStore.settings.ttsSid)"
                    >
                      {{ ttsTesting ? '合成中…' : `重听 ${settingsStore.settings.ttsSid} 号` }}
                    </button>
                    <button
                      class="mini-btn"
                      :disabled="ttsTesting"
                      title="按顺序听下一个"
                      @click="previewVoice((settingsStore.settings.ttsSid + 1) % speakerCount)"
                    >
                      下一个
                    </button>
                    <span class="row-sub" style="flex: 1">
                      点编号即选定并朗读。103 个里约 55 个中文女声、45 个中文男声、3 个英文，
                      具体哪个编号对应哪种由模型打包顺序决定，点一下就能听出来。
                    </span>
                  </div>
                </div>

                <div v-else class="field">
                  <div class="stt-head">
                    <span>音色</span>
                    <span class="stt-state">
                      {{
                        !ttsReady
                          ? '需先下载模型'
                          : activeKind === 'kokoro'
                            ? '模型加载后可调'
                            : '单音色（女声）'
                      }}
                    </span>
                  </div>
                  <div class="tts-row">
                    <span class="row-sub" style="flex: 1">
                      现在用的是 Matcha，只有一个人声（Baker 数据集），没有可选的音色。
                      想要 103 个音色：把上面的「合成模型」改成 <strong>Kokoro</strong>，
                      下载完这里会变成编号网格，点一下就能试听。代价是 RTF 0.5，通话首声约 1.8 秒。
                    </span>
                    <button class="mini-btn" :disabled="!ttsReady || ttsTesting" @click="previewVoice()">
                      {{ ttsTesting ? '合成中…' : '试听' }}
                    </button>
                  </div>
                </div>

                <div class="field last">
                  <div class="stt-head">
                    <span>语速</span>
                    <span class="stt-state">{{ settingsStore.settings.ttsSpeed.toFixed(2) }}×</span>
                  </div>
                  <input
                    v-model.number="settingsStore.settings.ttsSpeed"
                    type="range"
                    min="0.6"
                    max="1.6"
                    step="0.05"
                  />
                  <p class="help">通话建议 1.0。超过 1.3 会明显变调、发闷。</p>
                </div>
              </div>
            </section>

            <!-- 人设 -->
            <section>
              <div class="section-title">系统提示词</div>
              <div class="group">
                <div class="field last">
                  <textarea
                    v-model="settingsStore.settings.systemPrompt"
                    rows="3"
                    spellcheck="false"
                    placeholder="描述你希望它扮演的角色与行为方式…"
                  />
                </div>
              </div>
            </section>

            <!-- 外观与行为 -->
            <section>
              <div class="section-title">外观与行为</div>
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

                <div class="row">
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

                <div class="row">
                  <div class="row-text">
                    <span class="row-label">工具调用</span>
                    <span class="row-sub">允许读取时间、系统信息与本机文件</span>
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
            </section>

            <!-- 数据与更新 -->
            <section>
              <div class="section-title">数据与更新</div>
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

                <div class="row">
                  <div class="row-text">
                    <span class="row-label">版本</span>
                    <span class="row-sub">{{ APP_VERSION }}</span>
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
              </div>
            </section>

            <p class="foot-note">
              设置保存在 <code>{{ settingsStore.filePath || '~/.shania/settings.json' }}</code>，命令行工具
              <code>shania</code> 读同一份文件。
            </p>
            <p class="foot-note">
              对话历史在 <code>~/.shania/sessions/</code>，卸载或清理后不可恢复。
            </p>
            <p class="foot-note">
              出问题时先看日志：<code>~/.shania/logs/</code>
              （<code>shania.log</code> 是主日志，<code>stt.log</code> 是语音服务输出）
              <button class="mini-btn" @click="openLogs">打开日志目录</button>
            </p>
          </div>

          <footer class="sheet-foot">
            <button class="btn ghost" @click="resetAll">恢复默认</button>
            <button class="btn primary" @click="close">完成</button>
          </footer>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  z-index: 100;
  display: grid;
  place-items: center;
  padding: 32px;
  background: rgba(0, 0, 0, 0.28);
  backdrop-filter: blur(6px);
  -webkit-backdrop-filter: blur(6px);
}

.sheet {
  width: 560px;
  max-width: 100%;
  max-height: 86vh;
  display: flex;
  flex-direction: column;
  border-radius: 18px;
  background: var(--content);
  border: 1px solid var(--line);
  box-shadow: var(--shadow-lg);
  overflow: hidden;
}

.sheet-head {
  flex: none;
  height: 50px;
  display: flex;
  align-items: center;
  padding: 0 16px 0 20px;
  border-bottom: 1px solid var(--line-soft);
}

.sheet-head h2 {
  margin: 0;
  font-size: 14px;
  font-weight: 600;
  letter-spacing: -0.01em;
}

.close {
  margin-left: auto;
  width: 26px;
  height: 26px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  background: var(--fill);
  color: var(--text-2);
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.close:hover {
  background: var(--fill-strong);
  color: var(--text);
}

.sheet-body {
  flex: 1;
  overflow-y: auto;
  padding: 16px 20px 16px;
}

section {
  margin-bottom: 18px;
}

.section-title {
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--text-3);
  padding: 0 2px 7px;
}

.group {
  background: var(--surface-2);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius-md);
  overflow: hidden;
}
.group + .group {
  margin-top: 8px;
}

.field {
  padding: 10px 13px;
  border-bottom: 1px solid var(--line-soft);
}
.field.last,
.row.last {
  border-bottom: none;
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
  line-height: 1.5;
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
  white-space: nowrap;
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

/* 本地语音服务状态条 */
.stt-head {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 12.5px;
  color: var(--text);
  margin-bottom: 8px;
}
.stt-switch-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding-bottom: 10px;
  margin-bottom: 10px;
  border-bottom: 1px solid var(--line-soft);
}
/* 预加载那一行：和 .stt-switch-row 一样的排布，但它是这一组的最后一行，不要下边框 */
.stt-preload {
  display: flex;
  align-items: center;
  gap: 12px;
}
/* 引擎三选一单独占一行，下面紧跟一行说明 */
.field > .seg-group {
  margin: 2px 0 8px;
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

/* 音色：滑块占满剩余宽度，试听按钮固定宽度 */
.tts-row {
  display: flex;
  align-items: center;
  gap: 12px;
}
/* 合成相关的提示：错误红、限制说明琥珀，和「正常运行」的灰字区分开 */
.tts-error,
.tts-warn {
  margin: 2px 0 0;
  font-size: 12px;
  line-height: 1.65;
}
.tts-error {
  color: var(--danger, #d24b4b);
}
.tts-warn {
  color: #c2410c;
  background: rgba(217, 119, 6, 0.08);
  border: 1px solid rgba(217, 119, 6, 0.22);
  border-radius: 8px;
  padding: 8px 10px;
}
/* 三选一的模型卡：一行一个，选中状态一眼能看出来 */
.tts-pick {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.tts-opt {
  display: flex;
  align-items: baseline;
  gap: 10px;
  width: 100%;
  padding: 7px 11px;
  border-radius: 9px;
  border: 1px solid var(--line);
  background: transparent;
  color: var(--text);
  text-align: left;
  cursor: pointer;
  transition: border-color 0.14s var(--ease), background 0.14s var(--ease);
}
.tts-opt:hover {
  background: var(--hover);
}
.tts-opt:disabled {
  opacity: 0.55;
  cursor: default;
}
.tts-opt.on {
  border-color: var(--accent);
  background: var(--accent-soft, rgba(10, 111, 240, 0.1));
}
.tts-opt-name {
  font-weight: 600;
  flex-shrink: 0;
  min-width: 62px;
}
.tts-opt.on .tts-opt-name {
  color: var(--accent);
}
.tts-opt-desc {
  font-size: 11px;
  color: var(--text-3);
  line-height: 1.4;
}
/* 音色编号网格：103 个编号用滑块挨个试太慢，直接铺开点。
   最多 9 行，超出滚动，不会把设置面板撑爆 */
.voice-grid {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  max-height: 168px;
  overflow-y: auto;
  padding: 2px;
}
.voice-dot {
  min-width: 29px;
  height: 24px;
  padding: 0 4px;
  border-radius: 6px;
  border: 1px solid var(--line);
  background: transparent;
  color: var(--text-2, var(--text));
  font-size: 10.5px;
  font-variant-numeric: tabular-nums;
  cursor: pointer;
  transition: background 0.12s var(--ease), border-color 0.12s var(--ease);
}
.voice-dot:hover {
  background: var(--hover);
}
.voice-dot.on {
  border-color: var(--accent);
  background: var(--accent-soft, rgba(10, 111, 240, 0.14));
  color: var(--accent);
  font-weight: 600;
}
.tts-row input[type='range'] {
  flex: 1;
  min-width: 0;
  accent-color: var(--accent);
}
/* CPU / GPU 二选一：分段按钮比下拉框更直观，也省一次展开 */
.seg-group {
  display: inline-flex;
  flex-shrink: 0;
  padding: 2px;
  gap: 2px;
  border-radius: 8px;
  background: var(--fill);
}
.seg {
  padding: 3px 10px;
  border: none;
  border-radius: 6px;
  font-size: 11.5px;
  color: var(--text-2);
  background: transparent;
  cursor: pointer;
  transition: all 0.15s var(--ease);
}
.seg:hover:not(:disabled) {
  color: var(--text);
}
.seg.on {
  color: var(--text);
  background: var(--surface);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
}
.seg:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
.stt-dir {
  font-size: 11px;
  color: var(--text-3);
  font-family: ui-monospace, monospace;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 320px;
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

.row-control {
  margin-left: auto;
  display: flex;
  align-items: center;
  gap: 7px;
  flex: none;
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

.select {
  height: 26px;
  max-width: 168px;
  padding: 0 6px;
  border-radius: 7px;
  border: 1px solid var(--line);
  background: var(--surface);
  font-size: 12px;
  color: var(--text);
  outline: none;
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

/* ---------- 快捷键录制按钮 ---------- */
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
.row-sub.warn {
  color: var(--red);
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
  margin: 4px 2px 12px;
  font-size: 10.5px;
  color: var(--text-3);
  line-height: 1.6;
}
/* 行内的小按钮：和文字基线对齐，别把行高撑开 */
.foot-note .mini-btn {
  height: 21px;
  padding: 0 8px;
  margin-left: 4px;
  vertical-align: middle;
  font-size: 10.5px;
}

/* ---------- 底部 ---------- */
.sheet-foot {
  flex: none;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 20px calc(12px + env(safe-area-inset-bottom));
  border-top: 1px solid var(--line-soft);
  background: var(--surface-2);
}

.btn {
  height: 30px;
  padding: 0 15px;
  border-radius: 8px;
  font-size: 12.5px;
  font-weight: 500;
  transition: background 0.14s var(--ease), transform 0.12s var(--ease);
}
.btn:active {
  transform: scale(0.97);
}
.btn.ghost {
  color: var(--text-2);
}
.btn.ghost:hover {
  background: var(--hover);
  color: var(--text);
}
.btn.primary {
  margin-left: auto;
  background: var(--accent);
  color: #fff;
}
.btn.primary:hover {
  background: var(--accent-hover);
}

/* ---------- 过渡 ---------- */
.sheet-enter-active,
.sheet-leave-active {
  transition: opacity 0.2s var(--ease);
}
.sheet-enter-active .sheet,
.sheet-leave-active .sheet {
  transition: transform 0.24s var(--ease), opacity 0.2s var(--ease);
}
.sheet-enter-from,
.sheet-leave-to {
  opacity: 0;
}
.sheet-enter-from .sheet,
.sheet-leave-to .sheet {
  opacity: 0;
  transform: scale(0.97) translateY(8px);
}
</style>
