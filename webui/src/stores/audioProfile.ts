/**
 * 音频 profile / Speaker Layout store（task-73；docs/API.md §12 v1.2）。
 *
 * 职责：
 * - 只读快照（GET /api/audio-profile + GET /api/device/inputs）：失败保留上一次成功快照并标注；
 *   后端未接线时 profile=null + reasons，页面如实显示「未接线」，不编数据；
 * - 两步保存：PUT prepare（草稿 + validation/diff/token）→ commit/abort；
 * - 应用链路（用户确认后）：POST /api/audio-profile/plan → /api/plan/inspect → /api/prepare → /api/commit；
 *   任一步失败立即停止并记录步骤，不自动回退、不伪造回执；
 * - 测试音：POST /api/speaker-test（须 audible+confirm）→ 本地倒计时（用 startedAt + ms 计算）
 *   → POST /api/speaker-test/stop（幂等）；
 * - 所有写操作串行（busy 互斥）；401 交给会话 store；错误归类为稳定类别 + i18n 键。
 *
 * 可测试性：createAudioProfileController(state, deps) 注入 api/时钟，Node 单测覆盖状态机。
 */
import { reactive } from 'vue'
import {
  AUDIO_PROFILE_SCHEMA,
  classifyAudioProfileError,
  createAudioProfileApi,
  createDefaultProfile,
  validateSpeakerTestRequest,
  validateProfileLocally,
  type AudioProfile,
  type AudioProfileApi,
  type AudioProfileCommitAck,
  type AudioProfileDraft,
  type AudioProfileErrorInfo,
  type AudioProfileSnapshot,
  type AudioSourceKind,
  type CommitPlanAck,
  type DeviceInputsCapability,
  type GeneratedPlan,
  type PreparePlanDraft,
  type ProfileValidation,
  type SpeakerTestAck,
  type SpeakerTestRequest
} from '../api/audioProfile.ts'
import { apiClient } from './session.ts'

/** 当前进行中的操作（互斥）。 */
export type AudioProfileBusy = 'profile' | 'inputs' | 'prepare' | 'commit' | 'abort' | 'apply' | 'test' | 'stop' | null

export type ApplyStepName = 'plan' | 'inspect' | 'prepare' | 'commit'

export interface ApplyStep {
  step: ApplyStepName
  ok: boolean
  /** 失败时的稳定码 / 说明（成功为 null）。 */
  detail: string | null
  at: number
}

export interface ApplyOutcome {
  ok: boolean
  steps: ApplyStep[]
  plan: GeneratedPlan | null
  /** /api/prepare 的草稿 token（commit 成功后保留供展示；失败时可手动 abort）。 */
  planDraft: PreparePlanDraft | null
  commit: CommitPlanAck | null
  error: AudioProfileErrorInfo | null
}

/** 测试音运行态（remainingMs 由页面按 now-startedAt 计算，store 不持有定时器）。 */
export interface SpeakerTestState {
  active: boolean
  dac: number
  ms: number
  kind: 'noise' | 'sine'
  levelDb: number
  startedAt: number
  /** 服务端回执 remaining_ms（null 时用 ms）。 */
  serverRemainingMs: number | null
}

export interface AudioProfileState {
  /** 只读快照；null = 还没成功读到。 */
  snapshot: AudioProfileSnapshot | null
  profilePhase: 'idle' | 'loading' | 'ready' | 'error'
  profileError: AudioProfileErrorInfo | null
  profileLoadedAt: number | null
  /** 声卡输入能力（GET /api/device/inputs）。 */
  deviceInputs: DeviceInputsCapability | null
  deviceInputsPhase: 'idle' | 'loading' | 'ready' | 'error'
  deviceInputsError: AudioProfileErrorInfo | null
  /** 已 prepare 未 decided 的 profile 草稿。 */
  draft: AudioProfileDraft | null
  /** 最近一次 commit 成功回执。 */
  lastCommit: AudioProfileCommitAck | null
  lastCommitAt: number | null
  /** 应用链路最近一次结果（步骤逐条如实记录）。 */
  apply: ApplyOutcome | null
  /** 测试音运行态。 */
  test: SpeakerTestState | null
  lastTest: SpeakerTestAck | null
  busy: AudioProfileBusy
  error: AudioProfileErrorInfo | null
}

