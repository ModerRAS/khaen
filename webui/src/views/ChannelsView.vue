<script setup lang="ts">
/**
 * 方案与声道页——方案上传/历史/回滚 + 声道热重配（PUT /api/channels 两阶段）。
 *
 * 数据来源（页面唯一事实源，缺失即「未接入/未上报」）：
 * - GET /api/status（5s 轮询）：plan.graph_id / version / latency_samples / applied_at / source；
 * - GET /api/channels（恒 200 khaen-channels/v1）：current/device/format/generation/busy/pending/
 *   last_gap_ms/last_result/input/output/mapping/plan/hot_reconfigure/not_wired/reasons；
 *   mapping=null 时显示「当前值未知」+ 原因（不拿旧值/直通冒充当前值）；
 * - PUT /api/channels 两阶段：prepare（可带 channels/sample_rate/aes67 系列字段/jitter_frames/mapping/dac_map）
 *   → commit {stage:'commit',engine_token} 或 abort {stage:'abort',engine_token}；
 *   mapping/dac_map 是方案 routing：commit_supported=false 时禁用 commit、逐条显示 blockers，
 *   引导走方案上传 → prepare → commit（不签发假 token、不假成功）；
 * - POST /api/plan/upload（客户端 sha256 + 服务端 sha 双侧比对）→ POST /api/plan/inspect {plan_path}
 *   （plan_path = 上传回执的 stored_path；失败降级为上传摘要）→ POST /api/prepare {plan_path, plan_sha256}
 *   → POST /api/commit / POST /api/abort；
 * - GET /api/plan/history（daemon 真实形态 current:{version},entries[]）→ POST /api/rollback {target}
 *   → 回滚草稿 → commit（版本号递增，历史不改写）。
 *
 * 诚实边界：
 * - 后端不可用/未接线时显示 NOT_AVAILABLE + 稳定码 + 原因，不编造历史/映射；
 * - MOTU 物理输出只标注已知位置（1-2 L/R、3-4 SL/SR、5-6 TF/TFR、8 LFE），未知位置显示输出序号，不猜；
 * - 声道/样本率变更会中断出声：commit 前必须确认；502 reconfig_failed 的 details.rolled_back 原样展示；
 * - 回执里的 applied/generation/gap_ms/format 是数据面真实值；「设备已换格式」只按 rt-host 回执标注；
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
  CHANNELS_MAX_CONTENT,
  CHANNELS_SAMPLE_RATES_HZ,
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
  discardChannelLayoutDraft,
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
  const rows: KvRow[] = [
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
    },
    {
      label: t('channels.input.field.streams'),
      value: input.streams === null ? null : t('channels.input.streamsValue', { count: input.streams.length }),
      reason: reasonForChannelsPath(info, 'input.streams')
    }
  ]
  for (const [index, stream] of (input.streams ?? []).entries()) {
    const group = stream.group ?? t('common.dash')
    const port = stream.port === null ? t('common.dash') : String(stream.port)
    const channels = stream.channels === null ? t('common.dash') : String(stream.channels)
    const stats = stream.stats
    rows.push({
      label: t('channels.input.streamLabel', { index: index + 1 }),
      value:
        stats === null
          ? t('channels.input.streamSummaryNoStats', { group, port, channels })
          : t('channels.input.streamSummary', {
              group,
              port,
              channels,
              packets: formatInteger(stats.packets, localeId.value),
              underruns: formatInteger(stats.underruns, localeId.value)
            }),
      reason: stats === null ? reasonForChannelsPath(info, 'input.streams.stats') : null
    })
  }
  return rows
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

/* ------------------------------------------------- 声道热重配（PUT /api/channels 两阶段） */

const channelsCurrent = computed(() => channels.value?.current ?? null)
/** 当前输出通道数：channels_status.current 优先，方案 routing / 契约 B format 是降级来源（都真实上报）。 */
const currentOutputChannels = computed(
  () => channelsCurrent.value?.channels ?? channels.value?.output.channels ?? null
)
/** 当前输入宽度：rt-host current.content_channels；旧 rt-host 不报时回退输出宽度（旧耦合语义）。 */
const currentContentChannels = computed(
  () => channelsCurrent.value?.content_channels ?? currentOutputChannels.value
)
const currentSampleRate = computed(
  () => channelsCurrent.value?.sample_rate ?? channels.value?.format?.sample_rate ?? null
)
const currentAes67Channels = computed(
  () => channelsCurrent.value?.aes67_channels ?? channels.value?.input.channels ?? null
)
const currentAes67Map = computed<number[] | null>(
  () => channelsCurrent.value?.aes67_map ?? channels.value?.input.map ?? null
)
/** 从 mapping（lane->dac）还原 dac_map（dac -> lane；null = 该 DAC 无 lane）；mapping=null 时为 null（未知）。 */
const currentDacMap = computed<Array<number | null> | null>(() => {
  const info = channels.value
  if (info === null || info.mapping === null) {
    return null
  }
  const length =
    info.output.channels ?? (info.mapping.length > 0 ? Math.max(...info.mapping.map((entry) => entry.dac)) + 1 : 0)
  const result: Array<number | null> = []
  for (let dac = 0; dac < length; dac += 1) {
    result.push(null)
  }
  for (const entry of info.mapping) {
    if (Number.isInteger(entry.dac) && entry.dac >= 0 && entry.dac < length) {
      result[entry.dac] = entry.lane
    }
  }
  return result
})

