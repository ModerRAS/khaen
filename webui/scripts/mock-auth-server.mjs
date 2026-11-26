#!/usr/bin/env node
/**
 * Contract mock for /api/auth/* — DEV / TEST ONLY (scripts/ is never bundled into dist).
 *
 * It implements exactly the PLAN §4.1/§4.2 frontend contract:
 *   GET  /api/auth/status   -> {configured, authenticated}            (anonymous)
 *   POST /api/auth/setup    -> 201 {csrf, expires_in} + Set-Cookie   (409 when configured)
 *   POST /api/auth/login    -> 200 {csrf, expires_in} + Set-Cookie   (401 wrong, 429 rate limited)
 *   GET  /api/auth/session  -> {authenticated, csrf, expires_at}     (401 when anonymous)
 *   POST /api/auth/logout   -> clears session (idempotent)
 *   POST /api/auth/password -> {old, new}, requires session + CSRF
 *   GET  /api/status        -> 401 without session; khaen-status/v1 fixture with session
 *                              (fixtures: present | stale | missing, see scripts/status-fixtures.mjs)
 *   GET  /healthz           -> {ok, version}                          (anonymous)
 *   GET  /__mock/status     -> {fixture, fixtures}                     (dev-only switch helper)
 *   POST /__mock/status-fixture {fixture} -> switch /api/status fixture (dev-only)
 *
 * Cookie: khaen_session=<opaque>; HttpOnly; SameSite=Strict; Path=/; Max-Age=28800
 * CSRF: X-KHAEN-CSRF must equal the session csrf for session-bound writes.
 * State is in-memory only (restart = fresh setup flow). It never replaces the real backend:
 * the real one is Rust/axum (task T34) and must be validated separately before M1 ships.
 *
 * Usage:
 *   node scripts/mock-auth-server.mjs                          # 127.0.0.1:8790
 *   node scripts/mock-auth-server.mjs --port 8730
 *   node scripts/mock-auth-server.mjs --status-fixture stale   # present | stale | missing
 *   KHAEN_DEV_API=http://127.0.0.1:8790 pnpm dev               # vite proxies /api to the mock
 */
import { createHash, randomBytes } from 'node:crypto'
import { createServer } from 'node:http'
import { pathToFileURL } from 'node:url'
import {
  DEFAULT_STATUS_FIXTURE,
  STATUS_FIXTURES,
  buildStatusFixture,
  isStatusFixture
} from './status-fixtures.mjs'

export const DEFAULT_MOCK_PORT = 8790

const SESSION_TTL_MS = 8 * 60 * 60 * 1000
const SESSION_TTL_S = SESSION_TTL_MS / 1000
const MIN_PASSWORD_LENGTH = 8
const RATE_LIMIT_MAX_FAILURES = 5
const RATE_LIMIT_WINDOW_MS = 60 * 1000
const MAX_BODY_BYTES = 64 * 1024
const COOKIE_NAME = 'khaen_session'

function hashPassword(password) {
  return createHash('sha256').update('khaen-mock:' + password).digest('hex')
}

function randomToken() {
  return randomBytes(32).toString('hex')
}

