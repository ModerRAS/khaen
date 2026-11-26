/**
 * 控制面写操作 store：五个语义界面（volume/routing/layout/mode/format）共用的两阶段草稿状态机
 * + 静音 + 回执/操作历史。
 *
 * 职责：
 * - 持有当前 prepare 草稿（最多一份，与后端"单 pending 槽"一致）、最近一次 commit 回执、
 *   最近一次静音/设备重试回执、操作历史（最近 8 条，如实记录成功/失败与稳定码）；
 * - 所有动作串行（busy 互斥），避免两个控制请求并发把后端的单草稿槽打乱；
 * - prepare 失败时清空本地草稿（不把旧草稿当成新意图提交）；commit 失败保留草稿（token 仍可能有效），
 *   但 token_invalid 时清空（后端草稿已不存在）；
 * - 回执里的 mode/format 只作为"控制面已生效"的回执展示，**不是设备回读**：设备速率仍以
 *   /api/status 的 format.device_rate 为准（重启 rt-host 后才会变化），页面必须分开显示；
 * - 401 交给共享 ApiClient（会话 store 清态跳登录），本模块只归类展示。
 *
 * 可测试性：createControlController(state, deps) 注入 api/时钟，Node 单测覆盖状态机；
 * 模块底部用 reactive state + 共享 ApiClient 组装单例供 Vue 组件使用。
 */
import { reactive } from 'vue'
import {
  MAX_OPERATION_HISTORY,
  classifyControlError,
  createControlApi,
  type ControlApi,
  type ControlCommitAck,
  type ControlDraft,
  type ControlErrorInfo,
  type ControlFormat,
  type ControlMuteAck,
  type DeviceRetryAck,
  type FormatPatch,
  type GainEntry,
  type LayoutPatch,
  type RoutingPatch,
  type RunMode
} from '../api/control.ts'
import { apiClient } from './session.ts'

/** 控制面操作进行中的动作种类（互斥）。 */
export type ControlBusy = 'mute' | 'prepare' | 'commit' | 'abort' | 'retry' | null

export type ControlAction = 'mute' | 'prepare' | 'commit' | 'abort' | 'retry'

/** 操作历史条目（页面「最近操作」显示；detail 为后端 message 原样）。 */
export interface ControlOperationEntry {
  at: number
  action: ControlAction
  ok: boolean
  surface: string | null
  version: number | null
  token: string | null
  code: string | null
  detail: string | null
}

export interface ControlState {
  /** 已 prepare 未 commit 的草稿；null = 没有草稿。 */
  draft: ControlDraft | null
  /** 最近一次 commit 成功回执（控制面已应用；设备是否生效仍看 /api/status）。 */
  lastCommit: ControlCommitAck | null
  lastCommitAt: number | null
  /** 最近一次静音成功回执（mute 值是后端回读）。 */
  lastMute: ControlMuteAck | null
  /** 最近一次设备重试回执。 */
  lastRetry: DeviceRetryAck | null
  /** 最近一次失败（成功后清空）。 */
  error: ControlErrorInfo | null
  /** 当前进行中的操作；非 null 时禁止发起其它控制操作。 */
  busy: ControlBusy
  /** 最近操作历史（新的在前，最多 MAX_OPERATION_HISTORY 条）。 */
  history: ControlOperationEntry[]
  /** 控制面回执里的模式（不是 /api/status 回读；页面必须分开标注）。 */
  appliedMode: string | null
  /** 控制面回执里的格式（同上，不是设备回读）。 */
  appliedFormat: ControlFormat | null
}

export function createInitialControlState(): ControlState {
  return {
    draft: null,
    lastCommit: null,
    lastCommitAt: null,
    lastMute: null,
    lastRetry: null,
    error: null,
    busy: null,
    history: [],
    appliedMode: null,
    appliedFormat: null
  }
}

export interface ControlDeps {
  api: ControlApi
  now?: () => number
}

export interface ControlController {
  prepareVolume(gains: readonly GainEntry[]): Promise<ControlDraft | null>
  prepareMode(mode: RunMode): Promise<ControlDraft | null>
  prepareRouting(patch: RoutingPatch): Promise<ControlDraft | null>
  prepareLayout(patch: LayoutPatch): Promise<ControlDraft | null>
  prepareFormat(patch: FormatPatch): Promise<ControlDraft | null>
  commit(): Promise<ControlCommitAck | null>
  abort(): Promise<ControlAbortAckLike | null>
  setMute(mute: boolean): Promise<ControlMuteAck | null>
  retryDevice(): Promise<DeviceRetryAck | null>
  /** 仅清空本地状态（重置页面/测试用；不向后端发 abort）。 */
  reset(): void
}

/** abort 回执的最小形状（api/control.ts 的 ControlAbortAck 已经过校验）。 */
export interface ControlAbortAckLike {
  ok: true
  aborted: boolean
  aborted_surface: string | null
}

