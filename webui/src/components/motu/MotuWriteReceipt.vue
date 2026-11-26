<script setup lang="ts">
/**
 * MOTU 写入回执（commit 回执 + 逐通道真值回读）。
 *
 * 诚实边界：
 * - 只有 applied=true 且 writes_device=true 才显示“已写入设备并通过回读校验”；
 * - 每通道展示 before / wrote / readback（linear）与 verified/changed/frame_sent，不做“全部成功”推断；
 * - 回执缺逐通道回读（channels=null）时如实显示“服务端未上报”。
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { MotuVolumeCommitAck } from '@/api/motu'
import {
  formatMotuTimestamp,
  motuCommitWritesDevice,
  motuReceiptViews,
  motuVerifiedCount
} from '@/components/motu/format'
import { formatClock } from '@/utils/format'

const props = defineProps<{
  ack: MotuVolumeCommitAck
  /** 回执收到的本地时钟（毫秒）。 */
  receivedAt: number | null
}>()

const { t, locale } = useI18n({ useScope: 'global' })

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))

const writesDevice = computed(() => motuCommitWritesDevice(props.ack))
const verified = computed(() => motuVerifiedCount(props.ack))
const rows = computed(() => motuReceiptViews(props.ack.channels, localeId.value))

function channelLabel(channel: number | null): string {
  return channel === null ? t('common.dash') : t('motu.receipt.channel', { channel })
}

function boolText(value: boolean | null): string | null {
  if (value === null) {
    return null
  }
  return value ? t('common.yes') : t('common.no')
}

const appliedText = computed(() => boolText(props.ack.applied))
const idempotentText = computed(() => boolText(props.ack.idempotent))
const writesDeviceText = computed(() => boolText(props.ack.writes_device))
const createdText = computed(() => formatMotuTimestamp(props.ack.created_at, localeId.value))
const receiptAtText = computed(() => formatClock(props.receivedAt, localeId.value))

const verifiedText = computed(() => {
  const value = verified.value
  return value === null ? null : t('motu.receipt.verifiedCount', { verified: value.verified, total: value.total })
})

const writePlanText = computed(() => {
  const plan = props.ack.write_plan
  if (plan === null) {
    return null
  }
  try {
    return JSON.stringify(plan.raw, null, 2)
  } catch {
    return String(plan.raw)
  }
})
</script>

