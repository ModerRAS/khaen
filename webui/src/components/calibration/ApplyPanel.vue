<script lang="ts">
/** 对外类型（普通 script 块：<script setup> 不能有 ES 导出）。 */
export interface BeforePlan {
  version: number | null
  graph_id: string | null
  sha256: string | null
}
</script>

<script setup lang="ts">
/**
 * 第 8 步「回顾 / 应用 / 回滚」共用面板。
 *
 * 端点（docs/API.md §7）：apply = POST /jobs/{id}/apply，rollback = POST /jobs/{id}/rollback，
 * 无 job 时 apply = POST /wizard/apply {job_id?}。请求按**单发**语义发送（body `{}`）；
 * 若后端回执是 staged（stage=prepare + engine_token），则把草稿如实标成「未生效」，
 * 并提供 commit / abort 两步（token 只消费一次）；否则只展示真实单发回执。
 *
 * 纪律：回执里的 applied_version/graph_id 可能为 null -> 显示「—」+ reasons，不编造；
 * ack.synthetic=true 时标「模拟提交」，不当成 actual。
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import { Download, RefreshLeft, Upload } from '@element-plus/icons-vue'
import TruthTag from '@/components/TruthTag.vue'
import { jobResultTruth, type CalibrationJob } from '@/api/calibration'
import { formatTimestamp, shortenId } from '@/utils/format'
import {
  abortCalibrationApply,
  abortCalibrationRollback,
  applyCalibrationJob,
  applyCalibrationWizard,
  calibrationState,
  clearCalibrationActionError,
  commitCalibrationApply,
  commitCalibrationRollback,
  rollbackCalibrationJob
} from '@/stores/calibration'

const props = defineProps<{
  job: CalibrationJob | null
  beforePlan: BeforePlan | null
}>()

const emit = defineEmits<{ (event: 'changed'): void }>()

const { t, locale } = useI18n({ useScope: 'global' })
const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))

type DialogKind = 'apply' | 'rollback'
const dialogKind = ref<DialogKind | null>(null)
const dialogVisible = computed({
  get: () => dialogKind.value !== null,
  set: (value: boolean) => {
    if (!value) {
      dialogKind.value = null
    }
  }
})

const afterPlan = computed(() => props.job?.result?.plan ?? null)
const truth = computed(() => jobResultTruth(props.job?.engine ?? null))
const applyStateKey = computed(() =>
  props.job?.apply_state ? 'calibration.applyState.' + props.job.apply_state : null
)
const canApply = computed(
  () =>
    props.job !== null &&
    props.job.status === 'succeeded' &&
    afterPlan.value !== null &&
    props.job.apply_state !== 'applied'
)
const canRollback = computed(() => props.job !== null && props.job.apply_state === 'applied')
const busy = computed(() => calibrationState.busy.apply || calibrationState.busy.rollback)

const applyReceipt = computed(() => calibrationState.lastApply)
const rollbackReceipt = computed(() => calibrationState.lastRollback)
const actionError = computed(() => calibrationState.actionError)

/* ------------------------------------------------- 回执按 kind 收窄（不交叉取值） */

const applyApplied = computed(() =>
  applyReceipt.value !== null && applyReceipt.value.kind === 'applied' ? applyReceipt.value : null
)
const applyPrepare = computed(() =>
  applyReceipt.value !== null && applyReceipt.value.kind === 'prepare' ? applyReceipt.value : null
)
const applyAbort = computed(() =>
  applyReceipt.value !== null && applyReceipt.value.kind === 'abort' ? applyReceipt.value : null
)
const wizardApplyJob = computed(() =>
  applyReceipt.value !== null && applyReceipt.value.kind === 'job' ? applyReceipt.value : null
)
const rollbackDone = computed(() =>
  rollbackReceipt.value !== null && rollbackReceipt.value.kind === 'rolled_back' ? rollbackReceipt.value : null
)
const rollbackPrepare = computed(() =>
  rollbackReceipt.value !== null && rollbackReceipt.value.kind === 'prepare' ? rollbackReceipt.value : null
)
const rollbackAbort = computed(() =>
  rollbackReceipt.value !== null && rollbackReceipt.value.kind === 'abort' ? rollbackReceipt.value : null
)

