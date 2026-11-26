import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, ApiError } from '../src/api/client.ts'
import {
  CALIBRATION_JOB_SCHEMA,
  CALIBRATION_JOBS_SCHEMA,
  CALIBRATION_WIZARD_SCHEMA,
  CALIBRATION_WIZARD_STATE_SCHEMA,
  DEFAULT_CENTERS_HZ,
  JOB_POLL_INTERVAL_MS,
  WIZARD_STEP_COUNT,
  WIZARD_STEP_KEYS,
  blockReason,
  canAdvance,
  classifyCalibrationError,
  createCalibrationApi,
  engineTruth,
  firstIncompleteStep,
  gateForStep,
  isJobActive,
  isJobTerminal,
  jobDurationMs,
  jobResultTruth,
  maxUnlockedStep,
  parseApplyResult,
  parseCancelResult,
  parseCreateJobResponse,
  parseJobResponse,
  parseJobsResponse,
  parseRollbackResult,
  parseUploadResponse,
  parseWizardResponse,
  parseWizardStateResponse,
  stepDisplayState,
  stepKey,
  verdictTone
} from '../src/api/calibration.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value))
}

async function captureError(run: () => Promise<unknown> | unknown): Promise<ApiError> {
  try {
    await run()
  } catch (error) {
    assert.ok(error instanceof ApiError, 'expected an ApiError, got: ' + String(error))
    return error
  }
  return assert.fail('expected the call to reject') as never
}

const PLAN_SHA = 'a'.repeat(64)

function jobsBody(): Record<string, any> {
  return {
    schema: CALIBRATION_JOBS_SCHEMA,
    available: true,
    reason: null,
    engine: null,
    active_job_id: 'job-1',
    total: 2,
    limit: 20,
    offset: 0,
    jobs: [
      {
        id: 'job-1',
        kind: 'single_point_offline',
        status: 'running',
        verdict: null,
        created_at: '2026-11-26T06:00:00.000Z',
        started_at: '2026-11-26T06:00:01.000Z',
        finished_at: null,
        duration_ms: null,
        error: null,
        reason: null
      },
      {
        id: 'job-2',
        kind: 'single_point_offline',
        status: 'succeeded',
        verdict: 'PASS',
        created_at: '2026-11-25T06:00:00.000Z',
        started_at: '2026-11-25T06:00:01.000Z',
        finished_at: '2026-11-25T06:00:09.000Z',
        duration_ms: 8000,
        error: null,
        reason: null
      }
    ]
  }
}

function resultBody(): Record<string, any> {
  return {
    verdict: 'PASS',
    max_delta_db: 0.42,
    compared_bands: 30,
    missing_bands: 0,
    violations: [],
    bands: [
      { hz: 100, predicted_db: -3.0, observed_db: -2.7, delta_db: 0.3, missing: false },
      { hz: 1000, predicted_db: 0.0, observed_db: 0.1, delta_db: 0.1, missing: false }
    ],
    design: { residual: 'FULL', admission: { accepted: true, code: null, reason: null } },
    plan: { sha256: PLAN_SHA, graph_id: 'graph-abc' }
  }
}

function jobBody(): Record<string, any> {
  return {
    schema: CALIBRATION_JOB_SCHEMA,
    job: {
      id: 'job-2',
      kind: 'single_point_offline',
      status: 'succeeded',
      verdict: 'PASS',
      created_at: '2026-11-26T06:00:00.000Z',
      started_at: '2026-11-26T06:00:01.000Z',
      finished_at: '2026-11-26T06:00:09.000Z',
      duration_ms: 8000,
      error: null,
      reason: null,
      engine: 'synthetic',
      apply_state: 'not_applied',
      applied_version: null,
      steps: [
        {
          name: 'compile',
          status: 'succeeded',
          started_at: '2026-11-26T06:00:01.000Z',
          finished_at: '2026-11-26T06:00:02.000Z',
          sha256: 'b'.repeat(64),
          detail: null
        },
        {
          name: 'levels',
          status: 'running',
          started_at: '2026-11-26T06:00:02.000Z',
          finished_at: null,
          sha256: null,
          detail: null
        }
      ],
      artifacts: [
        { name: 'report.json', sha256: 'c'.repeat(64), bytes: 512, content_type: 'application/json' }
      ],
      inputs: {
        plan_sha: PLAN_SHA,
        baseline_upload_id: 'u1',
        applied_upload_id: 'u2',
        centers_hz: [100, 1000],
        tolerance_db: 2.0,
        min_bands: 3
      },
      result: resultBody()
    },
    reasons: {}
  }
}

