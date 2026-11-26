<script setup lang="ts">
/**
 * 方案与声道页（M4 / 任务 T54）——方案上传/历史/回滚 + 输出声道与前级映射。
 *
 * 数据来源（页面唯一事实源，缺失即「未接入」）：
 * - GET /api/status（5s 轮询）：plan.graph_id / version / latency_samples / applied_at / source；
 * - GET /api/channels：khaen-channels/v1（output.channels/dac_map、input.channels/map/source/sdp_sha256、
 *   mapping、reasons、not_wired）；
 * - PUT /api/channels（prepare）→ POST /api/commit（热重配提交；M3b/T52 未完成前返回 501 host_link_unsupported）；
 * - POST /api/plan/upload（客户端 sha256 + 服务端 sha 双侧比对）→ POST /api/plan/inspect {plan_path}
 *   （plan_path = 上传回执的 stored_path；失败降级为上传摘要）→ POST /api/prepare {plan_path, plan_sha256}
 *   → POST /api/commit / POST /api/abort；
 * - GET /api/plan/history（daemon 真实形态 current:{version},entries[]）→ POST /api/rollback {target}
 *   → 回滚草稿 → commit（版本号递增，历史不改写）。
 *
 * 诚实边界：
 * - 后端端点未实现/不可用时整块显示 NOT_AVAILABLE + 稳定码 + 原因 + 任务号（T55 / T52），不编造历史/映射；
 * - MOTU 物理输出只标注已知位置（1-2 L/R、3-4 SL/SR、5-6 TF/TFR、8 LFE），未知位置显示输出序号，不猜；
 * - 声道/样本率变更会中断出声：提交前必须确认；M3b 未完成时提交按钮仍可点，但后端 501 如实显示为
 *   「热重配未接入」，绝不显示成功；
 * - 上传 sha 两侧不一致即失败（不信任任何一侧）；历史/预览缺失时清空对应数据，不用旧数据冒充最新。
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage, ElMessageBox } from 'element-plus'
import { Refresh, Upload } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import StatRow from '@/components/StatRow.vue'
import StatusBadge from '@/components/StatusBadge.vue'
import TruthTag from '@/components/TruthTag.vue'
import ControlErrorNotice from '@/components/control/ControlErrorNotice.vue'
import { useStatusPolling } from '@/composables/useStatusPolling'
import { reasonForPath, runStateKind, runStateLabelKey } from '@/api/status'
import {
  MOTU_KNOWN_OUTPUT_NUMBERS,
  PLAN_UPLOAD_MAX_BYTES,
  formatChannelMap,
  motuOutputLabelKey,
  reasonForChannelsPath,
  reasonForPlanHistoryPath,
  validateChannelsPatch,
  type ChannelsPatch
} from '@/api/channels'
import {
  abortChannelLayout,
  abortChannelPlan,
  channelsState,
  commitChannelLayout,
  commitChannelPlan,
  loadChannels,
  loadChannelsHistory,
  prepareChannelLayout,
  prepareChannelPlan,
  prepareChannelRollback,
  refreshChannels,
  selectChannelPlanFile,
  uploadChannelPlan
} from '@/stores/channels'
import { formatClock, formatInteger, formatTimestamp, shortenId, toByteUnit } from '@/utils/format'
import type { StatusKind } from '@/types/ui'

const { t, locale } = useI18n({ useScope: 'global' })
const { state: statusState, refresh: refreshStatus } = useStatusPolling()

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const data = computed(() => statusState.data)
const channels = computed(() => channelsState.info)
const channelsPhase = computed(() => channelsState.phase)
const channelsError = computed(() => channelsState.error)
const refreshing = computed(() => channelsState.refreshing || statusState.refreshing)
const plan = computed(() => channelsState.plan)
const planError = computed(() => plan.value.error ?? plan.value.upload.error)
const planBusy = computed(() => plan.value.busy)
const history = computed(() => channelsState.history)
const historyError = computed(() => channelsState.historyError)

interface KvRow {
  label: string
  value: string | null
  copy?: string | null
  reason?: string | null
  tone?: 'ok' | 'warn' | 'bad' | null
}

/* ------------------------------------------------------------- 格式化工具 */

function intText(value: number | null | undefined): string | null {
  return formatInteger(value ?? null, localeId.value)
}

function timeText(iso: string | null): string | null {
  return formatTimestamp(iso, localeId.value)
}

function clockText(epochMs: number | null): string | null {
  return formatClock(epochMs, localeId.value)
}

function bytesText(bytes: number | null): string | null {
  const unit = toByteUnit(bytes)
  if (unit === null) {
    return null
  }
  const digits = unit.unitKey === 'unit.bytes' ? 0 : 2
  const value = (unit.value as number).toFixed(digits)
  return value + ' ' + t(unit.unitKey)
}

function boolText(value: boolean | null | undefined): string | null {
  if (value === null || value === undefined) {
    return null
  }
  return value ? t('common.yes') : t('common.no')
}

const lastUpdateText = computed(() => {
  if (channelsState.loadedAt === null) {
    return t('channels.updated.never')
  }
  const time = clockText(channelsState.loadedAt)
  return time === null ? t('channels.updated.never') : t('channels.updated.last', { time })
})

const runStateBadge = computed<{ kind: StatusKind; value: string | undefined }>(() => {
  if (data.value === null) {
    return { kind: 'unknown', value: undefined }
  }
  const key = runStateLabelKey(data.value.state)
  const value = key !== null ? t(key) : data.value.state.value ?? undefined
  return { kind: runStateKind(data.value.state), value }
})

const planVersionText = computed(() => {
  const version = data.value?.plan.version ?? null
  return version === null ? null : 'v' + (formatInteger(version, localeId.value) ?? String(version))
})

