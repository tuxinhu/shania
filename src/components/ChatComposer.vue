<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import Icon from './Icon.vue'
import { fileToAttachment } from '@/lib/image'
import { fileToTextAttachment, isTextFile } from '@/lib/files'
import { sttEnsure, sttStatus, transcribeAudio } from '@/lib/api'
import { PcmSegmenter, pcmToWav } from '@/lib/pcm-recorder'
import { StreamStt } from '@/lib/stream-stt'
import { closeSegment, closeSession, joinVoice } from '@/lib/punctuate'
import { useChatStore } from '@/stores/chat'
import { useSettingsStore } from '@/stores/settings'
import type { Attachment } from '@/lib/types'

defineEmits<{ (e: 'open-settings'): void }>()

const chat = useChatStore()
const settingsStore = useSettingsStore()

const draft = ref('')
const ta = ref<HTMLTextAreaElement | null>(null)
const fileInput = ref<HTMLInputElement | null>(null)
const attachments = ref<Attachment[]>([])
const attachError = ref('')
const dragging = ref(false)
const modelMenu = ref(false)
let composing = false

const MAX_IMAGES = 4
const MAX_TEXTS = 4
const MAX_IMAGE_BYTES = 4 * 1024 * 1024
const DRAFT_KEY = 'shania.drafts.v1'

const displayName = computed(() => settingsStore.settings.displayName || 'Shania')
const canSend = computed(
  () => (draft.value.trim().length > 0 || attachments.value.length > 0) && !chat.streaming,
)

/* ------------------------------ 草稿 ------------------------------ */

type DraftMap = Record<string, string>

function loadDrafts(): DraftMap {
  try {
    const raw = localStorage.getItem(DRAFT_KEY)
    return raw ? (JSON.parse(raw) as DraftMap) : {}
  } catch {
    return {}
  }
}

const drafts = loadDrafts()
let draftTimer: ReturnType<typeof setTimeout> | null = null

function saveDrafts() {
  if (draftTimer) clearTimeout(draftTimer)
  draftTimer = setTimeout(() => {
    draftTimer = null
    try {
      // 只保留非空草稿，避免长期累积
      const compact: DraftMap = {}
      for (const [k, v] of Object.entries(drafts)) if (v.trim()) compact[k] = v
      localStorage.setItem(DRAFT_KEY, JSON.stringify(compact))
    } catch {
      /* 配额满时忽略，草稿丢失不影响主流程 */
    }
  }, 320)
}

// 切换会话时：先把当前草稿归档，再载入目标会话的草稿
watch(
  () => chat.currentId,
  (id, old) => {
    if (old) drafts[old] = draft.value
    draft.value = drafts[id] ?? ''
    attachments.value = []
    attachError.value = ''
    nextTick(() => {
      resize()
      ta.value?.focus()
    })
  },
)

watch(draft, (value) => {
  if (chat.currentId) drafts[chat.currentId] = value
  saveDrafts()
})

/* ------------------------------ 输入区 ------------------------------ */

function onCompositionStart() {
  composing = true
}

function onCompositionEnd() {
  composing = false
}

function resize() {
  const el = ta.value
  if (!el) return
  el.style.height = 'auto'
  el.style.height = `${Math.min(el.scrollHeight, 176)}px`
}

function setDraft(text: string) {
  draft.value = text
  nextTick(() => {
    resize()
    ta.value?.focus()
    ta.value?.setSelectionRange(text.length, text.length)
  })
}

function focus() {
  ta.value?.focus()
}

function submit() {
  if (!canSend.value) return
  // 语音还没收尾就点发送：末尾挂着的「，」要先收成句号再取文本
  settleVoice()
  // 边说话边发送：把这一轮语音彻底掐掉。
  // 不掐的话，模型那边还会在 1 秒内定稿一句，结果又被写回输入框——
  // 用户看到的就是「刚发出去的话又冒出来了」。
  if (stream) {
    void stream.abort()
    stream = null
    streamMode.value = false
  }
  if (recording.value) {
    recording.value = false
    const current = segmenter
    segmenter = null
    void current?.stop()
  }
  const text = draft.value.trim()
  const files = attachments.value
  draft.value = ''
  attachments.value = []
  attachError.value = ''
  if (chat.currentId) drafts[chat.currentId] = ''
  saveDrafts()
  nextTick(resize)
  void chat.send(text, files)
}

function onKeydown(event: KeyboardEvent) {
  if (event.key !== 'Enter' || event.shiftKey) return
  if (composing || event.isComposing || event.keyCode === 229) return
  event.preventDefault()
  submit()
}

