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
  parseApplyOutcome,
  parseArtifactDownload,
  parseCancelResult,
  parseContentDispositionFilename,
  parseCreateJobResponse,
  parseJobResponse,
  parseJobsResponse,
  parseRollbackOutcome,
  parseUploadResponse,
  parseWizardApplyOutcome,
  parseWizardResponse,
  parseWizardStateResponse,
  resultTruth,
  stepDisplayState,
  stepKey,
  verdictTone
} from '../src/api/calibration.ts'
import { createRecordingFetch, jsonResponse, textResponse } from '../scripts/mock-test-support.mjs'

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
const BASELINE_ID = 'b'.repeat(64)
const APPLIED_ID = 'c'.repeat(64)

/**
 * 兼顾合同 §7（job_id/state）与 daemon 实际（id/status）：
 * 列表 fixture 用 canonical job_id/state + 【加法】字段，另有一条用实际 id/status。
 */
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
    reasons: { engine: 'engine is not reported by this daemon' },
    jobs: [
      {
        // canonical（docs/API.md §7 v1.1 裁决 H）：id/status。
        id: 'job-1',
        kind: 'single_point_offline',
        status: 'running',
        verdict: null,
        created_at: '2026-11-26T06:00:00.000Z',
        started_at: '2026-11-26T06:00:01.000Z',
        finished_at: null,
        duration_ms: null,
        error: null,
        reason: null,
        created_unix: 1764136800,
        updated_unix: 1764136801,
        progress: 0.25
      },
      {
        // 旧文档别名 job_id/state：只作防御性兼容（不该再出现在 canonical 断言里）。
        job_id: 'job-2',
        kind: 'single_point_offline',
        state: 'succeeded',
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
    plan: { sha256: PLAN_SHA, graph_id: 'graph-abc' },
    // 【加法】来源标注：模拟结果不得写成实测。
    measured: null,
    simulated: { predicted_from: 'designer prediction', plan_sha256: PLAN_SHA, graph_id: 'graph-abc' }
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
      step: null,
      apply_state: 'applied',
      applied_version: 4,
      // daemon 实际会带 apply 明细对象（合同 §7 列表里也有 apply?）。
      apply: {
        state: 'applied',
        applied_version: 4,
        previous_version: 3,
        applied_at: '2026-11-26T06:00:10.000Z',
        plan_sha256: PLAN_SHA
      },
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
        {
          name: 'report.json',
          sha256: 'c'.repeat(64),
          bytes: 512,
          content_type: 'application/json',
          downloadable: true
        }
      ],
      inputs: {
        plan_sha: PLAN_SHA,
        baseline_upload_id: BASELINE_ID,
        applied_upload_id: APPLIED_ID,
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
    reasons: {},
    set_fields: ['mic_cal_sha256']
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
      baseline_upload_id: BASELINE_ID,
      applied_upload_id: APPLIED_ID,
      baseline_sha256: 'e'.repeat(64),
      applied_sha256: 'f'.repeat(64)
    },
    snapshot: { version: 3, graph_id: 'graph-old', sha256: PLAN_SHA },
    updated_at: '2026-11-26T06:00:00.000Z',
    reasons: {}
  }
}

/* ------------------------------------------------------------- 响应运行时校验 */

test('parseJobsResponse reads canonical id/status + additive fields and tolerates job_id/state aliases', () => {
  const parsed = parseJobsResponse(jobsBody())
  assert.equal(parsed.schema, CALIBRATION_JOBS_SCHEMA)
  assert.equal(parsed.available, true)
  assert.equal(parsed.active_job_id, 'job-1')
  assert.equal(parsed.jobs.length, 2)
  assert.equal(parsed.jobs[0].id, 'job-1')
  assert.equal(parsed.jobs[0].status, 'running')
  assert.equal(parsed.jobs[0].progress, 0.25)
  assert.equal(parsed.jobs[0].created_unix, 1764136800)
  assert.equal(parsed.jobs[1].id, 'job-2')
  assert.equal(parsed.jobs[1].status, 'succeeded')
  assert.equal(parsed.jobs[1].verdict, 'PASS')
  assert.equal(parsed.jobs[1].progress, null)
  assert.equal(parsed.total, 2)
  assert.equal(parsed.limit, 20)
  assert.equal(parsed.reasons.engine.length > 0, true)

  const withoutAvailable = clone(jobsBody())
  delete withoutAvailable.available
  assert.equal(parseJobsResponse(withoutAvailable).available, true)

  // available=false 是合同 §7 的"未接入"形态：空数组 + reason，不等于"0 个 job"。
  const unavailable = clone(jobsBody())
  unavailable.available = false
  unavailable.reason = 'calibration job backend not wired'
  unavailable.jobs = []
  unavailable.total = 0
  const parsedUnavailable = parseJobsResponse(unavailable)
  assert.equal(parsedUnavailable.available, false)
  assert.equal(parsedUnavailable.reason, 'calibration job backend not wired')
  assert.equal(parsedUnavailable.jobs.length, 0)
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

  const noStatus = clone(jobsBody())
  delete noStatus.jobs[0].status
  assert.equal((await captureError(() => parseJobsResponse(noStatus))).code, 'invalid_response')
})

