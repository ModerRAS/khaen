/**
 * 方案与声道页 store（M4 / 任务 T54）。
 *
 * 两个独立控制器状态机：
 * - 声道（channels）：GET /api/channels（读） + PUT /api/channels prepare → 共享 commit/abort（写）。
 *   M3b 热重配未完成时 commit 返回 501 host_link_unsupported：页面如实显示 NOT_AVAILABLE + 原因；
 *   此前 prepare 的 preview/warnings/blockers 照实展示（不假装已应用）。
 * - 方案（plan）：客户端 sha256（WebCrypto）→ POST /api/plan/upload → 服务端 sha 双侧比对 →
 *   POST /api/plan/inspect {plan_path} 预览（plan_path = 上传回执的 stored_path；失败降级为上传摘要，
 *   不编造）→ POST /api/prepare {plan_path, plan_sha256} → commit/abort；
 *   历史 GET /api/plan/history（daemon 真实形态 current:{version},entries[]）；回滚 POST /api/rollback
 *   （版本号递增，仍需 commit）。上传回执没有 stored_path（旧后端/mock）时 inspect/prepare 显示
 *   NOT_AVAILABLE + 稳定码 plan_path_unavailable，不猜路径、不假成功。
 *
 * 纪律：
 * - 同一时刻只允许一个写操作（busy 互斥）；失败保留已有数据并记录稳定错误码；
 * - 上传 sha 双侧不一致 = 失败（不信任任何一侧）；
 * - 历史/预览接口不可用（404/501/503）显示「未接入」+ 原因，清空对应数据而不是留旧数据冒充最新；
 * - 401 交给共享 ApiClient（会话 store 清态跳登录）。
 *
 * 可测试性：控制器注入 api/时钟；模块底部用 reactive state + 共享 ApiClient 组装单例。
 */
import { reactive } from 'vue'
import {
  classifyChannelsError,
  createChannelsApi,
  sha256Hex,
  type ChannelsApi,
  type ChannelsErrorInfo,
  type ChannelsInfo,
  type ChannelsPatch,
  type ChannelsPrepareAck,
  type PlanHistory,
  type PlanInspect,
  type PlanUploadAck
} from '../api/channels.ts'
import type { ControlCommitAck, ControlDraft } from '../api/control.ts'
import { apiClient } from './session.ts'

export type ChannelsPhase = 'loading' | 'ready' | 'error'
export type PlanBusy = 'upload' | 'inspect' | 'prepare' | 'commit' | 'abort' | 'rollback' | null
export type ChannelsWriteBusy = 'prepare' | 'commit' | 'abort' | null

/** 本地上传态：文件名/字节/客户端 sha256 + 后端回执。byteLength 足够页面显示大小。 */
export interface PlanUploadState {
  name: string | null
  byteLength: number | null
  bytes: Uint8Array | null
  sha256: string | null
  ack: PlanUploadAck | null
  error: ChannelsErrorInfo | null
}

export interface ChannelsState {
  info: ChannelsInfo | null
  phase: ChannelsPhase
  error: ChannelsErrorInfo | null
  refreshing: boolean
  loadedAt: number | null
  /** 方案上传/预览/两阶段状态。 */
  plan: {
    upload: PlanUploadState
    inspect: PlanInspect | null
    inspectError: ChannelsErrorInfo | null
    draft: ControlDraft | null
    lastCommit: ControlCommitAck | null
    lastCommitAt: number | null
    error: ChannelsErrorInfo | null
    busy: PlanBusy
  }
  history: PlanHistory | null
  historyError: ChannelsErrorInfo | null
  /** 声道布局编辑（两阶段；M3b 未完成时 commit 如实失败）。 */
  channelsDraft: ChannelsPrepareAck | null
  channelsCommit: ControlCommitAck | null
  channelsError: ChannelsErrorInfo | null
  channelsBusy: ChannelsWriteBusy
}

export function createInitialPlanUploadState(): PlanUploadState {
  return { name: null, byteLength: null, bytes: null, sha256: null, ack: null, error: null }
}

