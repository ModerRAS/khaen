import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, ApiError } from '../src/api/client.ts'
import {
  MIC_PROFILE_SCHEMA,
  createMicApi,
  parseMicCalUploadResponse,
  parseMicProfileResponse,
  parseMicStatusResponse
} from '../src/api/mic.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

const SHA = '49482d87'.padEnd(64, '0')
const PROFILE = {
  schema: MIC_PROFILE_SCHEMA,
  device: 'hw:CARD=U18dB,DEV=0',
  serial: '718-7696',
  orientation: '90deg',
  cal_sha256: SHA,
  cal_filename: 'umik1_718-7696_90deg.txt',
  cal_path: '/var/lib/khaen/calibration/mics/' + SHA + '.txt',
  parsed: { serno: '718-7696', sens_factor_db: -0.0109999, points: 1000 },
  updated_at: '2026-11-27T09:30:00Z'
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value))
}

async function captureError(run: () => unknown): Promise<ApiError> {
  try {
    await run()
  } catch (error) {
    assert.ok(error instanceof ApiError, 'expected an ApiError, got: ' + String(error))
    return error
  }
  return assert.fail('expected the call to throw') as never
}

function profileBody(profile: unknown = clone(PROFILE)): Record<string, unknown> {
  return { ok: true, schema: MIC_PROFILE_SCHEMA, profile, reasons: [] }
}

function statusBody(profile: unknown = clone(PROFILE)): Record<string, unknown> {
  return { ...profileBody(profile), rt_host: { available: false, reason: 'rt-host calibration bridge is not wired yet' } }
}

test('parseMicProfileResponse keeps the saved profile fields', () => {
  const parsed = parseMicProfileResponse(profileBody())
  assert.equal(parsed.schema, MIC_PROFILE_SCHEMA)
  assert.ok(parsed.profile)
  assert.equal(parsed.profile.device, 'hw:CARD=U18dB,DEV=0')
  assert.equal(parsed.profile.serial, '718-7696')
  assert.equal(parsed.profile.orientation, '90deg')
  assert.equal(parsed.profile.cal_sha256, SHA)
  assert.equal(parsed.profile.parsed.points, 1000)
  assert.equal(parsed.profile.parsed.sens_factor_db, -0.0109999)
  assert.deepEqual(parsed.reasons, [])
})

test('parseMicProfileResponse accepts a missing profile only with a reason', () => {
  const missing = parseMicProfileResponse({ ok: true, schema: MIC_PROFILE_SCHEMA, profile: null, reasons: ['no microphone profile saved yet'] })
  assert.equal(missing.profile, null)
  assert.deepEqual(missing.reasons, ['no microphone profile saved yet'])

  return captureError(() =>
    parseMicProfileResponse({ ok: true, schema: MIC_PROFILE_SCHEMA, profile: null, reasons: [] })
  ).then((error) => {
    assert.equal(error.code, 'invalid_response')
  })
})

test('parseMicProfileResponse rejects malformed profiles instead of faking values', async () => {
  const cases: Array<(body: Record<string, any>) => void> = [
    (body) => { body.profile.orientation = '180deg' },
    (body) => { body.profile.cal_sha256 = 'not-a-sha' },
    (body) => { body.profile.parsed.points = 0 },
    (body) => { body.profile.parsed.sens_factor_db = 'loud' },
    (body) => { body.profile.schema = 'khaen-mic-profile/v2' },
    (body) => { delete body.profile.cal_path },
    (body) => { body.reasons = 'none' }
  ]
  for (const mutate of cases) {
    const body = profileBody()
    mutate(body)
    const error = await captureError(() => parseMicProfileResponse(body))
    assert.equal(error.code, 'invalid_response')
  }
})