test('parseJobResponse parses steps/artifacts/result/apply and tolerates additive top-level fields', () => {
  const parsed = parseJobResponse(jobBody())
  assert.equal(parsed.schema, CALIBRATION_JOB_SCHEMA)
  assert.equal(parsed.job.id, 'job-2')
  assert.equal(parsed.job.apply_state, 'applied')
  assert.equal(parsed.job.applied_version, 4)
  assert.equal(parsed.job.apply?.previous_version, 3)
  assert.equal(parsed.job.steps.length, 2)
  assert.equal(parsed.job.steps[1].status, 'running')
  assert.equal(parsed.job.artifacts[0].name, 'report.json')
  assert.equal(parsed.job.artifacts[0].downloadable, true)
  assert.equal(parsed.job.result?.verdict, 'PASS')
  assert.equal(parsed.job.result?.bands.length, 2)
  assert.equal(parsed.job.result?.design?.residual, 'FULL')
  assert.equal(parsed.job.result?.plan?.graph_id, 'graph-abc')
  // 【加法】measured/simulated 如实透传：simulated 非 null，measured 为 null。
  assert.equal(parsed.job.result?.measured, null)
  assert.equal(parsed.job.result?.simulated?.plan_sha256, PLAN_SHA)

  // 详情顶层【加法】result/artifacts：job 内缺省时用顶层回退，不丢数据。
  const topLevel = jobBody()
  delete topLevel.job.result
  delete topLevel.job.artifacts
  topLevel.result = resultBody()
  topLevel.artifacts = [
    { name: 'levels.json', sha256: 'd'.repeat(64), bytes: 10, content_type: 'application/json' }
  ]
  topLevel.ok = true
  const fallback = parseJobResponse(topLevel)
  assert.equal(fallback.job.result?.verdict, 'PASS')
  assert.equal(fallback.job.artifacts[0].name, 'levels.json')

  const pending = clone(jobBody())
  pending.job.status = 'queued'
  pending.job.verdict = null
  pending.job.result = null
  pending.job.steps = []
  pending.job.artifacts = []
  pending.job.apply_state = null
  pending.job.applied_version = null
  pending.job.apply = null
  const parsedPending = parseJobResponse(pending)
  assert.equal(parsedPending.job.result, null)
  assert.equal(parsedPending.job.steps.length, 0)
})

test('parseJobResponse rejects unknown step status / bad enum / bad design residual', async () => {
  const badStep = clone(jobBody())
  badStep.job.steps[0].status = 'done'
  assert.equal((await captureError(() => parseJobResponse(badStep))).code, 'invalid_response')

  const badStepsType = clone(jobBody())
  badStepsType.job.steps = 'compile'
  assert.equal((await captureError(() => parseJobResponse(badStepsType))).code, 'invalid_response')

  const badResidual = clone(jobBody())
  badResidual.job.result.design.residual = 'MOSTLY'
  assert.equal((await captureError(() => parseJobResponse(badResidual))).code, 'invalid_response')

  const badApplyState = clone(jobBody())
  badApplyState.job.apply_state = 'committed'
  assert.equal((await captureError(() => parseJobResponse(badApplyState))).code, 'invalid_response')
})

