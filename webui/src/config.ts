/**
 * 构建期注入的只读常量（vite.config.ts define）与产品级常量。
 *
 * 构建信息**不含时间戳**：dist 入库，确定性构建才能保证"重新 build 后 git diff 为空"。
 * 发布时用 KHAEN_WEBUI_BUILD_ID 注入版本标识（缺省 'local'）。
 */
export const APP_VERSION: string = __APP_VERSION__
export const APP_BUILD_ID: string = __APP_BUILD_ID__

/** 管理员密码最小长度（PLAN §6.1：至少 8 个字符；后端 M1 复核）。 */
export const MIN_PASSWORD_LENGTH = 8

// 概览轮询间隔与 stale 阈值属于状态模块：见 src/stores/status.ts（STATUS_POLL_INTERVAL_MS）
// 与 src/api/status.ts（STALE_STATUS_AGE_MS），避免构建期常量与纯逻辑模块耦合。
