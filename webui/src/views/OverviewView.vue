<script setup lang="ts">
// 概览：引擎真实性、运行状态、模式、音频、计数、音量、当前 job —— 全部来自 REST 实际值。
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import CopyText from '@/components/CopyText.vue'
import TruthTag from '@/components/TruthTag.vue'
import { refreshRuntime, runtimeState } from '@/stores/runtime'
import { contentState, loadContentLayout } from '@/stores/content'
import { api, errorText } from '@/api/client'
import type { CalJobsBody } from '@/api/types'
import {
  formatClock, formatDb, formatUnix, jobStateClass, runtimeStateClass, shortHash
} from '@/utils/format'

const router = useRouter()
const jobs = ref<CalJobsBody | null>(null)
const jobsError = ref('')
const refreshing = ref(false)

const status = computed(() => runtimeState.status)
const host = computed(() => runtimeState.status?.host ?? null)
const hostCounters = computed(() => {
  const snapshot = host.value
  if (!snapshot) { return [] }
  const items: Array<{ label: string; value: number }> = []
  const push = (label: string, value?: number) => {
    if (typeof value === 'number' && Number.isFinite(value)) { items.push({ label, value }) }
  }
  push('blocks', snapshot.blocks)
  push('frames', snapshot.frames)
  push('xruns', snapshot.xruns)
  push('underruns', snapshot.underruns)
  push('overruns', snapshot.overruns)
  push('recoveries', snapshot.recoveries)
  push('faults', snapshot.faults)
  push('deadline misses', snapshot.deadline_misses)
  push('silence blocks', snapshot.silence_blocks)
  push('forced mute blocks', snapshot.forced_mute_blocks)
  return items
})
const activeJob = computed(() => {
  const list = jobs.value?.jobs ?? []
  return list.find((job) => !['complete', 'failed', 'cancelled', 'rolled_back'].includes(job.state)) ?? null
})
const lastUpdatedText = computed(() => (runtimeState.lastUpdated ? formatClock(runtimeState.lastUpdated) : '—'))

async function loadJobs(): Promise<void> {
  try {
    jobs.value = await api.calJobsList()
    jobsError.value = ''
  } catch (error) {
    jobsError.value = errorText(error)
  }
}

async function refreshAll(): Promise<void> {
  refreshing.value = true
  await Promise.all([refreshRuntime(), loadJobs()])
  refreshing.value = false
}

onMounted(() => {
  void refreshAll()
  void loadContentLayout()
})

const contentLayoutText = computed(() => {
  switch (contentState.phase) {
    case 'not_wired': return '内容布局：待接线（host 需 plan-arena 图）'
    case 'presets_unavailable': return '内容布局：布局预设不可用'
    case 'busy': return '内容布局：内容路径忙（content_busy）'
    case 'unavailable': return '内容布局：host 不可用'
    case 'timeout': return '内容布局：内容适配超时'
    case 'error': return '内容布局：读取失败'
    case 'loading': return '内容布局：读取中…'
    default: break
  }
  const current = contentState.current
  if (!current) { return '内容布局：读取中…' }
  const appliedText = current.applied ? `applied@${current.applied_block}` : (current.adapter_status || 'disabled')
  if (!current.input_layout && !current.output_layout) {
    return '内容布局：' + appliedText + '（未配置布局）'
  }
  return '内容布局：' + (current.input_layout || '—') + ' → ' + (current.output_layout || '—') + ' / ' + appliedText
})
</script>

