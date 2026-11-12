/**
 * 实时录音 + 静音切句。
 *
 * 原来的做法是 MediaRecorder 录完整个 webm 再一次性转写，用户得等「停止录音」之后
 * 几秒才看到字。这里改成：直接拿原始 PCM，边录边用能量判断「这句话说完了」，
 * 一句话结束就把这一小段送去转写——用户还在说第二句时，第一句的字已经出现了。
 *
 * 三个必须自己做的事（whisper-server 不提供流式接口）：
 *   1. 采集：AudioWorklet 拿 float32 帧，重采样到 16k 交给 VAD
 *   2. 切句：RMS 低于自适应阈值且持续 `silenceMs` 就认为一句结束
 *   3. 编码：每段直接写 WAV（16k/16bit/单声道），不经过 webm/ffmpeg
 *
 * 切句参数是「延迟 vs 完整度」的权衡：静音判定越长越不容易把一个词切成两半，
 * 但字幕出现得越晚。500ms 是中文口语比较稳的值。
 *
 * 想做到「边说边出字」，光靠缩短 `silenceMs` 不够——一口气说 10 秒没有停顿，
 * 就得等 10 秒才有字。所以另有 `onPartial`：说话过程中每隔 `partialIntervalMs`
 * 把「当前这段到目前为止的音频」整体快照出去，调用方送去做一次临时转写，
 * 结果直接覆盖显示。等真正切段时再给一次最终结果。等于滑动窗口重新解码，
 * 代价是同一段音频会算几遍（CUDA 上 3 秒音频只要 120ms，完全算得过来）。
 */

const TARGET_RATE = 16000
/** Worklet 每帧 128 采样；累积到 1024（64ms）算一次能量，减少抖动 */
const ENERGY_WINDOW = 1024
/**
 * 流式出帧粒度：1600 采样 = 100ms。
 *
 * 太小（比如每 128 采样一帧）会让 IPC 每秒跑 125 次，base64+JSON 的开销比推理还大；
 * 太大则首字延迟和定稿延迟一起变长。100ms 在两者之间。
 */
const FRAME_CHUNK = 1600

export interface SegmentOptions {
  /** 判定「说完了」需要的静音时长 */
  silenceMs?: number
  /**
   * 允许切段的最短长度。比这个短的停顿**不算句末**，继续往下攒。
   *
   * 这是被真实数据打回来加上的：把 `silenceMs` 压到 280ms 后，用户正常说话
   * 被切成一堆 0.9-1.3 秒的碎片——服务端日志里 53 次请求有 23 次不到 1 秒，
   * 而且 29 次被 VAD 判定「没有语音」直接丢掉。碎片的两个后果都很致命：
   * 模型没有上下文，识别质量掉一截；短片段中文经常不带标点，几段拼起来
   * 就是一坨连在一起的字，没法读。
   */
  minCutMs?: number
  /** 一句最长多久：超过就硬切，避免服务端 audio_ctx 装不下 */
  maxSegmentMs?: number
  /** 太短的段直接丢掉（咳嗽、碰麦） */
  minSegmentMs?: number
  /** 检测到语音后，向前补一点，避免吞掉首字 */
  prerollMs?: number
  /** 说话过程中多久吐一次临时快照（0 = 关闭实时预览） */
  partialIntervalMs?: number
  /**
   * 快照至少要有这么长的音频才发。实测 1.2 秒以下的片段 whisper 一个标点都不给，
   * 服务端 VAD 也经常判「无语音」直接丢弃——白发一趟请求，白占一次推理。
   */
  minPartialMs?: number
}

export interface SegmenterCallbacks {
  /** 一段话结束（或硬切），拿到的已经是 16k 单声道 PCM */
  onSegment: (pcm: Float32Array, durationMs: number) => void
  /**
   * 当前这一段的临时快照：从段首到此刻，**不会**重置分段状态。
   * 用来边说话边显示结果，内容随时会被下一次快照改写。
   */
  onPartial?: (pcm: Float32Array, durationMs: number) => void
  /**
   * 连续的原始音频流，每约 100ms 一块，**不做任何切分**。
   *
   * 给流式识别用（`lib/stream-stt.ts`）：那边由模型自己判断句末，需要的是一条
   * 不断的音频流，而不是按能量切好的碎片——切碎了反而丢上下文。
   * 和 `onSegment`/`onPartial` 是两条**互斥**的路子，接了这个就别再用那个。
   */
  onFrame?: (pcm: Float32Array) => void
  /** VAD 状态变化，用来在界面上显示「正在说话 / 等待中」 */
  onSpeechChange?: (speaking: boolean) => void
  onError?: (err: Error) => void
}

