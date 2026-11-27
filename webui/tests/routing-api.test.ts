import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, isApiError } from '../src/api/client.ts'
import { CONTROL_PATHS, createControlApi } from '../src/api/control.ts'
import { parseStatusResponse } from '../src/api/status.ts'
import {
  ROUTING_PATHS,
  buildPlanSummary,
  buildRoutingChain,
  buildRoutingDraftPatch,
  createRoutingApi,
  currentRoutingSource,
  laneRows,
  nextRoutingLane,
  parseRoutingChannels,
  parseRoutingPlanHistory,
  reasonForRoutingChannelsPath,
  reasonForRoutingHistoryPath,
  rowsToRoutes,
  speakerRoleForDac,
  validateRoutingDraft
} from '../src/api/routing.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'
import { buildStatusFixture } from '../scripts/status-fixtures.mjs'
import {
  ROUTING_GRAPH_ID,
  ROUTING_PLANNED_VERSION,
  ROUTING_PREPARED_TOKEN,
  buildMockHistoryDocument,
  buildRealChannelsDocument,
  buildRealHistoryDocument,
  buildRoutingAbortAck,
  buildRoutingCommitAck,
  buildRoutingPrepareAck,
  withSpeakerRoles
} from './routing-fixtures.ts'

function clone(value) {
  return JSON.parse(JSON.stringify(value))
}

function statusPresent() {
  return parseStatusResponse(buildStatusFixture('present'))
}

function statusMissing() {
  return parseStatusResponse(buildStatusFixture('missing'))
}

function wiredChannels() {
  return parseRoutingChannels(buildRealChannelsDocument('wired'))
}

test('routing: editable two-phase contract constants (read + write paths)', () => {
  assert.deepEqual({ ...ROUTING_PATHS }, { channels: '/api/channels', planHistory: '/api/plan/history' })
  // 写入口复用控制面共享路径（docs/API.md §4）：surface / commit / abort。
  assert.equal(CONTROL_PATHS.surface, '/api/surface')
  assert.equal(CONTROL_PATHS.commit, '/api/commit')
  assert.equal(CONTROL_PATHS.abort, '/api/abort')
})

test('routing: parseRoutingChannels accepts the real daemon shape (plan section, 22 lanes)', () => {
  const info = parseRoutingChannels(clone(buildRealChannelsDocument('wired')))
  assert.equal(info.output.channels, 22)
  assert.equal(info.output.dac_map?.length, 22)
  assert.deepEqual(info.mapping.slice(0, 3), [
    { lane: 0, dac: 0 },
    { lane: 1, dac: 1 },
    { lane: 2, dac: 2 }
  ])
  assert.equal(info.input.channels, null)
  assert.equal(info.input.source, 'sdp')
  assert.deepEqual(info.plan, { source: 'rt-host', version: 7, graph_id: ROUTING_GRAPH_ID, routing_known: true })
  assert.equal(info.not_wired.length, 1)
  assert.match(reasonForRoutingChannelsPath(info, 'input.channels') ?? '', /SDP descriptor/)
})

test('routing: parseRoutingChannels tolerates null dac_map entries (partial routing) and sorts mapping', () => {
  const doc = clone(buildRealChannelsDocument('partial'))
  doc.mapping = [
    { lane: 1, dac: 2 },
    { lane: 0, dac: 0 }
  ]
  const info = parseRoutingChannels(doc)
  assert.equal(info.output.channels, 4)
  assert.deepEqual(info.output.dac_map, [0, null, 1, null])
  assert.deepEqual(info.mapping, [
    { lane: 0, dac: 0 },
    { lane: 1, dac: 2 }
  ])
  const rows = laneRows(info)
  assert.deepEqual(
    rows.map((row) => ({ lane: row.lane, dac: row.dac, physical: row.physical })),
    [
      { lane: 0, dac: 0, physical: 1 },
      { lane: 1, dac: 2, physical: 3 }
    ]
  )
  assert.equal(rows[0].labelKey, 'channels.label.l')
  assert.equal(rows[1].labelKey, 'channels.label.sl')
})

