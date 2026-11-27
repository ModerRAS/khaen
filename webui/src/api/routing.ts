/**
 * 路由矩阵页（task-68）——「输入 → 矩阵域 → DAC 输出」只读诊断 + lane → DAC 可编辑两阶段的数据组合。
 *
 * 只读诊断来源（缺数据一律 null + 原因）：
 * - GET /api/channels      khaen-channels/v1：output.channels/dac_map、input.*、mapping[{lane,dac}]、
 *                          plan.{source,version,graph_id,routing_known}、reasons、not_wired；
 * - GET /api/status        khaen-status/v1：format.channels / format.aes67.{channels,map} /
 *                          plan.{version,latency_samples,applied_at,source,graph_id} / host_link / reasons；
 * - GET /api/plan/history  已生效历史：current_version + entries（当前版本、latency 的回退来源、applied_at）。
 *
 * 当前可编辑值（唯一来源）：GET /api/channels 的 `mapping`（lane → 0-based DAC）。它的优先来源是
 * rt-host 控制 status 的 `readback.routing`（真实已加载 base plan），拿不到才退回 daemon 在
 * prepare/upload 时记录的方案 routing；mapping = null 表示 routing 段未知，页面必须显示
 * 「当前值未知」+ 原因，**不得用 dac_map / 旧值 / 默认值冒充当前值**（API.md §5 末条、
 * §9.7、§11 裁决 G3/G6）。
 *
 * 写入口（两阶段，API.md §1 / §4）：`POST /api/surface {surface:"routing", data:{routes:[lane,dac],
 * dac_channels?}}` → 草稿 + engine_token；`POST /api/commit {engine_token}` 生效；
 * `POST /api/abort {engine_token}` 丢弃。routes 是 lane → DAC 的**全量替换表**；dac_channels 缺省沿用
 * 方案值（CONTRACTS B.1.2）。changed/diff/current/target 等是可选回执字段，缺失时页面显示「服务端未上报」。
 *
 * 本模块只放纯函数（解析 / 当前值来源 / 草稿校验与构造），不依赖 Vue/DOM，Node 单测直接覆盖；
 * 两阶段请求与草稿状态机在 api/control.ts + stores/control.ts（本页复用，不另造一套）。
 *
 * 解析纪律（对真实 daemon 形态更宽容，仍拒绝结构漂移）：
 * - 真实 daemon 的 `dac_map` 允许 null（该 DAC 没有 lane 喂入/静音），旧契约是纯 number[]；
 * - `/api/plan/history` 同时接受 daemon 的 `{current:{version},entries:[]}` 与 T54 mock 契约的
 *   `{current_version,versions:[]}`（也接受 history/items 别名），条目 applied_at 兼容 recorded_at；
 * - 缺字段是合法缺失形态（null + reasons），类型漂移/结构非法才抛 invalid_response；
 * - 纯函数，不依赖 Vue/DOM，Node 单测直接覆盖；组件只消费派生结果。
 */
import { ApiClient, ApiError } from './client.ts'
import { CHANNELS_SCHEMA, motuOutputLabelKey } from './channels.ts'
import {
  buildRoutingPatch,
  validateRoutingPatch,
  type RoutingPatch
} from './control.ts'
import {
  reasonForPath,
  type StatusReadback,
  type StatusResponse
} from './status.ts'

/** 路由页组合用的只读端点（写入口复用 api/control.ts 的 CONTROL_PATHS）。 */
export const ROUTING_PATHS = {
  channels: '/api/channels',
  planHistory: '/api/plan/history'
} as const

/** AES67 输入来源（SDP 描述符 / 手工配置）；null = 未上报。 */
export type RoutingInputSource = 'sdp' | 'manual'

/** /api/channels 的 plan 段（真实 daemon 提供；mock/旧契约可能缺省）。 */
export interface RoutingChannelsPlan {
  source: string | null
  version: number | null
  graph_id: string | null
  /** 当前生效方案的 routing 段是否被 daemon 关联到（真实 daemon 字段）。 */
  routing_known: boolean | null
}

/**
 * GET /api/channels 的【加法】字段 speaker_roles 条目（docs/API.md §12.2；后端已实现，
 * 缺失/null 是合法形态）：某个 0-based DAC 的 canonical 角色（L/R/C/LFE/Tfl…）。
 *
 * 纪律：role 原样展示（代码不维护中文映射表、不猜测），enabled 未知时为 null。
 */
