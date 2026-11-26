/**
 * 校准 REST 客户端（PLAN §4.7 job 列表/详情/uploads/artifacts + §10.4 wizard 端点；任务 T37）。
 *
 * 契约（后端由 T36 并行实现，前端按此契约 + mock 开发；字段见 webui/README.md「校准契约」）：
 *   GET  /api/calibration/jobs?limit&offset        -> khaen-calibration-jobs/v1（列表/分页/active job）
 *   POST /api/calibration/jobs                    -> 202 {job_id}（同一时刻只允许一个 active job，冲突 409 switch_busy）
 *   GET  /api/calibration/jobs/{id}               -> khaen-calibration-job/v1（状态机/步骤/产物/verdict）
 *   POST /api/calibration/jobs/{id}/cancel        -> {job_id,status}
 *   POST /api/calibration/jobs/{id}/apply         -> 应用结果（applied/applied_version/graph_id/snapshot/readback）
 *   POST /api/calibration/jobs/{id}/rollback      -> 回滚结果（rolled_back/restored_version/graph_id）
 *   POST /api/calibration/uploads                 -> {upload_id,sha256,bytes}（原始 WAV/校准文件字节 body）
 *   GET  /api/calibration/jobs/{id}/artifacts/{name} -> 产物下载（浏览器直链；daemon 侧按 manifest sha256 校验）
 *   GET  /api/calibration/wizard                  -> khaen-calibration-wizard/v1（配置 + 默认值）
 *   PUT  /api/calibration/wizard                  -> 保存配置（未知字段后端 400）
 *   GET  /api/calibration/wizard/state            -> khaen-calibration-wizard-state/v1（每步 gate，刷新可恢复）
 *   POST /api/calibration/wizard/apply            -> 应用最近成功 job 的 plan（走 prepare/commit）
 *
 * 纪律：
 * - **gate 由后端事实驱动**：每步能否继续只认 wizard/state 的 steps[].status（passed/blocked/not_available）；
 *   前端不自行把任何步骤判成 passed（缺失/畸形 steps 一律按 invalid_response 拒绝，绝不"默认放行"）。
 * - 响应做运行时校验：schema 不符、枚举未知、类型漂移 -> ApiError(code=invalid_response)，页面显示错误 + 重试。
 * - 后端未接入（503 not_wired）如实映射成「未接入」类别，页面显示 NOT_AVAILABLE + 原因/任务号，不显示假数据。
 * - 上传原始字节用 client.postRaw（Content-Type 由调用方给）；sha256/bytes 只信服务端返回值。
 * - 纯函数（解析/派生/错误归类）不依赖 Vue / DOM，Node 单测直接覆盖。
 */
import { ApiClient, ApiError } from './client.ts'
import type { TruthKind } from '../types/ui.ts'

/** 端点 schema 标识（与 daemon 契约一致）。 */
export const CALIBRATION_JOBS_SCHEMA = 'khaen-calibration-jobs/v1'
export const CALIBRATION_JOB_SCHEMA = 'khaen-calibration-job/v1'
export const CALIBRATION_WIZARD_SCHEMA = 'khaen-calibration-wizard/v1'
export const CALIBRATION_WIZARD_STATE_SCHEMA = 'khaen-calibration-wizard-state/v1'

/** 校准 REST 单次请求超时（轮询用短超时；上传用更长的超时）。 */
export const CALIBRATION_FETCH_TIMEOUT_MS = 12000
export const CALIBRATION_UPLOAD_TIMEOUT_MS = 120000
/** job 状态轮询间隔（PLAN §10.3：进度由真实 job 状态驱动，不做假进度）。 */
export const JOB_POLL_INTERVAL_MS = 1500
/** 列表默认分页大小。 */
export const JOBS_PAGE_SIZE = 20

/** 校准后端未接入时的稳定错误码（daemon 503 / GET 列表 reason）。 */
export const NOT_WIRED_CODE = 'not_wired'

/* ------------------------------------------------------------------ 类型 */

export type CalibrationJobStatus = 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled'
export type CalibrationVerdict = 'PASS' | 'FAIL' | 'INCOMPLETE'
export type CalibrationStepStatus = 'pending' | 'running' | 'succeeded' | 'failed' | 'cancelled' | 'skipped'
export type DesignResidual = 'FULL' | 'PARTIAL' | 'NONE'
export type WizardGateStatus = 'passed' | 'blocked' | 'not_available'
export type WizardStepKey =
  | 'device_check'
  | 'layout_channels'
  | 'mic_settings'
  | 'positions'
  | 'measurement'
  | 'analysis'
  | 'design'
  | 'review_apply'

/** 8 步向导的固定顺序（PLAN §10.2 表格；后端 steps 必须逐项对上）。 */
export const WIZARD_STEP_KEYS: readonly WizardStepKey[] = [
  'device_check',
  'layout_channels',
  'mic_settings',
  'positions',
  'measurement',
  'analysis',
  'design',
  'review_apply'
]

export const WIZARD_STEP_COUNT = WIZARD_STEP_KEYS.length

/** 校准 job 的固定步骤（PLAN §4.7：compile→levels→closed_loop→bundle，不允许跳步直接成功）。 */
export const JOB_STEP_NAMES = ['compile', 'levels', 'closed_loop', 'bundle'] as const

export interface CalibrationErrorDetail {
  code: string
  message: string
}

export interface CalibrationJobSummary {
  id: string
  kind: string
  status: CalibrationJobStatus
  verdict: CalibrationVerdict | null
  created_at: string | null
  started_at: string | null
  finished_at: string | null
  duration_ms: number | null
  error: CalibrationErrorDetail | null
  reason: string | null
}

export interface CalibrationJobsResponse {
  schema: typeof CALIBRATION_JOBS_SCHEMA
  /** false = daemon 校准后端未接入（列表为空 + reason），页面显示 NOT_AVAILABLE。 */
  available: boolean
  reason: string | null
  /** 引擎真实性原样透传（如 real_audio / synthetic / not_wired）；UI 据此打 TruthTag。 */
  engine: string | null
  active_job_id: string | null
  jobs: CalibrationJobSummary[]
  total: number
  limit: number
  offset: number
}

export interface CalibrationStep {
  name: string
  status: CalibrationStepStatus
  started_at: string | null
  finished_at: string | null
  sha256: string | null
  detail: string | null
}

