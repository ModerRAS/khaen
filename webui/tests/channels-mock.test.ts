import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { ApiClient, isApiError } from '../src/api/client.ts'
import { createAuthApi } from '../src/api/auth.ts'
import { createStatusApi } from '../src/api/status.ts'
import { createChannelsApi } from '../src/api/channels.ts'
import { createInitialSessionState, createSessionController } from '../src/stores/session.ts'
import { createMockAuthServer } from '../scripts/mock-auth-server.mjs'
import { CHANNELS_FIXTURES } from '../scripts/channels-fixtures.mjs'

const PASSWORD = 'khaen-channels-mock-passphrase'

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
    channels: createChannelsApi(client),
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

function planBytes(seed = 7, length = 128): Uint8Array {
  const bytes = new Uint8Array(length)
  bytes.fill(seed)
  return bytes
}

function sha256Of(bytes: Uint8Array): string {
  return createHash('sha256').update(Buffer.from(bytes)).digest('hex')
}

test('mock M4: layout and history require a session and a 401 reaches the shared hook', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    const error = await expectApiError(() => h.channels.fetchChannels())
    assert.equal(error.status, 401)
    assert.equal(error.code, 'not_authenticated')
    assert.deepEqual(h.notifications, ['client-401'])
  })
})

test('mock M4: PUT /api/channels without CSRF is rejected with 403', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const headers = new Headers({ 'Content-Type': 'application/json' })
    const cookie = h.jar.cookie()
    if (cookie !== null) {
      headers.set('Cookie', cookie)
    }
    const response = await fetch(baseUrl + '/api/channels', {
      method: 'PUT',
      headers,
      body: JSON.stringify({ output: { channels: 20 } })
    })
    assert.equal(response.status, 403)
    const body = await response.json()
    assert.equal(body.error.code, 'csrf_invalid')
  })
})

test('mock M4: wired fixture exposes 22 output channels, mapping, AES67 input and history', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const info = await h.channels.fetchChannels()
    assert.equal(info.output.channels, 22)
    assert.equal(info.mapping?.length, 22)
    assert.deepEqual(info.mapping?.[0], { lane: 0, dac: 0 })
    assert.equal(info.input.channels, 2)
    assert.deepEqual(info.input.map, [0, 1])
    assert.equal(info.input.source, 'sdp')
    assert.match(String(info.input.sdp_sha256), /^[0-9a-f]{64}$/)
    assert.equal(info.not_wired.length, 1)
    assert.match(info.not_wired[0].reason, /M3b/)

    // daemon 真实 history 形态（current 对象 + entries + recorded_at）：页面归一化为 current_version/versions。
    const history = await h.channels.fetchPlanHistory()
    assert.equal(history.current_version, 7)
    assert.equal(history.current_graph_id, mock.control.history.at(-1)?.graph_id)
    assert.equal(history.source, 'daemon-process')
    assert.deepEqual(history.versions.map((entry) => entry.version), [5, 6, 7])
    assert.equal(history.versions.at(-1)?.recorded_at, '2026-11-26T06:20:00.000Z')
    assert.equal(history.versions.at(-1)?.applied_at, '2026-11-26T06:20:00.000Z')

    // POST /api/plan/inspect {plan_path}：plan_path 来自上传回执的 stored_path（只读试运行已落盘的方案）。
    const upload = await h.channels.uploadPlan(planBytes(11), 'inspect.bin')
    const inspect = await h.channels.fetchPlanInspect(upload.stored_path!, upload.sha256)
    assert.equal(inspect.compiled, true)
    assert.equal(inspect.admission_accepted, true)
    assert.equal(inspect.lanes, 22)
    assert.equal(inspect.sha256, upload.sha256)
    assert.equal(inspect.graph_id, upload.graph_id)

    const dev = await fetch(baseUrl + '/__mock/channels')
    const devBody = await dev.json()
    assert.equal(devBody.fixture, 'wired')
    assert.deepEqual(devBody.fixtures, CHANNELS_FIXTURES)
    assert.equal(mock.channelsFixture, 'wired')
  })
})