function toggleTools() {
  settingsStore.settings.toolsEnabled = !settingsStore.settings.toolsEnabled
}

/* ------------------------------ 语音输入 ------------------------------ */

const recording = ref(false)
/** 队列里还有几段没转完，用来显示「识别中」而不是一停止就消失 */
const pendingSegments = ref(0)
/** VAD 判定当前是否在说话，驱动麦克风的呼吸效果 */
const speaking = ref(false)
/** 这一次录音走的是流式（true）还是 whisper 整段（false），决定标题与收尾逻辑 */
const streamMode = ref(false)

let segmenter: PcmSegmenter | null = null
/**
 * 流式识别会话；null 表示这次走 whisper。
 *
 * 两套引擎是互斥的：流式把音频原样喂给模型自己断句，whisper 那条路靠前端能量
 * 切段再整段转写。同时开会重复识别同一段话。
 */
let stream: StreamStt | null = null
/**
 * 流式模型装没装。
 *
 * 只在「装好了」时长期缓存：用户可能在设置里刚装完就回来点麦克风，
 * 缓存 false 会让他白重启一次。查一次状态只是一次 IPC，失败路径不值得省。
 */
let streamReadyCache = false

/** 所有分段串行排队：模型一次只能处理一个请求，并发反而更慢、且会打乱文字顺序 */
let transcribeQueue: Promise<void> = Promise.resolve()
/** 会话内缓存解析好的语音服务地址，避免每段都走一次 IPC */
let cachedSttBaseUrl = ''

const voiceTitle = computed(() => {
  if (recording.value) {
    return speaking.value ? '正在听…边说边出字（再点结束）' : '聆听中（说话即可）'
  }
  if (pendingSegments.value > 0) return '正在识别…'
  return '语音输入（边说边出字）'
})

function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => {
      const result = String(reader.result ?? '')
      resolve(result.slice(result.indexOf(',') + 1))
    }
    reader.onerror = () => reject(reader.error)
    reader.readAsDataURL(blob)
  })
}

/**
 * 语音输入期间，输入框里「由声音转出来的那段文字」是整体管理的，不是一段段往后追加。
 *
 *   voiceBase      开始说话前已有的内容（末尾带一个分隔空格）
 *   voiceConfirmed 已经定稿的句子
 *   voicePreview   正在说的这一句的临时结果，随时会被下一次识别改写
 *
 * 为什么必须拆开：whisper 没有流式接口，想「边说边出字」只能把当前这段音频
 * 反复整体重解（见 pcm-recorder 的 onPartial）。结果是每次都会吐出一整段文本，
 * 直接往后追加的话同一句话会出现三五遍，所以只能「覆盖预览、定稿才追加」。
 */
let voiceBase = ''
let voiceConfirmed = ''
let voicePreview = ''

/** 每定稿一段就 +1：在路上飞着的预览结果如果属于上一段，回来时直接丢掉 */
let voiceEpoch = 0
/** 预览请求还没回来：期间不再发新的，免得 CPU 后端排队越积越多 */
let previewInFlight = false
/** 麦克风正在打开：这期间重复点击直接忽略，否则 start() 和 stop() 会打架 */
let voiceStarting = false
/**
 * 启动过程中用户又点了一下「结束」。
 *
 * 首次加载流式模型要好几秒，这期间 `stream` 还没赋值、`segmenter` 也还没建，
 * 停止分支什么抓手都没有，只能记个标志，等启动流程走到检查点时自己回滚。
 */
let voiceCancelled = false

function ensureVoiceAnchor() {
  // 用户一边说话一边自己改输入框（或发了消息）——以他改后的内容为新基准，
  // 别把他的手打内容覆盖掉
  if (draft.value !== voiceFull()) {
    voiceBase = draft.value
    voiceConfirmed = ''
    voicePreview = ''
  }
}

/**
 * 两段语音之间怎么接。实现在 `lib/punctuate.ts`——中文靠标点断句不要空格，
 * 英文必须补空格，规则比看上去啰嗦。
 */
function voiceFull(): string {
  return voiceBase + joinVoice(voiceConfirmed, voicePreview)
}

function renderVoice() {
  const next = voiceFull()
  if (next !== draft.value) setDraft(next)
}

function beginVoiceSession() {
  // 语音文字直接接在原有内容后面，不额外补空格：中文不需要，英文用户也很少
  // 在已有内容后面接着口述。补了反而在输入框里留一个突兀的尾巴。
  voiceBase = draft.value
  voiceConfirmed = ''
  voicePreview = ''
}

