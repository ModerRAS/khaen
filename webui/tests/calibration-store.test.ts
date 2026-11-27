import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiError } from '../src/api/client.ts'
import {
  CALIBRATION_JOB_SCHEMA,
  CALIBRATION_JOBS_SCHEMA,
  CALIBRATION_WIZARD_SCHEMA,
  CALIBRATION_WIZARD_STATE_SCHEMA,
  DEFAULT_CENTERS_HZ,
  WIZARD_STEP_KEYS,
  createCalibrationApi,
  type CalibrationApi,
  type CalibrationApplyOutcome,
  type CalibrationArtifactDownload,
  type CalibrationJob,
  type CalibrationJobResponse,
  type CalibrationJobsResponse,
  type CalibrationRollbackOutcome,
  type CalibrationStagedRequest,
  type CalibrationUploadResult,
  type CalibrationWizardApplyOutcome,
  type CalibrationWizardResponse,
  type CalibrationWizardStateResponse,
  type WizardConfig,
  type WizardSaveRequest
} from '../src/api/calibration.ts'
import {
  createCalibrationController,
  createInitialCalibrationState,
  type CalibrationController,
  type CalibrationState
} from '../src/stores/calibration.ts'
import { ApiClient } from '../src/api/client.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

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
    /** 触发所有待执行定时器（模拟一轮轮询）。 */
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

function wizardResponse(saved = true): CalibrationWizardResponse {
  const config: WizardConfig = {
    mic_cal_sha256: 'd'.repeat(64),
    mic_cal_filename: 'umik.txt',
    orientation: '90deg',
    target_mode: 'flat',
    single_point: { centers_hz: [...DEFAULT_CENTERS_HZ], tolerance_db: 2, min_bands: 3 }
  }
  return {
    schema: CALIBRATION_WIZARD_SCHEMA,
    config,
    defaults: {
      mic_cal_sha256: null,
      mic_cal_filename: null,
      orientation: null,
      target_mode: 'flat',
      single_point: { centers_hz: [...DEFAULT_CENTERS_HZ], tolerance_db: 2, min_bands: 3 }
    },
    saved,
    updated_at: '2026-11-26T06:00:00.000Z',
    reasons: {},
    set_fields: []
  }
}

function wizardState(status: 'passed' | 'blocked' | 'not_available' = 'passed'): CalibrationWizardStateResponse {
  return {
    schema: CALIBRATION_WIZARD_STATE_SCHEMA,
    current_step: 3,
    steps: WIZARD_STEP_KEYS.map((key, index) => ({
      step: index + 1,
      key,
      status: index === 1 ? 'not_available' : status,
      reason: status === 'blocked' ? 'blocked by test' : null,
      task: index === 1 ? 'M4' : null
    })),
    active_job_id: null,
    last_job_id: null,
    measurement: {
      baseline_upload_id: null,
      applied_upload_id: null,
      baseline_sha256: null,
      applied_sha256: null
    },
    snapshot: { version: 3, graph_id: 'graph-old', sha256: 'a'.repeat(64) },
    updated_at: '2026-11-26T06:00:00.000Z',
    reasons: {}
  }
}

function jobsResponse(available = true): CalibrationJobsResponse {
  return {
    schema: CALIBRATION_JOBS_SCHEMA,
    available,
    reason: available ? null : 'calibration job backend not wired',
    engine: null,
    active_job_id: null,
    jobs: [],
    total: 0,
    limit: 20,
    offset: 0,
    reasons: available ? {} : { jobs: 'calibration job backend not wired' }
  }
}

function jobDetail(status: CalibrationJob['status'] = 'running'): CalibrationJobResponse {
  const job: CalibrationJob = {
    id: 'job-new',
    kind: 'single_point_offline',
    status,
    verdict: status === 'succeeded' ? 'PASS' : null,
    created_at: '2026-11-26T06:00:00.000Z',
    started_at: '2026-11-26T06:00:01.000Z',
    finished_at: status === 'succeeded' ? '2026-11-26T06:00:09.000Z' : null,
    duration_ms: status === 'succeeded' ? 8000 : null,
    error: null,
    reason: null,
    created_unix: null,
    updated_unix: null,
    progress: null,
    engine: 'synthetic',
    step: null,
    apply_state: 'not_applied',
    applied_version: null,
    steps: [
      {
        name: 'compile',
        status: status === 'succeeded' ? 'succeeded' : 'running',
        started_at: '2026-11-26T06:00:01.000Z',
        finished_at: null,
        sha256: null,
        detail: null
      }
    ],
    artifacts: [],
    inputs: null,
    inputs_rt_host: null,
    result: null,
    apply: null
  }
  return { schema: CALIBRATION_JOB_SCHEMA, job, reasons: {} }
}

