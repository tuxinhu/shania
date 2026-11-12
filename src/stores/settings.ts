import { defineStore } from 'pinia'
import { reactive, ref, watch } from 'vue'
import type { AppSettings, ModelProfile } from '@/lib/types'
import { DEFAULT_SHORTCUT } from '@/lib/shortcut'
import { isTauri, loadSettingsRaw, saveSettingsRaw, settingsLocation } from '@/lib/api'

/** 本地缓存键：仅用于首屏抢先渲染与浏览器预览；Tauri 下真正的存储是 ~/.shania/settings.json */
const CACHE_KEY = 'shania.settings.v1'

const DEFAULTS: AppSettings = {
  baseUrl: 'http://127.0.0.1:4000',
  apiKey: '',
  model: 'qwen3-vl-8b',
  temperature: 0.7,
  maxTokens: 2048,
  systemPrompt:
    '你是 Shania，一个运行在用户桌面上的 AI 智能体。回答要准确、简洁、有条理；' +
    '需要实时信息或本机信息时，主动调用可用工具，不要凭空猜测。' +
    '默认使用简体中文回答，代码与技术术语保持原样。',
  toolsEnabled: true,
  theme: 'system',
  displayName: 'Shania',
  shortcut: DEFAULT_SHORTCUT,
  closeAction: 'tray',
  autoStart: false,
  contextLimit: 32000,
  sttPreload: true,
  ttsSid: 0,
  ttsSpeed: 1,
  ttsModel: 'auto',
  profiles: [],
  activeProfileId: '',
}

/**
 * 已经废弃的设置项：whisper 引擎的端口/地址/后端/语言，以及引擎选择本身。
 *
 * 语音输入统一走 X-ASR 之后这些都没有意义了。解析时直接丢掉，否则它们会随着
 * 「整对象写回」一直留在 `settings.json` 里——用户打开文件看到一堆不起作用的
 * 字段，只会以为程序还在用它。
 */
const OBSOLETE_KEYS = ['sttModel', 'sttBaseUrl', 'sttPort', 'sttBackend', 'sttLanguage', 'sttEngine']

function parse(raw: string | null): AppSettings {
  if (!raw) return { ...DEFAULTS, profiles: [] }
  try {
    const parsed = JSON.parse(raw) as Partial<AppSettings>
    const merged: AppSettings = {
      ...DEFAULTS,
      ...parsed,
      // 数组字段必须是新引用，避免多个 store 实例共享同一份默认值
      profiles: Array.isArray(parsed.profiles) ? parsed.profiles : [],
    }
    for (const key of OBSOLETE_KEYS) delete (merged as unknown as Record<string, unknown>)[key]
    return merged
  } catch {
    return { ...DEFAULTS, profiles: [] }
  }
}

function newProfileId(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) return crypto.randomUUID()
  return `p_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 8)}`
}