/**
 * 一句话说完了：定稿。
 *
 * 定稿时才补标点：预览是半句话，补了会跟着识别结果一起跳；而且一段话到底
 * 该用逗号还是句号，得看它有多长（`closeSegment` 里的规则）。
 */
function commitVoiceSegment(text: string, durationMs: number) {
  ensureVoiceAnchor()
  voiceConfirmed = joinVoice(voiceConfirmed, closeSegment(text, durationMs))
  voicePreview = ''
  renderVoice()
}

/** 还在说：只改预览部分 */
function previewVoice(text: string) {
  ensureVoiceAnchor()
  if (!text || text === voicePreview) return
  voicePreview = text
  renderVoice()
}

/**
 * 把正在进行的语音收尾。用户说完不等「结束」直接按发送时会走到这里——
 * 不处理的话消息就会以逗号结尾，看着像话说了一半。
 */
function settleVoice() {
  if (!voiceConfirmed && !voicePreview) return
  if (voicePreview) voiceConfirmed = joinVoice(voiceConfirmed, closeSegment(voicePreview))
  voicePreview = ''
  voiceConfirmed = closeSession(voiceConfirmed)
  renderVoice()
  voiceBase = ''
  voiceConfirmed = ''
}

function endVoiceSession() {
  ensureVoiceAnchor()
  // 最后那段音频被判定成噪声丢掉了，但预览已经显示出来——留着比让它凭空消失好
  if (voicePreview) voiceConfirmed = joinVoice(voiceConfirmed, closeSegment(voicePreview))
  voicePreview = ''
  // 定稿时一律补「，」（那时不知道还会不会继续说），这里改成句号收尾
  voiceConfirmed = closeSession(voiceConfirmed)
  renderVoice()
  voiceBase = ''
  voiceConfirmed = ''
}

/** 流式模型装好没有。只在成功时缓存 true——用户中途装完模型，下一次录音就该用上。 */
async function streamReadyNow(): Promise<boolean> {
  if (streamReadyCache) return true
  try {
    const status = await sttStatus()
    streamReadyCache = status?.streamReady ?? false
  } catch {
    return false
  }
  return streamReadyCache
}

/**
 * 这次录音该不该走流式。
 *
 * 两个前提：设置没强制 whisper、流式模型装好了。
 * 「自动」在模型缺失时静默退回 whisper；「流式」不允许静默——调用方必须先
 * 用 [`streamReadyNow`] 拦一次（见 `toggleVoice`），否则用户改了设置却看不出
 * 任何区别，只会以为开关坏了。
 */
async function streamUsable(): Promise<boolean> {
  if (settingsStore.settings.sttEngine === 'whisper') return false
  return streamReadyNow()
}

/**
 * 打开流式识别会话。失败返回 false，调用方据此回退到 whisper。
 *
 * 第一次会加载 134MB 模型（1-2 秒），之后常驻内存，再次 start 是毫秒级。
 */
async function startStream(): Promise<boolean> {
  if (!(await streamUsable())) return false

  const session = new StreamStt({
    // 预览是**整句覆盖**：这个模型每次吐的都是从头到现在的结果，不是增量
    onPartial: (text) => previewVoice(text),
    // 定稿：durationMs 传 0，标点由模型自己给（`closeSegment` 见到已有标点就不动）
    onFinal: (text) => commitVoiceSegment(text, 0),
    onError: (err) => {
      attachError.value = `语音识别出错：${err.message}`
    },
  })

  try {
    await session.start()
    stream = session
    return true
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err)
    if (msg.includes('STREAM_NOT_INSTALLED')) {
      // 模型没装：安静退回 whisper，不打扰用户
      return false
    }
    // 模型坏了/加载失败：说清楚，但仍然回退，别让语音按钮彻底不可用
    attachError.value = `流式识别启动失败，已退回兼容模式：${msg}`
    return false
  }
}

/** 解析语音服务地址：留空走内置本地服务（第一次会顺手把进程拉起来） */
async function resolveSttBaseUrl(): Promise<string> {
  if (settingsStore.settings.sttBaseUrl) return settingsStore.settings.sttBaseUrl
  if (cachedSttBaseUrl) return cachedSttBaseUrl
  try {
    const port = await sttEnsure(
      settingsStore.settings.sttPort || 0,
      settingsStore.settings.sttBackend,
    )
    cachedSttBaseUrl = `http://127.0.0.1:${port}/v1`
    return cachedSttBaseUrl
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err)
    if (msg.includes('STT_NOT_INSTALLED')) {
      throw new Error('还没装本地语音服务，去「设置 → 语音输入」点一下安装')
    }
    throw err
  }
}

