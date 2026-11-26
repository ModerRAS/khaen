/**
 * 校准 store（PLAN §4.7 / §10；任务 T37）。
 *
 * 职责：
 * - 向导数据：GET /api/calibration/wizard（配置+默认值）与 GET /api/calibration/wizard/state（每步 gate）；
 *   gate 只认后端 steps[].status，前端不自行判过（缺失/畸形 state 一律错误态，不放行）；
 * - job 列表：GET /api/calibration/jobs（分页 / available=false 时如实「未接入」）；
 * - 测量上传：POST /api/calibration/uploads（原始字节）+ 可选 PUT /wizard 的 measurement 草稿
 *   （刷新后第 5 步可恢复）；sha256/bytes 只信服务端返回；
 * - job 轮询：创建/重试后按 JOB_POLL_INTERVAL_MS 轮询 GET /jobs/{id}，终态即停；页面隐藏暂停；
 *   所有进度来自真实 job 状态（无假进度条）；
 * - apply/rollback：POST /jobs/{id}/apply|rollback 与 POST /wizard/apply；结果回读 + 刷新 gate；
 * - 401 交给共享 ApiClient 的 onUnauthorized（会话 store 清态跳登录），本模块只归类展示。
 *
 * 可测试性：createCalibrationController(state, deps) 是纯逻辑工厂（注入 api/时钟/定时器），
 * 模块底部用 reactive state + 共享 ApiClient 组装单例供 Vue 组件使用。
 */
import { reactive } from 'vue'
import { apiClient } from './session.ts'
import {
  CALIBRATION_UPLOAD_TIMEOUT_MS,
  JOB_POLL_INTERVAL_MS,
  classifyCalibrationError,
  createCalibrationApi,
  isJobTerminal,
  type CalibrationApi,
  type CalibrationApplyResult,
  type CalibrationCreateJobInputs,
  type CalibrationErrorInfo,
  type CalibrationJob,
  type CalibrationJobStatus,
  type CalibrationJobsResponse,
  type CalibrationRollbackResult,
  type CalibrationUploadResult,
  type CalibrationWizardResponse,
  type CalibrationWizardStateResponse,
  type WizardConfig,
  type WizardMeasurementDraft,
  type WizardSaveRequest
} from '../api/calibration.ts'

export type CalibrationPhase = 'loading' | 'ready' | 'error' | 'unavailable'
export type JobPhase = 'idle' | 'loading' | 'ready' | 'error'
export type CalibrationBusyKey = 'wizard' | 'jobs' | 'upload' | 'saveWizard' | 'createJob' | 'cancelJob' | 'apply' | 'rollback'

export interface MeasurementDraft {
  baseline: CalibrationUploadResult | null
  applied: CalibrationUploadResult | null
}

export interface CalibrationState {
  /** 向导配置（GET /wizard）。 */
  wizard: CalibrationWizardResponse | null
  /** 向导状态 / 每步 gate（GET /wizard/state）。 */
  wizardState: CalibrationWizardStateResponse | null
  wizardPhase: CalibrationPhase
  wizardError: CalibrationErrorInfo | null
  /** job 列表（GET /jobs）。 */
  jobs: CalibrationJobsResponse | null
  jobsPhase: CalibrationPhase
  jobsError: CalibrationErrorInfo | null
  /** 当前查看/轮询的 job 详情。 */
  job: CalibrationJob | null
  jobPhase: JobPhase
  jobError: CalibrationErrorInfo | null
  /** job 状态轮询是否开启。 */
  polling: boolean
  /** 本会话上传结果（仅展示/兜底；gate 仍由后端 state 驱动）。 */
  uploads: { baseline: CalibrationUploadResult | null; applied: CalibrationUploadResult | null; micCal: CalibrationUploadResult | null }
  /** 测量草稿（PUT /wizard measurement 扩展；刷新后从 state.measurement 恢复）。 */
  measurementDraft: WizardMeasurementDraft
  busy: Record<CalibrationBusyKey, boolean>
  /** 最近一次动作失败（页面横幅/表单提示）。 */
  actionError: CalibrationErrorInfo | null
  /** 最近一次 apply / rollback 回执（第 8 步/job 页展示）。 */
  lastApply: CalibrationApplyResult | null
  lastRollback: CalibrationRollbackResult | null
}

