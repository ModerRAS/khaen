<script setup lang="ts">
/**
 * 概览页（PLAN §6.2 / §11.4；任务 T35b）——接真实 `GET /api/status` 聚合。
 *
 * - 数据源唯一：daemon 的 khaen-status/v1；null 字段一律「—」+「未接入」TruthTag（tooltip 显示 reasons），
 *   不编造/不演示任何数值；请求失败保留最后一次成功数据并明确提示（错误不造假数据）。
 * - 轮询：5s；页面隐藏暂停、恢复可见立即刷新（composables/useStatusPolling）。
 * - 过期：host_link.connected=false 或 status_age_ms > 2000 -> 黄色 stale 徽标 + 警示条。
 * - 401 交给共享 ApiClient / session store 统一跳登录（本页不自行处理）。
 * - 8 张卡按 PLAN §11.4 顺序：运行状态 / 输入 / 输出 / 方案 / 格式与设备 / AES67 / 最近校准 / 控制通道。
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import StatusBadge from '@/components/StatusBadge.vue'
import StatRow from '@/components/StatRow.vue'
import TruthTag from '@/components/TruthTag.vue'
import { useStatusPolling } from '@/composables/useStatusPolling'
import {
  formatChannelMap,
  hasAnyValue,
  isStatusStale,
  reasonForPath,
  runStateKind,
  runStateLabelKey,
  underrunRate,
  underrunSeverity,
  uptimeParts,
  type StatusResponse
} from '@/api/status'
import {
  formatClock,
  formatDecimal,
  formatInteger,
  formatPercent,
  formatTimestamp,
  shortenId,
  toByteUnit,
  toRateUnit
} from '@/utils/format'
import type { StatusKind, TruthKind } from '@/types/ui'

const { t, locale } = useI18n({ useScope: 'global' })
const { state, refresh } = useStatusPolling()

type Tone = 'ok' | 'warn' | 'bad'

interface OverviewMetric {
  value: string | null
  suffix?: string | null
  tone?: Tone | null
  reason?: string | null
}

interface OverviewRow {
  label: string
  value: string | null
  suffix?: string | null
  copy?: string | null
  reason?: string | null
  tone?: Tone | null
  truth?: TruthKind | null
}

interface OverviewCard {
  key: string
  titleKey: string
  descKey: string
  truth: TruthKind
  metric: OverviewMetric
  rows: OverviewRow[]
  note?: string | null
}

interface NoticeBanner {
  tone: 'warn' | 'danger'
  text: string
}

const data = computed<StatusResponse | null>(() => state.data)
const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const initialLoading = computed(() => state.phase === 'loading' && data.value === null)
const fatalError = computed(() => state.phase === 'error' && data.value === null)
const refreshing = computed(() => state.refreshing)
const stale = computed(() => isStatusStale(data.value))

/* ------------------------------------------------------------ 格式化小工具 */

function intText(value: number | null): string | null {
  return formatInteger(value, localeId.value)
}

function rateText(hz: number | null): string | null {
  const unit = toRateUnit(hz)
  if (unit === null) {
    return null
  }
  const value = formatDecimal(unit.value, 2, localeId.value)
  return value === null ? null : value + ' ' + t(unit.unitKey)
}

function bytesText(bytes: number | null): string | null {
  const unit = toByteUnit(bytes)
  if (unit === null) {
    return null
  }
  const digits = unit.unitKey === 'unit.bytes' ? 0 : 2
  const value = formatDecimal(unit.value, digits, localeId.value)
  return value === null ? null : value + ' ' + t(unit.unitKey)
}

function boolText(value: boolean | null): string | null {
  if (value === null) {
    return null
  }
  return t(value ? 'common.yes' : 'common.no')
}

function percentText(rate: number | null): string | null {
  return formatPercent(rate, localeId.value)
}

function timeText(iso: string | null): string | null {
  return formatTimestamp(iso, localeId.value)
}

function clockText(epochMs: number | null): string | null {
  return formatClock(epochMs, localeId.value)
}

function msText(ms: number | null): string | null {
  const value = formatInteger(ms, localeId.value)
  return value === null ? null : value + ' ' + t('unit.ms')
}

function stateText(): string | null {
  const current = data.value?.state ?? null
  const key = runStateLabelKey(current)
  if (key !== null) {
    return t(key)
  }
  return current !== null && current.value !== null ? current.value : null
}

