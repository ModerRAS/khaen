<script setup lang="ts">
/**
 * 工作模式页（task-62 前端地区）—— off / aes67 输入使能两阶段（docs/API.md v1.1 §4 / §9）。
 *
 * 数据来源：
 * - GET /api/meta（AppShell 预热 + 本页 ensureMeta）：surface=mode 的能力三态与 not_wired 原因
 *   （§9.1：未接通的能力显示原因，不渲染可点击的假操作）；
 * - GET /api/status（5s 轮询）：顶层 mode（当前运行模式）+ run state / safe / last_error（写锁）；
 * - POST /api/surface {surface:"mode", data:{mode}} → POST /api/commit {engine_token} /
 *   POST /api/abort {engine_token}：两阶段草稿统一由 control store 管理（busy 互斥、prepare 失败清草稿、
 *   token 只能消费一次、401 交给会话 store、错误归类成稳定码 + 本地化文案）。
 *
 * 诚实边界（不编数据、不假成功）：
 * - 当前模式只认 /api/status 顶层 mode；为 null 时显示 reasons.mode，不用上次提交、默认值或 host_link.mode 顶替
 *   （最近一次提交回执单独标注「不是设备回读」）；
 * - UI 只提供后端已接通的 off / aes67（on 是 aes67 的别名；analog/toslink/dante 未实现，后端 bad_request）；
 * - prepare/commit/abort 失败只显示稳定错误码 + 本地化原因（不弹窗轰炸），401 由共享 ApiClient 交会话 store。
 */
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import StatRow from '@/components/StatRow.vue'
import TruthTag from '@/components/TruthTag.vue'
import DraftReview from '@/components/control/DraftReview.vue'
import ControlErrorNotice from '@/components/control/ControlErrorNotice.vue'
import { useStatusPolling } from '@/composables/useStatusPolling'
import { reasonForPath, runStateKind } from '@/api/status'
import type { ControlCommitAck, ControlErrorInfo, RunMode } from '@/api/control'
import { abortControlDraft, commitControlDraft, controlState, prepareMode } from '@/stores/control'
import {
  ensureMeta,
  metaCapabilityReason,
  metaState,
  metaSurfaceState,
  refreshMeta
} from '@/stores/meta'
import {
  MODE_CHOICES,
  MODE_WRITE_BLOCK_KEYS,
  capabilityStateKey,
  commitChangedNoop,
  draftChangedNoop,
  foreignDraftSurface,
  modeCommitBlocked,
  modeDraftOf,
  modeLabelKey,
  modeWriteBlock
} from '@/components/mode/modeDraft'
import { formatClock, formatInteger, shortenId } from '@/utils/format'
import type { TruthKind } from '@/types/ui'

interface KvRow {
  label: string
  value: string | null
  copy?: string | null
  reason?: string | null
  truth?: TruthKind | null
}

const { t, locale } = useI18n({ useScope: 'global' })
const { state: statusState } = useStatusPolling()

/** 目标模式（本地选择；未 prepare/commit 前不代表任何运行态）。 */
const target = ref<RunMode | null>(null)
/** 本页最近一次 prepare/commit/abort 的错误（不显示其它页面残留的错误）。 */
const actionError = ref<ControlErrorInfo | null>(null)
/** 本页最近一次 commit 回执（只记录本页发起的提交，不把其它页面的回执当成本页结果）。 */
const lastCommit = ref<ControlCommitAck | null>(null)
const lastCommitAt = ref<number | null>(null)
/** 重试按钮重跑的本页动作。 */
const lastAction = ref<(() => Promise<void>) | null>(null)

