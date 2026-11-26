/**
 * M4 REST 客户端（任务 T54）：声道布局/映射读取 + 方案上传/历史/回滚 + 两阶段提交。
 *
 * 契约来源（T54/T55 任务契约；后端由 T55 并行实现，前端用契约 mock + 单测开发）：
 * - GET  /api/channels      -> khaen-channels/v1（output/input/mapping/reasons/not_wired；字段缺失为 null + reason）
 * - PUT  /api/channels      -> 两阶段 prepare（校验并返回 engine_token/planned diff；M3b 热重配未完成时 commit 501）
 * - POST /api/commit        -> 共享两阶段提交（channels 草稿在 M3b 前返回 501 host_link_unsupported）
 * - POST /api/abort         -> 丢弃草稿
 * - POST /api/plan/upload   -> 原始 plan 字节（≤ 4 MiB）→ {upload_id?, sha256, graph_id, version, segments, latency_samples}
 * - POST /api/prepare       -> {upload_id | plan_path, sha256} → 方案草稿（engine_token/planned_version/graph_id）
 * - POST /api/rollback      -> {target?} → 回滚草稿（rolled_back_to）；仍需 commit 生效
 * - GET  /api/plan/history  -> 已生效历史（版本/图/来源/时间）
 * - GET  /api/plan/inspect  -> 当前/上传方案的只读预览（失败时页面降级为上传摘要，不伪造）
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
  dac_map: number[] | null
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
  mapping: ChannelMappingEntry[]
  reasons: Record<string, string>
  not_wired: ChannelsNotWiredEntry[]
}

/** 方案上传回执（T54 契约；upload_id 可能与 sha256 相同，缺失时用 sha256）。 */
export interface PlanUploadAck {
  upload_id: string | null
  sha256: string
  graph_id: string | null
  version: number | null
  segments: number | null
  latency_samples: number | null
}

/** 已生效方案历史条目。 */
export interface PlanHistoryEntry {
  version: number
  graph_id: string | null
  sha256: string | null
  applied_at: string | null
  source: string | null
}

/** GET /api/plan/history 归一化响应。 */
export interface PlanHistory {
  current_version: number | null
  versions: PlanHistoryEntry[]
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

function parseMapping(value: unknown): ChannelMappingEntry[] {
  if (value === null || value === undefined) {
    return []
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
      dac_map: readNumberArray(output, 'dac_map', 'output')
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

/** 校验并归一化 POST /api/plan/upload 响应（sha256 必须是 64 位小写十六进制）。 */
export function parsePlanUploadAck(body: unknown): PlanUploadAck {
  const record = asRecord(body)
  if (record === null) {
    throw invalidResponse('plan upload body must be an object')
  }
  const sha = readString(record, 'sha256', 'upload')
  if (sha === null || !/^[0-9a-f]{64}$/.test(sha)) {
    throw invalidResponse('sha256 must be a 64-char lowercase hex string')
  }
  return {
    upload_id: readString(record, 'upload_id', 'upload'),
    sha256: sha,
    graph_id: readString(record, 'graph_id', 'upload'),
    version: readNumber(record, 'version', 'upload'),
    segments: readNumber(record, 'segments', 'upload'),
    latency_samples: readNumber(record, 'latency_samples', 'upload')
  }
}

/** 校验并归一化 GET /api/plan/history 响应（兼容数组 / versions / history / items 形态）。 */
export function parsePlanHistory(body: unknown): PlanHistory {
  let rawVersions: unknown
  let currentVersion: number | null = null
  if (Array.isArray(body)) {
    rawVersions = body
  } else {
    const record = asRecord(body)
    if (record === null) {
      throw invalidResponse('plan history body must be an object or array')
    }
    rawVersions = record.versions ?? record.history ?? record.items
    currentVersion = readNumber(record, 'current_version', 'history') ?? readNumber(record, 'current', 'history')
  }
  if (!Array.isArray(rawVersions)) {
    throw invalidResponse('history versions must be an array')
  }
  const versions = rawVersions.map((item, index) => {
    const path = 'versions[' + String(index) + ']'
    const record = asRecord(item)
    if (record === null) {
      throw invalidResponse(path + ' must be an object')
    }
    const version = readNumber(record, 'version', path)
    if (version === null || !Number.isInteger(version)) {
      throw invalidResponse(path + '.version must be an integer')
    }
    return {
      version,
      graph_id: readString(record, 'graph_id', path),
      sha256: readString(record, 'sha256', path),
      applied_at: readString(record, 'applied_at', path),
      source: readString(record, 'source', path)
    }
  })
  versions.sort((a, b) => a.version - b.version)
  if (currentVersion === null && versions.length > 0) {
    currentVersion = versions[versions.length - 1].version
  }
  return { current_version: currentVersion, versions }
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

/** 已知位置的 i18n 标签键；未知位置返回 null（页面显示输出序号，不猜）。 */
export function motuOutputLabelKey(channel: number): string | null {
  return MOTU_OUTPUT_LABEL_KEYS[channel] ?? null
}

/** 通道映射 [0,1] -> "0,1"；null/空返回 null。 */
export function formatChannelMap(map: readonly number[] | null | undefined): string | null {
  if (!map || map.length === 0) {
    return null
  }
  return map.join(',')
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
  fetchPlanInspect(): Promise<PlanInspect>
  /** 原始 plan 字节上传（≤ PLAN_UPLOAD_MAX_BYTES；X-KHAEN-Filename 只作提示）。 */
  uploadPlan(bytes: Uint8Array, filename: string): Promise<PlanUploadAck>
  /** 用已上传的 upload_id/sha256 准备方案（两阶段：commit 才生效）。 */
  preparePlan(upload: { upload_id: string; sha256: string }): Promise<ControlDraft>
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
    async fetchPlanInspect(): Promise<PlanInspect> {
      const body = await client.get<unknown>(CHANNELS_PATHS.planInspect)
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
    async preparePlan(upload: { upload_id: string; sha256: string }): Promise<ControlDraft> {
      const body = await client.post<unknown>(CHANNELS_PATHS.planPrepare, {
        upload_id: upload.upload_id,
        sha256: upload.sha256
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
