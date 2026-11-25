<script setup lang="ts">
// 系统信息：版本/构建/last-good/WS 状态；未接线的后端能力明确标「待接线」，不放假数据。
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { Connection, Refresh, Link, Warning } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import CopyText from '@/components/CopyText.vue'
import AckPanel from '@/components/AckPanel.vue'
import TruthTag from '@/components/TruthTag.vue'
import { api, ApiError, errorText } from '@/api/client'
import type { CalEngineInfo } from '@/api/types'
import { refreshRuntime, runtimeState } from '@/stores/runtime'
import { connectWs, disconnectWs, probeWsSupport, wsState, clearWsLog } from '@/stores/live'
import type { AckView, HealthBody, ModeStatus, RuntimeStatus } from '@/api/types'
import { APP_VERSION, APP_BUILD_TIME, APP_BUILD_ID } from '@/utils/build'
import { formatUnix, formatClock, shortHash } from '@/utils/format'

const health = ref<HealthBody | null>(null)
const healthHeaders = ref<Record<string, string>>({})
const healthError = ref('')
const mode = ref<ModeStatus | null>(null)
const calEngine = ref<CalEngineInfo | null>(null)
const calJobCount = ref<number | null>(null)
const modeError = ref('')
const switching = ref('')
const ack = ref<AckView | null>(null)
const now = ref(Date.now())
let clockTimer: number | null = null

const status = computed<RuntimeStatus | null>(() => runtimeState.status)
const buildTime = computed(() => new Date(APP_BUILD_TIME).toLocaleString('zh-CN', { hour12: false }))
const uptimeText = computed(() => {
  const secs = health.value?.uptime_secs
  if (secs === undefined) { return '—' }
  const hours = Math.floor(secs / 3600)
  const mins = Math.floor((secs % 3600) / 60)
  return hours > 0 ? `${hours} 小时 ${mins} 分` : `${mins} 分 ${Math.floor(secs % 60)} 秒`
})
const wsPhaseText = computed(() => {
  switch (wsState.phase) {
    case 'open': return '已连接'
    case 'connecting': return '连接中'
    case 'unsupported': return '不可用（已降级 REST）'
    case 'error': return '错误（已降级 REST）'
    case 'closed': return '已断开'
    default: return '未连接'
  }
})

async function loadHealth(): Promise<void> {
  try {
    const result = await api.healthz()
    health.value = result.data
    healthHeaders.value = result.headers
    healthError.value = ''
  } catch (error) {
    healthError.value = errorText(error)
  }
}

async function loadCalibrationFacts(): Promise<void> {
  try {
    const jobs = await api.calJobsList()
    calEngine.value = jobs.cal_engine
    calJobCount.value = jobs.count
  } catch {
    calEngine.value = null
    calJobCount.value = null
  }
}

async function loadMode(): Promise<void> {
  try {
    mode.value = await api.modeGet()
    modeError.value = ''
  } catch (error) {
    modeError.value = errorText(error)
  }
}

async function switchMode(next: string): Promise<void> {
  const current = status.value?.mode
  if (next === current) { return }
  const services = mode.value?.mode_services ?? status.value?.mode_services ?? 'unknown'
  try {
    await ElMessageBox.confirm(
      `切换工作模式：${current ?? '—'} → ${next}。\n` +
      `mode_services=${services}${services === 'systemd' ? '（真实系统服务会被重启/停止；请确认现场安全）' : '（模拟服务，仅状态机演示）'}。\n` +
      '切换后以 actual 回读为准；synthetic 回执会明确标注。',
      '确认切换模式',
      { confirmButtonText: '切换', cancelButtonText: '取消', type: 'warning' }
    )
  } catch { return }
  switching.value = next
  ack.value = null
  try {
    const payload = await api.modePut(next)
    ack.value = {
      kind: payload.synthetic ? 'synthetic' : payload.mode_actual ? 'actual' : 'info',
      engine: payload.engine,
      synthetic: payload.synthetic,
      actual: payload.mode_actual,
      detail: `模式切换已接受：${current ?? '—'} → ${payload.mode}（state=${payload.state}，${payload.mode_services}）` +
        `${payload.mode_evidence ? ' · epoch=' + payload.mode_evidence.epoch + ' · servo=' + payload.mode_evidence.servo_owner : ''}`,
      raw: payload
    }
    await Promise.all([refreshRuntime(), loadMode()])
    ElMessage.success('模式切换请求已接受，等待 actual 回读')
  } catch (error) {
    const code = error instanceof ApiError ? error.code : 'error'
    ack.value = {
      kind: ['unsupported', 'not_implemented', 'not_supported'].includes(code) ? 'unsupported' : 'error',
      engine: null, synthetic: null, actual: null,
      detail: `模式切换失败（未生效）：${errorText(error)}`,
      raw: error instanceof ApiError ? error.body : String(error)
    }
  } finally {
    switching.value = ''
  }
}

