import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, isApiError } from '../src/api/client.ts'
import { createAuthApi } from '../src/api/auth.ts'
import { createRecordingFetch, emptyResponse, jsonResponse } from '../scripts/mock-test-support.mjs'

function build(handler, csrfToken = () => null) {
  const recorder = createRecordingFetch(handler)
  const client = new ApiClient({ fetchImpl: recorder.fetch, csrfToken })
  return { recorder, client, auth: createAuthApi(client) }
}

async function expectApiError(run, expectedStatus, expectedCode) {
  try {
    await run()
  } catch (error) {
    assert.ok(isApiError(error))
    assert.equal(error.status, expectedStatus)
    if (expectedCode !== undefined) {
      assert.equal(error.code, expectedCode)
    }
    return error
  }
  assert.fail('expected the call to reject')
}

test('status() calls GET /api/auth/status without CSRF', async () => {
  const { recorder, auth } = build(() => jsonResponse(200, { configured: true, authenticated: false }))
  const status = await auth.status()
  assert.deepEqual(status, { configured: true, authenticated: false })
  assert.equal(recorder.calls[0].url, '/api/auth/status')
  assert.equal(recorder.calls[0].method, 'GET')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], undefined)
})

test('status() rejects malformed bodies instead of faking a state', async () => {
  const { auth } = build(() => jsonResponse(200, { configured: 'yes' }))
  await expectApiError(() => auth.status(), 200, 'invalid_response')
})

test('setup() posts password+confirm (no CSRF) and returns the token payload', async () => {
  const { recorder, auth } = build(
    () => jsonResponse(201, { csrf: 'csrf-1', expires_in: 28800 }),
    () => 'stale-csrf'
  )
  const token = await auth.setup('correct horse', 'correct horse')
  assert.deepEqual(token, { csrf: 'csrf-1', expires_in: 28800 })
  assert.equal(recorder.calls[0].url, '/api/auth/setup')
  assert.equal(recorder.calls[0].method, 'POST')
  assert.equal(recorder.calls[0].body, '{"password":"correct horse","confirm":"correct horse"}')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], undefined)
})

test('setup() rejects a token payload without expires_in', async () => {
  const { auth } = build(() => jsonResponse(201, { csrf: 'csrf-1' }))
  await expectApiError(() => auth.setup('password-one', 'password-one'), 200, 'invalid_response')
})

test('login() posts only the password and propagates 401', async () => {
  const { recorder, auth } = build(() => jsonResponse(401, { error: { code: 'invalid_credentials', message: 'wrong password' } }))
  const error = await expectApiError(() => auth.login('nope'), 401, 'invalid_credentials')
  assert.equal(error.message, 'wrong password')
  assert.equal(recorder.calls[0].url, '/api/auth/login')
  assert.equal(recorder.calls[0].body, '{"password":"nope"}')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], undefined)
})

test('session() returns csrf and expiry', async () => {
  const { recorder, auth } = build(() =>
    jsonResponse(200, { authenticated: true, csrf: 'csrf-live', expires_at: '2026-11-27T00:00:00.000Z' })
  )
  const session = await auth.session()
  assert.deepEqual(session, {
    authenticated: true,
    csrf: 'csrf-live',
    expires_at: '2026-11-27T00:00:00.000Z'
  })
  assert.equal(recorder.calls[0].url, '/api/auth/session')
  assert.equal(recorder.calls[0].method, 'GET')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], undefined)
})

test('session() rejects an empty csrf', async () => {
  const { auth } = build(() => jsonResponse(200, { authenticated: true, csrf: '', expires_at: 'x' }))
  await expectApiError(() => auth.session(), 200, 'invalid_response')
})

test('logout() posts with the session CSRF and tolerates an empty 204', async () => {
  const { recorder, auth } = build(() => emptyResponse(204), () => 'csrf-live')
  assert.equal(await auth.logout(), undefined)
  assert.equal(recorder.calls[0].url, '/api/auth/logout')
  assert.equal(recorder.calls[0].method, 'POST')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], 'csrf-live')
})

test('logout() does not fire the global 401 handler (store owns the cleanup)', async () => {
  let notified = 0
  const recorder = createRecordingFetch(() => jsonResponse(401, { error: { code: 'not_authenticated', message: 'x' } }))
  const client = new ApiClient({
    fetchImpl: recorder.fetch,
    csrfToken: () => 'csrf-live',
    onUnauthorized: () => {
      notified += 1
    }
  })
  await expectApiError(() => createAuthApi(client).logout(), 401, 'not_authenticated')
  assert.equal(notified, 0)
})

test('changePassword() posts {old,new} with CSRF and surfaces wrong old password', async () => {
  const { recorder, auth } = build(
    () => jsonResponse(401, { error: { code: 'invalid_credentials', message: 'wrong current password' } }),
    () => 'csrf-live'
  )
  const error = await expectApiError(() => auth.changePassword('old', 'new-password'), 401, 'invalid_credentials')
  assert.equal(recorder.calls[0].url, '/api/auth/password')
  assert.equal(recorder.calls[0].body, '{"old":"old","new":"new-password"}')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], 'csrf-live')
  assert.equal(error.message, 'wrong current password')
})
