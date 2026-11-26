import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient } from '../src/api/client.ts'
import { createAuthApi } from '../src/api/auth.ts'
import { createControlApi } from '../src/api/control.ts'
import { createStatusApi, isStatusStale } from '../src/api/status.ts'
import {
  buildPlanSummary,
  buildRoutingChain,
  createRoutingApi,
  currentRoutingSource,
  laneRows
} from '../src/api/routing.ts'
import { createInitialSessionState, createSessionController } from '../src/stores/session.ts'
import { createInitialRoutingState, createRoutingController } from '../src/stores/routing.ts'
import { createControlController, createInitialControlState } from '../src/stores/control.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'
import { createMockAuthServer } from '../scripts/mock-auth-server.mjs'
import { CHANNELS_FIXTURES } from '../scripts/channels-fixtures.mjs'
import { STATUS_FIXTURES } from '../scripts/status-fixtures.mjs'
import {
  ROUTING_PLANNED_VERSION,
  ROUTING_PREPARED_TOKEN,
  buildRoutingAbortAck,
  buildRoutingCommitAck,
  buildRoutingPrepareAck
} from './routing-fixtures.ts'

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
    // T60 起 mock 也按真实 daemon 形态给出 plan 段（routing_known=true：routing 段已关联）。
    assert.equal(channels.plan.routing_known, true)
    assert.equal(channels.plan.version, 7)
    assert.equal(channels.plan.graph_id, mock.control.history.at(-1)?.graph_id)
    // 共享 channels fixture 在并行升级中：input.source 未上报时必须为 null（不编造 sdp/manual）。
    assert.ok(channels.input.source === null || channels.input.source === 'sdp' || channels.input.source === 'manual')
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

test('mock routing: partial fixture keeps the nullable dac_map and the real lane→DAC mapping', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    mock.setChannelsFixture('partial')
    const status = await h.status.fetchStatus()
    const channels = await h.routing.fetchChannels()
    // 真实 daemon 的部分路由：dac_map 的 null 项 = 该 DAC 没有 lane 喂入（合法），不补 0、不报 invalid_response。
    assert.deepEqual(channels.output.dac_map, [0, null, 1, null])
    assert.deepEqual(channels.mapping, [
      { lane: 0, dac: 0 },
      { lane: 1, dac: 2 }
    ])
    const chain = buildRoutingChain(status, channels)
    assert.equal(chain.output.channels.value, 4)
    assert.deepEqual(chain.output.dacMap.value, [0, null, 1, null])
    assert.equal(chain.matrix.routes.value, 2)
    const rows = laneRows(channels)
    assert.deepEqual(
      rows.map((row) => ({ lane: row.lane, dac: row.dac, physical: row.physical })),
      [
        { lane: 0, dac: 0, physical: 1 },
        { lane: 1, dac: 2, physical: 3 }
      ]
    )
    // 1-based 物理输出 3 = SL（已知位置标签），不是猜出来的。
    assert.equal(rows[1].labelKey, 'channels.label.sl')
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
    assert.ok((channels.reasons['output.channels'] ?? '').length > 0, 'degraded fixture must report an output.channels reason')

    const rows = laneRows(channels)
    assert.equal(rows.length, 0)
    const chain = buildRoutingChain(status, channels)
    assert.equal(chain.matrix.routedLanes.value, null)
    assert.equal(chain.matrix.routes.value, null)
    assert.equal(chain.output.channels.value, null)
    assert.match(chain.matrix.routes.reason ?? '', /routing section is not known/)

    const summary = buildPlanSummary(status, channels, await h.routing.fetchPlanHistory())
    assert.equal(summary.version, 3)
    assert.equal(summary.dacChannels, null)
    assert.equal(summary.routes, null)
    // T60 起 mock 也上报真实 plan 段：routingKnown=false（daemon 明确说不认识 routing），不是猜测。
    assert.equal(summary.routingKnown, false)
    assert.equal(summary.routingKnownReason, null)
  })
})