onMounted(async () => {
  await Promise.all([loadHealth(), loadMode(), loadCalibrationFacts(), refreshRuntime()])
  void probeWsSupport()
  clockTimer = window.setInterval(() => { now.value = Date.now() }, 1000)
})

onUnmounted(() => {
  if (clockTimer !== null) { window.clearInterval(clockTimer); clockTimer = null }
})

const placeholderCards = computed(() => {
  const cards = [
    {
      title: 'AES67 / AoIP',
      state: '待接线',
      detail: '当前 daemon 的 /api/* 未提供 AES67 控制端点；独立 aes67-daemon 进程在 :8854（管理）与 :8080 运行。本页不展示假状态，接线后在此接入真实 API。'
    },
    {
      title: 'PTP / 时钟伺服',
      state: '待接线',
      detail: 'REST 未暴露 PTP 锁相/漂移/uncertainty 端点；仅 mode_services 与 T44 evidence 反映模式状态机。板端 PTP 实测属 NOT_RUN。'
    },
    {
      title: 'CueMix / MOTU 硬件控制',
      state: '待接线',
      detail: 'MOTU 由 khaen-rt-host 独占（ALSA owner）；daemon 不直接控制硬件。CueMix 混音器控制未接线，页面不提供假旋钮。'
    },
    {
      title: 'blocks / underruns / faults 计数',
      state: '未提供',
      detail: '当前 /api/runtime/status 只提供 xruns / deadline_misses / clipped_samples；blocks、underruns、faults 字段在 REST 契约中不存在，页面显示「未提供」而不是编造数字。'
    }
  ]
  const engine = calEngine.value
  cards.push({
    title: '校准测量 / apply 真实性',
    state: engine ? (engine.synthetic ? 'synthetic' : 'real_audio') : '未接线',
    detail: engine
      ? ('engine=' + engine.engine + ' · synthetic=' + engine.synthetic + ' · real_audio=' + engine.real_audio +
         ' · backend=' + engine.backend + '；note：' + engine.note +
         (calJobCount.value !== null ? '（当前 job 数：' + calJobCount.value + '）' : ''))
      : '校准 port 未声明 engine 身份（按保守口径当 synthetic 处理，不冒充 actual）。'
  })
  return cards
})
</script>

