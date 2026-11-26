/**
 * 能力快照 store（GET /api/meta）—— 页面入口的单一事实来源（docs/API.md §9.1）。
 *
 * - 单例缓存：首个页面进入时拉取一次；手动 refresh 可强制刷新；并发去重（在途共享同一个 Promise）；
 * - 失败不清空上次成功快照（能力不是实时状态，展示最后已知能力 + 错误），但 phase='error' 可观察；
 * - meta 拿不到时 surfaceState() 返回 'unknown'：页面仍可降级渲染，但必须如实标注"能力未知"，
 *   不得把 unknown 当成 wired 或 not_wired。
 *
 * 可测试性：createMetaController(state, deps) 注入 api/时钟，Node 单测直接覆盖。
 */
import { reactive } from 'vue'
import {
  capabilityReason,
  createMetaApi,
  opState,
  surfaceState,
  type CapabilityState,
  type MetaApi,
  type MetaCapabilities
} from '../api/meta.ts'
import { ApiError } from '../api/client.ts'
import { apiClient } from './session.ts'

/** meta 拉取相位。 */
export type MetaPhase = 'idle' | 'loading' | 'ready' | 'error'

export interface MetaErrorInfo {
  code: string
  message: string
  /** true = 值得重试（网络/超时/5xx）；401 由会话 store 统一跳登录。 */
  retryable: boolean
}

export interface MetaState {
  data: MetaCapabilities | null
  phase: MetaPhase
  error: MetaErrorInfo | null
  loadedAt: number | null
}

export function createInitialMetaState(): MetaState {
  return { data: null, phase: 'idle', error: null, loadedAt: null }
}

export interface MetaDeps {
  api: MetaApi
  now?: () => number
}

export interface MetaController {
  /** 拉取能力（force=false 时已有快照直接返回；并发共享在途请求）。 */
  load(force?: boolean): Promise<MetaCapabilities | null>
  reset(): void
}

function classifyMetaError(error: unknown): MetaErrorInfo {
  if (error instanceof ApiError) {
    return {
      code: error.code,
      message: error.message,
      retryable: error.status === 0 || error.status >= 500
    }
  }
  const message = error instanceof Error ? error.message : 'meta request failed'
  return { code: 'unknown_error', message, retryable: true }
}

export function createMetaController(state: MetaState, deps: MetaDeps): MetaController {
  const now = deps.now ?? (() => Date.now())
  let inflight: Promise<MetaCapabilities | null> | null = null

  async function load(force = false): Promise<MetaCapabilities | null> {
    if (!force && state.data !== null) {
      return state.data
    }
    if (inflight !== null) {
      return inflight
    }
    state.phase = 'loading'
    state.error = null
    inflight = (async () => {
      try {
        const data = await deps.api.fetch()
        state.data = data
        state.phase = 'ready'
        state.loadedAt = now()
        return data
      } catch (error) {
        state.phase = 'error'
        state.error = classifyMetaError(error)
        return null
      } finally {
        inflight = null
      }
    })()
    return inflight
  }

  return {
    load,
    reset(): void {
      Object.assign(state, createInitialMetaState())
    }
  }
}

/** 共享能力状态（页面读取；AppShell 启动时 ensureMeta 预热）。 */
export const metaState = reactive<MetaState>(createInitialMetaState())

const controller = createMetaController(metaState, { api: createMetaApi(apiClient) })

/** 确保能力快照已加载（force=false 时有缓存即返回）。 */
export function ensureMeta(force = false): Promise<MetaCapabilities | null> {
  return controller.load(force)
}

/** 重新拉取（页面刷新按钮用）。 */
export function refreshMeta(): Promise<MetaCapabilities | null> {
  return controller.load(true)
}

export function resetMetaState(): void {
  controller.reset()
}

/** 语义界面能力三态（未加载时为 unknown）。 */
export function metaSurfaceState(surface: string): CapabilityState {
  return surfaceState(metaState.data, surface)
}

/** host 控制 op 能力三态。 */
export function metaOpState(op: string): CapabilityState {
  return opState(metaState.data, op)
}

/** 未接通原因（surface / op）。 */
export function metaCapabilityReason(key: { op?: string; surface?: string }): string | null {
  return capabilityReason(metaState.data, key)
}

export { controller as metaController }
