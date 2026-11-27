<script setup lang="ts">
/**
 * 内容布局页（task-62 前端地区）—— content folds 完整替换表两阶段编辑
 * （docs/API.md v1.1 §4 surface=layout / §9；CONTRACTS B.1.2）。
 *
 * 数据来源：
 * - GET /api/meta：surface=layout 的能力三态 + not_wired 原因（§9.1，未接通不渲染假操作）；
 * - GET /api/status（5s 轮询）：run state / safe / last_error（写锁）+ plan.version（no_base_plan 提前引导）；
 * - POST /api/surface {surface:"layout", data:{in_channels, out_lanes, folds}} → 草稿 →
 *   POST /api/commit {engine_token} / POST /api/abort {engine_token}：统一由 control store 管理
 *   （busy 互斥、prepare 失败清草稿、token 只能消费一次、401 交给会话 store）。
 *
 * 诚实边界（不编数据、不假成功）：
 * - 当前内容布局没有数据源（§9.7 / 裁决 G3）：控制协议没有读取 content.folds 的 op → 显示
 *   「不可读（无数据源）」；表单只是本地草稿，默认值不是当前值；
 * - changed/diff/current/target 是可选回执字段：后端未上报时如实显示「服务端未上报」（G2），不臆造；
 * - 409 no_base_plan 给出明确引导（先 prepare + commit 方案），不把失败包装成成功；
 * - prepare/commit/abort 失败只显示稳定错误码 + 本地化原因（不弹窗轰炸）。
 */