const PLAN_SOURCE_KEYS: Record<string, string> = {
  upload: 'overview.plan.source.upload',
  calibration: 'overview.plan.source.calibration'
}

const DEVICE_STATE_KEYS: Record<string, string> = {
  present: 'overview.device.present',
  absent: 'overview.device.absent'
}

function planSourceText(source: string | null): string | null {
  if (source === null) {
    return null
  }
  const key = PLAN_SOURCE_KEYS[source]
  return key === undefined ? source : t(key)
}

function deviceStateText(deviceState: string | null): string | null {
  if (deviceState === null) {
    return null
  }
  const key = DEVICE_STATE_KEYS[deviceState]
  return key === undefined ? deviceState : t(key)
}

function truthOf(...values: readonly unknown[]): TruthKind {
  return hasAnyValue(values) ? 'actual' : 'notWired'
}

/** 校准的「没有记录」与「拿不到」要区分：没有原因 => 真的没有记录。 */
function calibrationValue(current: StatusResponse, value: string | null, path: string): string | null {
  if (value !== null) {
    return value
  }
  return reasonForPath(current, path) === null ? t('common.none') : null
}

/* -------------------------------------------------------------- 顶部状态带 */

const runStateBadge = computed<{ kind: StatusKind; value: string | undefined }>(() => {
  const current = data.value?.state ?? null
  const value = runStateLabelKey(current)
  const fallback = current !== null && current.value !== null ? current.value : undefined
  return { kind: runStateKind(current), value: value !== null ? t(value) : fallback }
})

const hostHealth = computed<'ok' | 'stale' | 'disconnected' | 'unknown'>(() => {
  if (data.value === null) {
    return 'unknown'
  }
  if (data.value.host_link.connected === false) {
    return 'disconnected'
  }
  if (isStatusStale(data.value)) {
    return 'stale'
  }
  return data.value.host_link.connected === true ? 'ok' : 'unknown'
})

const hostBadge = computed<{ kind: StatusKind; value: string | undefined }>(() => {
  switch (hostHealth.value) {
    case 'ok':
      return { kind: 'running', value: t('overview.hostLink.connected') }
    case 'stale':
      return { kind: 'muted', value: t('overview.hostLink.stale') }
    case 'disconnected':
      return { kind: 'muted', value: t('overview.hostLink.disconnectedStale') }
    default:
      return { kind: 'unknown', value: undefined }
  }
})

const lastErrorText = computed(() => data.value?.state.last_error ?? t('overview.band.noError'))

const lastUpdateText = computed(() => {
  if (state.loadedAt === null) {
    return t('overview.band.neverUpdated')
  }
  const time = clockText(state.loadedAt)
  return time === null ? t('overview.band.neverUpdated') : t('overview.band.lastUpdate', { time })
})

/* ------------------------------------------------------------------ 警示条 */

const notices = computed<NoticeBanner[]>(() => {
  const banners: NoticeBanner[] = []
  const current = data.value
  if (current === null) {
    return banners
  }
  if (stale.value) {
    const link = current.host_link
    const connected = boolText(link.connected) ?? t('common.dash')
    const age = msText(link.status_age_ms) ?? t('common.dash')
    banners.push({
      tone: 'warn',
      text: t('overview.notice.stale', { connected, age })
    })
  }
  if (
    current.host_link.connected === null &&
    current.host_link.status_age_ms === null &&
    current.host_link.socket === null
  ) {
    const reason = reasonForPath(current, 'host_link.connected')
    banners.push({
      tone: 'warn',
      text: reason === null ? t('overview.notice.hostLinkNotWired') : t('overview.notice.hostLinkNotWiredReason', { reason })
    })
  }
  if (state.error !== null) {
    const time = state.loadedAt === null ? t('common.dash') : clockText(state.loadedAt) ?? t('common.dash')
    banners.push({
      tone: 'warn',
      text: t('overview.notice.refreshFailed', { message: t(state.error.i18nKey), time })
    })
  }
  return banners
})

const notWiredEntries = computed(() => data.value?.not_wired ?? [])

/* ---------------------------------------------------------------- 8 张卡片 */

