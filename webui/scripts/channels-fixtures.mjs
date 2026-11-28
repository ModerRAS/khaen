/**
 * M4 声道/方案契约 fixture（DEV / TEST ONLY；scripts/ 不进 dist）。
 *
 * 契约来源：docs/API.md v1.1 §5 + crates/daemon/src/channels.rs（T61 真实实现）+ crates/rt-host/src/control/reconfigure.rs。
 *
 * - GET /api/channels（恒 200 khaen-channels/v1）：
 *   target / runtime_mode / output{channels,dac_map,channels_source} /
 *   input{channels,map,source,sdp_sha256} / mapping / plan{source,version,graph_id,routing_known,routing_invalidated} /
 *   format{channels,sample_rate,block_frames,plan_rate,device,source} /
 *   device{name,state,probe} / current{aes67 group/port/payload/channels/map/jitter/device,…} / generation / busy / pending /
 *   last_gap_ms / last_result / applied / hot_reconfigure{wired,reason} / not_wired / reasons。
 *   mapping=null 表示当前方案 routing 段未知（不是「已知 0 条路由」）。
 * - PUT /api/channels 两阶段（stage=prepare|commit|abort）：
 *   prepare 回执 ok/changed/diff/current/target/engine_token/token/commit_supported/blockers/admission/
 *   graph{action,reason}/warnings（+ daemon 的 schema/stage/target/mapping/preview/reasons）；
 *   commit 回执 ok/applied/changed/generation/gap_ms/format/graph_dropped/rolled_back/idempotent/routing；
 *   abort 回执 ok/aborted/aborted_token/local。mapping 是方案 routing：只有 mapping 改动时
 *   commit_supported=false + blockers，engine_token=null（不签发假 token）。
 * - POST /api/plan/upload：{ok,schema,created,deduplicated,sha256,graph_id,size_bytes,sample_rate,block_size,
 *   segments:[区段名],segment_count,latency_samples,routing,layout,admission,source,recorded_at,stored_path}；
 * - GET /api/plan/history：{schema,source,current:{version,graph_id},entries:[{version,graph_id,applied_block,
 *   recorded_at,source,sha256,latency_samples,observed}],pagination,reasons}；
 * - POST /api/plan/inspect：{plan_path,plan_sha256?}（plan_path 来自上传回执的 stored_path）；
 * - POST /api/prepare：{plan_path,plan_sha256}。
 *
 * fixture：
 * - wired：22 lane -> 22 DAC 全路由、hot_reconfigure.wired=true，prepare/commit/abort 均可走；
 * - partial：**真实部分路由形态**（4 DAC，lane0->dac0、lane1->dac2，dac_map=[0,null,1,null]）：
 *   dac_map 的 null 项必须被页面如实显示为「该 DAC 没有 lane」，而不是 invalid_response；
 * - degraded：rt-host 状态不可用：/api/channels 200 但现场字段全 null + reasons（mapping=null = 当前值未知、
 *   hot_reconfigure.wired=null = 无法核实），页面逐字段「未接入/未上报」；
 * - unavailable：daemon 尚未实现 M4：/api/channels、plan upload/prepare/history/inspect/rollback
 *   全部 501 not_wired（页面整块「未接入」，不编造映射/历史）。
 *
 * 时间戳/sha/graph_id 固定，保证 mock 可复现。
 */

export const CHANNELS_FIXTURES = ['wired', 'partial', 'degraded', 'unavailable']
export const DEFAULT_CHANNELS_FIXTURE = 'wired'

export function isChannelsFixture(value) {
  return typeof value === 'string' && CHANNELS_FIXTURES.includes(value)
}

export const SDP_SHA256 = 'c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf'
export const PLAN_SHA256 = 'a7a7a7a7b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacb'
export const GRAPH_ID = '7c7c7c7c0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c'

/** lane -> DAC 的 mapping 为什么不能由 channels_commit 生效（与 daemon 稳定文案一致）。 */
export const MAPPING_PLAN_OWNED_REASON =
  'the lane -> DAC mapping is the plan routing section (ABI v3 routing), not a field of the rt-host channels op: channels_commit applies the output channel count / sample rate / AES67 input configuration at a block boundary and cannot rewrite the running plan graph; when the engine format changes rt-host drops the graph (graph_dropped=true), so the mapping takes effect again only after a plan whose routing section matches the new format is prepared and committed'

/** AES67 输入参数没有数据源（rt-host 控制通道只报接收参数，不报来源/SDP）时 daemon 的稳定原因文案。 */
const AES67_INPUT_REASON =
  'the rt-host channels status is unavailable: AES67 input parameters (KHAEN_AES67_*) are rt-host process inputs and the contract-B status op does not report them'
