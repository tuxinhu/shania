<script setup lang="ts">
/**
 * git 变更面板：上半是状态列表，点一条进 diff。
 *
 * diff 用纯文本渲染而不是接 diff2html：这里的用途是「扫一眼改了什么」，
 * 按行染色已经够用，省掉一个依赖和一堆 CSS。
 */
import { computed, onMounted, ref, watch } from 'vue'
import Icon from '../Icon.vue'
import { isTauri } from '@/lib/api'
import { gitBranch, gitChanges, gitDiff, type GitChange } from '@/lib/workspace'

const props = defineProps<{ root: string }>()

const changes = ref<GitChange[]>([])
const branch = ref('')
const busy = ref(false)
const error = ref('')

const picked = ref<GitChange | null>(null)
const diff = ref('')
const diffLoading = ref(false)

/** porcelain 两位状态码翻译成人话；第二位的含义和第一位不同，这里取「更要紧的那个」 */
function statusOf(change: GitChange): { label: string; tone: string } {
  if (change.untracked) return { label: '新', tone: 'add' }
  const x = change.code[0]
  const y = change.code[1]
  if (x === 'D' || y === 'D') return { label: '删', tone: 'del' }
  if (x === 'R') return { label: '改', tone: 'mod' }
  if (x === 'A') return { label: '增', tone: 'add' }
  if (x === 'M' || y === 'M') return { label: '修', tone: 'mod' }
  return { label: '?', tone: 'mod' }
}

const summary = computed(() => {
  if (changes.value.length === 0) return '工作区干净'
  return `${changes.value.length} 个文件有改动`
})

async function load() {
  if (!props.root) return
  busy.value = true
  error.value = ''
  try {
    const [list, head] = await Promise.all([gitChanges(props.root), gitBranch(props.root)])
    changes.value = list
    branch.value = head
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
    changes.value = []
  } finally {
    busy.value = false
  }
}

async function open(change: GitChange) {
  picked.value = change
  diffLoading.value = true
  try {
    diff.value = await gitDiff(props.root, change.path)
  } catch (err) {
    diff.value = `读取失败：${err instanceof Error ? err.message : String(err)}`
  } finally {
    diffLoading.value = false
  }
}

/** 拆成行并标类型，模板里直接按行渲染 */
const diffLines = computed(() =>
  diff.value.split('\n').map((text) => {
    if (text.startsWith('+++') || text.startsWith('---')) return { text, cls: 'meta' }
    if (text.startsWith('@@')) return { text, cls: 'hunk' }
    if (text.startsWith('diff ') || text.startsWith('index ')) return { text, cls: 'meta' }
    if (text.startsWith('+')) return { text, cls: 'add' }
    if (text.startsWith('-')) return { text, cls: 'del' }
    return { text, cls: '' }
  }),
)

watch(() => props.root, load)
onMounted(load)
</script>

<template>
  <div class="wrap">
    <!-- 列表视图 -->
    <template v-if="!picked">
      <div class="rootbar">
        <Icon name="git" :size="12" />
        <span class="branch">{{ branch || '—' }}</span>
        <span class="count">{{ summary }}</span>
        <button class="mini" title="刷新" @click="load">
          <Icon name="refresh" :size="12" />
        </button>
      </div>

      <p v-if="!isTauri" class="tip">浏览器预览模式下不可用，请在桌面端使用。</p>
      <p v-else-if="error" class="tip err">{{ error }}</p>
      <p v-else-if="busy" class="tip">读取中…</p>
      <p v-else-if="changes.length === 0" class="tip">这个工作空间没有未提交的改动。</p>

      <div v-else class="list">
        <button
          v-for="change in changes"
          :key="change.path"
          class="row"
          :title="change.path"
          @click="open(change)"
        >
          <span class="badge" :class="statusOf(change).tone">{{ statusOf(change).label }}</span>
          <span class="name">
            {{ change.path.split('/').pop() }}
            <em>{{ change.path.includes('/') ? change.path.slice(0, change.path.lastIndexOf('/')) : '' }}</em>
          </span>
          <Icon name="chevronRight" :size="11" class="go" />
        </button>
      </div>
    </template>

    <!-- diff 视图 -->
    <template v-else>
      <div class="rootbar">
        <button class="mini" title="返回变更列表" @click="((picked = null), (diff = ''))">
          <Icon name="arrowLeft" :size="12" />
        </button>
        <span class="branch" :title="picked.path">{{ picked.path.split('/').pop() }}</span>
        <span class="count">{{ picked.untracked ? '未跟踪' : picked.code.trim() || '已修改' }}</span>
      </div>

      <p v-if="diffLoading" class="tip">读取中…</p>
      <div v-else class="diff">
        <pre v-for="(line, index) in diffLines" :key="index" class="line" :class="line.cls">{{ line.text || ' ' }}</pre>
      </div>
    </template>
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

.branch {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-2);
  font-weight: 500;
}

.count {
  margin-left: auto;
  flex: none;
  font-size: 10.5px;
  color: var(--text-3);
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

.list {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 4px 0 12px;
}

.row {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px 6px 12px;
  font-size: 11.5px;
  color: var(--text);
  text-align: left;
}
.row:hover {
  background: var(--hover);
}

.badge {
  width: 18px;
  height: 18px;
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 5px;
  font-size: 10px;
  font-weight: 600;
  background: var(--fill);
  color: var(--text-2);
}
.badge.add {
  background: rgba(52, 199, 89, 0.14);
  color: #1f8f3d;
}
.badge.del {
  background: rgba(255, 59, 48, 0.13);
  color: #c22b22;
}
.badge.mod {
  background: rgba(255, 149, 0, 0.16);
  color: #a86400;
}

.name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.name em {
  font-style: normal;
  margin-left: 6px;
  font-size: 10.5px;
  color: var(--text-3);
}

.go {
  flex: none;
  color: var(--text-3);
}

.diff {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 6px 0 16px;
  background: var(--code-bg);
}

.line {
  margin: 0;
  padding: 0 12px;
  font-family: var(--font-mono);
  font-size: 10.5px;
  line-height: 1.65;
  white-space: pre-wrap;
  word-break: break-all;
  color: var(--text-2);
}
.line.add {
  background: rgba(52, 199, 89, 0.13);
  color: #1c7a35;
}
.line.del {
  background: rgba(255, 59, 48, 0.1);
  color: #a8271f;
}
.line.hunk {
  color: var(--text-3);
  background: var(--code-head);
}
.line.meta {
  color: var(--text-3);
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
