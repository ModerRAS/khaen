<script setup lang="ts">
/**
 * 系统信息页（任务 T47，原 M6 提前）——接真实 GET /api/system/*。
 *
 * 页面内容（自上而下）：
 * 1. 版本不一致醒目警示条（crate_version ≠ release_version：当前部署 v0.1.2 而 /healthz 曾报 0.1.0 的缺陷实例）；
 * 2. 版本与构建 / 主机 / 服务 / 设备 / 控制通道 / 重启 rt-host 六张卡；
 * 3. 修改密码卡（成功后所有会话失效 -> 重新登录）；
 * 4. 日志查看器（unit 切换 / 行数选择 / 自动刷新 / 复制，内嵌；另有 /system/logs 全页）；
 * 5. 诊断 JSON 下载（页面头部按钮；文件名与内容来自 daemon）。
 *
 * 纪律：null 字段一律「—」+「未接入」TruthTag（tooltip 为后端 reasons），不编造版本/服务/设备/日志；
 * 401 交给共享 ApiClient（会话 store 清态跳登录）；错误保留最后成功数据并明确提示。
 */
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { Download, Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import StatRow from '@/components/StatRow.vue'
import TruthTag from '@/components/TruthTag.vue'
import SystemLogsPanel from '@/components/system/SystemLogsPanel.vue'
import PasswordChangeCard from '@/components/system/PasswordChangeCard.vue'
import RestartCard from '@/components/system/RestartCard.vue'
import { APP_BUILD_ID } from '@/config'
import {
  classifySystemError,
  diagnosticsFallbackFilename,
  formatVersionLabel,
  isMotuDevice,
  reasonForSystemPath,
  versionConsistency,
  type SystemInfo
} from '@/api/system'
import { uptimeParts } from '@/api/status'
import {
  formatClock,
  formatDecimal,
  formatInteger,
  formatTimestamp,
  shortenId,
  toRateUnit
} from '@/utils/format'
import { loadSystemInfo, refreshSystemInfo, systemApi, systemInfoState } from '@/stores/system'
import { loadReleaseVersion } from '@/stores/version'
import { ensureSession, logout } from '@/stores/session'
import type { TruthKind } from '@/types/ui'

type Tone = 'ok' | 'warn' | 'bad'

interface InfoRow {
  label: string
  value: string | null
  suffix?: string | null
  copy?: string | null
  reason?: string | null
  tone?: Tone | null
  truth?: TruthKind | null
}

interface ServiceBlock {
  key: 'daemon' | 'rtHost'
  name: string
  rows: InfoRow[]
}

const { t, locale } = useI18n({ useScope: 'global' })
const route = useRoute()
const router = useRouter()

const data = computed<SystemInfo | null>(() => systemInfoState.data)
const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const refreshing = computed(() => systemInfoState.refreshing)
const initialLoading = computed(() => systemInfoState.phase === 'loading' && data.value === null)
const fatalError = computed(() => systemInfoState.phase === 'error' && data.value === null)

const consistency = computed(() =>
  versionConsistency(data.value?.crate_version ?? null, data.value?.release_version ?? null)
)
const mismatch = computed(() => consistency.value === 'mismatch')
const crateLabel = computed(() => formatVersionLabel(data.value?.crate_version ?? null))
const releaseLabel = computed(() => formatVersionLabel(data.value?.release_version ?? null))
const reasonsCount = computed(() => Object.keys(data.value?.reasons ?? {}).length)

const downloading = ref(false)

/* ------------------------------------------------------------ 格式化小工具 */

function boolText(value: boolean | null): string | null {
  if (value === null) {
    return null
  }
  return t(value ? 'common.yes' : 'common.no')
}

function rateListText(rates: number[] | null): string | null {
  if (rates === null || rates.length === 0) {
    return null
  }
  const parts: string[] = []
  for (const rate of rates) {
    const unit = toRateUnit(rate)
    if (unit === null) {
      continue
    }
    const value = formatDecimal(unit.value, 2, localeId.value)
    if (value !== null) {
      parts.push(value + ' ' + t(unit.unitKey))
    }
  }
  return parts.length === 0 ? null : parts.join(', ')
}

function msText(ms: number | null): string | null {
  const value = formatInteger(ms, localeId.value)
  return value === null ? null : value + ' ' + t('unit.ms')
}

function uptimeText(): string | null {
  const parts = uptimeParts(data.value?.uptime_s ?? null)
  if (parts === null) {
    return null
  }
  const params = { d: parts.days, h: parts.hours, m: parts.minutes, s: parts.seconds }
  return parts.days > 0 ? t('overview.uptime.withDays', params) : t('overview.uptime.withoutDays', params)
}

function timeText(iso: string | null): string | null {
  return formatTimestamp(iso, localeId.value)
}

function reason(path: string): string | null {
  return reasonForSystemPath(data.value, path)
}

/* ------------------------------------------------------------------ 提示条 */

const notices = computed<Array<{ tone: 'warn' | 'info'; text: string }>>(() => {
  const banners: Array<{ tone: 'warn' | 'info'; text: string }> = []
  const current = data.value
  if (current === null) {
    return banners
  }
  if (systemInfoState.error !== null) {
    const time =
      systemInfoState.loadedAt === null
        ? t('common.dash')
        : formatClock(systemInfoState.loadedAt, localeId.value) ?? t('common.dash')
    banners.push({
      tone: 'warn',
      text: t('system.notices.refreshFailed', { message: t(systemInfoState.error.i18nKey), time })
    })
  }
  if (consistency.value === 'unknown') {
    banners.push({ tone: 'warn', text: t('system.version.unknown') })
  }
  if (reasonsCount.value > 0) {
    banners.push({ tone: 'info', text: t('system.notices.degraded', { count: reasonsCount.value }) })
  }
  return banners
})

const lastUpdateText = computed(() => {
  if (systemInfoState.loadedAt === null) {
    return t('system.notices.neverUpdated')
  }
  const time = formatClock(systemInfoState.loadedAt, localeId.value)
  return time === null ? t('system.notices.neverUpdated') : t('system.notices.lastUpdate', { time })
})

/* ------------------------------------------------------------------ 卡片行 */

const versionRows = computed<InfoRow[]>(() => {
  const current = data.value
  if (current === null) {
    return []
  }
  return [
    {
      label: t('system.version.field.crateVersion'),
      value: crateLabel.value,
      reason: reason('crate_version')
    },
    {
      label: t('system.version.field.releaseVersion'),
      value: releaseLabel.value,
      reason: reason('release_version'),
      truth: current.release_version === null ? 'notWired' : 'actual'
    },
    {
      label: t('system.version.field.commit'),
      value: current.commit === null ? null : shortenId(current.commit, 10, 6),
      copy: current.commit,
      reason: reason('commit')
    },
    {
      label: t('system.version.field.buildProfile'),
      value: current.build_profile,
      reason: reason('build_profile')
    },
    {
      label: t('system.version.field.rustc'),
      value: current.rustc,
      reason: reason('rustc')
    },
    {
      label: t('system.version.field.target'),
      value: current.target,
      reason: reason('target')
    },
    {
      label: t('system.version.field.webuiBuild'),
      value: APP_BUILD_ID,
      copy: APP_BUILD_ID,
      truth: 'actual'
    }
  ]
})

const hostRows = computed<InfoRow[]>(() => {
  const current = data.value
  if (current === null) {
    return []
  }
  return [
    {
      label: t('system.host.field.hostname'),
      value: current.hostname,
      copy: current.hostname,
      reason: reason('hostname')
    },
    { label: t('system.host.field.kernel'), value: current.kernel, reason: reason('kernel') },
    { label: t('system.host.field.os'), value: current.os, reason: reason('os') },
    { label: t('system.host.field.uptime'), value: uptimeText(), reason: reason('uptime_s') },
    { label: t('system.host.field.bind'), value: current.bind, copy: current.bind, reason: reason('bind') }
  ]
})

const serviceBlocks = computed<ServiceBlock[]>(() => {
  const current = data.value
  if (current === null) {
    return []
  }
  const info: SystemInfo = current
  function block(key: 'daemon' | 'rtHost'): ServiceBlock {
    const service = key === 'daemon' ? info.services.daemon : info.services.rt_host
    const path = 'services.' + (key === 'daemon' ? 'daemon' : 'rt_host')
    return {
      key,
      name: t(key === 'daemon' ? 'system.services.daemon' : 'system.services.rtHost'),
      rows: [
        {
          label: t('system.services.field.active'),
          value: boolText(service.active),
          tone: service.active === true ? 'ok' : service.active === false ? 'bad' : null,
          reason: service.reason ?? reason(path + '.active')
        },
        {
          label: t('system.services.field.enabled'),
          value: boolText(service.enabled),
          reason: service.reason ?? reason(path + '.enabled')
        },
        {
          label: t('system.services.field.pid'),
          value: formatInteger(service.pid, localeId.value),
          copy: service.pid === null ? null : String(service.pid),
          reason: service.reason ?? reason(path + '.pid')
        }
      ]
    }
  }
  return [block('daemon'), block('rtHost')]
})

const devices = computed(() => data.value?.devices ?? [])
const devicesReason = computed(() => reason('devices'))

const hostLinkRows = computed<InfoRow[]>(() => {
  const current = data.value
  if (current === null) {
    return []
  }
  const link = current.host_link
  return [
    {
      label: t('system.hostLink.field.connected'),
      value: boolText(link.connected),
      tone: link.connected === true ? 'ok' : link.connected === false ? 'bad' : null,
      reason: reason('host_link.connected')
    },
    {
      label: t('system.hostLink.field.statusAgeMs'),
      value: msText(link.status_age_ms),
      reason: reason('host_link.status_age_ms')
    },
    {
      label: t('system.hostLink.field.since'),
      value: timeText(link.since),
      reason: reason('host_link.since')
    },
    {
      label: t('system.hostLink.field.socket'),
      value: link.socket === null ? null : shortenId(link.socket, 18, 12),
      copy: link.socket,
      reason: reason('host_link.socket')
    }
  ]
})

function truthOf(...values: readonly unknown[]): TruthKind {
  return values.some((value) => value !== null && value !== undefined) ? 'actual' : 'notWired'
}

const versionTruth = computed<TruthKind>(() =>
  truthOf(data.value?.crate_version, data.value?.release_version, data.value?.commit, data.value?.rustc)
)
const hostTruth = computed<TruthKind>(() =>
  truthOf(data.value?.hostname, data.value?.kernel, data.value?.os, data.value?.uptime_s, data.value?.bind)
)
const servicesTruth = computed<TruthKind>(() => {
  const services = data.value?.services
  if (services === undefined) {
    return 'notWired'
  }
  return truthOf(
    services.daemon.active,
    services.daemon.enabled,
    services.daemon.pid,
    services.rt_host.active,
    services.rt_host.enabled,
    services.rt_host.pid
  )
})
const hostLinkTruth = computed<TruthKind>(() => {
  const link = data.value?.host_link
  if (link === undefined) {
    return 'notWired'
  }
  return truthOf(link.connected, link.status_age_ms)
})

/* ------------------------------------------------------------------ 动作 */

async function onRefresh(): Promise<void> {
  await refreshSystemInfo()
}

async function downloadDiagnostics(): Promise<void> {
  if (downloading.value) {
    return
  }
  downloading.value = true
  try {
    const download = await systemApi.fetchDiagnostics()
    const filename = download.filename ?? diagnosticsFallbackFilename(data.value?.release_version ?? null)
    saveTextFile(filename, download.text)
    ElMessage.success(t('system.diagnostics.success', { filename }))
  } catch (error) {
    const info = classifySystemError(error)
    const detail = info.reason ?? info.detail
    ElMessage.error(
      t('system.diagnostics.failed') + ': ' + t(info.i18nKey) + (detail === null ? '' : ' — ' + detail)
    )
  } finally {
    downloading.value = false
  }
}

function saveTextFile(filename: string, text: string): void {
  const blob = new Blob([text], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = filename
  document.body.appendChild(link)
  link.click()
  link.remove()
  URL.revokeObjectURL(url)
}

async function onPasswordChanged(): Promise<void> {
  // 改密成功后后端使旧会话失效，并可能直接下发一个新会话（复用 auth password 语义）。
  // 先强制重探会话以拿到新 csrf（若后端没下发新会话则得到未登录），再登出清 Cookie，
  // 否则浏览器仍持有新会话，路由守卫会拒绝进入登录页（「成功后重新登录」就失效了）。
  await ensureSession({ force: true })
  await logout()
  await router.replace({ name: 'login', query: { changed: '1' } })
}

async function onRestarted(): Promise<void> {
  await refreshSystemInfo()
}

/* ---------------------------------------------------------- 深链定位（#password 等） */

async function scrollToHash(): Promise<void> {
  const hash = route.hash
  if (hash.length < 2) {
    return
  }
  await nextTick()
  const target = document.getElementById(hash.slice(1))
  if (target !== null) {
    target.scrollIntoView({ block: 'start', behavior: 'smooth' })
  }
}

onMounted(() => {
  void loadSystemInfo()
  void loadReleaseVersion()
  void scrollToHash()
})

watch(
  () => route.hash,
  () => {
    void scrollToHash()
  }
)

// 深链（如 /system#system-password）在首次进入时数据还没加载：密码卡/日志面板渲染后再滚动一次。
watch(
  () => data.value !== null,
  (ready) => {
    if (ready) {
      void scrollToHash()
    }
  }
)
</script>

<template>
  <div class="system-page">
    <PageHeader title-key="system.pageTitle" subtitle-key="system.subtitle">
      <template #actions>
        <span class="system-page__updated">{{ lastUpdateText }}</span>
        <el-button
          type="primary"
          plain
          :loading="refreshing"
          :disabled="refreshing"
          :aria-label="t('system.actions.refresh')"
          @click="onRefresh"
        >
          <el-icon v-if="!refreshing"><Refresh /></el-icon>
          <span>{{ t('system.actions.refresh') }}</span>
        </el-button>
        <el-button :loading="downloading" :disabled="downloading" @click="downloadDiagnostics">
          <el-icon v-if="!downloading"><Download /></el-icon>
          <span>{{ downloading ? t('system.actions.downloading') : t('system.actions.downloadDiagnostics') }}</span>
        </el-button>
        <el-button plain @click="router.push({ name: 'logs' })">
          {{ t('system.actions.openLogs') }}
        </el-button>
      </template>
    </PageHeader>

    <!-- 版本不一致：本页最重要的警示（crate 0.1.0 vs release v0.1.2 的真实缺陷实例）。 -->
    <div v-if="mismatch && data" class="banner banner--danger system-page__mismatch" role="alert" aria-live="assertive">
      <div class="system-page__mismatch-text">
        <strong>
          {{ t('system.version.mismatchTitle', {
            crate: crateLabel ?? t('common.unknown'),
            release: releaseLabel ?? t('common.unknown')
          }) }}
        </strong>
        <span>{{ t('system.version.mismatchDesc') }}</span>
      </div>
    </div>

    <p v-for="banner in notices" :key="banner.text" class="banner" :class="'banner--' + banner.tone" role="status">
      {{ banner.text }}
    </p>

    <div v-if="fatalError" class="banner banner--danger system-page__error" role="alert" aria-live="assertive">
      <div class="system-page__error-text">
        <p class="system-page__error-title">{{ t('system.error.title') }}</p>
        <p>{{ systemInfoState.error === null ? t('error.system.unknown') : t(systemInfoState.error.i18nKey) }}</p>
        <p v-if="systemInfoState.error?.detail" class="aux-text">{{ systemInfoState.error.detail }}</p>
      </div>
      <el-button type="primary" :loading="refreshing" @click="onRefresh">{{ t('system.actions.retry') }}</el-button>
    </div>

    <div v-if="initialLoading" class="system-page__skeleton" aria-busy="true" :aria-label="t('common.loading')">
      <el-skeleton :rows="6" animated />
    </div>

    <template v-else-if="data">
      <div class="card-grid">
        <PanelCard title-key="system.version.title" desc-key="system.version.desc" :truth="versionTruth">
          <p v-if="mismatch" class="system-page__consistency system-page__consistency--bad">
            {{ t('system.version.mismatchBadge') }}
          </p>
          <p v-else-if="consistency === 'consistent'" class="system-page__consistency system-page__consistency--ok">
            {{ t('system.version.consistent') }}
          </p>
          <ul class="kv-list">
            <StatRow
              v-for="row in versionRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :suffix="row.suffix ?? null"
              :copy="row.copy ?? null"
              :reason="row.reason ?? null"
              :truth="row.truth ?? null"
            />
          </ul>
        </PanelCard>

        <PanelCard title-key="system.host.title" desc-key="system.host.desc" :truth="hostTruth">
          <ul class="kv-list">
            <StatRow
              v-for="row in hostRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :copy="row.copy ?? null"
              :reason="row.reason ?? null"
            />
          </ul>
        </PanelCard>

        <PanelCard title-key="system.services.title" desc-key="system.services.desc" :truth="servicesTruth">
          <div v-for="block in serviceBlocks" :key="block.key" class="system-page__service">
            <h3 class="system-page__service-name mono">{{ block.name }}</h3>
            <ul class="kv-list">
              <StatRow
                v-for="row in block.rows"
                :key="row.label"
                :label="row.label"
                :value="row.value"
                :copy="row.copy ?? null"
                :reason="row.reason ?? null"
                :tone="row.tone ?? null"
              />
            </ul>
          </div>
        </PanelCard>

        <PanelCard
          title-key="system.devices.title"
          desc-key="system.devices.desc"
          :truth="devices.length > 0 ? 'actual' : 'notWired'"
        >
          <ul v-if="devices.length > 0" class="system-page__devices">
            <li v-for="device in devices" :key="(device.card_id ?? '') + (device.model ?? '')" class="system-page__device">
              <div class="system-page__device-head">
                <span class="system-page__device-model">{{ device.model ?? t('common.dash') }}</span>
                <span v-if="isMotuDevice(device)" class="system-page__motu">{{ t('system.devices.motu') }}</span>
                <TruthTag kind="actual" />
              </div>
              <dl class="system-page__device-facts">
                <div>
                  <dt>{{ t('system.devices.field.cardId') }}</dt>
                  <dd class="mono">{{ device.card_id ?? t('common.dash') }}</dd>
                </div>
                <div>
                  <dt>{{ t('system.devices.field.driver') }}</dt>
                  <dd class="mono">{{ device.driver ?? t('common.dash') }}</dd>
                </div>
                <div>
                  <dt>{{ t('system.devices.field.channels') }}</dt>
                  <dd class="mono">{{ formatInteger(device.channels, localeId) ?? t('common.dash') }}</dd>
                </div>
                <div>
                  <dt>{{ t('system.devices.field.rates') }}</dt>
                  <dd class="mono">{{ rateListText(device.rates) ?? t('system.devices.noRates') }}</dd>
                </div>
              </dl>
            </li>
          </ul>
          <p v-else class="system-page__empty">
            <span>{{ t('system.devices.empty') }}</span>
            <TruthTag kind="notWired" :reason="devicesReason" />
          </p>
        </PanelCard>

        <PanelCard title-key="system.hostLink.title" desc-key="system.hostLink.desc" :truth="hostLinkTruth">
          <ul class="kv-list">
            <StatRow
              v-for="row in hostLinkRows"
              :key="row.label"
              :label="row.label"
              :value="row.value"
              :copy="row.copy ?? null"
              :reason="row.reason ?? null"
              :tone="row.tone ?? null"
            />
          </ul>
        </PanelCard>

        <RestartCard :info="data" @restarted="onRestarted" />
      </div>

      <PasswordChangeCard @changed="onPasswordChanged" />

      <SystemLogsPanel show-open-link />
    </template>
  </div>
</template>

<style scoped>
.system-page {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}

.system-page__updated {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
  font-family: var(--khaen-font-mono);
  white-space: nowrap;
}

.system-page__mismatch {
  align-items: flex-start;
}

.system-page__mismatch-text {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
  overflow-wrap: anywhere;
}

.system-page__error {
  align-items: flex-start;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--khaen-space-3);
}

.system-page__error-text {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
  overflow-wrap: anywhere;
}

.system-page__error-title {
  font-weight: 600;
}

.system-page__skeleton {
  padding: var(--khaen-space-4);
  border: 1px dashed var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
}

.system-page__consistency {
  padding: var(--khaen-space-2) var(--khaen-space-3);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-button);
  font-size: var(--khaen-font-aux);
  font-weight: 600;
  overflow-wrap: anywhere;
}

