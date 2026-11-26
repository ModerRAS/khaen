/**
 * 方案与声道页 store。
 *
 * 两个独立控制器状态机：
 * - 声道热重配（channels）：GET /api/channels（只读，恒 200）+ PUT /api/channels 两阶段
 *   （stage=prepare → commit / abort，docs/API.md §5）。prepare 只生成 rt-host 草稿不改变运行态；
 *   commit 在块边界生效并返回真实回执（applied/generation/gap_ms/format/graph_dropped/…）；
 *   mapping/dac_map 是**方案 routing**：commit_supported=false 时 store 拒绝发 commit
 *   （本地 error=commit_unsupported + 展示 blockers），绝不假成功；502 reconfig_failed 的
 *   details.rolled_back 等失败明细保存在 channelsFailure 里原样展示。
 * - 方案（plan）：客户端 sha256（WebCrypto）→ POST /api/plan/upload → 服务端 sha 双侧比对 →
 *   POST /api/plan/inspect {plan_path} 预览（plan_path = 上传回执的 stored_path；失败降级为上传摘要，
 *   不编造）→ POST /api/prepare {plan_path, plan_sha256} → POST /api/commit / POST /api/abort；
 *   历史 GET /api/plan/history（daemon 真实形态 current:{version},entries[]）；回滚 POST /api/rollback
 *   （版本号递增，仍需 commit）。上传回执没有 stored_path（旧后端/mock）时 inspect/prepare 显示
 *   NOT_AVAILABLE + 稳定码 plan_path_unavailable，不猜路径、不假成功。
 *
 * 纪律：
 * - 同一时刻只允许一个写操作（busy 互斥）；失败保留已有读取数据并记录稳定错误码；
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
  type ChannelsAbortAck,
  type ChannelsApi,
  type ChannelsCommitAck,
  type ChannelsCommitFailure,
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
/** 声道写操作阶段（失败时用于页面标注是 prepare / commit / abort 哪一步）。 */
export type ChannelsWriteStage = 'prepare' | 'commit' | 'abort'

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
  /** 声道热重配两阶段（PUT /api/channels）。 */
  channelsDraft: ChannelsPrepareAck | null
  /** 最近一次 commit 的真实回执（applied/generation/gap_ms/format/graph_dropped/routing）。 */
  channelsCommit: ChannelsCommitAck | null
  channelsCommitAt: number | null
  channelsError: ChannelsErrorInfo | null
  /** 502 热重配失败明细（details.rolled_back/device_started/applied/…）；非 502 为 null。 */
  channelsFailure: ChannelsCommitFailure | null
  /** 最近一次失败发生在哪一阶段（页面据此提示「prepare/commit 失败」）。 */
  channelsErrorStage: ChannelsWriteStage | null
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
    channelsCommitAt: null,
    channelsError: null,
    channelsFailure: null,
    channelsErrorStage: null,
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
  /** PUT /api/channels {stage:'prepare', ...patch}：只生成草稿，不改变运行态。 */
  prepareChannels(patch: ChannelsPatch): Promise<ChannelsPrepareAck | null>
  /**
   * PUT /api/channels {stage:'commit', engine_token}。
   * commit_supported=false（mapping 属方案 routing）时不发请求：本地记 commit_unsupported + blockers。
   */
  commitChannels(): Promise<ChannelsCommitAck | null>
  /** PUT /api/channels {stage:'abort', engine_token}；没有服务端 token 时等价本地丢弃。 */
  abortChannels(): Promise<boolean>
  /** 本地丢弃草稿（不假定服务端草稿已删；用于 token=null / abort 失败后的诚实退出）。 */
  discardChannelsDraft(): void
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
      state.plan.inspectError = null
    } catch (error) {
      state.plan.inspect = null
      state.plan.inspectError = classifyChannelsError(error)
    }
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

  /* ------------------------------------------------------- 声道热重配（PUT 两阶段） */

  function localCommitUnsupportedError(draft: ChannelsPrepareAck): ChannelsErrorInfo {
    const blockers = draft.blockers.length > 0 ? draft.blockers : [draft.mapping?.reason ?? 'commit_supported=false']
    return {
      category: 'commit_unsupported',
      status: 0,
      code: 'commit_unsupported',
      i18nKey: 'channelsEdit.error.commitUnsupported',
      detail: blockers.join(' | '),
      retryable: false
    }
  }

  async function prepareChannels(patch: ChannelsPatch): Promise<ChannelsPrepareAck | null> {
    if (state.channelsBusy !== null) {
      return null
    }
    state.channelsBusy = 'prepare'
    state.channelsError = null
    state.channelsErrorStage = null
    state.channelsFailure = null
    try {
      const ack = await deps.api.prepareChannels(patch)
      state.channelsDraft = ack
      return ack
    } catch (error) {
      state.channelsDraft = null
      state.channelsError = classifyChannelsError(error)
      state.channelsErrorStage = 'prepare'
      state.channelsFailure = state.channelsError.failure ?? null
      return null
    } finally {
      state.channelsBusy = null
    }
  }

  async function commitChannels(): Promise<ChannelsCommitAck | null> {
    const draft = state.channelsDraft
    if (draft === null || state.channelsBusy !== null) {
      return null
    }
    // mapping/dac_map 属方案 routing：后端明确 commit_supported=false 时禁止发 commit（不假成功）。
    if (!draft.commit_supported || draft.engine_token === null) {
      state.channelsError = localCommitUnsupportedError(draft)
      state.channelsErrorStage = 'commit'
      state.channelsFailure = null
      return null
    }
    state.channelsBusy = 'commit'
    state.channelsError = null
    state.channelsErrorStage = null
    state.channelsFailure = null
    try {
      const ack = await deps.api.commitChannels(draft.engine_token)
      state.channelsCommit = ack
      state.channelsCommitAt = now()
      state.channelsDraft = null
      // 生效后重新读 GET /api/channels（数据面真实配置 + mapping 可信度 + generation）。
      await load()
      return ack
    } catch (error) {
      const info = classifyChannelsError(error)
      state.channelsError = info
      state.channelsErrorStage = 'commit'
      state.channelsFailure = info.failure ?? null
      // rt-host 在 commit 开始时消费草稿（失败即丢弃）：token 不可重放。
      // 只有 409 冲突（busy/cal_busy/reconfiguring/token 不符）可能草稿仍在，保留以便重试/abort。
      if (info.category !== 'conflict') {
        state.channelsDraft = null
      }
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
    const token = draft.engine_token
    if (token === null) {
      // commit_supported=false（mapping-only）：后端没有草稿/token，本地丢弃即可（不假装调用服务端）。
      state.channelsDraft = null
      state.channelsError = null
      state.channelsErrorStage = null
      state.channelsFailure = null
      return true
    }
    state.channelsBusy = 'abort'
    state.channelsError = null
    state.channelsErrorStage = null
    state.channelsFailure = null
    try {
      const ack: ChannelsAbortAck = await deps.api.abortChannels(token)
      if (ack.aborted === false) {
        // 后端明确说没丢（例如 no_channels_draft）：如实报错，不显示成功。
        state.channelsError = {
          category: 'conflict',
          status: 200,
          code: 'abort_not_applied',
          i18nKey: 'channelsEdit.error.abortNotApplied',
          detail: ack.note ?? 'the backend reported aborted=false',
          retryable: true
        }
        state.channelsErrorStage = 'abort'
        return false
      }
      state.channelsDraft = null
      return true
    } catch (error) {
      const info = classifyChannelsError(error)
      state.channelsError = info
      state.channelsErrorStage = 'abort'
      state.channelsFailure = info.failure ?? null
      return false
    } finally {
      state.channelsBusy = null
    }
  }

  function discardChannelsDraft(): void {
    if (state.channelsBusy !== null) {
      return
    }
    state.channelsDraft = null
    state.channelsError = null
    state.channelsErrorStage = null
    state.channelsFailure = null
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
    discardChannelsDraft,
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

export function commitChannelLayout(): Promise<ChannelsCommitAck | null> {
  return controller.commitChannels()
}

export function abortChannelLayout(): Promise<boolean> {
  return controller.abortChannels()
}

export function discardChannelLayoutDraft(): void {
  controller.discardChannelsDraft()
}

export function resetChannelsState(): void {
  controller.reset()
}

export { channelsApi, controller as channelsController }
