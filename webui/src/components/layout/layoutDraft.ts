/**
 * Layout 页纯逻辑（无 Vue / DOM 依赖；Node 单测直接覆盖）。
 *
 * 契约来源（docs/API.md v1.1 / CONTRACTS B.1.2）：
 * - POST /api/surface {surface:"layout", data:{in_channels, out_lanes, folds:[[src,dst,gain],...]}}
 *   是**完整替换表**（prepare 只生成草稿，commit 才在块边界生效，abort 丢弃）；
 * - 没有已生效方案时后端返回 409 no_base_plan（不允许对着空基线打补丁）；
 * - 当前布局内容没有读回数据源（§9.7）：表单只是**本地草稿**，默认值不是当前值；
 * - 空表/越界由后端拒绝；前端复用 api/control.ts 的 validateLayoutPatch 只做形状校验。
 */
import {
  buildLayoutPatch,
  validateLayoutPatch,
  type LayoutPatch
} from '../../api/control.ts'
import type { ControlCommitAck, ControlDraft } from '../../api/control.ts'
import type { CapabilityState } from '../../api/meta.ts'

/** 后端 in_channels / out_lanes / fold src|dst 字段是 u16（超过即 400 bad_request）。 */
export const LAYOUT_U16_MAX = 65535

/** 单条折叠项草稿：[src, dst, gain]；输入框清空时为 null（本地校验报 entry）。 */
export interface LayoutFoldDraft {
  src: number | null
  dst: number | null
  gain: number | null
}

/** 内容布局表单草稿（全部是本地值，不是当前布局）。 */
export interface LayoutDraft {
  inChannels: number | null
  outLanes: number | null
  folds: LayoutFoldDraft[]
}

/** 新建一条折叠项（默认 src=0 / dst=0 / gain=1，仅作编辑起点）。 */
export function createFoldDraft(): LayoutFoldDraft {
  return { src: 0, dst: 0, gain: 1 }
}

/**
 * 本地草稿默认值：明确是**默认编辑起点**，不是当前布局（当前值无数据源，页面必须标注）。
 * 2 in / 2 lanes / 恒等折叠，与 demo 示例一致，便于用户先看到一条合法形状。
 */
export function createLayoutDraft(): LayoutDraft {
  return { inChannels: 2, outLanes: 2, folds: [createFoldDraft()] }
}

/** 追加一条折叠项（直接修改传入的响应式草稿）。 */
export function addFoldRow(draft: LayoutDraft): void {
  draft.folds.push(createFoldDraft())
}

/** 删除指定行；允许删到 0 行（随后本地校验给出 empty 错误，用户可感知并补回）。 */
export function removeFoldRow(draft: LayoutDraft, index: number): boolean {
  if (!Number.isInteger(index) || index < 0 || index >= draft.folds.length) {
    return false
  }
  draft.folds.splice(index, 1)
  return true
}

/** 输入框归一化：有限数取整，否则 NaN（交给 validateLayoutPatch 报 entry/channels）。 */
function toInteger(value: number | null): number {
  if (value === null || typeof value !== 'number' || !Number.isFinite(value)) {
    return Number.NaN
  }
  return Math.trunc(value)
}

/** 增益原样传递：validateLayoutPatch 只要求有限数（非有限 → entry，不静默改写）。 */
function toGain(value: number | null): number {
  if (value === null || typeof value !== 'number') {
    return Number.NaN
  }
  return value
}

/**
 * 本地草稿 → surface 补丁（形状与 docs/API.md §4 layout.data 完全一致；
 * prepare 只在 validateLayoutDraft() === null 时调用，因此不会把 NaN 发到后端）。
 */
export function buildLayoutPatchFromDraft(draft: LayoutDraft): LayoutPatch {
  return buildLayoutPatch(
    toInteger(draft.inChannels),
    toInteger(draft.outLanes),
    draft.folds.map((fold) => [toInteger(fold.src), toInteger(fold.dst), toGain(fold.gain)] as [number, number, number])
  )
}

/**
 * 本地形状校验：直接复用 api/control.ts 的 validateLayoutPatch；
 * 返回前端错误码（channels / empty / entry）或 null（合法）。
 */