export interface CalibrationArtifact {
  name: string
  sha256: string | null
  bytes: number | null
  content_type: string | null
}

export interface CalibrationJobInputs {
  plan_sha: string | null
  baseline_upload_id: string | null
  applied_upload_id: string | null
  centers_hz: number[]
  tolerance_db: number | null
  min_bands: number | null
}

export interface CalibrationBand {
  hz: number
  predicted_db: number | null
  observed_db: number | null
  delta_db: number | null
  missing: boolean
}

export interface CalibrationDesign {
  residual: DesignResidual
  admission: {
    accepted: boolean
    code: string | null
    reason: string | null
  } | null
}

export interface CalibrationJobResult {
  verdict: CalibrationVerdict | null
  max_delta_db: number | null
  compared_bands: number | null
  missing_bands: number | null
  violations: string[]
  bands: CalibrationBand[]
  design: CalibrationDesign | null
  plan: { sha256: string | null; graph_id: string | null } | null
}

/** job 的 apply 状态（daemon 在 detail 里上报，列表不带）。 */
export type CalibrationApplyState = 'not_applied' | 'applied' | 'rolled_back'

export interface CalibrationJob extends CalibrationJobSummary {
  /** 引擎真实性原样透传（真实闭环 = actual；mock/fixture = demo）。 */
  engine: string | null
  /** 该 job 产出的 plan 的应用状态（not_applied / applied / rolled_back）。 */
  apply_state: CalibrationApplyState | null
  /** 已应用版本（apply 后由 daemon 回填）。 */
  applied_version: number | null
  steps: CalibrationStep[]
  artifacts: CalibrationArtifact[]
  inputs: CalibrationJobInputs | null
  result: CalibrationJobResult | null
}

export interface CalibrationJobResponse {
  schema: typeof CALIBRATION_JOB_SCHEMA
  job: CalibrationJob
  reasons: Record<string, string>
}

export interface CalibrationUploadResult {
  upload_id: string
  sha256: string
  bytes: number
}

export interface CalibrationCreateJobInputs {
  plan_sha: string
  baseline_upload_id: string
  applied_upload_id: string
  centers_hz: number[]
  tolerance_db: number
  min_bands: number
}

export interface CalibrationCreateJobRequest {
  kind: 'single_point_offline'
  inputs: CalibrationCreateJobInputs
}

export interface CalibrationCreateJobResult {
  job_id: string
  status: CalibrationJobStatus
}

export interface CalibrationApplyResult {
  applied: true
  job_id: string | null
  applied_version: number
  graph_id: string
  /** 控制面只暴露 version/graph_id 时可能为 null（reasons 里给原因），不当失败。 */
  sha256: string | null
  snapshot: { version: number; graph_id: string; sha256: string | null } | null
  ack: { synthetic: boolean; actual: boolean } | null
  readback: Record<string, unknown> | null
  applied_at: string | null
}

export interface CalibrationRollbackResult {
  rolled_back: true
  restored_version: number
  graph_id: string
  sha256: string | null
  readback: Record<string, unknown> | null
  rolled_back_at: string | null
}

export interface WizardSinglePoint {
  centers_hz: number[]
  tolerance_db: number
  min_bands: number
}

/** UMIK 朝向（daemon 只接受 0deg / 90deg；null = 未选择）。 */
export type WizardOrientation = '0deg' | '90deg'
/** 目标曲线模式（designer::BuiltinTarget 的真实 id：flat / tilt10；null = 未选择）。 */
export type WizardTargetMode = 'flat' | 'tilt10'

export interface WizardConfig {
  mic_cal_sha256: string | null
  mic_cal_filename: string | null
  orientation: WizardOrientation | null
  target_mode: WizardTargetMode | null
  single_point: WizardSinglePoint
}

/** PUT /wizard 的可选测量草稿扩展（daemon 允许；不传=不变）：刷新后恢复第 5 步的两段上传。 */
export interface WizardMeasurementDraft {
  baseline_upload_id: string | null
  applied_upload_id: string | null
  plan_sha: string | null
}

/** PUT /api/calibration/wizard 的请求体：config 字段 + 可选 measurement 草稿。 */
export type WizardSaveRequest = WizardConfig & { measurement?: WizardMeasurementDraft }

export interface CalibrationWizardResponse {
  schema: typeof CALIBRATION_WIZARD_SCHEMA
  config: WizardConfig
  defaults: WizardConfig
  saved: boolean
  updated_at: string | null
  reasons: Record<string, string>
}

export interface WizardStepGate {
  step: number
  key: WizardStepKey
  status: WizardGateStatus
  reason: string | null
  /** 缺失能力对应的后续任务/里程碑（如 M4 / P3 / T42），无则为 null。 */
  task: string | null
}

export interface WizardMeasurementState {
  baseline_upload_id: string | null
  applied_upload_id: string | null
  baseline_sha256: string | null
  applied_sha256: string | null
}

export interface WizardSnapshot {
  version: number
  graph_id: string
  sha256: string | null
}

export interface CalibrationWizardStateResponse {
  schema: typeof CALIBRATION_WIZARD_STATE_SCHEMA
  current_step: number
  steps: WizardStepGate[]
  active_job_id: string | null
  last_job_id: string | null
  measurement: WizardMeasurementState
  snapshot: WizardSnapshot | null
  updated_at: string | null
  reasons: Record<string, string>
}

/* ------------------------------------------------------------ 运行时校验 */

function invalidResponse(endpoint: string, detail: string): ApiError {
  return new ApiError({
    status: 200,
    code: 'invalid_response',
    message: 'unexpected response from ' + endpoint + ': ' + detail
  })
}

function asRecord(value: unknown, endpoint: string, path: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw invalidResponse(endpoint, path + ' must be a JSON object')
  }
  return value as Record<string, unknown>
}

function optionalRecord(value: unknown, endpoint: string, path: string): Record<string, unknown> | null {
  if (value === null || value === undefined) {
    return null
  }
  return asRecord(value, endpoint, path)
}

function readString(record: Record<string, unknown>, field: string, endpoint: string, path: string): string | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'string') {
    throw invalidResponse(endpoint, path + '.' + field + ' must be a string or null')
  }
  return value
}

