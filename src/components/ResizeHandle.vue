<script setup lang="ts">
/**
 * 可拖拽的宽度分隔条。
 *
 * 不直接碰任何组件的布局，只负责更新一个挂在 :root 上的 CSS 变量并记住值——
 * 谁引用这个变量谁就跟着变。这样侧栏、右侧面板、对话列（引用 --col-left 做
 * 偏移）都能自动联动，不用互相通知。
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
let cleanup: (() => void) | null = null

function widthNow(): number {
  const saved = Number(localStorage.getItem(props.store))
  return saved >= props.min && saved <= props.max ? saved : props.initial
}

function apply(width: number) {
  const w = Math.round(Math.min(props.max, Math.max(props.min, width)))
  document.documentElement.style.setProperty(props.variable, `${w}px`)
  localStorage.setItem(props.store, String(w))
}

// 挂载即恢复记忆值，别等用户第一次拖
document.documentElement.style.setProperty(props.variable, `${widthNow()}px`)

/**
 * 用 Pointer Events + setPointerCapture，而不是 mousemove on window。
 *
 * 关键差别在「松手时鼠标已经跑到窗口外面」这种情况：`mouseup` 派发到别的
 * 窗口去了，window 上的监听器收不到 → 拖拽状态永远不结束。老实现还顺手给
 * 所有元素加了 pointer-events: none 防止划过选中，一旦卡住，整个界面就点
 * 不动了，表现出来的就是「拖了一下，程序死了」。
 *
 * 指针捕获保证后续 pointermove/pointerup 都回到这个元素，系统层面兜住越界；
 * 再监听 lostpointercapture / pointercancel 兜住 Alt+Tab 之类的中断。
 */
function onDown(event: PointerEvent) {
  if (event.button !== 0) return
  event.preventDefault()

  const el = event.currentTarget as HTMLElement
  const pointerId = event.pointerId
  el.setPointerCapture(pointerId)

  const startX = event.clientX
  const start = widthNow()
  const sign = props.grow === 'right' ? 1 : -1
  dragging.value = true
  document.body.classList.add('col-resizing')

  const move = (ev: PointerEvent) => apply(start + (ev.clientX - startX) * sign)

  const end = () => {
    dragging.value = false
    document.body.classList.remove('col-resizing')
    el.removeEventListener('pointermove', move)
    el.removeEventListener('pointerup', end)
    el.removeEventListener('pointercancel', end)
    el.removeEventListener('lostpointercapture', end)
    if (el.hasPointerCapture(pointerId)) el.releasePointerCapture(pointerId)
    cleanup = null
  }

  el.addEventListener('pointermove', move)
  el.addEventListener('pointerup', end)
  el.addEventListener('pointercancel', end)
  el.addEventListener('lostpointercapture', end)
  cleanup = end
}

/** 双击复位：拖歪了不用去翻 localStorage */
function onReset() {
  apply(props.initial)
  cleanup?.()
}

onBeforeUnmount(() => {
  document.body.classList.remove('col-resizing')
  cleanup?.()
})
</script>

<template>
  <div
    class="handle"
    :class="{ dragging }"
    title="拖动调整宽度，双击复位"
    @pointerdown="onDown"
    @dblclick="onReset"
  />
</template>

<style scoped>
.handle {
  position: absolute;
  top: 0;
  bottom: 0;
  /* 热区 10px：1px 的线太细，抓不住；又不能太宽，否则挡住内容的点击 */
  width: 10px;
  cursor: col-resize;
  z-index: 30;
  touch-action: none;
}
/* 视觉线只有 1px 宽，落在边界上 */
.handle::after {
  content: '';
  position: absolute;
  inset: 0 4.5px;
  background: transparent;
  transition: background 0.15s var(--ease), opacity 0.15s var(--ease);
}
.handle:hover::after,
.handle.dragging::after {
  background: var(--accent);
  opacity: 0.45;
}
</style>

<style>
/* 拖拽中的全局状态（非 scoped：要作用到整个文档） */
body.col-resizing,
body.col-resizing * {
  cursor: col-resize !important;
  user-select: none !important;
}
</style>