async function transcribePcm(pcm: Float32Array): Promise<string> {
  const baseUrl = await resolveSttBaseUrl()
  const wav = new Blob([pcmToWav(pcm)], { type: 'audio/wav' })
  const base64 = await blobToBase64(wav)
  const text = await transcribeAudio(base64, 'audio/wav', {
    baseUrl,
    apiKey: settingsStore.settings.apiKey,
    model: settingsStore.settings.sttModel || 'whisper-1',
    language: settingsStore.settings.sttLanguage || undefined,
  })
  const clean = text
    .trim()
    // whisper 对「说到一半就被切断」的音频会补 `...` / `--` / `…` 表示后面还有，
    // 那不是用户说的话。只删连续两个以上的点和破折号——单个 `.` 是英文正常句号，
    // 中文的 `。` 也不在匹配范围里。
    .replace(/\s*(?:\.{2,}|…+|-{2,}|—+)$/u, '')
    .trim()
  // whisper 对纯静音偶尔会吐出「谢谢观看」这类幻觉，短到这个程度直接丢
  return clean.length < 2 ? '' : clean
}

/** 一段话结束就排队送去识别；串行执行保证文字按说话顺序落进输入框 */
function enqueueSegment(pcm: Float32Array) {
  voiceEpoch += 1
  pendingSegments.value += 1
  transcribeQueue = transcribeQueue
    .then(async () => {
      const text = await transcribePcm(pcm)
      // 段长用来决定段末补逗号还是句号，16k 单声道单字节采样数直接换算
      if (text) commitVoiceSegment(text, (pcm.length / 16000) * 1000)
    })
    .catch((err) => {
      attachError.value = err instanceof Error ? err.message : String(err)
    })
    .finally(() => {
      pendingSegments.value -= 1
      // 录音已经停了、队列也空了：这一轮语音结束
      if (!recording.value && pendingSegments.value === 0) endVoiceSession()
    })
}

/**
 * 说话过程中的临时结果。**不进串行队列**：它在语义上优先级最低，
 * 一旦被最终结果顶掉就该扔，排进队列只会拖慢定稿。
 */
function enqueuePreview(pcm: Float32Array) {
  if (previewInFlight) return
  previewInFlight = true
  const epoch = voiceEpoch
  void transcribePcm(pcm)
    .then((text) => {
      // 这段已经定稿（或被下一段顶掉），旧结果不许盖新内容
      if (epoch !== voiceEpoch || !recording.value) return
      if (text) previewVoice(text)
    })
    .catch(() => {
      // 预览失败不打扰用户：最终结果还会再来一次
    })
    .finally(() => {
      previewInFlight = false
    })
}

