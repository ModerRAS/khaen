/**
 * M4 REST 客户端（任务 T54）：声道布局/映射读取 + 方案上传/历史/回滚 + 两阶段提交。
 *
 * 契约来源（T54/T55 任务契约；后端由 T55 并行实现，前端用契约 mock + 单测开发）：
 * - GET  /api/channels      -> khaen-channels/v1（output/input/mapping/reasons/not_wired；字段缺失为 null + reason）
 * - PUT  /api/channels      -> 两阶段 prepare（校验并返回 engine_token/planned diff；M3b 热重配未完成时 commit 501）
 * - POST /api/commit        -> 共享两阶段提交（channels 草稿在 M3b 前返回 501 host_link_unsupported）
 * - POST /api/abort         -> 丢弃草稿
 * - POST /api/plan/upload   -> 原始 plan 字节（≤ 4 MiB）→ daemon 真实回执
 *   {sha256, graph_id, segments:[区段名], segment_count, latency_samples, stored_path, ...}
 * - POST /api/prepare       -> {plan_path, plan_sha256} → 方案草稿（engine_token/planned_version/graph_id）
 * - POST /api/rollback      -> {target?} → 回滚草稿（rolled_back_to）；仍需 commit 生效
 * - GET  /api/plan/history  -> daemon 真实形态 {schema, source, current:{version,graph_id}|null,
 *   entries:[{version, graph_id, applied_block, recorded_at, source, sha256, latency_samples}], pagination, reasons}
 * - POST /api/plan/inspect  -> {plan_path, plan_sha256?} → 只读预览（控制面 inspect 嵌套形态；失败时页面降级为上传摘要，不伪造）
 *
 * T60 契约对齐：本文件按 crates/daemon/src（plan_api.rs / plan_store.rs / channels.rs / lib.rs 的
 * inspect 路由）的**真实响应形态**解析，同时保留旧 T54 mock 契约（current_version/versions、
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

/** 声道布局响应 schema（T55 契约）。 */
export const CHANNELS_SCHEMA = 'khaen-channels/v1'

/** 方案上传字节上限（T55 契约：4 MiB）。 */
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

/** 输入源：SDP 描述符 / 手工配置；null = 未上报。 */
export type InputSource = 'sdp' | 'manual'