export function createMockAuthServer(options = {}) {
  const sessions = new Map()
  const failuresByIp = new Map()
  let passwordHash = null
  let configured = false
  /** /api/status fixture name（present | stale | missing）；未知值回退 present。 */
  let statusFixture = isStatusFixture(options.statusFixture) ? options.statusFixture : DEFAULT_STATUS_FIXTURE

  function sendJson(res, status, body, extraHeaders) {
    const headers = {
      'Content-Type': 'application/json; charset=utf-8',
      'Cache-Control': 'no-store',
      'X-KHAEN-API-Version': '1',
      'X-KHAEN-Web-Version': 'mock'
    }
    if (extraHeaders) {
      Object.assign(headers, extraHeaders)
    }
    res.writeHead(status, headers)
    res.end(JSON.stringify(body))
  }

  function sendError(res, status, code, message, details, extraHeaders) {
    sendJson(res, status, { error: { code, message, details: details === undefined ? {} : details } }, extraHeaders)
  }

  function readJsonBody(req) {
    return new Promise((resolve, reject) => {
      const chunks = []
      let size = 0
      req.on('data', (chunk) => {
        size += chunk.length
        if (size > MAX_BODY_BYTES) {
          reject(new Error('body too large'))
          req.destroy()
          return
        }
        chunks.push(chunk)
      })
      req.on('end', () => {
        const text = Buffer.concat(chunks).toString('utf8')
        if (text.length === 0) {
          resolve({})
          return
        }
        try {
          const parsed = JSON.parse(text)
          resolve(typeof parsed === 'object' && parsed !== null ? parsed : {})
        } catch {
          reject(new Error('invalid JSON body'))
        }
      })
      req.on('error', reject)
    })
  }

  function parseCookies(header) {
    const cookies = {}
    if (typeof header !== 'string') {
      return cookies
    }
    for (const part of header.split(';')) {
      const index = part.indexOf('=')
      if (index < 0) {
        continue
      }
      cookies[part.slice(0, index).trim()] = decodeURIComponent(part.slice(index + 1).trim())
    }
    return cookies
  }

  function sessionFromRequest(req) {
    const cookies = parseCookies(req.headers.cookie)
    let token = cookies[COOKIE_NAME]
    if ((typeof token !== 'string' || token.length === 0) && typeof req.headers['x-khaen-token'] === 'string') {
      token = req.headers['x-khaen-token']
    }
    if (typeof token !== 'string' || token.length === 0) {
      return null
    }
    const session = sessions.get(token)
    if (!session) {
      return null
    }
    if (session.expiresAt <= Date.now()) {
      sessions.delete(token)
      return null
    }
    return { token, session }
  }

  function createSession() {
    const token = randomToken()
    const session = { csrf: randomToken(), expiresAt: Date.now() + SESSION_TTL_MS }
    sessions.set(token, session)
    return { token, session }
  }

  function setSessionCookie(res, token) {
    res.setHeader(
      'Set-Cookie',
      COOKIE_NAME + '=' + token + '; Path=/; HttpOnly; SameSite=Strict; Max-Age=' + String(SESSION_TTL_S)
    )
  }

  function clearSessionCookie(res) {
    res.setHeader('Set-Cookie', COOKIE_NAME + '=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0')
  }

  function hasValidCsrf(req, session) {
    const sent = req.headers['x-khaen-csrf']
    return typeof sent === 'string' && sent === session.csrf
  }

  function clientIp(req) {
    return req.socket.remoteAddress || 'unknown'
  }

  function isRateLimited(ip) {
    const now = Date.now()
    const recent = (failuresByIp.get(ip) || []).filter((timestamp) => now - timestamp < RATE_LIMIT_WINDOW_MS)
    failuresByIp.set(ip, recent)
    return recent.length >= RATE_LIMIT_MAX_FAILURES
  }

  function recordFailure(ip) {
    const list = failuresByIp.get(ip) || []
    list.push(Date.now())
    failuresByIp.set(ip, list)
  }

  async function handle(req, res) {
    const url = new URL(req.url || '/', 'http://mock.invalid')
    const path = url.pathname
    const method = req.method || 'GET'

    if (method === 'GET' && path === '/healthz') {
      sendJson(res, 200, { ok: true, version: 'mock', host_link: 'mock' })
      return
    }

    // Dev-only helper: 在运行中切换 /api/status fixture（浏览器验收时演练 stale/missing）。
    if (method === 'GET' && path === '/__mock/status') {
      sendJson(res, 200, { fixture: statusFixture, fixtures: STATUS_FIXTURES })
      return
    }
    if (method === 'POST' && path === '/__mock/status-fixture') {
      const body = await readJsonBody(req)
      if (!isStatusFixture(body.fixture)) {
        sendError(res, 400, 'invalid_fixture', 'unknown status fixture: ' + String(body.fixture))
        return
      }
      statusFixture = body.fixture
      sendJson(res, 200, { fixture: statusFixture })
      return
    }

    if (!path.startsWith('/api/')) {
      sendError(res, 404, 'not_found', 'unknown path: ' + path)
      return
    }

    if (method === 'GET' && path === '/api/auth/status') {
      sendJson(res, 200, { configured, authenticated: sessionFromRequest(req) !== null })
      return
    }

    if (method === 'POST' && path === '/api/auth/setup') {
      if (configured) {
        sendError(res, 409, 'already_configured', 'admin password is already configured')
        return
      }
      const body = await readJsonBody(req)
      if (typeof body.password !== 'string' || body.password.length < MIN_PASSWORD_LENGTH) {
        sendError(res, 400, 'invalid_password', 'password must be at least ' + String(MIN_PASSWORD_LENGTH) + ' characters', {
          min_length: MIN_PASSWORD_LENGTH
        })
        return
      }
      if (typeof body.confirm !== 'string' || body.confirm !== body.password) {
        sendError(res, 400, 'password_mismatch', 'confirm does not match password')
        return
      }
      passwordHash = hashPassword(body.password)
      configured = true
      const created = createSession()
      setSessionCookie(res, created.token)
      sendJson(res, 201, { csrf: created.session.csrf, expires_in: SESSION_TTL_S })
      return
    }

    if (method === 'POST' && path === '/api/auth/login') {
      if (!configured) {
        sendError(res, 409, 'not_configured', 'no admin password configured yet; run setup first')
        return
      }
      const ip = clientIp(req)
      if (isRateLimited(ip)) {
        sendError(res, 429, 'rate_limited', 'too many failed login attempts', { retry_after_s: 60 }, { 'Retry-After': '60' })
        return
      }
      const body = await readJsonBody(req)
      if (typeof body.password !== 'string' || passwordHash !== hashPassword(body.password)) {
        recordFailure(ip)
        sendError(res, 401, 'invalid_credentials', 'wrong password')
        return
      }
      const created = createSession()
      setSessionCookie(res, created.token)
      sendJson(res, 200, { csrf: created.session.csrf, expires_in: SESSION_TTL_S })
      return
    }

    if (method === 'GET' && path === '/api/auth/session') {
      const found = sessionFromRequest(req)
      if (!found) {
        sendError(res, 401, 'not_authenticated', 'no valid session')
        return
      }
      sendJson(res, 200, {
        authenticated: true,
        csrf: found.session.csrf,
        expires_at: new Date(found.session.expiresAt).toISOString()
      })
      return
    }

    if (method === 'POST' && path === '/api/auth/logout') {
      const found = sessionFromRequest(req)
      if (!found) {
        // Idempotent: logging out without a session just clears the cookie.
        clearSessionCookie(res)
        sendJson(res, 200, {})
        return
      }
      if (!hasValidCsrf(req, found.session)) {
        sendError(res, 403, 'csrf_invalid', 'missing or invalid CSRF token')
        return
      }
      sessions.delete(found.token)
      clearSessionCookie(res)
      sendJson(res, 200, {})
      return
    }

    if (method === 'POST' && path === '/api/auth/password') {
      const found = sessionFromRequest(req)
      if (!found) {
        sendError(res, 401, 'not_authenticated', 'no valid session')
        return
      }
      if (!hasValidCsrf(req, found.session)) {
        sendError(res, 403, 'csrf_invalid', 'missing or invalid CSRF token')
        return
      }
      const body = await readJsonBody(req)
      if (typeof body.old !== 'string' || passwordHash !== hashPassword(body.old)) {
        sendError(res, 401, 'invalid_credentials', 'wrong current password')
        return
      }
      if (typeof body.new !== 'string' || body.new.length < MIN_PASSWORD_LENGTH) {
        sendError(res, 400, 'invalid_password', 'new password must be at least ' + String(MIN_PASSWORD_LENGTH) + ' characters', {
          min_length: MIN_PASSWORD_LENGTH
        })
        return
      }
      passwordHash = hashPassword(body.new)
      // Invalidate every existing session, then issue a fresh one.
      sessions.clear()
      const created = createSession()
      setSessionCookie(res, created.token)
      sendJson(res, 200, { csrf: created.session.csrf, expires_in: SESSION_TTL_S })
      return
    }

    if (method === 'GET' && path === '/api/status') {
      if (!sessionFromRequest(req)) {
        sendError(res, 401, 'not_authenticated', 'authentication required')
        return
      }
      sendJson(res, 200, buildStatusFixture(statusFixture))
      return
    }

    sendError(res, 405, 'method_not_allowed', 'unsupported method/path: ' + method + ' ' + path)
  }

  const server = createServer((req, res) => {
    handle(req, res).catch((error) => {
      if (res.headersSent) {
        res.end()
        return
      }
      sendError(res, 500, 'internal_error', error instanceof Error ? error.message : 'mock internal error')
    })
  })

  return {
    server,
    /** Start listening; returns {host, port, baseUrl}. */
    listen({ host = '127.0.0.1', port = 0 } = {}) {
      return new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(port, host, () => {
          server.removeListener('error', reject)
          const address = server.address()
          const boundPort = typeof address === 'object' && address !== null ? address.port : port
          resolve({ host, port: boundPort, baseUrl: 'http://' + host + ':' + String(boundPort) })
        })
      })
    },
    close() {
      return new Promise((resolve) => {
        server.close(() => resolve())
        if (typeof server.closeAllConnections === 'function') {
          server.closeAllConnections()
        }
      })
    },
    get configured() {
      return configured
    },
    /** 切换 /api/status fixture（测试用；也便于开发时演练 stale/missing 形态）。 */
    setStatusFixture(next) {
      if (!isStatusFixture(next)) {
        throw new Error('unknown status fixture: ' + String(next) + ' (expected ' + STATUS_FIXTURES.join(' | ') + ')')
      }
      statusFixture = next
    },
    get statusFixture() {
      return statusFixture
    },
    setPasswordForTest(password) {
      passwordHash = hashPassword(password)
      configured = true
    },
    reset() {
      sessions.clear()
      failuresByIp.clear()
      passwordHash = null
      configured = false
    }
  }
}

