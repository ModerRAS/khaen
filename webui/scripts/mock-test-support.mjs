/**
 * Test-only helpers for the frontend auth tests (Node test runner).
 * scripts/ is never bundled into dist; this file is NOT shipped to the browser.
 */

/** Build a JSON Response with optional extra headers (Node 24 global Response). */
export function jsonResponse(status, body, headers = {}) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers }
  })
}

/** Build an empty Response (204 / 200 with no body). */
export function emptyResponse(status = 204, headers = {}) {
  return new Response(null, { status, headers })
}

/** Build a Response with an arbitrary text body. */
export function textResponse(status, text, headers = {}) {
  return new Response(text, { status, headers: { 'Content-Type': 'text/plain', ...headers } })
}

/**
 * Recording fetch stub.
 * handler(call, index) must return a Response (or throw to simulate a network failure).
 * Returns { fetch, calls } where calls records url/method/headers/body/credentials.
 */
export function createRecordingFetch(handler) {
  const calls = []
  const fetchImpl = async (url, init = {}) => {
    const call = {
      url,
      method: init.method ?? 'GET',
      headers: { ...(init.headers ?? {}) },
      body: init.body,
      credentials: init.credentials,
      signal: init.signal
    }
    calls.push(call)
    return await handler(call, calls.length - 1)
  }
  return { fetch: fetchImpl, calls }
}

/** Convenience: fake auth API for session-store tests. */
export function createFakeAuth(overrides = {}) {
  const calls = { status: 0, session: 0, login: 0, setup: 0, logout: 0 }
  const base = {
    status: async () => {
      calls.status += 1
      return { configured: false, authenticated: false }
    },
    session: async () => {
      calls.session += 1
      return { authenticated: true, csrf: 'csrf-session', expires_at: '2026-11-27T00:00:00.000Z' }
    },
    login: async () => {
      calls.login += 1
      return { csrf: 'csrf-login', expires_in: 28800 }
    },
    setup: async () => {
      calls.setup += 1
      return { csrf: 'csrf-setup', expires_in: 28800 }
    },
    logout: async () => {
      calls.logout += 1
    }
  }
  return { auth: Object.assign(base, overrides), calls }
}
