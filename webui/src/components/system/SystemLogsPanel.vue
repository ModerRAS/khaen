<script setup lang="ts">
/**
 * 日志查看器（任务 T47）：unit 切换 / 行数选择 / 自动刷新 / 复制。
 *
 * 数据来自 GET /api/system/logs（journalctl 最近 N 行，只读）。纪律：
 * - 缺失/不可用如实显示「未接入」+ 后端原因（503 logs_unavailable），不伪造日志行；
 * - 自动刷新默认关闭（阅读时不跳动），开启后 5s 一次；页面隐藏暂停、恢复可见立即刷新；
 * - 刷新失败保留最后一次成功日志并提示；401 交给共享 ApiClient -> 会话 store 跳登录。
 */
import { computed, onBeforeUnmount, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import { DocumentCopy, Refresh } from '@element-plus/icons-vue'
import TruthTag from '@/components/TruthTag.vue'
import {
  pauseSystemLogs,
  refreshSystemLogs,
  resumeSystemLogs,
  setSystemLogsAutoRefresh,
  setSystemLogsLines,
  setSystemLogsUnit,
  startSystemLogs,
  stopSystemLogs,
  systemLogsState
} from '@/stores/system'
import { LOG_LINE_OPTIONS, LOG_UNITS, isLogUnit, type LogUnit } from '@/api/system'
import { formatClock } from '@/utils/format'

const props = defineProps<{
  /** 内嵌在系统页时显示「打开完整日志」链接。 */
  showOpenLink?: boolean
}>()

const { t, locale } = useI18n({ useScope: 'global' })
const state = systemLogsState
const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))

const unitOptions = LOG_UNITS.map((unit) => ({ value: unit, label: unit }))
const lineOptions = LOG_LINE_OPTIONS.map((lines) => ({ value: lines, label: String(lines) }))

const lines = computed(() => state.data?.lines ?? [])
const unavailable = computed(() => state.phase === 'error' && state.error?.category === 'unavailable')
const unavailableReason = computed(
  () => state.error?.reason ?? state.error?.detail ?? t('common.unknown')
)
const lastUpdate = computed(() => {
  if (state.loadedAt === null) {
    return t('system.notices.neverUpdated')
  }
  const time = formatClock(state.loadedAt, localeId.value)
  return time === null
    ? t('system.notices.neverUpdated')
    : t('system.logs.lastUpdate', { time })
})
const metaText = computed(() => {
  const parts: string[] = []
  if (state.data !== null && state.data.requested_lines !== null) {
    parts.push(t('system.logs.requested', { lines: state.data.requested_lines }))
  }
  if (state.data?.source !== null && state.data?.source !== undefined) {
    parts.push(t('system.logs.source', { source: state.data.source }))
  }
  if (state.data?.truncated === true) {
    parts.push(t('system.logs.truncated'))
  }
  return parts.join(' · ')
})

function isDocumentHidden(): boolean {
  return typeof document !== 'undefined' && document.visibilityState === 'hidden'
}

function onVisibilityChange(): void {
  if (isDocumentHidden()) {
    pauseSystemLogs()
    return
  }
  resumeSystemLogs()
}

function onUnitChange(value: unknown): void {
  if (typeof value === 'string' && isLogUnit(value)) {
    setSystemLogsUnit(value as LogUnit)
  }
}

function onLinesChange(value: unknown): void {
  if (typeof value === 'number' && Number.isFinite(value)) {
    setSystemLogsLines(value)
  }
}

function onAutoRefreshChange(value: boolean | string | number): void {
  setSystemLogsAutoRefresh(value === true)
}

function onRefresh(): void {
  void refreshSystemLogs()
}

async function copyLogs(): Promise<void> {
  if (lines.value.length === 0) {
    ElMessage.info(t('system.logs.copyEmpty'))
    return
  }
  const clipboard = typeof navigator !== 'undefined' ? navigator.clipboard : undefined
  if (!clipboard || typeof clipboard.writeText !== 'function') {
    ElMessage.error(t('system.logs.copyFailed'))
    return
  }
  try {
    await clipboard.writeText(lines.value.join('\n'))
    ElMessage.success(t('system.logs.copied'))
  } catch {
    ElMessage.error(t('system.logs.copyFailed'))
  }
}

onMounted(() => {
  document.addEventListener('visibilitychange', onVisibilityChange)
  if (isDocumentHidden()) {
    startSystemLogs()
    pauseSystemLogs()
    return
  }
  startSystemLogs()
})

onBeforeUnmount(() => {
  document.removeEventListener('visibilitychange', onVisibilityChange)
  stopSystemLogs()
})
</script>