.system-page__consistency--bad {
  border-color: color-mix(in srgb, var(--khaen-danger) 45%, transparent);
  background: var(--khaen-danger-soft);
  color: var(--khaen-danger);
}

.system-page__consistency--ok {
  border-color: color-mix(in srgb, var(--khaen-success) 45%, transparent);
  background: var(--khaen-success-soft);
  color: var(--khaen-success);
}

.system-page__service + .system-page__service {
  margin-top: var(--khaen-space-3);
  padding-top: var(--khaen-space-3);
  border-top: 1px dashed var(--khaen-border);
}

.system-page__service-name {
  margin-bottom: var(--khaen-space-2);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  font-weight: 600;
}

.system-page__devices {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-3);
  margin: 0;
  padding: 0;
  list-style: none;
}

.system-page__device {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding-bottom: var(--khaen-space-2);
  border-bottom: 1px dashed var(--khaen-border);
  min-width: 0;
}

.system-page__device:last-child {
  border-bottom: 0;
  padding-bottom: 0;
}

.system-page__device-head {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  min-width: 0;
}

.system-page__device-model {
  font-weight: 600;
  overflow-wrap: anywhere;
}

.system-page__motu {
  display: inline-flex;
  align-items: center;
  height: 20px;
  padding: 0 6px;
  border: 1px solid color-mix(in srgb, var(--khaen-accent) 45%, transparent);
  border-radius: var(--khaen-radius-tag);
  background: var(--khaen-accent-soft);
  color: var(--khaen-accent);
  font-size: 11px;
  font-weight: 600;
}

.system-page__device-facts {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--khaen-space-1) var(--khaen-space-3);
  margin: 0;
  min-width: 0;
}

.system-page__device-facts dt {
  color: var(--khaen-text-muted);
  font-size: 11px;
}

.system-page__device-facts dd {
  margin: 0;
  color: var(--khaen-text);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.system-page__empty {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  padding: var(--khaen-space-2) var(--khaen-space-3);
  border: 1px dashed var(--khaen-border-strong);
  border-radius: var(--khaen-radius-button);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

@media (max-width: 767px) {
  .system-page__device-facts {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