const draftChannels = ref<number | null>(null)
const draftContentChannels = ref<number | null>(null)
/** 输入宽度是否跟随输出通道（旧行为默认开；关掉后显式发送 content_channels）。 */
const followOutputChannels = ref(true)
const draftSampleRate = ref<number | null>(null)
/** 原始 SDP 文本（IO I2c）：非空时整体替换 AES67 接收参数，显式 aes67_* 字段不生效。 */
const draftSdp = ref<string>('')
const draftAes67Group = ref<string>('')
const draftAes67Port = ref<number | null>(null)
const draftPayloadType = ref<number | null>(null)
const draftAes67Channels = ref<number | null>(null)
const draftAes67MapText = ref<string>('')

/** SDP 非空时整体替换 AES67 接收参数：逐字段草稿不参与补丁，也不参与逐字段校验。 */
const sdpActive = computed(() => draftSdp.value.trim().length > 0)
const draftJitter = ref<number | null>(null)
/** 方案 routing 的 DAC -> lane 文本（'-' = 该 DAC 无 lane）；空 = 未编辑，不猜当前路由。 */
const draftDacMapText = ref<string>('')

function joinNumberList(values: readonly number[] | null): string {
  return values === null ? '' : values.join(', ')
}

function dacMapToText(values: readonly (number | null)[] | null): string {
  return values === null ? '' : values.map((value) => (value === null ? '-' : String(value))).join(', ')
}

function parseNumberList(text: string): number[] | null {
  const trimmed = text.trim()
  if (trimmed.length === 0) {
    return null
  }
  const values: number[] = []
  for (const part of trimmed.split(',')) {
    const token = part.trim()
    if (!/^\d+$/.test(token)) {
      return null
    }
    values.push(Number(token))
  }
  return values
}

function parseDacMapList(text: string): Array<number | null> | null {
  const trimmed = text.trim()
  if (trimmed.length === 0) {
    return null
  }
  const values: Array<number | null> = []
  for (const part of trimmed.split(',')) {
    const token = part.trim()
    if (token === '-') {
      values.push(null)
      continue
    }
    if (!/^\d+$/.test(token)) {
      return null
    }
    values.push(Number(token))
  }
  return values
}

function sameNumberList(a: readonly number[] | null, b: readonly number[] | null): boolean {
  if (a === null || b === null) {
    return a === b
  }
  return a.length === b.length && a.every((value, index) => value === b[index])
}

function sameNullableList(
  a: readonly (number | null)[] | null,
  b: readonly (number | null)[] | null
): boolean {
  if (a === null || b === null) {
    return a === b
  }
  return a.length === b.length && a.every((value, index) => value === b[index])
}

function syncDraftsFromChannels(): void {
  const info = channels.value
  draftChannels.value = currentOutputChannels.value
  draftContentChannels.value = currentContentChannels.value
  followOutputChannels.value =
    currentContentChannels.value === null ||
    currentOutputChannels.value === null ||
    currentContentChannels.value === currentOutputChannels.value
  draftSampleRate.value = currentSampleRate.value
  draftSdp.value = ''
  draftAes67Group.value = channelsCurrent.value?.aes67_group ?? ''
  draftAes67Port.value = channelsCurrent.value?.aes67_port ?? null
  draftPayloadType.value = channelsCurrent.value?.aes67_payload_type ?? null
  draftAes67Channels.value = currentAes67Channels.value
  draftAes67MapText.value = joinNumberList(currentAes67Map.value)
  draftJitter.value = channelsCurrent.value?.jitter_frames ?? null
  draftDacMapText.value = info !== null && info.mapping !== null ? dacMapToText(currentDacMap.value) : ''
}

watch(draftChannels, (value) => {
  // 跟随模式下输入宽度字段只读展示输出宽度；关掉后保留独立草稿。
  if (followOutputChannels.value) {
    draftContentChannels.value = value
  }
})

watch(
  () => channels.value,
  (info) => {
    // 准备中/有草稿时不覆盖用户编辑；commit 成功会清草稿并重新 load，随后按新 current 同步。
    if (channelsState.channelsDraft !== null || channelsState.channelsBusy !== null) {
      return
    }
    if (info === null) {
      return
    }
    syncDraftsFromChannels()
  },
  { immediate: true }
)

