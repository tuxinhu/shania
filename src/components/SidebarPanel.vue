<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from 'vue'
import Icon from './Icon.vue'
import ResizeHandle from './ResizeHandle.vue'
import WindowControls from './WindowControls.vue'
import { exportSession } from '@/lib/export'
import { pickSessionsToImport } from '@/lib/importer'
import { groupSessions, timeAgo } from '@/lib/format'
import type { Session } from '@/lib/types'
import { useChatStore } from '@/stores/chat'
import { useSettingsStore } from '@/stores/settings'

defineEmits<{ (e: 'open-settings'): void }>()

const chat = useChatStore()
const settingsStore = useSettingsStore()

const keyword = ref('')
const editingId = ref('')
const editingTitle = ref('')
const exportingId = ref('')
const exportedId = ref('')
const exportFailedId = ref('')
const showArchived = ref(false)
const isMac = /Mac|iPhone|iPad/.test(navigator.userAgent)

interface Hit {
  id: string
  parts: { t: string; hit: boolean }[]
}

/** 把匹配片段拆成可高亮的片段，只算一次 */
function snippetParts(text: string, q: string): { t: string; hit: boolean }[] {
  const out: { t: string; hit: boolean }[] = []
  const lower = text.toLowerCase()
  let cursor = 0
  while (cursor < text.length) {
    const idx = lower.indexOf(q, cursor)
    if (idx < 0) {
      out.push({ t: text.slice(cursor), hit: false })
      break
    }
    if (idx > cursor) out.push({ t: text.slice(cursor, idx), hit: false })
    out.push({ t: text.slice(idx, idx + q.length), hit: true })
    cursor = idx + q.length
  }
  return out
}

const searchQuery = computed(() => keyword.value.trim().toLowerCase())

/** 搜索命中：会话 id → 第一条匹配消息的片段 */
const hitMap = computed(() => {
  const q = searchQuery.value
  const map = new Map<string, Hit>()
  if (!q) return map

  for (const s of chat.sortedSessions) {
    for (const m of s.messages) {
      const idx = m.content.toLowerCase().indexOf(q)
      if (idx < 0) continue
      // 前缀只留几个字：关键词必须落在单行可见范围内，否则会被省略号吞掉
      const start = Math.max(0, idx - 8)
      const end = Math.min(m.content.length, idx + q.length + 28)
      const raw =
        (start > 0 ? '…' : '') +
        m.content.slice(start, end).replace(/\s+/g, ' ') +
        (end < m.content.length ? '…' : '')
      map.set(s.id, { id: m.id, parts: snippetParts(raw, q) })
      break
    }
  }
  return map
})

const visibleSessions = computed(() => {
  if (!searchQuery.value) return chat.activeSessions
  // 搜索时连归档会话一起搜，避免"明明聊过却找不到"
  return chat.sortedSessions.filter(
    (s) =>
      s.title.toLowerCase().includes(searchQuery.value) ||
      s.messages.some((m) => m.content.toLowerCase().includes(searchQuery.value)),
  )
})

const groups = computed(() => groupSessions(visibleSessions.value))
const archivedCount = computed(() => chat.archivedSessions.length)

const themeIcon = computed(() => {
  const t = settingsStore.settings.theme
  if (t === 'light') return 'sun'
  if (t === 'dark') return 'moon'
  return 'monitor'
})

const themeLabel = computed(() => {
  const t = settingsStore.settings.theme
  return t === 'light' ? '浅色外观' : t === 'dark' ? '深色外观' : '跟随系统'
})

const themes = ['system', 'light', 'dark'] as const
function cycleTheme() {
  const idx = themes.indexOf(settingsStore.settings.theme)
  settingsStore.settings.theme = themes[(idx + 1) % themes.length]
}

function openSession(s: Session) {
  const hit = hitMap.value.get(s.id)
  if (hit) chat.jumpToMessage(s.id, hit.id)
  else chat.selectSession(s.id)
}

async function startRename(id: string, current: string) {
  editingId.value = id
  editingTitle.value = current
  await nextTick()
  const el = document.querySelector<HTMLInputElement>('.rename-input')
  el?.focus()
  el?.select()
}

function commitRename() {
  if (!editingId.value) return
  chat.renameSession(editingId.value, editingTitle.value)
  editingId.value = ''
}