export function createInitialCalibrationState(): CalibrationState {
  return {
    wizard: null,
    wizardState: null,
    wizardPhase: 'loading',
    wizardError: null,
    jobs: null,
    jobsPhase: 'loading',
    jobsError: null,
    job: null,
    jobPhase: 'idle',
    jobError: null,
    polling: false,
    uploads: { baseline: null, applied: null, micCal: null },
    measurementDraft: { baseline_upload_id: null, applied_upload_id: null, plan_sha: null },
    busy: {
      wizard: false,
      jobs: false,
      upload: false,
      saveWizard: false,
      createJob: false,
      cancelJob: false,
      apply: false,
      rollback: false
    },
    actionError: null,
    lastApply: null,
    lastRollback: null
  }
}

export type CalibrationActionResult<T> = { ok: true; value: T } | { ok: false; error: CalibrationErrorInfo }

export interface MeasurementUploadInput {
  /** 原始文件字节（File.arrayBuffer()）；上传用 ArrayBuffer，解析 WAV 时由调用方转 Uint8Array。 */
  bytes: ArrayBuffer
  filename: string
  contentType: string
  /** 只用于 mock 区分 baseline/applied；真实 daemon 可忽略。 */
  role: 'baseline' | 'applied'
}

/** 定时器句柄（浏览器 number / Node Timeout，测试注入任意值）。 */
export type TimerHandle = unknown

export interface CalibrationControllerDeps {
  api: CalibrationApi
  now?: () => number
  schedule?: (callback: () => void, delayMs: number) => TimerHandle
  cancel?: (handle: TimerHandle) => void
  intervalMs?: number
}

export interface CalibrationController {
  loadWizard(): Promise<void>
  loadWizardState(): Promise<void>
  loadJobs(params?: { limit?: number; offset?: number }): Promise<void>
  saveWizard(config: WizardConfig, measurement?: WizardMeasurementDraft): Promise<CalibrationActionResult<CalibrationWizardResponse>>
  uploadMeasurement(input: MeasurementUploadInput): Promise<CalibrationActionResult<CalibrationUploadResult>>
  uploadMicCal(input: { bytes: ArrayBuffer; filename: string; contentType: string }): Promise<CalibrationActionResult<CalibrationUploadResult>>
  createJob(inputs: CalibrationCreateJobInputs): Promise<CalibrationActionResult<{ job_id: string }>>
  loadJob(jobId: string): Promise<CalibrationActionResult<CalibrationJob>>
  startJobPolling(jobId: string): void
  stopJobPolling(): void
  pauseJobPolling(): void
  resumeJobPolling(): void
  cancelJob(jobId: string): Promise<CalibrationActionResult<CalibrationJobStatus | null>>
  applyJob(jobId: string): Promise<CalibrationActionResult<CalibrationApplyResult>>
  applyWizard(): Promise<CalibrationActionResult<CalibrationApplyResult>>
  rollbackJob(jobId: string): Promise<CalibrationActionResult<CalibrationRollbackResult>>
  refreshAll(params?: { limit?: number; offset?: number }): Promise<void>
  clearActionError(): void
  reset(): void
}

function isNotWired(error: CalibrationErrorInfo | null): boolean {
  return error !== null && error.category === 'not_wired'
}