/* ------------------------------------------------------------ 顶部摘要带 */

const currentPlanRows = computed<KvRow[]>(() => {
  const statusPlan = data.value?.plan ?? null
  const current = history.value?.current_version ?? statusPlan?.version ?? null
  const entry =
    history.value?.versions.find((item) => item.version === current) ??
    (history.value !== null && history.value.versions.length > 0
      ? history.value.versions[history.value.versions.length - 1]
      : null)
  return [
    {
      label: t('channels.plan.field.graphId'),
      value: shortenId(statusPlan?.graph_id ?? entry?.graph_id ?? null, 10, 6),
      copy: statusPlan?.graph_id ?? entry?.graph_id ?? null,
      reason:
        statusPlan?.graph_id === null
          ? reasonForPath(data.value, 'plan.graph_id') ?? t('channels.plan.noPlan')
          : null
    },
    {
      label: t('channels.plan.field.sha256'),
      value: shortenId(entry?.sha256 ?? statusPlan?.sha256 ?? null, 12, 8),
      copy: entry?.sha256 ?? statusPlan?.sha256 ?? null,
      reason:
        (entry?.sha256 ?? statusPlan?.sha256) === null
          ? reasonForPath(data.value, 'plan.sha256') ?? t('channels.plan.shaUnavailable')
          : null
    },
    {
      label: t('channels.plan.field.version'),
      value: current === null ? null : formatInteger(current, localeId.value),
      reason:
        current === null
          ? reasonForPath(data.value, 'plan.version') ??
            reasonForPlanHistoryPath(history.value, 'history.current') ??
            t('channels.plan.noPlan')
          : null
    },
    {
      label: t('channels.plan.field.latency'),
      value: intText(statusPlan?.latency_samples ?? null),
      reason: reasonForPath(data.value, 'plan.latency_samples')
    },
    {
      label: t('channels.plan.field.appliedAt'),
      value: timeText(entry?.applied_at ?? statusPlan?.applied_at ?? null),
      reason: reasonForPath(data.value, 'plan.applied_at')
    },
    {
      label: t('channels.plan.field.source'),
      value: entry?.source ?? statusPlan?.source ?? null,
      reason: reasonForPath(data.value, 'plan.source')
    }
  ]
})

const historyTruth = computed(() => (history.value !== null && history.value.versions.length > 0 ? 'actual' : 'notWired'))

/* -------------------------------------------------------------- 输出通道 */

const outputRows = computed(() => {
  const info = channels.value
  if (info === null || info.mapping === null) {
    // mapping=null = 当前方案 routing 段未知（不是「已知 0 条路由」）：不画 lane → DAC 表。
    return []
  }
  return info.mapping.map((entry) => {
    const physical = entry.dac + 1
    const labelKey = motuOutputLabelKey(physical)
    return {
      lane: entry.lane,
      dac: entry.dac,
      physical,
      label: labelKey === null ? t('channels.output.unknownLabel', { channel: physical }) : t(labelKey)
    }
  })
})

/** /api/channels 上报了 mapping:null（daemon 关联不到当前方案 routing 段）：显示 NOT_AVAILABLE + 原因。 */
const mappingUnknown = computed(() => channels.value !== null && channels.value.mapping === null)
const mappingReason = computed(() =>
  channels.value === null ? null : reasonForChannelsPath(channels.value, 'mapping')
)

const outputCount = computed(() => channels.value?.output.channels ?? null)
/** dac_map 的 null 项 = 该 DAC 没有 lane 喂入（页面显示 "-"，不补 0）。 */
const dacMapText = computed(() => formatChannelMap(channels.value?.output.dac_map ?? null))
const knownLabelsText = computed(() =>
  MOTU_KNOWN_OUTPUT_NUMBERS.map((channel) => channel + ' ' + t(motuOutputLabelKey(channel) ?? 'channels.output.unknownLabel'))
    .join(' / ')
)

/* -------------------------------------------------------------- 输入 AES67 */

const inputRows = computed<KvRow[]>(() => {
  const info = channels.value
  if (info === null) {
    return []
  }
  const input = info.input
  return [
    {
      label: t('channels.input.field.channels'),
      value: intText(input.channels),
      reason: reasonForChannelsPath(info, 'input.channels')
    },
    {
      label: t('channels.input.field.map'),
      value: formatChannelMap(input.map),
      reason: reasonForChannelsPath(info, 'input.map')
    },
    {
      label: t('channels.input.field.source'),
      value: input.source === null ? null : input.source === 'sdp' ? t('channels.input.source.sdp') : t('channels.input.source.manual'),
      reason: reasonForChannelsPath(info, 'input.source')
    },
    {
      label: t('channels.input.field.sdpSha'),
      value: shortenId(input.sdp_sha256, 12, 8),
      copy: input.sdp_sha256,
      reason: reasonForChannelsPath(info, 'input.sdp_sha256')
    }
  ]
})

/* --------------------------------------------------------- 文件选择 / 上传 */

const fileInput = ref<HTMLInputElement | null>(null)
const localError = ref<string | null>(null)

function triggerFilePicker(): void {
  localError.value = null
  fileInput.value?.click()
}

async function onFileChange(event: Event): Promise<void> {
  const target = event.target as HTMLInputElement | null
  const file = target?.files?.[0] ?? null
  if (target !== null) {
    target.value = ''
  }
  if (file === null) {
    return
  }
  localError.value = null
  if (file.size > PLAN_UPLOAD_MAX_BYTES) {
    localError.value = t('channels.upload.tooLarge', { limit: bytesText(PLAN_UPLOAD_MAX_BYTES) })
    return
  }
  const bytes = new Uint8Array(await file.arrayBuffer())
  await selectChannelPlanFile(file.name, bytes)
  if (channelsState.plan.upload.error !== null) {
    localError.value = t(channelsState.plan.upload.error.i18nKey)
  }
}

