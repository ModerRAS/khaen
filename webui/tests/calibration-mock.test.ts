import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { ApiClient, isApiError } from '../src/api/client.ts'
import { createAuthApi } from '../src/api/auth.ts'
import { createCalibrationApi, DEFAULT_CENTERS_HZ } from '../src/api/calibration.ts'
import { createInitialSessionState, createSessionController } from '../src/stores/session.ts'
import { createMockAuthServer } from '../scripts/mock-auth-server.mjs'
import { CALIBRATION_FIXTURES } from '../scripts/calibration-fixtures.mjs'

const PASSWORD = 'correct-horse-battery-staple'
const PLAN_SHA = 'a'.repeat(64)

/** Node fetch has no cookie jar: emulate the browser's HttpOnly cookie store. */
function createJarFetch() {
  let cookie = null
  const fetchImpl = async (input, init = {}) => {
    const headers = new Headers(init.headers ?? {})
    if (cookie !== null) {
      headers.set('Cookie', cookie)
    }
    const response = await fetch(input, { ...init, headers })
    const setCookies = typeof response.headers.getSetCookie === 'function' ? response.headers.getSetCookie() : []
    for (const raw of setCookies) {
      const pair = raw.split(';')[0]
      const index = pair.indexOf('=')
      if (index < 0) {
        continue
      }
      const name = pair.slice(0, index)
      const value = pair.slice(index + 1)
      if (name !== 'khaen_session') {
        continue
      }
      cookie = value.length > 0 ? pair : null
    }
    return response
  }
  return { fetch: fetchImpl, cookie: () => cookie }
}

function createHarness(baseUrl) {
  const jar = createJarFetch()
  const state = createInitialSessionState()
  const notifications = []
  const client = new ApiClient({
    baseUrl,
    fetchImpl: jar.fetch,
    csrfToken: () => state.csrfToken,
    onUnauthorized: () => notifications.push('client-401')
  })
  const session = createSessionController(state, { auth: createAuthApi(client) })
  const calibration = createCalibrationApi(client)
  return { jar, state, client, session, calibration, notifications }
}

async function withMock(run, options = {}) {
  const mock = createMockAuthServer(options)
  const address = await mock.listen()
  try {
    await run({ mock, baseUrl: address.baseUrl })
  } finally {
    await mock.close()
  }
}

async function setupAuth(h) {
  assert.equal(await h.session.ensureSession(), 'unauthenticated')
  const result = await h.session.setup(PASSWORD, PASSWORD)
  assert.deepEqual(result, { ok: true })
  assert.equal(h.state.authenticated, true)
}

async function expectApiError(run) {
  try {
    await run()
  } catch (error) {
    assert.ok(isApiError(error), 'expected an ApiError, got: ' + String(error))
    return error
  }
  return assert.fail('expected the request to reject')
}

/** 构造结构合法的 48k/16-bit/mono WAV（44 字节头 + 静音帧）。 */
function buildWav(frames = 4800): Uint8Array {
  const dataBytes = frames * 2
  const bytes = new Uint8Array(44 + dataBytes)
  const view = new DataView(bytes.buffer)
  const write = (offset, text) => {
    for (let index = 0; index < text.length; index += 1) {
      bytes[offset + index] = text.charCodeAt(index)
    }
  }
  write(0, 'RIFF')
  view.setUint32(4, 36 + dataBytes, true)
  write(8, 'WAVE')
  write(12, 'fmt ')
  view.setUint32(16, 16, true)
  view.setUint16(20, 1, true)
  view.setUint16(22, 1, true)
  view.setUint32(24, 48000, true)
  view.setUint32(28, 96000, true)
  view.setUint16(32, 2, true)
  view.setUint16(34, 16, true)
  write(36, 'data')
  view.setUint32(40, dataBytes, true)
  return bytes
}

function arrayBufferOf(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer
}

function sha256Of(text: string): string {
  return createHash('sha256').update(text).digest('hex')
}