const APPLY_RESULT: CalibrationApplyOutcome = {
  kind: 'applied',
  stage: 'single',
  applied: true,
  job_id: 'job-new',
  applied_version: 4,
  previous_version: 3,
  graph_id: 'graph-new',
  sha256: 'b'.repeat(64),
  snapshot: { version: 3, graph_id: 'graph-old', sha256: 'a'.repeat(64) },
  ack: { synthetic: true, actual: false },
  readback: { actual_version: 4 },
  applied_at: '2026-11-26T06:00:10.000Z',
  job: null,
  reasons: {}
}

const APPLY_PREPARE: CalibrationApplyOutcome = {
  kind: 'prepare',
  stage: 'prepare',
  job_id: 'job-new',
  engine_token: 'token-apply',
  plan_sha256: 'b'.repeat(64),
  previous_version: 3,
  target_version: null,
  job: null,
  reasons: {},
  raw: {}
}

const ROLLBACK_RESULT: CalibrationRollbackOutcome = {
  kind: 'rolled_back',
  stage: 'single',
  rolled_back: true,
  job_id: 'job-new',
  restored_version: 3,
  applied_version: 3,
  graph_id: 'graph-old',
  sha256: 'a'.repeat(64),
  readback: null,
  rolled_back_at: '2026-11-26T06:00:11.000Z',
  job: null,
  reasons: {}
}

const ROLLBACK_PREPARE: CalibrationRollbackOutcome = {
  kind: 'prepare',
  stage: 'prepare',
  job_id: 'job-new',
  engine_token: 'token-rollback',
  plan_sha256: null,
  previous_version: 4,
  target_version: 3,
  job: null,
  reasons: {},
  raw: {}
}

const DOWNLOAD_BYTES = new TextEncoder().encode('{"ok":true}').buffer
const DOWNLOAD: CalibrationArtifactDownload = {
  job_id: 'job-new',
  name: 'report.json',
  filename: 'report.json',
  content_type: 'application/json',
  sha256: 'c'.repeat(64),
  status: 200,
  data: DOWNLOAD_BYTES,
  text: '{"ok":true}',
  byte_length: 11,
  from_content_disposition: true
}

interface FakeApiCalls {
  getWizard: number
  getWizardState: number
  listJobs: number
  putWizard: WizardSaveRequest[]
  upload: Array<{ body: ArrayBuffer | Blob; options: Record<string, unknown> | undefined }>
  createJob: Array<{ kind: string; inputs: Record<string, unknown> }>
  getJob: string[]
  cancelJob: string[]
  applyJob: Array<{ jobId: string; request: CalibrationStagedRequest | null }>
  rollbackJob: Array<{ jobId: string; request: CalibrationStagedRequest | null }>
  applyWizard: Array<string | null>
  downloadArtifact: Array<{ jobId: string; name: string }>
}

