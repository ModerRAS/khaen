<script setup lang="ts">
/** 状态徽标：圆点 + 文本（PLAN §11.5 StatusBadge）。unknown 用灰色表示「未接入」。 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { StatusKind } from '@/types/ui'

const props = defineProps<{
  /** 徽标种类（决定圆点/文字颜色）。 */
  kind: StatusKind
  /** 左侧标签文案（已本地化）。 */
  label: string
  /** 右侧取值；缺省表示「未接入」。 */
  value?: string
}>()

const { t } = useI18n({ useScope: 'global' })

const valueText = computed(() => props.value ?? t('common.notWired'))
const tooltip = computed(() => props.label + ': ' + valueText.value)
</script>

<template>
  <span class="status-badge" :class="'status-badge--' + props.kind" :title="tooltip">
    <span class="status-badge__dot" aria-hidden="true" />
    <span class="status-badge__label">{{ props.label }}</span>
    <span class="status-badge__value">{{ valueText }}</span>
  </span>
</template>

<style scoped>
.status-badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 26px;
  padding: 0 8px;
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-tag);
  background: var(--khaen-panel-hover);
  font-size: 11px;
  white-space: nowrap;
}

.status-badge__dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--khaen-muted);
  flex: none;
}

.status-badge__label {
  color: var(--khaen-text-secondary);
}

.status-badge__value {
  color: var(--khaen-text);
  font-family: var(--khaen-font-mono);
}

.status-badge--running .status-badge__dot {
  background: var(--khaen-success);
}

.status-badge--faulted .status-badge__dot {
  background: var(--khaen-danger);
}

.status-badge--muted .status-badge__dot {
  background: var(--khaen-warning);
}

.status-badge--safe .status-badge__dot {
  background: var(--khaen-accent);
}

.status-badge--unknown .status-badge__dot {
  background: var(--khaen-muted);
}

.status-badge--unknown .status-badge__value {
  color: var(--khaen-muted);
}
</style>