function wizardConfig(): Record<string, any> {
  return {
    mic_cal_sha256: 'd'.repeat(64),
    mic_cal_filename: 'umik.txt',
    orientation: '90deg',
    target_mode: 'flat',
    single_point: { centers_hz: [...DEFAULT_CENTERS_HZ], tolerance_db: 2.0, min_bands: 3 }
  }
}

function wizardBody(): Record<string, any> {
  return {
    schema: CALIBRATION_WIZARD_SCHEMA,
    config: wizardConfig(),
    defaults: {
      mic_cal_sha256: null,
      mic_cal_filename: null,
      orientation: null,
      target_mode: 'flat',
      single_point: { centers_hz: [...DEFAULT_CENTERS_HZ], tolerance_db: 2.0, min_bands: 3 }
    },
    saved: true,
    updated_at: '2026-11-26T06:00:00.000Z',
    reasons: {}
  }
}

function wizardStateBody(): Record<string, any> {
  const statuses = ['passed', 'not_available', 'passed', 'passed', 'passed', 'passed', 'passed', 'blocked']
  const keys = [...WIZARD_STEP_KEYS]
  return {
    schema: CALIBRATION_WIZARD_STATE_SCHEMA,
    current_step: 8,
    steps: keys.map((key, index) => ({
      step: index + 1,
      key,
      status: statuses[index],
      reason: statuses[index] === 'blocked' ? 'plan not applied yet' : null,
      task: statuses[index] === 'not_available' ? 'M4' : null
    })),
    active_job_id: null,
    last_job_id: 'job-2',
    measurement: {
      baseline_upload_id: 'u1',
      applied_upload_id: 'u2',
      baseline_sha256: 'e'.repeat(64),
      applied_sha256: 'f'.repeat(64)
    },
    snapshot: { version: 3, graph_id: 'graph-old', sha256: PLAN_SHA },
    updated_at: '2026-11-26T06:00:00.000Z',
    reasons: {}
  }
}

/* ------------------------------------------------------------- 响应运行时校验 */

test('parseJobsResponse keeps list/pagination fields and defaults available=true', () => {
  const body = jobsBody()
  const parsed = parseJobsResponse(body)
  assert.equal(parsed.schema, CALIBRATION_JOBS_SCHEMA)
  assert.equal(parsed.available, true)
  assert.equal(parsed.active_job_id, 'job-1')
  assert.equal(parsed.jobs.length, 2)
  assert.equal(parsed.jobs[0].status, 'running')
  assert.equal(parsed.jobs[1].verdict, 'PASS')
  assert.equal(parsed.total, 2)
  assert.equal(parsed.limit, 20)

  const withoutAvailable = clone(body)
  delete withoutAvailable.available
  assert.equal(parseJobsResponse(withoutAvailable).available, true)
})

test('parseJobsResponse rejects wrong schema / unknown enums / non-array jobs', async () => {
  const wrongSchema = clone(jobsBody())
  wrongSchema.schema = 'khaen-calibration-jobs/v2'
  assert.equal((await captureError(() => parseJobsResponse(wrongSchema))).code, 'invalid_response')

  const unknownStatus = clone(jobsBody())
  unknownStatus.jobs[0].status = 'complete'
  assert.equal((await captureError(() => parseJobsResponse(unknownStatus))).code, 'invalid_response')

  const unknownVerdict = clone(jobsBody())
  unknownVerdict.jobs[1].verdict = 'PASSED'
  assert.equal((await captureError(() => parseJobsResponse(unknownVerdict))).code, 'invalid_response')

  const notArray = clone(jobsBody())
  notArray.jobs = {}
  assert.equal((await captureError(() => parseJobsResponse(notArray))).code, 'invalid_response')

  const noTotal = clone(jobsBody())
  delete noTotal.total
  assert.equal((await captureError(() => parseJobsResponse(noTotal))).code, 'invalid_response')
})

