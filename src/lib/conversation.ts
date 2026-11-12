/**
 * 全双工语音通话的状态机。
 *
 * # 一轮对话的完整流程
 *
 * ```
 * 麦克风 → X-ASR（连续） → 端点判定 → LLM 流式 → 分句 → TTS → 播放队列
 *                              ↑                                  │
 *                              └──── 插话：停播 + 停生成 + 重置识别 ┘
 * ```
 *
 * # 全双工在这里意味着什么
 *
 * 识别、生成、播放三路**同时**在跑，所以不能用「一个 phase 走到底」的串行状态机。
 * 核心是 `turnId`：每轮自增，所有异步回调进来先比对，不匹配就直接丢掉——
 * 被打断的轮次里飞在半路上的 TTS、chat 事件、ASR 结果都不会污染新一轮。
 *
 * # 播放时怎么区分「用户在说话」和「模型自己的回声」
 *
 * 播放时麦克风里混着 AEC 没消干净的 TTS 残留，识别器会把它解成零碎词，
 * 处理不好就会「自己接自己」。两个判据配合：
 *   - **能量**（`onBargeIn`）：快，但外放时阈值必须抬高，容易打不中
 *   - **识别文本**：慢半拍（要等端点），但「解出来的字是不是已经出现在这轮
 *     回答里」几乎能百分百区分回声和真人
 *
 * 只靠能量的时候，外放用户基本打断不了；所以文本这条必须留着——判定为真人
 * 就立刻打断，并把他刚说的这句话直接拿去开新一轮，不浪费。
 */

import { TTS_NOT_INSTALLED, stopChat, streamChat, ttsSay, ttsStatus, ttsWarmup } from './api'
import type { ChatConfigPayload } from './api'
import { AudioEngine } from './audio-engine'
import { STREAM_NOT_INSTALLED, StreamStt } from './stream-stt'
import type { TtsModel, WireMessage } from './types'

/** 通话的断句阈值，必须和 Rust 侧 `stream_stt::CALL_SILENCE` 保持一致 */
const CALL_SILENCE = 0.5

/**
 * 通话模式额外追加的系统指令。
 *
 * 不追加这一段，模型会把 markdown 表格、代码块原样吐出来，TTS 念出来就是一串
 * 「星号星号」「反引号」。而且默认的书面语回答动辄五六句，用户要等十几秒才听完，
 * 通话体验直接崩掉。
 */
export const CALL_SYSTEM_PROMPT = [
  '你现在正在和用户进行语音通话，你的回答会被立刻朗读出来，用户看不到文字。',
  '请遵守：',
  '1. 用口语说话，像打电话聊天，不要用书面语。',
  '2. 每次回答不超过三句话；用户要求详细说明时才展开。',
  '3. 不要输出任何 markdown：没有列表符号、没有加粗、没有代码块、没有标题、没有表格。',
  '4. 不要念文件路径、网址或代码，需要提到时用一句话概括。',
  '5. 数字、金额、时间用口语读法。',
].join('\n')

export type CallPhase = 'idle' | 'listening' | 'thinking' | 'speaking'

export interface CallState {
  phase: CallPhase
  /** 用户正在说的这句（实时覆盖，不是追加） */
  hint: string
  /** 模型这一轮回答的完整文本 */
  reply: string
  /** 已经交给播放队列的部分（历史里只写这个） */
  heard: string
  /** 麦克风电平（0-1），界面画电平条用 */
  level: number
  muted: boolean
  error: string
}

export interface CallTurn {
  user: string
  /** 用户实际听到的回答（被打断时是截断的） */
  assistant: string
  interrupted: boolean
}

