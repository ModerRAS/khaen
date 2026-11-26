import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, ApiError } from '../src/api/client.ts'
import {
  CONTROL_PATHS,
  SUPPORTED_BLOCK_SIZES,
  SUPPORTED_SAMPLE_RATES_HZ,
  VOLUME_GAIN_MAX_DB,
  VOLUME_GAIN_MIN_DB,
  VOLUME_GAIN_STEP_DB,
  buildGainPatch,
  buildLayoutPatch,
  buildRoutingPatch,
  classifyControlError,
  createControlApi,
  createLaneDrafts,
  normalizeGainDb,
  parseControlAbortAck,
  parseControlCommitAck,
  parseControlDraft,
  parseControlMuteAck,
  parseDeviceRetryAck,
  validateGainPatch,
  validateLayoutPatch,
  validateRoutingPatch
} from '../src/api/control.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

const VOLUME_PREPARE = {
  ok: true,
  surface: 'volume',
  engine_token: '0000000000000007abcabcabcabcabca',
  planned_version: 8,
  graph_id: '8bcc0f0b1d2e3f405162738495a6b7c8',
  admission: { accepted: true, budget: {} }
}

const MODE_PREPARE = {
  ok: true,
  surface: 'mode',
  engine_token: 'mode-token',
  planned_version: 9,
  mode: 'off',
  changed: true,
  applies_to: 'control-plane engine (output enable)',
  note: 'real capture-source switching belongs to rt-host/device supervisor'
}

const FORMAT_PREPARE = {
  ok: true,
  engine_token: 'format-token',
  planned_version: 10,
  format: { sample_rate: 96000, block_size: 64, channels: 22 },
  changed: true,
  applies_to: 'control-plane engine',
  device_restart_required: true,
  note: 'format change rebuilds the control-plane engine'
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
  return { client, calls: recording.calls, api: createControlApi(client) }
}

test('parseControlDraft normalizes the volume prepare response', () => {
  const draft = parseControlDraft(VOLUME_PREPARE)
  assert.equal(draft.ok, true)
  assert.equal(draft.surface, 'volume')
  assert.equal(draft.engine_token, VOLUME_PREPARE.engine_token)
  assert.equal(draft.planned_version, 8)
  assert.equal(draft.graph_id, VOLUME_PREPARE.graph_id)
  // volume 补丁不返回 changed（后端语义），不能编一个。
  assert.equal(draft.changed, null)
  assert.equal(draft.device_restart_required, null)
  assert.equal(draft.format, null)
  assert.equal(draft.mode, null)
})

test('parseControlDraft normalizes mode and format prepare responses', () => {
  const mode = parseControlDraft(MODE_PREPARE)
  assert.equal(mode.surface, 'mode')
  assert.equal(mode.mode, 'off')
  assert.equal(mode.changed, true)
  assert.equal(mode.note, MODE_PREPARE.note)

  const format = parseControlDraft(FORMAT_PREPARE)
  assert.equal(format.surface, 'format')
  assert.deepEqual(format.format, { sample_rate: 96000, block_size: 64, channels: 22 })
  assert.equal(format.device_restart_required, true)
  assert.equal(format.changed, true)
})

test('parseControlDraft infers rollback and plan drafts', () => {
  const rollback = parseControlDraft({
    ok: true,
    rolled_back_to: 5,
    engine_token: 'rollback-token',
    planned_version: 11,
    graph_id: VOLUME_PREPARE.graph_id,
    admission: { accepted: true }
  })
  assert.equal(rollback.surface, 'rollback')
  assert.equal(rollback.rolled_back_to, 5)

  const plan = parseControlDraft({
    ok: true,
    engine_token: 'plan-token',
    planned_version: 12,
    graph_id: VOLUME_PREPARE.graph_id
  })
  assert.equal(plan.surface, 'plan')
})

test('parseControlDraft rejects malformed responses instead of faking success', () => {
  assert.throws(() => parseControlDraft({ ...VOLUME_PREPARE, ok: false }), /ok must be true/)
  assert.throws(() => parseControlDraft({ ...VOLUME_PREPARE, engine_token: '' }), /engine_token/)
  assert.throws(() => parseControlDraft({ ...VOLUME_PREPARE, planned_version: 'eight' }), /planned_version/)
  assert.throws(
    () => parseControlDraft({ ...FORMAT_PREPARE, format: { sample_rate: 'fast' } }),
    /sample_rate/
  )
  assert.throws(() => parseControlDraft(null), /body must be an object/)
})

