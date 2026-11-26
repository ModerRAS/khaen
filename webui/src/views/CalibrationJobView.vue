<script setup lang="ts">
/**
 * 校准 job 详情页（PLAN §5.1 / §6.7；任务 T37）。
 *
 * - GET /api/calibration/jobs/{id}：状态机 + 步骤 + 产物 + verdict + apply_state；
 * - active job 轮询真实状态（终态即停）；页面隐藏暂停；
 * - 应用/回滚走 ApplyPanel（before/after + 二次确认 + 回执），成功后刷新 status（回读 plan 版本/graph_id）；
 * - 所有数值来自 daemon；mock/fixture 引擎标「示意」。
 */
import { computed, onBeforeUnmount, onMounted, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Back, Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import TruthTag from '@/components/TruthTag.vue'
import VerdictBadge from '@/components/calibration/VerdictBadge.vue'
import JobTimeline from '@/components/calibration/JobTimeline.vue'
import JobResultPanel from '@/components/calibration/JobResultPanel.vue'
import ApplyPanel, { type BeforePlan } from '@/components/calibration/ApplyPanel.vue'
import { useStatusPolling } from '@/composables/useStatusPolling'
import {
  isJobActive,
  isJobTerminal,
  jobDurationMs,
  resultTruth,
  type CalibrationJob
} from '@/api/calibration'
import {
  calibrationState,
  loadCalibrationJob,
  loadCalibrationWizardState,
  pauseCalibrationJobPolling,
  resumeCalibrationJobPolling,
  startCalibrationJobPolling,
  stopCalibrationJobPolling
} from '@/stores/calibration'
import { formatDecimal, formatTimestamp, shortenId } from '@/utils/format'

const { t, locale } = useI18n({ useScope: 'global' })
const route = useRoute()
const router = useRouter()
const { state: statusState, refresh: refreshStatus } = useStatusPolling()

const state = calibrationState
const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const jobId = computed(() => (typeof route.params.id === 'string' ? route.params.id : ''))
const job = computed<CalibrationJob | null>(() => state.job)
const phase = computed(() => state.jobPhase)
const error = computed(() => state.jobError)
const truth = computed(() => resultTruth(job.value?.result ?? null, job.value?.engine ?? null))
/** 503/501 类错误显示「未接入（NOT_AVAILABLE）+ 原因」，不弹假数据/假错误。 */
const unavailable = computed(() => error.value?.category === 'not_wired')
const unavailableReason = computed(() => error.value?.detail ?? t('calibrationEdit.list.notAvailableNoReason'))
const beforePlan = computed<BeforePlan | null>(() => {
  const plan = statusState.data?.plan ?? null
  if (plan === null) {
    return null
  }
  return { version: plan.version, graph_id: plan.graph_id, sha256: plan.sha256 }
})
const durationText = computed(() => {
  const ms = jobDurationMs(job.value)
  if (ms === null) {
    return t('common.dash')
  }
  return (formatDecimal(ms / 1000, 1, localeId.value) ?? t('common.dash')) + ' ' + t('unit.s')
})
const applyStateKey = computed(() =>
  job.value?.apply_state ? 'calibration.applyState.' + job.value.apply_state : null
)

function timeText(iso: string | null): string {
  return formatTimestamp(iso, localeId.value) ?? t('common.dash')
}

async function load(): Promise<void> {
  if (jobId.value.length === 0) {
    return
  }
  const result = await loadCalibrationJob(jobId.value)
  if (result.ok === true && isJobActive(result.value.status)) {
    startCalibrationJobPolling(jobId.value)
  }
  await refreshStatus()
}

function backToList(): void {
  void router.push({ name: 'calibration' })
}

async function onApplyChanged(): Promise<void> {
  await load()
  await loadCalibrationWizardState()
}

function onVisibilityChange(): void {
  if (document.visibilityState === 'hidden') {
    pauseCalibrationJobPolling()
  } else {
    resumeCalibrationJobPolling()
  }
}

watch(
  () => job.value?.status,
  (status) => {
    if (status !== undefined && isJobTerminal(status)) {
      void refreshStatus()
    }
  }
)

onMounted(() => {
  document.addEventListener('visibilitychange', onVisibilityChange)
  void load()
})

onBeforeUnmount(() => {
  document.removeEventListener('visibilitychange', onVisibilityChange)
  stopCalibrationJobPolling()
})
</script>

<template>
  <div class="job-page">
    <PageHeader title-key="calibration.job.pageTitle" :subtitle-key="undefined">
      <template #actions>
        <el-button plain @click="backToList">
          <el-icon><Back /></el-icon>
          <span>{{ t('calibration.job.back') }}</span>
        </el-button>
        <el-button plain :loading="phase === 'loading'" @click="load">
          <el-icon v-if="phase !== 'loading'"><Refresh /></el-icon>
          <span>{{ t('common.refresh') }}</span>
        </el-button>
      </template>
    </PageHeader>

    <div v-if="phase === 'error' && error" :class="unavailable ? 'banner banner--warn' : 'banner banner--danger'" :role="unavailable ? 'status' : 'alert'">
      <div class="job-page__notice">
        <strong v-if="unavailable">NOT_AVAILABLE · {{ t('calibrationEdit.list.notAvailableTitle') }}</strong>
        <strong v-else>{{ t('calibration.job.loadError') }}</strong>
        <span v-if="unavailable">{{ unavailableReason }}</span>
        <span v-else>{{ t(error.i18nKey) }}</span>
        <span v-if="error && error.detail && !unavailable" class="aux-text">{{ error.detail }}</span>
      </div>
      <el-button v-if="!unavailable" size="small" @click="load">{{ t('common.retry') }}</el-button>
    </div>

    <div v-else-if="phase === 'loading' && job === null" class="job-page__loading" aria-busy="true">
      <el-skeleton :rows="6" animated />
    </div>

    <template v-else-if="job">
      <section class="job-page__panel">
        <header class="job-page__head">
          <div class="job-page__title-wrap">
            <h2 class="job-page__id mono">{{ job.id }}</h2>
            <span class="calibration-status" :class="'calibration-status--' + (job.status === 'succeeded' ? 'ok' : job.status === 'failed' ? 'bad' : job.status === 'cancelled' ? 'warn' : 'accent')">
              <span class="calibration-status__dot" aria-hidden="true" />
              {{ t('calibration.jobStatus.' + job.status) }}
            </span>
            <VerdictBadge :verdict="job.verdict" :engine="job.engine" :truth="truth" />
            <TruthTag :kind="truth" :reason="job.engine ? t('calibration.engine.label', { engine: job.engine }) : null" />
          </div>
        </header>
        <dl class="job-page__facts">
          <div class="job-page__fact">
            <dt>{{ t('calibration.job.created') }}</dt>
            <dd class="mono">{{ timeText(job.created_at) }}</dd>
          </div>
          <div class="job-page__fact">
            <dt>{{ t('calibration.job.started') }}</dt>
            <dd class="mono">{{ timeText(job.started_at) }}</dd>
          </div>
          <div class="job-page__fact">
            <dt>{{ t('calibration.job.finished') }}</dt>
            <dd class="mono">{{ timeText(job.finished_at) }}</dd>
          </div>
          <div class="job-page__fact">
            <dt>{{ t('calibration.job.duration') }}</dt>
            <dd class="mono">{{ durationText }}</dd>
          </div>
          <div class="job-page__fact">
            <dt>{{ t('calibration.job.applyState') }}</dt>
            <dd>
              {{ applyStateKey ? t(applyStateKey) : t('common.dash') }}
              <span v-if="job.applied_version !== null" class="aux-text mono"> · v{{ job.applied_version }}</span>
            </dd>
          </div>
          <div class="job-page__fact">
            <dt>{{ t('calibration.job.engine') }}</dt>
            <dd class="mono">{{ job.engine ?? t('common.notWired') }}</dd>
          </div>
        </dl>
      </section>

      <div v-if="job.error" class="banner banner--danger" role="alert">
        <div class="job-page__notice">
          <strong>{{ t('calibration.job.error') }}</strong>
          <span class="mono">{{ job.error.code }}</span>
          <span class="aux-text">{{ job.error.message }}</span>
        </div>
      </div>

      <div class="job-page__grid">
        <section class="job-page__panel">
          <h3 class="job-page__section-title">{{ t('calibration.job.steps') }}</h3>
          <JobTimeline :steps="job.steps" :truth="truth" />
        </section>

        <section class="job-page__panel">
          <h3 class="job-page__section-title">{{ t('calibration.job.inputs') }}</h3>
          <dl v-if="job.inputs" class="job-page__facts job-page__facts--stack">
            <div class="job-page__fact">
              <dt>{{ t('calibration.job.inputsPlanSha') }}</dt>
              <dd class="mono" :title="job.inputs.plan_sha ?? undefined">{{ shortenId(job.inputs.plan_sha, 16, 12) ?? t('common.dash') }}</dd>
            </div>
            <div class="job-page__fact">
              <dt>{{ t('calibration.job.inputsBaseline') }}</dt>
              <dd class="mono">{{ shortenId(job.inputs.baseline_upload_id, 12, 8) ?? t('common.dash') }}</dd>
            </div>
            <div class="job-page__fact">
              <dt>{{ t('calibration.job.inputsApplied') }}</dt>
              <dd class="mono">{{ shortenId(job.inputs.applied_upload_id, 12, 8) ?? t('common.dash') }}</dd>
            </div>
            <div class="job-page__fact">
              <dt>{{ t('calibration.job.inputsCenters') }}</dt>
              <dd class="mono">{{ job.inputs.centers_hz.length > 0 ? job.inputs.centers_hz.join(', ') : t('common.dash') }}</dd>
            </div>
            <div class="job-page__fact">
              <dt>{{ t('calibration.job.inputsTolerance') }}</dt>
              <dd class="mono">{{ job.inputs.tolerance_db === null ? t('common.dash') : job.inputs.tolerance_db + ' dB' }}</dd>
            </div>
            <div class="job-page__fact">
              <dt>{{ t('calibration.job.inputsMinBands') }}</dt>
              <dd class="mono">{{ job.inputs.min_bands ?? t('common.dash') }}</dd>
            </div>
          </dl>
          <p v-else class="aux-text">{{ t('common.notWired') }}</p>
        </section>
      </div>

      <section class="job-page__panel">
        <h3 class="job-page__section-title">{{ t('calibration.job.resultTitle') }}</h3>
        <JobResultPanel :job="job" />
      </section>

      <section class="job-page__panel">
        <h3 class="job-page__section-title">{{ t('calibration.job.applyTitle') }}</h3>
        <ApplyPanel :job="job" :before-plan="beforePlan" @changed="onApplyChanged" />
      </section>
    </template>

    <div v-else class="banner banner--warn" role="status">
      {{ t('calibration.job.notFound') }}
    </div>
  </div>
</template>

<style scoped>
.job-page {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}

.job-page__notice {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
  overflow-wrap: anywhere;
}

.job-page__loading {
  padding: var(--khaen-space-4);
  border: 1px dashed var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
}

.job-page__panel {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-3);
  padding: var(--khaen-space-4);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
  box-shadow: var(--khaen-shadow-card);
  min-width: 0;
}

