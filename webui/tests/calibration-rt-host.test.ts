/**
 * C5：校准向导第 5 步 rt-host 真实测量（single_point_rt_host）前端契约测试。
 *
 * 覆盖（任务 task-81）：
 * - POST /api/calibration/jobs 的 create request body/kind（discriminated union，§7.2 的 audible/channels/level_dbfs）；
 * - audible 未显式 true 时 store 本地拒绝、不发请求（P08）；
 * - 真实 job 轮询状态机（running -> succeeded，终态停轮询并刷新 wizard/state）；
 * - 创建失败保留稳定 code（409 mic_profile_missing -> conflict）；
 * - 取消（POST /jobs/{id}/cancel）与状态回读；
 * - artifacts 带认证下载（getRaw，不是 <a href>）；
 * - job 详情解析：kind/steps 不硬编码离线名、inputs_rt_host 回显、未知步骤名原样保留；
 * - i18n zh-CN/en-US 新键存在（i18n-check 另由门禁覆盖键集对等）。
 *
 * 全部使用 mock/fixture，不调用真实设备、不出声。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, ApiError } from '../src/api/client.ts'
import {
  CALIBRATION_JOB_SCHEMA,
  CALIBRATION_WIZARD_STATE_SCHEMA,
  JOB_KIND_SINGLE_POINT_RT_HOST,
  RT_HOST_JOB_STEP_NAMES,
  WIZARD_STEP_KEYS,
  createCalibrationApi,
  isRtHostJobKind,
  parseJobResponse,
  type CalibrationApi,
  type CalibrationJob,
  type CalibrationJobStatus,
  type CalibrationWizardStateResponse,
  type WizardGateStatus
} from '../src/api/calibration.ts'
import {
  createCalibrationController,
  createInitialCalibrationState,
  type CalibrationController,
  type CalibrationState
} from '../src/stores/calibration.ts'
import { createRecordingFetch, jsonResponse, textResponse } from '../scripts/mock-test-support.mjs'
import zhCN from '../src/i18n/zh-CN.ts'
import enUS from '../src/i18n/en-US.ts'

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0))
}

function createFakeClock() {
  const tasks = new Map<number, () => void>()
  let nextId = 1
  return {
    schedule: (callback: () => void): unknown => {
      const id = nextId
      nextId += 1
      tasks.set(id, callback)
      return id
    },
    cancel: (handle: unknown): void => {
      tasks.delete(handle as number)
    },
    tick: async (): Promise<void> => {
      const callbacks = [...tasks.values()]
      tasks.clear()
      for (const callback of callbacks) {
        callback()
      }
      await flush()
    },
    pending: (): number => tasks.size
  }
}

/* ------------------------------------------------------------------ fixtures */

function rtHostSteps(status: CalibrationJob['status'], activeName: string | null = null) {
  return RT_HOST_JOB_STEP_NAMES.map((name, index) => ({
    name,
    status:
      status === 'succeeded'
        ? 'succeeded'
        : status === 'running' && name === activeName
          ? 'running'
          : index === 0 && (status === 'running' || status === 'failed')
            ? 'succeeded'
            : status === 'failed' && name === 'sweep_capture'
              ? 'failed'
              : 'pending',
    started_at: null,
    finished_at: null,
    sha256: null,
    detail: null
  }))
}

const RT_HOST_ARTIFACTS = ['measurement.json', 'quality.json', 'report.json', 'artifacts.manifest.json'].map(
  (name) => ({
    name,
    sha256: 'a'.repeat(64),
    bytes: 512,
    content_type: 'application/json',
    downloadable: true
  })
)

