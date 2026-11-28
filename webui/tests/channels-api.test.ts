import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, ApiError } from '../src/api/client.ts'
import {
  CHANNELS_MAX_OUTPUT,
  CHANNELS_PATHS,
  CHANNELS_SAMPLE_RATES_HZ,
  CHANNELS_SCHEMA,
  MOTU_KNOWN_OUTPUT_NUMBERS,
  PLAN_UPLOAD_MAX_BYTES,
  classifyChannelsError,
  createChannelsApi,
  formatChannelMap,
  motuOutputLabelKey,
  parseChannelsAbortAck,
  parseChannelsCommitAck,
  parseChannelsCommitFailure,
  CHANNELS_MAX_CONTENT,
  parseChannelsInfo,
  parseChannelsPrepareAck,
  parsePlanHistory,
  parsePlanInspect,
  parsePlanUploadAck,
  reasonForChannelsPath,
  reasonForPlanHistoryPath,
  sha256Hex,
  validateChannelsPatch
} from '../src/api/channels.ts'
import {
  GRAPH_ID,
  MAPPING_PLAN_OWNED_REASON,
  PLAN_SHA256,
  buildChannelsAbortAck,
  buildChannelsCommitAck,
  buildChannelsCommitFailureDetails,
  buildChannelsMappingOnlyAck,
  buildChannelsPrepareAck,
  buildChannelsSeed
} from '../scripts/channels-fixtures.mjs'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value))
}

async function captureError(run: () => Promise<unknown>): Promise<ApiError> {
  try {
    await run()
  } catch (error) {
    assert.ok(error instanceof ApiError, 'expected an ApiError, got: ' + String(error))
    return error
  }
  return assert.fail('expected the call to reject') as never
}

function createClient(handler: (call: any, index: number) => Response, csrf = 'csrf-test') {
  const recording = createRecordingFetch(handler)
  const client = new ApiClient({
    baseUrl: 'http://mock.local',
    fetchImpl: recording.fetch,
    csrfToken: () => csrf
  })
  return { client, calls: recording.calls, api: createChannelsApi(client) }
}

/* ------------------------------------------------------------------ GET 解析 */

test('parseChannelsInfo keeps the real khaen-channels/v1 shape (current/device/format/generation/hot_reconfigure)', () => {
  const doc = clone(buildChannelsSeed('wired').channels)
  const info = parseChannelsInfo(doc)
  assert.equal(info.schema, CHANNELS_SCHEMA)
  assert.equal(info.target, 'rt-host')
  assert.equal(info.runtime_mode, 'aes67')
  // output：channels + dac_map（lane 0..21）+ 来源
  assert.equal(info.output.channels, 22)
  assert.equal(info.output.dac_map?.length, 22)
  assert.equal(info.output.channels_source, 'rt-host-channels-status')
  // input：AES67 接收参数来自 channels_status.current；source/sdp 控制通道不报 -> null + reasons
  assert.equal(info.input.channels, 2)
  assert.deepEqual(info.input.map, [0, 1])
  assert.equal(info.input.source, null)
  assert.equal(info.input.sdp_sha256, null)
  assert.equal(info.input.streams?.length, 1)
  assert.equal(info.input.streams?.[0].primary, true)
  assert.equal(info.input.streams?.[0].group, '239.69.1.1')
  assert.equal(info.input.streams?.[0].port, 5004)
  assert.equal(info.input.streams?.[0].stats?.packets, 1200)
  assert.equal(info.input.streams?.[0].stats?.underruns, 2)
  // mapping 是方案 routing 镜像
  assert.equal(info.mapping?.length, 22)
  assert.deepEqual(info.mapping?.[3], { lane: 3, dac: 3 })
  // plan / format / device / current
  assert.equal(info.plan.version, 7)
  assert.equal(info.plan.graph_id, GRAPH_ID)
  assert.equal(info.plan.routing_known, true)
  assert.equal(info.format?.sample_rate, 48000)
  assert.equal(info.format?.block_frames, 128)
  assert.equal(info.device?.name, 'motu')
  assert.equal(info.device?.state, 'present')
  assert.equal(info.device?.probe?.available, true)
  assert.equal(info.current?.channels, 22)
  assert.equal(info.current?.content_channels, 2)
  assert.equal(info.current?.aes67_group, '239.69.1.1')
  assert.equal(info.current?.aes67_port, 5004)
  assert.equal(info.current?.aes67_payload_type, 98)
  assert.equal(info.current?.jitter_frames, 256)
  // 数据面状态
  assert.equal(info.generation, 3)
  assert.equal(info.busy, false)
  assert.equal(info.pending, null)
  assert.equal(info.last_gap_ms, 37)
  assert.equal(info.last_result?.gap_ms, 37)
  assert.equal(info.hot_reconfigure.wired, true)
  assert.equal(info.hot_reconfigure.reason, null)
  assert.deepEqual(info.not_wired, [])
  assert.match(String(info.reasons['input.source']), /not fabricated/)
})

