/**
 * /api/system/* —— 系统信息页唯一数据源（任务 T47；后端 T46 并行实现，本模块按契约 + 契约 mock 开发）。
 *
 * 端点（全部需要会话；写操作由共享 ApiClient 自动带 X-KHAEN-CSRF）：
 * - GET  /api/system/info      版本/构建、主机、服务（active/enabled/pid）、设备、控制通道、重启能力
 * - GET  /api/system/logs      ?unit=daemon|rt-host&lines=N（N ∈ [1,500]）journalctl 最近 N 行
 * - GET  /api/system/diagnostics 合并 status+info+设备+日志摘要的 JSON 下载（响应头带文件名）
 * - POST /api/system/password  {old,new}（登录 + CSRF；成功后会话失效，前端重新登录）
 * - POST /api/system/restart   {target:"rt-host"}（仅当 info.restart.allowed=true 时可用；二次确认在前端）
 * - GET  /healthz              匿名健康检查；version = **release 版本**（缺失时如实回退 crate 版本）
 *
 * 纪律（与 /api/status 一致）：
 * - 缺失值一律 null，原因放 reasons（字段路径 -> 原因）；整个子系统缺失时该组可为 null/缺省（归一化为空组），
 *   前端如实显示「未接入」，绝不编造数值/假构建信息。
 * - 响应做运行时校验：类型漂移 / 结构不合法 -> ApiError(code=invalid_response)，上层展示错误并允许重试。
 * - 版本展示必须区分 **crate_version（编译进二进制的 crate 版本，即 /healthz 曾经错误上报的值）**、
 *   **release_version（部署发布版本）**、**commit（构建 commit）**；crate ≠ release 时页面必须醒目提示。
 * - 401 走共享 ApiClient 的统一会话处理；改密端点用 auth:'probe'（401 可能是旧密码错误，不能一律跳登录）。
 * - 纯函数（校验/派生）不依赖 Vue / DOM，Node 单测可直接覆盖；组件只消费派生结果。
 */
import { ApiError } from './client.ts'
import type { ApiClient } from './client.ts'

/** 系统信息端点 schema（T46 若暂未带 schema，前端按字段校验并容忍缺省）。 */
export const SYSTEM_INFO_SCHEMA = 'khaen-system-info/v1'
/** 日志端点 schema。 */
export const SYSTEM_LOGS_SCHEMA = 'khaen-system-logs/v1'
/** 诊断包 schema（JSON 下载内容）。 */
export const SYSTEM_DIAGNOSTICS_SCHEMA = 'khaen-diagnostics/v1'

/** info 请求超时（毫秒）：系统页手动刷新，不宜无限等待。 */
export const SYSTEM_INFO_FETCH_TIMEOUT_MS = 8000
/** 日志请求超时（毫秒）。 */
export const SYSTEM_LOGS_FETCH_TIMEOUT_MS = 8000

/** 日志单元（与后端 journalctl -u 对应）。 */
export const LOG_UNITS = ['daemon', 'rt-host'] as const
export type LogUnit = (typeof LOG_UNITS)[number]

/** 日志行数可选项（后端上限 500）。 */
export const LOG_LINE_OPTIONS = [100, 200, 500] as const
export const DEFAULT_LOG_LINES = 200
export const MAX_LOG_LINES = 500

/** 允许重启的目标（daemon 自身重启不实现，后端 501 + reason）。 */
export const RESTART_TARGET = 'rt-host'

/** 服务状态：active/enabled/pid 都可能为 null（systemctl 不可用）+ reason。 */
export interface SystemServiceState {
  active: boolean | null
  enabled: boolean | null
  pid: number | null
  reason: string | null
}

export interface SystemServices {
  daemon: SystemServiceState
  rt_host: SystemServiceState
}

/** /proc/asound 设备项（MOTU 由前端标出，不臆造设备）。 */
export interface SystemDevice {
  card_id: string | null
  driver: string | null
  model: string | null
  channels: number | null
  rates: number[] | null
}

