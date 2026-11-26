<script setup lang="ts">
/** 校准 verdict 徽标（PASS/FAIL/INCOMPLETE + TruthTag：mock/fixture 引擎标「示意」）。 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import TruthTag from '@/components/TruthTag.vue'
import { jobResultTruth, verdictTone, type CalibrationVerdict } from '@/api/calibration'
import type { TruthKind } from '@/types/ui'

const props = defineProps<{
  verdict: CalibrationVerdict | null
  engine?: string | null
  large?: boolean
  /** 显式真实性（调用方已按 result.measured/simulated 判过时传入，优先于 engine 推断）。 */
  truth?: TruthKind | null
}>()

const { t } = useI18n({ useScope: 'global' })

const tone = computed(() => verdictTone(props.verdict))
const text = computed(() => (props.verdict === null ? t('calibration.verdict.none') : props.verdict))
const truth = computed(() => props.truth ?? jobResultTruth(props.engine ?? null))
</script>

<template>
  <span class="verdict" :class="[tone ? 'verdict--' + tone : 'verdict--none', { 'verdict--large': props.large }]">
    <span class="verdict__text">{{ text }}</span>
    <TruthTag :kind="truth" :reason="props.engine ? t('calibration.engine.label', { engine: props.engine }) : null" />
  </span>
</template>

<style scoped>
.verdict {
  display: inline-flex;
  align-items: center;
  gap: var(--khaen-space-2);
}

.verdict__text {
  display: inline-flex;
  align-items: center;
  padding: 2px 10px;
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-tag);
  background: var(--khaen-panel-hover);
  color: var(--khaen-text-secondary);
  font-family: var(--khaen-font-mono);
  font-weight: 600;
  font-size: var(--khaen-font-body);
  letter-spacing: 0.04em;
}

.verdict--large .verdict__text {
  font-size: 22px;
  padding: 6px 16px;
}

.verdict--ok .verdict__text {
  border-color: color-mix(in srgb, var(--khaen-success) 45%, transparent);
  background: var(--khaen-success-soft);
  color: var(--khaen-success);
}

.verdict--bad .verdict__text {
  border-color: color-mix(in srgb, var(--khaen-danger) 45%, transparent);
  background: var(--khaen-danger-soft);
  color: var(--khaen-danger);
}

.verdict--warn .verdict__text {
  border-color: color-mix(in srgb, var(--khaen-warning) 45%, transparent);
  background: var(--khaen-warning-soft);
  color: var(--khaen-warning);
}
</style>