function requireString(record: Record<string, unknown>, field: string, endpoint: string, path: string): string {
  const value = readString(record, field, endpoint, path)
  if (value === null || value.length === 0) {
    throw invalidResponse(endpoint, path + '.' + field + ' must be a non-empty string')
  }
  return value
}

function readNumber(record: Record<string, unknown>, field: string, endpoint: string, path: string): number | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw invalidResponse(endpoint, path + '.' + field + ' must be a finite number or null')
  }
  return value
}

function requireNumber(record: Record<string, unknown>, field: string, endpoint: string, path: string): number {
  const value = readNumber(record, field, endpoint, path)
  if (value === null) {
    throw invalidResponse(endpoint, path + '.' + field + ' must be a finite number')
  }
  return value
}

function readBoolean(record: Record<string, unknown>, field: string, endpoint: string, path: string): boolean | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'boolean') {
    throw invalidResponse(endpoint, path + '.' + field + ' must be a boolean or null')
  }
  return value
}

function readNumberArray(record: Record<string, unknown>, field: string, endpoint: string, path: string): number[] {
  const value = record[field]
  if (value === null || value === undefined) {
    return []
  }
  if (!Array.isArray(value)) {
    throw invalidResponse(endpoint, path + '.' + field + ' must be an array of numbers')
  }
  return value.map((item, index) => {
    if (typeof item !== 'number' || !Number.isFinite(item)) {
      throw invalidResponse(endpoint, path + '.' + field + '[' + String(index) + '] must be a finite number')
    }
    return item
  })
}

function readStringArray(record: Record<string, unknown>, field: string, endpoint: string, path: string): string[] {
  const value = record[field]
  if (value === null || value === undefined) {
    return []
  }
  if (!Array.isArray(value)) {
    throw invalidResponse(endpoint, path + '.' + field + ' must be an array of strings')
  }
  return value.map((item, index) => {
    if (typeof item !== 'string') {
      throw invalidResponse(endpoint, path + '.' + field + '[' + String(index) + '] must be a string')
    }
    return item
  })
}

function readEnum<T extends string>(
  record: Record<string, unknown>,
  field: string,
  allowed: readonly T[],
  endpoint: string,
  path: string
): T | null {
  const value = readString(record, field, endpoint, path)
  if (value === null) {
    return null
  }
  if (!(allowed as readonly string[]).includes(value)) {
    throw invalidResponse(endpoint, path + '.' + field + ' has unknown value "' + value + '"')
  }
  return value as T
}

function requireEnum<T extends string>(
  record: Record<string, unknown>,
  field: string,
  allowed: readonly T[],
  endpoint: string,
  path: string
): T {
  const value = readEnum(record, field, allowed, endpoint, path)
  if (value === null) {
    throw invalidResponse(endpoint, path + '.' + field + ' is required')
  }
  return value
}

function parseReasons(value: unknown, endpoint: string): Record<string, string> {
  if (value === null || value === undefined) {
    return {}
  }
  const record = asRecord(value, endpoint, 'reasons')
  const reasons: Record<string, string> = {}
  for (const [key, reason] of Object.entries(record)) {
    if (typeof reason !== 'string') {
      throw invalidResponse(endpoint, 'reasons.' + key + ' must be a string')
    }
    reasons[key] = reason
  }
  return reasons
}

const JOB_STATUSES: readonly CalibrationJobStatus[] = ['queued', 'running', 'succeeded', 'failed', 'cancelled']
const VERDICTS: readonly CalibrationVerdict[] = ['PASS', 'FAIL', 'INCOMPLETE']
const STEP_STATUSES: readonly CalibrationStepStatus[] = [
  'pending',
  'running',
  'succeeded',
  'failed',
  'cancelled',
  'skipped'
]
const GATE_STATUSES: readonly WizardGateStatus[] = ['passed', 'blocked', 'not_available']
const APPLY_STATES: readonly CalibrationApplyState[] = ['not_applied', 'applied', 'rolled_back']
const RESIDUALS: readonly DesignResidual[] = ['FULL', 'PARTIAL', 'NONE']
const ORIENTATIONS: readonly WizardOrientation[] = ['0deg', '90deg']
const TARGET_MODES: readonly WizardTargetMode[] = ['flat', 'tilt10']

function parseJobError(value: unknown, endpoint: string, path: string): CalibrationErrorDetail | null {
  const record = optionalRecord(value, endpoint, path)
  if (record === null) {
    return null
  }
  return {
    code: requireString(record, 'code', endpoint, path),
    message: readString(record, 'message', endpoint, path) ?? ''
  }
}

function parseJobSummary(value: unknown, endpoint: string, path: string): CalibrationJobSummary {
  const record = asRecord(value, endpoint, path)
  return {
    id: requireString(record, 'id', endpoint, path),
    kind: requireString(record, 'kind', endpoint, path),
    status: requireEnum(record, 'status', JOB_STATUSES, endpoint, path),
    verdict: readEnum(record, 'verdict', VERDICTS, endpoint, path),
    created_at: readString(record, 'created_at', endpoint, path),
    started_at: readString(record, 'started_at', endpoint, path),
    finished_at: readString(record, 'finished_at', endpoint, path),
    duration_ms: readNumber(record, 'duration_ms', endpoint, path),
    error: parseJobError(record.error, endpoint, path + '.error'),
    reason: readString(record, 'reason', endpoint, path)
  }
}

function parseJobStep(value: unknown, endpoint: string, path: string): CalibrationStep {
  const record = asRecord(value, endpoint, path)
  return {
    name: requireString(record, 'name', endpoint, path),
    status: requireEnum(record, 'status', STEP_STATUSES, endpoint, path),
    started_at: readString(record, 'started_at', endpoint, path),
    finished_at: readString(record, 'finished_at', endpoint, path),
    sha256: readString(record, 'sha256', endpoint, path),
    detail: readString(record, 'detail', endpoint, path)
  }
}

function parseArtifact(value: unknown, endpoint: string, path: string): CalibrationArtifact {
  const record = asRecord(value, endpoint, path)
  return {
    name: requireString(record, 'name', endpoint, path),
    sha256: readString(record, 'sha256', endpoint, path),
    bytes: readNumber(record, 'bytes', endpoint, path),
    content_type: readString(record, 'content_type', endpoint, path)
  }
}