const upload = computed(() => plan.value.upload)
const uploadAck = computed(() => upload.value.ack)

const uploadRows = computed<KvRow[]>(() => {
  const ack = uploadAck.value
  if (ack === null) {
    return []
  }
  const segmentNames = ack.segments === null ? null : ack.segments.join(', ')
  return [
    {
      label: t('channels.upload.field.sha256'),
      value: shortenId(ack.sha256, 12, 8),
      copy: ack.sha256
    },
    {
      label: t('channels.upload.field.graphId'),
      value: shortenId(ack.graph_id, 10, 6),
      copy: ack.graph_id
    },
    {
      label: t('channels.upload.field.version'),
      value: intText(ack.version),
      // 真实 daemon 的上传回执不含目标版本：如实 NOT_AVAILABLE，不拿别的数字冒充。
      reason: ack.version === null ? t('channels.upload.versionUnavailable') : null
    },
    {
      label: t('channels.upload.field.segments'),
      value: segmentNames,
      copy: segmentNames,
      reason:
        segmentNames === null && ack.segment_count !== null ? t('channels.upload.segmentsUnavailable') : null
    },
    { label: t('channels.upload.field.segmentCount'), value: intText(ack.segment_count) },
    { label: t('channels.upload.field.latency'), value: intText(ack.latency_samples) }
  ]
})

const inspectRows = computed<KvRow[]>(() => {
  const inspect = plan.value.inspect
  if (inspect === null) {
    return []
  }
  return [
    { label: t('channels.inspect.field.sampleRate'), value: intText(inspect.sample_rate) },
    { label: t('channels.inspect.field.blockSize'), value: intText(inspect.block_size) },
    { label: t('channels.inspect.field.inChannels'), value: intText(inspect.in_channels) },
    { label: t('channels.inspect.field.lanes'), value: intText(inspect.lanes) },
    { label: t('channels.inspect.field.dacChannels'), value: intText(inspect.dac_channels) },
    { label: t('channels.inspect.field.latency'), value: intText(inspect.latency_samples) },
    { label: t('channels.inspect.field.compiled'), value: boolText(inspect.compiled) },
    {
      label: t('channels.inspect.field.admission'),
      value: inspect.admission_accepted === null ? null : inspect.admission_accepted ? t('channels.inspect.accepted') : t('channels.inspect.rejected'),
      tone: inspect.admission_accepted === false ? ('bad' as const) : null
    }
  ]
})

const inspectUnavailable = computed(() => plan.value.inspectError !== null)

/* ------------------------------------------------------- 方案两阶段（prepare/commit） */

const planDraft = computed(() => plan.value.draft)
const planCommit = computed(() => plan.value.lastCommit)

function surfaceText(surface: string | null): string | null {
  if (surface === null) {
    return null
  }
  const key = 'control.surface.' + surface
  const text = t(key)
  return text === key ? surface : text
}

const draftRows = computed<KvRow[]>(() => {
  const draft = planDraft.value
  if (draft === null) {
    return []
  }
  const rows: KvRow[] = [
    { label: t('control.receipt.surface'), value: surfaceText(draft.surface) },
    { label: t('control.receipt.engineToken'), value: shortenId(draft.engine_token, 8, 4), copy: draft.engine_token },
    { label: t('control.receipt.plannedVersion'), value: intText(draft.planned_version) }
  ]
  if (draft.graph_id !== null) {
    rows.push({ label: t('control.receipt.graphId'), value: shortenId(draft.graph_id, 10, 6), copy: draft.graph_id })
  }
  if (draft.rolled_back_to !== null) {
    rows.push({ label: t('control.receipt.rolledBackTo'), value: intText(draft.rolled_back_to) })
  }
  return rows
})

const commitRows = computed<KvRow[]>(() => {
  const ack = planCommit.value
  if (ack === null) {
    return []
  }
  const rows: KvRow[] = [
    { label: t('control.receipt.version'), value: intText(ack.version) },
    { label: t('control.receipt.appliedBlock'), value: intText(ack.applied_block) },
    { label: t('control.receipt.allAck'), value: boolText(ack.all_ack) }
  ]
  if (ack.graph_id !== null) {
    rows.push({ label: t('control.receipt.graphId'), value: shortenId(ack.graph_id, 10, 6), copy: ack.graph_id })
  }
  return rows
})

const planDraftHint = computed(() => {
  const draft = planDraft.value
  if (draft === null) {
    return null
  }
  if (draft.surface === 'rollback') {
    return t('channels.draft.rollbackHint', { version: draft.rolled_back_to ?? t('common.dash') })
  }
  return t('channels.draft.planHint')
})

async function onSelectAndUpload(): Promise<void> {
  if (planBusy.value !== null) {
    return
  }
  const ack = await uploadChannelPlan()
  if (ack !== null) {
    ElMessage.success(t('channels.upload.uploaded'))
  }
}

async function onPreparePlan(): Promise<void> {
  if (uploadAck.value === null || planBusy.value !== null) {
    return
  }
  const draft = await prepareChannelPlan()
  if (draft !== null) {
    ElMessage.success(t('channels.plan.prepared'))
  }
}

async function onCommitPlan(): Promise<void> {
  if (planDraft.value === null || planBusy.value !== null) {
    return
  }
  const draft = planDraft.value
  const isRollback = draft.surface === 'rollback'
  try {
    await ElMessageBox.confirm(
      isRollback
        ? t('channels.draft.confirmRollbackMessage', { version: draft.rolled_back_to ?? t('common.dash') })
        : t('channels.draft.confirmCommitMessage', { version: draft.planned_version ?? t('common.dash') }),
      isRollback ? t('channels.draft.confirmRollbackTitle') : t('channels.draft.confirmCommitTitle'),
      { confirmButtonText: t('common.confirm'), cancelButtonText: t('common.cancel'), type: 'warning' }
    )
  } catch {
    return
  }
  const ack = await commitChannelPlan()
  if (ack !== null) {
    ElMessage.success(t('channels.plan.committed'))
    await refreshStatus()
  }
}

