/**
 * X-ASR 流式识别的前端句柄（IPC 直连，不发 HTTP）。
 *
 * 和 `transcribeAudio`（whisper，一次一整段、走后端 HTTP）的区别：这里音频是**持续**
 * 流进去的，识别器自己判断「这句说完了没有」，所以不用能量切句、不用每隔 900ms 重解码
 * 一遍整个快照。用户说话时字跟着往外冒，说完 1 秒内定稿。
 *
 * 三件必须自己管好的事：
 *   1. **顺序**：IPC 是异步的，但音频绝不能乱序，所以排队串行发
 *   2. **不丢帧**：流式模型没有「重来」的机会，丢一块就是丢几个字（和预览不同，
 *      预览是整段重解，丢一次无所谓）
 *   3. **会话隔离**：录音停/开很快时，上一轮飞在半路上的结果不能写进新会话，
 *      每个会话带一个 id，Rust 侧对不上就丢
 */

import { invoke } from '@tauri-apps/api/core'

/** Rust 侧 `stream_stt::Feed` 的镜像 */
export interface StreamFeed {
  /** 当前这句到目前为止的**完整**文本，直接覆盖预览，不要往后追加 */
  partial: string
  /** 这一轮里判定说完的句子 */
  finals: string[]
}

export interface StreamSttCallbacks {
  /** 说话过程中不断更新；空串表示「这句还没解出字」（比如刚 reset） */
  onPartial?: (text: string) => void
  /** 一句说完 */
  onFinal?: (text: string) => void
  onError?: (err: Error) => void
}

/** 模型没装：调用方据此回退到 whisper 那条路 */
export const STREAM_NOT_INSTALLED = 'STREAM_NOT_INSTALLED'

function randomId(): string {
  const c = globalThis.crypto
  if (c && 'randomUUID' in c) return c.randomUUID()
  return `s_${Date.now()}_${Math.random().toString(36).slice(2, 10)}`
}

/**
 * Float32Array → base64。
 *
 * 直接传 `number[]` 走 JSON 会膨胀 3-5 倍（每个采样变成一个浮点字面量），
 * 150ms 一块就要几十 KB 文本；按字节 base64 只有 8KB 左右。
 *
 * 分块 `fromCharCode.apply` 而不是 `String.fromCharCode(...bytes)`：
 * 后者在 16k 个参数时会爆栈。
 */
export function pcmToBase64(pcm: Float32Array): string {
  const bytes = new Uint8Array(pcm.buffer, pcm.byteOffset, pcm.byteLength)
  let binary = ''
  const CHUNK = 8192
  for (let i = 0; i < bytes.length; i += CHUNK) {
    const slice = bytes.subarray(i, i + CHUNK)
    binary += String.fromCharCode.apply(null, slice as unknown as number[])
  }
  return btoa(binary)
}

export class StreamStt {
  private id = ''
  /** 串行队列：保证音频按顺序到达识别器 */
  private queue: Promise<void> = Promise.resolve()
  private active = false
  /** 已经排进队列但还没回来的块数，用于「识别中」状态 */
  private pending = 0
  private cbs: StreamSttCallbacks

  constructor(cbs: StreamSttCallbacks) {
    this.cbs = cbs
  }

  get running(): boolean {
    return this.active
  }

  /** 还有多少块在路上 */
  get backlog(): number {
    return this.pending
  }

  /**
   * 打开一个会话。模型未安装时抛 `STREAM_NOT_INSTALLED`。
   *
   * 第一次调用会加载 134MB 模型（1-2 秒），之后常驻，stop 之后再次 start 是毫秒级。
   */
  async start(): Promise<void> {
    if (this.active) return
    this.id = randomId()
    await invoke('stt_stream_start', { sessionId: this.id })
    this.active = true
    this.queue = Promise.resolve()
    this.pending = 0
  }

  /**
   * 送一块 16k 单声道 PCM。同步返回，内部排队。
   *
   * 刻意**不**做「上一块没回来就丢」——流式丢一块就是丢几个字。正常速度下
   * 一块 150ms 音频推理约 30ms，队列本来就是空的；只有机器特别慢时才会积压，
   * 那种情况下宁可延迟大一点也不能漏词。
   */
  feed(pcm: Float32Array): void {
    if (!this.active || pcm.length === 0) return
    const id = this.id
    const b64 = pcmToBase64(pcm)
    this.pending += 1
    this.queue = this.queue
      .then(async () => {
        const res = await invoke<StreamFeed>('stt_stream_feed', { sessionId: id, pcm: b64 })
        // 会话已经换掉：这批结果属于上一轮，丢掉
        if (id !== this.id) return
        this.apply(res)
      })
      .catch((err) => {
        if (id !== this.id) return
        this.cbs.onError?.(err instanceof Error ? err : new Error(String(err)))
      })
      .finally(() => {
        this.pending = Math.max(0, this.pending - 1)
      })
  }

  /**
   * 结束会话：把尾部没定稿的那段冲出来。
   *
   * 等队列排空再发 stop，否则最后几块音频会被判定成「已结束」之后才到，
   * 结果是最后半句话凭空消失。
   */
  async stop(): Promise<void> {
    if (!this.active) return
    const id = this.id
    try {
      await this.queue
      const res = await invoke<StreamFeed>('stt_stream_stop', { sessionId: id })
      if (id === this.id) this.apply(res)
    } catch (err) {
      this.cbs.onError?.(err instanceof Error ? err : new Error(String(err)))
    } finally {
      this.active = false
      this.id = ''
      this.pending = 0
    }
  }

  /** 放弃这个会话（出错/用户切走）：不冲尾部，结果全部作废 */
  async abort(): Promise<void> {
    if (!this.active) return
    this.active = false
    this.id = ''
    this.pending = 0
    this.queue = Promise.resolve()
  }

  private apply(res: StreamFeed) {
    for (const text of res.finals) {
      if (text) this.cbs.onFinal?.(text)
    }
    // finals 之后 partial 一定是空的（Rust 侧 endpoint 就 reset 了），
    // 这里只在非空时更新，避免把刚定稿的内容又拿空串盖掉
    if (res.partial) this.cbs.onPartial?.(res.partial)
  }
}