async function saveWizardSettings(h, overrides = {}) {
  const current = await h.calibration.getWizard()
  const saved = await h.calibration.putWizard({
    ...current.config,
    mic_cal_sha256: 'd'.repeat(64),
    mic_cal_filename: 'umik-1_90deg.txt',
    orientation: '90deg',
    target_mode: 'flat',
    ...overrides
  })
  assert.equal(saved.saved, true)
  return saved
}

async function uploadMeasurements(h) {
  const baseline = await h.calibration.upload(arrayBufferOf(buildWav(4800)), {
    contentType: 'audio/wav',
    filename: 'baseline.wav',
    role: 'baseline'
  })
  const applied = await h.calibration.upload(arrayBufferOf(buildWav(4800)), {
    contentType: 'audio/wav',
    filename: 'applied.wav',
    role: 'applied'
  })
  return { baseline, applied }
}

async function prepareWizardThroughStep5(h) {
  await saveWizardSettings(h)
  const uploads = await uploadMeasurements(h)
  await h.calibration.putWizard({
    ...(await h.calibration.getWizard()).config,
    measurement: {
      baseline_upload_id: uploads.baseline.upload_id,
      applied_upload_id: uploads.applied.upload_id,
      plan_sha: PLAN_SHA
    }
  })
  return uploads
}

/* ------------------------------------------------------------------ 鉴权 */

test('mock calibration: anonymous requests are 401 and the shared hook fires', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    const error = await expectApiError(() => h.calibration.listJobs())
    assert.equal(error.status, 401)
    assert.equal(error.code, 'not_authenticated')
    assert.deepEqual(h.notifications, ['client-401'])
  })
})

test('mock calibration: session-bound writes without CSRF are rejected with 403', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await setupAuth(h)
    const noCsrf = createCalibrationApi(new ApiClient({ baseUrl, fetchImpl: h.jar.fetch }))
    const config = (await h.calibration.getWizard()).config
    const error = await expectApiError(() => noCsrf.putWizard(config))
    assert.equal(error.status, 403)
    assert.equal(error.code, 'csrf_invalid')
  })
})

/* ---------------------------------------------------------------- uploads */

test('mock calibration uploads: content-addressed, idempotent, type/size validated', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await setupAuth(h)
    const wav = buildWav(4800)
    const first = await h.calibration.upload(arrayBufferOf(wav), {
      contentType: 'audio/wav',
      filename: 'baseline.wav',
      role: 'baseline'
    })
    assert.match(first.sha256, /^[0-9a-f]{64}$/)
    assert.equal(first.upload_id, first.sha256)
    assert.equal(first.bytes, wav.byteLength)

    const second = await h.calibration.upload(arrayBufferOf(wav), { contentType: 'audio/wav' })
    assert.equal(second.upload_id, first.upload_id)
    assert.equal(mock.calibration.uploads.length, 1)

    const wrongType = await expectApiError(() =>
      h.calibration.upload(arrayBufferOf(wav), { contentType: 'text/plain' })
    )
    assert.equal(wrongType.status, 415)
    assert.equal(wrongType.code, 'unsupported_format')

    const empty = await expectApiError(() =>
      h.calibration.upload(new ArrayBuffer(0), { contentType: 'application/octet-stream' })
    )
    assert.equal(empty.status, 400)
    assert.equal(empty.code, 'invalid_upload')
  })
})

test('mock calibration uploads: oversized body is 413 upload_too_large', async () => {
  await withMock(
    async ({ baseUrl }) => {
      const h = createHarness(baseUrl)
      await setupAuth(h)
      const error = await expectApiError(() =>
        h.calibration.upload(arrayBufferOf(new Uint8Array(100)), { contentType: 'audio/wav' })
      )
      assert.equal(error.status, 413)
      assert.equal(error.code, 'upload_too_large')
    },
    { maxUploadBytes: 32 }
  )
})

/* ------------------------------------------------------------------ wizard */

