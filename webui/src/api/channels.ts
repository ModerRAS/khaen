/**
 * M4 REST 客户端：声道热重配两阶段（PUT /api/channels）+ 方案上传/历史/回滚 + 控制面两阶段。
 *
 * 契约来源：docs/API.md v1.1 §5（唯一合同）+ crates/daemon/src/channels.rs（T61 实现）。
 * - GET  /api/channels -> 恒 200 khaen-channels/v1：current/device/format/generation/busy/pending/
 *   last_gap_ms/last_result/input/output/mapping/plan/hot_reconfigure/not_wired/reasons 等；
 *   mapping=null = 当前方案 routing 段未知（不是「已知 0 条路由」），页面必须显示「当前值未知」+ 原因；
 * - PUT  /api/channels -> 数据面热重配两阶段：
 *   prepare 请求可带 channels/sample_rate/aes67_group/aes67_port/aes67_payload_type/aes67_channels/
 *   aes67_map/jitter_frames/mapping/dac_map，响应 ok/changed/diff/current/target/engine_token/token/
 *   commit_supported/blockers/admission/graph/warnings；mapping/dac_map 是**方案 routing**，
 *   只有 mapping 改动时 commit_supported=false + blockers（不签发 token，不允许假成功）；
 *   commit {stage:'commit',engine_token} 回执 applied/changed/generation/gap_ms/format/graph_dropped/
 *   rolled_back?/routing/idempotent?；abort {stage:'abort',engine_token} 丢弃草稿；
 *   502 reconfig_failed/ERR_RECONFIGURE_*（details.rolled_back 原样带出）、503 host_link_unavailable、
 *   501 host_link_unsupported（process 目标/rt-host 未接线）、409 busy/token/admission、400 参数；
 * - POST /api/commit / POST /api/abort -> 控制面共享两阶段（方案/回滚草稿）；
 * - POST /api/plan/upload   -> 原始 plan 字节（≤ 4 MiB）→ daemon 真实回执
 *   {sha256, graph_id, segments:[区段名], segment_count, latency_samples, stored_path, ...}
 * - POST /api/prepare       -> {plan_path, plan_sha256} → 方案草稿（engine_token/planned_version/graph_id）
 * - POST /api/rollback      -> {target?} → 回滚草稿（rolled_back_to）；仍需 commit 生效
 * - GET  /api/plan/history  -> daemon 真实形态 {schema, source, current:{version,graph_id}|null,
 *   entries:[{version, graph_id, applied_block, recorded_at, source, sha256, latency_samples}], pagination, reasons}
 * - POST /api/plan/inspect  -> {plan_path, plan_sha256?} → 只读预览（控制面 inspect 嵌套形态；失败时页面降级为上传摘要，不伪造）
 *
 * 形状对齐：本文件按 crates/daemon/src（plan_api.rs / plan_store.rs / channels.rs / lib.rs 的
 * inspect 路由）的**真实响应形态**解析，同时保留旧 mock 契约（current_version/versions、
 * 数字 segments、applied_at）的宽松兼容；缺失字段是合法形态（null + reasons），不编假值。
 * 真实 daemon 的上传回执没有 upload_id/version，但给出 stored_path（inspect/prepare 的 plan_path 来源）。
 *
 * 纪律：
 * - 所有响应运行时校验（schema/字段类型/结构漂移 -> invalid_response），不把畸形响应当成功；
 * - 上传前在浏览器算 sha256（WebCrypto），上传后与后端返回的 sha256 比对；不一致即拒绝准备（不信任单侧）；
 * - 历史/预览接口缺失（404/501/503）由页面显示「未接入」+ 稳定码 + 任务号，不编造历史；
 * - MOTU 物理输出标签只标注已知位置（L/R=1-2、SL/SR=3-4、TF/TFR=5-6、LFE=8），未知位置显示输出序号，不猜；
 * - 纯函数（解析/校验/标签/错误归类）不依赖 Vue / DOM，Node 单测可直接覆盖。
 */
import { ApiClient, ApiError } from './client.ts'
import {
  parseControlAbortAck,
  parseControlCommitAck,
  parseControlDraft,
  type ControlAbortAck,
  type ControlCommitAck,
  type ControlDraft
} from './control.ts'

/** 声道布局响应 schema（docs/API.md §5）。 */
export const CHANNELS_SCHEMA = 'khaen-channels/v1'

/** 方案上传字节上限（docs/API.md §6：4 MiB）。 */
export const PLAN_UPLOAD_MAX_BYTES = 4 * 1024 * 1024

/** 方案上传最小字节数（ABI v3 头 + section 表远超该值；小于它必然是坏文件）。 */
export const PLAN_MIN_BYTES = 64

/** plan/声道 REST 路径。 */
export const CHANNELS_PATHS = {
  channels: '/api/channels',
  planUpload: '/api/plan/upload',
  planPrepare: '/api/prepare',
  planHistory: '/api/plan/history',
  planInspect: '/api/plan/inspect',
  planRollback: '/api/rollback',
  commit: '/api/commit',
  abort: '/api/abort'
} as const

/** 已知 MOTU 物理输出标签（1-based 通道号 -> i18n 键）；其余位置不猜。 */
export const MOTU_OUTPUT_LABEL_KEYS: Readonly<Record<number, string>> = {
  1: 'channels.label.l',
  2: 'channels.label.r',
  3: 'channels.label.sl',
  4: 'channels.label.sr',
  5: 'channels.label.tf',
  6: 'channels.label.tfr',
  8: 'channels.label.lfe'
}

/** 已知标签的通道号（升序；用于说明「哪些位置已标注」）。 */
export const MOTU_KNOWN_OUTPUT_NUMBERS: readonly number[] = Object.keys(MOTU_OUTPUT_LABEL_KEYS)
  .map((key) => Number(key))
  .sort((a, b) => a - b)

/** 输入源：SDP 描述符 / 手工配置；null = 未上报（rt-host 控制通道不报来源，不猜）。 */
export type InputSource = 'sdp' | 'manual'

/** 数据面当前配置（rt-host channels_status.current；docs/API.md §5 的 current 段）。 */
export interface ChannelsCurrent {
  channels: number | null
  sample_rate: number | null
  block_frames: number | null
  aes67_group: string | null
  aes67_port: number | null
  aes67_payload_type: number | null
  aes67_channels: number | null
  aes67_map: number[] | null
  jitter_frames: number | null
  plan_rate: number | null
  device: string | null
}

/** 设备探测（rt-host channels_status.device.probe）。 */
export interface ChannelsDeviceProbe {
  available: boolean | null
  sample_rate: number | null
  channels: number | null
  reason: string | null
}

/** 输出设备与只读探测（device 段）。 */
export interface ChannelsDevice {
  name: string | null
  state: string | null
  probe: ChannelsDeviceProbe | null
}

/** 数据面格式（format 段；可能是 rt-host channels_status 或契约 B status 的降级来源）。 */
export interface ChannelsFormat {
  channels: number | null
  sample_rate: number | null
  block_frames: number | null
  plan_rate: number | null
  device: string | null
  source: string | null
}

/** 方案 routing 失效标记（plan.routing_invalidated；graph_dropped 后的旧路由不再被信任）。 */
export interface ChannelsRoutingInvalidated {
  generation: number | null
  recorded_at: string | null
  reason: string | null
}

/** GET /api/channels 的 plan 段（当前已生效方案元数据 + routing 是否可信）。 */
export interface ChannelsPlan {
  source: string | null
  version: number | null
  graph_id: string | null
  /** routing 段是否可代表运行中的图；false/null 时 mapping 为 null。 */
  routing_known: boolean | null
  routing_invalidated: ChannelsRoutingInvalidated | null
}

/** 最近一次热重配结果（rt-host 追踪器 last_result；形状由 rt-host 决定，原样保留）。 */
export type ChannelsLastResult = Record<string, unknown> | null

/** daemon 本地镜像的最近一次 channels_commit（快照，不是实时回读；原样保留）。 */
export type ChannelsApplied = Record<string, unknown> | null

/** 热重配能力上报：true / false + reason / null = 无法核实。 */
export interface ChannelsHotReconfigure {
  wired: boolean | null
  reason: string | null
}

/** 输出侧：物理输出通道数 + DAC 映射（dac 为 0-based 物理输出下标）。 */
export interface ChannelsOutput {
  channels: number | null
  /**
   * 长度 = output.channels：第 i 项是喂给 DAC i 的 lane（0-based）。
   * null 项表示该 DAC 没有 lane 喂入（静音/未路由）——真实 daemon 的部分路由是合法形态；
   * 整个数组为 null 表示未知（不补 0）。
   */
  dac_map: Array<number | null> | null
  /** output.channels 的来源（rt-host-channels-status / plan-routing / rt-host-control-status）；不猜。 */
  channels_source: string | null
}

