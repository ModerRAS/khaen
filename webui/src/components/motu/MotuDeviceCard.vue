<script setup lang="ts">
/**
 * MOTU 设备状态卡（GET /api/motu 只读快照）。
 *
 * 诚实边界（docs/API.md §3 / §9）：
 * - detected=false / 控制网不通时显示原因，且写能力区如实标“不可写”；
 * - 拿不到的字段（vol/state/mute、设备身份等）显示「—」+「未上报 + reasons 原文」，不补 0/false；
 * - detected=null 表示检测任务失败（未知 ≠ 未检测到），页面不做推测；
 * - read_error / not_wired / reasons 全部原样展示。
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import PanelCard from '@/components/PanelCard.vue'
import TruthTag from '@/components/TruthTag.vue'
import { MOTU_WRITE_STATUS_KEYS, motuIsWritable, motuReasonFor, motuReasonEntries } from '@/api/motu'
import type { MotuErrorInfo, MotuStatus } from '@/api/motu'
import { formatClock, formatInteger } from '@/utils/format'

const props = defineProps<{
  status: MotuStatus | null
  /** 轮询失败（保留最后一次成功快照时用于标注） */
  error: MotuErrorInfo | null
  loadedAt: number | null
  busy: boolean
}>()

const emit = defineEmits<{ refresh: [] }>()

const { t, locale } = useI18n({ useScope: 'global' })

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))

interface Row {
  label: string
  value: string | null
  /** null = 不是“数据缺失”（例如设备不存在时型号天然为 null）；非 null = 未上报原因 */
  reason: string | null
}

function textOrNull(value: string | null): string | null {
  return value === null || value.length === 0 ? null : value
}

function yesNo(value: boolean | null): string | null {
  if (value === null) {
    return null
  }
  return value ? t('common.yes') : t('common.no')
}

function integer(value: number | null): string | null {
  return formatInteger(value, localeId.value)
}

const truth = computed(() => (props.status === null ? 'notWired' : 'actual'))

const detectedText = computed(() => {
  const detected = props.status?.detected ?? null
  if (detected === true) {
    return t('motu.device.detectedYes')
  }
  if (detected === false) {
    return t('motu.device.detectedNo')
  }
  return t('motu.device.detectedUnknown')
})

const reachableText = computed(() => {
  const reachable = props.status?.control_reachable ?? null
  if (reachable === true) {
    return t('motu.device.reachableYes')
  }
  if (reachable === false) {
    return t('motu.device.reachableNo')
  }
  return t('motu.device.reachableUnknown')
})

const detectionRows = computed<Row[]>(() => {
  const status = props.status
  if (status === null) {
    return []
  }
  const reasonKey =
    status.reason === null ? null : t('motu.device.reason') + ': ' + status.reason
  const rows: Row[] = [
    { label: t('motu.device.detected'), value: detectedText.value, reason: null },
    { label: t('motu.device.model'), value: textOrNull(status.model), reason: null },
    { label: t('motu.device.cardId'), value: textOrNull(status.card_id), reason: null },
    { label: t('motu.device.cardIndex'), value: integer(status.card_index), reason: null },
    { label: t('motu.device.controlReachable'), value: reachableText.value, reason: null },
    { label: t('motu.device.reason'), value: textOrNull(reasonKey), reason: null },
    { label: t('motu.device.reasonDetail'), value: textOrNull(status.reason_detail), reason: null },
    { label: t('motu.device.channelCount'), value: integer(status.channel_count), reason: null }
  ]
  const cache = status.cache
  if (cache !== null) {
    const age = cache.age_ms === null ? null : formatInteger(cache.age_ms, localeId.value)
    rows.push({
      label: t('motu.device.cacheAge'),
      value: age === null ? null : t('motu.device.cacheAgeValue', { age }),
      reason: cache.stale === true ? cache.last_error ?? t('motu.device.cacheStale') : null
    })
  }
  return rows
})

const channelListText = computed(() => {
  const channels = props.status?.channels ?? []
  if (channels.length === 0) {
    return null
  }
  return channels
    .map((channel) => (channel.name === null ? String(channel.channel) : channel.channel + ' ' + channel.name))
    .join(', ')
})

