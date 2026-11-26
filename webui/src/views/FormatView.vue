<script setup lang="ts">
/**
 * 音频格式页（M3 / 任务 T53）——接真实控制面两阶段接口（T70f-b 转发 rt-host）。
 *
 * 数据来源：
 * - GET /api/status（5s 轮询）：format.plan_rate / device_rate / rate_asserted / block_frames /
 *   channels / encoding / aes67.* 与 plan.version；
 * - POST /api/format {sample_rate?, chunksize?} → POST /api/commit {engine_token}：两阶段；
 * - POST /api/device/retry：rate_asserted=false 时请求设备监督器立即重试一次并回读；
 * - abort：POST /api/abort 丢弃草稿。
 *
 * 诚实边界（不把"控制面已切换"说成"设备已生效"）：
 * - 后端 format commit 的语义是"重建控制面引擎并丢弃正在跑的图"，响应显式带 device_restart_required：
 *   真实 ALSA 设备必须重开 PCM 流，只有重启 rt-host 后 /api/status 的 device_rate 才会变化；
 * - 页面把「控制面回执」与「设备回读（/api/status）」分成两栏显示，重启前不假装设备已按新速率运行；
 * - 编码（encoding）后端没有数据源：只显示「未接入」+ 原因，不做只读占位假值；
 * - 支持的采样率/块长来自后端能力（48k/96k、64/128）；设备是否真的支持由后端/设备拒绝，前端不猜。
 */
