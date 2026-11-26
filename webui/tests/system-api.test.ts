import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, ApiError } from '../src/api/client.ts'
import {
  DEFAULT_LOG_LINES,
  LOG_LINE_OPTIONS,
  LOG_UNITS,
  MAX_LOG_LINES,
  SYSTEM_INFO_SCHEMA,
  SYSTEM_LOGS_SCHEMA,
  classifySystemError,
  createSystemApi,
  diagnosticsFallbackFilename,
  formatVersionLabel,
  isLogUnit,
  isMotuDevice,
  isRestartAvailable,
  normalizeLogLines,
  normalizeVersion,
  parseContentDispositionFilename,
  parseHealthz,
  parseRestartResult,
  parseSystemInfo,
  parseSystemLogs,
  reasonForSystemPath,
  validatePasswordChange,
  versionConsistency
} from '../src/api/system.ts'
import {
  buildSystemInfoFixture,
  buildSystemLogsFixture
} from '../scripts/system-fixtures.mjs'
import { createRecordingFetch, jsonResponse, textResponse } from '../scripts/mock-test-support.mjs'

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

test('parseSystemInfo keeps every fixture field and separates crate/release/commit', () => {
  const info = parseSystemInfo(buildSystemInfoFixture('mismatch'))
  assert.equal(info.schema, SYSTEM_INFO_SCHEMA)
  assert.equal(info.crate_version, '0.1.0')
  assert.equal(info.release_version, 'v0.1.2')
  assert.equal(info.commit, '855f4c4')
  assert.equal(info.build_profile, 'release')
  assert.match(String(info.rustc), /^rustc /)
  assert.equal(info.target, 'x86_64-unknown-linux-gnu')
  assert.equal(info.bind, '0.0.0.0:80')
  assert.equal(info.state_dir, '/var/lib/khaen')
  assert.equal(info.rt_status_file, '/var/lib/khaen/rt-host-status.json')
  assert.equal(info.uptime_s, 18342)
  assert.equal(info.hostname, 'AoipDSP')
  assert.match(String(info.kernel), /^Linux /)
  assert.match(String(info.os), /^Debian /)
  assert.deepEqual(info.services.daemon, { active: true, enabled: true, pid: 4211, reason: null })
  assert.equal(info.services.rt_host.active, false)
  assert.equal(info.services.rt_host.pid, null)
  assert.equal(info.devices.length, 2)
  assert.equal(info.devices[0].card_id, 'UltraLitemk5')
  assert.deepEqual(info.devices[0].rates, [44100, 48000, 88200, 96000])
  assert.equal(info.host_link.connected, false)
  assert.equal(info.restart.allowed, true)
  assert.equal(info.restart.target, 'rt-host')
  assert.equal(info.reasons['services.rt_host.pid'], 'rt-host is not running')
})

test('parseSystemInfo accepts the consistent fixture', () => {
  const info = parseSystemInfo(buildSystemInfoFixture('consistent'))
  assert.equal(info.crate_version, info.release_version?.replace(/^v/, ''))
  assert.equal(info.services.rt_host.active, true)
  assert.equal(info.services.rt_host.pid, 4247)
  assert.equal(info.host_link.connected, true)
  assert.deepEqual(info.reasons, {})
})

test('parseSystemInfo keeps degraded fields null instead of inventing values', () => {
  const info = parseSystemInfo(buildSystemInfoFixture('degraded'))
  assert.equal(info.crate_version, '0.1.0')
  assert.equal(info.release_version, null)
  assert.equal(info.commit, null)
  assert.equal(info.build_profile, null)
  assert.equal(info.hostname, null)
  assert.equal(info.kernel, null)
  assert.equal(info.os, null)
  assert.equal(info.uptime_s, null)
  assert.equal(info.bind, null)
  assert.deepEqual(info.services.daemon, { active: null, enabled: null, pid: null, reason: 'systemctl is not available on this host' })
  assert.deepEqual(info.devices, [])
  assert.equal(info.host_link.connected, null)
  assert.equal(info.restart.allowed, false)
  assert.match(String(info.restart.reason), /KHAEN_ALLOW_RESTART/)
  assert.ok(Object.keys(info.reasons).length >= 10)
})

