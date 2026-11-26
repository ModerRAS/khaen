import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, isApiError } from '../src/api/client.ts'
import { createAuthApi } from '../src/api/auth.ts'
import { createInitialSessionState, createSessionController } from '../src/stores/session.ts'
import {
  classifySystemError,
  createSystemApi,
  isMotuDevice,
  versionConsistency
} from '../src/api/system.ts'
import { createMockAuthServer } from '../scripts/mock-auth-server.mjs'
import { SYSTEM_FIXTURES } from '../scripts/system-fixtures.mjs'

const PASSWORD = 'khaen-system-mock-passphrase'
const NEW_PASSWORD = 'khaen-system-mock-passphrase-2'
const HEX_64 = /^[0-9a-f]{64}$/
const DIAGNOSTICS_FILENAME = /^khaen-diagnostics-v0\.1\.2-\d{8}T\d{6}Z\.json$/

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
  const auth = createAuthApi(client)
  const controller = createSessionController(state, {
    auth,
    onUnauthorized: () => notifications.push('redirect')
  })
  return { jar, state, client, auth, controller, notifications, system: createSystemApi(client) }
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

async function signIn(h) {
  await h.controller.ensureSession()
  const result = await h.controller.setup(PASSWORD, PASSWORD)
  assert.deepEqual(result, { ok: true })
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

test('mock contract: anonymous /healthz reports the release version without a session', async () => {
  await withMock(async ({ baseUrl }) => {
    const response = await fetch(baseUrl + '/healthz')
    assert.equal(response.status, 200)
    const body = await response.json()
    assert.deepEqual(body.ok, true)
    assert.equal(body.version, 'v0.1.2')
    assert.equal(body.uptime_s, 18342)
  })
})

test('mock contract: /api/system/info requires a session and a 401 reaches the shared hook', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    const error = await expectApiError(() => h.system.fetchInfo())
    assert.equal(error.status, 401)
    assert.equal(error.code, 'not_authenticated')
    assert.deepEqual(h.notifications, ['client-401'])
  })
})

test('mock contract: info fixture separates crate/release/commit and flags the mismatch', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const info = await h.system.fetchInfo()
    assert.equal(info.crate_version, '0.1.0')
    assert.equal(info.release_version, 'v0.1.2')
    assert.equal(info.commit, '855f4c4')
    assert.equal(info.build_profile, 'release')
    assert.equal(versionConsistency(info.crate_version, info.release_version), 'mismatch')
    assert.equal(info.services.daemon.active, true)
    assert.equal(info.services.daemon.pid, 4211)
    assert.equal(info.services.rt_host.active, false)
    assert.equal(info.services.rt_host.pid, null)
    assert.match(String(info.reasons['services.rt_host.pid']), /not running/)
    assert.equal(info.devices.length, 2)
    assert.equal(info.devices.some((device) => isMotuDevice(device)), true)
    assert.equal(info.host_link.connected, false)
    assert.equal(info.restart.allowed, true)
    assert.equal(info.restart.target, 'rt-host')

    const healthz = await h.system.fetchHealthz()
    assert.equal(healthz.version, 'v0.1.2')
  })
})

test('mock contract: /api/system/info fixtures can be switched at runtime', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)

    mock.setSystemFixture('consistent')
    const consistent = await h.system.fetchInfo()
    assert.equal(consistent.crate_version, '0.1.2')
    assert.equal(consistent.release_version, 'v0.1.2')
    assert.equal(versionConsistency(consistent.crate_version, consistent.release_version), 'consistent')
    assert.equal(consistent.services.rt_host.active, true)
    assert.equal(consistent.services.rt_host.pid, 4247)
    assert.equal(consistent.host_link.connected, true)
    assert.deepEqual(consistent.reasons, {})

    mock.setSystemFixture('degraded')
    const degraded = await h.system.fetchInfo()
    assert.equal(degraded.release_version, null)
    assert.equal(degraded.commit, null)
    assert.equal(degraded.hostname, null)
    assert.equal(degraded.uptime_s, null)
    assert.equal(degraded.services.daemon.active, null)
    assert.deepEqual(degraded.devices, [])
    assert.equal(degraded.restart.allowed, false)
    assert.equal(versionConsistency(degraded.crate_version, degraded.release_version), 'unknown')

    // degraded 时 /healthz 如实回退 crate 版本（release 版本未知）。
    const healthz = await h.system.fetchHealthz()
    assert.equal(healthz.version, '0.1.0')

    assert.throws(() => mock.setSystemFixture('nope'), /unknown system fixture/)
  })
})