async function doExport(session: Session) {
  if (exportingId.value) return
  exportingId.value = session.id
  exportFailedId.value = ''

  const result = await exportSession(session, settingsStore.settings.displayName || 'Shania')
  exportingId.value = ''

  if (result === 'saved') {
    exportedId.value = session.id
    window.setTimeout(() => {
      if (exportedId.value === session.id) exportedId.value = ''
    }, 1800)
  } else if (result === 'failed') {
    exportFailedId.value = session.id
    window.setTimeout(() => {
      if (exportFailedId.value === session.id) exportFailedId.value = ''
    }, 2400)
  }
}

/* ------------------------------ 导入 ------------------------------ */

const importing = ref(false)
const importToast = ref('')
let importToastTimer: ReturnType<typeof setTimeout> | null = null

function toast(text: string, ms = 2600) {
  importToast.value = text
  if (importToastTimer) clearTimeout(importToastTimer)
  importToastTimer = setTimeout(() => {
    importToast.value = ''
  }, ms)
}

async function doImport() {
  if (importing.value) return
  importing.value = true
  try {
    const picked = await pickSessionsToImport()
    if (picked.status === 'cancelled') return
    if (picked.status === 'failed') {
      toast(`导入失败：${picked.message}`, 4200)
      return
    }

    const { added, skipped } = await chat.importSessions(picked.sessions)
    if (!added && !skipped) {
      toast('没有可导入的会话')
      return
    }
    toast(skipped ? `已导入 ${added} 条，跳过 ${skipped} 条重复` : `已导入 ${added} 条会话`)
  } finally {
    importing.value = false
  }
}

/* ------------------------------ 右键菜单 ------------------------------ */

const ctx = ref<{ x: number; y: number; session: Session } | null>(null)

function openCtx(event: MouseEvent, s: Session) {
  event.preventDefault()
  const x = Math.min(event.clientX, window.innerWidth - 176)
  const y = Math.min(event.clientY, window.innerHeight - 190)
  ctx.value = { x, y, session: s }
}

function closeCtx() {
  ctx.value = null
}

function ctxAction(action: 'rename' | 'archive' | 'export' | 'delete') {
  const item = ctx.value
  if (!item) return
  const s = item.session
  closeCtx()

  if (action === 'rename') void startRename(s.id, s.title)
  else if (action === 'archive') chat.toggleArchive(s.id)
  else if (action === 'export') void doExport(s)
  else if (action === 'delete') chat.removeSession(s.id)
}

function onGlobalKey(event: KeyboardEvent) {
  if (event.key === 'Escape') closeCtx()
}

onMounted(() => {
  document.addEventListener('click', closeCtx)
  document.addEventListener('keydown', onGlobalKey)
})
onUnmounted(() => {
  document.removeEventListener('click', closeCtx)
  document.removeEventListener('keydown', onGlobalKey)
})
</script>

