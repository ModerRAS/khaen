<script setup lang="ts">
/** 数据真实性标签（PLAN §11.5）：实测 / 示意 / 未接入。非实测数据必须带。 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { TruthKind } from '@/types/ui'

const props = defineProps<{
  /** 真实性种类。 */
  kind: TruthKind
}>()

const { t } = useI18n({ useScope: 'global' })

const LABEL_KEYS: Record<TruthKind, string> = {
  actual: 'truth.actual',
  demo: 'truth.demo',
  notWired: 'truth.notWired'
}

const label = computed(() => t(LABEL_KEYS[props.kind]))
</script>

<template>
  <span class="truth-tag" :class="'truth-tag--' + props.kind">{{ label }}</span>
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
