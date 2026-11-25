<script setup lang="ts">
// 校准：job 列表/详情、启动完整/复测、取消/apply/retest/rollback、状态机时间线。
// 合成状态机与「未应用到音频」事实必须原样展示（applied_to_audio 恒 false）。
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { Refresh, VideoPlay, CircleCheck, RefreshRight, Back, Close } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import AckPanel from '@/components/AckPanel.vue'
import TruthTag from '@/components/TruthTag.vue'
import { api, ApiError, errorText } from '@/api/client'
import type { AckView, CalJob, CalJobActionBody, CalJobsBody, PrepareBody } from '@/api/types'
import {
  formatClock, formatUnix, isTerminalJob, jobStateClass, JOB_STEPS, JOB_STEP_LABELS
} from '@/utils/format'

const jobsBody = ref<CalJobsBody | null>(null)
const loadError = ref('')
const loading = ref(false)
const startBusy = ref(false)
const actionBusyId = ref('')
const ack = ref<AckView | null>(null)

const kind = ref<'full' | 'retest' | 'emperor'>('full')
const label = ref('')
const targetCurve = ref<'flat' | 'tilt10'>('flat')
const fHi = ref<number | null>(150)
const supportsText = ref('')
const audibleAuthorized = ref(false)
const advancedOpen = ref<string[]>([])
const stage = ref<'measure' | 'solve' | 'remeasure'>('measure')

const detailOpen = ref(false)
const detailLoading = ref(false)
const detail = ref<CalJobActionBody | null>(null)
const detailJobId = ref('')

const calEngine = computed(() => jobsBody.value?.cal_engine ?? null)
const jobs = computed(() => jobsBody.value?.jobs ?? [])

function jobField<T>(payload: CalJobActionBody, key: 'job_id' | 'state' | 'progress'): T | undefined {
  return (payload[key] as T | undefined) ?? (payload.job?.[key] as T | undefined)
}

function ackFromJob(payload: CalJobActionBody, detailText: string): AckView {
  const synthetic = payload.synthetic ?? payload.cal_engine?.synthetic ?? null
  const realAudio = payload.real_audio ?? payload.cal_engine?.real_audio ?? null
  return {
    kind: synthetic === true ? 'synthetic' : realAudio === true ? 'actual' : 'info',
    engine: payload.engine ?? payload.cal_engine?.engine ?? null,
    synthetic,
    actual: realAudio,
    detail: detailText,
    raw: payload
  }
}

function ackFromError(prefix: string, error: unknown): AckView {
  const code = error instanceof ApiError ? error.code : 'error'
  const unsupported = ['unsupported', 'not_implemented', 'not_supported', 'service_unavailable'].includes(code)
  return {
    kind: unsupported ? 'unsupported' : 'error',
    engine: null,
    synthetic: null,
    actual: null,
    detail: `${prefix}：${errorText(error)}`,
    raw: error instanceof ApiError ? error.body : String(error)
  }
}

async function loadJobs(silent = false): Promise<void> {
  if (!silent) { loading.value = true }
  try {
    jobsBody.value = await api.calJobsList()
    loadError.value = ''
  } catch (error) {
    loadError.value = errorText(error)
  } finally {
    loading.value = false
  }
}

let timer: number | null = null

onMounted(() => {
  void loadJobs()
  timer = window.setInterval(() => {
    if (!document.hidden) { void loadJobs(true) }
  }, 2500)
})

onUnmounted(() => {
  if (timer !== null) { window.clearInterval(timer); timer = null }
})

