<script setup lang="ts">
/**
 * 路由矩阵页（M3b / 任务 T58）——「输入 → 矩阵域 → DAC 输出」的只读诊断。
 *
 * 数据来源（页面唯一事实源，缺失即「未接入」+ 原因）：
 * - GET /api/status（5s 轮询，共享 store）：format.channels、format.aes67.{channels,map}、
 *   plan.{version,latency_samples,graph_id,applied_at,source}、host_link、not_wired/reasons；
 * - GET /api/channels：output.channels/dac_map、mapping[{lane,dac}]、input.*、
 *   plan.{source,version,graph_id,routing_known}、reasons/not_wired；
 * - GET /api/plan/history：current_version + entries（当前版本、latency 的回退来源、applied_at）。
 *
 * 诚实边界：
 * - 没有 mapping 时不假设「lane i → DAC i」的直通映射（不编造路由）；
 * - 计划区段数（segments）没有只读数据源（daemon 只在 /api/plan/upload 回执里报告）：
 *   如实显示 NOT_AVAILABLE + 原因，不拿路由条目数冒充；
 * - 路由编辑/重排 NOT_AVAILABLE：需要 PlanGraph 编辑器/designer（T59）；控制面低层
 *   /api/surface {surface:"routing"} 片段补丁不是图形编辑器，本页不暴露写入口（不假成功）；
 * - rt-host 不可达 / host_link 断开 / 方案缺失时只显示最后成功快照 + 警示，不清空也不造假。
 *
 * 与 /channels 页的关系：映射口径一致（lane / 0-based DAC / 1-based MOTU 物理输出），
 * 编辑入口不在这里重复——页面给「去 /channels」跳转。
 */
