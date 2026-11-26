/**
 * 系统信息页 store（任务 T47）。
 *
 * 两个可独立测试的控制器：
 * - info：GET /api/system/info —— 一次性加载 + 手动刷新（并发去重、失败保留最后成功数据、
 *   首次失败进入 error 相位供页面重试）；401 由共享 ApiClient 统一跳登录。
 * - logs：GET /api/system/logs —— unit/行数切换 + 可选自动刷新（默认关闭，5s 间隔），
 *   页面隐藏暂停（stop timer + abort 在途请求），切换 unit/行数时清空旧数据避免串台。
 *
 * 重型动作（改密 / 重启 / 诊断下载）在页面用共享 ApiClient 直接调用 api/system.ts 的端点封装，
 * 不放进轮询控制器（避免把一次性交互写成状态机）。
 */
import { reactive } from 'vue'
import {
  DEFAULT_LOG_LINES,
  classifySystemError,
  createSystemApi,
  normalizeLogLines,
  type LogUnit,
  type SystemErrorInfo,
  type SystemInfo,
  type SystemLogs
} from '../api/system.ts'
import { apiClient } from './session.ts'

/* ------------------------------------------------------------------ info */

export type SystemInfoPhase = 'loading' | 'ready' | 'error'

export interface SystemInfoState {
  /** 最近一次成功的 /api/system/info；null = 尚未成功获取。 */
  data: SystemInfo | null
  /** loading = 首次加载；ready = 有数据（可能带刷新错误）；error = 无数据且请求失败。 */
  phase: SystemInfoPhase
  /** 最近一次失败（成功后清空）；保留 data 时用于提示「显示的是最后成功数据」。 */
  error: SystemErrorInfo | null
  /** 是否有请求在途（刷新按钮 loading / 禁重入）。 */
  refreshing: boolean
  loadedAt: number | null
}

export function createInitialSystemInfoState(): SystemInfoState {
  return { data: null, phase: 'loading', error: null, refreshing: false, loadedAt: null }
}

export interface SystemInfoDeps {
  fetchInfo: (options?: { signal?: AbortSignal }) => Promise<SystemInfo>
  now?: () => number
}

export interface SystemInfoController {
  load(): Promise<void>
  refresh(): Promise<void>
  reset(): void
}

export function createSystemInfoController(state: SystemInfoState, deps: SystemInfoDeps): SystemInfoController {
  const now = deps.now ?? (() => Date.now())
  let inflight: Promise<void> | null = null

  async function run(): Promise<void> {
    state.refreshing = true
    if (state.data === null) {
      state.phase = 'loading'
    }
    try {
      const info = await deps.fetchInfo()
      state.data = info
      state.phase = 'ready'
      state.error = null
      state.loadedAt = now()
    } catch (error) {
      state.error = classifySystemError(error)
      state.phase = state.data === null ? 'error' : 'ready'
    } finally {
      state.refreshing = false
      inflight = null
    }
  }

  function load(): Promise<void> {
    if (inflight !== null) {
      return inflight
    }
    const started = run()
    inflight = started
    return started
  }

  function reset(): void {
    Object.assign(state, createInitialSystemInfoState())
    inflight = null
  }

  return { load, refresh: load, reset }
}

/* ------------------------------------------------------------------ logs */

export type SystemLogsPhase = 'loading' | 'ready' | 'error'

export interface SystemLogsState {
  unit: LogUnit
  lines: number
  /** 是否自动刷新（页面开关；默认关闭，避免阅读日志时反复跳动）。 */
  autoRefresh: boolean
  data: SystemLogs | null
  phase: SystemLogsPhase
  error: SystemErrorInfo | null
  loading: boolean
  loadedAt: number | null
}

export function createInitialSystemLogsState(): SystemLogsState {
  return {
    unit: 'daemon',
    lines: DEFAULT_LOG_LINES,
    autoRefresh: false,
    data: null,
    phase: 'loading',
    error: null,
    loading: false,
    loadedAt: null
  }
}

export type SystemLogsTimerHandle = unknown

export interface SystemLogsDeps {
  fetchLogs: (
    params: { unit: LogUnit; lines: number },
    options?: { signal?: AbortSignal }
  ) => Promise<SystemLogs>
  now?: () => number
  schedule?: (callback: () => void, delayMs: number) => SystemLogsTimerHandle
  cancel?: (handle: SystemLogsTimerHandle) => void
  intervalMs?: number
}

export interface SystemLogsController {
  /** 开始加载（幂等）。 */
  start(): void
  /** 停止定时器并中止在途请求（离开页面）。 */
  stop(): void
  /** 暂停（页面隐藏）：停止定时器 + 中止在途请求，数据保留。 */
  pause(): void
  /** 恢复：立即刷新并恢复自动刷新（如开启）。 */
  resume(): void
  /** 手动/定时刷新（并发去重）；silent=true 时不显示 loading（自动刷新用）。 */
  refresh(options?: { silent?: boolean }): Promise<void>
  /** 切换日志单元（清空旧数据后刷新）。 */
  setUnit(unit: LogUnit): void
  /** 切换行数（清空旧数据后刷新）。 */
  setLines(lines: number): void
  /** 开/关自动刷新（开启时立即刷新）。 */
  setAutoRefresh(enabled: boolean): void
  isStarted(): boolean
  isPaused(): boolean
}

/** 自动刷新间隔（毫秒）：日志页每 5s 拉一次最近 N 行。 */
export const SYSTEM_LOGS_INTERVAL_MS = 5000