export interface ConversationOptions {
  /** 每一轮现取配置，用户中途改设置也能生效 */
  buildConfig: () => ChatConfigPayload
  /** 把本轮用户说的话拼成完整消息列表（含历史） */
  buildMessages: (userText: string) => WireMessage[]
  /** 音色编号 */
  sid: number
  speed: number
  /** 用哪个合成模型，和设置里的 `ttsModel` 保持一致 */
  model: TtsModel
  onState: (state: CallState) => void
  /** 一轮结束（正常说完或被打断），外部据此落库 */
  onTurn: (turn: CallTurn) => void
}

/* ------------------------------------------------------------------ */
/* 分句                                                                */
/* ------------------------------------------------------------------ */

const SENTENCE_END = '。！？；!?;…\n'
const SOFT_BREAK = '，、：,:—'

/** 有没有能念出来的字。纯标点（「……」「——」）送进合成器只会白跑一趟 */
const SPEAKABLE = /[\p{L}\p{N}]/u

function speakable(text: string): boolean {
  return SPEAKABLE.test(text)
}

/**
 * 这个符号位置能不能切。
 *
 * 数字中间的逗号不能切：`1,234` 从这里断开会变成「一」和「234」两段，
 * 读出来是「一 二百三十四」——用户听到的就是「读错了一个数」。
 */
function canCutAt(buf: string, i: number): boolean {
  const ch = buf[i]
  if (ch === ',' || ch === '，') {
    const prev = buf[i - 1]
    const next = buf[i + 1]
    if (prev && next && prev >= '0' && prev <= '9' && next >= '0' && next <= '9') {
      return false
    }
  }
  return true
}

/**
 * `| --- | --- |` 这种表格分隔行。念不出来，还会白占一次合成。
 */
function isTableRule(text: string): boolean {
  return /^[\s|:\-—]+$/.test(text)
}

/**
 * 一次切分的结果。
 *
 * 分开保存是为了两件事互不干扰：`raw` 原样写进会话，`\n`、表格竖线、代码围栏都还在，
 * 会话里渲染出来还是原样的排版；`speech` 是清洗过、适合念的版本。
 * 只留 `speech` 会把整段回答压成一行——实测会话里就是这样：
 * 「## 目录扫描完成我看了 …的结构| 层 | 位置 | 职责 || --- | --- | ---|」。
 */
interface Segment {
  raw: string
  /** null 表示这段不念（代码块、表格分隔行、纯标点） */
  speech: string | null
}

/**
 * 把 LLM 的增量文本切成适合合成的句子。
 *
 * 和 `core/src/tts.rs` 里的 `Splitter` 是同一套规则，只是这边必须在浏览器里跑
 * （delta 在这里产生，绕一圈去 Rust 切要多一次 IPC 往返）。
 *
 * 规则：
 *   - 首句从第一个句末标点切，最短 6 字——用户等的是第一声，不是整段
 *   - 后续句取最后一个句末标点，合并得更多、语调更连贯
 *   - 没有标点时按软阈值（逗号）和硬上限兜底，避免长段无限等
 *   - ``` 代码块与表格分隔行整段跳过，不送进合成器
 */
class Splitter {
  private buf = ''
  private first = true
  /** 正在一段 ``` 代码块里 */
  private inFence = false

  push(delta: string): Segment[] {
    if (!delta) return []
    this.buf += delta
    const out: Segment[] = []
    // 上限保护：异常输入（模型把整篇文档吐进来）不能在这里死循环
    for (let guard = 0; guard < 8; guard += 1) {
      const cut = this.findCut()
      if (cut <= 0) break
      // 切点前一个字符不是标点时，说明是按硬上限强行截断的
      const marker = this.buf[cut - 1] ?? ''
      const forced = !SENTENCE_END.includes(marker) && !SOFT_BREAK.includes(marker)
      const raw = this.buf.slice(0, cut)
      this.buf = this.buf.slice(cut)
      this.first = false

      const text = raw.trim()
      // 强行截断的地方补个逗号：合成器对没有标点的长句处理不好，
      // 最后一个字经常被吞（实测「把现象记」只剩「把现象」）。
      // 只补在朗读版本上，会话里那份保持原样。
      const spoken = forced && text ? `${text}，` : text

      // 代码块整段不念：块里的 `for line in buf` 在 Kokoro 的英文词表里查得到
      // for/in/line，会被一本正经念成英文；matcha 则删光英文只剩括号，白合成一段静音
      const fences = text.split('```').length - 1
      if (fences > 0) {
        if (fences % 2 === 1) this.inFence = !this.inFence
        out.push({ raw, speech: null })
        continue
      }
      // 表格分隔行、纯标点、块内代码同理：会话里留着，但不念
      const quiet = this.inFence || isTableRule(spoken) || !spoken || !speakable(spoken)
      out.push({ raw, speech: quiet ? null : spoken })
    }
    return out
  }

