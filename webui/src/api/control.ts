/**
 * 控制面两阶段 REST 客户端：静音、五个语义界面（/api/surface）—— volume（per-lane protection
 * gains）、routing（lane→DAC）、layout（内容布局折叠加权）、mode（off/on/aes67）、format
 * （sample_rate/chunksize，也走 /api/format）；commit/abort 统一 /api/commit|/api/abort。
 *
 * 契约来源（以后端代码为准，不靠猜测）：
 * - `crates/daemon/src/lib.rs` 的 REST 路由：POST /api/mute、POST /api/surface、POST /api/commit、
 *   POST /api/abort、POST /api/format、POST /api/device/retry（T70f-b 转发到 rt-host 控制通道）；
 * - `crates/host/src/control/mod.rs` 的 op 语义：surface=volume 时 `data.gains_db = [{lane, gain_db}]`，
 *   以"最近一次 commit 生效的方案"为基线只改列出的 lane；surface=mode 时 `data.mode` ∈ off/on/aes67；
 *   format 准备返回 engine_token / planned_version / format / changed / device_restart_required；
 *   commit 统一 POST /api/commit {engine_token}，abort 统一 POST /api/abort {engine_token}。
 *
 * 纪律：
 * - 所有响应都做运行时校验：ok !== true、engine_token 缺失、字段类型漂移 -> ApiError(invalid_response)，
 *   不把畸形响应当成成功（不伪造"已生效"）；
 * - 错误体固定 `{error:{code,message,details}}`，由共享 ApiClient 解析成 ApiError；本模块把稳定
 *   code 归类成 UI 可分支的类别 + 本地化文案键（后端 message 只作详情），401 交给会话 store；
 * - 当前值只能来自 GET /api/status（state.muted / format.*）：控制协议没有 master volume、没有
 *   "读取当前 per-lane 增益"的 op，前端如实显示「未接入」而不是编一个 0 dB；
 * - 纯函数（解析/校验/增益模型/错误归类）不依赖 Vue / DOM，Node 单测可直接覆盖。
 */
import { ApiClient, ApiError } from './client.ts'

/** 后端支持的采样率（镜像 crates/core/src/rt.rs 的 SAMPLE_RATES_HZ；不另编一套）。 */
export const SUPPORTED_SAMPLE_RATES_HZ: readonly number[] = [48000, 96000]

/** 后端支持的块长（镜像 crates/core/src/rt.rs 的 BLOCK_SIZES）。 */
export const SUPPORTED_BLOCK_SIZES: readonly number[] = [64, 128]

/** 音量滑杆范围（dB）：前端约束，后端只校验有限数；滑杆步进 0.5 dB。 */
export const VOLUME_GAIN_MIN_DB = -150
export const VOLUME_GAIN_MAX_DB = 50
export const VOLUME_GAIN_STEP_DB = 0.5

/** 逐 lane 增益草稿允许的最大 lane 数（防御性上限；实际 lane 数来自 /api/status.format.channels）。 */
export const MAX_LANES = 32

/** 操作历史最多保留条数（页面「最近操作」列表）。 */
export const MAX_OPERATION_HISTORY = 8

/** 控制面 REST 路径（集中定义，mock 与单测共用）。 */
export const CONTROL_PATHS = {
  mute: '/api/mute',
  surface: '/api/surface',
  format: '/api/format',
  commit: '/api/commit',
  abort: '/api/abort',
  deviceRetry: '/api/device/retry'
} as const

/**
 * 控制面语义界面（surface op）。
 *
 * 五个界面全部由后端 WIRED_SURFACES 上报（/api/meta.surfaces）；
 * volume/routing/layout 是方案补丁（需要已生效方案作为基线），format/mode 走执行器配置两阶段。
 */
export type ControlSurface = 'volume' | 'routing' | 'layout' | 'mode' | 'format'

/** 运行模式：控制面真正接通的只有 off / aes67（"on" 是 aes67 的别名）。 */
export type RunMode = 'off' | 'aes67'

/** 前端候选模式；analog 是产品意向，但控制面没有接通（T70f/T70g 未实现模拟输入源）。 */
export type ModeOption = RunMode | 'analog'

