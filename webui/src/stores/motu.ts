/**
 * MOTU 设备 store（音量页）：GET /api/motu 5s 轮询 + PUT /api/motu/volume 两阶段草稿状态机。
 *
 * 纪律（docs/API.md v1.1 §3 / §4.1 / §9）：
 * - 轮询：页面可见时 5s 拉 GET /api/motu；页面隐藏暂停并中止在途请求，恢复可见立即刷新再继续定时；
 *   失败保留最后一次成功快照 + 归类错误（不伪造/清空数据），401 由共享 ApiClient 清态跳登录；
 * - 两阶段：prepare 只写草稿（writes_device=false），commit 才写设备并返回逐通道真值回读；
 * - **该端点没有 abort**：本地“取消草稿”只清本地状态并记录 TTL，不向服务端发请求（不假装已删）；
 * - commit 失败：token_invalid 或后端 details.draft_consumed=true（设备写失败时草稿已被消费）
 *   都清本地草稿；其它失败保留草稿（后端设备不可用时草稿还在，可重试）；
 * - 失败绝不写 lastCommit（页面不会显示成已写入）。
 *
 * 可测试性：createMotuPollController / createMotuVolumeController 是纯逻辑工厂（注入 api/时钟/定时器），
 * 模块底部用 reactive state + 共享 ApiClient 组装单例供 Vue 组件使用。
 */
import { reactive } from 'vue'
import {
  createMotuApi,
  classifyMotuError,
  validateMotuChannelSelection,
  validateMotuVolumeDb,
  type MotuApi,
  type MotuChannelSelection,
  type MotuErrorInfo,
  type MotuStatus,
  type MotuVolumeCommitAck,
  type MotuVolumeDraft
} from '../api/motu.ts'
import { apiClient } from './session.ts'

/** MOTU 快照轮询间隔（任务契约：5 秒）。 */
export const MOTU_POLL_INTERVAL_MS = 5000

/** MOTU 音量操作历史最多保留条数。 */
export const MOTU_VOLUME_HISTORY_LIMIT = 8

/* ------------------------------------------------------------------ 轮询状态 */

export type MotuPollPhase = 'loading' | 'ready' | 'error'

export interface MotuPollState {
  /** 最近一次成功的 GET /api/motu 快照；null = 尚未成功获取。 */
  status: MotuStatus | null
  /** loading = 首次加载；ready = 有快照（可能带刷新错误）；error = 无快照且请求失败。 */
  phase: MotuPollPhase
  /** 最近一次失败（成功后清空）；保留快照时页面标注「显示的是最后一次成功数据」。 */
  error: MotuErrorInfo | null
  /** 是否有请求在途（刷新按钮 loading / 禁重入）。 */
  refreshing: boolean
  /** 最近一次成功响应的本地时钟（毫秒）。 */
  loadedAt: number | null
}

export function createInitialMotuPollState(): MotuPollState {
  return { status: null, phase: 'loading', error: null, refreshing: false, loadedAt: null }
}

export type MotuTimerHandle = unknown

export interface MotuPollDeps {
  fetchMotu: (options?: { signal?: AbortSignal }) => Promise<MotuStatus>
  now?: () => number
  schedule?: (callback: () => void, delayMs: number) => MotuTimerHandle
  cancel?: (handle: MotuTimerHandle) => void
  intervalMs?: number
}

export interface MotuPollController {
  start(): void
  stop(): void
  pause(): void
  resume(): void
  refresh(): Promise<void>
  isPolling(): boolean
  isPaused(): boolean
}

