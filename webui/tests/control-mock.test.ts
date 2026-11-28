import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, isApiError } from '../src/api/client.ts'
import { createAuthApi } from '../src/api/auth.ts'
import { createStatusApi } from '../src/api/status.ts'
import { createControlApi } from '../src/api/control.ts'
import { createInitialSessionState, createSessionController } from '../src/stores/session.ts'
import { createMockAuthServer } from '../scripts/mock-auth-server.mjs'
import { CONTROL_FIXTURES } from '../scripts/control-fixtures.mjs'

const PASSWORD = 'khaen-control-mock-passphrase'

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
  return {
    jar,
    state,
    client,
    auth,
    controller,
    notifications,
    control: createControlApi(client),
    status: createStatusApi(client)
  }
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

/** 带 cookie 但不带 CSRF 的原始 POST（验证 mock 的 CSRF 语义）。 */
async function rawFetch(jar, baseUrl, path, body) {
  const headers = new Headers({ 'Content-Type': 'application/json' })
  const cookie = jar.cookie()
  if (cookie !== null) {
    headers.set('Cookie', cookie)
  }
  return fetch(baseUrl + path, { method: 'POST', headers, body: JSON.stringify(body) })
}

test('mock control: writes require a session and a 401 reaches the shared hook', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    const error = await expectApiError(() => h.control.setMute(true))
    assert.equal(error.status, 401)
    assert.equal(error.code, 'not_authenticated')
    assert.deepEqual(h.notifications, ['client-401'])
  })
})

test('mock control: session-bound writes without CSRF are rejected with 403', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const response = await rawFetch(h.jar, baseUrl, '/api/mute', { mute: true })
    assert.equal(response.status, 403)
    const body = await response.json()
    assert.equal(body.error.code, 'csrf_invalid')
  })
})

test('mock control: mute applies immediately and /api/status reads it back', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const before = await h.status.fetchStatus()
    assert.equal(before.state.muted, false)

    const ack = await h.control.setMute(true)
    assert.deepEqual(ack, { ok: true, mute: true, applied_block: 897, idempotent: false, software_only: true })
    assert.equal(mock.control.muted, true)
    const after = await h.status.fetchStatus()
    assert.equal(after.state.muted, true)

    const off = await h.control.setMute(false)
    assert.equal(off.mute, false)
    const readback = await h.status.fetchStatus()
    assert.equal(readback.state.muted, false)
  })
})

test('mock control: volume prepare is idempotent, commit applies the version', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const gains = [{ lane: 3, gain_db: -6 }, { lane: 0, gain_db: 2.5 }]

    const prepared = await h.control.prepareVolume(gains)
    assert.equal(prepared.surface, 'volume')
    assert.equal(prepared.planned_version, 8)
    assert.equal(mock.control.pending, 'volume')
    const again = await h.control.prepareVolume(gains)
    assert.equal(again.engine_token, prepared.engine_token)

    const ack = await h.control.commit(prepared.engine_token)
    assert.equal(ack.version, 8)
    assert.equal(ack.applied_block, 8 * 128 + 1)
    assert.equal(ack.graph_id, prepared.graph_id)
    assert.equal(ack.all_ack, true)
    assert.equal(mock.control.pending, null)
    assert.equal(mock.control.version, 8)
    assert.equal(mock.control.gainsDb[3], -6)
    assert.equal(mock.control.gainsDb[0], 2.5)
    assert.equal(mock.control.history.at(-1).version, 8)

    // commit 后旧 token 不可重用。
    const error = await expectApiError(() => h.control.commit(prepared.engine_token))
    assert.equal(error.status, 409)
    assert.equal(error.code, 'engine_token_invalid')
  })
})

test('mock control: volume prepare rejects malformed gains / unknown lanes', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const empty = await expectApiError(() => h.control.prepareVolume([]))
    assert.equal(empty.status, 400)
    assert.equal(empty.code, 'bad_request')

    const outOfRange = await expectApiError(() => h.control.prepareVolume([{ lane: 22, gain_db: 0 }]))
    assert.equal(outOfRange.status, 400)
    assert.match(outOfRange.message, /protection has no lane 22/)
  })
})

test('mock control: abort drops the draft, a later commit fails with engine_token_invalid', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const prepared = await h.control.prepareVolume([{ lane: 1, gain_db: -1 }])
    const aborted = await h.control.abort(prepared.engine_token)
    assert.equal(aborted.aborted, true)
    assert.equal(mock.control.pending, null)
    const error = await expectApiError(() => h.control.commit(prepared.engine_token))
    assert.equal(error.code, 'engine_token_invalid')
  })
})

test('mock control: mode two-phase reports changed honestly and off mutes the chain', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    assert.equal(mock.control.mode, 'aes67')

    const same = await h.control.prepareMode('aes67')
    assert.equal(same.changed, false)
    const noop = await h.control.commit(same.engine_token)
    assert.equal(noop.changed, false)
    assert.equal(noop.applied_block, null)
    assert.equal(mock.control.mode, 'aes67')

    const off = await h.control.prepareMode('off')
    assert.equal(off.changed, true)
    const applied = await h.control.commit(off.engine_token)
    assert.equal(applied.changed, true)
    assert.equal(applied.mode, 'off')
    assert.equal(mock.control.muted, true)
    const status = await h.status.fetchStatus()
    assert.equal(status.state.muted, true)
  })
})

