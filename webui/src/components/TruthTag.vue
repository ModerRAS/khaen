<script setup lang="ts">
/** 数据真实性标签（PLAN §11.5）：实测 / 示意 / 未接入。非实测数据必须带；null 字段用 reason 说明原因。 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { TruthKind } from '@/types/ui'

const props = defineProps<{
  /** 真实性种类。 */
  kind: TruthKind
  /** 缺失原因（displayed as tooltip；来自 /api/status 的 reasons 映射）。 */
  reason?: string | null
}>()

const { t } = useI18n({ useScope: 'global' })

const LABEL_KEYS: Record<TruthKind, string> = {
  actual: 'truth.actual',
  demo: 'truth.demo',
  notWired: 'truth.notWired'
}

const label = computed(() => t(LABEL_KEYS[props.kind]))
const tooltip = computed(() => {
  if (typeof props.reason === 'string' && props.reason.length > 0) {
    return label.value + ': ' + props.reason
  }
  return label.value
})
</script>

<template>
  <span class="truth-tag" :class="'truth-tag--' + props.kind" :title="tooltip">{{ label }}</span>
</template>

<style scoped>
.truth-tag {
  display: inline-flex;
  align-items: center;
  height: 20px;
  padding: 0 6px;
  border-radius: var(--khaen-radius-tag);
  border: 1px solid var(--khaen-border);
  font-size: 11px;
  font-weight: 500;
  white-space: nowrap;
  flex: none;
}

.truth-tag--actual {
  border-color: color-mix(in srgb, var(--khaen-success) 45%, transparent);
  background: var(--khaen-success-soft);
  color: var(--khaen-success);
}

.truth-tag--demo {
  border-color: color-mix(in srgb, var(--khaen-warning) 45%, transparent);
  background: var(--khaen-warning-soft);
  color: var(--khaen-warning);
}

.truth-tag--notWired {
  border-color: var(--khaen-border);
  background: var(--khaen-muted-soft);
  color: var(--khaen-muted);
}
</style>