import { computed, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import { Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import StatRow from '@/components/StatRow.vue'
import StatusBadge from '@/components/StatusBadge.vue'
import ControlErrorNotice from '@/components/control/ControlErrorNotice.vue'
import { useStatusPolling } from '@/composables/useStatusPolling'
import { isStatusStale, reasonForPath, runStateKind, runStateLabelKey } from '@/api/status'
import {
  SUPPORTED_BLOCK_SIZES,
  SUPPORTED_SAMPLE_RATES_HZ,
  type ControlCommitAck,
  type ControlDraft,
  type FormatPatch
} from '@/api/control'
import {
  abortControlDraft,
  commitControlDraft,
  controlState,
  prepareFormat,
  retryDeviceReadback
} from '@/stores/control'
import { formatClock, formatDecimal, formatInteger, shortenId, toRateUnit } from '@/utils/format'
import type { StatusKind } from '@/types/ui'

const { t, locale } = useI18n({ useScope: 'global' })
const router = useRouter()
const { state: statusState, refresh } = useStatusPolling()

const data = computed(() => statusState.data)
const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const refreshing = computed(() => statusState.refreshing)
const controlBusy = computed(() => controlState.busy)
const controlError = computed(() => controlState.error)

const runState = computed<StatusKind>(() => (data.value === null ? 'unknown' : runStateKind(data.value.state)))
const faulted = computed(() => runState.value === 'faulted')
const safe = computed(() => data.value?.state.safe === true)
const writeLocked = computed(() => faulted.value || safe.value)
const lockReason = computed<string | null>(() => data.value?.state.last_error ?? null)

const stale = computed(() => isStatusStale(data.value))

const lastUpdateText = computed(() => {
  if (statusState.loadedAt === null) {
    return t('formatPage.band.neverUpdated')
  }
  const time = formatClock(statusState.loadedAt, localeId.value)
  return time === null ? t('formatPage.band.neverUpdated') : t('formatPage.band.lastUpdate', { time })
})

/* ------------------------------------------------------------ 格式化小工具 */

function rateText(hz: number | null): string | null {
  const unit = toRateUnit(hz)
  if (unit === null) {
    return null
  }
  const value = formatDecimal(unit.value, 2, localeId.value)
  return value === null ? null : value + ' ' + t(unit.unitKey)
}

function intText(value: number | null): string | null {
  return formatInteger(value, localeId.value)
}

function boolText(value: boolean | null): string | null {
  if (value === null) {
    return null
  }
  return value ? t('common.yes') : t('common.no')
}

/* ------------------------------------------------------------ 状态带与提示 */

const rateBadge = computed<{ kind: StatusKind; value: string | undefined }>(() => {
  const rate = rateText(data.value?.format.device_rate ?? null)
  if (rate === null) {
    return { kind: 'unknown', value: undefined }
  }
  return {
    kind: data.value?.format.rate_asserted === false ? 'muted' : 'safe',
    value: rate
  }
})

const runStateBadgeText = computed(() => {
  const key = data.value === null ? null : runStateLabelKey(data.value.state)
  if (key !== null) {
    return t(key)
  }
  return data.value?.state.value ?? undefined
})

const planVersionText = computed(() => {
  const version = data.value?.plan.version ?? null
  return version === null ? null : 'v' + (formatInteger(version, localeId.value) ?? String(version))
})

const rateAsserted = computed<boolean | null>(() => data.value?.format.rate_asserted ?? null)
const rateMismatch = computed(() => {
  const format = data.value?.format
  if (format === undefined || format === null) {
    return false
  }
  const plan = format.plan_rate
  const device = format.device_rate
  return plan !== null && device !== null && plan !== device
})

type NoticeTone = 'warn' | 'danger' | 'info'

const notices = computed<Array<{ tone: NoticeTone; text: string }>>(() => {
  const banners: Array<{ tone: NoticeTone; text: string }> = []
  const current = data.value
  if (current === null) {
    return banners
  }
  if (safe.value) {
    banners.push({
      tone: 'warn',
      text:
        lockReason.value === null
          ? t('formatPage.notice.safeReadOnly')
          : t('formatPage.notice.safeReadOnlyReason', { reason: lockReason.value })
    })
  } else if (faulted.value) {
    banners.push({
      tone: 'danger',
      text:
        lockReason.value === null
          ? t('formatPage.notice.faultedReadOnly')
          : t('formatPage.notice.faultedReadOnlyReason', { reason: lockReason.value })
    })
  }
  if (rateAsserted.value === false) {
    banners.push({ tone: 'warn', text: t('formatPage.notice.rateNotAsserted') })
  }
  if (rateMismatch.value) {
    banners.push({ tone: 'warn', text: t('formatPage.notice.rateMismatch') })
  }
  if (stale.value) {
    banners.push({ tone: 'warn', text: t('formatPage.notice.stale') })
  }
  if (statusState.error !== null) {
    const time =
      statusState.loadedAt === null
        ? t('common.dash')
        : formatClock(statusState.loadedAt, localeId.value) ?? t('common.dash')
    banners.push({ tone: 'warn', text: t('formatPage.notice.refreshFailed', { message: t(statusState.error.i18nKey), time }) })
  }
  return banners
})

/* -------------------------------------------------------------- 当前格式卡 */

interface KvRow {
  label: string
  value: string | null
  copy?: string | null
  reason?: string | null
  tone?: 'ok' | 'warn' | 'bad' | null
}

const currentRows = computed<KvRow[]>(() => {
  const current = data.value
  if (current === null) {
    return []
  }
  const format = current.format
  const aes67 = format.aes67
  return [
    { label: t('formatPage.current.field.planRate'), value: rateText(format.plan_rate), reason: reasonForPath(current, 'format.plan_rate') },
    {
      label: t('formatPage.current.field.deviceRate'),
      value: rateText(format.device_rate),
      reason: reasonForPath(current, 'format.device_rate'),
      tone: rateAsserted.value === false ? 'warn' : null
    },
    {
      label: t('formatPage.current.field.rateAsserted'),
      value: boolText(format.rate_asserted),
      reason: reasonForPath(current, 'format.rate_asserted'),
      tone: format.rate_asserted === false ? 'warn' : null
    },
    { label: t('formatPage.current.field.blockFrames'), value: intText(format.block_frames), reason: reasonForPath(current, 'format.block_frames') },
    { label: t('formatPage.current.field.channels'), value: intText(format.channels), reason: reasonForPath(current, 'format.channels') },
    {
      label: t('formatPage.current.field.encoding'),
      value: format.encoding,
      reason: format.encoding === null ? reasonForPath(current, 'format.encoding') ?? t('formatPage.current.encodingReason') : null
    },
    {
      label: t('formatPage.current.field.aes67Channels'),
      value: intText(aes67.channels),
      reason: reasonForPath(current, 'format.aes67.channels') ?? reasonForPath(current, 'aes67.channels')
    },
    {
      label: t('formatPage.current.field.aes67Map'),
      value: aes67.map === null || aes67.map.length === 0 ? null : aes67.map.join(','),
      reason: reasonForPath(current, 'format.aes67.map') ?? reasonForPath(current, 'aes67.map')
    },
    {
      label: t('formatPage.current.field.aes67Group'),
      value: aes67.group,
      reason: reasonForPath(current, 'format.aes67.group') ?? reasonForPath(current, 'aes67.group')
    },
    {
      label: t('formatPage.current.field.aes67Pt'),
      value: intText(aes67.pt),
      reason: reasonForPath(current, 'format.aes67.pt') ?? reasonForPath(current, 'aes67.pt')
    },
    {
      label: t('formatPage.current.field.aes67Port'),
      value: intText(aes67.port),
      reason: reasonForPath(current, 'format.aes67.port') ?? reasonForPath(current, 'aes67.port')
    }
  ]
})

const currentTruth = computed(() => {
  const current = data.value
  if (current === null) {
    return 'notWired' as const
  }
  const format = current.format
  const anyValue =
    format.plan_rate !== null ||
    format.device_rate !== null ||
    format.rate_asserted !== null ||
    format.block_frames !== null ||
    format.channels !== null
  return anyValue ? ('actual' as const) : ('notWired' as const)
})

/* ------------------------------------------------------------------ 应用区 */

const selectedRate = ref<number | undefined>(undefined)
const selectedBlock = ref<number | undefined>(undefined)

function onRateSelect(value: unknown): void {
  selectedRate.value = typeof value === 'number' ? value : undefined
}

function onBlockSelect(value: unknown): void {
  selectedBlock.value = typeof value === 'number' ? value : undefined
}

const formatPatch = computed<FormatPatch>(() => {
  const patch: FormatPatch = {}
  if (selectedRate.value !== undefined) {
    patch.sample_rate = selectedRate.value
  }
  if (selectedBlock.value !== undefined) {
    patch.chunksize = selectedBlock.value
  }
  return patch
})

const hasPatch = computed(() => Object.keys(formatPatch.value).length > 0)

const canPrepare = computed(
  () => hasPatch.value && !writeLocked.value && controlState.busy === null
)

const formatDraft = computed<ControlDraft | null>(() =>
  controlState.draft !== null && controlState.draft.surface === 'format' ? controlState.draft : null
)

const prepareBusy = computed(() => controlState.busy === 'prepare')
const commitBusy = computed(() => controlState.busy === 'commit')
const abortBusy = computed(() => controlState.busy === 'abort')

async function onPrepareFormat(): Promise<void> {
  if (!canPrepare.value) {
    return
  }
  const draft = await prepareFormat(formatPatch.value)
  if (draft !== null) {
    ElMessage.success(t('formatPage.apply.prepared'))
  }
}

async function onCommit(): Promise<void> {
  const ack = await commitControlDraft()
  if (ack === null) {
    return
  }
  ElMessage.success(t('control.toast.committed'))
  await refresh()
}

async function onAbort(): Promise<void> {
  const ack = await abortControlDraft()
  if (ack !== null) {
    ElMessage.success(t('control.toast.aborted'))
  }
}

async function onRetryReadback(): Promise<void> {
  const ack = await retryDeviceReadback()
  if (ack === null) {
    return
  }
  if (ack.rate_asserted === false) {
    ElMessage.warning(t('formatPage.current.retryStillUnasserted'))
  } else {
    ElMessage.success(t('formatPage.current.retryOk'))
  }
  await refresh()
}

/* -------------------------------------------------------------- 回执与历史 */

function surfaceText(surface: string | null): string | null {
  if (surface === null) {
    return null
  }
  const key = 'control.surface.' + surface
  const text = t(key)
  return text === key ? surface : text
}

function formatText(format: { sample_rate: number | null; block_size: number | null; channels: number | null }): string {
  return t('control.format.summary', {
    rate: format.sample_rate === null ? t('common.dash') : formatInteger(format.sample_rate, localeId.value) ?? t('common.dash'),
    block: format.block_size === null ? t('common.dash') : formatInteger(format.block_size, localeId.value) ?? t('common.dash'),
    channels: format.channels === null ? t('common.dash') : formatInteger(format.channels, localeId.value) ?? t('common.dash')
  })
}

const draftRows = computed<KvRow[]>(() => {
  const draft = controlState.draft
  if (draft === null) {
    return []
  }
  const rows: KvRow[] = [
    { label: t('control.receipt.surface'), value: surfaceText(draft.surface) },
    {
      label: t('control.receipt.engineToken'),
      value: shortenId(draft.engine_token, 8, 4),
      copy: draft.engine_token
    },
    {
      label: t('control.receipt.plannedVersion'),
      value: draft.planned_version === null ? null : formatInteger(draft.planned_version, localeId.value)
    }
  ]
  if (draft.changed !== null) {
    rows.push({ label: t('control.receipt.changed'), value: draft.changed ? t('common.yes') : t('common.no') })
  }
  if (draft.format !== null) {
    rows.push({ label: t('control.receipt.format'), value: formatText(draft.format) })
  }
  if (draft.device_restart_required !== null) {
    rows.push({
      label: t('control.receipt.deviceRestart'),
      value: draft.device_restart_required ? t('common.yes') : t('common.no')
    })
  }
  if (draft.note !== null) {
    rows.push({ label: t('control.receipt.note'), value: draft.note })
  }
  return rows
})

const commitRows = computed<KvRow[]>(() => {
  const ack: ControlCommitAck | null = controlState.lastCommit
  if (ack === null) {
    return []
  }
  const rows: KvRow[] = [
    { label: t('control.receipt.version'), value: ack.version === null ? null : formatInteger(ack.version, localeId.value) },
    { label: t('control.receipt.appliedBlock'), value: ack.applied_block === null ? null : formatInteger(ack.applied_block, localeId.value) }
  ]
  if (ack.changed !== null) {
    rows.push({ label: t('control.receipt.changed'), value: ack.changed ? t('common.yes') : t('common.no') })
  }
  if (ack.format !== null) {
    rows.push({ label: t('control.receipt.format'), value: formatText(ack.format) })
  }
  if (ack.device_restart_required !== null) {
    rows.push({
      label: t('control.receipt.deviceRestart'),
      value: ack.device_restart_required ? t('common.yes') : t('common.no')
    })
  }
  if (ack.graph_dropped === true) {
    rows.push({ label: t('control.receipt.graphDropped'), value: t('common.yes') })
  }
  if (ack.history_cleared === true) {
    rows.push({ label: t('control.receipt.historyCleared'), value: t('common.yes') })
  }
  return rows
})

const commitDeviceRestart = computed(() => controlState.lastCommit?.device_restart_required === true)

const appliedFormatText = computed(() => {
  const format = controlState.appliedFormat
  return format === null ? null : formatText(format)
})

const historyRows = computed(() => controlState.history)

const ACTION_KEYS: Record<string, string> = {
  mute: 'control.history.action.mute',
  prepare: 'control.history.action.prepare',
  commit: 'control.history.action.commit',
  abort: 'control.history.action.abort',
  retry: 'control.history.action.retry'
}

function actionText(action: string): string {
  const key = ACTION_KEYS[action]
  return key === undefined ? action : t(key)
}

function historyTime(at: number): string {
  return formatClock(at, localeId.value) ?? t('common.dash')
}

function goToRestart(): void {
  void router.push({ name: 'system', hash: '#system-restart' })
}

function onRefresh(): void {
  void refresh()
}
</script>

<template>
  <div class="format-page">
    <PageHeader title-key="formatPage.pageTitle" subtitle-key="formatPage.subtitle">
      <template #actions>
        <span class="format-page__updated">{{ lastUpdateText }}</span>
        <el-button
          type="primary"
          plain
          :loading="refreshing"
          :disabled="refreshing"
          :aria-label="t('formatPage.actions.refresh')"
          @click="onRefresh"
        >
          <el-icon v-if="!refreshing"><Refresh /></el-icon>
          <span>{{ t('formatPage.actions.refresh') }}</span>
        </el-button>
      </template>
    </PageHeader>

    <div v-if="data === null && statusState.phase === 'error'" class="banner banner--danger format-page__fatal" role="alert">
      <div class="format-page__fatal-text">
        <span class="format-page__fatal-title">{{ t('formatPage.error.title') }}</span>
        <span>{{ statusState.error === null ? t('error.status.unknown') : t(statusState.error.i18nKey) }}</span>
      </div>
      <el-button type="primary" :loading="refreshing" @click="onRefresh">{{ t('common.retry') }}</el-button>
    </div>

    <ControlErrorNotice :error="controlError" @retry="onRefresh" />

    <p v-for="banner in notices" :key="banner.text" class="banner" :class="'banner--' + banner.tone" role="status">
      {{ banner.text }}
    </p>

    <div v-if="data" class="status-band">
      <StatusBadge :kind="runState" :label="t('formatPage.band.runState')" :value="runStateBadgeText" />
      <span class="status-band__divider" aria-hidden="true" />
      <StatusBadge :kind="rateBadge.kind" :label="t('formatPage.band.deviceRate')" :value="rateBadge.value" />
      <span class="status-band__divider" aria-hidden="true" />
      <StatusBadge
        :kind="rateAsserted === true ? 'safe' : rateAsserted === false ? 'muted' : 'unknown'"
        :label="t('formatPage.band.rateAsserted')"
        :value="rateAsserted === null ? undefined : rateAsserted ? t('common.yes') : t('common.no')"
      />
      <span class="status-band__divider" aria-hidden="true" />
      <StatusBadge :kind="planVersionText === null ? 'unknown' : 'safe'" :label="t('formatPage.band.planVersion')" :value="planVersionText ?? undefined" />
      <span class="status-band__meta">{{ lastUpdateText }}</span>
    </div>

    <div v-if="data" class="grid-12 format-grid">
      <PanelCard class="format-grid__span-7" title-key="formatPage.current.title" desc-key="format.current.desc" :truth="currentTruth">
        <div v-if="rateAsserted === false" class="banner banner--warn format-current__warn" role="status">
          <span>{{ t('formatPage.current.rateNotAsserted') }}</span>
          <el-button size="small" :loading="controlBusy === 'retry'" :disabled="controlBusy !== null" @click="onRetryReadback">
            {{ t('formatPage.current.retryReadback') }}
          </el-button>
        </div>
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
        <p class="aux-text">{{ t('formatPage.current.sourceHint') }}</p>
      </PanelCard>

      <PanelCard class="format-grid__span-5" title-key="formatPage.apply.title" desc-key="format.apply.desc" :truth="formatDraft !== null || controlState.lastCommit !== null ? 'actual' : 'notWired'">
        <fieldset class="format-apply__group">
          <legend class="format-apply__legend">{{ t('formatPage.apply.field.sampleRate') }}</legend>
          <el-radio-group
            :model-value="selectedRate"
            :disabled="writeLocked || controlBusy !== null"
            @change="onRateSelect"
          >
            <el-radio-button v-for="rate in SUPPORTED_SAMPLE_RATES_HZ" :key="rate" :value="rate">
              {{ formatInteger(rate, localeId) }} Hz
            </el-radio-button>
          </el-radio-group>
        </fieldset>
        <fieldset class="format-apply__group">
          <legend class="format-apply__legend">{{ t('formatPage.apply.field.block') }}</legend>
          <el-radio-group
            :model-value="selectedBlock"
            :disabled="writeLocked || controlBusy !== null"
            @change="onBlockSelect"
          >
            <el-radio-button v-for="frames in SUPPORTED_BLOCK_SIZES" :key="frames" :value="frames">
              {{ formatInteger(frames, localeId) }} {{ t('formatPage.apply.frames') }}
            </el-radio-button>
          </el-radio-group>
        </fieldset>
        <p class="aux-text">{{ t('formatPage.apply.note') }}</p>
        <div class="format-apply__actions">
          <el-button type="primary" :loading="prepareBusy" :disabled="!canPrepare" @click="onPrepareFormat">
            {{ t('control.actions.prepare') }}
          </el-button>
          <span v-if="writeLocked" class="aux-text">{{ t('formatPage.apply.lockedHint') }}</span>
          <span v-else-if="!hasPatch" class="aux-text">{{ t('formatPage.apply.selectHint') }}</span>
        </div>

        <template v-if="formatDraft !== null">
          <h3 class="format-apply__subheading">{{ t('control.receipt.draftTitle') }}</h3>
          <ul class="kv-list">
            <StatRow
              v-for="row in draftRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :copy="row.copy ?? null"
              :reason="row.reason ?? null"
            />
          </ul>
          <div class="format-apply__actions">
            <el-button type="primary" :loading="commitBusy" :disabled="controlBusy !== null && !commitBusy" @click="onCommit">
              {{ commitBusy ? t('control.actions.committing') : t('control.actions.commit') }}
            </el-button>
            <el-button :loading="abortBusy" :disabled="controlBusy !== null && !abortBusy" @click="onAbort">
              {{ abortBusy ? t('control.actions.aborting') : t('control.actions.abort') }}
            </el-button>
          </div>
          <p class="aux-text">{{ t('control.receipt.commitHint') }}</p>
        </template>

        <template v-if="controlState.lastCommit !== null">
          <h3 class="format-apply__subheading">{{ t('control.receipt.commitTitle') }}</h3>
          <ul class="kv-list">
            <StatRow
              v-for="row in commitRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :copy="row.copy ?? null"
              :reason="row.reason ?? null"
            />
            <StatRow :label="t('control.receipt.at')" :value="formatClock(controlState.lastCommitAt, localeId)" :reason="null" />
            <StatRow
              :label="t('formatPage.apply.field.controlFormat')"
              :value="appliedFormatText"
              :reason="appliedFormatText === null ? t('formatPage.apply.noControlReceipt') : null"
            />
          </ul>
          <div v-if="commitDeviceRestart" class="banner banner--warn format-apply__restart" role="status">
            <div>
              <p class="format-apply__restart-title">{{ t('formatPage.apply.restartTitle') }}</p>
              <p class="aux-text">{{ t('formatPage.apply.restartDesc') }}</p>
            </div>
            <el-button size="small" @click="goToRestart">{{ t('control.deviceRestart.goto') }}</el-button>
          </div>
        </template>
      </PanelCard>

      <PanelCard class="format-grid__span-12" title-key="control.receipt.title" desc-key="control.receipt.desc" :truth="historyRows.length > 0 ? 'actual' : 'notWired'">
        <h3 class="format-apply__subheading">{{ t('control.history.title') }}</h3>
        <p v-if="historyRows.length === 0" class="aux-text">{{ t('control.history.empty') }}</p>
        <ul v-else class="control-history">
          <li v-for="(entry, index) in historyRows" :key="index" class="control-history__item">
            <span class="control-history__time mono">{{ historyTime(entry.at) }}</span>
            <span class="control-history__action">{{ actionText(entry.action) }}</span>
            <span class="control-history__state" :class="entry.ok ? 'control-history__state--ok' : 'control-history__state--failed'">
              {{ entry.ok ? t('control.history.ok') : t('control.history.failed') }}
            </span>
            <span v-if="entry.version !== null" class="control-history__meta mono">v{{ entry.version }}</span>
            <span v-if="entry.code !== null" class="control-history__meta mono">{{ entry.code }}</span>
          </li>
        </ul>
      </PanelCard>
    </div>

    <div v-else-if="statusState.phase === 'loading'" class="banner banner--info" aria-busy="true">
      {{ t('common.loading') }}
    </div>
  </div>
</template>

<style scoped>
.format-page {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}

.format-page__updated {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
  font-family: var(--khaen-font-mono);
  white-space: nowrap;
}

.format-page__fatal {
  align-items: flex-start;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--khaen-space-3);
}

.format-page__fatal-text {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
}

.format-page__fatal-title {
  font-weight: 600;
}

.format-grid__span-7 {
  grid-column: span 7;
}

.format-grid__span-5 {
  grid-column: span 5;
}

.format-grid__span-12 {
  grid-column: span 12;
}

.format-current__warn {
  align-items: center;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.format-apply__group {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  margin: 0;
  padding: 0;
  border: 0;
  min-width: 0;
}

.format-apply__legend {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  padding: 0;
  margin-bottom: 4px;
}

.format-apply__actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.format-apply__actions :deep(.el-button) {
  min-height: 36px;
}

.format-apply__subheading {
  margin-top: var(--khaen-space-2);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  font-weight: 600;
}

.format-apply__restart {
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.format-apply__restart-title {
  font-weight: 600;
}

.control-history {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.control-history__item {
  display: flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  min-width: 0;
  padding-bottom: 4px;
  border-bottom: 1px dashed var(--khaen-border);
  font-size: var(--khaen-font-aux);
}

.control-history__time {
  color: var(--khaen-text-muted);
  font-size: 11px;
}

.control-history__action {
  color: var(--khaen-text);
}

.control-history__state--ok {
  color: var(--khaen-success);
}

.control-history__state--failed {
  color: var(--khaen-danger);
}

.control-history__meta {
  color: var(--khaen-text-secondary);
  font-size: 11px;
}

@media (max-width: 1279px) {
  .format-grid__span-7,
  .format-grid__span-5 {
    grid-column: span 12;
  }
}
</style>