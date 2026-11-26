/**
 * 声道页 mock daemon 集成测试（node --test；不依赖 networking / 不用旧 scripts/mock-auth-server）。
 *
 * 用 scripts/mock-test-support.mjs 的 createRecordingFetch 注入 ApiClient：断言真实调用序列
 * （PUT /api/channels stage=prepare/commit/abort 的 path/method/body）、401/503/501/502/409 语义、
 * commit_supported=false 时 store 不发 commit、以及方案上传/两阶段仍走控制面端点。
 *
 * 所有 mock 返回值都是同步构造的 Response / 立即 resolve 的 promise（node --test 默认无超时，
 * 不能有挂起的 promise）。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { ApiClient, ApiError, isApiError } from '../src/api/client.ts'
import {
  classifyChannelsError,
  createChannelsApi,
  sha256Hex
} from '../src/api/channels.ts'
import { createChannelsController, createInitialChannelsState } from '../src/stores/channels.ts'
import {
  MAPPING_PLAN_OWNED_REASON,
  buildChannelsAbortAck,
  buildChannelsCommitAck,
  buildChannelsCommitFailureDetails,
  buildChannelsMappingOnlyAck,
  buildChannelsPrepareAck,
  buildChannelsSeed
} from '../scripts/channels-fixtures.mjs'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

const CHANNELS = buildChannelsSeed('wired').channels
const PASSWORD = 'khaen-channels-mock-passphrase'

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value))
}

interface Call {
  url: string
  method: string
  headers: Record<string, string>
  body: unknown
  credentials: string | undefined
}

function pathOf(call: Call): string {
  return String(call.url).replace('http://mock-daemon', '')
}

function jsonBody(call: Call): unknown {
  // 原始字节上传（plan/upload）的 body 不是 JSON：不解析，交给调用方按需检查。
  if (typeof call.body !== 'string') {
    return undefined
  }
  return JSON.parse(call.body)
}

interface Harness {
  api: ReturnType<typeof createChannelsApi>
  calls: Call[]
  unauthorized: ApiError[]
}

function createHarness(
  handler: (call: Call, body: unknown) => Response,
  options: { csrf?: string | null; baseUrl?: string } = {}
): Harness {
  const recording = createRecordingFetch(async (call) => handler(call, jsonBody(call)))
  const unauthorized: ApiError[] = []
  const client = new ApiClient({
    baseUrl: options.baseUrl ?? 'http://mock-daemon',
    fetchImpl: recording.fetch,
    csrfToken: () => (options.csrf === undefined ? 'csrf-channel' : options.csrf),
    onUnauthorized: (error) => unauthorized.push(error)
  })
  return {
    api: createChannelsApi(client),
    calls: recording.calls as unknown as Call[],
    unauthorized
  }
}

function wiredDaemon(overrides: { prepare?: unknown; commit?: unknown; abort?: unknown } = {}) {
  return (call: Call, body: unknown): Response => {
    const path = pathOf(call)
    if (path === '/api/channels' && call.method === 'GET') {
      return jsonResponse(200, clone(CHANNELS))
    }
    if (path === '/api/channels' && call.method === 'PUT') {
      const stage = (body as Record<string, unknown>)?.stage
      if (stage === 'prepare') {
        return jsonResponse(200, clone(overrides.prepare ?? buildChannelsPrepareAck()))
      }
      if (stage === 'commit') {
        return jsonResponse(200, clone(overrides.commit ?? buildChannelsCommitAck()))
      }
      if (stage === 'abort') {
        return jsonResponse(200, clone(overrides.abort ?? buildChannelsAbortAck()))
      }
      return jsonResponse(400, { error: { code: 'bad_request', message: 'unknown stage' } })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'unexpected ' + call.method + ' ' + path } })
  }
}

function m1Error(status: number, code: string, message: string, details?: unknown): Response {
  return jsonResponse(status, { ok: false, error: { code, message, details } })
}

async function captureError(run: () => Promise<unknown>): Promise<ApiError> {
  try {
    await run()
  } catch (error) {
    assert.ok(isApiError(error), 'expected an ApiError, got: ' + String(error))
    return error
  }
  return assert.fail('expected the request to reject') as never
}

/* ------------------------------------------------------------------ 401 / CSRF */

