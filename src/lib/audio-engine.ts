/**
 * 通话音频引擎：采集、播放、打断判定，全部跑在**同一个 AudioContext** 上。
 *
 * # 为什么要合并成一个上下文
 *
 * Chromium 的回声消除（AEC3）在 `getUserMedia` 里靠 `echoCancellation: true` 打开，
 * 但它需要「本页面正在播放什么」作为参考信号，才能从麦克风信号里减掉自己说的话。
 * 如果播放走另一个 AudioContext、或者干脆走系统播放器，参考信号就断了，
 * AEC 退化成无效——全双工立刻变成自问自答的死循环。
 *
 * 所以这里固定 48kHz（Chromium 音频栈的原生工作频率，AEC3 也在这一档），
 * 录音和放音共用它；16k 只是喂给识别器的那一路，自己降采样。
 *
 * # 和 `pcm-recorder.ts` 的区别
 *
 * 那个是给「聊天输入框的语音输入」用的：16k 上下文、只采集不播放、靠能量切句
 * 驱动界面。它不能兼这个差事——16k 的上下文播 24k 的合成音频会先降采样再升采样，
 * 声音发闷，而且 AEC 的参考信号链也不对。两个功能、两条链路，不是重复实现。
 */

/** 音频上下文的采样率。固定值，别改成跟随硬件——48k 才能整除到 16k */
const CTX_RATE = 48000
/** 识别器要的采样率 */
const ASR_RATE = 16000
/** 48k → 16k，整数比 3:1，可以直接块均值抗混叠 */
const DOWN = CTX_RATE / ASR_RATE
/** 每次 IPC 送 100ms。太小会让 IPC 次数比推理还费，太大首字延迟变高 */
const FRAME = ASR_RATE / 10

/** 播放中判定「用户插话」需要的连续有声帧数（100ms/帧） */
const BARGE_FRAMES = 3
/** 一次打断之后多久内不再判定，避免自己打断自己 */
const BARGE_COOLDOWN_MS = 700

export interface AudioEngineCallbacks {
  /** 连续的 16k 单声道 PCM，每 100ms 一块，不做任何切分（断句交给模型 endpoint） */
  onFrame: (pcm: Float32Array) => void
  /** 用户是否在说话（能量判定），只用于界面 */
  onSpeech?: (speaking: boolean) => void
  /** 播放中被判定为用户插话 */
  onBargeIn?: () => void
  onError?: (err: Error) => void
}

/** AudioWorklet 只负责把麦克风数据递出来，所有判断在主线程做，改参数不用重建节点 */
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

let cachedWorkletUrl = ''
function workletUrl(): string {
  if (!cachedWorkletUrl) {
    cachedWorkletUrl = URL.createObjectURL(new Blob([WORKLET_CODE], { type: 'text/javascript' }))
  }
  return cachedWorkletUrl
}

export class AudioEngine {
  private ctx: AudioContext | null = null
  private stream: MediaStream | null = null
  private node: AudioWorkletNode | null = null
  private source: MediaStreamAudioSourceNode | null = null

  /** 降采样时跨帧残留的采样（每帧 128 不是 3 的整数倍，会剩 1-2 个） */
  private residue: Float32Array = new Float32Array(0)
  /** 累积到 100ms 再抛 */
  private outBuf: Float32Array[] = []
  private outLen = 0

  /** 正在出声的播放节点。打断时要逐个 stop */
  private playing = new Set<AudioBufferSourceNode>()
  /**
   * 下一块音频的排程时间。连播时用它把句子首尾接上——
   * 每次都 `start(now)` 会在句间留出几毫秒空隙，听感是「一顿一顿」的。
   */
  private nextStart = 0
  /** 待播队列（已解码成 AudioBuffer） */
  private queue: AudioBuffer[] = []

  /** 环境底噪，用于自适应阈值 */
  private noiseFloor = 0.004
  private calibrated = 0
  /** 连续超出打断阈值的帧数 */
  private loudRun = 0
  private lastBargeAt = 0
  private speaking = false
  private muted = false
  private running = false

  private cbs: AudioEngineCallbacks

  constructor(cbs: AudioEngineCallbacks) {
    this.cbs = cbs
  }

  get active(): boolean {
    return this.running
  }

  /** 当前是否在出声（界面的「说话中」状态） */
  get isPlaying(): boolean {
    return this.playing.size > 0 || this.queue.length > 0
  }