const cards = computed<OverviewCard[]>(() => {
  const current = data.value
  if (current === null) {
    return []
  }
  const input = current.input
  const output = current.output
  const plan = current.plan
  const format = current.format
  const aes67 = format.aes67
  const device = current.device
  const calibration = current.calibration
  const link = current.host_link
  const rate = underrunRate(input, output)
  const severity = underrunSeverity(rate)
  const rateTone: Tone | null = severity === null ? null : severity
  const rateReason = reasonForPath(current, 'input.underruns') ?? reasonForPath(current, 'output.blocks')
  const rateAssertedTone: Tone | null = format.rate_asserted === false ? 'warn' : null
  const runtimeTone: Tone | null =
    runStateKind(current.state) === 'faulted'
      ? 'bad'
      : runStateKind(current.state) === 'muted'
        ? 'warn'
        : runStateKind(current.state) === 'running'
          ? 'ok'
          : null

  return [
    {
      key: 'runtime',
      titleKey: 'overview.card.runtime.title',
      descKey: 'overview.card.runtime.desc',
      truth: truthOf(current.state.value, current.state.muted, current.state.safe, output.blocks, current.daemon.uptime_s),
      metric: { value: stateText(), tone: runtimeTone, reason: reasonForPath(current, 'state.value') },
      rows: [
        {
          label: t('overview.card.runtime.field.muted'),
          value: boolText(current.state.muted),
          reason: reasonForPath(current, 'state.muted')
        },
        {
          label: t('overview.card.runtime.field.safe'),
          value: boolText(current.state.safe),
          reason: reasonForPath(current, 'state.safe')
        },
        {
          label: t('overview.card.runtime.field.blocks'),
          value: intText(output.blocks),
          reason: reasonForPath(current, 'output.blocks')
        },
        {
          label: t('overview.card.runtime.field.uptime'),
          value: uptimeText(),
          reason: reasonForPath(current, 'daemon.uptime_s')
        }
      ]
    },
    {
      key: 'input',
      titleKey: 'overview.card.input.title',
      descKey: 'overview.card.input.desc',
      truth: truthOf(input.packets, input.underruns, input.jitter_fill, input.ssrc, input.queue_full_drops, input.recv_errors),
      metric: {
        value: intText(input.packets),
        suffix: t('unit.packets'),
        reason: reasonForPath(current, 'input.packets')
      },
      rows: [
        {
          label: t('overview.card.input.field.underruns'),
          value: intText(input.underruns),
          reason: reasonForPath(current, 'input.underruns')
        },
        {
          label: t('overview.card.input.field.underrunRate'),
          value: percentText(rate),
          tone: rateTone,
          reason: rateReason
        },
        {
          label: t('overview.card.input.field.jitterFill'),
          value: intText(input.jitter_fill),
          reason: reasonForPath(current, 'input.jitter_fill')
        },
        {
          label: t('overview.card.input.field.ssrc'),
          value: intText(input.ssrc),
          reason: reasonForPath(current, 'input.ssrc')
        },
        {
          label: t('overview.card.input.field.queueFullDrops'),
          value: intText(input.queue_full_drops),
          reason: reasonForPath(current, 'input.queue_full_drops')
        },
        {
          label: t('overview.card.input.field.recvErrors'),
          value: intText(input.recv_errors),
          reason: reasonForPath(current, 'input.recv_errors')
        }
      ]
    },
    {
      key: 'output',
      titleKey: 'overview.card.output.title',
      descKey: 'overview.card.output.desc',
      truth: truthOf(output.blocks, output.xruns, output.out_errors, output.bytes_written),
      metric: {
        value: intText(output.blocks),
        suffix: t('unit.blocks'),
        reason: reasonForPath(current, 'output.blocks')
      },
      rows: [
        {
          label: t('overview.card.output.field.xruns'),
          value: intText(output.xruns),
          reason: reasonForPath(current, 'output.xruns'),
          tone: output.xruns !== null && output.xruns > 0 ? 'bad' : null
        },
        {
          label: t('overview.card.output.field.errors'),
          value: intText(output.out_errors),
          reason: reasonForPath(current, 'output.out_errors'),
          tone: output.out_errors !== null && output.out_errors > 0 ? 'bad' : null
        },
        {
          label: t('overview.card.output.field.bytes'),
          value: bytesText(output.bytes_written),
          reason: reasonForPath(current, 'output.bytes_written')
        }
      ]
    },
    {
      key: 'plan',
      titleKey: 'overview.card.plan.title',
      descKey: 'overview.card.plan.desc',
      truth: truthOf(
        plan.graph_id,
        plan.sha256,
        plan.version,
        plan.latency_samples,
        plan.applied_at,
        plan.source
      ),
      metric: {
        value: plan.version === null ? null : 'v' + (intText(plan.version) ?? String(plan.version)),
        reason: reasonForPath(current, 'plan.version')
      },
      rows: [
        {
          label: t('overview.card.plan.field.graphId'),
          value: shortenId(plan.graph_id, 8, 4),
          copy: plan.graph_id,
          reason: reasonForPath(current, 'plan.graph_id')
        },
        {
          label: t('overview.card.plan.field.sha256'),
          value: shortenId(plan.sha256, 12, 8),
          copy: plan.sha256,
          reason: reasonForPath(current, 'plan.sha256')
        },
        {
          label: t('overview.card.plan.field.version'),
          value: intText(plan.version),
          reason: reasonForPath(current, 'plan.version')
        },
        {
          label: t('overview.card.plan.field.latencySamples'),
          value: intText(plan.latency_samples),
          reason: reasonForPath(current, 'plan.latency_samples')
        },
        {
          label: t('overview.card.plan.field.appliedAt'),
          value: timeText(plan.applied_at),
          reason: reasonForPath(current, 'plan.applied_at')
        },
        {
          label: t('overview.card.plan.field.source'),
          value: planSourceText(plan.source),
          reason: reasonForPath(current, 'plan.source')
        }
      ]
    },
    {
      key: 'format',
      titleKey: 'overview.card.format.title',
      descKey: 'overview.card.format.desc',
      truth: truthOf(
        format.plan_rate,
        format.device_rate,
        format.rate_asserted,
        format.block_frames,
        format.channels,
        format.encoding,
        device.id,
        device.state,
        device.rate_readback
      ),
      metric: {
        value: rateText(format.device_rate) ?? rateText(format.plan_rate),
        tone: rateAssertedTone,
        reason: reasonForPath(current, 'format.device_rate') ?? reasonForPath(current, 'format.plan_rate')
      },
      rows: [
        {
          label: t('overview.card.format.field.planRate'),
          value: rateText(format.plan_rate),
          reason: reasonForPath(current, 'format.plan_rate')
        },
        {
          label: t('overview.card.format.field.rateAsserted'),
          value: boolText(format.rate_asserted),
          tone: rateAssertedTone,
          reason: reasonForPath(current, 'format.rate_asserted')
        },
        {
          label: t('overview.card.format.field.blockFrames'),
          value: intText(format.block_frames),
          reason: reasonForPath(current, 'format.block_frames')
        },
        {
          label: t('overview.card.format.field.channels'),
          value: intText(format.channels),
          reason: reasonForPath(current, 'format.channels')
        },
        {
          label: t('overview.card.format.field.encoding'),
          value: format.encoding,
          reason: reasonForPath(current, 'format.encoding')
        },
        {
          label: t('overview.card.format.field.deviceId'),
          value: device.id,
          reason: reasonForPath(current, 'device.id')
        },
        {
          label: t('overview.card.format.field.deviceState'),
          value: deviceStateText(device.state),
          tone: device.state === 'absent' ? 'warn' : null,
          reason: reasonForPath(current, 'device.state')
        },
        {
          label: t('overview.card.format.field.deviceCard'),
          value: device.card,
          copy: device.card,
          reason: reasonForPath(current, 'device.card')
        },
        {
          label: t('overview.card.format.field.rateReadback'),
          value: rateText(device.rate_readback),
          reason: reasonForPath(current, 'device.rate_readback')
        }
      ]
    },
    {
      key: 'aoip',
      titleKey: 'overview.card.aoip.title',
      descKey: 'overview.card.aoip.desc',
      truth: truthOf(aes67.channels, aes67.map, aes67.pt, aes67.group, aes67.port),
      metric: {
        value: intText(aes67.channels),
        suffix: t('unit.channels'),
        reason: reasonForPath(current, 'format.aes67.channels') ?? reasonForPath(current, 'aes67.channels')
      },
      rows: [
        {
          label: t('overview.card.aoip.field.group'),
          value: aes67.group,
          reason: reasonForPath(current, 'format.aes67.group') ?? reasonForPath(current, 'aes67.group')
        },
        {
          label: t('overview.card.aoip.field.port'),
          value: intText(aes67.port),
          reason: reasonForPath(current, 'format.aes67.port') ?? reasonForPath(current, 'aes67.port')
        },
        {
          label: t('overview.card.aoip.field.pt'),
          value: intText(aes67.pt),
          reason: reasonForPath(current, 'format.aes67.pt') ?? reasonForPath(current, 'aes67.pt')
        },
        {
          label: t('overview.card.aoip.field.map'),
          value: formatChannelMap(aes67.map),
          reason: reasonForPath(current, 'format.aes67.map') ?? reasonForPath(current, 'aes67.map')
        }
      ]
    },
    {
      key: 'calibration',
      titleKey: 'overview.card.calibration.title',
      descKey: 'overview.card.calibration.desc',
      truth: truthOf(calibration.active_job, calibration.last_job_id, calibration.last_verdict),
      metric: {
        value: calibration.last_verdict ?? t('overview.calibration.none'),
        tone:
          calibration.last_verdict === 'PASS'
            ? 'ok'
            : calibration.last_verdict === 'FAIL'
              ? 'bad'
              : calibration.last_verdict === 'INCOMPLETE'
                ? 'warn'
                : null,
        reason: reasonForPath(current, 'calibration.last_verdict')
      },
      rows: [
        {
          label: t('overview.card.calibration.field.lastJob'),
          value: shortenId(calibration.last_job_id, 8, 4),
          copy: calibration.last_job_id,
          reason: reasonForPath(current, 'calibration.last_job_id')
        },
        {
          label: t('overview.card.calibration.field.activeJob'),
          value:
            calibration.active_job !== null
              ? shortenId(calibration.active_job, 8, 4)
              : calibrationValue(current, null, 'calibration.active_job'),
          copy: calibration.active_job,
          reason: reasonForPath(current, 'calibration.active_job')
        }
      ],
      note: t('overview.card.calibration.stub')
    },
    {
      key: 'control',
      titleKey: 'overview.card.control.title',
      descKey: 'overview.card.control.desc',
      truth: truthOf(link.connected, link.since, link.socket, link.status_age_ms),
      metric: {
        value: boolText(link.connected),
        tone: hostHealth.value === 'ok' ? 'ok' : hostHealth.value === 'unknown' ? null : 'warn',
        reason: reasonForPath(current, 'host_link.connected')
      },
      rows: [
        {
          label: t('overview.card.control.field.statusAgeMs'),
          value: msText(link.status_age_ms),
          tone: stale.value ? 'warn' : null,
          reason: reasonForPath(current, 'host_link.status_age_ms')
        },
        {
          label: t('overview.card.control.field.since'),
          value: timeText(link.since),
          reason: reasonForPath(current, 'host_link.since')
        },
        {
          label: t('overview.card.control.field.socket'),
          value: shortenId(link.socket, 16, 12),
          copy: link.socket,
          reason: reasonForPath(current, 'host_link.socket')
        }
      ],
      note:
        link.connected === null && link.status_age_ms === null && link.socket === null
          ? t('overview.card.control.notWiredHint')
          : null
    }
  ]
})