test('mock M4: channel prepare returns preview/warnings and commit is 501 host_link_unsupported', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const prepared = await h.channels.prepareChannels({ output: { channels: 20, dac_map: Array.from({ length: 20 }, (_, index) => index) } })
    assert.equal(prepared.changed, true)
    assert.equal(prepared.preview?.old_channels, 22)
    assert.equal(prepared.preview?.new_channels, 20)
    assert.equal(prepared.preview?.audio_gap_ms_estimate, 1200)
    assert.equal(prepared.warnings.length, 1)
    assert.deepEqual(prepared.blockers, [])
    assert.equal(mock.control.pending, 'channels')

    // M3b/T52 未完成：commit 必须 501，且不假装成功。
    const error = await expectApiError(() => h.channels.commit(prepared.engine_token))
    assert.equal(error.status, 501)
    assert.equal(error.code, 'host_link_unsupported')
    assert.equal(mock.control.pending, 'channels')

    const aborted = await h.channels.abort(prepared.engine_token)
    assert.equal(aborted.aborted, true)
    assert.equal(mock.control.pending, null)
  })
})

test('mock M4: invalid channel patches are rejected with bad_request', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const empty = await expectApiError(() => h.channels.prepareChannels({ output: { channels: 33 } }))
    assert.equal(empty.status, 400)
    const mapLength = await expectApiError(() => h.channels.prepareChannels({ output: { channels: 20, dac_map: [0, 1] } }))
    assert.equal(mapLength.status, 400)
    assert.match(mapLength.message, /dac_map/)
    const inputRange = await expectApiError(() => h.channels.prepareChannels({ input: { channels: 9 } }))
    assert.equal(inputRange.status, 400)
  })
})

test('mock M4: plan upload verifies sha, prepare/commit apply a new plan version', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const bytes = planBytes(7)
    const sha = sha256Of(bytes)

    const upload = await h.channels.uploadPlan(bytes, 'demo.bin')
    assert.equal(upload.sha256, sha)
    // daemon 真实回执：没有 upload_id/version；segments 是区段名数组 + segment_count + stored_path。
    assert.equal(upload.upload_id, null)
    assert.equal(upload.version, null)
    assert.equal(upload.graph_id, sha)
    assert.equal(upload.segment_count, 1 + (bytes.length % 5))
    assert.equal(upload.segments?.length, upload.segment_count)
    assert.equal(upload.segments?.[0], 'content')
    assert.equal(upload.stored_path, '/var/lib/khaen/plans/' + sha + '.plan')
    // 幂等：同一字节再上传返回同一记录（created=false）。
    const again = await h.channels.uploadPlan(bytes, 'demo.bin')
    assert.deepEqual(again, upload)
    assert.equal(mock.m4.uploads.length, 1)

    const draft = await h.channels.preparePlan({ plan_path: upload.stored_path!, sha256: sha })
    assert.equal(draft.surface, 'plan')
    assert.equal(draft.graph_id, sha)
    assert.equal(draft.planned_version, 8)

    const ack = await h.channels.commit(draft.engine_token)
    assert.equal(ack.version, 8)
    assert.equal(ack.graph_id, sha)

    const history = await h.channels.fetchPlanHistory()
    assert.equal(history.current_version, 8)
    assert.equal(history.current_graph_id, sha)
    assert.equal(history.versions.at(-1)?.version, 8)
    assert.equal(history.versions.at(-1)?.sha256, sha)

    const status = await h.status.fetchStatus()
    assert.equal(status.plan.version, 8)
    assert.equal(status.plan.graph_id, sha)

    const inspect = await h.channels.fetchPlanInspect(upload.stored_path!, sha)
    assert.equal(inspect.sha256, sha)
    assert.equal(inspect.graph_id, sha)
  })
})