function parseJobInputs(value: unknown, endpoint: string, path: string): CalibrationJobInputs | null {
  const record = optionalRecord(value, endpoint, path)
  if (record === null) {
    return null
  }
  return {
    plan_sha: readString(record, 'plan_sha', endpoint, path),
    baseline_upload_id: readString(record, 'baseline_upload_id', endpoint, path),
    applied_upload_id: readString(record, 'applied_upload_id', endpoint, path),
    centers_hz: readNumberArray(record, 'centers_hz', endpoint, path),
    tolerance_db: readNumber(record, 'tolerance_db', endpoint, path),
    min_bands: readNumber(record, 'min_bands', endpoint, path)
  }
}

function parseBand(value: unknown, endpoint: string, path: string): CalibrationBand {
  const record = asRecord(value, endpoint, path)
  return {
    hz: requireNumber(record, 'hz', endpoint, path),
    predicted_db: readNumber(record, 'predicted_db', endpoint, path),
    observed_db: readNumber(record, 'observed_db', endpoint, path),
    delta_db: readNumber(record, 'delta_db', endpoint, path),
    missing: readBoolean(record, 'missing', endpoint, path) ?? false
  }
}

function parseDesign(value: unknown, endpoint: string, path: string): CalibrationDesign | null {
  const record = optionalRecord(value, endpoint, path)
  if (record === null) {
    return null
  }
  const admissionRecord = optionalRecord(record.admission, endpoint, path + '.admission')
  return {
    residual: requireEnum(record, 'residual', RESIDUALS, endpoint, path),
    admission:
      admissionRecord === null
        ? null
        : {
            accepted: readBoolean(admissionRecord, 'accepted', endpoint, path + '.admission') ?? false,
            code: readString(admissionRecord, 'code', endpoint, path + '.admission'),
            reason: readString(admissionRecord, 'reason', endpoint, path + '.admission')
          }
  }
}

function parseJobResult(value: unknown, endpoint: string, path: string): CalibrationJobResult | null {
  const record = optionalRecord(value, endpoint, path)
  if (record === null) {
    return null
  }
  const bandsRaw = record.bands
  if (bandsRaw !== null && bandsRaw !== undefined && !Array.isArray(bandsRaw)) {
    throw invalidResponse(endpoint, path + '.bands must be an array or null')
  }
  const planRecord = optionalRecord(record.plan, endpoint, path + '.plan')
  return {
    verdict: readEnum(record, 'verdict', VERDICTS, endpoint, path),
    max_delta_db: readNumber(record, 'max_delta_db', endpoint, path),
    compared_bands: readNumber(record, 'compared_bands', endpoint, path),
    missing_bands: readNumber(record, 'missing_bands', endpoint, path),
    violations: readStringArray(record, 'violations', endpoint, path),
    bands: Array.isArray(bandsRaw)
      ? bandsRaw.map((item, index) => parseBand(item, endpoint, path + '.bands[' + String(index) + ']'))
      : [],
    design: parseDesign(record.design, endpoint, path + '.design'),
    plan:
      planRecord === null
        ? null
        : {
            sha256: readString(planRecord, 'sha256', endpoint, path + '.plan'),
            graph_id: readString(planRecord, 'graph_id', endpoint, path + '.plan')
          }
  }
}

/** 校验 /api/calibration/jobs 响应。 */
export function parseJobsResponse(body: unknown): CalibrationJobsResponse {
  const endpoint = '/api/calibration/jobs'
  const root = asRecord(body, endpoint, 'body')
  if (root.schema !== CALIBRATION_JOBS_SCHEMA) {
    throw invalidResponse(endpoint, 'schema must be "' + CALIBRATION_JOBS_SCHEMA + '"')
  }
  const jobsRaw = root.jobs
  if (!Array.isArray(jobsRaw)) {
    throw invalidResponse(endpoint, 'jobs must be an array')
  }
  return {
    schema: CALIBRATION_JOBS_SCHEMA,
    available: readBoolean(root, 'available', endpoint, 'body') ?? true,
    reason: readString(root, 'reason', endpoint, 'body'),
    engine: readString(root, 'engine', endpoint, 'body'),
    active_job_id: readString(root, 'active_job_id', endpoint, 'body'),
    jobs: jobsRaw.map((item, index) => parseJobSummary(item, endpoint, 'jobs[' + String(index) + ']')),
    total: requireNumber(root, 'total', endpoint, 'body'),
    limit: requireNumber(root, 'limit', endpoint, 'body'),
    offset: requireNumber(root, 'offset', endpoint, 'body')
  }
}

/** 校验 /api/calibration/jobs/{id} 响应。 */
export function parseJobResponse(body: unknown): CalibrationJobResponse {
  const endpoint = '/api/calibration/jobs/{id}'
  const root = asRecord(body, endpoint, 'body')
  if (root.schema !== CALIBRATION_JOB_SCHEMA) {
    throw invalidResponse(endpoint, 'schema must be "' + CALIBRATION_JOB_SCHEMA + '"')
  }
  const jobRecord = asRecord(root.job, endpoint, 'job')
  const summary = parseJobSummary(jobRecord, endpoint, 'job')
  const stepsRaw = jobRecord.steps
  const artifactsRaw = jobRecord.artifacts
  if (!Array.isArray(stepsRaw)) {
    throw invalidResponse(endpoint, 'job.steps must be an array')
  }
  if (!Array.isArray(artifactsRaw)) {
    throw invalidResponse(endpoint, 'job.artifacts must be an array')
  }
  return {
    schema: CALIBRATION_JOB_SCHEMA,
    job: {
      ...summary,
      engine: readString(jobRecord, 'engine', endpoint, 'job'),
      apply_state: readEnum(jobRecord, 'apply_state', APPLY_STATES, endpoint, 'job'),
      applied_version: readNumber(jobRecord, 'applied_version', endpoint, 'job'),
      steps: stepsRaw.map((item, index) => parseJobStep(item, endpoint, 'job.steps[' + String(index) + ']')),
      artifacts: artifactsRaw.map((item, index) =>
        parseArtifact(item, endpoint, 'job.artifacts[' + String(index) + ']')
      ),
      inputs: parseJobInputs(jobRecord.inputs, endpoint, 'job.inputs'),
      result: parseJobResult(jobRecord.result, endpoint, 'job.result')
    },
    reasons: parseReasons(root.reasons, endpoint)
  }
}

