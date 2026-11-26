<script setup lang="ts">
/**
 * MOTU 主输出音量两阶段控制（PUT /api/motu/volume）。
 *
 * 流程（docs/API.md §4.1）：
 *   prepare {stage:"prepare", channel:1|2|"both", db}  -> 只生成草稿（writes_device=false）
 *   commit  {stage:"commit", engine_token}             -> 逐通道单键写 + 设备回读（writes_device=true）
 * 该端点**没有 abort**：取消只丢弃本地草稿，服务端草稿按 TTL 过期（不假装已删除）。
 *
 * 诚实边界：
 * - 设备不可写（detected=false / 控制网不通 / write.status != wired）时 prepare 禁用并显示原因；
 * - 目标 dB 的初始值取设备当前回读（真值）；回读不可用时从 -60 dB 的安全本地默认开始并标注；
 * - 回执只显示后端真值：prepare 的 writes_device=false 明示“未写设备”，commit 的逐通道回读逐条展示。
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import PanelCard from '@/components/PanelCard.vue'
import MotuDraftReceipt from '@/components/motu/MotuDraftReceipt.vue'
import MotuWriteReceipt from '@/components/motu/MotuWriteReceipt.vue'
import MotuErrorNotice from '@/components/motu/MotuErrorNotice.vue'
import {
  MOTU_VOLUME_DB_MAX,
  MOTU_VOLUME_DB_MIN,
  MOTU_VOLUME_DB_STEP,
  motuIsWritable,
  motuReasonFor,
  validateMotuChannelSelection,
  validateMotuVolumeDb
} from '@/api/motu'
import type { MotuChannelSelection, MotuStatus } from '@/api/motu'
import {
  commitMotuVolume,
  cancelMotuVolumeDraft,
  dismissMotuVolumeError,
  motuPollState,
  motuVolumeState,
  prepareMotuVolume,
  refreshMotu
} from '@/stores/motu'
import { formatMotuDb } from '@/components/motu/format'
import { formatClock, formatDecimal } from '@/utils/format'

/** 回读不可用时的本地安全默认（明确标注“本地默认”，不是设备当前值）。 */
const FALLBACK_DB = -60

const props = defineProps<{
  status: MotuStatus | null
}>()

const { t, locale } = useI18n({ useScope: 'global' })

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))

const CHANNEL_LABEL_KEYS: Record<MotuChannelSelection, string> = {
  1: 'motu.control.channel1',
  2: 'motu.control.channel2',
  both: 'motu.control.channelBoth'
}

const selection = ref<MotuChannelSelection>(1)
const db = ref(FALLBACK_DB)
const dbInitialised = ref(false)
const dbFromReadback = ref(false)

// 首次拿到设备回读时，用真值初始化目标 dB（只初始化一次，不覆盖用户编辑）。
watch(
  () => props.status,
  (status) => {
    if (dbInitialised.value || status === null) {
      return
    }
    const volume = status.vol?.find((entry) => entry.channel === 1) ?? status.vol?.[0] ?? null
    if (volume !== null && volume.linear === 0) {
      db.value = MOTU_VOLUME_DB_MIN
      dbFromReadback.value = true
    } else if (volume !== null && typeof volume.db === 'number' && Number.isFinite(volume.db)) {
      const clamped = Math.min(MOTU_VOLUME_DB_MAX, Math.max(MOTU_VOLUME_DB_MIN, volume.db))
      db.value = Math.round(clamped * 10) / 10
      dbFromReadback.value = true
    }
    dbInitialised.value = true
  },
  { immediate: true }
)

const writable = computed(() => motuIsWritable(props.status))
const busy = computed(() => motuVolumeState.busy)
const draft = computed(() => motuVolumeState.draft)
const lastCommit = computed(() => motuVolumeState.lastCommit)
const error = computed(() => motuVolumeState.error)
const truth = computed(() => (writable.value ? 'actual' : 'notWired'))

const channelError = computed(() => validateMotuChannelSelection(selection.value))
const dbError = computed(() => validateMotuVolumeDb(db.value))

const validationText = computed(() => {
  if (channelError.value !== null) {
    return t('motu.control.validationChannel')
  }
  if (dbError.value !== null) {
    return t('motu.control.validationDb', { min: MOTU_VOLUME_DB_MIN, max: MOTU_VOLUME_DB_MAX })
  }
  return null
})