export function createControlController(state: ControlState, deps: ControlDeps): ControlController {
  const now = deps.now ?? (() => Date.now())

  function pushHistory(entry: Omit<ControlOperationEntry, 'at'>): void {
    state.history.unshift({ at: now(), ...entry })
    if (state.history.length > MAX_OPERATION_HISTORY) {
      state.history.length = MAX_OPERATION_HISTORY
    }
  }

  function begin(action: ControlAction): boolean {
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

  function fail(action: ControlAction, surface: string | null, error: unknown): void {
    const info = classifyControlError(error)
    state.error = info
    pushHistory({
      action,
      ok: false,
      surface,
      version: null,
      token: null,
      code: info.code,
      detail: info.detail ?? null
    })
  }

  async function prepare(
    surface: string,
    request: () => Promise<ControlDraft>
  ): Promise<ControlDraft | null> {
    if (!begin('prepare')) {
      return null
    }
    try {
      const draft = await request()
      state.draft = draft
      pushHistory({
        action: 'prepare',
        ok: true,
        surface: draft.surface ?? surface,
        version: draft.planned_version,
        token: draft.engine_token,
        code: null,
        detail: draft.note
      })
      return draft
    } catch (error) {
      // prepare 失败：不保留旧草稿（避免把上一次意图当成这一次提交）。
      state.draft = null
      fail('prepare', surface, error)
      return null
    } finally {
      end()
    }
  }

  async function commit(): Promise<ControlCommitAck | null> {
    const draft = state.draft
    if (draft === null || !begin('commit')) {
      return null
    }
    try {
      const ack = await deps.api.commit(draft.engine_token)
      state.lastCommit = ack
      state.lastCommitAt = now()
      if (ack.mode !== null) {
        state.appliedMode = ack.mode
      }
      if (ack.format !== null) {
        state.appliedFormat = ack.format
      }
      state.draft = null
      pushHistory({
        action: 'commit',
        ok: true,
        surface: draft.surface,
        version: ack.version,
        token: draft.engine_token,
        code: null,
        detail: ack.note
      })
      return ack
    } catch (error) {
      const info = classifyControlError(error)
      // token 失效说明后端草稿已经不存在（或已被替换），本地不能再拿它 commit。
      if (info.category === 'token_invalid') {
        state.draft = null
      }
      fail('commit', draft.surface, error)
      return null
    } finally {
      end()
    }
  }

  async function abort(): Promise<ControlAbortAckLike | null> {
    const draft = state.draft
    if (draft === null || !begin('abort')) {
      return null
    }
    try {
      const ack = await deps.api.abort(draft.engine_token)
      state.draft = null
      pushHistory({
        action: 'abort',
        ok: true,
        surface: ack.aborted_surface ?? draft.surface,
        version: null,
        token: draft.engine_token,
        code: null,
        detail: null
      })
      return ack
    } catch (error) {
      fail('abort', draft.surface, error)
      return null
    } finally {
      end()
    }
  }

  async function setMute(mute: boolean): Promise<ControlMuteAck | null> {
    if (!begin('mute')) {
      return null
    }
    try {
      const ack = await deps.api.setMute(mute)
      state.lastMute = ack
      pushHistory({
        action: 'mute',
        ok: true,
        surface: null,
        version: null,
        token: null,
        code: null,
        detail: null
      })
      return ack
    } catch (error) {
      fail('mute', null, error)
      return null
    } finally {
      end()
    }
  }

  async function retryDevice(): Promise<DeviceRetryAck | null> {
    if (!begin('retry')) {
      return null
    }
    try {
      const ack = await deps.api.retryDevice()
      state.lastRetry = ack
      pushHistory({
        action: 'retry',
        ok: true,
        surface: null,
        version: null,
        token: null,
        code: null,
        detail: ack.last_error_message
      })
      return ack
    } catch (error) {
      fail('retry', null, error)
      return null
    } finally {
      end()
    }
  }

  return {
    prepareVolume: (gains) => prepare('volume', () => deps.api.prepareVolume(gains)),
    prepareMode: (mode) => prepare('mode', () => deps.api.prepareMode(mode)),
    prepareRouting: (patch) => prepare('routing', () => deps.api.prepareRouting(patch)),
    prepareLayout: (patch) => prepare('layout', () => deps.api.prepareLayout(patch)),
    prepareFormat: (patch) => prepare('format', () => deps.api.prepareFormat(patch)),
    commit,
    abort,
    setMute,
    retryDevice,
    reset(): void {
      Object.assign(state, createInitialControlState())
    }
  }
}

/* ---------------------------------------------------------------- 单例装配 */

/** 控制面写操作共享状态（音量 / 格式 / 模式页共用；离开页面时保留最后回执供回看）。 */
export const controlState = reactive<ControlState>(createInitialControlState())

const controlApi = createControlApi(apiClient)

const controller = createControlController(controlState, { api: controlApi })

export function prepareVolume(gains: readonly GainEntry[]): Promise<ControlDraft | null> {
  return controller.prepareVolume(gains)
}

export function prepareMode(mode: RunMode): Promise<ControlDraft | null> {
  return controller.prepareMode(mode)
}

export function prepareRouting(patch: RoutingPatch): Promise<ControlDraft | null> {
  return controller.prepareRouting(patch)
}

export function prepareLayout(patch: LayoutPatch): Promise<ControlDraft | null> {
  return controller.prepareLayout(patch)
}

export function prepareFormat(patch: FormatPatch): Promise<ControlDraft | null> {
  return controller.prepareFormat(patch)
}

export function commitControlDraft(): Promise<ControlCommitAck | null> {
  return controller.commit()
}

export function abortControlDraft(): Promise<ControlAbortAckLike | null> {
  return controller.abort()
}

export function setControlMute(mute: boolean): Promise<ControlMuteAck | null> {
  return controller.setMute(mute)
}

export function retryDeviceReadback(): Promise<DeviceRetryAck | null> {
  return controller.retryDevice()
}

/** 重置控制面状态（测试 / 重新进入页面时显式丢弃本地草稿；不发 abort）。 */
export function resetControlState(): void {
  controller.reset()
}

export { controlApi, controller as controlController }