/** 控制通道（daemon → rt-host），字段语义同 /api/status 的 host_link。 */
export interface SystemHostLink {
  connected: boolean | null
  since: string | null
  socket: string | null
  status_age_ms: number | null
}

/** 重启能力：allowed=false 时前端不显示重启按钮（只显示 NOT_AVAILABLE + reason）。 */
export interface SystemRestartCapability {
  allowed: boolean
  reason: string | null
  target: string | null
  last_restarted_at: string | null
}

/** GET /api/system/info 响应。 */
export interface SystemInfo {
  schema: string
  /** 编译进二进制的 crate 版本（/healthz 历史上错误上报的值）。 */
  crate_version: string | null
  /** 部署 release 版本（如 v0.1.2）；非 release 部署为 null + reason。 */
  release_version: string | null
  /** 构建 commit（短/长 sha 原样）。 */
  commit: string | null
  build_profile: string | null
  rustc: string | null
  target: string | null
  /** daemon 监听地址。 */
  bind: string | null
  state_dir: string | null
  rt_status_file: string | null
  uptime_s: number | null
  hostname: string | null
  kernel: string | null
  os: string | null
  services: SystemServices
  devices: SystemDevice[]
  host_link: SystemHostLink
  restart: SystemRestartCapability
  /** 字段路径 -> 缺失原因。 */
  reasons: Record<string, string>
}

/** GET /api/system/logs 响应。 */
export interface SystemLogs {
  schema: string
  unit: LogUnit
  unit_name: string | null
  requested_lines: number | null
  /** 原始日志行（journalctl --no-pager，按时间序，最后一行最新）。 */
  lines: string[]
  truncated: boolean | null
  source: string | null
  reason: string | null
}

/** GET /api/system/diagnostics 下载结果。 */
export interface SystemDiagnosticsDownload {
  /** 服务端建议文件名（Content-Disposition；没有则为 null，由前端生成带时间戳的回退名）。 */
  filename: string | null
  /** 原始 JSON 文本（直接落盘，不重排）。 */
  text: string
  /** 解析后的 JSON（为空/非对象时为 null；不阻断下载）。 */
  json: unknown
}

/** POST /api/system/restart 响应。 */
export interface SystemRestartResult {
  ok: boolean
  target: string | null
  restarted: boolean
  restarted_at: string | null
  reason: string | null
}

/** GET /healthz 响应（匿名；version = release 版本）。 */
export interface HealthzInfo {
  ok: boolean
  version: string | null
  uptime_s: number | null
}

/* ------------------------------------------------------------------ 运行时校验 */