/** 输入侧：AES67 接收通道数/映射/来源/SDP sha256。 */
export interface ChannelsInput {
  channels: number | null
  map: number[] | null
  source: InputSource | null
  sdp_sha256: string | null
}

/** 单个 lane -> dac 路由项。 */
export interface ChannelMappingEntry {
  lane: number
  dac: number
}

/** not_wired 条目（与 /api/status 同形状；兼容后端只给字符串的旧形态）。 */
export interface ChannelsNotWiredEntry {
  path: string
  reason: string
}

/** GET /api/channels 归一化响应（恒 200；拿不到的部分 null + reasons，不编数值）。 */
export interface ChannelsInfo {
  schema: typeof CHANNELS_SCHEMA
  /** host_link 目标（rt-host / daemon-process），process 目标下热重配不可用。 */
  target: string | null
  runtime_mode: string | null
  output: ChannelsOutput
  input: ChannelsInput
  /** lane -> dac；null = 当前方案 routing 段未知（不是「已知 0 条路由」）。 */
  mapping: ChannelMappingEntry[] | null
  plan: ChannelsPlan
  format: ChannelsFormat | null
  device: ChannelsDevice | null
  /** 数据面当前配置（channels_status.current）；rt-host 状态不可用时 null + reasons。 */
  current: ChannelsCurrent | null
  generation: number | null
  busy: boolean | null
  /** 已 prepare 未 commit 的草稿（rt-host channels_status.pending；原样保留）。 */
  pending: Record<string, unknown> | null
  last_gap_ms: number | null
  last_result: ChannelsLastResult
  applied: ChannelsApplied
  hot_reconfigure: ChannelsHotReconfigure
  reasons: Record<string, string>
  not_wired: ChannelsNotWiredEntry[]
}

/** 方案上传回执（daemon 真实形态 + 旧 mock 契约的宽松兼容）。 */
export interface PlanUploadAck {
  /** 旧 mock 契约字段；真实 daemon 不回 upload_id（用 sha256/stored_path）。 */
  upload_id: string | null
  sha256: string
  graph_id: string | null
  /** 真实 daemon 的上传回执不带目标版本（版本在 commit 回执里才产生）——通常 null。 */
  version: number | null
  /** 真实 daemon：方案区段名数组（content/crossover/matrix/protection/routing）。 */
  segments: string[] | null
  /** 区段数：真实 daemon 的 segment_count；旧 mock 的 segments 数字归一化到这里。 */
  segment_count: number | null
  latency_samples: number | null
  /** 真实 daemon：内容寻址落盘路径——POST /api/plan/inspect 与 /api/prepare 的 plan_path 来源。 */
  stored_path: string | null
}

/** 已生效方案历史条目（daemon entries[] + 旧 mock versions[] 的并集）。 */
export interface PlanHistoryEntry {
  version: number
  graph_id: string | null
  sha256: string | null
  /** 应用/记录时间：旧 mock 的 applied_at 优先，回退 daemon 的 recorded_at。 */
  applied_at: string | null
  /** daemon 原始字段 recorded_at（保留原值便于核对来源；旧 mock 形态为 null）。 */
  recorded_at: string | null
  /** daemon commit 回执里的生效块号（拿不到为 null）。 */
  applied_block: number | null
  latency_samples: number | null
  /** daemon 标记该条目来自自己记录过的 commit 回执（observed:true）。 */
  observed: boolean | null
  source: string | null
}

/** GET /api/plan/history 归一化响应。 */
export interface PlanHistory {
  current_version: number | null
  /** daemon 的 current.graph_id（旧 mock 形态没有该字段时为 null，不猜）。 */
  current_graph_id: string | null
  /** daemon 上报的历史数据源（daemon-process / rt-host / file / none）。 */
  source: string | null
  versions: PlanHistoryEntry[]
  /** daemon 上报的缺失原因（history.current / history.available_versions / history.historic_graph_ids）。 */
  reasons: Record<string, string>
}

/** 方案预览（GET /api/plan/inspect 或控制面 inspect 的并集）。 */
export interface PlanInspect {
  sha256: string | null
  graph_id: string | null
  sample_rate: number | null
  block_size: number | null
  in_channels: number | null
  lanes: number | null
  dac_channels: number | null
  latency_samples: number | null
  admission_accepted: boolean | null
  rejections: Array<{ code: string; detail: string | null }>
  compiled: boolean | null
  error_message: string | null
}

/**
 * PUT /api/channels stage=prepare 的补丁（docs/API.md §5；未提供的字段不修改）。
 *
 * mapping/dac_map 是**方案 routing**（lane -> DAC），不是 rt-host 通道字段：
 * channels_commit 不能改它；只有 mapping/dac_map 改动时 prepare 返回 commit_supported=false。
 */
export interface ChannelsPatch {
  /** 目标输出通道数（显式给；dac_map 长度会自动推导，两者不一致 400）。 */
  channels?: number
  sample_rate?: number
  aes67_group?: string
  aes67_port?: number
  aes67_payload_type?: number
  aes67_channels?: number
  /** AES67 输出槽映射（长度必须 = aes67_channels；每个槽 < channels 且不重复）。 */
  aes67_map?: number[]
  jitter_frames?: number
  /** 方案 routing：lane -> DAC（dac 0-based；与 dac_map 二选一，同时给必须一致）。 */
  mapping?: ChannelMappingEntry[]
  /** 方案 routing 的 DAC -> lane 形式（null = 该 DAC 没有 lane 喂入）；长度 = 输出通道数。 */
  dac_map?: Array<number | null>
}

/** prepare 的 mapping 段（daemon 诚实说明「这是方案 routing，本次 commit 不生效」）。 */
export interface ChannelsPrepareMapping {
  requested: boolean | null
  source: string | null
  current: ChannelMappingEntry[] | null
  target: ChannelMappingEntry[] | null
  changed: boolean | null
  diff: unknown
  applied_by: string | null
  applies_with_this_commit: boolean | null
  reason: string | null
}

/** 声道准备预览（old/new 为可读配置；不变的部分为 null；gap 只测不估）。 */
export interface ChannelsPreview {
  old_channels: number | null
  new_channels: number | null
  old_input_channels: number | null
  new_input_channels: number | null
  audio_gap_ms_estimate: number | null
  audio_gap_ms_estimate_reason: string | null
}

/** prepare 的图编译回执（rt-host：action=dropped 表示 commit 会丢弃运行中的旧图）。 */
export interface ChannelsGraph {
  action: string | null
  reason: string | null
}

/** prepare 的准入回执（accepted + limits/budget/rejections；原样保留）。 */
export type ChannelsAdmission = Record<string, unknown> | null

/**
 * PUT /api/channels stage=prepare 回执。
 *
 * commit_supported=false（只有方案 routing 改动）时 engine_token=null 且 blockers 非空：
 * 页面必须禁用 commit、逐条显示 blockers，并引导走「方案上传 → prepare → commit」。
 */
export interface ChannelsPrepareAck {
  ok: true
  stage: string | null
  /**
   * 两种真实形态：rt-host 成功回执是 host 目标字符串（"rt-host"）；纯 mapping 草稿是
   * 方案 routing 数组（daemon 在该分支不覆盖 target）。页面只按 JSON 原文展示，不猜。
   */
  target: unknown
  engine_token: string | null
  planned_generation: number | null
  changed: boolean | null
  commit_supported: boolean
  /** 后端上报的补丁差异；未上报为 null（不臆造）。 */
  diff: unknown
  /** 数据面当前视图（rt-host current）；未上报为 null。 */
  current: unknown
  format: ChannelsFormat | null
  graph: ChannelsGraph | null
  admission: ChannelsAdmission
  mapping: ChannelsPrepareMapping | null
  preview: ChannelsPreview | null
  warnings: string[]
  blockers: string[]
  reasons: Record<string, string>
  not_wired: ChannelsNotWiredEntry[]
}

/** commit 回执的 routing 段：channels_commit 不改方案 routing，如实说明是否还需方案重提交。 */
export interface ChannelsCommitRouting {
  requested: boolean | null
  changed: boolean | null
  applied: boolean | null
  current: ChannelMappingEntry[] | null
  target: ChannelMappingEntry[] | null
  graph_dropped: boolean | null
  plan_recommit_required: boolean | null
  reason: string | null
}

