<script setup lang="ts">
/**
 * 校准列表页（PLAN §6.7 / §11.4；任务 T37）。
 *
 * - 数据来自 daemon：GET /api/calibration/jobs（分页）+ GET /api/calibration/wizard/state（摘要）；
 * - 后端未接入（available=false / 503 not_wired）如实显示 NOT_AVAILABLE + 原因 + 任务号，不展示假数据；
 * - 空态区分「真的没有 job」与「后端未接入」；
 * - 列表行操作：详情 / 取消（active job，二次确认）；应用/回滚在 job 详情页（带 before/after 确认）；
 * - 有 active job 时 5s 刷新列表；页面隐藏暂停。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { ElMessage, ElMessageBox } from 'element-plus'
import { Plus, Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import VerdictBadge from '@/components/calibration/VerdictBadge.vue'
import TruthTag from '@/components/TruthTag.vue'
import {
  calibrationState,
  cancelCalibrationJob,
  loadCalibrationJobs,
  loadCalibrationWizardState
} from '@/stores/calibration'
import {
  JOBS_PAGE_SIZE,
  isJobActive,
  jobDurationMs,
  type CalibrationJobSummary
} from '@/api/calibration'
import { formatDecimal, formatTimestamp, shortenId } from '@/utils/format'
import type { TruthKind } from '@/types/ui'

const { t, locale } = useI18n({ useScope: 'global' })
const router = useRouter()

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const state = calibrationState
const jobs = computed(() => state.jobs)
const phase = computed(() => state.jobsPhase)
const error = computed(() => state.jobsError)
const list = computed<CalibrationJobSummary[]>(() => state.jobs?.jobs ?? [])
const total = computed(() => state.jobs?.total ?? 0)
const offset = ref(0)
const cancelling = ref<string | null>(null)
let refreshTimer: number | null = null

const activeJob = computed(() => list.value.find((job) => isJobActive(job.status)) ?? null)
const lastJob = computed(() => list.value.find((job) => !isJobActive(job.status)) ?? null)
const measurement = computed(() => state.wizardState?.measurement ?? null)
const currentWizardStep = computed(() => state.wizardState?.current_step ?? null)
const unavailable = computed(() => phase.value === 'unavailable')
const unavailableReason = computed(
  () => state.jobs?.reason ?? (error.value?.detail ?? error.value?.code ?? t('common.unknown'))
)
const engineText = computed(() => state.jobs?.engine ?? null)
const engineTruth = computed<TruthKind>(() => (engineText.value === null ? 'notWired' : 'actual'))

const page = computed(() => Math.floor(offset.value / JOBS_PAGE_SIZE) + 1)
const pageCount = computed(() => Math.max(1, Math.ceil(total.value / JOBS_PAGE_SIZE)))
const hasPrev = computed(() => offset.value > 0)
const hasNext = computed(() => offset.value + JOBS_PAGE_SIZE < total.value)

const STATUS_TONE: Record<string, string> = {
  queued: 'muted',
  running: 'accent',
  succeeded: 'ok',
  failed: 'bad',
  cancelled: 'warn'
}

function statusLabel(status: string): string {
  return t('calibration.jobStatus.' + status)
}

function statusTone(status: string): string {
  return STATUS_TONE[status] ?? 'muted'
}

function timeText(iso: string | null): string {
  return formatTimestamp(iso, localeId.value) ?? t('common.dash')
}

function durationText(job: CalibrationJobSummary): string {
  const ms = jobDurationMs(job)
  if (ms === null) {
    return t('common.dash')
  }
  const seconds = formatDecimal(ms / 1000, 1, localeId.value) ?? t('common.dash')
  return seconds + ' ' + t('unit.s')
}

function shortId(id: string | null): string {
  return shortenId(id, 12, 6) ?? t('common.dash')
}

function truthForJob(job: CalibrationJobSummary): TruthKind {
  return job.status === 'queued' || job.status === 'running' ? 'notWired' : 'actual'
}

async function refresh(): Promise<void> {
  await Promise.all([loadCalibrationJobs({ limit: JOBS_PAGE_SIZE, offset: offset.value }), loadCalibrationWizardState()])
}

function scheduleRefresh(): void {
  stopRefreshTimer()
  if (document.visibilityState === 'hidden') {
    return
  }
  if (activeJob.value === null) {
    return
  }
  refreshTimer = window.setInterval(() => {
    void loadCalibrationJobs({ limit: JOBS_PAGE_SIZE, offset: offset.value })
  }, 5000)
}

function stopRefreshTimer(): void {
  if (refreshTimer !== null) {
    window.clearInterval(refreshTimer)
    refreshTimer = null
  }
}

function onVisibilityChange(): void {
  if (document.visibilityState === 'hidden') {
    stopRefreshTimer()
    return
  }
  void refresh()
}

function goPage(nextOffset: number): void {
  offset.value = Math.max(0, nextOffset)
  void loadCalibrationJobs({ limit: JOBS_PAGE_SIZE, offset: offset.value })
}

function openJob(job: CalibrationJobSummary): void {
  void router.push({ name: 'calibration-job', params: { id: job.id } })
}

function openWizard(): void {
  void router.push({ name: 'calibration-wizard' })
}

async function onCancel(job: CalibrationJobSummary): Promise<void> {
  if (cancelling.value !== null) {
    return
  }
  try {
    await ElMessageBox.confirm(
      t('calibration.list.cancelConfirm.message', { id: job.id }),
      t('calibration.list.cancelConfirm.title'),
      {
        confirmButtonText: t('calibration.list.cancelConfirm.ok'),
        cancelButtonText: t('common.cancel'),
        type: 'warning'
      }
    )
  } catch {
    return
  }
  cancelling.value = job.id
  const result = await cancelCalibrationJob(job.id)
  cancelling.value = null
  if (result.ok === true) {
    ElMessage.success(t('calibration.list.cancelConfirm.done'))
    await refresh()
  }
}

watch(activeJob, () => scheduleRefresh())
watch(
  () => state.jobsPhase,
  () => scheduleRefresh()
)

onMounted(() => {
  document.addEventListener('visibilitychange', onVisibilityChange)
  void refresh()
})

onBeforeUnmount(() => {
  document.removeEventListener('visibilitychange', onVisibilityChange)
  stopRefreshTimer()
})
</script>

<template>
  <div class="calibration-page">
    <PageHeader title-key="calibration.pageTitle" subtitle-key="calibration.subtitle">
      <template #actions>
        <el-button plain :loading="state.busy.jobs" :disabled="state.busy.jobs" @click="refresh">
          <el-icon v-if="!state.busy.jobs"><Refresh /></el-icon>
          <span>{{ t('common.refresh') }}</span>
        </el-button>
        <el-button type="primary" :disabled="unavailable" @click="openWizard">
          <el-icon><Plus /></el-icon>
          <span>{{ t('calibration.list.newWizard') }}</span>
        </el-button>
      </template>
    </PageHeader>

    <div v-if="unavailable" class="banner banner--warn" role="status">
      <strong>{{ t('calibration.list.unavailableTitle') }}</strong>
      <span>{{ t('calibration.list.unavailableDesc', { reason: unavailableReason, task: 'T70h-b' }) }}</span>
      <TruthTag kind="notWired" />
    </div>

    <div v-if="phase === 'error' && error" class="banner banner--danger" role="alert">
      <span>{{ t(error.i18nKey) }}</span>
      <span v-if="error && error.detail" class="aux-text">— {{ error.detail }}</span>
      <el-button size="small" @click="refresh">{{ t('common.retry') }}</el-button>
    </div>

    <div class="calibration-summary">
      <PanelCard
        title-key="calibration.list.summary.lastJob"
        :truth="lastJob === null ? 'notWired' : 'actual'"
      >
        <template v-if="lastJob">
          <div class="calibration-summary__row">
            <button type="button" class="calibration-summary__id mono" @click="openJob(lastJob)">
              {{ shortId(lastJob.id) }}
            </button>
            <VerdictBadge :verdict="lastJob.verdict" :engine="state.jobs?.engine ?? null" />
          </div>
          <p class="aux-text">
            {{ statusLabel(lastJob.status) }} · {{ timeText(lastJob.created_at) }} · {{ durationText(lastJob) }}
          </p>
        </template>
        <p v-else class="aux-text">{{ unavailable ? t('calibration.list.summary.backendNotWired') : t('calibration.list.summary.none') }}</p>
      </PanelCard>

      <PanelCard
        title-key="calibration.list.summary.wizard"
        :truth="currentWizardStep === null ? 'notWired' : 'actual'"
      >
        <p v-if="currentWizardStep !== null" class="calibration-summary__metric mono">
          {{ t('calibration.list.summary.wizardStep', { step: currentWizardStep }) }}
        </p>
        <p v-else class="aux-text">{{ t('common.notWired') }}</p>
        <p v-if="state.wizardState" class="aux-text">
          {{ t('calibration.list.summary.gatesPassed', {
            passed: state.wizardState.steps.filter((gate) => gate.status === 'passed').length,
            total: state.wizardState.steps.length
          }) }}
        </p>
        <el-button size="small" plain @click="openWizard">{{ t('calibration.list.summary.openWizard') }}</el-button>
      </PanelCard>

      <PanelCard
        title-key="calibration.list.summary.uploads"
        :truth="measurement && (measurement.baseline_upload_id || measurement.applied_upload_id) ? 'actual' : 'notWired'"
      >
        <dl v-if="measurement" class="calibration-summary__uploads">
          <div class="calibration-summary__upload">
            <dt>{{ t('calibration.list.summary.baseline') }}</dt>
            <dd class="mono">{{ shortId(measurement.baseline_sha256) }}</dd>
          </div>
          <div class="calibration-summary__upload">
            <dt>{{ t('calibration.list.summary.applied') }}</dt>
            <dd class="mono">{{ shortId(measurement.applied_sha256) }}</dd>
          </div>
        </dl>
        <p v-else class="aux-text">{{ t('common.notWired') }}</p>
      </PanelCard>
    </div>

    <section class="calibration-table-card">
      <header class="calibration-table-card__head">
        <h2 class="calibration-table-card__title">{{ t('calibration.list.title') }}</h2>
        <span class="calibration-table-card__engine">
          <span>{{ t('calibration.list.engine') }}</span>
          <span class="mono">{{ engineText ?? t('calibration.list.engineUnknown') }}</span>
          <TruthTag :kind="engineTruth" />
        </span>
      </header>

      <div v-if="phase === 'loading' && jobs === null" class="calibration-table-card__loading" aria-busy="true">
        <el-skeleton :rows="4" animated />
      </div>

      <div v-else-if="list.length === 0" class="calibration-empty">
        <p class="calibration-empty__title">
          {{ unavailable ? t('calibration.list.emptyUnavailableTitle') : t('calibration.list.emptyTitle') }}
        </p>
        <p class="aux-text">
          {{ unavailable ? unavailableReason : t('calibration.list.emptyDesc') }}
        </p>
        <el-button type="primary" plain :disabled="unavailable" @click="openWizard">
          {{ t('calibration.list.newWizard') }}
        </el-button>
      </div>

      <template v-else>
        <div class="calibration-table-wrap">
          <table class="calibration-table">
            <caption class="sr-only">{{ t('calibration.list.title') }}</caption>
            <thead>
              <tr>
                <th scope="col">{{ t('calibration.list.table.id') }}</th>
                <th scope="col">{{ t('calibration.list.table.kind') }}</th>
                <th scope="col">{{ t('calibration.list.table.status') }}</th>
                <th scope="col">{{ t('calibration.list.table.verdict') }}</th>
                <th scope="col">{{ t('calibration.list.table.created') }}</th>
                <th scope="col">{{ t('calibration.list.table.duration') }}</th>
                <th scope="col">{{ t('calibration.list.table.actions') }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="job in list" :key="job.id">
                <td>
                  <button type="button" class="calibration-table__id mono" @click="openJob(job)">
                    {{ shortId(job.id) }}
                  </button>
                  <TruthTag :kind="truthForJob(job)" />
                </td>
                <td class="mono">{{ job.kind }}</td>
                <td>
                  <span class="calibration-status" :class="'calibration-status--' + statusTone(job.status)">
                    <span class="calibration-status__dot" aria-hidden="true" />
                    {{ statusLabel(job.status) }}
                  </span>
                </td>
                <td><VerdictBadge :verdict="job.verdict" :engine="state.jobs?.engine ?? null" /></td>
                <td class="mono">{{ timeText(job.created_at) }}</td>
                <td class="mono">{{ durationText(job) }}</td>
                <td class="calibration-table__actions">
                  <el-button size="small" plain @click="openJob(job)">{{ t('calibration.list.action.detail') }}</el-button>
                  <el-button
                    v-if="isJobActive(job.status)"
                    size="small"
                    plain
                    type="warning"
                    :loading="cancelling === job.id"
                    @click="onCancel(job)"
                  >
                    {{ t('calibration.list.action.cancel') }}
                  </el-button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <footer class="calibration-pager">
          <span class="aux-text">
            {{ t('calibration.list.pageInfo', { page, pages: pageCount, total }) }}
          </span>
          <div class="calibration-pager__actions">
            <el-button size="small" plain :disabled="!hasPrev" @click="goPage(offset - JOBS_PAGE_SIZE)">
              {{ t('calibration.list.pager.prev') }}
            </el-button>
            <el-button size="small" plain :disabled="!hasNext" @click="goPage(offset + JOBS_PAGE_SIZE)">
              {{ t('calibration.list.pager.next') }}
            </el-button>
          </div>
        </footer>
      </template>
    </section>
  </div>
</template>

<style scoped>
.calibration-page {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}

.calibration-summary {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: var(--khaen-space-4);
}

.calibration-summary__row {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.calibration-summary__id {
  padding: 0;
  border: 0;
  background: transparent;
  color: var(--khaen-accent);
  cursor: pointer;
  font-size: var(--khaen-font-body);
  min-height: 32px;
}

.calibration-summary__metric {
  font-size: 22px;
  font-weight: 600;
}

.calibration-summary__uploads {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  margin: 0;
}

.calibration-summary__upload {
  display: flex;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  font-size: var(--khaen-font-aux);
  border-bottom: 1px dashed var(--khaen-border);
  padding-bottom: 2px;
}

.calibration-summary__upload dt {
  color: var(--khaen-text-secondary);
}

.calibration-summary__upload dd {
  margin: 0;
}

.calibration-table-card {
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

.calibration-table-card__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
}

.calibration-table-card__title {
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
}

.calibration-table-card__engine {
  display: inline-flex;
  align-items: center;
  gap: var(--khaen-space-2);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  flex-wrap: wrap;
}

.calibration-table-card__loading {
  padding: var(--khaen-space-3);
}

.calibration-table-wrap {
  overflow-x: auto;
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-button);
}

.calibration-table {
  width: 100%;
  min-width: 720px;
  border-collapse: collapse;
  font-size: var(--khaen-font-aux);
}

.calibration-table th,
.calibration-table td {
  padding: 8px 10px;
  text-align: left;
  border-bottom: 1px solid var(--khaen-border);
  white-space: nowrap;
}

.calibration-table th {
  position: sticky;
  top: 0;
  background: var(--khaen-panel-hover);
  color: var(--khaen-text-secondary);
  font-weight: 500;
}

.calibration-table tbody tr:last-child td {
  border-bottom: 0;
}

.calibration-table__id {
  padding: 0;
  border: 0;
  background: transparent;
  color: var(--khaen-accent);
  cursor: pointer;
  font-size: var(--khaen-font-aux);
  min-height: 32px;
}

.calibration-table__actions {
  display: flex;
  gap: var(--khaen-space-1);
}

.calibration-status {
  display: inline-flex;
  align-items: center;
  gap: 6px;
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

.calibration-empty {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-6) var(--khaen-space-3);
}

.calibration-empty__title {
  font-weight: 600;
}

.calibration-pager {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
}

.calibration-pager__actions {
  display: flex;
  gap: var(--khaen-space-1);
}

.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}

@media (max-width: 1279px) {
  .calibration-summary {
    grid-template-columns: minmax(0, 1fr);
  }
}

@media (max-width: 767px) {
  .calibration-table-card {
    padding: var(--khaen-space-3);
  }
}
</style>