function parseSinglePoint(value: unknown, endpoint: string, path: string): WizardSinglePoint {
  const record = asRecord(value, endpoint, path)
  const centers = readNumberArray(record, 'centers_hz', endpoint, path)
  return {
    centers_hz: centers,
    tolerance_db: requireNumber(record, 'tolerance_db', endpoint, path),
    min_bands: requireNumber(record, 'min_bands', endpoint, path)
  }
}

function parseWizardConfig(value: unknown, endpoint: string, path: string): WizardConfig {
  const record = asRecord(value, endpoint, path)
  return {
    mic_cal_sha256: readString(record, 'mic_cal_sha256', endpoint, path),
    mic_cal_filename: readString(record, 'mic_cal_filename', endpoint, path),
    orientation: readEnum(record, 'orientation', ORIENTATIONS, endpoint, path),
    target_mode: readEnum(record, 'target_mode', TARGET_MODES, endpoint, path),
    single_point: parseSinglePoint(record.single_point, endpoint, path + '.single_point')
  }
}

/** 校验 GET/PUT /api/calibration/wizard 响应。 */
export function parseWizardResponse(body: unknown): CalibrationWizardResponse {
  const endpoint = '/api/calibration/wizard'
  const root = asRecord(body, endpoint, 'body')
  if (root.schema !== CALIBRATION_WIZARD_SCHEMA) {
    throw invalidResponse(endpoint, 'schema must be "' + CALIBRATION_WIZARD_SCHEMA + '"')
  }
  return {
    schema: CALIBRATION_WIZARD_SCHEMA,
    config: parseWizardConfig(root.config, endpoint, 'config'),
    defaults: parseWizardConfig(root.defaults, endpoint, 'defaults'),
    saved: readBoolean(root, 'saved', endpoint, 'body') ?? false,
    updated_at: readString(root, 'updated_at', endpoint, 'body'),
    reasons: parseReasons(root.reasons, endpoint)
  }
}

function parseMeasurement(value: unknown, endpoint: string, path: string): WizardMeasurementState {
  const record = optionalRecord(value, endpoint, path) ?? {}
  return {
    baseline_upload_id: readString(record, 'baseline_upload_id', endpoint, path),
    applied_upload_id: readString(record, 'applied_upload_id', endpoint, path),
    baseline_sha256: readString(record, 'baseline_sha256', endpoint, path),
    applied_sha256: readString(record, 'applied_sha256', endpoint, path)
  }
}

function parseSnapshot(value: unknown, endpoint: string, path: string): WizardSnapshot | null {
  const record = optionalRecord(value, endpoint, path)
  if (record === null) {
    return null
  }
  return {
    version: requireNumber(record, 'version', endpoint, path),
    graph_id: requireString(record, 'graph_id', endpoint, path),
    sha256: readString(record, 'sha256', endpoint, path)
  }
}

/**
 * 校验 GET /api/calibration/wizard/state 响应。
 * steps 必须完整覆盖 1..8 且 key 顺序固定——缺 gate 就等于"无法证明可继续"，一律拒绝而不是默认放行。
 */
export function parseWizardStateResponse(body: unknown): CalibrationWizardStateResponse {
  const endpoint = '/api/calibration/wizard/state'
  const root = asRecord(body, endpoint, 'body')
  if (root.schema !== CALIBRATION_WIZARD_STATE_SCHEMA) {
    throw invalidResponse(endpoint, 'schema must be "' + CALIBRATION_WIZARD_STATE_SCHEMA + '"')
  }
  const stepsRaw = root.steps
  if (!Array.isArray(stepsRaw)) {
    throw invalidResponse(endpoint, 'steps must be an array')
  }
  if (stepsRaw.length !== WIZARD_STEP_COUNT) {
    throw invalidResponse(endpoint, 'steps must contain exactly ' + String(WIZARD_STEP_COUNT) + ' entries')
  }
  const steps = stepsRaw.map((item, index) => {
    const path = 'steps[' + String(index) + ']'
    const record = asRecord(item, endpoint, path)
    const step = requireNumber(record, 'step', endpoint, path)
    const key = requireEnum(record, 'key', WIZARD_STEP_KEYS, endpoint, path)
    if (step !== index + 1) {
      throw invalidResponse(endpoint, path + '.step must be ' + String(index + 1))
    }
    if (key !== WIZARD_STEP_KEYS[index]) {
      throw invalidResponse(endpoint, path + '.key must be "' + WIZARD_STEP_KEYS[index] + '"')
    }
    return {
      step,
      key,
      status: requireEnum(record, 'status', GATE_STATUSES, endpoint, path),
      reason: readString(record, 'reason', endpoint, path),
      task: readString(record, 'task', endpoint, path)
    } satisfies WizardStepGate
  })
  const currentStep = requireNumber(root, 'current_step', endpoint, 'body')
  if (!Number.isInteger(currentStep) || currentStep < 1 || currentStep > WIZARD_STEP_COUNT) {
    throw invalidResponse(endpoint, 'current_step must be an integer in 1..' + String(WIZARD_STEP_COUNT))
  }
  return {
    schema: CALIBRATION_WIZARD_STATE_SCHEMA,
    current_step: currentStep,
    steps,
    active_job_id: readString(root, 'active_job_id', endpoint, 'body'),
    last_job_id: readString(root, 'last_job_id', endpoint, 'body'),
    measurement: parseMeasurement(root.measurement, endpoint, 'measurement'),
    snapshot: parseSnapshot(root.snapshot, endpoint, 'snapshot'),
    updated_at: readString(root, 'updated_at', endpoint, 'body'),
    reasons: parseReasons(root.reasons, endpoint)
  }
}

/** 校验 POST /api/calibration/uploads 响应。 */
export function parseUploadResponse(body: unknown): CalibrationUploadResult {
  const endpoint = '/api/calibration/uploads'
  const root = asRecord(body, endpoint, 'body')
  return {
    upload_id: requireString(root, 'upload_id', endpoint, 'body'),
    sha256: requireString(root, 'sha256', endpoint, 'body'),
    bytes: requireNumber(root, 'bytes', endpoint, 'body')
  }
}

