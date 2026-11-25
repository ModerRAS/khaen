<script setup lang="ts">
import { ElMessage } from 'element-plus'
import { computed } from 'vue'

const props = withDefaults(defineProps<{
  value?: string | null
  short?: number
  label?: string
  full?: boolean
}>(), {
  value: '',
  short: 18,
  label: '',
  full: false
})

const shown = computed(() => {
  const raw = props.value ?? ''
  if (!raw) { return '—' }
  if (props.full || raw.length <= props.short) { return raw }
  return raw.slice(0, props.short) + '…'
})

async function copy(): Promise<void> {
  const raw = props.value ?? ''
  if (!raw) { return }
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(raw)
    } else {
      const area = document.createElement('textarea')
      area.value = raw
      area.style.position = 'fixed'
      area.style.opacity = '0'
      document.body.appendChild(area)
      area.select()
      document.execCommand('copy')
      document.body.removeChild(area)
    }
    ElMessage.success('已复制')
  } catch {
    ElMessage.error('复制失败（浏览器限制）')
  }
}
</script>

<template>
  <span class="copy-wrap" :title="value ?? ''">
    <span v-if="label" class="copy-label">{{ label }}</span>
    <code class="copy-value">{{ shown }}</code>
    <el-button v-if="value" link size="small" class="copy-btn" @click="copy">复制</el-button>
  </span>
</template>

<style scoped>
.copy-wrap { display: inline-flex; align-items: center; gap: 6px; max-width: 100%; min-width: 0; }
.copy-label { color: var(--khaen-text-muted); font-size: 11.5px; flex: none; }
.copy-value { font-family: ui-monospace, SFMono-Regular, Consolas, monospace; font-size: 12px; word-break: break-all; }
.copy-btn { flex: none; padding: 0; height: auto; }
</style>