import { computed, onMounted, reactive, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import StatRow from '@/components/StatRow.vue'
import TruthTag from '@/components/TruthTag.vue'
import DraftReview from '@/components/control/DraftReview.vue'
import ControlErrorNotice from '@/components/control/ControlErrorNotice.vue'
import { annotateGapById, gapLocaleOf } from '@/api/capabilityGaps'
import { useStatusPolling } from '@/composables/useStatusPolling'
import { runStateKind } from '@/api/status'
import type { ControlCommitAck, ControlErrorInfo } from '@/api/control'
import { abortControlDraft, commitControlDraft, controlState, prepareLayout } from '@/stores/control'
import {
  ensureMeta,
  metaCapabilityReason,
  metaState,
  metaSurfaceState,
  refreshMeta
} from '@/stores/meta'
import {
  LAYOUT_U16_MAX,
  LAYOUT_WRITE_BLOCK_KEYS,
  addFoldRow,
  buildLayoutPatchFromDraft,
  capabilityStateKey,
  changedStateKey,
  commitChangedNoop,
  createLayoutDraft,
  foreignDraftSurface,
  layoutCommitBlocked,
  layoutDraftOf,
  layoutValidationKey,
  layoutWriteBlock,
  optionalReceiptText,
  removeFoldRow,
  statusShowsNoBasePlan,
  validateLayoutDraft
} from '@/components/layout/layoutDraft'
import { formatClock, formatInteger, shortenId } from '@/utils/format'
import type { TruthKind } from '@/types/ui'

interface KvRow {
  label: string
  value: string | null
  copy?: string | null
  reason?: string | null
  truth?: TruthKind | null
}

interface DraftFieldRow {
  label: string
  text: string
  json: boolean
}

const { t, locale } = useI18n({ useScope: 'global' })
const router = useRouter()
const { state: statusState } = useStatusPolling()

/** 表单是本地草稿：默认值只是编辑起点（当前布局无数据源）。 */
const form = reactive(createLayoutDraft())
/** 本页最近一次 prepare/commit/abort 的错误（不显示其它页面残留的错误）。 */
const actionError = ref<ControlErrorInfo | null>(null)
/** 本页最近一次 commit 回执。 */
const lastCommit = ref<ControlCommitAck | null>(null)
const lastCommitAt = ref<number | null>(null)
/** 重试按钮重跑的本页动作。 */
const lastAction = ref<(() => Promise<void>) | null>(null)

onMounted(() => {
  void ensureMeta()
})

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const statusData = computed(() => statusState.data)
const surface = computed(() => metaSurfaceState('layout'))
const capabilityReason = computed(() => metaCapabilityReason({ surface: 'layout' }))
const metaErrorCode = computed(() => (metaState.error === null ? null : metaState.error.code))
const metaLoading = computed(() => metaState.phase === 'loading')
const safe = computed(() => statusData.value?.state.safe === true)
const faulted = computed(() => runStateKind(statusData.value?.state ?? null) === 'faulted')
const writeLocked = computed(() => safe.value || faulted.value)
const lockReason = computed<string | null>(() => statusData.value?.state.last_error ?? null)
const busy = computed(() => controlState.busy !== null)
const formDisabled = computed(
  () => writeLocked.value || surface.value === 'not_wired' || busy.value
)

const validation = computed(() => validateLayoutDraft(form))
const validationKey = computed(() => layoutValidationKey(validation.value))
const block = computed(() =>
  layoutWriteBlock({
    surface: surface.value,
    safe: safe.value,
    faulted: faulted.value,
    busy: busy.value,
    validation: validation.value
  })
)
const blockHint = computed(() => {
  const reason = block.value
  return reason === null ? null : t(LAYOUT_WRITE_BLOCK_KEYS[reason])
})

const draft = computed(() => layoutDraftOf(controlState.draft))
const foreignDraft = computed(() => foreignDraftSurface(controlState.draft, 'layout'))
const prepareBusy = computed(() => controlState.busy === 'prepare')
const abortBusy = computed(() => controlState.busy === 'abort')
/** safe/faulted（或界面未接通）时 commit 也禁止：只读态不允许改变运行态；abort 仍可达。 */
const commitBlocked = computed(() =>
  layoutCommitBlocked({ surface: surface.value, safe: safe.value, faulted: faulted.value })
)
const commitBlockText = computed(() =>
  writeLocked.value ? t('layout.notice.lockedDraft') : t('layout.block.capability')
)
const lockedDraftVersion = computed(() => {
  const current = draft.value
  return current === null ? null : formatInteger(current.planned_version, localeId.value)
})
const lockedDraftToken = computed(() => {
  const current = draft.value
  return current === null ? null : current.engine_token
})
const noopCommit = computed(() => commitChangedNoop(lastCommit.value))
const noBasePlanError = computed(
  () => actionError.value !== null && actionError.value.category === 'no_base_plan'
)
const noBasePlanDetected = computed(
  () => statusData.value !== null && statusShowsNoBasePlan(statusData.value.plan.version)
)

/* ------------------------------------------------------------- 能力 / 当前值 */

const capabilityTruth = computed<TruthKind>(() => (surface.value === 'wired' ? 'actual' : 'notWired'))
const capabilityText = computed(() => t(capabilityStateKey(surface.value)))
const capabilityHint = computed(() => {
  if (surface.value === 'wired') {
    return t('layout.capability.wiredHint')
  }
  if (surface.value === 'not_wired') {
    return t('layout.capability.notWiredHint')
  }
  return t('layout.capability.unknownHint')
})

const capabilityRows = computed<KvRow[]>(() => {
  const rows: KvRow[] = [
    { label: t('layout.capability.field.surface'), value: 'layout' },
    {
      label: t('layout.capability.field.state'),
      value: capabilityText.value,
      truth: capabilityTruth.value
    }
  ]
  if (surface.value !== 'wired') {
    rows.push({
      label: t('layout.capability.field.reason'),
      value: capabilityReason.value ?? t('layout.capability.reasonMissing'),
      truth: 'notWired'
    })
  }
  return rows
})

const notices = computed<Array<{ tone: 'warn' | 'danger' | 'info'; text: string }>>(() => {
  const banners: Array<{ tone: 'warn' | 'danger' | 'info'; text: string }> = []
  if (statusData.value === null) {
    banners.push({ tone: 'info', text: t('layout.notice.statusUnknown') })
  }
  if (safe.value) {
    banners.push({
      tone: 'warn',
      text:
        lockReason.value === null
          ? t('layout.notice.safeReadOnly')
          : t('layout.notice.safeReadOnlyReason', { reason: lockReason.value })
    })
  } else if (faulted.value) {
    banners.push({
      tone: 'danger',
      text:
        lockReason.value === null
          ? t('layout.notice.faultedReadOnly')
          : t('layout.notice.faultedReadOnlyReason', { reason: lockReason.value })
    })
  }
  if (writeLocked.value && draft.value !== null) {
    banners.push({ tone: 'info', text: t('layout.notice.abortStillAvailable') })
  }
  if (foreignDraft.value !== null) {
    banners.push({
      tone: 'info',
      text: t('layout.notice.foreignDraft', { surface: foreignDraft.value })
    })
  }
  return banners
})

/* ---------------------------------------------------- prepare 回执可选字段 */

const draftFieldRows = computed<DraftFieldRow[]>(() => {
  const current = draft.value
  if (current === null) {
    return []
  }
  const diff = optionalReceiptText(current.diff)
  const currentValue = optionalReceiptText(current.current)
  const targetValue = optionalReceiptText(current.target)
  return [
    {
      label: t('layout.draftFields.changed'),
      text: t(changedStateKey(current.changed)),
      json: false
    },
    {
      label: t('layout.draftFields.diff'),
      text: diff ?? t('draftReview.notReported'),
      json: diff !== null
    },
    {
      label: t('layout.draftFields.current'),
      text: currentValue ?? t('draftReview.notReported'),
      json: currentValue !== null
    },
    {
      label: t('layout.draftFields.target'),
      text: targetValue ?? t('draftReview.notReported'),
      json: targetValue !== null
    }
  ]
})

/* ------------------------------------------------------------------- 回执 */

function receiptText(value: string | null): string {
  return value ?? t('draftReview.notReported')
}

const receiptRows = computed<KvRow[]>(() => {
  const ack = lastCommit.value
  if (ack === null) {
    return []
  }
  return [
    {
      label: t('control.receipt.version'),
      value: receiptText(ack.version === null ? null : formatInteger(ack.version, localeId.value)),
      truth: ack.version === null ? 'notWired' : null
    },
    {
      label: t('control.receipt.appliedBlock'),
      value: receiptText(
        ack.applied_block === null ? null : formatInteger(ack.applied_block, localeId.value)
      ),
      truth: ack.applied_block === null ? 'notWired' : null
    },
    {
      label: t('control.receipt.graphId'),
      value: receiptText(ack.graph_id),
      copy: ack.graph_id,
      truth: ack.graph_id === null ? 'notWired' : null
    },
    {
      label: t('control.receipt.changed'),
      value: receiptText(ack.changed === null ? null : t(changedStateKey(ack.changed))),
      truth: ack.changed === null ? 'notWired' : null
    }
  ]
})

/* ------------------------------------------------------------------- 动作 */

function normalizeNumberInput(value: number | undefined): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null
}

