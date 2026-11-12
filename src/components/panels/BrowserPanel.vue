<script setup lang="ts">
/**
 * 内置浏览器面板。
 *
 * 真正的网页由 Rust 侧创建的子 webview 渲染，这个组件只提供一个「占位框」：
 * 把 .host 的屏幕坐标算出来交给后端，webview 就浮在上面。所以：
 * - 任何尺寸变化（面板开合、窗口缩放、最大化）都要重新上报，否则会错位
 * - .host 上的东西点不到（被 webview 盖住），交互一律放在框外面
 */
import { nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import Icon from '../Icon.vue'
import { isTauri } from '@/lib/api'
import { browserBounds, browserClose, browserOpen, browserScript, browserUrl } from '@/lib/workspace'

const DEFAULT_URL = 'https://www.bing.com'

const host = ref<HTMLElement | null>(null)
const address = ref(DEFAULT_URL)
const editing = ref('')
const focused = ref(false)
const loading = ref(false)

let observer: ResizeObserver | null = null
let poller: number | null = null

function box() {
  const el = host.value
  if (!el) return { x: 0, y: 0, width: 0, height: 0 }
  const r = el.getBoundingClientRect()
  // 无边框窗口：页面左上角就是客户区左上角，CSS px 即逻辑像素，不用换算 DPI
  return { x: r.left, y: r.top, width: r.width, height: r.height }
}

function sync() {
  if (!isTauri) return
  void browserBounds(box()).catch(() => {})
}

async function go(url: string) {
  if (!url.trim()) return
  loading.value = true
  try {
    await browserOpen(url, box())
    address.value = url
  } finally {
    loading.value = false
  }
}

function submit() {
  const value = (focused.value ? editing.value : address.value).trim()
  if (value) void go(value)
}

function act(script: string) {
  void browserScript(script).catch(() => {})
}

onMounted(async () => {
  await nextTick()
  if (isTauri) await go(DEFAULT_URL).catch(() => {})

  observer = new ResizeObserver(sync)
  if (host.value) observer.observe(host.value)
  window.addEventListener('resize', sync)

  // webview 里点链接后地址会变，但那边没有回调通路，只能轮询回来对齐地址栏。
  // 2 秒一次，只在面板开着的时候跑。
  poller = window.setInterval(() => {
    void browserUrl()
      .then((url) => {
        if (url && url !== 'about:blank' && !focused.value) address.value = url
      })
      .catch(() => {})
  }, 2000)
})

onBeforeUnmount(() => {
  observer?.disconnect()
  window.removeEventListener('resize', sync)
  if (poller) window.clearInterval(poller)
  // 组件销毁了但 webview 还在的话，它会一直浮在界面上挡着别的东西
  void browserClose().catch(() => {})
})
</script>

<template>
  <div class="wrap">
    <div class="bar">
      <button class="nav" title="后退" @click="act('history.back()')">
        <Icon name="arrowLeft" :size="13" />
      </button>
      <button class="nav" title="前进" @click="act('history.forward()')">
        <Icon name="arrowRight" :size="13" />
      </button>
      <button class="nav" title="刷新" @click="act('location.reload()')">
        <Icon name="refresh" :size="13" />
      </button>

      <input
        v-model="address"
        class="addr"
        spellcheck="false"
        placeholder="输入网址或搜索词，回车打开"
        @focus="((focused = true), (editing = address))"
        @blur="(focused = false)"
        @keydown.enter.prevent="submit"
      />

      <button class="nav" title="重新加载地址" @click="submit">
        <Icon name="chevronRight" :size="13" />
      </button>
    </div>

    <!-- 网页本体由后端 webview 填进来，这个 div 只负责报坐标 -->
    <div ref="host" class="host">
      <p v-if="!isTauri" class="tip">浏览器预览模式下不显示内置浏览器，请在桌面端使用。</p>
      <p v-else-if="loading" class="tip">正在打开…</p>
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

.bar {
  flex: none;
  display: flex;
  align-items: center;
  gap: 2px;
  padding: 8px 8px;
  border-bottom: 1px solid var(--line-soft);
}

.nav {
  width: 24px;
  height: 26px;
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 6px;
  color: var(--text-3);
}
.nav:hover {
  background: var(--hover);
  color: var(--text);
}

.addr {
  flex: 1;
  min-width: 0;
  height: 26px;
  margin: 0 4px;
  padding: 0 9px;
  border-radius: 7px;
  background: var(--fill);
  font-size: 11.5px;
  color: var(--text);
}
.addr:focus {
  outline: none;
  box-shadow: inset 0 0 0 1px var(--accent);
  background: var(--surface);
}

.host {
  flex: 1;
  min-height: 0;
  position: relative;
  background: var(--surface);
}

.tip {
  margin: 0;
  padding: 22px 18px;
  font-size: 11.5px;
  line-height: 1.7;
  color: var(--text-3);
}
</style>