test('mock control: analog mode is rejected with a stable bad_request (not silently accepted)', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const error = await expectApiError(() => h.control.prepareMode('analog'))
    assert.equal(error.status, 400)
    assert.equal(error.code, 'bad_request')
    assert.match(error.message, /toslink\/dante\/analog/)
  })
})

test('mock control: format prepare reports device_restart_required and commit rebuilds the engine', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const prepared = await h.control.prepareFormat({ sample_rate: 96000, chunksize: 64 })
    assert.equal(prepared.surface, 'format')
    assert.deepEqual(prepared.format, { sample_rate: 96000, block_size: 64, channels: 22 })
    assert.equal(prepared.changed, true)
    assert.equal(prepared.device_restart_required, true)

    const ack = await h.control.commit(prepared.engine_token)
    assert.equal(ack.version, prepared.planned_version)
    assert.deepEqual(ack.format, { sample_rate: 96000, block_size: 64, channels: 22 })
    assert.equal(ack.device_restart_required, true)
    assert.equal(ack.graph_dropped, true)
    assert.equal(mock.control.format.sample_rate, 96000)
    assert.equal(mock.control.format.block_size, 64)

    const abortable = await h.control.prepareFormat({ chunksize: 128 })
    const aborted = await h.control.abort(abortable.engine_token)
    assert.equal(aborted.aborted_surface, 'format')
  })
})

test('mock control: invalid format values are rejected with bad_request', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const rate = await expectApiError(() => h.control.prepareFormat({ sample_rate: 44100 }))
    assert.equal(rate.status, 400)
    assert.match(rate.message, /sample_rate/)
    const block = await expectApiError(() => h.control.prepareFormat({ chunksize: 256 }))
    assert.equal(block.status, 400)
    assert.match(block.message, /chunksize/)
    const empty = await expectApiError(() => h.control.prepareFormat({}))
    assert.equal(empty.status, 400)
  })
})

test('mock control: device retry returns the device snapshot', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const ack = await h.control.retryDevice()
    assert.equal(ack.wired, true)
    assert.equal(ack.state, 'streaming')
    assert.equal(ack.device_rate, 48000)
    assert.equal(ack.rate_asserted, true)
  })
})

test('mock control: unavailable fixture answers 503 host_link_unavailable for every write', async () => {
  await withMock(
    async ({ mock, baseUrl }) => {
      const h = createHarness(baseUrl)
      await signIn(h)
      assert.equal(mock.controlFixture, 'unavailable')

      const mute = await expectApiError(() => h.control.setMute(true))
      assert.equal(mute.status, 503)
      assert.equal(mute.code, 'host_link_unavailable')
      assert.match(String(mute.message), /unavailable/)

      const prepare = await expectApiError(() => h.control.prepareVolume([{ lane: 0, gain_db: 0 }]))
      assert.equal(prepare.status, 503)
      assert.equal(prepare.code, 'host_link_unavailable')

      const commit = await expectApiError(() => h.control.commit('whatever'))
      assert.equal(commit.status, 503)

      // 读路径不受影响：/api/status 仍然可用（daemon 的文件回退语义）。
      const status = await h.status.fetchStatus()
      assert.equal(status.schema, 'khaen-status/v1')

      const dev = await fetch(baseUrl + '/__mock/control')
      const devBody = await dev.json()
      assert.equal(devBody.wired, false)
      assert.deepEqual(devBody.fixtures, CONTROL_FIXTURES)
    },
    { controlFixture: 'unavailable' }
  )
})

test('mock control: fixture can be switched at runtime and resets in-memory state', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const prepared = await h.control.prepareVolume([{ lane: 0, gain_db: 0 }])
    assert.equal(mock.control.pending, 'volume')

    const raw = await rawFetch(h.jar, baseUrl, '/__mock/control-fixture', { fixture: 'unavailable' })
    // dev helper 是匿名路径：无需会话/CSRF（真实 daemon 不存在该端点）。
    assert.equal(raw.status, 200)
    assert.equal(mock.controlFixture, 'unavailable')
    assert.equal(mock.control.pending, null)
    const error = await expectApiError(() => h.control.commit(prepared.engine_token))

    const invalid = await fetch(baseUrl + '/__mock/control-fixture', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ fixture: 'nope' })
    })
    assert.equal(invalid.status, 400)
    const invalidBody = await invalid.json()
    assert.equal(invalidBody.error.code, 'invalid_fixture')

    mock.setControlFixture('wired')
    assert.equal(mock.controlFixture, 'wired')
    const ok = await h.control.setMute(true)
    assert.equal(ok.mute, true)
    assert.equal(error.status, 503)
  })
})