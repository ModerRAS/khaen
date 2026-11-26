/**
 * GET /api/status —— 概览页唯一数据源（PLAN §4.4 `khaen-status/v1`；任务 T35b）。
 *
 * 契约（后端由 T35a 并行实现，前端按此契约 + mock 开发）：
 *   daemon{version,uptime_s,bind}
 *   host_link{connected,since,socket,status_age_ms}
 *   state{value,muted,safe,last_error}
 *   plan{graph_id,sha256,version,latency_samples,applied_at,source}
 *   format{plan_rate,device_rate,rate_asserted,block_frames,channels,encoding,
 *          aes67{channels,map,pt,group,port}}
 *   input{packets,underruns,jitter_fill,ssrc,queue_full_drops,recv_errors}
 *   output{blocks,xruns,out_errors,bytes_written}
 *   device{id,card,state,rate_readback}
 *   calibration{active_job,last_job_id,last_verdict}
 *   not_wired[{path,reason}] + reasons{field:reason}
 *
 * 纪律：
 * - **缺失值一律 null**，原因在 `reasons`（字段名 -> 原因）；整个子系统缺失时该组可为
 *   null/缺省（按全 null 组归一化），前端如实显示「未接入」，绝不编造数值。
 * - 响应做运行时校验：schema 不对、类型漂移、结构不合法 -> ApiError(code=invalid_response)，
 *   上层展示错误 + 重试，不把畸形响应当成成功数据。
 * - 401 走共享 ApiClient 的统一处理（会话 store 清态并跳登录）；本模块不认识会话。
 * - 纯函数（校验/派生）不依赖 Vue / DOM，Node 单测可直接覆盖；组件只消费派生结果。
 */
import { ApiClient, ApiError } from './client.ts'
import type { StatusKind } from '../types/ui.ts'

/** 概览页聚合端点的 schema 标识（PLAN §4.4）。 */
export const STATUS_SCHEMA = 'khaen-status/v1'

/** 单次状态请求超时（毫秒）：轮询 5s，超时需明显短于间隔避免堆积。 */
export const STATUS_FETCH_TIMEOUT_MS = 8000

/** host_link.status_age_ms 超过该值视为「状态过期」（任务 T35b：> 2000ms 黄色 stale）。 */
export const STALE_STATUS_AGE_MS = 2000

/** 欠载率分级阈值（欠载次数 / 输出块数）：<= 0.01% 正常，<= 0.1% 警告，更高危险。 */
export const UNDERRUN_WARN_RATE = 0.0001
export const UNDERRUN_BAD_RATE = 0.001

export interface StatusDaemon {
  version: string | null
  uptime_s: number | null
  bind: string | null
}

export interface StatusHostLink {
  connected: boolean | null
  since: string | null
  socket: string | null
  status_age_ms: number | null
}

export interface StatusRunState {
  value: string | null
  muted: boolean | null
  safe: boolean | null
  last_error: string | null
}

export interface StatusPlan {
  graph_id: string | null
  sha256: string | null
  version: number | null
  latency_samples: number | null
  applied_at: string | null
  source: string | null
}

export interface StatusAes67 {
  channels: number | null
  map: number[] | null
  pt: number | null
  group: string | null
  port: number | null
}

export interface StatusFormat {
  plan_rate: number | null
  device_rate: number | null
  rate_asserted: boolean | null
  block_frames: number | null
  channels: number | null
  encoding: string | null
  aes67: StatusAes67
}

export interface StatusInput {
  packets: number | null
  underruns: number | null
  jitter_fill: number | null
  ssrc: number | null
  queue_full_drops: number | null
  recv_errors: number | null
}

export interface StatusOutput {
  blocks: number | null
  xruns: number | null
  out_errors: number | null
  bytes_written: number | null
}

export interface StatusDevice {
  id: string | null
  card: string | null
  state: string | null
  rate_readback: number | null
}

export interface StatusCalibration {
  active_job: string | null
  last_job_id: string | null
  last_verdict: string | null
}

/** not_wired 列表项：daemon 如实上报的未接入能力。 */
export interface StatusNotWiredEntry {
  path: string
  reason: string
}

export interface StatusResponse {
  schema: typeof STATUS_SCHEMA
  daemon: StatusDaemon
  host_link: StatusHostLink
  state: StatusRunState
  plan: StatusPlan
  format: StatusFormat
  input: StatusInput
  output: StatusOutput
  device: StatusDevice
  calibration: StatusCalibration
  not_wired: StatusNotWiredEntry[]
  /** 字段名（与契约字段一致，如 "device_rate"）-> 缺失原因。 */
  reasons: Record<string, string>
}

/* ------------------------------------------------------------------ 运行时校验 */