test('mock channels: GET/PUT without a session is 401 and reaches the shared onUnauthorized hook', async () => {
  const { api, unauthorized } = createHarness(() =>
    jsonResponse(401, { error: { code: 'not_authenticated', message: 'sign in first' } })
  )
  const error = await captureError(() => api.fetchChannels())
  assert.equal(error.status, 401)
  assert.equal(error.code, 'not_authenticated')
  assert.deepEqual(unauthorized.map((item) => item.status), [401])

  const prepareError = await captureError(() => api.prepareChannels({ channels: 20 }))
  assert.equal(prepareError.status, 401)
  assert.equal(unauthorized.length, 2)
})

test('mock channels: every write carries X-KHAEN-CSRF and credentials include', async () => {
  const harness = createHarness(wiredDaemon())
  await harness.api.fetchChannels()
  await harness.api.prepareChannels({ channels: 20 })
  await harness.api.commitChannels('channels-token-3')
  await harness.api.abortChannels('channels-token-3')
  for (const call of harness.calls) {
    assert.equal(call.credentials, 'include')
    if (call.method !== 'GET') {
      assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-channel')
      assert.equal(call.headers['Content-Type'], 'application/json')
    }
  }
})

/* -------------------------------------------------- 完整两阶段：prepare→commit→abort */

test('mock channels: wired two-phase goes prepare -> commit (receipt) -> GET refresh -> abort', async () => {
  const harness = createHarness(wiredDaemon())
  const info = await harness.api.fetchChannels()
  assert.equal(info.output.channels, 22)
  assert.equal(info.hot_reconfigure.wired, true)

  const prepared = await harness.api.prepareChannels({ channels: 20, sample_rate: 48000 })
  assert.equal(prepared.commit_supported, true)
  assert.equal(prepared.engine_token, 'channels-token-3')

  const committed = await harness.api.commitChannels(prepared.engine_token!)
  assert.equal(committed.applied, true)
  assert.equal(committed.generation, 4)
  assert.equal(committed.gap_ms, 37)
  assert.equal(committed.graph_dropped, true)
  assert.equal(committed.routing?.plan_recommit_required, true)

  const aborted = await harness.api.abortChannels('channels-token-3')
  assert.equal(aborted.aborted, true)
  assert.equal(aborted.aborted_token, 'channels-token-3')

  // 三次写：prepare / commit / abort 都是 PUT /api/channels + stage 字段，body 精确。
  const writes = harness.calls.filter((call) => call.method === 'PUT')
  assert.deepEqual(
    writes.map((call) => jsonBody(call)),
    [
      { stage: 'prepare', channels: 20, sample_rate: 48000 },
      { stage: 'commit', engine_token: 'channels-token-3' },
      { stage: 'abort', engine_token: 'channels-token-3' }
    ]
  )
})

/* ---------------------------------------- commit_supported=false：store 不发 commit */

test('mock channels: mapping-only prepare returns commit_supported=false and the store never sends commit', async () => {
  const harness = createHarness((call, body) => {
    const path = pathOf(call)
    if (path === '/api/channels' && call.method === 'GET') {
      return jsonResponse(200, clone(CHANNELS))
    }
    if (path === '/api/channels' && call.method === 'PUT') {
      const stage = (body as Record<string, unknown>)?.stage
      if (stage === 'prepare') {
        return jsonResponse(200, clone(buildChannelsMappingOnlyAck()))
      }
      return jsonResponse(200, clone(buildChannelsCommitAck()))
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'nope' } })
  })

  const state = createInitialChannelsState()
  const controller = createChannelsController(state, { api: harness.api })
  await controller.load()
  const draft = await controller.prepareChannels({ dac_map: [0, null, 1, null] })
  assert.equal(draft?.commit_supported, false)
  assert.deepEqual(draft?.blockers, [MAPPING_PLAN_OWNED_REASON])
  assert.equal(draft?.engine_token, null)

  const committed = await controller.commitChannels()
  assert.equal(committed, null)
  assert.equal(state.channelsError?.category, 'commit_unsupported')
  // 只发生了 prepare 一次 PUT，没有 commit 请求（不假成功）。
  const writes = harness.calls.filter((call) => call.method === 'PUT')
  assert.equal(writes.length, 1)
  assert.deepEqual(jsonBody(writes[0]), { stage: 'prepare', dac_map: [0, null, 1, null] })

  // 无服务端 token：abort 走本地丢弃，不发请求。
  assert.equal(await controller.abortChannels(), true)
  assert.equal(harness.calls.filter((call) => call.method === 'PUT').length, 1)
})

