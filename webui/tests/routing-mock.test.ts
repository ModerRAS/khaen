import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient } from '../src/api/client.ts'
import { createAuthApi } from '../src/api/auth.ts'
import { createStatusApi, isStatusStale } from '../src/api/status.ts'
import { createRoutingApi, buildPlanSummary, buildRoutingChain, laneRows } from '../src/api/routing.ts'
import { createInitialSessionState, createSessionController } from '../src/stores/session.ts'
import { createInitialRoutingState, createRoutingController } from '../src/stores/routing.ts'
import { createMockAuthServer } from '../scripts/mock-auth-server.mjs'
import { CHANNELS_FIXTURES } from '../scripts/channels-fixtures.mjs'
import { STATUS_FIXTURES } from '../scripts/status-fixtures.mjs'

const PASSWORD = 'khaen-routing-mock-passphrase'

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
    routing: createRoutingApi(client),
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

test('mock routing: read-only endpoints require a session (401 reaches the shared hook)', async () => {
  await withMock(async ({ baseUrl }) => {
    const h = createHarness(baseUrl)
    for (const run of [() => h.routing.fetchChannels(), () => h.routing.fetchPlanHistory()]) {
      try {
        await run()
        assert.fail('expected the request to reject')
      } catch (error) {
        assert.equal(error.status, 401)
        assert.equal(error.code, 'not_authenticated')
      }
    }
    assert.deepEqual(h.notifications, ['client-401', 'client-401'])
  })
})

test('mock routing: wired fixture composes the 22-lane chain, DAC map, plan summary and history', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)

    const status = await h.status.fetchStatus()
    const channels = await h.routing.fetchChannels()
    const history = await h.routing.fetchPlanHistory()
    assert.deepEqual(mock.channelsFixture, 'wired')
    assert.deepEqual(STATUS_FIXTURES, ['present', 'stale', 'missing'])

    assert.equal(channels.output.channels, 22)
    assert.equal(channels.mapping?.length, 22)
    // mock 的 /api/channels 是 T54 契约形态（没有真实 daemon 的 plan 段）：如实 null，不猜。
    assert.equal(channels.plan.routing_known, null)
    assert.equal(channels.input.source, 'sdp')
    assert.deepEqual(channels.input.map, [0, 1])
    assert.deepEqual(history.entries.map((entry) => entry.version), [5, 6, 7])
    assert.equal(history.current_version, 7)

    const chain = buildRoutingChain(status, channels)
    assert.equal(chain.input.channels.value, 2)
    assert.deepEqual(chain.input.map.value, [0, 1])
    assert.equal(chain.matrix.lanes.value, 22)
    assert.equal(chain.matrix.routedLanes.value, 22)
    assert.equal(chain.matrix.routes.value, 22)
    assert.equal(chain.output.channels.value, 22)
    assert.equal(chain.output.knownLabels, 7)

    const rows = laneRows(channels)
    assert.equal(rows.length, 22)
    assert.deepEqual({ lane: rows[0].lane, dac: rows[0].dac, physical: rows[0].physical }, { lane: 0, dac: 0, physical: 1 })
    assert.equal(rows[2].labelKey, 'channels.label.sl')
    // 7 不在已知位置表里：不猜标签，页面显示 OUT 7。
    assert.equal(rows[6].physical, 7)
    assert.equal(rows[6].labelKey, null)

    const summary = buildPlanSummary(status, channels, history)
    assert.equal(summary.version, 3)
    assert.equal(summary.versionSource, 'status')
    assert.equal(summary.latencySamples, 1023)
    assert.equal(summary.segments, null)
    assert.equal(summary.segmentsReasonKey, 'routing.plan.segmentsReason')
    assert.equal(summary.routingKnown, true)
    assert.equal(summary.dacChannels, 22)
    assert.equal(summary.routes, 22)
    assert.equal(summary.historyCurrentVersion, 7)
  })
})

test('mock routing: degraded channels fixture yields nulls + reasons and never invents a mapping', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const status = await h.status.fetchStatus()
    mock.setChannelsFixture('degraded')
    assert.deepEqual(mock.channelsFixture, 'degraded')
    const channels = await h.routing.fetchChannels()
    assert.equal(channels.output.channels, null)
    assert.equal(channels.output.dac_map, null)
    assert.equal(channels.mapping, null)
    assert.match(channels.reasons['output.channels'] ?? '', /rt-host control status is unavailable/)

    const rows = laneRows(channels)
    assert.equal(rows.length, 0)
    const chain = buildRoutingChain(status, channels)
    assert.equal(chain.matrix.routedLanes.value, null)
    assert.equal(chain.matrix.routes.value, null)
    assert.equal(chain.output.channels.value, null)
    assert.match(chain.matrix.routes.reason ?? '', /rt-host control status is unavailable/)

    const summary = buildPlanSummary(status, channels, await h.routing.fetchPlanHistory())
    assert.equal(summary.version, 3)
    assert.equal(summary.dacChannels, null)
    assert.equal(summary.routes, null)
    assert.equal(summary.routingKnown, null)
  })
})

test('mock routing: unavailable channels fixture is NOT_AVAILABLE (501 not_wired) end to end', async () => {
  await withMock(async ({ mock, baseUrl }, ) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    mock.setChannelsFixture('unavailable')
    assert.deepEqual(CHANNELS_FIXTURES, ['wired', 'degraded', 'unavailable'])

    const state = createInitialRoutingState()
    const controller = createRoutingController(state, { api: h.routing })
    await controller.load()
    assert.equal(state.channels, null)
    assert.equal(state.channelsPhase, 'error')
    assert.equal(state.channelsError?.category, 'not_wired')
    assert.equal(state.channelsError?.code, 'not_wired')
    assert.equal(state.history, null)
    assert.equal(state.historyError?.category, 'not_wired')
  })
})

test('mock routing: stale status fixture is flagged stale while format facts stay readable', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const channels = await h.routing.fetchChannels()
    mock.setStatusFixture('stale')
    const stale = await h.status.fetchStatus()
    assert.equal(isStatusStale(stale), true)
    const chain = buildRoutingChain(stale, channels)
    assert.equal(chain.matrix.lanes.value, 22)
    assert.equal(chain.output.channels.value, 22)
  })
})

test('mock routing: missing status fixture degrades input/matrix while history keeps the version', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    mock.setStatusFixture('missing')
    const status = await h.status.fetchStatus()
    const channels = await h.routing.fetchChannels()
    const history = await h.routing.fetchPlanHistory()

    const chain = buildRoutingChain(status, channels)
    // 输入声道只有 /api/channels 上报了 2（AES67 SDP），矩阵 lane 数没有数据源 -> null。
    assert.equal(chain.input.channels.value, 2)
    assert.equal(chain.matrix.lanes.value, null)
    assert.equal(chain.output.channels.value, 22)

    const summary = buildPlanSummary(status, channels, history)
    assert.equal(summary.version, 7)
    assert.equal(summary.versionSource, 'history')
    assert.equal(summary.latencySamples, null)
    assert.equal(summary.historyCurrentVersion, 7)
  })
})