const DEFAULTS: Required<SegmentOptions> = {
  silenceMs: 500,
  minCutMs: 1200,
  maxSegmentMs: 8000,
  minSegmentMs: 320,
  prerollMs: 200,
  partialIntervalMs: 0,
  minPartialMs: 0,
}

/** AudioWorklet 只负责把麦克风数据递出来，判断逻辑全在主线程，改参数不用重建节点 */
const WORKLET_CODE = `
class PcmTap extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0] && inputs[0][0]
    if (ch) this.port.postMessage(ch.slice(0))
    return true
  }
}
registerProcessor('pcm-tap', PcmTap)
`

/** 模块 URL 只建一次：每次录音都新建再 revoke 纯属浪费，内容又不会变 */
let cachedWorkletUrl = ''
function workletUrl(): string {
  if (!cachedWorkletUrl) {
    cachedWorkletUrl = URL.createObjectURL(new Blob([WORKLET_CODE], { type: 'text/javascript' }))
  }
  return cachedWorkletUrl
}

export class PcmSegmenter {
  private ctx: AudioContext | null = null
  private stream: MediaStream | null = null
  private node: AudioWorkletNode | null = null
  private source: MediaStreamAudioSourceNode | null = null

  private buf: Float32Array[] = []
  private bufLen = 0
  /** 当前段的 PCM（已包含 preroll） */
  private segment: Float32Array[] = []
  private segmentLen = 0
  private preRoll: Float32Array[] = []

  private speaking = false
  private silenceRun = 0
  private noiseFloor = 0.004
  private calibrated = 0
  private running = false
  /** 当前段上次抛临时快照时的长度（毫秒），控制快照频率 */
  private partialAt = 0
  /** 流式出帧的累积缓冲（只在接了 onFrame 时使用） */
  private outBuf: Float32Array[] = []
  private outLen = 0

  private opts: Required<SegmentOptions>
  private cbs: SegmenterCallbacks

  constructor(cbs: SegmenterCallbacks, opts: SegmentOptions = {}) {
    this.cbs = cbs
    this.opts = { ...DEFAULTS, ...opts }
  }

  async start() {
    if (this.running) return

    // 麦克风和 AudioContext 互不依赖，并行起。串行做的话光是等设备打开就要
    // 200-400ms，用户点完麦克风会觉得「没反应」。
    const ctxTask = (async () => {
      // 直接要 16k：Chromium 会做重采样，省掉我们自己写 resampler
      const c = new AudioContext({ sampleRate: TARGET_RATE })
      if (c.state === 'suspended') await c.resume()
      return c
    })()

    let stream: MediaStream
    let ctx: AudioContext
    try {
      ;[stream, ctx] = await Promise.all([
        navigator.mediaDevices.getUserMedia({
          audio: {
            channelCount: 1,
            // 关掉浏览器侧的自动增益和降噪：它们会把静音段的底噪抬起来，
            // 让「有没有人说话」变得难判断，也容易在句子间隙产生虚假能量
            echoCancellation: true,
            noiseSuppression: false,
            autoGainControl: false,
          },
        }),
        ctxTask,
      ])
    } catch (err) {
      // 麦克风被拒时 AudioContext 可能已经建好了，别漏掉
      void ctxTask.then((c) => c.close()).catch(() => {})
      throw err
    }

    this.stream = stream
    this.ctx = ctx

    if (!ctx.audioWorklet) {
      await ctx.close()
      this.ctx = null
      stream.getTracks().forEach((track) => track.stop())
      this.stream = null
      throw new Error('当前环境不支持实时音频处理，无法边录边转')
    }

    await ctx.audioWorklet.addModule(workletUrl())

    this.node = new AudioWorkletNode(ctx, 'pcm-tap')
    this.node.port.onmessage = (event) => this.feed(event.data as Float32Array)

    this.source = ctx.createMediaStreamSource(stream)
    this.source.connect(this.node)
    // 不接 destination：接了会把麦克风回放出来，形成啸叫
    this.partialAt = 0
    this.outBuf = []
    this.outLen = 0
    this.running = true
  }

  /** 停止采集，并把最后一段（如果够长）交出去 */
  async stop() {
    this.running = false
    this.flush()
    this.flushFrames()

    this.node?.port.close()
    this.node?.disconnect()
    this.source?.disconnect()
    this.stream?.getTracks().forEach((track) => track.stop())
    if (this.ctx && this.ctx.state !== 'closed') await this.ctx.close()

    this.node = null
    this.source = null
    this.stream = null
    this.ctx = null
    this.buf = []
    this.bufLen = 0
    this.preRoll = []
    this.speaking = false
  }

