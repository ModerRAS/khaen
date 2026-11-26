/**
 * 校准 REST 合同测试（docs/API.md v1.1 §7「实际形状 + 加法目标」）。
 *
 * 目的：逐端点断言前端实际发出的 **path / method / body / headers** 与合同一致，
 * 并覆盖 400/401/503（not_wired）与 jobs.available=false 的语义。
 * 响应形状的细节校验在 tests/calibration-api.test.ts；这里聚焦"接线"。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, ApiError } from '../src/api/client.ts'
import {
  CALIBRATION_JOB_SCHEMA,
  CALIBRATION_JOBS_SCHEMA,
  CALIBRATION_WIZARD_SCHEMA,
  CALIBRATION_WIZARD_STATE_SCHEMA,
  DEFAULT_CENTERS_HZ,
  WIZARD_STEP_KEYS,
  classifyCalibrationError,
  createCalibrationApi
} from '../src/api/calibration.ts'
import { createRecordingFetch, jsonResponse, textResponse } from '../scripts/mock-test-support.mjs'

const PLAN_SHA = 'a'.repeat(64)
const BASELINE_ID = '1'.repeat(64)
const APPLIED_ID = '2'.repeat(64)
const JOB_ID = 'job-20261126-0001'

const INPUTS = {
  plan_sha: PLAN_SHA,
  baseline_upload_id: BASELINE_ID,
  applied_upload_id: APPLIED_ID,
  centers_hz: [...DEFAULT_CENTERS_HZ],
  tolerance_db: 2.0,
  min_bands: 3
}

function jobSummary(): Record<string, unknown> {
  // daemon 实际形状（id/status）+ §7【加法】；合同文档写 job_id/state，两种都必须能解析。
  return {
    id: JOB_ID,
    kind: 'single_point_offline',
    status: 'succeeded',
    verdict: 'PASS',
    created_at: '2026-11-26T06:00:00.000Z',
    started_at: '2026-11-26T06:00:01.000Z',
    finished_at: '2026-11-26T06:00:09.000Z',
    duration_ms: 8000,
    created_unix: 1764136800,
    updated_unix: 1764136809,
    progress: 1,
    error: null,
    reason: null
  }
}

function jobsPage(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    schema: CALIBRATION_JOBS_SCHEMA,
    available: true,
    reason: null,
    engine: null,
    active_job_id: null,
    total: 1,
    limit: 20,
    offset: 0,
    jobs: [jobSummary()],
    reasons: {},
    ...overrides
  }
}

function jobDetail(): Record<string, unknown> {
  const result = {
    verdict: 'PASS',
    max_delta_db: 0.3,
    compared_bands: 30,
    missing_bands: 0,
    violations: [],
    bands: [],
    design: { residual: 'FULL', admission: { accepted: true, code: null, reason: null } },
    plan: { sha256: PLAN_SHA, graph_id: 'graph-new' },
    measured: null,
    simulated: { predicted_from: 'designer prediction', plan_sha256: PLAN_SHA, graph_id: 'graph-new' }
  }
  const artifacts = [{ name: 'report.json', sha256: 'b'.repeat(64), bytes: 128, content_type: 'application/json', downloadable: true }]
  return {
    ok: true,
    schema: CALIBRATION_JOB_SCHEMA,
    job: {
      ...jobSummary(),
      engine: 'synthetic',
      apply_state: 'not_applied',
      applied_version: null,
      apply: { state: 'not_applied', applied_version: null, previous_version: null, applied_at: null, plan_sha256: null },
      steps: [
        { name: 'compile', status: 'succeeded', started_at: null, finished_at: null, sha256: null, detail: null },
        { name: 'levels', status: 'succeeded', started_at: null, finished_at: null, sha256: null, detail: null },
        { name: 'closed_loop', status: 'succeeded', started_at: null, finished_at: null, sha256: null, detail: null },
        { name: 'bundle', status: 'succeeded', started_at: null, finished_at: null, sha256: null, detail: null }
      ],
      inputs: INPUTS
    },
    // canonical 详情顶层：{ok,schema,job,result,artifacts,error,reasons}。
    result,
    artifacts,
    error: null,
    reasons: {}
  }
}

function wizardConfig(): Record<string, unknown> {
  return {
    mic_cal_sha256: null,
    mic_cal_filename: null,
    orientation: '0deg',
    target_mode: 'flat',
    single_point: { centers_hz: [...DEFAULT_CENTERS_HZ], tolerance_db: 2.0, min_bands: 3 }
  }
}

function wizardBody(): Record<string, unknown> {
  return {
    schema: CALIBRATION_WIZARD_SCHEMA,
    config: wizardConfig(),
    defaults: wizardConfig(),
    saved: false,
    updated_at: null,
    reasons: {}
  }
}

function wizardStateBody(): Record<string, unknown> {
  const statuses = ['passed', 'not_available', 'passed', 'passed', 'passed', 'passed', 'passed', 'blocked']
  return {
    schema: CALIBRATION_WIZARD_STATE_SCHEMA,
    current_step: 8,
    steps: WIZARD_STEP_KEYS.map((key, index) => ({
      step: index + 1,
      key,
      status: statuses[index],
      reason: null,
      task: null
    })),
    active_job_id: null,
    last_job_id: JOB_ID,
    measurement: {
      baseline_upload_id: BASELINE_ID,
      applied_upload_id: APPLIED_ID,
      baseline_sha256: BASELINE_ID,
      applied_sha256: APPLIED_ID
    },
    snapshot: null,
    updated_at: '2026-11-26T06:00:00.000Z',
    reasons: {}
  }
}

function bodyOf(call: { body: unknown }): Record<string, unknown> | null {
  if (call.body === undefined || call.body === null) {
    return null
  }
  if (typeof call.body === 'string') {
    return JSON.parse(call.body) as Record<string, unknown>
  }
  return null
}

async function captureError(run: () => Promise<unknown>): Promise<ApiError> {
  try {
    await run()
  } catch (error) {
    assert.ok(error instanceof ApiError, 'expected ApiError, got: ' + String(error))
    return error
  }
  return assert.fail('expected the request to reject') as never
}

/** 用一份路由表构造 recording fetch：未匹配请求直接失败（测试断言 path 拼写）。 */
function createContractFetch(routes: Record<string, (call: { body: unknown }) => Response>) {
  return createRecordingFetch((call) => {
    const key = call.method + ' ' + call.url
    const handler = routes[key]
    assert.ok(handler !== undefined, 'unexpected request: ' + key)
    return handler(call)
  })
}

