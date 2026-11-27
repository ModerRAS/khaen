/**
 * MOTU 设备 REST 客户端（T57 后端实际实现；docs/API.md v1.1 §3 / §4.1）。
 *
 * 端点（以后端 crates/daemon/src/motu/api.rs 为准，不靠猜）：
 * - GET  /api/motu          只读设备快照（恒 200；拿不到的字段 null + reasons，不编数值）；
 * - PUT  /api/motu/volume   stage=prepare：{stage:"prepare", channel:1|2|"both", db}，只生成草稿
 *                           （writes_device=false，不写设备）；stage=commit：{stage:"commit", engine_token}，
 *                           逐通道单键写 + 设备回读（writes_device=true）。
 *   后端也接受 POST（同一 handler）；前端统一用 PUT（任务契约按 PUT 断言），单测断言 method。
 * - 该端点**没有 abort**：未提交草稿由后端 TTL 过期（PENDING_TTL_SECS=120）；UI 的“取消草稿”只丢本地
 *   状态，不假定服务端草稿已删除。
 *
 * 诚实边界（与 control.ts 同一套纪律）：
 * - 所有响应做运行时校验：ok !== true、engine_token 缺失、字段类型漂移 -> ApiError(invalid_response)，
 *   不把畸形响应当成功；GET 的 null 字段如实保留（不是 0/false）；
 * - prepare 回执 writes_device=true 视为契约漂移（prepare 绝不能写设备）；
 * - 错误按稳定码归类（503 motu_not_detected / motu_control_unreachable / motu_control_disabled，
 *   400 motu_batch_rejected / invalid_request，409 engine_token_invalid，502 E_* 设备错误），
 *   details.motu / details.completed 原样带出，绝不报告“全部成功”；
 * - 401 由共享 ApiClient 的 onUnauthorized 统一处理（会话 store 清态跳登录），这里只归类展示。
 *
 * 纯函数（解析/归类/校验）不依赖 Vue / DOM，Node 单测可直接覆盖。
 */
import { ApiClient, ApiError } from './client.ts'

/** GET /api/motu（只读设备快照）。 */
export const MOTU_STATUS_PATH = '/api/motu'
/** PUT /api/motu/volume（两阶段；后端也收 POST）。 */
export const MOTU_VOLUME_PATH = '/api/motu/volume'
/** GET /api/motu 的 schema（后端 SCHEMA_STATUS）。 */
export const MOTU_STATUS_SCHEMA = 'khaen-motu/v1'
/** prepare/commit 回执的 schema（后端 SCHEMA_VOLUME）。 */
export const MOTU_VOLUME_SCHEMA = 'khaen-motu-volume/v1'

/** 可寻址的主输出通道（Main 1 / Main 2；后端 CHANNEL_COUNT=2，不暴露其它 CueMix 总线）。 */
export const MOTU_MAIN_CHANNELS = [1, 2] as const
export type MotuMainChannel = (typeof MOTU_MAIN_CHANNELS)[number]
/** UI 的通道选择：Main 1 / Main 2 / both（与后端 channel 字段一致）。 */
export type MotuChannelSelection = MotuMainChannel | 'both'

/**
 * UI 的 dB 目标范围（前端约束，不是后端能力上限）：
 * 后端 db_to_fader 只要求有限且 <= 0 dB（1.0 线性 = 0 dB；<= -144.49 dB 量化后为静音）。
 * 这里给一个可操作的滑杆范围，避免无限小值。
 */
export const MOTU_VOLUME_DB_MIN = -144.5
export const MOTU_VOLUME_DB_MAX = 0
export const MOTU_VOLUME_DB_STEP = 0.1

/** 后端 write.status 的取值（GET 的写能力上报）。 */
export const MOTU_WRITE_STATUSES = ['wired', 'unreachable', 'not_detected', 'unknown'] as const
export type MotuWriteStatus = (typeof MOTU_WRITE_STATUSES)[number]

/** write.status 的本地化文案键（组件 t() 使用；字面量便于 i18n-check 校验）。 */
export const MOTU_WRITE_STATUS_KEYS: Record<MotuWriteStatus, string> = {
  wired: 'motu.writeStatus.wired',
  unreachable: 'motu.writeStatus.unreachable',
  not_detected: 'motu.writeStatus.notDetected',
  unknown: 'motu.writeStatus.unknown'
}

