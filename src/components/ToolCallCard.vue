<script setup lang="ts">
import { computed, ref } from 'vue'
import Icon from './Icon.vue'
import { copyText } from '@/lib/clipboard'
import type { ToolCallRecord } from '@/lib/types'

const props = defineProps<{ call: ToolCallRecord }>()

const expanded = ref(false)
const resultOpen = ref(false)
const resultFull = ref(false)
const copied = ref(false)

const TOOL_META: Record<string, { label: string; icon: string }> = {
  get_current_time: { label: '获取当前时间', icon: 'clock' },
  get_system_info: { label: '读取系统信息', icon: 'cpu' },
  read_text_file: { label: '读取文件', icon: 'file' },
  list_directory: { label: '浏览目录', icon: 'folder' },
}

const meta = computed(
  () => TOOL_META[props.call.name] ?? { label: props.call.name, icon: 'tool' },
)

const statusText = computed(() => {
  if (props.call.status === 'running') return '运行中'
  if (props.call.status === 'error') return '失败'
  return props.call.elapsedMs != null ? `${props.call.elapsedMs} ms` : '完成'
})

const summary = computed(() => {
  const raw = props.call.arguments?.trim()
  if (!raw || raw === '{}') return ''
  try {
    const obj = JSON.parse(raw) as Record<string, unknown>
    return Object.entries(obj)
      .map(([k, v]) => `${k}: ${String(v)}`)
      .join(' · ')
  } catch {
    return raw
  }
})

/** 结果规模：行数 + 字符数，用于决定是否需要折叠 */
const resultStat = computed(() => {
  const text = props.call.result ?? ''
  if (!text) return null
  const lines = text.split('\n').length
  const chars = text.length
  return {
    lines,
    chars,
    label: `${lines} 行 · ${chars.toLocaleString('en-US')} 字符`,
    long: lines > 14 || chars > 1200,
  }
})

async function copyResult() {
  const ok = await copyText(props.call.result ?? '')
  if (!ok) return
  copied.value = true
  window.setTimeout(() => (copied.value = false), 1400)
}
</script>

<template>
  <div class="tool-card" :class="call.status">
    <button class="tool-head" type="button" @click="expanded = !expanded">
      <span class="tool-icon">
        <Icon :name="meta.icon" :size="13" />
      </span>
      <span class="tool-name">{{ meta.label }}</span>
      <code class="tool-fn">{{ call.name }}</code>

      <span class="tool-status">
        <template v-if="call.status === 'running'">
          <span class="spinner" />
        </template>
        <template v-else-if="call.status === 'done'">
          <Icon name="check" :size="11" />
        </template>
        <template v-else>
          <Icon name="x" :size="11" />
        </template>
        <span>{{ statusText }}</span>
      </span>

      <Icon class="chev" :class="{ open: expanded }" name="chevron" :size="13" />
    </button>

    <div v-if="!expanded && summary" class="tool-summary">{{ summary }}</div>

    <div v-if="expanded" class="tool-body">
      <div class="tool-block">
        <div class="tool-block-label">参数</div>
        <pre class="args">{{ call.arguments || '{}' }}</pre>
      </div>

      <div v-if="resultStat" class="result-panel">
        <div class="result-bar">
          <button class="result-toggle" type="button" @click="resultOpen = !resultOpen">
            <Icon class="chev sm" :class="{ open: resultOpen }" name="chevron" :size="11" />
            <span class="result-title">返回</span>
            <span class="result-meta">{{ resultStat.label }}</span>
          </button>
          <button class="result-copy" type="button" title="复制结果" @click="copyResult">
            <Icon :name="copied ? 'check' : 'copy'" :size="11" />
          </button>
        </div>

        <div v-if="resultOpen" class="result-body" :class="{ full: resultFull }">
          <pre>{{ call.result }}</pre>
        </div>

        <button
          v-if="resultOpen && resultStat.long"
          class="result-more"
          type="button"
          @click="resultFull = !resultFull"
        >
          {{ resultFull ? '收起' : `展开全部（${resultStat.lines} 行）` }}
        </button>
      </div>

      <div v-else-if="call.status === 'running'" class="result-pending">
        <span class="spinner" />
        <span>正在执行…</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.tool-card {
  margin: 8px 0;
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  background: var(--surface-2);
  overflow: hidden;
  max-width: 100%;
}
.tool-card.error {
  border-color: color-mix(in srgb, var(--red) 32%, var(--line));
}

