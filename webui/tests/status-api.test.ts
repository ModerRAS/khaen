import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, ApiError } from '../src/api/client.ts'
import {
  STATUS_FETCH_TIMEOUT_MS,
  STATUS_SCHEMA,
  classifyStatusError,
  createStatusApi,
  formatChannelMap,
  hasAnyValue,
  isStatusStale,
  parseStatusResponse,
  reasonForPath,
  runStateKind,
  runStateLabelKey,
  underrunRate,
  underrunSeverity,
  uptimeParts
} from '../src/api/status.ts'
import { buildStatusFixture } from '../scripts/status-fixtures.mjs'
import { createRecordingFetch, jsonResponse, textResponse } from '../scripts/mock-test-support.mjs'

function presentBody(): Record<string, any> {
  return buildStatusFixture('present')
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value))
}

async function captureError(run: () => Promise<unknown>): Promise<ApiError> {
  try {
    await run()
  } catch (error) {
    assert.ok(error instanceof ApiError, 'expected an ApiError, got: ' + String(error))
    return error
  }
  return assert.fail('expected the call to reject') as never
}

test('parseStatusResponse keeps all present fixture values', () => {
  const status = parseStatusResponse(presentBody())
  assert.equal(status.schema, STATUS_SCHEMA)
  assert.equal(status.daemon.version, '0.3.0-mock')
  assert.equal(status.daemon.uptime_s, 43210)
  assert.equal(status.host_link.connected, true)
  assert.equal(status.host_link.status_age_ms, 120)
  assert.equal(status.state.value, 'running')
  assert.equal(status.plan.version, 3)
  assert.equal(status.plan.source, 'upload')
  assert.equal(status.format.plan_rate, 48000)
  assert.deepEqual(status.format.aes67.map, [0, 1])
  assert.equal(status.format.aes67.pt, 97)
  assert.equal(status.input.underruns, 21)
  assert.equal(status.output.bytes_written, 2973696000)
  assert.equal(status.device.state, 'present')
  assert.equal(status.calibration.last_verdict, 'PASS')
  assert.equal(status.not_wired.length, 2)
  assert.deepEqual(status.reasons, {})
  assert.equal(status.mode, null)
})

test('parseStatusResponse reads top-level mode and does not borrow host_link.mode', async () => {
  const present = presentBody()
  present.mode = 'aes67'
  present.host_link.mode = 'socket'
  assert.equal(parseStatusResponse(present).mode, 'aes67')

  const missing = presentBody()
  missing.mode = null
  missing.reasons = { mode: 'rt-host did not report mode' }
  missing.host_link.mode = 'file'
  const status = parseStatusResponse(missing)
  assert.equal(status.mode, null)
  assert.equal(reasonForPath(status, 'mode'), 'rt-host did not report mode')

  const wrong = presentBody()
  wrong.mode = 1
  assert.equal((await captureError(async () => parseStatusResponse(wrong))).code, 'invalid_response')
})

test('parseStatusResponse rejects a wrong or missing schema', async () => {
  const wrong = presentBody()
  wrong.schema = 'khaen-status/v2'
  assert.equal((await captureError(async () => parseStatusResponse(wrong))).code, 'invalid_response')

  const missing = presentBody()
  delete missing.schema
  assert.equal((await captureError(async () => parseStatusResponse(missing))).code, 'invalid_response')
})

test('parseStatusResponse rejects non-object bodies', async () => {
  for (const body of [null, 42, 'status', [1, 2]]) {
    assert.equal((await captureError(async () => parseStatusResponse(body))).code, 'invalid_response')
  }
})