test('mock calibration wizard: defaults -> saved config -> gates advance', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await setupAuth(h)

    const wizard = await h.calibration.getWizard()
    assert.equal(wizard.saved, false)
    assert.equal(wizard.config.mic_cal_sha256, null)
    assert.equal(wizard.defaults.target_mode, 'flat')
    assert.equal(wizard.defaults.single_point.centers_hz.length, 30)

    const initial = await h.calibration.getWizardState()
    assert.equal(initial.steps[0].status, 'passed') // /api/status present fixture
    assert.equal(initial.steps[1].status, 'not_available')
    assert.equal(initial.steps[1].task, 'M4')
    assert.equal(initial.steps[2].status, 'blocked')
    assert.equal(initial.current_step, 3)

    const saved = await saveWizardSettings(h)
    assert.equal(saved.saved, true)
    assert.equal(saved.config.orientation, '90deg')

    const afterSave = await h.calibration.getWizardState()
    assert.equal(afterSave.steps[2].status, 'passed')
    assert.equal(afterSave.steps[3].status, 'passed')
    assert.equal(afterSave.steps[4].status, 'blocked')
    assert.equal(afterSave.current_step, 5)

    // 未知字段 / 非法枚举 -> 400 invalid_request（不静默忽略）。
    const unknownField = await expectApiError(() =>
      h.calibration.putWizard({ ...saved.config, surprise: true } as never)
    )
    assert.equal(unknownField.status, 400)
    assert.equal(unknownField.code, 'invalid_request')

    const badOrientation = await expectApiError(() =>
      h.calibration.putWizard({ ...saved.config, orientation: 'free' } as never)
    )
    assert.equal(badOrientation.status, 400)
    assert.equal(badOrientation.code, 'invalid_request')

    const badTarget = await expectApiError(() =>
      h.calibration.putWizard({ ...saved.config, target_mode: 'single_point' } as never)
    )
    assert.equal(badTarget.status, 400)
  })
})

test('mock calibration wizard: measurement draft survives a page refresh', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await setupAuth(h)
    await saveWizardSettings(h)
    const uploads = await uploadMeasurements(h)

    await h.calibration.putWizard({
      ...(await h.calibration.getWizard()).config,
      measurement: {
        baseline_upload_id: uploads.baseline.upload_id,
        applied_upload_id: uploads.applied.upload_id,
        plan_sha: PLAN_SHA
      }
    })

    // 模拟刷新：新 client 复用同一 cookie jar（浏览器刷新后仍持有 HttpOnly cookie）。
    const refreshed = createCalibrationApi(new ApiClient({ baseUrl, fetchImpl: h.jar.fetch }))
    const state = await refreshed.getWizardState()
    assert.equal(state.measurement.baseline_upload_id, uploads.baseline.upload_id)
    assert.equal(state.measurement.applied_upload_id, uploads.applied.upload_id)
    assert.equal(state.measurement.baseline_sha256, uploads.baseline.sha256)
    assert.equal(state.steps[4].status, 'passed')
    assert.equal(state.current_step, 6)
  })
})

/* --------------------------------------------------------------------- job */

