#!/usr/bin/env node
/**
 * 校准 REST 契约 fixture（DEV/TEST ONLY；scripts/ 不参与 vite 打包，绝不进 dist）。
 *
 * 四种诚实形态（PLAN §4.7 / §10；任务 T37）：
 *  - empty:       无向导配置、无 job、无上传（全新状态；向导从第 1 步开始）；
 *  - success:     向导配置已保存 + 两段测量上传 + 一个 succeeded/PASS 的离线 job（第 6/7 步可演示；
 *                 第 8 步 apply 未做，所以 gate=blocked，等待用户在页面里真实 apply）；
 *  - failed:      配置已保存 + 一个 failed job（稳定错误码，verdict=null；演示失败态/重试）；
 *  - unavailable: 模拟 daemon 的 NotWired 后端（GET /jobs → available:false；job/apply/artifact → 503 not_wired），
 *                 wizard 配置与 state 的真实部分照常可用（与 T36 的默认实现一致）。
 *
 * 所有时间戳固定、artifact 内容固定、sha256 由内容计算：测试断言与截图可复现。
 * 这些是**mock 数据**：job 的 engine 字段固定为 "synthetic"，前端据此打「示意」TruthTag，绝不冒充实测。
 */
import { createHash } from 'node:crypto'

export const CALIBRATION_FIXTURES = ['empty', 'success', 'failed', 'unavailable']
export const DEFAULT_CALIBRATION_FIXTURE = 'empty'

export function isCalibrationFixture(value) {
  return typeof value === 'string' && CALIBRATION_FIXTURES.includes(value)
}

/** 1/3-oct 中心频率 25 Hz–20 kHz（30 个；与 calibration CLI / daemon 默认一致）。 */
export const DEFAULT_CENTERS_HZ = [
  25, 31.5, 40, 50, 63, 80, 100, 125, 160, 200, 250, 315, 400, 500, 630, 800, 1000, 1250, 1600, 2000,
  2500, 3150, 4000, 5000, 6300, 8000, 10000, 12500, 16000, 20000
]

/** 向导配置默认值（daemon GET /wizard 的 defaults；真实来源 designer::BuiltinTarget）。 */
export function buildWizardDefaults() {
  return {
    mic_cal_sha256: null,
    mic_cal_filename: null,
    orientation: null,
    target_mode: 'flat',
    single_point: {
      centers_hz: [...DEFAULT_CENTERS_HZ],
      tolerance_db: 2.0,
      min_bands: 3
    }
  }
}

const FIXED_AT = '2026-11-26T06:00:00.000Z'
const JOB_CREATED_AT = '2026-11-26T06:05:00.000Z'
const JOB_STARTED_AT = '2026-11-26T06:05:01.000Z'
const JOB_FINISHED_AT = '2026-11-26T06:05:09.000Z'
const UPLOAD_CREATED_AT = '2026-11-26T06:04:00.000Z'

/** sha256 hex（确定性 artifact/上传 id）。 */
export function sha256Hex(text) {
  return createHash('sha256').update(text).digest('hex')
}

const SUCCESS_JOB_ID = 'job-20261126-0001'
const FAILED_JOB_ID = 'job-20261126-0002'

const BASELINE_TEXT = 'mock-baseline-wav-fixture'
const APPLIED_TEXT = 'mock-applied-wav-fixture'
const MIC_CAL_TEXT = 'mock-umik-calibration-fixture'

const BASELINE_SHA = sha256Hex(BASELINE_TEXT)
const APPLIED_SHA = sha256Hex(APPLIED_TEXT)
const MIC_CAL_SHA = sha256Hex(MIC_CAL_TEXT)

/** 演示用上传记录（bytes 只是 mock 占位长度；不是真实 WAV）。 */
export function buildFixtureUploads() {
  return [
    {
      upload_id: BASELINE_SHA,
      sha256: BASELINE_SHA,
      bytes: 1048576,
      content_type: 'audio/wav',
      role: 'baseline',
      created_at: UPLOAD_CREATED_AT
    },
    {
      upload_id: APPLIED_SHA,
      sha256: APPLIED_SHA,
      bytes: 1048576,
      content_type: 'audio/wav',
      role: 'applied',
      created_at: UPLOAD_CREATED_AT
    },
    {
      upload_id: MIC_CAL_SHA,
      sha256: MIC_CAL_SHA,
      bytes: 4096,
      content_type: 'application/octet-stream',
      role: 'mic_cal',
      created_at: UPLOAD_CREATED_AT
    }
  ]
}