export function createInitialAudioProfileState(): AudioProfileState {
  return {
    snapshot: null,
    profilePhase: 'idle',
    profileError: null,
    profileLoadedAt: null,
    deviceInputs: null,
    deviceInputsPhase: 'idle',
    deviceInputsError: null,
    draft: null,
    lastCommit: null,
    lastCommitAt: null,
    apply: null,
    test: null,
    lastTest: null,
    busy: null,
    error: null
  }
}

/** 页面编辑用的本地 profile（读不到/未接线时 = 默认 2.0；页面必须标注这是本地草稿）。 */
export function editingProfile(snapshot: AudioProfileSnapshot | null): AudioProfile {
  if (snapshot?.profile != null) {
    return snapshot.profile
  }
  return createDefaultProfile(snapshot?.plan.dac_channels ?? undefined)
}

/** 草稿校验：后端 validation 优先，本地 advisory 合并（错误取并集；重复码去重）。 */
export function mergedValidation(draft: AudioProfileDraft | null): ProfileValidation {
  if (draft === null) {
    return { ok: true, errors: [], warnings: [] }
  }
  const local = validateProfileLocally(draft.profile)
  const errors = [...draft.validation.errors]
  const warnings = [...draft.validation.warnings]
  const seen = new Set(errors.map((issue) => (issue.code ?? '') + '|' + (issue.path ?? '')))
  for (const issue of local.errors) {
    const key = (issue.code ?? '') + '|' + (issue.path ?? '')
    if (!seen.has(key)) {
      errors.push(issue)
      seen.add(key)
    }
  }
  for (const issue of local.warnings) {
    if (!warnings.some((existing) => existing.code === issue.code && existing.path === issue.path)) {
      warnings.push(issue)
    }
  }
  return { ok: errors.length === 0, errors, warnings }
}

export interface AudioProfileDeps {
  api: AudioProfileApi
  now?: () => number
}

export interface AudioProfileController {
  loadProfile(force?: boolean): Promise<AudioProfileSnapshot | null>
  loadDeviceInputs(force?: boolean): Promise<DeviceInputsCapability | null>
  prepare(profile: AudioProfile): Promise<AudioProfileDraft | null>
  commit(): Promise<AudioProfileCommitAck | null>
  abort(): Promise<boolean>
  apply(): Promise<ApplyOutcome>
  abortApply(): Promise<boolean>
  startTest(request: SpeakerTestRequest): Promise<SpeakerTestAck | null>
  stopTest(): Promise<boolean>
  /** 清理测试音本地状态（页面倒计时归零后调用；不请求后端）。 */
  clearTest(): void
  reset(): void
}

