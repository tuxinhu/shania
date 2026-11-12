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
  sttEnsure,
  sttInstall,
  sttStatus,
  sttStop,
  sttStreamInstall,
  sttStreamUnload,
  testConnection,
  type SttStatus,
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

/* ---------- 本地语音服务 ---------- */
const stt = ref<SttStatus | null>(null)
const sttInstalling = ref(false)
const sttError = ref('')
const sttProgress = ref<{ stage: string; done: number; total: number | null }>({
  stage: '',
  done: 0,
  total: null,
})
let unlistenStt: UnlistenFn | null = null

/** whisper 引擎本地就绪 = 服务端 exe + 模型都在。X-ASR 的模型状态另算（见 streamReady） */
const whisperReady = computed(() => !!stt.value?.serverReady && !!stt.value?.modelReady)

/**
 * whisper 能不能用（含「填了远程服务地址」的情况）。
 *
 * 引擎判断要用这个而不是 whisperReady：用户完全可以把 whisper 指向别处自己管的
 * 服务，那时本地什么都没装也照样能转写。
 */
const whisperUsable = computed(
  () => !!settingsStore.settings.sttBaseUrl.trim() || whisperReady.value,
)

/** 后端那一行的说明：把「能不能选 / 选了要付什么代价」直接写在脸上（只对 whisper 有效） */
const backendHint = computed(() => {
  const info = stt.value
  if (!info) return '浏览器预览环境不可用'
  if (!info.gpuAvailable) return '没检测到 NVIDIA 显卡，whisper 只能用 CPU'
  if (info.backend === 'cuda') {
    return info.cudaReady
      ? 'NVIDIA GPU 加速已就绪，whisper 处理 8 秒音频约 0.3 秒出字'
      : '需要先下载 GPU 版 whisper（约 650MB）'
  }
  return info.cudaReady
    ? '当前用 CPU；GPU 版已装好，切过去即可提速'
    : '兼容性最好。有 NVIDIA 显卡可切到 GPU，快 3-8 倍（额外约 650MB）'
})

const whisperStateText = computed(() => {
  if (!stt.value) return '浏览器预览环境不可用'
  if (!stt.value.serverReady) return '未安装'
  if (!stt.value.modelReady) {
    const got = Math.round(stt.value.modelBytes / 1024 / 1024)
    return got > 0 ? `服务端已装，模型下载未完成（${got}MB）` : '服务端已装，缺模型'
  }
  const who = stt.value.runningBackend === 'cuda' ? 'GPU' : 'CPU'
  const base = stt.value.running
    ? `就绪（端口 ${stt.value.port}，${who}）`
    : '已安装，用到时自动启动'
  // VAD 缺失不影响可用性，但噪声里容易编出内容，值得提一句
  return stt.value.vadReady ? base : `${base} · 缺降噪模型，重装可补上`
})

const sttPercent = computed(() => {
  const { done, total } = sttProgress.value
  if (!total) return 0
  return Math.min(100, Math.round((done / total) * 100))
})

const sttStageText = computed(() => {
  const stage = sttProgress.value.stage
  if (!stage) return ''
  switch (stage) {
    case 'server':
      return '下载 whisper 服务端'
    case 'model':
      return '下载 whisper 模型'
    case 'vad':
      return '下载语音检测模型'
    case 'stream':
      return '下载 X-ASR 模型'
    case 'done':
      return '收尾'
    default:
      return '下载服务端'
  }
})

/* ---------- X-ASR 流式识别 ---------- */

const streamInstalling = ref(false)
const streamError = ref('')

/** X-ASR 模型在不在盘上（装好了才可能被选中当主引擎） */
const streamReady = computed(() => !!stt.value?.streamReady)

/** X-ASR 这一块的状态文字：装没装、装了多少、有没有加载进内存 */
const streamStateText = computed(() => {
  if (!stt.value) return '浏览器预览环境不可用'
  if (!stt.value.streamReady) {
    const mb = Math.round(stt.value.streamBytes / 1024 / 1024)
    return mb > 0 ? `下载未完成（${mb}MB）` : '未安装'
  }
  return stt.value.streamLoaded ? '已就绪（模型已加载）' : '已安装，用到时自动加载'
})

/**
 * 引擎三选一的说明。
 *
 * 两套引擎**地位对等**，没有谁是谁的升级版：选谁就先用谁，指定的那个不可用时
 * 自动换另一个兜底。所以这里要说的是「现在实际会走哪个」，而不是暗示流式更高级。
 */