const AES67_SOURCE_REASON =
  'the rt-host control channel reports the AES67 receive parameters but not whether they came from an SDP descriptor or a manual configuration; source is not fabricated'
const AES67_SDP_REASON =
  'the rt-host channels status reports the AES67 input configuration but not its SDP descriptor; sdp_sha256 is not fabricated'

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

/** rt-host channels_status 的 current 视图（数据面真实配置）。 */
/** 多流流表（channels_status.input_streams）：主流程 + 可选附加流，stats 可缺省。 */
export function buildInputStreams(extra = []) {
  return [
    {
      primary: true,
      group: '239.69.1.1',
      port: 5004,
      payload_type: 98,
      channels: 2,
      map: [0, 1],
      output_channels: 2,
      jitter_frames: 256,
      stats: { packets: 1200, frames_received: 57600, frames_filled: 57344, underruns: 2, overruns: 0, lost: 0, late: 0, malformed: 0, foreign_payloads: 0, ssrc: 305419896, jitter_fill: 192, asrc_bypasses: 0 }
    },
    ...extra
  ]
}

export function buildChannelsCurrent(overrides = {}) {
  const channels = overrides.channels ?? 22
  return {
    channels,
    // 输入宽度与输出宽度默认同宽；2 输入 → 22 输出的 fixture 显式覆盖。
    content_channels: overrides.content_channels ?? channels,
    sample_rate: 48000,
    block_frames: 128,
    aes67_group: '239.69.1.1',
    aes67_port: 5004,
    aes67_payload_type: 98,
    aes67_channels: 2,
    aes67_map: [0, 1],
    jitter_frames: 256,
    plan_rate: 48000,
    device: 'motu',
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
      reason: 'channel layout / plan history API (/api/channels, /api/plan/*) is not implemented in the daemon yet',
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
        target: 'rt-host',
        runtime_mode: null,
        output: { channels: null, dac_map: null, channels_source: null },
        input: { channels: null, map: null, source: null, sdp_sha256: null },
        // routing 段未知时真实 daemon 上报 mapping: null（不是空数组：空数组会被当成「已知 0 条路由」）。
        mapping: null,
        plan: { source: 'none', version: null, graph_id: null, routing_known: false, routing_invalidated: null },
        format: null,
        device: null,
        current: null,
        generation: null,
        busy: null,
        pending: null,
        last_gap_ms: null,
        last_result: null,
        applied: null,
        hot_reconfigure: { wired: null, reason: 'cannot verify hot-reconfiguration support: rt-host control link is unavailable' },
        reasons: {
          'output.channels':
            'the rt-host channels status, the rt-host data-plane format and the applied-plan routing all fail to report output channels',
          'output.dac_map': 'the applied plan routing section is not known to the daemon',
          mapping: 'the applied plan routing section is not known to the daemon',
          'input.channels': AES67_INPUT_REASON,
          'input.map': AES67_INPUT_REASON,
          'input.source': AES67_SOURCE_REASON,
          'input.sdp_sha256': AES67_SDP_REASON,
          format: 'the rt-host channels status (which reports the data-plane engine format) is unavailable',
          device: 'the rt-host channels status does not report the output device/probe',
          generation: 'neither the rt-host channels status nor the control status reports a reconfiguration generation',
          'channels.hot_reconfigure': 'cannot verify hot-reconfiguration support: rt-host control link is unavailable',
          applied: 'no channels configuration has been committed through this daemon since it started'
        },
        not_wired: [
          {
            path: 'channels.hot_reconfigure',
            reason: 'cannot verify hot-reconfiguration support: rt-host control link is unavailable'
          }
        ]
      },
      inspect: {
        ok: true,
        staged: false,
        sha256: null,
        graph_id: null,
        plan: { block_size: null, sample_rate: null, in_channels: null, lanes: null, dac_channels: null },
        admission: {
          accepted: false,
          rejections: [{ code: 'rt_host_unavailable', detail: 'rt-host status is unavailable, plan inspect cannot run' }],
          budget: {}
        },
        compile: { compiled: false, error_code: 'rt_host_unavailable', message: 'rt-host status is unavailable' }
      }
    }
  }
  if (name === 'partial') {
    // 真实 M4 测试形态（plan_store.rs routing_views_serialize_mapping_and_dac_map）：dac_channels=4，
    // 只有 lane0->dac0 / lane1->dac2；dac_map 里的 null = 该 DAC 静音/没有 lane 喂入（合法）。
    const current = buildChannelsCurrent({
      channels: 4,
      content_channels: 2,
      sample_rate: 48000,
      aes67_channels: 2,
      aes67_map: [0, 1],
      jitter_frames: 256
    })
    return {
      fixture: name,
      unavailable: false,
      reason: null,
      channels: {
        schema: 'khaen-channels/v1',
        target: 'rt-host',
        runtime_mode: 'aes67',
        output: { channels: 4, dac_map: [0, null, 1, null], channels_source: 'rt-host-channels-status' },
        input: { channels: 2, map: [0, 1], source: null, sdp_sha256: null, streams: buildInputStreams() },
        mapping: [
          { lane: 0, dac: 0 },
          { lane: 1, dac: 2 }
        ],
        plan: {
          source: 'rt-host',
          version: 6,
          graph_id: GRAPH_ID,
          routing_known: true,
          routing_invalidated: null
        },
        format: {
          channels: 4,
          sample_rate: 48000,
          block_frames: 128,
          plan_rate: 48000,
          device: 'motu',
          source: 'rt-host-channels-status'
        },
        device: { name: 'motu', state: 'present', probe: { available: true, sample_rate: 48000, channels: 4 } },
        current,
        generation: 2,
        busy: false,
        pending: null,
        last_gap_ms: 31,
        last_result: { ok: true, applied: true, changed: true, generation: 2, gap_ms: 31 },
        applied: null,
        hot_reconfigure: { wired: true, reason: null },
        reasons: {
          'input.source': AES67_SOURCE_REASON,
          'input.sdp_sha256': AES67_SDP_REASON
        },
        not_wired: []
      },
      inspect: buildInspect({
        plan: { block_size: 128, sample_rate: 48000, in_channels: 2, lanes: 4, dac_channels: 4 }
      })
    }
  }
  const channels = 22
  const current = buildChannelsCurrent({ channels, content_channels: 2 })
  return {
    fixture: name,
    unavailable: false,
    reason: null,
    channels: {
      schema: 'khaen-channels/v1',
      target: 'rt-host',
      runtime_mode: 'aes67',
      output: {
        channels,
        dac_map: buildMapping(channels).map((entry) => entry.dac),
        channels_source: 'rt-host-channels-status'
      },
      input: { channels: 2, map: [0, 1], source: null, sdp_sha256: null, streams: buildInputStreams() },
      mapping: buildMapping(channels),
      plan: {
        source: 'rt-host',
        version: 7,
        graph_id: GRAPH_ID,
        routing_known: true,
        routing_invalidated: null
      },
      format: {
        channels,
        sample_rate: 48000,
        block_frames: 128,
        plan_rate: 48000,
        device: 'motu',
        source: 'rt-host-channels-status'
      },
      device: { name: 'motu', state: 'present', probe: { available: true, sample_rate: 48000, channels } },
      current,
      generation: 3,
      busy: false,
      pending: null,
      last_gap_ms: 37,
      last_result: { ok: true, applied: true, changed: true, generation: 3, gap_ms: 37 },
      applied: {
        generation: 3,
        recorded_at: '2026-11-26T06:20:00.000Z',
        format: { channels, sample_rate: 48000, block_frames: 128 }
      },
      hot_reconfigure: { wired: true, reason: null },
      reasons: {
        'input.source': AES67_SOURCE_REASON,
        'input.sdp_sha256': AES67_SDP_REASON
      },
      not_wired: []
    },
    inspect: buildInspect()
  }
}