function createFakeApi(overrides: Partial<CalibrationApi> = {}) {
  const calls: FakeApiCalls = {
    getWizard: 0,
    getWizardState: 0,
    listJobs: 0,
    putWizard: [],
    upload: [],
    createJob: [],
    getJob: [],
    cancelJob: [],
    applyJob: [],
    rollbackJob: [],
    applyWizard: [],
    downloadArtifact: []
  }
  const api: CalibrationApi = {
    async listJobs() {
      calls.listJobs += 1
      return jobsResponse()
    },
    async getJob(jobId) {
      calls.getJob.push(jobId)
      return jobDetail('running')
    },
    async createJob(request) {
      calls.createJob.push(request as unknown as { kind: string; inputs: Record<string, unknown> })
      return { job_id: 'job-new', location: '/api/calibration/jobs/job-new', status: 'queued' }
    },
    async cancelJob(jobId) {
      calls.cancelJob.push(jobId)
      return { job_id: jobId, status: 'cancelled' }
    },
    async applyJob(jobId, request = null) {
      calls.applyJob.push({ jobId, request })
      if (request?.stage === 'prepare') {
        return APPLY_PREPARE
      }
      if (request?.stage === 'abort') {
        return {
          kind: 'abort',
          stage: 'abort',
          aborted: true,
          job_id: jobId,
          plan_sha256: null,
          target_version: null,
          control: null,
          reasons: {},
          raw: {}
        }
      }
      return APPLY_RESULT
    },
    async rollbackJob(jobId, request = null) {
      calls.rollbackJob.push({ jobId, request })
      if (request?.stage === 'prepare') {
        return ROLLBACK_PREPARE
      }
      if (request?.stage === 'abort') {
        return {
          kind: 'abort',
          stage: 'abort',
          aborted: true,
          job_id: jobId,
          plan_sha256: null,
          target_version: 3,
          control: null,
          reasons: {},
          raw: {}
        }
      }
      return ROLLBACK_RESULT
    },
    async upload(body, options) {
      calls.upload.push({ body, options: options as unknown as Record<string, unknown> | undefined })
      const result: CalibrationUploadResult = {
        upload_id: 'up-' + String(calls.upload.length),
        sha256: 'c'.repeat(64),
        bytes: 44,
        schema: null,
        created: true
      }
      return result
    },
    async getWizard() {
      calls.getWizard += 1
      return wizardResponse()
    },
    async putWizard(request) {
      calls.putWizard.push(request)
      return wizardResponse()
    },
    async getWizardState() {
      calls.getWizardState += 1
      return wizardState()
    },
    async applyWizard(jobId = null) {
      calls.applyWizard.push(jobId)
      const outcome: CalibrationWizardApplyOutcome = APPLY_RESULT
      return outcome
    },
    async downloadArtifact(jobId, name) {
      calls.downloadArtifact.push({ jobId, name })
      return DOWNLOAD
    },
    artifactUrl(jobId, name) {
      return '/api/calibration/jobs/' + jobId + '/artifacts/' + name
    }
  }
  return { api: Object.assign(api, overrides), calls }
}

function createHarness(overrides: Partial<CalibrationApi> = {}) {
  const clock = createFakeClock()
  const { api, calls } = createFakeApi(overrides)
  const state: CalibrationState = createInitialCalibrationState()
  const controller: CalibrationController = createCalibrationController(state, {
    api,
    schedule: clock.schedule,
    cancel: clock.cancel,
    intervalMs: 1000
  })
  return { clock, api, calls, state, controller }
}

/* --------------------------------------------------------------- wizard 加载 */

test('loadWizard fills config + gate state and clears errors', async () => {
  const h = createHarness()
  await h.controller.loadWizard()
  assert.equal(h.state.wizardPhase, 'ready')
  assert.equal(h.state.wizardError, null)
  assert.equal(h.state.wizard?.saved, true)
  assert.equal(h.state.wizardState?.steps.length, 8)
  assert.equal(h.state.busy.wizard, false)
  assert.equal(h.calls.getWizard, 1)
  assert.equal(h.calls.getWizardState, 1)
})

test('loadWizard maps 503 not_wired to unavailable, not to a fake ready state', async () => {
  const h = createHarness({
    async getWizard() {
      throw new ApiError({ status: 503, code: 'not_wired', message: 'backend not wired' })
    }
  })
  await h.controller.loadWizard()
  assert.equal(h.state.wizardPhase, 'unavailable')
  assert.equal(h.state.wizardError?.category, 'not_wired')
  assert.equal(h.state.wizard, null)

  // 后续 state 成功也不能把"配置缺失"洗成 ready。
  await h.controller.loadWizardState()
  assert.equal(h.state.wizardPhase, 'unavailable')
})