.tool-head {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 10px;
  font-size: 12px;
  text-align: left;
  transition: background 0.14s var(--ease);
}
.tool-head:hover {
  background: var(--hover);
}

.tool-icon {
  width: 21px;
  height: 21px;
  flex: none;
  display: grid;
  place-items: center;
  border-radius: 6px;
  background: var(--surface);
  border: 1px solid var(--line-soft);
  color: var(--text-2);
}

.tool-name {
  font-weight: 500;
  color: var(--text);
}

.tool-fn {
  font-family: var(--font-mono);
  font-size: 10.5px;
  color: var(--text-3);
  padding: 0 5px;
  border-radius: 4px;
  background: var(--fill);
}

.tool-status {
  margin-left: auto;
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 10.5px;
  color: var(--text-2);
  font-variant-numeric: tabular-nums;
}
.tool-card.done .tool-status {
  color: var(--green);
}
.tool-card.error .tool-status {
  color: var(--red);
}

.spinner {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  border: 1.5px solid var(--fill-strong);
  border-top-color: var(--accent);
  animation: spin 0.8s linear infinite;
}

.chev {
  color: var(--text-3);
  transition: transform 0.18s var(--ease);
}
.chev.open {
  transform: rotate(180deg);
}
.chev.sm {
  transition: transform 0.16s var(--ease);
}

.tool-summary {
  padding: 0 10px 8px 39px;
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--text-3);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.tool-body {
  border-top: 1px solid var(--line-soft);
  padding: 9px 10px 10px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  animation: fade-up 0.16s var(--ease);
}

.tool-block-label {
  font-size: 10px;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  color: var(--text-3);
  margin-bottom: 3px;
}

pre {
  margin: 0;
  padding: 8px 10px;
  background: var(--code-bg);
  border: 1px solid var(--line-soft);
  border-radius: 7px;
  font-family: var(--font-mono);
  font-size: 11.5px;
  line-height: 1.55;
  color: var(--text-2);
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-word;
}
pre.args {
  max-height: 120px;
}

/* ---------- 结果折叠面板 ---------- */
.result-panel {
  border: 1px solid var(--line-soft);
  border-radius: 7px;
  background: var(--code-bg);
  overflow: hidden;
}

.result-bar {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 0 6px 0 8px;
  height: 28px;
}

.result-toggle {
  flex: 1;
  min-width: 0;
  height: 100%;
  display: flex;
  align-items: center;
  gap: 6px;
  text-align: left;
  color: var(--text-2);
}
.result-toggle:hover .result-title {
  color: var(--text);
}

.result-title {
  font-size: 11.5px;
  font-weight: 500;
  transition: color 0.14s var(--ease);
}

.result-meta {
  font-size: 10.5px;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.result-copy {
  flex: none;
  width: 22px;
  height: 22px;
  display: grid;
  place-items: center;
  border-radius: 5px;
  color: var(--text-3);
  transition: background 0.14s var(--ease), color 0.14s var(--ease);
}
.result-copy:hover {
  background: var(--fill);
  color: var(--text);
}

.result-body {
  border-top: 1px solid var(--line-soft);
  max-height: 300px;
  overflow: auto;
  animation: fade-up 0.16s var(--ease);
}
.result-body.full {
  max-height: none;
}
.result-body pre {
  border: none;
  border-radius: 0;
  background: transparent;
  padding: 8px 10px;
}

.result-more {
  width: 100%;
  height: 26px;
  border-top: 1px solid var(--line-soft);
  font-size: 11px;
  color: var(--text-3);
  background: var(--surface-2);
  transition: color 0.14s var(--ease), background 0.14s var(--ease);
}
.result-more:hover {
  color: var(--text);
  background: var(--fill);
}

.result-pending {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11.5px;
  color: var(--text-3);
}
</style>