test('parseChannelsInfo degraded shape keeps nulls + reasons and mapping=null (current value unknown)', () => {
  const info = parseChannelsInfo(clone(buildChannelsSeed('degraded').channels))
  assert.equal(info.output.channels, null)
  assert.equal(info.output.dac_map, null)
  assert.equal(info.mapping, null)
  // mapping=null 表示「当前值未知」：页面拿 reasons['mapping'] 显示原因，不得归一化成 []。
  assert.match(String(reasonForChannelsPath(info, 'mapping')), /not known/)
  assert.equal(info.current, null)
  assert.equal(info.format, null)
  assert.equal(info.device, null)
  assert.equal(info.hot_reconfigure.wired, null)
  assert.match(String(info.hot_reconfigure.reason), /cannot verify/)
  assert.equal(info.not_wired.length, 1)
  assert.equal(info.not_wired[0].path, 'channels.hot_reconfigure')
  assert.match(String(info.reasons['output.channels']), /fail to report output channels/)
})

test('parseChannelsInfo accepts partial routing (dac_map null items) and rejects malformed documents', () => {
  const partial = parseChannelsInfo(clone(buildChannelsSeed('partial').channels))
  assert.equal(partial.output.channels, 4)
  // dac_map 的 null 项 = 该 DAC 没有 lane 喂入（合法形态），不能当畸形响应。
  assert.deepEqual(partial.output.dac_map, [0, null, 1, null])
  assert.deepEqual(partial.mapping, [
    { lane: 0, dac: 0 },
    { lane: 1, dac: 2 }
  ])
  assert.equal(formatChannelMap(partial.output.dac_map), '0,-,1,-')

  const wired = clone(buildChannelsSeed('wired').channels)
  assert.throws(() => parseChannelsInfo({ ...wired, schema: 'nope' }), /schema must be/)
  assert.throws(() => parseChannelsInfo({ ...wired, mapping: [{ lane: 'x', dac: 0 }] }), /mapping/)
  assert.throws(() => parseChannelsInfo({ ...wired, reasons: { x: 1 } }), /reasons\.x/)
  assert.throws(() => parseChannelsInfo({ ...wired, current: [] }), /current/)
  assert.throws(() => parseChannelsInfo({ ...wired, hot_reconfigure: 'wired' }), /hot_reconfigure/)
  assert.throws(() => parseChannelsInfo(null), /must be an object/)
})

test('MOTU output labels only cover the known positions and constants match the real limits', () => {
  assert.equal(motuOutputLabelKey(1), 'channels.label.l')
  assert.equal(motuOutputLabelKey(8), 'channels.label.lfe')
  assert.equal(motuOutputLabelKey(7), null)
  assert.deepEqual([...MOTU_KNOWN_OUTPUT_NUMBERS], [1, 2, 3, 4, 5, 6, 8])
  assert.equal(CHANNELS_MAX_OUTPUT, 22)
  assert.deepEqual([...CHANNELS_SAMPLE_RATES_HZ], [48000, 96000])
  assert.equal(formatChannelMap([0, 1]), '0,1')
  assert.equal(formatChannelMap([]), null)
  assert.equal(formatChannelMap(null), null)
})

/* ------------------------------------------------------------- PUT prepare 解析 */

