/**
 * 校准 REST 客户端（PLAN §4.7 job 列表/详情/uploads/artifacts + §10.4 wizard 端点；任务 T37）。
 *
 * 契约：docs/API.md v1.1 §7「实际形状 + 加法目标」（以实际形状为准；【加法】字段容忍但不依赖）：
 *   POST /api/calibration/uploads                 -> 原始字节 {schema?,upload_id,sha256,bytes,created?}（同 sha 200 幂等）
 *   GET  /api/calibration/jobs?limit&offset       -> {schema,available,reason?,engine?,active_job_id?,total,limit,offset,jobs,reasons}
 *        canonical job 字段 = id/status（docs/API.md §7 v1.1 裁决 H 已按后端实际修正）；
 *        解析层保留 job_id/state 旧别名的宽容归一化作防御（不写死旧文档期望）。
 *   POST /api/calibration/jobs                    -> 202 {job_id,location}（+ok/state）
 *   GET  /api/calibration/jobs/{id}               -> {schema,job:{...},reasons}（+顶层 result/artifacts/ok）
 *   POST /api/calibration/jobs/{id}/cancel        -> {job_id,status}（canonical 状态 'cancelled' 双 l）
 *   POST /api/calibration/jobs/{id}/apply         -> 单发 {}；或 staged {stage:prepare|commit|abort,engine_token?}
 *   POST /api/calibration/jobs/{id}/rollback      -> 同 apply
 *   GET  /api/calibration/jobs/{id}/artifacts/{name} -> 二进制产物（带认证下载：getRaw + Content-Disposition）
 *   GET/PUT /api/calibration/wizard               -> {schema,config,defaults?,saved,updated_at,reasons}
 *   GET  /api/calibration/wizard/state            -> khaen-calibration-wizard-state/v1（每步 gate，刷新可恢复）
 *   POST /api/calibration/wizard/apply {job_id?}  -> apply 回执或 {job_id,location}
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
import type { RawBytesResponse } from './client.ts'
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

/** 校准 job 种类（docs/API.md §7 / §7.2）：离线闭环 / rt-host 真实测量。 */
export const JOB_KIND_SINGLE_POINT_OFFLINE = 'single_point_offline'
export const JOB_KIND_SINGLE_POINT_RT_HOST = 'single_point_rt_host'
export type CalibrationJobKind =
  | typeof JOB_KIND_SINGLE_POINT_OFFLINE
  | typeof JOB_KIND_SINGLE_POINT_RT_HOST

/**
 * 已知 job 步骤名：**只用于可选的本地化标签**，解析不依赖它——后端返回的 name 一律原样接受并呈现。
 * - 离线 job（PLAN §4.7）：compile → levels → closed_loop → bundle；
 * - rt-host 真实测量 job（docs/API.md §7.2）：validate → sweep_capture → quality_check → bundle。
 */
export const OFFLINE_JOB_STEP_NAMES = ['compile', 'levels', 'closed_loop', 'bundle'] as const
export const RT_HOST_JOB_STEP_NAMES = [
  'validate',
  'sweep_capture',
  'quality_check',
  'solve_g',
  'compile',
  'bundle'
] as const
export const JOB_STEP_NAMES = [
  'compile',
  'levels',
  'closed_loop',
  'validate',
  'sweep_capture',
  'quality_check',
  'bundle'
] as const

export interface CalibrationErrorDetail {
  code: string
  message: string
}

export interface CalibrationJobSummary {
  /** canonical job id（daemon/API 合同 §7 为 `id`；旧文档 `job_id` 仅作防御性兼容）。 */
  id: string
  kind: string
  /** canonical 状态（daemon/API 合同 §7 为 `status`；旧文档 `state` 仅作防御性兼容）。 */
  status: CalibrationJobStatus
  verdict: CalibrationVerdict | null
  created_at: string | null
  started_at: string | null
  finished_at: string | null
  duration_ms: number | null
  error: CalibrationErrorDetail | null
  reason: string | null
  /** 【加法】epoch 秒创建时间（缺失为 null，不依赖）。 */
  created_unix: number | null
  /** 【加法】epoch 秒更新时间（缺失为 null，不依赖）。 */
  updated_unix: number | null
  /** 【加法】0..1 真实进度（缺失为 null：UI 不画假进度条）。 */
  progress: number | null
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
  /** daemon 为缺失字段给出的原因（如 jobs/engine/active_job_id）；展示用，不当数据。 */
  reasons: Record<string, string>
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
  /** 【加法】daemon 是否允许经 /artifacts/{name} 下载（缺失按白名单语义处理）。 */
  downloadable: boolean | null
}

export interface CalibrationJobInputs {
  plan_sha: string | null
  baseline_upload_id: string | null
  applied_upload_id: string | null
  centers_hz: number[]
  tolerance_db: number | null
  min_bands: number | null
}

/**
 * rt-host 真实测量 job 的会话输入回显（详情**顶层** inputs_rt_host，docs/API.md §7.2 加法字段）。
 * 缺失为 null：不把"未上报"补成默认值。离线 job 没有该字段。
 */