onMounted(() => {
  // 能力快照通常已由 AppShell 预热；直接深链进入 /mode 时这里保证也有数据。
  void ensureMeta()
})

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const statusData = computed(() => statusState.data)
const surface = computed(() => metaSurfaceState('mode'))
const capabilityReason = computed(() => metaCapabilityReason({ surface: 'mode' }))
const metaErrorCode = computed(() => (metaState.error === null ? null : metaState.error.code))
const metaLoading = computed(() => metaState.phase === 'loading')
const safe = computed(() => statusData.value?.state.safe === true)
const faulted = computed(() => runStateKind(statusData.value?.state ?? null) === 'faulted')
const writeLocked = computed(() => safe.value || faulted.value)
const lockReason = computed<string | null>(() => statusData.value?.state.last_error ?? null)
const busy = computed(() => controlState.busy !== null)
const targetDisabled = computed(
  () => writeLocked.value || surface.value === 'not_wired' || busy.value
)
const block = computed(() =>
  modeWriteBlock({
    surface: surface.value,
    safe: safe.value,
    faulted: faulted.value,
    busy: busy.value,
    target: target.value
  })
)
const blockHint = computed(() => {
  const reason = block.value
  return reason === null ? null : t(MODE_WRITE_BLOCK_KEYS[reason])
})
const draft = computed(() => modeDraftOf(controlState.draft))
const foreignDraft = computed(() => foreignDraftSurface(controlState.draft, 'mode'))
const draftNoop = computed(() => draftChangedNoop(draft.value))
const noopCommit = computed(() => commitChangedNoop(lastCommit.value))
const prepareBusy = computed(() => controlState.busy === 'prepare')
const abortBusy = computed(() => controlState.busy === 'abort')
/** safe/faulted（或界面未接通）时 commit 也禁止：只读态不允许改变运行态；abort 仍可达。 */
const commitBlocked = computed(() =>
  modeCommitBlocked({ surface: surface.value, safe: safe.value, faulted: faulted.value })
)
const commitBlockText = computed(() =>
  writeLocked.value ? t('mode.notice.lockedDraft') : t('mode.block.capability')
)
const lockedDraftVersion = computed(() => {
  const current = draft.value
  return current === null ? null : formatInteger(current.planned_version, localeId.value)
})
const lockedDraftToken = computed(() => {
  const current = draft.value
  return current === null ? null : current.engine_token
})

/* ------------------------------------------------------------- 能力 / 当前值 */

const capabilityTruth = computed<TruthKind>(() => (surface.value === 'wired' ? 'actual' : 'notWired'))
const capabilityText = computed(() => t(capabilityStateKey(surface.value)))
const capabilityHint = computed(() => {
  if (surface.value === 'wired') {
    return t('mode.capability.wiredHint')
  }
  if (surface.value === 'not_wired') {
    return t('mode.capability.notWiredHint')
  }
  return t('mode.capability.unknownHint')
})

const capabilityRows = computed<KvRow[]>(() => {
  const rows: KvRow[] = [
    { label: t('mode.capability.field.surface'), value: 'mode' },
    { label: t('mode.capability.field.state'), value: capabilityText.value, truth: capabilityTruth.value }
  ]
  if (surface.value !== 'wired') {
    rows.push({
      label: t('mode.capability.field.reason'),
      value: capabilityReason.value ?? t('mode.capability.reasonMissing'),
      truth: 'notWired'
    })
  }
  return rows
})

const mutedText = computed(() => {
  const muted = statusData.value?.state.muted
  if (muted === null || muted === undefined) {
    return null
  }
  return muted ? t('mode.current.mutedOn') : t('mode.current.mutedOff')
})

function modeDisplay(raw: string | null): string | null {
  if (raw === null) {
    return null
  }
  const key = modeLabelKey(raw)
  return key === null ? raw : t(key)
}

/** 设备此刻的运行模式。只认 status.mode，不看 host_link.mode 或提交回执。 */
const runtimeMode = computed(() => (statusData.value === null ? null : statusData.value.mode))
const modeReason = computed<string | null>(() => {
  if (statusData.value === null) {
    return t('mode.current.statusMissing')
  }
  if (runtimeMode.value !== null) {
    return null
  }
  return reasonForPath(statusData.value, 'mode') ?? t('mode.current.reason')
})
const currentTruth = computed<TruthKind>(() => (runtimeMode.value === null ? 'notWired' : 'actual'))
const currentModeText = computed(() => {
  if (runtimeMode.value === null) {
    return t('mode.current.unavailable')
  }
  return modeDisplay(runtimeMode.value)
})

