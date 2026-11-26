import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  ApiClient,
  DEFAULT_API_VERSION,
  isApiError,
  parseRetryAfter,
  type ApiError
} from '../src/api/client.ts'
import { createRecordingFetch, emptyResponse, jsonResponse, textResponse } from '../scripts/mock-test-support.mjs'

async function captureError(run: () => Promise<unknown>): Promise<ApiError> {
  try {
    await run()
  } catch (error) {
    assert.ok(isApiError(error), 'expected an ApiError, got: ' + String(error))
    return error as ApiError
  }
  return assert.fail('expected the request to reject') as never
}

test('GET uses credentials include, Accept header and no CSRF', async () => {
  const recorder = createRecordingFetch(() => jsonResponse(200, { ok: true }))
  const client = new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-should-not-appear' })
  const body = await client.get<{ ok: boolean }>('/api/ping')
  assert.deepEqual(body, { ok: true })
  assert.equal(recorder.calls.length, 1)
  assert.equal(recorder.calls[0].url, '/api/ping')
  assert.equal(recorder.calls[0].method, 'GET')
  assert.equal(recorder.calls[0].credentials, 'include')
  assert.equal(recorder.calls[0].headers.Accept, 'application/json')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], undefined)
  assert.equal(recorder.calls[0].body, undefined)
})

test('baseUrl and query params are composed in a stable order', async () => {
  const recorder = createRecordingFetch(() => jsonResponse(200, {}))
  const client = new ApiClient({ baseUrl: 'http://127.0.0.1:8790/', fetchImpl: recorder.fetch })
  await client.get('/api/status', { query: { tail: 2, skip: undefined, flag: true, nil: null } })
  assert.equal(recorder.calls[0].url, 'http://127.0.0.1:8790/api/status?tail=2&flag=true')
})

test('paths must be absolute', async () => {
  const client = new ApiClient({ fetchImpl: async () => jsonResponse(200, {}) })
  await assert.rejects(() => client.get('api/status'), /must start with/)
})

test('POST sends JSON body without CSRF when csrf is omitted', async () => {
  const recorder = createRecordingFetch(() => jsonResponse(201, { csrf: 'c', expires_in: 100 }))
  const client = new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-abc' })
  await client.post('/api/auth/setup', { password: 'x', confirm: 'x' }, { csrf: 'omit' })
  const call = recorder.calls[0]
  assert.equal(call.method, 'POST')
  assert.equal(call.headers['Content-Type'], 'application/json')
  assert.equal(call.headers['X-KHAEN-CSRF'], undefined)
  assert.equal(call.body, '{"password":"x","confirm":"x"}')
})

test('write requests attach the session CSRF token automatically', async () => {
  const recorder = createRecordingFetch(() => jsonResponse(200, {}))
  const client = new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => 'csrf-abc' })
  await client.post('/api/auth/logout')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], 'csrf-abc')
})

test('empty CSRF token is not sent', async () => {
  const recorder = createRecordingFetch(() => jsonResponse(200, {}))
  const client = new ApiClient({ fetchImpl: recorder.fetch, csrfToken: () => null })
  await client.post('/api/auth/logout')
  assert.equal(recorder.calls[0].headers['X-KHAEN-CSRF'], undefined)
})

test('204 and empty 200 bodies resolve to undefined', async () => {
  const recorder = createRecordingFetch((call) =>
    call.url === '/api/a' ? emptyResponse(204) : new Response(null, { status: 200 })
  )
  const client = new ApiClient({ fetchImpl: recorder.fetch })
  assert.equal(await client.post('/api/a'), undefined)
  assert.equal(await client.post('/api/b'), undefined)
})

test('error bodies are parsed into ApiError with code/message/details/Retry-After', async () => {
  const recorder = createRecordingFetch(() =>
    jsonResponse(
      429,
      { error: { code: 'rate_limited', message: 'too many attempts', details: { retry_after_s: 60 } } },
      { 'Retry-After': '60' }
    )
  )
  const client = new ApiClient({ fetchImpl: recorder.fetch })
  const error = await captureError(() => client.post('/api/auth/login', { password: 'x' }, { csrf: 'omit' }))
  assert.equal(error.status, 429)
  assert.equal(error.code, 'rate_limited')
  assert.equal(error.message, 'too many attempts')
  assert.deepEqual(error.details, { retry_after_s: 60 })
  assert.equal(error.retryAfterSeconds, 60)
  assert.equal(error.isUnauthorized, false)
  assert.equal(error.isNetworkError, false)
})

test('non-JSON error bodies keep the http_<status> fallback code', async () => {
  const recorder = createRecordingFetch(() => textResponse(502, '<html>bad gateway</html>'))
  const client = new ApiClient({ fetchImpl: recorder.fetch })
  const error = await captureError(() => client.get('/api/ping'))
  assert.equal(error.status, 502)
  assert.equal(error.code, 'http_502')
})

test('401 notifies onUnauthorized by default', async () => {
  const seen: ApiError[] = []
  const recorder = createRecordingFetch(() =>
    jsonResponse(401, { error: { code: 'not_authenticated', message: 'auth required' } })
  )
  const client = new ApiClient({ fetchImpl: recorder.fetch, onUnauthorized: (error) => seen.push(error) })
  const error = await captureError(() => client.get('/api/status'))
  assert.equal(error.status, 401)
  assert.equal(seen.length, 1)
  assert.equal(seen[0].code, 'not_authenticated')
})