async function toggleVoice() {
  if (recording.value) {
    recording.value = false
    // 还在启动（打开麦克风 / 加载模型）里：这会儿 segmenter 和 stream 都还是空的，
    // 直接返回会让启动流程跑完后又把录音拉起来，界面和实际状态对不上
    if (voiceStarting) {
      voiceCancelled = true
      return
    }
    // 停止采集；手上剩下的音频会被 flush 成最后一段，识别在后台继续跑
    const currentSeg = segmenter
    segmenter = null
    // 注意顺序：先停采集（会把残余帧同步抛给 onFrame），再收流式会话
    await currentSeg?.stop()

    const currentStream = stream
    stream = null
    if (currentStream) {
      // 等队列排空 + 冲尾部，最后一句才不会丢
      await currentStream.stop()
      streamMode.value = false
      // 流式不经过 whisper 队列，收尾得自己叫
      if (pendingSegments.value === 0) endVoiceSession()
    }
    return
  }

  try {
    attachError.value = ''
    if (voiceStarting) return
    voiceStarting = true
    voiceCancelled = false

    // 引擎选了「流式」就是硬要求：模型不在时直接拦住，不静默退回 whisper。
    // 静默退回的结果是「设置改了但听感一模一样」，用户只会怀疑开关没生效。
    if (settingsStore.settings.sttEngine === 'stream' && !(await streamReadyNow())) {
      recording.value = false
      attachError.value =
        '流式识别模型还没下载：去「设置 → 语音输入」点「下载流式模型」（约 134MB）'
      return
    }

    // 先把界面切到录音态：start() 里要等麦克风设备打开（200-400ms），
    // 那段时间界面一动不动，用户会以为没点上
    recording.value = true
    // 语音文字直接接在原有内容后面，基准要在任何识别结果回来之前定好
    beginVoiceSession()

    // 有流式模型就用流式（边说边出字、说完即定稿），没有就退回 whisper 整段转写
    const useStream = await startStream()
    streamMode.value = useStream
    // 加载模型这几秒里用户可能已经点了「结束」
    if (voiceCancelled) {
      voiceCancelled = false
      recording.value = false
      streamMode.value = false
      const pending = stream
      stream = null
      void pending?.abort()
      return
    }

    const seg = new PcmSegmenter(
      useStream
        ? {
            // 流式不关心切段结果：句末由模型自己的 endpoint 规则判定，
            // 这里留空实现，切段逻辑只用来驱动 speaking 的界面状态
            onSegment: () => {},
            onFrame: (pcm) => stream?.feed(pcm),
            onSpeechChange: (value) => {
              speaking.value = value
            },
            onError: (err) => {
              attachError.value = `录音出错：${err.message}`
            },
          }
        : {
            onSegment: (pcm) => enqueueSegment(pcm),
            onPartial: (pcm) => enqueuePreview(pcm),
            onSpeechChange: (value) => {
              speaking.value = value
            },
            onError: (err) => {
              attachError.value = `录音出错：${err.message}`
            },
          },
      {
        // 中文口语换气停顿大约 200-300ms，但**不能**贴着这个值切：把阈值压到
        // 280ms 之后，实测一次输入被切成 53 个碎片（23 个不到 1 秒，29 个被服务端
        // VAD 判定无语音丢掉），识别质量崩了。420ms 是「能抓住句末」和「别切碎」
        // 之间的平衡点。
        silenceMs: 420,
        // 段不够长就继续攒。这是最关键的一条：1 秒的碎片送给模型，没有上下文、
        // 没有标点，几段拼起来是一坨看不懂的字。
        minCutMs: 1200,
        // 一口气说很久的兜底。实时性靠 onPartial 保证，这里纯粹是防单段无限增长。
        maxSegmentMs: 6000,
        // 「好的」这类短语约 400ms，加上 preroll 也要能出字
        minSegmentMs: 250,
        // 只有 whisper 那条路需要：每 900ms 把整段快照重解一遍做预览。
        // 流式是模型自己连续解码，再叠一层纯属重复劳动。
        partialIntervalMs: useStream ? 0 : 900,
        // 第一版预览等音频够 1.5 秒再发。原来 900ms 就发，服务端日志里
        // 1.0 秒的请求占了三成多，其中一半被 VAD 判「无语音」白丢掉。
        minPartialMs: useStream ? 0 : 1500,
      },
    )
    await seg.start()
    // 打开麦克风也要 200-400ms，同样可能在这期间被取消
    if (voiceCancelled) {
      voiceCancelled = false
      recording.value = false
      streamMode.value = false
      await seg.stop()
      const pending = stream
      stream = null
      void pending?.abort()
      return
    }
    segmenter = seg
  } catch (err) {
    voiceCancelled = false
    recording.value = false
    streamMode.value = false
    // 麦克风打不开时识别器还挂着等音频，主动收掉，别让它白占内存
    const pendingStream = stream
    stream = null
    void pendingStream?.abort()
    attachError.value = `无法使用麦克风：${err instanceof Error ? err.message : String(err)}`
  } finally {
    voiceStarting = false
  }
}

/* ------------------------------ 图片附件 ------------------------------ */

async function addFiles(input: FileList | File[]) {
  attachError.value = ''
  const files = Array.from(input)
  if (!files.length) return

  const rejected: string[] = []

  for (const file of files) {
    const imageCount = attachments.value.filter((a) => (a.kind ?? 'image') === 'image').length
    const textCount = attachments.value.filter((a) => a.kind === 'text').length

    if (file.type.startsWith('image/')) {
      if (imageCount >= MAX_IMAGES) {
        attachError.value = `最多添加 ${MAX_IMAGES} 张图片`
        continue
      }
      if (file.size > MAX_IMAGE_BYTES) {
        attachError.value = `${file.name || '图片'} 超过 4MB，已跳过`
        continue
      }
      try {
        attachments.value.push(await fileToAttachment(file, imageCount))
      } catch {
        attachError.value = '读取图片失败'
      }
      continue
    }

    if (isTextFile(file)) {
      if (textCount >= MAX_TEXTS) {
        attachError.value = `最多添加 ${MAX_TEXTS} 个文本附件`
        continue
      }
      try {
        attachments.value.push(await fileToTextAttachment(file, textCount))
      } catch (err) {
        attachError.value = err instanceof Error ? err.message : '读取文本文件失败'
      }
      continue
    }

    rejected.push(file.name || file.type || '未知文件')
  }

  // 图片/文本的限额提示优先，避免被「不支持类型」覆盖掉
  if (rejected.length && !attachError.value) {
    const shown = rejected.slice(0, 2).join('、')
    attachError.value = `不支持的文件类型：${shown}${rejected.length > 2 ? ' 等' : ''}（仅支持图片与文本文件）`
  }
}

