import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiError } from '../src/api/client.ts'
import { createInitialSessionState, createSessionController } from '../src/stores/session.ts'
import { createFakeAuth } from '../scripts/mock-test-support.mjs'

const NOW = Date.parse('2026-11-26T12:00:00.000Z')

function apiError(status, code) {
  return new ApiError({ status, code, message: 'backend message' })
}

function createHarness(overrides = {}) {
  const state = createInitialSessionState()
  const fake = createFakeAuth(overrides)
  const notifications = []
  const controller = createSessionController(state, {
    auth: fake.auth,
    onUnauthorized: () => notifications.push('redirect'),
    now: () => NOW
  })
  return { state, calls: fake.calls, controller, notifications }
}

function deferred() {
  let resolve
  let reject
  const promise = new Promise((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

test('ensureSession detects an unconfigured daemon', async () => {
  const h = createHarness()
  const status = await h.controller.ensureSession()
  assert.equal(status, 'unauthenticated')
  assert.equal(h.state.status, 'unauthenticated')
  assert.equal(h.state.authenticated, false)
  assert.equal(h.state.configured, false)
  assert.equal(h.state.checked, true)
  assert.equal(h.state.csrfToken, null)
  assert.equal(h.state.expiresAt, null)
  assert.equal(h.calls.status, 1)
  assert.equal(h.calls.session, 0)
})

test('ensureSession loads csrf/expiry from the session endpoint', async () => {
  let statusCalls = 0
  let sessionCalls = 0
  const h = createHarness({
    status: async () => {
      statusCalls += 1
      return { configured: true, authenticated: true }
    },
    session: async () => {
      sessionCalls += 1
      return { authenticated: true, csrf: 'csrf-session', expires_at: '2026-11-27T00:00:00.000Z' }
    }
  })
  const status = await h.controller.ensureSession()
  assert.equal(status, 'authenticated')
  assert.equal(h.state.configured, true)
  assert.equal(h.state.csrfToken, 'csrf-session')
  assert.equal(h.state.expiresAt, '2026-11-27T00:00:00.000Z')
  assert.equal(statusCalls, 1)
  assert.equal(sessionCalls, 1)
})

test('status says authenticated but the session probe says 401 -> unauthenticated', async () => {
  const h = createHarness({
    status: async () => ({ configured: true, authenticated: true }),
    session: async () => {
      throw apiError(401, 'not_authenticated')
    }
  })
  const status = await h.controller.ensureSession()
  assert.equal(status, 'unauthenticated')
  assert.equal(h.state.authenticated, false)
  assert.equal(h.state.configured, true)
  assert.equal(h.state.lastError, 'session_expired')
  assert.equal(h.state.csrfToken, null)
  assert.equal(h.notifications.length, 0)
})

test('unreachable daemon stays unknown (never fakes a login)', async () => {
  const h = createHarness({
    status: async () => {
      throw apiError(0, 'network_error')
    }
  })
  const status = await h.controller.ensureSession()
  assert.equal(status, 'unknown')
  assert.equal(h.state.status, 'unknown')
  assert.equal(h.state.authenticated, false)
  assert.equal(h.state.configured, null)
  assert.equal(h.state.lastError, 'network_error')
  assert.equal(h.state.checked, true)
})

test('concurrent ensureSession calls share a single status request', async () => {
  let statusCalls = 0
  const h = createHarness({
    status: async () => {
      statusCalls += 1
      await Promise.resolve()
      return { configured: false, authenticated: false }
    }
  })
  const results = await Promise.all(Array.from({ length: 5 }, () => h.controller.ensureSession()))
  assert.deepEqual(results, ['unauthenticated', 'unauthenticated', 'unauthenticated', 'unauthenticated', 'unauthenticated'])
  assert.equal(statusCalls, 1)
})

test('cached session is reused until force is requested', async () => {
  const h = createHarness()
  await h.controller.ensureSession()
  await h.controller.ensureSession()
  assert.equal(h.calls.status, 1)
  await h.controller.ensureSession({ force: true })
  assert.equal(h.calls.status, 2)
})

test('an expired local session triggers a re-probe', async () => {
  let statusCalls = 0
  const h = createHarness({
    status: async () => {
      statusCalls += 1
      return { configured: true, authenticated: true }
    }
  })
  await h.controller.ensureSession()
  assert.equal(statusCalls, 1)
  h.state.expiresAt = new Date(NOW - 1000).toISOString()
  await h.controller.ensureSession()
  assert.equal(statusCalls, 2)
})

test('login stores csrf, computes expiresAt and marks configured', async () => {
  const h = createHarness()
  const result = await h.controller.login('correct horse')
  assert.deepEqual(result, { ok: true })
  assert.equal(h.state.authenticated, true)
  assert.equal(h.state.status, 'authenticated')
  assert.equal(h.state.configured, true)
  assert.equal(h.state.csrfToken, 'csrf-login')
  assert.equal(h.state.expiresAt, new Date(NOW + 28800 * 1000).toISOString())
  assert.equal(h.state.lastError, null)
  assert.equal(h.state.busy, false)
  assert.equal(h.state.checked, true)
})

test('login failure is classified and leaves the session unauthenticated', async () => {
  const h = createHarness({
    login: async () => {
      throw apiError(401, 'invalid_credentials')
    }
  })
  const result = await h.controller.login('nope')
  assert.equal(result.ok, false)
  assert.equal(result.code, 'invalid_credentials')
  assert.equal(result.i18nKey, 'error.auth.invalidCredentials')
  assert.match(result.error.message, /backend message/)
  assert.equal(h.state.lastError, 'invalid_credentials')
  assert.equal(h.state.authenticated, false)
  assert.equal(h.state.busy, false)
})

test('rate-limited login is surfaced as rate_limited', async () => {
  const h = createHarness({
    login: async () => {
      throw apiError(429, 'rate_limited')
    }
  })
  const result = await h.controller.login('nope')
  assert.equal(result.ok, false)
  assert.equal(result.code, 'rate_limited')
  assert.equal(h.state.lastError, 'rate_limited')
})

test('setup success sets configured and stores the csrf token', async () => {
  const h = createHarness()
  const result = await h.controller.setup('correct horse', 'correct horse')
  assert.deepEqual(result, { ok: true })
  assert.equal(h.state.configured, true)
  assert.equal(h.state.authenticated, true)
  assert.equal(h.state.csrfToken, 'csrf-setup')
  assert.equal(h.calls.setup, 1)
})

test('setup 409 already_configured switches the store to configured', async () => {
  const h = createHarness({
    setup: async () => {
      throw apiError(409, 'already_configured')
    }
  })
  const result = await h.controller.setup('correct horse', 'correct horse')
  assert.equal(result.ok, false)
  assert.equal(result.code, 'already_configured')
  assert.equal(h.state.configured, true)
  assert.equal(h.state.authenticated, false)
})

test('busy is true while a login request is in flight', async () => {
  const pending = deferred()
  const h = createHarness({ login: () => pending.promise })
  const loginPromise = h.controller.login('correct horse')
  assert.equal(h.state.busy, true)
  pending.resolve({ csrf: 'csrf-login', expires_in: 3600 })
  const result = await loginPromise
  assert.deepEqual(result, { ok: true })
  assert.equal(h.state.busy, false)
})

test('logout calls the endpoint, clears state but keeps configured', async () => {
  const h = createHarness()
  await h.controller.login('correct horse')
  await h.controller.logout()
  assert.equal(h.calls.logout, 1)
  assert.equal(h.state.authenticated, false)
  assert.equal(h.state.status, 'unauthenticated')
  assert.equal(h.state.csrfToken, null)
  assert.equal(h.state.expiresAt, null)
  assert.equal(h.state.configured, true)
  assert.equal(h.state.checked, false)
  await h.controller.ensureSession()
  assert.equal(h.calls.status, 1)
})

test('logout clears local state even when the request fails', async () => {
  const h = createHarness({
    logout: async () => {
      throw apiError(0, 'network_error')
    }
  })
  await h.controller.login('correct horse')
  await h.controller.logout()
  assert.equal(h.state.authenticated, false)
  assert.equal(h.state.csrfToken, null)
  assert.equal(h.state.status, 'unauthenticated')
})

test('handleUnauthorized clears the session and redirects only once', async () => {
  const h = createHarness()
  await h.controller.login('correct horse')
  h.controller.handleUnauthorized()
  h.controller.handleUnauthorized()
  assert.deepEqual(h.notifications, ['redirect'])
  assert.equal(h.state.authenticated, false)
  assert.equal(h.state.status, 'unauthenticated')
  assert.equal(h.state.csrfToken, null)
  assert.equal(h.state.lastError, 'session_expired')
})

test('a fresh login re-arms the 401 redirect', async () => {
  const h = createHarness()
  await h.controller.login('correct horse')
  h.controller.handleUnauthorized()
  await h.controller.login('correct horse')
  h.controller.handleUnauthorized()
  assert.deepEqual(h.notifications, ['redirect', 'redirect'])
})

test('isExpired compares expiresAt against the injected clock', () => {
  const h = createHarness()
  assert.equal(h.controller.isExpired(), false)
  h.state.expiresAt = new Date(NOW + 1000).toISOString()
  assert.equal(h.controller.isExpired(), false)
  h.state.expiresAt = new Date(NOW - 1000).toISOString()
  assert.equal(h.controller.isExpired(), true)
  h.state.expiresAt = 'not-a-date'
  assert.equal(h.controller.isExpired(), false)
})

test('a later force probe recovers from an unreachable daemon', async () => {
  let fail = true
  const h = createHarness({
    status: async () => {
      if (fail) {
        throw apiError(0, 'network_error')
      }
      return { configured: false, authenticated: false }
    }
  })
  assert.equal(await h.controller.ensureSession(), 'unknown')
  fail = false
  assert.equal(await h.controller.ensureSession({ force: true }), 'unauthenticated')
  assert.equal(h.state.configured, false)
  assert.equal(h.state.lastError, null)
})

test('clear() resets the session to the initial unauthenticated state', async () => {
  const h = createHarness()
  await h.controller.login('correct horse')
  h.controller.clear()
  assert.equal(h.state.status, 'unauthenticated')
  assert.equal(h.state.authenticated, false)
  assert.equal(h.state.csrfToken, null)
  assert.equal(h.state.checked, false)
  assert.equal(h.state.lastError, null)
})