/**
 * PUT /api/channels stage=commit 回执。
 *
 * 数据面热重配成功 ≠ 设备已换格式：applied/changed/generation/gap_ms/format/graph_dropped 均按
 * rt-host 真实回执展示；graph_dropped=true / routing.plan_recommit_required=true 时页面提示
 * 需要准备并提交匹配新格式的方案，routing 不是本次 commit 改的。
 */
export interface ChannelsCommitAck {
  ok: true
  stage: string | null
  target: string | null
  applied: boolean | null
  changed: boolean | null
  idempotent: boolean | null
  generation: number | null
  gap_ms: number | null
  format: ChannelsFormat | null
  /** commit 生效后的数据面配置（rt-host channels 视图；原样保留）。 */
  channels: Record<string, unknown> | null
  graph_dropped: boolean | null
  /** 重配失败后 rt-host 是否已回滚旧配置（成功回执里通常 false/缺省）。 */
  rolled_back: boolean | null
  device_restarted: boolean | null
  pcm_reconfigured: boolean | null
  engine_rebuilt: boolean | null
  receiver_rebound: boolean | null
  device_was_absent: boolean | null
  /** 方案 routing 段状态（本次 commit 不改 routing，见 plan_recommit_required）。 */
  routing: ChannelsCommitRouting | null
  note: string | null
  local: Record<string, unknown> | null
}

/** PUT /api/channels stage=abort 回执。 */
export interface ChannelsAbortAck {
  ok: true
  aborted: boolean | null
  aborted_token: string | null
  note: string | null
  local: Record<string, unknown> | null
}

/**
 * 502 reconfig_failed / ERR_RECONFIGURE_* 的 details（M1 错误体 details，rt-host 失败回执原样带出）。
 *
 * 失败发生在块边界之后（数据面已尝试重配）：rolled_back/device_started/receiver_running/applied/
 * generation 必须原样展示，不得把失败写成成功。
 */
export interface ChannelsCommitFailure {
  code: string | null
  message: string | null
  stage: string | null
  rolled_back: boolean | null
  device_started: boolean | null
  receiver_running: boolean | null
  applied: boolean | null
  gap_ms: number | null
  generation: number | null
  phase: string | null
  target: string | null
  raw: Record<string, unknown>
}

/* ------------------------------------------------------------------ 运行时校验 */

function invalidResponse(detail: string): ApiError {
  return new ApiError({
    status: 200,
    code: 'invalid_response',
    message: 'unexpected channels response: ' + detail
  })
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return null
  }
  return value as Record<string, unknown>
}

function readString(record: Record<string, unknown>, field: string, path: string): string | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'string') {
    throw invalidResponse(path + '.' + field + ' must be a string or null')
  }
  return value
}

function readNumber(record: Record<string, unknown>, field: string, path: string): number | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw invalidResponse(path + '.' + field + ' must be a finite number or null')
  }
  return value
}

function readBoolean(record: Record<string, unknown>, field: string, path: string): boolean | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'boolean') {
    throw invalidResponse(path + '.' + field + ' must be a boolean or null')
  }
  return value
}

function readNumberArray(record: Record<string, unknown>, field: string, path: string): number[] | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (!Array.isArray(value)) {
    throw invalidResponse(path + '.' + field + ' must be an array of numbers or null')
  }
  return value.map((item, index) => {
    if (typeof item !== 'number' || !Number.isFinite(item)) {
      throw invalidResponse(path + '.' + field + '[' + String(index) + '] must be a finite number')
    }
    return item
  })
}

/** number|null 数组：dac_map 的 null 项表示「该 DAC 没有 lane 喂入」，是合法值（不补 0）。 */
function readNullableNumberArray(
  record: Record<string, unknown>,
  field: string,
  path: string
): Array<number | null> | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (!Array.isArray(value)) {
    throw invalidResponse(path + '.' + field + ' must be an array of numbers/nulls or null')
  }
  return value.map((item, index) => {
    if (item === null || item === undefined) {
      return null
    }
    if (typeof item !== 'number' || !Number.isFinite(item)) {
      throw invalidResponse(path + '.' + field + '[' + String(index) + '] must be a finite number or null')
    }
    return item
  })
}

function readInteger(record: Record<string, unknown>, field: string, path: string): number | null {
  const value = readNumber(record, field, path)
  if (value !== null && !Number.isInteger(value)) {
    throw invalidResponse(path + '.' + field + ' must be an integer or null')
  }
  return value
}

function group(value: unknown, path: string): Record<string, unknown> {
  if (value === null || value === undefined) {
    return {}
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse(path + ' must be an object or null')
  }
  return record
}

function readReasons(value: unknown): Record<string, string> {
  const record = group(value, 'reasons')
  const result: Record<string, string> = {}
  for (const [key, reason] of Object.entries(record)) {
    if (typeof reason !== 'string') {
      throw invalidResponse('reasons.' + key + ' must be a string')
    }
    result[key] = reason
  }
  return result
}

function parseNotWired(value: unknown): ChannelsNotWiredEntry[] {
  if (value === null || value === undefined) {
    return []
  }
  if (!Array.isArray(value)) {
    throw invalidResponse('not_wired must be an array or null')
  }
  return value.map((item, index) => {
    const path = 'not_wired[' + String(index) + ']'
    if (typeof item === 'string') {
      return { path: item, reason: '' }
    }
    const record = asRecord(item)
    if (record === null) {
      throw invalidResponse(path + ' must be an object or string')
    }
    const entryPath = readString(record, 'path', path)
    const reason = readString(record, 'reason', path)
    if (entryPath === null) {
      throw invalidResponse(path + '.path must be a string')
    }
    return { path: entryPath, reason: reason ?? '' }
  })
}

function parseMapping(value: unknown): ChannelMappingEntry[] | null {
  if (value === null || value === undefined) {
    // 真实 daemon 在 routing 段未知时上报 null（不是空数组：空数组会被误读成「已知 0 条路由」）。
    return null
  }
  if (!Array.isArray(value)) {
    throw invalidResponse('mapping must be an array or null')
  }
  return value.map((item, index) => {
    const path = 'mapping[' + String(index) + ']'
    const record = asRecord(item)
    if (record === null) {
      throw invalidResponse(path + ' must be an object')
    }
    const lane = readNumber(record, 'lane', path)
    const dac = readNumber(record, 'dac', path)
    if (lane === null || dac === null || !Number.isInteger(lane) || !Number.isInteger(dac)) {
      throw invalidResponse(path + ' must have integer lane and dac')
    }
    return { lane, dac }
  })
}

/** 原样保留的 JSON 对象（pending / last_result / applied / channels 等）：缺省或 null -> null。 */
function readObjectOrNull(
  record: Record<string, unknown>,
  field: string,
  path: string
): Record<string, unknown> | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  const parsed = asRecord(value)
  if (parsed === null) {
    throw invalidResponse(path + '.' + field + ' must be an object or null')
  }
  return parsed
}

/** string[]（warnings / blockers 等）：缺省或 null -> []；非字符串条目 -> invalid_response。 */
function readStringArray(record: Record<string, unknown>, field: string, path: string): string[] {
  const value = record[field]
  if (value === null || value === undefined) {
    return []
  }
  if (!Array.isArray(value)) {
    throw invalidResponse(path + '.' + field + ' must be an array of strings or null')
  }
  return value.map((item, index) => {
    if (typeof item !== 'string') {
      throw invalidResponse(path + '.' + field + '[' + String(index) + '] must be a string')
    }
    return item
  })
}

/** 数据面当前配置（channels_status.current）：逐字段可缺省，不补默认值。 */
function parseChannelsCurrent(value: unknown, path: string): ChannelsCurrent | null {
  const record = asRecord(value)
  if (record === null) {
    if (value === null || value === undefined) {
      return null
    }
    throw invalidResponse(path + ' must be an object or null')
  }
  return {
    channels: readNumber(record, 'channels', path),
    sample_rate: readNumber(record, 'sample_rate', path),
    block_frames: readNumber(record, 'block_frames', path),
    aes67_group: readString(record, 'aes67_group', path),
    aes67_port: readNumber(record, 'aes67_port', path),
    aes67_payload_type: readNumber(record, 'aes67_payload_type', path),
    aes67_channels: readNumber(record, 'aes67_channels', path),
    aes67_map: readNumberArray(record, 'aes67_map', path),
    jitter_frames: readNumber(record, 'jitter_frames', path),
    plan_rate: readNumber(record, 'plan_rate', path),
    device: readString(record, 'device', path)
  }
}