/* ------------------------------------------------------ 错误语义：502/503/501/409 */

test('mock channels: 502 ERR_RECONFIGURE_* keeps rolled_back details and the store shows the failure', async () => {
  const harness = createHarness((call, body) => {
    const path = pathOf(call)
    if (path === '/api/channels' && call.method === 'GET') {
      return jsonResponse(200, clone(CHANNELS))
    }
    if (path === '/api/channels' && call.method === 'PUT' && (body as Record<string, unknown>)?.stage === 'prepare') {
      return jsonResponse(200, clone(buildChannelsPrepareAck()))
    }
    if (path === '/api/channels' && call.method === 'PUT' && (body as Record<string, unknown>)?.stage === 'commit') {
      return m1Error(502, 'ERR_RECONFIGURE_DEVICE', 'cannot reopen the output device', {
        ...buildChannelsCommitFailureDetails(),
        phase: 'channels_commit',
        target: 'rt-host'
      })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'nope' } })
  })

  const state = createInitialChannelsState()
  const controller = createChannelsController(state, { api: harness.api })
  await controller.load()
  await controller.prepareChannels({ channels: 20 })
  const committed = await controller.commitChannels()
  assert.equal(committed, null)
  assert.equal(state.channelsError?.category, 'reconfig_failed')
  assert.equal(state.channelsFailure?.rolled_back, true)
  assert.equal(state.channelsFailure?.device_started, false)
  assert.equal(state.channelsFailure?.applied, false)
  assert.equal(state.channelsFailure?.gap_ms, 412)
  assert.equal(state.channelsFailure?.code, 'ERR_RECONFIGURE_DEVICE')
  // 失败后草稿已被 rt-host 消费，本地不再展示可重放的 token。
  assert.equal(state.channelsDraft, null)
})

test('mock channels: 503 host_link_unavailable / 501 host_link_unsupported / 409 busy map to honest categories', async () => {
  const unavailable = createHarness(() =>
    m1Error(503, 'host_link_unavailable', 'control link down')
  )
  const down = await captureError(() => unavailable.api.prepareChannels({ channels: 20 }))
  assert.equal(down.status, 503)
  assert.equal(classifyChannelsError(down).category, 'host_link_unavailable')

  // process 目标：任何 stage 都 501 host_link_unsupported（不签发 token、不假成功）。
  const process = createHarness(() =>
    m1Error(501, 'host_link_unsupported', 'no data-plane hot reconfiguration for the process target', {
      target: 'daemon-process',
      stage: 'prepare',
      commit_supported: false
    })
  )
  const unsupported = await captureError(() => process.api.prepareChannels({ channels: 20 }))
  assert.equal(unsupported.status, 501)
  assert.equal(unsupported.code, 'host_link_unsupported')
  assert.equal(classifyChannelsError(unsupported).category, 'host_link_unsupported')

  const busy = createHarness((call, body) => {
    if ((body as Record<string, unknown>)?.stage === 'commit') {
      return m1Error(409, 'channels_busy', 'a previous channels commit is still in flight')
    }
    return jsonResponse(200, clone(buildChannelsPrepareAck()))
  })
  const conflict = await captureError(() => busy.api.commitChannels('channels-token-3'))
  assert.equal(conflict.status, 409)
  assert.equal(classifyChannelsError(conflict).category, 'conflict')
})

/* --------------------------------------------- 方案上传/两阶段（既有功能）仍走控制面 */