async function startJob(): Promise<void> {
  startBusy.value = true
  ack.value = null
  try {
    const base: Record<string, unknown> = {
      auto_apply: false,
      audible_sweep_authorized: audibleAuthorized.value
    }
    if (label.value.trim()) { base.label = label.value.trim() }
    // 高级参数：不同 daemon 构建接受字段不同（当前部署版拒绝 f_hi/stage/supports/target）。
    // 先带高级参数；若后端 unknown_field 则最小请求重试，并如实说明。
    const advanced: Record<string, unknown> = {}
    if (advancedOpen.value.includes('adv')) {
      if (fHi.value !== null) { advanced.f_hi = fHi.value }
      const supports = supportsText.value.split(/[,，\s]+/).map((item) => item.trim()).filter(Boolean)
      if (supports.length) { advanced.supports = supports }
      advanced.target = targetCurve.value
      if (kind.value === 'emperor' && stage.value) { advanced.stage = stage.value }
    }
    let payload: CalJobActionBody
    let fallbackNote = ''
    try {
      payload = Object.keys(advanced).length
        ? await api.calJobStart(kind.value, { ...base, ...advanced })
        : await api.calJobStart(kind.value, base)
    } catch (error) {
      const fields = Object.keys(advanced)
      if (error instanceof ApiError && error.code === 'unknown_field' && fields.length) {
        payload = await api.calJobStart(kind.value, base)
        fallbackNote = `（注意：本后端版本不接受高级参数 ${fields.join(', ')}，已用最小请求重发；该次启动未包含高级设置）`
      } else {
        throw error
      }
    }
    ack.value = ackFromJob(
      payload,
      `已启动 job ${payload.job_id}（kind=${kind.value}，state=${payload.state}）· audio_status=${payload.audio_status} · applied_to_audio=${payload.applied_to_audio}${fallbackNote}`
    )
    await loadJobs()
    ElMessage.success('校准 job 已启动')
  } catch (error) {
    ack.value = ackFromError('启动校准失败', error)
  } finally {
    startBusy.value = false
  }
}

async function runAction(job: CalJob, action: 'cancel' | 'apply' | 'retest' | 'rollback'): Promise<void> {
  const labels: Record<string, string> = { cancel: '取消', apply: '应用', retest: '复测', rollback: '回滚' }
  if (action === 'rollback') {
    try {
      await ElMessageBox.confirm(
        `回滚 job ${job.job_id} 到 last-good（${job.last_good_version}）。这是控制面动作，回执会标注 synthetic 或 actual。`,
        '确认回滚', { confirmButtonText: '回滚', cancelButtonText: '取消', type: 'warning' }
      )
    } catch { return }
  }
  actionBusyId.value = `${job.job_id}:${action}`
  ack.value = null
  try {
    const handler = {
      cancel: api.calJobCancel,
      apply: api.calJobApply,
      retest: api.calJobRetest,
      rollback: api.calJobRollback
    }[action]
    const payload = await handler(job.job_id)
    const resultId = payload.job_id ?? payload.job?.job_id ?? job.job_id
    const resultState = payload.state ?? payload.job?.state ?? job.state
    ack.value = ackFromJob(
      payload,
      `${labels[action]}成功：job=${resultId} state=${resultState}` +
      `${payload.job?.applied_version ? ' · applied_version=' + payload.job.applied_version : ''}` +
      `${payload.job?.last_good_version ? ' · last_good=' + payload.job.last_good_version : ''}` +
      ` · applied_to_audio=${payload.applied_to_audio ?? '—'}`
    )
    await loadJobs()
    ElMessage.success(`${labels[action]}成功`)
  } catch (error) {
    ack.value = ackFromError(`${labels[action]}失败`, error)
  } finally {
    actionBusyId.value = ''
  }
}

async function openDetail(job: CalJob): Promise<void> {
  detailJobId.value = job.job_id
  detailOpen.value = true
  detailLoading.value = true
  detail.value = null
  try {
    detail.value = await api.calJobGet(job.job_id)
  } catch (error) {
    ack.value = ackFromError('读取 job 详情失败', error)
  } finally {
    detailLoading.value = false
  }
}

function timelineIndex(state: string): number {
  const index = JOB_STEPS.indexOf(state as never)
  if (index >= 0) { return index }
  return -1
}

function stepClass(step: string, state: string): string {
  const current = timelineIndex(state)
  const index = JOB_STEPS.indexOf(step as never)
  if (state === 'failed') {
    if (index < 0) { return '' }
    return index === Math.max(0, JOB_STEPS.length - 2) ? 'is-failed' : (index < 5 ? 'is-done' : '')
  }
  if (current >= 0) {
    if (index < current) { return 'is-done' }
    if (index === current) { return 'is-current' }
    return ''
  }
  if (state === 'complete') { return 'is-done' }
  if (state === 'cancelled' || state === 'rolled_back') { return index <= 1 ? 'is-done' : '' }
  return ''
}

const canCancel = (job: CalJob): boolean => !isTerminalJob(job.state)
const canApply = (job: CalJob): boolean => job.state === 'prepared'
const canRetest = (job: CalJob): boolean => !!job.applied_version
const canRollback = (job: CalJob): boolean => !!job.last_good_version
</script>

