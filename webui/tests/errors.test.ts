import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiError } from '../src/api/client.ts'
import { classifyAuthError } from '../src/api/errors.ts'

function apiError(status: number, code: string, message = 'backend message', retryAfterSeconds: number | null = null): ApiError {
  return new ApiError({ status, code, message, retryAfterSeconds })
}

test('401 on the login form is a wrong password', () => {
  const info = classifyAuthError(apiError(401, 'invalid_credentials'), 'login')
  assert.equal(info.code, 'invalid_credentials')
  assert.equal(info.i18nKey, 'error.auth.invalidCredentials')
  assert.equal(info.status, 401)
  assert.equal(info.detail, 'backend message')
})

test('401 in a password-change context means the current password is wrong', () => {
  const info = classifyAuthError(apiError(401, 'invalid_credentials'), 'changePassword')
  assert.equal(info.code, 'invalid_current_password')
  assert.equal(info.i18nKey, 'error.auth.invalidCurrentPassword')
})

test('401 in a generic context means the session is gone', () => {
  const info = classifyAuthError(apiError(401, 'invalid_credentials'), 'generic')
  assert.equal(info.code, 'unauthenticated')
  assert.equal(info.i18nKey, 'error.auth.unauthenticated')
})

test('401 during setup is classified as a validation problem', () => {
  assert.equal(classifyAuthError(apiError(401, 'mystery'), 'setup').code, 'validation')
})

test('429 rate limiting keeps the Retry-After seconds', () => {
  const info = classifyAuthError(apiError(429, 'rate_limited', 'slow down', 60), 'login')
  assert.equal(info.code, 'rate_limited')
  assert.equal(info.i18nKey, 'error.auth.rateLimited')
  assert.equal(info.retryAfterSeconds, 60)
})

test('409 already configured is recognized', () => {
  const info = classifyAuthError(apiError(409, 'already_configured'), 'setup')
  assert.equal(info.code, 'already_configured')
  assert.equal(info.i18nKey, 'error.auth.alreadyConfigured')
})

test('409 not configured is recognized', () => {
  const info = classifyAuthError(apiError(409, 'not_configured'), 'login')
  assert.equal(info.code, 'not_configured')
  assert.equal(info.i18nKey, 'error.auth.notConfigured')
})

test('403 is a CSRF failure', () => {
  const info = classifyAuthError(apiError(403, 'csrf_missing'), 'generic')
  assert.equal(info.code, 'csrf_invalid')
  assert.equal(info.i18nKey, 'error.auth.csrfInvalid')
})

test('400 is a validation failure', () => {
  const info = classifyAuthError(apiError(400, 'mystery'), 'setup')
  assert.equal(info.code, 'validation')
  assert.equal(info.i18nKey, 'error.auth.validation')
})

test('network failures are classified as unreachable', () => {
  const info = classifyAuthError(apiError(0, 'network_error'), 'generic')
  assert.equal(info.code, 'network')
  assert.equal(info.i18nKey, 'error.auth.network')
})

test('5xx responses are classified as server errors', () => {
  assert.equal(classifyAuthError(apiError(503, 'mystery'), 'generic').code, 'server')
})

test('session_expired backend code wins over the status', () => {
  const info = classifyAuthError(apiError(401, 'session_expired'), 'generic')
  assert.equal(info.code, 'session_expired')
  assert.equal(info.i18nKey, 'error.auth.sessionExpired')
})

test('unrecognized statuses fall back to unknown', () => {
  assert.equal(classifyAuthError(apiError(418, 'teapot'), 'generic').code, 'unknown')
})

test('non-ApiError values are unknown with a detail message', () => {
  const info = classifyAuthError(new Error('boom'), 'login')
  assert.equal(info.code, 'unknown')
  assert.equal(info.i18nKey, 'error.auth.unknown')
  assert.equal(info.status, 0)
  assert.equal(info.detail, 'boom')
})

test('every normalized code maps to a distinct i18n key', () => {
  const codes = [
    classifyAuthError(apiError(401, 'invalid_credentials'), 'login'),
    classifyAuthError(apiError(401, 'invalid_credentials'), 'changePassword'),
    classifyAuthError(apiError(429, 'rate_limited'), 'login'),
    classifyAuthError(apiError(409, 'already_configured'), 'setup'),
    classifyAuthError(apiError(409, 'not_configured'), 'login'),
    classifyAuthError(apiError(403, 'csrf_missing'), 'generic'),
    classifyAuthError(apiError(400, 'mystery'), 'setup'),
    classifyAuthError(apiError(401, 'not_authenticated'), 'generic'),
    classifyAuthError(apiError(401, 'session_expired'), 'generic'),
    classifyAuthError(apiError(0, 'network_error'), 'generic'),
    classifyAuthError(apiError(503, 'mystery'), 'generic'),
    classifyAuthError(apiError(418, 'teapot'), 'generic')
  ]
  const keys = codes.map((info) => info.i18nKey)
  assert.equal(new Set(keys).size, keys.length)
  for (const key of keys) {
    assert.match(key, /^error\.auth\.[A-Za-z]+$/)
  }
})