export function createInitialChannelsState(): ChannelsState {
  return {
    info: null,
    phase: 'loading',
    error: null,
    refreshing: false,
    loadedAt: null,
    plan: {
      upload: createInitialPlanUploadState(),
      inspect: null,
      inspectError: null,
      draft: null,
      lastCommit: null,
      lastCommitAt: null,
      error: null,
      busy: null
    },
    history: null,
    historyError: null,
    channelsDraft: null,
    channelsCommit: null,
    channelsError: null,
    channelsBusy: null
  }
}

export interface ChannelsDeps {
  api: ChannelsApi
  now?: () => number
  /** 可注入的 sha256（单测避免依赖 WebCrypto；缺省用 WebCrypto）。 */
  sha256?: (bytes: Uint8Array) => Promise<string>
}

export interface ChannelsController {
  load(): Promise<void>
  refresh(): Promise<void>
  loadHistory(): Promise<void>
  /** plan_path 缺省 = 最近一次上传回执的 stored_path；没有则记 NOT_AVAILABLE（不猜路径）。 */
  loadInspect(planPath?: string | null): Promise<void>
  /** 选择本地 plan 文件：算客户端 sha256（不发网络）。 */
  selectPlanFile(name: string, bytes: Uint8Array): Promise<void>
  /** 上传已选文件并与后端 sha256 双侧比对。 */
  uploadPlan(): Promise<PlanUploadAck | null>
  /** 用上传回执准备方案草稿（两阶段）。 */
  preparePlan(): Promise<ControlDraft | null>
  /** 提交当前方案草稿（上传方案或回滚草稿）。 */
  commitPlan(): Promise<ControlCommitAck | null>
  abortPlan(): Promise<boolean>
  /** 准备历史回滚草稿（版本号递增，仍需 commit）。 */
  prepareRollback(target: number): Promise<ControlDraft | null>
  prepareChannels(patch: ChannelsPatch): Promise<ChannelsPrepareAck | null>
  commitChannels(): Promise<ControlCommitAck | null>
  abortChannels(): Promise<boolean>
  reset(): void
}

