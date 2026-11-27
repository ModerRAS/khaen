/**
 * 麦克风 profile（真实校准桥接 C1/C4）的 REST 客户端与严格解析。
 *
 * 数据纪律：
 * - 一切来自 daemon GET /api/calibration/mic、POST /api/calibration/mic/calibration、
 *   PUT /api/calibration/mic、GET /api/calibration/mic/status；前端不自己算 sha256/不伪造解析结果；
 * - 响应缺字段/类型不符一律抛 invalid_response（ApiError），不静默补 null；
 * - rt-host 未接线时 status 的 rt_host.available=false + reason 原样透出，页面显示 NOT_AVAILABLE。
 */

import { ApiError, type ApiClient } from './client.ts'

/** MicProfile schema（与 daemon audio_profile/model.rs SCHEMA 同源）。 */
export const MIC_PROFILE_SCHEMA = 'khaen-mic-profile/v1'

/** UMIK 朝向（校准文件按 0°/90° 提供）。 */
export const MIC_ORIENTATIONS = ['0deg', '90deg'] as const
export type MicOrientation = (typeof MIC_ORIENTATIONS)[number]

/** 服务端解析出的校准文件摘要。 */
export interface MicCalParsed {
  serno: string
  sens_factor_db: number | null
  points: number
}

/** 持久化的麦克风 profile（daemon <state_dir>/calibration/mic.json）。 */
export interface MicProfile {
  schema: string
  device: string
  serial: string
  orientation: MicOrientation
  cal_sha256: string
  cal_filename: string
  cal_path: string
  parsed: MicCalParsed
  updated_at: string
}

/** GET /api/calibration/mic。profile=null 时 reasons 必非空（没有保存过 profile）。 */
export interface MicProfileResponse {
  ok: true
  schema: string
  profile: MicProfile | null
  reasons: string[]
}

/** POST /api/calibration/mic/calibration 的回执（文件已内容寻址保存）。 */
export interface MicCalUploadResponse {
  ok: true
  sha256: string
  filename: string
  bytes: number
  serial: string
  parsed: MicCalParsed
}

/** rt-host 实时 UMIK 状态；未接线时 available=false + reason。 */
export interface MicRtHostStatus {
  available: boolean
  reason: string | null
}

/** GET /api/calibration/mic/status（恒 200，只读）。 */
export interface MicStatusResponse extends MicProfileResponse {
  rt_host: MicRtHostStatus
}

/** PUT /api/calibration/mic 的请求体（未知字段由后端 400）。 */
export interface MicProfileUpdate {
  device?: string
  serial?: string
  orientation?: MicOrientation
  cal_sha256?: string
}

export interface MicRequestOptions {
  signal?: AbortSignal
}

export interface MicCalUploadOptions extends MicRequestOptions {
  /** 原始文本 MIME；缺省 text/plain; charset=utf-8。 */
  contentType?: string
  /** 文件名提示（X-KHAEN-FILENAME）；缺省 umik.txt。 */
  filename?: string
}

export interface MicApi {
  getProfile(options?: MicRequestOptions): Promise<MicProfileResponse>
  putProfile(update: MicProfileUpdate, options?: MicRequestOptions): Promise<MicProfileResponse>
  uploadCalibration(body: string | ArrayBuffer | Uint8Array, options?: MicCalUploadOptions): Promise<MicCalUploadResponse>
  getStatus(options?: MicRequestOptions): Promise<MicStatusResponse>
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function invalid(path: string, message: string): ApiError {
  return new ApiError({
    status: 200,
    code: 'invalid_response',
    message: path + ': ' + message,
    details: { path }
  })
}

function readRecord(value: unknown, path: string): Record<string, unknown> {
  if (!isRecord(value)) {
    throw invalid(path, 'expected an object')
  }
  return value
}

function readString(value: unknown, path: string): string {
  if (typeof value !== 'string') {
    throw invalid(path, 'expected a string')
  }
  return value
}

function readNumber(value: unknown, path: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw invalid(path, 'expected a finite number')
  }
  return value
}

function readInteger(value: unknown, path: string): number {
  const number = readNumber(value, path)
  if (!Number.isInteger(number)) {
    throw invalid(path, 'expected an integer')
  }
  return number
}

function readBoolean(value: unknown, path: string): boolean {
  if (typeof value !== 'boolean') {
    throw invalid(path, 'expected a boolean')
  }
  return value
}

function readStringArray(value: unknown, path: string): string[] {
  if (!Array.isArray(value)) {
    throw invalid(path, 'expected an array of strings')
  }
  return value.map((item, index) => readString(item, path + '[' + index + ']'))
}

function readOrientation(value: unknown, path: string): MicOrientation {
  const text = readString(value, path)
  if (!(MIC_ORIENTATIONS as readonly string[]).includes(text)) {
    throw invalid(path, 'expected one of ' + MIC_ORIENTATIONS.join('/') + ', got ' + JSON.stringify(text))
  }
  return text as MicOrientation
}

function readSha256(value: unknown, path: string): string {
  const text = readString(value, path)
  if (!/^[0-9a-f]{64}$/.test(text)) {
    throw invalid(path, 'expected a lowercase sha256 hex digest')
  }
  return text
}

function readParsed(value: unknown, path: string): MicCalParsed {
  const record = readRecord(value, path)
  const serno = readString(record.serno, path + '.serno')
  const points = readInteger(record.points, path + '.points')
  if (points < 1) {
    throw invalid(path + '.points', 'expected at least one calibration point')
  }
  const sens = record.sens_factor_db
  if (sens !== null && (typeof sens !== 'number' || !Number.isFinite(sens))) {
    throw invalid(path + '.sens_factor_db', 'expected a finite number or null')
  }
  return { serno, sens_factor_db: sens as number | null, points }
}