test('mock M4: upload errors are honest (too small / rejected bytes / unknown upload / sha mismatch)', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)

    const tooSmall = await expectApiError(() => h.channels.uploadPlan(new Uint8Array(8), 'tiny.bin'))
    assert.equal(tooSmall.status, 400)
    assert.equal(tooSmall.code, 'invalid_plan')

    const reject = new Uint8Array(128)
    reject.set(Array.from(new TextEncoder().encode('REJECTED')), 0)
    const rejected = await expectApiError(() => h.channels.uploadPlan(reject, 'bad.bin'))
    assert.equal(rejected.status, 400)
    assert.equal(rejected.code, 'invalid_plan')

    // daemon 真实 prepare 读 plan_path：读不到文件 -> 400 bad_request（不是 upload_not_found）。
    const unknown = await expectApiError(() =>
      h.channels.preparePlan({ plan_path: '/var/lib/khaen/plans/' + 'f'.repeat(64) + '.plan', sha256: 'f'.repeat(64) })
    )
    assert.equal(unknown.status, 400)
    assert.equal(unknown.code, 'bad_request')
    assert.match(unknown.message, /cannot read plan file/)

    const bytes = planBytes(9)
    const upload = await h.channels.uploadPlan(bytes, 'demo.bin')
    const mismatch = await expectApiError(() =>
      h.channels.preparePlan({ plan_path: upload.stored_path!, sha256: 'e'.repeat(64) })
    )
    assert.equal(mismatch.status, 400)
    assert.equal(mismatch.code, 'bad_request')
    assert.match(mismatch.message, /sha256 mismatch/)

    // 4 MiB 上限：mock 用 raw body 限制，超出即 413。
    const cookie = h.jar.cookie() ?? ''
    const csrf = h.state.csrfToken ?? ''
    const oversized = await fetch(baseUrl + '/api/plan/upload', {
      method: 'POST',
      headers: {
        Cookie: cookie,
        'X-KHAEN-CSRF': csrf,
        'Content-Type': 'application/octet-stream'
      },
      body: Buffer.alloc(4 * 1024 * 1024 + 1)
    })
    assert.equal(oversized.status, 413)
    const body = await oversized.json()
    assert.equal(body.error.code, 'upload_too_large')
  })
})

test('mock M4: rollback creates a new version from history and still needs a commit', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const draft = await h.channels.rollbackPlan(5)
    assert.equal(draft.surface, 'rollback')
    assert.equal(draft.rolled_back_to, 5)
    assert.equal(draft.planned_version, 8)
    const ack = await h.channels.commit(draft.engine_token)
    assert.equal(ack.version, 8)
    const history = await h.channels.fetchPlanHistory()
    assert.equal(history.current_version, 8)
    assert.equal(history.versions.at(-1)?.version, 8)
    assert.equal(history.versions.at(-1)?.graph_id, draft.graph_id)

    const missing = await expectApiError(() => h.channels.rollbackPlan(999))
    assert.equal(missing.status, 400)
    const current = await expectApiError(() => h.channels.rollbackPlan(8))
    assert.equal(current.status, 400)
  })
})

test('mock M4: degraded fixture reports null fields with reasons, plan endpoints stay honest', async () => {
  await withMock(
    async ({ baseUrl }) => {
      const h = createHarness(baseUrl)
      await signIn(h)
      const info = await h.channels.fetchChannels()
      assert.equal(info.output.channels, null)
      assert.equal(info.output.dac_map, null)
      // 真实 daemon 的 routing 未知形态是 mapping=null（不是空数组）。
      assert.equal(info.mapping, null)
      assert.match(String(info.reasons['output.channels']), /does not report channels/)

      const prepare = await expectApiError(() => h.channels.prepareChannels({ output: { channels: 20 } }))
      assert.equal(prepare.status, 409)
      assert.equal(prepare.code, 'channel_layout_unavailable')

      // inspect 仍是 POST {plan_path}：degraded 下上传可用，试运行如实报不可用（不是假成功）。
      const upload = await h.channels.uploadPlan(planBytes(6), 'degraded.bin')
      const inspect = await h.channels.fetchPlanInspect(upload.stored_path!, upload.sha256)
      assert.equal(inspect.admission_accepted, false)
      assert.equal(inspect.compiled, false)
      assert.equal(inspect.rejections[0]?.code, 'rt_host_unavailable')
      assert.equal(inspect.sha256, upload.sha256)

      // daemon 自己的计划历史仍可读（只有 rt-host 现场字段不可用）。
      const history = await h.channels.fetchPlanHistory()
      assert.equal(history.current_version, 7)
    },
    { channelsFixture: 'degraded' }
  )
})

