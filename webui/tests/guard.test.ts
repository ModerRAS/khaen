import { test } from 'node:test'
import assert from 'node:assert/strict'
import { postLoginPath, resolveNavigation, safeNext, type GuardTarget } from '../src/router/guard.ts'

function target(overrides: Partial<GuardTarget>): GuardTarget {
  return { fullPath: '/', ...overrides }
}

test('safeNext keeps same-origin relative paths', () => {
  assert.equal(safeNext('/'), '/')
  assert.equal(safeNext('/volume'), '/volume')
  assert.equal(safeNext('/volume?target=-6#slider'), '/volume?target=-6#slider')
  assert.equal(safeNext('/a/../b'), '/b')
  assert.equal(safeNext(' /system?tab=logs '), '/system?tab=logs')
})

test('safeNext rejects open-redirect payloads', () => {
  assert.equal(safeNext('https://evil.example/'), '/')
  assert.equal(safeNext('http://evil.example'), '/')
  assert.equal(safeNext('//evil.example/path'), '/')
  assert.equal(safeNext('/\\evil.example'), '/')
  assert.equal(safeNext('\\\\evil.example'), '/')
  assert.equal(safeNext('javascript:alert(1)'), '/')
  assert.equal(safeNext('volume'), '/')
  assert.equal(safeNext(''), '/')
  assert.equal(safeNext('   '), '/')
  assert.equal(safeNext('/bad\npath'), '/')
  assert.equal(safeNext(42), '/')
  assert.equal(safeNext(null), '/')
  assert.equal(safeNext(undefined), '/')
  assert.equal(safeNext('/' + 'a'.repeat(2048)), '/')
})

test('postLoginPath never bounces back to /login', () => {
  assert.equal(postLoginPath(undefined), '/')
  assert.equal(postLoginPath('/volume'), '/volume')
  assert.equal(postLoginPath('/login'), '/')
  assert.equal(postLoginPath('/login?next=%2Fvolume'), '/')
  assert.equal(postLoginPath('/login/setup'), '/')
  assert.equal(postLoginPath('//evil.example'), '/')
})

test('unauthenticated user is sent to /login with a next for protected routes', () => {
  const decision = resolveNavigation(
    target({ name: 'volume', fullPath: '/volume?target=-6', meta: { requiresAuth: true } }),
    'unauthenticated'
  )
  assert.deepEqual(decision, { kind: 'login', next: '/volume?target=-6' })
})

test('unknown session status is treated as unauthenticated (fail closed)', () => {
  const decision = resolveNavigation(target({ name: 'overview', fullPath: '/', meta: { requiresAuth: true } }), 'unknown')
  assert.deepEqual(decision, { kind: 'login', next: '/' })
})

test('authenticated user passes protected routes', () => {
  const decision = resolveNavigation(target({ name: 'overview', fullPath: '/', meta: { requiresAuth: true } }), 'authenticated')
  assert.deepEqual(decision, { kind: 'allow' })
})

test('anonymous user can open the login page', () => {
  const decision = resolveNavigation(target({ name: 'login', fullPath: '/login', meta: { public: true } }), 'unauthenticated')
  assert.deepEqual(decision, { kind: 'allow' })
})

test('login page stays usable when the backend is unreachable', () => {
  const decision = resolveNavigation(target({ name: 'login', fullPath: '/login', meta: { public: true } }), 'unknown')
  assert.deepEqual(decision, { kind: 'allow' })
})

test('authenticated user on /login is redirected to next (or /)', () => {
  assert.deepEqual(
    resolveNavigation(target({ name: 'login', fullPath: '/login', query: { next: '/volume' }, meta: { public: true } }), 'authenticated'),
    { kind: 'redirect', path: '/volume' }
  )
  assert.deepEqual(
    resolveNavigation(target({ name: 'login', fullPath: '/login', meta: { public: true } }), 'authenticated'),
    { kind: 'redirect', path: '/' }
  )
})

test('an evil next never escapes the app', () => {
  assert.deepEqual(
    resolveNavigation(
      target({ name: 'login', fullPath: '/login', query: { next: 'https://evil.example' }, meta: { public: true } }),
      'authenticated'
    ),
    { kind: 'redirect', path: '/' }
  )
  assert.deepEqual(
    resolveNavigation(
      target({ name: 'login', fullPath: '/login', query: { next: '/login' }, meta: { public: true } }),
      'authenticated'
    ),
    { kind: 'redirect', path: '/' }
  )
})

test('routes without auth metadata are allowed', () => {
  assert.deepEqual(resolveNavigation(target({ name: 'other', fullPath: '/other' }), 'unauthenticated'), { kind: 'allow' })
})