test('parseSystemInfo rejects a wrong schema but tolerates a missing one', () => {
  const wrong = clone(buildSystemInfoFixture('consistent'))
  wrong.schema = 'khaen-system-info/v2'
  assert.throws(() => parseSystemInfo(wrong), /schema must be/)

  const missing = clone(buildSystemInfoFixture('consistent'))
  delete missing.schema
  assert.equal(parseSystemInfo(missing).crate_version, '0.1.2')
})

test('parseSystemInfo rejects non-object bodies', () => {
  for (const body of [null, 42, 'info', [1, 2, 3]]) {
    assert.throws(() => parseSystemInfo(body), /must be .*object/)
  }
})

test('parseSystemInfo rejects wrong leaf types instead of faking values', () => {
  const cases: Array<(body: Record<string, any>) => void> = [
    (body) => {
      body.crate_version = 1
    },
    (body) => {
      body.uptime_s = '18342'
    },
    (body) => {
      body.services.daemon.active = 'yes'
    },
    (body) => {
      body.services.rt_host = 'nope'
    },
    (body) => {
      body.devices = 'cards'
    },
    (body) => {
      body.devices[0].rates = [44100, 'x']
    },
    (body) => {
      body.host_link.status_age_ms = Number.POSITIVE_INFINITY
    },
    (body) => {
      body.reasons = { hostname: 42 }
    },
    (body) => {
      body.restart = { allowed: 'yes' }
    }
  ]
  for (const mutate of cases) {
    const body = buildSystemInfoFixture('consistent')
    mutate(body)
    assert.throws(() => parseSystemInfo(body), /invalid|must be|unexpected response/, JSON.stringify(body).slice(0, 80))
  }
})

test('parseSystemInfo normalizes missing groups and treats missing restart capability as not allowed', () => {
  const body: Record<string, any> = buildSystemInfoFixture('mismatch')
  delete body.services
  delete body.devices
  delete body.host_link
  delete body.restart
  delete body.reasons
  const info = parseSystemInfo(body)
  assert.deepEqual(info.services.daemon, { active: null, enabled: null, pid: null, reason: null })
  assert.deepEqual(info.devices, [])
  assert.equal(info.host_link.connected, null)
  // fail closed：没有能力信息时不显示危险操作。
  assert.equal(info.restart.allowed, false)
  assert.equal(info.restart.target, null)
  assert.deepEqual(info.reasons, {})
})

test('parseSystemInfo accepts the rt-host hyphen alias for the service key', () => {
  const body = buildSystemInfoFixture('consistent')
  body.services['rt-host'] = body.services.rt_host
  delete body.services.rt_host
  const info = parseSystemInfo(body)
  assert.equal(info.services.rt_host.active, true)
  assert.equal(info.services.rt_host.pid, 4247)
})

test('parseSystemInfo ignores unknown extra fields (forward compatible)', () => {
  const body = buildSystemInfoFixture('consistent')
  body.future_field = { anything: true }
  body.services.daemon.extra = 1
  const info = parseSystemInfo(body)
  assert.equal(info.crate_version, '0.1.2')
  assert.equal(Object.prototype.hasOwnProperty.call(info.services.daemon, 'extra'), false)
})

test('version helpers normalize labels and detect crate/release mismatch', () => {
  assert.equal(normalizeVersion('0.1.2'), '0.1.2')
  assert.equal(normalizeVersion('v0.1.2'), '0.1.2')
  assert.equal(normalizeVersion(' V0.1.2 '), '0.1.2')
  assert.equal(normalizeVersion(''), null)
  assert.equal(normalizeVersion(null), null)
  assert.equal(formatVersionLabel('0.1.2'), 'v0.1.2')
  assert.equal(formatVersionLabel('v0.1.2'), 'v0.1.2')
  assert.equal(formatVersionLabel(null), null)

  assert.equal(versionConsistency('0.1.2', 'v0.1.2'), 'consistent')
  assert.equal(versionConsistency('0.1.0', 'v0.1.2'), 'mismatch')
  assert.equal(versionConsistency(null, 'v0.1.2'), 'unknown')
  assert.equal(versionConsistency('0.1.0', null), 'unknown')
  assert.equal(versionConsistency(null, null), 'unknown')
})