test('mock contract: logs returns the requested unit/lines and validates parameters', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)

    const daemon = await h.system.fetchLogs({ unit: 'daemon', lines: 200 })
    assert.equal(daemon.unit, 'daemon')
    assert.equal(daemon.lines.length, 200)
    assert.match(daemon.lines[0], /khaen-daemon\[4211\]/)

    const rtHost = await h.system.fetchLogs({ unit: 'rt-host', lines: 3 })
    assert.equal(rtHost.unit, 'rt-host')
    assert.equal(rtHost.lines.length, 3)
    assert.match(rtHost.lines[0], /khaen-rt-host\[4247\]/)

    const badUnit = await expectApiError(() => h.client.get('/api/system/logs?unit=bogus'))
    assert.equal(badUnit.status, 400)
    assert.equal(badUnit.code, 'invalid_request')

    const badLines = await expectApiError(() => h.client.get('/api/system/logs?unit=daemon&lines=501'))
    assert.equal(badLines.status, 400)
    assert.equal(badLines.code, 'invalid_request')

    const notNumber = await expectApiError(() => h.client.get('/api/system/logs?unit=daemon&lines=abc'))
    assert.equal(notNumber.status, 400)
  })
})

test('mock contract: degraded logs return 503 logs_unavailable with a reason', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)

    mock.setSystemFixture('degraded')
    const error = await expectApiError(() => h.system.fetchLogs({ unit: 'daemon', lines: 100 }))
    assert.equal(error.status, 503)
    assert.equal(error.code, 'logs_unavailable')
    const classified = classifySystemError(error)
    assert.equal(classified.category, 'unavailable')
    assert.match(String(classified.reason), /journalctl is not available/)
  })
})

test('mock contract: diagnostics downloads a versioned filename and never leaks secrets', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const download = await h.system.fetchDiagnostics()
    assert.ok(download.filename !== null)
    assert.match(String(download.filename), DIAGNOSTICS_FILENAME)
    assert.ok(download.json !== null)
    const body = download.json as Record<string, any>
    assert.equal(body.schema, 'khaen-diagnostics/v1')
    assert.equal(body.build.crate_version, '0.1.0')
    assert.equal(body.build.release_version, 'v0.1.2')
    assert.equal(body.build.commit, '855f4c4')
    assert.equal(body.info.crate_version, '0.1.0')
    assert.equal(body.status.schema, 'khaen-status/v1')
    assert.equal(body.devices.length, 2)
    assert.equal(body.logs.daemon.available, true)
    assert.equal(body.logs['rt-host'].available, true)

    // 敏感字段绝不出现：密码明文/哈希、CSRF、会话 cookie。
    const serialized = download.text.toLowerCase()
    assert.equal(serialized.includes(PASSWORD.toLowerCase()), false)
    assert.equal(serialized.includes('csrf'), false)
    assert.equal(serialized.includes('khaen_session'), false)
    assert.equal(serialized.includes('passwordhash'), false)
  })
})