/** 纯逻辑校准控制器：状态写入 state（Vue reactive 或普通对象均可）。 */
export function createCalibrationController(state: CalibrationState, deps: CalibrationControllerDeps): CalibrationController {
  const schedule =
    deps.schedule ?? ((callback: () => void, delayMs: number): TimerHandle => setTimeout(callback, delayMs))
  const cancelTimer =
    deps.cancel ?? ((handle: TimerHandle): void => clearTimeout(handle as ReturnType<typeof setTimeout>))
  const intervalMs = deps.intervalMs ?? JOB_POLL_INTERVAL_MS

  let timer: TimerHandle | null = null
  let pollingJobId: string | null = null
  let paused = false
  let pollInflight: Promise<void> | null = null

  function clearTimer(): void {
    if (timer !== null) {
      cancelTimer(timer)
      timer = null
    }
  }

  function setBusy(key: CalibrationBusyKey, value: boolean): void {
    state.busy[key] = value
  }

  function setWizardError(error: CalibrationErrorInfo): void {
    state.wizardError = error
    state.wizardPhase = isNotWired(error) ? 'unavailable' : 'error'
  }

  function applyWizardResponse(response: CalibrationWizardResponse): void {
    state.wizard = response
    state.wizardError = null
    state.wizardPhase = 'ready'
  }

  function applyWizardState(response: CalibrationWizardStateResponse): void {
    state.wizardState = response
    // state.measurement 是后端事实：有就同步到本地草稿（刷新恢复第 5 步），没有就保留本地。
    const measurement = response.measurement
    if (measurement.baseline_upload_id !== null) {
      state.measurementDraft.baseline_upload_id = measurement.baseline_upload_id
    }
    if (measurement.applied_upload_id !== null) {
      state.measurementDraft.applied_upload_id = measurement.applied_upload_id
    }
  }

  async function loadWizard(): Promise<void> {
    setBusy('wizard', true)
    try {
      const [wizard, wizardState] = await Promise.all([deps.api.getWizard(), deps.api.getWizardState()])
      applyWizardResponse(wizard)
      applyWizardState(wizardState)
      state.wizardError = null
      state.wizardPhase = 'ready'
    } catch (error) {
      setWizardError(classifyCalibrationError(error))
    } finally {
      setBusy('wizard', false)
    }
  }

  async function loadWizardState(): Promise<void> {
    try {
      const response = await deps.api.getWizardState()
      applyWizardState(response)
      // 只有配置也拿到了才算 ready；否则保留 loadWizard 的错误态（向导页需要配置）。
      if (state.wizard !== null && state.wizardPhase !== 'ready') {
        state.wizardPhase = 'ready'
        state.wizardError = null
      }
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.wizardError = info
      state.wizardPhase = isNotWired(info) ? 'unavailable' : 'error'
    }
  }

  async function loadJobs(params: { limit?: number; offset?: number } = {}): Promise<void> {
    setBusy('jobs', true)
    if (state.jobs === null) {
      state.jobsPhase = 'loading'
    }
    try {
      const response = await deps.api.listJobs(params)
      state.jobs = response
      state.jobsError = null
      state.jobsPhase = response.available ? 'ready' : 'unavailable'
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.jobsError = info
      state.jobsPhase = isNotWired(info) ? 'unavailable' : 'error'
    } finally {
      setBusy('jobs', false)
    }
  }

  function measurementDraftFromState(planSha: string | null): WizardMeasurementDraft {
    return {
      baseline_upload_id: state.measurementDraft.baseline_upload_id,
      applied_upload_id: state.measurementDraft.applied_upload_id,
      plan_sha: planSha
    }
  }

  async function saveWizard(
    config: WizardConfig,
    measurement?: WizardMeasurementDraft
  ): Promise<CalibrationActionResult<CalibrationWizardResponse>> {
    setBusy('saveWizard', true)
    state.actionError = null
    try {
      const draft = measurement ?? measurementDraftFromState(null)
      const request: WizardSaveRequest = { ...config, measurement: draft }
      const response = await deps.api.putWizard(request)
      applyWizardResponse(response)
      state.measurementDraft = { ...draft }
      await loadWizardState()
      return { ok: true, value: response }
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.actionError = info
      return { ok: false, error: info }
    } finally {
      setBusy('saveWizard', false)
    }
  }

  async function uploadMeasurement(input: MeasurementUploadInput): Promise<CalibrationActionResult<CalibrationUploadResult>> {
    setBusy('upload', true)
    state.actionError = null
    try {
      const result = await deps.api.upload(input.bytes, {
        contentType: input.contentType,
        filename: input.filename,
        role: input.role
      })
      if (input.role === 'baseline') {
        state.uploads.baseline = result
        state.measurementDraft.baseline_upload_id = result.upload_id
      } else {
        state.uploads.applied = result
        state.measurementDraft.applied_upload_id = result.upload_id
      }
      // 若向导配置已保存，把 measurement 草稿持久化，刷新后第 5 步仍能恢复（daemon 扩展字段）。
      if (state.wizard !== null && state.wizard.saved) {
        try {
          await deps.api.putWizard({ ...state.wizard.config, measurement: { ...state.measurementDraft } })
        } catch {
          // 草稿持久化失败不掩盖上传成功；gate 仍以 state 为准（页面会提示需重新上传）。
        }
      }
      await loadWizardState()
      return { ok: true, value: result }
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.actionError = info
      return { ok: false, error: info }
    } finally {
      setBusy('upload', false)
    }
  }

  async function uploadMicCal(input: {
    bytes: ArrayBuffer
    filename: string
    contentType: string
  }): Promise<CalibrationActionResult<CalibrationUploadResult>> {
    setBusy('upload', true)
    state.actionError = null
    try {
      const result = await deps.api.upload(input.bytes, {
        contentType: input.contentType,
        filename: input.filename,
        role: 'mic_cal'
      })
      state.uploads.micCal = result
      return { ok: true, value: result }
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.actionError = info
      return { ok: false, error: info }
    } finally {
      setBusy('upload', false)
    }
  }

  function stopJobPolling(): void {
    clearTimer()
    state.polling = false
    pollingJobId = null
    paused = false
  }

  function scheduleNext(): void {
    clearTimer()
    if (pollingJobId === null || paused) {
      return
    }
    timer = schedule(() => {
      timer = null
      void pollOnce()
    }, intervalMs)
  }

  async function pollOnce(): Promise<void> {
    if (pollInflight !== null) {
      return pollInflight
    }
    const jobId = pollingJobId
    if (jobId === null) {
      return
    }
    const started = (async () => {
      try {
        const response = await deps.api.getJob(jobId)
        state.job = response.job
        state.jobPhase = 'ready'
        state.jobError = null
        if (isJobTerminal(response.job.status)) {
          stopJobPolling()
          await loadWizardState()
          return
        }
      } catch (error) {
        const info = classifyCalibrationError(error)
        state.jobError = info
        state.jobPhase = state.job === null ? 'error' : 'ready'
        // 网络/超时/5xx 继续轮询（后端可能恢复）；404/401/not_wired 停止。
        if (info.category === 'not_found' || info.category === 'unauthorized' || info.category === 'not_wired') {
          stopJobPolling()
        }
      } finally {
        pollInflight = null
      }
      scheduleNext()
    })()
    pollInflight = started
    return started
  }

  function startJobPolling(jobId: string): void {
    if (pollingJobId === jobId && state.polling) {
      return
    }
    clearTimer()
    pollingJobId = jobId
    paused = false
    state.polling = true
    void pollOnce()
  }

  function pauseJobPolling(): void {
    if (!state.polling) {
      return
    }
    paused = true
    clearTimer()
  }

  function resumeJobPolling(): void {
    if (!state.polling || !paused) {
      return
    }
    paused = false
    void pollOnce()
  }

  async function createJob(
    inputs: CalibrationCreateJobInputs
  ): Promise<CalibrationActionResult<{ job_id: string }>> {
    setBusy('createJob', true)
    state.actionError = null
    try {
      const created = await deps.api.createJob({ kind: 'single_point_offline', inputs })
      state.job = null
      state.jobPhase = 'loading'
      state.jobError = null
      // 进度全部来自真实 job 状态轮询（创建响应只有 job_id，不猜状态）。
      startJobPolling(created.job_id)
      return { ok: true, value: { job_id: created.job_id } }
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.actionError = info
      state.jobPhase = 'idle'
      return { ok: false, error: info }
    } finally {
      setBusy('createJob', false)
    }
  }

  async function loadJob(jobId: string): Promise<CalibrationActionResult<CalibrationJob>> {
    state.jobPhase = 'loading'
    state.jobError = null
    try {
      const response = await deps.api.getJob(jobId)
      state.job = response.job
      state.jobPhase = 'ready'
      return { ok: true, value: response.job }
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.jobError = info
      state.jobPhase = 'error'
      return { ok: false, error: info }
    }
  }

  async function cancelJob(jobId: string): Promise<CalibrationActionResult<CalibrationJobStatus | null>> {
    setBusy('cancelJob', true)
    state.actionError = null
    try {
      const result = await deps.api.cancelJob(jobId)
      const job = await deps.api.getJob(jobId)
      state.job = job.job
      state.jobPhase = 'ready'
      stopJobPolling()
      await loadWizardState()
      return { ok: true, value: result.status ?? job.job.status }
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.actionError = info
      return { ok: false, error: info }
    } finally {
      setBusy('cancelJob', false)
    }
  }

  async function applyJob(jobId: string): Promise<CalibrationActionResult<CalibrationApplyResult>> {
    setBusy('apply', true)
    state.actionError = null
    try {
      const result = await deps.api.applyJob(jobId)
      state.lastApply = result
      await loadWizardState()
      return { ok: true, value: result }
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.actionError = info
      return { ok: false, error: info }
    } finally {
      setBusy('apply', false)
    }
  }

  async function applyWizard(): Promise<CalibrationActionResult<CalibrationApplyResult>> {
    setBusy('apply', true)
    state.actionError = null
    try {
      const result = await deps.api.applyWizard()
      state.lastApply = result
      await loadWizardState()
      return { ok: true, value: result }
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.actionError = info
      return { ok: false, error: info }
    } finally {
      setBusy('apply', false)
    }
  }

  async function rollbackJob(jobId: string): Promise<CalibrationActionResult<CalibrationRollbackResult>> {
    setBusy('rollback', true)
    state.actionError = null
    try {
      const result = await deps.api.rollbackJob(jobId)
      state.lastRollback = result
      await loadWizardState()
      return { ok: true, value: result }
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.actionError = info
      return { ok: false, error: info }
    } finally {
      setBusy('rollback', false)
    }
  }

  async function refreshAll(params: { limit?: number; offset?: number } = {}): Promise<void> {
    await Promise.all([loadWizard(), loadJobs(params)])
  }

  function clearActionError(): void {
    state.actionError = null
  }

  function reset(): void {
    stopJobPolling()
    Object.assign(state, createInitialCalibrationState())
  }

  return {
    loadWizard,
    loadWizardState,
    loadJobs,
    saveWizard,
    uploadMeasurement,
    uploadMicCal,
    createJob,
    loadJob,
    startJobPolling,
    stopJobPolling,
    pauseJobPolling,
    resumeJobPolling,
    cancelJob,
    applyJob,
    applyWizard,
    rollbackJob,
    refreshAll,
    clearActionError,
    reset
  }
}

