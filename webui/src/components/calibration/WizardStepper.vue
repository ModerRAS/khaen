<script lang="ts">
/** 步骤条对外类型（普通 script 块：<script setup> 不能有 ES 导出）。 */
import type { StepDisplayState } from '@/api/calibration'

export interface WizardStepperItem {
  step: number
  key: string
  label: string
  state: StepDisplayState
}
</script>

<script setup lang="ts">
/**
 * 8 步向导步骤条（PLAN §10.3 / §11.4）。
 *
 * - 已完成步骤可点击回看（只读）；当前步高亮；未解锁步骤禁用（aria-disabled + title 原因）；
 * - not_available 步骤灰显并带「未接入」标记，但仍可点击查看原因（不伪造数据）；
 * - 窄屏横向滚动，不产生横向页面溢出（容器 overflow-x: auto）。
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, Lock, Minus, Warning } from '@element-plus/icons-vue'

const props = defineProps<{
  steps: WizardStepperItem[]
  current: number
}>()

const emit = defineEmits<{ (event: 'select', step: number): void }>()

const { t } = useI18n({ useScope: 'global' })

const currentIndex = computed(() => Math.min(Math.max(props.current, 1), props.steps.length))

function isDisabled(item: WizardStepperItem): boolean {
  return item.state === 'locked'
}

function stateTitle(item: WizardStepperItem): string {
  if (item.state === 'locked') {
    return t('calibration.gate.lockedStep')
  }
  if (item.state === 'notAvailable') {
    return t('calibration.gate.not_available')
  }
  return item.label
}

function onSelect(item: WizardStepperItem): void {
  if (isDisabled(item)) {
    return
  }
  emit('select', item.step)
}
</script>

<template>
  <nav class="wizard-stepper" :aria-label="t('calibration.wizard.stepNav')">
    <ol class="wizard-stepper__list" role="list">
      <li v-for="item in props.steps" :key="item.step" class="wizard-stepper__item">
        <button
          type="button"
          class="wizard-stepper__button"
          :class="['wizard-stepper__button--' + item.state, { 'is-current': item.step === currentIndex }]"
          :disabled="isDisabled(item)"
          :aria-current="item.step === currentIndex ? 'step' : undefined"
          :aria-disabled="isDisabled(item) ? 'true' : undefined"
          :title="stateTitle(item)"
          @click="onSelect(item)"
        >
          <span class="wizard-stepper__index" aria-hidden="true">
            <el-icon v-if="item.state === 'done'" :size="13"><Check /></el-icon>
            <el-icon v-else-if="item.state === 'notAvailable'" :size="13"><Minus /></el-icon>
            <el-icon v-else-if="item.state === 'blocked'" :size="13"><Warning /></el-icon>
            <el-icon v-else-if="item.state === 'locked'" :size="12"><Lock /></el-icon>
            <span v-else>{{ item.step }}</span>
          </span>
          <span class="wizard-stepper__label">
            <span class="wizard-stepper__step">{{ t('calibration.wizard.stepShort', { step: item.step }) }}</span>
            <span class="wizard-stepper__name">{{ item.label }}</span>
          </span>
        </button>
        <span v-if="item.step < props.steps.length" class="wizard-stepper__arrow" aria-hidden="true" />
      </li>
    </ol>
  </nav>
</template>

<style scoped>
.wizard-stepper {
  overflow-x: auto;
  padding: 2px 0;
  scrollbar-width: thin;
}

.wizard-stepper__list {
  display: flex;
  align-items: stretch;
  gap: var(--khaen-space-1);
  margin: 0;
  padding: 0;
  list-style: none;
  min-width: max-content;
}

.wizard-stepper__item {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-1);
}

.wizard-stepper__button {
  display: inline-flex;
  align-items: center;
  gap: var(--khaen-space-2);
  min-height: 44px;
  padding: 6px 10px;
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-button);
  background: var(--khaen-panel);
  color: var(--khaen-text-secondary);
  font-family: inherit;
  font-size: var(--khaen-font-aux);
  cursor: pointer;
  transition: border-color var(--khaen-transition), background var(--khaen-transition), color var(--khaen-transition);
  white-space: nowrap;
}

.wizard-stepper__button:hover:not(:disabled) {
  border-color: var(--khaen-border-strong);
  color: var(--khaen-text);
}

.wizard-stepper__button:disabled {
  cursor: not-allowed;
  opacity: 0.55;
}

.wizard-stepper__button.is-current {
  border-color: var(--khaen-accent);
  background: var(--khaen-accent-soft);
  color: var(--khaen-text);
}

.wizard-stepper__button--done .wizard-stepper__index {
  border-color: var(--khaen-success);
  color: var(--khaen-success);
}

.wizard-stepper__button--blocked .wizard-stepper__index {
  border-color: var(--khaen-warning);
  color: var(--khaen-warning);
}

.wizard-stepper__button--notAvailable .wizard-stepper__index {
  border-color: var(--khaen-muted);
  color: var(--khaen-muted);
}

.wizard-stepper__button--notAvailable .wizard-stepper__name {
  color: var(--khaen-muted);
}

.wizard-stepper__index {
  display: inline-grid;
  place-items: center;
  width: 24px;
  height: 24px;
  border: 1px solid var(--khaen-border-strong);
  border-radius: 50%;
  font-family: var(--khaen-font-mono);
  font-size: 11px;
  flex: none;
}

.wizard-stepper__label {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  line-height: 1.2;
}

.wizard-stepper__step {
  color: var(--khaen-text-muted);
  font-size: 10px;
  font-family: var(--khaen-font-mono);
}

.wizard-stepper__name {
  font-size: 12px;
}

.wizard-stepper__arrow {
  width: 8px;
  height: 1px;
  background: var(--khaen-border);
}
</style>