function invalidResponse(detail: string): ApiError {
  return new ApiError({
    status: 200,
    code: 'invalid_response',
    message: 'unexpected response from /api/system: ' + detail
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

function readStringArray(record: Record<string, unknown>, field: string, path: string): string[] | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
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

function group(value: unknown, path: string): Record<string, unknown> {
  return asRecord(value, path) ?? {}
}

/** schema 可选（T46 尚可补），存在时必须匹配；不匹配视为契约漂移。 */
function checkSchema(record: Record<string, unknown>, expected: string): void {
  const value = record.schema
  if (value === null || value === undefined) {
    return
  }
  if (typeof value !== 'string' || value !== expected) {
    throw invalidResponse('schema must be "' + expected + '" (got ' + JSON.stringify(value) + ')')
  }
}

function readReasons(value: unknown): Record<string, string> {
  const record = asRecord(value, 'reasons')
  if (record === null) {
    return {}
  }
  const reasons: Record<string, string> = {}
  for (const [key, item] of Object.entries(record)) {
    if (typeof item !== 'string') {
      throw invalidResponse('reasons.' + key + ' must be a string')
    }
    reasons[key] = item
  }
  return reasons
}

function parseService(value: unknown, path: string): SystemServiceState {
  const record = group(value, path)
  return {
    active: readBoolean(record, 'active', path),
    enabled: readBoolean(record, 'enabled', path),
    pid: readNumber(record, 'pid', path),
    reason: readString(record, 'reason', path)
  }
}

function parseServices(value: unknown): SystemServices {
  const record = group(value, 'services')
  // 后端字段命名兼容 rt_host（JSON 惯例）与 "rt-host"（unit 名）两种写法。
  const rtHostValue = record.rt_host ?? record['rt-host']
  return {
    daemon: parseService(record.daemon, 'services.daemon'),
    rt_host: parseService(rtHostValue, 'services.rt_host')
  }
}

function parseDevice(value: unknown, index: number): SystemDevice {
  const path = 'devices[' + String(index) + ']'
  const record = asRecord(value, path)
  if (record === null) {
    throw invalidResponse(path + ' must be an object')
  }
  return {
    card_id: readString(record, 'card_id', path),
    driver: readString(record, 'driver', path),
    model: readString(record, 'model', path),
    channels: readNumber(record, 'channels', path),
    rates: readNumberArray(record, 'rates', path)
  }
}

function parseDevices(value: unknown): SystemDevice[] {
  if (value === null || value === undefined) {
    return []
  }
  if (!Array.isArray(value)) {
    throw invalidResponse('devices must be an array or null')
  }
  return value.map((item, index) => parseDevice(item, index))
}

function parseHostLink(value: unknown): SystemHostLink {
  const record = group(value, 'host_link')
  return {
    connected: readBoolean(record, 'connected', 'host_link'),
    since: readString(record, 'since', 'host_link'),
    socket: readString(record, 'socket', 'host_link'),
    status_age_ms: readNumber(record, 'status_age_ms', 'host_link')
  }
}

function parseRestart(value: unknown): SystemRestartCapability {
  const record = group(value, 'restart')
  const allowed = readBoolean(record, 'allowed', 'restart')
  const target = readString(record, 'target', 'restart')
  return {
    // 缺失能力一律按「不允许」处理（fail closed：不显示危险操作）。
    allowed: allowed === true,
    reason: readString(record, 'reason', 'restart'),
    target: target ?? (allowed === true ? RESTART_TARGET : null),
    last_restarted_at: readString(record, 'last_restarted_at', 'restart')
  }
}

/** 解析 /api/system/info；结构不合法一律 invalid_response（不把畸形响应当成功）。 */
export function parseSystemInfo(body: unknown): SystemInfo {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('body must be a JSON object')
  }
  checkSchema(record, SYSTEM_INFO_SCHEMA)
  return {
    schema: SYSTEM_INFO_SCHEMA,
    crate_version: readString(record, 'crate_version', 'system'),
    release_version: readString(record, 'release_version', 'system'),
    commit: readString(record, 'commit', 'system'),
    build_profile: readString(record, 'build_profile', 'system'),
    rustc: readString(record, 'rustc', 'system'),
    target: readString(record, 'target', 'system'),
    bind: readString(record, 'bind', 'system'),
    state_dir: readString(record, 'state_dir', 'system'),
    rt_status_file: readString(record, 'rt_status_file', 'system'),
    uptime_s: readNumber(record, 'uptime_s', 'system'),
    hostname: readString(record, 'hostname', 'system'),
    kernel: readString(record, 'kernel', 'system'),
    os: readString(record, 'os', 'system'),
    services: parseServices(record.services),
    devices: parseDevices(record.devices),
    host_link: parseHostLink(record.host_link),
    restart: parseRestart(record.restart),
    reasons: readReasons(record.reasons)
  }
}

/** 解析 /api/system/logs；兼容 lines: string[]（契约）与 text: string（整段文本）两种诚实回传。 */
export function parseSystemLogs(body: unknown): SystemLogs {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('body must be a JSON object')
  }
  checkSchema(record, SYSTEM_LOGS_SCHEMA)
  const unitValue = record.unit
  let unit: LogUnit
  if (unitValue === undefined || unitValue === null) {
    unit = 'daemon'
  } else if (isLogUnit(unitValue)) {
    unit = unitValue
  } else {
    throw invalidResponse('unit must be "daemon" or "rt-host"')
  }
  let lines = readStringArray(record, 'lines', 'logs')
  if (lines === null) {
    const text = record.text
    if (typeof text === 'string') {
      lines = text.length === 0 ? [] : text.split(/\r?\n/)
    } else {
      throw invalidResponse('logs.lines must be an array of strings')
    }
  }
  return {
    schema: SYSTEM_LOGS_SCHEMA,
    unit,
    unit_name: readString(record, 'unit_name', 'logs'),
    requested_lines: readNumber(record, 'requested_lines', 'logs'),
    lines,
    truncated: readBoolean(record, 'truncated', 'logs'),
    source: readString(record, 'source', 'logs'),
    reason: readString(record, 'reason', 'logs')
  }
}

/** 解析 POST /api/system/restart 响应（ok/restarted 两种回执字段都接受，至少一个为 true）。 */
export function parseRestartResult(body: unknown): SystemRestartResult {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('body must be a JSON object')
  }
  const ok = readBoolean(record, 'ok', 'restart')
  const restarted = readBoolean(record, 'restarted', 'restart')
  if (ok !== true && restarted !== true) {
    throw invalidResponse('restart response must carry ok:true or restarted:true')
  }
  return {
    ok: true,
    target: readString(record, 'target', 'restart'),
    restarted: restarted === true,
    restarted_at: readString(record, 'restarted_at', 'restart'),
    reason: readString(record, 'reason', 'restart')
  }
}