test('routing: parseRoutingChannels accepts the degraded shape (nulls + reasons, mapping null)', () => {
  const info = parseRoutingChannels(clone(buildRealChannelsDocument('degraded')))
  assert.equal(info.output.channels, null)
  assert.equal(info.output.dac_map, null)
  // routing 段未知时 daemon 上报 mapping: null（不是空数组：空数组会被当成「已知 0 条路由」）。
  assert.equal(info.mapping, null)
  assert.equal(info.input.source, null)
  assert.equal(info.plan.routing_known, false)
  assert.match(reasonForRoutingChannelsPath(info, 'output.dac_map') ?? '', /routing section is not known/)
  assert.equal(reasonForRoutingChannelsPath(info, 'missing_field'), null)
})

test('routing: parseRoutingChannels rejects schema/structure drift instead of faking data', () => {
  const wired = buildRealChannelsDocument('wired')
  assert.throws(() => parseRoutingChannels({ ...clone(wired), schema: 'nope' }), /schema must be/)
  assert.throws(() => parseRoutingChannels({ ...clone(wired), mapping: [{ lane: 'x', dac: 0 }] }), /mapping/)
  assert.throws(() => parseRoutingChannels({ ...clone(wired), output: { channels: 1.5, dac_map: [] } }), /output.channels/)
  assert.throws(() => parseRoutingChannels({ ...clone(wired), output: { channels: 22, dac_map: ['x'] } }), /dac_map/)
  assert.throws(() => parseRoutingChannels({ ...clone(wired), reasons: { x: 1 } }), /reasons.x/)
  assert.throws(() => parseRoutingChannels({ ...clone(wired), plan: { version: 1.5 } }), /plan.version/)
  assert.throws(() => parseRoutingChannels(null), /must be an object/)
})

test('routing: parseRoutingPlanHistory accepts the real daemon shape (current object + entries)', () => {
  const history = parseRoutingPlanHistory(clone(buildRealHistoryDocument()))
  assert.equal(history.current_version, 7)
  assert.deepEqual(history.entries.map((entry) => entry.version), [6, 7])
  const current = history.entries[1]
  assert.equal(current.graph_id, ROUTING_GRAPH_ID)
  assert.equal(current.applied_at, '2026-11-26T06:20:00.000Z')
  assert.equal(current.latency_samples, 1023)
  assert.equal(history.entries[0].applied_at, '2026-11-26T05:40:00.000Z')
  assert.equal(history.entries[0].graph_id, null)
  assert.deepEqual(history.reasons, {})
})

test('routing: parseRoutingPlanHistory accepts the T54 mock shape and plain arrays', () => {
  const mock = parseRoutingPlanHistory(clone(buildMockHistoryDocument()))
  assert.equal(mock.current_version, 7)
  assert.deepEqual(mock.entries.map((entry) => entry.version), [5, 7])
  assert.equal(mock.entries[0].applied_at, '2026-11-26T05:00:00.000Z')

  const array = parseRoutingPlanHistory([{ version: 3 }, { version: 2 }])
  assert.equal(array.current_version, 3)
  assert.deepEqual(array.entries.map((entry) => entry.version), [2, 3])
  assert.equal(array.entries[0].applied_at, null)
})

test('routing: parseRoutingPlanHistory rejects malformed history (no fabricated versions)', () => {
  assert.throws(() => parseRoutingPlanHistory({ entries: {} }), /entries must be an array/)
  assert.throws(() => parseRoutingPlanHistory({ entries: [{ graph_id: 'x' }] }), /version must be an integer/)
  assert.throws(() => parseRoutingPlanHistory({ entries: [{ version: 'x' }] }), /version/)
  assert.throws(() => parseRoutingPlanHistory({ versions: [{ version: 1 }], reasons: { x: 2 } }), /reasons.x/)
  assert.throws(() => parseRoutingPlanHistory('nope'), /must be an object or array/)
})