function rtHostJob(status: CalibrationJobStatus, overrides: Partial<CalibrationJob> = {}): CalibrationJob {
  return {
    id: 'job-rt-1',
    kind: JOB_KIND_SINGLE_POINT_RT_HOST,
    status,
    verdict: null,
    created_at: '2026-11-28T01:00:00.000Z',
    started_at: '2026-11-28T01:00:01.000Z',
    finished_at: status === 'succeeded' || status === 'failed' || status === 'cancelled' ? '2026-11-28T01:00:09.000Z' : null,
    duration_ms: status === 'succeeded' ? 8000 : null,
    error: status === 'failed' ? { code: 'measure_failed', message: 'rt-host measurement failed' } : null,
    reason: null,
    created_unix: null,
    updated_unix: null,
    progress: status === 'running' ? 0.4 : status === 'succeeded' ? 1 : null,
    engine: status === 'succeeded' ? 'in-process' : null,
    step: status === 'running' ? 'sweep_capture' : null,
    apply_state: 'not_applied',
    applied_version: null,
    steps: rtHostSteps(status, status === 'running' ? 'sweep_capture' : null),
    artifacts: status === 'succeeded' ? RT_HOST_ARTIFACTS : [],
    inputs: null,
    inputs_rt_host: { audible: true, channels: [0, 1], level_dbfs: -12 },
    result: null,
    apply: null,
    ...overrides
  }
}

function rtHostGateState(status: WizardGateStatus): CalibrationWizardStateResponse {
  return {
    schema: CALIBRATION_WIZARD_STATE_SCHEMA,
    current_step: 5,
    steps: WIZARD_STEP_KEYS.map((key, index) => ({
      step: index + 1,
      key,
      status: index === 4 ? status : index < 4 ? 'passed' : 'not_available',
      reason: index === 4 ? 'rt-host measurement job state' : index >= 5 ? 'solve/design/apply belong to C3b2' : null,
      task: index >= 5 ? 'C3b2' : null
    })),
    active_job_id: null,
    last_job_id: 'job-rt-1',
    measurement: {
      baseline_upload_id: null,
      applied_upload_id: null,
      baseline_sha256: null,
      applied_sha256: null
    },
    snapshot: null,
    updated_at: null,
    reasons: {}
  }
}

function jobResponse(job: CalibrationJob) {
  return { schema: CALIBRATION_JOB_SCHEMA, job, reasons: {} }
}

const DOWNLOAD = {
  job_id: 'job-rt-1',
  name: 'measurement.json',
  filename: 'measurement.json',
  content_type: 'application/json',
  sha256: 'b'.repeat(64),
  status: 200,
  data: new TextEncoder().encode('{"schema":"khaen-calibration-rt-host-measurement/v1"}').buffer,
  text: '{"schema":"khaen-calibration-rt-host-measurement/v1"}',
  byte_length: 50,
  from_content_disposition: true
}

interface RtHostCalls {
  createJob: Array<{ kind: string; inputs: Record<string, unknown> }>
  getJob: string[]
  getWizardState: number
  cancelJob: string[]
  downloadArtifact: Array<{ jobId: string; name: string }>
}