/** 后端检测原因码 -> 本地化文案键（未知码时组件回退到 reason_detail 原文）。 */
export const MOTU_DETECTION_REASON_KEYS: Record<string, string> = {
  detected: 'motu.detectReason.detected',
  no_motu_card: 'motu.detectReason.noMotuCard',
  cards_unavailable: 'motu.detectReason.cardsUnavailable',
  control_unreachable: 'motu.detectReason.controlUnreachable',
  motu_control_disabled: 'motu.detectReason.disabled'
}

/* ------------------------------------------------------------------ 数据形状 */

/** CueMix datastore 引用（e.g. {key:"koBusFader", index:0}）。 */
export interface MotuDatastoreRef {
  key: string
  index: number
}

/** 可寻址主通道（GET channels[]）。 */
export interface MotuChannel {
  channel: number
  name: string | null
  datastore: MotuDatastoreRef | null
}

/** 每通道当前音量（GET vol[]；设备真值回读，linear 0.0..=1.0）。 */
export interface MotuVolumeReadback {
  channel: number
  name: string | null
  datastore: MotuDatastoreRef | null
  linear: number | null
  db: number | null
}

/** 设备自身信息（GET state.device）。 */
export interface MotuDeviceInfo {
  model_id: string | null
  name: string | null
  version: string | null
  api_version: number | null
}

/** 设备状态细节（GET state；拿不到的字段 null，missing 如实列出）。 */
export interface MotuDeviceState {
  sample_rate_hz: number | null
  main_trim: number | null
  main_bus_mute: boolean | null
  device: MotuDeviceInfo | null
  missing: string[] | null
}

/** 读设备失败的如实回执（GET read_error = MotuError::to_json）。 */
export interface MotuReadError {
  code: string
  message: string
  retryable: boolean | null
}

/** 写能力上报（GET write；batch 恒 false，设备对批量写敏感）。 */
export interface MotuWriteCapability {
  status: MotuWriteStatus | null
  single_key: boolean | null
  readback: boolean | null
  two_phase: boolean | null
  batch: boolean | null
  max_channels_per_request: number | null
  reason: string | null
}

/** not_wired 条目（本页面专属能力缺口；后端当前恒空）。 */
export interface MotuNotWiredEntry {
  path: string | null
  reason: string | null
}

/** 设备检测对象（GET 顶层字段 / 写回执 target_device / 错误 details.detection 同形）。 */
export interface MotuDetection {
  detected: boolean | null
  model: string | null
  card_id: string | null
  card_index: number | null
  control_reachable: boolean | null
  reason: string | null
  reason_detail: string | null
}

/** daemon 按需缓存元数据（加法字段；旧响应没有它时为 null）。 */
export interface MotuCacheMeta {
  age_ms: number | null
  stale: boolean | null
  last_error: string | null
}

/** GET /api/motu 归一化快照。 */
export interface MotuStatus extends MotuDetection {
  schema: string | null
  channel_count: number | null
  channels: MotuChannel[]
  mute: boolean | null
  vol: MotuVolumeReadback[] | null
  state: MotuDeviceState | null
  read_error: MotuReadError | null
  write: MotuWriteCapability | null
  not_wired: MotuNotWiredEntry[]
  /** 路径 -> 原因（e.g. "mute"/"vol"/"state"）；字段为 null 时据此显示“未上报 + 原因”。 */
  reasons: Record<string, string>
  /** 快照缓存。瞬时读失败时字段仍是上一份成功值，stale/last_error 说明这一点。 */
  cache: MotuCacheMeta | null
}

/** 写计划（prepare/commit 回执里的 write_plan；逐通道单键写 + 回读）。 */
export interface MotuWritePlanStep {
  channel: number | null
  key: string | null
  index: number | null
  frames: number | null
}

export interface MotuWritePlan {
  kind: string | null
  batch: boolean | null
  writes_per_channel: number | null
  writes: number | null
  steps: MotuWritePlanStep[] | null
  verified_by: string | null
  /** 原始 JSON（详情展示用，字段随后端演进不丢信息）。 */
  raw: unknown
}

/** prepare 回执里的目标（通道 + 8.24 量化后的线性值 + 等效 dB）。 */
export interface MotuVolumeDraftTarget {
  channels: MotuChannel[]
  linear: number | null
  db: number | null
}