const engineHint = computed(() => {
  const engine = settingsStore.settings.sttEngine
  const stream = streamReady.value
  const whisper = whisperUsable.value
  const none = '；两套都没装，点麦克风会提示去安装'

  if (engine === 'whisper') {
    if (whisper) return '优先 whisper-server：说完一整句才出字，走下面那组的服务地址与端口'
    if (stream) return '优先 whisper，但本地服务和模型都没装——会改用 X-ASR 流式识别兜底'
    return `优先 whisper，但本地服务和模型都没装${none}`
  }
  if (engine === 'stream') {
    if (stream) return '优先 X-ASR：边说边出字，说完约 1 秒定稿（进程内推理，不占端口）'
    if (whisper) return '优先 X-ASR，但它的模型没装——会改用 whisper 兜底'
    return `优先 X-ASR，但它的模型没装${none}`
  }
  if (stream) return '不指定：X-ASR 已就绪就走它（边说边出字）'
  if (whisper) return '不指定：X-ASR 没装，会走 whisper'
  return `不指定：两套都没装${none}`
})

/** 下次录音实际会走哪个引擎——比任何形容词都直观 */
const engineNextText = computed(() => {
  const engine = settingsStore.settings.sttEngine
  const stream = streamReady.value
  const whisper = whisperUsable.value
  let now: string
  if (engine === 'whisper') now = whisper ? 'whisper' : stream ? 'X-ASR（兜底）' : '无可用引擎'
  else if (engine === 'stream') now = stream ? 'X-ASR' : whisper ? 'whisper（兜底）' : '无可用引擎'
  else now = stream ? 'X-ASR' : whisper ? 'whisper' : '无可用引擎'
  return `下次录音会走：${now}`
})

/** 预加载的说明：只预热当前指定的引擎，避免两套模型同时占内存 */
const preloadHint = computed(() => {
  if (!settingsStore.settings.sttPreload) return '关：点麦克风时才加载模型，第一次会等 1-2 秒'
  if (settingsStore.settings.sttEngine === 'whisper') {
    return '开：程序启动后拉起 whisper 进程（约 380MB 常驻），点麦克风立刻可用'
  }
  if (whisperReady.value && !streamReady.value) {
    return '开：启动后在后台拉起 whisper 进程（X-ASR 没装，暂时只能用它）'
  }
  return '开：启动后把 X-ASR 模型读进内存（约 500MB），点麦克风立刻出字'
})

async function installStream() {
  if (streamInstalling.value) return
  streamInstalling.value = true
  streamError.value = ''
  try {
    await sttStreamInstall()
  } catch (err) {
    streamError.value = err instanceof Error ? err.message : String(err)
  } finally {
    streamInstalling.value = false
    sttProgress.value = { stage: '', done: 0, total: null }
    await refreshStt()
  }
}

async function unloadStream() {
  await sttStreamUnload().catch(() => undefined)
  await refreshStt()
}

async function refreshStt() {
  stt.value = await sttStatus().catch(() => null)
}

async function installStt(backend?: 'cpu' | 'cuda') {
  if (sttInstalling.value) return
  sttInstalling.value = true
  sttError.value = ''
  try {
    await sttInstall(backend)
  } catch (err) {
    sttError.value = err instanceof Error ? err.message : String(err)
  } finally {
    sttInstalling.value = false
    sttProgress.value = { stage: '', done: 0, total: null }
    await refreshStt()
  }
}

/**
 * 切换推理后端。
 *
 * GPU 版是另一个 653MB 的二进制包，第一次选它要先下载——所以这里不只是改个设置项，
 * 而是一个「选了就装、装完就换过去」的动作。
 */
async function setSttBackend(value: 'cpu' | 'cuda') {
  if (sttInstalling.value) return
  const s = settingsStore.settings
  if (s.sttBackend === value && stt.value?.runningBackend === value) return

  sttError.value = ''
  if (value === 'cuda' && !stt.value?.cudaReady) {
    s.sttBackend = 'cuda'
    await installStt('cuda')
    if (sttError.value) {
      // 下载失败就退回 CPU，别把用户留在「选了 GPU 但其实没有」的状态
      s.sttBackend = 'cpu'
      return
    }
  } else {
    s.sttBackend = value
  }

  // 后端变了必须重启进程：现在跑的还是旧的那个可执行文件
  await sttStop().catch(() => undefined)
  await refreshStt()
}

async function stopStt() {
  await sttStop().catch(() => undefined)
  await refreshStt()
}

async function openLogs() {
  // 打不开目录没什么可补救的，记一笔即可；界面上不用弹错
  await openLogsDir().catch((err) => console.error('打开日志目录失败', err))
}

