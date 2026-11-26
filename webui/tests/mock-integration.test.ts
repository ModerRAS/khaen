import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, isApiError } from '../src/api/client.ts'
import { createAuthApi } from '../src/api/auth.ts'
import { createInitialSessionState, createSessionController } from '../src/stores/session.ts'
import { isStatusStale, parseStatusResponse } from '../src/api/status.ts'
import { createMockAuthServer } from '../scripts/mock-auth-server.mjs'
import { STATUS_FIXTURES, buildStatusFixture } from '../scripts/status-fixtures.mjs'

const PASSWORD = 'correct-horse-battery-staple'
const NEW_PASSWORD = 'another-good-passphrase'
const HEX_64 = /^[0-9a-f]{64}$/

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
  return { jar, state, client, auth, controller, notifications }
}

async function withMock(run) {
  const mock = createMockAuthServer()
  const address = await mock.listen()
  try {
    await run({ mock, baseUrl: address.baseUrl })
  } finally {
    await mock.close()
  }
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

test('mock contract: setup -> session -> password change -> logout -> login', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)

    assert.equal(await h.controller.ensureSession(), 'unauthenticated')
    assert.equal(h.state.configured, false)

    const setupResult = await h.controller.setup(PASSWORD, PASSWORD)
    assert.deepEqual(setupResult, { ok: true })
    assert.equal(h.state.authenticated, true)
    assert.equal(h.state.configured, true)
    assert.match(h.state.csrfToken, HEX_64)
    assert.notEqual(h.jar.cookie(), null)
    assert.ok(Date.parse(h.state.expiresAt) > Date.now())

    const status = await h.client.get('/api/auth/status')
    assert.deepEqual(status, { configured: true, authenticated: true })

    const session = await h.auth.session()
    assert.equal(session.authenticated, true)
    assert.equal(session.csrf, h.state.csrfToken)

    assert.equal(await h.controller.ensureSession({ force: true }), 'authenticated')

    const oldCsrf = h.state.csrfToken
    assert.equal(await h.auth.changePassword(PASSWORD, NEW_PASSWORD), undefined)
    assert.equal(await h.controller.ensureSession({ force: true }), 'authenticated')
    assert.match(h.state.csrfToken, HEX_64)
    assert.notEqual(h.state.csrfToken, oldCsrf)

    await h.controller.logout()
    assert.equal(h.jar.cookie(), null)
    assert.equal(h.state.authenticated, false)
    assert.equal(h.state.configured, true)
    assert.equal(await h.controller.ensureSession({ force: true }), 'unauthenticated')

    const loginResult = await h.controller.login(NEW_PASSWORD)
    assert.deepEqual(loginResult, { ok: true })
    assert.equal(h.state.authenticated, true)
    const statusAfterLogin = await h.client.get('/api/auth/status')
    assert.deepEqual(statusAfterLogin, { configured: true, authenticated: true })
  })
})

test('mock contract: second setup is rejected with 409 already_configured', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    assert.equal(await h.controller.ensureSession(), 'unauthenticated')
    assert.deepEqual(await h.controller.setup(PASSWORD, PASSWORD), { ok: true })

    const freshState = createInitialSessionState()
    const freshJar = createJarFetch()
    const freshClient = new ApiClient({ baseUrl, fetchImpl: freshJar.fetch })
    const freshController = createSessionController(freshState, { auth: createAuthApi(freshClient) })
    const result = await freshController.setup(PASSWORD, PASSWORD)
    assert.equal(result.ok, false)
    assert.equal(result.code, 'already_configured')
    assert.equal(freshState.configured, true)
  })
})

test('mock contract: wrong passwords get 401 and the sixth attempt is rate limited (429 + Retry-After)', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await h.controller.ensureSession()
    await h.controller.setup(PASSWORD, PASSWORD)
    await h.controller.logout()

    for (let attempt = 0; attempt < 5; attempt += 1) {
      const error = await expectApiError(() => h.auth.login('wrong-password-' + String(attempt)))
      assert.equal(error.status, 401)
      assert.equal(error.code, 'invalid_credentials')
    }
    const limited = await expectApiError(() => h.auth.login('wrong-password-again'))
    assert.equal(limited.status, 429)
    assert.equal(limited.code, 'rate_limited')
    assert.equal(limited.retryAfterSeconds, 60)

    const stillLimited = await expectApiError(() => h.auth.login(PASSWORD))
    assert.equal(stillLimited.status, 429)
  })
})