const entry = process.argv[1]
const isMain = typeof entry === 'string' && import.meta.url === pathToFileURL(entry).href
if (isMain) {
  const portIndex = process.argv.indexOf('--port')
  const portValue = portIndex >= 0 ? process.argv[portIndex + 1] : process.env.KHAEN_MOCK_PORT
  const port = portValue === undefined ? DEFAULT_MOCK_PORT : Number(portValue)
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    console.error('[mock-auth] invalid port: ' + String(portValue))
    process.exit(2)
  }
  const fixtureIndex = process.argv.indexOf('--status-fixture')
  const fixtureValue =
    fixtureIndex >= 0 ? process.argv[fixtureIndex + 1] : process.env.KHAEN_MOCK_STATUS_FIXTURE
  if (fixtureValue !== undefined && !isStatusFixture(fixtureValue)) {
    console.error('[mock-auth] invalid status fixture: ' + String(fixtureValue) + ' (expected ' + STATUS_FIXTURES.join(' | ') + ')')
    process.exit(2)
  }
  const mock = createMockAuthServer({ statusFixture: fixtureValue })
  const address = await mock.listen({ port })
  console.log('[mock-auth] contract mock listening on ' + address.baseUrl)
  console.log('[mock-auth] /api/status fixture: ' + mock.statusFixture + ' (' + STATUS_FIXTURES.join(' | ') + ')')
  console.log('[mock-auth] dev: KHAEN_DEV_API=' + address.baseUrl + ' pnpm dev')
  console.log('[mock-auth] in-memory state; restart resets the setup flow')
}