test('mock channels: plan upload/inspect/prepare/commit keep the control-plane POST contract', async () => {
  const sha = 'a'.repeat(64)
  const harness = createHarness((call, body) => {
    const path = pathOf(call)
    if (path === '/api/plan/upload' && call.method === 'POST') {
      // 原始字节上传：body 不在 JSON 解析里（jsonBody 对二进制会抛 / undefined）；只检查头和字节长度。
      return jsonResponse(201, {
        ok: true,
        schema: 'khaen-plan/v1',
        created: true,
        sha256: sha,
        graph_id: 'b'.repeat(64),
        size_bytes: 128,
        segments: ['content', 'matrix'],
        segment_count: 2,
        latency_samples: 1023,
        stored_path: '/var/lib/khaen/plans/' + sha + '.plan'
      })
    }
    if (path === '/api/plan/inspect' && call.method === 'POST') {
      return jsonResponse(200, {
        sha256: sha,
        graph_id: 'b'.repeat(64),
        plan: { sample_rate: 48000, lanes: 22 },
        compile: { compiled: true, latency_samples: 1023 }
      })
    }
    if (path === '/api/prepare' && call.method === 'POST') {
      return jsonResponse(200, { ok: true, engine_token: 'plan-token', planned_version: 8, graph_id: 'b'.repeat(64) })
    }
    if (path === '/api/commit' && call.method === 'POST' && (body as Record<string, unknown>)?.engine_token === 'plan-token') {
      return jsonResponse(200, { ok: true, version: 8, applied_block: 1025, all_ack: true })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'unexpected ' + call.method + ' ' + path } })
  })

  const bytes = new Uint8Array(128).fill(7)
  const expectedSha = createHash('sha256').update(Buffer.from(bytes)).digest('hex')
  const computed = await sha256Hex(bytes)
  assert.equal(computed, expectedSha)

  const upload = await harness.api.uploadPlan(bytes, 'demo.bin')
  assert.equal(upload.sha256, sha)
  assert.equal(upload.segment_count, 2)
  const uploadCall = harness.calls.find((call) => pathOf(call) === '/api/plan/upload')
  assert.ok(uploadCall !== undefined)
  assert.equal(uploadCall.method, 'POST')
  assert.equal(uploadCall.headers['Content-Type'], 'application/octet-stream')
  assert.equal((uploadCall.body as Uint8Array).length, 128)

  const inspect = await harness.api.fetchPlanInspect(upload.stored_path!, upload.sha256)
  assert.equal(inspect.compiled, true)
  const draft = await harness.api.preparePlan({ plan_path: upload.stored_path!, sha256: upload.sha256 })
  assert.equal(draft.engine_token, 'plan-token')
  const commit = await harness.api.commit('plan-token')
  assert.equal(commit.version, 8)

  const controlCommit = harness.calls.find((call) => pathOf(call) === '/api/commit')
  assert.ok(controlCommit !== undefined)
  assert.deepEqual(jsonBody(controlCommit), { engine_token: 'plan-token' })
})

test('mock channels: degraded fixture keeps nulls + reasons; mapping=null is surfaced as unknown', async () => {
  const degraded = buildChannelsSeed('degraded').channels
  const harness = createHarness((call) => {
    if (pathOf(call) === '/api/channels' && call.method === 'GET') {
      return jsonResponse(200, clone(degraded))
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'nope' } })
  })
  const info = await harness.api.fetchChannels()
  assert.equal(info.output.channels, null)
  assert.equal(info.mapping, null)
  assert.match(String(info.reasons['mapping']), /not known/)
  assert.equal(info.hot_reconfigure.wired, null)

  // degraded 下热重配不可核实：prepare 由后端 503/501 如实拒绝（这里模拟 503）。
  const rejecting = createHarness(() => m1Error(503, 'host_link_unavailable', 'rt-host channels status unavailable'))
  const error = await captureError(() => rejecting.api.prepareChannels({ channels: 20 }))
  assert.equal(classifyChannelsError(error).category, 'host_link_unavailable')
})