test('parseJobResponse parses steps/artifacts/result/apply_state and null result', () => {
  const parsed = parseJobResponse(jobBody())
  assert.equal(parsed.schema, CALIBRATION_JOB_SCHEMA)
  assert.equal(parsed.job.id, 'job-2')
  assert.equal(parsed.job.apply_state, 'not_applied')
  assert.equal(parsed.job.applied_version, null)
  assert.equal(parsed.job.steps.length, 2)
  assert.equal(parsed.job.steps[1].status, 'running')
  assert.equal(parsed.job.artifacts[0].name, 'report.json')
  assert.equal(parsed.job.result?.verdict, 'PASS')
  assert.equal(parsed.job.result?.bands.length, 2)
  assert.equal(parsed.job.result?.design?.residual, 'FULL')
  assert.equal(parsed.job.result?.plan?.graph_id, 'graph-abc')

  const pending = clone(jobBody())
  pending.job.status = 'queued'
  pending.job.verdict = null
  pending.job.result = null
  pending.job.steps = []
  pending.job.artifacts = []
  pending.job.apply_state = null
  const parsedPending = parseJobResponse(pending)
  assert.equal(parsedPending.job.result, null)
  assert.equal(parsedPending.job.steps.length, 0)
})

test('parseJobResponse rejects unknown step status / missing steps / bad design residual', async () => {
  const badStep = clone(jobBody())
  badStep.job.steps[0].status = 'done'
  assert.equal((await captureError(() => parseJobResponse(badStep))).code, 'invalid_response')

  const noSteps = clone(jobBody())
  delete noSteps.job.steps
  assert.equal((await captureError(() => parseJobResponse(noSteps))).code, 'invalid_response')

  const badResidual = clone(jobBody())
  badResidual.job.result.design.residual = 'MOSTLY'
  assert.equal((await captureError(() => parseJobResponse(badResidual))).code, 'invalid_response')

  const badApplyState = clone(jobBody())
  badApplyState.job.apply_state = 'committed'
  assert.equal((await captureError(() => parseJobResponse(badApplyState))).code, 'invalid_response')
})

test('parseWizardResponse validates config/defaults and target/orientation enums', () => {
  const parsed = parseWizardResponse(wizardBody())
  assert.equal(parsed.schema, CALIBRATION_WIZARD_SCHEMA)
  assert.equal(parsed.config.target_mode, 'flat')
  assert.equal(parsed.config.orientation, '90deg')
  assert.equal(parsed.config.single_point.centers_hz.length, 30)
  assert.equal(parsed.defaults.mic_cal_sha256, null)
  assert.equal(parsed.saved, true)

  const tilt = clone(wizardBody())
  tilt.config.target_mode = 'tilt10'
  assert.equal(parseWizardResponse(tilt).config.target_mode, 'tilt10')
})

test('parseWizardResponse rejects invalid orientation / target mode / single point', async () => {
  for (const bad of ['free', 'up', '45deg']) {
    const body = clone(wizardBody())
    body.config.orientation = bad
    assert.equal((await captureError(() => parseWizardResponse(body))).code, 'invalid_response')
  }
  for (const bad of ['single_point', 'room', 'none']) {
    const body = clone(wizardBody())
    body.config.target_mode = bad
    assert.equal((await captureError(() => parseWizardResponse(body))).code, 'invalid_response')
  }
  const noSingle = clone(wizardBody())
  delete noSingle.config.single_point
  assert.equal((await captureError(() => parseWizardResponse(noSingle))).code, 'invalid_response')
})