test('routing: history reasons support nested paths and flat fallback', () => {
  const history = parseRoutingPlanHistory({ entries: [], reasons: { 'history.current': 'no applied version', current: 'flat reason' } })
  assert.equal(reasonForRoutingHistoryPath(history, 'history.current'), 'no applied version')
  const flat = parseRoutingPlanHistory({ entries: [], reasons: { current: 'flat reason' } })
  assert.equal(reasonForRoutingHistoryPath(flat, 'history.current'), 'flat reason')
  assert.equal(reasonForRoutingHistoryPath(null, 'history.current'), null)
})

test('routing: laneRows uses dac_map as fallback and never invents an identity mapping', () => {
  const fallback = parseRoutingChannels({
    schema: 'khaen-channels/v1',
    output: { channels: 3, dac_map: [2, null, 0] },
    input: {},
    mapping: null,
    reasons: {},
    not_wired: []
  })
  const rows = laneRows(fallback)
  assert.deepEqual(
    rows.map((row) => ({ lane: row.lane, dac: row.dac, physical: row.physical })),
    [
      { lane: 0, dac: 2, physical: 3 },
      { lane: 2, dac: 0, physical: 1 }
    ]
  )
  assert.equal(laneRows(null).length, 0)
  const noSource = parseRoutingChannels({
    schema: 'khaen-channels/v1',
    output: { channels: null, dac_map: null },
    input: {},
    mapping: null,
    reasons: {},
    not_wired: []
  })
  assert.deepEqual(laneRows(noSource), [])
})

test('routing: buildRoutingChain wires input -> matrix -> DAC without guessing', () => {
  const chain = buildRoutingChain(statusPresent(), wiredChannels())
  assert.equal(chain.input.channels.value, 2)
  assert.equal(chain.input.channels.reason, null)
  assert.deepEqual(chain.input.map.value, [0, 1])
  assert.equal(chain.input.source.value, 'sdp')
  assert.equal(chain.matrix.lanes.value, 22)
  assert.equal(chain.matrix.routedLanes.value, 22)
  assert.equal(chain.matrix.routes.value, 22)
  assert.equal(chain.output.channels.value, 22)
  assert.equal(chain.output.dacMap.value?.length, 22)
  assert.equal(chain.output.knownLabels, 7)
})

test('routing: buildRoutingChain keeps missing sides null with reasons (rt-host down / plan lost)', () => {
  const withoutChannels = buildRoutingChain(statusPresent(), null)
  assert.equal(withoutChannels.input.channels.value, 2)
  assert.equal(withoutChannels.matrix.lanes.value, 22)
  assert.equal(withoutChannels.matrix.routes.value, null)
  assert.equal(withoutChannels.matrix.routedLanes.value, null)
  assert.equal(withoutChannels.output.channels.value, null)
  assert.equal(withoutChannels.output.knownLabels, 0)

  const withoutStatus = buildRoutingChain(null, wiredChannels())
  assert.equal(withoutStatus.input.channels.value, null)
  assert.equal(withoutStatus.matrix.lanes.value, null)
  assert.equal(withoutStatus.matrix.routedLanes.value, 22)
  assert.equal(withoutStatus.output.channels.value, 22)

  const degraded = buildRoutingChain(statusMissing(), parseRoutingChannels(buildRealChannelsDocument('degraded')))
  assert.equal(degraded.matrix.lanes.value, null)
  assert.match(degraded.matrix.routes.reason ?? '', /routing section is not known/)
  assert.equal(degraded.output.channels.value, null)
  assert.match(degraded.output.channels.reason ?? '', /format does not report channels/)
})