/* ------------------------------------------------------------------ PUT 两阶段回执 */

/** prepare 的 mapping 段（只有方案 routing 改动时；daemon 稳定形状）。 */
export function buildMappingSection(overrides = {}) {
  return {
    requested: true,
    source: 'mapping',
    current: buildMapping(22),
    target: buildMapping(20),
    changed: true,
    diff: [{ lane: 20, from: 20, to: null }, { lane: 21, from: 21, to: null }],
    applied_by: 'plan routing section (plan prepare + commit)',
    applies_with_this_commit: false,
    reason: MAPPING_PLAN_OWNED_REASON,
    ...overrides
  }
}

/** PUT /api/channels stage=prepare 成功回执（rt-host 两阶段 + daemon 增量字段）。 */
export function buildChannelsPrepareAck(overrides = {}) {
  return {
    ok: true,
    schema: 'khaen-channels/v1',
    stage: 'prepare',
    target: 'rt-host',
    token: 'channels-token-3',
    engine_token: 'channels-token-3',
    planned_generation: 4,
    changed: true,
    diff: {
      channels: { from: 22, to: 20 },
      aes67_map: { from: [0, 1], to: [0, 1] }
    },
    current: buildChannelsCurrent({ channels: 22 }),
    // daemon 在成功分支用 host_link 目标字符串覆盖 rt-host 的 target 视图（channels.rs prepare_channels）。
    target: 'rt-host',
    format: { block_frames: 128, sample_rate: 48000, channels: 20 },
    device: { name: 'motu', state: 'present', probe: { available: true, sample_rate: 48000, channels: 22 } },
    graph: {
      action: 'dropped',
      reason:
        'the engine format (channels/sample rate/block) changes, so the running graph is compiled for the old format; it is dropped at commit and a matching plan must be prepared+committed again'
    },
    admission: {
      accepted: true,
      limits: {
        max_channels: 32,
        max_aes67_channels: 8,
        supported_sample_rates: [44100, 48000, 96000],
        payload_type_range: [96, 127]
      }
    },
    applies_to: 'rt-host data plane (applied at a block boundary by channels_commit; the rt-host process/PID stays up)',
    commit_supported: true,
    mapping: { requested: false, reason: 'no lane -> DAC mapping change was requested; the running plan routing is untouched' },
    preview: {
      old: { channels: 22, input_channels: 2 },
      new: { channels: 20, input_channels: 2 },
      audio_gap_ms_estimate: null,
      audio_gap_ms_estimate_reason: 'the real mute-to-resume gap is measured at commit (gap_ms); prepare does not estimate it'
    },
    warnings: [
      'rt-host will drop the running plan graph at commit (the engine format changes): a plan whose routing matches the new format must be prepared and committed afterwards'
    ],
    blockers: [],
    not_wired: [],
    reasons: {},
    ...overrides
  }
}