test('parseWizardStateResponse requires exactly 8 ordered gates', () => {
  const parsed = parseWizardStateResponse(wizardStateBody())
  assert.equal(parsed.schema, CALIBRATION_WIZARD_STATE_SCHEMA)
  assert.equal(parsed.steps.length, WIZARD_STEP_COUNT)
  assert.equal(parsed.current_step, 8)
  assert.equal(parsed.steps[1].status, 'not_available')
  assert.equal(parsed.steps[1].task, 'M4')
  assert.equal(parsed.steps[7].reason, 'plan not applied yet')
  assert.equal(parsed.measurement.baseline_upload_id, 'u1')
  assert.equal(parsed.snapshot?.version, 3)
})

test('parseWizardStateResponse rejects short/unordered/unknown gates instead of默认放行', async () => {
  const short = clone(wizardStateBody())
  short.steps = short.steps.slice(0, 7)
  assert.equal((await captureError(() => parseWizardStateResponse(short))).code, 'invalid_response')

  const wrongKey = clone(wizardStateBody())
  wrongKey.steps[2].key = 'positions'
  assert.equal((await captureError(() => parseWizardStateResponse(wrongKey))).code, 'invalid_response')

  const wrongStep = clone(wizardStateBody())
  wrongStep.steps[3].step = 5
  assert.equal((await captureError(() => parseWizardStateResponse(wrongStep))).code, 'invalid_response')

  const unknownGate = clone(wizardStateBody())
  unknownGate.steps[0].status = 'skipped'
  assert.equal((await captureError(() => parseWizardStateResponse(unknownGate))).code, 'invalid_response')

  const badCurrent = clone(wizardStateBody())
  badCurrent.current_step = 9
  assert.equal((await captureError(() => parseWizardStateResponse(badCurrent))).code, 'invalid_response')

  const noSteps = clone(wizardStateBody())
  delete noSteps.steps
  assert.equal((await captureError(() => parseWizardStateResponse(noSteps))).code, 'invalid_response')
})

test('upload/create/apply/rollback/cancel responses are validated', () => {
  assert.deepEqual(parseUploadResponse({ upload_id: 'u1', sha256: 'a'.repeat(64), bytes: 1024, created: true }), {
    upload_id: 'u1',
    sha256: 'a'.repeat(64),
    bytes: 1024
  })
  assert.deepEqual(parseCreateJobResponse({ job_id: 'job-9', status: 'queued' }), { job_id: 'job-9', status: 'queued' })
  assert.equal(parseCreateJobResponse({ job_id: 'job-9' }).status, 'queued')

  const applied = parseApplyResult({
    applied: true,
    job_id: 'job-9',
    applied_version: 4,
    graph_id: 'g-1',
    sha256: null,
    snapshot: { version: 3, graph_id: 'g-0', sha256: null },
    ack: { synthetic: false, actual: true },
    readback: { actual_version: 4 },
    applied_at: '2026-11-26T06:00:00.000Z'
  })
  assert.equal(applied.applied, true)
  assert.equal(applied.applied_version, 4)
  assert.equal(applied.sha256, null)
  assert.equal(applied.snapshot?.sha256, null)
  assert.equal(applied.ack?.actual, true)

  const rolledBack = parseRollbackResult({
    rolled_back: true,
    restored_version: 3,
    graph_id: 'g-0',
    sha256: null,
    readback: null,
    rolled_back_at: null
  })
  assert.equal(rolledBack.restored_version, 3)
  assert.equal(rolledBack.sha256, null)

  assert.deepEqual(parseCancelResult(null), { job_id: null, status: null })
  assert.deepEqual(parseCancelResult({ job_id: 'job-9', status: 'cancelled' }), {
    job_id: 'job-9',
    status: 'cancelled'
  })
})

test('apply/rollback responses reject applied=false (no fake success)', async () => {
  const body = {
    applied: false,
    applied_version: 4,
    graph_id: 'g-1',
    sha256: null
  }
  assert.equal((await captureError(() => parseApplyResult(body))).code, 'invalid_response')

  const rollbackBody = { rolled_back: false, restored_version: 3, graph_id: 'g-0', sha256: null }
  assert.equal((await captureError(() => parseRollbackResult(rollbackBody))).code, 'invalid_response')
})

/* ------------------------------------------------------------------ 派生逻辑 */