test('mock contract: session-bound writes without CSRF are rejected with 403', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await h.controller.ensureSession()
    await h.controller.setup(PASSWORD, PASSWORD)

    // Reuse the authenticated cookie but talk through clients without a (valid) CSRF token.
    const noCsrfClient = new ApiClient({ baseUrl, fetchImpl: (url, init) => h.jar.fetch(url, init) })
    const noCsrfAuth = createAuthApi(noCsrfClient)
    const first = await expectApiError(() => noCsrfAuth.logout())
    assert.equal(first.status, 403)
    assert.equal(first.code, 'csrf_invalid')

    const wrongCsrfClient = new ApiClient({
      baseUrl,
      fetchImpl: (url, init) => h.jar.fetch(url, init),
      csrfToken: () => 'deadbeef'.repeat(8)
    })
    const wrongCsrfAuth = createAuthApi(wrongCsrfClient)
    const second = await expectApiError(() => wrongCsrfAuth.logout())
    assert.equal(second.status, 403)
    assert.equal(second.code, 'csrf_invalid')
  })
})

test('mock contract: anonymous /api/status 401 triggers the client 401 hook', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    const error = await expectApiError(() => h.client.get('/api/status'))
    assert.equal(error.status, 401)
    assert.equal(error.code, 'not_authenticated')
    assert.deepEqual(h.notifications, ['client-401'])
  })
})

test('mock contract: /api/status returns the present khaen-status/v1 fixture when authenticated', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await h.controller.ensureSession()
    await h.controller.setup(PASSWORD, PASSWORD)

    const status = parseStatusResponse(await h.client.get('/api/status'))
    assert.equal(status.schema, 'khaen-status/v1')
    assert.equal(status.daemon.version, '0.3.0-mock')
    assert.equal(status.host_link.connected, true)
    assert.equal(isStatusStale(status), false)
    assert.equal(status.state.value, 'running')
    assert.equal(status.plan.version, 3)
    assert.equal(status.format.aes67.pt, 97)
    assert.equal(status.input.underruns, 21)
    assert.equal(status.output.blocks, 352000)
    assert.equal(status.device.state, 'present')
    assert.equal(status.calibration.last_verdict, 'PASS')
    assert.equal(status.not_wired.length, 2)
  })
})

test('mock contract: stale fixture flags the control link as stale', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await h.controller.ensureSession()
    await h.controller.setup(PASSWORD, PASSWORD)
    mock.setStatusFixture('stale')

    const status = parseStatusResponse(await h.client.get('/api/status'))
    assert.equal(status.host_link.connected, false)
    assert.equal(status.host_link.status_age_ms, 15000)
    assert.equal(isStatusStale(status), true)
    assert.equal(status.state.value, 'faulted')
    assert.match(String(status.state.last_error), /E_HOST_LINK_LOST/)
    assert.equal(status.reasons['host_link.socket'], 'rt-host did not report its socket path')
  })
})

test('mock contract: missing fixture keeps every value null with reasons', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await h.controller.ensureSession()
    await h.controller.setup(PASSWORD, PASSWORD)
    mock.setStatusFixture('missing')

    const status = parseStatusResponse(await h.client.get('/api/status'))
    assert.equal(status.daemon.version, null)
    assert.equal(status.host_link.connected, null)
    assert.equal(status.state.value, null)
    assert.equal(status.plan.sha256, null)
    assert.deepEqual(status.format.aes67, { channels: null, map: null, pt: null, group: null, port: null })
    assert.equal(status.input.packets, null)
    assert.equal(status.output.blocks, null)
    assert.equal(status.device.id, null)
    assert.equal(status.calibration.last_verdict, null)
    assert.equal(isStatusStale(status), false)
    assert.ok(Object.keys(status.reasons).length >= 10)
    assert.equal(status.reasons['format.device_rate'], 'rt-host did not report the device rate')
    assert.equal(status.reasons['input.packets'], 'AES67 receiver not started')
    assert.equal(status.not_wired.length, 2)
  })
})

test('mock contract: status fixture can be switched at runtime and rejects unknown names', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await h.controller.ensureSession()
    await h.controller.setup(PASSWORD, PASSWORD)

    assert.equal(mock.statusFixture, 'present')
    assert.equal(parseStatusResponse(await h.client.get('/api/status')).state.value, 'running')

    mock.setStatusFixture('missing')
    assert.equal(mock.statusFixture, 'missing')
    assert.equal(parseStatusResponse(await h.client.get('/api/status')).state.value, null)

    mock.setStatusFixture('present')
    assert.equal(parseStatusResponse(await h.client.get('/api/status')).state.value, 'running')

    assert.throws(() => mock.setStatusFixture('nope'), /unknown status fixture/)
  })
})

test('fixtures are deep copies and expose exactly three honest shapes', () => {
  assert.deepEqual(STATUS_FIXTURES, ['present', 'stale', 'missing'])
  const first = buildStatusFixture('present')
  first.state.value = 'mutated'
  assert.equal(buildStatusFixture('present').state.value, 'running')
  assert.equal(buildStatusFixture('unknown-name').state.value, 'running')
})
