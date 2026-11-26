/**
 * 路由矩阵页 store（M3b / 任务 T58）。
 *
 * 只读控制器：并行拉取 /api/channels（lane -> DAC 映射 / 输出声道 / plan 段）与
 * /api/plan/history（当前版本 / latency 回退来源）。/api/status 由共享轮询 store
 * （stores/status.ts + useStatusPolling）提供，本模块不重复拉取。
 *
 * 纪律（与 M4 声道 store 一致）：
 * - 并发去重：同一时刻只有一趟读取在途（刷新按钮/自动加载共用）；
 * - 失败保留最后一次成功数据并记录稳定错误码（不把旧数据冒充最新，也不清空）；
 * - /api/channels 整块不可用（501/503 not_wired）时 channels 为 null，页面显示 NOT_AVAILABLE + 任务号；
 * - /api/plan/history 只是补充来源：失败时清空 history（避免用旧版本冒充当前版本），
 *   但不清空 channels 数据，页面给出「历史不可用」提示；
 * - 401 交给共享 ApiClient（会话 store 清态跳登录）。
 *
 * 可测试性：createRoutingController(state, deps) 注入 api/时钟，模块底部组装 reactive 单例。
 *
 * 本模块另含**编辑草稿状态机**（createRoutingEditorController）：把 /api/channels.mapping 预填成
 * lane → DAC 草稿行、行增删/改值、dac_channels 显式开关与本地校验；它只产生草稿，不自己发请求——
 * prepare/commit/abort 一律复用 stores/control.ts（两阶段状态机只有一份，避免两套 token 状态）。
 */
import { reactive } from 'vue'
import {
  classifyChannelsError,
  type ChannelsErrorInfo
} from '../api/channels.ts'
import {
  buildRoutingDraftPatch,
  createRoutingApi,
  currentRoutingSource as currentRoutingSourceOf,
  nextRoutingLane,
  validateRoutingDraft,
  type RoutingApi,
  type RoutingChannelsInfo,
  type RoutingCurrentSource,
  type RoutingDraftRow,
  type RoutingPlanHistory
} from '../api/routing.ts'
import type { RoutingPatch } from '../api/control.ts'
import { apiClient } from './session.ts'

export type RoutingChannelsPhase = 'loading' | 'ready' | 'error'

export interface RoutingState {
  /** GET /api/channels 的最近成功快照；null = 尚未成功获取。 */
  channels: RoutingChannelsInfo | null
  /** loading = 首次加载；ready = 有数据（可能带刷新错误）；error = 无数据且请求失败。 */
  channelsPhase: RoutingChannelsPhase
  /** /api/channels 最近一次失败（成功后清空；保留 channels 时表示「显示最后成功数据」）。 */
  channelsError: ChannelsErrorInfo | null
  /** GET /api/plan/history 的最近成功快照；失败时清空（避免旧版本冒充当前版本）。 */
  history: RoutingPlanHistory | null
  historyError: ChannelsErrorInfo | null
  /** 是否有读取在途（刷新按钮 loading / 禁重入）。 */
  refreshing: boolean
  /** 最近一次成功读取的本地时钟（毫秒）。 */
  loadedAt: number | null
}

export function createInitialRoutingState(): RoutingState {
  return {
    channels: null,
    channelsPhase: 'loading',
    channelsError: null,
    history: null,
    historyError: null,
    refreshing: false,
    loadedAt: null
  }
}

export interface RoutingDeps {
  api: RoutingApi
  now?: () => number
}

export interface RoutingController {
  /** 读取 channels + history（并发去重）；重复调用返回同一在途 Promise。 */
  load(): Promise<void>
  refresh(): Promise<void>
  reset(): void
}