/** 待决草稿：按当前 job 匹配（其它 job 的 token 不能混用）。 */
const pendingApply = computed(() => {
  const pending = calibrationState.pendingApply
  if (pending === null) {
    return null
  }
  return props.job === null || pending.jobId === props.job.id ? pending : null
})
const pendingRollback = computed(() => {
  const pending = calibrationState.pendingRollback
  if (pending === null) {
    return null
  }
  return props.job === null || pending.jobId === props.job.id ? pending : null
})
const hasPendingJob = computed(
  () => pendingApply.value !== null || pendingRollback.value !== null
)

function openDialog(kind: DialogKind): void {
  clearCalibrationActionError()
  dialogKind.value = kind
}

function versionText(value: number | null): string {
  return value === null ? t('common.dash') : 'v' + String(value)
}

function shaText(value: string | null): string {
  return shortenId(value, 16, 12) ?? t('common.dash')
}

function timeText(iso: string | null): string {
  return formatTimestamp(iso, localeId.value) ?? t('common.dash')
}

async function confirmDialog(): Promise<void> {
  const kind = dialogKind.value
  if (kind === null) {
    return
  }
  if (kind === 'apply') {
    const result =
      props.job !== null ? await applyCalibrationJob(props.job.id) : await applyCalibrationWizard()
    if (result.ok === true) {
      dialogKind.value = null
      ElMessage.success(t('calibration.review.appliedToast'))
      emit('changed')
    }
    return
  }
  if (props.job === null) {
    return
  }
  const result = await rollbackCalibrationJob(props.job.id)
  if (result.ok === true) {
    dialogKind.value = null
    ElMessage.success(t('calibration.review.rolledBackToast'))
    emit('changed')
  }
}

/** 明确发起 staged prepare（只生成草稿，不生效）：与单发按钮分开，避免把草稿当成功。 */
async function prepareStaged(kind: 'apply' | 'rollback'): Promise<void> {
  if (props.job === null) {
    return
  }
  const result =
    kind === 'apply'
      ? await applyCalibrationJob(props.job.id, { stage: 'prepare' })
      : await rollbackCalibrationJob(props.job.id, { stage: 'prepare' })
  if (result.ok === true) {
    ElMessage.info(t('calibrationEdit.staged.prepareHint'))
  }
}

async function commitStaged(kind: 'apply' | 'rollback'): Promise<void> {
  const pending = kind === 'apply' ? pendingApply.value : pendingRollback.value
  if (pending === null) {
    ElMessage.error(t('calibrationEdit.staged.noPending'))
    return
  }
  const result =
    kind === 'apply' ? await commitCalibrationApply(pending.jobId) : await commitCalibrationRollback(pending.jobId)
  if (result.ok === true) {
    ElMessage.success(t('calibrationEdit.staged.commitDone'))
    emit('changed')
  }
}

async function abortStaged(kind: 'apply' | 'rollback'): Promise<void> {
  const pending = kind === 'apply' ? pendingApply.value : pendingRollback.value
  if (pending === null) {
    ElMessage.error(t('calibrationEdit.staged.noPending'))
    return
  }
  const result =
    kind === 'apply' ? await abortCalibrationApply(pending.jobId) : await abortCalibrationRollback(pending.jobId)
  if (result.ok === true) {
    ElMessage.info(t('calibrationEdit.staged.abortDone'))
    emit('changed')
  }
}