function onInChannels(value: number | undefined): void {
  form.inChannels = normalizeNumberInput(value)
}

function onOutLanes(value: number | undefined): void {
  form.outLanes = normalizeNumberInput(value)
}

function onFoldChange(
  index: number,
  field: 'src' | 'dst' | 'gain',
  value: number | undefined
): void {
  const fold = form.folds[index]
  if (fold === undefined) {
    return
  }
  fold[field] = normalizeNumberInput(value)
}

function onAddFold(): void {
  addFoldRow(form)
}

function onRemoveFold(index: number): void {
  removeFoldRow(form, index)
}

function goToChannels(): void {
  void router.push({ name: 'channels' })
}

async function onPrepare(): Promise<void> {
  if (block.value !== null) {
    return
  }
  lastAction.value = onPrepare
  const prepared = await prepareLayout(buildLayoutPatchFromDraft(form))
  actionError.value = controlState.error
  if (prepared !== null) {
    ElMessage.success(t('layout.form.prepared'))
  }
}

async function onCommit(): Promise<void> {
  if (draft.value === null) {
    return
  }
  lastAction.value = onCommit
  const ack = await commitControlDraft()
  actionError.value = controlState.error
  if (ack === null) {
    // token 失效时后端草稿已不存在：重试需要重新 prepare，而不是再 commit 旧 token。
    if (controlState.draft === null) {
      lastAction.value = onPrepare
    }
    return
  }
  lastCommit.value = ack
  lastCommitAt.value = controlState.lastCommitAt
  ElMessage.success(t('control.toast.committed'))
}