/** PUT /api/motu/volume {stage:"prepare"} 归一化回执（不写设备）。 */
export interface MotuVolumeDraft {
  ok: true
  schema: string | null
  stage: string | null
  engine_token: string
  token: string | null
  created_at: string | null
  expires_in_s: number | null
  target: MotuVolumeDraftTarget | null
  write_plan: MotuWritePlan | null
  commit_supported: boolean | null
  two_phase: boolean | null
  /** prepare 必须为 false/null（true 视为契约漂移并抛错）。 */
  writes_device: boolean | null
  applies_to: string | null
  target_device: MotuDetection | null
}

/** 已验证的单键写回执（commit channels[]；linear 值，verified=true 才代表回读一致）。 */
export interface MotuWriteReceipt {
  channel: number | null
  name: string | null
  key: string | null
  index: number | null
  before: number | null
  wrote: number | null
  readback: number | null
  changed: boolean | null
  frame_sent: boolean | null
  verified: boolean | null
}

/** PUT /api/motu/volume {stage:"commit"} 归一化回执（写设备 + 回读）。 */
export interface MotuVolumeCommitAck {
  ok: true
  schema: string | null
  stage: string | null
  engine_token: string | null
  applied: boolean | null
  idempotent: boolean | null
  created_at: string | null
  linear: number | null
  db: number | null
  channels: MotuWriteReceipt[] | null
  write_plan: MotuWritePlan | null
  writes_device: boolean | null
  applies_to: string | null
  target_device: MotuDetection | null
}

/* ---------------------------------------------------------------- 运行时校验 */

function invalidResponse(detail: string): ApiError {
  return new ApiError({
    status: 200,
    code: 'invalid_response',
    message: 'unexpected MOTU response: ' + detail
  })
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return null
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

function readRequiredString(record: Record<string, unknown>, field: string): string {
  const value = readString(record, field)
  if (value === null || value.length === 0) {
    throw invalidResponse(field + ' must be a non-empty string')
  }
  return value
}

function readNumber(record: Record<string, unknown>, field: string): number | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw invalidResponse(field + ' must be a finite number or null')
  }
  return value
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

function readOk(record: Record<string, unknown>): true {
  if (record.ok !== true) {
    throw invalidResponse('ok must be true')
  }
  return true
}

function readArray(record: Record<string, unknown>, field: string): unknown[] | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (!Array.isArray(value)) {
    throw invalidResponse(field + ' must be an array or null')
  }
  return value
}

function readStringArray(record: Record<string, unknown>, field: string): string[] | null {
  const value = readArray(record, field)
  if (value === null) {
    return null
  }
  return value.map((entry) => {
    if (typeof entry !== 'string') {
      throw invalidResponse(field + ' entries must be strings')
    }
    return entry
  })
}

function readSchema(record: Record<string, unknown>, expected: string): string | null {
  const schema = readString(record, 'schema')
  if (schema !== null && schema !== expected) {
    throw invalidResponse('schema must be ' + expected + ' (got ' + schema + ')')
  }
  return schema
}

function parseDatastore(value: unknown): MotuDatastoreRef | null {
  if (value === null || value === undefined) {
    return null
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('datastore must be an object or null')
  }
  return {
    key: readRequiredString(record, 'key'),
    index: readNumber(record, 'index') ?? 0
  }
}

function parseChannel(value: unknown): MotuChannel {
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('channel entry must be an object')
  }
  const channel = readNumber(record, 'channel')
  if (channel === null || !Number.isInteger(channel) || channel < 1) {
    throw invalidResponse('channel entry needs a positive integer channel')
  }
  return {
    channel,
    name: readString(record, 'name'),
    datastore: parseDatastore(record.datastore)
  }
}

function parseChannels(value: unknown): MotuChannel[] {
  const array = value
  if (array === undefined || array === null) {
    return []
  }
  if (!Array.isArray(array)) {
    throw invalidResponse('channels must be an array')
  }
  return array.map((entry) => parseChannel(entry))
}

function parseVolumeReadback(value: unknown): MotuVolumeReadback {
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('vol entry must be an object')
  }
  const channel = readNumber(record, 'channel')
  if (channel === null || !Number.isInteger(channel) || channel < 1) {
    throw invalidResponse('vol entry needs a positive integer channel')
  }
  return {
    channel,
    name: readString(record, 'name'),
    datastore: parseDatastore(record.datastore),
    linear: readNumber(record, 'linear'),
    db: readNumber(record, 'db')
  }
}