/** 与当前值不同的字段才进补丁（未提供的字段后端不修改）。 */
const channelsPatch = computed<ChannelsPatch>(() => {
  const patch: ChannelsPatch = {}
  if (channels.value === null) {
    return patch
  }
  if (draftChannels.value !== null && draftChannels.value !== currentOutputChannels.value) {
    patch.channels = draftChannels.value
  }
  // 跟随模式由后端保持旧语义（输入宽度 = channels）；独立模式显式发送 content_channels。
  const contentChanged =
    draftContentChannels.value !== null &&
    currentContentChannels.value !== null &&
    draftContentChannels.value !== currentContentChannels.value
  if (
    !followOutputChannels.value &&
    draftContentChannels.value !== null &&
    (contentChanged || patch.channels !== undefined)
  ) {
    patch.content_channels = draftContentChannels.value
  }
  if (draftSampleRate.value !== null && draftSampleRate.value !== currentSampleRate.value) {
    patch.sample_rate = draftSampleRate.value
  }
  // SDP 是整体替换：非空时只发 sdp，不再逐字段发 aes67_*（后端也会拒绝混用）。
  const sdp = draftSdp.value.trim()
  if (sdp.length > 0) {
    patch.sdp = sdp
  } else {
    const group = draftAes67Group.value.trim()
    if (group.length > 0 && group !== (channelsCurrent.value?.aes67_group ?? null)) {
      patch.aes67_group = group
    }
    if (draftAes67Port.value !== null && draftAes67Port.value !== (channelsCurrent.value?.aes67_port ?? null)) {
      patch.aes67_port = draftAes67Port.value
    }
    if (
      draftPayloadType.value !== null &&
      draftPayloadType.value !== (channelsCurrent.value?.aes67_payload_type ?? null)
    ) {
      patch.aes67_payload_type = draftPayloadType.value
    }
    if (draftAes67Channels.value !== null && draftAes67Channels.value !== currentAes67Channels.value) {
      patch.aes67_channels = draftAes67Channels.value
    }
    const parsedAes67Map = parseNumberList(draftAes67MapText.value)
    if (parsedAes67Map !== null && !sameNumberList(parsedAes67Map, currentAes67Map.value)) {
      patch.aes67_map = parsedAes67Map
    }
  }
  if (draftJitter.value !== null && draftJitter.value !== (channelsCurrent.value?.jitter_frames ?? null)) {
    patch.jitter_frames = draftJitter.value
  }
  const parsedDacMap = parseDacMapList(draftDacMapText.value)
  if (parsedDacMap !== null && !sameNullableList(parsedDacMap, currentDacMap.value)) {
    patch.dac_map = parsedDacMap
  }
  return patch
})

/** 文本解析失败先于契约校验（否则会把非法文本当「未修改」静默丢掉）。 */
const patchTextError = computed<string | null>(() => {
  if (
    !sdpActive.value &&
    draftAes67MapText.value.trim().length > 0 &&
    parseNumberList(draftAes67MapText.value) === null
  ) {
    return 'aes67_map_value'
  }
  if (draftDacMapText.value.trim().length > 0 && parseDacMapList(draftDacMapText.value) === null) {
    return 'dac_map_value'
  }
  return null
})

/** 只改 aes67_channels 而映射长度不匹配时也要在客户端拦住（后端会 400）。 */
const patchAes67ConsistencyError = computed<string | null>(() => {
  if (sdpActive.value) {
    return null
  }
  const patch = channelsPatch.value
  const targetChannels = patch.aes67_channels ?? channelsCurrent.value?.aes67_channels ?? null
  const targetMap = patch.aes67_map ?? currentAes67Map.value
  if (targetChannels !== null && targetMap !== null && targetMap.length !== targetChannels) {
    return 'aes67_map_length'
  }
  return null
})

const patchValidation = computed<string | null>(() => {
  if (Object.keys(channelsPatch.value).length === 0) {
    return 'empty'
  }
  return (
    patchTextError.value ??
    patchAes67ConsistencyError.value ??
    validateChannelsPatch(channelsPatch.value)
  )
})

const channelsDraft = computed(() => channelsState.channelsDraft)
const channelsCommitAck = computed(() => channelsState.channelsCommit)
const channelsWriteBusy = computed(() => channelsState.channelsBusy)
const channelsWriteError = computed(() => channelsState.channelsError)
/** commit_supported=false 是页面内诚实保护（blockers 在草稿卡逐条展示），不重复弹错误条。 */
const channelsNoticeError = computed(() =>
  channelsWriteError.value !== null && channelsWriteError.value.category === 'commit_unsupported'
    ? null
    : channelsWriteError.value
)
const channelsErrorStage = computed(() => channelsState.channelsErrorStage)
const channelsFailure = computed(() => channelsState.channelsFailure)

const canPrepareChannels = computed(
  () => channelsWriteBusy.value === null && channels.value !== null && patchValidation.value === null
)

/** commit_supported=false 或没有 token：禁止发 commit（不假成功）。 */
const canCommitChannels = computed(
  () =>
    channelsDraft.value !== null &&
    channelsWriteBusy.value === null &&
    channelsDraft.value.commit_supported &&
    channelsDraft.value.engine_token !== null
)
const canAbortChannels = computed(
  () => channelsDraft.value !== null && channelsWriteBusy.value === null
)

/* --------------------------------------------- 只读状态（capability / 当前值来源） */

const hotCapabilityText = computed(() => {
  const wired = channels.value?.hot_reconfigure.wired ?? null
  if (wired === true) {
    return t('channelsEdit.hotswap.capabilityWired')
  }
  if (wired === false) {
    return t('channelsEdit.hotswap.capabilityNotWired')
  }
  return t('channelsEdit.hotswap.capabilityUnknown')
})

const hotCapabilityReason = computed(() => channels.value?.hot_reconfigure.reason ?? null)

const pendingText = computed(() => {
  const pending = channels.value?.pending ?? null
  return pending === null ? t('channelsEdit.hotswap.pendingNo') : t('channelsEdit.hotswap.pendingYes')
})

const lastGapText = computed(() => {
  const gap = channels.value?.last_gap_ms ?? null
  return gap === null ? null : t('channelsEdit.hotswap.lastGapValue', { ms: intText(gap) ?? String(gap) })
})

const lastResultJson = computed(() => {
  const result = channels.value?.last_result ?? null
  if (result === null) {
    return null
  }
  try {
    return JSON.stringify(result, null, 2)
  } catch {
    return String(result)
  }
})

const deviceText = computed(() => {
  const device = channels.value?.device ?? null
  if (device === null || device.name === null) {
    return null
  }
  return t('channelsEdit.hotswap.deviceValue', {
    name: device.name,
    state: device.state ?? t('common.dash')
  })
})