/** 逐 lane 增益补丁项（与后端 data.gains_db 的元素一致）。 */
export interface GainEntry {
  lane: number
  gain_db: number
}

/** 格式快照（format_json 的字段名）。 */
export interface ControlFormat {
  sample_rate: number | null
  block_size: number | null
  channels: number | null
}

/** 格式准备补丁（至少一个字段）。 */
export interface FormatPatch {
  sample_rate?: number
  chunksize?: number
}

/**
 * 路由界面的补丁（/api/surface {surface:"routing", data}）。
 * routes 是 lane -> DAC 的完整替换表；dac_channels 缺省沿用方案值（CONTRACTS B.1.2）。
 */
export interface RoutingPatch {
  routes: Array<[number, number]>
  dac_channels?: number
}

/**
 * 内容布局界面的补丁（/api/surface {surface:"layout", data}）。
 * folds 是 [src, dst, gain] 的完整替换表；越界/空表由后端准入拒绝（前端只做形状校验）。
 */
export interface LayoutPatch {
  in_channels: number
  out_lanes: number
  folds: Array<[number, number, number]>
}

/** 语义界面补丁的并集（commit/abort 与草稿形状共用）。 */
export type SurfacePatch = FormatPatch | RoutingPatch | LayoutPatch

/** 草稿种类（volume/mode/format 由 M3 使用；plan/rollback 由 M4 方案页复用同一 commit/abort）。 */
export type ControlDraftSurface = ControlSurface | 'plan' | 'rollback'

/**
 * prepare 归一化回执（三种 surface 的响应取并集；缺失字段为 null，不猜）。
 *
 * - volume：engine_token / planned_version / graph_id / admission（无 changed）；
 * - mode：engine_token / planned_version / mode / changed / applies_to / note；
 * - format：engine_token / planned_version / format / changed / device_restart_required / note；
 * - rollback：engine_token / planned_version / rolled_back_to / graph_id / admission。
 */
export interface ControlDraft {
  ok: true
  surface: ControlDraftSurface | null
  engine_token: string
  planned_version: number | null
  changed: boolean | null
  graph_id: string | null
  rolled_back_to: number | null
  mode: string | null
  format: ControlFormat | null
  device_restart_required: boolean | null
  applies_to: string | null
  note: string | null
  /** 后端上报的补丁差异（API.md §1 通用两阶段语义）；未上报为 null（不臆造）。 */
  diff: unknown
  /** 后端上报的当前值；未上报为 null。 */
  current: unknown
  /** 后端上报的目标值；未上报为 null。 */
  target: unknown
  /** 后端上报的图编译结果（graph）；未上报为 null。 */
  graph: unknown
  /** 准入结果（accepted/budget/rejections）；未上报为 null。 */
  admission: Record<string, unknown> | null
  /** 阻塞项（例如 mapping 改动只能经方案 commit 生效）；未上报为 null。 */
  blockers: string[] | null
  /** 该草稿是否支持 commit（/api/channels 的 commit_supported）；未上报为 null。 */
  commit_supported: boolean | null
}

/** commit 归一化回执（三类草稿的响应取并集；字段缺失为 null）。 */
export interface ControlCommitAck {
  ok: true
  version: number | null
  applied_block: number | null
  changed: boolean | null
  all_ack: boolean | null
  graph_id: string | null
  mode: string | null
  format: ControlFormat | null
  device_restart_required: boolean | null
  graph_dropped: boolean | null
  history_cleared: boolean | null
  mute_preserved: boolean | null
  note: string | null
}

/** 静音回执（mute_json）。 */
export interface ControlMuteAck {
  ok: true
  mute: boolean
  applied_block: number | null
  idempotent: boolean | null
}

/** abort 回执。 */
export interface ControlAbortAck {
  ok: true
  aborted: boolean
  aborted_surface: string | null
}

/** /api/device/retry 回执（设备快照；字段缺失为 null，不编数）。 */
export interface DeviceRetryAck {
  wired: boolean | null
  state: string | null
  device_rate: number | null
  rate_asserted: boolean | null
  last_error_code: string | null
  last_error_message: string | null
}