function parseVolumes(value: unknown): MotuVolumeReadback[] | null {
  if (value === null || value === undefined) {
    return null
  }
  if (!Array.isArray(value)) {
    throw invalidResponse('vol must be an array or null')
  }
  return value.map((entry) => parseVolumeReadback(entry))
}

function parseDeviceInfo(value: unknown): MotuDeviceInfo | null {
  if (value === null || value === undefined) {
    return null
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('state.device must be an object or null')
  }
  return {
    model_id: readString(record, 'model_id'),
    name: readString(record, 'name'),
    version: readString(record, 'version'),
    api_version: readNumber(record, 'api_version')
  }
}

function parseDeviceState(value: unknown): MotuDeviceState | null {
  if (value === null || value === undefined) {
    return null
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('state must be an object or null')
  }
  return {
    sample_rate_hz: readNumber(record, 'sample_rate_hz'),
    main_trim: readNumber(record, 'main_trim'),
    main_bus_mute: readBoolean(record, 'main_bus_mute'),
    device: parseDeviceInfo(record.device),
    missing: readStringArray(record, 'missing')
  }
}

function parseReadError(value: unknown): MotuReadError | null {
  if (value === null || value === undefined) {
    return null
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('read_error must be an object or null')
  }
  return {
    code: readRequiredString(record, 'code'),
    message: typeof record.message === 'string' ? record.message : '',
    retryable: readBoolean(record, 'retryable')
  }
}

function parseWriteStatus(value: unknown): MotuWriteStatus | null {
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'string' || !(MOTU_WRITE_STATUSES as readonly string[]).includes(value)) {
    throw invalidResponse('write.status must be one of ' + MOTU_WRITE_STATUSES.join('|'))
  }
  return value as MotuWriteStatus
}

function parseWriteCapability(value: unknown): MotuWriteCapability | null {
  if (value === null || value === undefined) {
    return null
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('write must be an object or null')
  }
  return {
    status: parseWriteStatus(record.status),
    single_key: readBoolean(record, 'single_key'),
    readback: readBoolean(record, 'readback'),
    two_phase: readBoolean(record, 'two_phase'),
    batch: readBoolean(record, 'batch'),
    max_channels_per_request: readNumber(record, 'max_channels_per_request'),
    reason: readString(record, 'reason')
  }
}

function parseNotWired(value: unknown): MotuNotWiredEntry[] {
  if (value === undefined || value === null) {
    return []
  }
  if (!Array.isArray(value)) {
    throw invalidResponse('not_wired must be an array')
  }
  const entries: MotuNotWiredEntry[] = []
  for (const item of value) {
    const record = asRecord(item)
    if (record === null) {
      throw invalidResponse('not_wired entries must be objects')
    }
    entries.push({ path: readString(record, 'path'), reason: readString(record, 'reason') })
  }
  return entries
}

function parseReasons(value: unknown): Record<string, string> {
  if (value === undefined || value === null) {
    return {}
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('reasons must be an object')
  }
  const reasons: Record<string, string> = {}
  for (const [key, entry] of Object.entries(record)) {
    if (typeof entry === 'string') {
      reasons[key] = entry
    }
  }
  return reasons
}

function parseDetection(record: Record<string, unknown>): MotuDetection {
  return {
    detected: readBoolean(record, 'detected'),
    model: readString(record, 'model'),
    card_id: readString(record, 'card_id'),
    card_index: readNumber(record, 'card_index'),
    control_reachable: readBoolean(record, 'control_reachable'),
    reason: readString(record, 'reason'),
    reason_detail: readString(record, 'reason_detail')
  }
}

function parseTargetDevice(value: unknown): MotuDetection | null {
  if (value === null || value === undefined) {
    return null
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('target_device must be an object or null')
  }
  return parseDetection(record)
}

function parseCache(value: unknown): MotuCacheMeta | null {
  if (value === null || value === undefined) {
    return null
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('cache must be an object or null')
  }
  const lastError = record.last_error
  if (lastError !== null && lastError !== undefined && typeof lastError !== 'string') {
    throw invalidResponse('cache.last_error must be a string or null')
  }
  return {
    age_ms: readNumber(record, 'age_ms'),
    stale: readBoolean(record, 'stale'),
    last_error: typeof lastError === 'string' ? lastError : null
  }
}