function pickFiles() {
  fileInput.value?.click()
}

function onFileChange(event: Event) {
  const input = event.target as HTMLInputElement
  if (input.files?.length) void addFiles(input.files)
  input.value = ''
}

function removeAttachment(id: string) {
  attachments.value = attachments.value.filter((a) => a.id !== id)
  attachError.value = ''
}

function onPaste(event: ClipboardEvent) {
  const files = Array.from(event.clipboardData?.files ?? [])
  // 粘贴文件（截图、复制的文件）时接管；纯文本粘贴仍走 textarea 默认行为
  if (files.length) {
    event.preventDefault()
    void addFiles(files)
  }
}

function onDragOver(event: DragEvent) {
  if (!event.dataTransfer?.types.includes('Files')) return
  event.preventDefault()
  dragging.value = true
}

function onDragLeave(event: DragEvent) {
  // 只有真正离开整个容器才取消高亮
  const next = event.relatedTarget as Node | null
  if (next && (event.currentTarget as HTMLElement).contains(next)) return
  dragging.value = false
}

function onDrop(event: DragEvent) {
  event.preventDefault()
  dragging.value = false
  const files = event.dataTransfer?.files
  if (files?.length) void addFiles(files)
}

function onDocClick(event: MouseEvent) {
  const el = event.target as HTMLElement | null
  if (el && !el.closest('.model-wrap')) modelMenu.value = false
}

function chooseProfile(id: string) {
  settingsStore.applyProfile(id)
  modelMenu.value = false
}

onMounted(() => {
  draft.value = drafts[chat.currentId] ?? ''
  nextTick(resize)
  document.addEventListener('click', onDocClick)
})

onUnmounted(() => {
  // 组件要销毁了，麦克风轨道必须放掉，否则会一直亮着
  if (segmenter) {
    void segmenter.stop()
    segmenter = null
  }
  // 流式会话也要收掉：识别器还挂着等音频会白占一块内存
  if (stream) {
    void stream.abort()
    stream = null
  }
  if (draftTimer) clearTimeout(draftTimer)
  document.removeEventListener('click', onDocClick)
})

defineExpose({ setDraft, focus })
</script>