test('isMotuDevice flags MOTU/UltraLite cards only', () => {
  assert.equal(isMotuDevice({ card_id: 'UltraLitemk5', model: 'MOTU UltraLite mk5', driver: 'snd_usb_audio' }), true)
  assert.equal(isMotuDevice({ card_id: 'UltraLitemk5', model: null, driver: null }), true)
  assert.equal(isMotuDevice({ card_id: null, model: 'MOTU 828', driver: null }), true)
  assert.equal(isMotuDevice({ card_id: 'PCH', model: 'HDA Intel PCH', driver: 'snd_hda_intel' }), false)
  assert.equal(isMotuDevice({ card_id: null, model: null, driver: null }), false)
})

test('isRestartAvailable requires the capability and the rt-host target', () => {
  assert.equal(isRestartAvailable(null), false)
  assert.equal(
    isRestartAvailable({ restart: { allowed: true, target: 'rt-host', reason: null, last_restarted_at: null } }),
    true
  )
  assert.equal(
    isRestartAvailable({ restart: { allowed: true, target: null, reason: null, last_restarted_at: null } }),
    true
  )
  assert.equal(
    isRestartAvailable({ restart: { allowed: false, target: 'rt-host', reason: 'disabled', last_restarted_at: null } }),
    false
  )
  assert.equal(
    isRestartAvailable({ restart: { allowed: true, target: 'daemon', reason: null, last_restarted_at: null } }),
    false
  )
})

test('reasonForSystemPath resolves nested and flat reasons', () => {
  const info = parseSystemInfo(buildSystemInfoFixture('degraded'))
  assert.equal(reasonForSystemPath(info, 'host_link.connected'), 'rt-host status file is not readable')
  assert.equal(reasonForSystemPath(info, 'services.rt_host.pid'), 'systemctl is not available on this host')
  assert.equal(reasonForSystemPath(info, 'no.such.field'), null)
  assert.equal(reasonForSystemPath(null, 'host_link.connected'), null)

  const flat = clone(info)
  flat.reasons = { hostname: 'flat reason' }
  assert.equal(reasonForSystemPath(flat, 'host.name.hostname'), 'flat reason')
  assert.equal(reasonForSystemPath(flat, 'hostname'), 'flat reason')
})

test('parseSystemLogs validates the contract lines shape', () => {
  const built = buildSystemLogsFixture('mismatch', 'rt-host', 10)
  const logs = parseSystemLogs(built.body)
  assert.equal(logs.schema, SYSTEM_LOGS_SCHEMA)
  assert.equal(logs.unit, 'rt-host')
  assert.equal(logs.lines.length, 10)
  assert.match(logs.lines[0], /khaen-rt-host/)
  assert.equal(logs.requested_lines, 10)
  assert.equal(logs.truncated, false)
  assert.equal(logs.source, 'journalctl')
})

test('parseSystemLogs tolerates a text body as an honest alternative', () => {
  const logs = parseSystemLogs({ unit: 'daemon', text: 'line one\nline two\r\nline three', reason: null })
  assert.deepEqual(logs.lines, ['line one', 'line two', 'line three'])
  assert.equal(logs.schema, SYSTEM_LOGS_SCHEMA)
})

test('parseSystemLogs rejects malformed bodies and wrong units', () => {
  assert.throws(() => parseSystemLogs(null), /must be a JSON object/)
  assert.throws(() => parseSystemLogs({ lines: 'nope' }), /must be an array/)
  assert.throws(() => parseSystemLogs({ lines: ['a', 2] }), /must be a string/)
  assert.throws(() => parseSystemLogs({ unit: 'systemd-journald', lines: [] }), /unit must be/)
  assert.throws(() => parseSystemLogs({ lines: [] , schema: 'khaen-system-logs/v2' }), /schema must be/)
})

