/**
 * GET /api/meta —— 版本与能力自描述（docs/API.md §2 / §9.1：页面先读能力再决定入口）。
 *
 * 实际回执（daemon meta.rs，`ok` 可缺省）：
 *   {api_version, web_version, ops:[...], surfaces:[...],
 *    calibration:{wired, reason?}, host_link:{...},
 *    not_wired:[{op,reason}|{surface,reason}]}
 *
 * 纪律：
 * - 只读端点：结构不合法 -> ApiError(invalid_response)，不把畸形响应当成"能力齐全"；
 * - surfaces 兼容数组（实际）与对象 {surface:bool}（加法/旧形态）两种形状；
 * - not_wired 项 op/surface 二选一；reason 缺失时给空串（调用方回退通用文案），不编造原因；
 * - 能力未知（meta 拿不到）不得当成"已接通"：surfaceState 返回 'unknown'，页面可降级显示但必须标注。
 *
 * 纯函数 + 可注入 client，Node 单测直接覆盖。
 */
import { ApiClient, ApiError } from './client.ts'

/** /api/meta 路径（mock/契约测试共用）。 */
export const META_PATH = '/api/meta'

/** 单次 meta 请求超时（毫秒）：每个页面首次进入会触发，不宜长等。 */
export const META_FETCH_TIMEOUT_MS = 8000

/** not_wired 项：op 或 surface 二选一（API.md §2）。 */
export interface MetaNotWiredEntry {
  op: string | null
  surface: string | null
  reason: string
}

/** 归一化后的能力快照。 */
export interface MetaCapabilities {
  apiVersion: string | null
  webVersion: string | null
  /** 已接通的 host 控制 op（status/mute/prepare/...）。 */
  ops: string[]
  /** 已接通的语义界面（volume/routing/layout/format/mode）。 */
  surfaces: string[]
  /** surfaces 以对象形式上报时的逐项布尔（兼容形状；空对象 = 未上报）。 */
  surfaceStates: Record<string, boolean>
  /** 校准 job 后端是否接线（默认未接线，原因如实上报）。 */
  calibration: { wired: boolean | null; reason: string | null }
  /** daemon→rt-host 控制通道能力（形状由 daemon 维护，前端只展示）。 */
  hostLink: Record<string, unknown> | null
  /** 未接通能力清单（op/surface 二选一）。 */
  notWired: MetaNotWiredEntry[]
}

/** 能力三态：unknown = meta 不可用/未上报（不是"可用"，也不是"不可用"）。 */
export type CapabilityState = 'wired' | 'not_wired' | 'unknown'

function invalidResponse(detail: string): ApiError {
  return new ApiError({
    status: 200,
    code: 'invalid_response',
    message: 'unexpected response from /api/meta: ' + detail
  })
}

function asRecord(value: unknown, field: string): Record<string, unknown> | null {
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'object' || Array.isArray(value)) {
    throw invalidResponse(field + ' must be an object or null')
  }
  return value as Record<string, unknown>
}

function readString(record: Record<string, unknown>, field: string): string | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'string') {
    throw invalidResponse(field + ' must be a string or null')
  }
  return value
}

/** 标量文本字段：真实 daemon 的 api_version 是整数，旧/mock 可能是字符串；归一化成 string。 */
function readScalarText(record: Record<string, unknown>, field: string): string | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value === 'string') {
    return value
  }
  if (typeof value === 'number' && Number.isFinite(value)) {
    return String(value)
  }
  throw invalidResponse(field + ' must be a string/number or null')
}

function readBoolean(record: Record<string, unknown>, field: string): boolean | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'boolean') {
    throw invalidResponse(field + ' must be a boolean or null')
  }
  return value
}

function readStringArray(record: Record<string, unknown>, field: string): string[] | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (!Array.isArray(value)) {
    throw invalidResponse(field + ' must be an array of strings or null')
  }
  return value.map((item) => {
    if (typeof item !== 'string') {
      throw invalidResponse(field + ' entries must be strings')
    }
    return item
  })
}