test('parseStatusResponse rejects wrong leaf types instead of faking values', async () => {
  const wrongNumber = presentBody()
  wrongNumber.daemon.uptime_s = '43210'
  assert.equal((await captureError(async () => parseStatusResponse(wrongNumber))).code, 'invalid_response')

  const wrongBoolean = presentBody()
  wrongBoolean.host_link.connected = 'yes'
  assert.equal((await captureError(async () => parseStatusResponse(wrongBoolean))).code, 'invalid_response')

  const wrongString = presentBody()
  wrongString.state.value = 3
  assert.equal((await captureError(async () => parseStatusResponse(wrongString))).code, 'invalid_response')

  const wrongArray = presentBody()
  wrongArray.format.aes67.map = [0, 'x']
  assert.equal((await captureError(async () => parseStatusResponse(wrongArray))).code, 'invalid_response')

  const wrongGroup = presentBody()
  wrongGroup.input = 'nope'
  assert.equal((await captureError(async () => parseStatusResponse(wrongGroup))).code, 'invalid_response')

  const notFinite = presentBody()
  notFinite.input.packets = Number.POSITIVE_INFINITY
  assert.equal((await captureError(async () => parseStatusResponse(notFinite))).code, 'invalid_response')
})

test('parseStatusResponse normalizes missing groups to all-null groups', () => {
  const body = presentBody()
  delete body.daemon
  delete body.format.aes67
  body.device = null
  const status = parseStatusResponse(body)
  assert.deepEqual(status.daemon, { version: null, uptime_s: null, bind: null })
  assert.deepEqual(status.format.aes67, { channels: null, map: null, pt: null, group: null, port: null })
  assert.deepEqual(status.device, { id: null, card: null, state: null, rate_readback: null })
})

test('parseStatusResponse ignores unknown extra fields (forward compatible)', () => {
  const body = presentBody()
  body.future_field = { anything: true }
  body.daemon.extra = 1
  const status = parseStatusResponse(body)
  assert.equal(status.daemon.version, '0.3.0-mock')
  assert.equal(Object.prototype.hasOwnProperty.call(status.daemon, 'extra'), false)
})

test('parseStatusResponse validates not_wired and reasons shapes', async () => {
  const badNotWired = presentBody()
  badNotWired.not_wired = 'not-an-array'
  assert.equal((await captureError(async () => parseStatusResponse(badNotWired))).code, 'invalid_response')

  const missingReason = presentBody()
  missingReason.not_wired = [{ path: '/api/x' }]
  assert.equal((await captureError(async () => parseStatusResponse(missingReason))).code, 'invalid_response')

  const badReasons = presentBody()
  badReasons.reasons = { device_rate: 42 }
  assert.equal((await captureError(async () => parseStatusResponse(badReasons))).code, 'invalid_response')

  const bare = presentBody()
  delete bare.reasons
  delete bare.not_wired
  const status = parseStatusResponse(bare)
  assert.deepEqual(status.reasons, {})
  assert.deepEqual(status.not_wired, [])
})

test('isStatusStale follows connected=false or status_age_ms > 2000', () => {
  const fresh = parseStatusResponse(presentBody())
  assert.equal(isStatusStale(fresh), false)

  const slow = clone(fresh)
  slow.host_link.status_age_ms = 2000
  assert.equal(isStatusStale(slow), false)

  slow.host_link.status_age_ms = 2001
  assert.equal(isStatusStale(slow), true)

  const disconnected = clone(fresh)
  disconnected.host_link.connected = false
  assert.equal(isStatusStale(disconnected), true)

  const unknown = clone(fresh)
  unknown.host_link.connected = null
  unknown.host_link.status_age_ms = null
  assert.equal(isStatusStale(unknown), false)
  assert.equal(isStatusStale(null), false)
})

