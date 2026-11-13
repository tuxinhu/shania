<script setup lang="ts">
/**
 * 右侧工具条：竖排图标 + 展开式面板。
 *
 * 为什么不用弹窗/抽屉：这几个功能是「边看边用」的——看文件、看 diff、开网页，
 * 关掉再开就要重新定位。所以做成常驻的窄竖条，点开在右侧推开一块面板，
 * 对话区宽度自适应收窄，不遮挡、不打断。
 *
 * 终端没有面板：直接拉起系统终端（工作目录 = 工作空间），比在里面养一个
 * xterm 实例划算得多。
 */
import { computed, onMounted, ref } from 'vue'
import Icon from './Icon.vue'
import ResizeHandle from './ResizeHandle.vue'
import BrowserPanel from './panels/BrowserPanel.vue'
import ChangesPanel from './panels/ChangesPanel.vue'
import FilesPanel from './panels/FilesPanel.vue'
import { getWorkspaceRoot, openTerminal, setWorkspaceRoot } from '@/lib/workspace'
import { isTauri } from '@/lib/api'

type ToolId = 'browser' | 'files' | 'changes'

const tools: { id: ToolId; icon: string; label: string; hint: string }[] = [
  { id: 'browser', icon: 'globe', label: '浏览器', hint: '内置浏览器（再点一次收起）' },
  { id: 'files', icon: 'folder', label: '工作空间', hint: '浏览工作空间里的文件' },
  { id: 'changes', icon: 'git', label: '变更', hint: '当前工作空间的 git 变更' },
]

const active = ref<ToolId | null>(null)
const root = ref('')
const error = ref('')

/** 终端点击后的短暂反馈：拉起外部窗口没有回调，只能给个视觉确认 */
const flash = ref(false)
const toast = ref('')

const current = computed(() => tools.find((t) => t.id === active.value) ?? null)

function toggle(id: ToolId) {
  active.value = active.value === id ? null : id
}

async function reloadRoot() {
  try {
    root.value = await getWorkspaceRoot()
    error.value = ''
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
  }
}

async function pickRoot() {
  if (!isTauri) return
  try {
    const { open } = await import('@tauri-apps/plugin-dialog')
    const picked = await open({ directory: true, multiple: false, title: '选择工作空间' })
    if (typeof picked === 'string') root.value = await setWorkspaceRoot(picked)
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
  }
}

let toastTimer: ReturnType<typeof setTimeout> | null = null

function say(text: string) {
  toast.value = text
  if (toastTimer) clearTimeout(toastTimer)
  toastTimer = setTimeout(() => (toast.value = ''), 2600)
}

/**
 * 拉起终端。
 *
 * 一定要给反馈：外部窗口是别的进程，开没开成功前端拿不到回调。之前失败时
 * 只往面板头部塞一句红字，可面板没开的时候根本看不见——用户看到的就是
 * 「点了终端没反应，这功能是不是没做」。
 */
async function terminal() {
  try {
    const app = await openTerminal(root.value)
    flash.value = true
    setTimeout(() => (flash.value = false), 700)
    say(`已在 ${app} 打开 ${root.value || '工作空间'}`)
    error.value = ''
  } catch (err) {
    const text = err instanceof Error ? err.message : String(err)
    error.value = text
    say(`打开终端失败：${text}`)
  }
}

onMounted(reloadRoot)
</script>