async function onAbortPlan(): Promise<void> {
  const ok = await abortChannelPlan()
  if (ok) {
    ElMessage.success(t('control.toast.aborted'))
  }
}

/* ---------------------------------------------------------- 历史与回滚 */

const historyRows = computed(() =>
  (history.value?.versions ?? []).map((entry) => ({
    version: entry.version,
    graph_id: entry.graph_id,
    sha256: entry.sha256,
    applied_at: entry.applied_at,
    source: entry.source
  }))
)

const currentVersion = computed(() => history.value?.current_version ?? null)

/** 历史为空且 daemon 上报了「没有数据源」原因（history.current / history.available_versions / history）：
 *  显示 NOT_AVAILABLE + 原因，而不是「尚无历史」。 */
const historyNoSourceReason = computed(() => {
  const snapshot = history.value
  if (snapshot === null || snapshot.versions.length > 0) {
    return null
  }
  return (
    reasonForPlanHistoryPath(snapshot, 'history.current') ??
    reasonForPlanHistoryPath(snapshot, 'history.available_versions') ??
    reasonForPlanHistoryPath(snapshot, 'history')
  )
})

async function onRollback(target: number): Promise<void> {
  if (planBusy.value !== null) {
    return
  }
  try {
    await ElMessageBox.confirm(
      t('channels.history.confirmRollback.message', { version: target }),
      t('channels.history.confirmRollback.title'),
      { confirmButtonText: t('channels.history.confirmRollback.ok'), cancelButtonText: t('common.cancel'), type: 'warning' }
    )
  } catch {
    return
  }
  const draft = await prepareChannelRollback(target)
  if (draft !== null) {
    ElMessage.success(t('channels.history.rollbackPrepared', { version: target }))
  }
}

/* ------------------------------------------------------- 声道热重配（M3b） */

const outputDraft = ref<number | null>(null)
const inputDraft = ref<number | null>(null)

watch(
  () => channels.value,
  (info) => {
    if (info === null) {
      outputDraft.value = null
      inputDraft.value = null
      return
    }
    if (outputDraft.value === null && info.output.channels !== null) {
      outputDraft.value = info.output.channels
    }
    if (inputDraft.value === null && info.input.channels !== null) {
      inputDraft.value = info.input.channels
    }
  },
  { immediate: true }
)

const channelsPatch = computed<ChannelsPatch>(() => {
  const patch: ChannelsPatch = {}
  const output = outputDraft.value
  if (output !== null && output !== (channels.value?.output.channels ?? null)) {
    const previous = channels.value?.output.dac_map ?? null
    // daemon 的 dac_map 允许 null 项（该 DAC 静音/没有 lane 喂入）：T54 的补丁契约只接受
    // number[]（每个输出通道的 DAC 序号），null 项无法原样复用 -> 退回按新通道数的 identity 草稿
    // （只是草稿，仍需 prepare 校验 + commit；提交在 M3b/T52 前恒 501，不假装生效）。
    const reusable =
      previous !== null &&
      previous.length === output &&
      previous.every((value): value is number => value !== null)
    if (reusable && previous !== null) {
      patch.output = { channels: output, dac_map: previous }
    } else {
      const dacMap: number[] = []
      for (let index = 0; index < output; index += 1) {
        dacMap.push(index)
      }
      patch.output = { channels: output, dac_map: dacMap }
    }
  }
  const input = inputDraft.value
  if (input !== null && input !== (channels.value?.input.channels ?? null)) {
    const previous = channels.value?.input.map ?? null
    if (previous !== null && previous.length === input) {
      patch.input = { channels: input, map: previous }
    } else {
      const map: number[] = []
      for (let index = 0; index < input; index += 1) {
        map.push(index)
      }
      patch.input = { channels: input, map }
    }
  }
  return patch
})

const patchValidation = computed(() =>
  Object.keys(channelsPatch.value).length === 0
    ? 'empty'
    : validateChannelsPatch(channelsPatch.value, channels.value?.output.channels ?? null)
)

const channelsDraft = computed(() => channelsState.channelsDraft)
const channelsCommitAck = computed(() => channelsState.channelsCommit)
const channelsWriteBusy = computed(() => channelsState.channelsBusy)
const channelsWriteError = computed(() => channelsState.channelsError)

const canPrepareChannels = computed(
  () =>
    channelsWriteBusy.value === null &&
    channels.value !== null &&
    patchValidation.value === null
)

/** M3b 热重配未完成：commit 后端会返回 501 host_link_unsupported；UI 明示、不假装成功。 */
const hotSwapNotAvailableReason = computed(() => t('channels.hotswap.notAvailableReason'))

async function onPrepareChannels(): Promise<void> {
  if (!canPrepareChannels.value) {
    return
  }
  const ack = await prepareChannelLayout(channelsPatch.value)
  if (ack !== null) {
    ElMessage.success(t('channels.hotswap.prepared'))
  }
}

async function onCommitChannels(): Promise<void> {
  if (channelsDraft.value === null || channelsWriteBusy.value !== null) {
    return
  }
  try {
    await ElMessageBox.confirm(
      t('channels.hotswap.confirmMessage'),
      t('channels.hotswap.confirmTitle'),
      { confirmButtonText: t('channels.hotswap.confirmOk'), cancelButtonText: t('common.cancel'), type: 'warning' }
    )
  } catch {
    return
  }
  const ack = await commitChannelLayout()
  if (ack !== null) {
    ElMessage.success(t('channels.hotswap.committed'))
    await refreshStatus()
  }
}