test('log unit and line helpers clamp invalid values', () => {
  assert.deepEqual([...LOG_UNITS], ['daemon', 'rt-host'])
  assert.deepEqual([...LOG_LINE_OPTIONS], [100, 200, 500])
  assert.equal(isLogUnit('daemon'), true)
  assert.equal(isLogUnit('rt-host'), true)
  assert.equal(isLogUnit('other'), false)
  assert.equal(normalizeLogLines(100), 100)
  assert.equal(normalizeLogLines(1000), MAX_LOG_LINES)
  assert.equal(normalizeLogLines(0), DEFAULT_LOG_LINES)
  assert.equal(normalizeLogLines(Number.NaN), DEFAULT_LOG_LINES)
  assert.equal(normalizeLogLines('500'), DEFAULT_LOG_LINES)
})

test('parseRestartResult accepts ok/restarted receipts and rejects empty ones', () => {
  const byOk = parseRestartResult({ ok: true, target: 'rt-host', restarted_at: '2026-11-26T08:00:00Z' })
  assert.equal(byOk.ok, true)
  assert.equal(byOk.target, 'rt-host')
  assert.equal(byOk.restarted_at, '2026-11-26T08:00:00Z')

  const byRestarted = parseRestartResult({ restarted: true })
  assert.equal(byRestarted.ok, true)
  assert.equal(byRestarted.restarted, true)

  assert.throws(() => parseRestartResult({ ok: false }), /must carry/)
  assert.throws(() => parseRestartResult(null), /must be a JSON object/)
})

test('parseHealthz keeps the anonymous release version or null', () => {
  const ok = parseHealthz({ ok: true, version: 'v0.1.2', uptime_s: 12 })
  assert.equal(ok.ok, true)
  assert.equal(ok.version, 'v0.1.2')
  assert.equal(ok.uptime_s, 12)

  const missing = parseHealthz({ ok: true })
  assert.equal(missing.version, null)
  assert.equal(missing.uptime_s, null)

  assert.throws(() => parseHealthz({ ok: 'yes' }), /must be a boolean/)
})

test('parseContentDispositionFilename supports quoted, extended and bare filenames', () => {
  assert.equal(
    parseContentDispositionFilename('attachment; filename="khaen-diagnostics-v0.1.2-20261126T080000Z.json"'),
    'khaen-diagnostics-v0.1.2-20261126T080000Z.json'
  )
  assert.equal(
    parseContentDispositionFilename('attachment; filename=diag.json'),
    'diag.json'
  )
  assert.equal(
    parseContentDispositionFilename("attachment; filename*=UTF-8''khaen%20diag.json"),
    'khaen diag.json'
  )
  assert.equal(parseContentDispositionFilename(null), null)
  assert.equal(parseContentDispositionFilename(''), null)
  assert.equal(parseContentDispositionFilename('attachment'), null)
  assert.equal(parseContentDispositionFilename("attachment; filename*=UTF-8''%E0%A4%A"), null)
})

test('diagnosticsFallbackFilename includes the release version and a deterministic UTC stamp', () => {
  const stamp = Date.parse('2026-11-26T08:00:00.000Z')
  assert.equal(
    diagnosticsFallbackFilename('v0.1.2', stamp),
    'khaen-diagnostics-v0.1.2-20261126T080000Z.json'
  )
  assert.equal(
    diagnosticsFallbackFilename(null, stamp),
    'khaen-diagnostics-vunknown-20261126T080000Z.json'
  )
})

test('validatePasswordChange checks old/new/confirm locally', () => {
  assert.deepEqual(validatePasswordChange('old-password', 'new-password', 'new-password', 8), { ok: true })
  assert.deepEqual(validatePasswordChange('', 'new-password', 'new-password', 8), {
    ok: false,
    key: 'system.password.validation.required'
  })
  assert.deepEqual(validatePasswordChange('old-password', 'short', 'short', 8), {
    ok: false,
    key: 'system.password.validation.tooShort'
  })
  assert.deepEqual(validatePasswordChange('old-password', 'new-password', 'other-password', 8), {
    ok: false,
    key: 'system.password.validation.mismatch'
  })
  // 非有限最小长度回退 1。
  assert.equal(validatePasswordChange('old', 'x', 'x', Number.NaN).ok, true)
})