async function onAbort(): Promise<void> {
  if (draft.value === null) {
    return
  }
  lastAction.value = onAbort
  const ack = await abortControlDraft()
  actionError.value = controlState.error
  if (ack !== null) {
    ElMessage.success(t('control.toast.aborted'))
  }
}

function onRetry(): void {
  const action = lastAction.value
  if (action !== null) {
    void action()
  }
}

async function onRefreshMeta(): Promise<void> {
  await refreshMeta()
}
</script>

<template>
  <div class="layout-page">
    <PageHeader title-key="layout.pageTitle" subtitle-key="layout.subtitle">
      <template #actions>
        <el-button size="small" plain :loading="metaLoading" @click="onRefreshMeta">
          {{ t('layout.capability.refresh') }}
        </el-button>
      </template>
    </PageHeader>

    <ControlErrorNotice :error="actionError" @retry="onRetry" />

    <div v-if="noBasePlanError" class="banner banner--warn layout-nobase" role="alert">
      <div class="layout-nobase__text">
        <p class="layout-nobase__title">{{ t('layout.noBasePlan.title') }}</p>
        <p class="aux-text">{{ t('layout.noBasePlan.guide') }}</p>
      </div>
      <el-button size="small" @click="goToChannels">{{ t('layout.noBasePlan.goto') }}</el-button>
    </div>
    <div v-else-if="noBasePlanDetected" class="banner banner--info layout-nobase" role="status">
      <div class="layout-nobase__text">
        <p class="aux-text">{{ t('layout.noBasePlan.detected') }}</p>
      </div>
      <el-button size="small" @click="goToChannels">{{ t('layout.noBasePlan.goto') }}</el-button>
    </div>

    <p
      v-for="notice in notices"
      :key="notice.text"
      class="banner"
      :class="'banner--' + notice.tone"
      role="status"
    >
      {{ notice.text }}
    </p>

    <div class="grid-12 layout-grid">
      <PanelCard
        class="layout-grid__span-5"
        title-key="layout.capability.title"
        desc-key="layout.capability.desc"
        :truth="capabilityTruth"
      >
        <p v-if="metaLoading" class="aux-text">{{ t('layout.capability.loading') }}</p>
        <p v-if="metaErrorCode !== null" class="layout-inline-warn" role="status">
          {{ t('layout.capability.metaError', { code: metaErrorCode }) }}
        </p>
        <ul class="kv-list">
          <StatRow
            v-for="row in capabilityRows"
            :key="row.label"
            :label="row.label"
            :value="row.value"
            :truth="row.truth ?? null"
            :reason="row.reason ?? null"
          />
        </ul>
        <p class="aux-text">{{ capabilityHint }}</p>
      </PanelCard>

      <PanelCard
        class="layout-grid__span-7"
        title-key="layout.current.title"
        desc-key="layout.current.desc"
        truth="notWired"
      >
        <div class="metric">
          <span class="metric__value metric__value--muted">{{ t('layout.current.unavailable') }}</span>
          <TruthTag kind="notWired" :reason="annotateGapById(t('layout.current.reason'), 'layoutFolds', gapLocaleOf(locale))" />
        </div>
        <p class="aux-text">{{ t('layout.current.note') }}</p>
      </PanelCard>

      <PanelCard
        class="layout-grid__span-12"
        title-key="layout.form.title"
        desc-key="layout.form.desc"
        truth="demo"
      >
        <div class="layout-form__numbers">
          <label class="layout-field">
            <span class="layout-field__label">{{ t('layout.form.inChannels') }}</span>
            <el-input-number
              :model-value="form.inChannels ?? undefined"
              :min="1"
              :max="LAYOUT_U16_MAX"
              :step="1"
              :precision="0"
              :disabled="formDisabled"
              @change="onInChannels"
            />
          </label>
          <label class="layout-field">
            <span class="layout-field__label">{{ t('layout.form.outLanes') }}</span>
            <el-input-number
              :model-value="form.outLanes ?? undefined"
              :min="1"
              :max="LAYOUT_U16_MAX"
              :step="1"
              :precision="0"
              :disabled="formDisabled"
              @change="onOutLanes"
            />
          </label>
          <span class="aux-text layout-field__count">
            {{ t('layout.form.count', { count: form.folds.length }) }}
          </span>
        </div>

        <p class="layout-folds__title">{{ t('layout.form.foldsTitle') }}</p>
        <div v-if="form.folds.length > 0" class="layout-folds__head aux-text" aria-hidden="true">
          <span />
          <span>{{ t('layout.form.src') }}</span>
          <span>{{ t('layout.form.dst') }}</span>
          <span>{{ t('layout.form.gain') }}</span>
          <span />
        </div>
        <p v-if="form.folds.length === 0" class="layout-inline-error" role="alert">
          {{ t('layout.validation.empty') }}
        </p>
        <div
          v-for="(fold, index) in form.folds"
          :key="index"
          class="layout-folds__row"
          :aria-label="t('layout.form.foldRow', { index: index + 1 })"
        >
          <span class="layout-folds__index mono">{{ index + 1 }}</span>
          <el-input-number
            :model-value="fold.src ?? undefined"
            :min="0"
            :max="LAYOUT_U16_MAX"
            :step="1"
            :precision="0"
            :disabled="formDisabled"
            :aria-label="t('layout.form.src')"
            @change="onFoldChange(index, 'src', $event)"
          />
          <el-input-number
            :model-value="fold.dst ?? undefined"
            :min="0"
            :max="LAYOUT_U16_MAX"
            :step="1"
            :precision="0"
            :disabled="formDisabled"
            :aria-label="t('layout.form.dst')"
            @change="onFoldChange(index, 'dst', $event)"
          />
          <el-input-number
            :model-value="fold.gain ?? undefined"
            :step="0.5"
            :disabled="formDisabled"
            :aria-label="t('layout.form.gain')"
            @change="onFoldChange(index, 'gain', $event)"
          />
          <el-button
            size="small"
            :disabled="formDisabled"
            :aria-label="t('layout.form.removeFold')"
            @click="onRemoveFold(index)"
          >
            {{ t('layout.form.removeFold') }}
          </el-button>
        </div>
        <div class="layout-folds__actions">
          <el-button size="small" :disabled="formDisabled" @click="onAddFold">
            {{ t('layout.form.addFold') }}
          </el-button>
        </div>

        <p v-if="validationKey !== null" class="layout-inline-error" role="alert">
          {{ t(validationKey) }}
        </p>
        <div class="layout-actions">
          <el-button
            type="primary"
            :loading="prepareBusy"
            :disabled="block !== null"
            @click="onPrepare"
          >
            {{ t('control.actions.prepare') }}
          </el-button>
          <span v-if="blockHint !== null" class="aux-text">{{ blockHint }}</span>
          <span v-else-if="draft !== null" class="aux-text">{{ t('layout.form.prepared') }}</span>
        </div>
        <p class="aux-text">{{ t('layout.form.hint') }}</p>
        <p class="aux-text">{{ t('layout.form.localDraft') }}</p>
      </PanelCard>

      <DraftReview
        v-if="draft !== null && !commitBlocked"
        class="layout-grid__span-12"
        :draft="draft"
        :busy="busy"
        @commit="onCommit"
        @abort="onAbort"
      />
      <div v-else-if="draft !== null" class="layout-grid__span-12 layout-locked-draft">
        <p class="banner banner--warn" role="status">{{ commitBlockText }}</p>
        <div class="layout-locked-draft__actions">
          <span class="aux-text">{{ t('layout.notice.abortStillAvailable') }}</span>
          <el-button :loading="abortBusy" :disabled="busy && !abortBusy" @click="onAbort">
            {{ t('control.actions.abort') }}
          </el-button>
        </div>
        <ul class="kv-list">
          <StatRow
            :label="t('control.receipt.plannedVersion')"
            :value="lockedDraftVersion ?? t('draftReview.notReported')"
            :truth="lockedDraftVersion === null ? 'notWired' : null"
          />
          <StatRow
            :label="t('control.receipt.engineToken')"
            :value="shortenId(lockedDraftToken, 8) ?? t('draftReview.notReported')"
            :copy="lockedDraftToken"
          />
        </ul>
      </div>

      <PanelCard
        v-if="draft !== null"
        class="layout-grid__span-12"
        title-key="layout.draftFields.title"
        desc-key="layout.draftFields.desc"
        truth="actual"
      >
        <dl class="layout-fields">
          <div v-for="row in draftFieldRows" :key="row.label" class="layout-fields__row">
            <dt>{{ row.label }}</dt>
            <dd>
              <pre v-if="row.json" class="layout-fields__json mono">{{ row.text }}</pre>
              <span v-else>{{ row.text }}</span>
            </dd>
          </div>
        </dl>
      </PanelCard>

      <PanelCard
        class="layout-grid__span-12"
        title-key="layout.receipt.title"
        desc-key="layout.receipt.desc"
        :truth="lastCommit === null ? 'notWired' : 'actual'"
      >
        <p v-if="lastCommit === null" class="aux-text">{{ t('layout.receipt.empty') }}</p>
        <template v-else>
          <ul class="kv-list">
            <StatRow
              v-for="row in receiptRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :copy="row.copy ?? null"
              :truth="row.truth ?? null"
            />
            <StatRow
              :label="t('control.receipt.at')"
              :value="lastCommitAt === null ? t('draftReview.notReported') : formatClock(lastCommitAt, localeId)"
            />
          </ul>
          <p v-if="noopCommit" class="banner banner--info" role="status">
            {{ t('layout.notice.noopCommit') }}
          </p>
        </template>
      </PanelCard>
    </div>
  </div>