async function onAbortChannels(): Promise<void> {
  const ok = await abortChannelLayout()
  if (ok) {
    ElMessage.success(t('control.toast.aborted'))
  }
}

const channelsPreviewRows = computed<KvRow[]>(() => {
  const ack = channelsDraft.value
  if (ack === null || ack.preview === null) {
    return []
  }
  const preview = ack.preview
  return [
    {
      label: t('channels.hotswap.field.outputChannels'),
      value: t('channels.hotswap.previewChange', {
        from: intText(preview.old_channels) ?? t('common.dash'),
        to: intText(preview.new_channels) ?? t('common.dash')
      })
    },
    {
      label: t('channels.hotswap.field.inputChannels'),
      value: t('channels.hotswap.previewChange', {
        from: intText(preview.old_input_channels) ?? t('common.dash'),
        to: intText(preview.new_input_channels) ?? t('common.dash')
      })
    },
    {
      label: t('channels.hotswap.field.audioGap'),
      value:
        preview.audio_gap_ms_estimate === null
          ? null
          : t('channels.hotswap.audioGapValue', { ms: intText(preview.audio_gap_ms_estimate) ?? t('common.dash') })
    }
  ]
})

/* ------------------------------------------------------------------ 告警 */

const fatalChannels = computed(() => channels.value === null && channelsPhase.value === 'error' && channelsError.value !== null)
const notWiredEntries = computed(() => channels.value?.not_wired ?? [])

interface NoticeBanner {
  tone: 'warn' | 'danger' | 'info'
  text: string
}

const notices = computed<NoticeBanner[]>(() => {
  const banners: NoticeBanner[] = []
  if (channelsError.value !== null && channels.value !== null) {
    banners.push({ tone: 'warn', text: t('channels.notice.refreshFailed', { message: t(channelsError.value.i18nKey) }) })
  }
  if (statusState.error !== null) {
    const time = statusState.loadedAt === null ? t('common.dash') : clockText(statusState.loadedAt) ?? t('common.dash')
    banners.push({ tone: 'warn', text: t('channels.notice.statusRefreshFailed', { message: t(statusState.error.i18nKey), time }) })
  }
  if (historyError.value !== null) {
    banners.push({ tone: 'info', text: t('channels.notice.historyUnavailable', { message: t(historyError.value.i18nKey) }) })
  }
  if (inspectUnavailable.value) {
    banners.push({ tone: 'info', text: t('channels.notice.inspectUnavailable') })
  }
  return banners
})

function onRefresh(): void {
  void refreshChannels()
  void loadChannelsHistory()
  void refreshStatus()
}

void loadChannels()
void loadChannelsHistory()
</script>

