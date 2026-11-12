import type { Session } from './types'

const pad = (n: number) => String(n).padStart(2, '0')

export function formatTime(ts: number): string {
  const d = new Date(ts)
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`
}

function startOfDay(ts: number): number {
  const d = new Date(ts)
  d.setHours(0, 0, 0, 0)
  return d.getTime()
}

export function timeAgo(ts: number): string {
  const diff = Date.now() - ts
  if (diff < 60_000) return '刚刚'
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`
  if (diff < 172_800_000) return '昨天'
  if (diff < 604_800_000) return `${Math.floor(diff / 86_400_000)} 天前`
  const d = new Date(ts)
  return `${d.getMonth() + 1} 月 ${d.getDate()} 日`
}

export interface SessionGroup {
  label: string
  items: Session[]
}

export function groupSessions(list: Session[]): SessionGroup[] {
  const today = startOfDay(Date.now())
  const buckets: Record<string, Session[]> = {
    今天: [],
    昨天: [],
    '近 7 天': [],
    更早: [],
  }

  for (const s of list) {
    const day = startOfDay(s.updatedAt)
    if (day === today) buckets['今天'].push(s)
    else if (day === today - 86_400_000) buckets['昨天'].push(s)
    else if (day > today - 7 * 86_400_000) buckets['近 7 天'].push(s)
    else buckets['更早'].push(s)
  }

  return Object.entries(buckets)
    .filter(([, items]) => items.length > 0)
    .map(([label, items]) => ({ label, items }))
}

export function formatTokens(n?: number): string {
  if (!n || n <= 0) return '0'
  if (n < 1000) return String(n)
  return `${(n / 1000).toFixed(1)}k`
}