<template>
  <div class="composer">
    <div
      class="composer-inner"
      :class="{ dragging }"
      @dragover="onDragOver"
      @dragleave="onDragLeave"
      @drop="onDrop"
    >
      <div v-if="attachments.length || attachError" class="attach-area">
        <div v-for="a in attachments" :key="a.id" class="att" :class="{ doc: a.kind === 'text' }">
          <img v-if="a.kind !== 'text'" :src="a.dataUrl" :alt="a.name" />
          <span v-else class="doc-chip" :title="a.name">
            <Icon name="file" :size="13" />
            <span class="doc-name">{{ a.name }}</span>
          </span>
          <button class="rm" title="移除" @click="removeAttachment(a.id)">
            <Icon name="x" :size="9" />
          </button>
        </div>
        <span v-if="attachError" class="attach-error">{{ attachError }}</span>
      </div>

      <div class="box" :class="{ busy: chat.streaming }">
        <textarea
          ref="ta"
          v-model="draft"
          rows="1"
          :placeholder="`给 ${displayName} 发消息…`"
          spellcheck="false"
          @input="resize"
          @keydown="onKeydown"
          @paste="onPaste"
          @compositionstart="onCompositionStart"
          @compositionend="onCompositionEnd"
        />

        <div class="bar">
          <button class="chip icon" title="添加图片或文本文件（也可粘贴或拖入）" @click="pickFiles">
            <Icon name="paperclip" :size="13" />
          </button>
          <button
            class="chip icon"
            :class="{ rec: recording, listening: speaking }"
            :title="voiceTitle"
            @click="toggleVoice"
          >
            <Icon :name="recording ? 'stop' : 'mic'" :size="13" />
            <span v-if="pendingSegments > 0" class="seg-badge">{{ pendingSegments }}</span>
          </button>

          <button class="chip" :class="{ active: settingsStore.settings.toolsEnabled }" @click="toggleTools">
            <Icon name="tool" :size="12" />
            <span>工具{{ settingsStore.settings.toolsEnabled ? '已开' : '已关' }}</span>
          </button>

          <div class="model-wrap">
            <button class="chip model" @click.stop="modelMenu = !modelMenu">
              <Icon name="cpu" :size="12" />
              <span>{{ settingsStore.settings.model || '未选择模型' }}</span>
              <Icon name="chevron" :size="11" />
            </button>

            <Transition name="pop">
              <div v-if="modelMenu" class="model-menu">
                <div class="menu-title">模型配置</div>
                <button
                  v-for="p in settingsStore.settings.profiles"
                  :key="p.id"
                  class="menu-item"
                  :class="{ on: p.id === settingsStore.settings.activeProfileId }"
                  @click="chooseProfile(p.id)"
                >
                  <span class="p-name">{{ p.name }}</span>
                  <span class="p-model">{{ p.model }}</span>
                  <Icon
                    v-if="p.id === settingsStore.settings.activeProfileId"
                    name="check"
                    :size="12"
                  />
                </button>
                <div v-if="!settingsStore.settings.profiles.length" class="menu-empty">
                  还没有保存的配置
                </div>
                <button class="menu-item manage" @click="modelMenu = false; $emit('open-settings')">
                  <Icon name="settings" :size="12" />
                  <span>管理模型配置…</span>
                </button>
              </div>
            </Transition>
          </div>

          <span class="hint">Enter 发送 · Shift + Enter 换行</span>

          <button v-if="chat.streaming" class="send stop" title="停止生成" @click="chat.stop()">
            <Icon name="stop" :size="13" />
          </button>
          <button v-else class="send" title="发送" :disabled="!canSend" @click="submit">
            <Icon name="send" :size="15" />
          </button>
        </div>
      </div>

      <div v-if="dragging" class="drop-hint">
        <Icon name="image" :size="16" />
        <span>松开即可添加图片或文本文件</span>
      </div>

      <input
        ref="fileInput"
        class="file-input"
        type="file"
        accept="image/*,text/*,.md,.markdown,.json,.jsonc,.csv,.tsv,.log,.ini,.conf,.env,.yml,.yaml,.toml,.xml,.html,.css,.vue,.js,.mjs,.cjs,.ts,.jsx,.tsx,.rs,.go,.py,.rb,.php,.java,.cs,.c,.h,.cc,.cpp,.hpp,.lua,.sh,.bash,.ps1,.bat,.cmd,.sql,.graphql,.proto"
        multiple
        @change="onFileChange"
      />
    </div>
  </div>
</template>

<style scoped>
.composer {
  flex: none;
  padding: 8px 24px 16px;
  background: linear-gradient(to top, var(--content) 62%, transparent);
}

.composer-inner {
  position: relative;
  max-width: 800px;
  margin: 0 auto;
}

/* ---------- 附件预览 ---------- */
.attach-area {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 7px;
  margin-bottom: 8px;
  padding: 0 2px;
}

.att {
  position: relative;
  width: 54px;
  height: 54px;
  border-radius: 9px;
  overflow: hidden;
  border: 1px solid var(--line);
  background: var(--surface-2);
  animation: fade-up 0.18s var(--ease) both;
}
.att img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}

