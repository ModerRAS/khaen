/**
 * Mode 页纯逻辑（无 Vue / DOM 依赖；Node 单测直接覆盖）。
 *
 * 契约来源（docs/API.md v1.1，唯一 API 合同）：
 * - §4 控制面两阶段：POST /api/surface {surface:"mode", data:{mode:"off"|"on"|"aes67"}} 只生成草稿
 *   （engine_token / planned_version / mode / changed）；POST /api/commit {engine_token} 在块边界生效；
 *   POST /api/abort {engine_token} 丢弃草稿；
 * - §9.1 能力：页面先读 GET /api/meta 的 surfaces / not_wired，未接通的界面显示原因、不渲染假操作；
 * - §9.7 当前值：/api/status 不含当前 mode，控制协议也没有读取 op —— 页面显示「不可读（无数据源）」，
 *   不得用默认值/上次提交值冒充当前值；
 * - CONTRACTS B.1.5：「on」是「aes67」的别名；其它输入源（toslink/dante/analog）未实现，后端 bad_request。
 *
 * 本文件只做"把契约字段映射成 UI 可分支的纯数据"，不碰网络、不读全局 store。
 */
import type { ControlCommitAck, ControlDraft, RunMode } from '../../api/control.ts'
import type { CapabilityState } from '../../api/meta.ts'

/** 目标模式候选：只含控制面真正接通的 off / aes67；未实现的输入源不渲染成可选项。 */
export interface ModeChoice {
  value: RunMode
  /** 选项文案 i18n 键（mode 分片）。 */
  labelKey: string
}

export const MODE_CHOICES: readonly ModeChoice[] = [
  { value: 'off', labelKey: 'mode.target.off' },
  { value: 'aes67', labelKey: 'mode.target.aes67' }
]

/** 是否为 UI 可提交的规范值（off / aes67）。 */
export function isSelectableMode(value: unknown): value is RunMode {
  return value === 'off' || value === 'aes67'
}

/**
 * 模式值归一化：后端/回执可能出现别名 "on" → 规范化为 "aes67"；
 * 未知值返回 null（页面显示原始字符串，不猜）。
 */
export function canonicalMode(raw: string | null | undefined): RunMode | null {
  if (typeof raw !== 'string') {
    return null
  }
  if (raw === 'off') {
    return 'off'
  }
  if (raw === 'aes67' || raw === 'on') {
    return 'aes67'
  }
  return null
}

/** 模式值 → 展示文案 i18n 键；未知/缺失返回 null（调用方回退原始值或「未上报」）。 */
export function modeLabelKey(raw: string | null | undefined): string | null {
  const mode = canonicalMode(raw)
  if (mode === 'off') {
    return 'mode.target.off'
  }
  if (mode === 'aes67') {
    return 'mode.target.aes67'
  }
  return null
}

/** 能力三态 → 文案 i18n 键（unknown 不是 wired，页面必须如实标注）。 */
export function capabilityStateKey(state: CapabilityState): string {
  if (state === 'wired') {
    return 'mode.capability.wired'
  }
  if (state === 'not_wired') {
    return 'mode.capability.notWired'
  }
  return 'mode.capability.unknown'
}

/** 阻止 prepare 的原因（顺序：界面未接通 > 安全/故障写锁 > 控制面忙 > 未选目标）。 */
export type ModeWriteBlock = 'capability' | 'locked' | 'busy' | 'no-target'

/** 阻止原因 → 提示文案 i18n 键。 */
export const MODE_WRITE_BLOCK_KEYS: Record<ModeWriteBlock, string> = {
  capability: 'mode.block.capability',
  locked: 'mode.block.locked',
  busy: 'mode.block.busy',
  'no-target': 'mode.block.noTarget'
}

export interface ModeWriteInput {
  /** GET /api/meta 上报的 mode 界面能力三态。 */
  surface: CapabilityState
  /** /api/status.state.safe === true。 */
  safe: boolean
  /** 运行状态为 faulted。 */
  faulted: boolean
  /** 控制面有操作在途（store.busy !== null）。 */
  busy: boolean
  /** 当前选择的目标模式。 */
  target: RunMode | null
}

/** 返回第一个阻止 prepare 的原因；null = 可以 prepare（commit/abort 由 DraftReview 的 busy 控制）。 */
export function modeWriteBlock(input: ModeWriteInput): ModeWriteBlock | null {
  if (input.surface === 'not_wired') {
    return 'capability'
  }
  if (input.safe || input.faulted) {
    return 'locked'
  }
  if (input.busy) {
    return 'busy'
  }
  if (input.target === null) {
    return 'no-target'
  }
  return null
}

/**
 * 已准备草稿的 commit 是否禁止：安全/故障态（只读）或界面未接通时禁止改变运行态；
 * abort（丢弃草稿）不受此限，必须保持可达。
 */
export function modeCommitBlocked(input: Pick<ModeWriteInput, 'surface' | 'safe' | 'faulted'>): boolean {
  return input.surface === 'not_wired' || input.safe || input.faulted
}

/** 从共享控制面草稿里挑出 mode 草稿（其它界面的草稿不属于本页）。 */
export function modeDraftOf(draft: ControlDraft | null): ControlDraft | null {
  if (draft === null) {
    return null
  }
  if (draft.surface === 'mode') {
    return draft
  }
  // parseControlDraft 在报错缺 surface 时用 mode 字段推断；这里保持同样的兜底口径。
  if (draft.surface === null && draft.mode !== null) {
    return draft
  }
  return null
}

/**
 * 控制面单 pending 槽里有其它界面的草稿时返回其 surface 名（prepare 会替换它）；
 * 没有草稿 / 草稿属于 expected 界面返回 null。
 */
export function foreignDraftSurface(draft: ControlDraft | null, expected: string): string | null {
  if (draft === null || draft.surface === null || draft.surface === expected) {
    return null
  }
  return draft.surface
}

/** prepare 回执 changed=false = 当前已是目标模式，commit 是空操作（不应用新块）。 */
export function draftChangedNoop(draft: ControlDraft | null): boolean {
  return draft !== null && draft.changed === false
}

/** commit 回执 changed=false = 本次提交为空操作。 */
export function commitChangedNoop(ack: ControlCommitAck | null): boolean {
  return ack !== null && ack.changed === false
}