const lockText = computed(() => {
  const status = props.status
  if (status === null) {
    return t('motu.device.noData')
  }
  if (writable.value) {
    return null
  }
  const reason = status.write?.reason ?? status.reason_detail ?? motuReasonFor(status, 'vol') ?? t('motu.notReported')
  return t('motu.device.notWritable', { reason })
})

const channelsText = computed(() => t(CHANNEL_LABEL_KEYS[selection.value]))

const currentReadbackText = computed(() => {
  const status = props.status
  if (status === null) {
    return null
  }
  const channels = selection.value === 'both' ? [1, 2] : [selection.value]
  const parts: string[] = []
  for (const channel of channels) {
    const volume = status.vol?.find((entry) => entry.channel === channel) ?? null
    const name = volume?.name ?? t('motu.channelFallback', { channel })
    if (volume === null || volume.linear === null) {
      return null
    }
    const dbText = formatMotuDb(volume.db, localeId.value)
    const value = dbText === null ? t('motu.readback.minusInfinity') : dbText + ' ' + t('unit.db')
    parts.push(name + ' = ' + value)
  }
  return parts.join(' · ')
})

const previewText = computed(() => {
  const dbText = formatDecimal(db.value, 1, localeId.value) ?? String(db.value)
  return t('motu.control.preview', { channels: channelsText.value, db: dbText })
})

const readbackMissingReason = computed(() => motuReasonFor(props.status, 'vol') ?? t('motu.notReported'))

const canPrepare = computed(
  () => writable.value && busy.value === null && validationText.value === null
)

const commitUnconfirmed = computed(
  () => lastCommit.value !== null && !(lastCommit.value.applied === true && lastCommit.value.writes_device === true)
)

function cancelNoticeText(): string | null {
  const notice = motuVolumeState.localCancel
  if (notice === null) {
    return null
  }
  return notice.ttlSeconds === null
    ? t('motu.draft.cancelNoteUnknownTtl')
    : t('motu.draft.cancelNote', { seconds: notice.ttlSeconds })
}

const localCancelText = computed(cancelNoticeText)

const HISTORY_ACTION_KEYS: Record<string, string> = {
  prepare: 'motu.history.prepare',
  commit: 'motu.history.commit',
  cancel: 'motu.history.cancel'
}

function historyActionText(action: string): string {
  const key = HISTORY_ACTION_KEYS[action]
  return key === undefined ? action : t(key)
}

function historyTime(at: number): string {
  return formatClock(at, localeId.value) ?? t('common.dash')
}

async function onPrepare(): Promise<void> {
  if (!canPrepare.value) {
    return
  }
  const prepared = await prepareMotuVolume(selection.value, db.value)
  if (prepared === null) {
    ElMessage.error(t('motu.control.prepareFailed'))
    return
  }
  ElMessage.success(t('motu.control.prepared'))
}

async function onCommit(): Promise<void> {
  const ack = await commitMotuVolume()
  if (ack === null) {
    ElMessage.error(t('motu.control.commitFailed'))
    return
  }
  if (ack.applied === true && ack.writes_device === true) {
    ElMessage.success(t('motu.control.committed'))
  } else {
    ElMessage.warning(t('motu.control.commitUnconfirmed'))
  }
}

function onCancelLocal(): void {
  const notice = cancelMotuVolumeDraft()
  if (notice !== null) {
    ElMessage.info(t('motu.draft.cancelled'))
  }
}

function onRetry(): void {
  dismissMotuVolumeError()
  // 设备现在可写就重试 prepare；否则先刷新设备快照（原因由设备卡展示）。
  if (canPrepare.value) {
    void onPrepare()
    return
  }
  void refreshMotu()
}

function onDismissError(): void {
  dismissMotuVolumeError()
}
</script>