/* 文本附件：横向长条，显示文件名 */
.att.doc {
  width: auto;
  max-width: 200px;
  padding-right: 26px;
}
.doc-chip {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 100%;
  padding: 0 10px;
  color: var(--text-2);
  font-size: 12px;
}
.doc-name {
  max-width: 150px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.att .rm {
  position: absolute;
  top: 2px;
  right: 2px;
  width: 15px;
  height: 15px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  background: rgba(0, 0, 0, 0.55);
  color: #fff;
  backdrop-filter: blur(2px);
}
.att .rm:hover {
  background: rgba(0, 0, 0, 0.75);
}

.attach-error {
  font-size: 11px;
  color: var(--red);
}

.drop-hint {
  position: absolute;
  inset: -6px;
  z-index: 5;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  border-radius: 18px;
  border: 1.5px dashed color-mix(in srgb, var(--accent) 55%, transparent);
  background: color-mix(in srgb, var(--accent) 8%, var(--surface));
  color: var(--accent);
  font-size: 12.5px;
  font-weight: 500;
  pointer-events: none;
}

.file-input {
  display: none;
}

/* ---------- 输入框 ---------- */
.box {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 16px;
  box-shadow: var(--shadow-md);
  transition: border-color 0.18s var(--ease), box-shadow 0.18s var(--ease);
}
.box:focus-within {
  border-color: color-mix(in srgb, var(--accent) 48%, var(--line));
  box-shadow: 0 0 0 3.5px var(--accent-soft), var(--shadow-md);
}

textarea {
  display: block;
  width: 100%;
  min-height: 46px;
  max-height: 176px;
  padding: 13px 15px 4px;
  border: none;
  background: transparent;
  outline: none;
  resize: none;
  font-size: 13.5px;
  line-height: 1.62;
  color: var(--text);
  overflow-y: auto;
}
textarea::placeholder {
  color: var(--text-3);
}

.bar {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 3px 8px 8px 10px;
}

.chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 24px;
  padding: 0 8px;
  border-radius: 7px;
  font-size: 11.5px;
  color: var(--text-2);
  max-width: 210px;
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.chip span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.chip:hover {
  background: var(--hover);
  color: var(--text);
}
.chip.active {
  color: var(--accent);
  background: var(--accent-soft);
}
/* 录音中：红点常亮 + 呼吸，明确告诉用户麦克风还开着 */
.chip.rec {
  color: var(--red);
  background: color-mix(in srgb, var(--red) 12%, transparent);
  animation: rec-pulse 1.4s var(--ease) infinite;
}
.chip.rec:hover {
  background: color-mix(in srgb, var(--red) 18%, transparent);
  color: var(--red);
}
@keyframes rec-pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.55; }
}
.chip.icon {
  padding: 0 6px;
  position: relative;
}
/* 检测到有人说话：边框亮起来，让用户确认麦克风真的收到了 */
.chip.listening {
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 14%, transparent);
}
/* 排队待识别的段数：实时转写时文字是陆续出现的，这个数字说明还有多少在跑 */
.seg-badge {
  position: absolute;
  top: -5px;
  right: -5px;
  min-width: 13px;
  height: 13px;
  padding: 0 3px;
  border-radius: 7px;
  font-size: 9px;
  line-height: 13px;
  text-align: center;
  color: var(--white, #fff);
  background: var(--accent);
}
.chip.model {
  font-family: var(--font-mono);
  font-size: 11px;
}

/* ---------- 模型快切 ---------- */
.model-wrap {
  position: relative;
}

.model-menu {
  position: absolute;
  bottom: calc(100% + 8px);
  left: 0;
  z-index: 20;
  width: 250px;
  padding: 5px;
  border-radius: 11px;
  background: var(--surface);
  border: 1px solid var(--line);
  box-shadow: var(--shadow-lg);
}

.menu-title {
  padding: 6px 9px 5px;
  font-size: 10.5px;
  font-weight: 600;
  letter-spacing: 0.04em;
  color: var(--text-3);
  text-transform: uppercase;
}

.menu-item {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 7px 9px;
  border-radius: 7px;
  font-size: 12.5px;
  color: var(--text);
  text-align: left;
}
.menu-item:hover {
  background: var(--hover);
}
.menu-item.on {
  background: var(--accent-soft);
  color: var(--accent);
}
.menu-item .p-name {
  flex: none;
  max-width: 100px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.menu-item .p-model {
  margin-left: auto;
  font-family: var(--font-mono);
  font-size: 10.5px;
  color: var(--text-3);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.menu-item.manage {
  margin-top: 3px;
  padding-top: 8px;
  border-top: 1px solid var(--line-soft);
  border-radius: 0 0 7px 7px;
  color: var(--text-2);
  font-size: 12px;
}

.menu-empty {
  padding: 8px 9px 10px;
  font-size: 11.5px;
  color: var(--text-3);
}

.pop-enter-active,
.pop-leave-active {
  transition: opacity 0.14s var(--ease), transform 0.14s var(--ease);
}
.pop-enter-from,
.pop-leave-to {
  opacity: 0;
  transform: translateY(4px) scale(0.98);
}

.hint {
  margin-left: auto;
  font-size: 10.5px;
  color: var(--text-3);
  padding-right: 4px;
  user-select: none;
}

.send {
  width: 28px;
  height: 28px;
  flex: none;
  border-radius: 50%;
  display: grid;
  place-items: center;
  background: var(--accent);
  color: #fff;
  transition: background 0.16s var(--ease), transform 0.12s var(--ease), opacity 0.16s var(--ease);
}
.send:hover:not(:disabled) {
  background: var(--accent-hover);
}
.send:active:not(:disabled) {
  transform: scale(0.92);
}
.send:disabled {
  background: var(--fill-strong);
  color: var(--text-3);
  cursor: default;
}
.send.stop {
  background: var(--text);
  color: var(--surface);
}

@media (max-width: 760px) {
  .hint {
    display: none;
  }
}
</style>
