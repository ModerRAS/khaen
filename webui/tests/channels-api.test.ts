import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, ApiError } from '../src/api/client.ts'
import {
  CHANNELS_PATHS,
  CHANNELS_SCHEMA,
  MOTU_KNOWN_OUTPUT_NUMBERS,
  PLAN_UPLOAD_MAX_BYTES,
  classifyChannelsError,
  createChannelsApi,
  formatChannelMap,
  motuOutputLabelKey,
  parseChannelsInfo,
  parseChannelsPrepareAck,
  parsePlanHistory,
  parsePlanInspect,
  parsePlanUploadAck,
  sha256Hex,
  validateChannelsPatch
} from '../src/api/channels.ts'
import { buildChannelsSeed } from '../scripts/channels-fixtures.mjs'
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

const WIRED_DOC = buildChannelsSeed('wired').channels

test('parseChannelsInfo keeps the wired 22-channel layout and mapping', () => {
  const info = parseChannelsInfo(clone(WIRED_DOC))
  assert.equal(info.schema, CHANNELS_SCHEMA)
  assert.equal(info.output.channels, 22)
  assert.equal(info.output.dac_map.length, 22)
  assert.equal(info.input.channels, 2)
  assert.deepEqual(info.input.map, [0, 1])
  assert.equal(info.input.source, 'sdp')
  assert.match(String(info.input.sdp_sha256), /^[0-9a-f]{64}$/)
  assert.equal(info.mapping.length, 22)
  assert.deepEqual(info.mapping[3], { lane: 3, dac: 3 })
  assert.deepEqual(info.reasons, {})
  assert.equal(info.not_wired.length, 1)
})

test('parseChannelsInfo accepts null groups with reasons (degraded shape) and rejects schema drift', () => {
  const degraded = buildChannelsSeed('degraded').channels
  const info = parseChannelsInfo(clone(degraded))
  assert.equal(info.output.channels, null)
  assert.equal(info.output.dac_map, null)
  assert.equal(info.input.source, null)
  assert.equal(info.mapping.length, 0)
  assert.match(String(info.reasons['output.channels']), /unavailable/)
  assert.throws(() => parseChannelsInfo({ ...clone(WIRED_DOC), schema: 'nope' }), /schema must be/)
  assert.throws(() => parseChannelsInfo({ ...clone(WIRED_DOC), mapping: [{ lane: 'x', dac: 0 }] }), /mapping/)
  assert.throws(() => parseChannelsInfo({ ...clone(WIRED_DOC), reasons: { x: 1 } }), /reasons\.x/)
  assert.throws(() => parseChannelsInfo(null), /must be an object/)
})

test('MOTU output labels only cover the known positions', () => {
  assert.equal(motuOutputLabelKey(1), 'channels.label.l')
  assert.equal(motuOutputLabelKey(2), 'channels.label.r')
  assert.equal(motuOutputLabelKey(3), 'channels.label.sl')
  assert.equal(motuOutputLabelKey(4), 'channels.label.sr')
  assert.equal(motuOutputLabelKey(5), 'channels.label.tf')
  assert.equal(motuOutputLabelKey(6), 'channels.label.tfr')
  assert.equal(motuOutputLabelKey(8), 'channels.label.lfe')
  // 未知位置不猜（页面显示 OUT n）。
  assert.equal(motuOutputLabelKey(7), null)
  assert.equal(motuOutputLabelKey(9), null)
  assert.equal(motuOutputLabelKey(22), null)
  assert.deepEqual([...MOTU_KNOWN_OUTPUT_NUMBERS], [1, 2, 3, 4, 5, 6, 8])
  assert.equal(formatChannelMap([0, 1]), '0,1')
  assert.equal(formatChannelMap([]), null)
  assert.equal(formatChannelMap(null), null)
})

test('parsePlanUploadAck requires a lowercase 64-hex sha256', () => {
  const ack = parsePlanUploadAck({
    upload_id: 'abc',
    sha256: 'a'.repeat(64),
    graph_id: 'b'.repeat(64),
    version: 8,
    segments: 3,
    latency_samples: 1023
  })
  assert.equal(ack.upload_id, 'abc')
  assert.equal(ack.version, 8)
  assert.throws(() => parsePlanUploadAck({ sha256: 'ABC' }), /sha256/)
  assert.throws(() => parsePlanUploadAck({ sha256: undefined }), /sha256/)
})

test('parsePlanHistory accepts object and array shapes and sorts by version', () => {
  const object = parsePlanHistory({
    schema: 'khaen-plan-history/v1',
    current_version: 7,
    versions: [
      { version: 7, graph_id: 'g7', sha256: 's7', applied_at: 't7', source: 'upload' },
      { version: 5, graph_id: 'g5', sha256: null, applied_at: 't5', source: 'calibration' }
    ]
  })
  assert.equal(object.current_version, 7)
  assert.deepEqual(object.versions.map((entry) => entry.version), [5, 7])
  assert.equal(object.versions[0].sha256, null)

  const array = parsePlanHistory([{ version: 3 }, { version: 2 }])
  assert.equal(array.current_version, 3)
  assert.deepEqual(array.versions.map((entry) => entry.version), [2, 3])
  assert.throws(() => parsePlanHistory({ versions: {} }), /array/)
  assert.throws(() => parsePlanHistory({ versions: [{ version: 'x' }] }), /version/)
})