test('classifySystemError maps daemon errors to UI categories', () => {
  const network = classifySystemError(new ApiError({ status: 0, code: 'network_error', message: 'boom' }))
  assert.equal(network.category, 'network')
  assert.equal(network.i18nKey, 'error.system.network')
  assert.equal(network.retryable, true)

  const timeout = classifySystemError(new ApiError({ status: 0, code: 'request_timeout', message: 'slow' }))
  assert.equal(timeout.category, 'timeout')

  const unauthorized = classifySystemError(
    new ApiError({ status: 401, code: 'not_authenticated', message: 'nope' })
  )
  assert.equal(unauthorized.category, 'unauthorized')
  assert.equal(unauthorized.retryable, false)

  const disabled = classifySystemError(
    new ApiError({
      status: 403,
      code: 'restart_disabled',
      message: 'restart is disabled',
      details: { reason: 'KHAEN_ALLOW_RESTART is not set' }
    })
  )
  assert.equal(disabled.category, 'disabled')
  assert.equal(disabled.reason, 'KHAEN_ALLOW_RESTART is not set')

  const unavailable = classifySystemError(
    new ApiError({
      status: 503,
      code: 'logs_unavailable',
      message: 'journalctl unavailable',
      details: { reason: 'systemd not detected' }
    })
  )
  assert.equal(unavailable.category, 'unavailable')
  assert.equal(unavailable.i18nKey, 'error.system.unavailable')
  assert.equal(unavailable.reason, 'systemd not detected')

  const invalid = classifySystemError(new ApiError({ status: 200, code: 'invalid_response', message: 'bad' }))
  assert.equal(invalid.category, 'invalid_response')
  assert.equal(invalid.i18nKey, 'error.system.invalidResponse')

  const validation = classifySystemError(new ApiError({ status: 400, code: 'invalid_password', message: 'short' }))
  assert.equal(validation.category, 'validation')

  const server = classifySystemError(new ApiError({ status: 502, code: 'bad_gateway', message: 'down' }))
  assert.equal(server.category, 'server')
  assert.equal(server.retryable, true)

  const other = classifySystemError(new ApiError({ status: 418, code: 'teapot', message: 'tea' }))
  assert.equal(other.category, 'unknown')

  const notApi = classifySystemError(new Error('code bug'))
  assert.equal(notApi.category, 'unknown')
  assert.equal(notApi.code, 'unknown_error')
  assert.equal(notApi.detail, 'code bug')
})

test('createSystemApi GETs /api/system/info and parses the body', async () => {
  const recorder = createRecordingFetch(() => jsonResponse(200, buildSystemInfoFixture('mismatch')))
  const api = createSystemApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const info = await api.fetchInfo()
  assert.equal(info.crate_version, '0.1.0')
  assert.equal(recorder.calls.length, 1)
  assert.equal(recorder.calls[0].url, '/api/system/info')
  assert.equal(recorder.calls[0].method, 'GET')
  assert.equal(recorder.calls[0].credentials, 'include')
})

test('createSystemApi fetches logs with unit/lines query and clamps lines', async () => {
  const recorder = createRecordingFetch(() => jsonResponse(200, buildSystemLogsFixture('mismatch', 'rt-host', 5).body))
  const api = createSystemApi(new ApiClient({ fetchImpl: recorder.fetch }))
  await api.fetchLogs({ unit: 'rt-host', lines: 5 })
  assert.equal(recorder.calls[0].url, '/api/system/logs?unit=rt-host&lines=5')

  await api.fetchLogs({ unit: 'daemon', lines: 9999 })
  assert.equal(recorder.calls[1].url, '/api/system/logs?unit=daemon&lines=500')
})

test('createSystemApi downloads diagnostics with filename from Content-Disposition', async () => {
  const body = JSON.stringify({ schema: 'khaen-diagnostics/v1', generated_at: '2026-11-26T08:00:00.000Z' })
  const recorder = createRecordingFetch(() =>
    textResponse(200, body, {
      'Content-Type': 'application/json',
      'Content-Disposition': 'attachment; filename="khaen-diagnostics-v0.1.2-20261126T080000Z.json"'
    })
  )
  const api = createSystemApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const download = await api.fetchDiagnostics()
  assert.equal(download.filename, 'khaen-diagnostics-v0.1.2-20261126T080000Z.json')
  assert.equal(download.text, body)
  assert.deepEqual(download.json, { schema: 'khaen-diagnostics/v1', generated_at: '2026-11-26T08:00:00.000Z' })
  assert.equal(recorder.calls[0].url, '/api/system/diagnostics')
  assert.equal(recorder.calls[0].method, 'GET')
})