/* ------------------------------------------------------------------ 运行时校验 */

function invalidResponse(detail: string): ApiError {
  return new ApiError({
    status: 200,
    code: 'invalid_response',
    message: 'unexpected control response: ' + detail
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

/** 任意 JSON 值（diff/current/target/graph 的形状由后端定义，前端只做展示）。 */
function readJson(record: Record<string, unknown>, field: string): unknown {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  return value
}

/** JSON 对象字段（admission）；缺失 = null，非对象 = 契约漂移。 */
function readJsonObject(record: Record<string, unknown>, field: string): Record<string, unknown> | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  const object = asRecord(value)
  if (object === null) {
    throw invalidResponse(field + ' must be an object or null')
  }
  return object
}

/** 字符串数组字段（blockers）；缺失 = null。 */
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

function readOk(record: Record<string, unknown>): true {
  if (record.ok !== true) {
    throw invalidResponse('ok must be true')
  }
  return true
}

/** format_json：{sample_rate, block_size, channels}；整组缺失返回 null。 */
function readFormatField(value: unknown): ControlFormat | null {
  if (value === null || value === undefined) {
    return null
  }
  const record = asRecord(value)
  if (record === null) {
    throw invalidResponse('format must be an object or null')
  }
  return {
    sample_rate: readNumber(record, 'sample_rate'),
    block_size: readNumber(record, 'block_size'),
    channels: readNumber(record, 'channels')
  }
}

/**
 * 校验并归一化 prepare 响应（POST /api/surface 或 /api/format、/api/prepare、/api/rollback）。
 * 不合法抛 ApiError(code=invalid_response)；合法返回全字段类型化对象。
 */
export function parseControlDraft(body: unknown): ControlDraft {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('body must be an object')
  }
  readOk(record)
  const token = readString(record, 'engine_token')
  if (token === null || token.length === 0) {
    throw invalidResponse('engine_token must be a non-empty string')
  }
  const surfaceRaw = readString(record, 'surface')
  const rolledBackTo = readNumber(record, 'rolled_back_to')
  const mode = readString(record, 'mode')
  const format = readFormatField(record.format)
  const graphId = readString(record, 'graph_id')
  let surface: ControlDraftSurface | null = null
  if (
    surfaceRaw === 'volume' ||
    surfaceRaw === 'routing' ||
    surfaceRaw === 'layout' ||
    surfaceRaw === 'mode' ||
    surfaceRaw === 'format'
  ) {
    surface = surfaceRaw
  } else if (rolledBackTo !== null) {
    surface = 'rollback'
  } else if (mode !== null) {
    surface = 'mode'
  } else if (format !== null) {
    surface = 'format'
  } else if (graphId !== null) {
    surface = 'plan'
  }
  return {
    ok: true,
    surface,
    engine_token: token,
    planned_version: readNumber(record, 'planned_version'),
    changed: readBoolean(record, 'changed'),
    graph_id: graphId,
    rolled_back_to: rolledBackTo,
    mode,
    format,
    device_restart_required: readBoolean(record, 'device_restart_required'),
    applies_to: readString(record, 'applies_to'),
    note: readString(record, 'note'),
    diff: readJson(record, 'diff'),
    current: readJson(record, 'current'),
    target: readJson(record, 'target'),
    graph: readJson(record, 'graph'),
    admission: readJsonObject(record, 'admission'),
    blockers: readStringArray(record, 'blockers'),
    commit_supported: readBoolean(record, 'commit_supported')
  }
}

/** 校验并归一化 POST /api/commit 响应。 */
export function parseControlCommitAck(body: unknown): ControlCommitAck {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('body must be an object')
  }
  readOk(record)
  return {
    ok: true,
    version: readNumber(record, 'version'),
    applied_block: readNumber(record, 'applied_block'),
    changed: readBoolean(record, 'changed'),
    all_ack: readBoolean(record, 'all_ack'),
    graph_id: readString(record, 'graph_id'),
    mode: readString(record, 'mode'),
    format: readFormatField(record.format),
    device_restart_required: readBoolean(record, 'device_restart_required'),
    graph_dropped: readBoolean(record, 'graph_dropped'),
    history_cleared: readBoolean(record, 'history_cleared'),
    mute_preserved: readBoolean(record, 'mute_preserved'),
    note: readString(record, 'note')
  }
}