test('probe requests do not notify onUnauthorized on 401', async () => {
  let notified = 0
  const recorder = createRecordingFetch(() => jsonResponse(401, { error: { code: 'not_authenticated', message: 'x' } }))
  const client = new ApiClient({ fetchImpl: recorder.fetch, onUnauthorized: () => { notified += 1 } })
  await captureError(() => client.get('/api/auth/session', { auth: 'probe' }))
  assert.equal(notified, 0)
})

test('network failures become ApiError status 0 code network_error', async () => {
  const recorder = createRecordingFetch(() => {
    throw new TypeError('fetch failed')
  })
  const client = new ApiClient({ fetchImpl: recorder.fetch })
  const error = await captureError(() => client.get('/api/ping'))
  assert.equal(error.status, 0)
  assert.equal(error.code, 'network_error')
  assert.equal(error.isNetworkError, true)
  assert.match(error.message, /fetch failed/)
})

test('a 200 response with invalid JSON is rejected as invalid_response', async () => {
  const recorder = createRecordingFetch(() => textResponse(200, 'not json at all'))
  const client = new ApiClient({ fetchImpl: recorder.fetch })
  const error = await captureError(() => client.get('/api/ping'))
  assert.equal(error.code, 'invalid_response')
  assert.equal(error.status, 200)
})

test('API version mismatch is reported exactly once and exposed', async () => {
  const versions: string[] = []
  const recorder = createRecordingFetch(() => jsonResponse(200, {}, { 'X-KHAEN-API-Version': '2' }))
  const client = new ApiClient({ fetchImpl: recorder.fetch, onVersionMismatch: (version) => versions.push(version) })
  await client.get('/api/a')
  await client.get('/api/b')
  assert.deepEqual(versions, ['2'])
  assert.equal(client.serverApiVersion, '2')
})

test('matching API version does not report a mismatch', async () => {
  const versions: string[] = []
  const recorder = createRecordingFetch(() =>
    jsonResponse(200, {}, { 'X-KHAEN-API-Version': DEFAULT_API_VERSION })
  )
  const client = new ApiClient({ fetchImpl: recorder.fetch, onVersionMismatch: (version) => versions.push(version) })
  await client.get('/api/a')
  assert.deepEqual(versions, [])
  assert.equal(client.serverApiVersion, DEFAULT_API_VERSION)
})

test('parseRetryAfter accepts seconds and rejects garbage', () => {
  assert.equal(parseRetryAfter('60'), 60)
  assert.equal(parseRetryAfter('2.2'), 3)
  assert.equal(parseRetryAfter('0'), 0)
  assert.equal(parseRetryAfter('-1'), null)
  assert.equal(parseRetryAfter('Wed, 21 Oct 2015 07:28:00 GMT'), null)
  assert.equal(parseRetryAfter(null), null)
})
test('forwarded rt-host error bodies ({ok:false,error:"<code>",message}) map to ApiError', async () => {
  const recorder = createRecordingFetch(() =>
    jsonResponse(502, {
      ok: false,
      error: 'ERR_RECONFIGURE_DEVICE',
      message: 'device reopen failed, rolled back',
      details: { rolled_back: true }
    })
  )
  const client = new ApiClient({ fetchImpl: recorder.fetch })
  const error = await captureError(() => client.post('/api/channels', { stage: 'commit' }))
  assert.equal(error.status, 502)
  assert.equal(error.code, 'ERR_RECONFIGURE_DEVICE')
  assert.equal(error.message, 'device reopen failed, rolled back')
  assert.deepEqual(error.details, { rolled_back: true })
})

test('M1 error body keeps priority over the forwarded string form', async () => {
  const recorder = createRecordingFetch(() =>
    jsonResponse(409, { error: { code: 'engine_token_invalid', message: 'token spent' } })
  )
  const client = new ApiClient({ fetchImpl: recorder.fetch })
  const error = await captureError(() => client.post('/api/commit', {}))
  assert.equal(error.code, 'engine_token_invalid')
  assert.equal(error.message, 'token spent')
})
test('getRawBytes keeps binary payloads lossless (text getRaw is lossy for binary)', async () => {
  const payload = new Uint8Array([0xff, 0xfe, 0x00, 0x80, 0x41])
  const recording = createRecordingFetch(
    () =>
      new Response(payload, {
        status: 200,
        headers: { 'Content-Type': 'application/octet-stream', 'Content-Disposition': 'attachment; filename="plan.bin"' }
      })
  )
  const client = new ApiClient({ fetchImpl: recording.fetch, csrfToken: () => 'csrf' })
  const raw = await client.getRawBytes('/api/calibration/jobs/j1/artifacts/plan.bin')
  assert.equal(raw.status, 200)
  assert.equal(raw.headers.get('Content-Disposition'), 'attachment; filename="plan.bin"')
  assert.deepEqual([...new Uint8Array(raw.bytes)], [...payload])
  assert.equal(recording.calls[0].method, 'GET')
  assert.equal(recording.calls[0].credentials, 'include')
})
