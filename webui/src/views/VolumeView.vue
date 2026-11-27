<script setup lang="ts">
/**
 * 音量与静音页（M3 / 任务 T53；MOTU 接入 task-62）——控制面两阶段 + MOTU 设备真值。
 *
 * 数据来源（页面唯一事实源）：
 * - GET /api/status（5s 轮询）：state.muted / state.safe / state.value / format.channels / plan.version；
 * - POST /api/mute：静音立即生效（安全操作），回读以 /api/status 的下一次刷新为准（不伪造成功）；
 * - POST /api/surface {surface:'volume', data:{gains_db}} → POST /api/commit {engine_token}：逐 lane 保护增益
 *   两阶段应用；prepare 只准备草稿，commit 才在块边界生效；abort 丢弃（共享 control store，本页不改其行为）；
 * - GET /api/motu（5s 轮询，页面隐藏暂停）：MOTU 设备检测 / 当前音量（vol[] 设备真值回读）/ 写能力；
 * - PUT /api/motu/volume：{stage:'prepare', channel, db} 只生成草稿（writes_device=false），
 *   {stage:'commit', engine_token} 才逐通道写设备并返回真值回读（writes_device=true）；
 *   该端点**没有 abort**：取消只丢本地草稿，服务端草稿按 TTL（expires_in_s=120s）过期。
 *
 * 诚实边界（不编数据、不假成功）：
 * - 控制协议没有 master volume op：主音量卡如实显示「未接入」+ 原因 + 任务号，不把"写所有 lane"伪装成主音量；
 * - 控制协议没有读取"当前 per-lane 保护增益"的 op：草稿从 0 dB 中性值开始，当前实际增益标「未接入」；
 * - MOTU vol[] 是 CueMix 设备（koBusFader）真值回读，**不是**逐 lane 保护增益，两者不混用；
 * - MOTU 设备不可用 / 控制网不通 / 未检测到时显示原因并禁用写；失败回执绝不显示成已写入；
 * - 工作模式卡已移出本页（ModeView 由独立任务实现），本页不再提交 mode；
 * - safe / faulted 时禁用音量/格式写入（避免故障下继续写），但**保留静音开关**——设备安全锁存后的
 *   恢复路径就是显式 /api/mute false（crates/daemon/src/device.rs），不能把恢复入口也关掉。
 */