import { computed } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { ArrowRight, Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import StatRow from '@/components/StatRow.vue'
import StatusBadge from '@/components/StatusBadge.vue'
import TruthTag from '@/components/TruthTag.vue'
import { useStatusPolling } from '@/composables/useStatusPolling'
import { isStatusStale, reasonForPath, runStateKind, runStateLabelKey } from '@/api/status'
import { MOTU_KNOWN_OUTPUT_NUMBERS, motuOutputLabelKey } from '@/api/channels'
import {
  ROUTING_EDIT_TASK,
  ROUTING_MATRIX_LANES,
  buildPlanSummary,
  buildRoutingChain,
  laneRows,
  reasonForRoutingChannelsPath,
  type RoutingLaneRow
} from '@/api/routing'
import { loadRouting, refreshRouting, routingState } from '@/stores/routing'
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
const rows = computed(() => laneRows(channels.value))

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

function laneLabel(row: RoutingLaneRow): string | null {
  if (row.labelKey !== null) {
    return t(row.labelKey)
  }
  if (row.physical === null) {
    return null
  }
  return t('routing.lanes.unknownLabel', { channel: row.physical })
}

function statusReason(path: string): string | null {
  return reasonForPath(data.value, path)
}

function channelsReason(path: string): string | null {
  return reasonForRoutingChannelsPath(channels.value, path)
}

/** /api/channels 整体不可用时的兜底原因（字段级 reason 缺失时用）。 */
const noChannelsReason = computed(() =>
  channelsError.value !== null ? t(channelsError.value.i18nKey) : t('routing.unavailable.reason')
)

const lastUpdateText = computed(() => {
  if (routingState.loadedAt === null) {
    return t('routing.updated.never')
  }
  const time = clockText(routingState.loadedAt)
  return time === null ? t('routing.updated.never') : t('routing.updated.last', { time })
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
  chain.value.output.channels.value !== null ||
  rows.value.length > 0
    ? ('actual' as const)
    : ('notWired' as const)
)

const lanesTruth = computed(() => (rows.value.length > 0 ? ('actual' as const) : ('notWired' as const)))

const planTruth = computed(() =>
  plan.value.version !== null ||
  plan.value.latencySamples !== null ||
  plan.value.graphId !== null ||
  plan.value.dacChannels !== null
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

/* ------------------------------------------------------------------ 操作 */

function openChannels(): void {
  void router.push({ name: 'channels' })
}

function onRefresh(): void {
  void refreshRouting()
  void refreshStatus()
}

void loadRouting()
</script>

<template>
  <div class="routing-page">
    <PageHeader title-key="routing.pageTitle" subtitle-key="routing.subtitle">
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

    <!-- /api/channels 未接入：整块 NOT_AVAILABLE + 稳定码 + 原因 + 任务号（T55） -->
    <div v-if="channelsFatal" class="banner banner--warn routing-page__not-wired" role="status">
      <div class="routing-page__not-wired-text">
        <span class="routing-page__not-wired-title">{{ t('routing.unavailable.title') }}</span>
        <span>{{ t('routing.unavailable.desc', { task: 'T55', message: t(channelsError!.i18nKey) }) }}</span>
        <span v-if="channelsError!.detail" class="aux-text mono">{{ channelsError!.code }} — {{ channelsError!.detail }}</span>
      </div>
      <el-button size="small" @click="onRefresh">{{ t('common.retry') }}</el-button>
    </div>

    <p
      v-for="banner in notices"
      :key="banner.text"
      class="banner"
      :class="'banner--' + banner.tone"
      role="status"
    >
      {{ banner.text }}
    </p>

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
      <!-- ① 输入 → 矩阵域 → DAC 输出 -->
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
            <el-icon :size="18"><ArrowRight /></el-icon>
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
            <el-icon :size="18"><ArrowRight /></el-icon>
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

      <!-- ② lane -> DAC 映射表（与 /channels 同口径，不重复编辑入口） -->
      <PanelCard
        class="routing-grid__span-7"
        title-key="routing.lanes.title"
        desc-key="routing.lanes.desc"
        :truth="lanesTruth"
      >
        <template #actions>
          <el-button size="small" @click="openChannels">
            <span>{{ t('routing.lanes.openChannels') }}</span>
            <el-icon><ArrowRight /></el-icon>
          </el-button>
        </template>
        <ul class="kv-list routing-lanes__summary">
          <StatRow
            :label="t('routing.lanes.routedLanes')"
            :value="intText(chain.matrix.routedLanes.value)"
            :reason="chain.matrix.routedLanes.reason ?? (channels === null ? noChannelsReason : null)"
          />
          <StatRow
            :label="t('routing.lanes.routes')"
            :value="intText(chain.matrix.routes.value)"
            :reason="chain.matrix.routes.reason ?? (channels === null ? noChannelsReason : null)"
          />
        </ul>
        <div v-if="rows.length > 0" class="routing-table__wrap">
          <table class="routing-table">
            <thead>
              <tr>
                <th>{{ t('routing.lanes.table.lane') }}</th>
                <th>{{ t('routing.lanes.table.dac') }}</th>
                <th>{{ t('routing.lanes.table.physical') }}</th>
                <th>{{ t('routing.lanes.table.label') }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="row in rows" :key="row.lane">
                <td class="mono">{{ intText(row.lane) }}</td>
                <td class="mono">{{ row.dac === null ? t('common.dash') : intText(row.dac) }}</td>
                <td class="mono">{{ row.physical === null ? t('common.dash') : intText(row.physical) }}</td>
                <td>{{ laneLabel(row) ?? t('common.dash') }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-else class="aux-text routing-lanes__empty">
          <TruthTag kind="notWired" :reason="channelsReason('mapping') ?? noChannelsReason" />
          <span>{{ t('routing.lanes.empty') }}</span>
        </p>
        <p class="aux-text">{{ t('routing.lanes.editHint') }}</p>
      </PanelCard>

      <!-- ③ 计划内 routing 段摘要 -->
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
            :reason="plan.segmentsReason ?? t(plan.segmentsReasonKey)"
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
            :label="t('routing.plan.field.routingKnown')"
            :value="plan.routingKnown === null ? null : plan.routingKnown ? t('common.yes') : t('common.no')"
            :reason="plan.routingKnownReason"
          />
          <StatRow
            :label="t('routing.plan.field.dacChannels')"
            :value="intText(plan.dacChannels)"
            :reason="channelsReason('output.channels')"
          />
          <StatRow
            :label="t('routing.plan.field.routes')"
            :value="intText(plan.routes)"
            :reason="channelsReason('mapping')"
          />
          <StatRow
            :label="t('routing.plan.field.historyCurrent')"
            :value="intText(plan.historyCurrentVersion)"
            :reason="historyError !== null ? t(historyError.i18nKey) : history === null ? t('routing.plan.historyReason') : null"
          />
        </ul>
        <p class="aux-text routing-plan__note">{{ t('routing.plan.segmentsNote') }}</p>
      </PanelCard>

      <!-- ④ 路由编辑/重排：NOT_AVAILABLE（T59），不假成功 -->
      <PanelCard
        class="routing-grid__span-12"
        title-key="routing.edit.title"
        desc-key="routing.edit.desc"
        truth="notWired"
      >
        <div class="banner banner--warn routing-edit__banner" role="status">
          <div class="routing-edit__banner-text">
            <strong>{{ t('routing.edit.notAvailableTitle', { task: ROUTING_EDIT_TASK }) }}</strong>
            <span>{{ t('routing.edit.notAvailableDesc', { task: ROUTING_EDIT_TASK }) }}</span>
          </div>
          <TruthTag kind="notWired" :reason="t('routing.edit.reason')" />
        </div>
        <div class="routing-edit__actions">
          <el-button disabled :title="t('routing.edit.disabledHint')">{{ t('routing.edit.action.reorder') }}</el-button>
          <el-button disabled :title="t('routing.edit.disabledHint')">
            {{ t('routing.edit.action.crosspoint') }}
          </el-button>
          <el-button disabled :title="t('routing.edit.disabledHint')">{{ t('routing.edit.action.apply') }}</el-button>
          <el-button @click="openChannels">{{ t('routing.edit.action.channels') }}</el-button>
        </div>
        <ul class="routing-edit__requirements">
          <li>{{ t('routing.edit.requirement.editor', { task: ROUTING_EDIT_TASK }) }}</li>
          <li>{{ t('routing.edit.requirement.patch') }}</li>
          <li>{{ t('routing.edit.requirement.commit', { task: 'T52' }) }}</li>
        </ul>
      </PanelCard>

      <!-- ⑤ 后端上报的未接入能力 -->
      <PanelCard
        class="routing-grid__span-12"
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

    <div v-else-if="statusState.phase === 'loading' || routingState.channelsPhase === 'loading'" class="banner banner--info" aria-busy="true">
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

/* --------------------------------------------------------------- 映射表 */

.routing-lanes__summary {
  margin-bottom: var(--khaen-space-1);
}

.routing-table__wrap {
  max-height: 420px;
  overflow: auto;
}

.routing-table {
  width: 100%;
  min-width: 360px;
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

.routing-lanes__empty {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

/* ------------------------------------------------------------- 计划摘要 */

.routing-plan__note {
  margin-top: var(--khaen-space-2);
}

/* ----------------------------------------------------------- 编辑不可用 */

.routing-edit__banner {
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
}

.routing-edit__banner-text {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
}

.routing-edit__actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  margin-top: var(--khaen-space-1);
}

.routing-edit__requirements {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin: var(--khaen-space-2) 0 0;
  padding-left: var(--khaen-space-4);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
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