/* ---------------------------------------------------------------- 单例装配 */

export const calibrationState = reactive<CalibrationState>(createInitialCalibrationState())

const calibrationApi = createCalibrationApi(apiClient)

const controller = createCalibrationController(calibrationState, { api: calibrationApi })

export function loadCalibrationWizard(): Promise<void> {
  return controller.loadWizard()
}

export function loadCalibrationWizardState(): Promise<void> {
  return controller.loadWizardState()
}

export function loadCalibrationJobs(params?: { limit?: number; offset?: number }): Promise<void> {
  return controller.loadJobs(params)
}

export function saveCalibrationWizard(
  config: WizardConfig,
  measurement?: WizardMeasurementDraft
): Promise<CalibrationActionResult<CalibrationWizardResponse>> {
  return controller.saveWizard(config, measurement)
}

export function uploadCalibrationMeasurement(
  input: MeasurementUploadInput
): Promise<CalibrationActionResult<CalibrationUploadResult>> {
  return controller.uploadMeasurement(input)
}

export function uploadCalibrationMicCal(input: {
  bytes: ArrayBuffer
  filename: string
  contentType: string
}): Promise<CalibrationActionResult<CalibrationUploadResult>> {
  return controller.uploadMicCal(input)
}

export function createCalibrationJob(
  inputs: CalibrationCreateJobInputs
): Promise<CalibrationActionResult<{ job_id: string }>> {
  return controller.createJob(inputs)
}