const stateRows = computed<Row[]>(() => {
  const status = props.status
  if (status === null) {
    return []
  }
  const state = status.state
  const stateReason = motuReasonFor(status, 'state')
  if (state === null) {
    return [
      { label: t('motu.device.sampleRate'), value: null, reason: stateReason },
      { label: t('motu.device.mainTrim'), value: null, reason: stateReason },
      { label: t('motu.device.mainBusMute'), value: null, reason: stateReason }
    ]
  }
  const rate = integer(state.sample_rate_hz)
  const missing = state.missing
  return [
    {
      label: t('motu.device.sampleRate'),
      value: rate === null ? null : rate + ' ' + t('unit.hz'),
      reason: stateReason
    },
    { label: t('motu.device.mainTrim'), value: integer(state.main_trim), reason: null },
    { label: t('motu.device.mainBusMute'), value: yesNo(state.main_bus_mute), reason: null },
    { label: t('motu.device.modelId'), value: textOrNull(state.device?.model_id ?? null), reason: null },
    { label: t('motu.device.deviceName'), value: textOrNull(state.device?.name ?? null), reason: null },
    { label: t('motu.device.firmware'), value: textOrNull(state.device?.version ?? null), reason: null },
    { label: t('motu.device.apiVersion'), value: integer(state.device?.api_version ?? null), reason: null },
    {
      label: t('motu.device.missing'),
      value: missing === null ? null : missing.length === 0 ? t('motu.device.missingEmpty') : missing.join(', '),
      reason: null
    }
  ]
})

const writeRows = computed<Row[]>(() => {
  const status = props.status
  if (status === null) {
    return []
  }
  const write = status.write
  if (write === null) {
    return [{ label: t('motu.device.writeStatus'), value: null, reason: null }]
  }
  const statusKey = write.status === null ? null : MOTU_WRITE_STATUS_KEYS[write.status]
  return [
    {
      label: t('motu.device.writeStatus'),
      value: statusKey === null ? null : t(statusKey),
      reason: write.reason
    },
    { label: t('motu.device.singleKey'), value: yesNo(write.single_key), reason: null },
    { label: t('motu.device.readback'), value: yesNo(write.readback), reason: null },
    { label: t('motu.device.twoPhase'), value: yesNo(write.two_phase), reason: null },
    { label: t('motu.device.batch'), value: yesNo(write.batch), reason: null },
    { label: t('motu.device.maxChannels'), value: integer(write.max_channels_per_request), reason: null }
  ]
})

const readErrorRows = computed<Row[]>(() => {
  const error = props.status?.read_error ?? null
  if (error === null) {
    return []
  }
  return [
    { label: t('motu.device.readErrorCode'), value: error.code, reason: null },
    {
      label: t('motu.device.readErrorMessage'),
      value: error.message.length === 0 ? null : error.message,
      reason: null
    },
    { label: t('motu.device.readErrorRetryable'), value: yesNo(error.retryable), reason: null }
  ]
})

const reasonEntries = computed(() => motuReasonEntries(props.status))

const notWiredEntries = computed(() =>
  (props.status?.not_wired ?? []).filter((entry) => entry.path !== null || entry.reason !== null)
)

const writable = computed(() => motuIsWritable(props.status))

const notWritableText = computed(() => {
  const status = props.status
  if (status === null || writable.value) {
    return null
  }
  const reason =
    textOrNull(status.write?.reason ?? null) ??
    textOrNull(status.reason_detail) ??
    t('motu.notReported')
  const code = textOrNull(status.write?.status ?? null) ?? textOrNull(status.reason) ?? t('common.unknown')
  return t('motu.device.notWritableCode', { code, reason })
})

const staleText = computed(() => {
  const error = props.error
  if (error === null || props.status === null) {
    return null
  }
  const time = formatClock(props.loadedAt, localeId.value) ?? t('motu.device.neverUpdated')
  return t('motu.device.staleNotice', { message: t(error.i18nKey), time })
})

const lastUpdateText = computed(() => {
  if (props.loadedAt === null) {
    return t('motu.device.neverUpdated')
  }
  const time = formatClock(props.loadedAt, localeId.value)
  return time === null ? t('motu.device.neverUpdated') : t('motu.device.lastUpdate', { time })
})
</script>

