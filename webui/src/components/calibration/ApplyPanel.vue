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
 * 第 8 步「回顾 / 应用 / 回滚」共用面板（PLAN §10.2 / §10.3）：
 * - 展示 before（应用前运行方案 / 快照）与 after（job 产出的 plan）；
 * - 应用 = POST /jobs/{id}/apply（无 job 时 POST /wizard/apply），回滚 = POST /jobs/{id}/rollback；
 * - 两者都必须二次确认（居中弹窗，展示 before/after 与快照版本）；
 * - 结果回执如实展示 applied_version / graph_id / sha256 / ack(synthetic|actual) / readback；
 * - 导出配置 JSON（PLAN §10.3：回顾页导出配置 + 报告/manifest 由产物列表提供）。
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import { Download, RefreshLeft, Upload } from '@element-plus/icons-vue'
import TruthTag from '@/components/TruthTag.vue'
import { jobResultTruth, type CalibrationJob } from '@/api/calibration'
import { formatTimestamp, shortenId } from '@/utils/format'
import {
  applyCalibrationJob,
  applyCalibrationWizard,
  calibrationState,
  clearCalibrationActionError,
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
      <el-button plain :disabled="props.job === null" @click="exportConfig">
        <el-icon><Download /></el-icon>
        <span>{{ t('calibration.review.exportConfig') }}</span>
      </el-button>
    </div>

    <p v-if="actionError" class="banner banner--danger" role="alert">
      {{ t(actionError.i18nKey) }}<span v-if="actionError.detail" class="aux-text"> — {{ actionError.detail }}</span>
    </p>

    <section v-if="applyReceipt" class="apply-panel__receipt">
      <h3 class="apply-panel__side-title">{{ t('calibration.review.applyReceipt') }}</h3>
      <dl class="apply-panel__kv">
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibration.review.appliedVersion') }}</dt>
          <dd class="mono">{{ versionText(applyReceipt.applied_version) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibration.review.graphId') }}</dt>
          <dd class="mono">{{ applyReceipt.graph_id }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibration.review.sha256') }}</dt>
          <dd class="mono" :title="applyReceipt.sha256 ?? undefined">{{ shaText(applyReceipt.sha256) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibration.review.snapshot') }}</dt>
          <dd class="mono">
            {{ applyReceipt.snapshot ? versionText(applyReceipt.snapshot.version) + ' · ' + applyReceipt.snapshot.graph_id : t('common.dash') }}
          </dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibration.review.ack') }}</dt>
          <dd>
            <template v-if="applyReceipt.ack">
              <span :class="applyReceipt.ack.actual ? 'tone-ok' : 'tone-warn'">
                {{ applyReceipt.ack.actual ? t('calibration.review.ackActual') : t('calibration.review.ackSynthetic') }}
              </span>
            </template>
            <span v-else class="aux-text">{{ t('common.dash') }}</span>
          </dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibration.review.appliedAt') }}</dt>
          <dd class="mono">{{ formatTimestamp(applyReceipt.applied_at, localeId) ?? t('common.dash') }}</dd>
        </div>
      </dl>
      <p v-if="applyReceipt.readback" class="aux-text mono apply-panel__readback">{{ JSON.stringify(applyReceipt.readback) }}</p>
    </section>

    <section v-if="rollbackReceipt" class="apply-panel__receipt">
      <h3 class="apply-panel__side-title">{{ t('calibration.review.rollbackReceipt') }}</h3>
      <dl class="apply-panel__kv">
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibration.review.restoredVersion') }}</dt>
          <dd class="mono">{{ versionText(rollbackReceipt.restored_version) }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibration.review.graphId') }}</dt>
          <dd class="mono">{{ rollbackReceipt.graph_id }}</dd>
        </div>
        <div class="apply-panel__kv-row">
          <dt>{{ t('calibration.review.sha256') }}</dt>
          <dd class="mono" :title="rollbackReceipt.sha256 ?? undefined">{{ shaText(rollbackReceipt.sha256) }}</dd>
        </div>
      </dl>
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
