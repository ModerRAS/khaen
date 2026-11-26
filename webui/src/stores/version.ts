/**
 * release 版本 store（任务 T47：顶栏 / 侧栏 / 登录页页脚统一显示 **release 版本**）。
 *
 * 背景：当前部署 v0.1.2，而 /healthz 曾上报 version=0.1.0（crate 版本）——版本展示必须区分
 * crate/release/commit（详见 api/system.ts 与系统信息页）。T46 起 /healthz 的 version 改为 release 版本，
 * 且该端点匿名可用，所以登录前后都能统一显示。
 *
 * 数据源：GET /healthz（匿名）。纪律：
 * - 只缓存成功结果；失败保持 null + 记录错误（绝不拿 webui package 版本冒充 release 版本）；
 * - force=true 强制重查；并发去重；
 * - 纯逻辑控制器可注入 fetch/时钟，Node 单测直接覆盖。
 */
import { reactive } from 'vue'
import {
  classifySystemError,
  createSystemApi,
  formatVersionLabel,
  type HealthzInfo,
  type SystemErrorInfo
} from '../api/system.ts'
import { apiClient } from './session.ts'

export type ReleaseVersionPhase = 'idle' | 'loading' | 'ready' | 'error'

export interface ReleaseVersionState {
  /** release 版本原文（来自 /healthz；null = 未知）。 */
  version: string | null
  /** 统一展示格式（带 v 前缀；null = 未知）。 */
  label: string | null
  phase: ReleaseVersionPhase
  error: SystemErrorInfo | null
  /** 是否已完成过一次探测（成功或失败）。 */
  checked: boolean
  loadedAt: number | null
}

export function createInitialReleaseVersionState(): ReleaseVersionState {
  return {
    version: null,
    label: null,
    phase: 'idle',
    error: null,
    checked: false,
    loadedAt: null
  }
}

export interface ReleaseVersionDeps {
  /** 拉取 /healthz（匿名）。 */
  fetchHealthz: () => Promise<HealthzInfo>
  /** 可注入时钟。 */
  now?: () => number
}

export interface ReleaseVersionController {
  /** 拉取 release 版本；已有成功结果且 force=false 时直接返回缓存。 */
  load(force?: boolean): Promise<void>
  /** 重置（登出/测试用）。 */
  clear(): void
}

export function createReleaseVersionController(
  state: ReleaseVersionState,
  deps: ReleaseVersionDeps
): ReleaseVersionController {
  const now = deps.now ?? (() => Date.now())
  let inflight: Promise<void> | null = null

  async function run(): Promise<void> {
    state.phase = state.version === null ? 'loading' : state.phase
    try {
      const healthz = await deps.fetchHealthz()
      state.version = healthz.version
      state.label = formatVersionLabel(healthz.version)
      state.error = null
      state.phase = 'ready'
      state.loadedAt = now()
    } catch (error) {
      state.error = classifySystemError(error)
      state.phase = 'error'
      // 失败时保留最后一次成功版本（不清空），但没有版本时保持 null。
    } finally {
      state.checked = true
      inflight = null
    }
  }

  function load(force = false): Promise<void> {
    if (inflight !== null) {
      return inflight
    }
    if (!force && state.phase === 'ready' && state.version !== null) {
      return Promise.resolve()
    }
    const started = run()
    inflight = started
    return started
  }

  function clear(): void {
    const initial = createInitialReleaseVersionState()
    Object.assign(state, initial)
    inflight = null
  }

  return { load, clear }
}

/* ---------------------------------------------------------------- 单例装配 */

export const releaseVersionState = reactive<ReleaseVersionState>(createInitialReleaseVersionState())

const systemApi = createSystemApi(apiClient)

const controller = createReleaseVersionController(releaseVersionState, {
  fetchHealthz: () => systemApi.fetchHealthz()
})

/** 拉取 release 版本（AppShell / LoginView 挂载时调用；失败不阻塞页面）。 */
export function loadReleaseVersion(force = false): Promise<void> {
  return controller.load(force)
}

/** 当前 release 版本展示值（未知 -> null，由页面显示「—」）。 */
export function releaseVersionLabel(): string | null {
  return releaseVersionState.label
}
