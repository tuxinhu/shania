<script setup lang="ts">
/**
 * 窗口按钮（关闭 / 最小化 / 最大化）。
 *
 * 窗口是无边框的（`decorations: false`），系统不画按钮，只能自己来。抽成组件是
 * 因为会话页和设置页各有一份——设置页整页独占时会话侧栏被隐藏，按钮不能跟着
 * 一起消失，那样用户就没法关窗口了。
 */
import Icon from './Icon.vue'
import { isTauri } from '@/lib/api'

async function win(action: 'close' | 'min' | 'max') {
  if (!isTauri) return
  const { getCurrentWindow } = await import('@tauri-apps/api/window')
  const w = getCurrentWindow()
  if (action === 'close') await w.close()
  else if (action === 'min') await w.minimize()
  else await w.toggleMaximize()
}
</script>

<template>
  <div class="traffic">
    <button class="tl close" title="关闭" @click="win('close')">
      <Icon name="x" :size="8" />
    </button>
    <button class="tl min" title="最小化" @click="win('min')">
      <span class="bar" />
    </button>
    <button class="tl max" title="最大化" @click="win('max')">
      <span class="plus" />
    </button>
  </div>
</template>

<style scoped>
.traffic {
  display: flex;
  gap: 8px;
  align-items: center;
}

.tl {
  width: 12px;
  height: 12px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  color: rgba(0, 0, 0, 0.55);
  transition: filter var(--dur) var(--ease);
  position: relative;
}
.tl.close {
  background: #ff5f57;
}
.tl.min {
  background: #febc2e;
}
.tl.max {
  background: #28c840;
}
.tl :deep(svg),
.tl .bar,
.tl .plus {
  opacity: 0;
  transition: opacity 0.12s var(--ease);
}
.traffic:hover .tl :deep(svg),
.traffic:hover .tl .bar,
.traffic:hover .tl .plus {
  opacity: 1;
}
.tl .bar {
  width: 6px;
  height: 1.5px;
  border-radius: 1px;
  background: rgba(0, 0, 0, 0.55);
}
.tl .plus {
  width: 6px;
  height: 6px;
  position: relative;
}
.tl .plus::before,
.tl .plus::after {
  content: '';
  position: absolute;
  background: rgba(0, 0, 0, 0.55);
  border-radius: 1px;
}
.tl .plus::before {
  left: 0;
  top: 2.2px;
  width: 6px;
  height: 1.5px;
}
.tl .plus::after {
  top: 0;
  left: 2.2px;
  height: 6px;
  width: 1.5px;
}
.tl:active {
  filter: brightness(0.86);
}
</style>
