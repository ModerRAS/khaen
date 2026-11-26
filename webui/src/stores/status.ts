/**
 * 概览状态 store（PLAN §6.2 / §11.4；任务 T35b）。
 *
 * 职责：
 * - 持有最近一次成功的 `/api/status` 快照（data）+ 请求相位（loading/ready/error）+ 错误归类 + loadedAt；
 * - 5 秒轮询：页面可见时按 `STATUS_POLL_INTERVAL_MS` 拉取，页面隐藏（visibilitychange）暂停并中止在途请求，
 *   恢复可见时立即刷新一次再继续定时（由 composables/useStatusPolling 接线 DOM）；
 * - 手动刷新（refresh）与轮询共用同一在途请求（并发去重），失败时保留最后一次成功数据（标记 error，
 *   不伪造/清空数据），无数据时进入 error 相位供页面显示错误 + 重试；
 * - 401 由共享 ApiClient 的 onUnauthorized 统一处理（会话 store 清态跳登录），本模块只归类展示。
 *
 * 可测试性：createStatusController(state, deps) 是纯逻辑工厂（注入时钟/定时器/fetch），
 * 模块底部用 reactive state + 共享 ApiClient 组装单例供 Vue 组件使用。
 */
import { reactive } from 'vue'
import {
  classifyStatusError,
  createStatusApi,
  type StatusErrorInfo,
  type StatusResponse
} from '../api/status.ts'
import { apiClient } from './session.ts'

/** 概览轮询间隔（任务 T35b：5 秒；PLAN §6 通用规范为 2s，本里程碑按任务契约取 5s）。 */
export const STATUS_POLL_INTERVAL_MS = 5000

export type StatusPhase = 'loading' | 'ready' | 'error'

export interface StatusState {
  /** 最近一次成功的聚合状态；null = 尚未成功获取。 */
  data: StatusResponse | null
  /** loading = 首次加载；ready = 有数据（可能带刷新错误）；error = 无数据且请求失败。 */
  phase: StatusPhase
  /** 最近一次失败（成功后清空）；保留 data 时用于页面提示「显示的是最后成功数据」。 */
  error: StatusErrorInfo | null
  /** 是否有请求在途（刷新按钮 loading / 禁重入）。 */
  refreshing: boolean
  /** 最近一次成功响应的本地时钟（毫秒）；页面显示「最后更新」。 */
  loadedAt: number | null
}

export function createInitialStatusState(): StatusState {
  return {
    data: null,
    phase: 'loading',
    error: null,
    refreshing: false,
    loadedAt: null
  }
}

/** 定时器句柄（浏览器 number / Node Timeout，测试注入任意值）。 */
export type StatusTimerHandle = unknown

export interface StatusControllerDeps {
  /** 拉取一次 /api/status（signal 用于页面隐藏/离开时中止）。 */
  fetchStatus: (options?: { signal?: AbortSignal }) => Promise<StatusResponse>
  /** 可注入时钟（测 loadedAt）。 */
  now?: () => number
  /** 可注入定时器（单测用假实现）。 */
  schedule?: (callback: () => void, delayMs: number) => StatusTimerHandle
  /** 对应的取消函数。 */
  cancel?: (handle: StatusTimerHandle) => void
  /** 轮询间隔，缺省 STATUS_POLL_INTERVAL_MS。 */
  intervalMs?: number
}

export interface StatusController {
  /** 开始轮询并立即拉一次（重复调用幂等）。 */
  start(): void
  /** 停止轮询并中止在途请求（离开页面）。 */
  stop(): void
  /** 暂停轮询并中止在途请求（页面隐藏）。 */
  pause(): void
  /** 恢复轮询：立即刷新一次后继续定时（页面重新可见）。 */
  resume(): void
  /** 手动刷新（与轮询并发去重）。 */
  refresh(): Promise<void>
  isPolling(): boolean
  isPaused(): boolean
}

/** 纯逻辑轮询控制器：状态写入 state（Vue reactive 或普通对象均可）。 */
export function createStatusController(state: StatusState, deps: StatusControllerDeps): StatusController {
  const now = deps.now ?? (() => Date.now())
  const schedule =
    deps.schedule ?? ((callback: () => void, delayMs: number): StatusTimerHandle => setTimeout(callback, delayMs))
  const cancel =
    deps.cancel ?? ((handle: StatusTimerHandle): void => clearTimeout(handle as ReturnType<typeof setTimeout>))
  const intervalMs = deps.intervalMs ?? STATUS_POLL_INTERVAL_MS

  let running = false
  let paused = false
  let timer: StatusTimerHandle | null = null
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
    if (!running || paused) {
      return
    }
    timer = schedule(() => {
      timer = null
      void load()
    }, intervalMs)
  }

  function abortActive(): void {
    if (activeAbort !== null) {
      activeAbort.abort()
      activeAbort = null
    }
  }

  async function run(): Promise<void> {
    state.refreshing = true
    if (state.data === null) {
      state.phase = 'loading'
    }
    const abort = new AbortController()
    activeAbort = abort
    try {
      const response = await deps.fetchStatus({ signal: abort.signal })
      if (abort.signal.aborted) {
        return
      }
      state.data = response
      state.loadedAt = now()
      state.phase = 'ready'
      state.error = null
    } catch (error) {
      // 页面隐藏/离开导致的中止不算错误（不显示「请求失败」，也不清掉已有数据）。
      if (abort.signal.aborted) {
        return
      }
      state.error = classifyStatusError(error)
      state.phase = state.data === null ? 'error' : 'ready'
    } finally {
      if (activeAbort === abort) {
        activeAbort = null
      }
      state.refreshing = false
      inflight = null
      scheduleNext()
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

  function start(): void {
    if (running) {
      return
    }
    running = true
    paused = false
    void load()
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
    void load()
  }

  return {
    start,
    stop,
    pause,
    resume,
    refresh: load,
    isPolling: () => running,
    isPaused: () => paused
  }
}

/* ---------------------------------------------------------------- 单例装配 */

/** 概览状态（AppShell 顶栏徽标与概览页共用）。 */
export const statusState = reactive<StatusState>(createInitialStatusState())

const statusApi = createStatusApi(apiClient)

const controller = createStatusController(statusState, {
  fetchStatus: (options) => statusApi.fetchStatus(options)
})

export function startStatusPolling(): void {
  controller.start()
}

export function stopStatusPolling(): void {
  controller.stop()
}

export function pauseStatusPolling(): void {
  controller.pause()
}

export function resumeStatusPolling(): void {
  controller.resume()
}

/** 手动刷新：返回请求完成的 Promise（页面/顶栏刷新按钮共用，失败已写入 state.error）。 */
export function refreshStatus(): Promise<void> {
  return controller.refresh()
}