test('mock M4: partial fixture exposes the real nullable dac_map (partial routing) without invalid_response', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    mock.setChannelsFixture('partial')
    assert.equal(mock.channelsFixture, 'partial')
    const info = await h.channels.fetchChannels()
    assert.equal(info.output.channels, 4)
    // 真实 daemon 的部分路由：dac_map 里 null = 该 DAC 没有 lane 喂入（合法形态，不是畸形响应）。
    assert.deepEqual(info.output.dac_map, [0, null, 1, null])
    assert.deepEqual(info.mapping, [
      { lane: 0, dac: 0 },
      { lane: 1, dac: 2 }
    ])
    // 该 fixture 下上传 + inspect 照常可用（POST {plan_path} 契约）。
    const upload = await h.channels.uploadPlan(planBytes(12), 'partial.bin')
    const inspect = await h.channels.fetchPlanInspect(upload.stored_path!, upload.sha256)
    assert.equal(inspect.compiled, true)
    assert.equal(inspect.dac_channels, 4)
  })
})

test('mock M4: POST /api/plan/inspect requires plan_path (GET is 405, unknown stored path is 400)', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const cookie = h.jar.cookie() ?? ''
    const csrf = h.state.csrfToken ?? ''
    const getResponse = await fetch(baseUrl + '/api/plan/inspect', { headers: { Cookie: cookie } })
    assert.equal(getResponse.status, 405)

    const headers = { Cookie: cookie, 'X-KHAEN-CSRF': csrf, 'Content-Type': 'application/json' }
    const missing = await fetch(baseUrl + '/api/plan/inspect', {
      method: 'POST',
      headers,
      body: JSON.stringify({})
    })
    assert.equal(missing.status, 400)
    assert.equal((await missing.json()).error.code, 'bad_request')

    const unknown = await fetch(baseUrl + '/api/plan/inspect', {
      method: 'POST',
      headers,
      body: JSON.stringify({ plan_path: '/var/lib/khaen/plans/' + 'f'.repeat(64) + '.plan' })
    })
    assert.equal(unknown.status, 400)
    const body = await unknown.json()
    assert.match(body.error.message, /cannot read plan file/)
  })
})

test('mock M4: unavailable fixture answers 501 not_wired for every M4 endpoint', async () => {
  await withMock(
    async ({ mock, baseUrl }) => {
      const h = createHarness(baseUrl)
      await signIn(h)
      for (const run of [
        () => h.channels.fetchChannels(),
        () => h.channels.fetchPlanHistory(),
        () => h.channels.fetchPlanInspect('/var/lib/khaen/plans/whatever.plan'),
        () => h.channels.prepareChannels({ output: { channels: 20 } }),
        () => h.channels.uploadPlan(planBytes(3), 'demo.bin'),
        () => h.channels.rollbackPlan(5)
      ]) {
        const error = await expectApiError(run)
        assert.equal(error.status, 501)
        assert.equal(error.code, 'not_wired')
      }
      const dev = await fetch(baseUrl + '/__mock/channels')
      const devBody = await dev.json()
      assert.equal(devBody.unavailable, true)
      assert.equal(mock.channelsFixture, 'unavailable')
    },
    { channelsFixture: 'unavailable' }
  )
})

test('mock M4: fixture switching resets upload/pending state and rejects unknown names', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const bytes = planBytes(4)
    const upload = await h.channels.uploadPlan(bytes, 'demo.bin')
    await h.channels.preparePlan({ plan_path: upload.stored_path!, sha256: upload.sha256 })
    assert.equal(mock.control.pending, 'plan')

    const response = await fetch(baseUrl + '/__mock/channels-fixture', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ fixture: 'degraded' })
    })
    assert.equal(response.status, 200)
    assert.equal(mock.channelsFixture, 'degraded')
    assert.equal(mock.control.pending, null)
    assert.equal(mock.m4.uploads.length, 0)

    const invalid = await fetch(baseUrl + '/__mock/channels-fixture', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ fixture: 'nope' })
    })
    assert.equal(invalid.status, 400)
    const invalidBody = await invalid.json()
    assert.equal(invalidBody.error.code, 'invalid_fixture')
    assert.throws(() => mock.setChannelsFixture('nope'), /unknown channels fixture/)
    mock.setChannelsFixture('wired')
    assert.equal((await h.channels.fetchChannels()).output.channels, 22)
  })
})