test('parseChannelsPrepareAck keeps changed/diff/current/target/graph/admission/warnings/token', () => {
  const ack = parseChannelsPrepareAck(clone(buildChannelsPrepareAck()))
  assert.equal(ack.stage, 'prepare')
  assert.equal(ack.target, 'rt-host')
  assert.equal(ack.engine_token, 'channels-token-3')
  assert.equal(ack.planned_generation, 4)
  assert.equal(ack.changed, true)
  assert.equal(ack.commit_supported, true)
  assert.deepEqual(ack.diff, { channels: { from: 22, to: 20 }, aes67_map: { from: [0, 1], to: [0, 1] } })
  assert.equal((ack.current as Record<string, unknown>).channels, 22)
  assert.equal(ack.graph?.action, 'dropped')
  assert.match(String(ack.graph?.reason), /dropped at commit/)
  assert.equal(ack.admission?.accepted, true)
  assert.equal(ack.format?.channels, 20)
  assert.equal(ack.preview?.old_channels, 22)
  assert.equal(ack.preview?.new_channels, 20)
  assert.equal(ack.preview?.audio_gap_ms_estimate, null)
  assert.match(String(ack.preview?.audio_gap_ms_estimate_reason), /measured at commit/)
  assert.equal(ack.warnings.length, 1)
  assert.deepEqual(ack.blockers, [])
})

test('parseChannelsPrepareAck mapping-only draft has commit_supported=false, no token and blockers', () => {
  const ack = parseChannelsPrepareAck(clone(buildChannelsMappingOnlyAck()))
  assert.equal(ack.commit_supported, false)
  assert.equal(ack.engine_token, null)
  assert.equal(ack.mapping?.requested, true)
  assert.equal(ack.mapping?.applied_by, 'plan routing section (plan prepare + commit)')
  assert.equal(ack.mapping?.applies_with_this_commit, false)
  assert.deepEqual(ack.blockers, [MAPPING_PLAN_OWNED_REASON])
  assert.equal(ack.not_wired.length, 1)
  assert.equal(ack.not_wired[0].path, 'channels.hot_reconfigure.mapping')

  // 合同不变量：commit_supported 必须显式；缺 token / 假 token 都是 invalid_response（不假成功）。
  assert.throws(() => parseChannelsPrepareAck({ ok: true, engine_token: 't' }), /commit_supported/)
  assert.throws(
    () => parseChannelsPrepareAck({ ok: true, commit_supported: true }),
    /requires a non-empty engine_token/
  )
  assert.throws(
    () => parseChannelsPrepareAck({ ok: true, commit_supported: false, engine_token: 'fake' }),
    /must not issue/
  )
  assert.throws(() => parseChannelsPrepareAck({ ok: false, commit_supported: true, engine_token: 't' }), /ok must be true/)
  assert.throws(
    () => parseChannelsPrepareAck({ ok: true, commit_supported: true, engine_token: 't', warnings: [1] }),
    /warnings/
  )
})

/* -------------------------------------------------- PUT commit / abort / failure 解析 */

test('parseChannelsCommitAck keeps the real rt-host receipt (applied/generation/gap_ms/format/graph_dropped/routing)', () => {
  const ack = parseChannelsCommitAck(clone(buildChannelsCommitAck()))
  assert.equal(ack.applied, true)
  assert.equal(ack.changed, true)
  assert.equal(ack.idempotent, false)
  assert.equal(ack.generation, 4)
  assert.equal(ack.gap_ms, 37)
  assert.equal(ack.format?.channels, 20)
  assert.equal(ack.format?.sample_rate, 48000)
  assert.equal(ack.graph_dropped, true)
  assert.equal(ack.rolled_back, false)
  assert.equal(ack.routing?.applied, false)
  assert.equal(ack.routing?.plan_recommit_required, true)
  assert.match(String(ack.routing?.reason), /never changes the plan routing/)
  assert.match(String(ack.note), /hw readback/)
  assert.equal(ack.local?.mirror_updated, true)
  assert.throws(() => parseChannelsCommitAck({ ok: false }), /ok must be true/)
  assert.throws(() => parseChannelsCommitAck({ ok: true, gap_ms: 'fast' }), /gap_ms/)
})

test('parseChannelsAbortAck keeps aborted/aborted_token/local', () => {
  const ack = parseChannelsAbortAck(clone(buildChannelsAbortAck()))
  assert.equal(ack.aborted, true)
  assert.equal(ack.aborted_token, 'channels-token-3')
  assert.match(String(ack.note), /running data-plane configuration is untouched/)
  assert.equal(ack.local?.pending_routing_discarded, true)
  assert.throws(() => parseChannelsAbortAck({ ok: true, aborted: 'yes' }), /aborted/)
})