/** 打开「启动时预加载」时顺便立刻拉起来，不然用户会觉得开关没反应 */
async function toggleSttPreload() {
  const next = !settingsStore.settings.sttPreload
  settingsStore.settings.sttPreload = next
  if (!next) return
  // 能立刻生效的只有 whisper 进程：X-ASR 没有「现在就读盘」的命令，它在下一次
  // 点麦克风（或下次启动）时加载。所以引擎指向 X-ASR 时这里什么都不用做。
  const engine = settingsStore.settings.sttEngine
  if (engine === 'stream') return
  if (engine === 'auto' && streamReady.value) return
  if (!whisperReady.value) return
  await sttEnsure(settingsStore.settings.sttPort || 0).catch((err) => {
    sttError.value = err instanceof Error ? err.message : String(err)
  })
  await refreshStt()
}

onMounted(async () => {
  window.addEventListener('keydown', onKeydown)
  autoStart.value = await isAutoStartEnabled().catch(() => false)
  await refreshStt()
  if (isTauri) {
    unlistenStt = await listen<{ stage: string; done: number; total: number | null }>(
      'stt-progress',
      (event) => {
        sttProgress.value = event.payload
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

              <!-- 引擎：指定用哪个，另一个自动兜底 -->
              <div class="group">
                <div class="field">
                  <div class="stt-head">
                    <span>识别引擎</span>
                    <span class="stt-state">{{ engineNextText }}</span>
                  </div>
                  <div class="seg-group">
                    <button
                      class="seg"
                      :class="{ on: settingsStore.settings.sttEngine === 'auto' }"
                      title="装了什么用什么：X-ASR 装了就走它，没装走 whisper"
                      @click="settingsStore.settings.sttEngine = 'auto'"
                    >
                      自动
                    </button>
                    <button
                      class="seg"
                      :class="{ on: settingsStore.settings.sttEngine === 'stream' }"
                      :disabled="!streamReady"
                      :title="
                        streamReady ? '优先用 X-ASR（边说边出字）' : '先下载 X-ASR 模型才能选'
                      "
                      @click="settingsStore.settings.sttEngine = 'stream'"
                    >
                      X-ASR
                    </button>
                    <button
                      class="seg"
                      :class="{ on: settingsStore.settings.sttEngine === 'whisper' }"
                      title="优先用 whisper-server（整段转写，需要服务地址与端口）"
                      @click="settingsStore.settings.sttEngine = 'whisper'"
                    >
                      whisper
                    </button>
                  </div>
                  <p class="help">{{ engineHint }}</p>
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

              <!-- X-ASR：进程内推理，不占端口 -->
              <div class="group">
                <div class="field last">
                  <div class="stt-head">
                    <span>X-ASR 流式模型</span>
                    <span class="stt-state" :class="{ ok: streamReady, warn: !!stt && !streamReady }">
                      {{ streamStateText }}
                    </span>
                  </div>
                  <div class="stt-actions">
                    <button
                      v-if="!streamReady"
                      class="mini-btn primary"
                      :disabled="streamInstalling"
                      @click="installStream"
                    >
                      {{ streamInstalling ? '下载中…' : '下载模型（约 134MB）' }}
                    </button>
                    <button
                      v-else-if="stt?.streamLoaded"
                      class="mini-btn"
                      title="释放内存，下次录音时再重新加载"
                      @click="unloadStream"
                    >
                      卸载模型
                    </button>
                    <span class="stt-dir" title="~/.shania/models/stream-x-asr-160ms-zh-en-punct-int8/">
                      ~/.shania/models/stream-x-asr-160ms-…
                    </span>
                  </div>
                  <div v-if="streamInstalling" class="stt-progress">
                    <div class="stt-bar"><i :style="{ width: `${sttPercent}%` }" /></div>
                    <span>{{ sttStageText }} {{ sttPercent }}%</span>
                  </div>
                  <p v-if="streamError" class="stt-error">{{ streamError }}</p>
                  <p class="help">
                    <code>X-ASR 160ms</code>（中英双语，模型自带标点，int8 量化 134MB）。
                    识别在<strong>本进程内</strong>完成，<strong>不监听端口、不需要服务地址</strong>——
                    下面「whisper 本地服务」里的端口和地址对它无效。
                    说话时字跟着往外冒，首字约 0.4 秒，停 1 秒左右自动定稿。
                  </p>
                </div>
              </div>
              <!-- whisper：独立进程 + HTTP，下面这组的端口/地址只对它有效 -->
              <div class="group">
                <div class="field">
                  <div class="stt-head">
                    <span>whisper 本地服务</span>
                    <span class="stt-state" :class="{ ok: whisperReady, warn: !!stt && !whisperReady }">
                      {{ whisperStateText }}
                    </span>
                  </div>
                  <div class="stt-switch-row">
                    <div class="row-text">
                      <span class="row-label">推理后端</span>
                      <span class="row-sub">{{ backendHint }}</span>
                    </div>
                    <div class="seg-group">
                      <button
                        class="seg"
                        :class="{ on: stt?.backend === 'cpu' }"
                        :disabled="sttInstalling"
                        @click="setSttBackend('cpu')"
                      >
                        CPU
                      </button>
                      <button
                        class="seg"
                        :class="{ on: stt?.backend === 'cuda' }"
                        :disabled="sttInstalling || !stt?.gpuAvailable"
                        :title="stt?.gpuAvailable ? '用 NVIDIA 显卡推理' : '没有检测到可用的 NVIDIA 显卡'"
                        @click="setSttBackend('cuda')"
                      >
                        GPU
                      </button>
                    </div>
                  </div>
                  <div class="stt-actions">
                    <button
                      v-if="!whisperReady"
                      class="mini-btn primary"
                      :disabled="sttInstalling"
                      @click="installStt()"
                    >
                      {{
                        sttInstalling
                          ? '安装中…'
                          : stt?.backend === 'cuda'
                            ? '下载 GPU 版（约 650MB）'
                            : '一键安装（约 190MB）'
                      }}
                    </button>
                    <button
                      v-else-if="stt?.running"
                      class="mini-btn"
                      title="进程会在下次转写时重新拉起"
                      @click="stopStt"
                    >
                      停止服务
                    </button>
                    <span v-if="stt?.dir" class="stt-dir" :title="stt.dir">{{ stt.dir }}</span>
                  </div>
                  <div v-if="sttInstalling" class="stt-progress">
                    <div class="stt-bar"><i :style="{ width: `${sttPercent}%` }" /></div>
                    <span>{{ sttStageText }} {{ sttPercent }}%</span>
                  </div>
                  <p v-if="sttError" class="stt-error">{{ sttError }}</p>
                  <p class="help">
                    独立子进程 <code>whisper-server</code>，模型 small-q5_1，装到
                    <code>~/.shania/</code>。它靠 HTTP 收音频，<strong>所以下面四个字段都只对它有效；X-ASR 完全用不到</strong>。
                    进程用到才起、常驻复用，退出应用时杀掉；点「停止服务」可以立即释放内存（约 380MB）。
                    手机热点或网络慢的话，下载可能要好几分钟。
                  </p>
                </div>
                <div class="field">
                  <label for="sttPort">服务端口</label>
                  <input
                    id="sttPort"
                    v-model.number="settingsStore.settings.sttPort"
                    type="number"
                    min="0"
                    max="65535"
                    placeholder="28080"
                  />
                  <p class="help">
                    whisper 进程的监听端口，默认 <code>28080</code>，和 <code>pnpm stt</code> 启动的脚本一致，
                    方便直接 curl 调试（没用 8080 是因为它常被其他开发工具占）。被占用时会自动往后顺延，
                    不会启动失败；填 <code>0</code> 表示每次都让系统随机分配。改完下次转写生效
                    （正在跑的进程不受影响）。
                  </p>
                </div>
                <div class="field">
                  <label for="sttBaseUrl">识别服务地址</label>
                  <input
                    id="sttBaseUrl"
                    v-model.trim="settingsStore.settings.sttBaseUrl"
                    type="text"
                    spellcheck="false"
                    placeholder="http://127.0.0.1:28080/v1"
                  />
                  <p class="help">
                    <strong>留空 = 用上面这套内置 whisper 服务</strong>，不必手动起进程。
                    只有想接远程或自己管的 whisper 服务时才填，要填到 <code>/v1</code> 为止，
                    程序会自己补 <code>/audio/transcriptions</code>。填了之后就不再检查本地是否安装。
                  </p>
                </div>
                <div class="field">
                  <label for="sttModel">识别模型</label>
                  <input
                    id="sttModel"
                    v-model.trim="settingsStore.settings.sttModel"
                    type="text"
                    spellcheck="false"
                    placeholder="whisper-1"
                  />
                  <p class="help">
                    只对 whisper 生效。复用上面的 API Key；本地 whisper-server 忽略这个字段
                    （模型在启动参数里定），填什么都行；接云端 OpenAI 才需要填 <code>whisper-1</code>。
                    X-ASR 的模型是固定的，没有这一项。
                  </p>
                </div>
                <div class="field last">
                  <label for="sttLanguage">语言提示</label>
                  <input
                    id="sttLanguage"
                    v-model.trim="settingsStore.settings.sttLanguage"
                    type="text"
                    spellcheck="false"
                    placeholder="zh"
                  />
                  <p class="help">
                    只对 whisper 生效：留空由服务端自动判断，填 <code>zh</code> / <code>en</code>
                    能减少短句误识别。X-ASR 是中英双语模型，语言由模型自己判，不看这一项。
                  </p>
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