/** 只有 mapping/dac_map 改动（方案 routing）时的 prepare 回执：commit_supported=false、不签发 token。 */
export function buildChannelsMappingOnlyAck(overrides = {}) {
  return {
    ok: true,
    schema: 'khaen-channels/v1',
    stage: 'prepare',
    target: 'rt-host',
    engine_token: null,
    token: null,
    planned_generation: null,
    changed: true,
    commit_supported: false,
    mapping: buildMappingSection(),
    diff: null,
    current: buildChannelsCurrent({ channels: 22 }),
    target: buildMapping(20),
    preview: null,
    warnings: [
      'the lane -> DAC mapping change cannot be applied by channels_commit: it is the plan routing section and takes effect when a matching plan is prepared and committed (see mapping.reason)'
    ],
    blockers: [MAPPING_PLAN_OWNED_REASON],
    not_wired: [{ path: 'channels.hot_reconfigure.mapping', reason: MAPPING_PLAN_OWNED_REASON }],
    reasons: {
      'mapping.current': 'the rt-host control status reports no applied graph_id, so the current routing (and the planned diff) is unknown'
    },
    recorded_at: '2026-11-26T06:30:00.000Z',
    ...overrides
  }
}

/** PUT /api/channels stage=commit 成功回执（rt-host + daemon routing/local 段）。 */
export function buildChannelsCommitAck(overrides = {}) {
  return {
    ok: true,
    schema: 'khaen-channels/v1',
    stage: 'commit',
    target: 'rt-host',
    op: 'channels_commit',
    applied: true,
    changed: true,
    idempotent: false,
    generation: 4,
    gap_ms: 37,
    format: { block_frames: 128, sample_rate: 48000, channels: 20 },
    channels: buildChannelsCurrent({ channels: 20 }),
    graph_dropped: true,
    rolled_back: false,
    device_restarted: true,
    pcm_reconfigured: true,
    engine_rebuilt: true,
    note: 'configuration applied and verified by hw readback at a block boundary',
    routing: {
      requested: false,
      applied: false,
      graph_dropped: true,
      plan_recommit_required: true,
      reason:
        'no lane -> DAC routing draft is known to this daemon for this token (prepared by another client or before a daemon restart); channels_commit never changes the plan routing section, so a plan whose routing matches the new format must be committed separately'
    },
    local: { mirror_updated: true, pending_routing_known: false, recorded_at: '2026-11-26T06:31:00.000Z' },
    ...overrides
  }
}

/** PUT /api/channels stage=abort 成功回执。 */
export function buildChannelsAbortAck(overrides = {}) {
  return {
    ok: true,
    schema: 'khaen-channels/v1',
    stage: 'abort',
    target: 'rt-host',
    op: 'channels_abort',
    aborted: true,
    aborted_token: 'channels-token-3',
    note: 'draft discarded; the running data-plane configuration is untouched',
    local: { pending_routing_discarded: true, recorded_at: '2026-11-26T06:32:00.000Z' },
    ...overrides
  }
}

/** 502 ERR_RECONFIGURE_* 的 M1 错误体 details（rt-host 失败回执原样 + phase/target）。 */
export function buildChannelsCommitFailureDetails(overrides = {}) {
  return {
    ok: false,
    op: 'channels_commit',
    error: 'ERR_RECONFIGURE_DEVICE',
    stage: 'reopen',
    message: 'cannot reopen the output device with the target format',
    rolled_back: true,
    device_started: false,
    receiver_running: false,
    gap_ms: 412,
    generation: 3,
    applied: false,
    phase: 'channels_commit',
    target: 'rt-host',
    ...overrides
  }
}