<template>
  <PanelCard title-key="motu.device.title" desc-key="motu.device.desc" :truth="truth">
    <template #actions>
      <span class="aux-text motu-device__updated">{{ lastUpdateText }}</span>
      <el-button
        size="small"
        :loading="busy"
        :disabled="busy"
        :aria-label="t('motu.device.refresh')"
        @click="emit('refresh')"
      >
        {{ t('motu.device.refresh') }}
      </el-button>
    </template>

    <p v-if="staleText !== null" class="banner banner--warn" role="status">{{ staleText }}</p>
    <p v-else-if="error !== null && status === null" class="banner banner--danger" role="alert">
      {{ t(error.i18nKey) }}
      <span v-if="Boolean(error.detail)" class="aux-text">· {{ error.detail }}</span>
    </p>
    <p v-if="notWritableText !== null" class="banner banner--warn" role="status">{{ notWritableText }}</p>
    <p v-if="status !== null && status.detected === null" class="aux-text">{{ t('motu.device.detectedUnknownHint') }}</p>

    <p v-if="status === null && error === null" class="aux-text" aria-busy="true">{{ t('motu.device.loading') }}</p>
    <p v-else-if="status === null" class="aux-text">{{ t('motu.device.noData') }}</p>

    <template v-else>
      <h3 class="motu-device__section">{{ t('motu.device.title') }}</h3>
      <ul class="kv-list motu-device__rows">
        <li v-for="row in detectionRows" :key="row.label" class="kv-list__row">
          <span class="kv-list__label">{{ row.label }}</span>
          <span class="motu-device__value">
            <span v-if="row.value !== null" class="kv-list__value">{{ row.value }}</span>
            <span v-else class="kv-list__value kv-list__value--muted">{{ t('common.dash') }}</span>
            <TruthTag v-if="row.reason !== null" kind="notWired" :reason="row.reason" />
          </span>
        </li>
        <li v-if="channelListText !== null" class="kv-list__row">
          <span class="kv-list__label">{{ t('motu.device.channels') }}</span>
          <span class="kv-list__value mono">{{ channelListText }}</span>
        </li>
      </ul>

      <h3 class="motu-device__section">{{ t('motu.device.stateTitle') }}</h3>
      <ul class="kv-list motu-device__rows">
        <li v-for="row in stateRows" :key="row.label" class="kv-list__row">
          <span class="kv-list__label">{{ row.label }}</span>
          <span class="motu-device__value">
            <span v-if="row.value !== null" class="kv-list__value">{{ row.value }}</span>
            <span v-else class="kv-list__value kv-list__value--muted">{{ t('common.dash') }}</span>
            <TruthTag v-if="row.reason !== null" kind="notWired" :reason="row.reason" />
          </span>
        </li>
      </ul>

      <h3 class="motu-device__section">{{ t('motu.device.writeTitle') }}</h3>
      <ul class="kv-list motu-device__rows">
        <li v-for="row in writeRows" :key="row.label" class="kv-list__row">
          <span class="kv-list__label">{{ row.label }}</span>
          <span class="motu-device__value">
            <span v-if="row.value !== null" class="kv-list__value">{{ row.value }}</span>
            <span v-else class="kv-list__value kv-list__value--muted">{{ t('common.dash') }}</span>
            <TruthTag v-if="row.reason !== null" kind="notWired" :reason="row.reason" />
          </span>
        </li>
      </ul>

      <template v-if="readErrorRows.length > 0">
        <h3 class="motu-device__section">{{ t('motu.device.readError') }}</h3>
        <ul class="kv-list motu-device__rows">
          <li v-for="row in readErrorRows" :key="row.label" class="kv-list__row">
            <span class="kv-list__label">{{ row.label }}</span>
            <span class="kv-list__value mono">{{ row.value ?? t('common.dash') }}</span>
          </li>
        </ul>
      </template>

      <template v-if="reasonEntries.length > 0">
        <h3 class="motu-device__section">{{ t('motu.device.reasons') }}</h3>
        <ul class="motu-device__reasons">
          <li v-for="entry in reasonEntries" :key="entry.path" class="aux-text">
            <span class="mono">{{ entry.path }}</span>: {{ entry.reason }}
          </li>
        </ul>
      </template>

      <template v-if="notWiredEntries.length > 0">
        <h3 class="motu-device__section">{{ t('motu.device.notWired') }}</h3>
        <ul class="motu-device__reasons">
          <li v-for="(entry, index) in notWiredEntries" :key="index" class="aux-text">
            <span class="mono">{{ entry.path ?? t('common.dash') }}</span>:
            {{ entry.reason ?? t('motu.notReported') }}
          </li>
        </ul>
      </template>
    </template>
  </PanelCard>
</template>

<style scoped>
.motu-device__updated {
  font-family: var(--khaen-font-mono);
  font-size: 11px;
  white-space: nowrap;
}

.motu-device__section {
  margin-top: var(--khaen-space-2);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  font-weight: 600;
}

.motu-device__rows {
  margin: 0;
}

.motu-device__value {
  display: inline-flex;
  align-items: center;
  gap: var(--khaen-space-1);
  flex-wrap: wrap;
  justify-content: flex-end;
}

.motu-device__reasons {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin: 0;
  padding-left: 18px;
}
</style>
