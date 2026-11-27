/**
 * 麦克风 profile store（真实校准桥接 C1/C4）。
 *
 * 数据纪律：
 * - profile / upload 结果 / rt-host UMIK 状态全部来自 daemon API，前端不自己算 sha256、不伪造解析/状态；
 * - 上传只把服务端回执放进 state；保存 profile 需要显式 cal_sha256（来自上传回执），不能凭文件名猜；
 * - rt-host 未接线（available=false）不当作错误，原样把 reason 交给页面显示 NOT_AVAILABLE。
 *
 * 可测试性：createMicController(state, deps) 注入 MicApi；模块底部用共享 ApiClient 组装单例。
 */
import { reactive } from 'vue'
import { apiClient } from './session.ts'
import { classifyCalibrationError, type CalibrationErrorInfo } from '../api/calibration.ts'
import {
  createMicApi,
  type MicApi,
  type MicCalUploadResponse,
  type MicProfile,
  type MicProfileUpdate,
  type MicStatusResponse
} from '../api/mic.ts'

export type MicPhase = 'idle' | 'loading' | 'ready' | 'error'

export interface MicState {
  phase: MicPhase
  /** 当前持久化 profile；null = 未保存（reasons 必非空）。 */
  profile: MicProfile | null
  reasons: string[]
  /** GET /api/calibration/mic/status 的完整回执（含 rt_host 可用性）。 */
  status: MicStatusResponse | null
  statusError: CalibrationErrorInfo | null
  /** 最近一次上传回执（服务端已内容寻址保存）。 */
  upload: MicCalUploadResponse | null
  busy: { load: boolean; save: boolean; upload: boolean; status: boolean }
  error: CalibrationErrorInfo | null
}

export type MicActionResult<T> = { ok: true; value: T } | { ok: false; error: CalibrationErrorInfo }

export interface MicController {
  load(): Promise<MicActionResult<MicProfile | null>>
  save(update: MicProfileUpdate): Promise<MicActionResult<MicProfile>>
  upload(input: { bytes: string | ArrayBuffer | Uint8Array; filename?: string }): Promise<MicActionResult<MicCalUploadResponse>>
  loadStatus(): Promise<MicActionResult<MicStatusResponse | null>>
  clearError(): void
}

export interface MicDeps {
  api: MicApi
}

export function createInitialMicState(): MicState {
  return {
    phase: 'idle',
    profile: null,
    reasons: [],
    status: null,
    statusError: null,
    upload: null,
    busy: { load: false, save: false, upload: false, status: false },
    error: null
  }
}

function failure(state: MicState, error: unknown): { ok: false; error: CalibrationErrorInfo } {
  const info = classifyCalibrationError(error)
  state.error = info
  return { ok: false, error: info }
}

export function createMicController(state: MicState, deps: MicDeps): MicController {
  async function load(): Promise<MicActionResult<MicProfile | null>> {
    state.phase = 'loading'
    state.busy.load = true
    state.error = null
    try {
      const response = await deps.api.getProfile()
      state.profile = response.profile
      state.reasons = response.reasons
      state.phase = 'ready'
      return { ok: true, value: response.profile }
    } catch (error) {
      state.phase = 'error'
      return failure(state, error)
    } finally {
      state.busy.load = false
    }
  }

  async function save(update: MicProfileUpdate): Promise<MicActionResult<MicProfile>> {
    state.busy.save = true
    state.error = null
    try {
      const response = await deps.api.putProfile(update)
      if (response.profile === null) {
        // PUT 成功却回 null 是合同违约：不能把空 profile 当成保存成功。
        return failure(state, new Error('PUT /api/calibration/mic returned a null profile'))
      }
      state.profile = response.profile
      state.reasons = response.reasons
      state.phase = 'ready'
      return { ok: true, value: response.profile }
    } catch (error) {
      return failure(state, error)
    } finally {
      state.busy.save = false
    }
  }

  async function upload(input: {
    bytes: string | ArrayBuffer | Uint8Array
    filename?: string
  }): Promise<MicActionResult<MicCalUploadResponse>> {
    state.busy.upload = true
    state.error = null
    try {
      const result = await deps.api.uploadCalibration(input.bytes, { filename: input.filename })
      state.upload = result
      return { ok: true, value: result }
    } catch (error) {
      return failure(state, error)
    } finally {
      state.busy.upload = false
    }
  }

  async function loadStatus(): Promise<MicActionResult<MicStatusResponse | null>> {
    state.busy.status = true
    state.statusError = null
    try {
      const response = await deps.api.getStatus()
      state.status = response
      state.profile = response.profile
      state.reasons = response.reasons
      state.phase = 'ready'
      return { ok: true, value: response }
    } catch (error) {
      const info = classifyCalibrationError(error)
      state.statusError = info
      return { ok: false, error: info }
    } finally {
      state.busy.status = false
    }
  }

  function clearError(): void {
    state.error = null
    state.statusError = null
  }

  return { load, save, upload, loadStatus, clearError }
}

/* ------------------------------------------------------------------ 单例装配 */

export const micState = reactive<MicState>(createInitialMicState())

const micApi = createMicApi(apiClient)

const controller = createMicController(micState, { api: micApi })

export function loadMicProfile(): Promise<MicActionResult<MicProfile | null>> {
  return controller.load()
}

export function saveMicProfile(update: MicProfileUpdate): Promise<MicActionResult<MicProfile>> {
  return controller.save(update)
}

export function uploadMicCalibration(input: {
  bytes: string | ArrayBuffer | Uint8Array
  filename?: string
}): Promise<MicActionResult<MicCalUploadResponse>> {
  return controller.upload(input)
}

export function loadMicStatus(): Promise<MicActionResult<MicStatusResponse | null>> {
  return controller.loadStatus()
}

export function clearMicError(): void {
  controller.clearError()
}