function uptimeText(): string | null {
  const parts = uptimeParts(data.value?.daemon.uptime_s ?? null)
  if (parts === null) {
    return null
  }
  const params = { d: parts.days, h: parts.hours, m: parts.minutes, s: parts.seconds }
  return parts.days > 0 ? t('overview.uptime.withDays', params) : t('overview.uptime.withoutDays', params)
}

function metricToneClass(tone: Tone | null | undefined): string {
  return tone === null || tone === undefined ? '' : 'metric__value--' + tone
}

function onRefresh(): void {
  void refresh()
}
</script>

<template>
  <div class="overview">
    <PageHeader title-key="overview.title" subtitle-key="overview.subtitle">
      <template #actions>
        <el-button
          type="primary"
          plain
          :loading="refreshing"
          :disabled="refreshing"
          :aria-label="t('common.refresh')"
          @click="onRefresh"
        >
          <el-icon v-if="!refreshing"><Refresh /></el-icon>
          <span>{{ t('common.refresh') }}</span>
        </el-button>
      </template>
    </PageHeader>

    <div v-if="fatalError" class="banner banner--danger overview__error" role="alert" aria-live="assertive">
      <div class="overview__error-text">
        <p class="overview__error-title">{{ t('overview.error.title') }}</p>
        <p>{{ state.error === null ? t('error.status.unknown') : t(state.error.i18nKey) }}</p>
        <p v-if="state.error && state.error.detail" class="aux-text overview__error-detail">
          {{ state.error.detail }}
        </p>
      </div>
      <el-button type="primary" :loading="refreshing" @click="onRefresh">{{ t('common.retry') }}</el-button>
    </div>

    <div v-if="initialLoading" class="card-grid" aria-busy="true" :aria-label="t('common.loading')">
      <section v-for="index in 8" :key="index" class="skeleton-card">
        <el-skeleton :rows="4" animated />
      </section>
    </div>

    <template v-else-if="data">
      <p v-for="banner in notices" :key="banner.text" class="banner" :class="'banner--' + banner.tone" role="status">
        {{ banner.text }}
      </p>

      <div class="status-band">
        <StatusBadge :kind="runStateBadge.kind" :label="t('overview.band.runState')" :value="runStateBadge.value" />
        <span class="status-band__divider" aria-hidden="true" />
        <span class="status-band__error">
          <span class="status-band__error-label">{{ t('overview.band.lastError') }}</span>
          <span
            class="status-band__error-value"
            :class="{ 'status-band__error-value--set': data.state.last_error !== null }"
          >
            {{ lastErrorText }}
          </span>
        </span>
        <span class="status-band__divider" aria-hidden="true" />
        <StatusBadge :kind="hostBadge.kind" :label="t('overview.band.hostLink')" :value="hostBadge.value" />
        <span class="status-band__meta">{{ lastUpdateText }}</span>
      </div>

      <div class="card-grid">
        <PanelCard
          v-for="card in cards"
          :key="card.key"
          :title-key="card.titleKey"
          :desc-key="card.descKey"
          :truth="card.truth"
        >
          <div class="metric">
            <span v-if="card.metric.value !== null" class="metric__value" :class="metricToneClass(card.metric.tone)">
              {{ card.metric.value }}
            </span>
            <span v-else class="metric__value metric__value--muted">{{ t('common.dash') }}</span>
            <span v-if="card.metric.suffix && card.metric.value !== null" class="metric__unit">
              {{ card.metric.suffix }}
            </span>
            <TruthTag
              v-if="card.metric.value === null"
              kind="notWired"
              :reason="card.metric.reason ?? null"
            />
          </div>
          <ul v-if="card.rows.length > 0" class="kv-list">
            <StatRow
              v-for="row in card.rows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :suffix="row.suffix ?? null"
              :copy="row.copy ?? null"
              :reason="row.reason ?? null"
              :tone="row.tone ?? null"
              :truth="row.truth ?? null"
            />
          </ul>
          <p v-if="card.note" class="aux-text">{{ card.note }}</p>
        </PanelCard>
      </div>

      <section v-if="notWiredEntries.length > 0" class="banner banner--info not-wired">
        <div class="not-wired__head">
          <strong>{{ t('overview.notWiredList.title') }}</strong>
          <span class="aux-text">{{ t('overview.notWiredList.desc') }}</span>
        </div>
        <ul class="not-wired__list">
          <li v-for="entry in notWiredEntries" :key="entry.path" class="not-wired__item">
            <code class="mono not-wired__path">{{ entry.path }}</code>
            <span class="not-wired__reason">{{ entry.reason }}</span>
          </li>
        </ul>
      </section>
    </template>
  </div>