<template>
  <aside class="sidebar">
    <!-- 顶部：窗口控制 + 品牌 -->
    <div class="sidebar-top" data-tauri-drag-region>
      <WindowControls />
    </div>

    <div class="sidebar-head">
      <div class="brand">
        <div class="logo">
          <Icon name="sparkles" :size="15" />
        </div>
        <div class="brand-text">
          <span class="brand-name">Shania</span>
          <span class="brand-sub">桌面智能体</span>
        </div>
      </div>

      <button class="new-btn" @click="chat.newSession()">
        <Icon name="plus" :size="14" />
        <span>新建对话</span>
        <kbd>{{ isMac ? '⌘N' : 'Ctrl N' }}</kbd>
      </button>

      <div class="search">
        <Icon name="search" :size="13" />
        <input v-model="keyword" type="text" placeholder="搜索对话与内容" spellcheck="false" />
        <button v-if="keyword" class="clear" @click="keyword = ''">
          <Icon name="x" :size="11" />
        </button>
      </div>
    </div>

    <div class="session-list">
      <template v-for="group in groups" :key="group.label">
        <div class="group-label">{{ group.label }}</div>
        <div
          v-for="s in group.items"
          :key="s.id"
          class="session-item"
          :class="{ active: s.id === chat.currentId, archived: s.archived }"
          @click="openSession(s)"
          @dblclick="startRename(s.id, s.title)"
          @contextmenu="openCtx($event, s)"
        >
          <div class="session-main">
            <input
              v-if="editingId === s.id"
              v-model="editingTitle"
              class="rename-input"
              @click.stop
              @keydown.enter.prevent="commitRename"
              @keydown.esc.prevent="editingId = ''"
              @blur="commitRename"
            />
            <template v-else>
              <span class="session-title">{{ s.title }}</span>
              <span v-if="hitMap.get(s.id)" class="session-hit">
                <template
                  v-for="(p, i) in hitMap.get(s.id)!.parts"
                  :key="i"
                ><mark v-if="p.hit">{{ p.t }}</mark><template v-else>{{ p.t }}</template></template>
              </span>
              <span v-else class="session-time">{{ timeAgo(s.updatedAt) }}</span>
            </template>
          </div>
          <div v-if="editingId !== s.id" class="session-actions">
            <button
              class="act-export"
              :class="{ ok: exportedId === s.id, bad: exportFailedId === s.id }"
              :title="
                exportedId === s.id
                  ? '已导出'
                  : exportFailedId === s.id
                    ? '导出失败'
                    : '导出会话'
              "
              :disabled="exportingId === s.id"
              @click.stop="doExport(s)"
            >
              <Icon
                :name="
                  exportedId === s.id ? 'check' : exportFailedId === s.id ? 'x' : 'download'
                "
                :size="12"
              />
            </button>
            <button title="置顶" @click.stop="chat.togglePin(s.id)">
              <Icon name="pin" :size="12" />
            </button>
            <button title="删除" @click.stop="chat.removeSession(s.id)">
              <Icon name="trash" :size="12" />
            </button>
          </div>
          <span v-if="s.pinned" class="pin-dot" />
        </div>
      </template>

      <!-- 归档区：默认折叠，避免占满列表 -->
      <template v-if="!searchQuery && archivedCount">
        <button class="archive-toggle" @click="showArchived = !showArchived">
          <Icon name="archive" :size="12" />
          <span>已归档</span>
          <span class="count">{{ archivedCount }}</span>
          <Icon class="chev" :class="{ open: showArchived }" name="chevron" :size="12" />
        </button>
        <template v-if="showArchived">
          <div
            v-for="s in chat.archivedSessions"
            :key="s.id"
            class="session-item archived"
            :class="{ active: s.id === chat.currentId }"
            @click="chat.selectSession(s.id)"
            @contextmenu="openCtx($event, s)"
          >
            <div class="session-main">
              <span class="session-title">{{ s.title }}</span>
              <span class="session-time">{{ timeAgo(s.updatedAt) }}</span>
            </div>
          </div>
        </template>
      </template>

      <div v-if="!groups.length && !archivedCount" class="empty-hint">
        {{ keyword ? '没有找到相关对话' : '还没有对话' }}
      </div>
      <div v-else-if="!groups.length" class="empty-hint">没有找到相关对话</div>
    </div>

    <div class="sidebar-foot">
      <button class="foot-btn" @click="$emit('open-settings')">
        <Icon name="settings" :size="15" />
        <span>设置</span>
      </button>
      <button
        class="foot-btn icon-only"
        :title="importing ? '正在导入…' : '导入会话（JSON）'"
        :disabled="importing"
        @click="doImport"
      >
        <Icon name="upload" :size="15" />
      </button>
      <button class="foot-btn icon-only" :title="themeLabel" @click="cycleTheme">
        <Icon :name="themeIcon" :size="15" />
      </button>
    </div>

    <Transition name="toast">
      <div v-if="importToast" class="import-toast">{{ importToast }}</div>
    </Transition>

    <!-- 右缘拖宽：宽度写进 --sidebar-w，消息列的居中补偿会自动跟着重算 -->
    <ResizeHandle
      variable="--sidebar-w"
      store="shania.sidebarW"
      :min="220"
      :max="420"
      :initial="268"
      grow="right"
      class="sidebar-handle"
    />
  </aside>

  <Teleport to="body">
    <div
      v-if="ctx"
      class="ctx-menu"
      :style="{ left: `${ctx.x}px`, top: `${ctx.y}px` }"
      @click.stop
    >
      <button @click="ctxAction('rename')">
        <Icon name="edit" :size="12" />
        <span>重命名</span>
      </button>
      <button @click="ctxAction('archive')">
        <Icon name="archive" :size="12" />
        <span>{{ ctx.session.archived ? '取消归档' : '归档' }}</span>
      </button>
      <button @click="ctxAction('export')">
        <Icon name="download" :size="12" />
        <span>导出</span>
      </button>
      <div class="sep" />
      <button class="danger" @click="ctxAction('delete')">
        <Icon name="trash" :size="12" />
        <span>删除</span>
      </button>
    </div>
  </Teleport>