export function createRoutingController(state: RoutingState, deps: RoutingDeps): RoutingController {
  const now = deps.now ?? (() => Date.now())
  let inflight: Promise<void> | null = null

  /** 返回是否成功（loadedAt 只在至少一侧成功时推进）。 */
  async function loadChannels(): Promise<boolean> {
    if (state.channels === null) {
      state.channelsPhase = 'loading'
    }
    try {
      const channels = await deps.api.fetchChannels()
      state.channels = channels
      state.channelsPhase = 'ready'
      state.channelsError = null
      return true
    } catch (error) {
      state.channelsError = classifyChannelsError(error)
      state.channelsPhase = state.channels === null ? 'error' : 'ready'
      return false
    }
  }

  /** 返回是否成功（失败清空 history，避免用旧版本冒充当前版本；channels 数据保持不动）。 */
  async function loadHistory(): Promise<boolean> {
    try {
      state.history = await deps.api.fetchPlanHistory()
      state.historyError = null
      return true
    } catch (error) {
      state.history = null
      state.historyError = classifyChannelsError(error)
      return false
    }
  }

  async function runLoad(): Promise<void> {
    state.refreshing = true
    try {
      // 两个只读端点互不依赖：并行拉取；各自失败互不牵连。
      const [channelsOk, historyOk] = await Promise.all([loadChannels(), loadHistory()])
      if (channelsOk || historyOk) {
        state.loadedAt = now()
      }
    } catch (error) {
      // 两个子任务内部已捕获自己的错误；这里只兜住不可预期异常（不静默吞掉、不假成功）。
      state.channelsError = classifyChannelsError(error)
      state.channelsPhase = state.channels === null ? 'error' : state.channelsPhase
    } finally {
      state.refreshing = false
      inflight = null
    }
  }

  function load(): Promise<void> {
    if (inflight !== null) {
      return inflight
    }
    const started = runLoad()
    inflight = started
    return started
  }

  return {
    load,
    refresh: load,
    reset(): void {
      Object.assign(state, createInitialRoutingState())
    }
  }
}

/* ---------------------------------------------------------------- 单例装配 */

export const routingState = reactive<RoutingState>(createInitialRoutingState())

const routingApi = createRoutingApi(apiClient)

const controller = createRoutingController(routingState, { api: routingApi })

/** 页面挂载时调用（并发去重，重复调用安全）。 */
export function loadRouting(): Promise<void> {
  return controller.load()
}

/** 手动刷新（与自动加载共用同一在途请求）。 */
export function refreshRouting(): Promise<void> {
  return controller.refresh()
}

export function resetRoutingState(): void {
  controller.reset()
}

export { routingApi, controller as routingController }

/* ------------------------------------------------------------ 编辑草稿状态机 */

/**
 * 路由编辑草稿状态（页面里用 reactive 包装；本状态机不依赖 Vue）。
 *
 * - rows：lane → DAC 草稿行（提交时是全量替换表，不是增量）；
 * - dirty：用户是否手动改过草稿 —— 刷新不会覆盖用户的编辑（force prefill 才重建）；
 * - explicitDacChannels：是否显式指定 dac_channels；false = 沿用方案值（不发送该字段）；
 * - dacChannels：显式值；未显式时保存从当前值推导的显示值（不作为「当前值」出站）；
 * - validation：最近一次本地校验错误码（null = 通过；页面负责 i18n 映射）。
 */
export interface RoutingEditorState {
  rows: RoutingDraftRow[]
  dirty: boolean
  explicitDacChannels: boolean
  dacChannels: number | null
  /** 最近一次从 /api/channels.mapping 预填的本地时间（毫秒）；null = 当前值未知/从未预填。 */
  prefilledAt: number | null
  validation: string | null
}

export function createInitialRoutingEditorState(): RoutingEditorState {
  return {
    rows: [],
    dirty: false,
    explicitDacChannels: false,
    dacChannels: null,
    prefilledAt: null,
    validation: null
  }
}

export interface RoutingEditorDeps {
  /** 当前值来源快照（/api/channels.mapping；每次调用都会读最新值）。 */
  current: () => RoutingCurrentSource
  now?: () => number
}