/* ------------------------------------------------------------------ uploads */

test('contract §7: POST /api/calibration/uploads sends raw WAV bytes (octet-stream / audio-wav)', async () => {
  const recorder = createContractFetch({
    'POST /api/calibration/uploads': () =>
      jsonResponse(201, { upload_id: BASELINE_ID, sha256: BASELINE_ID, bytes: 44, created: true })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-upload' }))
  const bytes = new Uint8Array([82, 73, 70, 70, 1, 2, 3, 4])
  const result = await api.upload(bytes.buffer as ArrayBuffer, {
    contentType: 'application/octet-stream',
    filename: 'baseline.wav',
    role: 'baseline'
  })
  assert.equal(result.upload_id, BASELINE_ID)
  assert.equal(result.created, true)
  const call = recorder.calls[0]
  assert.equal(call.method, 'POST')
  assert.equal(call.url, '/api/calibration/uploads')
  assert.equal(call.headers['Content-Type'], 'application/octet-stream')
  assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-upload')
  assert.deepEqual([...new Uint8Array(call.body as ArrayBuffer)], [...bytes])

  // 同 sha 幂等：200 + created:false（前端如实透传，不擅自当新上传）。
  const idempotentRecorder = createContractFetch({
    'POST /api/calibration/uploads': () =>
      jsonResponse(200, { upload_id: BASELINE_ID, sha256: BASELINE_ID, bytes: 8, created: false })
  })
  const idempotentApi = createCalibrationApi(new ApiClient({ fetchImpl: idempotentRecorder.fetch }))
  const again = await idempotentApi.upload(bytes.buffer as ArrayBuffer, { contentType: 'audio/wav' })
  assert.equal(again.created, false)
})

test('contract §7: upload failure keeps the M1 stable error code (400/413/415)', async () => {
  const cases: Array<[Response, number, string]> = [
    [jsonResponse(400, { error: { code: 'invalid_upload', message: 'upload body is empty' } }), 400, 'invalid_upload'],
    [jsonResponse(413, { error: { code: 'upload_too_large', message: 'too big' } }), 413, 'upload_too_large'],
    [jsonResponse(415, { error: { code: 'unsupported_format', message: 'bad type' } }), 415, 'unsupported_format']
  ]
  for (const [response, status, code] of cases) {
    const recorder = createContractFetch({
      'POST /api/calibration/uploads': () => response
    })
    const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch }))
    const error = await captureError(() => api.upload(new ArrayBuffer(1), { contentType: 'audio/wav' }))
    assert.equal(error.status, status)
    assert.equal(error.code, code)
  }
})