function createRtHostFakeApi(
  options: {
    jobs?: CalibrationJob[]
    overrides?: Partial<CalibrationApi>
    createJobError?: ApiError
  } = {}
) {
  const calls: RtHostCalls = {
    createJob: [],
    getJob: [],
    getWizardState: 0,
    cancelJob: [],
    downloadArtifact: []
  }
  const jobs = options.jobs ?? [rtHostJob('running'), rtHostJob('succeeded')]
  let jobIndex = 0
  const api: CalibrationApi = {
    async listJobs() {
      return {
        schema: 'khaen-calibration-jobs/v1',
        available: true,
        reason: null,
        engine: null,
        active_job_id: null,
        jobs: [],
        total: 0,
        limit: 20,
        offset: 0,
        reasons: {}
      }
    },
    async getJob(jobId) {
      calls.getJob.push(jobId)
      const job = jobs[Math.min(jobIndex, jobs.length - 1)]
      jobIndex += 1
      return jobResponse(job)
    },
    async createJob(request) {
      calls.createJob.push(request as unknown as { kind: string; inputs: Record<string, unknown> })
      if (options.createJobError !== undefined) {
        throw options.createJobError
      }
      return { job_id: 'job-rt-1', location: '/api/calibration/jobs/job-rt-1', status: 'queued' }
    },
    async cancelJob(jobId) {
      calls.cancelJob.push(jobId)
      return { job_id: jobId, status: 'cancelled' }
    },
    async applyJob() {
      throw new Error('not used in rt-host measurement tests')
    },
    async rollbackJob() {
      throw new Error('not used in rt-host measurement tests')
    },
    async upload() {
      throw new Error('not used in rt-host measurement tests')
    },
    async getWizard() {
      throw new Error('not used in rt-host measurement tests')
    },
    async putWizard() {
      throw new Error('not used in rt-host measurement tests')
    },
    async getWizardState() {
      calls.getWizardState += 1
      return rtHostGateState('passed')
    },
    async applyWizard() {
      throw new Error('not used in rt-host measurement tests')
    },
    async downloadArtifact(jobId, name) {
      calls.downloadArtifact.push({ jobId, name })
      return DOWNLOAD
    },
    artifactUrl(jobId, name) {
      return '/api/calibration/jobs/' + jobId + '/artifacts/' + name
    }
  }
  return { api: Object.assign(api, options.overrides ?? {}), calls }
}

function createHarness(
  options: {
    jobs?: CalibrationJob[]
    overrides?: Partial<CalibrationApi>
    createJobError?: ApiError
  } = {}
) {
  const clock = createFakeClock()
  const { api, calls } = createRtHostFakeApi(options)
  const state: CalibrationState = createInitialCalibrationState()
  const controller: CalibrationController = createCalibrationController(state, {
    api,
    schedule: clock.schedule,
    cancel: clock.cancel,
    intervalMs: 1000
  })
  return { clock, api, calls, state, controller }
}

/* ------------------------------------------------------- create request body */

test('rt-host create sends exactly kind + §7.2 inputs (no offline upload fields)', async () => {
  const recorder = createRecordingFetch(() =>
    jsonResponse(202, {
      ok: true,
      job_id: 'job-rt-1',
      state: 'queued',
      location: '/api/calibration/jobs/job-rt-1'
    })
  )
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-rt' }))

  const created = await api.createJob({
    kind: 'single_point_rt_host',
    inputs: { audible: true }
  })
  assert.equal(created.job_id, 'job-rt-1')
  assert.equal(created.location, '/api/calibration/jobs/job-rt-1')
  assert.equal(created.status, 'queued')

  const minimal = recorder.calls[0]
  assert.equal(minimal.method, 'POST')
  assert.equal(minimal.url, '/api/calibration/jobs')
  assert.equal(minimal.headers['X-KHAEN-CSRF'], 'csrf-rt')
  // 省略的可选项不出现：默认由 rt-host 配置决定。
  assert.deepEqual(JSON.parse(String(minimal.body)), {
    kind: 'single_point_rt_host',
    inputs: { audible: true }
  })

  await api.createJob({
    kind: 'single_point_rt_host',
    inputs: { audible: true, channels: [0, 1], level_dbfs: -12 }
  })
  assert.deepEqual(JSON.parse(String(recorder.calls[1].body)), {
    kind: 'single_point_rt_host',
    inputs: { audible: true, channels: [0, 1], level_dbfs: -12 }
  })
})

test('rt-host create failure keeps the daemon stable code and HTTP status', async () => {
  for (const [status, code] of [
    [409, 'mic_profile_missing'],
    [409, 'switch_busy'],
    [503, 'not_wired'],
    [400, 'invalid_request']
  ] as Array<[number, string]>) {
    const recorder = createRecordingFetch(() =>
      jsonResponse(status, { error: { code, message: 'daemon refused the job: ' + code } })
    )
    const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch }))
    try {
      await api.createJob({ kind: 'single_point_rt_host', inputs: { audible: true } })
      assert.fail('expected the request to reject')
    } catch (error) {
      assert.ok(error instanceof ApiError)
      assert.equal(error.status, status)
      assert.equal(error.code, code)
    }
  }
})