test('mock routing: unavailable channels fixture is NOT_AVAILABLE (501 not_wired) end to end', async () => {
  await withMock(async ({ mock, baseUrl }, ) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    mock.setChannelsFixture('unavailable')
    assert.deepEqual(CHANNELS_FIXTURES, ['wired', 'partial', 'degraded', 'unavailable'])

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
    // status 缺 plan.version 时回退 /api/channels.plan（T60 起 mock 有真实 plan 段），再回退 history。
    assert.equal(summary.versionSource, 'channels')
    // status 缺 latency 时回退当前历史条目的 latency_samples（daemon entries 会带该字段）。
    assert.equal(summary.latencySamples, 1023)
    assert.equal(summary.latencySource, 'history')
    assert.equal(summary.historyCurrentVersion, 7)
  })
})

/* ------------------------------------------- routing surface 两阶段（mock 契约） */

/** 注入 ApiClient 的 recording fetch：不依赖真实 daemon，断言 path/method/body 与回执处理。 */
function createSurfaceHarness(handler) {
  const recording = createRecordingFetch(handler)
  const notifications = []
  const client = new ApiClient({
    baseUrl: 'http://routing.test',
    fetchImpl: recording.fetch,
    csrfToken: () => 'csrf-routing-mock',
    onUnauthorized: () => notifications.push('client-401')
  })
  const state = createInitialControlState()
  const controller = createControlController(state, { api: createControlApi(client), now: () => 777 })
  return { state, controller, calls: recording.calls, notifications }
}

function surfaceRoute(call) {
  if (call.url.endsWith('/api/surface')) {
    return jsonResponse(200, buildRoutingPrepareAck())
  }
  if (call.url.endsWith('/api/commit')) {
    return jsonResponse(200, buildRoutingCommitAck())
  }
  if (call.url.endsWith('/api/abort')) {
    return jsonResponse(200, buildRoutingAbortAck())
  }
  return jsonResponse(404, { error: { code: 'not_found', message: 'unexpected ' + call.url } })
}

test('mock routing surface: prepare -> commit sends the exact contract body and consumes the token', async () => {
  const h = createSurfaceHarness(surfaceRoute)
  const draft = await h.controller.prepareRouting({ routes: [[0, 1], [2, 3]], dac_channels: 22 })
  assert.equal(draft?.surface, 'routing')
  assert.equal(draft?.engine_token, ROUTING_PREPARED_TOKEN)
  assert.equal(draft?.planned_version, ROUTING_PLANNED_VERSION)
  // 可选回执字段缺失 = 服务端未上报（页面显示「服务端未上报」，不臆造）。
  assert.equal(draft?.changed, null)
  assert.equal(draft?.diff, null)
  assert.equal(h.state.draft?.engine_token, ROUTING_PREPARED_TOKEN)

  assert.deepEqual(
    h.calls.map((call) => call.method + ' ' + call.url),
    ['POST http://routing.test/api/surface']
  )
  assert.deepEqual(JSON.parse(h.calls[0].body), {
    surface: 'routing',
    data: { routes: [[0, 1], [2, 3]], dac_channels: 22 }
  })

  const ack = await h.controller.commit()
  assert.equal(ack?.version, ROUTING_PLANNED_VERSION)
  assert.equal(ack?.applied_block, 1024)
  assert.equal(ack?.changed, true)
  assert.equal(h.state.draft, null)
  assert.deepEqual(
    h.calls.map((call) => call.method + ' ' + call.url),
    ['POST http://routing.test/api/surface', 'POST http://routing.test/api/commit']
  )
  assert.deepEqual(JSON.parse(h.calls[1].body), { engine_token: ROUTING_PREPARED_TOKEN })
})

test('mock routing surface: abort sends {engine_token} and drops the draft (no commit)', async () => {
  const h = createSurfaceHarness(surfaceRoute)
  await h.controller.prepareRouting({ routes: [[0, 1]] })
  const aborted = await h.controller.abort()
  assert.equal(aborted?.aborted, true)
  assert.equal(h.state.draft, null)
  assert.deepEqual(
    h.calls.map((call) => call.method + ' ' + call.url),
    ['POST http://routing.test/api/surface', 'POST http://routing.test/api/abort']
  )
  assert.deepEqual(JSON.parse(h.calls[1].body), { engine_token: ROUTING_PREPARED_TOKEN })
})