/** 校验并归一化 GET /api/motu（只读快照；null 字段保留为 null）。 */
export function parseMotuStatus(body: unknown): MotuStatus {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('body must be an object')
  }
  const detection = parseDetection(record)
  const channels = parseChannels(record.channels)
  const vol = parseVolumes(record.vol)
  const state = parseDeviceState(record.state)
  const readError = parseReadError(record.read_error)
  const write = parseWriteCapability(record.write)
  return {
    schema: readSchema(record, MOTU_STATUS_SCHEMA),
    ...detection,
    channel_count: readNumber(record, 'channel_count'),
    channels,
    mute: readBoolean(record, 'mute'),
    vol,
    state,
    read_error: readError,
    write,
    not_wired: parseNotWired(record.not_wired),
    reasons: parseReasons(record.reasons),
    cache: parseCache(record.cache)
  }
}

function parseDraftTarget(value: unknown): MotuVolumeDraftTarget | null {
  if (value === null || value === undefined) {
    return null
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('target must be an object or null')
  }
  return {
    channels: parseChannels(record.channels),
    linear: readNumber(record, 'linear'),
    db: readNumber(record, 'db')
  }
}

function parseWritePlan(value: unknown): MotuWritePlan | null {
  if (value === null || value === undefined) {
    return null
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('write_plan must be an object or null')
  }
  const stepsValue = readArray(record, 'steps')
  const steps: MotuWritePlanStep[] | null =
    stepsValue === null
      ? null
      : stepsValue.map((entry) => {
          const step = asRecord(entry)
          if (step === null) {
            throw invalidResponse('write_plan.steps entries must be objects')
          }
          return {
            channel: readNumber(step, 'channel'),
            key: readString(step, 'key'),
            index: readNumber(step, 'index'),
            frames: readNumber(step, 'frames')
          }
        })
  return {
    kind: readString(record, 'kind'),
    batch: readBoolean(record, 'batch'),
    writes_per_channel: readNumber(record, 'writes_per_channel'),
    writes: readNumber(record, 'writes'),
    steps,
    verified_by: readString(record, 'verified_by'),
    raw: value
  }
}

function parseWriteReceipts(value: unknown): MotuWriteReceipt[] | null {
  const array = Array.isArray(value) ? value : null
  if (array === null) {
    return null
  }
  return array.map((entry) => {
    const record = asRecord(entry)
    if (record === null) {
      throw invalidResponse('channels entries must be objects')
    }
    return {
      channel: readNumber(record, 'channel'),
      name: readString(record, 'name'),
      key: readString(record, 'key'),
      index: readNumber(record, 'index'),
      before: readNumber(record, 'before'),
      wrote: readNumber(record, 'wrote'),
      readback: readNumber(record, 'readback'),
      changed: readBoolean(record, 'changed'),
      frame_sent: readBoolean(record, 'frame_sent'),
      verified: readBoolean(record, 'verified')
    }
  })
}

/** 校验并归一化 PUT /api/motu/volume {stage:"prepare"}（必须不写设备）。 */
export function parseMotuVolumeDraft(body: unknown): MotuVolumeDraft {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('body must be an object')
  }
  readOk(record)
  const token = readRequiredString(record, 'engine_token')
  const writesDevice = readBoolean(record, 'writes_device')
  if (writesDevice === true) {
    // prepare 只生成草稿；若声称已写设备，拒绝把它当 prepare 回执（不假成功）。
    throw invalidResponse('prepare must not report writes_device=true')
  }
  return {
    ok: true,
    schema: readSchema(record, MOTU_VOLUME_SCHEMA),
    stage: readString(record, 'stage'),
    engine_token: token,
    token: readString(record, 'token'),
    created_at: readString(record, 'created_at'),
    expires_in_s: readNumber(record, 'expires_in_s'),
    target: parseDraftTarget(record.target),
    write_plan: parseWritePlan(record.write_plan),
    commit_supported: readBoolean(record, 'commit_supported'),
    two_phase: readBoolean(record, 'two_phase'),
    writes_device: writesDevice,
    applies_to: readString(record, 'applies_to'),
    target_device: parseTargetDevice(record.target_device)
  }
}