test('parsePlanInspect accepts the control-plane nested shape and flat fallbacks', () => {
  const nested = clone(buildChannelsSeed('wired').inspect)
  const inspect = parsePlanInspect(nested)
  assert.equal(inspect.sha256, nested.sha256)
  assert.equal(inspect.graph_id, nested.graph_id)
  assert.equal(inspect.sample_rate, 48000)
  assert.equal(inspect.block_size, 128)
  assert.equal(inspect.in_channels, 2)
  assert.equal(inspect.lanes, 22)
  assert.equal(inspect.dac_channels, 22)
  assert.equal(inspect.latency_samples, 1023)
  assert.equal(inspect.compiled, true)
  assert.equal(inspect.admission_accepted, true)

  const flat = parsePlanInspect({ sha256: 'a'.repeat(64), graph_id: 'b'.repeat(64), latency_samples: 99, sample_rate: 96000 })
  assert.equal(flat.sample_rate, 96000)
  assert.equal(flat.latency_samples, 99)

  const rejected = parsePlanInspect({
    admission: { accepted: false, rejections: [{ code: 'over_budget', detail: 'taps' }] },
    compile: { compiled: false, error_code: 'admission_rejected', message: 'rejected' }
  })
  assert.equal(rejected.admission_accepted, false)
  assert.deepEqual(rejected.rejections, [{ code: 'over_budget', detail: 'taps' }])
  assert.equal(rejected.error_message, 'rejected')
})

test('parseChannelsPrepareAck keeps preview/warnings/blockers and requires a token', () => {
  const ack = parseChannelsPrepareAck({
    ok: true,
    engine_token: 'token-channels',
    planned_version: 9,
    changed: true,
    preview: {
      old: { channels: 22, input_channels: 2 },
      new: { channels: 20, input_channels: 2 },
      audio_gap_ms_estimate: 1200
    },
    warnings: ['audio output will be interrupted'],
    blockers: []
  })
  assert.equal(ack.engine_token, 'token-channels')
  assert.equal(ack.preview?.old_channels, 22)
  assert.equal(ack.preview?.new_channels, 20)
  assert.equal(ack.preview?.audio_gap_ms_estimate, 1200)
  assert.deepEqual(ack.warnings, ['audio output will be interrupted'])
  assert.deepEqual(ack.blockers, [])
  assert.throws(() => parseChannelsPrepareAck({ ok: true, engine_token: '' }), /engine_token/)
  assert.throws(() => parseChannelsPrepareAck({ ok: false, engine_token: 'x' }), /ok must be true/)
  assert.throws(() => parseChannelsPrepareAck({ ok: true, engine_token: 'x', warnings: [1] }), /warnings/)
})

test('validateChannelsPatch enforces ranges, lengths and uniqueness', () => {
  assert.equal(validateChannelsPatch({}, 22), 'empty')
  assert.equal(validateChannelsPatch({ output: { channels: 0 } }, 22), 'output_range')
  assert.equal(validateChannelsPatch({ output: { channels: 33 } }, 22), 'output_range')
  assert.equal(validateChannelsPatch({ output: { channels: 20, dac_map: [0, 1] } }, 22), 'dac_map_length')
  assert.equal(validateChannelsPatch({ output: { channels: 2, dac_map: [0, 0] } }, 22), 'dac_map_value')
  assert.equal(validateChannelsPatch({ input: { channels: 9 } }, 22), 'input_range')
  assert.equal(validateChannelsPatch({ input: { channels: 2, map: [0] } }, 22), 'input_map_length')
  assert.equal(validateChannelsPatch({ input: { channels: 2, map: [0, 0] } }, 22), 'input_map_value')
  assert.equal(validateChannelsPatch({ output: { channels: 22 } }, 22), null)
  assert.equal(validateChannelsPatch({ input: { channels: 2, map: [0, 1] } }, 22), null)
})