/** 解析 GET /healthz（匿名版本端点）；version 缺失为 null（不冒充版本）。 */
export function parseHealthz(body: unknown): HealthzInfo {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('healthz body must be a JSON object')
  }
  const ok = readBoolean(record, 'ok', 'healthz')
  return {
    ok: ok === true,
    version: readString(record, 'version', 'healthz'),
    uptime_s: readNumber(record, 'uptime_s', 'healthz')
  }
}

/* -------------------------------------------------------------- Content-Disposition */

/**
 * 从 Content-Disposition 解析下载文件名（RFC 6266 的 filename / filename* 都支持）。
 * 无文件名 / 非法值返回 null（由调用方生成回退名，不编造服务端文件名）。
 */
export function parseContentDispositionFilename(header: string | null | undefined): string | null {
  if (typeof header !== 'string' || header.length === 0) {
    return null
  }
  const extended = /filename\*=UTF-8''([^;]+)/i.exec(header)
  if (extended !== null) {
    try {
      const decoded = decodeURIComponent(extended[1].trim())
      return decoded.length > 0 ? decoded : null
    } catch {
      return null
    }
  }
  const quoted = /filename="([^"]*)"/i.exec(header)
  if (quoted !== null) {
    return quoted[1].trim().length > 0 ? quoted[1].trim() : null
  }
  const bare = /filename=([^;]+)/i.exec(header)
  if (bare !== null) {
    const value = bare[1].trim()
    return value.length > 0 ? value : null
  }
  return null
}

/**
 * 诊断下载的回退文件名（服务端未给 Content-Disposition 时使用）：
 * khaen-diagnostics-v<version>-<UTC 时间>.json；版本未知用 unknown（不冒充）。
 */
export function diagnosticsFallbackFilename(releaseVersion: string | null, nowMs: number = Date.now()): string {
  const version = normalizeVersion(releaseVersion) ?? 'unknown'
  const stamp = new Date(nowMs).toISOString().replace(/[-:]/g, '').replace(/\.\d+Z$/, 'Z')
  return 'khaen-diagnostics-v' + version + '-' + stamp + '.json'
}

/* ------------------------------------------------------------------ 版本派生 */