/** 校验并归一化 POST /api/mute 响应：mute 必须是真实布尔值（回读值）。 */
export function parseControlMuteAck(body: unknown): ControlMuteAck {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('body must be an object')
  }
  readOk(record)
  const mute = record.mute
  if (typeof mute !== 'boolean') {
    throw invalidResponse('mute must be a boolean')
  }
  return {
    ok: true,
    mute,
    applied_block: readNumber(record, 'applied_block'),
    idempotent: readBoolean(record, 'idempotent')
  }
}

/** 校验并归一化 POST /api/abort 响应。 */
export function parseControlAbortAck(body: unknown): ControlAbortAck {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('body must be an object')
  }
  readOk(record)
  if (record.aborted !== true) {
    throw invalidResponse('aborted must be true')
  }
  return {
    ok: true,
    aborted: true,
    aborted_surface: readString(record, 'aborted_surface')
  }
}

/** 校验并归一化 POST /api/device/retry 响应（设备快照；宽松，只为页面提示）。 */
export function parseDeviceRetryAck(body: unknown): DeviceRetryAck {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('body must be an object')
  }
  const lastError = asRecord(record.last_error)
  return {
    wired: readBoolean(record, 'wired'),
    state: readString(record, 'state'),
    device_rate: readNumber(record, 'device_rate'),
    rate_asserted: readBoolean(record, 'rate_asserted'),
    last_error_code: lastError === null ? null : readString(lastError, 'code'),
    last_error_message: lastError === null ? null : readString(lastError, 'message')
  }
}

/* ------------------------------------------------------------------ 增益模型 */

/** 数据库步进对齐（四舍五入到 0.5 dB），并把非法值收敛为 0；不抛错（UI 输入用）。 */
export function normalizeGainDb(value: number | null | undefined): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return 0
  }
  const clamped = Math.min(Math.max(value, VOLUME_GAIN_MIN_DB), VOLUME_GAIN_MAX_DB)
  const steps = Math.round(clamped / VOLUME_GAIN_STEP_DB)
  // 二次收敛：极端值四舍五入后可能越过边界。
  return Math.min(Math.max(steps * VOLUME_GAIN_STEP_DB, VOLUME_GAIN_MIN_DB), VOLUME_GAIN_MAX_DB)
}

/** 单个 lane 草稿：lane 下标 + 目标增益 + 是否参与本次提交（默认 false = 未编辑）。 */
export interface LaneGainDraft {
  lane: number
  gain_db: number
  included: boolean
}

/** 按 lane 数创建草稿（全部 0 dB、未勾选；0 dB 是"未编辑"的显式中性值，不代表当前实际增益）。 */
export function createLaneDrafts(laneCount: number): LaneGainDraft[] {
  const count = Number.isFinite(laneCount) ? Math.min(Math.max(Math.trunc(laneCount), 0), MAX_LANES) : 0
  const drafts: LaneGainDraft[] = []
  for (let lane = 0; lane < count; lane += 1) {
    drafts.push({ lane, gain_db: 0, included: false })
  }
  return drafts
}

/** 从草稿构造提交用的 gains_db 补丁（只含 included 的 lane；顺序按 lane 升序）。 */
export function buildGainPatch(drafts: readonly LaneGainDraft[]): GainEntry[] {
  return drafts
    .filter((draft) => draft.included)
    .map((draft) => ({ lane: draft.lane, gain_db: normalizeGainDb(draft.gain_db) }))
    .sort((a, b) => a.lane - b.lane)
}

/**
 * 校验补丁（前端 error 码，供 i18n 文案映射；null = 合法）：
 * - empty：没有勾选任何 lane；- lane：lane 越界/重复；- gain：增益非有限/越界。
 */
