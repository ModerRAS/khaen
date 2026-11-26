/**
 * 登录 / 初始化表单的本地校验（与后端规则一致：密码最少 N 个字符、初始化两次一致）。
 *
 * 纯函数：返回通过结果或 i18n 键，界面只负责展示，便于 Node 单测覆盖边界。
 * 真正的权威校验仍在后端；这里只做提交前的快速反馈，不伪造成功。
 */
import type { AuthMode } from '../types/auth.ts'

/** 本地校验失败的文案键（zh-CN / en-US 对等）。 */
export type LoginValidationKey =
  | 'login.validation.required'
  | 'login.validation.tooShort'
  | 'login.validation.mismatch'

export type LoginValidationResult = { ok: true } | { ok: false; key: LoginValidationKey }

/**
 * @param mode    setup = 首次设密码（含确认）；signin = 登录（仅密码）
 * @param password 密码原文（不 trim：空格也是合法字符，由后端最终裁决）
 * @param confirm  再次输入的密码（仅 setup 使用）
 * @param minPasswordLength 最小长度（来自 config.MIN_PASSWORD_LENGTH；后端复核）
 */
export function validateCredentials(
  mode: AuthMode,
  password: string,
  confirm: string,
  minPasswordLength: number
): LoginValidationResult {
  if (password.length === 0) {
    return { ok: false, key: 'login.validation.required' }
  }
  if (mode === 'setup') {
    const min = Number.isFinite(minPasswordLength) && minPasswordLength > 0 ? Math.floor(minPasswordLength) : 1
    if (password.length < min) {
      return { ok: false, key: 'login.validation.tooShort' }
    }
    if (confirm !== password) {
      return { ok: false, key: 'login.validation.mismatch' }
    }
  }
  return { ok: true }
}