/* --------------------------------------------------------------------- jobs */

test('contract §7: GET /api/calibration/jobs parses actual shape and available=false as NOT_AVAILABLE', async () => {
  const recorder = createContractFetch({
    'GET /api/calibration/jobs?limit=20&offset=0': () => jsonResponse(200, jobsPage())
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const page = await api.listJobs({ limit: 20, offset: 0 })
  assert.equal(page.schema, CALIBRATION_JOBS_SCHEMA)
  assert.equal(page.available, true)
  assert.equal(page.jobs[0].id, JOB_ID)
  assert.equal(page.jobs[0].status, 'succeeded')
  assert.equal(page.jobs[0].progress, 1)
  assert.equal(page.total, 1)
  assert.equal(recorder.calls[0].query, undefined)
  assert.equal(recorder.calls[0].method, 'GET')

  // available=false：200 + reason，不是 500；页面必须显示 NOT_AVAILABLE + 原因而不是"0 个 job"。
  const unavailable = createContractFetch({
    'GET /api/calibration/jobs': () =>
      jsonResponse(
        200,
        jobsPage({ available: false, reason: 'calibration job backend not wired', jobs: [], total: 0, reasons: { jobs: 'not wired' } })
      )
  })
  const unavailableApi = createCalibrationApi(new ApiClient({ fetchImpl: unavailable.fetch }))
  const pageUnavailable = await unavailableApi.listJobs()
  assert.equal(pageUnavailable.available, false)
  assert.equal(pageUnavailable.reason, 'calibration job backend not wired')
  assert.equal(pageUnavailable.reasons.jobs, 'not wired')
  assert.equal(pageUnavailable.jobs.length, 0)
})

test('contract §7: GET /jobs 503 not_wired maps to the "not wired" UI category', async () => {
  const recorder = createContractFetch({
    'GET /api/calibration/jobs': () =>
      jsonResponse(503, { error: { code: 'not_wired', message: 'calibration job backend not wired' } })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const error = await captureError(() => api.listJobs())
  assert.equal(error.status, 503)
  const info = classifyCalibrationError(error)
  assert.equal(info.category, 'not_wired')
  assert.equal(info.retryable, false)
  assert.equal(info.i18nKey, 'error.calibration.notWired')
})

test('contract §7: POST /api/calibration/jobs sends canonical kind+inputs and parses 202 {job_id,location}', async () => {
  const recorder = createContractFetch({
    'POST /api/calibration/jobs': () =>
      jsonResponse(202, { job_id: JOB_ID, location: '/api/calibration/jobs/' + JOB_ID, ok: true, state: 'queued' })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-job' }))
  const created = await api.createJob({ kind: 'single_point_offline', inputs: INPUTS })
  assert.equal(created.job_id, JOB_ID)
  assert.equal(created.location, '/api/calibration/jobs/' + JOB_ID)
  assert.equal(created.status, 'queued')

  const call = recorder.calls[0]
  assert.equal(call.method, 'POST')
  assert.equal(call.url, '/api/calibration/jobs')
  assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-job')
  const body = bodyOf(call)
  assert.ok(body !== null)
  // 裁决 C：kind=single_point_offline，inputs 恰好六个字段；repeats/drive_level_dbfs/holdout_points 不进请求。
  assert.deepEqual(Object.keys(body as Record<string, unknown>).sort(), ['inputs', 'kind'])
  assert.equal(body?.kind, 'single_point_offline')
  assert.deepEqual(Object.keys(body?.inputs as Record<string, unknown>).sort(), [
    'applied_upload_id',
    'baseline_upload_id',
    'centers_hz',
    'min_bands',
    'plan_sha',
    'tolerance_db'
  ])
  assert.deepEqual(body?.inputs, INPUTS)
})

test('contract §7: create job 400 bad_request keeps code + details; 401 triggers login hook', async () => {
  const bad = createContractFetch({
    'POST /api/calibration/jobs': () =>
      jsonResponse(400, {
        error: { code: 'bad_request', message: 'inputs.plan_sha is required', details: { field: 'inputs.plan_sha' } }
      })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: bad.fetch, csrfToken: () => 'csrf' }))
  const error = await captureError(() => api.createJob({ kind: 'single_point_offline', inputs: INPUTS }))
  assert.equal(error.status, 400)
  assert.equal(error.code, 'bad_request')
  assert.deepEqual(error.details, { field: 'inputs.plan_sha' })
  assert.equal(classifyCalibrationError(error).category, 'invalid_request')

  const unauthorized = createContractFetch({
    'POST /api/calibration/jobs': () => jsonResponse(401, { error: { code: 'unauthorized', message: 'no session' } })
  })
  const seen: string[] = []
  const loginApi = createCalibrationApi(
    new ApiClient({ fetchImpl: unauthorized.fetch, onUnauthorized: (err) => seen.push(err.code) })
  )
  const authError = await captureError(() => loginApi.createJob({ kind: 'single_point_offline', inputs: INPUTS }))
  assert.equal(authError.status, 401)
  assert.deepEqual(seen, ['unauthorized'])
})

test('contract §7: GET /jobs/{id} reads canonical top-level result/artifacts and the job.apply object', async () => {
  const detail = jobDetail()
  const recorder = createContractFetch({
    ['GET /api/calibration/jobs/' + JOB_ID]: () => jsonResponse(200, detail)
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const parsed = await api.getJob(JOB_ID)
  assert.equal(parsed.job.id, JOB_ID)
  assert.equal(parsed.job.result?.verdict, 'PASS')
  assert.equal(parsed.job.apply?.state, 'not_applied')
  assert.equal(parsed.job.artifacts[0].name, 'report.json')
  assert.equal(parsed.job.result?.simulated !== null, true)
})

test('contract §7: POST /jobs/{id}/cancel returns canonical cancelled (double l)', async () => {
  const recorder = createContractFetch({
    ['POST /api/calibration/jobs/' + JOB_ID + '/cancel']: () =>
      jsonResponse(200, { job_id: JOB_ID, status: 'cancelled' })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-cancel' }))
  const result = await api.cancelJob(JOB_ID)
  assert.equal(result.status, 'cancelled')
  assert.equal(recorder.calls[0].method, 'POST')
  assert.equal(recorder.calls[0].url, '/api/calibration/jobs/' + JOB_ID + '/cancel')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], 'csrf-cancel')
})

/* -------------------------------------------------------- apply / rollback */

test('contract §7: apply/rollback are POSTed on /jobs/{id}/apply|rollback (never /api/calibration/apply)', async () => {
  const applyReceipt = {
    applied: true,
    job_id: JOB_ID,
    applied_version: 4,
    graph_id: 'graph-new',
    sha256: PLAN_SHA,
    snapshot: { version: 3, graph_id: 'graph-old', sha256: null },
    ack: { synthetic: false, actual: true },
    readback: { actual_version: 4 },
    applied_at: '2026-11-26T06:00:10.000Z'
  }
  const rollbackReceipt = {
    rolled_back: true,
    job_id: JOB_ID,
    restored_version: 3,
    graph_id: 'graph-old',
    sha256: null,
    readback: null,
    rolled_back_at: '2026-11-26T06:00:11.000Z'
  }
  const recorder = createContractFetch({
    ['POST /api/calibration/jobs/' + JOB_ID + '/apply']: () => jsonResponse(200, applyReceipt),
    ['POST /api/calibration/jobs/' + JOB_ID + '/rollback']: () => jsonResponse(200, rollbackReceipt)
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-two' }))

  const applied = await api.applyJob(JOB_ID)
  assert.equal(applied.kind, 'applied')
  assert.equal(recorder.calls[0].url, '/api/calibration/jobs/' + JOB_ID + '/apply')
  assert.deepEqual(bodyOf(recorder.calls[0]), {})
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], 'csrf-two')

  const rolledBack = await api.rollbackJob(JOB_ID)
  assert.equal(rolledBack.kind, 'rolled_back')
  assert.equal(recorder.calls[1].url, '/api/calibration/jobs/' + JOB_ID + '/rollback')
  assert.deepEqual(bodyOf(recorder.calls[1]), {})
  // 绝不能打到不存在的旧端点。
  for (const call of recorder.calls) {
    assert.equal(call.url.includes('/api/calibration/apply'), false)
    assert.equal(call.url.includes('/api/calibration/rollback'), false)
  }
})

test('contract §7【目标】: staged prepare → commit/abort bodies carry stage and engine_token', async () => {
  const recorder = createContractFetch({
    ['POST /api/calibration/jobs/' + JOB_ID + '/apply']: (call) => {
      const body = bodyOf(call)
      if (body?.stage === 'prepare') {
        return jsonResponse(200, {
          stage: 'prepare',
          job_id: JOB_ID,
          engine_token: 'eng-1',
          plan_sha256: PLAN_SHA,
          previous_version: 3,
          readback: null
        })
      }
      if (body?.stage === 'commit') {
        return jsonResponse(200, {
          stage: 'commit',
          applied: true,
          job_id: JOB_ID,
          applied_version: 4,
          previous_version: 3,
          graph_id: 'graph-new',
          readback: {}
        })
      }
      return jsonResponse(200, { stage: 'abort', aborted: true, job_id: JOB_ID, plan_sha256: PLAN_SHA })
    },
    ['POST /api/calibration/jobs/' + JOB_ID + '/rollback']: (call) => {
      const body = bodyOf(call)
      if (body?.stage === 'prepare') {
        return jsonResponse(200, {
          stage: 'prepare',
          job_id: JOB_ID,
          engine_token: 'eng-r',
          target_version: 3,
          readback: null
        })
      }
      return jsonResponse(200, { stage: 'abort', aborted: true, job_id: JOB_ID, target_version: 3 })
    }
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-staged' }))

  const prepare = await api.applyJob(JOB_ID, { stage: 'prepare' })
  assert.equal(prepare.kind, 'prepare')
  if (prepare.kind === 'prepare') {
    assert.equal(prepare.engine_token, 'eng-1')
  }
  assert.deepEqual(bodyOf(recorder.calls[0]), { stage: 'prepare' })

  const commit = await api.applyJob(JOB_ID, { stage: 'commit', engine_token: 'eng-1' })
  assert.equal(commit.kind, 'applied')
  if (commit.kind === 'applied') {
    assert.equal(commit.stage, 'commit')
    assert.equal(commit.previous_version, 3)
  }
  assert.deepEqual(bodyOf(recorder.calls[1]), { stage: 'commit', engine_token: 'eng-1' })

  const abort = await api.applyJob(JOB_ID, { stage: 'abort', engine_token: 'eng-1' })
  assert.equal(abort.kind, 'abort')
  assert.deepEqual(bodyOf(recorder.calls[2]), { stage: 'abort', engine_token: 'eng-1' })

  const rollbackPrepare = await api.rollbackJob(JOB_ID, { stage: 'prepare' })
  assert.equal(rollbackPrepare.kind, 'prepare')
  if (rollbackPrepare.kind === 'prepare') {
    assert.equal(rollbackPrepare.target_version, 3)
  }
})

test('contract §7: apply/rollback 409 engine_token_invalid is a conflict, not success', async () => {
  const recorder = createContractFetch({
    ['POST /api/calibration/jobs/' + JOB_ID + '/apply']: () =>
      jsonResponse(409, { error: { code: 'engine_token_invalid', message: 'unknown or consumed token' } })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const error = await captureError(() => api.applyJob(JOB_ID, { stage: 'commit', engine_token: 'stale' }))
  assert.equal(error.status, 409)
  assert.equal(error.code, 'engine_token_invalid')
  assert.equal(classifyCalibrationError(error).category, 'conflict')
})

/* --------------------------------------------------------------- artifacts */

test('contract §7: artifacts download goes through the authenticated client (getRaw), not <a href>', async () => {
  const recorder = createContractFetch({
    ['GET /api/calibration/jobs/' + JOB_ID + '/artifacts/report.json']: () =>
      textResponse(200, '{"report":true}', {
        'Content-Type': 'application/json',
        'Content-Disposition': 'attachment; filename="report.json"',
        'x-khaen-artifact-sha256': 'b'.repeat(64)
      }),
    ['GET /api/calibration/jobs/' + JOB_ID + '/artifacts/manifest.json']: () =>
      jsonResponse(404, { error: { code: 'artifact_not_found', message: 'not whitelisted' } })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const download = await api.downloadArtifact(JOB_ID, 'report.json')
  assert.equal(download.filename, 'report.json')
  assert.equal(download.text, '{"report":true}')
  assert.equal(download.byte_length, 15)
  assert.equal(new TextDecoder().decode(download.data), '{"report":true}')
  const call = recorder.calls[0]
  assert.equal(call.method, 'GET')
  assert.equal(call.credentials, 'include')

  const error = await captureError(() => api.downloadArtifact(JOB_ID, 'manifest.json'))
  assert.equal(error.status, 404)
  assert.equal(error.code, 'artifact_not_found')
})

/* ---------------------------------------------------------------- wizard */

test('contract §7: wizard GET/PUT/state/apply path & method', async () => {
  const recorder = createContractFetch({
    'GET /api/calibration/wizard': () => jsonResponse(200, wizardBody()),
    'PUT /api/calibration/wizard': () => jsonResponse(200, { ...wizardBody(), saved: true }),
    'GET /api/calibration/wizard/state': () => jsonResponse(200, wizardStateBody()),
    'POST /api/calibration/wizard/apply': () =>
      jsonResponse(200, { applied: true, job_id: JOB_ID, applied_version: 4, graph_id: 'graph-new', sha256: PLAN_SHA })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-wiz' }))
  await api.getWizard()
  await api.putWizard({ ...wizardConfig(), measurement: { baseline_upload_id: BASELINE_ID, applied_upload_id: APPLIED_ID, plan_sha: PLAN_SHA } } as never)
  const state = await api.getWizardState()
  assert.equal(state.steps.length, 8)
  const applied = await api.applyWizard(JOB_ID)
  assert.equal(applied.kind, 'applied')

  assert.deepEqual(
    recorder.calls.map((call) => call.method + ' ' + call.url),
    [
      'GET /api/calibration/wizard',
      'PUT /api/calibration/wizard',
      'GET /api/calibration/wizard/state',
      'POST /api/calibration/wizard/apply'
    ]
  )
  assert.deepEqual(bodyOf(recorder.calls[3]), { job_id: JOB_ID })
})

test('contract §7: wizard apply without job_id sends {} (daemon applies its last succeeded job)', async () => {
  const recorder = createContractFetch({
    'POST /api/calibration/wizard/apply': () =>
      jsonResponse(200, { applied: true, job_id: JOB_ID, applied_version: 4, graph_id: 'graph-new' })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-wiz' }))
  await api.applyWizard()
  assert.deepEqual(bodyOf(recorder.calls[0]), {})
})

test('contract §7: wizard 503 not_wired is displayed as NOT_AVAILABLE with the daemon reason', async () => {
  const recorder = createContractFetch({
    'POST /api/calibration/wizard/apply': () =>
      jsonResponse(503, { error: { code: 'not_wired', message: 'calibration job backend not wired' } })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const error = await captureError(() => api.applyWizard())
  const info = classifyCalibrationError(error)
  assert.equal(info.category, 'not_wired')
  assert.equal(info.detail, 'calibration job backend not wired')
})

test('contract §7: wizard apply without an applicable job is 409 no_applicable_job (not a fake config draft)', async () => {
  const recorder = createContractFetch({
    'POST /api/calibration/wizard/apply': () =>
      jsonResponse(409, { error: { code: 'no_applicable_job', message: 'no calibration job to apply' } })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const error = await captureError(() => api.applyWizard())
  assert.equal(error.status, 409)
  assert.equal(error.code, 'no_applicable_job')
  assert.equal(classifyCalibrationError(error).category, 'conflict')
})

test('contract §7: 401 on a polling read (GET /jobs/{id}) triggers the shared login hook exactly once', async () => {
  const recorder = createContractFetch({
    ['GET /api/calibration/jobs/' + JOB_ID]: () =>
      jsonResponse(401, { error: { code: 'unauthorized', message: 'expired' } })
  })
  const seen: string[] = []
  const api = createCalibrationApi(
    new ApiClient({ fetchImpl: recorder.fetch, onUnauthorized: (error) => seen.push(error.code) })
  )
  await captureError(() => api.getJob(JOB_ID))
  assert.deepEqual(seen, ['unauthorized'])
})
