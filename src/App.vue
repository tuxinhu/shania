<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import ChatPanel from '@/components/ChatPanel.vue'
import SettingsDialog from '@/components/SettingsDialog.vue'
import SidebarPanel from '@/components/SidebarPanel.vue'
import {
  applyGlobalShortcut,
  isAutoStartEnabled,
  isTauri,
  type CliAction,
  onCliArgs,
  setCloseAction,
  takeCliArgs,
} from '@/lib/api'
import { useChatStore } from '@/stores/chat'
import { useSettingsStore } from '@/stores/settings'

const chat = useChatStore()
const settingsStore = useSettingsStore()
const settingsOpen = ref(false)

let unlistenTray: (() => void) | null = null
let unlistenSettings: (() => void) | null = null
let unlistenCliArgs: (() => void) | null = null

/**
 * 处理命令行参数带来的意图。
 * 带文本时先开一个新对话再发送，避免把内容塞进用户正在看的旧会话里。
 */
function applyCliAction(action: CliAction) {
  if (action.action === 'settings') {
    settingsOpen.value = true
    return
  }
  if (action.action === 'newChat') {
    chat.newSession()
    return
  }
  chat.newSession()
  void chat.send(action.text)
}

function onKeydown(event: KeyboardEvent) {
  const mod = event.metaKey || event.ctrlKey
  if (!mod) return

  if (event.key.toLowerCase() === 'n') {
    event.preventDefault()
    chat.newSession()
  } else if (event.key === ',') {
    event.preventDefault()
    settingsOpen.value = true
  } else if (event.key === 'k') {
    // 聚焦搜索框：把光标交给侧栏的 input
    event.preventDefault()
    const input = document.querySelector<HTMLInputElement>('.search input')
    input?.focus()
    input?.select()
  }
}

onMounted(async () => {
  window.addEventListener('keydown', onKeydown)

  // 先读共享设置（~/.shania/settings.json），再由它驱动快捷键 / 关闭行为。
  // 顺序反了的话会拿默认值去覆盖用户已保存的组合键。
  await settingsStore.hydrate()

  // 会话加载完再处理首启参数：否则 send 进去的消息会被 init 覆盖掉
  void chat.init().then(async () => {
    if (!isTauri) return
    try {
      const action = await takeCliArgs()
      if (action) applyCliAction(action)
    } catch (err) {
      console.warn('读取命令行参数失败：', err)
    }
  })

  if (isTauri) {
    // 后端启动时先挂了默认热键，这里用用户设置覆盖它
    void applyGlobalShortcut(settingsStore.settings.shortcut).catch((err) => {
      console.warn('自定义快捷键注册失败，继续使用默认组合键：', err)
    })

    // 关闭行为由前端设置决定，启动时同步一次
    void setCloseAction(settingsStore.settings.closeAction).catch(() => {})

    // 自启状态以操作系统为准：用户在系统设置里关掉过，这里要跟上
    void isAutoStartEnabled()
      .then((enabled) => {
        if (enabled !== settingsStore.settings.autoStart) {
          settingsStore.settings.autoStart = enabled
        }
      })
      .catch(() => {})

    void import('@tauri-apps/api/event').then(async ({ listen }) => {
      unlistenTray = await listen('new-chat', () => chat.newSession())
      unlistenSettings = await listen('open-settings', () => {
        settingsOpen.value = true
      })
    })

    // 应用已在运行时，第二次启动带的参数走事件
    void onCliArgs(applyCliAction)
      .then((unlisten) => {
        unlistenCliArgs = unlisten
      })
      .catch(() => {})
  }
})

onUnmounted(() => {
  window.removeEventListener('keydown', onKeydown)
  unlistenTray?.()
  unlistenSettings?.()
  unlistenCliArgs?.()
})
</script>

<template>
  <div class="shell">
    <SidebarPanel @open-settings="settingsOpen = true" />
    <ChatPanel @open-settings="settingsOpen = true" />
    <SettingsDialog v-model:open="settingsOpen" />
  </div>
</template>

<style scoped>
.shell {
  display: flex;
  height: 100vh;
  min-height: 100vh;
  overflow: hidden;
  background: var(--bg);
}
</style>