test('routing: buildPlanSummary composes status/channels/history and never fakes segments', () => {
  const summary = buildPlanSummary(statusPresent(), wiredChannels(), parseRoutingPlanHistory(buildRealHistoryDocument()))
  assert.equal(summary.version, 3)
  assert.equal(summary.versionSource, 'status')
  assert.equal(summary.latencySamples, 1023)
  assert.equal(summary.latencySource, 'status')
  assert.equal(summary.segments, null)
  assert.equal(summary.segmentsReasonKey, 'routing.plan.segmentsReason')
  assert.equal(summary.graphId, '8bcc0f0b1d2e3f405162738495a6b7c8')
  assert.equal(summary.appliedAt, '2026-11-26T03:45:12.000Z')
  assert.equal(summary.planSource, 'upload')
  assert.equal(summary.routingKnown, true)
  assert.equal(summary.dacChannels, 22)
  assert.equal(summary.routes, 22)
  assert.equal(summary.historyCurrentVersion, 7)
})

test('routing: buildPlanSummary falls back to history/channels and reports reasons', () => {
  const fromHistory = buildPlanSummary(statusMissing(), null, parseRoutingPlanHistory(buildRealHistoryDocument()))
  assert.equal(fromHistory.version, 7)
  assert.equal(fromHistory.versionSource, 'history')
  assert.equal(fromHistory.latencySamples, 1023)
  assert.equal(fromHistory.latencySource, 'history')

  const fromChannels = buildPlanSummary(null, wiredChannels(), null)
  assert.equal(fromChannels.version, 7)
  assert.equal(fromChannels.versionSource, 'channels')
  assert.equal(fromChannels.latencySamples, null)
  assert.equal(fromChannels.routingKnown, true)

  const nothing = buildPlanSummary(statusMissing(), null, null)
  assert.equal(nothing.version, null)
  assert.equal(nothing.versionSource, null)
  assert.equal(nothing.latencySamples, null)
  assert.equal(nothing.routingKnown, null)
  assert.equal(nothing.segments, null)
  // /api/status 的 reasons 里没有 plan.version 时，页面用 i18n 兜底键（不编造原因）。
  assert.equal(nothing.versionReason, null)
})

test('routing: routingKnown / segments reasons prefer backend reasons when present', () => {
  const statusDoc = buildStatusFixture('present')
  statusDoc.reasons['plan.segments'] = 'the daemon does not expose plan sections'
  const summary = buildPlanSummary(parseStatusResponse(statusDoc), null, null)
  assert.equal(summary.segments, null)
  assert.equal(summary.segmentsReason, 'the daemon does not expose plan sections')

  const unknownRouting = parseRoutingChannels({
    schema: 'khaen-channels/v1',
    output: { channels: null, dac_map: null },
    input: {},
    mapping: [],
    plan: { routing_known: false },
    reasons: { plan: 'the applied plan routing section is not known to the daemon' },
    not_wired: []
  })
  const withChannels = buildPlanSummary(null, unknownRouting, null)
  assert.equal(withChannels.routingKnown, false)
  assert.equal(withChannels.routingKnownReason, null)

  const noFlag = parseRoutingChannels({
    schema: 'khaen-channels/v1',
    output: { channels: null, dac_map: null },
    input: {},
    mapping: [],
    reasons: { plan: 'the applied plan routing section is not known to the daemon' },
    not_wired: []
  })
  const unknown = buildPlanSummary(null, noFlag, null)
  assert.equal(unknown.routingKnown, null)
  assert.match(unknown.routingKnownReason ?? '', /routing section is not known/)
})