  /** 把主线程累积的采样按窗口切块做能量判断 */
  private feed(frame: Float32Array) {
    if (!this.running) return
    // 流式出帧用的是**原始**帧，和能量切句是两条独立的路：切句逻辑后面还会
    // 按 1024 窗口重排一遍，不能让它影响送出去的音频流
    this.emitFrame(frame)

    this.buf.push(frame)
    this.bufLen += frame.length

    while (this.bufLen >= ENERGY_WINDOW) {
      const chunk = this.take(ENERGY_WINDOW)
      this.process(chunk)
    }
  }

  /** 从缓冲区取 n 个采样，跨帧拼接 */
  private take(n: number): Float32Array {
    const out = new Float32Array(n)
    let offset = 0
    while (offset < n) {
      const head = this.buf[0]
      const want = n - offset
      if (head.length <= want) {
        out.set(head, offset)
        offset += head.length
        this.buf.shift()
      } else {
        out.set(head.subarray(0, want), offset)
        this.buf[0] = head.subarray(want)
        offset += want
      }
    }
    this.bufLen -= n
    return out
  }

  private process(chunk: Float32Array) {
    const rms = rootMeanSquare(chunk)
    const ms = (chunk.length / TARGET_RATE) * 1000

    // 开头 300ms 当作环境底噪采样。取**最小值**而不是最大值：用户很可能点完麦克风
    // 立刻就开口，用最大值会把说话的能量当成底噪，阈值被抬到判不出静音，句子永远切不开。
    if (this.calibrated < 300) {
      if (this.calibrated === 0) {
        this.noiseFloor = rms
      } else {
        this.noiseFloor = Math.min(this.noiseFloor, rms)
      }
      this.calibrated += ms
      // 校准期的音频留着当 preroll，不能丢——直接 return 会吞掉用户说的第一个字。
      // 多留几块（约 500ms），刚好覆盖「校准期里已经在说话」的情况。
      this.preRoll.push(chunk)
      const keepRaw = Math.ceil(this.opts.prerollMs / ms) + 8
      while (this.preRoll.length > keepRaw) this.preRoll.shift()
      return
    }

    // 上限 0.06：底噪估高了也不会把正常说话判成静音（人说话的 RMS 通常 0.05-0.2）
    const threshold = this.threshold()
    const voiced = rms > threshold

    if (voiced) {
      if (!this.speaking) {
        this.speaking = true
        this.cbs.onSpeechChange?.(true)
        // 把 preroll 拼到本段开头，防止「你好」的「你」被切在前面
        for (const p of this.preRoll) {
          this.segment.push(p)
          this.segmentLen += p.length
        }
        this.preRoll = []
      }
      this.segment.push(chunk)
      this.segmentLen += chunk.length
      this.silenceRun = 0
      this.emitPartial()
    } else if (this.speaking) {
      // 静音也要收进本段，尾部留一点静音对 whisper 更友好
      this.segment.push(chunk)
      this.segmentLen += chunk.length
      this.silenceRun += ms

      if (this.silenceRun >= this.opts.silenceMs) {
        if (this.segmentLen / TARGET_RATE * 1000 >= this.opts.minCutMs) {
          this.cut()
          return
        }
        // 段还太短：这次停顿只是句内换气，不是句末。切出去的话模型拿到的是
        // 一个没有上下文的碎渣，识别质量差、还没标点，几段拼起来读不通。
        this.silenceRun = 0
      }
    } else {
      // 没在说话：滚动保留最后一小段，供下一句做 preroll
      this.preRoll.push(chunk)
      const keep = Math.ceil(this.opts.prerollMs / ms)
      while (this.preRoll.length > keep) this.preRoll.shift()
    }

    // 一口气说太久（没停顿）就硬切，否则服务端的 audio_ctx 会吃不下
    if (this.speaking && this.segmentLen / TARGET_RATE * 1000 >= this.opts.maxSegmentMs) {
      this.cut()
    }
  }

  /**
   * 说话过程中每隔 `partialIntervalMs` 抛一次当前段的快照。
   *
   * 快照是「从段首到此刻」的**完整**音频，不是增量——whisper 没有上下文记忆，
   * 只给后半截会丢词、标点和用词都会飘。整段重解码才能保证显示的字一直稳定。
   * 代价是同一段要算好几遍，所以频率不能太高；调用方还会做「上一次没算完就跳过」。
   */
  private emitPartial() {
    const interval = this.opts.partialIntervalMs
    if (!this.cbs.onPartial || interval <= 0) return
    const elapsed = (this.segmentLen / TARGET_RATE) * 1000
    // 太短就别发：第一次快照原本落在 900ms，那会儿音频既不够识别（无标点、
    // 容易错字），服务端 VAD 还有一半概率直接判「无语音」丢掉
    if (elapsed < this.opts.minPartialMs) return
    if (elapsed - this.partialAt < interval) return
    // 离硬切只差一点点了，等最终结果更准，别白算一遍
    if (elapsed >= this.opts.maxSegmentMs - interval / 2) return
    this.partialAt = elapsed
    this.cbs.onPartial(concat(this.segment, this.segmentLen), elapsed)
  }