/** 设备只读探测（device 段）。 */
function parseChannelsDevice(value: unknown, path: string): ChannelsDevice | null {
  const record = asRecord(value)
  if (record === null) {
    if (value === null || value === undefined) {
      return null
    }
    throw invalidResponse(path + ' must be an object or null')
  }
  const probeRaw = readObjectOrNull(record, 'probe', path)
  const probe: ChannelsDeviceProbe | null =
    probeRaw === null
      ? null
      : {
          available: readBoolean(probeRaw, 'available', path + '.probe'),
          sample_rate: readNumber(probeRaw, 'sample_rate', path + '.probe'),
          channels: readNumber(probeRaw, 'channels', path + '.probe'),
          reason: readString(probeRaw, 'reason', path + '.probe')
        }
  return {
    name: readString(record, 'name', path),
    state: readString(record, 'state', path),
    probe
  }
}

/** 数据面格式（format 段；rt-host channels_status 或契约 B status 的降级来源）。 */
function parseChannelsFormat(value: unknown, path: string): ChannelsFormat | null {
  const record = asRecord(value)
  if (record === null) {
    if (value === null || value === undefined) {
      return null
    }
    throw invalidResponse(path + ' must be an object or null')
  }
  return {
    channels: readNumber(record, 'channels', path),
    sample_rate: readNumber(record, 'sample_rate', path),
    block_frames: readNumber(record, 'block_frames', path),
    plan_rate: readNumber(record, 'plan_rate', path),
    device: readString(record, 'device', path),
    source: readString(record, 'source', path)
  }
}

/** plan 段：daemon 真实形态；字段缺失一律 null（不猜当前方案）。 */
function parseChannelsPlan(value: unknown): ChannelsPlan {
  const record = group(value, 'plan')
  const invalidatedRaw = readObjectOrNull(record, 'routing_invalidated', 'plan')
  return {
    source: readString(record, 'source', 'plan'),
    version: readInteger(record, 'version', 'plan'),
    graph_id: readString(record, 'graph_id', 'plan'),
    routing_known: readBoolean(record, 'routing_known', 'plan'),
    routing_invalidated:
      invalidatedRaw === null
        ? null
        : {
            generation: readInteger(invalidatedRaw, 'generation', 'plan.routing_invalidated'),
            recorded_at: readString(invalidatedRaw, 'recorded_at', 'plan.routing_invalidated'),
            reason: readString(invalidatedRaw, 'reason', 'plan.routing_invalidated')
          }
  }
}

/** hot_reconfigure 能力上报：wired true/false + reason 或 null（无法核实）。 */
function parseHotReconfigure(value: unknown): ChannelsHotReconfigure {
  const record = group(value, 'hot_reconfigure')
  return {
    wired: readBoolean(record, 'wired', 'hot_reconfigure'),
    reason: readString(record, 'reason', 'hot_reconfigure')
  }
}

/** 校验并归一化 GET /api/channels 响应（恒 200；缺字段 null + reasons，不编数值）。 */
export function parseChannelsInfo(body: unknown): ChannelsInfo {
  const root = asRecord(body)
  if (root === null) {
    throw invalidResponse('body must be an object')
  }
  if (root.schema !== CHANNELS_SCHEMA) {
    throw invalidResponse('schema must be "' + CHANNELS_SCHEMA + '"')
  }
  const output = group(root.output, 'output')
  const input = group(root.input, 'input')
  const source = readString(input, 'source', 'input')
  return {
    schema: CHANNELS_SCHEMA,
    target: readString(root, 'target', 'channels'),
    runtime_mode: readString(root, 'runtime_mode', 'channels'),
    output: {
      channels: readNumber(output, 'channels', 'output'),
      dac_map: readNullableNumberArray(output, 'dac_map', 'output'),
      channels_source: readString(output, 'channels_source', 'output')
    },
    input: {
      channels: readNumber(input, 'channels', 'input'),
      map: readNumberArray(input, 'map', 'input'),
      source: source === 'sdp' || source === 'manual' ? source : null,
      sdp_sha256: readString(input, 'sdp_sha256', 'input')
    },
    mapping: parseMapping(root.mapping),
    plan: parseChannelsPlan(root.plan),
    format: parseChannelsFormat(root.format, 'format'),
    device: parseChannelsDevice(root.device, 'device'),
    current: parseChannelsCurrent(root.current, 'current'),
    generation: readInteger(root, 'generation', 'channels'),
    busy: readBoolean(root, 'busy', 'channels'),
    pending: readObjectOrNull(root, 'pending', 'channels'),
    last_gap_ms: readInteger(root, 'last_gap_ms', 'channels'),
    last_result: readObjectOrNull(root, 'last_result', 'channels'),
    applied: readObjectOrNull(root, 'applied', 'channels'),
    hot_reconfigure: parseHotReconfigure(root.hot_reconfigure),
    reasons: readReasons(root.reasons),
    not_wired: parseNotWired(root.not_wired)
  }
}

/**
 * 校验并归一化 POST /api/plan/upload 响应（sha256 必须是 64 位小写十六进制）。
 *
 * 真实 daemon 的 segments 是区段名数组且另有 segment_count；旧 T54 mock 的 segments 是数字（段数）。
 * 两者都接受：数字归一化到 segment_count，区段名保留在 segments（旧形态没有名字时如实 null，
 * 不拿数字伪造名字列表）。
 */
export function parsePlanUploadAck(body: unknown): PlanUploadAck {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('plan upload body must be an object')
  }
  const sha = readString(record, 'sha256', 'upload')
  if (sha === null || !/^[0-9a-f]{64}$/.test(sha)) {
    throw invalidResponse('sha256 must be a 64-char lowercase hex string')
  }
  const rawSegments = record.segments
  let segments: string[] | null = null
  let legacySegmentCount: number | null = null
  if (rawSegments !== null && rawSegments !== undefined) {
    if (typeof rawSegments === 'number') {
      if (!Number.isFinite(rawSegments)) {
        throw invalidResponse('upload.segments must be a finite number or an array of section names')
      }
      legacySegmentCount = rawSegments
    } else if (Array.isArray(rawSegments)) {
      segments = rawSegments.map((item, index) => {
        if (typeof item !== 'string') {
          throw invalidResponse('upload.segments[' + String(index) + '] must be a string')
        }
        return item
      })
    } else {
      throw invalidResponse('upload.segments must be an array of section names, a number (legacy) or null')
    }
  }
  return {
    upload_id: readString(record, 'upload_id', 'upload'),
    sha256: sha,
    graph_id: readString(record, 'graph_id', 'upload'),
    version: readNumber(record, 'version', 'upload'),
    segments,
    segment_count: readNumber(record, 'segment_count', 'upload') ?? legacySegmentCount,
    latency_samples: readNumber(record, 'latency_samples', 'upload'),
    stored_path: readString(record, 'stored_path', 'upload')
  }
}

/** 单条历史条目：daemon entries[] 与旧 mock versions[] 的字段并集（缺失一律 null，不编值）。 */
function parsePlanHistoryEntry(item: unknown, index: number): PlanHistoryEntry {
  const path = 'versions[' + String(index) + ']'
  const record = asRecord(item)
  if (record === null) {
    throw invalidResponse(path + ' must be an object')
  }
  const version = readNumber(record, 'version', path)
  if (version === null || !Number.isInteger(version)) {
    throw invalidResponse(path + '.version must be an integer')
  }
  const appliedAt = readString(record, 'applied_at', path)
  const recordedAt = readString(record, 'recorded_at', path)
  return {
    version,
    graph_id: readString(record, 'graph_id', path),
    sha256: readString(record, 'sha256', path),
    // daemon 的 commit 记录只有 recorded_at（记录时刻）；旧 mock 契约用 applied_at。
    applied_at: appliedAt ?? recordedAt,
    recorded_at: recordedAt,
    applied_block: readNumber(record, 'applied_block', path),
    latency_samples: readNumber(record, 'latency_samples', path),
    observed: readBoolean(record, 'observed', path),
    source: readString(record, 'source', path)
  }
}

/**
 * 校验并归一化 GET /api/plan/history 响应。
 *
 * 接受三种形态（以 daemon 真实形态为主，旧 mock 契约保留兼容）：
 * - daemon 真实形态：`{schema, source, current:{version,graph_id}, entries:[{version,graph_id,
 *   applied_block,recorded_at,source,sha256,latency_samples,observed}], pagination, reasons}`；
 * - 旧 T54 mock 契约：`{current_version, versions:[{version,graph_id,sha256,applied_at,source}]}`；
 * - 纯数组：`[{version,...}]`（更旧兼容）。
 *
 * current_version 只在**接口没有明确上报**时才从条目推导（旧兼容）；daemon 显式上报
 * current:{version:null} 时如实保持 null + reasons，不用「最大的条目版本」冒充当前版本。
 */