import { computed, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import { Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import StatRow from '@/components/StatRow.vue'
import StatusBadge from '@/components/StatusBadge.vue'
import TruthTag from '@/components/TruthTag.vue'
import ControlErrorNotice from '@/components/control/ControlErrorNotice.vue'
import DraftReview from '@/components/control/DraftReview.vue'
import MotuDeviceCard from '@/components/motu/MotuDeviceCard.vue'
import MotuVolumeReadback from '@/components/motu/MotuVolumeReadback.vue'
import MotuVolumeControl from '@/components/motu/MotuVolumeControl.vue'
import { useMotuPolling } from '@/components/motu/useMotuPolling'
import { useStatusPolling } from '@/composables/useStatusPolling'
import { isStatusStale, reasonForPath, runStateKind, runStateLabelKey } from '@/api/status'
import { annotateGapById, gapLocaleOf } from '@/api/capabilityGaps'
import {
  VOLUME_GAIN_MAX_DB,
  VOLUME_GAIN_MIN_DB,
  VOLUME_GAIN_STEP_DB,
  buildGainPatch,
  createLaneDrafts,
  normalizeGainDb,
  validateGainPatch,
  type ControlCommitAck,
  type ControlDraft,
  type LaneGainDraft
} from '@/api/control'
import {
  abortControlDraft,
  commitControlDraft,
  controlState,
  prepareVolume,
  setControlMute
} from '@/stores/control'
import { formatClock, formatDecimal, formatInteger, shortenId } from '@/utils/format'
import type { StatusKind } from '@/types/ui'

const { t, locale } = useI18n({ useScope: 'global' })
const router = useRouter()
const { state: statusState, refresh } = useStatusPolling()
// MOTU 设备快照：页面可见时 5s 轮询（composable 接 visibilitychange，卸载停止）。
const { state: motuState, refresh: refreshMotu } = useMotuPolling()

const data = computed(() => statusState.data)
const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const refreshing = computed(() => statusState.refreshing)
const controlError = computed(() => controlState.error)

/** /api/status 解析过但页面展示只需要这几个字段。 */
const muted = computed<boolean | null>(() => data.value?.state.muted ?? null)
const runState = computed<StatusKind>(() => (data.value === null ? 'unknown' : runStateKind(data.value.state)))
const faulted = computed(() => runState.value === 'faulted')
const safe = computed(() => data.value?.state.safe === true)
/** 故障/安全态禁用"写参数"（静音开关是恢复路径，单独放行）。 */
const writeLocked = computed(() => faulted.value || safe.value)
const lockReason = computed<string | null>(() => data.value?.state.last_error ?? null)

const laneCount = computed<number | null>(() => data.value?.format.channels ?? null)
const lanes = ref<LaneGainDraft[]>([])

function resetLaneDrafts(): void {
  lanes.value = laneCount.value === null ? [] : createLaneDrafts(laneCount.value)
}

// 首次拿到 status 时（laneCount 由 null -> N）生成草稿；轮询值不变时不重建，避免覆盖正在编辑的草稿。
let lastLaneCount: number | null = null
watch(
  laneCount,
  () => {
    if (laneCount.value === null) {
      lastLaneCount = null
      lanes.value = []
      return
    }
    if (laneCount.value !== lastLaneCount || lanes.value.length !== laneCount.value) {
      lastLaneCount = laneCount.value
      resetLaneDrafts()
    }
  },
  { immediate: true }
)

/* ------------------------------------------------------------- 提示条 / 状态带 */

const stale = computed(() => isStatusStale(data.value))

const lastUpdateText = computed(() => {
  if (statusState.loadedAt === null) {
    return t('volume.band.neverUpdated')
  }
  const time = formatClock(statusState.loadedAt, localeId.value)
  return time === null ? t('volume.band.neverUpdated') : t('volume.band.lastUpdate', { time })
})

const planVersionText = computed(() => {
  const version = data.value?.plan.version ?? null
  return version === null ? null : 'v' + (formatInteger(version, localeId.value) ?? String(version))
})

const muteBadge = computed<{ kind: StatusKind; value: string | undefined }>(() => {
  if (muted.value === null) {
    return { kind: 'unknown', value: undefined }
  }
  return muted.value
    ? { kind: 'muted', value: t('volume.band.mutedOn') }
    : { kind: 'running', value: t('volume.band.mutedOff') }
})

const runStateBadgeText = computed(() => {
  const key = data.value === null ? null : runStateLabelKey(data.value.state)
  if (key !== null) {
    return t(key)
  }
  return data.value?.state.value ?? undefined
})

const notices = computed<Array<{ tone: 'warn' | 'danger' | 'info'; text: string }>>(() => {
  const banners: Array<{ tone: 'warn' | 'danger' | 'info'; text: string }> = []
  const current = data.value
  if (current === null) {
    return banners
  }
  if (safe.value) {
    banners.push({
      tone: 'warn',
      text:
        lockReason.value === null
          ? t('volume.notice.safeReadOnly')
          : t('volume.notice.safeReadOnlyReason', { reason: lockReason.value })
    })
  } else if (faulted.value) {
    banners.push({
      tone: 'danger',
      text:
        lockReason.value === null
          ? t('volume.notice.faultedReadOnly')
          : t('volume.notice.faultedReadOnlyReason', { reason: lockReason.value })
    })
  }
  if (stale.value) {
    banners.push({ tone: 'warn', text: t('volume.notice.stale') })
  }
  if (statusState.error !== null) {
    const time =
      statusState.loadedAt === null
        ? t('common.dash')
        : formatClock(statusState.loadedAt, localeId.value) ?? t('common.dash')
    banners.push({ tone: 'warn', text: t('volume.notice.refreshFailed', { message: t(statusState.error.i18nKey), time }) })
  }
  if (muted.value === null && current.host_link.connected === false) {
    banners.push({ tone: 'info', text: t('volume.notice.muteUnknown') })
  }
  return banners
})

/* ------------------------------------------------------------------ 静音 */

const muteBusy = computed(() => controlState.busy === 'mute')

async function onToggleMute(next: boolean | string | number): Promise<void> {
  const target = next === true
  const ack = await setControlMute(target)
  if (ack === null) {
    // MOTU 副作用失败（502 + error.details.phase=motu_mute）：引擎已静音但设备写入失败，分开如实提示。
    const failure = controlState.error?.sideEffect ?? null
    if (failure !== null) {
      ElMessage.error(
        t('volume.mute.motu.failedToast', {
          code: failure.errorCode ?? t('common.dash'),
          message: failure.errorMessage ?? (controlState.error?.detail ?? '')
        })
      )
      return
    }
    ElMessage.error(controlState.error === null ? t('error.control.unknown') : t(controlState.error.i18nKey))
    return
  }
  if (ack.motu !== null && ack.motu.applied === false) {
    // 引擎静音成功、设备副作用跳过：别弹“成功”，让用户看到设备未写入的真实原因。
    ElMessage.warning(
      t('volume.mute.motu.skippedToast', {
        reason: ack.motu.reason_detail ?? ack.motu.reason ?? t('common.dash')
      })
    )
  } else {
    ElMessage.success(ack.mute ? t('volume.mute.successMuted') : t('volume.mute.successUnmuted'))
  }
  await refresh()
}

interface KvRow {
  label: string
  value: string | null
  copy?: string | null
  reason?: string | null
  tone?: 'ok' | 'warn' | 'bad' | null
}

const lastMuteAppliedBlockText = computed<string | null>(() => {
  const ack = controlState.lastMute
  if (ack === null || ack.applied_block === null) {
    return null
  }
  return formatInteger(ack.applied_block, localeId.value)
})

const muteRows = computed<KvRow[]>(() => {
  const current = data.value
  if (current === null) {
    return []
  }
  return [
    {
      label: t('volume.mute.field.readback'),
      value: muted.value === null ? null : muted.value ? t('volume.band.mutedOn') : t('volume.band.mutedOff'),
      reason: reasonForPath(current, 'state.muted'),
      tone: muted.value === true ? ('warn' as const) : null
    },
    {
      label: t('volume.mute.field.runState'),
      value: runStateBadgeText.value ?? null,
      reason: reasonForPath(current, 'state.value')
    },
    {
      label: t('volume.mute.field.appliedBlock'),
      value: lastMuteAppliedBlockText.value,
      reason: controlState.lastMute === null ? t('volume.mute.noReceipt') : null
    },
    ...muteMotuRows.value
  ]
})

/** /api/mute 的 MOTU 设备副作用：成功回执里的 motu 对象 + 失败回执里的 details 都如实展示。 */
const muteMotuRows = computed<KvRow[]>(() => {
  const rows: KvRow[] = []
  const motu = controlState.lastMute?.motu ?? null
  if (motu !== null) {
    rows.push({
      label: t('volume.mute.motu.field'),
      value:
        motu.applied === true
          ? t('volume.mute.motu.applied')
          : motu.detected === false
            ? t('volume.mute.motu.skipped')
            : t('volume.mute.motu.notApplied'),
      reason: motu.reason_detail ?? motu.reason,
      tone: motu.applied === true ? ('ok' as const) : motu.detected === false ? ('warn' as const) : ('bad' as const)
    })
  }
  const failure = controlState.error?.sideEffect ?? null
  if (failure !== null) {
    rows.push({
      label: t('volume.mute.motu.failedField'),
      value: failure.errorCode ?? t('common.dash'),
      reason:
        [failure.errorMessage, failure.reason_detail ?? failure.reason].filter((part) => part !== null && part.length > 0).join(' · ') ||
        null,
      tone: 'bad' as const
    })
  }
  return rows
})

/* --------------------------------------------------------- 逐 lane 增益草稿 */

const uniformValue = ref(0)
const gainValidation = ref<string | null>(null)

const gainPatch = computed(() => buildGainPatch(lanes.value))

function onLaneGain(slot: number, value: unknown): void {
  const draft = lanes.value[slot]
  if (draft === undefined) {
    return
  }
  const raw = Array.isArray(value) ? value[0] : value
  draft.gain_db = normalizeGainDb(typeof raw === 'number' ? raw : Number(raw))
  draft.included = true
}

function onLaneInclude(slot: number, value: unknown): void {
  const draft = lanes.value[slot]
  if (draft !== undefined) {
    draft.included = value === true
  }
}

function applyUniform(): void {
  const value = normalizeGainDb(Number(uniformValue.value))
  for (const draft of lanes.value) {
    draft.gain_db = value
  }
}

function includeAll(): void {
  for (const draft of lanes.value) {
    draft.included = true
  }
}

function clearAll(): void {
  resetLaneDrafts()
  gainValidation.value = null
}

function gainText(gainDb: number): string {
  const text = formatDecimal(gainDb, 1, localeId.value)
  return text === null ? t('common.dash') : text + ' ' + t('unit.db')
}

const laneHint = computed(() => {
  if (laneCount.value === null) {
    return t('volume.gains.noLanes', {
      reason: data.value === null ? t('common.unknown') : reasonForPath(data.value, 'format.channels') ?? t('volume.gains.noLanesReasonUnknown')
    })
  }
  return annotateGapById(t('volume.gains.readbackMissing'), 'volumeLaneGain', gapLocaleOf(locale.value))
})

const gainDraftRow = computed(() => {
  const patch = gainPatch.value
  return patch.map((entry) => ({ lane: entry.lane, gain_db: entry.gain_db }))
})

const canPrepareVolume = computed(() => {
  if (writeLocked.value || laneCount.value === null || controlState.busy !== null) {
    return false
  }
  return validateGainPatch(gainPatch.value, laneCount.value) === null
})

const volumeDraftReady = computed<ControlDraft | null>(() =>
  controlState.draft !== null && controlState.draft.surface === 'volume' ? controlState.draft : null
)

async function onPrepareVolume(): Promise<void> {
  const patch = gainPatch.value
  const invalid = validateGainPatch(patch, laneCount.value ?? 0)
  gainValidation.value = invalid
  if (invalid !== null) {
    return
  }
  const draft = await prepareVolume(patch)
  if (draft !== null) {
    ElMessage.success(t('volume.gains.prepared'))
  }
}

/* ------------------------------------------------------------ 两阶段回执区 */

function surfaceText(surface: string | null): string | null {
  if (surface === null) {
    return null
  }
  const key = 'control.surface.' + surface
  const text = t(key)
  return text === key ? surface : text
}

function commitRows(ack: ControlCommitAck): KvRow[] {
  const rows: KvRow[] = [
    {
      label: t('control.receipt.version'),
      value: ack.version === null ? null : formatInteger(ack.version, localeId.value)
    },
    {
      label: t('control.receipt.appliedBlock'),
      value: ack.applied_block === null ? null : formatInteger(ack.applied_block, localeId.value)
    }
  ]
  if (ack.changed !== null) {
    rows.push({ label: t('control.receipt.changed'), value: ack.changed ? t('common.yes') : t('common.no') })
  }
  if (ack.graph_id !== null) {
    rows.push({ label: t('control.receipt.graphId'), value: shortenId(ack.graph_id, 10, 6), copy: ack.graph_id })
  }
  if (ack.mode !== null) {
    rows.push({ label: t('control.receipt.mode'), value: surfaceText(ack.mode) ?? ack.mode })
  }
  if (ack.format !== null) {
    rows.push({ label: t('control.receipt.format'), value: formatText(ack.format) })
  }
  if (ack.device_restart_required !== null) {
    rows.push({
      label: t('control.receipt.deviceRestart'),
      value: ack.device_restart_required ? t('common.yes') : t('common.no')
    })
  }
  return rows
}

const commitReceiptRows = computed(() => {
  const ack = controlState.lastCommit
  if (ack === null) {
    return []
  }
  return commitRows(ack)
})

const commitDeviceRestart = computed(() => controlState.lastCommit?.device_restart_required === true)

function formatText(format: { sample_rate: number | null; block_size: number | null; channels: number | null }): string {
  return t('control.format.summary', {
    rate: format.sample_rate === null ? t('common.dash') : formatInteger(format.sample_rate, localeId.value) ?? t('common.dash'),
    block: format.block_size === null ? t('common.dash') : formatInteger(format.block_size, localeId.value) ?? t('common.dash'),
    channels: format.channels === null ? t('common.dash') : formatInteger(format.channels, localeId.value) ?? t('common.dash')
  })
}

const historyRows = computed(() => controlState.history)

const ACTION_KEYS: Record<string, string> = {
  mute: 'control.history.action.mute',
  prepare: 'control.history.action.prepare',
  commit: 'control.history.action.commit',
  abort: 'control.history.action.abort',
  retry: 'control.history.action.retry'
}

function actionText(action: string): string {
  const key = ACTION_KEYS[action]
  return key === undefined ? action : t(key)
}

function historyTime(at: number): string {
  return formatClock(at, localeId.value) ?? t('common.dash')
}

/* ------------------------------------------------------------------ 动作 */

const prepareBusy = computed(() => controlState.busy === 'prepare')

async function onCommit(): Promise<void> {
  if (controlState.draft === null) {
    return
  }
  const ack = await commitControlDraft()
  if (ack === null) {
    return
  }
  ElMessage.success(t('control.toast.committed'))
  await refresh()
}

async function onAbort(): Promise<void> {
  if (controlState.draft === null) {
    return
  }
  const ack = await abortControlDraft()
  if (ack !== null) {
    ElMessage.success(t('control.toast.aborted'))
  }
}

function goToRestart(): void {
  void router.push({ name: 'system', hash: '#system-restart' })
}

function onRefresh(): void {
  void refresh()
  void refreshMotu()
}

function onRefreshMotu(): void {
  void refreshMotu()
}
</script>

<template>
  <div class="volume-page">
    <PageHeader title-key="volume.pageTitle" subtitle-key="volume.subtitle">
      <template #actions>
        <span class="volume-page__updated">{{ lastUpdateText }}</span>
        <el-button
          type="primary"
          plain
          :loading="refreshing"
          :disabled="refreshing"
          :aria-label="t('volume.actions.refresh')"
          @click="onRefresh"
        >
          <el-icon v-if="!refreshing"><Refresh /></el-icon>
          <span>{{ t('volume.actions.refresh') }}</span>
        </el-button>
      </template>
    </PageHeader>

    <div v-if="data === null && statusState.phase === 'error'" class="banner banner--danger volume-page__fatal" role="alert">
      <div class="volume-page__fatal-text">
        <span class="volume-page__fatal-title">{{ t('volume.error.title') }}</span>
        <span>{{ statusState.error === null ? t('error.status.unknown') : t(statusState.error.i18nKey) }}</span>
      </div>
      <el-button type="primary" :loading="refreshing" @click="onRefresh">{{ t('common.retry') }}</el-button>
    </div>

    <ControlErrorNotice :error="controlError" @retry="onRefresh" />

    <p v-for="banner in notices" :key="banner.text" class="banner" :class="'banner--' + banner.tone" role="status">
      {{ banner.text }}
    </p>

    <div v-if="data" class="status-band">
      <StatusBadge :kind="muteBadge.kind" :label="t('volume.band.mute')" :value="muteBadge.value" />
      <span class="status-band__divider" aria-hidden="true" />
      <StatusBadge :kind="runState" :label="t('volume.band.runState')" :value="runStateBadgeText" />
      <span class="status-band__divider" aria-hidden="true" />
      <StatusBadge :kind="planVersionText === null ? 'unknown' : 'safe'" :label="t('volume.band.planVersion')" :value="planVersionText ?? undefined" />
      <span class="status-band__meta">{{ lastUpdateText }}</span>
    </div>

    <div v-if="data && !statusState.loadedAt && statusState.phase === 'loading'" class="banner banner--info">
      {{ t('common.loading') }}
    </div>

    <!-- MOTU 设备独立于 /api/status：即使控制面状态不可用也照实展示设备真值 -->
    <div class="grid-12 volume-grid volume-grid--motu">
      <MotuDeviceCard
        class="volume-grid__span-7"
        :status="motuState.status"
        :error="motuState.error"
        :loaded-at="motuState.loadedAt"
        :busy="motuState.refreshing"
        @refresh="onRefreshMotu"
      />
      <MotuVolumeReadback class="volume-grid__span-5" :status="motuState.status" />
      <MotuVolumeControl class="volume-grid__span-12" :status="motuState.status" />
    </div>

    <div v-if="data" class="grid-12 volume-grid">
      <!-- 静音：真实写操作（安全操作，faulted/safe 时也保留为恢复路径） -->
      <PanelCard class="volume-grid__span-5" title-key="volume.mute.title" desc-key="volume.mute.desc" :truth="muted === null ? 'notWired' : 'actual'">
        <div class="volume-mute">
          <div class="volume-mute__state">
            <span class="metric__value" :class="muted === true ? 'metric__value--warn' : muted === false ? 'metric__value--ok' : 'metric__value--muted'">
              {{ muted === null ? t('common.dash') : muted ? t('volume.mute.on') : t('volume.mute.off') }}
            </span>
            <TruthTag v-if="muted === null" kind="notWired" :reason="data === null ? null : reasonForPath(data, 'state.muted')" />
          </div>
          <div class="volume-mute__control">
            <span class="volume-mute__label">{{ t('volume.mute.switch') }}</span>
            <el-switch
              :model-value="muted === true"
              size="large"
              :loading="muteBusy"
              :disabled="muted === null || (controlState.busy !== null && !muteBusy)"
              :aria-label="t('volume.mute.switch')"
              @change="onToggleMute"
            />
          </div>
        </div>
        <ul class="kv-list">
          <StatRow
            v-for="row in muteRows"
            :key="row.label"
            :label="row.label"
            :value="row.value"
            :reason="row.reason ?? null"
            :tone="row.tone ?? null"
          />
        </ul>
        <p class="aux-text">{{ t('volume.mute.recoveryNote') }}</p>
      </PanelCard>

      <!-- 主音量：控制协议没有 master volume（如实 NOT_AVAILABLE） -->
      <PanelCard class="volume-grid__span-5" title-key="volume.master.title" desc-key="volume.master.desc" truth="notWired">
        <div class="metric">
          <span class="metric__value metric__value--muted">{{ t('common.dash') }}</span>
          <TruthTag kind="notWired" :reason="annotateGapById(t('volume.master.reason'), 'volumeMaster', gapLocaleOf(locale))" />
        </div>
        <p class="volume-master__reason">{{ annotateGapById(t('volume.master.reason'), 'volumeMaster', gapLocaleOf(locale)) }}</p>
        <ul class="kv-list">
          <StatRow :label="t('volume.master.field.source')" :value="t('volume.master.sourceValue')" :reason="null" />
          <StatRow :label="t('volume.master.field.task')" :value="t('volume.master.taskValue')" :reason="null" />
          <StatRow :label="t('volume.master.field.readback')" :value="null" :reason="annotateGapById(t('volume.master.reason'), 'volumeMaster', gapLocaleOf(locale))" />
        </ul>
        <p class="aux-text">{{ t('volume.master.replacedBy') }}</p>
      </PanelCard>

      <!-- 逐 lane 增益：两阶段写；当前值不可读（不编数值） -->
      <PanelCard
        class="volume-grid__span-7"
        title-key="volume.gains.title"
        desc-key="volume.gains.desc"
        :truth="laneCount === null ? 'notWired' : 'actual'"
      >
        <p class="volume-gains__readback">
          <TruthTag kind="notWired" :reason="laneHint" />
          <span class="aux-text">{{ laneHint }}</span>
        </p>
        <p class="aux-text">{{ t('motu.gainsSeparation') }}</p>
        <div v-if="laneCount !== null" class="volume-gains__toolbar">
          <span class="volume-gains__toolbar-label">{{ t('volume.gains.toolbar.uniform') }}</span>
          <el-input-number
            v-model="uniformValue"
            :min="VOLUME_GAIN_MIN_DB"
            :max="VOLUME_GAIN_MAX_DB"
            :step="VOLUME_GAIN_STEP_DB"
            :precision="1"
            size="small"
            :disabled="controlState.busy !== null"
            class="volume-gains__uniform-input"
          />
          <el-button size="small" :disabled="controlState.busy !== null" @click="applyUniform">{{ t('volume.gains.toolbar.applyUniform') }}</el-button>
          <el-button size="small" :disabled="controlState.busy !== null" @click="includeAll">{{ t('volume.gains.toolbar.includeAll') }}</el-button>
          <el-button size="small" :disabled="controlState.busy !== null" @click="clearAll">{{ t('volume.gains.toolbar.clear') }}</el-button>
          <span class="aux-text volume-gains__count">{{ t('volume.gains.count', { count: laneCount }) }}</span>
        </div>
        <div v-if="laneCount !== null" class="volume-gains__list">
          <div v-for="(draft, index) in lanes" :key="draft.lane" class="volume-gains__row">
            <el-checkbox
              :model-value="draft.included"
              :disabled="writeLocked || controlState.busy !== null"
              :aria-label="t('volume.gains.include') + ' ' + draft.lane"
              @change="onLaneInclude(index, $event)"
            />
            <span class="volume-gains__lane mono">lane {{ draft.lane }}</span>
            <el-slider
              :model-value="draft.gain_db"
              :min="VOLUME_GAIN_MIN_DB"
              :max="VOLUME_GAIN_MAX_DB"
              :step="VOLUME_GAIN_STEP_DB"
              :disabled="writeLocked || controlState.busy !== null"
              :aria-label="t('volume.gains.lane', { lane: draft.lane })"
              class="volume-gains__slider"
              @input="onLaneGain(index, $event)"
            />
            <span class="volume-gains__value mono" :class="{ 'volume-gains__value--included': draft.included }">
              {{ gainText(draft.gain_db) }}
            </span>
          </div>
        </div>
        <p v-if="gainValidation !== null" class="volume-gains__invalid" role="alert">
          {{ t('volume.gains.validation.' + gainValidation) }}
        </p>
        <p v-if="gainDraftRow.length > 0" class="aux-text">
          {{ t('volume.gains.patchSummary', { count: gainDraftRow.length }) }}
        </p>
        <div class="volume-gains__actions">
          <el-button
            type="primary"
            :loading="prepareBusy"
            :disabled="!canPrepareVolume"
            @click="onPrepareVolume"
          >
            {{ t('control.actions.prepare') }}
          </el-button>
          <span v-if="writeLocked" class="aux-text">{{ t('volume.gains.lockedHint') }}</span>
          <span v-else-if="laneCount !== null && gainDraftRow.length === 0" class="aux-text">{{ t('volume.gains.emptyPatchHint') }}</span>
          <span v-else-if="volumeDraftReady !== null" class="aux-text">{{ t('volume.gains.draftReady', { version: volumeDraftReady.planned_version ?? t('common.dash') }) }}</span>
        </div>
        <DraftReview
          v-if="volumeDraftReady !== null"
          :draft="volumeDraftReady"
          :busy="controlState.busy !== null"
          @commit="onCommit"
          @abort="onAbort"
        />
      </PanelCard>

      <!-- 两阶段回执：draft → commit → 回执 + 操作历史 -->
      <PanelCard class="volume-grid__span-12" title-key="control.receipt.title" desc-key="control.receipt.desc" :truth="controlState.lastCommit === null && controlState.draft === null ? 'notWired' : 'actual'">
        <div class="control-two-phase">
          <section class="control-two-phase__col">
            <h3 class="control-two-phase__heading">{{ t('control.receipt.commitTitle') }}</h3>
            <p v-if="controlState.lastCommit === null" class="aux-text">{{ t('control.receipt.commitEmpty') }}</p>
            <template v-else>
              <ul class="kv-list">
                <StatRow
                  v-for="row in commitReceiptRows"
                  :key="row.label"
                  :label="row.label"
                  :value="row.value"
                  :copy="row.copy ?? null"
                />
                <StatRow :label="t('control.receipt.at')" :value="formatClock(controlState.lastCommitAt, localeId)" :reason="null" />
              </ul>
              <div v-if="commitDeviceRestart" class="banner banner--warn control-two-phase__restart" role="status">
                <div>
                  <p class="control-two-phase__restart-title">{{ t('control.deviceRestart.title') }}</p>
                  <p class="aux-text">{{ t('control.deviceRestart.desc') }}</p>
                </div>
                <el-button size="small" @click="goToRestart">{{ t('control.deviceRestart.goto') }}</el-button>
              </div>
            </template>
          </section>

          <section class="control-two-phase__col control-two-phase__col--history">
            <h3 class="control-two-phase__heading">{{ t('control.history.title') }}</h3>
            <p v-if="historyRows.length === 0" class="aux-text">{{ t('control.history.empty') }}</p>
            <ul v-else class="control-history">
              <li v-for="(entry, index) in historyRows" :key="index" class="control-history__item">
                <span class="control-history__time mono">{{ historyTime(entry.at) }}</span>
                <span class="control-history__action">{{ actionText(entry.action) }}</span>
                <span class="control-history__state" :class="entry.ok ? 'control-history__state--ok' : 'control-history__state--failed'">
                  {{ entry.ok ? t('control.history.ok') : t('control.history.failed') }}
                </span>
                <span v-if="entry.version !== null" class="control-history__meta mono">v{{ entry.version }}</span>
                <span v-if="entry.code !== null" class="control-history__meta mono">{{ entry.code }}</span>
              </li>
            </ul>
          </section>
        </div>
      </PanelCard>
    </div>

    <div v-else-if="statusState.phase === 'loading'" class="banner banner--info" aria-busy="true">
      {{ t('common.loading') }}
    </div>
  </div>
</template>

<style scoped>
.volume-page {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}

.volume-page__updated {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
  font-family: var(--khaen-font-mono);
  white-space: nowrap;
}

.volume-page__fatal {
  align-items: flex-start;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--khaen-space-3);
}

.volume-page__fatal-text {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
}

.volume-page__fatal-title {
  font-weight: 600;
}

.volume-grid__span-5 {
  grid-column: span 5;
}

.volume-grid__span-7 {
  grid-column: span 7;
}

.volume-grid__span-12 {
  grid-column: span 12;
}

.volume-mute {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
}

.volume-mute__state {
  display: flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.volume-mute__control {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  min-height: 44px;
}

.volume-mute__label {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.volume-master__reason {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.volume-gains__readback {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.volume-gains__toolbar {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.volume-gains__toolbar-label {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.volume-gains__uniform-input {
  width: 120px;
}

.volume-gains__count {
  margin-left: auto;
  font-family: var(--khaen-font-mono);
}

.volume-gains__list {
  display: flex;
  flex-direction: column;
  gap: 2px;
  max-height: 420px;
  overflow-y: auto;
  padding-right: var(--khaen-space-1);
}

.volume-gains__row {
  display: grid;
  grid-template-columns: 24px 64px minmax(120px, 1fr) 88px;
  align-items: center;
  gap: var(--khaen-space-2);
  min-height: 36px;
}

.volume-gains__lane {
  color: var(--khaen-text-secondary);
  font-size: 11px;
}

.volume-gains__slider {
  min-width: 0;
}

.volume-gains__value {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.volume-gains__value--included {
  color: var(--khaen-accent);
}

.volume-gains__invalid {
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
}

.volume-gains__actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.volume-gains__actions :deep(.el-button) {
  min-height: 36px;
}

.control-two-phase {
  display: grid;
  gap: var(--khaen-space-4);
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.control-two-phase__col {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  min-width: 0;
  padding-right: var(--khaen-space-3);
  border-right: 1px dashed var(--khaen-border);
}

.control-two-phase__col:last-child {
  padding-right: 0;
  border-right: 0;
}

.control-two-phase__heading {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  font-weight: 600;
}

.control-two-phase__actions {
  display: flex;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.control-two-phase__actions :deep(.el-button) {
  min-height: 36px;
}

.control-two-phase__restart {
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.control-two-phase__restart-title {
  font-weight: 600;
}

.control-two-phase__col--history {
  min-width: 0;
}

.control-history {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.control-history__item {
  display: flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  min-width: 0;
  padding-bottom: 4px;
  border-bottom: 1px dashed var(--khaen-border);
  font-size: var(--khaen-font-aux);
}

.control-history__time {
  color: var(--khaen-text-muted);
  font-size: 11px;
}

.control-history__action {
  color: var(--khaen-text);
}

.control-history__state--ok {
  color: var(--khaen-success);
}

.control-history__state--failed {
  color: var(--khaen-danger);
}

.control-history__meta {
  color: var(--khaen-text-secondary);
  font-size: 11px;
}

@media (max-width: 1279px) {
  .volume-grid__span-5,
  .volume-grid__span-7 {
    grid-column: span 12;
  }

  .control-two-phase {
    grid-template-columns: minmax(0, 1fr);
  }

  .control-two-phase__col {
    padding-right: 0;
    padding-bottom: var(--khaen-space-3);
    border-right: 0;
    border-bottom: 1px dashed var(--khaen-border);
  }

  .control-two-phase__col:last-child {
    padding-bottom: 0;
    border-bottom: 0;
  }
}

@media (max-width: 767px) {
  .volume-gains__row {
    grid-template-columns: 24px 56px minmax(80px, 1fr) 72px;
  }

  .volume-mute {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>