<template>
  <div class="right">
    <aside v-if="active" class="panel">
      <ResizeHandle
        variable="--rail-panel-w"
        store="shania.railPanelW"
        :min="320"
        :max="640"
        :initial="420"
        grow="left"
        class="panel-handle"
      />
      <header class="panel-head">
        <Icon :name="current?.icon ?? 'globe'" :size="13" />
        <span class="panel-title">{{ current?.label }}</span>
        <span v-if="error" class="panel-error" :title="error">{{ error }}</span>
        <button class="icon-btn" title="收起面板" @click="active = null">
          <Icon name="x" :size="13" />
        </button>
      </header>

      <BrowserPanel v-if="active === 'browser'" />
      <FilesPanel
        v-else-if="active === 'files'"
        :root="root"
        @pick-root="pickRoot"
        @changed="reloadRoot"
      />
      <ChangesPanel v-else :root="root" />
    </aside>

    <nav class="rail">
      <div class="rail-top" data-tauri-drag-region />
      <button
        v-for="t in tools"
        :key="t.id"
        class="rail-btn"
        :class="{ on: active === t.id }"
        :title="t.hint"
        @click="toggle(t.id)"
      >
        <Icon :name="t.icon" :size="17" />
      </button>

      <div class="rail-gap" />

      <button
        class="rail-btn"
        :class="{ flash }"
        :title="`在 ${root || '工作空间'} 打开终端`"
        @click="terminal"
      >
        <Icon name="terminal" :size="17" />
      </button>
    </nav>

    <Transition name="rail-toast">
      <div v-if="toast" class="rail-toast">{{ toast }}</div>
    </Transition>
  </div>
</template>

<style scoped>
.right {
  flex: none;
  display: flex;
  height: 100%;
  min-height: 0;
}

/* ---------- 面板 ---------- */
.panel {
  /* 42vw 是兜底：窗口被拖窄时固定 420 会吃掉一半，对话区只剩几十像素 */
  width: min(var(--rail-panel-w, 420px), 42vw);
  flex: none;
  display: flex;
  flex-direction: column;
  min-height: 0;
  background: var(--bg);
  border-left: 1px solid var(--line-soft);
  position: relative;
}

/* 左缘拖宽 */
.panel-handle {
  left: -4px;
}

.panel-head {
  height: 40px;
  flex: none;
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 0 10px 0 14px;
  border-bottom: 1px solid var(--line-soft);
  color: var(--text-2);
  user-select: none;
}

.panel-title {
  font-size: 12px;
  font-weight: 600;
  color: var(--text);
}

.panel-error {
  margin-left: auto;
  max-width: 180px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 10.5px;
  color: var(--red);
}

.icon-btn {
  width: 24px;
  height: 24px;
  margin-left: auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 6px;
  color: var(--text-3);
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.icon-btn:hover {
  background: var(--hover);
  color: var(--text);
}
.panel-error + .icon-btn {
  margin-left: 6px;
}

/* ---------- 竖条 ---------- */
.rail {
  width: 46px;
  flex: none;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 2px;
  padding: 0 0 10px;
  background: var(--sidebar);
  border-left: 1px solid var(--line-soft);
}

/* 顶部留白兼拖拽区：无边框窗口的右上角也得能拖 */
.rail-top {
  height: 40px;
  width: 100%;
  flex: none;
}

.rail-gap {
  flex: 1;
}

.rail-btn {
  width: 32px;
  height: 32px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 8px;
  color: var(--text-3);
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.rail-btn:hover {
  background: var(--hover);
  color: var(--text);
}
.rail-btn.on {
  background: var(--active);
  color: var(--text);
}
/* 终端是「动作」不是「面板」，用一次性高亮告诉用户点到了 */
.rail-btn.flash {
  background: var(--accent-soft);
  color: var(--accent);
}

/* ---------- 结果提示 ---------- */
/* 固定贴在工具条左侧：面板开合、对话滚动都不影响它 */
.rail-toast {
  position: fixed;
  right: 58px;
  bottom: 18px;
  z-index: 60;
  max-width: 340px;
  padding: 8px 12px;
  border-radius: var(--radius-sm);
  background: var(--surface);
  border: 1px solid var(--line);
  box-shadow: var(--shadow-md);
  color: var(--text-2);
  font-size: 11.5px;
  line-height: 1.5;
  pointer-events: none;
}
.rail-toast-enter-active,
.rail-toast-leave-active {
  transition: opacity 0.18s var(--ease), transform 0.18s var(--ease);
}
.rail-toast-enter-from,
.rail-toast-leave-to {
  opacity: 0;
  transform: translateY(6px);
}
</style>