<template>
  <div class="page">
    <PageHeader
      title="概览"
      subtitle="运行状态与真实性徽标全部来自 REST /api/runtime/status；synthetic 回执不会被渲染成 actual。"
    >
      <template #right>
        <span class="hint hide-mobile">自动刷新 5s · 更新 {{ lastUpdatedText }}</span>
        <el-button :icon="Refresh" :loading="refreshing" @click="refreshAll">刷新</el-button>
      </template>
    </PageHeader>

    <div v-if="runtimeState.error" class="banner banner--danger">
      <div>
        <div class="banner__title">状态读取失败</div>
        <div class="banner__body">{{ runtimeState.error }}</div>
      </div>
    </div>

    <div v-if="status?.synthetic" class="banner banner--warn">
      <div>
        <div class="banner__title">当前 engine = {{ status.engine }}（synthetic）— 未接真实 host/MOTU</div>
        <div class="banner__body">
          所有 actual 回执（版本、epoch、graph_id、计数）来自内存模型，不是板端真实 4-worker all-ack。
          本界面按后端字段原样展示，不会把合成数据标成 actual。
        </div>
      </div>
    </div>

    <template v-if="!status">
      <div class="grid-2">
        <PanelCard title="运行状态"><el-skeleton :rows="5" animated /></PanelCard>
        <PanelCard title="引擎与真实性"><el-skeleton :rows="5" animated /></PanelCard>
      </div>
    </template>

    <template v-else>
      <div class="grid-2">
        <PanelCard title="运行状态" subtitle="REST /api/runtime/status（唯一 actual 来源）">
          <template #extra>
            <span class="chip" :class="runtimeStateClass(status.runtime_state)">{{ status.runtime_state }}</span>
          </template>
          <div class="kv">
            <div class="kv__item"><span class="kv__label">设备 owner</span><span class="kv__value">{{ status.device_owner }}</span></div>
            <div class="kv__item"><span class="kv__label">epoch</span><span class="kv__value">{{ status.epoch }}</span></div>
            <div class="kv__item"><span class="kv__label">actual_version</span><span class="kv__value">{{ status.actual_version }}</span></div>
            <div class="kv__item"><span class="kv__label">committed</span><span class="kv__value">{{ formatUnix(status.committed_unix) }}</span></div>
            <div class="kv__item"><span class="kv__label">校准独占设备</span>
              <span class="kv__value">{{ status.calibration_owner ? '是（calibration owner）' : '否' }}</span></div>
            <div class="kv__item"><span class="kv__label">pending prepare</span>
              <span class="kv__value mono">{{ status.pending_prepare ?? '无' }}</span>
              <span v-if="status.pending_plan" class="kv__hint">planned={{ status.pending_plan.planned_version }}</span>
            </div>
          </div>
        </PanelCard>

        <PanelCard title="引擎 / 真实性" subtitle="engine、synthetic、actual 原样带出（F-13 口径）">
          <TruthTag
            :engine="status.engine"
            :synthetic="status.synthetic"
            :actual="status.actual"
            :mode-services="status.mode_services"
          />
          <div class="kv" style="margin-top: 12px">
            <div class="kv__item"><span class="kv__label">synthetic</span><span class="kv__value">{{ status.synthetic }}</span></div>
            <div class="kv__item"><span class="kv__label">actual</span><span class="kv__value">{{ status.actual }}</span></div>
            <div class="kv__item"><span class="kv__label">mode_controller</span>
              <span class="kv__value">{{ status.mode_controller ? 'T44 状态机已接线' : '未接线' }}（{{ status.mode_services }}）</span></div>
            <div class="kv__item"><span class="kv__label">schema / api</span>
              <span class="kv__value mono">{{ status.schema }} · v{{ status.api_version }}</span></div>
          </div>
        </PanelCard>
      </div>

      <div class="grid-3">
        <PanelCard title="模式">
          <template #extra>
            <el-button link size="small" @click="router.push({ name: 'system' })">切换 / 详情</el-button>
          </template>
          <div class="stat">
            <span class="stat__label">{{ status.mode_label }}</span>
            <span class="stat__value" style="font-size: 20px">{{ status.mode }}</span>
            <span class="kv__hint">profile={{ status.mode_profile ?? '—' }} · 切换由 T44 状态机执行</span>
            <span
              class="chip"
              :class="contentState.phase === 'not_wired' ? 'chip--info' : contentState.phase === 'ok' ? (contentState.current?.applied ? 'chip--ok' : 'chip--warn') : 'chip--warn'"
              style="margin-top: 6px; align-self: flex-start"
            >
              {{ contentLayoutText }}
            </span>
          </div>
        </PanelCard>

        <PanelCard title="音频参数">
          <template #extra>
            <el-button link size="small" @click="router.push({ name: 'format' })">格式页</el-button>
          </template>
          <div class="stats-row">
            <div class="stat">
              <span class="stat__label">采样率</span>
              <span class="stat__value">{{ status.sample_rate.toLocaleString() }}<span class="stat__unit">Hz</span></span>
            </div>
            <div class="stat">
              <span class="stat__label">块长 / period</span>
              <span class="stat__value">{{ status.chunksize }}<span class="stat__unit">frames</span></span>
            </div>
          </div>
        </PanelCard>

        <PanelCard title="音量 / 静音">
          <template #extra>
            <el-button link size="small" @click="router.push({ name: 'volume' })">调节</el-button>
          </template>
          <div class="stats-row">
            <div class="stat">
              <span class="stat__label">音量</span>
              <span class="stat__value">{{ formatDb(status.volume_db) }}</span>
            </div>
            <div class="stat" :class="{ 'stat--warn': status.mute }">
              <span class="stat__label">静音</span>
              <span class="stat__value">{{ status.mute ? 'ON' : 'OFF' }}</span>
            </div>
          </div>
        </PanelCard>
      </div>

      <div class="grid-2">
        <PanelCard
          :title="host ? '运行计数（rt-host 实测快照）' : '运行计数'"
          :subtitle="host ? 'daemon 透传的 host.blocks/frames/xruns/... 原样展示' : 'xruns / deadline_misses / clipped_samples 来自实际状态快照'"
        >
          <template #extra>
            <span v-if="host" class="chip chip--ok">engine=rt-host actual</span>
          </template>
          <div v-if="hostCounters.length" class="stats-row">
            <div v-for="item in hostCounters" :key="item.label" class="stat">
              <span class="stat__label">{{ item.label }}</span>
              <span class="stat__value">{{ item.value.toLocaleString() }}</span>
            </div>
          </div>
          <div v-else class="stats-row">
            <div class="stat">
              <span class="stat__label">xruns</span>
              <span class="stat__value">{{ status.xruns }}</span>
            </div>
            <div class="stat" :class="{ 'stat--warn': status.deadline_misses > 0 }">
              <span class="stat__label">deadline misses</span>
              <span class="stat__value">{{ status.deadline_misses }}</span>
            </div>
            <div class="stat" :class="{ 'stat--warn': status.clipped_samples > 0 }">
              <span class="stat__label">clipped samples</span>
              <span class="stat__value">{{ status.clipped_samples }}</span>
            </div>
          </div>
          <div v-if="host" class="truth-tags" style="margin-top: 10px">
            <span v-if="host.safe_mode" class="chip">safe_mode={{ host.safe_mode }}</span>
            <span v-if="host.silent !== undefined" class="chip">silent={{ host.silent }}</span>
            <span v-if="host.forced_mute !== undefined" class="chip">forced_mute={{ host.forced_mute }}</span>
            <span v-if="host.safety_mute !== undefined" class="chip">safety_mute={{ host.safety_mute }}</span>
            <span v-if="host.graph" class="chip chip--mono">graph={{ shortHash(host.graph, 12) }}</span>
          </div>
          <p class="hint" style="margin: 10px 0 0">
            <template v-if="host">计数来自 host 音频块循环的真实快照（engine=rt-host, synthetic=false）；daemon 原样透传，页面不重算。</template>
            <template v-else>后端当前未提供 blocks / underruns / faults 字段（本页不编造数字）；计数语义见 T42 契约。task-313 的 rt-host 版本会在 status.host 中提供这些计数。</template>
          </p>
        </PanelCard>

        <PanelCard title="当前校准 job" subtitle="R48 /api/calibration/jobs（合成状态机也如实标注）">
          <template #extra>
            <el-button link size="small" @click="router.push({ name: 'calibration' })">校准页</el-button>
          </template>
          <div v-if="jobsError" class="banner banner--warn">{{ jobsError }}</div>
          <template v-else-if="activeJob">
            <div class="kv">
              <div class="kv__item"><span class="kv__label">job</span><span class="kv__value mono">{{ activeJob.job_id }}</span></div>
              <div class="kv__item"><span class="kv__label">kind</span><span class="kv__value">{{ activeJob.kind }}</span></div>
              <div class="kv__item"><span class="kv__label">state</span>
                <span class="kv__value"><span class="chip" :class="jobStateClass(activeJob.state)">{{ activeJob.state }}</span></span>
              </div>
              <div class="kv__item"><span class="kv__label">progress</span>
                <span class="kv__value">{{ Math.round(activeJob.progress * 100) }}%</span></div>
            </div>
          </template>
          <div v-else class="hint">
            当前无进行中的校准 job（共 {{ jobs?.count ?? 0 }} 个历史记录）。
            <span v-if="jobs">engine={{ jobs.engine }} · synthetic={{ jobs.synthetic }}</span>
          </div>
        </PanelCard>
      </div>

      <PanelCard title="目标 / 图 / 系数" subtitle="target 由 T57 overlay 解析；hash 与图 id 用于证据比对">
        <div class="kv">
          <div class="kv__item"><span class="kv__label">layout / channels / profile</span>
            <span class="kv__value">{{ status.target.layout }} · {{ status.target.channels }}ch · {{ status.target.profile }}</span></div>
          <div class="kv__item"><span class="kv__label">角色 → DAC</span>
            <span class="kv__value mono">
              {{ status.target.roles.map((r) => `${r.role}→${r.dac}`).join('  ') || '—' }}
            </span></div>
          <div class="kv__item"><span class="kv__label">低带 dense DAC</span>
            <span class="kv__value mono">{{ status.target.low_band_dacs.join(', ') || '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">target hash</span>
            <CopyText :value="status.target_hash" :short="20" /></div>
          <div class="kv__item"><span class="kv__label">graph_id</span>
            <CopyText :value="status.graph_id" :short="20" /></div>
          <div class="kv__item"><span class="kv__label">coeff / last-good</span>
            <span class="kv__value mono">
              {{ status.coeff ? status.coeff.version : '—' }} / {{ status.last_good_coeff ? status.last_good_coeff.version : '—' }}
            </span></div>
        </div>
      </PanelCard>
    </template>
  </div>
</template>

<style scoped>
.stats-row { display: grid; grid-template-columns: repeat(auto-fit, minmax(120px, 1fr)); gap: 10px; }
</style>