test('parseControlCommitAck keeps the union of plan/mode/format receipts', () => {
  const plan = parseControlCommitAck({
    ok: true,
    applied_block: 1025,
    version: 8,
    all_ack: true,
    graph_id: 'graph-8'
  })
  assert.equal(plan.version, 8)
  assert.equal(plan.applied_block, 1025)
  assert.equal(plan.all_ack, true)
  assert.equal(plan.graph_id, 'graph-8')
  assert.equal(plan.changed, null)

  const mode = parseControlCommitAck({ ok: true, changed: false, applied_block: null, version: 9, mode: 'aes67' })
  assert.equal(mode.mode, 'aes67')
  assert.equal(mode.changed, false)
  assert.equal(mode.applied_block, null)

  const format = parseControlCommitAck({
    ok: true,
    changed: true,
    applied_block: 1,
    version: 10,
    format: { sample_rate: 96000, block_size: 64, channels: 22 },
    device_restart_required: true,
    graph_dropped: true,
    history_cleared: false,
    mute_preserved: false
  })
  assert.deepEqual(format.format, { sample_rate: 96000, block_size: 64, channels: 22 })
  assert.equal(format.graph_dropped, true)
  assert.equal(format.history_cleared, false)
  assert.equal(format.mute_preserved, false)
})

test('parseControlMuteAck / parseControlAbortAck / parseDeviceRetryAck validate honestly', () => {
  const mute = parseControlMuteAck({ ok: true, mute: true, applied_block: 9, idempotent: false })
  assert.equal(mute.mute, true)
  assert.equal(mute.applied_block, 9)
  assert.throws(() => parseControlMuteAck({ ok: true, mute: 'yes' }), /mute must be a boolean/)

  const abort = parseControlAbortAck({ ok: true, aborted: true, aborted_surface: 'mode' })
  assert.equal(abort.aborted, true)
  assert.equal(abort.aborted_surface, 'mode')
  assert.throws(() => parseControlAbortAck({ ok: true }), /aborted/)

  const retry = parseDeviceRetryAck({
    wired: true,
    state: 'streaming',
    device_rate: 48000,
    rate_asserted: true,
    last_error: { code: 'E_NONE', message: 'none' }
  })
  assert.equal(retry.wired, true)
  assert.equal(retry.device_rate, 48000)
  assert.equal(retry.rate_asserted, true)
  assert.equal(retry.last_error_code, 'E_NONE')
})

test('control API posts the exact two-phase request shapes with CSRF', async () => {
  const bodies: Array<Record<string, unknown>> = []
  const { api, calls } = createClient((call) => {
    bodies.push(JSON.parse(String(call.body)))
    const path = String(call.url).replace('http://mock.local', '')
    if (path === CONTROL_PATHS.mute) {
      return jsonResponse(200, { ok: true, mute: true })
    }
    if (path === CONTROL_PATHS.surface) {
      return jsonResponse(200, VOLUME_PREPARE)
    }
    if (path === CONTROL_PATHS.format) {
      return jsonResponse(200, FORMAT_PREPARE)
    }
    if (path === CONTROL_PATHS.commit) {
      return jsonResponse(200, { ok: true, version: 8, applied_block: 1, all_ack: true, graph_id: 'g' })
    }
    if (path === CONTROL_PATHS.abort) {
      return jsonResponse(200, { ok: true, aborted: true })
    }
    if (path === CONTROL_PATHS.deviceRetry) {
      return jsonResponse(200, { wired: true, state: 'streaming', rate_asserted: true })
    }
    return jsonResponse(404, { error: { code: 'not_found', message: 'nope' } })
  })

  const mute = await api.setMute(true)
  assert.equal(mute.mute, true)
  assert.deepEqual(bodies.at(-1), { mute: true })

  const draft = await api.prepareVolume([{ lane: 3, gain_db: -6 }])
  assert.equal(draft.planned_version, 8)
  assert.deepEqual(bodies.at(-1), { surface: 'volume', data: { gains_db: [{ lane: 3, gain_db: -6 }] } })

  await api.prepareMode('off')
  assert.deepEqual(bodies.at(-1), { surface: 'mode', data: { mode: 'off' } })

  await api.prepareFormat({ sample_rate: 96000, chunksize: 64 })
  assert.deepEqual(bodies.at(-1), { sample_rate: 96000, chunksize: 64 })

  await api.commit('token-8')
  assert.deepEqual(bodies.at(-1), { engine_token: 'token-8' })
  await api.abort('token-8')
  assert.deepEqual(bodies.at(-1), { engine_token: 'token-8' })
  await api.retryDevice()

  for (const call of calls) {
    assert.equal(call.method, 'POST')
    assert.equal(call.credentials, 'include')
    assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-test')
    assert.equal(call.headers['Content-Type'], 'application/json')
  }
})