test('createSystemApi rejects a non-JSON diagnostics body', async () => {
  const recorder = createRecordingFetch(() => textResponse(200, '<html>proxy error</html>'))
  const api = createSystemApi(new ApiClient({ fetchImpl: recorder.fetch }))
  const error = await captureError(() => api.fetchDiagnostics())
  assert.equal(error.code, 'invalid_response')
})

test('createSystemApi fetches /healthz anonymously without tripping the 401 hook', async () => {
  const seen: string[] = []
  const recorder = createRecordingFetch(() =>
    jsonResponse(401, { error: { code: 'not_authenticated', message: 'no session' } })
  )
  const client = new ApiClient({ fetchImpl: recorder.fetch, onUnauthorized: (error) => seen.push(error.code) })
  const api = createSystemApi(client)
  const error = await captureError(() => api.fetchHealthz())
  assert.equal(error.status, 401)
  assert.deepEqual(seen, [])

  const okRecorder = createRecordingFetch(() => jsonResponse(200, { ok: true, version: 'v0.1.2', uptime_s: 9 }))
  const okApi = createSystemApi(new ApiClient({ fetchImpl: okRecorder.fetch }))
  const healthz = await okApi.fetchHealthz()
  assert.equal(healthz.version, 'v0.1.2')
  assert.equal(okRecorder.calls[0].url, '/healthz')
})

test('createSystemApi changePassword posts old/new with CSRF and probe auth', async () => {
  const seen: string[] = []
  const recorder = createRecordingFetch(() =>
    jsonResponse(200, { csrf: 'new-csrf', expires_in: 3600 })
  )
  const client = new ApiClient({
    fetchImpl: recorder.fetch,
    csrfToken: () => 'csrf-token-1',
    onUnauthorized: (error) => seen.push(error.code)
  })
  const api = createSystemApi(client)
  await api.changePassword('old-password', 'new-password')
  assert.equal(recorder.calls[0].url, '/api/system/password')
  assert.equal(recorder.calls[0].method, 'POST')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], 'csrf-token-1')
  assert.deepEqual(JSON.parse(String(recorder.calls[0].body)), { old: 'old-password', new: 'new-password' })

  // 改密 401 = 旧密码错误：probe 模式不触发全局跳登录。
  const failRecorder = createRecordingFetch(() =>
    jsonResponse(401, { error: { code: 'invalid_credentials', message: 'wrong current password' } })
  )
  const failApi = createSystemApi(
    new ApiClient({ fetchImpl: failRecorder.fetch, onUnauthorized: (error) => seen.push(error.code) })
  )
  await captureError(() => failApi.changePassword('wrong', 'new-password'))
  assert.deepEqual(seen, [])
})

test('createSystemApi restartRtHost posts the rt-host target and parses the receipt', async () => {
  const recorder = createRecordingFetch(() =>
    jsonResponse(200, { ok: true, target: 'rt-host', restarted: true, restarted_at: '2026-11-26T08:00:00Z' })
  )
  const client = new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-token-2' })
  const api = createSystemApi(client)
  const result = await api.restartRtHost()
  assert.equal(result.ok, true)
  assert.equal(result.target, 'rt-host')
  assert.equal(recorder.calls[0].url, '/api/system/restart')
  assert.equal(recorder.calls[0].method, 'POST')
  assert.deepEqual(JSON.parse(String(recorder.calls[0].body)), { target: 'rt-host' })
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], 'csrf-token-2')
})

test('createSystemApi lets a 401 on /api/system/info reach the shared unauthorized hook', async () => {
  const seen: string[] = []
  const recorder = createRecordingFetch(() =>
    jsonResponse(401, { error: { code: 'not_authenticated', message: 'auth required' } })
  )
  const client = new ApiClient({ fetchImpl: recorder.fetch, onUnauthorized: (error) => seen.push(error.code) })
  const api = createSystemApi(client)
  const error = await captureError(() => api.fetchInfo())
  assert.equal(error.status, 401)
  assert.deepEqual(seen, ['not_authenticated'])
})