test('routing: createRoutingApi GETs the two read-only endpoints and surfaces 401 to the shared hook', async () => {
  const calls = []
  const notifications = []
  const client = new ApiClient({
    baseUrl: 'http://routing.test',
    fetchImpl: async (url, init = {}) => {
      calls.push({ url: String(url), method: init.method ?? 'GET' })
      if (String(url).endsWith('/api/plan/history')) {
        return new Response(JSON.stringify(buildRealHistoryDocument()), { status: 200 })
      }
      return new Response(JSON.stringify(buildRealChannelsDocument('wired')), { status: 200 })
    },
    onUnauthorized: () => notifications.push('client-401')
  })
  const api = createRoutingApi(client)
  const channels = await api.fetchChannels()
  assert.equal(channels.mapping.length, 22)
  const history = await api.fetchPlanHistory()
  assert.deepEqual(history.entries.map((entry) => entry.version), [6, 7])
  assert.deepEqual(
    calls.map((call) => call.method + ' ' + call.url),
    ['GET http://routing.test/api/channels', 'GET http://routing.test/api/plan/history']
  )

  const unauthorized = new ApiClient({
    baseUrl: 'http://routing.test',
    fetchImpl: async () => new Response(JSON.stringify({ error: { code: 'not_authenticated', message: 'login' } }), { status: 401 }),
    onUnauthorized: () => notifications.push('client-401')
  })
  try {
    await createRoutingApi(unauthorized).fetchChannels()
    assert.fail('expected the request to reject')
  } catch (error) {
    assert.ok(isApiError(error))
    assert.equal(error.status, 401)
  }
  assert.deepEqual(notifications, ['client-401'])
})

test('routing: currentRoutingSource prefills from /api/channels.mapping with source/version semantics', () => {
  const wired = currentRoutingSource(wiredChannels())
  assert.equal(wired.source, 'mapping')
  assert.equal(wired.known, true)
  assert.equal(wired.reason, null)
  assert.equal(wired.rows?.length, 22)
  assert.deepEqual(wired.rows?.slice(0, 3), [
    { lane: 0, dac: 0 },
    { lane: 1, dac: 1 },
    { lane: 2, dac: 2 }
  ])
  // 来源快照语义：方案版本 / graph_id / routing_known 一并标注（不是实时回读）。
  assert.equal(wired.dacChannels, 22)
  assert.equal(wired.planVersion, 7)
  assert.equal(wired.planGraphId, ROUTING_GRAPH_ID)
  assert.equal(wired.routingKnown, true)

  const partial = currentRoutingSource(parseRoutingChannels(clone(buildRealChannelsDocument('partial'))))
  assert.deepEqual(partial.rows, [
    { lane: 0, dac: 0 },
    { lane: 1, dac: 2 }
  ])
  assert.equal(partial.dacChannels, 4)
})

test('routing: currentRoutingSource prefers status.readback base_plan over channels.mapping', () => {
  const readback = parseStatusResponse(buildStatusFixture('present')).readback
  assert.ok(readback)
  const source = currentRoutingSource(wiredChannels(), readback)
  assert.equal(source.source, 'base_plan')
  assert.equal(source.known, true)
  assert.equal(source.reason, null)
  assert.deepEqual(source.rows, [{ lane: 0, dac: 0 }])
  assert.equal(source.dacChannels, 22)

  // readback 不可用/没有 routing 段时退回 mapping，不丢已有来源。
  const unavailable = { ...readback, available: false, routing: null }
  const fallback = currentRoutingSource(wiredChannels(), unavailable)
  assert.equal(fallback.source, 'mapping')
  assert.equal(fallback.known, true)

  // /api/channels 不可用但 readback 可用：当前值仍可读（来源标 base_plan）。
  const noChannels = currentRoutingSource(null, readback)
  assert.equal(noChannels.source, 'base_plan')
  assert.equal(noChannels.known, true)
  assert.deepEqual(noChannels.rows, [{ lane: 0, dac: 0 }])
})

