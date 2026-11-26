/**
 * i18n 分片：meta（zh-CN）—— 并行开发专用，避免多人同时改巨型 i18n 文件。
 *
 * 规则（scripts/i18n-check.mjs 强制）：
 * - 本分片只放本区域新增的键；不得重复定义 src/i18n/zh-CN.ts 基础文件已有的键；
 * - zh-CN / en-US 两个分片必须键集完全对等（成对提交）；
 * - 顶层命名空间由本区域独占：meta.*（其它分片/基础文件不得再定义该顶层键）。
 */

const messages = {
  // 本区域命名空间：meta
  // meta: {
  //   title: '...'
  // }
}

export default messages