test('mock contract: /api/system/password requires CSRF and the current password', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    assert.match(String(h.state.csrfToken), HEX_64)

    // 无 CSRF -> 403
    const noCsrf = new ApiClient({ baseUrl, fetchImpl: (url, init) => h.jar.fetch(url, init) })
    const noCsrfError = await expectApiError(() =>
      noCsrf.post('/api/system/password', { old: PASSWORD, new: NEW_PASSWORD })
    )
    assert.equal(noCsrfError.status, 403)
    assert.equal(noCsrfError.code, 'csrf_invalid')

    // 旧密码错误 -> 401 invalid_credentials（改密语境下映射为「当前密码不正确」）
    const wrongOld = await expectApiError(() => h.system.changePassword('wrong-password', NEW_PASSWORD))
    assert.equal(wrongOld.status, 401)
    assert.equal(wrongOld.code, 'invalid_credentials')

    // 新密码太短 -> 400
    const tooShort = await expectApiError(() => h.system.changePassword(PASSWORD, 'short'))
    assert.equal(tooShort.status, 400)
    assert.equal(tooShort.code, 'invalid_password')

    // 成功：旧会话失效（用旧 cookie 再请求 /api/system/info 得到 401），新密码可登录。
    const oldCookie = h.jar.cookie()
    assert.notEqual(oldCookie, null)
    await h.system.changePassword(PASSWORD, NEW_PASSWORD)

    const stale = await fetch(baseUrl + '/api/system/info', { headers: { Cookie: String(oldCookie) } })
    assert.equal(stale.status, 401)

    const login = await fetch(baseUrl + '/api/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password: NEW_PASSWORD })
    })
    assert.equal(login.status, 200)
    const newCookie = login.headers.getSetCookie().find((raw) => raw.startsWith('khaen_session='))
    assert.ok(newCookie)
    const info = await fetch(baseUrl + '/api/system/info', { headers: { Cookie: newCookie.split(';')[0] } })
    assert.equal(info.status, 200)
  })
})

test('mock contract: restart requires CSRF, a valid target and the backend capability', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)

    // 无 CSRF -> 403
    const noCsrf = new ApiClient({ baseUrl, fetchImpl: (url, init) => h.jar.fetch(url, init) })
    const noCsrfError = await expectApiError(() => noCsrf.post('/api/system/restart', { target: 'rt-host' }))
    assert.equal(noCsrfError.status, 403)
    assert.equal(noCsrfError.code, 'csrf_invalid')

    // 不支持的目标 -> 400
    const badTarget = await expectApiError(() => h.client.post('/api/system/restart', { target: 'daemon' }))
    assert.equal(badTarget.status, 400)
    assert.equal(badTarget.code, 'invalid_request')

    // 默认 fixture 允许重启：回执 + 之后 info 里 rt-host 视为已拉起。
    const before = await h.system.fetchInfo()
    assert.equal(before.services.rt_host.active, false)
    const receipt = await h.system.restartRtHost()
    assert.equal(receipt.ok, true)
    assert.equal(receipt.target, 'rt-host')
    assert.equal(receipt.restarted, true)
    assert.notEqual(receipt.restarted_at, null)
    assert.notEqual(mock.system.restartedAt, null)
    const after = await h.system.fetchInfo()
    assert.equal(after.services.rt_host.active, true)
    assert.equal(after.services.rt_host.pid, 4247)

    // degraded fixture：重启未启用 -> 403 restart_disabled + reason。
    mock.setSystemFixture('degraded')
    const disabled = await expectApiError(() => h.system.restartRtHost())
    assert.equal(disabled.status, 403)
    assert.equal(disabled.code, 'restart_disabled')
    const classified = classifySystemError(disabled)
    assert.equal(classified.category, 'disabled')
    assert.match(String(classified.reason), /KHAEN_ALLOW_RESTART/)

    // fixture 切换会清空 mock 的重启状态。
    assert.equal(mock.system.rtHostStarted, false)
  })
})

test('fixtures expose exactly the honest system shapes', async () => {
  assert.deepEqual(SYSTEM_FIXTURES, ['mismatch', 'consistent', 'degraded'])
})