export function validateGainPatch(gains: readonly GainEntry[], laneCount: number): string | null {
  if (gains.length === 0) {
    return 'empty'
  }
  const seen = new Set<number>()
  for (const entry of gains) {
    if (!Number.isInteger(entry.lane) || entry.lane < 0 || entry.lane >= laneCount) {
      return 'lane'
    }
    if (seen.has(entry.lane)) {
      return 'lane'
    }
    seen.add(entry.lane)
    if (
      typeof entry.gain_db !== 'number' ||
      !Number.isFinite(entry.gain_db) ||
      entry.gain_db < VOLUME_GAIN_MIN_DB ||
      entry.gain_db > VOLUME_GAIN_MAX_DB
    ) {
      return 'gain'
    }
  }
  return null
}

/* ------------------------------------------------------------ 语义界面补丁校验 */

/**
 * 校验 routing 补丁（前端 error 码；null = 合法）：
 * - empty：routes 为空；- entry：表项不是 [lane, dac] 整数对；- duplicate：同一 lane 出现两次；
 * - dac_channels：dac_channels 不是正整数；- lane/dac 越界（只拒绝负数；上界由后端准入决定）。
 */
export function validateRoutingPatch(patch: RoutingPatch): string | null {
  const routes = patch.routes
  if (!Array.isArray(routes) || routes.length === 0) {
    return 'empty'
  }
  const seen = new Set<number>()
  for (const entry of routes) {
    if (!Array.isArray(entry) || entry.length !== 2) {
      return 'entry'
    }
    const [lane, dac] = entry
    if (!Number.isInteger(lane) || !Number.isInteger(dac) || lane < 0 || dac < 0) {
      return 'entry'
    }
    if (seen.has(lane)) {
      return 'duplicate'
    }
    seen.add(lane)
  }
  if (patch.dac_channels !== undefined) {
    if (!Number.isInteger(patch.dac_channels) || patch.dac_channels <= 0) {
      return 'dac_channels'
    }
  }
  return null
}

/**
 * 校验 layout 补丁（前端 error 码；null = 合法）：
 * - empty：folds 为空；- entry：折叠项不是 [src, dst, gain] 且 gain 非有限；- channels：in_channels/out_lanes 非正整数。
 */
export function validateLayoutPatch(patch: LayoutPatch): string | null {
  if (!Number.isInteger(patch.in_channels) || patch.in_channels <= 0) {
    return 'channels'
  }
  if (!Number.isInteger(patch.out_lanes) || patch.out_lanes <= 0) {
    return 'channels'
  }
  if (!Array.isArray(patch.folds) || patch.folds.length === 0) {
    return 'empty'
  }
  for (const entry of patch.folds) {
    if (!Array.isArray(entry) || entry.length !== 3) {
      return 'entry'
    }
    const [src, dst, gain] = entry
    if (!Number.isInteger(src) || !Number.isInteger(dst) || src < 0 || dst < 0) {
      return 'entry'
    }
    if (typeof gain !== 'number' || !Number.isFinite(gain)) {
      return 'entry'
    }
  }
  return null
}

/** 构造 routing 补丁（过滤出 [lane, dac] 整数对，保持输入顺序）。 */
export function buildRoutingPatch(routes: readonly (readonly [number, number])[], dacChannels?: number): RoutingPatch {
  const patch: RoutingPatch = { routes: routes.map(([lane, dac]) => [lane, dac] as [number, number]) }
  if (typeof dacChannels === 'number') {
    patch.dac_channels = dacChannels
  }
  return patch
}

/** 构造 layout 补丁（保持输入顺序）。 */
export function buildLayoutPatch(
  inChannels: number,
  outLanes: number,
  folds: readonly (readonly [number, number, number])[]
): LayoutPatch {
  return {
    in_channels: inChannels,
    out_lanes: outLanes,
    folds: folds.map(([src, dst, gain]) => [src, dst, gain] as [number, number, number])
  }
}

/* ---------------------------------------------------------------------- 端点 */