test('runStateKind maps daemon values to StatusKind, faults first', () => {
  assert.equal(runStateKind({ value: 'running', muted: false, safe: false, last_error: null }), 'running')
  assert.equal(runStateKind({ value: 'RUNNING', muted: null, safe: null, last_error: null }), 'running')
  assert.equal(runStateKind({ value: 'faulted', muted: true, safe: false, last_error: null }), 'faulted')
  assert.equal(runStateKind({ value: 'failed', muted: false, safe: false, last_error: null }), 'faulted')
  assert.equal(runStateKind({ value: 'safe', muted: true, safe: true, last_error: null }), 'safe')
  assert.equal(runStateKind({ value: 'running', muted: true, safe: false, last_error: null }), 'muted')
  assert.equal(runStateKind({ value: 'degraded', muted: false, safe: false, last_error: null }), 'unknown')
  assert.equal(runStateKind(null), 'unknown')
})

test('runStateLabelKey only localizes known states', () => {
  assert.equal(runStateLabelKey({ value: 'running', muted: false, safe: false, last_error: null }), 'status.running')
  assert.equal(runStateLabelKey({ value: 'faulted', muted: false, safe: false, last_error: null }), 'status.faulted')
  assert.equal(runStateLabelKey({ value: 'safe', muted: false, safe: true, last_error: null }), 'status.safe')
  assert.equal(runStateLabelKey({ value: 'running', muted: true, safe: false, last_error: null }), 'status.muted')
  assert.equal(runStateLabelKey({ value: 'degraded', muted: false, safe: false, last_error: null }), null)
  assert.equal(runStateLabelKey(undefined), null)
})

test('reasonForPath resolves nested and flat reasons', () => {
  const status = parseStatusResponse(buildStatusFixture('missing'))
  assert.equal(reasonForPath(status, 'format.device_rate'), 'rt-host did not report the device rate')
  assert.equal(reasonForPath(status, 'no.such.field'), null)
  assert.equal(reasonForPath(null, 'format.device_rate'), null)

  // 契约示例里 reasons 也用扁平字段名：完整路径找不到时回退到末段字段名。
  const flat = clone(status)
  flat.reasons = { device_rate: 'flat reason' }
  assert.equal(reasonForPath(flat, 'format.device_rate'), 'flat reason')
  assert.equal(reasonForPath(flat, 'device_rate'), 'flat reason')
})

test('underrunRate and underrunSeverity grade the counter ratio', () => {
  assert.equal(underrunRate({ underruns: 3 }, { blocks: 300000 }), 0.00001)
  assert.equal(underrunRate({ underruns: null }, { blocks: 300000 }), null)
  assert.equal(underrunRate({ underruns: 1 }, { blocks: 0 }), null)
  assert.equal(underrunRate({ underruns: -1 }, { blocks: 10 }), null)
  assert.equal(underrunSeverity(0), 'ok')
  assert.equal(underrunSeverity(0.0001), 'ok')
  assert.equal(underrunSeverity(0.0005), 'warn')
  assert.equal(underrunSeverity(0.002), 'bad')
  assert.equal(underrunSeverity(null), null)
  assert.equal(underrunSeverity(Number.NaN), null)
})

test('uptimeParts splits seconds into days/hours/minutes/seconds', () => {
  assert.deepEqual(uptimeParts(0), { days: 0, hours: 0, minutes: 0, seconds: 0 })
  assert.deepEqual(uptimeParts(59.9), { days: 0, hours: 0, minutes: 0, seconds: 59 })
  assert.deepEqual(uptimeParts(3661), { days: 0, hours: 1, minutes: 1, seconds: 1 })
  assert.deepEqual(uptimeParts(90061), { days: 1, hours: 1, minutes: 1, seconds: 1 })
  assert.equal(uptimeParts(-1), null)
  assert.equal(uptimeParts(Number.NaN), null)
  assert.equal(uptimeParts(null), null)
})

test('formatChannelMap and hasAnyValue handle null shapes', () => {
  assert.equal(formatChannelMap([0, 1]), '0,1')
  assert.equal(formatChannelMap([2]), '2')
  assert.equal(formatChannelMap([]), null)
  assert.equal(formatChannelMap(null), null)
  assert.equal(hasAnyValue([null, undefined]), false)
  assert.equal(hasAnyValue([null, 0]), true)
  assert.equal(hasAnyValue([null, false]), true)
})