test('gate helpers fail closed and treat not_available as a declared gap', () => {
  const state = parseWizardStateResponse(wizardStateBody())
  assert.equal(canAdvance(state, 1), true)
  assert.equal(canAdvance(state, 2), true) // not_available = 能力未接入，可继续但不产生数据
  assert.equal(canAdvance(state, 8), false)
  assert.equal(canAdvance(null, 1), false)

  assert.equal(gateForStep(state, 2)?.key, 'layout_channels')
  assert.equal(gateForStep(state, 9), null)
  assert.equal(blockReason(state, 8), 'plan not applied yet')
  assert.equal(blockReason(state, 1), null)
  assert.equal(blockReason(state, 9), 'missing gate')

  assert.equal(maxUnlockedStep(state), 8) // 前 7 步可继续，第 8 步是当前最后一步
  const blockedEarly = clone(wizardStateBody())
  blockedEarly.steps[2].status = 'blocked'
  const blockedState = parseWizardStateResponse(blockedEarly)
  assert.equal(maxUnlockedStep(blockedState), 3)
  assert.equal(firstIncompleteStep(blockedState), 3)
  assert.equal(firstIncompleteStep(state), 8)
  assert.equal(stepKey(1), 'device_check')
  assert.equal(stepKey(8), 'review_apply')
  assert.equal(stepKey(0), null)
})

test('stepDisplayState marks done/current/notAvailable/locked', () => {
  const state = parseWizardStateResponse(wizardStateBody())
  assert.equal(stepDisplayState(state, 1, 3), 'done')
  assert.equal(stepDisplayState(state, 2, 3), 'notAvailable')
  assert.equal(stepDisplayState(state, 3, 3), 'current')
  assert.equal(stepDisplayState(state, 8, 3), 'locked')
  assert.equal(stepDisplayState(state, 2, 2), 'notAvailable')
  assert.equal(stepDisplayState(null, 4, 1), 'locked')
})

test('job helpers classify active/terminal/duration/verdict/truth', () => {
  assert.equal(isJobActive('queued'), true)
  assert.equal(isJobActive('running'), true)
  assert.equal(isJobActive('succeeded'), false)
  assert.equal(isJobTerminal('succeeded'), true)
  assert.equal(isJobTerminal('failed'), true)
  assert.equal(isJobTerminal('cancelled'), true)
  assert.equal(isJobTerminal('running'), false)

  assert.equal(jobDurationMs({ duration_ms: 8000, started_at: null, finished_at: null } as any), 8000)
  assert.equal(
    jobDurationMs({
      duration_ms: null,
      started_at: '2026-11-26T06:00:00.000Z',
      finished_at: '2026-11-26T06:00:02.500Z'
    } as any),
    2500
  )
  assert.equal(jobDurationMs({ duration_ms: null, started_at: null, finished_at: null } as any), null)
  assert.equal(jobDurationMs(null), null)

  assert.equal(verdictTone('PASS'), 'ok')
  assert.equal(verdictTone('FAIL'), 'bad')
  assert.equal(verdictTone('INCOMPLETE'), 'warn')
  assert.equal(verdictTone(null), null)

  assert.equal(engineTruth('synthetic'), 'demo')
  assert.equal(engineTruth('mock'), 'demo')
  assert.equal(engineTruth('real_audio'), 'actual')
  assert.equal(engineTruth(null), 'notWired')
  assert.equal(jobResultTruth('synthetic'), 'demo')
  assert.equal(jobResultTruth(null), 'actual')
  assert.equal(JOB_POLL_INTERVAL_MS, 1500)
  assert.equal(DEFAULT_CENTERS_HZ.length, 30)
  assert.equal(WIZARD_STEP_COUNT, 8)
})

/* ---------------------------------------------------------------- 错误归类 */