test('control API refuses a non-ok body with invalid_response', async () => {
  const { api } = createClient(() => jsonResponse(200, { ok: false, error: 'nope' }))
  const error = await captureError(() => api.setMute(true))
  assert.equal(error.code, 'invalid_response')
})

test('gain model clamps to 0.5 dB steps inside -150..+50 dB', () => {
  assert.equal(VOLUME_GAIN_MIN_DB, -150)
  assert.equal(VOLUME_GAIN_MAX_DB, 50)
  assert.equal(VOLUME_GAIN_STEP_DB, 0.5)
  assert.equal(normalizeGainDb(-12.34), -12.5)
  assert.equal(normalizeGainDb(49.9), 50)
  assert.equal(normalizeGainDb(-500), -150)
  assert.equal(normalizeGainDb(Number.NaN), 0)
  assert.equal(normalizeGainDb(Number.POSITIVE_INFINITY), 0)
  assert.deepEqual(
    createLaneDrafts(3).map((draft) => ({ lane: draft.lane, gain_db: draft.gain_db, included: draft.included })),
    [
      { lane: 0, gain_db: 0, included: false },
      { lane: 1, gain_db: 0, included: false },
      { lane: 2, gain_db: 0, included: false }
    ]
  )
})

test('gain patch includes only selected lanes, sorted, and validation catches bad input', () => {
  const drafts = [
    { lane: 2, gain_db: -3, included: true },
    { lane: 0, gain_db: 6.2, included: true },
    { lane: 1, gain_db: 0, included: false }
  ]
  assert.deepEqual(buildGainPatch(drafts), [
    { lane: 0, gain_db: 6 },
    { lane: 2, gain_db: -3 }
  ])
  assert.equal(validateGainPatch([], 22), 'empty')
  assert.equal(validateGainPatch([{ lane: 22, gain_db: 0 }], 22), 'lane')
  assert.equal(validateGainPatch([{ lane: 0, gain_db: 0 }, { lane: 0, gain_db: 1 }], 22), 'lane')
  assert.equal(validateGainPatch([{ lane: 0, gain_db: Number.NaN }], 22), 'gain')
  assert.equal(validateGainPatch([{ lane: 0, gain_db: 60 }], 22), 'gain')
  assert.equal(validateGainPatch([{ lane: 0, gain_db: 0 }], 22), null)
})

test('classifyControlError maps stable codes to categories and i18n keys', () => {
  const cases: Array<[ApiError, string, string, boolean]> = [
    [new ApiError({ status: 503, code: 'host_link_unavailable', message: 'no link' }), 'host_link_unavailable', 'error.control.hostLinkUnavailable', true],
    [new ApiError({ status: 501, code: 'unsupported', message: 'no' }), 'unsupported', 'error.control.unsupported', true],
    [new ApiError({ status: 409, code: 'engine_token_invalid', message: 'bad' }), 'token_invalid', 'error.control.tokenInvalid', true],
    [new ApiError({ status: 409, code: 'no_base_plan', message: 'none' }), 'no_base_plan', 'error.control.noBasePlan', true],
    [new ApiError({ status: 400, code: 'bad_request', message: 'bad' }), 'invalid_request', 'error.control.invalidRequest', true],
    [new ApiError({ status: 401, code: 'not_authenticated', message: 'auth' }), 'unauthorized', 'error.control.unauthorized', false],
    [new ApiError({ status: 500, code: 'internal_error', message: 'boom' }), 'server', 'error.control.server', true],
    [new ApiError({ status: 0, code: 'network_error', message: 'down' }), 'network', 'error.control.network', true],
    [new ApiError({ status: 0, code: 'request_timeout', message: 'slow' }), 'timeout', 'error.control.timeout', true],
    [new ApiError({ status: 200, code: 'invalid_response', message: 'bad json' }), 'invalid_response', 'error.control.invalidResponse', false]
  ]
  for (const [error, category, i18nKey, retryable] of cases) {
    const info = classifyControlError(error)
    assert.equal(info.category, category)
    assert.equal(info.i18nKey, i18nKey)
    assert.equal(info.retryable, retryable)
    assert.equal(info.code, error.code)
  }
  const unknown = classifyControlError(new Error('boom'))
  assert.equal(unknown.category, 'unknown')
  assert.equal(unknown.i18nKey, 'error.control.unknown')
})