/** 严格解析单个 MicProfile；不需要的字段也必须是正确类型（不静默补值）。 */
export function parseMicProfile(value: unknown, path = 'profile'): MicProfile {
  const record = readRecord(value, path)
  const schema = readString(record.schema, path + '.schema')
  if (schema !== MIC_PROFILE_SCHEMA) {
    throw invalid(path + '.schema', 'unsupported schema ' + JSON.stringify(schema))
  }
  return {
    schema,
    device: readString(record.device, path + '.device'),
    serial: readString(record.serial, path + '.serial'),
    orientation: readOrientation(record.orientation, path + '.orientation'),
    cal_sha256: readSha256(record.cal_sha256, path + '.cal_sha256'),
    cal_filename: readString(record.cal_filename, path + '.cal_filename'),
    cal_path: readString(record.cal_path, path + '.cal_path'),
    parsed: readParsed(record.parsed, path + '.parsed'),
    updated_at: readString(record.updated_at, path + '.updated_at')
  }
}

/** 解析 GET/PUT /api/calibration/mic。 */
export function parseMicProfileResponse(value: unknown, path = 'mic'): MicProfileResponse {
  const record = readRecord(value, path)
  if (record.ok !== true) {
    throw invalid(path + '.ok', 'expected true')
  }
  const schema = readString(record.schema, path + '.schema')
  if (schema !== MIC_PROFILE_SCHEMA) {
    throw invalid(path + '.schema', 'unsupported schema ' + JSON.stringify(schema))
  }
  const profile = record.profile === null ? null : parseMicProfile(record.profile, path + '.profile')
  const reasons = readStringArray(record.reasons, path + '.reasons')
  if (profile === null && reasons.length === 0) {
    throw invalid(path + '.reasons', 'a missing profile must come with a reason')
  }
  return { ok: true, schema, profile, reasons }
}

/** 解析 POST /api/calibration/mic/calibration。 */
export function parseMicCalUploadResponse(value: unknown, path = 'mic_cal'): MicCalUploadResponse {
  const record = readRecord(value, path)
  if (record.ok !== true) {
    throw invalid(path + '.ok', 'expected true')
  }
  const bytes = readInteger(record.bytes, path + '.bytes')
  if (bytes < 1) {
    throw invalid(path + '.bytes', 'expected at least one byte')
  }
  return {
    ok: true,
    sha256: readSha256(record.sha256, path + '.sha256'),
    filename: readString(record.filename, path + '.filename'),
    bytes,
    serial: readString(record.serial, path + '.serial'),
    parsed: readParsed(record.parsed, path + '.parsed')
  }
}

/** 解析 GET /api/calibration/mic/status。 */
export function parseMicStatusResponse(value: unknown, path = 'mic_status'): MicStatusResponse {
  const base = parseMicProfileResponse(value, path)
  const record = readRecord(value, path)
  const rtHost = readRecord(record.rt_host, path + '.rt_host')
  const available = readBoolean(rtHost.available, path + '.rt_host.available')
  const reason = rtHost.reason === null ? null : readString(rtHost.reason, path + '.rt_host.reason')
  if (!available && reason === null) {
    throw invalid(path + '.rt_host.reason', 'an unavailable rt-host must come with a reason')
  }
  return { ...base, rt_host: { available, reason } }
}

function bytesOf(body: string | ArrayBuffer | Uint8Array): string | ArrayBuffer {
  if (typeof body === 'string') {
    if (body.length === 0) {
      throw new ApiError({ status: 400, code: 'invalid_request', message: 'the calibration file is empty' })
    }
    return body
  }
  if (body instanceof ArrayBuffer) {
    if (body.byteLength === 0) {
      throw new ApiError({ status: 400, code: 'invalid_request', message: 'the calibration file is empty' })
    }
    return body
  }
  if (body instanceof Uint8Array) {
    if (body.byteLength === 0) {
      throw new ApiError({ status: 400, code: 'invalid_request', message: 'the calibration file is empty' })
    }
    // BodyInit 不接受 Uint8Array<ArrayBufferLike>；复制成独立 ArrayBuffer，上传后不再引用视图。
    const copy = new Uint8Array(body.byteLength)
    copy.set(body)
    return copy.buffer
  }
  throw new ApiError({ status: 400, code: 'invalid_request', message: 'unsupported calibration body type' })
}

/** 创建麦克风 profile API；测试可注入假 client。 */
export function createMicApi(client: Pick<ApiClient, 'get' | 'put' | 'postRaw'>): MicApi {
  return {
    async getProfile(options = {}) {
      const body = await client.get<unknown>('/api/calibration/mic', { signal: options.signal })
      return parseMicProfileResponse(body)
    },
    async putProfile(update, options = {}) {
      const body = await client.put<unknown>('/api/calibration/mic', update, { signal: options.signal })
      return parseMicProfileResponse(body)
    },
    async uploadCalibration(body, options = {}) {
      const payload = bytesOf(body)
      const response = await client.postRaw<unknown>('/api/calibration/mic/calibration', payload, {
        contentType: options.contentType ?? 'text/plain; charset=utf-8',
        headers: { 'X-KHAEN-FILENAME': options.filename ?? 'umik.txt' },
        signal: options.signal
      })
      return parseMicCalUploadResponse(response)
    },
    async getStatus(options = {}) {
      const body = await client.get<unknown>('/api/calibration/mic/status', { signal: options.signal })
      return parseMicStatusResponse(body)
    }
  }
}