test('parseWizardResponse validates config/defaults and falls back when defaults are missing', () => {
  const parsed = parseWizardResponse(wizardBody())
  assert.equal(parsed.schema, CALIBRATION_WIZARD_SCHEMA)
  assert.equal(parsed.config.target_mode, 'flat')
  assert.equal(parsed.config.orientation, '90deg')
  assert.equal(parsed.config.single_point.centers_hz.length, 30)
  assert.equal(parsed.defaults.mic_cal_sha256, null)
  assert.equal(parsed.saved, true)
  assert.deepEqual(parsed.set_fields, ['mic_cal_sha256'])

  // 合同 §7 只保证 schema/config/state："..."：defaults 缺失不拒绝。
  const noDefaults = clone(wizardBody())
  delete noDefaults.defaults
  const fallback = parseWizardResponse(noDefaults)
  assert.equal(fallback.defaults.target_mode, 'flat')

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
  assert.equal(parsed.measurement.baseline_upload_id, BASELINE_ID)
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

test('upload/create/cancel responses are parsed with additive tolerance', () => {
  const upload = parseUploadResponse({
    schema: 'khaen-calibration-upload/v1',
    upload_id: BASELINE_ID,
    sha256: BASELINE_ID,
    bytes: 1024,
    created: true
  })
  assert.equal(upload.upload_id, BASELINE_ID)
  assert.equal(upload.created, true)
  assert.equal(upload.schema, 'khaen-calibration-upload/v1')
  // daemon 当前不返回 schema/created：缺失为 null，不拒绝。
  const legacyUpload = parseUploadResponse({ upload_id: BASELINE_ID, sha256: BASELINE_ID, bytes: 10 })
  assert.equal(legacyUpload.schema, null)
  assert.equal(legacyUpload.created, null)

  const created = parseCreateJobResponse({ job_id: 'job-9', location: '/api/calibration/jobs/job-9', ok: true, state: 'queued' })
  assert.equal(created.job_id, 'job-9')
  assert.equal(created.location, '/api/calibration/jobs/job-9')
  assert.equal(created.status, 'queued')
  assert.equal(parseCreateJobResponse({ job_id: 'job-9' }).status, 'queued')
  assert.equal(parseCreateJobResponse({ job_id: 'job-9' }).location, null)

  assert.deepEqual(parseCancelResult(null), { job_id: null, status: null })
  assert.deepEqual(parseCancelResult({ job_id: 'job-9', status: 'cancelled' }), {
    job_id: 'job-9',
    status: 'cancelled'
  })
  // 双 l 是 canonical；单 l 必须被拒绝（不静默接受旧拼写）。
  assert.equal((() => {
    try {
      parseCancelResult({ job_id: 'job-9', status: 'canceled' })
      return 'accepted'
    } catch (error) {
      return (error as ApiError).code
    }
  })(), 'invalid_response')
})

/* ------------------------------------------------------- apply / rollback 回执 */

const SINGLE_APPLY_RECEIPT = {
  applied: true,
  job_id: 'job-2',
  applied_version: 4,
  graph_id: 'graph-abc',
  sha256: PLAN_SHA,
  snapshot: { version: 3, graph_id: 'graph-old', sha256: null },
  ack: { synthetic: false, actual: true },
  readback: { actual_version: 4 },
  applied_at: '2026-11-26T06:00:10.000Z',
  reasons: { 'snapshot.sha256': 'control plane does not expose the running plan sha256' }
}

const STAGED_PREPARE = {
  stage: 'prepare',
  job_id: 'job-2',
  engine_token: 'token-1',
  plan_sha256: PLAN_SHA,
  previous_version: 3,
  graph_id: 'graph-abc',
  readback: null,
  reasons: {}
}

const STAGED_COMMIT_APPLY = {
  stage: 'commit',
  applied: true,
  job_id: 'job-2',
  applied_version: 5,
  previous_version: 3,
  graph_id: 'graph-abc',
  sha256: PLAN_SHA,
  readback: { actual_version: 5 },
  reasons: {}
}

const STAGED_ABORT = {
  stage: 'abort',
  aborted: true,
  job_id: 'job-2',
  plan_sha256: PLAN_SHA,
  control: { aborted: 'token-1' }
}

test('parseApplyOutcome covers single-shot, staged prepare/commit/abort', () => {
  const single = parseApplyOutcome(SINGLE_APPLY_RECEIPT)
  assert.equal(single.kind, 'applied')
  if (single.kind === 'applied') {
    assert.equal(single.stage, 'single')
    assert.equal(single.applied_version, 4)
    assert.equal(single.sha256, PLAN_SHA)
    assert.equal(single.snapshot?.version, 3)
    assert.equal(single.ack?.actual, true)
    assert.equal(single.reasons['snapshot.sha256'].length > 0, true)
  }

  const prepare = parseApplyOutcome(STAGED_PREPARE)
  assert.equal(prepare.kind, 'prepare')
  if (prepare.kind === 'prepare') {
    assert.equal(prepare.engine_token, 'token-1')
    assert.equal(prepare.previous_version, 3)
    assert.equal(prepare.plan_sha256, PLAN_SHA)
  }

  const commit = parseApplyOutcome(STAGED_COMMIT_APPLY)
  assert.equal(commit.kind, 'applied')
  if (commit.kind === 'applied') {
    assert.equal(commit.stage, 'commit')
    assert.equal(commit.previous_version, 3)
  }

  const abort = parseApplyOutcome(STAGED_ABORT)
  assert.equal(abort.kind, 'abort')
  if (abort.kind === 'abort') {
    assert.equal(abort.aborted, true)
    assert.equal(abort.plan_sha256, PLAN_SHA)
  }
})

test('parseApplyOutcome refuses fake success / prepare without token', async () => {
  const appliedFalse = { ...SINGLE_APPLY_RECEIPT, applied: false }
  assert.equal((await captureError(() => parseApplyOutcome(appliedFalse))).code, 'invalid_response')

  const prepareNoToken = { ...STAGED_PREPARE }
  delete prepareNoToken.engine_token
  assert.equal((await captureError(() => parseApplyOutcome(prepareNoToken))).code, 'invalid_response')

  const abortFalse = { ...STAGED_ABORT, aborted: false }
  assert.equal((await captureError(() => parseApplyOutcome(abortFalse))).code, 'invalid_response')

  assert.equal((await captureError(() => parseApplyOutcome(null))).code, 'invalid_response')
})

test('parseRollbackOutcome covers single-shot, staged prepare/commit/abort', () => {
  const single = parseRollbackOutcome({
    rolled_back: true,
    job_id: 'job-2',
    restored_version: 3,
    graph_id: 'graph-old',
    sha256: null,
    readback: null,
    rolled_back_at: '2026-11-26T06:00:11.000Z'
  })
  assert.equal(single.kind, 'rolled_back')
  if (single.kind === 'rolled_back') {
    assert.equal(single.stage, 'single')
    assert.equal(single.restored_version, 3)
    assert.equal(single.sha256, null)
  }

  const prepare = parseRollbackOutcome({
    stage: 'prepare',
    job_id: 'job-2',
    engine_token: 'token-2',
    target_version: 3,
    previous_version: 4,
    reasons: {}
  })
  assert.equal(prepare.kind, 'prepare')
  if (prepare.kind === 'prepare') {
    assert.equal(prepare.target_version, 3)
    assert.equal(prepare.engine_token, 'token-2')
  }

  const commit = parseRollbackOutcome({
    stage: 'commit',
    rolled_back: true,
    job_id: 'job-2',
    restored_version: 3,
    applied_version: 3,
    graph_id: 'graph-old',
    reasons: {}
  })
  assert.equal(commit.kind, 'rolled_back')
  if (commit.kind === 'rolled_back') {
    assert.equal(commit.stage, 'commit')
    assert.equal(commit.applied_version, 3)
  }

  const abort = parseRollbackOutcome({ stage: 'abort', aborted: true, job_id: 'job-2' })
  assert.equal(abort.kind, 'abort')

  assert.equal(
    (() => {
      try {
        parseRollbackOutcome({ rolled_back: false, restored_version: 3 })
        return 'accepted'
      } catch (error) {
        return (error as ApiError).code
      }
    })(),
    'invalid_response'
  )
})

test('parseWizardApplyOutcome reads the canonical apply receipt and only defensively tolerates {job_id,location}', () => {
  // canonical（docs/API.md §7 裁决 H）：wizard/apply 返回 apply 回执。
  const applied = parseWizardApplyOutcome(SINGLE_APPLY_RECEIPT)
  assert.equal(applied.kind, 'applied')

  // 旧文档形状只作防御性兼容：不因为形状旧就整页失败，但正常路径不会走。
  const draft = parseWizardApplyOutcome({
    job_id: null,
    location: '/api/calibration/wizard',
    config: { saved: true }
  })
  assert.equal(draft.kind, 'job')
  if (draft.kind === 'job') {
    assert.equal(draft.location, '/api/calibration/wizard')
  }

  assert.equal(
    (() => {
      try {
        parseWizardApplyOutcome({ nonsense: true })
        return 'accepted'
      } catch (error) {
        return (error as ApiError).code
      }
    })(),
    'invalid_response'
  )
})

/* --------------------------------------------------------------- 产物下载 */

test('artifact download parses Content-Disposition filename and sha header', () => {
  assert.equal(parseContentDispositionFilename('attachment; filename="report.json"'), 'report.json')
  assert.equal(
    parseContentDispositionFilename("attachment; filename*=UTF-8''report%20final.json"),
    'report final.json'
  )
  assert.equal(parseContentDispositionFilename(null), null)
  assert.equal(parseContentDispositionFilename('inline'), null)

  const text = '{"ok":true}'
  const bytes = new TextEncoder().encode(text).buffer
  const headers = new Headers({
    'Content-Type': 'application/json',
    'Content-Disposition': 'attachment; filename="report.json"',
    'x-khaen-artifact-sha256': 'e'.repeat(64)
  })
  const download = parseArtifactDownload('job-2', 'report.json', { status: 200, headers, bytes })
  assert.equal(download.filename, 'report.json')
  assert.equal(download.from_content_disposition, true)
  assert.equal(download.content_type, 'application/json')
  assert.equal(download.sha256, 'e'.repeat(64))
  assert.equal(download.text, text)
  assert.equal(download.byte_length, 11)
  assert.equal(new TextDecoder().decode(download.data), text)

  // 二进制产物：无损字节（不是 UTF-8 替换字符）。
  const binary = new Uint8Array([0xff, 0x00, 0x80, 0x01])
  const fallback = parseArtifactDownload('job-2', 'plan.bin', {
    status: 200,
    headers: new Headers({ 'Content-Type': 'application/octet-stream' }),
    bytes: binary.buffer
  })
  assert.equal(fallback.filename, 'plan.bin')
  assert.equal(fallback.from_content_disposition, false)
  assert.equal(fallback.byte_length, 4)
  assert.deepEqual([...new Uint8Array(fallback.data)], [0xff, 0x00, 0x80, 0x01])
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

test('resultTruth marks simulated results as demo and measured as actual (no fake measured)', () => {
  const simulated = resultBody()
  const measured = { ...resultBody(), measured: { baseline_upload_id: BASELINE_ID, applied_upload_id: APPLIED_ID, sample_rate_hz: 48000, source: 'upload' } }
  assert.equal(resultTruth(simulated, null), 'demo')
  assert.equal(resultTruth(measured, null), 'actual')
  assert.equal(resultTruth(null, 'synthetic'), 'demo')
  assert.equal(resultTruth(null, null), 'actual')
})

/* ---------------------------------------------------------------- 错误归类 */

test('classifyCalibrationError maps stable codes to UI categories', () => {
  const notWired = classifyCalibrationError(new ApiError({ status: 503, code: 'not_wired', message: 'pending' }))
  assert.equal(notWired.category, 'not_wired')
  assert.equal(notWired.i18nKey, 'error.calibration.notWired')
  assert.equal(notWired.retryable, false)

  assert.equal(
    classifyCalibrationError(new ApiError({ status: 501, code: 'unsupported', message: 'x' })).category,
    'not_wired'
  )
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
    classifyCalibrationError(new ApiError({ status: 409, code: 'engine_token_invalid', message: 'token' })).category,
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

test('listJobs/getJob/createJob build correct requests and create body is canonical §7', async () => {
  const recorder = createRecordingFetch((call) => {
    if (call.method === 'GET' && call.url.startsWith('/api/calibration/jobs?')) {
      return jsonResponse(200, jobsBody())
    }
    if (call.method === 'GET') {
      return jsonResponse(200, jobBody())
    }
    return jsonResponse(202, { job_id: 'job-new', location: '/api/calibration/jobs/job-new', ok: true, state: 'queued' })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-1' }))

  const list = await api.listJobs({ limit: 10, offset: 20 })
  assert.equal(list.jobs.length, 2)
  assert.match(recorder.calls[0].url, /limit=10/)
  assert.match(recorder.calls[0].url, /offset=20/)
  assert.equal(recorder.calls[0].method, 'GET')
  assert.equal(recorder.calls[0].credentials, 'include')

  const detail = await api.getJob('job/2')
  assert.equal(detail.job.id, 'job-2')
  assert.equal(recorder.calls[1].url, '/api/calibration/jobs/job%2F2')
  assert.equal(recorder.calls[1].method, 'GET')

  const created = await api.createJob({
    kind: 'single_point_offline',
    inputs: {
      plan_sha: PLAN_SHA,
      baseline_upload_id: BASELINE_ID,
      applied_upload_id: APPLIED_ID,
      centers_hz: [100, 1000],
      tolerance_db: 2.0,
      min_bands: 2
    }
  })
  assert.equal(created.job_id, 'job-new')
  assert.equal(created.location, '/api/calibration/jobs/job-new')
  assert.equal(created.status, 'queued')
  const createCall = recorder.calls[2]
  assert.equal(createCall.method, 'POST')
  assert.equal(createCall.url, '/api/calibration/jobs')
  assert.equal(createCall.headers['Content-Type'], 'application/json')
  assert.equal(createCall.headers['X-KHAEN-CSRF'], 'csrf-1')
  // 合同 §7 canonical 请求体：只允许 kind + 六个 inputs 字段（离线 job 无 repeats/drive_level_dbfs/holdout_points）。
  assert.deepEqual(JSON.parse(String(createCall.body)), {
    kind: 'single_point_offline',
    inputs: {
      plan_sha: PLAN_SHA,
      baseline_upload_id: BASELINE_ID,
      applied_upload_id: APPLIED_ID,
      centers_hz: [100, 1000],
      tolerance_db: 2.0,
      min_bands: 2
    }
  })
})

test('cancel posts to /jobs/{id}/cancel without a body and parses cancelled', async () => {
  const recorder = createRecordingFetch(() => jsonResponse(200, { job_id: 'job/1', status: 'cancelled' }))
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-1' }))
  const cancelled = await api.cancelJob('job/1')
  assert.equal(cancelled.status, 'cancelled')
  assert.equal(cancelled.job_id, 'job/1')
  const call = recorder.calls[0]
  assert.equal(call.method, 'POST')
  assert.equal(call.url, '/api/calibration/jobs/job%2F1/cancel')
  assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-1')
  assert.equal(call.body, undefined)
})

test('apply single-shot posts {} to /jobs/{id}/apply; staged bodies carry stage/engine_token', async () => {
  const recorder = createRecordingFetch((call) => {
    const body = call.body === undefined ? null : JSON.parse(String(call.body))
    if (body !== null && body.stage === 'prepare') {
      return jsonResponse(200, STAGED_PREPARE)
    }
    if (body !== null && body.stage === 'commit') {
      return jsonResponse(200, STAGED_COMMIT_APPLY)
    }
    if (body !== null && body.stage === 'abort') {
      return jsonResponse(200, STAGED_ABORT)
    }
    return jsonResponse(200, SINGLE_APPLY_RECEIPT)
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-9' }))

  const single = await api.applyJob('job/2')
  assert.equal(single.kind, 'applied')
  assert.equal(recorder.calls[0].url, '/api/calibration/jobs/job%2F2/apply')
  assert.equal(recorder.calls[0].method, 'POST')
  assert.deepEqual(JSON.parse(String(recorder.calls[0].body)), {})
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], 'csrf-9')

  const prepare = await api.applyJob('job/2', { stage: 'prepare' })
  assert.equal(prepare.kind, 'prepare')
  assert.deepEqual(JSON.parse(String(recorder.calls[1].body)), { stage: 'prepare' })

  const commit = await api.applyJob('job/2', { stage: 'commit', engine_token: 'token-1' })
  assert.equal(commit.kind, 'applied')
  assert.deepEqual(JSON.parse(String(recorder.calls[2].body)), { stage: 'commit', engine_token: 'token-1' })

  const abort = await api.applyJob('job/2', { stage: 'abort', engine_token: 'token-1' })
  assert.equal(abort.kind, 'abort')
  assert.deepEqual(JSON.parse(String(recorder.calls[3].body)), { stage: 'abort', engine_token: 'token-1' })
})

test('rollback single-shot posts {} and staged bodies to /jobs/{id}/rollback', async () => {
  const recorder = createRecordingFetch((call) => {
    const body = call.body === undefined ? null : JSON.parse(String(call.body))
    if (body !== null && body.stage === 'prepare') {
      return jsonResponse(200, {
        stage: 'prepare',
        job_id: 'job-2',
        engine_token: 'token-r',
        target_version: 3,
        previous_version: 4,
        reasons: {}
      })
    }
    if (body !== null && body.stage === 'commit') {
      return jsonResponse(200, {
        stage: 'commit',
        rolled_back: true,
        job_id: 'job-2',
        restored_version: 3,
        applied_version: 3,
        graph_id: 'graph-old',
        reasons: {}
      })
    }
    if (body !== null && body.stage === 'abort') {
      return jsonResponse(200, { stage: 'abort', aborted: true, job_id: 'job-2' })
    }
    return jsonResponse(200, {
      rolled_back: true,
      job_id: 'job-2',
      restored_version: 3,
      graph_id: 'graph-old',
      sha256: null,
      readback: null,
      rolled_back_at: null
    })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-r' }))

  const single = await api.rollbackJob('job-2')
  assert.equal(single.kind, 'rolled_back')
  assert.equal(recorder.calls[0].url, '/api/calibration/jobs/job-2/rollback')
  assert.deepEqual(JSON.parse(String(recorder.calls[0].body)), {})

  const prepare = await api.rollbackJob('job-2', { stage: 'prepare' })
  assert.equal(prepare.kind, 'prepare')
  assert.deepEqual(JSON.parse(String(recorder.calls[1].body)), { stage: 'prepare' })

  const commit = await api.rollbackJob('job-2', { stage: 'commit', engine_token: 'token-r' })
  assert.equal(commit.kind, 'rolled_back')
  assert.deepEqual(JSON.parse(String(recorder.calls[2].body)), { stage: 'commit', engine_token: 'token-r' })
})

test('upload posts raw bytes with content type/filename/role headers and CSRF', async () => {
  const recorder = createRecordingFetch(() =>
    jsonResponse(201, {
      schema: 'khaen-calibration-upload/v1',
      upload_id: BASELINE_ID,
      sha256: BASELINE_ID,
      bytes: 44,
      created: true
    })
  )
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-2' }))
  const buffer = new Uint8Array([82, 73, 70, 70]).buffer
  const result = await api.upload(buffer, {
    contentType: 'audio/wav',
    filename: 'baseline.wav',
    role: 'baseline'
  })
  assert.equal(result.upload_id, BASELINE_ID)
  assert.equal(result.created, true)
  const call = recorder.calls[0]
  assert.equal(call.url, '/api/calibration/uploads')
  assert.equal(call.method, 'POST')
  assert.equal(call.headers['Content-Type'], 'audio/wav')
  assert.equal(call.headers['X-KHAEN-Filename'], 'baseline.wav')
  assert.equal(call.headers['X-KHAEN-Upload-Role'], 'baseline')
  assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-2')
  assert.ok(call.body instanceof ArrayBuffer)
  // 原始字节必须原样发送（不是 JSON/base64 包装）。
  assert.deepEqual([...new Uint8Array(call.body as ArrayBuffer)], [82, 73, 70, 70])
})

test('wizard GET/PUT/state/apply build correct requests (apply body {job_id?})', async () => {
  const recorder = createRecordingFetch((call) => {
    if (call.url === '/api/calibration/wizard' && call.method === 'PUT') {
      return jsonResponse(200, wizardBody())
    }
    if (call.url === '/api/calibration/wizard/state') {
      return jsonResponse(200, wizardStateBody())
    }
    if (call.url === '/api/calibration/wizard/apply') {
      return jsonResponse(200, SINGLE_APPLY_RECEIPT)
    }
    return jsonResponse(200, wizardBody())
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-3' }))

  const wizard = await api.getWizard()
  assert.equal(wizard.saved, true)
  assert.equal(recorder.calls[0].url, '/api/calibration/wizard')
  assert.equal(recorder.calls[0].method, 'GET')

  const state = await api.getWizardState()
  assert.equal(state.steps.length, 8)
  assert.equal(recorder.calls[1].url, '/api/calibration/wizard/state')

  const saved = await api.putWizard({
    ...wizard.config,
    measurement: { baseline_upload_id: BASELINE_ID, applied_upload_id: APPLIED_ID, plan_sha: PLAN_SHA }
  })
  assert.equal(saved.saved, true)
  const putCall = recorder.calls[2]
  assert.equal(putCall.method, 'PUT')
  assert.equal(putCall.url, '/api/calibration/wizard')
  assert.equal(putCall.headers['X-KHAEN-CSRF'], 'csrf-3')
  assert.equal(JSON.parse(String(putCall.body)).measurement.baseline_upload_id, BASELINE_ID)

  const applied = await api.applyWizard()
  assert.equal(applied.kind, 'applied')
  assert.equal(recorder.calls[3].url, '/api/calibration/wizard/apply')
  assert.equal(recorder.calls[3].method, 'POST')
  assert.deepEqual(JSON.parse(String(recorder.calls[3].body)), {})

  await api.applyWizard('job-2')
  assert.deepEqual(JSON.parse(String(recorder.calls[4].body)), { job_id: 'job-2' })
})

test('downloadArtifact uses authenticated getRaw and parses Content-Disposition', async () => {
  const recorder = createRecordingFetch((call) => {
    if (call.url.includes('/nope.bin')) {
      return jsonResponse(404, { error: { code: 'artifact_not_found', message: 'unknown artifact' } })
    }
    return textResponse(200, '{"job_id":"job-2"}', {
      'Content-Type': 'application/json',
      'Content-Disposition': 'attachment; filename="report.json"',
      'x-khaen-artifact-sha256': 'f'.repeat(64)
    })
  })
  const api = createCalibrationApi(new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-4' }))
  const download = await api.downloadArtifact('job/2', 'report.json')
  assert.equal(download.filename, 'report.json')
  assert.equal(download.text, '{"job_id":"job-2"}')
  assert.equal(download.sha256, 'f'.repeat(64))
  const call = recorder.calls[0]
  assert.equal(call.method, 'GET')
  assert.equal(call.url, '/api/calibration/jobs/job%2F2/artifacts/report.json')
  assert.equal(call.credentials, 'include')

  const error = await captureError(() => api.downloadArtifact('job/2', 'nope.bin'))
  assert.equal(error.status, 404)
  assert.equal(error.code, 'artifact_not_found')
})

test('calibration 401 reaches the shared unauthorized hook; 503 not_wired never becomes fake data', async () => {
  const seen: string[] = []
  const unauthorized = createRecordingFetch(() =>
    jsonResponse(401, { error: { code: 'not_authenticated', message: 'auth required' } })
  )
  const api = createCalibrationApi(
    new ApiClient({ fetchImpl: unauthorized.fetch, onUnauthorized: (error) => seen.push(error.code) })
  )
  const unauthenticated = await captureError(() => api.listJobs())
  assert.equal(unauthenticated.status, 401)
  assert.deepEqual(seen, ['not_authenticated'])

  const notWired = createRecordingFetch(() =>
    jsonResponse(503, { error: { code: 'not_wired', message: 'calibration job backend not wired' } })
  )
  const pendingApi = createCalibrationApi(new ApiClient({ fetchImpl: notWired.fetch }))
  const error = await captureError(() => pendingApi.getJob('job-1'))
  assert.equal(error.status, 503)
  assert.equal(error.code, 'not_wired')
  assert.equal(classifyCalibrationError(error).category, 'not_wired')

  const badRequest = createRecordingFetch(() =>
    jsonResponse(400, { error: { code: 'invalid_request', message: 'plan_sha is required', details: { field: 'inputs.plan_sha' } } })
  )
  const strictApi = createCalibrationApi(new ApiClient({ fetchImpl: badRequest.fetch }))
  const invalid = await captureError(() =>
    strictApi.createJob({
      kind: 'single_point_offline',
      inputs: {
        plan_sha: '',
        baseline_upload_id: BASELINE_ID,
        applied_upload_id: APPLIED_ID,
        centers_hz: [100],
        tolerance_db: 2,
        min_bands: 1
      }
    })
  )
  assert.equal(invalid.status, 400)
  assert.equal(invalid.code, 'invalid_request')
  assert.equal(classifyCalibrationError(invalid).category, 'invalid_request')
})