export function createChannelsController(state: ChannelsState, deps: ChannelsDeps): ChannelsController {
  const now = deps.now ?? (() => Date.now())
  const sha256 = deps.sha256 ?? sha256Hex
  let inflight: Promise<void> | null = null

  async function runLoad(): Promise<void> {
    state.refreshing = true
    if (state.info === null) {
      state.phase = 'loading'
    }
    try {
      const info = await deps.api.fetchChannels()
      state.info = info
      state.phase = 'ready'
      state.error = null
      state.loadedAt = now()
    } catch (error) {
      state.error = classifyChannelsError(error)
      state.phase = state.info === null ? 'error' : 'ready'
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

  async function loadHistory(): Promise<void> {
    try {
      const history = await deps.api.fetchPlanHistory()
      state.history = history
      state.historyError = null
    } catch (error) {
      state.history = null
      state.historyError = classifyChannelsError(error)
    }
  }

  /** 上传回执没有 plan_path 时的诚实降级（不猜路径、不伪造预览）。 */
  function planPathUnavailableError(detail: string): ChannelsErrorInfo {
    return {
      category: 'plan_path_unavailable',
      status: 0,
      code: 'plan_path_unavailable',
      i18nKey: 'error.channels.planPathUnavailable',
      detail,
      retryable: false
    }
  }

  /** POST /api/plan/inspect {plan_path}：plan_path 来自上传回执的 stored_path。 */
  async function runInspect(planPath: string | null, planSha256: string | null): Promise<void> {
    try {
      if (planPath === null) {
        state.plan.inspect = null
        state.plan.inspectError = planPathUnavailableError(
          'the upload ack has no stored_path: POST /api/plan/inspect needs plan_path (old backend / mock)'
        )
        return
      }
      const inspect = await deps.api.fetchPlanInspect(planPath, planSha256)
      state.plan.inspect = inspect
      planInspectErrorClear()
    } catch (error) {
      state.plan.inspect = null
      state.plan.inspectError = classifyChannelsError(error)
    }
  }

  function planInspectErrorClear(): void {
    state.plan.inspectError = null
  }

  async function loadInspect(planPath?: string | null): Promise<void> {
    if (state.plan.busy !== null) {
      return
    }
    state.plan.busy = 'inspect'
    try {
      const resolved = planPath !== undefined ? planPath : (state.plan.upload.ack?.stored_path ?? null)
      await runInspect(resolved, state.plan.upload.ack?.sha256 ?? null)
    } finally {
      state.plan.busy = null
    }
  }

  async function selectPlanFile(name: string, bytes: Uint8Array): Promise<void> {
    state.plan.error = null
    state.plan.upload = createInitialPlanUploadState()
    state.plan.inspect = null
    state.plan.inspectError = null
    state.plan.upload.name = name
    state.plan.upload.byteLength = bytes.length
    const copy = new Uint8Array(bytes.length)
    copy.set(bytes)
    state.plan.upload.bytes = copy
    try {
      const digest = await sha256(bytes)
      state.plan.upload.sha256 = digest
    } catch (error) {
      state.plan.upload.error = classifyChannelsError(error)
    }
  }

  async function uploadPlan(): Promise<PlanUploadAck | null> {
    const upload = state.plan.upload
    if (state.plan.busy !== null || upload.name === null || upload.bytes === null) {
      return null
    }
    if (upload.error !== null) {
      return null
    }
    if (upload.sha256 === null) {
      state.plan.error = {
        category: 'unknown',
        status: 0,
        code: 'sha256_unavailable',
        i18nKey: 'error.channels.shaUnavailable',
        retryable: true
      }
      return null
    }
    state.plan.busy = 'upload'
    state.plan.error = null
    try {
      const ack = await deps.api.uploadPlan(upload.bytes, upload.name)
      if (ack.sha256 !== upload.sha256) {
        // 双侧 sha 不一致：拒绝（不信任任何一侧），不进入 prepare。
        state.plan.upload.error = {
          category: 'sha_mismatch',
          status: 0,
          code: 'sha_mismatch',
          i18nKey: 'error.channels.shaMismatch',
          detail: 'client ' + upload.sha256 + ' != server ' + ack.sha256,
          retryable: true
        }
        return null
      }
      state.plan.upload.ack = ack
      state.plan.upload.error = null
      // 真实 daemon 的回执带 stored_path：inspect/prepare 都用它；没有时如实降级（不猜路径）。
      await runInspect(ack.stored_path, ack.sha256)
      return ack
    } catch (error) {
      state.plan.upload.error = classifyChannelsError(error)
      return null
    } finally {
      state.plan.busy = null
    }
  }

  async function preparePlan(): Promise<ControlDraft | null> {
    const ack = state.plan.upload.ack
    if (ack === null || state.plan.busy !== null) {
      return null
    }
    if (ack.stored_path === null) {
      state.plan.error = planPathUnavailableError(
        'the upload ack has no stored_path: POST /api/prepare needs plan_path (old backend / mock)'
      )
      return null
    }
    state.plan.busy = 'prepare'
    state.plan.error = null
    try {
      const draft = await deps.api.preparePlan({
        plan_path: ack.stored_path,
        sha256: ack.sha256
      })
      state.plan.draft = draft
      return draft
    } catch (error) {
      state.plan.draft = null
      state.plan.error = classifyChannelsError(error)
      return null
    } finally {
      state.plan.busy = null
    }
  }

  async function commitPlan(): Promise<ControlCommitAck | null> {
    const draft = state.plan.draft
    if (draft === null || state.plan.busy !== null) {
      return null
    }
    state.plan.busy = 'commit'
    state.plan.error = null
    try {
      const ack = await deps.api.commit(draft.engine_token)
      state.plan.lastCommit = ack
      state.plan.lastCommitAt = now()
      state.plan.draft = null
      state.plan.inspect = null
      // 生效后刷新历史、预览与声道/方案概览（失败不清空旧数据，只记录错误）。
      // 预览仍针对这次上传的落盘文件（stored_path）；没有该字段时如实 NOT_AVAILABLE。
      await loadHistory()
      await runInspect(state.plan.upload.ack?.stored_path ?? null, state.plan.upload.ack?.sha256 ?? null)
      await load()
      return ack
    } catch (error) {
      state.plan.error = classifyChannelsError(error)
      return null
    } finally {
      state.plan.busy = null
    }
  }

  async function abortPlan(): Promise<boolean> {
    const draft = state.plan.draft
    if (draft === null || state.plan.busy !== null) {
      return false
    }
    state.plan.busy = 'abort'
    state.plan.error = null
    try {
      await deps.api.abort(draft.engine_token)
      state.plan.draft = null
      return true
    } catch (error) {
      state.plan.error = classifyChannelsError(error)
      return false
    } finally {
      state.plan.busy = null
    }
  }

  async function prepareRollback(target: number): Promise<ControlDraft | null> {
    if (state.plan.busy !== null) {
      return null
    }
    state.plan.busy = 'rollback'
    state.plan.error = null
    try {
      const draft = await deps.api.rollbackPlan(target)
      state.plan.draft = draft
      return draft
    } catch (error) {
      state.plan.draft = null
      state.plan.error = classifyChannelsError(error)
      return null
    } finally {
      state.plan.busy = null
    }
  }

  async function prepareChannels(patch: ChannelsPatch): Promise<ChannelsPrepareAck | null> {
    if (state.channelsBusy !== null) {
      return null
    }
    state.channelsBusy = 'prepare'
    state.channelsError = null
    try {
      const ack = await deps.api.prepareChannels(patch)
      state.channelsDraft = ack
      return ack
    } catch (error) {
      state.channelsDraft = null
      state.channelsError = classifyChannelsError(error)
      return null
    } finally {
      state.channelsBusy = null
    }
  }

  async function commitChannels(): Promise<ControlCommitAck | null> {
    const draft = state.channelsDraft
    if (draft === null || state.channelsBusy !== null) {
      return null
    }
    state.channelsBusy = 'commit'
    state.channelsError = null
    try {
      const ack = await deps.api.commit(draft.engine_token)
      state.channelsCommit = ack
      state.channelsDraft = null
      await load()
      return ack
    } catch (error) {
      // M3b 未完成时 commit 返回 501 host_link_unsupported：保留草稿与错误码，不假装成功。
      state.channelsError = classifyChannelsError(error)
      return null
    } finally {
      state.channelsBusy = null
    }
  }

  async function abortChannels(): Promise<boolean> {
    const draft = state.channelsDraft
    if (draft === null || state.channelsBusy !== null) {
      return false
    }
    state.channelsBusy = 'abort'
    state.channelsError = null
    try {
      await deps.api.abort(draft.engine_token)
      state.channelsDraft = null
      return true
    } catch (error) {
      state.channelsError = classifyChannelsError(error)
      return false
    } finally {
      state.channelsBusy = null
    }
  }

  return {
    load,
    refresh: load,
    loadHistory,
    loadInspect,
    selectPlanFile,
    uploadPlan,
    preparePlan,
    commitPlan,
    abortPlan,
    prepareRollback,
    prepareChannels,
    commitChannels,
    abortChannels,
    reset(): void {
      Object.assign(state, createInitialChannelsState())
    }
  }
}

/* ---------------------------------------------------------------- 单例装配 */

export const channelsState = reactive<ChannelsState>(createInitialChannelsState())

const channelsApi = createChannelsApi(apiClient)

const controller = createChannelsController(channelsState, { api: channelsApi })

export function loadChannels(): Promise<void> {
  return controller.load()
}

export function refreshChannels(): Promise<void> {
  return controller.refresh()
}

export function loadChannelsHistory(): Promise<void> {
  return controller.loadHistory()
}

export function loadChannelsInspect(planPath?: string | null): Promise<void> {
  return controller.loadInspect(planPath)
}

export function selectChannelPlanFile(name: string, bytes: Uint8Array): Promise<void> {
  return controller.selectPlanFile(name, bytes)
}

export function uploadChannelPlan(): Promise<PlanUploadAck | null> {
  return controller.uploadPlan()
}

export function prepareChannelPlan(): Promise<ControlDraft | null> {
  return controller.preparePlan()
}

export function commitChannelPlan(): Promise<ControlCommitAck | null> {
  return controller.commitPlan()
}

export function abortChannelPlan(): Promise<boolean> {
  return controller.abortPlan()
}

export function prepareChannelRollback(target: number): Promise<ControlDraft | null> {
  return controller.prepareRollback(target)
}

export function prepareChannelLayout(patch: ChannelsPatch): Promise<ChannelsPrepareAck | null> {
  return controller.prepareChannels(patch)
}

export function commitChannelLayout(): Promise<ControlCommitAck | null> {
  return controller.commitChannels()
}

export function abortChannelLayout(): Promise<boolean> {
  return controller.abortChannels()
}

export function resetChannelsState(): void {
  controller.reset()
}

export { channelsApi, controller as channelsController }
