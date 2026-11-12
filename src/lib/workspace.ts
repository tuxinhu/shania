import { invoke } from '@tauri-apps/api/core'
import { isTauri } from './api'

/** 文件树里的一行 */
export interface DirEntry {
  name: string
  path: string
  dir: boolean
  size: number
}

/** git status 的一行；code 是 porcelain 的两位状态码（"M "、" M"、"??"） */
export interface GitChange {
  path: string
  code: string
  staged: boolean
  untracked: boolean
}

/**
 * 浏览器预览模式下这些命令都不存在，调用会抛 invoke 未定义的错。
 * 统一在入口挡掉，让面板显示「桌面端可用」而不是一片报错。
 */
function requireDesktop(): void {
  if (!isTauri) throw new Error('此功能需要在桌面端使用')
}

export async function getWorkspaceRoot(): Promise<string> {
  requireDesktop()
  return invoke<string>('workspace_root')
}

/** 切换工作空间，返回规范化后的绝对路径 */
export async function setWorkspaceRoot(path: string): Promise<string> {
  requireDesktop()
  return invoke<string>('workspace_set', { path })
}

export async function listDir(path: string): Promise<DirEntry[]> {
  requireDesktop()
  return invoke<DirEntry[]>('workspace_list', { path })
}

/** 用系统默认程序打开（文件夹 → 资源管理器，文件 → 关联程序） */
export async function openPath(path: string): Promise<void> {
  requireDesktop()
  await invoke('workspace_open', { path })
}

/** 在工作空间里拉起终端（Windows Terminal，没装则 cmd） */
export async function openTerminal(path: string): Promise<void> {
  requireDesktop()
  await invoke('workspace_terminal', { path })
}

export async function gitChanges(path: string): Promise<GitChange[]> {
  requireDesktop()
  return invoke<GitChange[]>('git_changes', { path })
}

export async function gitBranch(path: string): Promise<string> {
  requireDesktop()
  return invoke<string>('git_branch', { path })
}

export async function gitDiff(path: string, file: string): Promise<string> {
  requireDesktop()
  return invoke<string>('git_diff', { path, file })
}

/* ---------------- 内置浏览器 ---------------- */

export interface Bounds {
  x: number
  y: number
  width: number
  height: number
}

export async function browserOpen(url: string, bounds: Bounds): Promise<void> {
  requireDesktop()
  await invoke('browser_open', { url, ...bounds })
}

/** 只更新位置尺寸（窗口缩放、面板拖宽时调用） */
export async function browserBounds(bounds: Bounds): Promise<void> {
  if (!isTauri) return
  await invoke('browser_bounds', {
    x: bounds.x,
    y: bounds.y,
    width: bounds.width,
    height: bounds.height,
  })
}

export async function browserNavigate(url: string): Promise<void> {
  requireDesktop()
  await invoke('browser_navigate', { url })
}

export async function browserScript(script: string): Promise<void> {
  requireDesktop()
  await invoke('browser_script', { script })
}

export async function browserUrl(): Promise<string | null> {
  if (!isTauri) return null
  return invoke<string | null>('browser_url')
}

export async function browserClose(): Promise<void> {
  if (!isTauri) return
  await invoke('browser_close')
}