  /** 结束当前段并发出去 */
  private cut() {
    const pcm = concat(this.segment, this.segmentLen)
    const durationMs = (pcm.length / TARGET_RATE) * 1000
    this.segment = []
    this.segmentLen = 0
    this.silenceRun = 0
    this.partialAt = 0
    this.speaking = false
    this.cbs.onSpeechChange?.(false)

    if (durationMs < this.opts.minSegmentMs) return
    // 整段平均能量太低，说明这次是被键盘、咳嗽、风扇之类的噪声误触发的。
    // 这种段送进去 whisper 最爱编东西——「(字幕:xxx)」「谢谢观看」「你不要再说了」
    // 都是这么来的，不如根本不解。0.02 是留给轻声说话的余量：人说话再怎么轻
    // 平均 RMS 也在 0.03 以上，而空调/风扇的底噪撑不到 0.02。
    if (rootMeanSquare(pcm) < Math.max(this.threshold(), 0.02)) return
    this.cbs.onSegment(pcm, durationMs)
  }

  /** 判定「这一小块是不是有声音」的能量线，跟着底噪走 */
  private threshold(): number {
    return Math.min(Math.max(this.noiseFloor * 2.5, 0.012), 0.06)
  }

  /**
   * 累积到 `FRAME_CHUNK` 就抛一块出去。没接 `onFrame` 时什么都不做，
   * 走 whisper 那条路时不会有任何额外开销。
   */
  private emitFrame(frame: Float32Array) {
    if (!this.cbs.onFrame) return
    this.outBuf.push(frame)
    this.outLen += frame.length
    if (this.outLen < FRAME_CHUNK) return
    const chunk = concat(this.outBuf, this.outLen)
    this.outBuf = []
    this.outLen = 0
    this.cbs.onFrame(chunk)
  }

  /** 停止时把不足一块的残余也送出去，否则最后 100ms 里的字会丢 */
  private flushFrames() {
    if (this.outLen > 0 && this.cbs.onFrame) {
      this.cbs.onFrame(concat(this.outBuf, this.outLen))
    }
    this.outBuf = []
    this.outLen = 0
  }

  /** 停止录音时把手上剩余的音频也送出去 */
  private flush() {
    if (this.bufLen > 0) {
      const rest = this.take(this.bufLen)
      if (this.speaking) {
        this.segment.push(rest)
        this.segmentLen += rest.length
      }
    }
    if (this.segmentLen > 0) this.cut()
  }
}

function rootMeanSquare(data: Float32Array): number {
  let sum = 0
  for (let i = 0; i < data.length; i += 1) sum += data[i] * data[i]
  return Math.sqrt(sum / data.length)
}

function concat(parts: Float32Array[], total: number): Float32Array {
  const out = new Float32Array(total)
  let offset = 0
  for (const part of parts) {
    out.set(part, offset)
    offset += part.length
  }
  return out
}

/** 16k / 16bit / 单声道 WAV；whisper-server 收到 wav 就不需要 ffmpeg 了 */
export function pcmToWav(pcm: Float32Array): ArrayBuffer {
  const dataBytes = pcm.length * 2
  const view = new DataView(new ArrayBuffer(44 + dataBytes))

  writeAscii(view, 0, 'RIFF')
  view.setUint32(4, 36 + dataBytes, true)
  writeAscii(view, 8, 'WAVE')
  writeAscii(view, 12, 'fmt ')
  view.setUint32(16, 16, true)
  view.setUint16(20, 1, true)
  view.setUint16(22, 1, true)
  view.setUint32(24, TARGET_RATE, true)
  view.setUint32(28, TARGET_RATE * 2, true)
  view.setUint16(32, 2, true)
  view.setUint16(34, 16, true)
  writeAscii(view, 36, 'data')
  view.setUint32(40, dataBytes, true)

  let offset = 44
  for (let i = 0; i < pcm.length; i += 1) {
    const s = Math.max(-1, Math.min(1, pcm[i]))
    view.setInt16(offset, s < 0 ? s * 0x8000 : s * 0x7fff, true)
    offset += 2
  }
  return view.buffer
}

function writeAscii(view: DataView, offset: number, text: string) {
  for (let i = 0; i < text.length; i += 1) view.setUint8(offset + i, text.charCodeAt(i))
}