<template>
  <div class="page">
    <PageHeader title="系统信息" subtitle="版本、构建、last-good、WS 状态；未接线能力以「待接线」占位，不放假数据。">
      <template #right>
        <el-button :icon="Refresh" @click="loadHealth(); loadMode(); loadCalibrationFacts()">刷新</el-button>
      </template>
    </PageHeader>

    <div class="grid-2">
      <PanelCard title="版本与构建">
        <div class="kv">
          <div class="kv__item"><span class="kv__label">WebUI 版本</span><span class="kv__value">v{{ APP_VERSION }}（Vue 3 + Element Plus）</span></div>
          <div class="kv__item"><span class="kv__label">构建时间</span><span class="kv__value">{{ buildTime }}</span></div>
          <div class="kv__item"><span class="kv__label">构建标识</span><span class="kv__value mono">{{ APP_BUILD_ID }}</span></div>
          <div class="kv__item"><span class="kv__label">daemon 版本</span><span class="kv__value">{{ health?.version ?? '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">daemon 运行时长</span><span class="kv__value">{{ uptimeText }}</span></div>
          <div class="kv__item"><span class="kv__label">mock</span><span class="kv__value">{{ health?.mock ?? '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">X-KHAEN-API-Version</span>
            <span class="kv__value mono">{{ healthHeaders['x-khaen-api-version'] ?? '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">X-KHAEN-Web-Version</span>
            <span class="kv__value mono">{{ healthHeaders['x-khaen-web-version'] ?? '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">schema</span>
            <span class="kv__value mono">{{ status?.schema ?? '—' }} · api v{{ status?.api_version ?? '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">浏览器时刻</span>
            <span class="kv__value mono">{{ formatClock(now) }}</span></div>
        </div>
        <div v-if="healthError" class="banner banner--warn" style="margin-top: 10px">{{ healthError }}</div>
      </PanelCard>

      <PanelCard title="系数 / last-good" subtitle="coeff 与 last-good 来自运行时快照">
        <div class="kv">
          <div class="kv__item"><span class="kv__label">actual coeff</span>
            <span class="kv__value mono">{{ status?.coeff ? status.coeff.version : '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">last_good_coeff</span>
            <span class="kv__value mono">{{ status?.last_good_coeff?.version ?? '—' }}</span></div>
          <div class="kv__item" v-if="status?.last_good_coeff"><span class="kv__label">last-good sha256</span>
            <CopyText :value="status.last_good_coeff.sha256" :short="20" /></div>
          <div class="kv__item"><span class="kv__label">available coeffs</span>
            <span class="kv__value">{{ status?.available_coeffs?.length ?? 0 }} 个</span></div>
          <div class="kv__item"><span class="kv__label">actual_version</span>
            <span class="kv__value">{{ status?.actual_version ?? '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">graph_id</span>
            <CopyText :value="status?.graph_id" :short="20" /></div>
          <div class="kv__item"><span class="kv__label">target_hash</span>
            <CopyText :value="status?.target_hash" :short="20" /></div>
          <div class="kv__item"><span class="kv__label">committed</span>
            <span class="kv__value">{{ formatUnix(status?.committed_unix) }}</span></div>
        </div>
        <div v-if="status?.synthetic" class="banner banner--warn" style="margin-top: 10px">
          {{ status.engine }}（synthetic）：以上 version/epoch/graph 均来自内存 engine，非板端 actual。
        </div>
      </PanelCard>
    </div>

    <PanelCard
      v-if="status?.host"
      title="rt-host 运行时快照"
      subtitle="daemon 原样透传 host 块循环快照（engine=rt-host, synthetic=false）；本页不重算"
    >
      <template #extra>
        <span class="chip" :class="status.actual ? 'chip--ok' : 'chip--warn'">actual={{ status.actual }}</span>
      </template>
      <div class="kv">
        <div class="kv__item"><span class="kv__label">blocks / frames</span>
          <span class="kv__value mono">{{ status.host.blocks ?? '—' }} / {{ status.host.frames ?? '—' }}</span></div>
        <div class="kv__item"><span class="kv__label">xruns / underruns / overruns</span>
          <span class="kv__value mono">{{ status.host.xruns ?? '—' }} / {{ status.host.underruns ?? '—' }} / {{ status.host.overruns ?? '—' }}</span></div>
        <div class="kv__item"><span class="kv__label">recoveries / faults</span>
          <span class="kv__value mono">{{ status.host.recoveries ?? '—' }} / {{ status.host.faults ?? '—' }}</span></div>
        <div class="kv__item"><span class="kv__label">deadline misses / silence blocks</span>
          <span class="kv__value mono">{{ status.host.deadline_misses ?? '—' }} / {{ status.host.silence_blocks ?? '—' }}</span></div>
        <div class="kv__item"><span class="kv__label">safe_mode / silent / safety_mute</span>
          <span class="kv__value mono">{{ status.host.safe_mode ?? '—' }} / {{ status.host.silent ?? '—' }} / {{ status.host.safety_mute ?? '—' }}</span></div>
        <div class="kv__item"><span class="kv__label">forced_mute / forced_mute_blocks</span>
          <span class="kv__value mono">{{ status.host.forced_mute ?? '—' }} / {{ status.host.forced_mute_blocks ?? '—' }}</span></div>
        <div class="kv__item"><span class="kv__label">host sample_rate / block_size</span>
          <span class="kv__value mono">{{ status.host.sample_rate ?? '—' }} / {{ status.host.block_size ?? '—' }}</span></div>
        <div class="kv__item"><span class="kv__label">host graph</span>
          <CopyText :value="status.host.graph" :short="20" /></div>
      </div>
      <div class="truth-tags" style="margin-top: 10px">
        <span v-for="flag in status.host.wired ?? []" :key="'w-' + flag" class="chip chip--ok">wired: {{ flag }}</span>
        <span v-for="flag in status.host.not_wired ?? []" :key="'n-' + flag" class="chip chip--warn">not_wired: {{ flag }}</span>
      </div>
    </PanelCard>

    <PanelCard title="工作模式切换" subtitle="PUT /api/runtime/mode；T44 唯一 mode owner / servo">
      <template #extra>
        <TruthTag :engine="mode?.engine ?? status?.engine" :synthetic="mode?.synthetic ?? status?.synthetic" :actual="mode?.actual ?? status?.actual" compact />
      </template>
      <div class="kv">
        <div class="kv__item"><span class="kv__label">当前模式</span>
          <span class="kv__value">{{ status?.mode_label ?? mode?.mode_label ?? '—' }}（{{ status?.mode ?? mode?.mode ?? '—' }}）</span></div>
        <div class="kv__item"><span class="kv__label">mode_profile</span>
          <span class="kv__value mono">{{ status?.mode_profile ?? '—' }}</span></div>
        <div class="kv__item"><span class="kv__label">mode_controller / services</span>
          <span class="kv__value">{{ mode?.mode_controller ?? status?.mode_controller }} / {{ mode?.mode_services ?? status?.mode_services ?? '—' }}</span></div>
        <div class="kv__item"><span class="kv__label">mode_actual</span>
          <span class="kv__value">{{ mode?.mode_actual ?? '—' }}</span></div>
        <div class="kv__item"><span class="kv__label">状态机 state / progress</span>
          <span class="kv__value">{{ mode?.state ?? '—' }} / {{ mode?.progress ?? '—' }}</span></div>
        <div class="kv__item"><span class="kv__label">epoch / servo_owner</span>
          <span class="kv__value mono">{{ mode?.mode_evidence ? mode.mode_evidence.epoch + ' / ' + mode.mode_evidence.servo_owner : '—' }}</span></div>
      </div>
      <div class="actions" style="margin-top: 12px">
        <el-button
          v-for="profile in mode?.profiles ?? []"
          :key="profile.mode"
          size="small"
          :type="profile.mode === (status?.mode ?? mode?.mode) ? 'primary' : 'default'"
          :loading="switching === profile.mode"
          @click="switchMode(profile.mode)"
        >
          {{ profile.label }}（{{ profile.mode }}）
        </el-button>
      </div>
      <p v-if="modeError" class="hint" style="color: var(--khaen-warn); margin-top: 8px">{{ modeError }}</p>
      <AckPanel :ack="ack" title="模式回执" style="margin-top: 12px" />
    </PanelCard>

    <PanelCard title="WS 实时状态" subtitle="T24 兼容子集；预览代理未反代 /ws 时显式降级为 REST 轮询">
      <template #extra>
        <span class="chip" :class="{ 'chip--ok': wsState.phase === 'open', 'chip--warn': wsState.phase === 'unsupported' || wsState.phase === 'error' }">{{ wsPhaseText }}</span>
      </template>
      <div class="actions">
        <el-button type="primary" :icon="Link" :disabled="wsState.phase === 'open' || wsState.phase === 'connecting'" @click="connectWs">连接 WS</el-button>
        <el-button :disabled="wsState.phase !== 'open'" @click="disconnectWs">断开</el-button>
        <el-button :icon="Connection" @click="probeWsSupport">探测 /ws 支持</el-button>
        <el-button @click="clearWsLog">清空日志</el-button>
        <span class="hint">sent={{ wsState.sent }} · received={{ wsState.received }}</span>
      </div>
      <p class="hint" style="margin: 8px 0">{{ wsState.detail }}</p>
      <div class="kv">
        <div class="kv__item"><span class="kv__label">GetState</span>
          <span class="kv__value mono">{{ JSON.stringify(wsState.values.GetState ?? '—') }}</span></div>
        <div class="kv__item"><span class="kv__label">GetVolume / GetMute</span>
          <span class="kv__value mono">{{ JSON.stringify(wsState.values.GetVolume ?? '—') }} / {{ JSON.stringify(wsState.values.GetMute ?? '—') }}</span></div>
        <div class="kv__item"><span class="kv__label">GetCaptureRate</span>
          <span class="kv__value mono">{{ JSON.stringify(wsState.values.GetCaptureRate ?? '—') }}</span></div>
        <div class="kv__item"><span class="kv__label">GetClippedSamples</span>
          <span class="kv__value mono">{{ JSON.stringify(wsState.values.GetClippedSamples ?? '—') }}</span></div>
        <div class="kv__item"><span class="kv__label">GetVersion</span>
          <span class="kv__value mono">{{ JSON.stringify(wsState.values.GetVersion ?? '—') }}</span></div>
        <div class="kv__item"><span class="kv__label">GetCalibrationJobs</span>
          <span class="kv__value mono">{{ JSON.stringify((wsState.values.GetCalibrationJobs as { count?: number } | undefined)?.count ?? '—') }}</span></div>
      </div>
      <pre class="ws-console">{{ wsState.log.join('\n') || '（暂无 WS 日志）' }}</pre>
    </PanelCard>

    <div class="grid-3">
      <PanelCard v-for="card in placeholderCards" :key="card.title" :title="card.title">
        <template #extra>
          <span class="chip" :class="card.state === 'real_audio' ? 'chip--ok' : card.state === '待接线' || card.state === '未提供' ? 'chip--info' : 'chip--warn'">{{ card.state }}</span>
        </template>
        <p class="hint" style="margin: 0">{{ card.detail }}</p>
      </PanelCard>
    </div>
  </div>
</template>