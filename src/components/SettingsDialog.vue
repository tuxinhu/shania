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
  type StreamStatus,
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