export interface ControlApi {
  /** POST /api/mute：立即生效（安全操作），返回后端回读的 mute。 */
  setMute(mute: boolean): Promise<ControlMuteAck>
  /** POST /api/surface {surface:'volume', data:{gains_db}}：只准备草稿，commit 才生效。 */
  prepareVolume(gains: readonly GainEntry[]): Promise<ControlDraft>
  /** POST /api/surface {surface:'routing', data:{routes,dac_channels?}}：只准备草稿。 */
  prepareRouting(patch: RoutingPatch): Promise<ControlDraft>
  /** POST /api/surface {surface:'layout', data:{in_channels,out_lanes,folds}}：只准备草稿。 */
  prepareLayout(patch: LayoutPatch): Promise<ControlDraft>
  /** POST /api/surface {surface:'mode', data:{mode}}：只准备草稿。 */
  prepareMode(mode: RunMode): Promise<ControlDraft>
  /** POST /api/format {sample_rate?, chunksize?}：只准备草稿。 */
  prepareFormat(patch: FormatPatch): Promise<ControlDraft>
  /** POST /api/commit {engine_token}：原子应用。 */
  commit(engineToken: string): Promise<ControlCommitAck>
  /** POST /api/abort {engine_token}：丢弃草稿。 */
  abort(engineToken: string): Promise<ControlAbortAck>
  /** POST /api/device/retry：让设备监督器跳过退避立即重试一次（回读速率）。 */
  retryDevice(): Promise<DeviceRetryAck>
}

/** 用共享 ApiClient 构造控制面 API（单测注入 fetchImpl 即可脱离网络）。 */
export function createControlApi(client: ApiClient): ControlApi {
  return {
    async setMute(mute: boolean): Promise<ControlMuteAck> {
      const body = await client.post<unknown>(CONTROL_PATHS.mute, { mute })
      return parseControlMuteAck(body)
    },
    async prepareVolume(gains: readonly GainEntry[]): Promise<ControlDraft> {
      const body = await client.post<unknown>(CONTROL_PATHS.surface, {
        surface: 'volume',
        data: { gains_db: gains.map((entry) => ({ lane: entry.lane, gain_db: entry.gain_db })) }
      })
      return parseControlDraft(body)
    },
    async prepareRouting(patch: RoutingPatch): Promise<ControlDraft> {
      const data: Record<string, unknown> = {
        routes: patch.routes.map(([lane, dac]) => [lane, dac])
      }
      if (patch.dac_channels !== undefined) {
        data.dac_channels = patch.dac_channels
      }
      const body = await client.post<unknown>(CONTROL_PATHS.surface, { surface: 'routing', data })
      return parseControlDraft(body)
    },
    async prepareLayout(patch: LayoutPatch): Promise<ControlDraft> {
      const body = await client.post<unknown>(CONTROL_PATHS.surface, {
        surface: 'layout',
        data: {
          in_channels: patch.in_channels,
          out_lanes: patch.out_lanes,
          folds: patch.folds.map(([src, dst, gain]) => [src, dst, gain])
        }
      })
      return parseControlDraft(body)
    },
    async prepareMode(mode: RunMode): Promise<ControlDraft> {
      const body = await client.post<unknown>(CONTROL_PATHS.surface, {
        surface: 'mode',
        data: { mode }
      })
      return parseControlDraft(body)
    },
    async prepareFormat(patch: FormatPatch): Promise<ControlDraft> {
      const body = await client.post<unknown>(CONTROL_PATHS.format, { ...patch })
      return parseControlDraft(body)
    },
    async commit(engineToken: string): Promise<ControlCommitAck> {
      const body = await client.post<unknown>(CONTROL_PATHS.commit, { engine_token: engineToken })
      return parseControlCommitAck(body)
    },
    async abort(engineToken: string): Promise<ControlAbortAck> {
      const body = await client.post<unknown>(CONTROL_PATHS.abort, { engine_token: engineToken })
      return parseControlAbortAck(body)
    },
    async retryDevice(): Promise<DeviceRetryAck> {
      const body = await client.post<unknown>(CONTROL_PATHS.deviceRetry, {})
      return parseDeviceRetryAck(body)
    }
  }
}