  flush(): Segment | null {
    const raw = this.buf
    this.buf = ''
    this.first = true
    // 代码块没闭合就到结尾了：剩下的同样不念
    const fenced = this.inFence
    this.inFence = false
    if (!raw.trim()) return null
    const text = raw.trim()
    return { raw, speech: !fenced && speakable(text) ? text : null }
  }

  reset() {
    this.buf = ''
    this.first = true
    this.inFence = false
  }

  private findCut(): number {
    const buf = this.buf
    if (!buf) return -1

    const minLen = this.first ? 6 : 2
    const softLimit = this.first ? 16 : 40
    const hardLimit = this.first ? 28 : 70

    let firstEnd = -1
    let lastEnd = -1
    let soft = -1

    for (let i = 0; i < buf.length; i += 1) {
      const ch = buf[i]
      const len = i + 1
      if (SENTENCE_END.includes(ch)) {
        if (len >= minLen) {
          if (firstEnd < 0) firstEnd = len
          lastEnd = len
        }
      } else if (SOFT_BREAK.includes(ch) && len >= minLen && canCutAt(buf, i)) {
        soft = len
      }
    }

    // 首句要快：第一个句末标点就切
    if (this.first) {
      if (firstEnd > 0) return firstEnd
    } else {
      // 后续句合并到最后一个句末标点，但别把一整篇都攒成一段
      if (lastEnd > 0 && lastEnd <= hardLimit) return lastEnd
      if (firstEnd > 0) return firstEnd
    }

    if (buf.length >= softLimit && soft > 0) return soft
    if (buf.length >= hardLimit) return hardLimit
    return -1
  }
}

/* ------------------------------------------------------------------ */
/* 通话                                                                */
/* ------------------------------------------------------------------ */

export class VoiceConversation {
  private engine: AudioEngine | null = null
  private asr: StreamStt | null = null
  private splitter = new Splitter()

  /** 每轮自增。异步回调进来先比对，过期的一律丢弃 */
  private turnId = 0
  private requestId = ''
  private generating = false
  private currentUser = ''
  /** 这条链保证 TTS 请求按句序串行——并发发出去会乱序播放 */
  private ttsChain: Promise<void> = Promise.resolve()
  /** 正在合成/入队的句子数。用来判断「本轮是不是真的说完了」 */
  private ttsBusy = 0
  /**
   * `done` 已经收到，但还有句子在合成或播放中。
   *
   * 不能在 `done` 里直接提交历史：那时候 `ttsChain` 里通常还压着两三句，
   * 提交上去的回答会比用户实际听到的短一截，下一轮模型就会重复说这些内容。
   */
  private pendingCommit = false
  private ticker: number | null = null
  /**
   * 播放期间连续几帧 partial 被判成「不是回声」。
   *
   * 单帧不算数：识别器会先吐一两个字的碎片（「我」「我想」），也可能被 AEC
   * 残留带出一帧假文本。要连续两帧都像人话才动手，代价是晚 100-200ms，
   * 换来的是不会自己打断自己。
   */
  private busyHits = 0

