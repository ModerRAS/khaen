<script setup lang="ts">
/**
 * 门禁卡（PLAN §10.3 / §11.4）：展示后端 wizard/state 给本步的 gate 事实。
 *
 * 纪律：gate 只来自后端；缺失 gate 一律按「不可继续」（fail closed）显示，绝不默认通过。
 * - passed：可以进入下一步；
 * - blocked：用户可修复，下一步禁用 + 原因；
 * - not_available：能力未接入（灰态），不产生数据、可继续（声明式缺口，不是用户可修复的门禁）。
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import TruthTag from '@/components/TruthTag.vue'
import type { WizardStepGate } from '@/api/calibration'
import type { TruthKind } from '@/types/ui'

const props = defineProps<{
  stepLabel: string
  gate: WizardStepGate | null
}>()

const { t } = useI18n({ useScope: 'global' })

const label = computed(() => {
  if (props.gate === null) {
    return t('calibration.gate.missing')
  }
  return t('calibration.gate.' + props.gate.status)
})
const toneClass = computed(() => (props.gate === null ? 'gate--missing' : 'gate--' + props.gate.status))
const truth = computed<TruthKind | null>(() => {
  if (props.gate === null || props.gate.status === 'not_available') {
    return 'notWired'
  }
  return null
})
const hint = computed(() => {
  if (props.gate === null) {
    return t('calibration.gate.failClosed')
  }
  if (props.gate.status === 'passed') {
    return t('calibration.gate.canContinue')
  }
  if (props.gate.status === 'blocked') {
    return t('calibration.gate.mustFix')
  }
  return t('calibration.gate.canSkip')
})
</script>

<template>
  <section class="gate-card" :class="toneClass" aria-live="polite">
    <header class="gate-card__head">
      <span class="gate-card__title">{{ t('calibration.gate.title') }}</span>
      <span class="gate-card__badge">
        <span class="gate-card__dot" aria-hidden="true" />
        <span>{{ label }}</span>
      </span>
      <TruthTag v-if="truth" :kind="truth" />
    </header>
    <p class="gate-card__step">{{ props.stepLabel }}</p>
    <p v-if="props.gate?.reason" class="gate-card__reason">{{ t('calibration.gate.reason', { reason: props.gate.reason }) }}</p>
    <p v-if="props.gate?.task" class="gate-card__task">{{ t('calibration.gate.task', { task: props.gate.task }) }}</p>
    <p class="gate-card__hint">{{ hint }}</p>
    <div class="gate-card__extra">
      <slot />
    </div>
  </section>
</template>

<style scoped>
.gate-card {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-4);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
  box-shadow: var(--khaen-shadow-card);
  min-width: 0;
}

.gate-card--passed {
  border-color: color-mix(in srgb, var(--khaen-success) 45%, transparent);
}

.gate-card--blocked {
  border-color: color-mix(in srgb, var(--khaen-warning) 45%, transparent);
}

.gate-card--not_available,
.gate-card--missing {
  border-style: dashed;
  border-color: var(--khaen-border-strong);
}

.gate-card__head {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.gate-card__title {
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
}

.gate-card__badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  margin-left: auto;
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  white-space: nowrap;
}

.gate-card__dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--khaen-muted);
}

.gate-card--passed .gate-card__dot {
  background: var(--khaen-success);
}

.gate-card--blocked .gate-card__dot {
  background: var(--khaen-warning);
}

.gate-card__step {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.gate-card__reason,
.gate-card__task,
.gate-card__hint {
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.gate-card__reason {
  color: var(--khaen-text);
}

.gate-card__task {
  color: var(--khaen-warning);
  font-family: var(--khaen-font-mono);
}

.gate-card__hint {
  color: var(--khaen-text-muted);
}

.gate-card__extra {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
}
</style>