<template>
  <section class="logs-panel" :aria-busy="state.loading">
    <header class="logs-panel__head">
      <div class="logs-panel__heading">
        <h2 class="logs-panel__title">{{ t('system.logs.title') }}</h2>
        <TruthTag :kind="unavailable ? 'notWired' : 'actual'" />
      </div>
      <div class="logs-panel__actions">
        <el-select
          class="logs-panel__select"
          size="small"
          :model-value="state.unit"
          :aria-label="t('system.logs.unit')"
          @update:model-value="onUnitChange"
        >
          <el-option v-for="option in unitOptions" :key="option.value" :label="option.label" :value="option.value" />
        </el-select>
        <el-select
          class="logs-panel__select"
          size="small"
          :model-value="state.lines"
          :aria-label="t('system.logs.lines')"
          @update:model-value="onLinesChange"
        >
          <el-option v-for="option in lineOptions" :key="option.value" :label="option.label" :value="option.value" />
        </el-select>
        <label class="logs-panel__auto">
          <el-switch
            size="small"
            :model-value="state.autoRefresh"
            :aria-label="t('system.logs.autoRefresh')"
            @update:model-value="onAutoRefreshChange"
          />
          <span>{{ t('system.logs.autoRefresh') }}</span>
        </label>
        <el-button size="small" :loading="state.loading" :disabled="state.loading" @click="onRefresh">
          <el-icon v-if="!state.loading"><Refresh /></el-icon>
          <span>{{ t('system.actions.refresh') }}</span>
        </el-button>
        <el-button size="small" :disabled="lines.length === 0" @click="copyLogs">
          <el-icon><DocumentCopy /></el-icon>
          <span>{{ t('system.logs.copy') }}</span>
        </el-button>
        <router-link v-if="props.showOpenLink" class="logs-panel__link" :to="{ name: 'logs' }">
          {{ t('system.actions.openLogs') }}
        </router-link>
      </div>
    </header>
    <p class="logs-panel__desc">{{ t('system.logs.desc') }}</p>

    <div class="logs-panel__meta">
      <span>{{ lastUpdate }}</span>
      <span v-if="metaText.length > 0" class="logs-panel__meta-extra">{{ metaText }}</span>
    </div>

    <div v-if="state.phase === 'loading' && state.data === null" class="logs-panel__placeholder" role="status">
      {{ t('system.logs.loading') }}
    </div>

    <div v-else-if="unavailable" class="banner banner--warn logs-panel__state" role="status">
      <strong>{{ t('system.logs.unavailableTitle') }}</strong>
      <span>{{ t('system.logs.unavailableReason', { reason: unavailableReason }) }}</span>
    </div>

    <div v-else-if="state.phase === 'error'" class="banner banner--danger logs-panel__state" role="alert">
      <div class="logs-panel__state-text">
        <strong>{{ t('system.logs.errorTitle') }}</strong>
        <span>{{ state.error === null ? t('error.system.unknown') : t(state.error.i18nKey) }}</span>
        <span v-if="state.error?.detail" class="aux-text">{{ state.error.detail }}</span>
      </div>
      <el-button size="small" :loading="state.loading" @click="onRefresh">{{ t('system.actions.retry') }}</el-button>
    </div>

    <template v-else>
      <p v-if="state.error !== null" class="banner banner--warn logs-panel__state" role="status">
        {{ t('system.notices.refreshFailed', {
          message: t(state.error.i18nKey),
          time: state.loadedAt === null ? t('common.dash') : formatClock(state.loadedAt, localeId) ?? t('common.dash')
        }) }}
      </p>
      <pre v-if="lines.length > 0" class="logs-panel__output" tabindex="0">{{ lines.join('\n') }}</pre>
      <p v-else class="aux-text">{{ t('system.logs.empty') }}</p>
    </template>
  </section>
</template>

<style scoped>
.logs-panel {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-4);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
  box-shadow: var(--khaen-shadow-card);
  min-width: 0;
}

.logs-panel__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.logs-panel__heading {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  min-width: 0;
}

.logs-panel__title {
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
}

.logs-panel__actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  justify-content: flex-end;
}

.logs-panel__select {
  width: 96px;
}

.logs-panel__auto {
  display: inline-flex;
  align-items: center;
  gap: var(--khaen-space-1);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  white-space: nowrap;
}

.logs-panel__link {
  font-size: var(--khaen-font-aux);
}

.logs-panel__desc {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

.logs-panel__meta {
  display: flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  color: var(--khaen-text-muted);
  font-size: 11px;
  font-family: var(--khaen-font-mono);
}

.logs-panel__meta-extra {
  color: var(--khaen-text-muted);
}

.logs-panel__placeholder {
  padding: var(--khaen-space-6);
  border: 1px dashed var(--khaen-border);
  border-radius: var(--khaen-radius-button);
  color: var(--khaen-text-muted);
  text-align: center;
  font-size: var(--khaen-font-aux);
}

.logs-panel__state {
  align-items: flex-start;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--khaen-space-2);
}

.logs-panel__state-text {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
  overflow-wrap: anywhere;
}

.logs-panel__output {
  margin: 0;
  padding: var(--khaen-space-3);
  max-height: 480px;
  overflow: auto;
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-button);
  background: var(--khaen-bg);
  color: var(--khaen-text);
  font-family: var(--khaen-font-mono);
  font-size: 12px;
  line-height: 1.5;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.logs-panel__output:focus-visible {
  outline: 2px solid var(--khaen-accent);
  outline-offset: 2px;
}
</style>