  private state: CallState = {
    phase: 'idle',
    hint: '',
    reply: '',
    heard: '',
    level: 0,
    muted: false,
    error: '',
  }

  constructor(private opts: ConversationOptions) {}

  get snapshot(): CallState {
    return this.state
  }

  /** 播放或生成中：这期间识别结果不可信，也不算用户发言 */
  private get busy(): boolean {
    return this.generating || (this.engine?.isPlaying ?? false)
  }

  /**
   * 接通。
   *
   * 顺序是刻意的：先查模型（没装直接给出可执行的提示），再开麦克风
   * （权限弹窗要贴近用户的点击），最后起识别器。
   */
  async start(): Promise<void> {
    if (this.state.phase !== 'idle') return

    const status = await ttsStatus(this.opts.model)
    if (!status?.ready) {
      throw new Error(
        '语音合成模型还没下载：去「设置 → 语音输出」下载 Matcha（约 126MB），下完再打这通电话。',
      )
    }
    // 模型已在内存时是毫秒级；没加载则这里等 1-2 秒，好过第一句话卡住
    await ttsWarmup(this.opts.model).catch(() => {})

    const asr = new StreamStt({
      onPartial: (text) => {
        const clean = text.trim()

        if (this.busy) {
          // 播放/生成期间的 partial 是最快的打断信号：比等端点判定快半秒以上。
          // 判据是「这几个字有没有出现在本轮回答里」——回声必然出现，用户自己
          // 的话必然不出现。连续两帧都判成真人再动手，避开识别碎片的误判。
          if (clean.length >= 2 && !this.looksLikeEcho(clean)) {
            this.busyHits += 1
            if (this.busyHits >= 2) {
              this.busyHits = 0
              this.interrupt(true)
            }
          } else {
            this.busyHits = 0
          }
          return
        }

        this.busyHits = 0
        this.state.hint = clean
        this.emit()
      },
      onFinal: (text) => {
        const clean = text.trim()
        if (!clean) return

        // 播放/生成期间解出来的字，绝大多数是自己声音的回声，丢掉；
        // 但也可能真是用户在说话——这种情况下必须打断，而不是把他的话吞了。
        // 「说了话没反应」是通话里最难受的失败方式，比偶尔误判一次严重得多。
        if (this.busy) {
          if (this.looksLikeEcho(clean)) return
          this.interrupt(true)
          void this.runTurn(clean)
          return
        }
        void this.runTurn(clean)
      },
      onError: (err) => this.fail(err.message),
    })

    const engine = new AudioEngine({
      onFrame: (pcm) => this.asr?.feed(pcm),
      // 能量判定比识别快（100ms 就能凑够帧），但它只知道「有声音」，不知道是
      // 谁的声音。所以它负责「尽快停」，文字判定负责「确认是真人」。
      onBargeIn: () => this.interrupt(true),
      onError: (err) => this.fail(err.message),
    })

    this.state = { ...this.state, phase: 'listening', error: '' }
    this.emit()

    try {
      await engine.start()
      this.engine = engine
      await asr.start(CALL_SILENCE)
      this.asr = asr
    } catch (err) {
      await engine.stop().catch(() => {})
      this.state = { ...this.state, phase: 'idle' }
      this.emit()
      throw this.describe(err)
    }

    // 状态机的相位由「是否在生成 / 是否在出声」推导，不靠手动维护
    this.ticker = window.setInterval(() => this.tick(), 150)
  }

  /** 挂断 */
  async stop(): Promise<void> {
    if (this.ticker !== null) {
      window.clearInterval(this.ticker)
      this.ticker = null
    }

    // 先让所有在飞的回调失效
    this.turnId += 1
    this.generating = false
    if (this.requestId) void stopChat(this.requestId).catch(() => {})

    const engine = this.engine
    const asr = this.asr
    this.engine = null
    this.asr = null

    engine?.flush()
    await asr?.stop().catch(() => {})
    await engine?.stop().catch(() => {})

    this.commit(true)
    this.pendingCommit = false
    this.splitter.reset()
    this.state = { ...this.state, phase: 'idle', hint: '', muted: false }
    this.emit()
  }