function exportConfig(): void {
  if (props.job === null) {
    return
  }
  const job = props.job
  const payload = {
    schema: 'khaen-calibration-export/v1',
    exported_at: new Date().toISOString(),
    job_id: job.id,
    kind: job.kind,
    inputs: job.inputs,
    result: job.result,
    artifacts: job.artifacts
  }
  const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  const stamp = new Date().toISOString().replace(/[:.]/g, '-')
  anchor.href = url
  anchor.download = 'khaen-calibration-' + job.id + '-' + stamp + '.json'
  document.body.appendChild(anchor)
  anchor.click()
  document.body.removeChild(anchor)
  URL.revokeObjectURL(url)
}
</script>

<template>
  <div class="apply-panel">
    <div class="apply-panel__state">
      <span class="apply-panel__state-label">{{ t('calibration.review.applyState') }}</span>
      <span v-if="applyStateKey" class="apply-panel__state-value">{{ t(applyStateKey) }}</span>
      <span v-else class="aux-text">{{ t('common.dash') }}</span>
      <TruthTag :kind="truth" />
      <span v-if="props.job?.applied_version !== null && props.job?.applied_version !== undefined" class="aux-text mono">
        {{ t('calibration.review.appliedVersion') }}: {{ versionText(props.job.applied_version) }}
      </span>
    </div>

    <div class="apply-panel__compare">
      <section class="apply-panel__side">
        <h3 class="apply-panel__side-title">{{ t('calibration.review.before') }}</h3>
        <dl class="apply-panel__kv">
          <div class="apply-panel__kv-row">
            <dt>{{ t('calibration.review.version') }}</dt>
            <dd class="mono">{{ versionText(props.beforePlan?.version ?? null) }}</dd>
          </div>
          <div class="apply-panel__kv-row">
            <dt>{{ t('calibration.review.graphId') }}</dt>
            <dd class="mono">{{ props.beforePlan?.graph_id ?? t('common.dash') }}</dd>
          </div>
          <div class="apply-panel__kv-row">
            <dt>{{ t('calibration.review.sha256') }}</dt>
            <dd class="mono" :title="props.beforePlan?.sha256 ?? undefined">{{ shaText(props.beforePlan?.sha256 ?? null) }}</dd>
          </div>
        </dl>
        <p class="aux-text">{{ t('calibration.review.beforeHint') }}</p>
      </section>
      <section class="apply-panel__side">
        <h3 class="apply-panel__side-title">{{ t('calibration.review.after') }}</h3>
        <dl class="apply-panel__kv">
          <div class="apply-panel__kv-row">
            <dt>{{ t('calibration.review.graphId') }}</dt>
            <dd class="mono">{{ afterPlan?.graph_id ?? t('common.dash') }}</dd>
          </div>
          <div class="apply-panel__kv-row">
            <dt>{{ t('calibration.review.sha256') }}</dt>
            <dd class="mono" :title="afterPlan?.sha256 ?? undefined">{{ shaText(afterPlan?.sha256 ?? null) }}</dd>
          </div>
        </dl>
        <p v-if="props.job === null" class="aux-text">{{ t('calibration.review.noJob') }}</p>
      </section>
    </div>

    <div class="apply-panel__actions">
      <el-button type="primary" :disabled="!canApply || busy" :loading="calibrationState.busy.apply" @click="openDialog('apply')">
        <el-icon v-if="!calibrationState.busy.apply"><Upload /></el-icon>
        <span>{{ t('calibration.review.apply') }}</span>
      </el-button>
      <el-button :disabled="!canRollback || busy" :loading="calibrationState.busy.rollback" @click="openDialog('rollback')">
        <el-icon v-if="!calibrationState.busy.rollback"><RefreshLeft /></el-icon>
        <span>{{ t('calibration.review.rollback') }}</span>
      </el-button>
      <el-button v-if="canApply" size="small" plain :disabled="busy || hasPendingJob" @click="prepareStaged('apply')">
        <span>{{ t('calibrationEdit.staged.prepareAction') }}</span>
      </el-button>
      <el-button v-if="canRollback" size="small" plain :disabled="busy || hasPendingJob" @click="prepareStaged('rollback')">
        <span>{{ t('calibrationEdit.staged.prepareAction') }}</span>
      </el-button>
      <el-button plain :disabled="props.job === null" @click="exportConfig">
        <el-icon><Download /></el-icon>
        <span>{{ t('calibration.review.exportConfig') }}</span>
      </el-button>
    </div>

    <section v-if="pendingApply || pendingRollback" class="apply-panel__receipt apply-panel__receipt--draft" role="status">
      <h3 class="apply-panel__side-title">{{ t('calibrationEdit.staged.pendingTitle') }}</h3>
      <p class="aux-text">{{ t('calibrationEdit.staged.pendingDesc') }}</p>
      <dl v-if="pendingApply" class="apply-panel__kv">
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.staged.token') }}</dt>
          <dd class="mono" :title="pendingApply.engineToken">{{ shaText(pendingApply.engineToken) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.planSha') }}</dt>
          <dd class="mono" :title="pendingApply.planSha256 ?? undefined">{{ shaText(pendingApply.planSha256) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.previousVersion') }}</dt>
          <dd class="mono">{{ versionText(pendingApply.previousVersion) }}</dd>
        </div>
      </dl>
      <dl v-if="pendingRollback" class="apply-panel__kv">
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.staged.token') }}</dt>
          <dd class="mono" :title="pendingRollback.engineToken">{{ shaText(pendingRollback.engineToken) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.targetVersion') }}</dt>
          <dd class="mono">{{ versionText(pendingRollback.targetVersion) }}</dd>
        </div>
      </dl>
      <div class="apply-panel__actions">
        <el-button
          v-if="pendingApply"
          type="primary"
          size="small"
          :loading="calibrationState.busy.apply"
          :disabled="busy"
          @click="commitStaged('apply')"
        >
          {{ t('calibrationEdit.staged.commit') }}
        </el-button>
        <el-button
          v-if="pendingApply"
          size="small"
          plain
          :loading="calibrationState.busy.apply"
          :disabled="busy"
          @click="abortStaged('apply')"
        >
          {{ t('calibrationEdit.staged.abort') }}
        </el-button>
        <el-button
          v-if="pendingRollback"
          type="primary"
          size="small"
          :loading="calibrationState.busy.rollback"
          :disabled="busy"
          @click="commitStaged('rollback')"
        >
          {{ t('calibrationEdit.staged.commit') }}
        </el-button>
        <el-button
          v-if="pendingRollback"
          size="small"
          plain
          :loading="calibrationState.busy.rollback"
          :disabled="busy"
          @click="abortStaged('rollback')"
        >
          {{ t('calibrationEdit.staged.abort') }}
        </el-button>
      </div>
    </section>

    <p v-if="actionError" class="banner banner--danger" role="alert">
      {{ t(actionError.i18nKey) }}<span v-if="actionError.detail" class="aux-text"> — {{ actionError.detail }}</span>
    </p>

    <section v-if="applyApplied" class="apply-panel__receipt">
      <h3 class="apply-panel__side-title">{{ t('calibrationEdit.receipt.applied') }}</h3>
      <dl class="apply-panel__kv">
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.appliedVersion') }}</dt>
          <dd class="mono">{{ versionText(applyApplied.applied_version) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.previousVersion') }}</dt>
          <dd class="mono">{{ versionText(applyApplied.previous_version ?? applyApplied.snapshot?.version ?? null) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.graphId') }}</dt>
          <dd class="mono">{{ applyApplied.graph_id ?? t('common.dash') }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.sha256') }}</dt>
          <dd class="mono" :title="applyApplied.sha256 ?? undefined">{{ shaText(applyApplied.sha256) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.snapshot') }}</dt>
          <dd class="mono">
            {{ applyApplied.snapshot ? versionText(applyApplied.snapshot.version) + ' · ' + (applyApplied.snapshot.graph_id ?? t('common.dash')) : t('common.dash') }}
          </dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.ack') }}</dt>
          <dd>
            <template v-if="applyApplied.ack">
              <span :class="applyApplied.ack.actual ? 'tone-ok' : 'tone-warn'">
                {{ applyApplied.ack.actual ? t('calibrationEdit.receipt.ackActual') : t('calibrationEdit.receipt.ackSynthetic') }}
              </span>
            </template>
            <span v-else class="aux-text">{{ t('common.dash') }}</span>
          </dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.appliedAt') }}</dt>
          <dd class="mono">{{ timeText(applyApplied.applied_at) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.jobId') }}</dt>
          <dd class="mono">{{ applyApplied.job_id ?? t('common.dash') }}</dd>
        </div>
      </dl>
      <p v-if="applyApplied.readback" class="aux-text mono apply-panel__readback">{{ JSON.stringify(applyApplied.readback) }}</p>
    </section>

    <section v-if="applyPrepare" class="apply-panel__receipt apply-panel__receipt--draft" role="status">
      <h3 class="apply-panel__side-title">{{ t('calibrationEdit.receipt.prepared') }}</h3>
      <p class="aux-text">{{ t('calibrationEdit.staged.notAppliedHint') }}</p>
      <dl class="apply-panel__kv">
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.staged.token') }}</dt>
          <dd class="mono" :title="applyPrepare.engine_token">{{ shaText(applyPrepare.engine_token) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.planSha') }}</dt>
          <dd class="mono" :title="applyPrepare.plan_sha256 ?? undefined">{{ shaText(applyPrepare.plan_sha256) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.previousVersion') }}</dt>
          <dd class="mono">{{ versionText(applyPrepare.previous_version) }}</dd>
        </div>
      </dl>
    </section>

    <section v-if="applyAbort" class="apply-panel__receipt" role="status">
      <h3 class="apply-panel__side-title">{{ t('calibrationEdit.receipt.aborted') }}</h3>
      <p class="aux-text">{{ t('calibrationEdit.receipt.abortedDesc') }}</p>
    </section>

    <section v-if="wizardApplyJob" class="apply-panel__receipt" role="status">
      <h3 class="apply-panel__side-title">{{ t('calibrationEdit.wizardApply.jobTitle') }}</h3>
      <p class="aux-text">{{ t('calibrationEdit.wizardApply.jobDesc') }}</p>
      <dl class="apply-panel__kv">
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.wizardApply.jobId') }}</dt>
          <dd class="mono">{{ wizardApplyJob.job_id ?? t('common.dash') }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.wizardApply.location') }}</dt>
          <dd class="mono">{{ wizardApplyJob.location ?? t('common.dash') }}</dd>
        </div>
      </dl>
    </section>

    <section v-if="rollbackDone" class="apply-panel__receipt">
      <h3 class="apply-panel__side-title">{{ t('calibrationEdit.receipt.rolledBack') }}</h3>
      <dl class="apply-panel__kv">
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.restoredVersion') }}</dt>
          <dd class="mono">{{ versionText(rollbackDone.restored_version) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.graphId') }}</dt>
          <dd class="mono">{{ rollbackDone.graph_id ?? t('common.dash') }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.sha256') }}</dt>
          <dd class="mono" :title="rollbackDone.sha256 ?? undefined">{{ shaText(rollbackDone.sha256) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.rolledBackAt') }}</dt>
          <dd class="mono">{{ timeText(rollbackDone.rolled_back_at) }}</dd>
        </div>
      </dl>
    </section>

    <section v-if="rollbackPrepare" class="apply-panel__receipt apply-panel__receipt--draft" role="status">
      <h3 class="apply-panel__side-title">{{ t('calibrationEdit.receipt.prepared') }}</h3>
      <p class="aux-text">{{ t('calibrationEdit.staged.notAppliedHint') }}</p>
      <dl class="apply-panel__kv">
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.staged.token') }}</dt>
          <dd class="mono" :title="rollbackPrepare.engine_token">{{ shaText(rollbackPrepare.engine_token) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibrationEdit.receipt.targetVersion') }}</dt>
          <dd class="mono">{{ versionText(rollbackPrepare.target_version) }}</dd>
        </div>
      </dl>
    </section>

    <section v-if="rollbackAbort" class="apply-panel__receipt" role="status">
      <h3 class="apply-panel__side-title">{{ t('calibrationEdit.receipt.aborted') }}</h3>
      <p class="aux-text">{{ t('calibrationEdit.receipt.abortedDesc') }}</p>
    </section>

    <el-dialog
      v-model="dialogVisible"
      :title="dialogKind === 'rollback' ? t('calibration.review.confirmRollback.title') : t('calibration.review.confirmApply.title')"
      width="560px"
      align-center
      destroy-on-close
    >
      <p class="apply-panel__dialog-message">
        <template v-if="dialogKind === 'rollback'">
          {{
            t('calibration.review.confirmRollback.message', {
              version: versionText(props.beforePlan?.version ?? null),
              graphId: props.beforePlan?.graph_id ?? t('common.dash')
            })
          }}
        </template>
        <template v-else>
          {{ t('calibration.review.confirmApply.message', { version: versionText(props.beforePlan?.version ?? null) }) }}
        </template>
      </p>
      <div class="apply-panel__dialog-grid">
        <div>
          <p class="aux-text">{{ t('calibration.review.before') }}</p>
          <p class="mono">{{ versionText(props.beforePlan?.version ?? null) }} · {{ props.beforePlan?.graph_id ?? t('common.dash') }}</p>
        </div>
        <div>
          <p class="aux-text">{{ t('calibration.review.after') }}</p>
          <p class="mono">{{ afterPlan?.graph_id ?? t('common.dash') }}</p>
        </div>
      </div>
      <template #footer>
        <el-button @click="dialogVisible = false">{{ t('common.cancel') }}</el-button>
        <el-button
          type="primary"
          :loading="busy"
          @click="confirmDialog"
        >
          {{ dialogKind === 'rollback' ? t('calibration.review.confirmRollback.ok') : t('calibration.review.confirmApply.ok') }}
        </el-button>
      </template>
    </el-dialog>
  </div>
</template>

<style scoped>
.apply-panel {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-3);
  min-width: 0;
}

.apply-panel__state {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  font-size: var(--khaen-font-aux);
}

.apply-panel__state-label {
  color: var(--khaen-text-secondary);
}

.apply-panel__state-value {
  font-weight: 600;
}

.apply-panel__compare {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--khaen-space-3);
}

.apply-panel__side {
  padding: var(--khaen-space-3);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
  min-width: 0;
}

.apply-panel__side-title {
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
  margin-bottom: var(--khaen-space-2);
}

.apply-panel__kv {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  margin: 0 0 var(--khaen-space-2);
}

.apply-panel__kv-row {
  display: flex;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  font-size: var(--khaen-font-aux);
  border-bottom: 1px dashed var(--khaen-border);
  padding-bottom: 2px;
  flex-wrap: wrap;
}

.apply-panel__kv-row dt {
  color: var(--khaen-text-secondary);
}

.apply-panel__kv-row dd {
  margin: 0;
  text-align: right;
  overflow-wrap: anywhere;
}

.apply-panel__actions {
  display: flex;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.apply-panel__receipt {
  padding: var(--khaen-space-3);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
}

/* prepare 草稿：未生效，用警告色边框避免被当成已应用。 */
.apply-panel__receipt--draft {
  border-color: var(--khaen-warning);
}

.apply-panel__readback {
  overflow-wrap: anywhere;
  font-size: 11px;
  color: var(--khaen-text-muted);
}

.apply-panel__dialog-message {
  margin-bottom: var(--khaen-space-3);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.apply-panel__dialog-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--khaen-space-3);
}

.tone-ok {
  color: var(--khaen-success);
}

.tone-warn {
  color: var(--khaen-warning);
}

@media (max-width: 767px) {
  .apply-panel__compare,
  .apply-panel__dialog-grid {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
