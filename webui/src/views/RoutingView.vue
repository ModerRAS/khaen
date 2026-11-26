<script setup lang="ts">
/**
 * 路由矩阵页（task-68）——「输入 → 矩阵域 → DAC 输出」只读诊断 + lane → DAC 可编辑两阶段。
 *
 * 当前值（唯一来源）：GET /api/channels 的 mapping（daemon 在 prepare/upload 时记录的方案 routing
 * 镜像，不是数据面实时回读）；页面标注来源/语义/版本/读取时间。mapping=null 时显示「当前值未知」+
 * 原因，不用 dac_map / 旧值 / 默认值冒充当前值（docs/API.md §5 末条、§9.7、§11 G3/G6）。
 * mapping 非 null 时预填编辑表（lane → 0-based DAC）。speaker_roles 是 /api/channels 的【加法】字段
 * （§12.2，后端已实现）：有就原样显示 canonical 角色，没有/null 就不显示，不编造。
 *
 * 可编辑两阶段（写入口复用控制面，docs/API.md §1/§4）：
 * - 本地 validateRoutingPatch（api/control.ts；本页再补 dac 上界）校验；
 * - POST /api/surface {surface:"routing", data:{routes, dac_channels?}} → stores/control.prepareRouting；
 * - DraftReview 展示可选回执（changed/diff/current/target/admission… 缺失显示「服务端未上报」）；
 * - POST /api/commit {engine_token} 生效 / POST /api/abort {engine_token} 丢弃；
 * - 409 no_base_plan 引导先 prepare+commit 方案；engine_token_invalid 由 control store 清草稿并提示；
 *   501/503 显示原因、不假成功；401 交给会话 store。
 *
 * 只读诊断仍保留 /api/status + /api/channels + /api/plan/history 摘要。
 */