  /**
   * 打断（用户点按钮，或播放中被检测到插话）。
   *
   * `keepAsr` 是这次改动里最要紧的一处：原来的实现打断时一律重启识别器，
   * 用意是清掉缓冲区里的回声。可打断的时机恰恰是**用户正在说**——重启等于
   * 把他已经出口的半句一起丢掉，体感就是「打断了，但我刚说的话没了，得重讲」。
   * 所以自动判定命中（能量或文字）时保留缓冲区，只清人工按钮才重启。
   */
  interrupt(keepAsr = false) {
    if (!this.busy) return

    this.turnId += 1
    this.generating = false
    if (this.requestId) void stopChat(this.requestId).catch(() => {})

    this.engine?.flush()
    this.splitter.reset()
    this.commit(true)
    this.pendingCommit = false
    this.busyHits = 0

    this.state = { ...this.state, hint: '', reply: '', heard: '' }
    this.emit()

    if (!keepAsr) {
      // 剩下半句被 TTS 残留污染了，重开一个新会话（用户点按钮时走这条）
      void this.asr?.restart(CALL_SILENCE).catch(() => {})
    }
  }

  setMuted(value: boolean) {
    this.engine?.setMuted(value)
    this.state = { ...this.state, muted: value }
    this.emit()
  }

  /* ------------------------------ 内部 ------------------------------ */

  private async runTurn(userText: string) {
    const turn = (this.turnId += 1)
    const requestId = `call-${Date.now()}-${turn}`
    this.requestId = requestId
    this.generating = true
    this.currentUser = userText
    this.splitter.reset()

    this.state = {
      ...this.state,
      phase: 'thinking',
      hint: '',
      reply: '',
      heard: '',
      error: '',
    }
    this.emit()

    const messages = this.opts.buildMessages(userText)

    try {
      await streamChat(
        { requestId, config: this.opts.buildConfig(), messages },
        (event) => {
          // 被打断的轮次：迟到的事件全部丢掉
          if (turn !== this.turnId) return

          switch (event.type) {
            case 'content': {
              this.state.reply += event.delta
              for (const segment of this.splitter.push(event.delta)) {
                this.hear(segment)
                if (segment.speech) this.speak(segment.speech, turn)
              }
              this.emit()
              break
            }
            case 'done': {
              const rest = this.splitter.flush()
              if (rest) {
                this.hear(rest)
                if (rest.speech) this.speak(rest.speech, turn)
              }
              this.generating = false
              // 真正的提交等播放队列排空（见 tick）
              this.pendingCommit = true
              break
            }
            case 'error': {
              this.generating = false
              this.state.error = event.message
              this.emit()
              break
            }
            default:
              // round / reasoning / tool* 在通话里不朗读，也不显示
              break
          }
        },
      )
    } catch (err) {
      if (turn !== this.turnId) return
      this.generating = false
      this.fail(err instanceof Error ? err.message : String(err))
    }
  }

  /**
   * 记下「用户听到了这段」。
   *
   * 写的是原文片段，不是清洗后的朗读稿：清洗会把 `\n`、表格竖线、代码围栏都吃掉，
   * 拼起来就是一根长条（「## 目录扫描完成我看了…| 层 | 位置 | 职责 || --- |」），
   * 会话里再渲染就跟没格式一样。打断时这份截断的记录也仍然是完整的 markdown。
   */
  private hear(segment: Segment) {
    this.state.heard += segment.raw
  }