/** 校验并归一化 PUT /api/motu/volume {stage:"commit"}（写设备 + 回读）。 */
export function parseMotuVolumeCommitAck(body: unknown): MotuVolumeCommitAck {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('body must be an object')
  }
  readOk(record)
  return {
    ok: true,
    schema: readSchema(record, MOTU_VOLUME_SCHEMA),
    stage: readString(record, 'stage'),
    engine_token: readString(record, 'engine_token'),
    applied: readBoolean(record, 'applied'),
    idempotent: readBoolean(record, 'idempotent'),
    created_at: readString(record, 'created_at'),
    linear: readNumber(record, 'linear'),
    db: readNumber(record, 'db'),
    channels: record.channels === undefined ? null : parseWriteReceipts(record.channels),
    write_plan: parseWritePlan(record.write_plan),
    writes_device: readBoolean(record, 'writes_device'),
    applies_to: readString(record, 'applies_to'),
    target_device: parseTargetDevice(record.target_device)
  }
}

/* --------------------------------------------------------------- 请求校验/构造 */

/**
 * 校验 UI 的 dB 目标（前端约束；null = 合法）。
 * 只接受有限数且落在 MOTU_VOLUME_DB_MIN..MOTU_VOLUME_DB_MAX（后端 db 必须 <= 0 dB）。
 */
export function validateMotuVolumeDb(db: unknown): string | null {
  if (typeof db !== 'number' || !Number.isFinite(db)) {
    return 'db'
  }
  if (db > MOTU_VOLUME_DB_MAX || db < MOTU_VOLUME_DB_MIN) {
    return 'db'
  }
  return null
}

/** 校验通道选择（null = 合法；UI 只提供 1 / 2 / "both"）。 */
export function validateMotuChannelSelection(selection: unknown): string | null {
  if (selection === 1 || selection === 2 || selection === 'both') {
    return null
  }
  return 'channel'
}

/** prepare 请求体（精确形状；契约测试断言）。 */
export function buildMotuVolumePrepareBody(selection: MotuChannelSelection, db: number): Record<string, unknown> {
  return { stage: 'prepare', channel: selection, db }
}

/** commit 请求体（精确形状；契约测试断言）。 */
export function buildMotuVolumeCommitBody(engineToken: string): Record<string, unknown> {
  return { stage: 'commit', engine_token: engineToken }
}

/* ---------------------------------------------------------------------- 端点 */

export interface MotuApi {
  /** GET /api/motu：只读设备快照（signal 供轮询暂停/离开页面时中止）。 */
  fetchMotu(options?: { signal?: AbortSignal }): Promise<MotuStatus>
  /** PUT /api/motu/volume {stage:"prepare", channel, db}：只生成草稿（不写设备）。 */
  prepareVolume(selection: MotuChannelSelection, db: number): Promise<MotuVolumeDraft>
  /** PUT /api/motu/volume {stage:"commit", engine_token}：写设备并返回逐通道真值回读。 */
  commitVolume(engineToken: string): Promise<MotuVolumeCommitAck>
}

/** 用共享 ApiClient 构造 MOTU API（单测注入 fetchImpl 即可脱离网络）。 */
export function createMotuApi(client: ApiClient): MotuApi {
  return {
    async fetchMotu(options: { signal?: AbortSignal } = {}): Promise<MotuStatus> {
      const body = await client.get<unknown>(MOTU_STATUS_PATH, { signal: options.signal })
      return parseMotuStatus(body)
    },
    async prepareVolume(selection: MotuChannelSelection, db: number): Promise<MotuVolumeDraft> {
      const body = await client.put<unknown>(MOTU_VOLUME_PATH, buildMotuVolumePrepareBody(selection, db))
      return parseMotuVolumeDraft(body)
    },
    async commitVolume(engineToken: string): Promise<MotuVolumeCommitAck> {
      const body = await client.put<unknown>(MOTU_VOLUME_PATH, buildMotuVolumeCommitBody(engineToken))
      return parseMotuVolumeCommitAck(body)
    }
  }
}

/* ---------------------------------------------------------------- 错误归类 */

export type MotuErrorCategory =
  | 'network'
  | 'timeout'
  | 'unauthorized'
  | 'not_detected'
  | 'control_unreachable'
  | 'control_disabled'
  | 'batch_rejected'
  | 'invalid_request'
  | 'token_invalid'
  | 'device_error'
  | 'unsupported'
  | 'not_wired'
  | 'server'
  | 'invalid_response'
  | 'unknown'

/** 设备错误 details.motu（MotuError::to_json 原样）。 */
export interface MotuDeviceErrorDetail {
  code: string | null
  message: string | null
  retryable: boolean | null
}