  async start(): Promise<void> {
    if (this.running) return

    const ctx = new AudioContext({ sampleRate: CTX_RATE })
    if (ctx.state === 'suspended') await ctx.resume()

    let stream: MediaStream
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          channelCount: 1,
          // 这一行是全双工的前提。关掉它，模型会把自己的声音识别成用户输入。
          echoCancellation: true,
          // 降噪会削掉轻声和辅音，识别准确率反而下降，交给模型自己抗噪
          noiseSuppression: false,
          // 自动增益对「离麦远近不定」的通话场景有帮助，且不影响 AEC
          autoGainControl: true,
        },
      })
    } catch (err) {
      await ctx.close()
      throw err
    }

    if (!ctx.audioWorklet) {
      await ctx.close()
      stream.getTracks().forEach((t) => t.stop())
      throw new Error('当前环境不支持实时音频处理')
    }

    await ctx.audioWorklet.addModule(workletUrl())

    this.ctx = ctx
    this.stream = stream
    this.node = new AudioWorkletNode(ctx, 'pcm-tap')
    this.node.port.onmessage = (e) => this.onRaw(e.data as Float32Array)
    this.source = ctx.createMediaStreamSource(stream)
    this.source.connect(this.node)
    // 刻意不接 destination：接了就是把麦克风原样放出来，啸叫

    this.residue = new Float32Array(0)
    this.outBuf = []
    this.outLen = 0
    this.playing.clear()
    this.queue = []
    this.nextStart = 0
    this.loudRun = 0
    this.noiseFloor = 0.004
    this.calibrated = 0
    this.muted = false
    this.running = true
  }

  async stop(): Promise<void> {
    this.running = false
    this.flush()

    this.node?.port.close()
    this.node?.disconnect()
    this.source?.disconnect()
    this.stream?.getTracks().forEach((t) => t.stop())
    const ctx = this.ctx
    if (ctx && ctx.state !== 'closed') await ctx.close()

    this.node = null
    this.source = null
    this.stream = null
    this.ctx = null
    this.outBuf = []
    this.outLen = 0
    this.residue = new Float32Array(0)
    this.speaking = false
  }

  /** 静音：不再往识别器送帧，但仍保留能量检测（插话要能打断静音） */
  setMuted(value: boolean) {
    this.muted = value
    if (value && this.speaking) {
      this.speaking = false
      this.cbs.onSpeech?.(false)
    }
  }

  /* ------------------------------ 播放 ------------------------------ */

  /**
   * 把一段合成好的 PCM 排进播放队列。
   *
   * `pcm` 是 base64 的小端 i16（Rust 侧 `Audio::to_pcm_base64` 的输出），
   * `rate` 是这段音频自己的采样率（Kokoro 是 24k）。
   *
   * AudioBuffer 的采样率不必等于上下文采样率——Chromium 播放时会自动重采样。
   */
  enqueue(pcm: string, rate: number) {
    if (!this.ctx || !pcm) return
    const samples = decodePcm(pcm)
    if (samples.length === 0) return

    const buffer = this.ctx.createBuffer(1, samples.length, rate)
    // 用 getChannelData().set() 而不是 copyToChannel：后者在 TS 5.7 起的
    // TypedArray 泛型下要求 ArrayBuffer（非 SharedArrayBuffer），会报类型不符
    buffer.getChannelData(0).set(samples)
    this.queue.push(buffer)
    this.pump()
  }

  /** 立即停掉所有声音并清空队列（打断 / 挂断） */
  flush() {
    this.queue = []
    for (const source of this.playing) {
      try {
        source.onended = null
        source.stop()
      } catch {
        // 已经播完的节点再 stop 会抛，忽略
      }
    }
    this.playing.clear()
    this.nextStart = 0
  }

  private pump() {
    const ctx = this.ctx
    if (!ctx) return

    while (this.queue.length > 0) {
      const buffer = this.queue.shift()!
      const source = ctx.createBufferSource()
      source.buffer = buffer
      source.connect(ctx.destination)

      // 留 20ms 余量，避免「排程时间已经过去」导致 start 立刻播放（会有爆音）
      const at = Math.max(ctx.currentTime + 0.02, this.nextStart)
      source.start(at)
      this.nextStart = at + buffer.duration

      this.playing.add(source)
      source.onended = () => {
        this.playing.delete(source)
      }
    }
  }

  /* ------------------------------ 采集 ------------------------------ */

  private onRaw(frame: Float32Array) {
    if (!this.running) return

    const pcm = this.downsample(frame)
    if (pcm.length === 0) return

    this.outBuf.push(pcm)
    this.outLen += pcm.length

    while (this.outLen >= FRAME) {
      const chunk = this.take(FRAME)
      this.analyze(chunk)
      // 播放中**也**继续送帧：识别器保持连续，插话后的半句不用从头重听；
      // 模型自己说的那部分会在状态机里被丢弃（那边知道当前轮次是谁的）
      if (!this.muted) this.cbs.onFrame(chunk)
    }
  }

  /** 48k → 16k：每 3 个采样取均值，顺便压掉 8k 以上的成分 */
  private downsample(input: Float32Array): Float32Array {
    let merged: Float32Array
    if (this.residue.length === 0) {
      merged = input
    } else {
      merged = new Float32Array(this.residue.length + input.length)
      merged.set(this.residue, 0)
      merged.set(input, this.residue.length)
    }

    const n = Math.floor(merged.length / DOWN)
    if (n === 0) {
      this.residue = merged
      return new Float32Array(0)
    }

    const out = new Float32Array(n)
    for (let i = 0; i < n; i += 1) {
      const base = i * DOWN
      out[i] = (merged[base] + merged[base + 1] + merged[base + 2]) / 3
    }
    // 显式构造而不是 slice()：slice 的返回类型在 TypedArray 泛型下是
    // ArrayBufferLike，和字段的 ArrayBuffer 对不上
    this.residue = new Float32Array(merged.subarray(n * DOWN))
    return out
  }

  private take(n: number): Float32Array {
    const out = new Float32Array(n)
    let offset = 0
    while (offset < n) {
      const head = this.outBuf[0]
      const want = n - offset
      if (head.length <= want) {
        out.set(head, offset)
        offset += head.length
        this.outBuf.shift()
      } else {
        out.set(head.subarray(0, want), offset)
        this.outBuf[0] = head.subarray(want)
        offset += want
      }
    }
    this.outLen -= n
    return out
  }

  /**
   * 能量判定。两个作用：
   *   1. 界面上的「正在说话」呼吸效果
   *   2. 播放中判定用户插话（全双工的关键）
   *
   * 播放时阈值要抬高：AEC 消掉的是大部分回声，不是全部，残余能量叠加 TTS 的
   * 低频成分后，用平时的阈值会被自己的声音打断。
   */
  private analyze(chunk: Float32Array) {
    const rms = rootMeanSquare(chunk)
    const ms = (chunk.length / ASR_RATE) * 1000

    // 开头 300ms 采环境底噪（取最小值：用户可能立刻开口）
    if (this.calibrated < 300) {
      this.noiseFloor = this.calibrated === 0 ? rms : Math.min(this.noiseFloor, rms)
      this.calibrated += ms
      return
    }

    const playing = this.isPlaying
    const threshold = playing
      ? // 约 +9dB：够挡残留回声，又不至于要用户吼才能打断
        Math.max(this.noiseFloor * 6, 0.045)
      : Math.min(Math.max(this.noiseFloor * 2.5, 0.012), 0.06)

    const loud = rms > threshold

    if (playing) {
      if (loud) {
        this.loudRun += 1
        const now = Date.now()
        if (this.loudRun >= BARGE_FRAMES && now - this.lastBargeAt > BARGE_COOLDOWN_MS) {
          this.lastBargeAt = now
          this.loudRun = 0
          this.cbs.onBargeIn?.()
        }
      } else {
        this.loudRun = 0
      }
      // 播放期间不更新「用户在说话」状态：那时候麦克风里混着模型的声
      return
    }

    if (loud !== this.speaking) {
      this.speaking = loud
      this.cbs.onSpeech?.(loud)
    }
  }
}

/** base64 的小端 i16 → float32（[-1, 1]） */
function decodePcm(b64: string): Float32Array {
  const binary = atob(b64)
  const bytes = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i)

  // 字节数不是偶数说明数据坏了，宁可丢掉也不要让 Int16Array 抛异常
  if (bytes.length < 2) return new Float32Array(0)
  const count = bytes.length >> 1
  const pcm = new Int16Array(bytes.buffer, bytes.byteOffset, count)
  const out = new Float32Array(count)
  for (let i = 0; i < count; i += 1) out[i] = pcm[i] / 32768
  return out
}

function rootMeanSquare(data: Float32Array): number {
  let sum = 0
  for (let i = 0; i < data.length; i += 1) sum += data[i] * data[i]
  return Math.sqrt(sum / data.length)
}