</template>

<style scoped>
.sidebar {
  width: var(--sidebar-w);
  flex: none;
  display: flex;
  flex-direction: column;
  background: var(--sidebar);
  border-right: 1px solid var(--line-soft);
  min-height: 0;
  position: relative;
}

/* 拖宽把手贴右缘 */
.sidebar-handle {
  right: -4px;
}

/* ---------- 顶部交通灯 ---------- */
.sidebar-top {
  height: 52px;
  flex: none;
  display: flex;
  align-items: center;
  padding: 0 18px;
  user-select: none;
}


/* ---------- 品牌 + 新建 + 搜索 ---------- */
.sidebar-head {
  padding: 2px 12px 8px;
}

.brand {
  display: flex;
  align-items: center;
  gap: 9px;
  padding: 0 6px 14px;
}

.logo {
  width: 28px;
  height: 28px;
  border-radius: 9px;
  display: grid;
  place-items: center;
  color: #fff;
  background: linear-gradient(145deg, #4ba7ff, #0a6ff0 60%, #7b5cff);
  box-shadow: 0 2px 8px rgba(10, 111, 240, 0.32), inset 0 0.5px 0 rgba(255, 255, 255, 0.45);
}

.brand-text {
  display: flex;
  flex-direction: column;
  line-height: 1.15;
}
.brand-name {
  font-size: 13.5px;
  font-weight: 600;
  letter-spacing: -0.01em;
}
.brand-sub {
  font-size: 10.5px;
  color: var(--text-3);
  letter-spacing: 0.02em;
}

.new-btn {
  width: 100%;
  height: 32px;
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 0 11px;
  border-radius: var(--radius-sm);
  background: var(--surface);
  border: 1px solid var(--line-soft);
  box-shadow: var(--shadow-sm);
  font-size: 12.5px;
  font-weight: 500;
  color: var(--text);
  transition: background var(--dur) var(--ease), transform 0.12s var(--ease);
}
.new-btn:hover {
  background: var(--surface-2);
}
.new-btn:active {
  transform: scale(0.985);
}
.new-btn kbd {
  margin-left: auto;
  font-family: var(--font-sans);
  font-size: 10px;
  color: var(--text-3);
  letter-spacing: 0.04em;
}

.search {
  margin-top: 8px;
  display: flex;
  align-items: center;
  gap: 6px;
  height: 29px;
  padding: 0 9px;
  border-radius: 8px;
  background: var(--fill);
  color: var(--text-3);
  transition: box-shadow var(--dur) var(--ease);
}
.search:focus-within {
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 40%, transparent);
}
.search input {
  flex: 1;
  min-width: 0;
  border: none;
  background: none;
  outline: none;
  font-size: 12.5px;
  color: var(--text);
}
.search input::placeholder {
  color: var(--text-3);
}
.search .clear {
  color: var(--text-3);
  display: grid;
  place-items: center;
  border-radius: 4px;
}
.search .clear:hover {
  color: var(--text);
}

/* ---------- 会话列表 ---------- */
.session-list {
  flex: 1;
  overflow-y: auto;
  padding: 4px 8px 10px;
  min-height: 0;
}

.group-label {
  padding: 10px 8px 5px;
  font-size: 10.5px;
  font-weight: 600;
  letter-spacing: 0.05em;
  color: var(--text-3);
  text-transform: uppercase;
  user-select: none;
}

.session-item {
  position: relative;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 7px 9px;
  border-radius: 8px;
  cursor: default;
  user-select: none;
  transition: background 0.14s var(--ease);
}
.session-item:hover {
  background: var(--hover);
}
.session-item.active {
  background: var(--active);
}
.session-item.active::before {
  content: '';
  position: absolute;
  left: 2px;
  top: 50%;
  width: 2.5px;
  height: 15px;
  border-radius: 2px;
  background: var(--accent);
  transform: translateY(-50%);
}
.session-item.archived .session-title {
  color: var(--text-2);
}