test('supported capability sets mirror the backend', () => {
  assert.deepEqual([...SUPPORTED_SAMPLE_RATES_HZ], [48000, 96000])
  assert.deepEqual([...SUPPORTED_BLOCK_SIZES], [64, 128])
})
test('parseControlDraft normalizes routing/layout surface drafts without inventing changed/diff', () => {
  const routing = parseControlDraft({
    ok: true,
    surface: 'routing',
    engine_token: 'routing-token',
    planned_version: 12,
    graph_id: 'routing-graph',
    admission: { accepted: true, budget: { entries: 3 } }
  })
  assert.equal(routing.surface, 'routing')
  assert.equal(routing.changed, null, 'routing prepare does not report changed (optional field)')
  assert.equal(routing.diff, null)
  assert.equal(routing.current, null)
  assert.deepEqual(routing.admission, { accepted: true, budget: { entries: 3 } })

  const layout = parseControlDraft({
    ok: true,
    surface: 'layout',
    engine_token: 'layout-token',
    planned_version: 13,
    changed: true,
    diff: { folds: [[0, 1, 1]] },
    current: { in_channels: 2, out_lanes: 2 },
    target: { in_channels: 2, out_lanes: 2, folds: [[0, 1, 1]] },
    graph: { action: 'replace' },
    blockers: ['mapping is plan-owned'],
    commit_supported: true
  })
  assert.equal(layout.surface, 'layout')
  assert.equal(layout.changed, true)
  assert.deepEqual(layout.diff, { folds: [[0, 1, 1]] })
  assert.deepEqual(layout.current, { in_channels: 2, out_lanes: 2 })
  assert.deepEqual(layout.target, { in_channels: 2, out_lanes: 2, folds: [[0, 1, 1]] })
  assert.deepEqual(layout.graph, { action: 'replace' })
  assert.deepEqual(layout.blockers, ['mapping is plan-owned'])
  assert.equal(layout.commit_supported, true)
})

test('control API posts the exact routing/layout surface request shapes', async () => {
  const bodies: Array<Record<string, unknown>> = []
  const { api } = createClient((call) => {
    bodies.push(JSON.parse(String(call.body)))
    return jsonResponse(200, {
      ok: true,
      surface: 'routing',
      engine_token: 'token-1',
      planned_version: 1,
      graph_id: 'g'
    })
  })
  await api.prepareRouting({ routes: [[0, 1], [1, 2]], dac_channels: 22 })
  await api.prepareLayout({ in_channels: 2, out_lanes: 4, folds: [[0, 0, 1], [1, 2, -3.5]] })
  assert.deepEqual(bodies[0], {
    surface: 'routing',
    data: { routes: [[0, 1], [1, 2]], dac_channels: 22 }
  })
  assert.deepEqual(bodies[1], {
    surface: 'layout',
    data: { in_channels: 2, out_lanes: 4, folds: [[0, 0, 1], [1, 2, -3.5]] }
  })
})

test('routing/layout validators reject malformed patches and builders keep tuples', () => {
  assert.equal(validateRoutingPatch({ routes: [] }), 'empty')
  assert.equal(validateRoutingPatch({ routes: [[0]] as never }), 'entry')
  assert.equal(validateRoutingPatch({ routes: [[0, 1], [0, 2]] }), 'duplicate')
  assert.equal(validateRoutingPatch({ routes: [[-1, 0]] }), 'entry')
  assert.equal(validateRoutingPatch({ routes: [[0, 1]], dac_channels: 0 }), 'dac_channels')
  assert.equal(validateRoutingPatch({ routes: [[0, 1]], dac_channels: 22 }), null)
  assert.deepEqual(buildRoutingPatch([[0, 1]], 22), { routes: [[0, 1]], dac_channels: 22 })

  assert.equal(validateLayoutPatch({ in_channels: 0, out_lanes: 2, folds: [[0, 0, 1]] }), 'channels')
  assert.equal(validateLayoutPatch({ in_channels: 2, out_lanes: 2, folds: [] }), 'empty')
  assert.equal(validateLayoutPatch({ in_channels: 2, out_lanes: 2, folds: [[0, 0, Number.NaN]] }), 'entry')
  assert.equal(validateLayoutPatch({ in_channels: 2, out_lanes: 2, folds: [[0, 0, 1]] }), null)
  assert.deepEqual(buildLayoutPatch(2, 2, [[0, 0, 1]]), { in_channels: 2, out_lanes: 2, folds: [[0, 0, 1]] })
})