/** 已保存的向导配置（success/failed fixture 共用；与 daemon 默认同值 + 麦克风文件）。 */
export function buildSavedWizardConfig() {
  return {
    ...buildWizardDefaults(),
    mic_cal_sha256: MIC_CAL_SHA,
    mic_cal_filename: 'umik-1_90deg.txt',
    orientation: '90deg',
    target_mode: 'flat'
  }
}

function round(value, digits = 2) {
  const factor = 10 ** digits
  return Math.round(value * factor) / factor
}

/** 由中心频率生成确定性的 bands / verdict（mock 数据，engine=synthetic）。 */
export function buildJobResult(centersHz, options = {}) {
  const tolerance = typeof options.tolerance_db === 'number' ? options.tolerance_db : 2.0
  const minBands = typeof options.min_bands === 'number' ? options.min_bands : 3
  const bands = centersHz.map((hz, index) => {
    const predicted = round(-1.5 * Math.log10(hz / 1000))
    const delta = round(0.35 * Math.sin(index * 1.7) + 0.15, 2)
    return {
      hz,
      predicted_db: predicted,
      observed_db: round(predicted + delta),
      delta_db: delta,
      missing: false
    }
  })
  const maxDelta = bands.reduce((max, band) => Math.max(max, Math.abs(band.delta_db ?? 0)), 0)
  const verdict = maxDelta <= tolerance && bands.length >= minBands ? 'PASS' : 'FAIL'
  const violations =
    verdict === 'PASS'
      ? []
      : ['max_delta_db=' + String(round(maxDelta)) + ' exceeds tolerance_db=' + String(tolerance)]
  return {
    verdict,
    max_delta_db: round(maxDelta),
    compared_bands: bands.length,
    missing_bands: 0,
    violations,
    bands,
    design: {
      residual: 'FULL',
      admission: { accepted: true, code: null, reason: null }
    },
    plan: null
  }
}

/** artifact 内容（确定性 JSON / 二进制）；返回 {name, content_type, content(Buffer)}。 */
export function buildJobArtifacts(jobId, result) {
  const planText = JSON.stringify({ mock: true, job_id: jobId, kind: 'single_point_offline' })
  const reportText = JSON.stringify({ job_id: jobId, result }, null, 2)
  const planSha = sha256Hex(planText)
  const reportSha = sha256Hex(reportText)
  const manifest = {
    job_id: jobId,
    artifacts: [
      { name: 'report.json', sha256: reportSha, content_type: 'application/json' },
      { name: 'plan.bin', sha256: planSha, content_type: 'application/octet-stream' }
    ]
  }
  const manifestText = JSON.stringify(manifest, null, 2)
  return {
    resultWithPlan: { ...result, plan: { sha256: planSha, graph_id: 'mock-' + planSha.slice(0, 12) } },
    artifacts: [
      { name: 'report.json', content_type: 'application/json', content: Buffer.from(reportText, 'utf8') },
      { name: 'plan.bin', content_type: 'application/octet-stream', content: Buffer.from(planText, 'utf8') },
      { name: 'artifacts.manifest.json', content_type: 'application/json', content: Buffer.from(manifestText, 'utf8') }
    ]
  }
}

function baseSteps() {
  return ['compile', 'levels', 'closed_loop', 'bundle'].map((name) => ({
    name,
    status: 'pending',
    started_at: null,
    finished_at: null,
    sha256: null,
    detail: null
  }))
}