</template>

<style scoped>
.layout-page {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}

.layout-grid__span-5 {
  grid-column: span 5;
}

.layout-grid__span-7 {
  grid-column: span 7;
}

.layout-grid__span-12 {
  grid-column: span 12;
}

.layout-locked-draft {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-3);
  border: 1px dashed var(--khaen-border-strong);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
}

.layout-locked-draft__actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.layout-inline-warn {
  color: var(--khaen-warning);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.layout-inline-error {
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.layout-nobase {
  align-items: flex-start;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--khaen-space-3);
}

.layout-nobase__text {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
}

.layout-nobase__title {
  font-weight: 600;
}

.layout-form__numbers {
  display: flex;
  align-items: flex-end;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
}

.layout-field {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
}

.layout-field__label {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.layout-field__count {
  margin-left: auto;
  font-family: var(--khaen-font-mono);
}

.layout-folds__title {
  font-size: var(--khaen-font-aux);
  color: var(--khaen-text-secondary);
  font-weight: 600;
}

.layout-folds__head,
.layout-folds__row {
  display: grid;
  grid-template-columns: 28px minmax(90px, 1fr) minmax(90px, 1fr) minmax(90px, 1fr) auto;
  align-items: center;
  gap: var(--khaen-space-2);
}

.layout-folds__head {
  padding: 0 var(--khaen-space-1);
}

.layout-folds__row {
  padding: var(--khaen-space-1);
  border: 1px dashed var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
}

.layout-folds__index {
  color: var(--khaen-text-muted);
  font-size: 11px;
  text-align: center;
}

.layout-folds__actions {
  display: flex;
  justify-content: flex-start;
}

.layout-actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.layout-fields {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  margin: 0;
}

.layout-fields__row {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.layout-fields__row dt {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

.layout-fields__row dd {
  margin: 0;
  overflow-wrap: anywhere;
}

.layout-fields__json {
  margin: 0;
  padding: var(--khaen-space-2);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-bg-subtle, rgba(127, 127, 127, 0.08));
  font-size: 12px;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  max-height: 220px;
  overflow: auto;
}

@media (max-width: 1279px) {
  .layout-grid__span-5,
  .layout-grid__span-7 {
    grid-column: span 12;
  }
}

@media (max-width: 767px) {
  .layout-folds__head,
  .layout-folds__row {
    grid-template-columns: 24px minmax(70px, 1fr) minmax(70px, 1fr) minmax(70px, 1fr);
  }

  .layout-folds__head span:last-child,
  .layout-folds__row :deep(.el-button) {
    grid-column: span 4;
    justify-self: start;
  }
}
</style>