test('sha256Hex matches the standard sha256 of "abc"', async () => {
  const digest = await sha256Hex(new TextEncoder().encode('abc'))
  assert.equal(digest, 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad')
})

test('channels API uses the documented paths, methods and CSRF header', async () => {
  const bodies: Array<{ path: string; body: unknown }> = []
  const { api, calls } = createClient((call) => {
    const path = String(call.url).replace('http://mock.local', '')
    if (path === CHANNELS_PATHS.planUpload) {
      // 原始字节上传：body 不是 JSON，不能按 JSON 解析。
      bodies.push({ path, body: call.body })
      return jsonResponse(200, { sha256: 'a'.repeat(64), upload_id: 'a'.repeat(64), graph_id: 'b'.repeat(64), version: 8, segments: 2, latency_samples: 1023 })
    }
    const body = call.body === undefined || call.body === null ? undefined : JSON.parse(String(call.body))
    bodies.push({ path, body })
    if (path === CHANNELS_PATHS.channels && call.method === 'GET') {
      return jsonResponse(200, clone(WIRED_DOC))
    }
    if (path === CHANNELS_PATHS.channels && call.method === 'PUT') {
      return jsonResponse(200, { ok: true, engine_token: 't', planned_version: 9, changed: true, preview: null, warnings: [], blockers: [] })
    }
    if (path === CHANNELS_PATHS.planPrepare) {
      return jsonResponse(200, { ok: true, engine_token: 'plan-token', planned_version: 8, graph_id: 'b'.repeat(64), admission: { accepted: true } })
    }
    if (path === CHANNELS_PATHS.planHistory) {
      return jsonResponse(200, { versions: [] })
    }
    if (path === CHANNELS_PATHS.planInspect) {
      return jsonResponse(200, clone(buildChannelsSeed('wired').inspect))
    }
    if (path === CHANNELS_PATHS.planRollback) {
      return jsonResponse(200, { ok: true, rolled_back_to: 5, engine_token: 'rb', planned_version: 9, graph_id: 'g5' })
    }
    if (path === CHANNELS_PATHS.commit) {
      return jsonResponse(200, { ok: true, version: 9, applied_block: 1, all_ack: true })
    }
    if (path === CHANNELS_PATHS.abort) {
      return jsonResponse(200, { ok: true, aborted: true })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'nope' } })
  })

  const info = await api.fetchChannels()
  assert.equal(info.output.channels, 22)

  const prepared = await api.prepareChannels({ output: { channels: 20 } })
  assert.equal(prepared.engine_token, 't')
  assert.deepEqual(bodies.at(-1), { path: '/api/channels', body: { output: { channels: 20 } } })

  const upload = await api.uploadPlan(new Uint8Array(128).fill(7), 'demo.bin')
  assert.equal(upload.sha256, 'a'.repeat(64))
  assert.equal(calls.at(-1).method, 'POST')
  assert.equal(calls.at(-1).headers['Content-Type'], 'application/octet-stream')
  assert.equal(calls.at(-1).headers['X-KHAEN-Filename'], 'demo.bin')
  assert.equal(calls.at(-1).body.length, 128)

  await api.preparePlan({ upload_id: 'a'.repeat(64), sha256: 'a'.repeat(64) })
  assert.deepEqual(bodies.at(-1).body, { upload_id: 'a'.repeat(64), sha256: 'a'.repeat(64) })

  await api.rollbackPlan(5)
  assert.deepEqual(bodies.at(-1).body, { target: 5 })
  await api.rollbackPlan()
  assert.deepEqual(bodies.at(-1).body, {})

  await api.commit('token-9')
  assert.deepEqual(bodies.at(-1).body, { engine_token: 'token-9' })
  await api.abort('token-9')
  assert.deepEqual(bodies.at(-1).body, { engine_token: 'token-9' })
  await api.fetchPlanHistory()
  await api.fetchPlanInspect()

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

test('classifyChannelsError maps M4 stable codes and retryability', () => {
  const cases: Array<[ApiError, string, string, boolean]> = [
    [new ApiError({ status: 501, code: 'host_link_unsupported', message: 'no' }), 'host_link_unsupported', 'error.channels.hostLinkUnsupported', true],
    [new ApiError({ status: 501, code: 'not_wired', message: 'no' }), 'not_wired', 'error.channels.notWired', true],
    [new ApiError({ status: 503, code: 'host_link_unavailable', message: 'no' }), 'host_link_unavailable', 'error.channels.hostLinkUnavailable', true],
    [new ApiError({ status: 413, code: 'upload_too_large', message: 'big' }), 'upload_too_large', 'error.channels.uploadTooLarge', true],
    [new ApiError({ status: 404, code: 'upload_not_found', message: 'gone' }), 'upload_not_found', 'error.channels.uploadNotFound', true],
    [new ApiError({ status: 409, code: 'sha_mismatch', message: 'sha' }), 'sha_mismatch', 'error.channels.shaMismatch', true],
    [new ApiError({ status: 400, code: 'invalid_plan', message: 'bad' }), 'invalid_plan', 'error.channels.invalidPlan', true],
    [new ApiError({ status: 401, code: 'not_authenticated', message: 'auth' }), 'unauthorized', 'error.channels.unauthorized', false],
    [new ApiError({ status: 200, code: 'invalid_response', message: 'bad' }), 'invalid_response', 'error.channels.invalidResponse', false],
    [new ApiError({ status: 500, code: 'internal_error', message: 'boom' }), 'server', 'error.channels.server', true],
    [new ApiError({ status: 0, code: 'network_error', message: 'down' }), 'network', 'error.channels.network', true]
  ]
  for (const [error, category, i18nKey, retryable] of cases) {
    const info = classifyChannelsError(error)
    assert.equal(info.category, category)
    assert.equal(info.i18nKey, i18nKey)
    assert.equal(info.retryable, retryable)
  }
  assert.equal(classifyChannelsError(new Error('boom')).category, 'unknown')
})
