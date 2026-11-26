/**
 * 路由页单测 fixture（TEST ONLY；tests/ 不进 dist）。
 *
 * 这里刻意保留**真实 daemon 形态**（crates/daemon/src/channels.rs + plan_api.rs + status.rs）：
 * - /api/channels 含 `plan` 段（source/version/graph_id/routing_known），dac_map 允许 null 项，
 *   routing 未知时 mapping 为 null；
 * - /api/plan/history 用 daemon 的 `{current:{version,graph_id}, entries:[{recorded_at,...}]}` 形态；
 * 同时提供 T54 mock 契约形态（current_version/versions）做兼容断言。
 * 状态文档复用 scripts/status-fixtures.mjs（present/stale/missing），不重复维护。
 */

export const ROUTING_GRAPH_ID = '7c7c7c7c0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c'
export const ROUTING_PLAN_SHA256 = 'a7a7a7a7b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacb'
export const ROUTING_SDP_SHA256 = 'c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf'

/** lane -> dac（0-based）identity。 */
function identityMapping(channels) {
  const mapping = []
  for (let index = 0; index < channels; index += 1) {
    mapping.push({ lane: index, dac: index })
  }
  return mapping
}

/**
 * 真实 daemon 形态的 GET /api/channels 文档。
 *
 * @param {'wired'|'partial'|'degraded'} kind
 */
export function buildRealChannelsDocument(kind = 'wired') {
  if (kind === 'degraded') {
    return {
      schema: 'khaen-channels/v1',
      output: { channels: null, dac_map: null },
      input: { channels: null, map: null, source: null, sdp_sha256: null },
      mapping: null,
      plan: { source: 'file', version: 4, graph_id: null, routing_known: false },
      reasons: {
        'output.channels': 'the rt-host data-plane format does not report channels and no applied-plan routing is known',
        'output.dac_map':
          'the applied plan routing section is not known to the daemon (rt-host reports graph_id but not routing)',
        mapping: 'the applied plan routing section is not known to the daemon',
        'input.channels': 'AES67 receiver channel layout comes from the SDP descriptor, not the rt-host status',
        'input.map': 'AES67 receiver channel map comes from the SDP descriptor',
        'input.source': 'the rt-host control status does not report the runtime mode',
        'input.sdp_sha256': 'AES67 SDP descriptor is not reported'
      },
      not_wired: [
        { path: 'channels.hot_reconfigure', reason: 'hot reconfiguration is not wired in this daemon build' }
      ]
    }
  }
  if (kind === 'partial') {
    // 真实 M4 测试形态：dac_channels=4，只有 lane0->dac0 / lane1->dac2；dac_map 里 null = 该 DAC 静音。
    return {
      schema: 'khaen-channels/v1',
      output: { channels: 4, dac_map: [0, null, 1, null] },
      input: { channels: null, map: null, source: 'manual', sdp_sha256: null },
      mapping: [
        { lane: 0, dac: 0 },
        { lane: 1, dac: 2 }
      ],
      plan: { source: 'rt-host', version: 6, graph_id: ROUTING_GRAPH_ID, routing_known: true },
      reasons: {
        'input.channels': 'AES67 receiver channel layout comes from the SDP descriptor, not the rt-host status',
        'input.map': 'AES67 receiver channel map comes from the SDP descriptor',
        'input.sdp_sha256': 'AES67 SDP descriptor is not reported'
      },
      not_wired: []
    }
  }
  const channels = 22
  return {
    schema: 'khaen-channels/v1',
    output: { channels, dac_map: identityMapping(channels).map((entry) => entry.dac) },
    input: { channels: null, map: null, source: 'sdp', sdp_sha256: ROUTING_SDP_SHA256 },
    mapping: identityMapping(channels),
    plan: { source: 'rt-host', version: 7, graph_id: ROUTING_GRAPH_ID, routing_known: true },
    reasons: {
      'input.channels': 'AES67 receiver channel layout comes from the SDP descriptor, not the rt-host status',
      'input.map': 'AES67 receiver channel map comes from the SDP descriptor'
    },
    not_wired: [
      {
        path: 'channels.hot_reconfigure',
        reason: 'hot reconfiguration (M3b/T52) is not wired; PUT /api/channels commit returns 501'
      }
    ]
  }
}

/** 真实 daemon 形态的 GET /api/plan/history 文档（current 对象 + entries + recorded_at）。 */
export function buildRealHistoryDocument() {
  return {
    schema: 'khaen-plan-history/v1',
    source: 'rt-host',
    current: { version: 7, graph_id: ROUTING_GRAPH_ID },
    entries: [
      {
        version: 7,
        graph_id: ROUTING_GRAPH_ID,
        applied_block: 897,
        recorded_at: '2026-11-26T06:20:00.000Z',
        source: 'commit',
        sha256: ROUTING_PLAN_SHA256,
        latency_samples: 1023,
        observed: true
      },
      {
        version: 6,
        graph_id: null,
        applied_block: null,
        recorded_at: '2026-11-26T05:40:00.000Z',
        source: 'rt-host',
        sha256: null,
        latency_samples: null
      }
    ],
    pagination: { limit: 20, offset: 0, total: 2, has_more: false },
    reasons: {}
  }
}

/** T54 mock 契约形态的 GET /api/plan/history 文档（current_version + versions + applied_at）。 */
export function buildMockHistoryDocument() {
  return {
    schema: 'khaen-plan-history/v1',
    current_version: 7,
    versions: [
      { version: 5, graph_id: '5a5a5a5a', sha256: 'a5a5', applied_at: '2026-11-26T05:00:00.000Z', source: 'upload' },
      { version: 7, graph_id: ROUTING_GRAPH_ID, sha256: ROUTING_PLAN_SHA256, applied_at: '2026-11-26T06:20:00.000Z', source: 'upload' }
    ],
    reasons: {}
  }
}