export function createAudioProfileController(state: AudioProfileState, deps: AudioProfileDeps): AudioProfileController {
  const now = deps.now ?? (() => Date.now())
  let profileInflight: Promise<AudioProfileSnapshot | null> | null = null
  let inputsInflight: Promise<DeviceInputsCapability | null> | null = null

  function begin(action: AudioProfileBusy): boolean {
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

  function fail(error: unknown): AudioProfileErrorInfo {
    const info = classifyAudioProfileError(error)
    state.error = info
    return info
  }

  async function loadProfile(force = false): Promise<AudioProfileSnapshot | null> {
    if (!force && state.snapshot !== null) {
      return state.snapshot
    }
    if (profileInflight !== null) {
      return profileInflight
    }
    if (!begin('profile')) {
      return null
    }
    state.profilePhase = 'loading'
    profileInflight = (async () => {
      try {
        const snapshot = await deps.api.fetchProfile()
        state.snapshot = snapshot
        state.profilePhase = 'ready'
        state.profileLoadedAt = now()
        state.profileError = null
        return snapshot
      } catch (error) {
        state.profilePhase = 'error'
        state.profileError = fail(error)
        return null
      } finally {
        profileInflight = null
        end()
      }
    })()
    return profileInflight
  }

  async function loadDeviceInputs(force = false): Promise<DeviceInputsCapability | null> {
    if (!force && state.deviceInputs !== null) {
      return state.deviceInputs
    }
    if (inputsInflight !== null) {
      return inputsInflight
    }
    if (!begin('inputs')) {
      return null
    }
    state.deviceInputsPhase = 'loading'
    inputsInflight = (async () => {
      try {
        const inputs = await deps.api.fetchDeviceInputs()
        state.deviceInputs = inputs
        state.deviceInputsPhase = 'ready'
        state.deviceInputsError = null
        return inputs
      } catch (error) {
        state.deviceInputsPhase = 'error'
        state.deviceInputsError = fail(error)
        return null
      } finally {
        inputsInflight = null
        end()
      }
    })()
    return inputsInflight
  }

  async function prepare(profile: AudioProfile): Promise<AudioProfileDraft | null> {
    if (!begin('prepare')) {
      return null
    }
    try {
      const draft = await deps.api.prepareProfile(profile)
      state.draft = draft
      return draft
    } catch (error) {
      // prepare 失败不保留旧草稿（避免把上一次意图当成这一次提交）。
      state.draft = null
      fail(error)
      return null
    } finally {
      end()
    }
  }

  async function commit(): Promise<AudioProfileCommitAck | null> {
    const draft = state.draft
    if (draft === null || !begin('commit')) {
      return null
    }
    try {
      const ack = await deps.api.commitProfile(draft.token)
      state.draft = null
      state.lastCommit = ack
      state.lastCommitAt = now()
      if (ack.profile !== null) {
        state.snapshot = state.snapshot === null ? null : { ...state.snapshot, profile: ack.profile }
      }
      return ack
    } catch (error) {
      const info = fail(error)
      if (info.category === 'token_invalid') {
        state.draft = null
      }
      return null
    } finally {
      end()
    }
  }

  async function abort(): Promise<boolean> {
    const draft = state.draft
    if (draft === null || !begin('abort')) {
      return false
    }
    try {
      const ack = await deps.api.abortProfile(draft.token)
      state.draft = null
      return ack.aborted
    } catch (error) {
      fail(error)
      return false
    } finally {
      end()
    }
  }

  async function apply(): Promise<ApplyOutcome> {
    const steps: ApplyStep[] = []
    const outcome: ApplyOutcome = { ok: false, steps, plan: null, planDraft: null, commit: null, error: null }
    state.apply = outcome
    if (!begin('apply')) {
      outcome.error = state.error
      return outcome
    }
    try {
      // 1) 生成方案（不自动提交）
      try {
        outcome.plan = await deps.api.generatePlan()
        steps.push({ step: 'plan', ok: true, detail: null, at: now() })
      } catch (error) {
        const info = fail(error)
        steps.push({ step: 'plan', ok: false, detail: info.code, at: now() })
        outcome.error = info
        return outcome
      }
      const plan = outcome.plan
      // 2) inspect 只读预览
      try {
        await deps.api.inspectPlan(plan.plan_path, plan.sha256)
        steps.push({ step: 'inspect', ok: true, detail: null, at: now() })
      } catch (error) {
        const info = fail(error)
        steps.push({ step: 'inspect', ok: false, detail: info.code, at: now() })
        outcome.error = info
        return outcome
      }
      // 3) prepare 方案草稿
      try {
        outcome.planDraft = await deps.api.preparePlan(plan.plan_path, plan.sha256)
        steps.push({ step: 'prepare', ok: true, detail: null, at: now() })
      } catch (error) {
        const info = fail(error)
        steps.push({ step: 'prepare', ok: false, detail: info.code, at: now() })
        outcome.error = info
        return outcome
      }
      // 4) commit 生效
      try {
        outcome.commit = await deps.api.commitPlan(outcome.planDraft.engine_token)
        steps.push({ step: 'commit', ok: true, detail: null, at: now() })
      } catch (error) {
        const info = fail(error)
        steps.push({ step: 'commit', ok: false, detail: info.code, at: now() })
        outcome.error = info
        return outcome
      }
      outcome.ok = true
      state.apply = outcome
      // 生效后刷新 profile 快照（plan lanes/graph_id 已变化）；失败不污染 apply 结果。
      void loadProfile(true)
      return outcome
    } finally {
      end()
    }
  }

  async function abortApply(): Promise<boolean> {
    const draft = state.apply?.planDraft ?? null
    if (draft === null) {
      return false
    }
    try {
      const ack = await deps.api.abortPlan(draft.engine_token)
      if (state.apply !== null) {
        state.apply = { ...state.apply, planDraft: null }
      }
      return ack.aborted
    } catch (error) {
      fail(error)
      return false
    }
  }

  async function startTest(request: SpeakerTestRequest): Promise<SpeakerTestAck | null> {
    const invalid = validateSpeakerTestRequest(request)
    if (invalid !== null) {
      state.error = {
        category: 'validation',
        status: 0,
        code: 'speaker_test_invalid_' + invalid,
        i18nKey: 'speakerLayout.error.validation',
        detail: invalid,
        reason: null,
        retryable: false
      }
      return null
    }
    if (!begin('test')) {
      return null
    }
    try {
      const ack = await deps.api.startSpeakerTest(request)
      state.lastTest = ack
      state.test = {
        active: true,
        dac: ack.dac,
        ms: ack.ms,
        kind: ack.kind,
        levelDb: ack.level_db ?? request.db,
        startedAt: now(),
        serverRemainingMs: ack.remaining_ms
      }
      return ack
    } catch (error) {
      fail(error)
      return null
    } finally {
      end()
    }
  }

  async function stopTest(): Promise<boolean> {
    if (!begin('stop')) {
      return false
    }
    try {
      const ack = await deps.api.stopSpeakerTest()
      state.test = null
      return ack.stopped
    } catch (error) {
      fail(error)
      return false
    } finally {
      end()
    }
  }

  return {
    loadProfile,
    loadDeviceInputs,
    prepare,
    commit,
    abort,
    apply,
    abortApply,
    startTest,
    stopTest,
    clearTest(): void {
      state.test = null
    },
    reset(): void {
      Object.assign(state, createInitialAudioProfileState())
    }
  }
}

/* ---------------------------------------------------------------- 单例装配 */

export const audioProfileState = reactive<AudioProfileState>(createInitialAudioProfileState())

const controller = createAudioProfileController(audioProfileState, { api: createAudioProfileApi(apiClient) })

export function loadAudioProfile(force = false): Promise<AudioProfileSnapshot | null> {
  return controller.loadProfile(force)
}

export function loadDeviceInputs(force = false): Promise<DeviceInputsCapability | null> {
  return controller.loadDeviceInputs(force)
}

export function prepareAudioProfile(profile: AudioProfile): Promise<AudioProfileDraft | null> {
  return controller.prepare(profile)
}

export function commitAudioProfile(): Promise<AudioProfileCommitAck | null> {
  return controller.commit()
}

export function abortAudioProfile(): Promise<boolean> {
  return controller.abort()
}

export function applyAudioProfile(): Promise<ApplyOutcome> {
  return controller.apply()
}

export function abortAudioProfileApply(): Promise<boolean> {
  return controller.abortApply()
}

export function startSpeakerTest(request: SpeakerTestRequest): Promise<SpeakerTestAck | null> {
  return controller.startTest(request)
}

export function stopSpeakerTest(): Promise<boolean> {
  return controller.stopTest()
}

export function clearSpeakerTestState(): void {
  controller.clearTest()
}

export function resetAudioProfileState(): void {
  controller.reset()
}

/** 测试音剩余毫秒（纯函数：页面倒计时用；active=false 返回 0）。 */
export function speakerTestRemainingMs(test: SpeakerTestState | null, nowMs: number): number {
  if (test === null || !test.active) {
    return 0
  }
  const total = test.serverRemainingMs ?? test.ms
  return Math.max(0, Math.min(test.ms, total - (nowMs - test.startedAt)))
}

/** 输入源展示辅助：kind -> 稳定标签（页面走 i18n）。 */
export function sourceKindLabelKey(kind: AudioSourceKind): 'speakerLayout.source.aes67' | 'speakerLayout.source.device' {
  return kind === 'aes67' ? 'speakerLayout.source.aes67' : 'speakerLayout.source.device'
}

/** profile schema 常量再导出（页面/测试共用）。 */
export { AUDIO_PROFILE_SCHEMA }

export { controller as audioProfileController }