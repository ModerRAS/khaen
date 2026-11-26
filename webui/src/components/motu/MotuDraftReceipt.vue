<script setup lang="ts">
/**
 * MOTU 音量草稿卡（prepare 回执）。
 *
 * - prepare 回执 writes_device 必须是 false/null：页面明确写“本回执不写设备”，不把草稿显示成已生效；
 * - TTL 与 created_at 原样展示；没有 abort —— 取消按钮只丢弃本地草稿，服务端草稿按 TTL 过期；
 * - write_plan / target_device 属于原始回执，折叠展示（不裁剪后端字段）。
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { MotuVolumeDraft } from '@/api/motu'
import {
  formatMotuDb,
  formatMotuLinear,
  formatMotuTimestamp,
  motuDraftChannels,
  motuDraftRemainingSeconds
} from '@/components/motu/format'
import { formatInteger, shortenId } from '@/utils/format'

const props = defineProps<{
  draft: MotuVolumeDraft
  /** 两阶段请求进行中（禁用按钮）。 */
  busy: boolean
  /** 当前快照更新时间（用于 TTL 剩余秒数随轮询刷新；null 也能算）。 */
  statusLoadedAt: number | null
}>()

const emit = defineEmits<{ commit: []; cancel: [] }>()

const { t, locale } = useI18n({ useScope: 'global' })

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))

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

const tokenText = computed(() => shortenId(props.draft.engine_token, 8, 4) ?? t('common.dash'))

const targetChannelsText = computed(() => {
  const channels = motuDraftChannels(props.draft)
  if (channels.length === 0) {
    return null
  }
  return channels.join(', ')
})

const targetLinearText = computed(() => formatMotuLinear(props.draft.target?.linear ?? null, localeId.value))
const targetDbText = computed(() => formatMotuDb(props.draft.target?.db ?? null, localeId.value))
const ttlText = computed(() =>
  props.draft.expires_in_s === null ? null : formatInteger(props.draft.expires_in_s, localeId.value)
)

const remainingSeconds = computed(() => {
  // 读取轮询时间戳让 TTL 显示随 5s 轮询刷新（轮询对象是 props.statusLoadedAt）。
  void props.statusLoadedAt
  return motuDraftRemainingSeconds(props.draft, Date.now())
})

const remainingText = computed(() => {
  const seconds = remainingSeconds.value
  return seconds === null ? t('motu.draft.remainingUnknown') : t('motu.draft.remaining', { seconds })
})

const writesDeviceText = computed(() => {
  const writesDevice = props.draft.writes_device
  if (writesDevice === false) {
    return t('motu.draft.writesDeviceFalse')
  }
  if (writesDevice === true) {
    return t('motu.draft.writesDeviceTrue')
  }
  return t('motu.draft.writesDeviceUnknown')
})

const createdText = computed(() => formatMotuTimestamp(props.draft.created_at, localeId.value))

const commitSupportedText = computed(() => boolText(props.draft.commit_supported))
const twoPhaseText = computed(() => boolText(props.draft.two_phase))

function boolText(value: boolean | null): string | null {
  if (value === null) {
    return null
  }
  return value ? t('common.yes') : t('common.no')
}

const writePlanText = computed(() => jsonText(props.draft.write_plan?.raw ?? null))
const targetDeviceText = computed(() => jsonText(props.draft.target_device))

const cancelNoteText = computed(() => {
  const seconds = remainingSeconds.value ?? props.draft.expires_in_s
  return seconds === null ? t('motu.draft.cancelNoteUnknownTtl') : t('motu.draft.cancelNote', { seconds })
})
</script>

<template>
  <section class="motu-draft" data-testid="motu-draft">
    <header class="motu-draft__head">
      <h3 class="motu-draft__title">{{ t('motu.draft.title') }}</h3>
      <span class="mono motu-draft__token">{{ tokenText }}</span>
    </header>

    <ul class="kv-list motu-draft__rows">
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.draft.targetChannels') }}</span>
        <span class="kv-list__value mono">{{ targetChannelsText ?? t('common.dash') }}</span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.draft.targetLinear') }}</span>
        <span class="kv-list__value mono">{{ targetLinearText ?? t('motu.notReported') }}</span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.draft.targetDb') }}</span>
        <span class="kv-list__value mono">
          {{ targetDbText === null ? t('motu.notReported') : targetDbText + ' ' + t('unit.db') }}
        </span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.draft.writesDevice') }}</span>
        <span class="kv-list__value">{{ writesDeviceText }}</span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.draft.createdAt') }}</span>
        <span class="kv-list__value mono">{{ createdText ?? t('motu.notReported') }}</span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.draft.expiresIn') }}</span>
        <span class="kv-list__value mono">{{ ttlText ?? t('motu.notReported') }}</span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.draft.remaining') }}</span>
        <span class="kv-list__value">{{ remainingText }}</span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.draft.commitSupported') }}</span>
        <span class="kv-list__value">{{ commitSupportedText ?? t('motu.notReported') }}</span>
      </li>
      <li class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.draft.twoPhase') }}</span>
        <span class="kv-list__value">{{ twoPhaseText ?? t('motu.notReported') }}</span>
      </li>
      <li v-if="draft.applies_to !== null" class="kv-list__row">
        <span class="kv-list__label">{{ t('motu.draft.appliesTo') }}</span>
        <span class="kv-list__value aux-text">{{ draft.applies_to }}</span>
      </li>
    </ul>

    <details v-if="writePlanText !== null" class="motu-draft__block">
      <summary>{{ t('motu.draft.writePlan') }}</summary>
      <pre class="motu-draft__json mono">{{ writePlanText }}</pre>
    </details>
    <details v-if="targetDeviceText !== null" class="motu-draft__block">
      <summary>{{ t('motu.draft.targetDevice') }}</summary>
      <pre class="motu-draft__json mono">{{ targetDeviceText }}</pre>
    </details>

    <p class="aux-text motu-draft__no-abort">{{ t('motu.draft.noAbortNote') }}</p>
    <p class="aux-text motu-draft__cancel-note">{{ cancelNoteText }}</p>

    <div class="motu-draft__actions">
      <el-button :disabled="busy" data-testid="motu-draft-cancel" @click="emit('cancel')">
        {{ t('motu.draft.cancelLocal') }}
      </el-button>
      <el-button type="primary" :loading="busy" :disabled="busy" data-testid="motu-draft-commit" @click="emit('commit')">
        {{ t('motu.draft.commit') }}
      </el-button>
    </div>
  </section>
</template>

<style scoped>
.motu-draft {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-3);
  border: 1px dashed var(--khaen-border-strong);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
}

.motu-draft__head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.motu-draft__title {
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
}

.motu-draft__token {
  color: var(--khaen-text-muted);
  font-size: 11px;
}

.motu-draft__rows {
  margin: 0;
}

.motu-draft__block summary {
  cursor: pointer;
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

.motu-draft__json {
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

.motu-draft__no-abort {
  color: var(--khaen-text-secondary);
}

.motu-draft__cancel-note {
  color: var(--khaen-text-muted);
}

.motu-draft__actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}
</style>