const probeText = computed(() => {
  const probe = channels.value?.device?.probe ?? null
  if (probe === null) {
    return null
  }
  if (probe.available === false) {
    return t('channelsEdit.hotswap.probeUnavailable')
  }
  return t('channelsEdit.hotswap.probeValue', {
    rate: intText(probe.sample_rate) ?? t('common.dash'),
    channels: intText(probe.channels) ?? t('common.dash')
  })
})

async function onPrepareChannels(): Promise<void> {
  if (!canPrepareChannels.value) {
    return
  }
  const ack = await prepareChannelLayout(channelsPatch.value)
  if (ack === null) {
    return
  }
  if (ack.commit_supported) {
    ElMessage.success(t('channelsEdit.hotswap.prepared'))
  } else {
    // 不签发假 token：commit 被本地禁用，明示只能走方案 prepare+commit。
    ElMessage.warning(t('channelsEdit.error.commitUnsupported'))
  }
}

async function onCommitChannels(): Promise<void> {
  if (!canCommitChannels.value) {
    return
  }
  try {
    await ElMessageBox.confirm(
      t('channelsEdit.hotswap.commitConfirmMessage'),
      t('channelsEdit.hotswap.commitConfirmTitle'),
      { confirmButtonText: t('channelsEdit.hotswap.commitConfirmOk'), cancelButtonText: t('common.cancel'), type: 'warning' }
    )
  } catch {
    return
  }
  const ack = await commitChannelLayout()
  if (ack === null) {
    return
  }
  ElMessage.success(t('channelsEdit.hotswap.committed'))
  await refreshStatus()
  if (planRecommitRequired.value === true) {
    ElMessage.warning(t('channelsEdit.hotswap.planRecommitLabel'))
  }
}

async function onAbortChannels(): Promise<void> {
  const hadToken = channelsDraft.value !== null && channelsDraft.value.engine_token !== null
  const ok = await abortChannelLayout()
  if (ok) {
    ElMessage.success(t(hadToken ? 'channelsEdit.hotswap.aborted' : 'channelsEdit.hotswap.discarded'))
  }
}

function onDiscardChannelsDraft(): void {
  discardChannelLayoutDraft()
  ElMessage.success(t('channelsEdit.hotswap.discarded'))
}

function onResetChannelsDrafts(): void {
  syncDraftsFromChannels()
}

/* -------------------------------------------------- 草稿 / 回执 / 失败明细展示 */

function formatText(format: {
  channels: number | null
  sample_rate: number | null
  block_frames: number | null
} | null): string | null {
  if (format === null) {
    return null
  }
  return t('channelsEdit.hotswap.formatValue', {
    rate: intText(format.sample_rate) ?? t('common.dash'),
    channels: intText(format.channels) ?? t('common.dash'),
    frames: intText(format.block_frames) ?? t('common.dash')
  })
}

function jsonText(value: unknown): string | null {
  if (value === null || value === undefined) {
    return null
  }
  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return String(value)
  }
}

const draftChangedText = computed(() => {
  const changed = channelsDraft.value?.changed ?? null
  if (changed === true) {
    return t('channelsEdit.hotswap.changedYes')
  }
  if (changed === false) {
    return t('channelsEdit.hotswap.changedNo')
  }
  return t('channelsEdit.hotswap.notReported')
})

const draftFormatText = computed(() => formatText(channelsDraft.value?.format ?? null))

const draftGraphText = computed(() => {
  const graph = channelsDraft.value?.graph ?? null
  if (graph === null || graph.action === null) {
    return null
  }
  const reason = graph.reason ?? t('channelsEdit.hotswap.notReported')
  return t('channelsEdit.hotswap.graphValue', { action: graph.action, reason })
})

const draftAdmissionText = computed(() => {
  const admission = channelsDraft.value?.admission ?? null
  if (admission === null) {
    return t('channelsEdit.hotswap.admissionUnknown')
  }
  const accepted = admission.accepted
  if (accepted === true) {
    return t('channelsEdit.hotswap.admissionAccepted')
  }
  if (accepted === false) {
    return t('channelsEdit.hotswap.admissionRejected')
  }
  return t('channelsEdit.hotswap.admissionUnknown')
})

const draftBlockers = computed(() => channelsDraft.value?.blockers ?? [])
const draftWarnings = computed(() => channelsDraft.value?.warnings ?? [])
const draftEngineToken = computed(() => shortenId(channelsDraft.value?.engine_token ?? null, 8, 4))
const draftDiffJson = computed(() => jsonText(channelsDraft.value?.diff ?? null))
const draftCurrentJson = computed(() => jsonText(channelsDraft.value?.current ?? null))
const draftTargetJson = computed(() => jsonText(channelsDraft.value?.target ?? null))
const draftRawJson = computed(() => jsonText(channelsDraft.value ?? null))
const draftCommitUnsupported = computed(
  () => channelsDraft.value !== null && !channelsDraft.value.commit_supported
)

