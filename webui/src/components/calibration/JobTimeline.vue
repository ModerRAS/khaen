<script setup lang="ts">
/**
 * 校准 job 步骤时间线（PLAN §4.7：compile→levels→closed_loop→bundle，不允许跳步直接成功）。
 * 所有状态/时间/sha 都来自 daemon 的 job 详情；缺失显示「—」，不补假进度。
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, Clock, Loading, Minus, Remove } from '@element-plus/icons-vue'
import TruthTag from '@/components/TruthTag.vue'
import { JOB_STEP_NAMES, type CalibrationStep } from '@/api/calibration'
import { formatTimestamp, shortenId } from '@/utils/format'
import type { TruthKind } from '@/types/ui'

const props = defineProps<{
  steps: CalibrationStep[]
  truth: TruthKind
}>()

const { t, locale } = useI18n({ useScope: 'global' })

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))

function stepLabel(name: string): string {
  if ((JOB_STEP_NAMES as readonly string[]).includes(name)) {
    return t('calibration.step.jobNames.' + name)
  }
  // 后端新增步骤：原样显示，不猜测含义。
  return name
}

function statusLabel(status: CalibrationStep['status']): string {
  return t('calibration.jobStepStatus.' + status)
}

function timeText(iso: string | null): string {
  return formatTimestamp(iso, localeId.value) ?? t('common.dash')
}

function iconFor(status: CalibrationStep['status']) {
  if (status === 'succeeded') {
    return Check
  }
  if (status === 'running') {
    return Loading
  }
  if (status === 'failed') {
    return Remove
  }
  if (status === 'cancelled') {
    return Minus
  }
  return Clock
}
</script>

<template>
  <ol class="job-timeline" role="list">
    <li v-for="step in props.steps" :key="step.name" class="job-timeline__item" :class="'job-timeline__item--' + step.status">
      <span class="job-timeline__icon" aria-hidden="true">
        <el-icon :size="14" :class="{ 'is-loading': step.status === 'running' }">
          <component :is="iconFor(step.status)" />
        </el-icon>
      </span>
      <div class="job-timeline__body">
        <div class="job-timeline__head">
          <span class="job-timeline__name">{{ stepLabel(step.name) }}</span>
          <span class="job-timeline__status">{{ statusLabel(step.status) }}</span>
          <TruthTag :kind="props.truth" />
        </div>
        <p class="job-timeline__meta">
          <span>{{ t('calibration.job.started') }}: {{ timeText(step.started_at) }}</span>
          <span>{{ t('calibration.job.finished') }}: {{ timeText(step.finished_at) }}</span>
        </p>
        <p v-if="step.sha256" class="job-timeline__sha" :title="step.sha256">
          sha256: {{ shortenId(step.sha256, 16, 12) }}
        </p>
        <p v-if="step.detail" class="job-timeline__detail">{{ step.detail }}</p>
      </div>
    </li>
  </ol>
</template>

<style scoped>
.job-timeline {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  margin: 0;
  padding: 0;
  list-style: none;
}

.job-timeline__item {
  display: flex;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-2);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-button);
  background: var(--khaen-panel);
  min-width: 0;
}

.job-timeline__item--running {
  border-color: var(--khaen-accent);
}

.job-timeline__item--failed {
  border-color: color-mix(in srgb, var(--khaen-danger) 45%, transparent);
}

.job-timeline__item--succeeded .job-timeline__icon {
  color: var(--khaen-success);
}

.job-timeline__item--running .job-timeline__icon {
  color: var(--khaen-accent);
}

.job-timeline__item--failed .job-timeline__icon {
  color: var(--khaen-danger);
}

.job-timeline__item--cancelled .job-timeline__icon,
.job-timeline__item--skipped .job-timeline__icon,
.job-timeline__item--pending .job-timeline__icon {
  color: var(--khaen-muted);
}

.job-timeline__icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  flex: none;
}

.job-timeline__body {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
  flex: 1 1 auto;
}

.job-timeline__head {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.job-timeline__name {
  font-weight: 600;
}

.job-timeline__status {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.job-timeline__meta {
  display: flex;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
  color: var(--khaen-text-muted);
  font-size: 11px;
  font-family: var(--khaen-font-mono);
}

.job-timeline__sha {
  color: var(--khaen-text-secondary);
  font-size: 11px;
  font-family: var(--khaen-font-mono);
  overflow-wrap: anywhere;
}

.job-timeline__detail {
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}
</style>