export interface RoutingSpeakerRole {
  dac: number
  role: string
  enabled: boolean | null
}

/** GET /api/channels 中路由页要用的归一化子集（dac_map 允许 null 项）。 */
export interface RoutingChannelsInfo {
  schema: typeof CHANNELS_SCHEMA
  output: {
    channels: number | null
    dac_map: Array<number | null> | null
  }
  input: {
    channels: number | null
    map: Array<number | null> | null
    source: RoutingInputSource | null
    sdp_sha256: string | null
  }
  /** lane -> 0-based dac（按 lane 升序）；null = routing 段未知（不是「0 条路由」）。 */
  mapping: Array<{ lane: number; dac: number }> | null
  /** 【加法】dac -> canonical 角色（docs/API.md §12.2）；未上报/null = 不显示角色（不编造）。 */
  speaker_roles: RoutingSpeakerRole[] | null
  plan: RoutingChannelsPlan
  reasons: Record<string, string>
  not_wired: Array<{ path: string; reason: string }>
}

/** 已生效历史条目（路由页只用来补 current version / latency / applied_at）。 */
export interface RoutingHistoryEntry {
  version: number
  graph_id: string | null
  sha256: string | null
  applied_at: string | null
  source: string | null
  latency_samples: number | null
}

/** GET /api/plan/history 的归一化形态（兼容 daemon 与 mock 两种形状）。 */
export interface RoutingPlanHistory {
  current_version: number | null
  entries: RoutingHistoryEntry[]
  /** 历史接口自带的 reasons（history.current / history.available_versions 等）。 */
  reasons: Record<string, string>
}

/* ------------------------------------------------------------------ 运行时校验 */

function invalidResponse(detail: string): ApiError {
  return new ApiError({
    status: 200,
    code: 'invalid_response',
    message: 'unexpected routing response: ' + detail
  })
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return null
  }
  return value as Record<string, unknown>
}

/** 组缺失 / null 归一化为空对象（整块子系统未接入是合法缺失形态）。 */
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