export function parsePlanHistory(body: unknown): PlanHistory {
  let rawVersions: unknown
  let currentVersion: number | null = null
  let currentGraphId: string | null = null
  let source: string | null = null
  let reasons: Record<string, string> = {}
  let explicitCurrent = false
  if (Array.isArray(body)) {
    rawVersions = body
  } else {
    const record = asRecord(body)
    if (record === null) {
      throw invalidResponse('plan history body must be an object or array')
    }
    rawVersions = record.versions ?? record.entries ?? record.history ?? record.items
    source = readString(record, 'source', 'history')
    reasons = readReasons(record.reasons)
    const current = record.current
    const currentObject = asRecord(current)
    if (currentObject !== null) {
      // daemon 真实形态：current:{version, graph_id}（version=null = 控制面不报当前版本，不猜）。
      explicitCurrent = true
      currentVersion = readInteger(currentObject, 'version', 'history.current')
      currentGraphId = readString(currentObject, 'graph_id', 'history.current')
    } else if (Object.prototype.hasOwnProperty.call(record, 'current_version')) {
      explicitCurrent = true
      currentVersion = readInteger(record, 'current_version', 'history')
    } else if (current !== null && current !== undefined) {
      // 旧兼容：current 是数字。
      explicitCurrent = true
      currentVersion = readInteger(record, 'current', 'history')
    } else if (Object.prototype.hasOwnProperty.call(record, 'current')) {
      // 显式 current:null = 未知；不从条目推导。
      explicitCurrent = true
    }
  }
  if (!Array.isArray(rawVersions)) {
    throw invalidResponse('history versions/entries must be an array')
  }
  const versions = rawVersions.map((item, index) => parsePlanHistoryEntry(item, index))
  versions.sort((a, b) => a.version - b.version)
  if (!explicitCurrent && currentVersion === null && versions.length > 0) {
    currentVersion = versions[versions.length - 1].version
  }
  return { current_version: currentVersion, current_graph_id: currentGraphId, source, versions, reasons }
}

/** 校验并归一化 GET /api/plan/inspect（控制面 inspect 的嵌套 plan/compile/compile 形态也接受）。 */
export function parsePlanInspect(body: unknown): PlanInspect {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('plan inspect body must be an object')
  }
  const plan = group(record.plan, 'plan')
  const admission = group(record.admission, 'admission')
  const compile = group(record.compile, 'compile')
  const rejectionsRaw = admission.rejections
  const rejections: Array<{ code: string; detail: string | null }> = []
  if (Array.isArray(rejectionsRaw)) {
    for (const item of rejectionsRaw) {
      const entry = asRecord(item)
      if (entry === null) {
        throw invalidResponse('admission.rejections entries must be objects')
      }
      rejections.push({
        code: readString(entry, 'code', 'admission.rejections') ?? 'unknown',
        detail: readString(entry, 'detail', 'admission.rejections')
      })
    }
  }
  const flatLatency = readNumber(record, 'latency_samples', 'inspect')
  return {
    sha256: readString(record, 'sha256', 'inspect'),
    graph_id: readString(record, 'graph_id', 'inspect'),
    sample_rate: readNumber(plan, 'sample_rate', 'plan') ?? readNumber(record, 'sample_rate', 'inspect'),
    block_size: readNumber(plan, 'block_size', 'plan') ?? readNumber(record, 'block_size', 'inspect'),
    in_channels: readNumber(plan, 'in_channels', 'plan') ?? readNumber(record, 'in_channels', 'inspect'),
    lanes: readNumber(plan, 'lanes', 'plan') ?? readNumber(record, 'lanes', 'inspect'),
    dac_channels: readNumber(plan, 'dac_channels', 'plan') ?? readNumber(record, 'dac_channels', 'inspect'),
    latency_samples: flatLatency ?? readNumber(compile, 'latency_samples', 'compile'),
    admission_accepted: readBoolean(admission, 'accepted', 'admission'),
    rejections,
    compiled: readBoolean(compile, 'compiled', 'compile'),
    error_message: readString(compile, 'message', 'compile') ?? readString(compile, 'error_code', 'compile')
  }
}

/** prepare 的 mapping 段（没有该段或 null -> null，不臆造）。 */
function parsePrepareMapping(value: unknown): ChannelsPrepareMapping | null {
  const record = asRecord(value)
  if (record === null) {
    if (value === null || value === undefined) {
      return null
    }
    throw invalidResponse('prepare.mapping must be an object or null')
  }
  return {
    requested: readBoolean(record, 'requested', 'prepare.mapping'),
    source: readString(record, 'source', 'prepare.mapping'),
    current: parseMapping(record.current),
    target: parseMapping(record.target),
    changed: readBoolean(record, 'changed', 'prepare.mapping'),
    diff: record.diff ?? null,
    applied_by: readString(record, 'applied_by', 'prepare.mapping'),
    applies_with_this_commit: readBoolean(record, 'applies_with_this_commit', 'prepare.mapping'),
    reason: readString(record, 'reason', 'prepare.mapping')
  }
}

/** prepare 的 preview 段（old/new channels + input_channels；gap 只测不估）。 */
function parseChannelsPreview(value: unknown): ChannelsPreview | null {
  const record = asRecord(value)
  if (record === null) {
    if (value === null || value === undefined) {
      return null
    }
    throw invalidResponse('prepare.preview must be an object or null')
  }
  const oldRaw = group(record.old, 'preview.old')
  const newRaw = group(record.new, 'preview.new')
  return {
    old_channels: readNumber(oldRaw, 'channels', 'preview.old'),
    new_channels: readNumber(newRaw, 'channels', 'preview.new'),
    old_input_channels: readNumber(oldRaw, 'input_channels', 'preview.old'),
    new_input_channels: readNumber(newRaw, 'input_channels', 'preview.new'),
    audio_gap_ms_estimate: readNumber(record, 'audio_gap_ms_estimate', 'preview'),
    audio_gap_ms_estimate_reason: readString(record, 'audio_gap_ms_estimate_reason', 'preview')
  }
}

/**
 * 校验并归一化 PUT /api/channels 的 prepare 回执。
 *
 * 不变量（docs/API.md §5 / §1）：
 * - commit_supported 必须显式上报（两阶段合同），缺失即 invalid_response；
 * - commit_supported=true 必须有非空 engine_token/token（否则 commit 无 token 可用 = 假成功）；
 * - commit_supported=false 不得签发 token（mapping 属方案 routing：只能走方案 prepare+commit）。
 */
export function parseChannelsPrepareAck(body: unknown): ChannelsPrepareAck {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('channels prepare body must be an object')
  }
  if (record.ok !== true) {
    throw invalidResponse('prepare.ok must be true')
  }
  const commitSupported = readBoolean(record, 'commit_supported', 'prepare')
  if (commitSupported === null) {
    throw invalidResponse('prepare.commit_supported must be a boolean (two-phase contract)')
  }
  const engineTokenRaw = readString(record, 'engine_token', 'prepare')
  const tokenRaw = readString(record, 'token', 'prepare')
  const engineToken =
    engineTokenRaw !== null && engineTokenRaw.length > 0
      ? engineTokenRaw
      : tokenRaw !== null && tokenRaw.length > 0
        ? tokenRaw
        : null
  if (commitSupported && engineToken === null) {
    throw invalidResponse(
      'prepare.commit_supported=true requires a non-empty engine_token/token (a commit without a token would be a fake success)'
    )
  }
  if (!commitSupported && engineToken !== null) {
    throw invalidResponse('prepare.commit_supported=false must not issue an engine_token/token')
  }
  const graphRaw = readObjectOrNull(record, 'graph', 'prepare')
  const graph: ChannelsGraph | null =
    graphRaw === null
      ? null
      : {
          action: readString(graphRaw, 'action', 'prepare.graph'),
          reason: readString(graphRaw, 'reason', 'prepare.graph')
        }
  return {
    ok: true,
    stage: readString(record, 'stage', 'prepare'),
    target: record.target ?? null,
    engine_token: engineToken,
    planned_generation: readInteger(record, 'planned_generation', 'prepare'),
    changed: readBoolean(record, 'changed', 'prepare'),
    commit_supported: commitSupported,
    diff: record.diff ?? null,
    current: record.current ?? null,
    format: parseChannelsFormat(record.format, 'prepare.format'),
    graph,
    admission: readObjectOrNull(record, 'admission', 'prepare'),
    mapping: parsePrepareMapping(record.mapping),
    preview: parseChannelsPreview(record.preview),
    warnings: readStringArray(record, 'warnings', 'prepare'),
    blockers: readStringArray(record, 'blockers', 'prepare'),
    reasons: readReasons(record.reasons),
    not_wired: parseNotWired(record.not_wired)
  }
}

