import { test } from 'node:test'
import assert from 'node:assert/strict'
import { validateCredentials } from '../src/auth/validation.ts'

const MIN = 8

test('signin requires a non-empty password', () => {
  assert.deepEqual(validateCredentials('signin', '', '', MIN), { ok: false, key: 'login.validation.required' })
  assert.deepEqual(validateCredentials('signin', 'short', '', MIN), { ok: true })
})

test('setup requires a non-empty password', () => {
  assert.deepEqual(validateCredentials('setup', '', '', MIN), { ok: false, key: 'login.validation.required' })
})

test('setup enforces the minimum length', () => {
  assert.deepEqual(validateCredentials('setup', 'a'.repeat(MIN - 1), 'a'.repeat(MIN - 1), MIN), {
    ok: false,
    key: 'login.validation.tooShort'
  })
  assert.deepEqual(validateCredentials('setup', 'a'.repeat(MIN), 'a'.repeat(MIN), MIN), { ok: true })
})

test('setup requires both passwords to match', () => {
  assert.deepEqual(validateCredentials('setup', 'password-one', 'password-two', MIN), {
    ok: false,
    key: 'login.validation.mismatch'
  })
})

test('setup checks length before mismatch', () => {
  assert.deepEqual(validateCredentials('setup', 'short', 'different', MIN), {
    ok: false,
    key: 'login.validation.tooShort'
  })
})

test('non-finite minimum falls back to 1 character', () => {
  assert.deepEqual(validateCredentials('setup', 'a', 'a', Number.NaN), { ok: true })
  assert.deepEqual(validateCredentials('setup', 'a', 'a', 0), { ok: true })
})