/* ---------------------------------------------------------- store / polling */

test('store.createRtHostMeasurementJob sends single_point_rt_host and polls the real job to terminal', async () => {
  const h = createHarness()
  const result = await h.controller.createRtHostMeasurementJob({
    audible: true,
    channels: [0, 1],
    level_dbfs: -12
  })
  assert.equal(result.ok, true)
  if (result.ok === true) {
    assert.equal(result.value.job_id, 'job-rt-1')
    assert.equal(result.value.location, '/api/calibration/jobs/job-rt-1')
  }
  assert.deepEqual(h.calls.createJob, [
    { kind: 'single_point_rt_host', inputs: { audible: true, channels: [0, 1], level_dbfs: -12 } }
  ])

  await flush()
  assert.equal(h.state.job?.kind, JOB_KIND_SINGLE_POINT_RT_HOST)
  assert.equal(h.state.job?.status, 'running')
  assert.equal(h.state.job?.progress, 0.4)
  assert.equal(h.state.polling, true)
  assert.equal(h.state.busy.createJob, false)

  await h.clock.tick()
  assert.equal(h.state.job?.status, 'succeeded')
  assert.equal(h.state.polling, false)
  assert.equal(h.clock.pending(), 0)
  // 4 个真实步骤（name 原样来自后端）与 4 个可下载产物。
  assert.deepEqual(
    h.state.job?.steps.map((step) => step.name),
    [...RT_HOST_JOB_STEP_NAMES]
  )
  assert.deepEqual(
    h.state.job?.artifacts.map((artifact) => artifact.name),
    ['measurement.json', 'quality.json', 'report.json', 'artifacts.manifest.json']
  )
  // 终态后刷新 gate（第 5 步 passed / 6-8 not_available 由后端决定）。
  assert.equal(h.calls.getWizardState > 0, true)
})

test('audible authorization is refused locally: no job request without audible:true (P08)', async () => {
  const h = createHarness()
  const result = await h.controller.createRtHostMeasurementJob({
    audible: false as unknown as true
  })
  assert.equal(result.ok, false)
  if (result.ok === false) {
    assert.equal(result.error.code, 'audible_required')
    assert.equal(result.error.category, 'invalid_request')
    assert.equal(result.error.retryable, false)
  }
  assert.equal(h.calls.createJob.length, 0)
  assert.equal(h.state.polling, false)
  assert.equal(h.state.actionError?.code, 'audible_required')
})

test('rt-host create failure (409 mic_profile_missing) surfaces the stable code and does not poll', async () => {
  const h = createHarness({
    createJobError: new ApiError({
      status: 409,
      code: 'mic_profile_missing',
      message: 'no microphone profile saved yet'
    })
  })
  const result = await h.controller.createRtHostMeasurementJob({ audible: true })
  assert.equal(result.ok, false)
  if (result.ok === false) {
    assert.equal(result.error.code, 'mic_profile_missing')
    assert.equal(result.error.category, 'conflict')
    assert.equal(result.error.detail, 'no microphone profile saved yet')
  }
  assert.equal(h.calls.createJob.length, 1)
  assert.equal(h.calls.getJob.length, 0)
  assert.equal(h.state.polling, false)
  assert.equal(h.state.jobPhase, 'idle')
  assert.equal(h.state.actionError?.code, 'mic_profile_missing')
})

test('cancel posts to /jobs/{id}/cancel, reloads the terminal job and stops polling', async () => {
  const h = createHarness({
    jobs: [rtHostJob('running'), rtHostJob('cancelled')]
  })
  await h.controller.createRtHostMeasurementJob({ audible: true })
  await flush()
  assert.equal(h.state.polling, true)
  assert.equal(h.state.job?.status, 'running')

  const result = await h.controller.cancelJob('job-rt-1')
  assert.equal(result.ok, true)
  if (result.ok === true) {
    assert.equal(result.value, 'cancelled')
  }
  assert.deepEqual(h.calls.cancelJob, ['job-rt-1'])
  assert.equal(h.state.job?.status, 'cancelled')
  assert.equal(h.state.polling, false)
  assert.equal(h.clock.pending(), 0)
})