test('parseChannelsCommitFailure exposes details.rolled_back/device_started/applied verbatim', () => {
  const failure = parseChannelsCommitFailure(clone(buildChannelsCommitFailureDetails()))
  assert.ok(failure !== null)
  assert.equal(failure.code, 'ERR_RECONFIGURE_DEVICE')
  assert.equal(failure.stage, 'reopen')
  assert.equal(failure.rolled_back, true)
  assert.equal(failure.device_started, false)
  assert.equal(failure.receiver_running, false)
  assert.equal(failure.applied, false)
  assert.equal(failure.gap_ms, 412)
  assert.equal(failure.generation, 3)
  assert.equal(failure.phase, 'channels_commit')
  assert.equal(failure.raw.rolled_back, true)
  assert.equal(parseChannelsCommitFailure(null), null)
  assert.equal(parseChannelsCommitFailure('nope'), null)
})

/* ------------------------------------------------------------------ 前端校验 */

test('validateChannelsPatch enforces the real ranges from khaen_core::rt / aes67_input', () => {
  assert.equal(validateChannelsPatch({}), 'empty')
  assert.equal(validateChannelsPatch({ channels: 0 }), 'channels_range')
  assert.equal(validateChannelsPatch({ channels: CHANNELS_MAX_OUTPUT + 1 }), 'channels_range')
  assert.equal(validateChannelsPatch({ sample_rate: 44100 }), 'sample_rate_value')
  assert.equal(validateChannelsPatch({ sample_rate: 96000 }), null)
  assert.equal(validateChannelsPatch({ content_channels: 2 }), null)
  assert.equal(validateChannelsPatch({ content_channels: 0 }), 'content_channels_range')
  assert.equal(validateChannelsPatch({ content_channels: CHANNELS_MAX_CONTENT + 1 }), 'content_channels_range')
  assert.equal(validateChannelsPatch({ channels: 22, content_channels: 2 }), null)
  assert.equal(validateChannelsPatch({ sdp: 'v=0\r\ns=test\r\n' }), null)
  assert.equal(validateChannelsPatch({ sdp: '   ' }), 'sdp_value')
  assert.equal(validateChannelsPatch({ sdp: 'v=0', aes67_port: 5004 }), 'sdp_conflict')
  // IO I2b：单播本端地址合法；0.0.0.0/广播没有具体接收端。
  assert.equal(validateChannelsPatch({ aes67_group: '192.168.1.1' }), null)
  assert.equal(validateChannelsPatch({ aes67_group: '0.0.0.0' }), 'aes67_group_value')
  assert.equal(validateChannelsPatch({ aes67_group: '255.255.255.255' }), 'aes67_group_value')
  assert.equal(validateChannelsPatch({ aes67_group: '239.69.1.1' }), null)
  assert.equal(validateChannelsPatch({ aes67_port: 0 }), 'aes67_port_value')
  assert.equal(validateChannelsPatch({ aes67_port: 5004 }), null)
  assert.equal(validateChannelsPatch({ aes67_payload_type: 95 }), 'payload_type_value')
  assert.equal(validateChannelsPatch({ aes67_channels: 9 }), 'aes67_channels_range')
  assert.equal(validateChannelsPatch({ aes67_channels: 2, aes67_map: [0] }), 'aes67_map_length')
  assert.equal(validateChannelsPatch({ aes67_channels: 2, aes67_map: [0, 0] }), 'aes67_map_value')
  assert.equal(validateChannelsPatch({ channels: 2, aes67_channels: 2, aes67_map: [0, 2] }), 'aes67_map_value')
  assert.equal(validateChannelsPatch({ jitter_frames: -1 }), 'jitter_frames_value')
  assert.equal(validateChannelsPatch({ dac_map: [0, 0] }), 'dac_map_value')
  assert.equal(validateChannelsPatch({ channels: 4, dac_map: [0, 1] }), 'dac_map_length')
  assert.equal(validateChannelsPatch({ dac_map: [0, null, 2, null] }), null)
  assert.equal(validateChannelsPatch({ mapping: [{ lane: 0, dac: 0 }, { lane: 0, dac: 1 }] }), 'mapping_value')
  assert.equal(
    validateChannelsPatch({ mapping: [{ lane: 0, dac: 0 }], dac_map: [1] }),
    'mapping_dac_map_conflict'
  )
  assert.equal(
    validateChannelsPatch({
      channels: 22,
      sample_rate: 48000,
      aes67_group: '239.69.1.1',
      aes67_port: 5004,
      aes67_payload_type: 98,
      aes67_channels: 2,
      aes67_map: [0, 1],
      jitter_frames: 256
    }),
    null
  )
})