/** 校验并归一化 PUT /api/channels 的 commit 回执（字段缺失为 null，不猜）。 */
export function parseChannelsCommitAck(body: unknown): ChannelsCommitAck {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('channels commit body must be an object')
  }
  if (record.ok !== true) {
    throw invalidResponse('commit.ok must be true')
  }
  const routingRaw = readObjectOrNull(record, 'routing', 'commit')
  const routing: ChannelsCommitRouting | null =
    routingRaw === null
      ? null
      : {
          requested: readBoolean(routingRaw, 'requested', 'commit.routing'),
          changed: readBoolean(routingRaw, 'changed', 'commit.routing'),
          applied: readBoolean(routingRaw, 'applied', 'commit.routing'),
          current: parseMapping(routingRaw.current),
          target: parseMapping(routingRaw.target),
          graph_dropped: readBoolean(routingRaw, 'graph_dropped', 'commit.routing'),
          plan_recommit_required: readBoolean(routingRaw, 'plan_recommit_required', 'commit.routing'),
          reason: readString(routingRaw, 'reason', 'commit.routing')
        }
  return {
    ok: true,
    stage: readString(record, 'stage', 'commit'),
    target: readString(record, 'target', 'commit'),
    applied: readBoolean(record, 'applied', 'commit'),
    changed: readBoolean(record, 'changed', 'commit'),
    idempotent: readBoolean(record, 'idempotent', 'commit'),
    generation: readInteger(record, 'generation', 'commit'),
    gap_ms: readInteger(record, 'gap_ms', 'commit'),
    format: parseChannelsFormat(record.format, 'commit.format'),
    channels: readObjectOrNull(record, 'channels', 'commit'),
    graph_dropped: readBoolean(record, 'graph_dropped', 'commit'),
    rolled_back: readBoolean(record, 'rolled_back', 'commit'),
    device_restarted: readBoolean(record, 'device_restarted', 'commit'),
    pcm_reconfigured: readBoolean(record, 'pcm_reconfigured', 'commit'),
    engine_rebuilt: readBoolean(record, 'engine_rebuilt', 'commit'),
    receiver_rebound: readBoolean(record, 'receiver_rebound', 'commit'),
    device_was_absent: readBoolean(record, 'device_was_absent', 'commit'),
    routing,
    note: readString(record, 'note', 'commit'),
    local: readObjectOrNull(record, 'local', 'commit')
  }
}

/** 校验并归一化 PUT /api/channels 的 abort 回执。 */
export function parseChannelsAbortAck(body: unknown): ChannelsAbortAck {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('channels abort body must be an object')
  }
  if (record.ok !== true) {
    throw invalidResponse('abort.ok must be true')
  }
  return {
    ok: true,
    aborted: readBoolean(record, 'aborted', 'abort'),
    aborted_token: readString(record, 'aborted_token', 'abort'),
    note: readString(record, 'note', 'abort'),
    local: readObjectOrNull(record, 'local', 'abort')
  }
}

/**
 * 解析 502 reconfig_failed / ERR_RECONFIGURE_* 错误体的 details（M1 错误体的 details 字段）。
 *
 * daemon 把 rt-host 的失败回执原样合并进 details：rolled_back / device_started /
 * receiver_running / applied / gap_ms / generation 都在里面，页面必须原样展示（不假成功）。
 */
export function parseChannelsCommitFailure(details: unknown): ChannelsCommitFailure | null {
  const record = asRecord(details)
  if (record === null) {
    return null
  }
  return {
    code: readString(record, 'error', 'failure'),
    message: readString(record, 'message', 'failure'),
    stage: readString(record, 'stage', 'failure'),
    rolled_back: readBoolean(record, 'rolled_back', 'failure'),
    device_started: readBoolean(record, 'device_started', 'failure'),
    receiver_running: readBoolean(record, 'receiver_running', 'failure'),
    applied: readBoolean(record, 'applied', 'failure'),
    gap_ms: readInteger(record, 'gap_ms', 'failure'),
    generation: readInteger(record, 'generation', 'failure'),
    phase: readString(record, 'phase', 'failure'),
    target: readString(record, 'target', 'failure'),
    raw: record
  }
}

/* ------------------------------------------------------------------ 派生逻辑 */

/** 读取字段缺失原因（完整路径优先，回退末段扁平键）。 */
export function reasonForChannelsPath(info: ChannelsInfo | null | undefined, path: string): string | null {
  if (!info) {
    return null
  }
  const direct = info.reasons[path]
  if (typeof direct === 'string' && direct.length > 0) {
    return direct
  }
  const dot = path.lastIndexOf('.')
  if (dot >= 0) {
    const flat = info.reasons[path.slice(dot + 1)]
    if (typeof flat === 'string' && flat.length > 0) {
      return flat
    }
  }
  return null
}

/** 读取 /api/plan/history 缺失原因（完整路径优先，回退末段扁平键）。 */
export function reasonForPlanHistoryPath(history: PlanHistory | null | undefined, path: string): string | null {
  if (!history) {
    return null
  }
  const direct = history.reasons[path]
  if (typeof direct === 'string' && direct.length > 0) {
    return direct
  }
  const dot = path.lastIndexOf('.')
  if (dot >= 0) {
    const flat = history.reasons[path.slice(dot + 1)]
    if (typeof flat === 'string' && flat.length > 0) {
      return flat
    }
  }
  return null
}

/** 已知位置的 i18n 标签键；未知位置返回 null（页面显示输出序号，不猜）。 */
export function motuOutputLabelKey(channel: number): string | null {
  return MOTU_OUTPUT_LABEL_KEYS[channel] ?? null
}

/** 通道映射 [0,1] -> "0,1"；null 项（该 DAC 没有 lane 喂入）显示 "-"（不编造 0）；null/空返回 null。 */
export function formatChannelMap(map: readonly (number | null)[] | null | undefined): string | null {
  if (!map || map.length === 0) {
    return null
  }
  return map.map((value) => (value === null ? '-' : String(value))).join(',')
}

/** 支持的采样率（khaen_core::rt::SAMPLE_RATES_HZ：48k / 96k）。 */
export const CHANNELS_SAMPLE_RATES_HZ: readonly number[] = [48000, 96000]
/** 输出通道上限（khaen_core::rt::MAX_OUTPUT_CHANNELS = MOTU UltraLite-mk5 的 22 通道）。 */
export const CHANNELS_MAX_OUTPUT = 22
/** AES67 RTP 通道上限（khaen_host::aes67_input::MAX_RTP_CHANNELS）。 */
export const CHANNELS_MAX_AES67 = 8

/** IO I2b：目的地址可以是组播组或单播本端地址；0.0.0.0 与广播没有具体接收端。 */
function isConcreteIpv4Destination(value: string): boolean {
  const parts = value.trim().split(".")
  if (parts.length !== 4) {
    return false
  }
  const octets: number[] = []
  for (const part of parts) {
    if (!/^\d{1,3}$/.test(part)) {
      return false
    }
    const octet = Number(part)
    if (!Number.isInteger(octet) || octet < 0 || octet > 255) {
      return false
    }
    octets.push(octet)
  }
  // 0.0.0.0/广播不是可 bind 的具体接收端；其余单播/组播都交给后端最终校验。
  return !(octets.every((octet) => octet === 0) || octets.every((octet) => octet === 255))
}

/**
 * 校验 PUT /api/channels prepare 补丁（返回前端 error 码；null = 合法）。
 *
 * 前端只做「提交前的可见校验」；后端是权威（docs/API.md §5）。范围取真实常量：
 * 输出 1..=22、采样率 48k/96k、AES67 1..=8、PT 96..=127、AES67 目的地址为具体单播或组播
 * （IO I2b：单播 SDP 的 c= 是本端地址）。
 */