const currentRows = computed<KvRow[]>(() => [
  {
    label: t('mode.current.receiptLabel'),
    value: modeDisplay(lastCommit.value === null ? null : lastCommit.value.mode),
    reason: lastCommit.value === null ? t('mode.current.noReceipt') : t('mode.current.receiptNote')
  },
  {
    label: t('mode.current.mutedLabel'),
    value: mutedText.value,
    reason: mutedText.value === null ? t('mode.current.mutedUnknown') : null
  }
])

const notices = computed<Array<{ tone: 'warn' | 'danger' | 'info'; text: string }>>(() => {
  const banners: Array<{ tone: 'warn' | 'danger' | 'info'; text: string }> = []
  if (statusData.value === null) {
    banners.push({ tone: 'info', text: t('mode.notice.statusUnknown') })
  }
  if (safe.value) {
    banners.push({
      tone: 'warn',
      text:
        lockReason.value === null
          ? t('mode.notice.safeReadOnly')
          : t('mode.notice.safeReadOnlyReason', { reason: lockReason.value })
    })
  } else if (faulted.value) {
    banners.push({
      tone: 'danger',
      text:
        lockReason.value === null
          ? t('mode.notice.faultedReadOnly')
          : t('mode.notice.faultedReadOnlyReason', { reason: lockReason.value })
    })
  }
  if (writeLocked.value && draft.value !== null) {
    banners.push({ tone: 'info', text: t('mode.notice.abortStillAvailable') })
  }
  if (foreignDraft.value !== null) {
    banners.push({ tone: 'info', text: t('mode.notice.foreignDraft', { surface: foreignDraft.value }) })
  }
  return banners
})

/* ------------------------------------------------------------------- 回执 */

function changedText(changed: boolean | null): string {
  if (changed === true) {
    return t('draftReview.changedYes')
  }
  if (changed === false) {
    return t('draftReview.changedNo')
  }
  return t('draftReview.notReported')
}

/** 缺失的可选回执字段如实显示「服务端未上报」，不臆造。 */
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
      label: t('control.receipt.mode'),
      value: receiptText(modeDisplay(ack.mode)),
      truth: ack.mode === null ? 'notWired' : null
    },
    {
      label: t('control.receipt.changed'),
      value: changedText(ack.changed),
      truth: ack.changed === null ? 'notWired' : null
    }
  ]
})

/* ------------------------------------------------------------------- 动作 */

function onTargetChange(value: string | number | boolean | undefined): void {
  if (value === 'off' || value === 'aes67') {
    target.value = value
  }
}