test('mock calibration job: create -> poll -> succeeded -> artifacts -> apply -> rollback', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await setupAuth(h)
    const uploads = await prepareWizardThroughStep5(h)

    const created = await h.calibration.createJob({
      kind: 'single_point_offline',
      inputs: {
        plan_sha: PLAN_SHA,
        baseline_upload_id: uploads.baseline.upload_id,
        applied_upload_id: uploads.applied.upload_id,
        centers_hz: [...DEFAULT_CENTERS_HZ],
        tolerance_db: 2.0,
        min_bands: 3
      }
    })
    assert.match(created.job_id, /^job-mock-/)

    const busy = await expectApiError(() =>
      h.calibration.createJob({
        kind: 'single_point_offline',
        inputs: {
          plan_sha: PLAN_SHA,
          baseline_upload_id: uploads.baseline.upload_id,
          applied_upload_id: uploads.applied.upload_id,
          centers_hz: [100],
          tolerance_db: 2.0,
          min_bands: 3
        }
      })
    )
    assert.equal(busy.status, 409)
    assert.equal(busy.code, 'switch_busy')

    // 每次 GET 详情推进一个 mock 步骤（真实 daemon 由 worker 推进）。
    let detail = await h.calibration.getJob(created.job_id)
    assert.equal(detail.job.status, 'running')
    for (let attempt = 0; attempt < 8 && detail.job.status !== 'succeeded'; attempt += 1) {
      detail = await h.calibration.getJob(created.job_id)
    }
    assert.equal(detail.job.status, 'succeeded')
    assert.equal(detail.job.verdict, 'PASS')
    assert.equal(detail.job.engine, 'synthetic')
    assert.equal(detail.job.steps.length, 4)
    assert.equal(detail.job.steps.every((step) => step.status === 'succeeded'), true)
    assert.equal(detail.job.result?.compared_bands, 30)
    assert.equal(detail.job.result?.design?.residual, 'FULL')
    assert.equal(detail.job.artifacts.length, 3)

    const report = detail.job.artifacts.find((artifact) => artifact.name === 'report.json')
    assert.ok(report)
    // 页面必须走带认证 client 的下载（mock 与 daemon 一致：产物带 Content-Disposition）。
    const download = await h.calibration.downloadArtifact(created.job_id, 'report.json')
    assert.equal(download.filename, 'report.json')
    assert.equal(download.from_content_disposition, true)
    assert.equal(download.content_type, 'application/json')
    assert.equal(sha256Of(download.text), report.sha256)
    assert.equal(download.sha256, report.sha256)
    assert.equal(download.byte_length, download.data.byteLength)
    assert.equal(sha256Of(new TextDecoder().decode(download.data)), report.sha256)
    const parsed = JSON.parse(download.text)
    assert.equal(parsed.job_id, created.job_id)

    const artifactUrl = h.calibration.artifactUrl(created.job_id, 'report.json')
    const response = await h.jar.fetch(baseUrl + artifactUrl)
    assert.equal(response.status, 200)
    assert.equal(response.headers.get('Content-Type'), 'application/json')
    assert.equal(response.headers.get('X-KHAEN-Artifact-SHA256'), report.sha256)

    const missingArtifact = await expectApiError(() => h.calibration.getJob('job-mock-9999'))
    assert.equal(missingArtifact.status, 404)
    assert.equal(missingArtifact.code, 'job_not_found')

    const unknownArtifact = await h.jar.fetch(
      baseUrl + h.calibration.artifactUrl(created.job_id, 'nope.bin')
    )
    assert.equal(unknownArtifact.status, 404)

    // 分析成功后：第 6/7 步通过，第 8 步等待用户 apply。
    const afterJob = await h.calibration.getWizardState()
    assert.equal(afterJob.steps[5].status, 'passed')
    assert.equal(afterJob.steps[6].status, 'passed')
    assert.equal(afterJob.steps[7].status, 'blocked')

    // mock 后端只有旧单发语义：回执必须是"已生效"（不是 prepare 草稿）。
    const applied = await h.calibration.applyJob(created.job_id)
    assert.equal(applied.kind, 'applied')
    if (applied.kind !== 'applied') {
      assert.fail('single-shot apply must return an applied receipt')
    }
    assert.equal(applied.stage, 'single')
    assert.equal(applied.applied_version, 4) // status fixture plan version 3 -> 4
    assert.equal(applied.ack?.synthetic, true)
    assert.equal(applied.snapshot?.version, 3)

    const afterApply = await h.calibration.getWizardState()
    assert.equal(afterApply.steps[7].status, 'passed')
    const appliedDetail = await h.calibration.getJob(created.job_id)
    assert.equal(appliedDetail.job.apply_state, 'applied')
    assert.equal(appliedDetail.job.applied_version, 4)

    const rolledBack = await h.calibration.rollbackJob(created.job_id)
    assert.equal(rolledBack.kind, 'rolled_back')
    if (rolledBack.kind !== 'rolled_back') {
      assert.fail('single-shot rollback must return a rolled_back receipt')
    }
    assert.equal(rolledBack.restored_version, 3)

    const afterRollback = await h.calibration.getWizardState()
    assert.equal(afterRollback.steps[7].status, 'blocked')
    assert.match(String(afterRollback.steps[7].reason), /rolled back/)

    const secondRollback = await expectApiError(() => h.calibration.rollbackJob(created.job_id))
    assert.equal(secondRollback.status, 409)
    assert.equal(secondRollback.code, 'rollback_unavailable')

    assert.equal(mock.calibration.jobs.length, 1)
  })
})

