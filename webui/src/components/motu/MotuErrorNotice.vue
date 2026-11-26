<script setup lang="ts">
/**
 * MOTU 错误提示（device_error 时原样展示 details.motu / details.completed）。
 *
 * 诚实边界（docs/API.md §1 / §9.3）：
 * - 稳定 code + 本地化主文案 + 后端 message 详情；details 原样折叠展示，不翻译、不裁剪；
 * - 设备写失败时列出 details.completed 里已写入并回读通过的通道，绝不显示“全部成功”；
 * - 401 由会话 store 跳登录，这里只显示归类文案。
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { MotuErrorInfo } from '@/api/motu'
import { motuReceiptViews } from '@/components/motu/format'

const props = defineProps<{
  error: MotuErrorInfo | null
  /** 是否显示「关闭」按钮（只清本地错误显示，不影响服务端）。 */
  dismissible?: boolean
}>()

const emit = defineEmits<{ retry: []; dismiss: [] }>()

const { t, locale } = useI18n({ useScope: 'global' })

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))

const detailsText = computed(() => {
  if (props.error === null || props.error.details === null || props.error.details === undefined) {
    return null
  }
  try {
    return JSON.stringify(props.error.details, null, 2)
  } catch {
    return String(props.error.details)
  }
})

const completedViews = computed(() => motuReceiptViews(props.error?.completed ?? null, localeId.value))

const deviceRows = computed(() => {
  const error = props.error
  if (error === null) {
    return []
  }
  const rows: Array<{ label: string; value: string | null }> = []
  if (error.deviceCode !== null) {
    rows.push({ label: t('motu.error.deviceCode'), value: error.deviceCode })
  }
  if (error.deviceMessage !== null) {
    rows.push({ label: t('motu.error.deviceMessage'), value: error.deviceMessage })
  }
  if (error.deviceRetryable !== null) {
    rows.push({
      label: t('motu.error.deviceRetryable'),
      value: error.deviceRetryable ? t('common.yes') : t('common.no')
    })
  }
  if (error.failedChannel !== null) {
    rows.push({ label: t('motu.error.failedChannel'), value: String(error.failedChannel) })
  }
  if (error.requestedChannels !== null) {
    rows.push({ label: t('motu.error.requestedChannels'), value: error.requestedChannels.join(', ') })
  }
  return rows
})

function channelsText(channels: readonly number[]): string {
  return channels.join(', ')
}
</script>

<template>
  <div v-if="error" class="banner banner--danger motu-error" role="alert" aria-live="assertive">
    <div class="motu-error__text">
      <div class="motu-error__head">
        <span class="motu-error__title">{{ t('motu.error.title') }}</span>
        <span class="mono motu-error__code">{{ error.code }}</span>
        <span v-if="error.deviceCode !== null" class="mono motu-error__device-code">{{ error.deviceCode }}</span>
      </div>
      <p class="motu-error__message">{{ t(error.i18nKey) }}</p>
      <p v-if="Boolean(error.detail)" class="aux-text motu-error__detail">{{ error.detail }}</p>

      <ul v-if="deviceRows.length > 0" class="kv-list motu-error__rows">
        <li v-for="row in deviceRows" :key="row.label" class="kv-list__row">
          <span class="kv-list__label">{{ row.label }}</span>
          <span class="kv-list__value">{{ row.value ?? t('common.dash') }}</span>
        </li>
      </ul>

      <div v-if="error.category === 'device_error'" class="motu-error__completed">
        <p class="motu-error__completed-title">
          {{ t('motu.error.completed') }}:
          <span class="mono">
            {{ error.completedChannels.length > 0 ? channelsText(error.completedChannels) : t('motu.error.completedEmpty') }}
          </span>
        </p>
        <ul v-if="completedViews.length > 0" class="motu-error__completed-list">
          <li v-for="(row, index) in completedViews" :key="index" class="motu-error__completed-item mono">
            {{ row.name ?? t('motu.channelFallback', { channel: row.channel ?? t('common.dash') }) }} ·
            {{ t('motu.receipt.wrote') }} {{ row.wroteLinear ?? t('common.dash') }} ({{ row.wroteDb ?? t('common.dash') }} {{ t('unit.db') }}) ·
            {{ t('motu.receipt.readback') }} {{ row.readbackLinear ?? t('common.dash') }} ({{ row.readbackDb ?? t('common.dash') }} {{ t('unit.db') }}) ·
            {{ row.verified === true ? t('common.yes') : row.verified === false ? t('common.no') : t('motu.notReported') }}
          </li>
        </ul>
        <p v-if="error.partialWrites === true" class="aux-text motu-error__partial">{{ t('motu.error.partial') }}</p>
      </div>

      <details v-if="detailsText !== null" class="motu-error__details">
        <summary>{{ t('motu.error.details') }}</summary>
        <pre class="motu-error__json mono">{{ detailsText }}</pre>
      </details>
    </div>
    <div class="motu-error__actions">
      <el-button v-if="props.dismissible === true" size="small" @click="emit('dismiss')">
        {{ t('motu.error.dismiss') }}
      </el-button>
      <el-button v-if="error.retryable === true" size="small" type="primary" plain @click="emit('retry')">
        {{ t('motu.error.retry') }}
      </el-button>
    </div>
  </div>
</template>

<style scoped>
.motu-error {
  align-items: flex-start;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--khaen-space-3);
}

.motu-error__text {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
  flex: 1 1 320px;
}

.motu-error__head {
  display: flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.motu-error__title {
  font-weight: 600;
}

.motu-error__code,
.motu-error__device-code {
  padding: 0 6px;
  border: 1px solid var(--khaen-border-strong);
  border-radius: var(--khaen-radius-tag);
  color: var(--khaen-text-secondary);
  font-size: 11px;
}

.motu-error__message {
  overflow-wrap: anywhere;
}

.motu-error__detail {
  overflow-wrap: anywhere;
}

.motu-error__rows {
  margin: 0;
}

.motu-error__completed {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.motu-error__completed-title {
  font-weight: 600;
  font-size: var(--khaen-font-aux);
}

.motu-error__completed-list {
  margin: 0;
  padding-left: 18px;
  font-size: 11px;
}

.motu-error__completed-item {
  overflow-wrap: anywhere;
}

.motu-error__partial {
  color: var(--khaen-warning);
}

.motu-error__details summary {
  cursor: pointer;
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

.motu-error__json {
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

.motu-error__actions {
  display: flex;
  gap: var(--khaen-space-2);
  flex: none;
}
</style>