/** 校验 POST /api/calibration/jobs 响应（202）。 */
export function parseCreateJobResponse(body: unknown): CalibrationCreateJobResult {
  const endpoint = '/api/calibration/jobs'
  const root = asRecord(body, endpoint, 'body')
  return {
    job_id: requireString(root, 'job_id', endpoint, 'body'),
    status: readEnum(root, 'status', JOB_STATUSES, endpoint, 'body') ?? 'queued'
  }
}

function parseAck(value: unknown, endpoint: string): { synthetic: boolean; actual: boolean } | null {
  const record = optionalRecord(value, endpoint, 'ack')
  if (record === null) {
    return null
  }
  return {
    synthetic: readBoolean(record, 'synthetic', endpoint, 'ack') ?? false,
    actual: readBoolean(record, 'actual', endpoint, 'ack') ?? false
  }
}

/** 校验 apply 结果（job 详情页与 wizard apply 共用）。 */
export function parseApplyResult(body: unknown): CalibrationApplyResult {
  const endpoint = '/api/calibration/apply'
  const root = asRecord(body, endpoint, 'body')
  if (readBoolean(root, 'applied', endpoint, 'body') !== true) {
    throw invalidResponse(endpoint, 'applied must be true')
  }
  const readback = optionalRecord(root.readback, endpoint, 'readback')
  return {
    applied: true,
    job_id: readString(root, 'job_id', endpoint, 'body'),
    applied_version: requireNumber(root, 'applied_version', endpoint, 'body'),
    graph_id: requireString(root, 'graph_id', endpoint, 'body'),
    sha256: readString(root, 'sha256', endpoint, 'body'),
    snapshot: parseSnapshot(root.snapshot, endpoint, 'snapshot'),
    ack: parseAck(root.ack, endpoint),
    readback,
    applied_at: readString(root, 'applied_at', endpoint, 'body')
  }
}

/** 校验 rollback 结果。 */
export function parseRollbackResult(body: unknown): CalibrationRollbackResult {
  const endpoint = '/api/calibration/rollback'
  const root = asRecord(body, endpoint, 'body')
  if (readBoolean(root, 'rolled_back', endpoint, 'body') !== true) {
    throw invalidResponse(endpoint, 'rolled_back must be true')
  }
  return {
    rolled_back: true,
    restored_version: requireNumber(root, 'restored_version', endpoint, 'body'),
    graph_id: requireString(root, 'graph_id', endpoint, 'body'),
    sha256: readString(root, 'sha256', endpoint, 'body'),
    readback: optionalRecord(root.readback, endpoint, 'readback'),
    rolled_back_at: readString(root, 'rolled_back_at', endpoint, 'body')
  }
}

/** 校验 cancel 响应（宽松：只要 200 即可，body 可选）。 */
export function parseCancelResult(body: unknown): { job_id: string | null; status: CalibrationJobStatus | null } {
  const endpoint = '/api/calibration/jobs/{id}/cancel'
  if (body === undefined || body === null) {
    return { job_id: null, status: null }
  }
  const root = asRecord(body, endpoint, 'body')
  return {
    job_id: readString(root, 'job_id', endpoint, 'body'),
    status: readEnum(root, 'status', JOB_STATUSES, endpoint, 'body')
  }
}

/* ---------------------------------------------------------------------- 端点 */

export interface CalibrationRequestOptions {
  signal?: AbortSignal
}

export interface CalibrationUploadOptions {
  /** 原始文件 MIME（audio/wav / text/plain 等）；缺省 application/octet-stream。 */
  contentType?: string
  /** 上传文件名提示（仅用于 daemon 侧日志/展示，不参与内容寻址）。 */
  filename?: string
  /** 上传角色提示（baseline/applied/mic_cal）；dev mock 用它区分两段录音，真实 daemon 可忽略。 */
  role?: 'baseline' | 'applied' | 'mic_cal'
  signal?: AbortSignal
}

export interface CalibrationApi {
  listJobs(params?: { limit?: number; offset?: number }, options?: CalibrationRequestOptions): Promise<CalibrationJobsResponse>
  getJob(jobId: string, options?: CalibrationRequestOptions): Promise<CalibrationJobResponse>
  createJob(request: CalibrationCreateJobRequest, options?: CalibrationRequestOptions): Promise<CalibrationCreateJobResult>
  cancelJob(jobId: string, options?: CalibrationRequestOptions): Promise<{ job_id: string | null; status: CalibrationJobStatus | null }>
  applyJob(jobId: string, options?: CalibrationRequestOptions): Promise<CalibrationApplyResult>
  rollbackJob(jobId: string, options?: CalibrationRequestOptions): Promise<CalibrationRollbackResult>
  upload(body: ArrayBuffer | Blob, options?: CalibrationUploadOptions): Promise<CalibrationUploadResult>
  getWizard(options?: CalibrationRequestOptions): Promise<CalibrationWizardResponse>
  putWizard(request: WizardSaveRequest, options?: CalibrationRequestOptions): Promise<CalibrationWizardResponse>
  getWizardState(options?: CalibrationRequestOptions): Promise<CalibrationWizardStateResponse>
  applyWizard(options?: CalibrationRequestOptions): Promise<CalibrationApplyResult>
  /** 产物下载直链（浏览器同源 GET，会话 Cookie 自动携带；daemon 侧校验 manifest sha256）。 */
  artifactUrl(jobId: string, name: string): string
}

function jobPath(jobId: string): string {
  return '/api/calibration/jobs/' + encodeURIComponent(jobId)
}

/** 产物下载直链（供 <a href> 使用；同源 GET 自动带会话 Cookie，daemon 侧校验 manifest sha256）。 */
export function calibrationArtifactUrl(jobId: string, name: string): string {
  return jobPath(jobId) + '/artifacts/' + encodeURIComponent(name)
}

