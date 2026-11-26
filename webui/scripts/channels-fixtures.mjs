/**
 * M4 声道/方案契约 fixture（DEV / TEST ONLY；scripts/ 不进 dist）。
 *
 * T60 契约对齐：形状按 **crates/daemon/src 的真实实现**（channels.rs / plan_store.rs / plan_api.rs /
 * lib.rs 的 POST /api/plan/inspect）构造：
 * - GET /api/channels：{output:{channels,dac_map}, input:{...}, mapping, plan:{source,version,graph_id,routing_known},
 *   reasons, not_wired}；dac_map 允许 null 项（该 DAC 没有 lane 喂入），routing 段未知时 mapping=null；
 * - POST /api/plan/upload 回执：{ok,schema,created,deduplicated,sha256,graph_id,size_bytes,sample_rate,block_size,
 *   segments:[区段名],segment_count,latency_samples,routing,layout,admission,source,recorded_at,stored_path}；
 * - GET /api/plan/history：{schema,source,current:{version,graph_id},entries:[{version,graph_id,applied_block,
 *   recorded_at,source,sha256,latency_samples,observed}],pagination,reasons}（条目来自 control fixture 的
 *   control.history，由 mock-auth-server 渲染）；
 * - POST /api/plan/inspect：{plan_path,plan_sha256?}（plan_path 来自上传回执的 stored_path）；
 * - POST /api/prepare：{plan_path,plan_sha256}。
 *
 * fixture：
 * - wired：22 lane -> 22 DAC 全路由（identity），plan 段 routing_known=true；上传/历史/inspect/prepare/rollback 可用，
 *   channels commit（PUT stage=commit / 共享 commit）在 M3b/T52 完成前返回 501 host_link_unsupported；
 * - partial：**真实部分路由形态**（4 DAC，lane0->dac0、lane1->dac2，dac_map=[0,null,1,null]）：
 *   dac_map 的 null 项必须被页面如实显示为「静音/无 lane」，而不是 invalid_response；
 * - degraded：rt-host 状态不可用：/api/channels 200 但字段全 null + reasons（plan 段 routing_known=false，
 *   mapping=null），页面逐字段「未接入」；inspect 如实报 rt_host_unavailable；
 * - unavailable：daemon 尚未实现 M4（T55）：/api/channels、plan upload/prepare/history/inspect/rollback
 *   全部 501 not_wired（页面整块「未接入」，不编造映射/历史）。
 *
 * 时间戳/sha/graph_id 固定，保证 mock 可复现。
 */

export const CHANNELS_FIXTURES = ['wired', 'partial', 'degraded', 'unavailable']
export const DEFAULT_CHANNELS_FIXTURE = 'wired'

export function isChannelsFixture(value) {
  return typeof value === 'string' && CHANNELS_FIXTURES.includes(value)
}

const SDP_SHA256 = 'c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf'
const PLAN_SHA256 = 'a7a7a7a7b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacb'
const GRAPH_ID = '7c7c7c7c0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c'

/** AES67 输入参数没有数据源（rt-host 进程输入）：真实 daemon 的稳定原因文案。 */
const AES67_INPUT_REASON =
  'AES67 input parameters (channels/map/sdp) are rt-host process inputs (KHAEN_AES67_*); the rt-host control status does not report them'

/** 输出通道与 lane→dac 映射（22ch；lane i -> dac i，dac 为 0-based 物理输出下标）。 */
function buildMapping(channels) {
  const mapping = []
  for (let index = 0; index < channels; index += 1) {
    mapping.push({ lane: index, dac: index })
  }
  return mapping
}

/** 真实控制面 inspect 响应（inspect_json：plan/admission/compile 嵌套，只读试运行）。 */
function buildInspect(overrides = {}) {
  return {
    ok: true,
    staged: false,
    sha256: PLAN_SHA256,
    graph_id: GRAPH_ID,
    plan: { block_size: 128, sample_rate: 48000, in_channels: 2, lanes: 22, dac_channels: 22 },
    admission: { accepted: true, rejections: [], budget: {} },
    compile: { compiled: true, latency_samples: 1023 },
    ...overrides
  }
}

/** 构造 fixture 的初始数据（调用方可直接修改；每次返回新对象）。 */
export function buildChannelsSeed(fixture) {
  const name = isChannelsFixture(fixture) ? fixture : DEFAULT_CHANNELS_FIXTURE
  if (name === 'unavailable') {
    return {
      fixture: name,
      unavailable: true,
      reason: 'channel layout / plan history API (/api/channels, /api/plan/*) is not implemented in the daemon yet (task T55)',
      channels: null,
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
        plan: { source: 'none', version: null, graph_id: null, routing_known: false },
        reasons: {
          'output.channels': 'the rt-host data-plane format does not report channels and no applied-plan routing is known',
          'output.dac_map': 'the applied plan routing section is not known to the daemon',
          mapping: 'the applied plan routing section is not known to the daemon',
          'input.channels': AES67_INPUT_REASON,
          'input.map': AES67_INPUT_REASON,
          'input.source': 'the rt-host control status does not report the runtime mode',
          'input.sdp_sha256': AES67_INPUT_REASON
        },
        not_wired: [
          { path: 'channels.hot_reconfigure', reason: 'hot reconfiguration is not wired in this daemon build (M3b / T52)' }
        ]
      },
      inspect: {
        ok: true,
        staged: false,
        sha256: null,
        graph_id: null,
        plan: { block_size: null, sample_rate: null, in_channels: null, lanes: null, dac_channels: null },
        admission: { accepted: false, rejections: [{ code: 'rt_host_unavailable', detail: 'rt-host status is unavailable, plan inspect cannot run' }], budget: {} },
        compile: { compiled: false, error_code: 'rt_host_unavailable', message: 'rt-host status is unavailable' }
      }
    }
  }
  if (name === 'partial') {
    // 真实 M4 测试形态（plan_store.rs routing_views_serialize_mapping_and_dac_map）：dac_channels=4，
    // 只有 lane0->dac0 / lane1->dac2；dac_map 里的 null = 该 DAC 静音/没有 lane 喂入（合法）。
    return {
      fixture: name,
      unavailable: false,
      reason: null,
      channels: {
        schema: 'khaen-channels/v1',
        output: { channels: 4, dac_map: [0, null, 1, null] },
        input: { channels: null, map: null, source: 'manual', sdp_sha256: null },
        mapping: [
          { lane: 0, dac: 0 },
          { lane: 1, dac: 2 }
        ],
        plan: { source: 'rt-host', version: 6, graph_id: GRAPH_ID, routing_known: true },
        reasons: {
          'input.channels': AES67_INPUT_REASON,
          'input.map': AES67_INPUT_REASON,
          'input.sdp_sha256': AES67_INPUT_REASON
        },
        not_wired: []
      },
      inspect: buildInspect()
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
      plan: { source: 'rt-host', version: 7, graph_id: GRAPH_ID, routing_known: true },
      reasons: {},
      not_wired: [
        {
          path: 'channels.hot_reconfigure',
          reason: 'hot reconfiguration (M3b/T52) is not wired; PUT /api/channels commit returns 501 host_link_unsupported'
        }
      ]
    },
    inspect: buildInspect()
  }
}