test('routing: currentRoutingSource reports unknown when mapping is null (never substitutes dac_map/old values)', () => {
  const degraded = currentRoutingSource(parseRoutingChannels(clone(buildRealChannelsDocument('degraded'))))
  assert.equal(degraded.known, false)
  assert.equal(degraded.source, null)
  assert.equal(degraded.rows, null)
  assert.equal(degraded.dacChannels, null)
  assert.match(degraded.reason ?? '', /routing section is not known/)

  // mapping null 但 output.channels 已知：DAC 声道数可展示，但当前映射仍是「未知」。
  const doc = clone(buildRealChannelsDocument('degraded'))
  doc.output = { channels: 4, dac_map: null }
  const partialUnknown = currentRoutingSource(parseRoutingChannels(doc))
  assert.equal(partialUnknown.rows, null)
  assert.equal(partialUnknown.dacChannels, 4)
  assert.match(partialUnknown.reason ?? '', /routing section is not known/)

  // mapping: [] 是「已知 0 条路由」，不是「未知」。
  const knownZero = currentRoutingSource(
    parseRoutingChannels({
      schema: 'khaen-channels/v1',
      output: { channels: 4, dac_map: null },
      input: {},
      mapping: [],
      reasons: {},
      not_wired: []
    })
  )
  assert.deepEqual(knownZero.rows, [])
  assert.equal(knownZero.known, true)
  assert.equal(knownZero.source, 'mapping')
  assert.equal(knownZero.reason, null)

  const nothing = currentRoutingSource(null)
  assert.equal(nothing.rows, null)
  assert.equal(nothing.known, false)
  assert.equal(nothing.dacChannels, null)
  assert.equal(nothing.reason, null)
})

test('routing: speaker_roles [additive §12.2] parses when present, stays null when absent, never fabricated', () => {
  const wired = wiredChannels()
  assert.equal(wired.speaker_roles, null)
  assert.equal(speakerRoleForDac(wired, 0), null)

  const info = parseRoutingChannels(withSpeakerRoles(buildRealChannelsDocument('wired')))
  assert.deepEqual(info.speaker_roles?.slice(0, 2), [
    { dac: 0, role: 'L', enabled: true },
    { dac: 1, role: 'R', enabled: true }
  ])
  assert.deepEqual(speakerRoleForDac(info, 2), { role: 'C', enabled: false })
  assert.equal(speakerRoleForDac(info, 9), null)
  assert.equal(speakerRoleForDac(null, 0), null)

  const doc = withSpeakerRoles(buildRealChannelsDocument('wired'))
  assert.throws(() => parseRoutingChannels({ ...clone(doc), speaker_roles: [{ dac: 'x', role: 'L' }] }), /speaker_roles/)
  assert.throws(() => parseRoutingChannels({ ...clone(doc), speaker_roles: [{ dac: 0, role: '' }] }), /role/)
  assert.throws(() => parseRoutingChannels({ ...clone(doc), speaker_roles: 'nope' }), /speaker_roles/)
})

test('routing: validateRoutingDraft rejects empty/duplicate/invalid/out-of-range drafts locally', () => {
  assert.equal(validateRoutingDraft([{ lane: 0, dac: 0 }, { lane: 1, dac: 2 }]), null)
  assert.equal(validateRoutingDraft([]), 'empty')
  assert.equal(validateRoutingDraft([{ lane: 0, dac: 0 }, { lane: 0, dac: 1 }]), 'duplicate')
  assert.equal(validateRoutingDraft([{ lane: 0, dac: -1 }]), 'entry')
  assert.equal(validateRoutingDraft([{ lane: 1.5, dac: 0 }]), 'entry')
  // 勾选显式 dac_channels 但没填值 -> dac_channels（不是默默沿用方案值）。
  assert.equal(validateRoutingDraft([{ lane: 0, dac: 0 }], { explicitDacChannels: null }), 'dac_channels')
  assert.equal(validateRoutingDraft([{ lane: 0, dac: 0 }], { explicitDacChannels: 0 }), 'dac_channels')
  // 已知 DAC 声道数时校验上界（后端准入仍会再判）。
  assert.equal(validateRoutingDraft([{ lane: 0, dac: 4 }], { knownDacChannels: 4 }), 'dac_range')
  assert.equal(validateRoutingDraft([{ lane: 0, dac: 3 }], { knownDacChannels: 4 }), null)
  assert.equal(validateRoutingDraft([{ lane: 0, dac: 2 }], { explicitDacChannels: 2 }), 'dac_range')
  assert.equal(validateRoutingDraft([{ lane: 0, dac: 2 }], { explicitDacChannels: 3, knownDacChannels: 4 }), null)
})