test('loadWizard maps malformed responses to error phase', async () => {
  // 响应校验在 api 层完成（tests/calibration-api.test.ts 覆盖）；store 只消费已解析对象，
  // 这里模拟 api 层把畸形响应归类为 invalid_response。
  const h = createHarness({
    async getWizard() {
      throw new ApiError({ status: 200, code: 'invalid_response', message: 'bad schema' })
    }
  })
  await h.controller.loadWizard()
  assert.equal(h.state.wizardPhase, 'error')
  assert.equal(h.state.wizardError?.category, 'invalid_response')
})

test('loadJobs distinguishes available=false from a hard failure', async () => {
  const h = createHarness({ async listJobs() { return jobsResponse(false) } })
  await h.controller.loadJobs()
  assert.equal(h.state.jobsPhase, 'unavailable')
  assert.equal(h.state.jobs?.available, false)
  assert.equal(h.state.jobs?.reason, 'calibration job backend not wired')
  assert.equal(h.state.jobsError, null)

  const failed = createHarness({
    async listJobs() {
      throw new ApiError({ status: 500, code: 'internal_error', message: 'boom' })
    }
  })
  await failed.controller.loadJobs()
  assert.equal(failed.state.jobsPhase, 'error')
  assert.equal(failed.state.jobsError?.category, 'server')
})

/* ------------------------------------------------------------- 保存 / 上传 */

test('saveWizard PUTs config + measurement draft and refreshes gates', async () => {
  const h = createHarness()
  await h.controller.loadWizard()
  const config = h.state.wizard?.config as WizardConfig
  const result = await h.controller.saveWizard(config, {
    baseline_upload_id: 'u1',
    applied_upload_id: 'u2',
    plan_sha: 'a'.repeat(64)
  })
  assert.equal(result.ok, true)
  assert.equal(h.calls.putWizard.length, 1)
  assert.equal(h.calls.putWizard[0].measurement?.baseline_upload_id, 'u1')
  assert.equal(h.calls.putWizard[0].target_mode, 'flat')
  assert.equal(h.calls.getWizardState, 2)
  assert.equal(h.state.busy.saveWizard, false)
})

test('saveWizard failure records actionError and keeps previous config', async () => {
  const h = createHarness({
    async putWizard() {
      throw new ApiError({ status: 400, code: 'invalid_request', message: 'bad field' })
    }
  })
  await h.controller.loadWizard()
  const before = h.state.wizard
  const result = await h.controller.saveWizard(h.state.wizard?.config as WizardConfig)
  assert.equal(result.ok, false)
  if (result.ok === false) {
    assert.equal(result.error.category, 'invalid_request')
  }
  assert.equal(h.state.actionError?.code, 'invalid_request')
  assert.equal(h.state.wizard, before)
})

test('uploadMeasurement stores server sha and persists the measurement draft', async () => {
  const h = createHarness()
  await h.controller.loadWizard()
  const bytes = new Uint8Array([1, 2, 3, 4]).buffer
  const result = await h.controller.uploadMeasurement({
    bytes,
    filename: 'baseline.wav',
    contentType: 'audio/wav',
    role: 'baseline'
  })
  assert.equal(result.ok, true)
  assert.equal(h.calls.upload.length, 1)
  assert.equal(h.calls.upload[0].options?.role, 'baseline')
  assert.equal(h.calls.upload[0].options?.contentType, 'audio/wav')
  assert.equal(h.state.uploads.baseline?.upload_id, 'up-1')
  assert.equal(h.state.measurementDraft.baseline_upload_id, 'up-1')
  // 配置已保存 -> 草稿随 PUT 持久化（刷新后第 5 步可恢复）。
  assert.equal(h.calls.putWizard.length, 1)
  assert.equal(h.calls.putWizard[0].measurement?.baseline_upload_id, 'up-1')
})

test('uploadMeasurement failure records actionError and keeps prior upload', async () => {
  const h = createHarness({
    async upload() {
      throw new ApiError({ status: 413, code: 'upload_too_large', message: 'too big' })
    }
  })
  const result = await h.controller.uploadMeasurement({
    bytes: new ArrayBuffer(2),
    filename: 'x.wav',
    contentType: 'audio/wav',
    role: 'applied'
  })
  assert.equal(result.ok, false)
  if (result.ok === false) {
    assert.equal(result.error.category, 'upload_too_large')
  }
  assert.equal(h.state.uploads.applied, null)
  assert.equal(h.state.actionError?.category, 'upload_too_large')
})