import { computed, reactive, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import { Delete, Plus, Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import StatRow from '@/components/StatRow.vue'
import StatusBadge from '@/components/StatusBadge.vue'
import TruthTag from '@/components/TruthTag.vue'
import DraftReview from '@/components/control/DraftReview.vue'
import ControlErrorNotice from '@/components/control/ControlErrorNotice.vue'
import { useStatusPolling } from '@/composables/useStatusPolling'
import { isStatusStale, reasonForPath, runStateKind, runStateLabelKey } from '@/api/status'
import { MOTU_KNOWN_OUTPUT_NUMBERS, motuOutputLabelKey } from '@/api/channels'
import {
  ROUTING_MATRIX_LANES,
  buildPlanSummary,
  buildRoutingChain,
  currentRoutingSource,
  reasonForRoutingChannelsPath,
  speakerRoleForDac,
  validateRoutingDraft
} from '@/api/routing'
import type { ControlCommitAck, ControlDraft } from '@/api/control'
import {
  createInitialRoutingEditorState,
  createRoutingEditorController,
  loadRouting,
  refreshRouting,
  routingState
} from '@/stores/routing'
import {
  abortControlDraft,
  commitControlDraft,
  controlState,
  prepareRouting
} from '@/stores/control'
import { ensureMeta, metaCapabilityReason, metaSurfaceState } from '@/stores/meta'
import { formatClock, formatInteger, formatTimestamp, shortenId } from '@/utils/format'
import type { StatusKind } from '@/types/ui'

const { t, locale } = useI18n({ useScope: 'global' })
const router = useRouter()
const { state: statusState, refresh: refreshStatus } = useStatusPolling()

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const data = computed(() => statusState.data)
const channels = computed(() => routingState.channels)
const channelsError = computed(() => routingState.channelsError)
const history = computed(() => routingState.history)
const historyError = computed(() => routingState.historyError)
const refreshing = computed(() => routingState.refreshing || statusState.refreshing)

const chain = computed(() => buildRoutingChain(data.value, channels.value))
const plan = computed(() => buildPlanSummary(data.value, channels.value, history.value))
const currentSource = computed(() => currentRoutingSource(channels.value))

const statusFatal = computed(() => statusState.phase === 'error' && data.value === null)
const channelsFatal = computed(
  () => routingState.channelsPhase === 'error' && channels.value === null && channelsError.value !== null
)
/** 有任一侧事实来源就渲染诊断卡片（另一侧缺失按字段「未接入」）。 */
const hasFacts = computed(() => data.value !== null || channels.value !== null)

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

/** 通道映射数组 → "0, 1"；null 项显示「—」（不编造 0）。 */
function mapText(map: ReadonlyArray<number | null> | null): string | null {
  if (map === null || map.length === 0) {
    return null
  }
  return map.map((value) => (value === null ? t('common.dash') : String(value))).join(', ')
}

function sourceText(source: 'sdp' | 'manual' | null): string | null {
  if (source === 'sdp') {
    return t('routing.chain.input.sourceSdp')
  }
  if (source === 'manual') {
    return t('routing.chain.input.sourceManual')
  }
  return null
}

function statusReason(path: string): string | null {
  return reasonForPath(data.value, path)
}

function channelsReason(path: string): string | null {
  return reasonForRoutingChannelsPath(channels.value, path)
}

/** /api/channels 整体不可用时的兜底原因（字段级 reason 缺失时用）。 */
const noChannelsReason = computed(() =>
  channelsError.value !== null ? t(channelsError.value.i18nKey) : t('routingEdit.current.sourceUnknown')
)

const lastUpdateText = computed(() => {
  if (routingState.loadedAt === null) {
    return t('routing.updated.never')
  }
  const time = clockText(routingState.loadedAt)
  return time === null ? t('routing.updated.never') : t('routing.updated.last', { time })
})

/** 当前值未知时的原因（后端 reasons 优先，其次本地化兜底；不得编造原因）。 */
const currentUnknownReason = computed<string>(() => {
  const source = currentSource.value
  if (source.reason !== null) {
    return source.reason
  }
  if (channels.value === null && channelsError.value !== null) {
    return t(channelsError.value.i18nKey)
  }
  return t('routingEdit.current.unknownReason')
})

/* ------------------------------------------------------------ 顶部状态带 */

const hostLinkBadge = computed<{ kind: StatusKind; value: string }>(() => {
  const current = data.value
  if (current === null) {
    return { kind: 'unknown', value: t('routing.band.hostLinkUnknown') }
  }
  if (isStatusStale(current)) {
    return {
      kind: current.host_link.connected === false ? 'faulted' : 'muted',
      value: t('routing.band.hostLinkStale')
    }
  }
  if (current.host_link.connected === true) {
    return { kind: 'running', value: t('routing.band.hostLinkOk') }
  }
  return { kind: 'unknown', value: t('routing.band.hostLinkUnknown') }
})

const runStateBadge = computed<{ kind: StatusKind; value: string | undefined }>(() => {
  if (data.value === null) {
    return { kind: 'unknown', value: undefined }
  }
  const key = runStateLabelKey(data.value.state)
  return {
    kind: runStateKind(data.value.state),
    value: key !== null ? t(key) : data.value.state.value ?? undefined
  }
})

const planVersionText = computed(() => {
  const version = plan.value.version
  return version === null ? null : 'v' + (intText(version) ?? String(version))
})

const matrixLanesText = computed(() => intText(chain.value.matrix.lanes.value))
const outputChannelsText = computed(() => intText(chain.value.output.channels.value))
const routedLanesText = computed(() => intText(chain.value.matrix.routedLanes.value))

const knownLabelsText = computed(() =>
  MOTU_KNOWN_OUTPUT_NUMBERS.map(
    (channel) => channel + ' ' + t(motuOutputLabelKey(channel) ?? 'routing.lanes.unknownLabel', { channel })
  ).join(' / ')
)

/* ------------------------------------------------------------------ 告警 */

interface NoticeBanner {
  tone: 'warn' | 'danger' | 'info'
  text: string
}

const notices = computed<NoticeBanner[]>(() => {
  const banners: NoticeBanner[] = []
  if (statusState.error !== null) {
    banners.push({
      tone: 'warn',
      text: t('routing.notice.statusRefreshFailed', { message: t(statusState.error.i18nKey) })
    })
  }
  if (data.value !== null && isStatusStale(data.value)) {
    banners.push({ tone: 'warn', text: t('routing.notice.hostLinkDown') })
  }
  if (channelsError.value !== null && channels.value !== null) {
    banners.push({
      tone: 'warn',
      text: t('routing.notice.channelsRefreshFailed', { message: t(channelsError.value.i18nKey) })
    })
  }
  if (historyError.value !== null) {
    banners.push({
      tone: 'info',
      text: t('routing.notice.historyUnavailable', { message: t(historyError.value.i18nKey) })
    })
  }
  if (data.value !== null && data.value.plan.version === null && data.value.plan.graph_id === null) {
    banners.push({ tone: 'info', text: t('routing.notice.noPlan') })
  }
  return banners
})

/* ------------------------------------------------------------ 卡片真实性标签 */

const chainTruth = computed(() =>
  chain.value.input.channels.value !== null ||
  chain.value.matrix.lanes.value !== null ||
  chain.value.output.channels.value !== null
    ? ('actual' as const)
    : ('notWired' as const)
)

const planTruth = computed(() =>
  plan.value.version !== null ||
  plan.value.latencySamples !== null ||
  plan.value.graphId !== null
    ? ('actual' as const)
    : ('notWired' as const)
)

const notWiredTruth = computed(() => (hasFacts.value ? ('actual' as const) : ('notWired' as const)))

const notWiredEntries = computed<Array<{ source: string; path: string; reason: string }>>(() => {
  const entries: Array<{ source: string; path: string; reason: string }> = []
  for (const entry of channels.value?.not_wired ?? []) {
    entries.push({ source: t('routing.notWired.source.channels'), path: entry.path, reason: entry.reason })
  }
  for (const entry of data.value?.not_wired ?? []) {
    entries.push({ source: t('routing.notWired.source.status'), path: entry.path, reason: entry.reason })
  }
  return entries
})

/* ------------------------------------------------- 当前值来源（只读诊断卡） */

interface KvRow {
  label: string
  value: string | null
  copy?: string | null
  reason?: string | null
  tone?: 'ok' | 'warn' | 'bad' | null
}

const routingKnownText = computed(() => {
  const known = currentSource.value.routingKnown
  if (known === null) {
    return null
  }
  return known ? t('common.yes') : t('common.no')
})

const currentRows = computed<KvRow[]>(() => {
  const source = currentSource.value
  return [
    {
      label: t('routingEdit.current.source'),
      value: source.known ? t('routingEdit.current.sourceValue') : t('routingEdit.current.sourceUnknown'),
      reason: source.known ? null : currentUnknownReason.value
    },
    {
      label: t('routingEdit.current.semantics'),
      value: source.known ? t('routingEdit.current.semanticsValue') : null,
      reason: source.known ? null : currentUnknownReason.value
    },
    {
      label: t('routingEdit.current.planVersion'),
      value: intText(source.planVersion),
      reason: source.planVersion === null ? (channelsReason('plan') ?? currentUnknownReason.value) : null
    },
    {
      label: t('routingEdit.current.graphId'),
      value: shortenId(source.planGraphId, 10, 6),
      copy: source.planGraphId,
      reason: source.planGraphId === null ? (channelsReason('plan') ?? currentUnknownReason.value) : null
    },
    {
      label: t('routingEdit.current.dacChannels'),
      value: intText(source.dacChannels),
      reason: source.dacChannels === null ? channelsReason('output.channels') : null
    },
    {
      label: t('routingEdit.current.routes'),
      value: source.rows === null ? null : String(source.rows.length),
      reason: source.rows === null ? (channelsReason('mapping') ?? currentUnknownReason.value) : null
    },
    {
      label: t('routingEdit.current.routingKnown'),
      value: routingKnownText.value,
      reason: routingKnownText.value === null ? channelsReason('plan') : null
    },
    {
      label: t('routingEdit.current.readAt'),
      value: clockText(routingState.loadedAt),
      reason: routingState.loadedAt === null ? t('routing.updated.never') : null
    }
  ]
})

/* ------------------------------------------------------------- 编辑草稿 */

const editorState = reactive(createInitialRoutingEditorState())
const editor = createRoutingEditorController(editorState, { current: () => currentSource.value })

/** 刷新时若草稿未被手动编辑，则重新用当前值预填；手动编辑过的草稿不被刷新覆盖。 */
watch(
  () => routingState.channels,
  () => {
    if (!editorState.dirty) {
      editor.prefill()
    }
  },
  { immediate: true }
)

const dacBound = computed<number | null>(() =>
  editorState.explicitDacChannels && editorState.dacChannels !== null
    ? editorState.dacChannels
    : currentSource.value.dacChannels
)

const dacMax = computed(() => (dacBound.value === null ? 65535 : Math.max(0, dacBound.value - 1)))

const validationKey = computed<string | null>(() =>
  validateRoutingDraft(editorState.rows, {
    explicitDacChannels: editorState.explicitDacChannels ? editorState.dacChannels : undefined,
    knownDacChannels: currentSource.value.dacChannels
  })
)

const validationText = computed<string | null>(() => {
  const key = validationKey.value
  if (key === null) {
    return null
  }
  if (key === 'dac_range') {
    return t('routingEdit.edit.validation.dac_range', {
      channels: intText(dacBound.value) ?? t('common.dash')
    })
  }
  return t('routingEdit.edit.validation.' + key)
})

const inheritDacText = computed(() =>
  currentSource.value.dacChannels === null
    ? t('routingEdit.edit.dacChannelsUnknown')
    : t('routingEdit.edit.inheritDacValue', { value: intText(currentSource.value.dacChannels) ?? t('common.dash') })
)

const prefillTimeText = computed(() =>
  editorState.prefilledAt === null ? null : clockText(editorState.prefilledAt)
)

/** DAC 物理输出标签（1-based MOTU 输出位；未知位置显示 OUT n，不猜）。 */
function dacOutputLabel(dac: number): string | null {
  if (!Number.isInteger(dac) || dac < 0) {
    return null
  }
  const channel = dac + 1
  const key = motuOutputLabelKey(channel)
  return key === null ? t('routing.lanes.unknownLabel', { channel }) : t(key)
}

/** speaker_roles（§12.2 加法字段）：有则原样显示 canonical 角色，缺失/null 显示「—」。 */
function dacRoleText(dac: number): string | null {
  const role = speakerRoleForDac(channels.value, dac)
  return role === null ? null : role.role
}

function toInteger(value: unknown): number | null {
  const raw = Array.isArray(value) ? value[0] : value
  return typeof raw === 'number' && Number.isFinite(raw) ? Math.trunc(raw) : null
}

function onLaneChange(index: number, value: unknown): void {
  editor.setLane(index, toInteger(value))
}

function onDacChange(index: number, value: unknown): void {
  editor.setDac(index, toInteger(value))
}

function onExplicitDacChange(value: unknown): void {
  editor.setExplicitDacChannels(value === true)
}

function onDacChannelsChange(value: unknown): void {
  editor.setDacChannels(toInteger(value))
}

function onPrefill(): void {
  if (!currentSource.value.known) {
    return
  }
  if (editor.prefill(true)) {
    ElMessage.success(t('routingEdit.toast.prefilled'))
  }
}

function onAddRow(): void {
  editor.addRow()
}

function onClearDraft(): void {
  editor.clear()
}

/* ----------------------------------------------------------- 能力 / 两阶段 */

const capability = computed(() => metaSurfaceState('routing'))
const capabilityReason = computed(() => metaCapabilityReason({ surface: 'routing' }))
const capabilityText = computed(() => {
  if (capability.value === 'wired') {
    return t('routingEdit.meta.wired')
  }
  if (capability.value === 'not_wired') {
    return t('routingEdit.meta.notWired')
  }
  return t('routingEdit.meta.unknown')
})
const capabilityTagType = computed<'success' | 'warning' | 'danger'>(() => {
  if (capability.value === 'wired') {
    return 'success'
  }
  return capability.value === 'not_wired' ? 'danger' : 'warning'
})

const busy = computed(() => controlState.busy !== null)
const prepareBusy = computed(() => controlState.busy === 'prepare')
const controlError = computed(() => controlState.error)
const noBasePlan = computed(() => controlError.value?.category === 'no_base_plan')
const tokenInvalid = computed(() => controlError.value?.category === 'token_invalid')

const routingDraft = computed<ControlDraft | null>(() => {
  const draft = controlState.draft
  return draft !== null && draft.surface === 'routing' ? draft : null
})

const foreignDraft = computed<ControlDraft | null>(() => {
  const draft = controlState.draft
  return draft !== null && draft.surface !== 'routing' ? draft : null
})

const canPrepare = computed(
  () =>
    !busy.value &&
    routingDraft.value === null &&
    capability.value !== 'not_wired' &&
    editorState.rows.length > 0 &&
    validationKey.value === null
)

const commitReceipt = ref<ControlCommitAck | null>(null)

const commitRows = computed<KvRow[]>(() => {
  const ack = commitReceipt.value
  if (ack === null) {
    return []
  }
  return [
    {
      label: t('routingEdit.receipt.version'),
      value: intText(ack.version),
      reason: ack.version === null ? t('routingEdit.receipt.changedUnreported') : null
    },
    {
      label: t('routingEdit.receipt.appliedBlock'),
      value: intText(ack.applied_block),
      reason: ack.applied_block === null ? t('routingEdit.receipt.changedUnreported') : null
    },
    {
      label: t('routingEdit.receipt.graphId'),
      value: shortenId(ack.graph_id, 10, 6),
      copy: ack.graph_id,
      reason: ack.graph_id === null ? t('routingEdit.receipt.changedUnreported') : null
    },
    {
      label: t('routingEdit.receipt.changed'),
      value:
        ack.changed === null
          ? t('routingEdit.receipt.changedUnreported')
          : ack.changed
            ? t('common.yes')
            : t('common.no')
    },
    {
      label: t('routingEdit.receipt.at'),
      value: clockText(controlState.lastCommitAt)
    }
  ]
})

const receiptTruth = computed(() => (commitReceipt.value === null ? ('notWired' as const) : ('actual' as const)))
const draftTruth = computed(() => (routingDraft.value === null ? ('notWired' as const) : ('actual' as const)))
const editorTruth = computed(() => (capability.value === 'not_wired' ? ('notWired' as const) : ('actual' as const)))
const sourceTruth = computed(() => (currentSource.value.known ? ('actual' as const) : ('notWired' as const)))

async function onPrepare(): Promise<void> {
  const error = editor.validate()
  if (error !== null || capability.value === 'not_wired' || routingDraft.value !== null) {
    return
  }
  const draft = await prepareRouting(editor.buildPatch())
  if (draft !== null) {
    // 新草稿会替换旧 intent：上一次 commit 回执不再代表当前草稿，先清掉避免误读。
    commitReceipt.value = null
    ElMessage.success(t('routingEdit.toast.prepared'))
  }
}

async function onCommit(): Promise<void> {
  if (routingDraft.value === null) {
    return
  }
  const ack = await commitControlDraft()
  if (ack === null) {
    return
  }
  commitReceipt.value = ack
  ElMessage.success(t('routingEdit.toast.committed'))
  await Promise.all([refreshRouting(), refreshStatus()])
  // commit 成功后当前值可能变了：强制按新的 /api/channels.mapping 重建草稿。
  editor.prefill(true)
}

async function onAbort(): Promise<void> {
  if (routingDraft.value === null) {
    return
  }
  const ack = await abortControlDraft()
  if (ack !== null) {
    ElMessage.success(t('routingEdit.toast.aborted'))
  }
}

/* ------------------------------------------------------------------ 操作 */

function openChannels(): void {
  void router.push({ name: 'channels' })
}

function onRefresh(): void {
  void refreshRouting()
  void refreshStatus()
}

void ensureMeta()
void loadRouting()
</script>
<template>
  <div class="routing-page">
    <PageHeader title-key="routing.pageTitle" subtitle-key="routingEdit.subtitle">
      <template #actions>
        <span class="routing-page__updated">{{ lastUpdateText }}</span>
        <el-button
          type="primary"
          plain
          :loading="refreshing"
          :disabled="refreshing"
          :aria-label="t('routing.actions.refresh')"
          @click="onRefresh"
        >
          <el-icon v-if="!refreshing"><Refresh /></el-icon>
          <span>{{ t('routing.actions.refresh') }}</span>
        </el-button>
      </template>
    </PageHeader>

    <!-- 状态整体不可用：保留重试入口，不渲染假数据 -->
    <div v-if="statusFatal" class="banner banner--danger" role="alert">
      <span>{{ t('routing.statusUnavailable') }}</span>
      <el-button size="small" @click="onRefresh">{{ t('common.retry') }}</el-button>
    </div>

    <!-- /api/channels 不可用：当前值无法读取（不展示假映射，也不用旧值冒充当前值） -->
    <div v-if="channelsFatal" class="banner banner--warn routing-page__not-wired" role="status">
      <div class="routing-page__not-wired-text">
        <span class="routing-page__not-wired-title">{{ t('routingEdit.current.unavailableTitle') }}</span>
        <span>
          {{ t('routingEdit.current.unavailableDesc', { message: t(channelsError!.i18nKey) }) }}
        </span>
        <span v-if="channelsError!.detail" class="aux-text mono">{{ channelsError!.code }} — {{ channelsError!.detail }}</span>
      </div>
      <el-button size="small" @click="onRefresh">{{ t('common.retry') }}</el-button>
    </div>

    <p v-for="banner in notices" :key="banner.text" class="banner" :class="'banner--' + banner.tone" role="status">
      {{ banner.text }}
    </p>

    <ControlErrorNotice :error="controlError" :show-retry="false" />

    <!-- no_base_plan（409）：routing 补丁需要已生效方案作为基线 -->
    <div v-if="noBasePlan" class="banner banner--warn routing-page__guide" role="status">
      <span>{{ t('routingEdit.error.noBasePlanGuide') }}</span>
      <el-button size="small" @click="openChannels">{{ t('routingEdit.error.goChannels') }}</el-button>
    </div>
    <!-- engine_token_invalid（409）：control store 已清掉本地草稿 -->
    <div v-if="tokenInvalid" class="banner banner--warn routing-page__guide" role="status">
      <span>{{ t('routingEdit.error.tokenInvalidGuide') }}</span>
      <el-button size="small" @click="onRefresh">{{ t('common.retry') }}</el-button>
    </div>

    <div v-if="data" class="status-band">
      <StatusBadge :kind="hostLinkBadge.kind" :label="t('routing.band.hostLink')" :value="hostLinkBadge.value" />
      <span class="status-band__divider" aria-hidden="true" />
      <StatusBadge :kind="runStateBadge.kind" :label="t('routing.band.runState')" :value="runStateBadge.value" />
      <span class="status-band__divider" aria-hidden="true" />
      <span class="routing-page__band-item">
        <span class="routing-page__band-label">{{ t('routing.band.planVersion') }}</span>
        <span class="mono">{{ planVersionText ?? t('common.dash') }}</span>
      </span>
      <span class="status-band__divider" aria-hidden="true" />
      <span class="routing-page__band-item">
        <span class="routing-page__band-label">{{ t('routing.band.matrixLanes') }}</span>
        <span class="mono">{{ matrixLanesText ?? t('common.dash') }}</span>
      </span>
      <span class="status-band__divider" aria-hidden="true" />
      <span class="routing-page__band-item">
        <span class="routing-page__band-label">{{ t('routing.band.outputChannels') }}</span>
        <span class="mono">{{ outputChannelsText ?? t('common.dash') }}</span>
      </span>
      <span class="status-band__divider" aria-hidden="true" />
      <span class="routing-page__band-item">
        <span class="routing-page__band-label">{{ t('routing.band.routedLanes') }}</span>
        <span class="mono">{{ routedLanesText ?? t('common.dash') }}</span>
      </span>
      <span class="status-band__meta">{{ lastUpdateText }}</span>
    </div>

    <div v-if="hasFacts" class="grid-12 routing-grid">
      <!-- ① 只读诊断：输入 → 矩阵域 → DAC 输出 -->
      <PanelCard
        class="routing-grid__span-12"
        title-key="routing.chain.title"
        desc-key="routing.chain.desc"
        :truth="chainTruth"
      >
        <div class="routing-chain">
          <section class="routing-chain__node" :aria-label="t('routing.chain.input.title')">
            <header class="routing-chain__head">
              <span class="routing-chain__step" aria-hidden="true">1</span>
              <h3 class="routing-chain__title">{{ t('routing.chain.input.title') }}</h3>
            </header>
            <ul class="kv-list">
              <StatRow
                :label="t('routing.chain.input.channels')"
                :value="intText(chain.input.channels.value)"
                :reason="chain.input.channels.reason ?? (channels === null ? noChannelsReason : null)"
              />
              <StatRow
                :label="t('routing.chain.input.map')"
                :value="mapText(chain.input.map.value)"
                :reason="chain.input.map.reason ?? (channels === null ? noChannelsReason : null)"
              />
              <StatRow
                :label="t('routing.chain.input.source')"
                :value="sourceText(chain.input.source.value)"
                :reason="chain.input.source.reason ?? (channels === null ? noChannelsReason : null)"
              />
            </ul>
          </section>

          <div class="routing-chain__arrow" aria-hidden="true">
            <el-icon :size="18">&#8594;</el-icon>
          </div>

          <section class="routing-chain__node" :aria-label="t('routing.chain.matrix.title')">
            <header class="routing-chain__head">
              <span class="routing-chain__step" aria-hidden="true">2</span>
              <h3 class="routing-chain__title">{{ t('routing.chain.matrix.title') }}</h3>
            </header>
            <ul class="kv-list">
              <StatRow
                :label="t('routing.chain.matrix.lanes')"
                :value="matrixLanesText"
                :suffix="t('routing.chain.matrix.expectedSuffix', { lanes: ROUTING_MATRIX_LANES })"
                :reason="chain.matrix.lanes.reason"
              />
              <StatRow
                :label="t('routing.chain.matrix.routedLanes')"
                :value="intText(chain.matrix.routedLanes.value)"
                :reason="chain.matrix.routedLanes.reason ?? (channels === null ? noChannelsReason : null)"
              />
              <StatRow
                :label="t('routing.chain.matrix.routes')"
                :value="intText(chain.matrix.routes.value)"
                :reason="chain.matrix.routes.reason ?? (channels === null ? noChannelsReason : null)"
              />
            </ul>
          </section>

          <div class="routing-chain__arrow" aria-hidden="true">
            <el-icon :size="18">&#8594;</el-icon>
          </div>

          <section class="routing-chain__node" :aria-label="t('routing.chain.output.title')">
            <header class="routing-chain__head">
              <span class="routing-chain__step" aria-hidden="true">3</span>
              <h3 class="routing-chain__title">{{ t('routing.chain.output.title') }}</h3>
            </header>
            <ul class="kv-list">
              <StatRow
                :label="t('routing.chain.output.channels')"
                :value="outputChannelsText"
                :reason="chain.output.channels.reason ?? (channels === null ? noChannelsReason : null)"
              />
              <StatRow
                :label="t('routing.chain.output.dacMap')"
                :value="mapText(chain.output.dacMap.value)"
                :copy="chain.output.dacMap.value === null ? null : mapText(chain.output.dacMap.value)"
                :reason="chain.output.dacMap.reason ?? (channels === null ? noChannelsReason : null)"
              />
              <StatRow
                :label="t('routing.chain.output.knownLabels')"
                :value="t('routing.chain.output.knownLabelsValue', { count: intText(chain.output.knownLabels) ?? '0' })"
                :reason="null"
              />
            </ul>
            <p class="routing-chain__labels aux-text">{{ knownLabelsText }}</p>
          </section>
        </div>
        <p class="aux-text routing-chain__note">{{ t('routing.chain.note') }}</p>
      </PanelCard>

      <!-- ② 当前值来源（/api/channels.mapping；标注来源/语义/版本/读取时间） -->
      <PanelCard
        class="routing-grid__span-5"
        title-key="routingEdit.current.title"
        desc-key="routingEdit.current.desc"
        :truth="sourceTruth"
      >
        <div v-if="!currentSource.known" class="banner banner--warn routing-current__unknown" role="status">
          <div class="routing-current__unknown-text">
            <strong>{{ t('routingEdit.current.unknownTitle') }}</strong>
            <span>{{ currentUnknownReason }}</span>
          </div>
          <TruthTag kind="notWired" :reason="currentUnknownReason" />
        </div>
        <p
          v-else-if="currentSource.rows !== null && currentSource.rows.length === 0"
          class="aux-text routing-current__known-zero"
        >
          {{ t('routingEdit.current.knownZero') }}
        </p>
        <ul class="kv-list">
          <StatRow
            v-for="row in currentRows"
            :key="row.label"
            :label="row.label"
            :value="row.value"
            :copy="row.copy ?? null"
            :reason="row.reason ?? null"
            :tone="row.tone ?? null"
          />
        </ul>
        <p v-if="currentSource.known" class="aux-text">{{ t('routingEdit.current.knownHint') }}</p>
        <p v-if="channelsError !== null && channels !== null" class="aux-text">
          {{ t('routingEdit.current.staleHint', { message: t(channelsError.i18nKey) }) }}
        </p>
      </PanelCard>

      <!-- ③ 可编辑 lane → DAC 两阶段 -->
      <PanelCard
        class="routing-grid__span-7"
        title-key="routingEdit.edit.title"
        desc-key="routingEdit.edit.desc"
        :truth="editorTruth"
      >
        <template #actions>
          <span class="aux-text">{{ t('routingEdit.edit.toolbar.count', { count: editorState.rows.length }) }}</span>
        </template>

        <div class="routing-edit__capability">
          <span class="aux-text">{{ t('routingEdit.meta.label') }}</span>
          <el-tag size="small" :type="capabilityTagType">{{ capabilityText }}</el-tag>
        </div>
        <p v-if="capability === 'not_wired' && capabilityReason !== null" class="banner banner--warn" role="status">
          {{ t('routingEdit.meta.reason', { reason: capabilityReason }) }}
        </p>

        <div v-if="!currentSource.known" class="banner banner--warn routing-current__unknown" role="status">
          <span>{{ t('routingEdit.edit.unknownCurrent', { reason: currentUnknownReason }) }}</span>
        </div>

        <div class="routing-edit__toolbar">
          <el-button
            size="small"
            :disabled="busy || !currentSource.known"
            :title="currentSource.known ? undefined : t('routingEdit.current.unknownTitle')"
            @click="onPrefill"
          >
            {{ t('routingEdit.edit.toolbar.prefill') }}
          </el-button>
          <el-button size="small" :disabled="busy" @click="onAddRow">
            <el-icon><Plus /></el-icon>
            <span>{{ t('routingEdit.edit.toolbar.add') }}</span>
          </el-button>
          <el-button size="small" :disabled="busy || editorState.rows.length === 0" @click="onClearDraft">
            {{ t('routingEdit.edit.toolbar.clear') }}
          </el-button>
        </div>

        <p v-if="editorState.dirty" class="aux-text">{{ t('routingEdit.edit.editedHint') }}</p>
        <p v-else-if="prefillTimeText !== null" class="aux-text">
          {{ t('routingEdit.edit.prefilled', { time: prefillTimeText }) }}
        </p>

        <div v-if="editorState.rows.length > 0" class="routing-table__wrap">
          <table class="routing-table">
            <thead>
              <tr>
                <th>{{ t('routingEdit.edit.table.lane') }}</th>
                <th>{{ t('routingEdit.edit.table.dac') }}</th>
                <th>{{ t('routingEdit.edit.table.output') }}</th>
                <th>{{ t('routingEdit.edit.table.role') }}</th>
                <th>{{ t('routingEdit.edit.table.actions') }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(row, index) in editorState.rows" :key="index">
                <td>
                  <el-input-number
                    size="small"
                    :model-value="row.lane"
                    :min="0"
                    :step="1"
                    :precision="0"
                    :disabled="busy"
                    :aria-label="t('routingEdit.edit.table.lane')"
                    @change="onLaneChange(index, $event)"
                  />
                </td>
                <td>
                  <el-input-number
                    size="small"
                    :model-value="row.dac"
                    :min="0"
                    :max="dacMax"
                    :step="1"
                    :precision="0"
                    :disabled="busy"
                    :aria-label="t('routingEdit.edit.table.dac')"
                    @change="onDacChange(index, $event)"
                  />
                </td>
                <td class="mono">{{ dacOutputLabel(row.dac) ?? t('common.dash') }}</td>
                <td class="mono">{{ dacRoleText(row.dac) ?? t('common.dash') }}</td>
                <td>
                  <el-button
                    size="small"
                    text
                    type="danger"
                    :disabled="busy"
                    :aria-label="t('routingEdit.edit.table.remove')"
                    @click="editor.removeRow(index)"
                  >
                    <el-icon><Delete /></el-icon>
                  </el-button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-else class="aux-text">{{ t('routingEdit.edit.noRows') }}</p>

        <div class="routing-edit__dac">
          <el-checkbox
            :model-value="editorState.explicitDacChannels"
            :disabled="busy"
            @change="onExplicitDacChange"
          >
            {{ t('routingEdit.edit.explicitDac') }}
          </el-checkbox>
          <template v-if="editorState.explicitDacChannels">
            <span class="routing-edit__label">{{ t('routingEdit.edit.dacChannels') }}</span>
            <el-input-number
              size="small"
              :model-value="editorState.dacChannels"
              :min="1"
              :step="1"
              :precision="0"
              :disabled="busy"
              :aria-label="t('routingEdit.edit.dacChannels')"
              @change="onDacChannelsChange"
            />
          </template>
          <span v-else class="aux-text">{{ t('routingEdit.edit.inheritDac') }} · {{ inheritDacText }}</span>
        </div>
        <p class="aux-text">{{ t('routingEdit.edit.dacChannelsHint') }}</p>

        <p v-if="validationText !== null" class="routing-edit__invalid" role="alert">{{ validationText }}</p>

        <div class="routing-edit__actions">
          <el-button type="primary" :loading="prepareBusy" :disabled="!canPrepare" @click="onPrepare">
            {{ prepareBusy ? t('routingEdit.edit.preparing') : t('routingEdit.edit.prepare') }}
          </el-button>
          <span v-if="capability === 'not_wired'" class="aux-text">{{ t('routingEdit.edit.prepareBlockedMeta') }}</span>
          <span v-else-if="routingDraft !== null" class="aux-text">{{ t('routingEdit.edit.prepareBlockedDraft') }}</span>
          <span v-else-if="foreignDraft !== null" class="aux-text">
            {{ t('routingEdit.edit.foreignDraft', { surface: foreignDraft.surface ?? t('common.unknown') }) }}
          </span>
        </div>
        <p v-if="capability === 'not_wired' && capabilityReason !== null" class="aux-text">
          {{ t('routingEdit.edit.prepareBlockedMetaReason', { reason: capabilityReason }) }}
        </p>
        <p v-else-if="capability === 'unknown'" class="aux-text">{{ t('routingEdit.meta.unknown') }}</p>
        <p class="aux-text">{{ t('routingEdit.edit.laneHint', { lanes: ROUTING_MATRIX_LANES }) }}</p>
      </PanelCard>

      <!-- ④ 草稿审查 + commit/abort -->
      <PanelCard
        class="routing-grid__span-7"
        title-key="routingEdit.draft.title"
        desc-key="routingEdit.draft.desc"
        :truth="draftTruth"
      >
        <DraftReview
          v-if="routingDraft !== null"
          :draft="routingDraft"
          :busy="busy"
          @commit="onCommit"
          @abort="onAbort"
        />
        <template v-else>
          <p class="aux-text">{{ t('routingEdit.draft.empty') }}</p>
          <p class="aux-text">{{ t('routingEdit.draft.abortHint') }}</p>
        </template>
      </PanelCard>

      <!-- ⑤ commit 回执（以服务端回执为准） -->
      <PanelCard
        class="routing-grid__span-5"
        title-key="routingEdit.receipt.title"
        desc-key="routingEdit.receipt.desc"
        :truth="receiptTruth"
      >
        <ul v-if="commitRows.length > 0" class="kv-list">
          <StatRow
            v-for="row in commitRows"
            :key="row.label"
            :label="row.label"
            :value="row.value"
            :copy="row.copy ?? null"
            :reason="row.reason ?? null"
          />
        </ul>
        <p v-else class="aux-text">{{ t('routingEdit.receipt.empty') }}</p>
      </PanelCard>

      <!-- ⑥ 只读诊断：计划内 routing 段摘要 -->
      <PanelCard
        class="routing-grid__span-5"
        title-key="routing.plan.title"
        desc-key="routing.plan.desc"
        :truth="planTruth"
      >
        <ul class="kv-list">
          <StatRow
            :label="t('routing.plan.field.version')"
            :value="planVersionText"
            :reason="plan.versionReason ?? t('routing.plan.versionReason')"
          />
          <StatRow
            :label="t('routing.plan.field.latency')"
            :value="intText(plan.latencySamples)"
            :reason="plan.latencyReason ?? t('routing.plan.latencyReason')"
          />
          <StatRow
            :label="t('routing.plan.field.segments')"
            :value="intText(plan.segments)"
            :reason="plan.segmentsReason ?? t('routingEdit.plan.segmentsReason')"
          />
          <StatRow
            :label="t('routing.plan.field.graphId')"
            :value="shortenId(plan.graphId, 10, 6)"
            :copy="plan.graphId"
            :reason="statusReason('plan.graph_id') ?? channelsReason('plan')"
          />
          <StatRow
            :label="t('routing.plan.field.appliedAt')"
            :value="timeText(plan.appliedAt)"
            :reason="statusReason('plan.applied_at')"
          />
          <StatRow
            :label="t('routing.plan.field.source')"
            :value="plan.planSource"
            :reason="statusReason('plan.source')"
          />
          <StatRow
            :label="t('routing.plan.field.historyCurrent')"
            :value="intText(plan.historyCurrentVersion)"
            :reason="historyError !== null ? t(historyError.i18nKey) : history === null ? t('routing.plan.historyReason') : null"
          />
        </ul>
        <p class="aux-text routing-plan__note">{{ t('routingEdit.plan.segmentsNote') }}</p>
      </PanelCard>

      <!-- ⑦ 后端上报的未接入能力 -->
      <PanelCard
        class="routing-grid__span-7"
        title-key="routing.notWired.title"
        desc-key="routing.notWired.desc"
        :truth="notWiredTruth"
      >
        <ul v-if="notWiredEntries.length > 0" class="routing-not-wired">
          <li v-for="entry in notWiredEntries" :key="entry.source + entry.path" class="routing-not-wired__item">
            <span class="routing-not-wired__source mono">{{ entry.source }}</span>
            <span class="mono routing-not-wired__path">{{ entry.path }}</span>
            <span class="aux-text">{{ entry.reason }}</span>
          </li>
        </ul>
        <p v-else class="aux-text">{{ t('routing.notWired.empty') }}</p>
      </PanelCard>
    </div>

    <div
      v-else-if="statusState.phase === 'loading' || routingState.channelsPhase === 'loading'"
      class="banner banner--info"
      aria-busy="true"
    >
      {{ t('common.loading') }}
    </div>
  </div>
</template>

<style scoped>
.routing-page {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}

.routing-page__updated {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
  font-family: var(--khaen-font-mono);
  white-space: nowrap;
}

.routing-page__not-wired {
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
}

.routing-page__not-wired-text {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
  overflow-wrap: anywhere;
}

.routing-page__not-wired-title {
  font-weight: 600;
}

.routing-page__guide {
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
}

.routing-page__band-item {
  display: inline-flex;
  align-items: baseline;
  gap: 6px;
}

.routing-page__band-label {
  color: var(--khaen-text-secondary);
  font-size: 11px;
}

.routing-grid__span-12 {
  grid-column: span 12;
}

.routing-grid__span-7 {
  grid-column: span 7;
}

.routing-grid__span-5 {
  grid-column: span 5;
}

/* --------------------------------------------------------------- 路由链 */

.routing-chain {
  display: flex;
  align-items: stretch;
  gap: var(--khaen-space-2);
}

.routing-chain__node {
  flex: 1 1 0;
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  min-width: 0;
  padding: var(--khaen-space-3);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel-hover);
}

.routing-chain__head {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
}

.routing-chain__step {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  border-radius: 50%;
  background: var(--khaen-accent-soft);
  color: var(--khaen-accent);
  font-size: 11px;
  font-weight: 600;
  flex: none;
}

.routing-chain__title {
  font-size: var(--khaen-font-aux);
  font-weight: 600;
  color: var(--khaen-text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.routing-chain__arrow {
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--khaen-text-muted);
  flex: none;
}

.routing-chain__labels {
  overflow-wrap: anywhere;
}

.routing-chain__note {
  margin-top: var(--khaen-space-2);
}

/* ----------------------------------------------------------- 当前值来源 */

.routing-current__unknown {
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.routing-current__unknown-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
  overflow-wrap: anywhere;
}

.routing-current__known-zero {
  color: var(--khaen-text-secondary);
}

/* --------------------------------------------------------------- 编辑表 */

.routing-edit__capability {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.routing-edit__toolbar {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.routing-edit__dac {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.routing-edit__label {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.routing-edit__invalid {
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
}

.routing-edit__actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.routing-edit__actions :deep(.el-button) {
  min-height: 36px;
}

.routing-table__wrap {
  max-height: 420px;
  overflow: auto;
}

.routing-table {
  width: 100%;
  min-width: 420px;
  border-collapse: collapse;
  font-size: var(--khaen-font-aux);
}

.routing-table th {
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

.routing-table td {
  padding: 6px 8px;
  border-bottom: 1px dashed var(--khaen-border);
  overflow-wrap: anywhere;
}

/* ------------------------------------------------------------- 计划摘要 */

.routing-plan__note {
  margin-top: var(--khaen-space-2);
}

/* --------------------------------------------------------- 未接入条目 */

.routing-not-wired {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  margin: 0;
  padding: 0;
  list-style: none;
}

.routing-not-wired__item {
  display: flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.routing-not-wired__source {
  color: var(--khaen-text-muted);
  font-size: 11px;
  flex: none;
}

.routing-not-wired__path {
  flex: none;
}

@media (max-width: 1279px) {
  .routing-grid__span-7,
  .routing-grid__span-5 {
    grid-column: span 12;
  }
}

@media (max-width: 767px) {
  .routing-chain {
    flex-direction: column;
  }

  .routing-chain__arrow {
    transform: rotate(90deg);
    padding: 2px 0;
  }
}
</style>