<template>
  <section class="motu-receipt" data-testid="motu-write-receipt">
    <header class="motu-receipt__head">
      <h3 class="motu-receipt__title">{{ t('motu.receipt.title') }}</h3>
      <span v-if="receiptAtText !== null" class="aux-text mono">{{ t('motu.receipt.at') }} {{ receiptAtText }}</span>
    </header>

    <p v-if="writesDevice" class="banner banner--info" role="status">{{ t('motu.receipt.success') }}</p>
    <p v-else class="banner banner--warn" role="status">{{ t('motu.receipt.notWritten') }}</p>

    <ul class="kv-list motu-receipt__rows">
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.receipt.applied') }}</span>
        <span class="kv-list__value">{{ appliedText ?? t('motu.notReported') }}</span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.receipt.idempotent') }}</span>
        <span class="kv-list__value">{{ idempotentText ?? t('motu.notReported') }}</span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.receipt.writesDevice') }}</span>
        <span class="kv-list__value">{{ writesDeviceText ?? t('motu.notReported') }}</span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.receipt.linear') }}</span>
        <span class="kv-list__value mono">{{ ack.linear === null ? t('motu.notReported') : ack.linear }}</span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.receipt.db') }}</span>
        <span class="kv-list__value mono">
          {{ ack.db === null ? t('motu.notReported') : ack.db.toFixed(2) + ' ' + t('unit.db') }}
        </span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.receipt.createdAt') }}</span>
        <span class="kv-list__value mono">{{ createdText ?? t('motu.notReported') }}</span>
      </li>
      <li v-if="verifiedText !== null" class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.receipt.verified') }}</span>
        <span class="kv-list__value">{{ verifiedText }}</span>
      </li>
    </ul>

    <h4 class="motu-receipt__section">{{ t('motu.receipt.channelRows') }}</h4>
    <p v-if="rows.length === 0" class="aux-text">{{ t('motu.receipt.noChannels') }}</p>
    <ul v-else class="motu-receipt__channels">
      <li v-for="(row, index) in rows" :key="index" class="motu-receipt__channel">
        <div class="motu-receipt__channel-head">
          <span class="mono motu-receipt__channel-name">{{ row.name ?? channelLabel(row.channel) }}</span>
          <span :class="row.verified === true ? 'motu-receipt__ok' : 'motu-receipt__bad'">
            {{ row.verified === true ? t('motu.receipt.verified') : row.verified === false ? t('common.no') : t('motu.notReported') }}
          </span>
        </div>
        <dl class="motu-receipt__channel-grid">
          <div>
            <dt>{{ t('motu.receipt.before') }}</dt>
            <dd class="mono">{{ row.beforeLinear ?? t('motu.notReported') }}</dd>
          </div>
          <div>
            <dt>{{ t('motu.receipt.wrote') }}</dt>
            <dd class="mono">
              {{ row.wroteLinear ?? t('motu.notReported') }}
              (<template v-if="row.wroteDb !== null">{{ row.wroteDb }} {{ t('unit.db') }}</template>
              <template v-else>{{ t('motu.notReported') }}</template>)
            </dd>
          </div>
          <div>
            <dt>{{ t('motu.receipt.readback') }}</dt>
            <dd class="mono">
              {{ row.readbackLinear ?? t('motu.notReported') }}
              (<template v-if="row.readbackDb !== null">{{ row.readbackDb }} {{ t('unit.db') }}</template>
              <template v-else>{{ t('motu.notReported') }}</template>)
            </dd>
          </div>
          <div>
            <dt>{{ t('motu.receipt.changed') }}</dt>
            <dd>{{ row.changed === null ? t('motu.notReported') : row.changed ? t('common.yes') : t('common.no') }}</dd>
          </div>
          <div>
            <dt>{{ t('motu.receipt.frameSent') }}</dt>
            <dd>{{ row.frameSent === null ? t('motu.notReported') : row.frameSent ? t('common.yes') : t('common.no') }}</dd>
          </div>
        </dl>
      </li>
    </ul>

    <details v-if="writePlanText !== null" class="motu-receipt__block">
      <summary>{{ t('motu.draft.writePlan') }}</summary>
      <pre class="motu-receipt__json mono">{{ writePlanText }}</pre>
    </details>
  </section>
</template>

<style scoped>
.motu-receipt {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-3);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
}

.motu-receipt__head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.motu-receipt__title {
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
}

.motu-receipt__rows {
  margin: 0;
}

.motu-receipt__section {
  margin-top: var(--khaen-space-2);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  font-weight: 600;
}

.motu-receipt__channels {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  margin: 0;
  padding: 0;
  list-style: none;
}

.motu-receipt__channel {
  padding: var(--khaen-space-2);
  border: 1px dashed var(--khaen-border);
  border-radius: var(--khaen-radius-tag);
}

.motu-receipt__channel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  font-size: var(--khaen-font-aux);
}

.motu-receipt__channel-name {
  font-weight: 600;
}

.motu-receipt__ok {
  color: var(--khaen-success);
}

.motu-receipt__bad {
  color: var(--khaen-danger);
}

.motu-receipt__channel-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: var(--khaen-space-1) var(--khaen-space-3);
  margin: var(--khaen-space-1) 0 0;
}

.motu-receipt__channel-grid dt {
  color: var(--khaen-text-muted);
  font-size: 11px;
}

.motu-receipt__channel-grid dd {
  margin: 0;
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.motu-receipt__block summary {
  cursor: pointer;
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

.motu-receipt__json {
  max-height: 220px;
  overflow: auto;
  margin: var(--khaen-space-1) 0 0;
  padding: var(--khaen-space-2);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-bg-subtle, rgba(127, 127, 127, 0.08));
  font-size: 12px;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
</style>