test('uploadMicCal uses the mic_cal role and does not touch measurement draft', async () => {
  const h = createHarness()
  const result = await h.controller.uploadMicCal({
    bytes: new Uint8Array([9]).buffer,
    filename: 'umik.txt',
    contentType: 'application/octet-stream'
  })
  assert.equal(result.ok, true)
  assert.equal(h.calls.upload[0].options?.role, 'mic_cal')
  assert.equal(h.state.uploads.micCal?.upload_id, 'up-1')
  assert.equal(h.state.measurementDraft.baseline_upload_id, null)
})

/* --------------------------------------------------------------- job 轮询 */

test('createJob starts real status polling and stops on a terminal job', async () => {
  let calls = 0
  const h = createHarness({
    async getJob(jobId) {
      calls += 1
      return calls >= 2 ? jobDetail('succeeded') : jobDetail('running')
    }
  })
  const result = await h.controller.createJob({
    plan_sha: 'a'.repeat(64),
    baseline_upload_id: 'u1',
    applied_upload_id: 'u2',
    centers_hz: [100, 1000],
    tolerance_db: 2,
    min_bands: 3
  })
  assert.equal(result.ok, true)
  if (result.ok === true) {
    assert.equal(result.value.job_id, 'job-new')
    assert.equal(result.value.location, '/api/calibration/jobs/job-new')
  }
  assert.equal(h.calls.createJob.length, 1)
  assert.equal(h.calls.createJob[0].kind, 'single_point_offline')
  await flush()
  assert.equal(h.state.job?.status, 'running')
  assert.equal(h.state.polling, true)

  // 第一轮轮询 -> succeeded -> 停止轮询并刷新 gate。
  await h.clock.tick()
  assert.equal(h.state.job?.status, 'succeeded')
  assert.equal(h.state.polling, false)
  assert.equal(h.clock.pending(), 0)
  assert.equal(h.state.busy.createJob, false)
  assert.equal(h.calls.getWizardState > 0, true)
})

test('createJob failure keeps jobPhase idle and records the stable error', async () => {
  const h = createHarness({
    async createJob() {
      throw new ApiError({ status: 409, code: 'switch_busy', message: 'busy' })
    }
  })
  const result = await h.controller.createJob({
    plan_sha: 'a'.repeat(64),
    baseline_upload_id: 'u1',
    applied_upload_id: 'u2',
    centers_hz: [100],
    tolerance_db: 2,
    min_bands: 3
  })
  assert.equal(result.ok, false)
  assert.equal(h.state.jobPhase, 'idle')
  assert.equal(h.state.actionError?.category, 'busy')
  assert.equal(h.state.polling, false)
})

test('polling continues on network errors and stops on 404', async () => {
  let attempt = 0
  const h = createHarness({
    async getJob() {
      attempt += 1
      if (attempt === 1) {
        throw new ApiError({ status: 0, code: 'network_error', message: 'down' })
      }
      throw new ApiError({ status: 404, code: 'job_not_found', message: 'gone' })
    }
  })
  h.controller.startJobPolling('job-new')
  await flush()
  assert.equal(h.state.jobError?.category, 'network')
  assert.equal(h.state.polling, true)
  assert.equal(h.clock.pending(), 1)

  await h.clock.tick()
  assert.equal(h.state.polling, false)
  assert.equal(h.clock.pending(), 0)
  assert.equal(h.state.jobError?.category, 'not_found')
})

test('pause/resume polling stops and restarts the timer without losing the job id', async () => {
  let calls = 0
  const h = createHarness({
    async getJob() {
      calls += 1
      return jobDetail('running')
    }
  })
  h.controller.startJobPolling('job-new')
  await flush()
  assert.equal(calls, 1)
  assert.equal(h.clock.pending(), 1)

  h.controller.pauseJobPolling()
  assert.equal(h.clock.pending(), 0)

  h.controller.resumeJobPolling()
  await flush()
  assert.equal(calls, 2)
  assert.equal(h.clock.pending(), 1)
  h.controller.stopJobPolling()
})