export interface RoutingEditorController {
  /** 用当前值预填；dirty 且 force=false 时不覆盖（返回 false）。 */
  prefill(force?: boolean): boolean
  addRow(): void
  removeRow(index: number): void
  setLane(index: number, lane: number | null): void
  setDac(index: number, dac: number | null): void
  setExplicitDacChannels(explicit: boolean): void
  setDacChannels(value: number | null): void
  /** 清空草稿行（保留 dac_channels 开关状态）；dirty=true。 */
  clear(): void
  /** 本地校验并写 state.validation（错误码见 api/routing.validateRoutingDraft）；返回错误码。 */
  validate(): string | null
  /** 构造 prepare 用的 data（routes 全量；dac_channels 仅显式时带）。调用前应先 validate()。 */
  buildPatch(): RoutingPatch
  reset(): void
}

export function createRoutingEditorController(
  state: RoutingEditorState,
  deps: RoutingEditorDeps
): RoutingEditorController {
  const now = deps.now ?? (() => Date.now())

  /**
   * 行内 lane/DAC 的 null 输入（清空输入框）不覆盖已有值，避免误清（非法数值留给 validate 报错）；
   * dac_channels 的 null 是**有意义的**：勾选显式但没填 → 校验报 'dac_channels'。
   */
  function applyNumber(target: 'lane' | 'dac' | 'dacChannels', index: number, value: number | null): void {
    if (target === 'dacChannels') {
      state.dacChannels = value
      state.validation = null
      return
    }
    if (value === null) {
      return
    }
    const row = state.rows[index]
    if (row === undefined) {
      return
    }
    if (target === 'lane') {
      row.lane = value
    } else {
      row.dac = value
    }
    state.dirty = true
    state.validation = null
  }

  return {
    prefill(force = false): boolean {
      if (state.dirty && !force) {
        return false
      }
      const source = deps.current()
      state.rows =
        source.rows === null ? [] : source.rows.map((row) => ({ lane: row.lane, dac: row.dac }))
      state.dacChannels = source.dacChannels
      state.explicitDacChannels = false
      state.dirty = false
      state.validation = null
      state.prefilledAt = source.rows === null ? null : now()
      return true
    },
    addRow(): void {
      state.rows.push({ lane: nextRoutingLane(state.rows), dac: 0 })
      state.dirty = true
      state.validation = null
    },
    removeRow(index: number): void {
      if (!Number.isInteger(index) || index < 0 || index >= state.rows.length) {
        return
      }
      state.rows.splice(index, 1)
      state.dirty = true
      state.validation = null
    },
    setLane(index: number, lane: number | null): void {
      applyNumber('lane', index, lane)
    },
    setDac(index: number, dac: number | null): void {
      applyNumber('dac', index, dac)
    },
    setExplicitDacChannels(explicit: boolean): void {
      state.explicitDacChannels = explicit
      if (explicit && state.dacChannels === null) {
        // 勾选时用当前值的 DAC 声道数作为默认输入，避免空值；当前值未知则保持 null（校验会报错）。
        state.dacChannels = deps.current().dacChannels
      }
      state.validation = null
    },
    setDacChannels(value: number | null): void {
      applyNumber('dacChannels', 0, value)
    },
    clear(): void {
      state.rows = []
      state.dirty = true
      state.validation = null
    },
    validate(): string | null {
      const explicit = state.explicitDacChannels ? state.dacChannels : undefined
      const error = validateRoutingDraft(state.rows, {
        explicitDacChannels: explicit,
        knownDacChannels: deps.current().dacChannels
      })
      state.validation = error
      return error
    },
    buildPatch(): RoutingPatch {
      return buildRoutingDraftPatch(state.rows, state.explicitDacChannels ? state.dacChannels : null)
    },
    reset(): void {
      Object.assign(state, createInitialRoutingEditorState())
    }
  }
}

/* ---------------------------------------------------------------- 当前值选择器 */

/** 页面读取当前值来源（/api/channels.mapping）的快照；mapping=null 时 rows=null（不冒充当前值）。 */
export function routingCurrentSource(): RoutingCurrentSource {
  return currentRoutingSourceOf(routingState.channels)
}