/** surfaces 兼容 string[]（实际）与 {surface:bool}（旧/加法形态）。 */
function readSurfaces(record: Record<string, unknown>): { list: string[]; states: Record<string, boolean> } {
  const value = record.surfaces
  if (value === null || value === undefined) {
    return { list: [], states: {} }
  }
  if (Array.isArray(value)) {
    const list = value.map((item) => {
      if (typeof item !== 'string') {
        throw invalidResponse('surfaces entries must be strings')
      }
      return item
    })
    return { list, states: {} }
  }
  const object = asRecord(value, 'surfaces')
  if (object === null) {
    return { list: [], states: {} }
  }
  const states: Record<string, boolean> = {}
  for (const [key, item] of Object.entries(object)) {
    if (typeof item !== 'boolean') {
      throw invalidResponse('surfaces.' + key + ' must be a boolean')
    }
    states[key] = item
  }
  return {
    list: Object.entries(states).filter(([, wired]) => wired).map(([key]) => key),
    states
  }
}

function readNotWired(record: Record<string, unknown>): MetaNotWiredEntry[] {
  const value = record.not_wired
  if (value === null || value === undefined) {
    return []
  }
  if (!Array.isArray(value)) {
    throw invalidResponse('not_wired must be an array or null')
  }
  return value.map((item, index) => {
    const path = 'not_wired[' + String(index) + ']'
    const object = asRecord(item, path)
    if (object === null) {
      throw invalidResponse(path + ' must be an object')
    }
    const op = readString(object, 'op')
    const surface = readString(object, 'surface')
    const reason = readString(object, 'reason') ?? ''
    if (op === null && surface === null) {
      throw invalidResponse(path + ' must carry op or surface')
    }
    return { op, surface, reason }
  })
}

/** 解析 GET /api/meta；结构不合法一律 invalid_response（不把畸形响应当成能力齐全）。 */
export function parseMeta(body: unknown): MetaCapabilities {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('body must be an object')
  }
  const surfaces = readSurfaces(record)
  const ops = readStringArray(record, 'ops') ?? []
  const calibrationRecord = asRecord(record.calibration, 'calibration')
  const calibration = {
    wired: calibrationRecord === null ? null : readBoolean(calibrationRecord, 'wired'),
    reason: calibrationRecord === null ? null : readString(calibrationRecord, 'reason')
  }
  return {
    apiVersion: readScalarText(record, 'api_version'),
    webVersion: readString(record, 'web_version'),
    ops,
    surfaces: surfaces.list,
    surfaceStates: surfaces.states,
    calibration,
    hostLink: asRecord(record.host_link, 'host_link'),
    notWired: readNotWired(record)
  }
}

/** 语义界面能力三态（unknown = meta 不可用或未上报，页面必须如实标注）。 */
export function surfaceState(meta: MetaCapabilities | null, surface: string): CapabilityState {
  if (meta === null) {
    return 'unknown'
  }
  if (meta.notWired.some((entry) => entry.surface === surface)) {
    return 'not_wired'
  }
  if (meta.surfaceStates[surface] === false) {
    return 'not_wired'
  }
  if (meta.surfaces.includes(surface) || meta.surfaceStates[surface] === true) {
    return 'wired'
  }
  return 'unknown'
}

/** host 控制 op 能力三态。 */
export function opState(meta: MetaCapabilities | null, op: string): CapabilityState {
  if (meta === null) {
    return 'unknown'
  }
  if (meta.notWired.some((entry) => entry.op === op)) {
    return 'not_wired'
  }
  return meta.ops.includes(op) ? 'wired' : 'unknown'
}

/** 未接通原因（surface 或 op；没有则 null）。 */
export function capabilityReason(meta: MetaCapabilities | null, key: { op?: string; surface?: string }): string | null {
  if (meta === null) {
    return null
  }
  const entry = meta.notWired.find(
    (item) =>
      (key.surface !== undefined && item.surface === key.surface) ||
      (key.op !== undefined && item.op === key.op)
  )
  if (entry === undefined) {
    return null
  }
  return entry.reason.length > 0 ? entry.reason : null
}

export interface MetaApi {
  /** GET /api/meta。 */
  fetch(options?: { signal?: AbortSignal }): Promise<MetaCapabilities>
}

/** 用共享 ApiClient 构造 meta API（单测注入 fetchImpl 即可脱离网络）。 */
export function createMetaApi(client: ApiClient): MetaApi {
  return {
    async fetch(options = {}) {
      const body = await client.get<unknown>(META_PATH, {
        signal: options.signal,
        timeoutMs: META_FETCH_TIMEOUT_MS
      })
      return parseMeta(body)
    }
  }
}