/* --------------------------------------------------------------- artifacts */

test('rt-host artifacts download through the authenticated client (not <a href>)', async () => {
  const body = '{"schema":"khaen-calibration-rt-host-measurement/v1"}'
  const recorder = createRecordingFetch(() =>
    textResponse(200, body, {
      'Content-Type': 'application/json',
      'Content-Disposition': 'attachment; filename="measurement.json"',
      'x-khaen-artifact-sha256': 'c'.repeat(64)
    })
  )
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const download = await api.downloadArtifact('job-rt-1', 'measurement.json')
  assert.equal(download.filename, 'measurement.json')
  assert.equal(download.sha256, 'c'.repeat(64))
  assert.equal(download.byte_length, body.length)
  assert.equal(download.text, body)
  assert.equal(new TextDecoder().decode(download.data), body)
  const call = recorder.calls[0]
  assert.equal(call.method, 'GET')
  assert.equal(call.url, '/api/calibration/jobs/job-rt-1/artifacts/measurement.json')
  assert.equal(call.credentials, 'include')

  // store 只把带认证下载的结果放进 state，页面负责 Blob 保存。
  const h = createHarness()
  const result = await h.controller.downloadArtifact('job-rt-1', 'quality.json')
  assert.equal(result.ok, true)
  assert.deepEqual(h.calls.downloadArtifact, [{ jobId: 'job-rt-1', name: 'quality.json' }])
  assert.equal(h.state.lastDownload?.name, 'measurement.json')
  assert.equal(h.state.busy.download, false)
})

/* ------------------------------------------------------------------ parsing */

test('job detail parsing keeps rt-host kind/steps/inputs_rt_host and unknown step names verbatim', () => {
  const detail: any = {
    ok: true,
    schema: CALIBRATION_JOB_SCHEMA,
    job: {
      id: 'job-rt-1',
      kind: 'single_point_rt_host',
      status: 'succeeded',
      verdict: null,
      created_at: null,
      started_at: null,
      finished_at: null,
      duration_ms: null,
      error: null,
      reason: null,
      created_unix: null,
      updated_unix: null,
      progress: 1,
      engine: 'in-process',
      step: null,
      apply_state: 'not_applied',
      applied_version: null,
      apply: { state: 'not_applied', applied_version: null, previous_version: null, applied_at: null, plan_sha256: null },
      steps: RT_HOST_JOB_STEP_NAMES.map((name, index) => ({
        name,
        status: 'succeeded',
        started_at: null,
        finished_at: null,
        sha256: index === 1 ? 'd'.repeat(64) : null,
        detail: null
      })),
      artifacts: RT_HOST_ARTIFACTS,
      inputs: null,
      result: null
    },
    inputs_rt_host: { audible: true, channels: [0, 1], level_dbfs: -12 },
    result: null,
    artifacts: RT_HOST_ARTIFACTS,
    error: null,
    reasons: { verdict: 'measurement-only slice: no PASS (C3b2)', inputs: 'session inputs are echoed as inputs_rt_host' }
  }

  const parsed = parseJobResponse(detail)
  assert.equal(parsed.job.kind, JOB_KIND_SINGLE_POINT_RT_HOST)
  assert.equal(isRtHostJobKind(parsed.job.kind), true)
  assert.equal(parsed.job.verdict, null)
  assert.equal(parsed.job.inputs, null)
  assert.deepEqual(parsed.job.inputs_rt_host, { audible: true, channels: [0, 1], level_dbfs: -12 })
  // steps 不按离线固定名解析：rt-host 的 6 个 name 原样保留（含 solve_g/compile）。
  assert.deepEqual(
    parsed.job.steps.map((step) => step.name),
    [...RT_HOST_JOB_STEP_NAMES]
  )
  assert.equal(parsed.job.steps[1].sha256, 'd'.repeat(64))
  assert.deepEqual(
    parsed.job.artifacts.map((artifact) => artifact.name),
    ['measurement.json', 'quality.json', 'report.json', 'artifacts.manifest.json']
  )

  // 未来新增的步骤名也必须如实呈现（不映射成 compile/levels 占位）。
  const future: any = structuredClone(detail)
  future.job.steps.push({ name: 'solve', status: 'pending', started_at: null, finished_at: null, sha256: null, detail: null })
  assert.equal(parseJobResponse(future).job.steps[RT_HOST_JOB_STEP_NAMES.length].name, 'solve')

  // inputs_rt_host 缺失 = null，不补默认值。
  const noEcho: any = structuredClone(detail)
  delete noEcho.inputs_rt_host
  assert.equal(parseJobResponse(noEcho).job.inputs_rt_host, null)

  // 类型漂移仍按 invalid_response 拒绝（不静默补值）。
  const malformed: any = structuredClone(detail)
  malformed.inputs_rt_host.channels = '0,1'
  assert.throws(
    () => parseJobResponse(malformed),
    (error: unknown) => error instanceof ApiError && error.code === 'invalid_response'
  )
})