test('routing: draft rows -> deterministic routes, next lane, and dac_channels patch (inherit vs explicit)', () => {
  const rows = [
    { lane: 2, dac: 5 },
    { lane: 0, dac: 1 }
  ]
  assert.deepEqual(rowsToRoutes(rows), [
    [0, 1],
    [2, 5]
  ])
  assert.equal(nextRoutingLane([]), 0)
  assert.equal(nextRoutingLane([{ lane: 1, dac: 0 }, { lane: 0, dac: 2 }]), 2)
  assert.equal(nextRoutingLane([{ lane: 0, dac: 0 }, { lane: 2, dac: 1 }]), 1)

  const inherit = buildRoutingDraftPatch(rows, null)
  assert.deepEqual(inherit, {
    routes: [
      [0, 1],
      [2, 5]
    ]
  })
  assert.equal('dac_channels' in inherit, false)
  assert.equal('dac_channels' in buildRoutingDraftPatch(rows, undefined), false)

  const explicit = buildRoutingDraftPatch(rows, 22)
  assert.deepEqual(explicit, {
    routes: [
      [0, 1],
      [2, 5]
    ],
    dac_channels: 22
  })
})

test('routing: POST /api/surface routing body is exact; commit/abort carry engine_token', async () => {
  const { fetch: fetchImpl, calls } = createRecordingFetch((call) => {
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
  })
  const client = new ApiClient({
    baseUrl: 'http://routing.test',
    fetchImpl,
    csrfToken: () => 'csrf-routing'
  })
  const api = createControlApi(client)

  const draft = await api.prepareRouting({ routes: [[0, 1], [2, 3]], dac_channels: 22 })
  assert.equal(draft.engine_token, ROUTING_PREPARED_TOKEN)
  assert.equal(draft.surface, 'routing')
  assert.equal(draft.planned_version, ROUTING_PLANNED_VERSION)
  // changed/diff/current/target 是可选回执：未上报就是 null（页面显示「服务端未上报」，不臆造）。
  assert.equal(draft.changed, null)
  assert.equal(draft.diff, null)
  assert.equal(draft.current, null)
  assert.equal(draft.target, null)
  assert.deepEqual(draft.admission, { accepted: true, budget: { matrix_ops: 22 } })

  const commit = await api.commit(draft.engine_token)
  assert.equal(commit.version, ROUTING_PLANNED_VERSION)
  assert.equal(commit.applied_block, 1024)
  assert.equal(commit.changed, true)

  const abort = await api.abort(draft.engine_token)
  assert.equal(abort.aborted, true)
  assert.equal(abort.aborted_surface, 'routing')

  assert.deepEqual(
    calls.map((call) => call.method + ' ' + call.url),
    [
      'POST http://routing.test/api/surface',
      'POST http://routing.test/api/commit',
      'POST http://routing.test/api/abort'
    ]
  )
  assert.deepEqual(JSON.parse(calls[0].body), {
    surface: 'routing',
    data: { routes: [[0, 1], [2, 3]], dac_channels: 22 }
  })
  assert.deepEqual(JSON.parse(calls[1].body), { engine_token: ROUTING_PREPARED_TOKEN })
  assert.deepEqual(JSON.parse(calls[2].body), { engine_token: ROUTING_PREPARED_TOKEN })
  assert.equal(calls[0].headers['X-KHAEN-CSRF'], 'csrf-routing')

  // 未显式给 dac_channels 时请求体不得带该键（缺省沿用方案值）。
  const second = createRecordingFetch(() => jsonResponse(200, buildRoutingPrepareAck()))
  const api2 = createControlApi(new ApiClient({ baseUrl: 'http://routing.test', fetchImpl: second.fetch }))
  await api2.prepareRouting({ routes: [[1, 0]] })
  assert.deepEqual(JSON.parse(second.calls[0].body), {
    surface: 'routing',
    data: { routes: [[1, 0]] }
  })
})