const channelsCommitAckRows = computed<KvRow[]>(() => {
  const ack = channelsCommitAck.value
  if (ack === null) {
    return []
  }
  const boolText = (value: boolean | null): string | null =>
    value === null ? null : value ? t('channelsEdit.hotswap.failureYes') : t('channelsEdit.hotswap.failureNo')
  return [
    {
      label: t('channelsEdit.hotswap.appliedLabel'),
      value:
        ack.applied === null
          ? t('channelsEdit.hotswap.notReported')
          : ack.applied
            ? t('channelsEdit.hotswap.appliedYes')
            : t('channelsEdit.hotswap.appliedNo'),
      tone: ack.applied === false ? ('warn' as const) : null
    },
    { label: t('channelsEdit.hotswap.changedLabel'), value: boolText(ack.changed) },
    { label: t('channelsEdit.hotswap.commitGenerationLabel'), value: intText(ack.generation) },
    {
      label: t('channelsEdit.hotswap.gapLabel'),
      value: ack.gap_ms === null ? null : t('channelsEdit.hotswap.gapValue', { ms: intText(ack.gap_ms) ?? String(ack.gap_ms) })
    },
    { label: t('channelsEdit.hotswap.formatLabel'), value: formatText(ack.format) },
    { label: t('channelsEdit.hotswap.graphDroppedLabel'), value: boolText(ack.graph_dropped) },
    { label: t('channelsEdit.hotswap.rolledBackLabel'), value: boolText(ack.rolled_back) },
    { label: t('channelsEdit.hotswap.idempotentLabel'), value: boolText(ack.idempotent) },
    {
      label: t('channelsEdit.hotswap.routingAppliedLabel'),
      value:
        ack.routing === null || ack.routing.applied === null
          ? t('channelsEdit.hotswap.notReported')
          : ack.routing.applied
            ? t('channelsEdit.hotswap.routingAppliedYes')
            : t('channelsEdit.hotswap.routingAppliedNo')
    },
    {
      label: t('channelsEdit.hotswap.planRecommitLabel'),
      value: boolText(planRecommitRequired.value)
    },
    { label: t('channelsEdit.hotswap.noteLabel'), value: ack.note }
  ]
})

const planRecommitRequired = computed<boolean | null>(() => {
  const ack = channelsCommitAck.value
  if (ack === null) {
    return null
  }
  if (ack.routing !== null && ack.routing.plan_recommit_required !== null) {
    return ack.routing.plan_recommit_required
  }
  if (ack.graph_dropped !== null) {
    return ack.graph_dropped
  }
  return null
})

const commitDetailsJson = computed(() => jsonText(channelsCommitAck.value ?? null))

const failureRows = computed<KvRow[]>(() => {
  const failure = channelsFailure.value
  if (failure === null) {
    return []
  }
  const boolText = (value: boolean | null): string | null =>
    value === null ? null : value ? t('channelsEdit.hotswap.failureYes') : t('channelsEdit.hotswap.failureNo')
  return [
    { label: t('channelsEdit.hotswap.failureCode'), value: failure.code, copy: failure.code },
    { label: t('channelsEdit.hotswap.failureStage'), value: failure.stage },
    { label: t('channelsEdit.hotswap.failureRolledBack'), value: boolText(failure.rolled_back), tone: failure.rolled_back === true ? ('ok' as const) : null },
    { label: t('channelsEdit.hotswap.failureDeviceStarted'), value: boolText(failure.device_started) },
    { label: t('channelsEdit.hotswap.failureReceiverRunning'), value: boolText(failure.receiver_running) },
    { label: t('channelsEdit.hotswap.failureApplied'), value: boolText(failure.applied), tone: failure.applied === true ? ('bad' as const) : null },
    {
      label: t('channelsEdit.hotswap.failureGap'),
      value: failure.gap_ms === null ? null : t('channelsEdit.hotswap.gapValue', { ms: intText(failure.gap_ms) ?? String(failure.gap_ms) })
    },
    { label: t('channelsEdit.hotswap.failureGeneration'), value: intText(failure.generation) }
  ]
})

const failureJson = computed(() => jsonText(channelsFailure.value?.raw ?? null))
const failureVisible = computed(
  () => channelsFailure.value !== null || channelsWriteError.value?.category === 'reconfig_failed'
)