export function validateChannelsPatch(patch: ChannelsPatch): string | null {
  const keys = Object.keys(patch).filter((key) => (patch as Record<string, unknown>)[key] !== undefined)
  if (keys.length === 0) {
    return 'empty'
  }
  const channels = patch.channels
  if (channels !== undefined) {
    if (!Number.isInteger(channels) || channels < 1 || channels > CHANNELS_MAX_OUTPUT) {
      return 'channels_range'
    }
  }
  const sampleRate = patch.sample_rate
  if (sampleRate !== undefined && !CHANNELS_SAMPLE_RATES_HZ.includes(sampleRate)) {
    return 'sample_rate_value'
  }
  if (patch.aes67_group !== undefined && !isConcreteIpv4Destination(patch.aes67_group)) {
    return 'aes67_group_value'
  }
  const port = patch.aes67_port
  if (port !== undefined && (!Number.isInteger(port) || port < 1 || port > 65535)) {
    return 'aes67_port_value'
  }
  const payloadType = patch.aes67_payload_type
  if (payloadType !== undefined && (!Number.isInteger(payloadType) || payloadType < 96 || payloadType > 127)) {
    return 'payload_type_value'
  }
  const aes67Channels = patch.aes67_channels
  if (aes67Channels !== undefined) {
    if (!Number.isInteger(aes67Channels) || aes67Channels < 1 || aes67Channels > CHANNELS_MAX_AES67) {
      return 'aes67_channels_range'
    }
  }
  const aes67Map = patch.aes67_map
  if (aes67Map !== undefined) {
    if (aes67Channels !== undefined && aes67Map.length !== aes67Channels) {
      return 'aes67_map_length'
    }
    const seen = new Set<number>()
    for (const slot of aes67Map) {
      const upper = channels === undefined ? Number.POSITIVE_INFINITY : channels - 1
      if (!Number.isInteger(slot) || slot < 0 || slot > upper || seen.has(slot)) {
        return 'aes67_map_value'
      }
      seen.add(slot)
    }
  }
  const jitter = patch.jitter_frames
  if (jitter !== undefined && (!Number.isInteger(jitter) || jitter < 0)) {
    return 'jitter_frames_value'
  }
  const dacMap = patch.dac_map
  if (dacMap !== undefined) {
    if (channels !== undefined && dacMap.length !== channels) {
      return 'dac_map_length'
    }
    const seen = new Set<number>()
    for (const lane of dacMap) {
      if (lane === null) {
        continue
      }
      if (!Number.isInteger(lane) || lane < 0 || seen.has(lane)) {
        return 'dac_map_value'
      }
      seen.add(lane)
    }
  }
  const mapping = patch.mapping
  if (mapping !== undefined) {
    const lanes = new Set<number>()
    const dacs = new Set<number>()
    for (const entry of mapping) {
      if (!Number.isInteger(entry.lane) || entry.lane < 0 || !Number.isInteger(entry.dac) || entry.dac < 0) {
        return 'mapping_value'
      }
      if (lanes.has(entry.lane) || dacs.has(entry.dac)) {
        return 'mapping_value'
      }
      lanes.add(entry.lane)
      dacs.add(entry.dac)
    }
    if (dacMap !== undefined) {
      const fromMapping = new Set(mapping.map((entry) => (entry.lane + ":" + entry.dac)))
      const fromDacMap = new Set<string>()
      dacMap.forEach((lane, dac) => {
        if (lane !== null) {
          fromDacMap.add(lane + ":" + dac)
        }
      })
      if (fromMapping.size !== fromDacMap.size || [...fromMapping].some((key) => !fromDacMap.has(key))) {
        return 'mapping_dac_map_conflict'
      }
    }
  }
  return null
}

/* -------------------------------------------------------------- sha256（客户端） */

/**
 * 浏览器/Node 通用的 sha256（WebCrypto）。不可用时抛错（不退回假值）。
 * 上传前算一次，上传后与后端返回的 sha256 比对（双侧一致才允许 prepare）。
 */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const subtle = globalThis.crypto?.subtle
  if (subtle === undefined) {
    throw new Error('sha256_unavailable: WebCrypto subtle is not available in this browser context')
  }
  const copy = new Uint8Array(bytes.length)
  copy.set(bytes)
  const digest = await subtle.digest('SHA-256', copy)
  return Array.from(new Uint8Array(digest))
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('')
}

/* ---------------------------------------------------------------------- 端点 */

export interface ChannelsApi {
  fetchChannels(): Promise<ChannelsInfo>
  /** PUT /api/channels {stage:'prepare', ...patch}：只生成草稿，不改变运行态。 */
  prepareChannels(patch: ChannelsPatch): Promise<ChannelsPrepareAck>
  /** PUT /api/channels {stage:'commit', engine_token}：块边界生效（失败不假成功）。 */
  commitChannels(engineToken: string): Promise<ChannelsCommitAck>
  /** PUT /api/channels {stage:'abort', engine_token}：丢弃 rt-host 草稿（不改变运行态）。 */
  abortChannels(engineToken: string): Promise<ChannelsAbortAck>
  /** POST /api/commit {engine_token}：控制面共享提交（方案/回滚草稿）。 */
  commit(engineToken: string): Promise<ControlCommitAck>
  /** POST /api/abort {engine_token}：控制面共享 abort（方案草稿）。 */
  abort(engineToken: string): Promise<ControlAbortAck>
  fetchPlanHistory(): Promise<PlanHistory>
  /** 只读试运行：POST {plan_path, plan_sha256}（plan_path 来自上传回执的 stored_path）。 */
  fetchPlanInspect(planPath: string, planSha256?: string | null): Promise<PlanInspect>
  /** 原始 plan 字节上传（≤ PLAN_UPLOAD_MAX_BYTES；X-KHAEN-Filename 只作提示）。 */
  uploadPlan(bytes: Uint8Array, filename: string): Promise<PlanUploadAck>
  /** 用 daemon 落盘路径 prepare 方案（POST {plan_path, plan_sha256}；两阶段：commit 才生效）。 */
  preparePlan(input: { plan_path: string; sha256: string }): Promise<ControlDraft>
  /** 历史回滚准备（版本号递增；仍需 commit）。 */
  rollbackPlan(target?: number): Promise<ControlDraft>
}

/** 用共享 ApiClient 构造 M4 API（单测注入 fetchImpl 即可脱离网络）。 */
export function createChannelsApi(client: ApiClient): ChannelsApi {
  return {
    async fetchChannels(): Promise<ChannelsInfo> {
      const body = await client.get<unknown>(CHANNELS_PATHS.channels)
      return parseChannelsInfo(body)
    },
    async prepareChannels(patch: ChannelsPatch): Promise<ChannelsPrepareAck> {
      // docs/API.md §5：PUT /api/channels，stage=prepare + 扁平字段（未提供 = 不修改）。
      const body = await client.put<unknown>(CHANNELS_PATHS.channels, { stage: 'prepare', ...patch })
      return parseChannelsPrepareAck(body)
    },
    async commitChannels(engineToken: string): Promise<ChannelsCommitAck> {
      const body = await client.put<unknown>(CHANNELS_PATHS.channels, {
        stage: 'commit',
        engine_token: engineToken
      })
      return parseChannelsCommitAck(body)
    },
    async abortChannels(engineToken: string): Promise<ChannelsAbortAck> {
      const body = await client.put<unknown>(CHANNELS_PATHS.channels, {
        stage: 'abort',
        engine_token: engineToken
      })
      return parseChannelsAbortAck(body)
    },
    async commit(engineToken: string): Promise<ControlCommitAck> {
      const body = await client.post<unknown>(CHANNELS_PATHS.commit, { engine_token: engineToken })
      return parseControlCommitAck(body)
    },
    async abort(engineToken: string): Promise<ControlAbortAck> {
      const body = await client.post<unknown>(CHANNELS_PATHS.abort, { engine_token: engineToken })
      return parseControlAbortAck(body)
    },
    async fetchPlanHistory(): Promise<PlanHistory> {
      const body = await client.get<unknown>(CHANNELS_PATHS.planHistory)
      return parsePlanHistory(body)
    },
    async fetchPlanInspect(planPath: string, planSha256?: string | null): Promise<PlanInspect> {
      // 真实 daemon 是 POST + {plan_path}（只读试运行：读文件 → 解码 → 准入 → 图编译，不建草稿）。
      if (typeof planPath !== 'string' || planPath.trim().length === 0) {
        throw new ApiError({
          status: 400,
          code: 'invalid_request',
          message: 'plan_path is required: upload the plan first and use the stored_path from the upload ack'
        })
      }
      const payload: Record<string, string> = { plan_path: planPath }
      if (typeof planSha256 === 'string' && planSha256.length > 0) {
        payload.plan_sha256 = planSha256
      }
      const body = await client.post<unknown>(CHANNELS_PATHS.planInspect, payload)
      return parsePlanInspect(body)
    },
    async uploadPlan(bytes: Uint8Array, filename: string): Promise<PlanUploadAck> {
      if (bytes.length > PLAN_UPLOAD_MAX_BYTES) {
        throw new ApiError({
          status: 413,
          code: 'upload_too_large',
          message: 'plan exceeds the 4 MiB upload limit'
        })
      }
      if (bytes.length < PLAN_MIN_BYTES) {
        throw new ApiError({
          status: 400,
          code: 'invalid_plan',
          message: 'plan file is too small to be a khaen plan (ABI v3)'
        })
      }
      const copy = new Uint8Array(bytes.length)
      copy.set(bytes)
      const body = await client.postRaw<unknown>(CHANNELS_PATHS.planUpload, copy, {
        contentType: 'application/octet-stream',
        headers: { 'X-KHAEN-Filename': filename }
      })
      return parsePlanUploadAck(body)
    },
    async preparePlan(input: { plan_path: string; sha256: string }): Promise<ControlDraft> {
      // 真实 daemon 的 /api/prepare 只接受 plan_path + plan_sha256（两个字符串；upload_id 是 T54 旧契约）。
      if (typeof input.plan_path !== 'string' || input.plan_path.trim().length === 0) {
        throw new ApiError({
          status: 400,
          code: 'invalid_request',
          message: 'plan_path is required: the daemon did not report stored_path for the uploaded plan'
        })
      }
      if (typeof input.sha256 !== 'string' || input.sha256.length === 0) {
        throw new ApiError({ status: 400, code: 'invalid_request', message: 'plan_sha256 is required' })
      }
      const body = await client.post<unknown>(CHANNELS_PATHS.planPrepare, {
        plan_path: input.plan_path,
        plan_sha256: input.sha256
      })
      return parseControlDraft(body)
    },
    async rollbackPlan(target?: number): Promise<ControlDraft> {
      const payload: Record<string, number> = {}
      if (target !== undefined) {
        payload.target = target
      }
      const body = await client.post<unknown>(CHANNELS_PATHS.planRollback, payload)
      return parseControlDraft(body)
    }
  }
}

