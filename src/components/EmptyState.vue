<script setup lang="ts">
import { computed } from 'vue'
import Icon from './Icon.vue'
import { useSettingsStore } from '@/stores/settings'

const emit = defineEmits<{ (e: 'pick', text: string): void }>()

const settingsStore = useSettingsStore()
const name = computed(() => settingsStore.settings.displayName || 'Shania')

const suggestions = [
  {
    icon: 'clock',
    title: '现在几点',
    desc: '让我调用工具查看本机时间',
    prompt: '现在几点了？',
  },
  {
    icon: 'file',
    title: '读一个文件',
    desc: '把文件路径发给我',
    prompt: '帮我读一下这个文件：D:/notes/todo.md',
  },
  {
    icon: 'zap',
    title: '解释一段代码',
    desc: '贴代码，我来讲清楚',
    prompt: '帮我解释这段代码：\n\n```ts\nconst total = items.reduce((acc, x) => acc + x.price, 0)\n```',
  },
  {
    icon: 'edit',
    title: '整理周报',
    desc: '零散要点变成一份周报',
    prompt: '帮我把下面这些要点整理成一份周报：\n- ',
  },
]
</script>

<template>
  <div class="empty">
    <div class="orb-wrap">
      <div class="glow" />
      <div class="orb">
        <Icon name="sparkles" :size="24" />
      </div>
    </div>

    <h1>你好，我是 {{ name }}</h1>
    <p class="lead">
      你桌面上的 AI 智能体。可以直接对话，也可以让我调用工具查看本机时间、读取文件、浏览目录。
    </p>

    <div class="cards">
      <button v-for="item in suggestions" :key="item.title" class="card" @click="emit('pick', item.prompt)">
        <span class="ico"><Icon :name="item.icon" :size="14" /></span>
        <span class="texts">
          <span class="title">{{ item.title }}</span>
          <span class="desc">{{ item.desc }}</span>
        </span>
      </button>
    </div>
  </div>
</template>

<style scoped>
.empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  padding: 0 0 4vh;
  animation: fade-up 0.4s var(--ease) both;
}

.orb-wrap {
  position: relative;
  margin-bottom: 22px;
}

.glow {
  position: absolute;
  inset: -26px;
  border-radius: 50%;
  background: radial-gradient(circle, rgba(10, 132, 255, 0.28), transparent 68%);
  filter: blur(10px);
}

.orb {
  position: relative;
  width: 50px;
  height: 50px;
  border-radius: 15px;
  display: grid;
  place-items: center;
  color: #fff;
  background: linear-gradient(145deg, #64b5ff 0%, #0a6ff0 52%, #7b5cff 100%);
  box-shadow: 0 10px 26px rgba(10, 111, 240, 0.34), inset 0 1px 0 rgba(255, 255, 255, 0.55);
}

h1 {
  margin: 0 0 9px;
  font-size: 22px;
  font-weight: 600;
  letter-spacing: -0.02em;
}

.lead {
  margin: 0 0 30px;
  max-width: 430px;
  font-size: 13px;
  line-height: 1.7;
  color: var(--text-2);
}

.cards {
  width: 100%;
  max-width: 560px;
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;
}

.card {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 12px 13px;
  text-align: left;
  border-radius: var(--radius-md);
  background: var(--surface);
  border: 1px solid var(--line-soft);
  box-shadow: var(--shadow-sm);
  transition: transform 0.18s var(--ease), border-color 0.18s var(--ease),
    box-shadow 0.18s var(--ease);
}
.card:hover {
  transform: translateY(-1.5px);
  border-color: color-mix(in srgb, var(--accent) 40%, var(--line));
  box-shadow: var(--shadow-md);
}
.card:active {
  transform: translateY(0) scale(0.99);
}

.ico {
  width: 27px;
  height: 27px;
  flex: none;
  border-radius: 8px;
  display: grid;
  place-items: center;
  background: var(--accent-soft);
  color: var(--accent);
}

.texts {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.title {
  font-size: 12.5px;
  font-weight: 500;
  color: var(--text);
}

.desc {
  font-size: 11px;
  line-height: 1.5;
  color: var(--text-3);
}

@media (max-width: 700px) {
  .cards {
    grid-template-columns: 1fr;
  }
}
</style>