  /**
   * 合成一句并排进播放队列。
   *
   * 走串行链：`tts_say` 是异步的，两句并发发出去回来的顺序不保证，
   * 播放就会串词。
   */
  private speak(sentence: string, turn: number) {
    this.ttsBusy += 1
    this.ttsChain = this.ttsChain
      .then(async () => {
        if (turn !== this.turnId) return
        const speech = await ttsSay(sentence, this.opts.sid, this.opts.speed, this.opts.model)
        // 合成期间可能已经被打断
        if (turn !== this.turnId) return
        // 空音频不是「没声音」这种小事：正常句子不该返回 0 采样，
        // 静默丢掉会变成「通话里凭空少了一句」，必须报出来
        if (!speech.pcm || speech.durationMs <= 0) {
          this.fail('语音合成返回了空音频：这一句没念出来（换模型重试或看日志）')
          return
        }
        this.engine?.enqueue(speech.pcm, speech.rate)
      })
      .catch((err) => {
        if (turn !== this.turnId) return
        this.fail(`语音合成失败：${err instanceof Error ? err.message : String(err)}`)
      })
      .finally(() => {
        this.ttsBusy = Math.max(0, this.ttsBusy - 1)
      })
  }

  /**
   * 这段识别结果像不像「模型自己的声音」。
   *
   * 播放时麦克风里混着 AEC 没消干净的残留，识别器会把它解成文字。判据是
   * 「这段字是不是已经出现在这一轮的回答里」——回声必然来自已经说过的内容。
   * 识别结果和原文常差一两个字，所以再按双字片段算一次命中率。
   */
  private looksLikeEcho(text: string): boolean {
    const norm = (s: string) => s.replace(/[\s\p{P}\p{S}]/gu, '').toLowerCase()
    const said = norm(text)
    const reply = norm(this.state.reply)
    // 空回答（正在思考，还没吐字）说明不可能有回声；一个字的短句也判不了，
    // 宁可当成用户在说话
    if (said.length < 2 || !reply) return false
    if (reply.includes(said)) return true

    const grams: string[] = []
    for (let i = 0; i + 2 <= said.length; i += 1) grams.push(said.slice(i, i + 2))
    if (grams.length === 0) return false
    const hit = grams.filter((g) => reply.includes(g)).length
    return hit / grams.length >= 0.6
  }

  /** 把这一轮写进历史。`heard` 而不是 `reply`：用户没听到的不该出现在上下文里 */
  private commit(interrupted: boolean) {
    const user = this.currentUser
    if (!user) return
    this.currentUser = ''
    this.opts.onTurn({
      user,
      assistant: this.state.heard,
      interrupted,
    })
  }

  private tick() {
    const playing = this.engine?.isPlaying ?? false

    // 说完 + 合成完 + 播完，才算这一轮真正结束
    if (this.pendingCommit && !this.generating && !playing && this.ttsBusy === 0) {
      this.pendingCommit = false
      this.commit(false)
    }

    const next: CallPhase = playing ? 'speaking' : this.generating ? 'thinking' : 'listening'
    // 电平每帧都变，所以这里无条件 emit：150ms 一次的小对象拷贝，比让界面
    // 看着像「没听见你说话」便宜得多
    this.state = { ...this.state, phase: next, level: this.engine?.level ?? 0 }
    this.emit()
  }

  private fail(message: string) {
    this.state = { ...this.state, error: message }
    this.emit()
  }

  private emit() {
    this.opts.onState({ ...this.state })
  }

  private describe(err: unknown): Error {
    const msg = err instanceof Error ? err.message : String(err)
    if (msg.includes(STREAM_NOT_INSTALLED)) {
      return new Error('识别模型还没下载：去「设置 → 语音输入」点「下载模型（约 134MB）」')
    }
    if (msg.includes(TTS_NOT_INSTALLED)) {
      return new Error('语音合成模型还没下载：去「设置 → 语音输出」下载后重试')
    }
    if (/NotAllowed|Permission|denied/i.test(msg)) {
      return new Error('麦克风权限被拒绝：允许后重新点电话按钮')
    }
    return new Error(msg)
  }
}