export const useSettingsStore = defineStore('settings', () => {
  const settings = reactive<AppSettings>(parse(localStorage.getItem(CACHE_KEY)))

  /** 共享配置文件读完之前不落盘，否则会用缓存里的旧值覆盖 ~/.shania/settings.json */
  const ready = ref(false)
  const filePath = ref('')

  let saveTimer: number | undefined

  const media = window.matchMedia('(prefers-color-scheme: dark)')

  function resolvedTheme(): 'light' | 'dark' {
    if (settings.theme === 'system') return media.matches ? 'dark' : 'light'
    return settings.theme
  }

  function applyTheme() {
    document.documentElement.dataset.theme = resolvedTheme()
  }

  media.addEventListener('change', () => {
    if (settings.theme === 'system') applyTheme()
  })

  /** 写共享配置文件（防抖，避免拖滑块时狂写磁盘） */
  function persist() {
    const json = JSON.stringify(settings)
    // 本地缓存同步写：首屏和浏览器预览都靠它，失败也不影响主流程
    try {
      localStorage.setItem(CACHE_KEY, json)
    } catch {
      /* 隐私模式等场景下 localStorage 可能不可用 */
    }

    if (!isTauri || !ready.value) return
    if (saveTimer) window.clearTimeout(saveTimer)
    saveTimer = window.setTimeout(() => {
      void saveSettingsRaw(json).catch((err) => console.warn('保存设置失败：', err))
    }, 250)
  }

  watch(
    settings,
    () => {
      persist()
      applyTheme()
    },
    { deep: true },
  )

  /**
   * 从共享配置文件加载设置。
   * 必须在应用快捷键 / 关闭行为之前调用，否则会拿默认值去覆盖用户配置。
   */
  async function hydrate() {
    if (!isTauri) {
      ready.value = true
      return
    }

    try {
      const raw = await loadSettingsRaw()
      if (raw) {
        // 触发 watch 时 ready 仍为 false，因此不会回写文件
        Object.assign(settings, parse(raw))
      } else if (localStorage.getItem(CACHE_KEY)) {
        // 老版本把设置存在 WebView 的 localStorage 里，首次启动搬进共享文件
        await saveSettingsRaw(JSON.stringify(settings))
      }
      filePath.value = (await settingsLocation()) ?? ''
      ready.value = true
    } catch (err) {
      // 读失败就保持只读状态：宁可用默认值，也不能把用户的配置文件冲掉
      console.warn('读取共享设置失败，本次使用默认值：', err)
    }
  }

  /* ------------------------------ 模型配置 ------------------------------ */

  /** 切换到某个已保存配置 */
  function applyProfile(id: string) {
    const p = settings.profiles.find((x) => x.id === id)
    if (!p) return
    settings.baseUrl = p.baseUrl
    settings.apiKey = p.apiKey
    settings.model = p.model
    settings.temperature = p.temperature
    settings.maxTokens = p.maxTokens
    settings.activeProfileId = p.id
  }

  /** 把当前生效的连接参数存成一条配置 */
  function saveCurrentAsProfile(name: string): ModelProfile {
    const profile: ModelProfile = {
      id: newProfileId(),
      name: name.trim() || settings.model || '未命名配置',
      baseUrl: settings.baseUrl,
      apiKey: settings.apiKey,
      model: settings.model,
      temperature: settings.temperature,
      maxTokens: settings.maxTokens,
    }
    settings.profiles = [...settings.profiles, profile]
    settings.activeProfileId = profile.id
    return profile
  }

  /** 用当前参数覆盖已有配置 */
  function overwriteProfile(id: string) {
    const p = settings.profiles.find((x) => x.id === id)
    if (!p) return
    p.baseUrl = settings.baseUrl
    p.apiKey = settings.apiKey
    p.model = settings.model
    p.temperature = settings.temperature
    p.maxTokens = settings.maxTokens
  }

  function renameProfile(id: string, name: string) {
    const p = settings.profiles.find((x) => x.id === id)
    if (p) p.name = name.trim() || p.name
  }

  function removeProfile(id: string) {
    settings.profiles = settings.profiles.filter((x) => x.id !== id)
    if (settings.activeProfileId === id) settings.activeProfileId = ''
  }

  // 手动改了连接参数 → 与已保存配置脱钩，避免下拉里显示错误的激活项
  watch(
    () => [settings.baseUrl, settings.apiKey, settings.model] as const,
    () => {
      if (!settings.activeProfileId) return
      const p = settings.profiles.find((x) => x.id === settings.activeProfileId)
      if (!p) {
        settings.activeProfileId = ''
        return
      }
      if (p.baseUrl !== settings.baseUrl || p.apiKey !== settings.apiKey || p.model !== settings.model) {
        settings.activeProfileId = ''
      }
    },
  )

  function reset() {
    Object.assign(settings, { ...DEFAULTS, profiles: [] })
  }

  applyTheme()

  return {
    settings,
    ready,
    filePath,
    hydrate,
    applyTheme,
    resolvedTheme,
    reset,
    applyProfile,
    saveCurrentAsProfile,
    overwriteProfile,
    renameProfile,
    removeProfile,
  }
})
