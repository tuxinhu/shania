<script setup lang="ts">
/**
 * 工作空间文件树。
 *
 * 用「扁平行数组 + depth」而不是递归组件：展开/收起只需要 splice 一段，
 * 不用为每个节点维护子组件实例。目录是懒加载的——点开才读盘。
 */
import { onMounted, ref, watch } from 'vue'
import Icon from '../Icon.vue'
import { isTauri } from '@/lib/api'
import { listDir, openPath, type DirEntry } from '@/lib/workspace'

const props = defineProps<{ root: string }>()
const emit = defineEmits<{ pickRoot: []; changed: [] }>()

interface Row {
  entry: DirEntry
  depth: number
}

const rows = ref<Row[]>([])
const expanded = ref(new Set<string>())
const busy = ref(false)
const error = ref('')

function prettySize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

async function loadRoot() {
  if (!props.root) return
  busy.value = true
  error.value = ''
  try {
    const list = await listDir(props.root)
    rows.value = list.map((entry) => ({ entry, depth: 0 }))
    expanded.value = new Set()
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
    rows.value = []
  } finally {
    busy.value = false
  }
}

async function toggle(row: Row, index: number) {
  const { entry } = row
  if (!entry.dir) {
    // 文件交给系统关联程序：比在里面做一个只读预览器实用
    await openPath(entry.path).catch((err) => (error.value = String(err)))
    return
  }

  if (expanded.value.has(entry.path)) {
    expanded.value.delete(entry.path)
    // 收起 = 把这一段 depth 更深的行全部删掉
    let end = index + 1
    while (end < rows.value.length && rows.value[end].depth > row.depth) end += 1
    rows.value.splice(index + 1, end - index - 1)
    expanded.value = new Set(expanded.value)
    return
  }

  try {
    const list = await listDir(entry.path)
    expanded.value.add(entry.path)
    expanded.value = new Set(expanded.value)
    if (list.length === 0) return
    rows.value.splice(
      index + 1,
      0,
      ...list.map((child) => ({ entry: child, depth: row.depth + 1 })),
    )
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
  }
}

/** 根目录太长时只露出末两段，完整路径放 title */
function shortPath(path: string): string {
  if (!path) return '未设置工作空间'
  const parts = path.replace(/\\/g, '/').split('/').filter(Boolean)
  return parts.length <= 2 ? path : `…/${parts.slice(-2).join('/')}`
}

watch(() => props.root, loadRoot)
onMounted(loadRoot)
</script>

<template>
  <div class="wrap">
    <div class="rootbar">
      <Icon name="folder" :size="12" />
      <span class="path" :title="root">{{ shortPath(root) }}</span>
      <button class="mini" title="切换工作空间" @click="emit('pickRoot')">
        <Icon name="edit" :size="12" />
      </button>
      <button class="mini" title="刷新" @click="loadRoot">
        <Icon name="refresh" :size="12" />
      </button>
      <button class="mini" title="在资源管理器中打开" @click="openPath(root)">
        <Icon name="external" :size="12" />
      </button>
    </div>

    <p v-if="!isTauri" class="tip">浏览器预览模式下不可用，请在桌面端使用。</p>
    <p v-else-if="error" class="tip err">{{ error }}</p>
    <p v-else-if="busy" class="tip">读取中…</p>
    <p v-else-if="rows.length === 0" class="tip">这个目录是空的。</p>

    <div v-else class="tree">
      <button
        v-for="(row, index) in rows"
        :key="row.entry.path"
        class="row"
        :class="{ dir: row.entry.dir }"
        :style="{ paddingLeft: 8 + row.depth * 13 + 'px' }"
        :title="row.entry.path"
        @click="toggle(row, index)"
      >
        <Icon
          :name="row.entry.dir ? 'chevronRight' : 'file'"
          :size="11"
          class="caret"
          :class="{ open: row.entry.dir && expanded.has(row.entry.path) }"
        />
        <Icon
          :name="row.entry.dir ? 'folder' : 'file'"
          :size="12"
          class="kind"
        />
        <span class="name">{{ row.entry.name }}</span>
        <span v-if="!row.entry.dir" class="size">{{ prettySize(row.entry.size) }}</span>
      </button>
    </div>
  </div>
</template>

<style scoped>
.wrap {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.rootbar {
  flex: none;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 8px 8px 8px 12px;
  border-bottom: 1px solid var(--line-soft);
  color: var(--text-3);
  font-size: 11px;
}

.path {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  direction: rtl;
  text-align: left;
  color: var(--text-2);
}

.mini {
  width: 22px;
  height: 22px;
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 5px;
  color: var(--text-3);
}
.mini:hover {
  background: var(--hover);
  color: var(--text);
}

.tree {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 4px 0 12px;
}

.row {
  width: 100%;
  height: 25px;
  display: flex;
  align-items: center;
  gap: 5px;
  padding-right: 10px;
  font-size: 11.5px;
  color: var(--text-2);
  text-align: left;
}
.row:hover {
  background: var(--hover);
  color: var(--text);
}
.row.dir {
  color: var(--text);
}

.caret {
  color: var(--text-3);
  transition: transform 0.14s var(--ease);
}
.caret.open {
  transform: rotate(90deg);
}

.kind {
  color: var(--text-3);
}

.name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.size {
  flex: none;
  font-size: 10px;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}

.tip {
  margin: 0;
  padding: 18px 14px;
  font-size: 11.5px;
  line-height: 1.7;
  color: var(--text-3);
}
.tip.err {
  color: var(--red);
}
</style>