/** 归一化版本号：去空白与一个前导 v/V；空/非字符串 -> null。 */
export function normalizeVersion(value: string | null | undefined): string | null {
  if (typeof value !== 'string') {
    return null
  }
  const trimmed = value.trim()
  if (trimmed.length === 0) {
    return null
  }
  return trimmed.replace(/^[vV]/, '')
}

/** 统一展示格式（带 v 前缀）：'0.1.2' / 'v0.1.2' -> 'v0.1.2'；无法解析 -> null。 */
export function formatVersionLabel(value: string | null | undefined): string | null {
  const normalized = normalizeVersion(value)
  return normalized === null ? null : 'v' + normalized
}

/** crate 版本与 release 版本的一致性（任一缺失 -> unknown；页面不再编造「一致」）。 */
export type VersionConsistency = 'consistent' | 'mismatch' | 'unknown'

export function versionConsistency(
  crateVersion: string | null | undefined,
  releaseVersion: string | null | undefined
): VersionConsistency {
  const crate = normalizeVersion(crateVersion)
  const release = normalizeVersion(releaseVersion)
  if (crate === null || release === null) {
    return 'unknown'
  }
  return crate === release ? 'consistent' : 'mismatch'
}

/* ------------------------------------------------------------------ 设备/能力派生 */

const MOTU_PATTERN = /motu|ultralite|ultra[\s_-]*lite/i

/**
 * 是否 MOTU 设备（任务 T47：设备列表标出 MOTU）。
 * 只做标签判定（card_id/model/driver 任一命中），不代表设备已就绪——就绪状态看 host_link/服务。
 */
export function isMotuDevice(device: Pick<SystemDevice, 'card_id' | 'model' | 'driver'>): boolean {
  return [device.card_id, device.model, device.driver].some(
    (value) => typeof value === 'string' && MOTU_PATTERN.test(value)
  )
}

/** 重启 rt-host 是否可用：能力允许 + 目标匹配（缺省 target 视为 rt-host）。 */
export function isRestartAvailable(info: Pick<SystemInfo, 'restart'> | null): boolean {
  if (info === null || info.restart.allowed !== true) {
    return false
  }
  const target = info.restart.target
  return target === null || target === RESTART_TARGET
}

/** 重启不可用原因（后端 reason 优先，缺失给前端兜底文案键）。 */
export function restartUnavailableReason(info: Pick<SystemInfo, 'restart'> | null): string | null {
  if (info === null) {
    return null
  }
  return info.restart.reason
}

/** 字段缺失原因查询：先查完整路径，再回退末段字段名（与 /api/status 的 reasons 约定一致）。 */
export function reasonForSystemPath(info: Pick<SystemInfo, 'reasons'> | null, path: string): string | null {
  if (info === null) {
    return null
  }
  const reasons = info.reasons
  if (typeof reasons[path] === 'string') {
    return reasons[path]
  }
  const lastDot = path.lastIndexOf('.')
  if (lastDot >= 0) {
    const tail = path.slice(lastDot + 1)
    if (typeof reasons[tail] === 'string') {
      return reasons[tail]
    }
  }
  return null
}

/* ------------------------------------------------------------------ 密码表单校验 */

export type PasswordChangeValidationKey =
  | 'system.password.validation.required'
  | 'system.password.validation.tooShort'
  | 'system.password.validation.mismatch'

export type PasswordChangeValidationResult = { ok: true } | { ok: false; key: PasswordChangeValidationKey }

/**
 * 修改密码表单本地校验（权威校验仍在后端；这里只做提交前快速反馈）。
 * @param oldPassword 当前密码（必填）
 * @param newPassword 新密码（>= minLength）
 * @param confirm     再次输入的新密码（必须一致）
 */