function readInteger(record: Record<string, unknown>, field: string, path: string): number | null {
  const value = readNumber(record, field, path)
  if (value !== null && !Number.isInteger(value)) {
    throw invalidResponse(path + '.' + field + ' must be an integer or null')
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

/** number|null 数组：dac_map 的 null 表示「该 DAC 没有 lane 喂入」，是合法值。 */
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
    throw invalidResponse(path + '.' + field + ' must be an array or null')
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

function parseNotWired(value: unknown): Array<{ path: string; reason: string }> {
  if (value === null || value === undefined) {
    return []
  }
  if (!Array.isArray(value)) {
    throw invalidResponse('not_wired must be an array or null')
  }
  return value.map((item, index) => {
    const path = 'not_wired[' + String(index) + ']'
    const record = asRecord(item)
    if (record === null) {
      throw invalidResponse(path + ' must be an object')
    }
    return {
      path: readString(record, 'path', path) ?? '',
      reason: readString(record, 'reason', path) ?? ''
    }
  })
}

function parseMapping(value: unknown): Array<{ lane: number; dac: number }> | null {
  if (value === null || value === undefined) {
    // null = routing 段未知（daemon 在关联不到已生效方案时这样上报），与「已知 0 条路由」区分。
    return null
  }
  if (!Array.isArray(value)) {
    throw invalidResponse('mapping must be an array or null')
  }
  const rows = value.map((item, index) => {
    const path = 'mapping[' + String(index) + ']'
    const record = asRecord(item)
    if (record === null) {
      throw invalidResponse(path + ' must be an object')
    }
    const lane = readInteger(record, 'lane', path)
    const dac = readInteger(record, 'dac', path)
    if (lane === null || dac === null || lane < 0 || dac < 0) {
      throw invalidResponse(path + ' must have non-negative integer lane and dac')
    }
    return { lane, dac }
  })
  rows.sort((left, right) => left.lane - right.lane || left.dac - right.dac)
  return rows
}

/**
 * 解析【加法】字段 speaker_roles（docs/API.md §12.2；旧后端/不可读时缺省/null）。
 * - 缺失/null → null（不是错误，也不编造角色）；
 * - 结构漂移（数组项非对象 / dac 非非负整数 / role 非字符串）→ invalid_response。
 */
function parseSpeakerRoles(value: unknown): RoutingSpeakerRole[] | null {
  if (value === null || value === undefined) {
    return null
  }
  if (!Array.isArray(value)) {
    throw invalidResponse('speaker_roles must be an array or null')
  }
  return value.map((item, index) => {
    const path = 'speaker_roles[' + String(index) + ']'
    const record = asRecord(item)
    if (record === null) {
      throw invalidResponse(path + ' must be an object')
    }
    const dac = readInteger(record, 'dac', path)
    const role = readString(record, 'role', path)
    if (dac === null || dac < 0) {
      throw invalidResponse(path + '.dac must be a non-negative integer')
    }
    if (role === null || role.length === 0) {
      throw invalidResponse(path + '.role must be a non-empty string')
    }
    return { dac, role, enabled: readBoolean(record, 'enabled', path) }
  })
}

function parseChannelsPlan(value: unknown): RoutingChannelsPlan {
  const record = group(value, 'plan')
  return {
    source: readString(record, 'source', 'plan'),
    version: readInteger(record, 'version', 'plan'),
    graph_id: readString(record, 'graph_id', 'plan'),
    routing_known: readBoolean(record, 'routing_known', 'plan')
  }
}

/** 校验并归一化 GET /api/channels（只保留路由页要用的字段，dac_map 允许 null 项）。 */
export function parseRoutingChannels(body: unknown): RoutingChannelsInfo {
  const root = asRecord(body)
  if (root === null) {
    throw invalidResponse('channels body must be an object')
  }
  if (root.schema !== CHANNELS_SCHEMA) {
    throw invalidResponse('channels schema must be "' + CHANNELS_SCHEMA + '"')
  }
  const output = group(root.output, 'output')
  const input = group(root.input, 'input')
  const source = readString(input, 'source', 'input')
  return {
    schema: CHANNELS_SCHEMA,
    output: {
      channels: readInteger(output, 'channels', 'output'),
      dac_map: readNullableNumberArray(output, 'dac_map', 'output')
    },
    input: {
      channels: readInteger(input, 'channels', 'input'),
      map: readNullableNumberArray(input, 'map', 'input'),
      source: source === 'sdp' || source === 'manual' ? source : null,
      sdp_sha256: readString(input, 'sdp_sha256', 'input')
    },
    mapping: parseMapping(root.mapping),
    speaker_roles: parseSpeakerRoles(root.speaker_roles),
    plan: parseChannelsPlan(root.plan),
    reasons: readReasons(root.reasons),
    not_wired: parseNotWired(root.not_wired)
  }
}

function parseHistoryEntry(item: unknown, index: number): RoutingHistoryEntry {
  const path = 'history.entries[' + String(index) + ']'
  const record = asRecord(item)
  if (record === null) {
    throw invalidResponse(path + ' must be an object')
  }
  const version = readInteger(record, 'version', path)
  if (version === null) {
    throw invalidResponse(path + '.version must be an integer')
  }
  return {
    version,
    graph_id: readString(record, 'graph_id', path),
    sha256: readString(record, 'sha256', path),
    // daemon 的 commit 记录用 recorded_at；T54 mock 契约与 /channels 页用 applied_at。
    applied_at: readString(record, 'applied_at', path) ?? readString(record, 'recorded_at', path),
    source: readString(record, 'source', path),
    latency_samples: readNumber(record, 'latency_samples', path)
  }
}

/**
 * 校验并归一化 GET /api/plan/history。
 *
 * 接受三种形态：
 * - daemon 真实形态：`{schema, current:{version,graph_id}, entries:[{version,graph_id,recorded_at,...}], reasons}`；
 * - T54 mock 契约：`{current_version, versions:[{version,graph_id,sha256,applied_at,source}]}`；
 * - 纯数组：`[{version,...}]`（旧兼容）。
 */
export function parseRoutingPlanHistory(body: unknown): RoutingPlanHistory {
  let rawEntries: unknown
  let current: number | null = null
  let reasons: Record<string, string> = {}
  if (Array.isArray(body)) {
    rawEntries = body
  } else {
    const record = asRecord(body)
    if (record === null) {
      throw invalidResponse('plan history body must be an object or array')
    }
    rawEntries = record.versions ?? record.entries ?? record.history ?? record.items
    current = readInteger(record, 'current_version', 'history')
    if (current === null) {
      const currentObject = asRecord(record.current)
      if (currentObject !== null) {
        current = readInteger(currentObject, 'version', 'history.current')
      } else {
        current = readInteger(record, 'current', 'history')
      }
    }
    reasons = readReasons(record.reasons)
  }
  if (!Array.isArray(rawEntries)) {
    throw invalidResponse('plan history entries must be an array')
  }
  const entries = rawEntries.map((item, index) => parseHistoryEntry(item, index))
  entries.sort((left, right) => left.version - right.version)
  if (current === null && entries.length > 0) {
    current = entries[entries.length - 1].version
  }
  return { current_version: current, entries, reasons }
}

/* ---------------------------------------------------------------------- 端点 */

export interface RoutingApi {
  fetchChannels(): Promise<RoutingChannelsInfo>
  fetchPlanHistory(): Promise<RoutingPlanHistory>
}

/** 用共享 ApiClient 构造路由页 API（单测注入 fetchImpl 即可脱离网络）。 */
export function createRoutingApi(client: ApiClient): RoutingApi {
  return {
    async fetchChannels(): Promise<RoutingChannelsInfo> {
      const body = await client.get<unknown>(ROUTING_PATHS.channels)
      return parseRoutingChannels(body)
    },
    async fetchPlanHistory(): Promise<RoutingPlanHistory> {
      const body = await client.get<unknown>(ROUTING_PATHS.planHistory)
      return parseRoutingPlanHistory(body)
    }
  }
}

/* ------------------------------------------------------------------ 原因查询 */

/** 读取 /api/channels 字段缺失原因（完整路径优先，回退末段扁平键）。 */
export function reasonForRoutingChannelsPath(
  info: RoutingChannelsInfo | null | undefined,
  path: string
): string | null {
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

/** 读取 /api/plan/history 字段缺失原因（完整路径优先，回退末段扁平键）。 */
export function reasonForRoutingHistoryPath(
  history: RoutingPlanHistory | null | undefined,
  path: string
): string | null {
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

/* ------------------------------------------------------------------ 派生逻辑 */

/** lane 映射表的单行：lane -> 0-based DAC -> 1-based MOTU 物理输出。 */
export interface RoutingLaneRow {
  lane: number
  /** 0-based DAC 下标；null = 该 lane 未出现在 mapping 里（没有路由信息，不猜直通）。 */
  dac: number | null
  /** 1-based MOTU 物理输出（dac+1）；null = 未知。 */
  physical: number | null
  /** 已知位置的 i18n 标签键；未知位置为 null（页面显示 OUT n）。 */
  labelKey: string | null
}

/**
 * lane 映射表行：优先 mapping（lane -> dac）；
 * mapping 缺失时退回 dac_map（第 i 项 = 喂给 DAC i 的 lane；null = 该 DAC 静音/未喂入）。
 * 两条来源都缺失时返回空数组（页面显示「未接入」+ 原因，不猜直通映射）。
 */
export function laneRows(info: RoutingChannelsInfo | null): RoutingLaneRow[] {
  if (info === null) {
    return []
  }
  const seen = new Set<number>()
  const rows: RoutingLaneRow[] = []
  const push = (lane: number, dac: number | null): void => {
    if (seen.has(lane)) {
      return
    }
    seen.add(lane)
    const physical = dac === null ? null : dac + 1
    rows.push({
      lane,
      dac,
      physical,
      labelKey: physical === null ? null : motuOutputLabelKey(physical)
    })
  }
  if (info.mapping !== null && info.mapping.length > 0) {
    for (const entry of info.mapping) {
      push(entry.lane, entry.dac)
    }
  } else if (info.output.dac_map !== null) {
    info.output.dac_map.forEach((lane, dac) => {
      if (lane !== null) {
        push(lane, dac)
      }
    })
  }
  rows.sort((left, right) => left.lane - right.lane)
  return rows
}

/** 路由链的单个缺失原因：优先后端原因，缺失时页面用 i18n 兜底键。 */
export interface RoutingChainField {
  value: number | null
  reason: string | null
}

/** 「输入 → 矩阵域 → DAC 输出」链路快照（null = 没有数据源，不是 0）。 */
export interface RoutingChain {
  input: {
    /** AES67 接收声道数（优先 /api/status format.aes67.channels）。 */
    channels: RoutingChainField
    /** AES67 通道映射。 */
    map: {
      value: Array<number | null> | null
      reason: string | null
    }
    /** 输入来源（sdp / manual）。 */
    source: {
      value: RoutingInputSource | null
      reason: string | null
    }
  }
  matrix: {
    /** 矩阵域 lane 数（优先 /api/status format.channels）。 */
    lanes: RoutingChainField
    /** 已路由 lane 数（mapping 里出现过的 lane 数）。 */
    routedLanes: RoutingChainField
    /** 路由条目数（mapping 条数）。 */
    routes: RoutingChainField
  }
  output: {
    /** DAC 输出声道数。 */
    channels: RoutingChainField
    /** DAC 映射（0-based；null 项 = 该 DAC 静音）。 */
    dacMap: {
      value: Array<number | null> | null
      reason: string | null
    }
    /** 已知物理位置标签数（L/R/SL/SR/TF/TFR/LFE）。 */
    knownLabels: number
  }
}

function field(value: number | null, reason: string | null): RoutingChainField {
  return { value, reason: value === null ? reason : null }
}

/**
 * 组合路由链路。
 *
 * 明确不推断：
 * - 没有 mapping 时不假设「lane i -> dac i」的直通映射（那会编造路由）；
 * - 输入声道优先 status.format.aes67.channels（rt-host 数据面），其次 /api/channels.input.channels；
 * - 矩阵 lane 数只认 status.format.channels：mapping 条数只是「已路由 lane 数」，不能当 lane 总数
 *   （部分路由是合法形态），所以不给 mapping 条数冒充 lane 总数。
 */
export function buildRoutingChain(
  status: StatusResponse | null,
  channels: RoutingChannelsInfo | null
): RoutingChain {
  const rows = laneRows(channels)
  const aes67 = status?.format.aes67 ?? null
  const inputChannels = aes67?.channels ?? channels?.input.channels ?? null
  const inputChannelsReason =
    aes67 !== null
      ? reasonForPath(status, 'format.aes67.channels') ?? reasonForRoutingChannelsPath(channels, 'input.channels')
      : reasonForRoutingChannelsPath(channels, 'input.channels')
  const inputMap = aes67?.map ?? channels?.input.map ?? null
  const inputMapReason =
    aes67 !== null
      ? reasonForPath(status, 'format.aes67.map') ?? reasonForRoutingChannelsPath(channels, 'input.map')
      : reasonForRoutingChannelsPath(channels, 'input.map')
  const statusChannels = status?.format.channels ?? null
  const mapping = channels?.mapping ?? null
  const routes = mapping === null ? null : mapping.length
  const routedLanes = mapping === null ? null : rows.filter((row) => row.dac !== null).length
  const dacMap = channels === null ? null : channels.output.dac_map
  const dacChannels = channels === null ? null : (channels.output.channels ?? (dacMap === null ? null : dacMap.length))
  return {
    input: {
      channels: field(inputChannels, inputChannelsReason),
      map: { value: inputMap, reason: inputMap === null ? inputMapReason : null },
      source: {
        value: channels?.input.source ?? null,
        reason: channels?.input.source === null || channels === null ? reasonForRoutingChannelsPath(channels, 'input.source') : null
      }
    },
    matrix: {
      lanes: field(statusChannels, reasonForPath(status, 'format.channels')),
      routedLanes: field(routedLanes, reasonForRoutingChannelsPath(channels, 'mapping')),
      routes: field(routes, reasonForRoutingChannelsPath(channels, 'mapping'))
    },
    output: {
      channels: field(dacChannels, reasonForRoutingChannelsPath(channels, 'output.channels')),
      dacMap: {
        value: channels?.output.dac_map ?? null,
        reason: channels?.output.dac_map === null || channels === null ? reasonForRoutingChannelsPath(channels, 'output.dac_map') : null
      },
      knownLabels: rows.filter((row) => row.labelKey !== null).length
    }
  }
}

/** 计划内 routing 段摘要（缺失字段 null + 原因；segments 数当前没有只读数据源）。 */
export interface RoutingPlanSummary {
  version: number | null
  versionSource: 'status' | 'channels' | 'history' | null
  versionReason: string | null
  latencySamples: number | null
  latencySource: 'status' | 'history' | null
  latencyReason: string | null
  /** plan 区段数（content/crossover/matrix/protection/routing）：当前只读接口都不暴露。 */
  segments: number | null
  /** 后端给出的 segments 缺失原因（若某天后端在 reasons 里上报 plan.segments）；null = 用 segmentsReasonKey。 */
  segmentsReason: string | null
  /** segments 缺失时的兜底 i18n 原因键（页面本地化展示）。 */
  segmentsReasonKey: string
  graphId: string | null
  appliedAt: string | null
  planSource: string | null
  /** 当前生效方案的 routing 段是否已知（/api/channels.plan.routing_known 或 mapping 非空）。 */
  routingKnown: boolean | null
  routingKnownReason: string | null
  dacChannels: number | null
  routes: number | null
  /** 历史接口报告的当前版本（与 status 交叉核对用）。 */
  historyCurrentVersion: number | null
}

/**
 * 组合计划摘要。
 *
 * - version：/api/status 优先（实时生效），其次 /api/channels.plan，其次 /api/plan/history；
 * - latency：/api/status.plan.latency_samples 优先（准入预算），缺失时回退当前历史条目的 latency_samples；
 * - segments（区段数）：**没有只读数据源** —— daemon 只在 POST /api/plan/upload 回执里报告
 *   segments/segment_count，/api/status 与 /api/plan/history 都不含它；这里如实 null + 原因，
 *   绝不用「路由条目数」冒充区段数。
 * - routingKnown：真实 daemon 的 /api/channels.plan.routing_known；缺失时用 mapping 是否非空判定，
 *   两者都没有则为 null + 原因（不假设已知/未知）。
 */
export function buildPlanSummary(
  status: StatusResponse | null,
  channels: RoutingChannelsInfo | null,
  history: RoutingPlanHistory | null
): RoutingPlanSummary {
  const currentVersion = history?.current_version ?? null
  const currentEntry =
    currentVersion === null
      ? null
      : (history?.entries.find((entry) => entry.version === currentVersion) ?? null)
  const statusVersion = status?.plan.version ?? null
  const channelsVersion = channels?.plan.version ?? null
  const version = statusVersion ?? channelsVersion ?? currentVersion
  const versionSource =
    statusVersion !== null
      ? ('status' as const)
      : channelsVersion !== null
        ? ('channels' as const)
        : currentVersion !== null
          ? ('history' as const)
          : null
  const versionReason =
    version !== null
      ? null
      : reasonForPath(status, 'plan.version') ??
        reasonForRoutingChannelsPath(channels, 'plan') ??
        reasonForRoutingHistoryPath(history, 'history.current')
  const statusLatency = status?.plan.latency_samples ?? null
  const historyLatency = currentEntry?.latency_samples ?? null
  const latency = statusLatency ?? historyLatency
  const latencySource = statusLatency !== null ? ('status' as const) : historyLatency !== null ? ('history' as const) : null
  const latencyReason =
    latency !== null
      ? null
      : reasonForPath(status, 'plan.latency_samples') ?? reasonForRoutingHistoryPath(history, 'history.current')
  const mapping = channels?.mapping ?? null
  const routingKnown =
    channels?.plan.routing_known ?? (mapping !== null && mapping.length > 0 ? true : null)
  const dacChannels = channels?.output.channels ?? null
  return {
    version,
    versionSource,
    versionReason,
    latencySamples: latency,
    latencySource,
    latencyReason,
    segments: null,
    segmentsReason: reasonForPath(status, 'plan.segments'),
    segmentsReasonKey: 'routing.plan.segmentsReason',
    graphId: status?.plan.graph_id ?? channels?.plan.graph_id ?? currentEntry?.graph_id ?? null,
    appliedAt: status?.plan.applied_at ?? currentEntry?.applied_at ?? null,
    planSource: status?.plan.source ?? channels?.plan.source ?? null,
    routingKnown,
    routingKnownReason:
      routingKnown === null
        ? reasonForRoutingChannelsPath(channels, 'plan') ??
          reasonForRoutingChannelsPath(channels, 'mapping')
        : null,
    dacChannels,
    routes: mapping === null ? null : mapping.length,
    historyCurrentVersion: currentVersion
  }
}

/* ------------------------------------------ 可编辑草稿（POST /api/surface routing） */

/** 可编辑草稿行：lane → 0-based DAC。提交时 routes 是**全量替换表**（不是增量）。 */
export interface RoutingDraftRow {
  lane: number
  dac: number
}

/** 草稿行 → routes（[lane, dac] 整数对，按 lane/DAC 升序，保证请求体确定）。 */
export function rowsToRoutes(rows: readonly RoutingDraftRow[]): Array<[number, number]> {
  return rows
    .map((row) => [row.lane, row.dac] as [number, number])
    .sort((left, right) => left[0] - right[0] || left[1] - right[1])
}

/** 新增行用的默认 lane：最小未占用的非负整数（越界由后端准入拒绝，前端不猜 lane 总数）。 */
export function nextRoutingLane(rows: readonly RoutingDraftRow[]): number {
  const used = new Set<number>()
  for (const row of rows) {
    if (Number.isInteger(row.lane) && row.lane >= 0) {
      used.add(row.lane)
    }
  }
  let lane = 0
  while (used.has(lane)) {
    lane += 1
  }
  return lane
}

/**
 * 「当前值」来源快照——唯一来源是 GET /api/channels 的 `mapping`（lane → 0-based DAC），
 * 它是 daemon 在 prepare/upload 时记录的**方案 routing 镜像**，不是数据面实时回读。
 *
 * 降级口径（API.md §5 末条 / §9.7 / §11 裁决 G3、G6）：
 * - mapping = null（方案图被 channels_commit 丢弃 / mirror 未记录）→ rows = null、source = null，
 *   页面必须显示「当前值未知」+ 原因，**禁止**用 dac_map / 旧值 / 默认值冒充；
 * - mapping = [] → known = true（明确的「已知 0 条路由」，与未知区分）；
 * - /api/channels 整体不可用 → rows = null、reason = null（页面用 i18n 兜底原因）。
 */
export interface RoutingCurrentSource {
  /** 可预填的草稿行（mapping，按 lane 升序）；null = 当前值未知。 */
  rows: RoutingDraftRow[] | null
  /** 'base_plan' = /api/status.readback；'mapping' = /api/channels 的方案 routing 镜像；null = 没有来源。 */
  source: 'base_plan' | 'mapping' | null
  /** true = mapping 已上报（含空数组）；false = 未知（mapping:null / /api/channels 不可用）。 */
  known: boolean
  /** 当前方案 DAC 声道数（/api/channels.output.channels，回退 dac_map 长度；不是实时回读）。 */
  dacChannels: number | null
  /** 来源方案版本 / graph_id（快照语义；null = 未上报）。 */
  planVersion: number | null
  planGraphId: string | null
  /** daemon 是否关联到当前方案 routing 段（/api/channels.plan.routing_known；null = 未上报）。 */
  routingKnown: boolean | null
  /** 当前值未知原因（优先后端 reasons.mapping，其次 reasons.plan；null = 后端未给原因）。 */
  reason: string | null
}

/** 组合「当前值」来源快照（纯函数）。
 *
 * 优先级：/api/status.readback（base_plan 的控制面语义，rt-host socket 主路径）> 
 * /api/channels.mapping（daemon 的 routing 镜像）> 未知。readback 没有 routing 段或不可用时
 * 才退回 mapping；两者都没有就如实未知，不猜任何值。
 */
export function currentRoutingSource(
  info: RoutingChannelsInfo | null,
  readback?: StatusReadback | null
): RoutingCurrentSource {
  if (readback !== null && readback !== undefined && readback.available && readback.routing !== null) {
    const rows = readback.routing.routes
      .map((entry) => ({ lane: entry.lane, dac: entry.dac }))
      .sort((left, right) => left.lane - right.lane || left.dac - right.dac)
    return {
      rows,
      source: 'base_plan',
      known: true,
      dacChannels: readback.routing.dac_channels ?? info?.output.channels ?? null,
      planVersion: info?.plan.version ?? null,
      planGraphId: info?.plan.graph_id ?? null,
      routingKnown: true,
      reason: null
    }
  }
  if (info === null) {
    return {
      rows: null,
      source: null,
      known: false,
      dacChannels: null,
      planVersion: null,
      planGraphId: null,
      routingKnown: null,
      reason: null
    }
  }
  const mapping = info.mapping
  const rows =
    mapping === null
      ? null
      : mapping
          .map((entry) => ({ lane: entry.lane, dac: entry.dac }))
          .sort((left, right) => left.lane - right.lane || left.dac - right.dac)
  const dacMap = info.output.dac_map
  const dacChannels = info.output.channels ?? (dacMap === null ? null : dacMap.length)
  const reason =
    rows === null
      ? (reasonForRoutingChannelsPath(info, 'mapping') ?? reasonForRoutingChannelsPath(info, 'plan'))
      : null
  return {
    rows,
    source: rows === null ? null : 'mapping',
    known: rows !== null,
    dacChannels,
    planVersion: info.plan.version,
    planGraphId: info.plan.graph_id,
    routingKnown: info.plan.routing_known,
    reason
  }
}

/**
 * 查询某个 0-based DAC 的 canonical 角色（GET /api/channels 的【加法】字段 speaker_roles）。
 *
 * - 字段缺失/null（旧后端/不可读，docs/API.md §12）→ null：页面不显示角色，也不编造；
 * - role 原样返回 canonical 字符串（显示层不做中文映射，避免两套角色表漂移）；
 * - enabled 未知时同样返回（页面按需显示），不推断。
 */
export function speakerRoleForDac(
  info: RoutingChannelsInfo | null,
  dac: number | null
): { role: string; enabled: boolean | null } | null {
  if (info === null || info.speaker_roles === null || dac === null || !Number.isInteger(dac)) {
    return null
  }
  const entry = info.speaker_roles.find((item) => item.dac === dac)
  return entry === undefined ? null : { role: entry.role, enabled: entry.enabled }
}

/** 本地草稿校验的可选项。 */
export interface RoutingDraftValidationOptions {
  /** 显式指定的 dac_channels；undefined = 未勾选（沿用方案值）；null = 勾选了但没填值。 */
  explicitDacChannels?: number | null
  /** 当前方案 DAC 声道数（dac 上界校验；null/undefined = 未知，跳过上界）。 */
  knownDacChannels?: number | null
}

/**
 * 本地校验 lane → DAC 草稿（错误码；null = 合法）。
 *
 * 在 api/control.ts 的 validateRoutingPatch 之上补充两条编辑期约束：
 * - 勾选「显式指定 dac_channels」但没填值 → 'dac_channels'；
 * - 已知 DAC 声道数时，DAC 序号必须 < 声道数 → 'dac_range'（最终仍以后端准入为准）。
 * 结构错误码沿用 validateRoutingPatch：'empty' / 'entry' / 'duplicate' / 'dac_channels'。
 */
export function validateRoutingDraft(
  rows: readonly RoutingDraftRow[],
  options: RoutingDraftValidationOptions = {}
): string | null {
  if (options.explicitDacChannels === null) {
    return 'dac_channels'
  }
  const routes = rowsToRoutes(rows)
  const patch: RoutingPatch = { routes }
  if (typeof options.explicitDacChannels === 'number') {
    patch.dac_channels = options.explicitDacChannels
  }
  const structural = validateRoutingPatch(patch)
  if (structural !== null) {
    return structural
  }
  const bound =
    typeof options.explicitDacChannels === 'number'
      ? options.explicitDacChannels
      : (options.knownDacChannels ?? null)
  if (bound !== null && Number.isInteger(bound) && bound > 0) {
    for (const [, dac] of routes) {
      if (dac >= bound) {
        return 'dac_range'
      }
    }
  }
  return null
}

/**
 * 构造 POST /api/surface {surface:"routing"} 的 `data`（即 RoutingPatch）：
 * - routes 永远是全量替换表；
 * - explicitDacChannels 为整数时带 `dac_channels`；null/undefined = 沿用方案值，**不发送该字段**。
 */
export function buildRoutingDraftPatch(
  rows: readonly RoutingDraftRow[],
  explicitDacChannels: number | null | undefined
): RoutingPatch {
  const routes = rowsToRoutes(rows)
  return typeof explicitDacChannels === 'number'
    ? buildRoutingPatch(routes, explicitDacChannels)
    : buildRoutingPatch(routes)
}
