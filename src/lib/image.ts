import type { Attachment } from './types'

/** 长边上限：与主流视觉模型推荐的输入尺寸对齐，再大基本是浪费 token */
const MAX_SIDE = 1568
/** 超过这个体积就重新编码，避免会话文件被 PNG 截图撑爆 */
const PASSTHROUGH_BYTES = 1_200_000
const JPEG_QUALITY = 0.86

function localId(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) return crypto.randomUUID()
  return `img_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 8)}`
}

function readAsDataUrl(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve(String(reader.result))
    reader.onerror = () => reject(reader.error)
    reader.readAsDataURL(file)
  })
}

async function compress(file: File): Promise<{ dataUrl: string; mime: string; bytes: number }> {
  const bitmap = await createImageBitmap(file)
  const scale = Math.min(1, MAX_SIDE / Math.max(bitmap.width, bitmap.height))
  const width = Math.max(1, Math.round(bitmap.width * scale))
  const height = Math.max(1, Math.round(bitmap.height * scale))

  const canvas = document.createElement('canvas')
  canvas.width = width
  canvas.height = height
  const ctx = canvas.getContext('2d')
  if (!ctx) throw new Error('无法创建画布')
  ctx.drawImage(bitmap, 0, 0, width, height)
  bitmap.close?.()

  // 有透明通道的图保留 PNG，其余转 JPEG 换取体积
  const hasAlpha = file.type === 'image/png' || file.type === 'image/webp'
  const dataUrl = hasAlpha
    ? canvas.toDataURL('image/png')
    : canvas.toDataURL('image/jpeg', JPEG_QUALITY)

  // PNG 重编码后有可能反而更大，此时退回原图
  if (dataUrl.length > file.size * 1.4) {
    const raw = await readAsDataUrl(file)
    return { dataUrl: raw, mime: file.type, bytes: file.size }
  }

  const base64 = dataUrl.slice(dataUrl.indexOf(',') + 1)
  return {
    dataUrl,
    mime: dataUrl.slice(5, dataUrl.indexOf(';')),
    bytes: Math.floor((base64.length * 3) / 4),
  }
}

/**
 * 把用户选的图片转成可随消息保存的附件。
 * 小图直传，大图缩放重编码——否则 base64 会把 sessions.json 撑到几十 MB。
 */
export async function fileToAttachment(file: File, index: number): Promise<Attachment> {
  const name = file.name || `粘贴的图片 ${index + 1}`

  if (file.size <= PASSTHROUGH_BYTES) {
    return {
      id: localId(),
      name,
      mime: file.type,
      dataUrl: await readAsDataUrl(file),
      size: file.size,
    }
  }

  try {
    const { dataUrl, mime, bytes } = await compress(file)
    return { id: localId(), name, mime, dataUrl, size: bytes }
  } catch {
    // 浏览器不支持 canvas 编码时退回原图，宁可大一点也不要传不上去
    return {
      id: localId(),
      name,
      mime: file.type,
      dataUrl: await readAsDataUrl(file),
      size: file.size,
    }
  }
}