const commitAtText = computed(() => {
  const at = channelsState.channelsCommitAt
  return at === null ? null : t('channelsEdit.hotswap.commitAt', { time: clockText(at) ?? t('common.dash') })
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
    <PageHeader title-key="channelsEdit.pageTitle" subtitle-key="channelsEdit.pageSubtitle">
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

    <!-- 声道后端不可用：整块 NOT_AVAILABLE + 稳定码 + 原因（不编造映射/通道数） -->
    <div v-if="fatalChannels" class="banner banner--warn channels-page__not-wired" role="status">
      <div class="channels-page__not-wired-text">
        <span class="channels-page__not-wired-title">{{ t('channelsEdit.unavailable.title') }}</span>
        <span>{{ t('channelsEdit.unavailable.desc', { message: t(channelsError!.i18nKey) }) }}</span>
        <span v-if="channelsError!.detail" class="aux-text mono">{{ channelsError!.code }} — {{ channelsError!.detail }}</span>
      </div>
      <el-button size="small" @click="onRefresh">{{ t('common.retry') }}</el-button>
    </div>

    <ControlErrorNotice :error="channelsNoticeError" @retry="onRefresh" />
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
        <p v-if="historyError" class="aux-text">{{ t('channelsEdit.plan.historyUnavailableHint') }}</p>
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
            :reason="channels === null ? t('channelsEdit.unavailable.reason') : reasonForChannelsPath(channels, 'output.channels')"
          />
          <StatRow
            :label="t('channels.output.field.contentChannels')"
            :value="intText(currentContentChannels)"
            :reason="channels === null ? t('channelsEdit.unavailable.reason') : (channelsCurrent?.content_channels ?? null) === null ? t('channelsEdit.hotswap.contentUnreported') : null"
          />
          <StatRow
            :label="t('channels.output.field.dacMap')"
            :value="dacMapText"
            :copy="dacMapText"
            :reason="channels === null ? t('channelsEdit.unavailable.reason') : reasonForChannelsPath(channels, 'output.dac_map')"
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
            :reason="row.reason ?? (channels === null ? t('channelsEdit.unavailable.reason') : null)"
          />
        </ul>
        <p class="aux-text">{{ t('channelsEdit.input.note') }}</p>
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
          {{ t('channelsEdit.history.unavailable', { message: t(historyError.i18nKey) }) }}
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

      <!-- 声道热重配（PUT /api/channels 两阶段：prepare → commit/abort；回执如实展示） -->
      <PanelCard
        class="channels-grid__span-7"
        title-key="channelsEdit.hotswap.title"
        desc-key="channelsEdit.hotswap.desc"
        :truth="channels === null ? 'notWired' : 'actual'"
      >
        <!-- 只读：能力/目标/当前值来源/数据面状态（GET /api/channels） -->
        <ul class="kv-list">
          <StatRow
            :label="t('channelsEdit.hotswap.capabilityLabel')"
            :value="hotCapabilityText"
            :reason="hotCapabilityReason"
            :tone="channels?.hot_reconfigure.wired === false ? 'bad' : null"
          />
          <StatRow :label="t('channelsEdit.hotswap.targetLabel')" :value="channels?.target ?? null" :reason="null" />
          <StatRow
            :label="t('channelsEdit.hotswap.sourceLabel')"
            :value="t('channelsEdit.hotswap.sourceValue')"
            :reason="null"
          />
          <StatRow
            :label="t('channelsEdit.hotswap.generationLabel')"
            :value="intText(channels?.generation ?? null)"
            :reason="null"
          />
          <StatRow :label="t('channelsEdit.hotswap.busyLabel')" :value="boolText(channels?.busy ?? null)" :reason="null" />
          <StatRow :label="t('channelsEdit.hotswap.pendingLabel')" :value="pendingText" :reason="null" />
          <StatRow :label="t('channelsEdit.hotswap.lastGapLabel')" :value="lastGapText" :reason="null" />
          <StatRow :label="t('channelsEdit.hotswap.deviceLabel')" :value="deviceText" :reason="null" />
          <StatRow :label="t('channelsEdit.hotswap.probeLabel')" :value="probeText" :reason="null" />
        </ul>
        <details v-if="lastResultJson !== null" class="channels-hotswap__details">
          <summary>{{ t('channelsEdit.hotswap.lastResultLabel') }}</summary>
          <pre class="channels-hotswap__pre mono">{{ lastResultJson }}</pre>
        </details>
        <p v-else class="aux-text">
          {{ t('channelsEdit.hotswap.lastResultLabel') }} · {{ t('channelsEdit.hotswap.lastResultNone') }}
        </p>

        <div class="banner banner--warn channels-hotswap__warning" role="status">
          <div class="channels-hotswap__warning-text">
            <strong>{{ t('channelsEdit.hotswap.warningTitle') }}</strong>
            <span>{{ t('channelsEdit.hotswap.warningDesc') }}</span>
          </div>
        </div>

        <h3 class="channels-subheading">{{ t('channelsEdit.hotswap.formTitle') }}</h3>
        <div class="channels-hotswap__form">
          <label class="channels-hotswap__field">
            <span>{{ t('channelsEdit.hotswap.field.channels') }}</span>
            <el-input-number
              v-model="draftChannels"
              :min="1"
              :max="22"
              :disabled="channels === null || channelsWriteBusy !== null"
              size="small"
            />
          </label>
          <label class="channels-hotswap__field">
            <span>{{ t('channelsEdit.hotswap.field.contentChannels') }}</span>
            <el-input-number
              v-model="draftContentChannels"
              :min="1"
              :max="CHANNELS_MAX_CONTENT"
              :disabled="channels === null || channelsWriteBusy !== null || followOutputChannels"
              size="small"
            />
            <el-checkbox v-model="followOutputChannels" :disabled="channels === null || channelsWriteBusy !== null" size="small">
              {{ t('channelsEdit.hotswap.followOutputChannels') }}
            </el-checkbox>
          </label>
          <label class="channels-hotswap__field">
            <span>{{ t('channelsEdit.hotswap.field.sampleRate') }}</span>
            <el-select
              v-model="draftSampleRate"
              :disabled="channels === null || channelsWriteBusy !== null"
              size="small"
            >
              <el-option v-for="rate in CHANNELS_SAMPLE_RATES_HZ" :key="rate" :label="String(rate)" :value="rate" />
            </el-select>
          </label>
          <label class="channels-hotswap__field channels-hotswap__field--wide">
            <span>{{ t('channelsEdit.hotswap.field.sdp') }}</span>
            <el-input
              v-model="draftSdp"
              type="textarea"
              :rows="4"
              :placeholder="t('channelsEdit.hotswap.sdpPlaceholder')"
              :disabled="channels === null || channelsWriteBusy !== null"
              size="small"
            />
          </label>
          <p class="aux-text channels-hotswap__sdp-hint">{{ t('channelsEdit.hotswap.sdpHint') }}</p>
          <label class="channels-hotswap__field">
            <span>{{ t('channelsEdit.hotswap.field.aes67Group') }}</span>
            <el-input v-model="draftAes67Group" :disabled="channels === null || channelsWriteBusy !== null || sdpActive" size="small" />
          </label>
          <label class="channels-hotswap__field">
            <span>{{ t('channelsEdit.hotswap.field.aes67Port') }}</span>
            <el-input-number
              v-model="draftAes67Port"
              :min="1"
              :max="65535"
              :disabled="channels === null || channelsWriteBusy !== null || sdpActive"
              size="small"
            />
          </label>
          <label class="channels-hotswap__field">
            <span>{{ t('channelsEdit.hotswap.field.payloadType') }}</span>
            <el-input-number
              v-model="draftPayloadType"
              :min="96"
              :max="127"
              :disabled="channels === null || channelsWriteBusy !== null || sdpActive"
              size="small"
            />
          </label>
          <label class="channels-hotswap__field">
            <span>{{ t('channelsEdit.hotswap.field.aes67Channels') }}</span>
            <el-input-number
              v-model="draftAes67Channels"
              :min="1"
              :max="8"
              :disabled="channels === null || channelsWriteBusy !== null || sdpActive"
              size="small"
            />
          </label>
          <label class="channels-hotswap__field">
            <span>{{ t('channelsEdit.hotswap.field.aes67Map') }}</span>
            <el-input v-model="draftAes67MapText" :disabled="channels === null || channelsWriteBusy !== null || sdpActive" size="small" />
          </label>
          <label class="channels-hotswap__field">
            <span>{{ t('channelsEdit.hotswap.field.jitterFrames') }}</span>
            <el-input-number
              v-model="draftJitter"
              :min="0"
              :disabled="channels === null || channelsWriteBusy !== null"
              size="small"
            />
          </label>
        </div>

        <!-- 方案 routing（mapping/dac_map）：单独改动时 commit_supported=false，只能走方案两阶段 -->
        <h3 class="channels-subheading">{{ t('channelsEdit.hotswap.routingTitle') }}</h3>
        <p class="aux-text">{{ t('channelsEdit.hotswap.routingDesc') }}</p>
        <p v-if="channels !== null && channels.mapping === null" class="aux-text">
          <TruthTag kind="notWired" :reason="mappingReason" />
          {{ t('channelsEdit.hotswap.mappingUnknown') }}
        </p>
        <div class="channels-hotswap__routing">
          <label class="channels-hotswap__field channels-hotswap__field--wide">
            <span>{{ t('channelsEdit.hotswap.field.dacMap') }}</span>
            <el-input
              v-model="draftDacMapText"
              :placeholder="t('channelsEdit.hotswap.routingPlaceholder')"
              :disabled="channels === null || channelsWriteBusy !== null"
              size="small"
            />
          </label>
          <el-button size="small" :disabled="channels === null || channelsWriteBusy !== null" @click="onResetChannelsDrafts">
            {{ t('channelsEdit.hotswap.resetDrafts') }}
          </el-button>
        </div>

        <p v-if="patchValidation !== null && patchValidation !== 'empty'" class="channels-upload__invalid" role="alert">
          {{ t('channelsEdit.hotswap.validation.' + patchValidation) }}
        </p>
        <div class="channels-hotswap__actions">
          <el-button
            type="primary"
            plain
            :loading="channelsWriteBusy === 'prepare'"
            :disabled="!canPrepareChannels"
            @click="onPrepareChannels"
          >
            {{ t('channelsEdit.hotswap.prepare') }}
          </el-button>
          <el-button
            type="primary"
            :loading="channelsWriteBusy === 'commit'"
            :disabled="!canCommitChannels"
            @click="onCommitChannels"
          >
            {{ t('channelsEdit.hotswap.commit') }}
          </el-button>
          <el-button
            v-if="channelsDraft !== null && channelsDraft.engine_token !== null"
            :loading="channelsWriteBusy === 'abort'"
            :disabled="!canAbortChannels"
            @click="onAbortChannels"
          >
            {{ t('channelsEdit.hotswap.abort') }}
          </el-button>
          <el-button
            v-else-if="channelsDraft !== null"
            :disabled="channelsWriteBusy !== null"
            @click="onDiscardChannelsDraft"
          >
            {{ t('channelsEdit.hotswap.discard') }}
          </el-button>
          <el-button
            v-if="channelsDraft !== null && channelsErrorStage === 'abort'"
            :disabled="channelsWriteBusy !== null"
            @click="onDiscardChannelsDraft"
          >
            {{ t('channelsEdit.hotswap.discard') }}
          </el-button>
        </div>

        <!-- prepare 草稿（不签发假 token；commit_supported=false 时逐条 blockers） -->
        <template v-if="channelsDraft !== null">
          <h3 class="channels-subheading">{{ t('channelsEdit.hotswap.draftTitle') }}</h3>
          <ul class="kv-list">
            <StatRow :label="t('channelsEdit.hotswap.changedLabel')" :value="draftChangedText" :reason="null" />
            <StatRow
              :label="t('channelsEdit.hotswap.commitSupportedLabel')"
              :value="channelsDraft.commit_supported ? t('channelsEdit.hotswap.commitSupportedYes') : t('channelsEdit.hotswap.commitSupportedNo')"
              :tone="channelsDraft.commit_supported ? 'ok' : 'bad'"
              :reason="null"
            />
            <StatRow
              :label="t('channelsEdit.hotswap.engineTokenLabel')"
              :value="draftEngineToken ?? t('common.dash')"
              :copy="channelsDraft.engine_token"
              :reason="channelsDraft.commit_supported ? null : t('channelsEdit.error.commitUnsupported')"
            />
            <StatRow
              :label="t('channelsEdit.hotswap.plannedGenerationLabel')"
              :value="intText(channelsDraft.planned_generation)"
              :reason="null"
            />
            <StatRow :label="t('channelsEdit.hotswap.formatLabel')" :value="draftFormatText" :reason="null" />
            <StatRow :label="t('channelsEdit.hotswap.graphActionLabel')" :value="draftGraphText" :reason="null" />
            <StatRow :label="t('channelsEdit.hotswap.admissionLabel')" :value="draftAdmissionText" :reason="null" />
          </ul>
          <ul v-if="draftWarnings.length > 0" class="channels-hotswap__list">
            <li v-for="warning in draftWarnings" :key="warning" class="aux-text">{{ warning }}</li>
          </ul>
          <template v-if="draftCommitUnsupported">
            <p class="banner banner--info" role="status">{{ t('channelsEdit.hotswap.guidance') }}</p>
            <h4 class="channels-hotswap__blockers-title">{{ t('channelsEdit.hotswap.blockersTitle') }}</h4>
            <ul class="channels-hotswap__list channels-hotswap__list--blockers">
              <li v-for="blocker in draftBlockers" :key="blocker">{{ blocker }}</li>
            </ul>
          </template>
          <ul v-else-if="draftBlockers.length > 0" class="channels-hotswap__list channels-hotswap__list--blockers">
            <li v-for="blocker in draftBlockers" :key="blocker">{{ blocker }}</li>
          </ul>
          <details v-if="draftDiffJson !== null" class="channels-hotswap__details">
            <summary>{{ t('channelsEdit.hotswap.diffTitle') }}</summary>
            <pre class="channels-hotswap__pre mono">{{ draftDiffJson }}</pre>
          </details>
          <details v-if="draftCurrentJson !== null" class="channels-hotswap__details">
            <summary>{{ t('channelsEdit.hotswap.currentTitle') }}</summary>
            <pre class="channels-hotswap__pre mono">{{ draftCurrentJson }}</pre>
          </details>
          <details v-if="draftTargetJson !== null" class="channels-hotswap__details">
            <summary>{{ t('channelsEdit.hotswap.targetTitle') }}</summary>
            <pre class="channels-hotswap__pre mono">{{ draftTargetJson }}</pre>
          </details>
          <details v-if="draftRawJson !== null" class="channels-hotswap__details">
            <summary>{{ t('channelsEdit.hotswap.detailsTitle') }}</summary>
            <pre class="channels-hotswap__pre mono">{{ draftRawJson }}</pre>
          </details>
        </template>

        <!-- commit 真实回执（applied/generation/gap_ms/format/graph_dropped/routing） -->
        <template v-if="channelsCommitAck !== null">
          <h3 class="channels-subheading">{{ t('channelsEdit.hotswap.receiptTitle') }}</h3>
          <ul class="kv-list">
            <StatRow
              v-for="row in channelsCommitAckRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :copy="row.copy ?? null"
              :tone="row.tone ?? null"
              :reason="null"
            />
          </ul>
          <p v-if="commitAtText !== null" class="aux-text">{{ commitAtText }}</p>
          <p class="aux-text">{{ t('channelsEdit.hotswap.commitFormatHint') }}</p>
          <p v-if="planRecommitRequired === true" class="banner banner--warn" role="status">
            {{ t('channelsEdit.hotswap.graphDropped') }}
          </p>
          <details v-if="commitDetailsJson !== null" class="channels-hotswap__details">
            <summary>{{ t('channelsEdit.hotswap.detailsTitle') }}</summary>
            <pre class="channels-hotswap__pre mono">{{ commitDetailsJson }}</pre>
          </details>
        </template>

        <!-- 502 reconfig_failed：details.rolled_back 等原样展示，不写成功 -->
        <template v-if="failureVisible">
          <h3 class="channels-subheading channels-hotswap__failure-title">{{ t('channelsEdit.hotswap.failureTitle') }}</h3>
          <ul class="kv-list">
            <StatRow
              v-for="row in failureRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :copy="row.copy ?? null"
              :tone="row.tone ?? null"
              :reason="null"
            />
          </ul>
          <p v-if="channelsWriteError?.detail" class="aux-text mono">{{ channelsWriteError.detail }}</p>
          <details v-if="failureJson !== null" class="channels-hotswap__details">
            <summary>{{ t('channelsEdit.hotswap.detailsTitle') }}</summary>
            <pre class="channels-hotswap__pre mono">{{ failureJson }}</pre>
          </details>
        </template>
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

.channels-hotswap__field--wide {
  flex: 1 1 320px;
  min-width: min(320px, 100%);
}

.channels-hotswap__sdp-hint {
  flex: 1 1 100%;
  margin: 0;
}

.channels-hotswap__routing {
  display: flex;
  align-items: flex-end;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.channels-hotswap__details {
  font-size: var(--khaen-font-aux);
}

.channels-hotswap__pre {
  margin: var(--khaen-space-1) 0 0;
  padding: var(--khaen-space-2);
  max-height: 240px;
  overflow: auto;
  border: 1px dashed var(--khaen-border);
  border-radius: var(--khaen-radius-tag);
  background: var(--khaen-bg);
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.channels-hotswap__blockers-title {
  margin-top: var(--khaen-space-2);
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
  font-weight: 600;
}

.channels-hotswap__failure-title {
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