</template>

<style scoped>
.overview {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}

.overview__error {
  align-items: flex-start;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--khaen-space-3);
}

.overview__error-text {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
  overflow-wrap: anywhere;
}

.overview__error-title {
  font-weight: 600;
}

.overview__error-detail {
  overflow-wrap: anywhere;
}

.skeleton-card {
  min-height: 150px;
  padding: var(--khaen-space-4);
  border: 1px dashed var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
  box-shadow: var(--khaen-shadow-card);
}

.status-band__error {
  display: inline-flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  min-width: 0;
  flex: 1 1 220px;
}

.status-band__error-label {
  color: var(--khaen-text-secondary);
  font-size: 11px;
  white-space: nowrap;
}

.status-band__error-value {
  font-family: var(--khaen-font-mono);
  font-size: var(--khaen-font-aux);
  color: var(--khaen-text-muted);
  overflow-wrap: anywhere;
  min-width: 0;
}

.status-band__error-value--set {
  color: var(--khaen-danger);
}

.status-band__meta {
  margin-left: auto;
}

.not-wired {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  align-items: stretch;
}

.not-wired__head {
  display: flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.not-wired__list {
  margin: 0;
  padding: 0;
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
}

.not-wired__item {
  display: flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  min-width: 0;
}

.not-wired__path {
  color: var(--khaen-text);
  overflow-wrap: anywhere;
}

.not-wired__reason {
  color: var(--khaen-text-muted);
  font-size: 11px;
  overflow-wrap: anywhere;
}
</style>