/* ---------------------------------------------------------------- 错误归类 */

export type ChannelsErrorCategory =
  | 'network'
  | 'timeout'
  | 'unauthorized'
  | 'host_link_unavailable'
  | 'host_link_unsupported'
  | 'not_wired'
  | 'not_found'
  | 'invalid_request'
  | 'invalid_plan'
  | 'upload_too_large'
  | 'upload_not_found'
  | 'sha_mismatch'
  | 'admission_rejected'
  | 'no_history'
  /** 上传回执没有 stored_path（旧后端/mock）：inspect/prepare 无 plan_path，页面显示 NOT_AVAILABLE。 */
  | 'plan_path_unavailable'
  | 'conflict'
  /** 502 reconfig_failed / ERR_RECONFIGURE_*：数据面热重配失败（details.rolled_back 原样展示）。 */
  | 'reconfig_failed'
  /** 本地保护：prepare 回执 commit_supported=false（mapping 属方案 routing）时禁止发 commit。 */
  | 'commit_unsupported'
  | 'server'
  | 'invalid_response'
  | 'unknown'

export interface ChannelsErrorInfo {
  category: ChannelsErrorCategory
  status: number
  code: string
  i18nKey: string
  detail?: string
  retryable: boolean
  /** 502 热重配失败回执的 details（rolled_back/device_started/applied/…）；其它错误不出现。 */
  failure?: ChannelsCommitFailure
}

/**
 * 错误类别 -> i18n 键。
 *
 * 走本区域 channelsEdit 分片：基础文件 error.channels.* 的旧文案含过期里程碑号（基础文件只读），
 * 新文案由本分片提供；基础键仍存在（其它调用方可用），但本模块不再引用。
 */
const CHANNELS_ERROR_KEYS: Record<ChannelsErrorCategory, string> = {
  network: 'channelsEdit.error.network',
  timeout: 'channelsEdit.error.timeout',
  unauthorized: 'channelsEdit.error.unauthorized',
  host_link_unavailable: 'channelsEdit.error.hostLinkUnavailable',
  host_link_unsupported: 'channelsEdit.error.hostLinkUnsupported',
  not_wired: 'channelsEdit.error.notWired',
  not_found: 'channelsEdit.error.notFound',
  invalid_request: 'channelsEdit.error.invalidRequest',
  invalid_plan: 'channelsEdit.error.invalidPlan',
  upload_too_large: 'channelsEdit.error.uploadTooLarge',
  upload_not_found: 'channelsEdit.error.uploadNotFound',
  sha_mismatch: 'channelsEdit.error.shaMismatch',
  admission_rejected: 'channelsEdit.error.admissionRejected',
  no_history: 'channelsEdit.error.noHistory',
  plan_path_unavailable: 'channelsEdit.error.planPathUnavailable',
  conflict: 'channelsEdit.error.conflict',
  reconfig_failed: 'channelsEdit.error.reconfigFailed',
  commit_unsupported: 'channelsEdit.error.commitUnsupported',
  server: 'channelsEdit.error.server',
  invalid_response: 'channelsEdit.error.invalidResponse',
  unknown: 'channelsEdit.error.unknown'
}

const CHANNELS_CODE_CATEGORIES: Record<string, ChannelsErrorCategory> = {
  host_link_unavailable: 'host_link_unavailable',
  host_link_unsupported: 'host_link_unsupported',
  unsupported: 'host_link_unsupported',
  not_wired: 'not_wired',
  not_implemented: 'not_wired',
  not_found: 'not_found',
  upload_not_found: 'upload_not_found',
  sha_mismatch: 'sha_mismatch',
  plan_sha_mismatch: 'sha_mismatch',
  invalid_plan: 'invalid_plan',
  invalid_abi: 'invalid_plan',
  decode_failed: 'invalid_plan',
  upload_too_large: 'upload_too_large',
  admission_rejected: 'admission_rejected',
  no_history: 'no_history',
  bad_request: 'invalid_request',
  invalid_request: 'invalid_request',
  invalid_response: 'invalid_response',
  switch_busy: 'conflict',
  reconfiguring: 'conflict',
  cal_busy: 'conflict',
  channels_busy: 'conflict',
  busy: 'conflict',
  engine_token_invalid: 'conflict',
  token_invalid: 'conflict',
  no_channels_draft: 'conflict',
  channel_layout_unavailable: 'conflict',
  device_unavailable: 'host_link_unavailable',
  device_absent: 'host_link_unavailable',
  reconfig_failed: 'reconfig_failed',
  request_timeout: 'timeout',
  network_error: 'network'
}

/** 502 / ERR_RECONFIGURE_* 归类（rollback 明细可解析时一并带出，失败回执不丢字段）。 */
function failureDetails(error: ApiError): ChannelsCommitFailure | undefined {
  try {
    const parsed = parseChannelsCommitFailure(error.details)
    return parsed ?? undefined
  } catch {
    // details 不是预期对象形状：不阻断错误归类（更不把畸形 details 当成功）。
    return undefined
  }
}

/** 把 M4 失败归类为 UI 可分支的稳定类别 + 本地化文案键。 */
export function classifyChannelsError(error: unknown): ChannelsErrorInfo {
  if (!(error instanceof ApiError)) {
    const detail = error instanceof Error ? error.message : undefined
    return {
      category: 'unknown',
      status: 0,
      code: 'unknown_error',
      i18nKey: CHANNELS_ERROR_KEYS.unknown,
      ...(detail ? { detail } : {}),
      retryable: true
    }
  }
  let category: ChannelsErrorCategory
  if (error.code === 'request_timeout') {
    category = 'timeout'
  } else if (error.code === 'network_error' || error.status === 0) {
    category = 'network'
  } else if (error.status === 401) {
    category = 'unauthorized'
  } else if (error.code === 'invalid_response') {
    category = 'invalid_response'
  } else if (error.code.startsWith('ERR_RECONFIGURE_')) {
    // 数据面已在块边界尝试重配（可能已回滚）：502 + 稳定码 + details.rolled_back 原样展示。
    category = 'reconfig_failed'
  } else {
    const fallback: ChannelsErrorCategory =
      error.status === 502
        ? 'reconfig_failed'
        : error.status === 501
          ? 'host_link_unsupported'
          : error.status === 503
            ? 'host_link_unavailable'
            : error.status >= 500
              ? 'server'
              : error.status === 404
                ? 'not_found'
                : 'conflict'
    category = CHANNELS_CODE_CATEGORIES[error.code] ?? fallback
  }
  const detail = error.message.length > 0 ? error.message : undefined
  const failure = category === 'reconfig_failed' ? failureDetails(error) : undefined
  return {
    category,
    status: error.status,
    code: error.code,
    i18nKey: CHANNELS_ERROR_KEYS[category],
    ...(detail ? { detail } : {}),
    retryable: category !== 'unauthorized' && category !== 'invalid_response',
    ...(failure !== undefined ? { failure } : {})
  }
}
