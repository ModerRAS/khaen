/**
 * i18n 分片：draftReview（en-US）—— 并行开发专用，避免多人同时改巨型 i18n 文件。
 *
 * 规则（scripts/i18n-check.mjs 强制）：
 * - 本分片只放本区域新增的键；不得重复定义 src/i18n/zh-CN.ts 基础文件已有的键；
 * - zh-CN / en-US 两个分片必须键集完全对等（成对提交）；
 * - 顶层命名空间由本区域独占：draftReview.*（其它分片/基础文件不得再定义该顶层键）。
 */

const messages = {
  draftReview: {
    title: "Two-phase draft (prepare)",
    desc: "prepare only stages a draft; nothing changes until commit applies at a block boundary, and abort discards it.",
    surface: "Surface",
    changed: "Changed",
    changedYes: "Changed",
    changedNo: "Unchanged",
    notReported: "Not reported by the server",
    plannedVersion: "Planned version",
    graphId: "Graph ID",
    mode: "Target mode",
    format: "Target format",
    deviceRestart: "Device PCM restart required",
    appliesTo: "Applies to",
    note: "Note",
    admission: "Admission",
    blockers: "Blockers",
    diff: "Diff",
    current: "Current",
    target: "Target",
    graph: "Compiled graph",
    engineToken: "Draft token (single use)",
    commit: "Commit",
    abort: "Abort draft",
    changedSourceHint: "changed/diff/current/target are shown only when the server reports them; nothing is invented."
  }
}

export default messages