test('cancelJob stops polling and reloads the terminal job', async () => {
  const h = createHarness({
    async getJob() {
      return jobDetail('cancelled')
    }
  })
  h.controller.startJobPolling('job-new')
  await flush()
  h.controller.startJobPolling('job-new')
  const result = await h.controller.cancelJob('job-new')
  assert.equal(result.ok, true)
  assert.deepEqual(h.calls.cancelJob, ['job-new'])
  assert.equal(h.state.job?.status, 'cancelled')
  assert.equal(h.state.polling, false)
})

/* --------------------------------------------------------- apply / rollback */

test('applyJob (single-shot) records the readback and refreshes gates', async () => {
  const h = createHarness()
  const result = await h.controller.applyJob('job-new')
  assert.equal(result.ok, true)
  assert.equal(h.calls.applyJob.length, 1)
  assert.deepEqual(h.calls.applyJob[0], { jobId: 'job-new', request: null })
  assert.equal(h.state.lastApply?.kind, 'applied')
  if (h.state.lastApply?.kind === 'applied') {
    assert.equal(h.state.lastApply.applied_version, 4)
    assert.equal(h.state.lastApply.ack?.synthetic, true)
  }
  assert.equal(h.state.busy.apply, false)
  assert.equal(h.calls.getWizardState, 1)
  assert.equal(h.state.pendingApply, null)
})

test('staged apply: prepare stores token, commit/abort consume it exactly once', async () => {
  const h = createHarness()
  const prepared = await h.controller.applyJob('job-new', { stage: 'prepare' })
  assert.equal(prepared.ok, true)
  assert.equal(h.state.pendingApply?.engineToken, 'token-apply')
  assert.equal(h.state.pendingApply?.jobId, 'job-new')
  assert.equal(h.state.lastApply?.kind, 'prepare')
  assert.equal(h.calls.applyJob[0].request?.stage, 'prepare')

  const committed = await h.controller.commitApply('job-new')
  assert.equal(committed.ok, true)
  assert.equal(h.calls.applyJob[1].request?.stage, 'commit')
  assert.equal(h.calls.applyJob[1].request?.engine_token, 'token-apply')
  assert.equal(h.state.pendingApply, null)

  // 再 commit：本地拒绝，不发请求（避免把"未生效"当成功）。
  const again = await h.controller.commitApply('job-new')
  assert.equal(again.ok, false)
  assert.equal(h.calls.applyJob.length, 2)
  assert.equal(h.state.actionError !== null, true)
})

test('staged apply: abort clears the pending draft and does not claim applied', async () => {
  const h = createHarness()
  await h.controller.applyJob('job-new', { stage: 'prepare' })
  const aborted = await h.controller.abortApply('job-new')
  assert.equal(aborted.ok, true)
  assert.equal(h.calls.applyJob[1].request?.stage, 'abort')
  assert.equal(h.state.pendingApply, null)
  assert.equal(h.state.lastApply?.kind, 'abort')
})

test('staged rollback: prepare + commit/abort path and local no-pending guard', async () => {
  const h = createHarness()
  const noPending = await h.controller.commitRollback('job-new')
  assert.equal(noPending.ok, false)
  assert.equal(h.calls.rollbackJob.length, 0)

  await h.controller.rollbackJob('job-new', { stage: 'prepare' })
  assert.equal(h.state.pendingRollback?.targetVersion, 3)
  const committed = await h.controller.commitRollback('job-new')
  assert.equal(committed.ok, true)
  assert.equal(h.calls.rollbackJob[1].request?.stage, 'commit')
  assert.equal(h.calls.rollbackJob[1].request?.engine_token, 'token-rollback')
  assert.equal(h.state.pendingRollback, null)

  await h.controller.rollbackJob('job-new', { stage: 'prepare' })
  await h.controller.abortRollback('job-new')
  assert.equal(h.calls.rollbackJob[3].request?.stage, 'abort')
})

