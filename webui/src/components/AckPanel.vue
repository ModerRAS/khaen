<script setup lang="ts">
// 操作回执：明确区分 synthetic / actual / unsupported / error，并保留原始 JSON。
import { computed } from 'vue'
import type { AckView } from '@/api/types'

const props = defineProps<{ ack: AckView | null; title?: string }>()

const kindText = computed(() => {
  switch (props.ack?.kind) {
    case 'synthetic': return 'synthetic（合成回执，非 actual）'
    case 'actual': return 'actual（真实回执）'
    case 'unsupported': return 'unsupported（后端未支持，未生效）'
    case 'error': return 'error（失败，未生效）'
    case 'info': return 'info'
    default: return ''
  }
})

const bannerClass = computed(() => {
  switch (props.ack?.kind) {
    case 'synthetic': return 'banner banner--warn'
    case 'actual': return 'banner banner--ok'
    case 'unsupported': return 'banner banner--info'
    case 'error': return 'banner banner--danger'
    default: return 'banner'
  }
})

const rawText = computed(() => {
  if (!props.ack?.raw) { return '' }
  try { return JSON.stringify(props.ack.raw, null, 2) } catch { return String(props.ack.raw) }
})
</script>

<template>
  <div v-if="ack" :class="bannerClass" role="status" aria-live="polite">
    <div style="flex: 1; min-width: 0">
      <div class="banner__title">
        {{ title || '操作回执' }}
        <span class="chip" :class="{
          'chip--warn': ack.kind === 'synthetic',
          'chip--ok': ack.kind === 'actual',
          'chip--info': ack.kind === 'unsupported',
          'chip--danger': ack.kind === 'error'
        }">{{
          ack.kind === 'synthetic' ? 'synthetic' : ack.kind === 'actual' ? 'actual' : ack.kind
        }}</span>
      </div>
      <div class="banner__body">{{ ack.detail }}</div>
      <div v-if="ack.engine || ack.synthetic !== null" class="banner__body" style="margin-top: 4px">
        <span v-if="ack.engine">engine={{ ack.engine }} · </span>
        <span v-if="ack.synthetic !== null">synthetic={{ ack.synthetic }} · </span>
        <span v-if="ack.actual !== null">actual={{ ack.actual }}</span>
      </div>
      <el-collapse v-if="rawText" style="margin-top: 8px">
        <el-collapse-item title="原始回执 JSON" name="raw">
          <pre class="spoiler-json">{{ rawText }}</pre>
        </el-collapse-item>
      </el-collapse>
    </div>
  </div>
</template>