test('classifyStatusError maps ApiError kinds to UI categories and i18n keys', () => {
  const network = classifyStatusError(new ApiError({ status: 0, code: 'network_error', message: 'boom' }))
  assert.equal(network.category, 'network')
  assert.equal(network.i18nKey, 'error.status.network')
  assert.equal(network.retryable, true)
  assert.equal(network.detail, 'boom')

  const timeout = classifyStatusError(new ApiError({ status: 0, code: 'request_timeout', message: 'sluggish' }))
  assert.equal(timeout.category, 'timeout')
  assert.equal(timeout.i18nKey, 'error.status.timeout')

  const unauthorized = classifyStatusError(new ApiError({ status: 401, code: 'not_authenticated', message: 'nope' }))
  assert.equal(unauthorized.category, 'unauthorized')
  assert.equal(unauthorized.i18nKey, 'error.status.unauthorized')
  assert.equal(unauthorized.retryable, false)

  const server = classifyStatusError(new ApiError({ status: 503, code: 'internal_error', message: 'down' }))
  assert.equal(server.category, 'server')
  assert.equal(server.i18nKey, 'error.status.server')

  const invalid = classifyStatusError(new ApiError({ status: 200, code: 'invalid_response', message: 'bad json' }))
  assert.equal(invalid.category, 'invalid_response')
  assert.equal(invalid.i18nKey, 'error.status.invalidResponse')

  const other = classifyStatusError(new ApiError({ status: 418, code: 'teapot', message: 'tea' }))
  assert.equal(other.category, 'unknown')

  const notApi = classifyStatusError(new Error('code bug'))
  assert.equal(notApi.category, 'unknown')
  assert.equal(notApi.code, 'unknown_error')
  assert.equal(notApi.detail, 'code bug')
})

test('createStatusApi GETs /api/status with credentials and parses the body', async () => {
  const recorder = createRecordingFetch(() => jsonResponse(200, presentBody()))
  const api = createStatusApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const status = await api.fetchStatus()
  assert.equal(status.state.value, 'running')
  assert.equal(recorder.calls.length, 1)
  assert.equal(recorder.calls[0].url, '/api/status')
  assert.equal(recorder.calls[0].method, 'GET')
  assert.equal(recorder.calls[0].credentials, 'include')
  assert.equal(recorder.calls[0].headers.Accept, 'application/json')
})

test('createStatusApi forwards the abort signal and reports abort as a network error', async () => {
  let received: AbortSignal | undefined
  const api = createStatusApi(
    new ApiClient({
      fetchImpl: async (_url, init) => {
        received = init?.signal ?? undefined
        throw new TypeError('aborted')
      }
    })
  )
  const controller = new AbortController()
  const error = await captureError(() => api.fetchStatus({ signal: controller.signal }))
  assert.equal(error.code, 'network_error')
  assert.equal(received, controller.signal)
})

test('createStatusApi rejects malformed JSON with invalid_response', async () => {
  const recorder = createRecordingFetch(() => textResponse(200, 'not json'))
  const api = createStatusApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const error = await captureError(() => api.fetchStatus())
  assert.equal(error.code, 'invalid_response')
})

test('createStatusApi lets 401 reach the shared unauthorized hook', async () => {
  const seen: string[] = []
  const recorder = createRecordingFetch(() =>
    jsonResponse(401, { error: { code: 'not_authenticated', message: 'auth required' } })
  )
  const client = new ApiClient({ fetchImpl: recorder.fetch, onUnauthorized: (error) => seen.push(error.code) })
  const api = createStatusApi(client)
  const error = await captureError(() => api.fetchStatus())
  assert.equal(error.status, 401)
  assert.deepEqual(seen, ['not_authenticated'])
  assert.equal(STATUS_FETCH_TIMEOUT_MS, 8000)
})