export interface CalibrationRtHostJobInputs {
  audible: boolean | null
  channels: number[] | null
  level_dbfs: number | null
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

/** 实测来源（合同 §7：measured 与 simulated 必须分开标注，不把模拟写成实测）。 */
export interface CalibrationMeasuredSource {
  baseline_upload_id: string | null
  applied_upload_id: string | null
  sample_rate_hz: number | null
  source: string | null
}

/** 模拟来源（方案预测电平；不是实测）。 */
export interface CalibrationSimulatedSource {
  predicted_from: string | null
  plan_sha256: string | null
  graph_id: string | null
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
  /** 【加法】上传录音来源（measured）；null = 后端未上报（UI 不冒充）。 */
  measured: CalibrationMeasuredSource | null
  /** 【加法】模拟/预测来源；null = 未上报。 */
  simulated: CalibrationSimulatedSource | null
}

/** job 的 apply 状态（daemon 在 detail 里上报，列表不带）。 */
export type CalibrationApplyState = 'not_applied' | 'applied' | 'rolled_back'

/** job 的 apply 记录（daemon 的 `apply` 对象；apply_state 是兼容旧字段）。 */
export interface CalibrationJobApply {
  state: CalibrationApplyState | null
  applied_version: number | null
  previous_version: number | null
  applied_at: string | null
  plan_sha256: string | null
}

export interface CalibrationJob extends CalibrationJobSummary {
  /** 引擎真实性原样透传（真实闭环 = actual；mock/fixture = demo）。 */
  engine: string | null
  /** 当前执行中的步骤名（【加法】，缺失为 null）。 */
  step: string | null
  /** 该 job 产出的 plan 的应用状态（not_applied / applied / rolled_back）。 */
  apply_state: CalibrationApplyState | null
  /** 已应用版本（apply 后由 daemon 回填）。 */
  applied_version: number | null
  steps: CalibrationStep[]
  artifacts: CalibrationArtifact[]
  inputs: CalibrationJobInputs | null
  /** rt-host 真实测量 job 的会话输入回显（离线 job 为 null）。 */
  inputs_rt_host: CalibrationRtHostJobInputs | null
  result: CalibrationJobResult | null
  /** daemon 的 apply 明细对象（缺失时由 apply_state/applied_version 合成）。 */
  apply: CalibrationJobApply | null
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
  /** 【加法】响应 schema（daemon 当前不返回；缺失为 null，不依赖）。 */
  schema: string | null
  /** 服务端是否新建（false = 同 sha 幂等命中已有上传）。 */
  created: boolean | null
}

export interface CalibrationCreateJobInputs {
  plan_sha: string
  baseline_upload_id: string
  applied_upload_id: string
  centers_hz: number[]
  tolerance_db: number
  min_bands: number
}

/**
 * rt-host 真实测量 job 的会话输入（docs/API.md §7.2）：
 * audible 必须显式为 true（P08：真实测量会出声/采音，daemon 拒绝静默请求）。
 */
export interface RtHostMeasurementInputs {
  audible: true
  /** 可选：测量通道（u32；非空、≤22、不重复）；省略 = 由 rt-host 的 KHAEN_CAL_CHANNELS 决定。 */
  channels?: number[]
  /** 可选：扫频电平 dBFS（[-60,-1]）；省略 = 由 rt-host 自己的电平决定。 */
  level_dbfs?: number
}

/**
 * POST /api/calibration/jobs 的请求体（discriminated union）。
 * 两种 kind 的 inputs 形状完全不同，不能把离线上传字段塞进 rt-host 请求（或反之）。
 */
export type CalibrationCreateJobRequest =
  | { kind: typeof JOB_KIND_SINGLE_POINT_OFFLINE; inputs: CalibrationCreateJobInputs }
  | { kind: typeof JOB_KIND_SINGLE_POINT_RT_HOST; inputs: RtHostMeasurementInputs }

export interface CalibrationCreateJobResult {
  job_id: string
  /** 合同 §7：202 回执的 `location`（缺失为 null，不猜 URL）。 */
  location: string | null
  status: CalibrationJobStatus
}

/* ---------------------------------------------------- apply / rollback 回执 */

/** 两阶段 stage（合同 §7【目标】：prepare/commit/abort；旧单发 = 省略 stage）。 */
export type CalibrationStage = 'prepare' | 'commit' | 'abort'

/** apply/rollback 的请求体：默认单发 `{}`；两阶段按 stage 传。 */
export type CalibrationStagedRequest =
  | { stage: 'prepare' }
  | { stage: 'commit'; engine_token: string }
  | { stage: 'abort'; engine_token: string }

/** apply 回执里的应用前快照（version/graph_id 可能为 null：不编造）。 */
export interface CalibrationApplySnapshot {
  version: number | null
  graph_id: string | null
  sha256: string | null
}

/** 单发（旧语义）或 staged commit 已生效的回执。 */
export interface CalibrationApplyResult {
  kind: 'applied'
  /** single = 旧单发（prepare+commit 一次完成）；commit = staged 的 commit 阶段。 */
  stage: 'single' | 'commit'
  applied: true
  job_id: string | null
  applied_version: number | null
  /** staged commit 回执带应用前版本（缺失为 null）。 */
  previous_version: number | null
  graph_id: string | null
  /** 控制面只暴露 version/graph_id 时可能为 null（reasons 里给原因），不当失败。 */
  sha256: string | null
  snapshot: CalibrationApplySnapshot | null
  ack: { synthetic: boolean; actual: boolean } | null
  readback: Record<string, unknown> | null
  applied_at: string | null
  /** 回执内嵌的 job（daemon 会带；缺失为 null，调用方应重新 GET 详情）。 */
  job: CalibrationJob | null
  reasons: Record<string, string>
}

/** staged prepare 回执：只生成草稿 + 一次性 token，**未生效**。 */
export interface CalibrationApplyPrepare {
  kind: 'prepare'
  stage: 'prepare'
  job_id: string | null
  engine_token: string
  plan_sha256: string | null
  previous_version: number | null
  target_version: number | null
  job: CalibrationJob | null
  reasons: Record<string, string>
  raw: Record<string, unknown>
}

/** staged abort 回执：草稿已丢弃，运行态不变。 */
export interface CalibrationApplyAbort {
  kind: 'abort'
  stage: 'abort'
  aborted: true
  job_id: string | null
  plan_sha256: string | null
  target_version: number | null
  control: Record<string, unknown> | null
  reasons: Record<string, string>
  raw: Record<string, unknown>
}

export type CalibrationApplyOutcome = CalibrationApplyResult | CalibrationApplyPrepare | CalibrationApplyAbort

/** 单发（旧语义）或 staged commit 已回滚的回执。 */
export interface CalibrationRollbackResult {
  kind: 'rolled_back'
  /** single = 旧单发；commit = staged 的 commit 阶段。 */
  stage: 'single' | 'commit'
  rolled_back: true
  job_id: string | null
  restored_version: number | null
  applied_version: number | null
  graph_id: string | null
  sha256: string | null
  readback: Record<string, unknown> | null
  rolled_back_at: string | null
  job: CalibrationJob | null
  reasons: Record<string, string>
}

/** staged rollback prepare 回执：只生成草稿 + 一次性 token，**未生效**。 */
export interface CalibrationRollbackPrepare {
  kind: 'prepare'
  stage: 'prepare'
  job_id: string | null
  engine_token: string
  plan_sha256: string | null
  previous_version: number | null
  target_version: number | null
  job: CalibrationJob | null
  reasons: Record<string, string>
  raw: Record<string, unknown>
}

/** staged rollback abort 回执。 */
export interface CalibrationRollbackAbort {
  kind: 'abort'
  stage: 'abort'
  aborted: true
  job_id: string | null
  plan_sha256: string | null
  target_version: number | null
  control: Record<string, unknown> | null
  reasons: Record<string, string>
  raw: Record<string, unknown>
}

export type CalibrationRollbackOutcome =
  | CalibrationRollbackResult
  | CalibrationRollbackPrepare
  | CalibrationRollbackAbort

/**
 * wizard/apply 的防御性兼容分支：旧文档曾写"无 job 时回 {job_id,location}"。
 * docs/API.md v1.1 裁决 H 后 canonical 只有 apply 回执（无 job 时 409 no_applicable_job）；
 * 这里保留解析能力只为不把未知形状误判成失败，正常路径不会走它。
 */
export interface CalibrationWizardApplyJob {
  kind: 'job'
  job_id: string | null
  location: string | null
  raw: Record<string, unknown>
}

export type CalibrationWizardApplyOutcome = CalibrationApplyOutcome | CalibrationWizardApplyJob

/**
 * 带认证的产物下载结果（ApiClient.getRawBytes：同一套 Cookie/401/错误体/版本头处理）。
 * `data` 是**无损原始字节**（二进制 plan.bin 也可直接 Blob 保存）；
 * `text` 只是 UTF-8 解码视图（JSON/CSV 便于展示/调试，不用于二进制）。
 */
export interface CalibrationArtifactDownload {
  job_id: string
  name: string
  /** Content-Disposition 的 filename；缺失时回退为 artifact name。 */
  filename: string
  content_type: string | null
  /** daemon 的 x-khaen-artifact-sha256（用于对账；不冒充已本地校验）。 */
  sha256: string | null
  status: number
  data: ArrayBuffer
  text: string
  /** 原始字节数（ByteLength）。 */
  byte_length: number
  /** filename 是否来自服务端 Content-Disposition。 */
  from_content_disposition: boolean
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
  /** 【加法】PUT 回执里 daemon 实际写入的字段（缺失为 []）。 */
  set_fields: string[]
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

/** 缺失/null = null（与 readNumberArray 的 [] 默认值区分：echo 字段不能补成空数组）。 */
function readNullableNumberArray(
  record: Record<string, unknown>,
  field: string,
  endpoint: string,
  path: string
): number[] | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (!Array.isArray(value)) {
    throw invalidResponse(endpoint, path + '.' + field + ' must be an array of numbers or null')
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

/**
 * 合同 §7 与 daemon 实际字段存在别名（`job_id`/`id`、`state`/`status`）：
 * 两者都接受并归一化为内部字段；两个都给时以 §7 canonical 名为准。
 */
function readAliasedString(
  record: Record<string, unknown>,
  fields: readonly string[],
  endpoint: string,
  path: string
): string | null {
  for (const field of fields) {
    const value = record[field]
    if (value !== undefined && value !== null) {
      return readString(record, field, endpoint, path)
    }
  }
  return null
}

function requireAliasedString(
  record: Record<string, unknown>,
  fields: readonly string[],
  endpoint: string,
  path: string
): string {
  const value = readAliasedString(record, fields, endpoint, path)
  if (value === null || value.length === 0) {
    throw invalidResponse(endpoint, path + '.' + fields[0] + ' must be a non-empty string')
  }
  return value
}

function readAliasedEnum<T extends string>(
  record: Record<string, unknown>,
  fields: readonly string[],
  allowed: readonly T[],
  endpoint: string,
  path: string,
  required: true
): T
function readAliasedEnum<T extends string>(
  record: Record<string, unknown>,
  fields: readonly string[],
  allowed: readonly T[],
  endpoint: string,
  path: string,
  required: false
): T | null
function readAliasedEnum<T extends string>(
  record: Record<string, unknown>,
  fields: readonly string[],
  allowed: readonly T[],
  endpoint: string,
  path: string,
  required: boolean
): T | null {
  const value = readAliasedString(record, fields, endpoint, path)
  if (value === null) {
    if (required) {
      throw invalidResponse(endpoint, path + '.' + fields[0] + ' is required')
    }
    return null
  }
  if (!(allowed as readonly string[]).includes(value)) {
    throw invalidResponse(endpoint, path + '.' + fields[0] + ' has unknown value "' + value + '"')
  }
  return value as T
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
const STAGES: readonly CalibrationStage[] = ['prepare', 'commit', 'abort']

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
    // canonical 是 id/status（docs/API.md §7 裁决 H）；job_id/state 旧别名只做防御性兼容。
    id: requireAliasedString(record, ['id', 'job_id'], endpoint, path),
    kind: requireString(record, 'kind', endpoint, path),
    status: readAliasedEnum(record, ['status', 'state'], JOB_STATUSES, endpoint, path, true),
    verdict: readEnum(record, 'verdict', VERDICTS, endpoint, path),
    created_at: readString(record, 'created_at', endpoint, path),
    started_at: readString(record, 'started_at', endpoint, path),
    finished_at: readString(record, 'finished_at', endpoint, path),
    duration_ms: readNumber(record, 'duration_ms', endpoint, path),
    error: parseJobError(record.error, endpoint, path + '.error'),
    reason: readString(record, 'reason', endpoint, path),
    // 【加法】字段：有则透传，没有就是 null，绝不补 0。
    created_unix: readNumber(record, 'created_unix', endpoint, path),
    updated_unix: readNumber(record, 'updated_unix', endpoint, path),
    progress: readNumber(record, 'progress', endpoint, path)
  }
}

/**
 * 解析一个 job 步骤。**不校验固定步骤名**：离线 job 的 compile/levels/... 与 rt-host 真实测量的
 * validate/sweep_capture/quality_check/... 都原样接收；未来后端新增的 name 也如实呈现（不映射、不丢弃）。
 */
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
    content_type: readString(record, 'content_type', endpoint, path),
    downloadable: readBoolean(record, 'downloadable', endpoint, path)
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

/**
 * 解析 rt-host job 的会话输入回显（详情顶层 inputs_rt_host；缺失 = null，不补默认值）。
 * 字段本身允许为 null（channels/level_dbfs 省略时后端回 null），类型不对仍按 invalid_response 拒绝。
 */
function parseRtHostJobInputs(
  value: unknown,
  endpoint: string,
  path: string
): CalibrationRtHostJobInputs | null {
  const record = optionalRecord(value, endpoint, path)
  if (record === null) {
    return null
  }
  return {
    audible: readBoolean(record, 'audible', endpoint, path),
    channels: readNullableNumberArray(record, 'channels', endpoint, path),
    level_dbfs: readNumber(record, 'level_dbfs', endpoint, path)
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
          },
    // 【加法】measured / simulated：如实分开标注，缺失为 null。
    measured: parseMeasuredSource(record.measured, endpoint, path + '.measured'),
    simulated: parseSimulatedSource(record.simulated, endpoint, path + '.simulated')
  }
}

function parseMeasuredSource(value: unknown, endpoint: string, path: string): CalibrationMeasuredSource | null {
  const record = optionalRecord(value, endpoint, path)
  if (record === null) {
    return null
  }
  return {
    baseline_upload_id: readString(record, 'baseline_upload_id', endpoint, path),
    applied_upload_id: readString(record, 'applied_upload_id', endpoint, path),
    sample_rate_hz: readNumber(record, 'sample_rate_hz', endpoint, path),
    source: readString(record, 'source', endpoint, path)
  }
}

function parseSimulatedSource(value: unknown, endpoint: string, path: string): CalibrationSimulatedSource | null {
  const record = optionalRecord(value, endpoint, path)
  if (record === null) {
    return null
  }
  return {
    predicted_from: readString(record, 'predicted_from', endpoint, path),
    plan_sha256: readString(record, 'plan_sha256', endpoint, path),
    graph_id: readString(record, 'graph_id', endpoint, path)
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
    offset: requireNumber(root, 'offset', endpoint, 'body'),
    reasons: parseReasons(root.reasons, endpoint)
  }
}

/** 校验 /api/calibration/jobs/{id} 响应（容忍 §7【加法】顶层 result/artifacts/ok）。 */
export function parseJobResponse(body: unknown): CalibrationJobResponse {
  const endpoint = '/api/calibration/jobs/{id}'
  const root = asRecord(body, endpoint, 'body')
  if (root.schema !== CALIBRATION_JOB_SCHEMA) {
    throw invalidResponse(endpoint, 'schema must be "' + CALIBRATION_JOB_SCHEMA + '"')
  }
  const jobRecord = asRecord(root.job, endpoint, 'job')
  return {
    schema: CALIBRATION_JOB_SCHEMA,
    job: parseJobObject(jobRecord, endpoint, 'job', root),
    reasons: parseReasons(root.reasons, endpoint)
  }
}

/**
 * 解析 job 对象；`fallback` 是详情响应顶层（§7【加法】把 result/artifacts/error 也放在顶层）。
 * steps/artifacts 缺失按空数组处理（列表摘要本来就不带），但类型不对仍然拒绝。
 */
function parseJobObject(
  jobRecord: Record<string, unknown>,
  endpoint: string,
  path: string,
  fallback?: Record<string, unknown>
): CalibrationJob {
  const summary = parseJobSummary(jobRecord, endpoint, path)
  const stepsRaw = jobRecord.steps ?? []
  const artifactsRaw = jobRecord.artifacts ?? fallback?.artifacts ?? []
  if (!Array.isArray(stepsRaw)) {
    throw invalidResponse(endpoint, path + '.steps must be an array')
  }
  if (!Array.isArray(artifactsRaw)) {
    throw invalidResponse(endpoint, path + '.artifacts must be an array')
  }
  const applyState = readEnum(jobRecord, 'apply_state', APPLY_STATES, endpoint, path)
  const appliedVersion = readNumber(jobRecord, 'applied_version', endpoint, path)
  const applyRecord = optionalRecord(jobRecord.apply, endpoint, path + '.apply')
  return {
    ...summary,
    engine: readString(jobRecord, 'engine', endpoint, path),
    step: readString(jobRecord, 'step', endpoint, path),
    apply_state: applyState,
    applied_version: appliedVersion,
    steps: stepsRaw.map((item, index) => parseJobStep(item, endpoint, path + '.steps[' + String(index) + ']')),
    artifacts: artifactsRaw.map((item, index) =>
      parseArtifact(item, endpoint, path + '.artifacts[' + String(index) + ']')
    ),
    inputs: parseJobInputs(jobRecord.inputs, endpoint, path + '.inputs'),
    // C3b1 加法字段在**详情顶层**（job 内没有）：兼容两种位置，缺失为 null。
    inputs_rt_host: parseRtHostJobInputs(
      jobRecord.inputs_rt_host ?? fallback?.inputs_rt_host,
      endpoint,
      path + '.inputs_rt_host'
    ),
    result: parseJobResult(jobRecord.result ?? fallback?.result, endpoint, path + '.result'),
    apply: applyRecord === null ? null : parseJobApply(applyRecord, applyState, appliedVersion, endpoint, path + '.apply')
  }
}

function parseJobApply(
  record: Record<string, unknown>,
  applyState: CalibrationApplyState | null,
  appliedVersion: number | null,
  endpoint: string,
  path: string
): CalibrationJobApply {
  return {
    state: readEnum(record, 'state', APPLY_STATES, endpoint, path) ?? applyState,
    applied_version: readNumber(record, 'applied_version', endpoint, path) ?? appliedVersion,
    previous_version: readNumber(record, 'previous_version', endpoint, path),
    applied_at: readString(record, 'applied_at', endpoint, path),
    plan_sha256: readString(record, 'plan_sha256', endpoint, path)
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

/**
 * 校验 GET/PUT /api/calibration/wizard 响应。
 * daemon 实际字段：schema/config/defaults/saved/updated_at/reasons（+set_fields）；
 * 合同 §7 只保证 schema/config/state（"..."）：defaults 缺失时回退到 config（同一形状），不拒绝。
 */
export function parseWizardResponse(body: unknown): CalibrationWizardResponse {
  const endpoint = '/api/calibration/wizard'
  const root = asRecord(body, endpoint, 'body')
  if (root.schema !== CALIBRATION_WIZARD_SCHEMA) {
    throw invalidResponse(endpoint, 'schema must be "' + CALIBRATION_WIZARD_SCHEMA + '"')
  }
  const config = parseWizardConfig(root.config, endpoint, 'config')
  return {
    schema: CALIBRATION_WIZARD_SCHEMA,
    config,
    defaults: root.defaults === undefined ? config : parseWizardConfig(root.defaults, endpoint, 'defaults'),
    saved: readBoolean(root, 'saved', endpoint, 'body') ?? false,
    updated_at: readString(root, 'updated_at', endpoint, 'body'),
    reasons: parseReasons(root.reasons, endpoint),
    set_fields: readStringArray(root, 'set_fields', endpoint, 'body')
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

/** 校验 POST /api/calibration/uploads 响应（201 新建 / 200 同 sha 幂等）。 */
export function parseUploadResponse(body: unknown): CalibrationUploadResult {
  const endpoint = '/api/calibration/uploads'
  const root = asRecord(body, endpoint, 'body')
  return {
    upload_id: requireString(root, 'upload_id', endpoint, 'body'),
    sha256: requireString(root, 'sha256', endpoint, 'body'),
    bytes: requireNumber(root, 'bytes', endpoint, 'body'),
    schema: readString(root, 'schema', endpoint, 'body'),
    created: readBoolean(root, 'created', endpoint, 'body')
  }
}

/** 校验 POST /api/calibration/jobs 响应（202 {job_id,location}；容忍 ok/state）。 */
export function parseCreateJobResponse(body: unknown): CalibrationCreateJobResult {
  const endpoint = '/api/calibration/jobs'
  const root = asRecord(body, endpoint, 'body')
  return {
    job_id: requireString(root, 'job_id', endpoint, 'body'),
    location: readString(root, 'location', endpoint, 'body'),
    status: readAliasedEnum(root, ['state', 'status'], JOB_STATUSES, endpoint, 'body', false) ?? 'queued'
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

/** apply 回执里可能内嵌的 job（{schema, job:{...}}）；不对就按缺失处理，不让回执整体失败。 */
function parseEmbeddedJob(value: unknown, endpoint: string): CalibrationJob | null {
  const record = optionalRecord(value, endpoint, 'job')
  if (record === null) {
    return null
  }
  const jobRecord = optionalRecord(record.job, endpoint, 'job.job')
  if (jobRecord === null) {
    return null
  }
  return parseJobObject(jobRecord, endpoint, 'job.job')
}

function parseApplySnapshot(value: unknown, endpoint: string): CalibrationApplySnapshot | null {
  const record = optionalRecord(value, endpoint, 'snapshot')
  if (record === null) {
    return null
  }
  return {
    version: readNumber(record, 'version', endpoint, 'snapshot'),
    graph_id: readString(record, 'graph_id', endpoint, 'snapshot'),
    sha256: readString(record, 'sha256', endpoint, 'snapshot')
  }
}

function readReadback(record: Record<string, unknown>, endpoint: string): Record<string, unknown> | null {
  const value = record.readback
  if (value === null || value === undefined) {
    return null
  }
  return asRecord(value, endpoint, 'readback')
}

/**
 * 校验 POST /jobs/{id}/apply 的回执：兼容
 *  - 旧单发：{applied:true, applied_version, graph_id, snapshot, ack, readback, applied_at}；
 *  - staged prepare：{stage:"prepare", engine_token, job_id, plan_sha256, previous_version}（**未生效**）；
 *  - staged commit：{stage:"commit", applied:true, ...}；
 *  - staged abort：{stage:"abort", aborted:true, ...}。
 * commit/abort 缺 engine_token 或语义字段（applied/aborted 非 true）一律拒绝，不把草稿当生效。
 */
export function parseApplyOutcome(body: unknown): CalibrationApplyOutcome {
  const endpoint = '/api/calibration/jobs/{id}/apply'
  const root = asRecord(body, endpoint, 'body')
  const stage = readEnum(root, 'stage', STAGES, endpoint, 'body')
  if (stage === 'prepare') {
    return {
      kind: 'prepare',
      stage: 'prepare',
      job_id: readString(root, 'job_id', endpoint, 'body'),
      engine_token: requireString(root, 'engine_token', endpoint, 'body'),
      plan_sha256: readString(root, 'plan_sha256', endpoint, 'body'),
      previous_version: readNumber(root, 'previous_version', endpoint, 'body'),
      target_version: readNumber(root, 'target_version', endpoint, 'body'),
      job: parseEmbeddedJob(root.job, endpoint),
      reasons: parseReasons(root.reasons, endpoint),
      raw: root
    }
  }
  if (stage === 'abort') {
    if (readBoolean(root, 'aborted', endpoint, 'body') !== true) {
      throw invalidResponse(endpoint, 'aborted must be true for stage=abort')
    }
    return {
      kind: 'abort',
      stage: 'abort',
      aborted: true,
      job_id: readString(root, 'job_id', endpoint, 'body'),
      plan_sha256: readString(root, 'plan_sha256', endpoint, 'body'),
      target_version: readNumber(root, 'target_version', endpoint, 'body'),
      control: optionalRecord(root.control, endpoint, 'control'),
      reasons: parseReasons(root.reasons, endpoint),
      raw: root
    }
  }
  // stage=commit 或旧单发：必须是真生效回执（applied:true）。
  if (readBoolean(root, 'applied', endpoint, 'body') !== true) {
    throw invalidResponse(endpoint, 'applied must be true (single-shot or stage=commit)')
  }
  return {
    kind: 'applied',
    stage: stage === 'commit' ? 'commit' : 'single',
    applied: true,
    job_id: readString(root, 'job_id', endpoint, 'body'),
    applied_version: readNumber(root, 'applied_version', endpoint, 'body'),
    previous_version: readNumber(root, 'previous_version', endpoint, 'body'),
    graph_id: readString(root, 'graph_id', endpoint, 'body'),
    sha256: readString(root, 'sha256', endpoint, 'body'),
    snapshot: parseApplySnapshot(root.snapshot, endpoint),
    ack: parseAck(root.ack, endpoint),
    readback: readReadback(root, endpoint),
    applied_at: readString(root, 'applied_at', endpoint, 'body'),
    job: parseEmbeddedJob(root.job, endpoint),
    reasons: parseReasons(root.reasons, endpoint)
  }
}

/**
 * 校验 POST /jobs/{id}/rollback 的回执：兼容旧单发、staged prepare/commit/abort。
 * 与 apply 相同：prepare 只是草稿（未生效），commit/abort 必须语义字段为 true。
 */
export function parseRollbackOutcome(body: unknown): CalibrationRollbackOutcome {
  const endpoint = '/api/calibration/jobs/{id}/rollback'
  const root = asRecord(body, endpoint, 'body')
  const stage = readEnum(root, 'stage', STAGES, endpoint, 'body')
  if (stage === 'prepare') {
    return {
      kind: 'prepare',
      stage: 'prepare',
      job_id: readString(root, 'job_id', endpoint, 'body'),
      engine_token: requireString(root, 'engine_token', endpoint, 'body'),
      plan_sha256: readString(root, 'plan_sha256', endpoint, 'body'),
      previous_version: readNumber(root, 'previous_version', endpoint, 'body'),
      target_version: readNumber(root, 'target_version', endpoint, 'body'),
      job: parseEmbeddedJob(root.job, endpoint),
      reasons: parseReasons(root.reasons, endpoint),
      raw: root
    }
  }
  if (stage === 'abort') {
    if (readBoolean(root, 'aborted', endpoint, 'body') !== true) {
      throw invalidResponse(endpoint, 'aborted must be true for stage=abort')
    }
    return {
      kind: 'abort',
      stage: 'abort',
      aborted: true,
      job_id: readString(root, 'job_id', endpoint, 'body'),
      plan_sha256: readString(root, 'plan_sha256', endpoint, 'body'),
      target_version: readNumber(root, 'target_version', endpoint, 'body'),
      control: optionalRecord(root.control, endpoint, 'control'),
      reasons: parseReasons(root.reasons, endpoint),
      raw: root
    }
  }
  if (readBoolean(root, 'rolled_back', endpoint, 'body') !== true) {
    throw invalidResponse(endpoint, 'rolled_back must be true (single-shot or stage=commit)')
  }
  return {
    kind: 'rolled_back',
    stage: stage === 'commit' ? 'commit' : 'single',
    rolled_back: true,
    job_id: readString(root, 'job_id', endpoint, 'body'),
    restored_version: readNumber(root, 'restored_version', endpoint, 'body'),
    applied_version: readNumber(root, 'applied_version', endpoint, 'body'),
    graph_id: readString(root, 'graph_id', endpoint, 'body'),
    sha256: readString(root, 'sha256', endpoint, 'body'),
    readback: readReadback(root, endpoint),
    rolled_back_at: readString(root, 'rolled_back_at', endpoint, 'body'),
    job: parseEmbeddedJob(root.job, endpoint),
    reasons: parseReasons(root.reasons, endpoint)
  }
}

/**
 * 校验 POST /api/calibration/wizard/apply 的回执。
 * canonical（docs/API.md §7 裁决 H）：apply 回执（applied/job_id/applied_version/snapshot/ack/readback/applied_at/job/reasons）；
 * 无 job 时 409 no_applicable_job。旧文档的 {job_id,location} 仅作防御性兼容；都不像就拒绝。
 */
export function parseWizardApplyOutcome(body: unknown): CalibrationWizardApplyOutcome {
  const endpoint = '/api/calibration/wizard/apply'
  const root = asRecord(body, endpoint, 'body')
  const hasApplyFields =
    root.stage !== undefined || readBoolean(root, 'applied', endpoint, 'body') !== null || root.rolled_back !== undefined
  if (hasApplyFields) {
    return parseApplyOutcome(body)
  }
  const jobId = readString(root, 'job_id', endpoint, 'body')
  const location = readString(root, 'location', endpoint, 'body')
  if (jobId !== null || location !== null) {
    return { kind: 'job', job_id: jobId, location, raw: root }
  }
  throw invalidResponse(endpoint, 'response has neither an apply receipt nor job_id/location')
}

/** 校验 cancel 响应（{job_id,status}；状态 canonical 为 cancelled 双 l）。 */
export function parseCancelResult(body: unknown): { job_id: string | null; status: CalibrationJobStatus | null } {
  const endpoint = '/api/calibration/jobs/{id}/cancel'
  if (body === undefined || body === null) {
    return { job_id: null, status: null }
  }
  const root = asRecord(body, endpoint, 'body')
  return {
    job_id: readString(root, 'job_id', endpoint, 'body'),
    status: readAliasedEnum(root, ['status', 'state'], JOB_STATUSES, endpoint, 'body', false)
  }
}

/* -------------------------------------------------------- 产物下载（带认证） */

/**
 * 从 Content-Disposition 解析下载文件名（RFC 6266 的 filename / filename* 都支持）。
 * system.ts 有同款实现；校准产物下载独立实现，避免跨区域耦合。
 */
export function parseContentDispositionFilename(header: string | null | undefined): string | null {
  if (typeof header !== 'string' || header.length === 0) {
    return null
  }
  const star = /filename\*\s*=\s*([^;]+)/i.exec(header)
  if (star !== null && star[1] !== undefined) {
    let value = star[1].trim()
    if (value.startsWith('"') && value.endsWith('"')) {
      value = value.slice(1, -1)
    }
    const encoded = /^[^']*'[^']*'(.*)$/.exec(value)
    if (encoded !== null && encoded[1] !== undefined) {
      try {
        return decodeURIComponent(encoded[1])
      } catch {
        return null
      }
    }
    return value.length > 0 ? value : null
  }
  const plain = /filename\s*=\s*("[^"]*"|[^;]*)/i.exec(header)
  if (plain === null || plain[1] === undefined) {
    return null
  }
  let value = plain[1].trim()
  if (value.startsWith('"') && value.endsWith('"')) {
    value = value.slice(1, -1)
  }
  return value.length > 0 ? value : null
}

/** 校验 GET /api/calibration/jobs/{id}/artifacts/{name} 的原始字节响应（下载走 getRawBytes）。 */
export function parseArtifactDownload(jobId: string, name: string, raw: RawBytesResponse): CalibrationArtifactDownload {
  const fromHeader = parseContentDispositionFilename(raw.headers.get('Content-Disposition'))
  const bytes = raw.bytes
  let text = ''
  try {
    text = new TextDecoder('utf-8').decode(bytes)
  } catch {
    text = ''
  }
  return {
    job_id: jobId,
    name,
    filename: fromHeader ?? name,
    content_type: raw.headers.get('Content-Type'),
    sha256: raw.headers.get('X-KHAEN-Artifact-SHA256'),
    status: raw.status,
    data: bytes,
    text,
    byte_length: bytes.byteLength,
    from_content_disposition: fromHeader !== null
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
  /**
   * POST /jobs/{id}/apply。省略 request = 旧单发（body `{}`）；
   * 传 {stage:"prepare"} 返回草稿+token（未生效），然后再传 commit/abort。
   */
  applyJob(
    jobId: string,
    request?: CalibrationStagedRequest | null,
    options?: CalibrationRequestOptions
  ): Promise<CalibrationApplyOutcome>
  /** POST /jobs/{id}/rollback，语义同 applyJob。 */
  rollbackJob(
    jobId: string,
    request?: CalibrationStagedRequest | null,
    options?: CalibrationRequestOptions
  ): Promise<CalibrationRollbackOutcome>
  upload(body: ArrayBuffer | Blob, options?: CalibrationUploadOptions): Promise<CalibrationUploadResult>
  getWizard(options?: CalibrationRequestOptions): Promise<CalibrationWizardResponse>
  putWizard(request: WizardSaveRequest, options?: CalibrationRequestOptions): Promise<CalibrationWizardResponse>
  getWizardState(options?: CalibrationRequestOptions): Promise<CalibrationWizardStateResponse>
  /** POST /wizard/apply {job_id?}：回执可能是 apply 结果，也可能是 {job_id,location}。 */
  applyWizard(jobId?: string | null, options?: CalibrationRequestOptions): Promise<CalibrationWizardApplyOutcome>
  /** GET /jobs/{id}/artifacts/{name} 的带认证下载（getRawBytes：Cookie 会话 + 统一 401/错误体处理，无损字节）。 */
  downloadArtifact(jobId: string, name: string, options?: CalibrationRequestOptions): Promise<CalibrationArtifactDownload>
  /**
   * 产物下载直链（仅供调试/测试拼 URL）。页面**不得**直接用 <a href> 下载：
   * 该路径需要会话 Cookie，必须走 downloadArtifact 的带认证请求。
   */
  artifactUrl(jobId: string, name: string): string
}

function jobPath(jobId: string): string {
  return '/api/calibration/jobs/' + encodeURIComponent(jobId)
}

/** 产物下载直链（调试/测试用；浏览器 JS 下载必须走 downloadArtifact）。 */
export function calibrationArtifactUrl(jobId: string, name: string): string {
  return jobPath(jobId) + '/artifacts/' + encodeURIComponent(name)
}

/** 把可选的 staged 请求转成请求体：省略 = 旧单发 `{}`（合同 §7 当前后端单发语义）。 */
function stagedBody(request: CalibrationStagedRequest | null | undefined): Record<string, unknown> {
  if (request === null || request === undefined) {
    return {}
  }
  if (request.stage === 'prepare') {
    return { stage: 'prepare' }
  }
  return { stage: request.stage, engine_token: request.engine_token }
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
      // 合同 §7：cancel 无请求体；状态由服务端回执给出（cancelled 双 l）。
      const body = await client.post<unknown>(jobPath(jobId) + '/cancel', undefined, {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseCancelResult(body)
    },

    async applyJob(jobId, request = null, options = {}) {
      const body = await client.post<unknown>(jobPath(jobId) + '/apply', stagedBody(request), {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseApplyOutcome(body)
    },

    async rollbackJob(jobId, request = null, options = {}) {
      const body = await client.post<unknown>(jobPath(jobId) + '/rollback', stagedBody(request), {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseRollbackOutcome(body)
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

    async applyWizard(jobId = null, options = {}) {
      // 合同 §7：{job_id?}；省略 = 应用最近成功 job（daemon 语义）。
      const body = jobId === null ? {} : { job_id: jobId }
      const reply = await client.post<unknown>('/api/calibration/wizard/apply', body, {
        timeoutMs: CALIBRATION_FETCH_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseWizardApplyOutcome(reply)
    },

    async downloadArtifact(jobId, name, options = {}) {
      // 带认证下载：getRawBytes 返回无损字节（二进制产物不能被 text 解码破坏）。
      const raw = await client.getRawBytes(calibrationArtifactUrl(jobId, name), {
        timeoutMs: CALIBRATION_UPLOAD_TIMEOUT_MS,
        ...(options.signal ? { signal: options.signal } : {})
      })
      return parseArtifactDownload(jobId, name, raw)
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

/** 该 job kind 是否为 rt-host 真实测量（C3b1；UI 据此区分真实测量与离线闭环两条路径）。 */
export function isRtHostJobKind(kind: string | null | undefined): boolean {
  return kind === JOB_KIND_SINGLE_POINT_RT_HOST
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

/**
 * job 结果数值的真实性徽标（合同 §7：measured / simulated 必须如实分开标注）：
 * - result.measured 非 null -> 实测（actual）；
 * - 否则 result.simulated 非 null -> 示意（demo，不得写成实测）；
 * - 两者都未上报时按引擎标识判定（engine=synthetic/mock -> 示意）。
 */
export function resultTruth(
  result: CalibrationJobResult | null | undefined,
  engine: string | null | undefined
): TruthKind {
  if (result !== null && result !== undefined) {
    if (result.measured !== null && result.measured !== undefined) {
      return 'actual'
    }
    if (result.simulated !== null && result.simulated !== undefined) {
      return 'demo'
    }
  }
  return jobResultTruth(engine)
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
  unsupported: 'not_wired',
  switch_busy: 'busy',
  mic_profile_missing: 'conflict',
  job_not_active: 'busy',
  job_not_succeeded: 'conflict',
  plan_unavailable: 'conflict',
  rollback_unavailable: 'conflict',
  wizard_not_saved: 'conflict',
  artifact_sha_mismatch: 'conflict',
  engine_token_invalid: 'conflict',
  no_applicable_job: 'conflict',
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