test('sha256Hex matches the standard sha256 of "abc"', async () => {
  const digest = await sha256Hex(new TextEncoder().encode('abc'))
  assert.equal(digest, 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad')
})

/* ------------------------------------------------------- 方案文件/历史/inspect 解析 */

test('parsePlanUploadAck accepts the real daemon receipt (segments array, segment_count, stored_path)', () => {
  const sha = 'a'.repeat(64)
  const ack = parsePlanUploadAck({
    ok: true,
    schema: 'khaen-plan/v1',
    created: true,
    deduplicated: false,
    sha256: sha,
    graph_id: 'b'.repeat(64),
    size_bytes: 4096,
    sample_rate: 48000,
    block_size: 128,
    segments: ['content', 'matrix', 'protection', 'routing'],
    segment_count: 4,
    latency_samples: 1023,
    routing: null,
    layout: { in_channels: 2, out_lanes: 22 },
    admission: { accepted: true, rejections: [] },
    source: 'upload',
    recorded_at: '2026-11-26T07:00:00.000Z',
    stored_path: '/var/lib/khaen/plans/' + sha + '.plan'
  })
  assert.deepEqual(ack.segments, ['content', 'matrix', 'protection', 'routing'])
  assert.equal(ack.segment_count, 4)
  assert.equal(ack.stored_path, '/var/lib/khaen/plans/' + sha + '.plan')
  // 真实 daemon 的旧字段兼容：segments 是数字时归一化到 segment_count，不伪造区段名。
  const legacy = parsePlanUploadAck({ sha256: sha, segments: 3 })
  assert.equal(legacy.segments, null)
  assert.equal(legacy.segment_count, 3)
  assert.throws(() => parsePlanUploadAck({ sha256: 'ABC' }), /sha256/)
})

test('parsePlanHistory accepts the real daemon shape and does not invent current_version', () => {
  const history = parsePlanHistory({
    schema: 'khaen-plan-history/v1',
    source: 'rt-host',
    current: { version: 7, graph_id: GRAPH_ID },
    entries: [
      { version: 7, graph_id: GRAPH_ID, recorded_at: 't7', sha256: PLAN_SHA256 },
      { version: 6, graph_id: null, recorded_at: 't6' }
    ],
    reasons: {}
  })
  assert.equal(history.current_version, 7)
  assert.equal(history.current_graph_id, GRAPH_ID)
  assert.deepEqual(history.versions.map((entry) => entry.version), [6, 7])
  assert.equal(history.versions[1].applied_at, 't7')

  const explicitNull = parsePlanHistory({
    current: { version: null },
    entries: [{ version: 5 }],
    reasons: { 'history.current': 'not reported by the live control plane' }
  })
  assert.equal(explicitNull.current_version, null)
  assert.equal(reasonForPlanHistoryPath(explicitNull, 'history.current'), 'not reported by the live control plane')
  assert.throws(() => parsePlanHistory({ versions: {} }), /array/)
})

test('parsePlanInspect accepts the control-plane nested shape and flat fallbacks', () => {
  const inspect = parsePlanInspect({
    sha256: PLAN_SHA256,
    graph_id: GRAPH_ID,
    plan: { block_size: 128, sample_rate: 48000, in_channels: 2, lanes: 22, dac_channels: 22 },
    admission: { accepted: true, rejections: [] },
    compile: { compiled: true, latency_samples: 1023 }
  })
  assert.equal(inspect.sample_rate, 48000)
  assert.equal(inspect.lanes, 22)
  assert.equal(inspect.latency_samples, 1023)
  assert.equal(inspect.compiled, true)
  const rejected = parsePlanInspect({
    admission: { accepted: false, rejections: [{ code: 'over_budget', detail: 'taps' }] },
    compile: { compiled: false, error_code: 'admission_rejected', message: 'rejected' }
  })
  assert.equal(rejected.admission_accepted, false)
  assert.deepEqual(rejected.rejections, [{ code: 'over_budget', detail: 'taps' }])
  assert.equal(rejected.error_message, 'rejected')
})

/* ------------------------------------------------- PUT /api/channels 契约测试 */

test('channels API uses documented paths/methods/bodies for the PUT two-phase and plan flows', async () => {
  const bodies: Array<{ method: string; path: string; body: unknown }> = []
  const { api, calls } = createClient((call) => {
    const path = String(call.url).replace('http://mock.local', '')
    if (path === CHANNELS_PATHS.planUpload) {
      bodies.push({ method: call.method, path, body: call.body })
      return jsonResponse(201, {
        ok: true,
        schema: 'khaen-plan/v1',
        created: true,
        sha256: 'a'.repeat(64),
        graph_id: 'b'.repeat(64),
        size_bytes: 128,
        sample_rate: 48000,
        block_size: 128,
        segments: ['content', 'matrix'],
        segment_count: 2,
        latency_samples: 1023,
        routing: null,
        layout: { in_channels: 2, out_lanes: 22 },
        admission: { accepted: true, rejections: [] },
        source: 'upload',
        recorded_at: '2026-11-26T07:00:00.000Z',
        stored_path: '/var/lib/khaen/plans/' + 'a'.repeat(64) + '.plan'
      })
    }
    const body = call.body === undefined || call.body === null ? undefined : JSON.parse(String(call.body))
    bodies.push({ method: call.method, path, body })
    if (path === CHANNELS_PATHS.channels && call.method === 'GET') {
      return jsonResponse(200, clone(buildChannelsSeed('partial').channels))
    }
    if (path === CHANNELS_PATHS.channels && call.method === 'PUT') {
      const stage = (body as Record<string, unknown>)?.stage
      if (stage === 'prepare') {
        // docs/API.md §5：纯 mapping 改动 -> commit_supported=false + blockers（不签发 token）。
        const mappingOnly = (body as Record<string, unknown>)?.dac_map !== undefined && (body as Record<string, unknown>)?.channels === undefined
        return jsonResponse(200, clone(mappingOnly ? buildChannelsMappingOnlyAck() : buildChannelsPrepareAck()))
      }
      if (stage === 'commit') {
        return jsonResponse(200, clone(buildChannelsCommitAck()))
      }
      if (stage === 'abort') {
        return jsonResponse(200, clone(buildChannelsAbortAck()))
      }
      return jsonResponse(400, { error: { code: 'bad_request', message: 'bad stage' } })
    }
    if (path === CHANNELS_PATHS.planPrepare) {
      return jsonResponse(200, { ok: true, engine_token: 'plan-token', planned_version: 8, graph_id: 'b'.repeat(64) })
    }
    if (path === CHANNELS_PATHS.planHistory) {
      return jsonResponse(200, { schema: 'khaen-plan-history/v1', current: { version: 7 }, entries: [], reasons: {} })
    }
    if (path === CHANNELS_PATHS.planInspect) {
      return jsonResponse(200, { sha256: PLAN_SHA256, graph_id: GRAPH_ID, plan: { sample_rate: 48000 }, compile: { compiled: true } })
    }
    if (path === CHANNELS_PATHS.planRollback) {
      return jsonResponse(200, { ok: true, engine_token: 'rb', rolled_back_to: 5, planned_version: 9 })
    }
    if (path === CHANNELS_PATHS.commit) {
      return jsonResponse(200, { ok: true, version: 9, applied_block: 1, all_ack: true })
    }
    if (path === CHANNELS_PATHS.abort) {
      return jsonResponse(200, { ok: true, aborted: true })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'nope' } })
  })

  // GET /api/channels
  const info = await api.fetchChannels()
  assert.equal(info.output.channels, 4)
  assert.equal(calls.at(-1)?.method, 'GET')
  assert.deepEqual(bodies.at(-1), { method: 'GET', path: CHANNELS_PATHS.channels, body: undefined })

  // PUT stage=prepare：body 精确 = {stage:'prepare', ...扁平字段}
  const patch = {
    channels: 20,
    sample_rate: 48000,
    aes67_group: '239.69.1.1',
    aes67_port: 5004,
    aes67_payload_type: 98,
    aes67_channels: 2,
    aes67_map: [0, 1],
    jitter_frames: 256
  }
  const prepared = await api.prepareChannels(patch)
  assert.equal(prepared.engine_token, 'channels-token-3')
  assert.deepEqual(bodies.at(-1), {
    method: 'PUT',
    path: '/api/channels',
    body: { stage: 'prepare', ...patch }
  })

  // 纯 mapping 改动（没有 rt-host 可应用字段）：commit_supported=false + blockers，且不签发 token。
  const mappingOnly = await api.prepareChannels({ dac_map: [0, null, 1, null] })
  assert.equal(mappingOnly.commit_supported, false)
  assert.equal(mappingOnly.engine_token, null)
  assert.deepEqual(mappingOnly.blockers, [MAPPING_PLAN_OWNED_REASON])
  assert.deepEqual(bodies.at(-1), {
    method: 'PUT',
    path: '/api/channels',
    body: { stage: 'prepare', dac_map: [0, null, 1, null] }
  })

  // SDP 补丁：raw SDP 原样放进 prepare body（rt-host 负责解析/替换接收参数）。
  const sdpText = 'v=0\r\nc=IN IP4 239.69.1.9\r\nm=audio 5004 RTP/AVP 96\r\na=rtpmap:96 L24/48000/2\r\n'
  await api.prepareChannels({ sdp: sdpText })
  assert.deepEqual(bodies.at(-1), {
    method: 'PUT',
    path: '/api/channels',
    body: { stage: 'prepare', sdp: sdpText }
  })

  // 独立输入宽度：content_channels 与输出 channels 一起进 prepare body。
  await api.prepareChannels({ channels: 22, content_channels: 2 })
  assert.deepEqual(bodies.at(-1), {
    method: 'PUT',
    path: '/api/channels',
    body: { stage: 'prepare', channels: 22, content_channels: 2 }
  })

  // PUT stage=commit：body 精确 = {stage:'commit', engine_token}
  const committed = await api.commitChannels('channels-token-3')
  assert.equal(committed.gap_ms, 37)
  assert.deepEqual(bodies.at(-1), {
    method: 'PUT',
    path: '/api/channels',
    body: { stage: 'commit', engine_token: 'channels-token-3' }
  })

  // PUT stage=abort：body 精确 = {stage:'abort', engine_token}
  const aborted = await api.abortChannels('channels-token-3')
  assert.equal(aborted.aborted, true)
  assert.deepEqual(bodies.at(-1), {
    method: 'PUT',
    path: '/api/channels',
    body: { stage: 'abort', engine_token: 'channels-token-3' }
  })

  // 方案流仍是控制面的 POST /api/prepare|commit|abort|rollback（不被 channels 两阶段替代）。
  const upload = await api.uploadPlan(new Uint8Array(128).fill(7), 'demo.bin')
  assert.equal(upload.stored_path, '/var/lib/khaen/plans/' + 'a'.repeat(64) + '.plan')
  assert.equal(calls.at(-1)?.method, 'POST')
  assert.equal(calls.at(-1)?.headers['Content-Type'], 'application/octet-stream')
  await api.preparePlan({ plan_path: upload.stored_path!, sha256: upload.sha256 })
  assert.deepEqual(bodies.at(-1), {
    method: 'POST',
    path: CHANNELS_PATHS.planPrepare,
    body: { plan_path: upload.stored_path, plan_sha256: upload.sha256 }
  })
  await api.commit('plan-token')
  assert.deepEqual(bodies.at(-1), { method: 'POST', path: CHANNELS_PATHS.commit, body: { engine_token: 'plan-token' } })
  await api.abort('plan-token')
  assert.deepEqual(bodies.at(-1), { method: 'POST', path: CHANNELS_PATHS.abort, body: { engine_token: 'plan-token' } })
  await api.rollbackPlan(5)
  assert.deepEqual(bodies.at(-1), { method: 'POST', path: CHANNELS_PATHS.planRollback, body: { target: 5 } })
  await api.fetchPlanHistory()
  assert.equal(calls.at(-1)?.method, 'GET')
  await api.fetchPlanInspect(upload.stored_path!, upload.sha256)
  assert.deepEqual(bodies.at(-1), {
    method: 'POST',
    path: CHANNELS_PATHS.planInspect,
    body: { plan_path: upload.stored_path, plan_sha256: upload.sha256 }
  })

  // 写请求一律带 CSRF + credential；读请求也带 credential。
  for (const call of calls) {
    assert.equal(call.credentials, 'include')
    if (call.method !== 'GET') {
      assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-test')
    }
  }
})