.job-page__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
}

.job-page__title-wrap {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  min-width: 0;
}

.job-page__id {
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
  overflow-wrap: anywhere;
}

.job-page__facts {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: var(--khaen-space-2);
  margin: 0;
}

.job-page__facts--stack {
  grid-template-columns: minmax(0, 1fr);
}

.job-page__fact {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.job-page__fact dt {
  color: var(--khaen-text-secondary);
  font-size: 11px;
}

.job-page__fact dd {
  margin: 0;
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.job-page__grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--khaen-space-4);
  align-items: start;
}

.job-page__section-title {
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
}

.calibration-status {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: var(--khaen-font-aux);
}

.calibration-status__dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--khaen-muted);
}

.calibration-status--ok .calibration-status__dot {
  background: var(--khaen-success);
}

.calibration-status--bad .calibration-status__dot {
  background: var(--khaen-danger);
}

.calibration-status--warn .calibration-status__dot {
  background: var(--khaen-warning);
}

.calibration-status--accent .calibration-status__dot {
  background: var(--khaen-accent);
}

@media (max-width: 1279px) {
  .job-page__facts {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .job-page__grid {
    grid-template-columns: minmax(0, 1fr);
  }
}

@media (max-width: 767px) {
  .job-page__facts {
    grid-template-columns: minmax(0, 1fr);
  }

  .job-page__panel {
    padding: var(--khaen-space-3);
  }
}
</style>