test('mock calibration job: invalid inputs / unknown uploads are rejected with stable codes', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await setupAuth(h)
    await prepareWizardThroughStep5(h)

    const badKind = await expectApiError(() =>
      h.calibration.createJob({ kind: 'multi_point' as never, inputs: {} as never })
    )
    assert.equal(badKind.status, 400)
    assert.equal(badKind.code, 'invalid_request')

    const unknownUpload = await expectApiError(() =>
      h.calibration.createJob({
        kind: 'single_point_offline',
        inputs: {
          plan_sha: PLAN_SHA,
          baseline_upload_id: 'f'.repeat(64),
          applied_upload_id: 'e'.repeat(64),
          centers_hz: [100],
          tolerance_db: 2.0,
          min_bands: 3
        }
      })
    )
    assert.equal(unknownUpload.status, 404)
    assert.equal(unknownUpload.code, 'upload_not_found')

    const uploads = await uploadMeasurements(h)
    const badCenters = await expectApiError(() =>
      h.calibration.createJob({
        kind: 'single_point_offline',
        inputs: {
          plan_sha: PLAN_SHA,
          baseline_upload_id: uploads.baseline.upload_id,
          applied_upload_id: uploads.applied.upload_id,
          centers_hz: [0],
          tolerance_db: 2.0,
          min_bands: 3
        }
      })
    )
    assert.equal(badCenters.status, 400)
    assert.equal(badCenters.code, 'invalid_request')
  })
})

test('mock calibration job: cancel a running job, second cancel is 409', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await setupAuth(h)
    const uploads = await prepareWizardThroughStep5(h)
    const created = await h.calibration.createJob({
      kind: 'single_point_offline',
      inputs: {
        plan_sha: PLAN_SHA,
        baseline_upload_id: uploads.baseline.upload_id,
        applied_upload_id: uploads.applied.upload_id,
        centers_hz: [100, 1000],
        tolerance_db: 2.0,
        min_bands: 3
      }
    })
    await h.calibration.getJob(created.job_id)
    const cancelled = await h.calibration.cancelJob(created.job_id)
    assert.equal(cancelled.status, 'cancelled')
    const detail = await h.calibration.getJob(created.job_id)
    assert.equal(detail.job.status, 'cancelled')
    assert.equal(detail.job.steps.filter((step) => step.status === 'cancelled').length >= 1, true)

    const again = await expectApiError(() => h.calibration.cancelJob(created.job_id))
    assert.equal(again.status, 409)
    assert.equal(again.code, 'job_not_active')
  })
})

/* ---------------------------------------------------------------- fixtures */

