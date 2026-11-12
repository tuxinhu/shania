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
 * # 为什么播放时要丢弃识别结果
 *
 * 播放时麦克风里混着 AEC 没消干净的 TTS 残留，识别器会解出一些零碎词，
 * 甚至触发端点判定，让模型「自己接自己」。所以：
 *   - 播放/生成期间的 `final` 一律丢弃
 *   - 用户插话靠**能量**判定（`onBargeIn`），不靠文本
 *   - 判定插话后立刻重置识别器，后面收到的才是真正用户在说的话
 *
 * 代价是插话的头一两个字可能丢。这个取舍是故意的：丢一个字几乎察觉不到，
 * 「不能插话」却非常明显。
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

  push(delta: string): string[] {
    if (!delta) return []
    this.buf += delta
    const out: string[] = []
    // 上限保护：异常输入（模型把整篇文档吐进来）不能在这里死循环
    for (let guard = 0; guard < 8; guard += 1) {
      const cut = this.findCut()
      if (cut <= 0) break
      // 切点前一个字符不是标点时，说明是按硬上限强行截断的
      const marker = this.buf[cut - 1] ?? ''
      const forced = !SENTENCE_END.includes(marker) && !SOFT_BREAK.includes(marker)
      let sentence = this.buf.slice(0, cut).trim()
      this.buf = this.buf.slice(cut)
      this.first = false
      // 强行截断的地方补个逗号：合成器对没有标点的长句处理不好，
      // 最后一个字经常被吞（实测「把现象记」只剩「把现象」）
      if (forced && sentence) sentence += '，'

      // 代码块整段不念：块里的 `for line in buf` 在 Kokoro 的英文词表里查得到
      // for/in/line，会被一本正经念成英文；matcha 则删光英文只剩括号，白合成一段静音
      const fences = sentence.split('```').length - 1
      if (fences > 0) {
        if (fences % 2 === 1) this.inFence = !this.inFence
        continue
      }
      if (this.inFence || isTableRule(sentence)) continue
      // 只有标点的片段丢掉：合成器对它返回空音频，白白占一次 IPC
      if (sentence && speakable(sentence)) out.push(sentence)
      if (!this.buf) break
    }
    return out
  }

  flush(): string | null {
    const rest = this.buf.trim()
    this.buf = ''
    this.first = true
    // 代码块没闭合就到结尾了：剩下的同样不念
    const fenced = this.inFence
    this.inFence = false
    return !fenced && rest && speakable(rest) ? rest : null
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

  private state: CallState = {
    phase: 'idle',
    hint: '',
    reply: '',
    heard: '',
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
        if (this.busy) return
        this.state.hint = text
        this.emit()
      },
      onFinal: (text) => {
        // 播放/生成期间解出来的字是模型自己的声音，丢掉
        if (this.busy) return
        void this.runTurn(text)
      },
      onError: (err) => this.fail(err.message),
    })

    const engine = new AudioEngine({
      onFrame: (pcm) => this.asr?.feed(pcm),
      onBargeIn: () => this.interrupt(),
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

  /** 打断（用户点按钮，或播放中被检测到插话） */
  interrupt() {
    if (!this.busy) return

    this.turnId += 1
    this.generating = false
    if (this.requestId) void stopChat(this.requestId).catch(() => {})

    this.engine?.flush()
    this.splitter.reset()
    this.commit(true)
    this.pendingCommit = false

    this.state = { ...this.state, hint: '', reply: '', heard: '' }
    this.emit()

    // 刚才那半句被 TTS 残留污染了，重开一个新会话
    void this.asr?.restart(CALL_SILENCE).catch(() => {})
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
              for (const sentence of this.splitter.push(event.delta)) {
                this.speak(sentence, turn)
              }
              this.emit()
              break
            }
            case 'done': {
              const rest = this.splitter.flush()
              if (rest) this.speak(rest, turn)
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
        this.engine?.enqueue(speech.pcm, speech.rate)
        // 近似：入队即算「听到」。打断时可能多记一句，对话历史里无关紧要
        this.state.heard += sentence
        this.emit()
      })
      .catch((err) => {
        if (turn !== this.turnId) return
        this.fail(`语音合成失败：${err instanceof Error ? err.message : String(err)}`)
      })
      .finally(() => {
        this.ttsBusy = Math.max(0, this.ttsBusy - 1)
      })
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
    if (next !== this.state.phase) {
      this.state = { ...this.state, phase: next }
      this.emit()
    }
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