/** 用共享 ApiClient 构造校准 API（单测注入 fetchImpl/baseUrl 即可脱离网络）。 */
export function createCalibrationApi(client: ApiClient): CalibrationApi {
  return {
    async listJobs(params = {}, options = {}) {
      const body = await client.get<unknown>('/api/calibration/jobs', {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        query: { limit: params.limit, offset: params.offset },
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseJobsResponse(body)
    },

    async getJob(jobId, options = {}) {
      const body = await client.get<unknown>(jobPath(jobId), {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseJobResponse(body)
    },

    async createJob(request, options = {}) {
      const body = await client.post<unknown>('/api/calibration/jobs', request, {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseCreateJobResponse(body)
    },

    async cancelJob(jobId, options = {}) {
      const body = await client.post<unknown>(jobPath(jobId) + '/cancel', undefined, {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseCancelResult(body)
    },

    async applyJob(jobId, options = {}) {
      const body = await client.post<unknown>(jobPath(jobId) + '/apply', undefined, {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseApplyResult(body)
    },

    async rollbackJob(jobId, options = {}) {
      const body = await client.post<unknown>(jobPath(jobId) + '/rollback', undefined, {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseRollbackResult(body)
    },

    async upload(body, options = {}) {
      const headers: Record<string, string> = {}
      if (options.filename !== undefined) {
        headers['X-KHAEN-Filename'] = options.filename
      }
      if (options.role !== undefined) {
        headers['X-KHAEN-Upload-Role'] = options.role
      }
      const raw = await client.postRaw<unknown>('/api/calibration/uploads', body, {
        contentType: options.contentType ?? 'application/octet-stream',
        headers,
        timeoutMs: CALIBRATION_UPLOAD_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseUploadResponse(raw)
    },

    async getWizard(options = {}) {
      const body = await client.get<unknown>('/api/calibration/wizard', {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseWizardResponse(body)
    },

    async putWizard(request, options = {}) {
      const body = await client.put<unknown>('/api/calibration/wizard', request, {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseWizardResponse(body)
    },

    async getWizardState(options = {}) {
      const body = await client.get<unknown>('/api/calibration/wizard/state', {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseWizardStateResponse(body)
    },

    async applyWizard(options = {}) {
      const body = await client.post<unknown>('/api/calibration/wizard/apply', undefined, {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseApplyResult(body)
    },

    artifactUrl(jobId, name) {
      return calibrationArtifactUrl(jobId, name)
    }
  }
}

/* ------------------------------------------------------------------ 派生逻辑 */

/** job 是否还在跑（轮询继续条件）。 */
export function isJobActive(status: CalibrationJobStatus | null | undefined): boolean {
  return status === 'queued' || status === 'running'
}

/** job 是否已到终态（轮询停止条件）。 */
export function isJobTerminal(status: CalibrationJobStatus | null | undefined): boolean {
  return status === 'succeeded' || status === 'failed' || status === 'cancelled'
}

/** 步骤序号（1 基）-> 固定 key；越界返回 null（不猜）。 */
export function stepKey(step: number): WizardStepKey | null {
  if (!Number.isInteger(step) || step < 1 || step > WIZARD_STEP_COUNT) {
    return null
  }
  return WIZARD_STEP_KEYS[step - 1]
}

/** 取某一步的 gate（缺失返回 null：调用方必须按"不可继续"处理，不得默认放行）。 */
export function gateForStep(
  state: CalibrationWizardStateResponse | null | undefined,
  step: number
): WizardStepGate | null {
  if (!state) {
    return null
  }
  return state.steps.find((item) => item.step === step) ?? null
}

/**
 * 该步是否允许进入下一步：
 * - passed：可以；
 * - not_available：能力未接入的**声明式**缺口（不是用户可修复的门禁），允许继续但不产生数据；
 * - blocked / 缺失 gate：不可以（fail closed）。
 */
export function canAdvance(state: CalibrationWizardStateResponse | null | undefined, step: number): boolean {
  const gate = gateForStep(state, step)
  if (gate === null) {
    return false
  }
  return gate.status === 'passed' || gate.status === 'not_available'
}

/** 该步 gate 未通过的原因（blocked/缺失时给可展示文案；passed 返回 null）。 */
export function blockReason(state: CalibrationWizardStateResponse | null | undefined, step: number): string | null {
  const gate = gateForStep(state, step)
  if (gate === null) {
    return 'missing gate'
  }
  if (gate.status === 'passed') {
    return null
  }
  return gate.reason ?? gate.status
}

/**
 * 已解锁到第几步（1..8）：从 1 开始，只要当前步 canAdvance 就解锁下一步；遇到 blocked/缺失停止。
 * 只用于 stepper 的禁用展示；"下一步"按钮本身仍以 canAdvance(currentStep) 判定。
 */
export function maxUnlockedStep(state: CalibrationWizardStateResponse | null | undefined): number {
  if (!state) {
    return 1
  }
  let unlocked = 1
  while (unlocked < WIZARD_STEP_COUNT && canAdvance(state, unlocked)) {
    unlocked += 1
  }
  return unlocked
}

/** 步骤 gate 的中性种类（用于 stepper 圆点/颜色）：done/current/blocked/notAvailable/locked。 */
export type StepDisplayState = 'done' | 'current' | 'blocked' | 'notAvailable' | 'locked'

export function stepDisplayState(
  state: CalibrationWizardStateResponse | null | undefined,
  step: number,
  currentStep: number
): StepDisplayState {
  const gate = gateForStep(state, step)
  if (step === currentStep) {
    return gate !== null && gate.status === 'not_available' ? 'notAvailable' : 'current'
  }
  if (gate === null) {
    return 'locked'
  }
  if (gate.status === 'passed') {
    return 'done'
  }
  if (gate.status === 'not_available') {
    return 'notAvailable'
  }
  return step < currentStep ? 'blocked' : 'locked'
}

/** job 结果中 verdict 对应的语义色种类。 */
export function verdictTone(verdict: CalibrationVerdict | null | undefined): 'ok' | 'bad' | 'warn' | null {
  switch (verdict) {
    case 'PASS':
      return 'ok'
    case 'FAIL':
      return 'bad'
    case 'INCOMPLETE':
      return 'warn'
    default:
      return null
  }
}

/**
 * job/列表引擎标识 -> TruthTag：
 * - 缺失 = 未接入（daemon 未上报引擎来源）；
 * - 只有 daemon 明确上报 real_audio 才算实测；
 * - 其它任何引擎标识（例如 mock 后端上报的 synthetic）一律按「示意」显示，不冒充实测。
 */
export function engineTruth(engine: string | null | undefined): TruthKind {
  if (typeof engine !== 'string' || engine.length === 0) {
    return 'notWired'
  }
  return engine.toLowerCase() === 'real_audio' ? 'actual' : 'demo'
}

/** job 是否来自 mock/fixture（页面据此把结果标成「示意」而不是实测）。 */
export function isSyntheticEngine(engine: string | null | undefined): boolean {
  return engineTruth(engine) === 'demo'
}

/**
 * job 结果数值的 TruthTag：mock/fixture 引擎 = 示意；其余（真实离线闭环）= 实测。
 * 注意与 engineTruth 的区别：engine 缺失表示"引擎来源未上报"（徽标未接入），
 * 不代表 job 结果不存在——真实 daemon 的离线 job 结果仍是真实计算。
 */
export function jobResultTruth(engine: string | null | undefined): TruthKind {
  return isSyntheticEngine(engine) ? 'demo' : 'actual'
}

/** 单点校准默认 1/3-oct 中心频率（25 Hz–20 kHz，30 个；与 calibration CLI 默认同值）。 */
export const DEFAULT_CENTERS_HZ: readonly number[] = [
  25, 31.5, 40, 50, 63, 80, 100, 125, 160, 200, 250, 315, 400, 500, 630, 800, 1000, 1250, 1600, 2000,
  2500, 3150, 4000, 5000, 6300, 8000, 10000, 12500, 16000, 20000
]

/** job 耗时：优先用后端 duration_ms；缺失时只在两端时间戳都存在时计算（不编造）。 */
export function jobDurationMs(job: CalibrationJobSummary | CalibrationJob | null | undefined): number | null {
  if (!job) {
    return null
  }
  if (job.duration_ms !== null) {
    return job.duration_ms
  }
  if (job.started_at === null || job.finished_at === null) {
    return null
  }
  const start = Date.parse(job.started_at)
  const finish = Date.parse(job.finished_at)
  if (!Number.isFinite(start) || !Number.isFinite(finish) || finish < start) {
    return null
  }
  return finish - start
}

/** 当前步骤里第一个尚未完成的步骤序号（用于"取消向导/回到未完成步骤"）；全部完成返回 8。 */
export function firstIncompleteStep(state: CalibrationWizardStateResponse | null | undefined): number {
  if (!state) {
    return 1
  }
  for (const gate of state.steps) {
    if (gate.status === 'blocked') {
      return gate.step
    }
  }
  return WIZARD_STEP_COUNT
}

/* ---------------------------------------------------------------- 错误归类 */

export type CalibrationErrorCategory =
  | 'network'
  | 'timeout'
  | 'unauthorized'
  | 'not_wired'
  | 'busy'
  | 'conflict'
  | 'not_found'
  | 'invalid_request'
  | 'upload_too_large'
  | 'unsupported_format'
  | 'server'
  | 'invalid_response'
  | 'unknown'

export interface CalibrationErrorInfo {
  category: CalibrationErrorCategory
  /** HTTP 状态码；0 = 网络层失败。 */
  status: number
  /** 后端稳定码（或本地归类码）。 */
  code: string
  /** 主文案 i18n 键（zh-CN / en-US 对等）。 */
  i18nKey: string
  /** 后端 message / 异常消息（仅详情展示，不当主文案）。 */
  detail?: string
  /** 是否值得重试（401/not_wired 不重试）。 */
  retryable: boolean
}

const ERROR_KEYS: Record<CalibrationErrorCategory, string> = {
  network: 'error.calibration.network',
  timeout: 'error.calibration.timeout',
  unauthorized: 'error.calibration.unauthorized',
  not_wired: 'error.calibration.notWired',
  busy: 'error.calibration.busy',
  conflict: 'error.calibration.conflict',
  not_found: 'error.calibration.notFound',
  invalid_request: 'error.calibration.invalidRequest',
  upload_too_large: 'error.calibration.uploadTooLarge',
  unsupported_format: 'error.calibration.unsupportedFormat',
  server: 'error.calibration.server',
  invalid_response: 'error.calibration.invalidResponse',
  unknown: 'error.calibration.unknown'
}

const CODE_CATEGORY: Record<string, CalibrationErrorCategory> = {
  not_wired: 'not_wired',
  switch_busy: 'busy',
  job_not_active: 'busy',
  job_not_succeeded: 'conflict',
  plan_unavailable: 'conflict',
  rollback_unavailable: 'conflict',
  wizard_not_saved: 'conflict',
  artifact_sha_mismatch: 'conflict',
  job_not_found: 'not_found',
  upload_not_found: 'not_found',
  artifact_not_found: 'not_found',
  invalid_request: 'invalid_request',
  invalid_upload: 'invalid_request',
  invalid_wizard_config: 'invalid_request',
  upload_too_large: 'upload_too_large',
  unsupported_format: 'unsupported_format',
  invalid_response: 'invalid_response'
}

/** 把校准 API 失败归类为 UI 可分支的稳定类别 + 本地化文案键。 */
export function classifyCalibrationError(error: unknown): CalibrationErrorInfo {
  if (!(error instanceof ApiError)) {
    const detail = error instanceof Error ? error.message : undefined
    return {
      category: 'unknown',
      status: 0,
      code: 'unknown_error',
      i18nKey: ERROR_KEYS.unknown,
      ...(detail ? { detail } : {}),
      retryable: true
    }
  }
  let category: CalibrationErrorCategory
  if (error.code === 'request_timeout') {
    category = 'timeout'
  } else if (error.code === 'network_error' || error.status === 0) {
    category = 'network'
  } else if (error.status === 401) {
    category = 'unauthorized'
  } else if (error.code === 'invalid_response') {
    category = 'invalid_response'
  } else if (CODE_CATEGORY[error.code] !== undefined) {
    category = CODE_CATEGORY[error.code]
  } else if (error.status === 413) {
    category = 'upload_too_large'
  } else if (error.status === 415) {
    category = 'unsupported_format'
  } else if (error.status === 409) {
    category = 'conflict'
  } else if (error.status === 404) {
    category = 'not_found'
  } else if (error.status === 400 || error.status === 422) {
    category = 'invalid_request'
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
    i18nKey: ERROR_KEYS[category],
    ...(detail ? { detail } : {}),
    retryable: category !== 'unauthorized' && category !== 'not_wired'
  }
}