test('classifyCalibrationError maps stable codes to UI categories', () => {
  const notWired = classifyCalibrationError(new ApiError({ status: 503, code: 'not_wired', message: 'pending' }))
  assert.equal(notWired.category, 'not_wired')
  assert.equal(notWired.i18nKey, 'error.calibration.notWired')
  assert.equal(notWired.retryable, false)

  assert.equal(classifyCalibrationError(new ApiError({ status: 409, code: 'switch_busy', message: 'busy' })).category, 'busy')
  assert.equal(
    classifyCalibrationError(new ApiError({ status: 409, code: 'job_not_succeeded', message: 'no' })).category,
    'conflict'
  )
  assert.equal(
    classifyCalibrationError(new ApiError({ status: 409, code: 'rollback_unavailable', message: 'no' })).category,
    'conflict'
  )
  assert.equal(
    classifyCalibrationError(new ApiError({ status: 409, code: 'artifact_sha_mismatch', message: 'sha' })).category,
    'conflict'
  )
  assert.equal(classifyCalibrationError(new ApiError({ status: 404, code: 'job_not_found', message: 'x' })).category, 'not_found')
  assert.equal(
    classifyCalibrationError(new ApiError({ status: 400, code: 'invalid_request', message: 'x' })).category,
    'invalid_request'
  )
  assert.equal(
    classifyCalibrationError(new ApiError({ status: 413, code: 'upload_too_large', message: 'big' })).category,
    'upload_too_large'
  )
  assert.equal(
    classifyCalibrationError(new ApiError({ status: 415, code: 'unsupported_format', message: 'fmt' })).category,
    'unsupported_format'
  )
  assert.equal(classifyCalibrationError(new ApiError({ status: 401, code: 'not_authenticated', message: 'x' })).category, 'unauthorized')
  assert.equal(classifyCalibrationError(new ApiError({ status: 0, code: 'network_error', message: 'down' })).category, 'network')
  assert.equal(classifyCalibrationError(new ApiError({ status: 0, code: 'request_timeout', message: 'slow' })).category, 'timeout')
  assert.equal(classifyCalibrationError(new ApiError({ status: 500, code: 'internal_error', message: 'x' })).category, 'server')
  assert.equal(
    classifyCalibrationError(new ApiError({ status: 200, code: 'invalid_response', message: 'bad' })).category,
    'invalid_response'
  )
  assert.equal(classifyCalibrationError(new ApiError({ status: 418, code: 'teapot', message: 'tea' })).category, 'unknown')

  const unknown = classifyCalibrationError(new Error('code bug'))
  assert.equal(unknown.category, 'unknown')
  assert.equal(unknown.code, 'unknown_error')
  assert.equal(unknown.detail, 'code bug')
})

/* ---------------------------------------------------------------------- 端点 */

test('listJobs/getJob/createJob build correct requests', async () => {
  const recorder = createRecordingFetch((call) => {
    if (call.method === 'GET' && (call.url.includes('/api/calibration/jobs?') || call.url.endsWith('/api/calibration/jobs'))) {
      return jsonResponse(200, jobsBody())
    }
    return jsonResponse(202, { job_id: 'job-new', status: 'queued' })
  })
  const api = createCalibrationApi(
    new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-1' })
  )

  const list = await api.listJobs({ limit: 10, offset: 20 })
  assert.equal(list.jobs.length, 2)
  assert.match(recorder.calls[0].url, /limit=10/)
  assert.match(recorder.calls[0].url, /offset=20/)
  assert.equal(recorder.calls[0].method, 'GET')

  const created = await api.createJob({
    kind: 'single_point_offline',
    inputs: {
      plan_sha: PLAN_SHA,
      baseline_upload_id: 'u1',
      applied_upload_id: 'u2',
      centers_hz: [100, 1000],
      tolerance_db: 2.0,
      min_bands: 3
    }
  })
  assert.equal(created.job_id, 'job-new')
  const createCall = recorder.calls[1]
  assert.equal(createCall.method, 'POST')
  assert.equal(createCall.headers['Content-Type'], 'application/json')
  assert.equal(createCall.headers['X-KHAEN-CSRF'], 'csrf-1')
  const body = JSON.parse(String(createCall.body))
  assert.equal(body.kind, 'single_point_offline')
  assert.equal(body.inputs.plan_sha, PLAN_SHA)
})