export function loadCalibrationJob(jobId: string): Promise<CalibrationActionResult<CalibrationJob>> {
  return controller.loadJob(jobId)
}

export function startCalibrationJobPolling(jobId: string): void {
  controller.startJobPolling(jobId)
}

export function stopCalibrationJobPolling(): void {
  controller.stopJobPolling()
}

export function pauseCalibrationJobPolling(): void {
  controller.pauseJobPolling()
}

export function resumeCalibrationJobPolling(): void {
  controller.resumeJobPolling()
}

export function cancelCalibrationJob(jobId: string): Promise<CalibrationActionResult<CalibrationJobStatus | null>> {
  return controller.cancelJob(jobId)
}

export function applyCalibrationJob(jobId: string): Promise<CalibrationActionResult<CalibrationApplyResult>> {
  return controller.applyJob(jobId)
}

export function applyCalibrationWizard(): Promise<CalibrationActionResult<CalibrationApplyResult>> {
  return controller.applyWizard()
}

export function rollbackCalibrationJob(jobId: string): Promise<CalibrationActionResult<CalibrationRollbackResult>> {
  return controller.rollbackJob(jobId)
}

export function refreshCalibrationAll(params?: { limit?: number; offset?: number }): Promise<void> {
  return controller.refreshAll(params)
}

export function clearCalibrationActionError(): void {
  controller.clearActionError()
}

/** 暴露给组件的时间戳工具（列表页显示「本会话」上传时间等）。 */
export function calibrationNow(): number {
  return Date.now()
}

/** 上传超时（供 UI 提示）：与 api 模块保持一致。 */
export const CALIBRATION_UPLOAD_TIMEOUT = CALIBRATION_UPLOAD_TIMEOUT_MS