<template>
  <div class="page">
    <PageHeader
      title="校准"
      subtitle="R48 端点：启动 full/retest、取消、apply、retest、rollback；job 状态机时间线。离线合成回执明确标 synthetic。"
    >
      <template #right>
        <el-button :icon="Refresh" :loading="loading" @click="loadJobs()">刷新</el-button>
      </template>
    </PageHeader>

    <div v-if="loadError" class="banner banner--danger">
      <div><div class="banner__title">校准 job 读取失败</div><div class="banner__body">{{ loadError }}</div></div>
    </div>

    <PanelCard v-if="calEngine" title="校准引擎 / 真实性" subtitle="engine、synthetic、real_audio、backend 原样来自 REST">
      <template #extra>
        <span class="truth-tags">
          <span class="chip chip--mono">engine={{ calEngine.engine }}</span>
          <span v-if="calEngine.synthetic" class="chip chip--warn">synthetic（离线合成测量/apply）</span>
          <span v-else class="chip chip--info">状态机与测量链路真实</span>
          <span class="chip" :class="calEngine.real_audio ? 'chip--ok' : 'chip--warn'">real_audio={{ calEngine.real_audio }}</span>
        </span>
      </template>
      <div class="kv">
        <div class="kv__item"><span class="kv__label">engine</span><span class="kv__value mono">{{ calEngine.engine }}</span></div>
        <div class="kv__item"><span class="kv__label">backend</span><span class="kv__value mono">{{ calEngine.backend }}</span></div>
        <div class="kv__item"><span class="kv__label">synthetic / real_audio</span>
          <span class="kv__value">{{ calEngine.synthetic }} / {{ calEngine.real_audio }}</span></div>
        <div class="kv__item"><span class="kv__label">note</span><span class="kv__value">{{ calEngine.note }}</span></div>
      </div>
      <div class="banner" :class="calEngine.synthetic ? 'banner--warn' : 'banner--info'" style="margin-top: 12px">
        <div>
          <div class="banner__title">
            apply 成功 ≠ 音频已应用（{{ calEngine.synthetic ? 'synthetic 离线合成 ack' : 'file 后端回执' }}）
          </div>
          <div class="banner__body">
            后端 note 原文：{{ calEngine.note }}<br />
            页面依据每个 job 的 applied / applied_to_audio / audio_status 字段展示（host 尚未消费校准结果时为 NOT_APPLIED_TO_AUDIO，不冒充板上生效）。
          </div>
        </div>
      </div>
    </PanelCard>

    <div class="grid-2">
      <PanelCard title="启动校准" subtitle="同一时刻只允许一个 active job（冲突会 409 回显）">
        <div class="form-grid">
          <div class="form-field">
            <label>类型</label>
            <el-select v-model="kind">
              <el-option label="完整校准（full）" value="full" />
              <el-option label="复测（retest）" value="retest" />
              <el-option label="emperor（多阶段）" value="emperor" />
            </el-select>
          </div>
          <div class="form-field">
            <label>label（可选，人读）</label>
            <el-input v-model="label" placeholder="例如：客厅-夜间" maxlength="128" />
          </div>
        </div>
        <el-collapse v-model="advancedOpen" style="margin-top: 10px">
          <el-collapse-item
            title="高级参数（目标曲线 / 频段 / 支援喇叭 / emperor 阶段 / 可闻扫频授权）"
            name="adv"
          >
            <div class="form-grid">
              <div class="form-field">
                <label>目标曲线</label>
                <el-select v-model="targetCurve">
                  <el-option label="flat（平直）" value="flat" />
                  <el-option label="tilt10（20Hz 0 → 20kHz -10dB）" value="tilt10" />
                </el-select>
              </div>
              <div class="form-field">
                <label>支援频段上沿 f_hi（Hz，后端上限 150）</label>
                <el-input-number v-model="fHi" :min="20" :max="150" :step="10" style="width: 100%" />
              </div>
              <div class="form-field">
                <label>emperor 阶段（仅 kind=emperor）</label>
                <el-select v-model="stage" :disabled="kind !== 'emperor'">
                  <el-option label="measure" value="measure" />
                  <el-option label="solve" value="solve" />
                  <el-option label="remeasure" value="remeasure" />
                </el-select>
              </div>
              <div class="form-field">
                <label>支援喇叭（逗号分隔，可空）</label>
                <el-input v-model="supportsText" placeholder="例如：FL,FR,SL,SR" />
              </div>
              <div class="form-field">
                <label>可闻扫频授权（真实测量必须；离线 synthetic 不发声）</label>
                <el-switch v-model="audibleAuthorized" active-text="已授权" inactive-text="未授权" />
              </div>
            </div>
            <p class="hint" style="margin: 8px 0 0">
              版本差异：不同 daemon 构建接受的启动字段不同（当前部署版接受 kind/label/auto_apply/audible_sweep_authorized，
              拒绝 f_hi/stage/supports/target 并返回 unknown_field）。UI 会自动用最小请求重试并如实标注，
              不会把未生效的高级参数写成已应用。
            </p>
          </el-collapse-item>
        </el-collapse>
        <div class="actions" style="margin-top: 12px">
          <el-button type="primary" :icon="VideoPlay" :loading="startBusy" @click="startJob">
            启动{{ kind === 'retest' ? '复测' : kind === 'emperor' ? ' emperor 分阶段' : '完整' }}校准
          </el-button>
        </div>
      </PanelCard>

      <PanelCard title="状态机说明" subtitle="T60 JobManager 状态（真实执行）">
        <div class="hint" style="line-height: 1.8">
          queued → preflight → sweep_capture → quality_check → fit_p → solve_g → compile →
          prepared → apply → remeasure → independent_holdout → complete。
          终态：complete / failed / cancelled / rolled_back。<br />
          apply 只对停在 prepared 的 job 放行；retest 需要已 applied 版本；rollback 回 last-good
          （离线模式返回 last-good-offline-v0，不代表板端恢复已执行）。
        </div>
      </PanelCard>
    </div>

    <PanelCard title="校准 job 列表" :subtitle="`共 ${jobsBody?.count ?? 0} 个 · engine=${jobsBody?.engine ?? '—'} · synthetic=${jobsBody?.synthetic ?? '—'}`">
      <template #extra>
        <span class="chip" :class="jobsBody?.synthetic ? 'chip--warn' : 'chip--ok'">
          {{ jobsBody?.synthetic ? 'synthetic 状态机' : 'actual' }}
        </span>
      </template>

      <div class="table-scroll is-jobs">
        <table class="data-table">
          <thead>
            <tr>
              <th>job</th><th>kind</th><th>state / stage</th><th>progress</th>
              <th>applied_version</th><th>last_good</th><th>updated</th><th>操作</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="job in jobs" :key="job.job_id">
              <td><a href="#" class="mono" @click.prevent="openDetail(job)">{{ job.job_id }}</a></td>
              <td>{{ job.kind }}</td>
              <td>
                <span class="chip" :class="jobStateClass(job.state)">{{ job.state }}</span>
                <span v-if="job.stage" class="hint"> / {{ job.stage }}</span>
                <div v-if="job.failure" class="hint" style="color: var(--khaen-danger)">{{ job.failure }}</div>
              </td>
              <td style="min-width: 110px"><el-progress :percentage="Math.round(job.progress * 100)" :show-text="false" :stroke-width="8" /></td>
              <td class="mono">{{ job.applied_version ?? '—' }}</td>
              <td class="mono">{{ job.last_good_version ?? '—' }}</td>
              <td class="hint">{{ formatClock(job.updated_unix * 1000) }}</td>
              <td class="nowrap">
                <el-button link size="small" @click="openDetail(job)">详情</el-button>
                <el-button link size="small" type="warning" :disabled="!canCancel(job)" :loading="actionBusyId === job.job_id + ':cancel'" @click="runAction(job, 'cancel')">取消</el-button>
                <el-button link size="small" type="primary" :disabled="!canApply(job)" :loading="actionBusyId === job.job_id + ':apply'" @click="runAction(job, 'apply')">应用</el-button>
                <el-button link size="small" :disabled="!canRetest(job)" :loading="actionBusyId === job.job_id + ':retest'" @click="runAction(job, 'retest')">复测</el-button>
                <el-button link size="small" type="danger" :disabled="!canRollback(job)" :loading="actionBusyId === job.job_id + ':rollback'" @click="runAction(job, 'rollback')">回滚</el-button>
              </td>
            </tr>
            <tr v-if="!jobs.length">
              <td colspan="8" class="hint">暂无校准 job（点击上方「启动校准」创建）。</td>
            </tr>
          </tbody>
        </table>
      </div>

      <div class="job-cards">
        <div v-for="job in jobs" :key="job.job_id" class="panel panel--flat">
          <div style="display: flex; justify-content: space-between; gap: 8px; align-items: center">
            <a href="#" class="mono" @click.prevent="openDetail(job)">{{ job.job_id }}</a>
            <span class="chip" :class="jobStateClass(job.state)">{{ job.state }}</span>
          </div>
          <div class="kv" style="margin-top: 8px">
            <div class="kv__item"><span class="kv__label">kind / stage</span>
              <span class="kv__value">{{ job.kind }} / {{ job.stage ?? '—' }}</span></div>
            <div class="kv__item"><span class="kv__label">applied · last-good</span>
              <span class="kv__value mono">{{ job.applied_version ?? '—' }} · {{ job.last_good_version ?? '—' }}</span></div>
            <div class="kv__item"><span class="kv__label">updated</span>
              <span class="kv__value">{{ formatUnix(job.updated_unix) }}</span></div>
          </div>
          <el-progress :percentage="Math.round(job.progress * 100)" :stroke-width="8" style="margin: 8px 0" />
          <div v-if="job.failure" class="hint" style="color: var(--khaen-danger)">{{ job.failure }}</div>
          <div class="actions" style="margin-top: 8px">
            <el-button size="small" @click="openDetail(job)">详情</el-button>
            <el-button size="small" :disabled="!canCancel(job)" @click="runAction(job, 'cancel')">取消</el-button>
            <el-button size="small" type="primary" :disabled="!canApply(job)" @click="runAction(job, 'apply')">应用</el-button>
            <el-button size="small" :disabled="!canRetest(job)" @click="runAction(job, 'retest')">复测</el-button>
            <el-button size="small" type="danger" :disabled="!canRollback(job)" @click="runAction(job, 'rollback')">回滚</el-button>
          </div>
        </div>
        <div v-if="!jobs.length" class="hint">暂无校准 job。</div>
      </div>
    </PanelCard>

    <PanelCard title="操作回执">
      <AckPanel :ack="ack" title="校准回执" />
      <div v-if="!ack" class="hint">尚无校准操作回执。</div>
    </PanelCard>

    <el-dialog v-model="detailOpen" :title="'校准 job 详情 · ' + detailJobId" width="680px" top="6vh">
      <div v-if="detailLoading" class="hint">读取中…</div>
      <template v-else-if="detail">
        <div class="banner" :class="(detail.synthetic ?? detail.cal_engine?.synthetic) ? 'banner--warn' : 'banner--info'" style="margin-bottom: 12px">
          <div>
            <div class="banner__title">applied_to_audio={{ detail.applied_to_audio ?? '—' }} · audio_status={{ detail.audio_status ?? '—' }}</div>
            <div class="banner__body">
              applied={{ detail.applied ?? '—' }} · not_applied_to_audio={{ detail.not_applied_to_audio ?? '—' }} ·
              measured_audio={{ detail.measured_audio ?? '—' }} · production_calibration={{ detail.production_calibration ?? '—' }}<br />
              scope={{ detail.calibration_scope ?? '—' }}
            </div>
          </div>
        </div>
        <div class="kv">
          <div class="kv__item"><span class="kv__label">kind / state</span>
            <span class="kv__value">{{ detail.job.kind }} / {{ detail.state ?? detail.job.state }}</span></div>
          <div class="kv__item"><span class="kv__label">progress</span>
            <span class="kv__value">{{ Math.round((detail.progress ?? detail.job.progress) * 100) }}%</span></div>
          <div class="kv__item"><span class="kv__label">applied_version</span>
            <span class="kv__value mono">{{ detail.job.applied_version ?? '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">last_good_version</span>
            <span class="kv__value mono">{{ detail.job.last_good_version ?? '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">created / updated</span>
            <span class="kv__value">{{ formatUnix(detail.job.created_unix) }} / {{ formatUnix(detail.job.updated_unix) }}</span></div>
          <div class="kv__item"><span class="kv__label">failure</span>
            <span class="kv__value">{{ detail.job.failure ?? '—' }}</span></div>
        </div>
        <h3 style="font-size: 14px; margin: 16px 0 8px">状态机时间线</h3>
        <ol class="timeline">
          <li v-for="step in JOB_STEPS" :key="step" class="timeline__step" :class="stepClass(step, detail.state ?? detail.job.state)">
            <div class="timeline__rail">
              <span class="timeline__dot" />
              <span v-if="step !== 'complete'" class="timeline__line" />
            </div>
            <div class="timeline__label">
              {{ JOB_STEP_LABELS[step] ?? step }}
              <span class="mono timeline__meta">{{ step }}</span>
            </div>
          </li>
        </ol>
        <el-collapse style="margin-top: 8px">
          <el-collapse-item title="原始 job JSON" name="raw">
            <pre class="spoiler-json">{{ JSON.stringify(detail, null, 2) }}</pre>
          </el-collapse-item>
        </el-collapse>
      </template>
      <div v-else class="hint">详情不可用。</div>
    </el-dialog>
  </div>
</template>