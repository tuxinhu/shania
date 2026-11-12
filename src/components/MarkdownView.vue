<script setup lang="ts">
import { computed } from 'vue'
import { isTauri } from '@/lib/api'
import { copyText } from '@/lib/clipboard'
import { renderMarkdown } from '@/lib/markdown'

const props = defineProps<{ content: string }>()

const html = computed(() => renderMarkdown(props.content))

async function onClick(event: MouseEvent) {
  const target = event.target as HTMLElement

  const copyBtn = target.closest<HTMLButtonElement>('.code-copy')
  if (copyBtn) {
    const raw = decodeURIComponent(copyBtn.dataset.code || '')
    const ok = await copyText(raw)
    copyBtn.textContent = ok ? '已复制' : '复制失败'
    copyBtn.classList.add('copied')
    window.setTimeout(() => {
      copyBtn.textContent = '复制'
      copyBtn.classList.remove('copied')
    }, 1400)
    return
  }

  const link = target.closest<HTMLAnchorElement>('a[href]')
  if (link) {
    event.preventDefault()
    const url = link.href
    if (!/^https?:/i.test(url)) return
    if (isTauri) {
      const { openUrl } = await import('@tauri-apps/plugin-opener')
      await openUrl(url)
    } else {
      window.open(url, '_blank', 'noopener,noreferrer')
    }
  }
}
</script>

<template>
  <div class="md-body" @click="onClick" v-html="html" />
</template>