export function validatePasswordChange(
  oldPassword: string,
  newPassword: string,
  confirm: string,
  minLength: number = 8
): PasswordChangeValidationResult {
  if (oldPassword.length === 0) {
    return { ok: false, key: 'system.password.validation.required' }
  }
  const min = Number.isFinite(minLength) && minLength > 0 ? Math.floor(minLength) : 1
  if (newPassword.length === 0 || newPassword.length < min) {
    return { ok: false, key: 'system.password.validation.tooShort' }
  }
  if (newPassword !== confirm) {
    return { ok: false, key: 'system.password.validation.mismatch' }
  }
  return { ok: true }
}

/* ------------------------------------------------------------------ 错误归类 */

export type SystemErrorCategory =
  | 'network'
  | 'timeout'
  | 'unauthorized'
  | 'invalid_response'
  | 'disabled'
  | 'unavailable'
  | 'validation'
  | 'server'
  | 'unknown'

export interface SystemErrorInfo {
  category: SystemErrorCategory
  /** 主文案 i18n 键（两种语言都有）。 */
  i18nKey: string
  /** 是否值得重试（网络/超时/服务器错误）。 */
  retryable: boolean
  /** 后端稳定错误码（原始）。 */
  code: string
  /** 后端 message（仅详情展示）。 */
  detail: string | null
  /** 503/403 时后端给出的具体原因（details.reason）。 */
  reason: string | null
}

const SYSTEM_ERROR_KEYS: Record<SystemErrorCategory, string> = {
  network: 'error.system.network',
  timeout: 'error.system.timeout',
  unauthorized: 'error.system.unauthorized',
  invalid_response: 'error.system.invalidResponse',
  disabled: 'error.system.disabled',
  unavailable: 'error.system.unavailable',
  validation: 'error.system.validation',
  server: 'error.system.server',
  unknown: 'error.system.unknown'
}

const UNAVAILABLE_CODES = new Set(['logs_unavailable', 'not_wired', 'not_implemented', 'unavailable'])
const DISABLED_CODES = new Set(['restart_disabled', 'forbidden', 'permission_denied'])
const VALIDATION_CODES = new Set([
  'invalid_request',
  'invalid_password',
  'invalid_credentials',
  'password_mismatch',
  'validation_failed'
])

function readDetailReason(details: unknown): string | null {
  if (typeof details !== 'object' || details === null || Array.isArray(details)) {
    return null
  }
  const record = details as Record<string, unknown>
  if (typeof record.reason === 'string' && record.reason.length > 0) {
    return record.reason
  }
  if (typeof record.message === 'string' && record.message.length > 0) {
    return record.message
  }
  return null
}

/** 把任意错误归类成 UI 可分支的类别 + 本地化文案键（不改写后端 message）。 */
export function classifySystemError(error: unknown): SystemErrorInfo {
  let category: SystemErrorCategory = 'unknown'
  let code = 'unknown_error'
  let detail: string | null = null
  let reason: string | null = null

  if (error instanceof ApiError) {
    code = error.code
    detail = error.message.length > 0 ? error.message : null
    reason = readDetailReason(error.details)
    if (error.status === 0 && code === 'request_timeout') {
      category = 'timeout'
    } else if (error.status === 0) {
      category = 'network'
    } else if (error.status === 401) {
      category = 'unauthorized'
    } else if (error.status === 403 || DISABLED_CODES.has(code)) {
      category = 'disabled'
    } else if (code === 'invalid_response') {
      category = 'invalid_response'
    } else if (error.status === 503 || UNAVAILABLE_CODES.has(code)) {
      category = 'unavailable'
      if (code === 'invalid_response') {
        category = 'invalid_response'
      }
    } else if (error.status === 400 || error.status === 422 || VALIDATION_CODES.has(code)) {
      category = 'validation'
    } else if (error.status >= 500) {
      category = 'server'
    } else {
      category = 'unknown'
    }
  } else if (error instanceof Error) {
    detail = error.message
  }

  return {
    category,
    i18nKey: SYSTEM_ERROR_KEYS[category],
    retryable: category === 'network' || category === 'timeout' || category === 'server',
    code,
    detail,
    reason
  }
}

