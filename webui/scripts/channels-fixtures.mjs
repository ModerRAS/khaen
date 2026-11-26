/**
 * M4 声道/方案契约 fixture（DEV / TEST ONLY；scripts/ 不进 dist）。
 *
 * fixture：
 * - wired：/api/channels 返回完整 khaen-channels/v1（22 输出 + lane→dac + AES67 2ch + sdp sha）+
 *   当前方案/历史/inspect；PUT /api/channels 可 prepare，commit 由 rt-host M3b 决定（mock 返回 501）；
 * - degraded：daemon/rt-host 状态不可用：/api/channels 200 但字段全 null + reasons（页面逐字段「未接入」），
 *   plan 历史/inspect 仍可用（daemon 自己的数据）；
 * - unavailable：daemon 尚未实现 M4（T55）：/api/channels、plan upload/prepare/history/inspect/rollback
 *   全部 501 not_wired（页面整块「未接入」，不编造映射/历史）。
 *
 * 时间戳/sha/graph_id 固定，保证 mock 可复现。
 */

export const CHANNELS_FIXTURES = ['wired', 'degraded', 'unavailable']
export const DEFAULT_CHANNELS_FIXTURE = 'wired'

export function isChannelsFixture(value) {
  return typeof value === 'string' && CHANNELS_FIXTURES.includes(value)
}

const SDP_SHA256 = 'c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf'
const PLAN_SHA256 = 'a7a7a7a7b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacb'
const GRAPH_ID = '7c7c7c7c0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c'
const APPLIED_AT = '2026-11-26T06:20:00.000Z'

/** 输出通道与 lane→dac 映射（22ch；lane i -> dac i，dac 为 0-based 物理输出下标）。 */
function buildMapping(channels) {
  const mapping = []
  for (let index = 0; index < channels; index += 1) {
    mapping.push({ lane: index, dac: index })
  }
  return mapping
}

/** 构造 fixture 的初始数据（调用方可安全修改；每次返回新对象）。 */
export function buildChannelsSeed(fixture) {
  const name = isChannelsFixture(fixture) ? fixture : DEFAULT_CHANNELS_FIXTURE
  const currentPlan = {
    version: 7,
    graph_id: GRAPH_ID,
    sha256: PLAN_SHA256,
    latency_samples: 1023,
    applied_at: APPLIED_AT,
    source: 'upload'
  }
  const history = [
    { version: 5, graph_id: '5a5a5a5a' + '0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c', sha256: 'a5a5a5a5' + 'b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacb', applied_at: '2026-11-26T05:00:00.000Z', source: 'upload' },
    { version: 6, graph_id: '6b6b6b6b' + '0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c', sha256: 'a6a6a6a6' + 'b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacb', applied_at: '2026-11-26T05:40:00.000Z', source: 'calibration' },
    { version: 7, graph_id: GRAPH_ID, sha256: PLAN_SHA256, applied_at: APPLIED_AT, source: 'upload' }
  ]
  if (name === 'unavailable') {
    return {
      fixture: name,
      unavailable: true,
      reason: 'channel layout / plan history API (/api/channels, /api/plan/*) is not implemented in the daemon yet (task T55)',
      channels: null,
      history,
      currentPlan,
      inspect: null
    }
  }
  if (name === 'degraded') {
    return {
      fixture: name,
      unavailable: false,
      reason: null,
      channels: {
        schema: 'khaen-channels/v1',
        output: { channels: null, dac_map: null },
        input: { channels: null, map: null, source: null, sdp_sha256: null },
        // 真实 daemon 在 routing 段未知时上报 mapping: null（不是空数组：空数组会被当成「已知 0 条路由」）。
        mapping: null,
        reasons: {
          'output.channels': 'rt-host control status is unavailable (host_link.mode=none); the daemon cannot read the current output layout',
          'output.dac_map': 'rt-host control status is unavailable',
          mapping: 'rt-host control status is unavailable (host_link.mode=none); the applied plan routing section cannot be associated',
          'input.channels': 'rt-host control status is unavailable',
          'input.map': 'rt-host control status is unavailable',
          'input.source': 'rt-host control status is unavailable',
          'input.sdp_sha256': 'rt-host control status is unavailable'
        },
        not_wired: [
          { path: '/api/channels', reason: 'rt-host control channel is unavailable: channel layout cannot be read (no fake data)' }
        ]
      },
      history,
      currentPlan,
      inspect: {
        ok: true,
        sha256: null,
        graph_id: null,
        plan: { block_size: null, sample_rate: null, in_channels: null, lanes: null, dac_channels: null },
        admission: { accepted: false, rejections: [{ code: 'rt_host_unavailable', detail: 'rt-host status is unavailable, plan inspect cannot run' }], budget: {} },
        compile: { compiled: false, error_code: 'rt_host_unavailable', message: 'rt-host status is unavailable' }
      }
    }
  }
  const channels = 22
  return {
    fixture: name,
    unavailable: false,
    reason: null,
    channels: {
      schema: 'khaen-channels/v1',
      output: { channels, dac_map: buildMapping(channels).map((entry) => entry.dac) },
      input: { channels: 2, map: [0, 1], source: 'sdp', sdp_sha256: SDP_SHA256 },
      mapping: buildMapping(channels),
      reasons: {},
      not_wired: [
        {
          path: '/api/channels/commit',
          reason: 'hot reconfiguration commit is implemented by rt-host M3b (T52) + daemon M4 (T55); until then commit returns 501 host_link_unsupported'
        }
      ]
    },
    history,
    currentPlan,
    inspect: {
      ok: true,
      sha256: PLAN_SHA256,
      graph_id: GRAPH_ID,
      plan: { block_size: 128, sample_rate: 48000, in_channels: 2, lanes: 22, dac_channels: 22 },
      admission: { accepted: true, rejections: [], budget: {} },
      compile: { compiled: true, latency_samples: 1023 }
    }
  }
}