function succeededJob(jobId, inputs) {
  const steps = baseSteps().map((step, index) => ({
    ...step,
    status: 'succeeded',
    started_at: '2026-11-26T06:05:0' + String(index + 1) + '.000Z',
    finished_at: '2026-11-26T06:05:0' + String(index + 2) + '.000Z',
    sha256: sha256Hex(jobId + ':' + step.name)
  }))
  const rawResult = buildJobResult(inputs.centers_hz, {
    tolerance_db: inputs.tolerance_db,
    min_bands: inputs.min_bands
  })
  const built = buildJobArtifacts(jobId, rawResult)
  return {
    id: jobId,
    kind: 'single_point_offline',
    status: 'succeeded',
    verdict: built.resultWithPlan.verdict,
    created_at: JOB_CREATED_AT,
    started_at: JOB_STARTED_AT,
    finished_at: JOB_FINISHED_AT,
    // 【加法】epoch 秒字段：真实 daemon 会带；缺失也应容忍（unit test 覆盖）。
    created_unix: Math.floor(Date.parse(JOB_CREATED_AT) / 1000),
    updated_unix: Math.floor(Date.parse(JOB_FINISHED_AT) / 1000),
    duration_ms: 8000,
    engine: 'synthetic',
    apply_state: 'not_applied',
    applied_version: null,
    steps,
    // 【加法】downloadable 由 daemon 的白名单决定（fixture 里与真实白名单一致）。
    artifacts: built.artifacts.map((artifact) => ({
      name: artifact.name,
      sha256: sha256Hex(artifact.content.toString('utf8')),
      bytes: artifact.content.length,
      content_type: artifact.content_type,
      downloadable: true
    })),
    inputs,
    result: built.resultWithPlan,
    error: null,
    reason: null,
    artifactContent: built.artifacts
  }
}

function failedJob(jobId, inputs) {
  const steps = baseSteps()
  steps[0] = {
    ...steps[0],
    status: 'succeeded',
    started_at: JOB_STARTED_AT,
    finished_at: '2026-11-26T06:05:02.000Z',
    sha256: sha256Hex(jobId + ':compile')
  }
  steps[1] = {
    ...steps[1],
    status: 'succeeded',
    started_at: '2026-11-26T06:05:02.000Z',
    finished_at: '2026-11-26T06:05:04.000Z',
    sha256: sha256Hex(jobId + ':levels')
  }
  steps[2] = {
    ...steps[2],
    status: 'failed',
    started_at: '2026-11-26T06:05:04.000Z',
    finished_at: '2026-11-26T06:05:06.000Z',
    detail: 'E_CLOSED_LOOP_DELTA: max delta 4.20 dB exceeds tolerance 2.00 dB'
  }
  steps[3] = { ...steps[3], status: 'cancelled' }
  return {
    id: jobId,
    kind: 'single_point_offline',
    status: 'failed',
    verdict: null,
    created_at: JOB_CREATED_AT,
    started_at: JOB_STARTED_AT,
    finished_at: '2026-11-26T06:05:06.000Z',
    created_unix: Math.floor(Date.parse(JOB_CREATED_AT) / 1000),
    updated_unix: Math.floor(Date.parse('2026-11-26T06:05:06.000Z') / 1000),
    duration_ms: 5000,
    engine: 'synthetic',
    apply_state: 'not_applied',
    applied_version: null,
    steps,
    artifacts: [],
    inputs,
    result: null,
    error: { code: 'E_CLOSED_LOOP_DELTA', message: 'max delta 4.20 dB exceeds tolerance 2.00 dB' },
    reason: null,
    artifactContent: []
  }
}

function buildInputs() {
  return {
    plan_sha: null,
    baseline_upload_id: BASELINE_SHA,
    applied_upload_id: APPLIED_SHA,
    centers_hz: [...DEFAULT_CENTERS_HZ],
    tolerance_db: 2.0,
    min_bands: 3
  }
}

/**
 * 构造 fixture 种子：{ config: 已保存配置|null, jobs: [], uploads: [], unavailable: bool, appliedJobId }
 * @param {string} name
 */
export function buildCalibrationSeed(name) {
  switch (name) {
    case 'success': {
      const job = succeededJob(SUCCESS_JOB_ID, buildInputs())
      return {
        config: buildSavedWizardConfig(),
        jobs: [job],
        uploads: buildFixtureUploads(),
        unavailable: false,
        appliedJobId: null,
        now: FIXED_AT
      }
    }
    case 'failed': {
      const job = failedJob(FAILED_JOB_ID, buildInputs())
      return {
        config: buildSavedWizardConfig(),
        jobs: [job],
        uploads: buildFixtureUploads(),
        unavailable: false,
        appliedJobId: null,
        now: FIXED_AT
      }
    }
    case 'unavailable':
      return {
        config: null,
        jobs: [],
        uploads: [],
        unavailable: true,
        appliedJobId: null,
        now: FIXED_AT
      }
    case 'empty':
    default:
      return {
        config: null,
        jobs: [],
        uploads: [],
        unavailable: false,
        appliedJobId: null,
        now: FIXED_AT
      }
  }
}