.session-main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.session-title {
  font-size: 12.5px;
  line-height: 1.35;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.session-item.active .session-title {
  font-weight: 500;
}

.session-time {
  font-size: 10.5px;
  color: var(--text-3);
}

.session-hit {
  font-size: 10.5px;
  line-height: 1.4;
  color: var(--text-3);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.session-hit mark {
  background: color-mix(in srgb, var(--accent) 32%, transparent);
  color: inherit;
  border-radius: 2px;
  padding: 0 1px;
}

.rename-input {
  width: 100%;
  height: 22px;
  padding: 0 5px;
  border-radius: 5px;
  border: 1px solid var(--accent);
  background: var(--surface);
  font-size: 12.5px;
  outline: none;
}

.session-actions {
  display: flex;
  gap: 1px;
  opacity: 0;
  transition: opacity 0.14s var(--ease);
}
.session-item:hover .session-actions {
  opacity: 1;
}
.session-actions button {
  width: 21px;
  height: 21px;
  border-radius: 5px;
  display: grid;
  place-items: center;
  color: var(--text-3);
}
.session-actions button:hover {
  background: var(--fill);
  color: var(--text);
}
.session-actions button.ok,
.session-actions button.ok:hover {
  color: var(--green);
}
.session-actions button.bad,
.session-actions button.bad:hover {
  color: var(--red);
}
.session-actions button:disabled {
  opacity: 0.45;
  cursor: default;
}

.pin-dot {
  position: absolute;
  right: 6px;
  top: 6px;
  width: 4px;
  height: 4px;
  border-radius: 50%;
  background: var(--accent);
}

/* ---------- 归档折叠 ---------- */
.archive-toggle {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 8px;
  padding: 7px 9px;
  border-radius: 8px;
  font-size: 11.5px;
  color: var(--text-3);
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.archive-toggle:hover {
  background: var(--hover);
  color: var(--text-2);
}
.archive-toggle .count {
  font-variant-numeric: tabular-nums;
  font-size: 10.5px;
  padding: 0 5px;
  border-radius: 4px;
  background: var(--fill);
}
.archive-toggle .chev {
  margin-left: auto;
  transition: transform 0.18s var(--ease);
}
.archive-toggle .chev.open {
  transform: rotate(180deg);
}

.empty-hint {
  padding: 28px 12px;
  text-align: center;
  font-size: 12px;
  color: var(--text-3);
}

/* ---------- 底部 ---------- */
.sidebar-foot {
  flex: none;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 9px 12px 12px;
  border-top: 1px solid var(--line-soft);
}

/* 导入结果提示：贴着底部浮一层，不挤占列表高度 */
.import-toast {
  position: absolute;
  left: 12px;
  right: 12px;
  bottom: 54px;
  z-index: 6;
  padding: 8px 11px;
  border-radius: 10px;
  background: var(--surface);
  border: 1px solid var(--line);
  box-shadow: var(--shadow-md);
  color: var(--text);
  font-size: 12px;
  line-height: 1.5;
}

.toast-enter-active,
.toast-leave-active {
  transition: opacity 0.18s var(--ease), transform 0.18s var(--ease);
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateY(6px);
}

.foot-btn {
  height: 30px;
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 0 10px;
  border-radius: 8px;
  font-size: 12.5px;
  color: var(--text-2);
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.foot-btn:hover {
  background: var(--hover);
  color: var(--text);
}
.foot-btn.icon-only {
  margin-left: auto;
  width: 30px;
  padding: 0;
  justify-content: center;
}

/* ---------- 右键菜单 ---------- */
.ctx-menu {
  position: fixed;
  z-index: 200;
  width: 168px;
  padding: 5px;
  border-radius: 10px;
  background: var(--surface);
  border: 1px solid var(--line);
  box-shadow: var(--shadow-lg);
  animation: fade-up 0.12s var(--ease);
}
.ctx-menu button {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 9px;
  border-radius: 6px;
  font-size: 12.5px;
  color: var(--text);
  text-align: left;
}
.ctx-menu button:hover {
  background: var(--hover);
}
.ctx-menu button.danger {
  color: color-mix(in srgb, var(--red) 88%, var(--text));
}
.ctx-menu button.danger:hover {
  background: color-mix(in srgb, var(--red) 10%, transparent);
}
.ctx-menu .sep {
  height: 1px;
  margin: 4px 6px;
  background: var(--line-soft);
}
</style>