test('mock routing surface: 401 reaches the shared hook and leaves no draft (no fake success)', async () => {
  const h = createSurfaceHarness(() => jsonResponse(401, { error: { code: 'not_authenticated', message: 'login required' } }))
  const draft = await h.controller.prepareRouting({ routes: [[0, 1]] })
  assert.equal(draft, null)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.error?.category, 'unauthorized')
  assert.equal(h.state.error?.code, 'not_authenticated')
  assert.deepEqual(h.notifications, ['client-401'])
})

test('mock routing surface: 503 not_wired shows the reason and never fakes a draft/receipt', async () => {
  const h = createSurfaceHarness(() =>
    jsonResponse(503, { error: { code: 'not_wired', message: 'routing surface is not wired into rt-host' } })
  )
  const draft = await h.controller.prepareRouting({ routes: [[0, 1]] })
  assert.equal(draft, null)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.lastCommit, null)
  assert.equal(h.state.error?.category, 'not_wired')
  assert.match(h.state.error?.detail ?? '', /not wired into rt-host/)
})

test('mock routing surface: 409 no_base_plan is classified for the "prepare+commit a plan first" guidance', async () => {
  const h = createSurfaceHarness(() =>
    jsonResponse(409, { error: { code: 'no_base_plan', message: 'no applied plan to patch routing onto' } })
  )
  const draft = await h.controller.prepareRouting({ routes: [[0, 1]] })
  assert.equal(draft, null)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.error?.category, 'no_base_plan')
  assert.equal(h.state.error?.code, 'no_base_plan')
})

test('mock routing surface: failed commit keeps the draft unless the token is invalid (honest draft semantics)', async () => {
  let mode = 'commit-503'
  const h = createSurfaceHarness((call) => {
    if (call.url.endsWith('/api/surface')) {
      return jsonResponse(200, buildRoutingPrepareAck())
    }
    if (call.url.endsWith('/api/commit')) {
      if (mode === 'commit-503') {
        return jsonResponse(503, {
          error: { code: 'host_link_unavailable', message: 'daemon -> rt-host control channel is unavailable' }
        })
      }
      return jsonResponse(409, { error: { code: 'engine_token_invalid', message: 'token mismatch or no draft' } })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'unexpected ' + call.url } })
  })

  await h.controller.prepareRouting({ routes: [[0, 1]] })
  const failed = await h.controller.commit()
  assert.equal(failed, null)
  assert.equal(h.state.error?.category, 'host_link_unavailable')
  // 失败≠成功：草稿必须保留（token 仍可能有效），不能显示假回执。
  assert.equal(h.state.draft?.engine_token, ROUTING_PREPARED_TOKEN)
  assert.equal(h.state.lastCommit, null)

  mode = 'commit-token-invalid'
  const invalid = await h.controller.commit()
  assert.equal(invalid, null)
  assert.equal(h.state.error?.category, 'token_invalid')
  // token 失效说明后端草稿已不存在：本地草稿必须清掉，不能继续拿它 commit。
  assert.equal(h.state.draft, null)
})

test('mock routing: current value source is /api/channels.mapping and degrades to unknown on null', async () => {
  await withMock(async ({ mock, baseUrl }) => {
    const h = createHarness(baseUrl)
    await signIn(h)
    const wired = currentRoutingSource(await h.routing.fetchChannels())
    assert.equal(wired.source, 'mapping')
    assert.equal(wired.rows?.length, 22)
    assert.equal(wired.dacChannels, 22)

    mock.setChannelsFixture('degraded')
    const degraded = currentRoutingSource(await h.routing.fetchChannels())
    assert.equal(degraded.source, null)
    assert.equal(degraded.rows, null)
    assert.equal(degraded.known, false)
    assert.match(degraded.reason ?? '', /routing section is not known/)
  })
})
