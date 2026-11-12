<script setup lang="ts">
withDefaults(defineProps<{ name: string; size?: number | string }>(), { size: 16 })

const paths: Record<string, string> = {
  plus: '<path d="M12 5v14M5 12h14"/>',
  search: '<circle cx="11" cy="11" r="7"/><path d="m20 20-3.2-3.2"/>',
  trash:
    '<path d="M3 6h18M8 6V4.5A1.5 1.5 0 0 1 9.5 3h5A1.5 1.5 0 0 1 16 4.5V6"/><path d="M18.5 6l-.9 13.1A2 2 0 0 1 15.6 21H8.4a2 2 0 0 1-2-1.9L5.5 6"/><path d="M10 11v6M14 11v6"/>',
  pin: '<path d="M9 4h6l-.9 6.2 4.4 3.3v1.5H5.5v-1.5l4.4-3.3L9 4Z"/><path d="M12 15v5"/>',
  settings:
    '<path d="M4 21v-6M4 11V3M12 21v-9M12 8V3M20 21v-4M20 13V3"/><path d="M1.5 15h5M9.5 8h5M17.5 17h5"/>',
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
  moon: '<path d="M21 12.8A8.5 8.5 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z"/>',
  monitor: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>',
  send: '<path d="M4.7 11.9 20 4.3l-5.4 15.6-3.1-6.2-6.8-1.8Z"/><path d="m11.5 13.7 8.5-9.4"/>',
  stop: '<rect x="6.5" y="6.5" width="11" height="11" rx="2.4" fill="currentColor" stroke="none"/>',
  copy: '<rect x="9" y="9" width="11.5" height="11.5" rx="2.5"/><path d="M15 5.8A2.3 2.3 0 0 0 12.7 3.5H6A2.5 2.5 0 0 0 3.5 6v6.7A2.3 2.3 0 0 0 5.8 15"/>',
  check: '<path d="m4.5 12.5 5 5L20 6.5"/>',
  chevron: '<path d="m6.5 9.5 5.5 5.5 5.5-5.5"/>',
  sparkles:
    '<path d="M12 3.2l1.7 4.6 4.6 1.7-4.6 1.7L12 15.8l-1.7-4.6L5.7 9.5l4.6-1.7L12 3.2Z"/><path d="M18.8 15.2l.8 2 2 .8-2 .8-.8 2-.8-2-2-.8 2-.8.8-2Z"/>',
  tool: '<path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.8-3.8a6 6 0 0 1-8 8l-6.9 6.9a2.1 2.1 0 0 1-3-3l6.9-6.9a6 6 0 0 1 8-8l-3.8 3.8Z"/>',
  clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5.2l3.4 2"/>',
  file: '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8l-5-5Z"/><path d="M14 3v5h5"/><path d="M9 13h6M9 17h4"/>',
  folder: '<path d="M3 7.2A2.2 2.2 0 0 1 5.2 5h3.6l1.9 2.4H19a2.2 2.2 0 0 1 2.2 2.2v8.2A2.2 2.2 0 0 1 19 20H5.2A2.2 2.2 0 0 1 3 17.8V7.2Z"/>',
  cpu: '<rect x="4.5" y="4.5" width="15" height="15" rx="2.5"/><rect x="9" y="9" width="6" height="6" rx="1"/><path d="M9 2v2.5M15 2v2.5M9 19.5V22M15 19.5V22M2 9h2.5M2 15h2.5M19.5 9H22M19.5 15H22"/>',
  edit: '<path d="M12 20h8.5"/><path d="M16.4 3.6a2.1 2.1 0 0 1 3 3L7 19l-4.2 1.2L4 16 16.4 3.6Z"/>',
  x: '<path d="M6 6l12 12M18 6 6 18"/>',
  refresh: '<path d="M20 11.5A8 8 0 1 0 17.9 17"/><path d="M20 4.5v7h-7"/>',
  key: '<circle cx="8" cy="15.5" r="4"/><path d="m11 12.5 8.5-8.5M17.5 3.5 21 7M15 6l3 3"/>',
  server: '<rect x="3" y="4" width="18" height="7" rx="2.2"/><rect x="3" y="13" width="18" height="7" rx="2.2"/><path d="M7 7.5h.01M7 16.5h.01"/>',
  eye: '<path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7-10-7-10-7Z"/><circle cx="12" cy="12" r="3"/>',
  eyeOff:
    '<path d="M3 3l18 18"/><path d="M10.6 5.2A11 11 0 0 1 12 5c6.4 0 10 7 10 7a17.4 17.4 0 0 1-3.2 4.1M6.5 6.6A17 17 0 0 0 2 12s3.6 7 10 7a10.7 10.7 0 0 0 4.4-.9"/><path d="M9.9 9.9a3 3 0 0 0 4.2 4.2"/>',
  arrowDown: '<path d="M12 5v14M6 13l6 6 6-6"/>',
  message: '<path d="M21 11.8a8.2 8.2 0 0 1-8.2 8.2H7.4L3 22l1.3-4.2A8.2 8.2 0 1 1 21 11.8Z"/>',
  info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5.5M12 7.8h.01"/>',
  zap: '<path d="M13.2 2 4.5 13.8h6.3L10 22l8.7-11.8h-6.3L13.2 2Z"/>',
  brain:
    '<path d="M12 5.5a3 3 0 0 0-5.9.6A2.9 2.9 0 0 0 4 9a3 3 0 0 0 .6 4.7A3 3 0 0 0 7.5 19 3 3 0 0 0 12 18.5Z"/><path d="M12 5.5a3 3 0 0 1 5.9.6A2.9 2.9 0 0 1 20 9a3 3 0 0 1-.6 4.7A3 3 0 0 1 16.5 19 3 3 0 0 1 12 18.5Z"/><path d="M12 5.5v13"/>',
  globe: '<circle cx="12" cy="12" r="9"/><path d="M3.5 9h17M3.5 15h17"/><path d="M12 3a15 15 0 0 1 0 18 15 15 0 0 1 0-18Z"/>',
  download:
    '<path d="M12 3.5v11"/><path d="m8 10.8 4 3.9 4-3.9"/><path d="M4.5 17.2v1.6a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2v-1.6"/>',
  upload:
    '<path d="M12 20.5v-11"/><path d="m8 13.2 4-3.9 4 3.9"/><path d="M4.5 8.3V6.7a2 2 0 0 1 2-2h11a2 2 0 0 1 2 2v1.6"/>',
  mic:
    '<rect x="9" y="2.5" width="6" height="11" rx="3"/><path d="M5.5 11.5a6.5 6.5 0 0 0 13 0"/><path d="M12 18v3.5M8.5 21.5h7"/>',
  phone:
    '<path d="M6.7 3.4h2.1l1.8 4.3-2.2 1.5a12.7 12.7 0 0 0 6.4 6.4l1.5-2.2 4.3 1.8v2.1a2.6 2.6 0 0 1-2.6 2.6A14.5 14.5 0 0 1 4.1 6a2.6 2.6 0 0 1 2.6-2.6Z"/>',
  volume:
    '<path d="M11 5.2 6.4 9H3v6h3.4L11 18.8V5.2Z"/><path d="M15.2 8.6a4.8 4.8 0 0 1 0 6.8M18.2 5.4a9 9 0 0 1 0 13.2"/>',
  volumeOff:
    '<path d="M11 5.2 6.4 9H3v6h3.4L11 18.8V5.2Z"/><path d="m16 9.6 5 4.8M21 9.6l-5 4.8"/>',
  image:
    '<rect x="3" y="4" width="18" height="16" rx="2.5"/><circle cx="8.6" cy="9.6" r="1.6"/><path d="m4 17.5 5-4.6 4 3.6 3-2.6 4 3.6"/>',
  paperclip:
    '<path d="M20.4 11.6 12 20a5 5 0 0 1-7.1-7.1l8.5-8.4a3.4 3.4 0 0 1 4.8 4.8l-8.5 8.4a1.8 1.8 0 0 1-2.6-2.6l7.8-7.7"/>',
  archive:
    '<rect x="3" y="4" width="18" height="4.6" rx="1.5"/><path d="M4.9 8.6v9.2A2.2 2.2 0 0 0 7.1 20h9.8a2.2 2.2 0 0 0 2.2-2.2V8.6"/><path d="M9.8 12.6h4.4"/>',
  power: '<path d="M12 3v9"/><path d="M6.6 6.6a8 8 0 1 0 10.8 0"/>',
  layers:
    '<path d="m12 3 9 5-9 5-9-5 9-5Z"/><path d="m3 13 9 5 9-5"/><path d="m3 17.5 9 5 9-5"/>',
  closeCircle:
    '<circle cx="12" cy="12" r="9"/><path d="M9 9l6 6M15 9l-6 6"/>',
  terminal: '<path d="m5 7.5 4.2 4.5L5 16.5"/><path d="M12.8 17h6.2"/>',
  git: '<circle cx="6.2" cy="5.6" r="2.2"/><circle cx="6.2" cy="18.4" r="2.2"/><circle cx="17.8" cy="9.4" r="2.2"/><path d="M6.2 7.8v8.4"/><path d="M17.8 11.6c0 3.6-3.4 4.6-6.2 4.9H8.4"/>',
  chevronRight: '<path d="m9.5 6.5 5.5 5.5-5.5 5.5"/>',
  arrowLeft: '<path d="M19.5 12h-15"/><path d="m11 5.5-6.5 6.5 6.5 6.5"/>',
  arrowRight: '<path d="M4.5 12h15"/><path d="m13 5.5 6.5 6.5-6.5 6.5"/>',
  external:
    '<path d="M13.5 4H20v6.5"/><path d="M20 4 11 13"/><path d="M18.5 14.5V19a2 2 0 0 1-2 2h-11a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2H10"/>',
  home: '<path d="m3 10.6 9-7.1 9 7.1"/><path d="M5.6 9.4V20h12.8V9.4"/>',
}
</script>

<template>
  <svg
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.7"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
    v-html="paths[name] ?? ''"
  />
</template>

<style scoped>
svg {
  display: block;
  flex: none;
}
</style>