<template>
  <div class="channels-page">
    <PageHeader title-key="channels.pageTitle" subtitle-key="channels.subtitle">
      <template #actions>
        <span class="channels-page__updated">{{ lastUpdateText }}</span>
        <el-button
          type="primary"
          plain
          :loading="refreshing"
          :disabled="refreshing"
          :aria-label="t('channels.actions.refresh')"
          @click="onRefresh"
        >
          <el-icon v-if="!refreshing"><Refresh /></el-icon>
          <span>{{ t('channels.actions.refresh') }}</span>
        </el-button>
      </template>
    </PageHeader>

    <!-- 声道后端未接入：整块 NOT_AVAILABLE + 稳定码 + 原因 + 任务号（T55） -->
    <div v-if="fatalChannels" class="banner banner--warn channels-page__not-wired" role="status">
      <div class="channels-page__not-wired-text">
        <span class="channels-page__not-wired-title">{{ t('channels.unavailable.title') }}</span>
        <span>{{ t('channels.unavailable.desc', { task: 'T55', message: t(channelsError!.i18nKey) }) }}</span>
        <span v-if="channelsError!.detail" class="aux-text mono">{{ channelsError!.code }} — {{ channelsError!.detail }}</span>
      </div>
      <el-button size="small" @click="onRefresh">{{ t('common.retry') }}</el-button>
    </div>

    <ControlErrorNotice :error="channelsWriteError" @retry="onRefresh" />
    <ControlErrorNotice :error="planError" @retry="onRefresh" />

    <p v-for="banner in notices" :key="banner.text" class="banner" :class="'banner--' + banner.tone" role="status">
      {{ banner.text }}
    </p>

    <div v-if="data" class="status-band">
      <StatusBadge :kind="runStateBadge.kind" :label="t('channels.band.runState')" :value="runStateBadge.value" />
      <span class="status-band__divider" aria-hidden="true" />
      <StatusBadge
        :kind="planVersionText === null ? 'unknown' : 'safe'"
        :label="t('channels.band.planVersion')"
        :value="planVersionText ?? undefined"
      />
      <span class="status-band__divider" aria-hidden="true" />
      <span class="channels-page__band-item">
        <span class="channels-page__band-label">{{ t('channels.band.outputChannels') }}</span>
        <span class="mono">{{ intText(outputCount) ?? t('common.dash') }}</span>
      </span>
      <span class="status-band__divider" aria-hidden="true" />
      <span class="channels-page__band-item">
        <span class="channels-page__band-label">{{ t('channels.band.inputChannels') }}</span>
        <span class="mono">{{ intText(channels?.input.channels ?? null) ?? t('common.dash') }}</span>
      </span>
      <span class="status-band__meta">{{ lastUpdateText }}</span>
    </div>

    <div v-if="data" class="grid-12 channels-grid">
      <!-- 当前方案摘要 -->
      <PanelCard class="channels-grid__span-12" title-key="channels.plan.title" desc-key="channels.plan.desc" :truth="currentPlanRows.some((row) => row.value !== null) ? 'actual' : 'notWired'">
        <ul class="channels-plan-band">
          <li v-for="row in currentPlanRows" :key="row.label" class="channels-plan-band__item">
            <span class="channels-plan-band__label">{{ row.label }}</span>
            <span class="channels-plan-band__value mono" :class="{ 'channels-plan-band__value--muted': row.value === null }">
              {{ row.value ?? t('common.dash') }}
            </span>
            <TruthTag v-if="row.value === null" kind="notWired" :reason="row.reason ?? null" />
          </li>
        </ul>
        <p v-if="historyError" class="aux-text">{{ t('channels.plan.historyUnavailableHint') }}</p>
      </PanelCard>

      <!-- 输出通道与 MOTU 物理映射 -->
      <PanelCard
        class="channels-grid__span-7"
        title-key="channels.output.title"
        desc-key="channels.output.desc"
        :truth="outputRows.length > 0 ? 'actual' : 'notWired'"
      >
        <ul class="kv-list">
          <StatRow
            :label="t('channels.output.field.channels')"
            :value="intText(outputCount)"
            :reason="channels === null ? t('channels.unavailable.reason') : reasonForChannelsPath(channels, 'output.channels')"
          />
          <StatRow
            :label="t('channels.output.field.dacMap')"
            :value="dacMapText"
            :copy="dacMapText"
            :reason="channels === null ? t('channels.unavailable.reason') : reasonForChannelsPath(channels, 'output.dac_map')"
          />
          <StatRow :label="t('channels.output.field.knownLabels')" :value="knownLabelsText" :reason="null" />
        </ul>
        <div v-if="outputRows.length > 0" class="channels-output__table-wrap">
          <table class="channels-table">
            <thead>
              <tr>
                <th>{{ t('channels.output.table.lane') }}</th>
                <th>{{ t('channels.output.table.physical') }}</th>
                <th>{{ t('channels.output.table.label') }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="row in outputRows" :key="row.lane">
                <td class="mono">{{ row.lane }}</td>
                <td class="mono">{{ row.physical }}</td>
                <td>{{ row.label }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-else-if="mappingUnknown" class="aux-text">
          <TruthTag kind="notWired" :reason="mappingReason" />
          {{ t('channels.output.mappingUnknown') }}
        </p>
        <p v-else class="aux-text">{{ t('channels.output.empty') }}</p>
      </PanelCard>

      <!-- AES67 输入 -->
      <PanelCard
        class="channels-grid__span-5"
        title-key="channels.input.title"
        desc-key="channels.input.desc"
        :truth="inputRows.some((row) => row.value !== null) ? 'actual' : 'notWired'"
      >
        <ul class="kv-list">
          <StatRow
            v-for="row in inputRows"
            :key="row.label"
            :label="row.label"
            :value="row.value"
            :copy="row.copy ?? null"
            :reason="row.reason ?? (channels === null ? t('channels.unavailable.reason') : null)"
          />
        </ul>
        <p class="aux-text">{{ t('channels.input.note') }}</p>
        <ul v-if="notWiredEntries.length > 0" class="channels-not-wired">
          <li v-for="entry in notWiredEntries" :key="entry.path" class="channels-not-wired__item">
            <span class="mono">{{ entry.path }}</span>
            <span class="aux-text">{{ entry.reason }}</span>
          </li>
        </ul>
      </PanelCard>

      <!-- 方案上传 -->
      <PanelCard
        class="channels-grid__span-7"
        title-key="channels.upload.title"
        desc-key="channels.upload.desc"
        :truth="uploadAck !== null ? 'actual' : 'notWired'"
      >
        <input
          ref="fileInput"
          type="file"
          class="channels-upload__hidden"
          :accept="'.bin,.khaenplan,.plan,application/octet-stream'"
          @change="onFileChange"
        />
        <div class="channels-upload__actions">
          <el-button :disabled="planBusy !== null" @click="triggerFilePicker">
            <el-icon><Upload /></el-icon>
            <span>{{ t('channels.upload.select') }}</span>
          </el-button>
          <span v-if="upload.name !== null" class="channels-upload__file mono">
            {{ upload.name }} · {{ bytesText(upload.byteLength) }}
          </span>
          <span v-else class="aux-text">{{ t('channels.upload.noFile') }}</span>
        </div>
        <p v-if="upload.sha256 !== null" class="aux-text channels-upload__sha">
          {{ t('channels.upload.clientSha') }}: <span class="mono">{{ upload.sha256 }}</span>
        </p>
        <p v-if="localError !== null" class="channels-upload__invalid" role="alert">{{ localError }}</p>
        <div class="channels-upload__actions">
          <el-button
            type="primary"
            plain
            :loading="planBusy === 'upload'"
            :disabled="upload.sha256 === null || planBusy !== null"
            @click="onSelectAndUpload"
          >
            {{ t('channels.upload.upload') }}
          </el-button>
          <el-button
            type="primary"
            :loading="planBusy === 'prepare'"
            :disabled="uploadAck === null || planBusy !== null"
            @click="onPreparePlan"
          >
            {{ t('channels.upload.prepare') }}
          </el-button>
        </div>

        <template v-if="uploadAck !== null">
          <h3 class="channels-subheading">{{ t('channels.upload.ackTitle') }}</h3>
          <ul class="kv-list">
            <StatRow
              v-for="row in uploadRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :copy="row.copy ?? null"
              :reason="row.reason ?? null"
            />
          </ul>
        </template>

        <template v-if="plan.inspect !== null">
          <h3 class="channels-subheading">{{ t('channels.inspect.title') }}</h3>
          <ul class="kv-list">
            <StatRow
              v-for="row in inspectRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :reason="null"
              :tone="row.tone ?? null"
            />
          </ul>
        </template>
        <p v-else-if="inspectUnavailable" class="aux-text">
          <TruthTag kind="notWired" :reason="plan.inspectError?.detail ?? null" />
          {{ t('channels.inspect.unavailable') }}
        </p>
      </PanelCard>

      <!-- 历史与回滚 -->
      <PanelCard
        class="channels-grid__span-5"
        title-key="channels.history.title"
        desc-key="channels.history.desc"
        :truth="historyTruth"
      >
        <p v-if="historyError !== null" class="aux-text channels-history__unavailable">
          <TruthTag kind="notWired" :reason="historyError.detail ?? null" />
          {{ t('channels.history.unavailable', { message: t(historyError.i18nKey), task: 'T55' }) }}
        </p>
        <div v-else-if="historyRows.length === 0" class="aux-text channels-history__unavailable">
          <template v-if="historyNoSourceReason !== null">
            <TruthTag kind="notWired" :reason="historyNoSourceReason" />
            {{ t('channels.history.noSource') }}
          </template>
          <template v-else>{{ t('channels.history.empty') }}</template>
        </div>
        <div v-else class="channels-history__table-wrap">
          <table class="channels-table">
            <thead>
              <tr>
                <th>{{ t('channels.history.table.version') }}</th>
                <th>{{ t('channels.history.table.graphId') }}</th>
                <th>{{ t('channels.history.table.appliedAt') }}</th>
                <th>{{ t('channels.history.table.source') }}</th>
                <th>{{ t('channels.history.table.actions') }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="row in historyRows" :key="row.version" :class="{ 'channels-history__row--current': row.version === currentVersion }">
                <td class="mono">
                  v{{ row.version }}
                  <span v-if="row.version === currentVersion" class="channels-history__current">{{ t('channels.history.current') }}</span>
                </td>
                <td class="mono" :title="row.graph_id ?? ''">{{ shortenId(row.graph_id, 8, 4) ?? t('common.dash') }}</td>
                <td class="mono">{{ timeText(row.applied_at) ?? t('common.dash') }}</td>
                <td>{{ row.source ?? t('common.dash') }}</td>
                <td>
                  <el-button
                    size="small"
                    :disabled="row.version === currentVersion || planBusy !== null"
                    @click="onRollback(row.version)"
                  >
                    {{ t('channels.history.rollback') }}
                  </el-button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <p class="aux-text">{{ t('channels.history.rollbackNote') }}</p>
      </PanelCard>

      <!-- 声道热重配（M3b/T52 未完成：commit 501 如实显示） -->
      <PanelCard
        class="channels-grid__span-7"
        title-key="channels.hotswap.title"
        desc-key="channels.hotswap.desc"
        :truth="channels === null ? 'notWired' : 'actual'"
      >
        <div class="banner banner--warn channels-hotswap__warning" role="status">
          <div class="channels-hotswap__warning-text">
            <strong>{{ t('channels.hotswap.warningTitle') }}</strong>
            <span>{{ t('channels.hotswap.warningDesc') }}</span>
          </div>
        </div>
        <div class="channels-hotswap__form">
          <label class="channels-hotswap__field">
            <span>{{ t('channels.hotswap.field.outputChannels') }}</span>
            <el-input-number
              v-model="outputDraft"
              :min="1"
              :max="32"
              :disabled="channels === null || channelsWriteBusy !== null"
              size="small"
            />
          </label>
          <label class="channels-hotswap__field">
            <span>{{ t('channels.hotswap.field.inputChannels') }}</span>
            <el-input-number
              v-model="inputDraft"
              :min="1"
              :max="8"
              :disabled="channels === null || channelsWriteBusy !== null"
              size="small"
            />
          </label>
        </div>
        <p v-if="patchValidation !== null && patchValidation !== 'empty'" class="channels-upload__invalid" role="alert">
          {{ t('channels.hotswap.validation.' + patchValidation) }}
        </p>
        <div class="channels-hotswap__actions">
          <el-button
            type="primary"
            plain
            :loading="channelsWriteBusy === 'prepare'"
            :disabled="!canPrepareChannels"
            @click="onPrepareChannels"
          >
            {{ t('channels.hotswap.prepare') }}
          </el-button>
          <el-button
            type="primary"
            :loading="channelsWriteBusy === 'commit'"
            :disabled="channelsDraft === null || channelsWriteBusy !== null"
            @click="onCommitChannels"
          >
            {{ t('channels.hotswap.commit') }}
          </el-button>
          <el-button
            :loading="channelsWriteBusy === 'abort'"
            :disabled="channelsDraft === null || channelsWriteBusy !== null"
            @click="onAbortChannels"
          >
            {{ t('control.actions.abort') }}
          </el-button>
        </div>
        <p class="channels-hotswap__not-available">
          <TruthTag kind="notWired" :reason="hotSwapNotAvailableReason" />
          <span>{{ t('channels.hotswap.notAvailable', { task: 'T52 / T55' }) }}</span>
        </p>
        <template v-if="channelsDraft !== null">
          <h3 class="channels-subheading">{{ t('channels.hotswap.previewTitle') }}</h3>
          <ul class="kv-list">
            <StatRow
              v-for="row in channelsPreviewRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :reason="null"
            />
          </ul>
          <ul v-if="channelsDraft.warnings.length > 0" class="channels-hotswap__list">
            <li v-for="warning in channelsDraft.warnings" :key="warning" class="aux-text">{{ warning }}</li>
          </ul>
          <ul v-if="channelsDraft.blockers.length > 0" class="channels-hotswap__list channels-hotswap__list--blockers">
            <li v-for="blocker in channelsDraft.blockers" :key="blocker">{{ blocker }}</li>
          </ul>
        </template>
        <p v-if="channelsCommitAck !== null" class="aux-text">
          {{ t('channels.hotswap.committedAt', { time: clockText(plan.lastCommitAt) ?? t('common.dash') }) }}
        </p>
      </PanelCard>

      <!-- 方案两阶段草稿 / 回执（上传方案与历史回滚共用） -->
      <PanelCard
        class="channels-grid__span-5"
        title-key="control.receipt.title"
        desc-key="channels.draft.desc"
        :truth="planDraft !== null || planCommit !== null ? 'actual' : 'notWired'"
      >
        <template v-if="planDraft !== null">
          <h3 class="channels-subheading">{{ t('control.receipt.draftTitle') }}</h3>
          <p class="aux-text">{{ planDraftHint }}</p>
          <ul class="kv-list">
            <StatRow
              v-for="row in draftRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :copy="row.copy ?? null"
              :reason="null"
            />
          </ul>
          <div class="channels-hotswap__actions">
            <el-button
              type="primary"
              :loading="planBusy === 'commit'"
              :disabled="planBusy !== null"
              @click="onCommitPlan"
            >
              {{ planBusy === 'commit' ? t('control.actions.committing') : t('control.actions.commit') }}
            </el-button>
            <el-button :loading="planBusy === 'abort'" :disabled="planBusy !== null" @click="onAbortPlan">
              {{ planBusy === 'abort' ? t('control.actions.aborting') : t('control.actions.abort') }}
            </el-button>
          </div>
          <p class="aux-text">{{ t('control.receipt.commitHint') }}</p>
        </template>
        <p v-else class="aux-text">{{ t('channels.draft.empty') }}</p>

        <template v-if="planCommit !== null">
          <h3 class="channels-subheading">{{ t('control.receipt.commitTitle') }}</h3>
          <ul class="kv-list">
            <StatRow
              v-for="row in commitRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :copy="row.copy ?? null"
              :reason="null"
            />
            <StatRow :label="t('control.receipt.at')" :value="clockText(plan.lastCommitAt)" :reason="null" />
          </ul>
        </template>
      </PanelCard>
    </div>

    <div v-else-if="statusState.phase === 'loading'" class="banner banner--info" aria-busy="true">
      {{ t('common.loading') }}
    </div>
  </div>
</template>

<style scoped>
.channels-page {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}

.channels-page__updated {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
  font-family: var(--khaen-font-mono);
  white-space: nowrap;
}

.channels-page__not-wired {
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
}

.channels-page__not-wired-text {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
  overflow-wrap: anywhere;
}

.channels-page__not-wired-title {
  font-weight: 600;
}

.channels-page__band-item {
  display: inline-flex;
  align-items: baseline;
  gap: 6px;
}

.channels-page__band-label {
  color: var(--khaen-text-secondary);
  font-size: 11px;
}

.channels-grid__span-7 {
  grid-column: span 7;
}

.channels-grid__span-5 {
  grid-column: span 5;
}

.channels-grid__span-12 {
  grid-column: span 12;
}

.channels-plan-band {
  display: grid;
  grid-template-columns: repeat(6, minmax(0, 1fr));
  gap: var(--khaen-space-2);
  margin: 0;
  padding: 0;
  list-style: none;
}

.channels-plan-band__item {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.channels-plan-band__label {
  color: var(--khaen-text-muted);
  font-size: 11px;
}

.channels-plan-band__value {
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.channels-plan-band__value--muted {
  color: var(--khaen-text-muted);
}

.channels-subheading {
  margin-top: var(--khaen-space-2);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  font-weight: 600;
}

.channels-table {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--khaen-font-aux);
}

.channels-table th {
  position: sticky;
  top: 0;
  padding: 6px 8px;
  border-bottom: 1px solid var(--khaen-border-strong);
  background: var(--khaen-panel);
  color: var(--khaen-text-secondary);
  font-weight: 600;
  text-align: left;
  white-space: nowrap;
}

.channels-table td {
  padding: 6px 8px;
  border-bottom: 1px dashed var(--khaen-border);
  overflow-wrap: anywhere;
}

.channels-output__table-wrap {
  max-height: 420px;
  overflow: auto;
}

.channels-history__table-wrap {
  max-height: 360px;
  overflow: auto;
}

.channels-history__row--current {
  background: var(--khaen-accent-soft);
}

.channels-history__current {
  margin-left: 4px;
  color: var(--khaen-accent);
  font-size: 11px;
}

.channels-history__unavailable {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.channels-not-wired {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.channels-not-wired__item {
  display: flex;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  font-size: 11px;
  overflow-wrap: anywhere;
}

.channels-upload__hidden {
  display: none;
}

.channels-upload__actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.channels-upload__file {
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.channels-upload__sha {
  overflow-wrap: anywhere;
}

.channels-upload__invalid {
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
}

.channels-hotswap__warning {
  align-items: flex-start;
}

.channels-hotswap__warning-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.channels-hotswap__form {
  display: flex;
  gap: var(--khaen-space-4);
  flex-wrap: wrap;
}

.channels-hotswap__field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.channels-hotswap__actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.channels-hotswap__actions :deep(.el-button) {
  min-height: 36px;
}

.channels-hotswap__not-available {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.channels-hotswap__list {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin: 0;
  padding-left: var(--khaen-space-4);
  font-size: var(--khaen-font-aux);
}

.channels-hotswap__list--blockers {
  color: var(--khaen-danger);
}

@media (max-width: 1279px) {
  .channels-grid__span-7,
  .channels-grid__span-5 {
    grid-column: span 12;
  }

  .channels-plan-band {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

@media (max-width: 767px) {
  .channels-plan-band {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .channels-hotswap__form {
    flex-direction: column;
  }
}
</style>