/** 新建 job 时的固定步骤模板（mock 用；真实 daemon 由 JobManager 上报）。 */
export function createQueuedJobRecord({ id, inputs, now }) {
  return {
    id,
    kind: 'single_point_offline',
    status: 'queued',
    verdict: null,
    created_at: new Date(now).toISOString(),
    started_at: null,
    finished_at: null,
    duration_ms: null,
    engine: 'synthetic',
    apply_state: 'not_applied',
    applied_version: null,
    steps: baseSteps(),
    artifacts: [],
    inputs,
    result: null,
    error: null,
    reason: null,
    artifactContent: [],
    /** mock 内部：GET 详情时推进的 tick 计数（真实后端由 worker 推进）。 */
    ticks: 0
  }
}

/** 成功收尾：填 result/artifacts/verdict（mock 用）。 */
export function finalizeJobSuccess(job, now) {
  const rawResult = buildJobResult(job.inputs.centers_hz, {
    tolerance_db: job.inputs.tolerance_db,
    min_bands: job.inputs.min_bands
  })
  const built = buildJobArtifacts(job.id, rawResult)
  job.status = 'succeeded'
  job.verdict = built.resultWithPlan.verdict
  job.finished_at = new Date(now).toISOString()
  job.duration_ms = job.started_at === null ? null : now - Date.parse(job.started_at)
  job.result = built.resultWithPlan
  job.artifacts = built.artifacts.map((artifact) => ({
    name: artifact.name,
    sha256: sha256Hex(artifact.content.toString('utf8')),
    bytes: artifact.content.length,
    content_type: artifact.content_type,
    downloadable: true
  }))
  job.artifactContent = built.artifacts
  job.steps = job.steps.map((step) => ({
    ...step,
    status: 'succeeded',
    started_at: step.started_at ?? new Date(now).toISOString(),
    finished_at: step.finished_at ?? new Date(now).toISOString(),
    sha256: step.sha256 ?? sha256Hex(job.id + ':' + step.name)
  }))
  return job
}

/** 失败收尾：填稳定错误码（mock 用）。 */
export function finalizeJobFailure(job, now, error) {
  job.status = 'failed'
  job.verdict = null
  job.finished_at = new Date(now).toISOString()
  job.duration_ms = job.started_at === null ? null : now - Date.parse(job.started_at)
  job.error = error
  const failingIndex = job.steps.findIndex((step) => step.status === 'running')
  job.steps = job.steps.map((step, index) => {
    if (index === failingIndex) {
      return { ...step, status: 'failed', finished_at: new Date(now).toISOString(), detail: error.code + ': ' + error.message }
    }
    if (failingIndex >= 0 && index > failingIndex) {
      return { ...step, status: 'cancelled' }
    }
    return step
  })
  return job
}

/** mock 状态推进：每次调用推进一步（queued→running→4 步→终态）。返回是否到终态。 */
export function tickJobRecord(job, now) {
  if (job.status === 'succeeded' || job.status === 'failed' || job.status === 'cancelled') {
    return true
  }
  if (job.status === 'queued') {
    job.status = 'running'
    job.started_at = new Date(now).toISOString()
    job.steps = job.steps.map((step, index) =>
      index === 0
        ? { ...step, status: 'running', started_at: new Date(now).toISOString() }
        : step
    )
    return false
  }
  const runningIndex = job.steps.findIndex((step) => step.status === 'running')
  if (runningIndex < 0) {
    finalizeJobSuccess(job, now)
    return true
  }
  job.steps = job.steps.map((step, index) => {
    if (index === runningIndex) {
      return {
        ...step,
        status: 'succeeded',
        finished_at: new Date(now).toISOString(),
        sha256: step.sha256 ?? sha256Hex(job.id + ':' + step.name)
      }
    }
    if (index === runningIndex + 1) {
      return { ...step, status: 'running', started_at: new Date(now).toISOString() }
    }
    return step
  })
  if (runningIndex + 1 >= job.steps.length) {
    finalizeJobSuccess(job, now)
    return true
  }
  return false
}

export const FIXTURE_IDS = {
  SUCCESS_JOB_ID,
  FAILED_JOB_ID,
  BASELINE_SHA,
  APPLIED_SHA,
  MIC_CAL_SHA
}