export interface MotuErrorInfo {
  category: MotuErrorCategory
  status: number
  code: string
  /** 主文案 i18n 键（zh-CN / en-US 对等）。 */
  i18nKey: string
  /** 后端 message / 异常消息（仅详情展示）。 */
  detail?: string
  /** 是否值得重试（401 / 请求形状错误不重试）。 */
  retryable: boolean
  /** 设备错误码（details.motu.code，e.g. E_READBACK_MISMATCH）；非设备错误为 null。 */
  deviceCode: string | null
  deviceMessage: string | null
  deviceRetryable: boolean | null
  /** 设备写失败时已写入并回读通过的通道（details.completed，原样）。 */
  completed: MotuWriteReceipt[] | null
  /** completed 里的通道号（页面直出「已写通道」）。 */
  completedChannels: number[]
  /** 请求的目标通道（details.requested_channels）。 */
  requestedChannels: number[] | null
  /** 失败的通道（details.channel）。 */
  failedChannel: number | null
  /** 失败时是否已有通道真的写进设备（details.writes_device）。 */
  partialWrites: boolean | null
  /** 草稿是否已被后端消费（details.draft_consumed=true 时本地草稿不能再用）。 */
  draftConsumed: boolean | null
  /** 后端 details 原样（details.motu 等由页面原样展示）。 */
  details: unknown
}

/** 稳定码 -> 类别（与 daemon MOTU 端点错误码一致；未知 4xx 归 invalid_request，未知 5xx 归 server）。 */
const MOTU_CODE_CATEGORIES: Record<string, MotuErrorCategory> = {
  motu_not_detected: 'not_detected',
  motu_control_unreachable: 'control_unreachable',
  motu_control_disabled: 'control_disabled',
  motu_batch_rejected: 'batch_rejected',
  invalid_request: 'invalid_request',
  bad_request: 'invalid_request',
  engine_token_invalid: 'token_invalid',
  unsupported: 'unsupported',
  not_implemented: 'unsupported',
  not_wired: 'not_wired',
  request_timeout: 'timeout',
  network_error: 'network'
}

const MOTU_ERROR_KEYS: Record<MotuErrorCategory, string> = {
  network: 'motu.error.network',
  timeout: 'motu.error.timeout',
  unauthorized: 'motu.error.unauthorized',
  not_detected: 'motu.error.notDetected',
  control_unreachable: 'motu.error.controlUnreachable',
  control_disabled: 'motu.error.controlDisabled',
  batch_rejected: 'motu.error.batchRejected',
  invalid_request: 'motu.error.invalidRequest',
  token_invalid: 'motu.error.tokenInvalid',
  device_error: 'motu.error.device',
  unsupported: 'motu.error.unsupported',
  not_wired: 'motu.error.notWired',
  server: 'motu.error.server',
  invalid_response: 'motu.error.invalidResponse',
  unknown: 'motu.error.unknown'
}

function readNumberArray(value: unknown): number[] | null {
  if (!Array.isArray(value)) {
    return null
  }
  const numbers: number[] = []
  for (const entry of value) {
    if (typeof entry !== 'number' || !Number.isFinite(entry)) {
      return null
    }
    numbers.push(entry)
  }
  return numbers
}

function extractReceipts(value: unknown): MotuWriteReceipt[] | null {
  if (!Array.isArray(value)) {
    return null
  }
  try {
    return parseWriteReceipts(value)
  } catch {
    return null
  }
}

function extractDetails(details: unknown): Pick<
  MotuErrorInfo,
  'deviceCode' | 'deviceMessage' | 'deviceRetryable' | 'completed' | 'completedChannels' | 'requestedChannels' | 'failedChannel' | 'partialWrites' | 'draftConsumed'
> {
  const record = asRecord(details)
  const motu = record === null ? null : asRecord(record.motu)
  const completed = record === null ? null : extractReceipts(record.completed)
  const completedChannels = completed === null ? [] : completed.map((receipt) => receipt.channel).filter((channel): channel is number => channel !== null)
  return {
    deviceCode: motu === null ? null : readStringSafe(motu, 'code'),
    deviceMessage: motu === null ? null : readStringSafe(motu, 'message'),
    deviceRetryable: motu === null ? null : typeof motu.retryable === 'boolean' ? motu.retryable : null,
    completed,
    completedChannels,
    requestedChannels: record === null ? null : readNumberArray(record.requested_channels),
    failedChannel: record === null ? null : readNumberSafe(record, 'channel'),
    partialWrites: record === null ? null : typeof record.writes_device === 'boolean' ? record.writes_device : null,
    draftConsumed: record === null ? null : typeof record.draft_consumed === 'boolean' ? record.draft_consumed : null
  }
}