test('applyWizard sends no job_id by default and records the outcome', async () => {
  const h = createHarness()
  const applied = await h.controller.applyWizard()
  assert.equal(applied.ok, true)
  assert.deepEqual(h.calls.applyWizard, [null])
  assert.equal(h.state.lastApply?.kind, 'applied')

  const withJob = await h.controller.applyWizard('job-new')
  assert.equal(withJob.ok, true)
  assert.deepEqual(h.calls.applyWizard, [null, 'job-new'])
})

test('rollbackJob records its receipt and failures', async () => {
  const h = createHarness()
  const rolled = await h.controller.rollbackJob('job-new')
  assert.equal(rolled.ok, true)
  assert.equal(h.calls.rollbackJob.length, 1)
  assert.equal(h.state.lastRollback?.kind, 'rolled_back')
  if (h.state.lastRollback?.kind === 'rolled_back') {
    assert.equal(h.state.lastRollback.restored_version, 3)
  }

  const failing = createHarness({
    async applyJob() {
      throw new ApiError({ status: 409, code: 'job_not_succeeded', message: 'not yet' })
    }
  })
  const failure = await failing.controller.applyJob('job-new')
  assert.equal(failure.ok, false)
  if (failure.ok === false) {
    assert.equal(failure.error.category, 'conflict')
  }
  assert.equal(failing.state.actionError?.category, 'conflict')
})

/* ------------------------------------------------------------ 产物下载（认证） */

test('downloadArtifact records the authenticated download payload', async () => {
  const h = createHarness()
  const result = await h.controller.downloadArtifact('job-new', 'report.json')
  assert.equal(result.ok, true)
  assert.deepEqual(h.calls.downloadArtifact, [{ jobId: 'job-new', name: 'report.json' }])
  assert.equal(h.state.lastDownload?.filename, 'report.json')
  assert.equal(h.state.lastDownload?.text, '{"ok":true}')
  assert.equal(h.state.busy.download, false)
})

test('downloadArtifact failure records the stable error and keeps no fake bytes', async () => {
  const h = createHarness({
    async downloadArtifact() {
      throw new ApiError({ status: 404, code: 'artifact_not_found', message: 'unknown artifact' })
    }
  })
  const result = await h.controller.downloadArtifact('job-new', 'nope.bin')
  assert.equal(result.ok, false)
  if (result.ok === false) {
    assert.equal(result.error.category, 'not_found')
  }
  assert.equal(h.state.lastDownload, null)
  assert.equal(h.state.actionError?.code, 'artifact_not_found')
})

test('reset clears state and stops polling', async () => {
  const h = createHarness()
  await h.controller.loadWizard()
  h.controller.startJobPolling('job-new')
  await flush()
  h.controller.reset()
  assert.equal(h.state.wizard, null)
  assert.equal(h.state.wizardPhase, 'loading')
  assert.equal(h.state.polling, false)
  assert.equal(h.state.pendingApply, null)
  assert.equal(h.state.lastDownload, null)
  assert.equal(h.clock.pending(), 0)
})

/* ------------------------------------------------------------- raw client */

test('ApiClient.postRaw sends raw bytes without JSON serialisation', async () => {
  const recorder = createRecordingFetch(() => jsonResponse(200, { ok: true }))
  const client = new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-raw' })
  const bytes = new Uint8Array([1, 2, 3]).buffer
  await client.postRaw('/api/calibration/uploads', bytes, {
    contentType: 'audio/wav',
    headers: { 'X-KHAEN-Filename': 'x.wav' }
  })
  const call = recorder.calls[0]
  assert.equal(call.method, 'POST')
  assert.equal(call.headers['Content-Type'], 'audio/wav')
  assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-raw')
  assert.ok(call.body instanceof ArrayBuffer)
})

test('createCalibrationApi is wired to the shared ApiClient shape', () => {
  const api = createCalibrationApi(new ApiClient({ fetchImpl: async () => jsonResponse(200, {}) }))
  assert.equal(typeof api.listJobs, 'function')
  assert.equal(typeof api.upload, 'function')
  assert.equal(typeof api.downloadArtifact, 'function')
  assert.equal(typeof api.applyJob, 'function')
  assert.equal(typeof api.applyWizard, 'function')
  assert.equal(typeof api.getWizardState, 'function')
})