export function validateLayoutDraft(draft: LayoutDraft): string | null {
  return validateLayoutPatch(buildLayoutPatchFromDraft(draft))
}

const LAYOUT_VALIDATION_KEYS: Record<string, string> = {
  channels: 'layout.validation.channels',
  empty: 'layout.validation.empty',
  entry: 'layout.validation.entry'
}

/** 校验错误码 → 文案 i18n 键；null / 未知码返回 null（不显示假文案）。 */
export function layoutValidationKey(code: string | null): string | null {
  if (code === null) {
    return null
  }
  return LAYOUT_VALIDATION_KEYS[code] ?? null
}

/** 能力三态 → 文案 i18n 键（unknown 不是 wired，页面必须如实标注）。 */
export function capabilityStateKey(state: CapabilityState): string {
  if (state === 'wired') {
    return 'layout.capability.wired'
  }
  if (state === 'not_wired') {
    return 'layout.capability.notWired'
  }
  return 'layout.capability.unknown'
}

/** 阻止 prepare 的原因（顺序：界面未接通 > 安全/故障写锁 > 控制面忙 > 本地校验失败）。 */
export type LayoutWriteBlock = 'capability' | 'locked' | 'busy' | 'invalid'

/** 阻止原因 → 提示文案 i18n 键。 */
export const LAYOUT_WRITE_BLOCK_KEYS: Record<LayoutWriteBlock, string> = {
  capability: 'layout.block.capability',
  locked: 'layout.block.locked',
  busy: 'layout.block.busy',
  invalid: 'layout.block.invalid'
}

export interface LayoutWriteInput {
  /** GET /api/meta 上报的 layout 界面能力三态。 */
  surface: CapabilityState
  safe: boolean
  faulted: boolean
  busy: boolean
  /** 本地形状校验结果（validateLayoutDraft）。 */
  validation: string | null
}

/** 返回第一个阻止 prepare 的原因；null = 可以 prepare。 */
export function layoutWriteBlock(input: LayoutWriteInput): LayoutWriteBlock | null {
  if (input.surface === 'not_wired') {
    return 'capability'
  }
  if (input.safe || input.faulted) {
    return 'locked'
  }
  if (input.busy) {
    return 'busy'
  }
  if (input.validation !== null) {
    return 'invalid'
  }
  return null
}

/**
 * 已准备草稿的 commit 是否禁止：安全/故障态（只读）或界面未接通时禁止改变运行态；
 * abort（丢弃草稿）不受此限，必须保持可达。
 */
export function layoutCommitBlocked(
  input: Pick<LayoutWriteInput, 'surface' | 'safe' | 'faulted'>
): boolean {
  return input.surface === 'not_wired' || input.safe || input.faulted
}

/** 从共享控制面草稿里挑出 layout 草稿（其它界面的草稿不属于本页）。 */
export function layoutDraftOf(draft: ControlDraft | null): ControlDraft | null {
  return draft !== null && draft.surface === 'layout' ? draft : null
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

/**
 * 可选回执字段（diff/current/target）→ 可展示文本；null/undefined = 服务端未上报。
 * 只用 JSON 序列化，不推断字段含义。
 */
export function optionalReceiptText(value: unknown): string | null {
  if (value === null || value === undefined) {
    return null
  }
  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return String(value)
  }
}

/** changed 三态 → 文案 i18n 键（复用 draftReview 分片，zh/en 对等）。 */
export function changedStateKey(changed: boolean | null): string {
  if (changed === true) {
    return 'draftReview.changedYes'
  }
  if (changed === false) {
    return 'draftReview.changedNo'
  }
  return 'draftReview.notReported'
}

/** commit 回执 changed=false = 本次提交为空操作（不应用新块）。 */
export function commitChangedNoop(ack: ControlCommitAck | null): boolean {
  return ack !== null && ack.changed === false
}

/**
 * /api/status 明确没有已生效方案版本（plan.version 为空）时返回 true：页面据此**提前**给出
 * 「先 prepare + commit 方案」的只读引导（真正的判定仍以后端 no_base_plan 为准）。
 */
export function statusShowsNoBasePlan(planVersion: number | null | undefined): boolean {
  return planVersion === null || planVersion === undefined
}