function readStringSafe(record: Record<string, unknown>, field: string): string | null {
  const value = record[field]
  return typeof value === 'string' && value.length > 0 ? value : null
}

function readNumberSafe(record: Record<string, unknown>, field: string): number | null {
  const value = record[field]
  return typeof value === 'number' && Number.isFinite(value) ? value : null
}

/**
 * 把 MOTU 失败归类为 UI 可分支的稳定类别 + 本地化文案键。
 * 同时把 details.motu / details.completed 等原样抽出（设备写失败时页面必须列出已写通道）。
 */
export function classifyMotuError(error: unknown): MotuErrorInfo {
  const emptyDetails = extractDetails(null)
  if (!(error instanceof ApiError)) {
    const detail = error instanceof Error ? error.message : undefined
    return {
      category: 'unknown',
      status: 0,
      code: 'unknown_error',
      i18nKey: MOTU_ERROR_KEYS.unknown,
      ...(detail === undefined ? {} : { detail }),
      retryable: true,
      ...emptyDetails,
      details: null
    }
  }
  const details = extractDetails(error.details)
  let category: MotuErrorCategory
  if (error.code === 'invalid_response') {
    category = 'invalid_response'
  } else if (error.code === 'request_timeout') {
    category = 'timeout'
  } else if (error.status === 0 || error.code === 'network_error') {
    category = 'network'
  } else if (error.status === 401) {
    category = 'unauthorized'
  } else {
    const mapped = MOTU_CODE_CATEGORIES[error.code]
    if (mapped !== undefined) {
      category = mapped
    } else if (error.status === 502 || error.code.startsWith('E_')) {
      category = 'device_error'
    } else if (error.status >= 500) {
      category = 'server'
    } else if (error.status === 400 || error.status === 409 || error.status === 404) {
      category = 'invalid_request'
    } else {
      category = 'unknown'
    }
  }
  const retryable =
    category !== 'unauthorized' && category !== 'invalid_response' && category !== 'batch_rejected' && category !== 'invalid_request'
  const detail = error.message.length > 0 ? error.message : undefined
  return {
    category,
    status: error.status,
    code: error.code,
    i18nKey: MOTU_ERROR_KEYS[category],
    ...(detail === undefined ? {} : { detail }),
    retryable,
    ...details,
    details: error.details
  }
}

/* ------------------------------------------------------------------ 诚实读取辅助 */

/** 写前门禁（与后端 ensure_ready 等价）：detected + control_reachable + write.status=wired。 */
export function motuIsWritable(status: MotuStatus | null): boolean {
  return (
    status !== null &&
    status.detected === true &&
    status.control_reachable === true &&
    status.write !== null &&
    status.write.status === 'wired'
  )
}

/** reasons 里某路径的原因（字段为 null 时页面显示“未上报 + 原因”）；没有则 null。 */
export function motuReasonFor(status: MotuStatus | null, path: string): string | null {
  if (status === null) {
    return null
  }
  const reason = status.reasons[path]
  return typeof reason === 'string' && reason.length > 0 ? reason : null
}

/** reasons 映射展开（路径排序，页面逐条展示）。 */
export function motuReasonEntries(status: MotuStatus | null): Array<{ path: string; reason: string }> {
  if (status === null) {
    return []
  }
  return Object.entries(status.reasons)
    .map(([path, reason]) => ({ path, reason }))
    .sort((left, right) => left.path.localeCompare(right.path))
}

/** 检测原因码对应的本地化键；未知码返回 null（页面回退 reason_detail 原文）。 */
export function motuDetectionReasonKey(reason: string | null): string | null {
  if (reason === null) {
    return null
  }
  return MOTU_DETECTION_REASON_KEYS[reason] ?? null
}

/** vol 里某通道的回读（channel 1 = Main 1）；没有该通道返回 null。 */
export function motuVolumeFor(status: MotuStatus | null, channel: number): MotuVolumeReadback | null {
  if (status === null || status.vol === null) {
    return null
  }
  for (const entry of status.vol) {
    if (entry.channel === channel) {
      return entry
    }
  }
  return null
}