test('upload posts raw bytes with content type/filename/role headers and CSRF', async () => {
  const recorder = createRecordingFetch(() =>
    jsonResponse(201, { upload_id: 'u1', sha256: 'a'.repeat(64), bytes: 44, created: true })
  )
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-2' }))
  const buffer = new Uint8Array([82, 73, 70, 70]).buffer
  const result = await api.upload(buffer, {
    contentType: 'audio/wav',
    filename: 'baseline.wav',
    role: 'baseline'
  })
  assert.equal(result.upload_id, 'u1')
  const call = recorder.calls[0]
  assert.equal(call.url, '/api/calibration/uploads')
  assert.equal(call.method, 'POST')
  assert.equal(call.headers['Content-Type'], 'audio/wav')
  assert.equal(call.headers['X-KHAEN-Filename'], 'baseline.wav')
  assert.equal(call.headers['X-KHAEN-Upload-Role'], 'baseline')
  assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-2')
  assert.ok(call.body instanceof ArrayBuffer)
  assert.equal((call.body as ArrayBuffer).byteLength, 4)
})

test('wizard GET/PUT and apply endpoints build correct requests', async () => {
  const recorder = createRecordingFetch((call) => {
    if (call.method === 'PUT') {
      return jsonResponse(200, wizardBody())
    }
    if (call.url.endsWith('/apply')) {
      return jsonResponse(200, {
        applied: true,
        applied_version: 4,
        graph_id: 'g-1',
        sha256: null,
        snapshot: null,
        ack: null,
        readback: null,
        applied_at: null
      })
    }
    if (call.url.endsWith('/state')) {
      return jsonResponse(200, wizardStateBody())
    }
    return jsonResponse(200, wizardBody())
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-3' }))

  const wizard = await api.getWizard()
  assert.equal(wizard.saved, true)
  assert.equal(recorder.calls[0].url, '/api/calibration/wizard')

  const state = await api.getWizardState()
  assert.equal(state.steps.length, 8)
  assert.equal(recorder.calls[1].url, '/api/calibration/wizard/state')

  const saved = await api.putWizard({
    ...wizard.config,
    measurement: { baseline_upload_id: 'u1', applied_upload_id: 'u2', plan_sha: PLAN_SHA }
  })
  assert.equal(saved.saved, true)
  const putCall = recorder.calls[2]
  assert.equal(putCall.method, 'PUT')
  assert.equal(putCall.headers['X-KHAEN-CSRF'], 'csrf-3')
  const putBody = JSON.parse(String(putCall.body))
  assert.equal(putBody.measurement.baseline_upload_id, 'u1')

  const applied = await api.applyWizard()
  assert.equal(applied.applied_version, 4)
  assert.equal(recorder.calls[3].url, '/api/calibration/wizard/apply')
})

test('job actions and artifact URLs use encoded ids', async () => {
  const recorder = createRecordingFetch((call) => {
    if (call.url.includes('/artifacts/')) {
      return jsonResponse(404, { error: { code: 'artifact_not_found', message: 'nope' } })
    }
    return jsonResponse(200, {
      job_id: 'job/1',
      status: 'cancelled'
    })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-4' }))

  assert.equal(api.artifactUrl('job/1', 'report.json'), '/api/calibration/jobs/job%2F1/artifacts/report.json')

  const cancelled = await api.cancelJob('job/1')
  assert.equal(cancelled.status, 'cancelled')
  assert.equal(recorder.calls[0].url, '/api/calibration/jobs/job%2F1/cancel')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], 'csrf-4')
})

test('calibration 401 reaches the shared unauthorized hook', async () => {
  const seen: string[] = []
  const recorder = createRecordingFetch(() =>
    jsonResponse(401, { error: { code: 'not_authenticated', message: 'auth required' } })
  )
  const api = createCalibrationApi(
    new ApiClient({ fetchImpl: recorder.fetch, onUnauthorized: (error) => seen.push(error.code) })
  )
  const error = await captureError(() => api.listJobs())
  assert.equal(error.status, 401)
  assert.deepEqual(seen, ['not_authenticated'])
})
