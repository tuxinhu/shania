<script setup lang="ts">
/**
 * 可拖拽的宽度分隔条。
 *
 * 不直接碰任何组件的布局，只负责更新一个挂在 :root 上的 CSS 变量并记住值——
 * 谁引用这个变量谁就跟着变。这样侧栏、右侧面板、甚至消息列的居中补偿
 * （引用 --sidebar-w 做 calc）都能自动联动，不用互相通知。
 */
import { onBeforeUnmount, ref } from 'vue'

const props = defineProps<{
  /** 要更新的 CSS 变量名 */
  variable: string
  /** localStorage 键，记住用户拖到的宽度 */
  store: string
  min: number
  max: number
  initial: number
  /** 鼠标向右拖时宽度增大（左侧栏）还是减小（右侧面板） */
  grow: 'right' | 'left'
}>()

const dragging = ref(false)

function widthNow(): number {
  const saved = Number(localStorage.getItem(props.store))
  return saved >= props.min && saved <= props.max ? saved : props.initial
}

// 挂载即恢复记忆值，别等用户第一次拖
document.documentElement.style.setProperty(props.variable, `${widthNow()}px`)

function onDown(event: MouseEvent) {
  event.preventDefault()
  dragging.value = true
  const startX = event.clientX
  const start = widthNow()
  const sign = props.grow === 'right' ? 1 : -1

  const move = (ev: MouseEvent) => {
    const width = Math.round(
      Math.min(props.max, Math.max(props.min, start + (ev.clientX - startX) * sign)),
    )
    document.documentElement.style.setProperty(props.variable, `${width}px`)
    localStorage.setItem(props.store, String(width))
  }
  const up = () => {
    dragging.value = false
    window.removeEventListener('mousemove', move)
    window.removeEventListener('mouseup', up)
    document.body.classList.remove('col-resizing')
  }
  window.addEventListener('mousemove', move)
  window.addEventListener('mouseup', up)
  // 拖拽期间全局锁光标、禁止选中：划过文字不选、出面板光标不丢
  document.body.classList.add('col-resizing')
}

onBeforeUnmount(() => {
  document.body.classList.remove('col-resizing')
})
</script>

<template>
  <div class="handle" :class="{ dragging }" @mousedown="onDown" />
</template>

<style scoped>
.handle {
  position: absolute;
  top: 0;
  bottom: 0;
  width: 8px;
  cursor: col-resize;
  z-index: 30;
}
/* 视觉线只有 1px 宽，热区 8px：够好抓，又不挤内容 */
.handle::after {
  content: '';
  position: absolute;
  inset: 0 3.5px;
  background: transparent;
  transition: background 0.15s var(--ease);
}
.handle:hover::after,
.handle.dragging::after {
  background: var(--accent);
  opacity: 0.4;
}
</style>

<style>
/* 拖拽中的全局状态（非 scoped：要作用到整个文档） */
body.col-resizing {
  cursor: col-resize !important;
  user-select: none !important;
}
body.col-resizing * {
  pointer-events: none !important;
}
</style>