/* ------------------------------------------------------------------ 端点封装 */

/** 是否合法日志单元。 */
export function isLogUnit(value: unknown): value is LogUnit {
  return typeof value === 'string' && (LOG_UNITS as readonly string[]).includes(value)
}

/** 行数规整：非法/越界回退默认值（UI 只允许选项值；查询参数发送前再夹一次）。 */
export function normalizeLogLines(value: unknown): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return DEFAULT_LOG_LINES
  }
  const integer = Math.trunc(value)
  if (integer < 1) {
    return DEFAULT_LOG_LINES
  }
  return Math.min(integer, MAX_LOG_LINES)
}

export interface SystemApi {
  /** GET /api/system/info。 */
  fetchInfo(options?: { signal?: AbortSignal }): Promise<SystemInfo>
  /** GET /api/system/logs?unit&lines。 */
  fetchLogs(params: { unit: LogUnit; lines: number }, options?: { signal?: AbortSignal }): Promise<SystemLogs>
  /** GET /api/system/diagnostics（原始 JSON 下载）。 */
  fetchDiagnostics(): Promise<SystemDiagnosticsDownload>
  /** GET /healthz（匿名；release 版本来源）。 */
  fetchHealthz(): Promise<HealthzInfo>
  /** POST /api/system/password（登录 + CSRF；成功后旧会话失效）。 */
  changePassword(oldPassword: string, newPassword: string): Promise<void>
  /** POST /api/system/restart {target:"rt-host"}。 */
  restartRtHost(): Promise<SystemRestartResult>
}

/** 用共享 client 构造系统 API（单测注入 baseUrl/fetchImpl 即可脱离网络）。 */
export function createSystemApi(client: ApiClient): SystemApi {
  return {
    async fetchInfo(options = {}) {
      const body = await client.get<unknown>('/api/system/info', {
        signal: options.signal,
        timeoutMs: SYSTEM_INFO_FETCH_TIMEOUT_MS
      })
      return parseSystemInfo(body)
    },

    async fetchLogs(params, options = {}) {
      const body = await client.get<unknown>('/api/system/logs', {
        query: { unit: params.unit, lines: normalizeLogLines(params.lines) },
        signal: options.signal,
        timeoutMs: SYSTEM_LOGS_FETCH_TIMEOUT_MS
      })
      return parseSystemLogs(body)
    },

    async fetchDiagnostics() {
      const raw = await client.getRaw('/api/system/diagnostics', { timeoutMs: SYSTEM_INFO_FETCH_TIMEOUT_MS })
      let json: unknown = null
      if (raw.body.length > 0) {
        try {
          json = JSON.parse(raw.body)
        } catch {
          throw new ApiError({
            status: raw.status,
            code: 'invalid_response',
            message: 'diagnostics response is not valid JSON'
          })
        }
      }
      return {
        filename: parseContentDispositionFilename(raw.headers.get('Content-Disposition')),
        text: raw.body,
        json
      }
    },

    async fetchHealthz() {
      // 匿名端点：401 不触发全局跳登录（auth:'probe'），失败由版本 store 如实记录。
      const body = await client.get<unknown>('/healthz', { auth: 'probe', timeoutMs: SYSTEM_INFO_FETCH_TIMEOUT_MS })
      return parseHealthz(body)
    },

    async changePassword(oldPassword: string, newPassword: string) {
      // 401 可能是「旧密码错误」而不是会话失效：probe 模式交给调用方按上下文映射，不全局跳登录。
      await client.post<unknown>(
        '/api/system/password',
        { old: oldPassword, new: newPassword },
        { auth: 'probe' }
      )
    },

    async restartRtHost() {
      const body = await client.post<unknown>('/api/system/restart', { target: RESTART_TARGET })
      return parseRestartResult(body)
    }
  }
}