<template>
  <PanelCard title-key="motu.control.title" desc-key="motu.control.desc" :truth="truth">
    <p v-if="lockText !== null" class="banner banner--warn" role="status">{{ lockText }}</p>

    <div class="motu-control">
      <div class="motu-control__field">
        <span class="motu-control__label">{{ t('motu.control.channelLabel') }}</span>
        <el-radio-group
          v-model="selection"
          class="motu-control__channels"
          :disabled="busy !== null"
        >
          <el-radio-button :value="1">{{ t('motu.control.channel1') }}</el-radio-button>
          <el-radio-button :value="2">{{ t('motu.control.channel2') }}</el-radio-button>
          <el-radio-button value="both">{{ t('motu.control.channelBoth') }}</el-radio-button>
        </el-radio-group>
      </div>

      <div class="motu-control__field">
        <span class="motu-control__label">{{ t('motu.control.dbLabel') }}</span>
        <el-input-number
          v-model="db"
          :min="MOTU_VOLUME_DB_MIN"
          :max="MOTU_VOLUME_DB_MAX"
          :step="MOTU_VOLUME_DB_STEP"
          :precision="1"
          :disabled="busy !== null"
          class="motu-control__input"
        />
      </div>
    </div>

    <p class="aux-text motu-control__hint">
      {{ t('motu.control.dbHint', { min: MOTU_VOLUME_DB_MIN }) }}
    </p>
    <p class="aux-text motu-control__readback">
      <template v-if="currentReadbackText !== null">
        {{ t('motu.control.currentReadback', { channels: channelsText, value: currentReadbackText }) }}
      </template>
      <template v-else>
        {{ t('motu.control.currentUnknown', { reason: readbackMissingReason }) }}
      </template>
    </p>
    <p v-if="!dbFromReadback" class="aux-text motu-control__default-note">
      {{ t('motu.control.defaultDbNote', { db: FALLBACK_DB }) }}
    </p>

    <p v-if="validationText !== null" class="motu-control__invalid" role="alert">{{ validationText }}</p>

    <div class="motu-control__actions">
      <el-button
        type="primary"
        :loading="busy === 'prepare'"
        :disabled="!canPrepare"
        data-testid="motu-prepare"
        @click="onPrepare"
      >
        {{ t('motu.control.prepare') }}
      </el-button>
      <span class="aux-text">{{ previewText }}</span>
    </div>

    <MotuDraftReceipt
      v-if="draft !== null"
      :draft="draft"
      :busy="busy !== null"
      :status-loaded-at="motuPollState.loadedAt"
      @commit="onCommit"
      @cancel="onCancelLocal"
    />

    <p v-if="localCancelText !== null" class="banner banner--info" role="status">{{ localCancelText }}</p>

    <MotuWriteReceipt
      v-if="lastCommit !== null"
      :ack="lastCommit"
      :received-at="motuVolumeState.lastCommitAt"
    />
    <p v-if="commitUnconfirmed" class="aux-text">{{ t('motu.control.commitUnconfirmed') }}</p>

    <MotuErrorNotice :error="error" :dismissible="true" @retry="onRetry" @dismiss="onDismissError" />

    <h4 class="motu-control__section">{{ t('motu.history.title') }}</h4>
    <p v-if="motuVolumeState.history.length === 0" class="aux-text">{{ t('motu.history.empty') }}</p>
    <ul v-else class="motu-history">
      <li v-for="(entry, index) in motuVolumeState.history" :key="index" class="motu-history__item">
        <span class="mono motu-history__time">{{ historyTime(entry.at) }}</span>
        <span class="motu-history__action">{{ historyActionText(entry.action) }}</span>
        <span :class="entry.ok ? 'motu-history__ok' : 'motu-history__failed'">
          {{ entry.ok ? t('motu.history.ok') : t('motu.history.failed') }}
        </span>
        <span v-if="entry.channels.length > 0" class="aux-text mono">{{ entry.channels.join(', ') }}</span>
        <span v-if="entry.db !== null" class="aux-text mono">{{ formatDecimal(entry.db, 1, localeId) }} {{ t('unit.db') }}</span>
        <span v-if="entry.code !== null" class="aux-text mono">{{ entry.code }}</span>
        <span v-if="entry.action === 'cancel'" class="aux-text">{{ t('motu.history.cancelHint') }}</span>
      </li>
    </ul>
  </PanelCard>
</template>

<style scoped>
.motu-control {
  display: flex;
  gap: var(--khaen-space-4);
  flex-wrap: wrap;
}

.motu-control__field {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.motu-control__label {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.motu-control__channels {
  flex-wrap: wrap;
}

.motu-control__input {
  width: 140px;
}

.motu-control__hint,
.motu-control__readback,
.motu-control__default-note {
  color: var(--khaen-text-muted);
}

.motu-control__invalid {
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
}

.motu-control__actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.motu-control__actions :deep(.el-button) {
  min-height: 36px;
}

.motu-control__section {
  margin-top: var(--khaen-space-2);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  font-weight: 600;
}

.motu-history {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.motu-history__item {
  display: flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  padding-bottom: 4px;
  border-bottom: 1px dashed var(--khaen-border);
  font-size: var(--khaen-font-aux);
}

.motu-history__time {
  color: var(--khaen-text-muted);
  font-size: 11px;
}

.motu-history__ok {
  color: var(--khaen-success);
}

.motu-history__failed {
  color: var(--khaen-danger);
}
</style>