test('parseMicCalUploadResponse reads the server parse result', async () => {
  const parsed = parseMicCalUploadResponse({
    ok: true,
    sha256: SHA,
    filename: 'umik.txt',
    bytes: 2048,
    serial: '718-7696',
    parsed: { serno: '718-7696', sens_factor_db: null, points: 512 }
  })
  assert.equal(parsed.sha256, SHA)
  assert.equal(parsed.bytes, 2048)
  assert.equal(parsed.parsed.points, 512)
  assert.equal(parsed.parsed.sens_factor_db, null)

  const error = await captureError(() => parseMicCalUploadResponse({ ok: true, sha256: SHA, filename: 'x', bytes: 0, serial: 's', parsed: { serno: 's', sens_factor_db: null, points: 1 } }))
  assert.equal(error.code, 'invalid_response')
})

test('parseMicStatusResponse requires a reason when rt-host is unavailable', async () => {
  const parsed = parseMicStatusResponse(statusBody())
  assert.equal(parsed.rt_host.available, false)
  assert.equal(parsed.rt_host.reason, 'rt-host calibration bridge is not wired yet')

  const available = parseMicStatusResponse({ ...statusBody(), rt_host: { available: true, reason: null } })
  assert.equal(available.rt_host.available, true)

  const body = statusBody()
  body.rt_host = { available: false, reason: null }
  const error = await captureError(() => parseMicStatusResponse(body))
  assert.equal(error.code, 'invalid_response')
})

test('createMicApi calls the documented endpoints and keeps upload bytes raw', async () => {
  const recorded = createRecordingFetch((call) => {
    if (call.url === '/api/calibration/mic' && call.method === 'GET') {
      return jsonResponse(200, profileBody())
    }
    if (call.url === '/api/calibration/mic' && call.method === 'PUT') {
      return jsonResponse(200, profileBody())
    }
    if (call.url === '/api/calibration/mic/calibration' && call.method === 'POST') {
      return jsonResponse(200, {
        ok: true,
        sha256: SHA,
        filename: 'umik.txt',
        bytes: 12,
        serial: '718-7696',
        parsed: { serno: '718-7696', sens_factor_db: null, points: 1 }
      })
    }
    if (call.url === '/api/calibration/mic/status' && call.method === 'GET') {
      return jsonResponse(200, statusBody())
    }
    return jsonResponse(500, { error: { code: 'unexpected', message: call.method + ' ' + call.url } })
  })
  const client = new ApiClient({ fetchImpl: recorded.fetch, csrfToken: () => 'csrf-1' })
  const api = createMicApi(client)

  await api.getProfile()
  await api.putProfile({ serial: '718-7696', orientation: '90deg' })
  await api.uploadCalibration('SERNO 718-7696\n20 0.0\n', { filename: 'umik.txt' })
  await api.getStatus()

  assert.deepEqual(recorded.calls.map((call) => call.method + ' ' + call.url), [
    'GET /api/calibration/mic',
    'PUT /api/calibration/mic',
    'POST /api/calibration/mic/calibration',
    'GET /api/calibration/mic/status'
  ])
  assert.equal(recorded.calls[1]?.body, JSON.stringify({ serial: '718-7696', orientation: '90deg' }))
  assert.equal(recorded.calls[1]?.headers['X-KHAEN-CSRF'], 'csrf-1')
  assert.equal(recorded.calls[2]?.body, 'SERNO 718-7696\n20 0.0\n')
  assert.equal(recorded.calls[2]?.headers['Content-Type'], 'text/plain; charset=utf-8')
  assert.equal(recorded.calls[2]?.headers['X-KHAEN-FILENAME'], 'umik.txt')
})

test('createMicApi refuses to upload an empty calibration body', async () => {
  const recorded = createRecordingFetch(() => jsonResponse(200, profileBody()))
  const api = createMicApi(new ApiClient({ fetchImpl: recorded.fetch }))
  const error = await captureError(() => api.uploadCalibration(''))
  assert.equal(error.status, 400)
  assert.equal(recorded.calls.length, 0)
})