/* --------------------------------------------------------------------- i18n */

function messageFor(messages: unknown, key: string): string | null {
  let current: unknown = messages
  for (const segment of key.split('.')) {
    if (typeof current !== 'object' || current === null) {
      return null
    }
    current = (current as Record<string, unknown>)[segment]
  }
  return typeof current === 'string' && current.length > 0 ? current : null
}

test('step-5 real measurement i18n keys exist in both locales (and rt-host step labels)', () => {
  const keys = [
    'calibration.wizard.measure.availability.micProfile',
    'calibration.wizard.measure.availability.profileMissing',
    'calibration.wizard.measure.availability.rtHost',
    'calibration.wizard.measure.availability.available',
    'calibration.wizard.measure.availability.unavailable',
    'calibration.wizard.measure.realtime.title',
    'calibration.wizard.measure.realtime.desc',
    'calibration.wizard.measure.realtime.audibleConfirmTitle',
    'calibration.wizard.measure.realtime.audibleConfirmMessage',
    'calibration.wizard.measure.realtime.audibleConfirmOk',
    'calibration.wizard.measure.realtime.start',
    'calibration.wizard.measure.realtime.advanced',
    'calibration.wizard.measure.realtime.channelsInvalid',
    'calibration.wizard.measure.realtime.levelInvalid',
    'calibration.wizard.measure.realtime.jobId',
    'calibration.wizard.measure.realtime.progress',
    'calibration.wizard.measure.realtime.artifacts',
    'calibration.wizard.measure.realtime.download',
    'calibration.wizard.measure.realtime.c3b2Note',
    'calibration.wizard.measure.offline.badge',
    'calibration.wizard.measure.offline.desc',
    'calibration.wizard.measure.errors.micProfileMissing',
    'calibration.wizard.measure.errors.busy',
    'calibration.wizard.measure.errors.notWired',
    'calibration.jobKind.single_point_offline',
    'calibration.jobKind.single_point_rt_host',
    'calibration.wizard.toast.measurementStarted'
  ]
  for (const key of keys) {
    assert.equal(typeof messageFor(zhCN, key), 'string', 'zh-CN missing ' + key)
    assert.equal(typeof messageFor(enUS, key), 'string', 'en-US missing ' + key)
  }
  for (const name of RT_HOST_JOB_STEP_NAMES) {
    assert.equal(typeof messageFor(zhCN, 'calibration.step.jobNames.' + name), 'string', 'zh-CN missing step label ' + name)
    assert.equal(typeof messageFor(enUS, 'calibration.step.jobNames.' + name), 'string', 'en-US missing step label ' + name)
  }
})