test('uploadPlan rejects oversized/too-small files before any network call', async () => {
  let calls = 0
  const { api } = createClient(() => {
    calls += 1
    return jsonResponse(500, {})
  })
  const tooLarge = await captureError(() => api.uploadPlan(new Uint8Array(PLAN_UPLOAD_MAX_BYTES + 1), 'big.bin'))
  assert.equal(tooLarge.status, 413)
  assert.equal(tooLarge.code, 'upload_too_large')
  const tooSmall = await captureError(() => api.uploadPlan(new Uint8Array(8), 'small.bin'))
  assert.equal(tooSmall.status, 400)
  assert.equal(tooSmall.code, 'invalid_plan')
  assert.equal(calls, 0)
})

/* ------------------------------------------------------------ 错误归类 */

test('classifyChannelsError maps the channels two-phase error codes and keeps rolled_back details', () => {
  const reconfig = new ApiError({
    status: 502,
    code: 'ERR_RECONFIGURE_DEVICE',
    message: 'cannot reopen the output device',
    details: clone(buildChannelsCommitFailureDetails())
  })
  const reconfigInfo = classifyChannelsError(reconfig)
  assert.equal(reconfigInfo.category, 'reconfig_failed')
  assert.equal(reconfigInfo.i18nKey, 'channelsEdit.error.reconfigFailed')
  assert.equal(reconfigInfo.failure?.rolled_back, true)
  assert.equal(reconfigInfo.failure?.applied, false)
  assert.equal(reconfigInfo.failure?.gap_ms, 412)
  assert.equal(reconfigInfo.retryable, true)

  const cases: Array<[ApiError, string, string]> = [
    [new ApiError({ status: 501, code: 'host_link_unsupported', message: 'process' }), 'host_link_unsupported', 'channelsEdit.error.hostLinkUnsupported'],
    [new ApiError({ status: 503, code: 'host_link_unavailable', message: 'down' }), 'host_link_unavailable', 'channelsEdit.error.hostLinkUnavailable'],
    [new ApiError({ status: 503, code: 'device_absent', message: 'absent' }), 'host_link_unavailable', 'channelsEdit.error.hostLinkUnavailable'],
    [new ApiError({ status: 501, code: 'not_wired', message: 'no' }), 'not_wired', 'channelsEdit.error.notWired'],
    [new ApiError({ status: 409, code: 'channels_busy', message: 'busy' }), 'conflict', 'channelsEdit.error.conflict'],
    [new ApiError({ status: 409, code: 'engine_token_invalid', message: 'token' }), 'conflict', 'channelsEdit.error.conflict'],
    [new ApiError({ status: 400, code: 'bad_request', message: 'bad' }), 'invalid_request', 'channelsEdit.error.invalidRequest'],
    [new ApiError({ status: 401, code: 'not_authenticated', message: 'auth' }), 'unauthorized', 'channelsEdit.error.unauthorized'],
    [new ApiError({ status: 0, code: 'network_error', message: 'down' }), 'network', 'channelsEdit.error.network'],
    [new ApiError({ status: 200, code: 'invalid_response', message: 'bad' }), 'invalid_response', 'channelsEdit.error.invalidResponse']
  ]
  for (const [error, category, i18nKey] of cases) {
    const info = classifyChannelsError(error)
    assert.equal(info.category, category)
    assert.equal(info.i18nKey, i18nKey)
  }
  const unknown502 = classifyChannelsError(new ApiError({ status: 502, code: 'gateway_error', message: 'x' }))
  assert.equal(unknown502.category, 'reconfig_failed')
  assert.equal(classifyChannelsError(new Error('boom')).category, 'unknown')
})