/** 输出侧：物理输出通道数 + DAC 映射（dac 为 0-based 物理输出下标）。 */
export interface ChannelsOutput {
  channels: number | null
  /**
   * 长度 = output.channels：第 i 项是喂给 DAC i 的 lane（0-based）。
   * null 项表示该 DAC 没有 lane 喂入（静音/未路由）——真实 daemon 的部分路由是合法形态；
   * 整个数组为 null 表示未知（不补 0）。
   */
  dac_map: Array<number | null> | null
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

/** GET /api/channels 归一化响应。 */
export interface ChannelsInfo {
  schema: typeof CHANNELS_SCHEMA
  output: ChannelsOutput
  input: ChannelsInput
  /** lane -> dac；null = 当前方案 routing 段未知（不是「已知 0 条路由」）。 */
  mapping: ChannelMappingEntry[] | null
  reasons: Record<string, string>
  not_wired: ChannelsNotWiredEntry[]
}

/** 方案上传回执（daemon 真实形态 + 旧 T54 mock 契约的宽松兼容）。 */
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

/** PUT /api/channels 的准备补丁（未提供的字段不修改）。 */
export interface ChannelsPatch {
  output?: { channels?: number; dac_map?: number[] }
  input?: { channels?: number; map?: number[] | null; source?: InputSource }
  plan_sha256?: string
}

/** 声道准备预览（old/new 为可读配置；不变的部分为 null）。 */
export interface ChannelsPreview {
  old_channels: number | null
  new_channels: number | null
  old_input_channels: number | null
  new_input_channels: number | null
  audio_gap_ms_estimate: number | null
}

/** PUT /api/channels 的准备回执。 */
export interface ChannelsPrepareAck {
  ok: true
  engine_token: string
  planned_version: number | null
  changed: boolean | null
  preview: ChannelsPreview | null
  warnings: string[]
  blockers: string[]
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

/** 校验并归一化 GET /api/channels 响应。 */
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
    output: {
      channels: readNumber(output, 'channels', 'output'),
      dac_map: readNullableNumberArray(output, 'dac_map', 'output')
    },
    input: {
      channels: readNumber(input, 'channels', 'input'),
      map: readNumberArray(input, 'map', 'input'),
      source: source === 'sdp' || source === 'manual' ? source : null,
      sdp_sha256: readString(input, 'sdp_sha256', 'input')
    },
    mapping: parseMapping(root.mapping),
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
 * 接受三种形态（T60：以 daemon 真实形态为主，旧 mock 契约保留兼容）：
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

/** 校验并归一化 PUT /api/channels 的 prepare 回执。 */
export function parseChannelsPrepareAck(body: unknown): ChannelsPrepareAck {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('channels prepare body must be an object')
  }
  if (record.ok !== true) {
    throw invalidResponse('ok must be true')
  }
  const token = readString(record, 'engine_token', 'prepare')
  if (token === null || token.length === 0) {
    throw invalidResponse('engine_token must be a non-empty string')
  }
  const previewRaw = record.preview
  let preview: ChannelsPreview | null = null
  if (previewRaw !== null && previewRaw !== undefined) {
    const previewRecord = asRecord(previewRaw)
    if (previewRecord === null) {
      throw invalidResponse('preview must be an object or null')
    }
    const oldRaw = group(previewRecord.old, 'preview.old')
    const newRaw = group(previewRecord.new, 'preview.new')
    preview = {
      old_channels: readNumber(oldRaw, 'channels', 'preview.old'),
      new_channels: readNumber(newRaw, 'channels', 'preview.new'),
      old_input_channels: readNumber(oldRaw, 'input_channels', 'preview.old'),
      new_input_channels: readNumber(newRaw, 'input_channels', 'preview.new'),
      audio_gap_ms_estimate: readNumber(previewRecord, 'audio_gap_ms_estimate', 'preview')
    }
  }
  function readStringArray(value: unknown, path: string): string[] {
    if (value === null || value === undefined) {
      return []
    }
    if (!Array.isArray(value)) {
      throw invalidResponse(path + ' must be an array or null')
    }
    return value.map((item, index) => {
      if (typeof item !== 'string') {
        throw invalidResponse(path + '[' + String(index) + '] must be a string')
      }
      return item
    })
  }
  return {
    ok: true,
    engine_token: token,
    planned_version: readNumber(record, 'planned_version', 'prepare'),
    changed: readBoolean(record, 'changed', 'prepare'),
    preview,
    warnings: readStringArray(record.warnings, 'warnings'),
    blockers: readStringArray(record.blockers, 'blockers')
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

/** 校验声道补丁（返回前端 error 码；null = 合法）。 */
export function validateChannelsPatch(patch: ChannelsPatch, laneCount: number | null): string | null {
  const outputChannels = patch.output?.channels
  if (outputChannels !== undefined) {
    if (!Number.isInteger(outputChannels) || outputChannels < 1 || outputChannels > 32) {
      return 'output_range'
    }
  }
  const dacMap = patch.output?.dac_map
  if (dacMap !== undefined) {
    const expected = outputChannels ?? laneCount
    if (expected !== null && dacMap.length !== expected) {
      return 'dac_map_length'
    }
    const seen = new Set<number>()
    for (const dac of dacMap) {
      if (!Number.isInteger(dac) || dac < 0 || dac > 31 || seen.has(dac)) {
        return 'dac_map_value'
      }
      seen.add(dac)
    }
  }
  const inputChannels = patch.input?.channels
  if (inputChannels !== undefined) {
    if (!Number.isInteger(inputChannels) || inputChannels < 1 || inputChannels > 8) {
      return 'input_range'
    }
  }
  const inputMap = patch.input?.map
  if (inputMap !== undefined && inputMap !== null) {
    const expected = inputChannels ?? null
    if (expected !== null && inputMap.length !== expected) {
      return 'input_map_length'
    }
    const seen = new Set<number>()
    for (const channel of inputMap) {
      if (!Number.isInteger(channel) || channel < 0 || channel > 63 || seen.has(channel)) {
        return 'input_map_value'
      }
      seen.add(channel)
    }
  }
  if (outputChannels === undefined && dacMap === undefined && inputChannels === undefined && inputMap === undefined) {
    return 'empty'
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
  prepareChannels(patch: ChannelsPatch): Promise<ChannelsPrepareAck>
  /** 共享两阶段提交（channels 草稿在 M3b 前返回 501 host_link_unsupported）。 */
  commit(engineToken: string): Promise<ControlCommitAck>
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
      const body = await client.put<unknown>(CHANNELS_PATHS.channels, patch)
      return parseChannelsPrepareAck(body)
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
}

const CHANNELS_ERROR_KEYS: Record<ChannelsErrorCategory, string> = {
  network: 'error.channels.network',
  timeout: 'error.channels.timeout',
  unauthorized: 'error.channels.unauthorized',
  host_link_unavailable: 'error.channels.hostLinkUnavailable',
  host_link_unsupported: 'error.channels.hostLinkUnsupported',
  not_wired: 'error.channels.notWired',
  not_found: 'error.channels.notFound',
  invalid_request: 'error.channels.invalidRequest',
  invalid_plan: 'error.channels.invalidPlan',
  upload_too_large: 'error.channels.uploadTooLarge',
  upload_not_found: 'error.channels.uploadNotFound',
  sha_mismatch: 'error.channels.shaMismatch',
  admission_rejected: 'error.channels.admissionRejected',
  no_history: 'error.channels.noHistory',
  plan_path_unavailable: 'error.channels.planPathUnavailable',
  conflict: 'error.channels.conflict',
  server: 'error.channels.server',
  invalid_response: 'error.channels.invalidResponse',
  unknown: 'error.channels.unknown'
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
  channel_layout_unavailable: 'conflict',
  request_timeout: 'timeout',
  network_error: 'network'
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
  } else {
    category =
      CHANNELS_CODE_CATEGORIES[error.code] ??
      (error.status >= 500 ? 'server' : error.status === 404 ? 'not_found' : 'conflict')
  }
  const detail = error.message.length > 0 ? error.message : undefined
  return {
    category,
    status: error.status,
    code: error.code,
    i18nKey: CHANNELS_ERROR_KEYS[category],
    ...(detail ? { detail } : {}),
    retryable: category !== 'unauthorized' && category !== 'invalid_response'
  }
}