function invalidResponse(detail: string): ApiError {
  return new ApiError({
    status: 200,
    code: 'invalid_response',
    message: 'unexpected response from /api/status: ' + detail
  })
}

function asRecord(value: unknown, path: string): Record<string, unknown> | null {
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'object' || Array.isArray(value)) {
    throw invalidResponse(path + ' must be an object or null')
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

/** 组缺失 / null 时归一化为「全 null」组：整块子系统未接入是合法的缺失形态。 */
function group(value: unknown, path: string): Record<string, unknown> {
  return asRecord(value, path) ?? {}
}

function parseDaemon(value: unknown): StatusDaemon {
  const record = group(value, 'daemon')
  return {
    version: readString(record, 'version', 'daemon'),
    uptime_s: readNumber(record, 'uptime_s', 'daemon'),
    bind: readString(record, 'bind', 'daemon')
  }
}

function parseHostLink(value: unknown): StatusHostLink {
  const record = group(value, 'host_link')
  return {
    connected: readBoolean(record, 'connected', 'host_link'),
    since: readString(record, 'since', 'host_link'),
    socket: readString(record, 'socket', 'host_link'),
    status_age_ms: readNumber(record, 'status_age_ms', 'host_link')
  }
}

function parseState(value: unknown): StatusRunState {
  const record = group(value, 'state')
  return {
    value: readString(record, 'value', 'state'),
    muted: readBoolean(record, 'muted', 'state'),
    safe: readBoolean(record, 'safe', 'state'),
    last_error: readString(record, 'last_error', 'state')
  }
}

function parsePlan(value: unknown): StatusPlan {
  const record = group(value, 'plan')
  return {
    graph_id: readString(record, 'graph_id', 'plan'),
    sha256: readString(record, 'sha256', 'plan'),
    version: readNumber(record, 'version', 'plan'),
    latency_samples: readNumber(record, 'latency_samples', 'plan'),
    applied_at: readString(record, 'applied_at', 'plan'),
    source: readString(record, 'source', 'plan')
  }
}

function parseAes67(value: unknown): StatusAes67 {
  const record = group(value, 'format.aes67')
  return {
    channels: readNumber(record, 'channels', 'format.aes67'),
    map: readNumberArray(record, 'map', 'format.aes67'),
    pt: readNumber(record, 'pt', 'format.aes67'),
    group: readString(record, 'group', 'format.aes67'),
    port: readNumber(record, 'port', 'format.aes67')
  }
}

function parseFormat(value: unknown): StatusFormat {
  const record = group(value, 'format')
  return {
    plan_rate: readNumber(record, 'plan_rate', 'format'),
    device_rate: readNumber(record, 'device_rate', 'format'),
    rate_asserted: readBoolean(record, 'rate_asserted', 'format'),
    block_frames: readNumber(record, 'block_frames', 'format'),
    channels: readNumber(record, 'channels', 'format'),
    encoding: readString(record, 'encoding', 'format'),
    aes67: parseAes67(record.aes67)
  }
}

function parseInput(value: unknown): StatusInput {
  const record = group(value, 'input')
  return {
    packets: readNumber(record, 'packets', 'input'),
    underruns: readNumber(record, 'underruns', 'input'),
    jitter_fill: readNumber(record, 'jitter_fill', 'input'),
    ssrc: readNumber(record, 'ssrc', 'input'),
    queue_full_drops: readNumber(record, 'queue_full_drops', 'input'),
    recv_errors: readNumber(record, 'recv_errors', 'input')
  }
}

function parseOutput(value: unknown): StatusOutput {
  const record = group(value, 'output')
  return {
    blocks: readNumber(record, 'blocks', 'output'),
    xruns: readNumber(record, 'xruns', 'output'),
    out_errors: readNumber(record, 'out_errors', 'output'),
    bytes_written: readNumber(record, 'bytes_written', 'output')
  }
}

function parseDevice(value: unknown): StatusDevice {
  const record = group(value, 'device')
  return {
    id: readString(record, 'id', 'device'),
    card: readString(record, 'card', 'device'),
    state: readString(record, 'state', 'device'),
    rate_readback: readNumber(record, 'rate_readback', 'device')
  }
}

function parseCalibration(value: unknown): StatusCalibration {
  const record = group(value, 'calibration')
  return {
    active_job: readString(record, 'active_job', 'calibration'),
    last_job_id: readString(record, 'last_job_id', 'calibration'),
    last_verdict: readString(record, 'last_verdict', 'calibration')
  }
}

function parseNotWired(value: unknown): StatusNotWiredEntry[] {
  if (value === null || value === undefined) {
    return []
  }
  if (!Array.isArray(value)) {
    throw invalidResponse('not_wired must be an array or null')
  }
  return value.map((item, index) => {
    const path = 'not_wired[' + String(index) + ']'
    const record = asRecord(item, path)
    if (record === null) {
      throw invalidResponse(path + ' must be an object')
    }
    const entryPath = readString(record, 'path', path)
    const reason = readString(record, 'reason', path)
    if (entryPath === null || reason === null) {
      throw invalidResponse(path + ' must have string path and reason')
    }
    return { path: entryPath, reason }
  })
}

function parseReasons(value: unknown): Record<string, string> {
  if (value === null || value === undefined) {
    return {}
  }
  const record = asRecord(value, 'reasons')
  if (record === null) {
    return {}
  }
  const result: Record<string, string> = {}
  for (const [key, reason] of Object.entries(record)) {
    if (typeof reason !== 'string') {
      throw invalidResponse('reasons.' + key + ' must be a string')
    }
    result[key] = reason
  }
  return result
}

/**
 * 校验並归一化 /api/status 响应体。
 * 不合法时抛 ApiError(code=invalid_response)；合法时返回全字段类型化对象。
 */
export function parseStatusResponse(body: unknown): StatusResponse {
  const root = asRecord(body, 'body')
  if (root === null) {
    throw invalidResponse('body must be an object')
  }
  if (root.schema !== STATUS_SCHEMA) {
    throw invalidResponse('schema must be "' + STATUS_SCHEMA + '"')
  }
  return {
    schema: STATUS_SCHEMA,
    daemon: parseDaemon(root.daemon),
    host_link: parseHostLink(root.host_link),
    state: parseState(root.state),
    plan: parsePlan(root.plan),
    format: parseFormat(root.format),
    input: parseInput(root.input),
    output: parseOutput(root.output),
    device: parseDevice(root.device),
    calibration: parseCalibration(root.calibration),
    not_wired: parseNotWired(root.not_wired),
    reasons: parseReasons(root.reasons)
  }
}

/* ---------------------------------------------------------------------- 端点 */

export interface StatusFetchOptions {
  /** 取消信号（页面隐藏/离开时中止在途请求）。 */
  signal?: AbortSignal
}

export interface StatusApi {
  fetchStatus(options?: StatusFetchOptions): Promise<StatusResponse>
}

/** 用共享 ApiClient 构造状态 API（单测注入 fetchImpl 即可脱离网络）。 */
export function createStatusApi(client: ApiClient): StatusApi {
  return {
    async fetchStatus(options: StatusFetchOptions = {}): Promise<StatusResponse> {
      // 401 交给 ApiClient 的 onUnauthorized（session store 清态并跳登录），这里不自行处理。
      const body = await client.get<unknown>('/api/status', {
        timeoutMs: STATUS_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseStatusResponse(body)
    }
  }
}

/* ------------------------------------------------------------------ 派生逻辑 */

/** 状态徽标种类：faulted > safe > muted > running > unknown（与 PLAN §11.5 一致）。 */
export function runStateKind(state: StatusRunState | null | undefined): StatusKind {
  if (!state) {
    return 'unknown'
  }
  const value = typeof state.value === 'string' ? state.value.toLowerCase() : ''
  if (value === 'faulted' || value === 'failed' || value === 'error') {
    return 'faulted'
  }
  if (value === 'safe') {
    return 'safe'
  }
  if (state.muted === true) {
    return 'muted'
  }
  if (value === 'running') {
    return 'running'
  }
  return 'unknown'
}

/** 运行状态文案 i18n 键；未知的 daemon 原始值返回 null（页面显示原文）。 */
export function runStateLabelKey(state: StatusRunState | null | undefined): string | null {
  switch (runStateKind(state)) {
    case 'running':
      return 'status.running'
    case 'faulted':
      return 'status.faulted'
    case 'muted':
      return 'status.muted'
    case 'safe':
      return 'status.safe'
    default:
      return null
  }
}

/**
 * 是否过期：host_link.connected === false 或 status_age_ms > staleAfterMs。
 * 无数据时不判过期（页面显示「未接入」，不是 stale）。
 */
export function isStatusStale(
  status: StatusResponse | null | undefined,
  staleAfterMs: number = STALE_STATUS_AGE_MS
): boolean {
  if (!status) {
    return false
  }
  const link = status.host_link
  if (link.connected === false) {
    return true
  }
  return link.status_age_ms !== null && link.status_age_ms > staleAfterMs
}

/** 读取字段缺失原因（reasons[field]）；不存在返回 null。 */
export function reasonFor(status: StatusResponse | null | undefined, field: string): string | null {
  if (!status) {
    return null
  }
  const reason = status.reasons[field]
  return typeof reason === 'string' && reason.length > 0 ? reason : null
}

/**
 * 读取嵌套字段的缺失原因：先按完整路径（"format.device_rate"），
 * 再回退到契约示例里的扁平字段名（"device_rate"）。
 */
export function reasonForPath(status: StatusResponse | null | undefined, path: string): string | null {
  const direct = reasonFor(status, path)
  if (direct !== null) {
    return direct
  }
  const dot = path.lastIndexOf('.')
  return dot >= 0 ? reasonFor(status, path.slice(dot + 1)) : null
}

/** 任一值非 null（用于卡片级 TruthTag：有真实数据 = actual，全 null = notWired）。 */
export function hasAnyValue(values: readonly unknown[]): boolean {
  return values.some((value) => value !== null && value !== undefined)
}

/** 欠载率 = underruns / blocks（0..1）；无法计算（缺值 / blocks<=0 / 负值）返回 null。 */
export function underrunRate(
  input: StatusInput | null | undefined,
  output: StatusOutput | null | undefined
): number | null {
  const underruns = input ? input.underruns : null
  const blocks = output ? output.blocks : null
  if (underruns === null || blocks === null || blocks <= 0 || underruns < 0) {
    return null
  }
  return underruns / blocks
}

export type RateSeverity = 'ok' | 'warn' | 'bad'

/** 欠载率分级（阈值见 UNDERRUN_WARN_RATE / UNDERRUN_BAD_RATE）；null 无法分级。 */
export function underrunSeverity(rate: number | null | undefined): RateSeverity | null {
  if (rate === null || rate === undefined || !Number.isFinite(rate)) {
    return null
  }
  if (rate <= UNDERRUN_WARN_RATE) {
    return 'ok'
  }
  return rate <= UNDERRUN_BAD_RATE ? 'warn' : 'bad'
}

export interface UptimeParts {
  days: number
  hours: number
  minutes: number
  seconds: number
}

/** 秒 -> 天/时/分/秒（向下取整）；null/非有限/负值返回 null。 */
export function uptimeParts(totalSeconds: number | null | undefined): UptimeParts | null {
  if (totalSeconds === null || totalSeconds === undefined || !Number.isFinite(totalSeconds) || totalSeconds < 0) {
    return null
  }
  const whole = Math.floor(totalSeconds)
  return {
    days: Math.floor(whole / 86400),
    hours: Math.floor((whole % 86400) / 3600),
    minutes: Math.floor((whole % 3600) / 60),
    seconds: whole % 60
  }
}

/** 通道映射 [0,1] -> "0,1"；null 返回 null。 */
export function formatChannelMap(map: readonly number[] | null | undefined): string | null {
  if (!map || map.length === 0) {
    return null
  }
  return map.join(',')
}

/* ---------------------------------------------------------------- 错误归类 */

export type StatusErrorCategory =
  | 'network'
  | 'timeout'
  | 'unauthorized'
  | 'server'
  | 'invalid_response'
  | 'unknown'

export interface StatusErrorInfo {
  category: StatusErrorCategory
  /** HTTP 状态码；0 = 网络层失败。 */
  status: number
  /** 原始稳定码（ApiError.code，或未知错误的 unknown_error）。 */
  code: string
  /** 主文案 i18n 键（zh-CN / en-US 对等）。 */
  i18nKey: string
  /** 后端 message / 异常消息（仅详情展示）。 */
  detail?: string
  /** 是否值得重试（401 不重试：会话失效应去登录）。 */
  retryable: boolean
}

const STATUS_ERROR_KEYS: Record<StatusErrorCategory, string> = {
  network: 'error.status.network',
  timeout: 'error.status.timeout',
  unauthorized: 'error.status.unauthorized',
  server: 'error.status.server',
  invalid_response: 'error.status.invalidResponse',
  unknown: 'error.status.unknown'
}

/** 把 /api/status 失败归类为 UI 可分支的稳定类别 + 本地化文案键。 */
export function classifyStatusError(error: unknown): StatusErrorInfo {
  if (!(error instanceof ApiError)) {
    const detail = error instanceof Error ? error.message : undefined
    return {
      category: 'unknown',
      status: 0,
      code: 'unknown_error',
      i18nKey: STATUS_ERROR_KEYS.unknown,
      ...(detail ? { detail } : {}),
      retryable: true
    }
  }
  let category: StatusErrorCategory
  if (error.code === 'request_timeout') {
    category = 'timeout'
  } else if (error.code === 'network_error' || error.status === 0) {
    category = 'network'
  } else if (error.status === 401) {
    category = 'unauthorized'
  } else if (error.code === 'invalid_response') {
    category = 'invalid_response'
  } else if (error.status >= 500) {
    category = 'server'
  } else {
    category = 'unknown'
  }
  const detail = error.message.length > 0 ? error.message : undefined
  return {
    category,
    status: error.status,
    code: error.code,
    i18nKey: STATUS_ERROR_KEYS[category],
    ...(detail ? { detail } : {}),
    retryable: category !== 'unauthorized'
  }
}
