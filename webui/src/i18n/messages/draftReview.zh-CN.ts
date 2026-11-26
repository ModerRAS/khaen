/**
 * i18n 分片：draftReview（zh-CN）—— 并行开发专用，避免多人同时改巨型 i18n 文件。
 *
 * 规则（scripts/i18n-check.mjs 强制）：
 * - 本分片只放本区域新增的键；不得重复定义 src/i18n/zh-CN.ts 基础文件已有的键；
 * - zh-CN / en-US 两个分片必须键集完全对等（成对提交）；
 * - 顶层命名空间由本区域独占：draftReview.*（其它分片/基础文件不得再定义该顶层键）。
 */

const messages = {
  draftReview: {
    title: "两阶段草稿（prepare）",
    desc: "prepare 只生成草稿，不改变运行态；commit 才在块边界生效，abort 丢弃草稿。",
    surface: "界面",
    changed: "是否有变化",
    changedYes: "有变化",
    changedNo: "无变化",
    notReported: "服务端未上报",
    plannedVersion: "计划版本",
    graphId: "方案图 ID",
    mode: "目标模式",
    format: "目标格式",
    deviceRestart: "设备需重启 PCM 流",
    appliesTo: "生效范围",
    note: "说明",
    admission: "准入",
    blockers: "阻塞项",
    diff: "差异（diff）",
    current: "当前值（current）",
    target: "目标值（target）",
    graph: "图编译（graph）",
    engineToken: "草稿令牌（只能消费一次）",
    commit: "提交生效",
    abort: "放弃草稿",
    changedSourceHint: "changed/diff/current/target 仅在后端上报时显示；未上报不臆造。"
  }
}

export default messages