/** 纯逻辑轮询控制器（MOTU 快照；与 stores/status.ts 同一套生命周期语义）。 */
export function createMotuPollController(state: MotuPollState, deps: MotuPollDeps): MotuPollController {
  const now = deps.now ?? (() => Date.now())
  const schedule =
    deps.schedule ?? ((callback: () => void, delayMs: number): MotuTimerHandle => setTimeout(callback, delayMs))
  const cancel =
    deps.cancel ?? ((handle: MotuTimerHandle): void => clearTimeout(handle as ReturnType<typeof setTimeout>))
  const intervalMs = deps.intervalMs ?? MOTU_POLL_INTERVAL_MS

  let running = false
  let paused = false
  let timer: MotuTimerHandle | null = null
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
    if (state.status === null) {
      state.phase = 'loading'
    }
    const abort = new AbortController()
    activeAbort = abort
    try {
      const status = await deps.fetchMotu({ signal: abort.signal })
      if (abort.signal.aborted) {
        return
      }
      state.status = status
      state.loadedAt = now()
      state.phase = 'ready'
      state.error = null
    } catch (error) {
      // 页面隐藏/离开导致的中止不算错误（不显示「请求失败」，也不清掉已有快照）。
      if (abort.signal.aborted) {
        return
      }
      state.error = classifyMotuError(error)
      state.phase = state.status === null ? 'error' : 'ready'
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

  return {
    start(): void {
      if (running) {
        return
      }
      running = true
      paused = false
      void load()
    },
    stop(): void {
      running = false
      paused = false
      clearTimer()
      abortActive()
    },
    pause(): void {
      if (!running || paused) {
        return
      }
      paused = true
      clearTimer()
      abortActive()
    },
    resume(): void {
      if (!running || !paused) {
        return
      }
      paused = false
      void load()
    },
    refresh: load,
    isPolling: () => running,
    isPaused: () => paused
  }
}

/* -------------------------------------------------------------- 音量两阶段状态 */

export type MotuVolumeAction = 'prepare' | 'commit' | 'cancel'

export interface MotuVolumeHistoryEntry {
  at: number
  action: MotuVolumeAction
  ok: boolean
  token: string | null
  channels: number[]
  db: number | null
  code: string | null
  detail: string | null
}

/** 本地取消草稿的回执（不请求后端；服务端草稿由 TTL 过期）。 */
export interface MotuLocalCancelNotice {
  token: string | null
  /** 后端 prepare 回执上报的 TTL（秒）；null = 未上报。 */
  ttlSeconds: number | null
  cancelledAt: number
  channels: number[]
}

export interface MotuVolumeState {
  /** 已 prepare 未 commit 的草稿；null = 没有草稿。 */
  draft: MotuVolumeDraft | null
  /** 最近一次 commit 成功回执（设备已写入 + 逐通道回读）。 */
  lastCommit: MotuVolumeCommitAck | null
  lastCommitAt: number | null
  /** 最近一次失败（成功后清空）。 */
  error: MotuErrorInfo | null
  /** 进行中的动作（互斥）。 */
  busy: 'prepare' | 'commit' | null
  /** 本地取消回执：显示“服务端草稿按 TTL 过期”，不假装已删。 */
  localCancel: MotuLocalCancelNotice | null
  /** 最近操作历史（新的在前）。 */
  history: MotuVolumeHistoryEntry[]
}

export function createInitialMotuVolumeState(): MotuVolumeState {
  return {
    draft: null,
    lastCommit: null,
    lastCommitAt: null,
    error: null,
    busy: null,
    localCancel: null,
    history: []
  }
}

export interface MotuVolumeDeps {
  api: Pick<MotuApi, 'prepareVolume' | 'commitVolume'>
  now?: () => number
}

export interface MotuVolumeController {
  prepare(selection: MotuChannelSelection, db: number): Promise<MotuVolumeDraft | null>
  commit(): Promise<MotuVolumeCommitAck | null>
  /** 本地取消：只清本地草稿并记录 TTL，不发任何请求（端点没有 abort）。 */
  cancelDraft(): MotuLocalCancelNotice | null
  dismissError(): void
  reset(): void
}

/** 纯逻辑两阶段控制器（prepare -> draft -> commit / 本地取消）。 */
export function createMotuVolumeController(state: MotuVolumeState, deps: MotuVolumeDeps): MotuVolumeController {
  const now = deps.now ?? (() => Date.now())

  function pushHistory(entry: Omit<MotuVolumeHistoryEntry, 'at'>): void {
    state.history.unshift({ at: now(), ...entry })
    if (state.history.length > MOTU_VOLUME_HISTORY_LIMIT) {
      state.history.length = MOTU_VOLUME_HISTORY_LIMIT
    }
  }

  function begin(action: 'prepare' | 'commit'): boolean {
    if (state.busy !== null) {
      return false
    }
    state.busy = action
    state.error = null
    return true
  }

  function end(): void {
    state.busy = null
  }

  function draftChannels(draft: MotuVolumeDraft): number[] {
    return draft.target === null ? [] : draft.target.channels.map((channel) => channel.channel)
  }

  return {
    async prepare(selection: MotuChannelSelection, db: number): Promise<MotuVolumeDraft | null> {
      // 前端校验（UI 已禁用非法输入，这里仍防御；不请求后端）。
      if (validateMotuChannelSelection(selection) !== null || validateMotuVolumeDb(db) !== null) {
        return null
      }
      if (!begin('prepare')) {
        return null
      }
      try {
        const draft = await deps.api.prepareVolume(selection, db)
        state.draft = draft
        state.localCancel = null
        pushHistory({
          action: 'prepare',
          ok: true,
          token: draft.engine_token,
          channels: draftChannels(draft),
          db: draft.target === null ? null : draft.target.db,
          code: null,
          detail: null
        })
        return draft
      } catch (error) {
        const info = classifyMotuError(error)
        state.draft = null
        state.error = info
        pushHistory({
          action: 'prepare',
          ok: false,
          token: null,
          channels: [],
          db,
          code: info.code,
          detail: info.detail ?? null
        })
        return null
      } finally {
        end()
      }
    },

    async commit(): Promise<MotuVolumeCommitAck | null> {
      const draft = state.draft
      if (draft === null || !begin('commit')) {
        return null
      }
      const channels = draftChannels(draft)
      const db = draft.target === null ? null : draft.target.db
      try {
        const ack = await deps.api.commitVolume(draft.engine_token)
        state.lastCommit = ack
        state.lastCommitAt = now()
        state.draft = null
        state.localCancel = null
        pushHistory({
          action: 'commit',
          ok: true,
          token: draft.engine_token,
          channels,
          db: ack.db ?? db,
          code: null,
          detail: null
        })
        return ack
      } catch (error) {
        const info = classifyMotuError(error)
        // token 失效 / 后端已消费草稿（设备写失败）时本地草稿不能再用；其余失败保留以便重试。
        if (info.category === 'token_invalid' || info.draftConsumed === true) {
          state.draft = null
        }
        state.error = info
        pushHistory({
          action: 'commit',
          ok: false,
          token: draft.engine_token,
          channels,
          db,
          code: info.code,
          detail: info.detail ?? null
        })
        return null
      } finally {
        end()
      }
    },

    cancelDraft(): MotuLocalCancelNotice | null {
      const draft = state.draft
      if (draft === null) {
        return null
      }
      const notice: MotuLocalCancelNotice = {
        token: draft.engine_token,
        ttlSeconds: draft.expires_in_s,
        cancelledAt: now(),
        channels: draftChannels(draft)
      }
      state.draft = null
      state.localCancel = notice
      pushHistory({
        action: 'cancel',
        ok: true,
        token: draft.engine_token,
        channels: notice.channels,
        db: draft.target === null ? null : draft.target.db,
        code: null,
        detail: null
      })
      return notice
    },

    dismissError(): void {
      state.error = null
    },

    reset(): void {
      Object.assign(state, createInitialMotuVolumeState())
    }
  }
}

/* ---------------------------------------------------------------- 单例装配 */

/** MOTU 快照（音量页专用；页面卸载停止轮询，离开页面不保留定时器）。 */
export const motuPollState = reactive<MotuPollState>(createInitialMotuPollState())

const motuApi = createMotuApi(apiClient)

const pollController = createMotuPollController(motuPollState, {
  fetchMotu: (options) => motuApi.fetchMotu(options)
})

export function startMotuPolling(): void {
  pollController.start()
}

export function stopMotuPolling(): void {
  pollController.stop()
}

export function pauseMotuPolling(): void {
  pollController.pause()
}

export function resumeMotuPolling(): void {
  pollController.resume()
}

/** 手动刷新（与轮询并发去重；失败写入 state.error）。 */
export function refreshMotu(): Promise<void> {
  return pollController.refresh()
}

/** MOTU 音量两阶段草稿（prepare/commit/本地取消）。 */
export const motuVolumeState = reactive<MotuVolumeState>(createInitialMotuVolumeState())

const volumeController = createMotuVolumeController(motuVolumeState, { api: motuApi })

export function prepareMotuVolume(
  selection: MotuChannelSelection,
  db: number
): Promise<MotuVolumeDraft | null> {
  return volumeController.prepare(selection, db)
}

export function commitMotuVolume(): Promise<MotuVolumeCommitAck | null> {
  return volumeController.commit()
}

/** 本地取消草稿（不请求后端；服务端草稿按 TTL 过期）。 */
export function cancelMotuVolumeDraft(): MotuLocalCancelNotice | null {
  return volumeController.cancelDraft()
}

export function dismissMotuVolumeError(): void {
  volumeController.dismissError()
}

export function resetMotuVolumeState(): void {
  volumeController.reset()
}

export { motuApi, pollController as motuPollController, volumeController as motuVolumeController }