test('mock calibration fixtures: success seeds a PASS job and waits for apply', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await setupAuth(h)
    const list = await h.calibration.listJobs()
    assert.equal(list.available, true)
    assert.equal(list.engine, null)
    assert.equal(list.total, 1)
    assert.equal(list.jobs[0].status, 'succeeded')
    assert.equal(list.jobs[0].verdict, 'PASS')

    const wizard = await h.calibration.getWizard()
    assert.equal(wizard.saved, true)
    const state = await h.calibration.getWizardState()
    assert.equal(state.steps[0].status, 'passed')
    assert.equal(state.steps[1].status, 'not_available')
    assert.equal(state.steps[2].status, 'passed')
    assert.equal(state.steps[3].status, 'passed')
    assert.equal(state.steps[4].status, 'passed')
    assert.equal(state.steps[5].status, 'passed')
    assert.equal(state.steps[6].status, 'passed')
    assert.equal(state.steps[7].status, 'blocked')
    assert.equal(state.measurement.baseline_upload_id !== null, true)

    const jobId = list.jobs[0].id
    const applied = await h.calibration.applyJob(jobId)
    assert.equal(applied.kind, 'applied')
    const afterApply = await h.calibration.getWizardState()
    assert.equal(afterApply.steps[7].status, 'passed')

    mock.setCalibrationFixture('empty')
    assert.equal(mock.calibrationFixture, 'empty')
    assert.equal((await h.calibration.listJobs()).total, 0)
  }, { calibrationFixture: 'success' })
})

test('mock calibration fixtures: failed job exposes a stable code and blocks apply', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await setupAuth(h)
    const list = await h.calibration.listJobs()
    assert.equal(list.jobs[0].status, 'failed')
    assert.equal(list.jobs[0].verdict, null)

    const detail = await h.calibration.getJob(list.jobs[0].id)
    assert.equal(detail.job.error?.code, 'E_CLOSED_LOOP_DELTA')
    assert.equal(detail.job.result, null)

    const state = await h.calibration.getWizardState()
    assert.equal(state.steps[5].status, 'blocked')
    assert.match(String(state.steps[5].reason), /E_CLOSED_LOOP_DELTA/)

    const applyError = await expectApiError(() => h.calibration.applyJob(list.jobs[0].id))
    assert.equal(applyError.status, 409)
    assert.equal(applyError.code, 'job_not_succeeded')

    const wizardApplyError = await expectApiError(() => h.calibration.applyWizard())
    assert.equal(wizardApplyError.status, 409)
    assert.equal(wizardApplyError.code, 'job_not_succeeded')
  }, { calibrationFixture: 'failed' })
})

test('mock calibration fixtures: unavailable reports available=false and 503 not_wired honestly', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await setupAuth(h)
    const list = await h.calibration.listJobs()
    assert.equal(list.available, false)
    assert.equal(list.jobs.length, 0)
    assert.match(String(list.reason), /not wired/)

    const createError = await expectApiError(() =>
      h.calibration.createJob({
        kind: 'single_point_offline',
        inputs: {
          plan_sha: PLAN_SHA,
          baseline_upload_id: 'b'.repeat(64),
          applied_upload_id: 'c'.repeat(64),
          centers_hz: [100],
          tolerance_db: 2.0,
          min_bands: 3
        }
      })
    )
    assert.equal(createError.status, 503)
    assert.equal(createError.code, 'not_wired')

    const getError = await expectApiError(() => h.calibration.getJob('job-1'))
    assert.equal(getError.code, 'not_wired')

    // wizard 的真实部分仍可用（与 T36 的默认 NotWired 实现一致）。
    const wizard = await h.calibration.getWizard()
    assert.equal(wizard.saved, false)
    const state = await h.calibration.getWizardState()
    assert.equal(state.steps[0].status, 'passed')
    assert.equal(state.steps[1].status, 'not_available')
    assert.equal(state.steps[5].status, 'blocked')
    assert.equal(state.steps[5].task, 'T70h-b')
  }, { calibrationFixture: 'unavailable' })
})

test('mock calibration fixtures: fixture list and dev helper endpoints', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    assert.deepEqual(CALIBRATION_FIXTURES, ['empty', 'success', 'failed', 'unavailable'])
    assert.throws(() => mock.setCalibrationFixture('nope'), /unknown calibration fixture/)
    const response = await fetch(baseUrl + '/__mock/calibration')
    assert.equal(response.status, 200)
    const body = await response.json()
    assert.equal(body.fixture, 'empty')
    assert.deepEqual(body.fixtures, CALIBRATION_FIXTURES)
  })
})