export function createSystemLogsController(state: SystemLogsState, deps: SystemLogsDeps): SystemLogsController {
  const now = deps.now ?? (() => Date.now())
  const schedule =
    deps.schedule ??
    ((callback: () => void, delayMs: number): SystemLogsTimerHandle => setTimeout(callback, delayMs))
  const cancel =
    deps.cancel ?? ((handle: SystemLogsTimerHandle): void => clearTimeout(handle as ReturnType<typeof setTimeout>))
  const intervalMs = deps.intervalMs ?? SYSTEM_LOGS_INTERVAL_MS

  let running = false
  let paused = false
  let timer: SystemLogsTimerHandle | null = null
  let inflight: Promise<void> | null = null
  let activeAbort: AbortController | null = null

  function clearTimer(): void {
    if (timer !== null) {
      cancel(timer)
      timer = null
    }
  }

  function scheduleNext(): void {
    clearTimer()
    if (!running || paused || !state.autoRefresh) {
      return
    }
    timer = schedule(() => {
      timer = null
      void refresh({ silent: true })
    }, intervalMs)
  }

  function abortActive(): void {
    if (activeAbort !== null) {
      activeAbort.abort()
      activeAbort = null
    }
  }

  async function run(silent: boolean): Promise<void> {
    state.loading = silent ? state.loading : true
    if (state.data === null) {
      state.phase = 'loading'
    }
    const abort = new AbortController()
    activeAbort = abort
    try {
      const logs = await deps.fetchLogs({ unit: state.unit, lines: state.lines }, { signal: abort.signal })
      if (abort.signal.aborted) {
        return
      }
      state.data = logs
      state.loadedAt = now()
      state.phase = 'ready'
      state.error = null
    } catch (error) {
      if (abort.signal.aborted) {
        return
      }
      state.error = classifySystemError(error)
      state.phase = state.data === null ? 'error' : 'ready'
    } finally {
      if (activeAbort === abort) {
        activeAbort = null
      }
      state.loading = false
      inflight = null
      scheduleNext()
    }
  }

  function refresh(options: { silent?: boolean } = {}): Promise<void> {
    if (inflight !== null) {
      return inflight
    }
    const started = run(options.silent === true)
    inflight = started
    return started
  }

  function start(): void {
    if (running) {
      return
    }
    running = true
    paused = false
    void refresh()
  }

  function stop(): void {
    running = false
    paused = false
    clearTimer()
    abortActive()
  }

  function pause(): void {
    if (!running || paused) {
      return
    }
    paused = true
    clearTimer()
    abortActive()
  }

  function resume(): void {
    if (!running || !paused) {
      return
    }
    paused = false
    void refresh()
  }

  function setUnit(unit: LogUnit): void {
    if (state.unit === unit) {
      return
    }
    state.unit = unit
    state.data = null
    state.phase = 'loading'
    state.error = null
    if (running) {
      void refresh()
    }
  }

  function setLines(lines: number): void {
    const next = normalizeLogLines(lines)
    if (state.lines === next) {
      return
    }
    state.lines = next
    state.data = null
    state.phase = 'loading'
    state.error = null
    if (running) {
      void refresh()
    }
  }

  function setAutoRefresh(enabled: boolean): void {
    if (state.autoRefresh === enabled) {
      return
    }
    state.autoRefresh = enabled
    if (enabled) {
      if (running && !paused) {
        // refresh() 的 finally 会安排下一次定时器，避免这里与 finally 各排一次。
        void refresh()
      }
      return
    }
    clearTimer()
  }

  return {
    start,
    stop,
    pause,
    resume,
    refresh,
    setUnit,
    setLines,
    setAutoRefresh,
    isStarted: () => running,
    isPaused: () => paused
  }
}

/* ---------------------------------------------------------------- 单例装配 */

/** 系统信息页共享状态。 */
export const systemInfoState = reactive<SystemInfoState>(createInitialSystemInfoState())

const systemApi = createSystemApi(apiClient)

const infoController = createSystemInfoController(systemInfoState, {
  fetchInfo: (options) => systemApi.fetchInfo(options)
})

export function loadSystemInfo(): Promise<void> {
  return infoController.load()
}

export function refreshSystemInfo(): Promise<void> {
  return infoController.refresh()
}

/** 日志查看器单例（系统页内嵌与 /system/logs 共用；离开页面 stop）。 */
export const systemLogsState = reactive<SystemLogsState>(createInitialSystemLogsState())

const logsController = createSystemLogsController(systemLogsState, {
  fetchLogs: (params, options) => systemApi.fetchLogs(params, options)
})

export function startSystemLogs(): void {
  logsController.start()
}

export function stopSystemLogs(): void {
  logsController.stop()
}

export function pauseSystemLogs(): void {
  logsController.pause()
}

export function resumeSystemLogs(): void {
  logsController.resume()
}

export function refreshSystemLogs(): Promise<void> {
  return logsController.refresh()
}

export function setSystemLogsUnit(unit: LogUnit): void {
  logsController.setUnit(unit)
}

export function setSystemLogsLines(lines: number): void {
  logsController.setLines(lines)
}

export function setSystemLogsAutoRefresh(enabled: boolean): void {
  logsController.setAutoRefresh(enabled)
}

/** 共享系统 API（改密 / 重启 / 诊断下载由页面直接调用）。 */
export { systemApi }