async function onPrepare(): Promise<void> {
  const selected = target.value
  if (selected === null || block.value !== null) {
    return
  }
  lastAction.value = onPrepare
  const prepared = await prepareMode(selected)
  actionError.value = controlState.error
  if (prepared !== null) {
    ElMessage.success(t('mode.target.prepared'))
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
  <div class="mode-page">
    <PageHeader title-key="mode.pageTitle" subtitle-key="mode.subtitle">
      <template #actions>
        <el-button size="small" plain :loading="metaLoading" @click="onRefreshMeta">
          {{ t('mode.capability.refresh') }}
        </el-button>
      </template>
    </PageHeader>

    <ControlErrorNotice :error="actionError" @retry="onRetry" />

    <p v-for="notice in notices" :key="notice.text" class="banner" :class="'banner--' + notice.tone" role="status">
      {{ notice.text }}
    </p>

    <div class="grid-12 mode-grid">
      <PanelCard
        class="mode-grid__span-5"
        title-key="mode.capability.title"
        desc-key="mode.capability.desc"
        :truth="capabilityTruth"
      >
        <p v-if="metaLoading" class="aux-text">{{ t('mode.capability.loading') }}</p>
        <p v-if="metaErrorCode !== null" class="mode-inline-warn" role="status">
          {{ t('mode.capability.metaError', { code: metaErrorCode }) }}
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
        class="mode-grid__span-7"
        title-key="mode.current.title"
        desc-key="mode.current.desc"
        :truth="currentTruth"
      >
        <div class="metric">
          <span class="metric__value" :class="{ 'metric__value--muted': runtimeMode === null }">
            {{ currentModeText }}
          </span>
          <TruthTag
            :kind="currentTruth"
            :reason="runtimeMode === null ? modeReason : t('mode.current.source')"
          />
        </div>
        <ul class="kv-list">
          <StatRow
            v-for="row in currentRows"
            :key="row.label"
            :label="row.label"
            :value="row.value"
            :reason="row.reason ?? null"
          />
        </ul>
        <p class="aux-text">{{ t('mode.current.mutedNote') }}</p>
      </PanelCard>

      <PanelCard
        class="mode-grid__span-12"
        title-key="mode.target.title"
        desc-key="mode.target.desc"
        :truth="surface === 'not_wired' ? 'notWired' : 'actual'"
      >
        <el-radio-group
          :model-value="target ?? undefined"
          :disabled="targetDisabled"
          @change="onTargetChange"
        >
          <el-radio-button v-for="choice in MODE_CHOICES" :key="choice.value" :value="choice.value">
            {{ t(choice.labelKey) }}
          </el-radio-button>
        </el-radio-group>
        <p class="aux-text">{{ t('mode.target.alias') }}</p>
        <p class="mode-analog">
          <TruthTag kind="notWired" :reason="t('mode.target.analogReason')" />
          <span class="aux-text">{{ t('mode.target.analogReason') }}</span>
        </p>
        <div class="mode-actions">
          <el-button
            type="primary"
            :loading="prepareBusy"
            :disabled="block !== null"
            @click="onPrepare"
          >
            {{ t('control.actions.prepare') }}
          </el-button>
          <span v-if="blockHint !== null" class="aux-text">{{ blockHint }}</span>
          <span v-else-if="draft !== null" class="aux-text">{{ t('mode.target.prepared') }}</span>
        </div>
      </PanelCard>

      <p v-if="draftNoop" class="mode-grid__span-12 banner banner--info" role="status">
        {{ t('mode.target.noopHint') }}
      </p>
      <DraftReview
        v-if="draft !== null && !commitBlocked"
        class="mode-grid__span-12"
        :draft="draft"
        :busy="busy"
        @commit="onCommit"
        @abort="onAbort"
      />
      <div v-else-if="draft !== null" class="mode-grid__span-12 mode-locked-draft">
        <p class="banner banner--warn" role="status">{{ commitBlockText }}</p>
        <div class="mode-locked-draft__actions">
          <span class="aux-text">{{ t('mode.notice.abortStillAvailable') }}</span>
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
        class="mode-grid__span-12"
        title-key="mode.receipt.title"
        desc-key="mode.receipt.desc"
        :truth="lastCommit === null ? 'notWired' : 'actual'"
      >
        <p v-if="lastCommit === null" class="aux-text">{{ t('mode.receipt.empty') }}</p>
        <template v-else>
          <ul class="kv-list">
            <StatRow
              v-for="row in receiptRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :truth="row.truth ?? null"
            />
            <StatRow
              :label="t('control.receipt.at')"
              :value="lastCommitAt === null ? t('draftReview.notReported') : formatClock(lastCommitAt, localeId)"
            />
          </ul>
          <p v-if="noopCommit" class="banner banner--info" role="status">
            {{ t('mode.notice.noopCommit') }}
          </p>
        </template>
      </PanelCard>
    </div>
  </div>
</template>

<style scoped>
.mode-page {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}

.mode-grid__span-5 {
  grid-column: span 5;
}

.mode-grid__span-7 {
  grid-column: span 7;
}

.mode-grid__span-12 {
  grid-column: span 12;
}

.mode-locked-draft {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-3);
  border: 1px dashed var(--khaen-border-strong);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
}

.mode-locked-draft__actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.mode-inline-warn {
  color: var(--khaen-warning);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.mode-analog {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.mode-actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}


@media (max-width: 1279px) {
  .mode-grid__span-5,
  .mode-grid__span-7 {
    grid-column: span 12;
  }
}
</style>