/* ---------------------------------------------------------------- 错误归类 */

export type ControlErrorCategory =
  | 'network'
  | 'timeout'
  | 'unauthorized'
  | 'host_link_unavailable'
  | 'unsupported'
  | 'not_wired'
  | 'no_base_plan'
  | 'token_invalid'
  | 'admission_rejected'
  | 'no_history'
  | 'not_found'
  | 'invalid_request'
  | 'conflict'
  | 'server'
  | 'invalid_response'
  | 'unknown'

export interface ControlErrorInfo {
  category: ControlErrorCategory
  status: number
  code: string
  /** 主文案 i18n 键（zh-CN / en-US 对等）。 */
  i18nKey: string
  /** 后端 message / 异常消息（仅详情展示）。 */
  detail?: string
  /** 是否值得重试（401 不重试：会话失效应去登录）。 */
  retryable: boolean
}

const CONTROL_ERROR_KEYS: Record<ControlErrorCategory, string> = {
  network: 'error.control.network',
  timeout: 'error.control.timeout',
  unauthorized: 'error.control.unauthorized',
  host_link_unavailable: 'error.control.hostLinkUnavailable',
  unsupported: 'error.control.unsupported',
  not_wired: 'error.control.notWired',
  no_base_plan: 'error.control.noBasePlan',
  token_invalid: 'error.control.tokenInvalid',
  admission_rejected: 'error.control.admissionRejected',
  no_history: 'error.control.noHistory',
  not_found: 'error.control.notFound',
  invalid_request: 'error.control.invalidRequest',
  conflict: 'error.control.conflict',
  server: 'error.control.server',
  invalid_response: 'error.control.invalidResponse',
  unknown: 'error.control.unknown'
}

/** 稳定 code -> 类别（与 daemon 控制面错误码映射一致；未知 4xx 归 conflict，未知 5xx 归 server）。 */
const CONTROL_CODE_CATEGORIES: Record<string, ControlErrorCategory> = {
  host_link_unavailable: 'host_link_unavailable',
  unsupported: 'unsupported',
  not_implemented: 'unsupported',
  not_wired: 'not_wired',
  no_base_plan: 'no_base_plan',
  engine_token_invalid: 'token_invalid',
  admission_rejected: 'admission_rejected',
  no_history: 'no_history',
  runtime_not_running: 'conflict',
  not_applied: 'conflict',
  bad_request: 'invalid_request',
  invalid_request: 'invalid_request',
  invalid_response: 'invalid_response',
  not_found: 'not_found',
  upload_not_found: 'not_found',
  switch_busy: 'conflict',
  reconfiguring: 'conflict',
  cal_busy: 'conflict',
  request_timeout: 'timeout',
  network_error: 'network'
}

/** 把控制面失败归类为 UI 可分支的稳定类别 + 本地化文案键。 */
export function classifyControlError(error: unknown): ControlErrorInfo {
  if (!(error instanceof ApiError)) {
    const detail = error instanceof Error ? error.message : undefined
    return {
      category: 'unknown',
      status: 0,
      code: 'unknown_error',
      i18nKey: CONTROL_ERROR_KEYS.unknown,
      ...(detail ? { detail } : {}),
      retryable: true
    }
  }
  let category: ControlErrorCategory
  if (error.code === 'request_timeout') {
    category = 'timeout'
  } else if (error.code === 'network_error' || error.status === 0) {
    category = 'network'
  } else if (error.status === 401) {
    category = 'unauthorized'
  } else if (error.code === 'invalid_response') {
    category = 'invalid_response'
  } else {
    category = CONTROL_CODE_CATEGORIES[error.code] ?? (error.status >= 500 ? 'server' : error.status === 404 ? 'not_found' : 'conflict')
  }
  const detail = error.message.length > 0 ? error.message : undefined
  return {
    category,
    status: error.status,
    code: error.code,
    i18nKey: CONTROL_ERROR_KEYS[category],
    ...(detail ? { detail } : {}),
    retryable: category !== 'unauthorized' && category !== 'invalid_response'
  }
}
