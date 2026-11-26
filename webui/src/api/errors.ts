/**
 * 认证错误归类：把 HTTP 状态码 + 后端稳定 error.code 映射成
 *   - 前端归一化 code（供 UI 分支，如 already_configured 要切到登录态）；
 *   - i18n 键（主文案；后端 message 只作为详情，不当主文案，PLAN §12.2）。
 *
 * 纯函数、无 Vue / DOM 依赖，Node 单测可直接覆盖。
 */
import { isApiError } from './client.ts'

/** 调用场景：同一 401 在登录/改密语境下含义不同。 */
export type AuthErrorContext = 'login' | 'setup' | 'changePassword' | 'generic'

/** 前端归一化后的认证错误类别（UI 只依赖这些稳定字符串）。 */
export type AuthErrorCode =
  | 'invalid_credentials'
  | 'invalid_current_password'
  | 'rate_limited'
  | 'already_configured'
  | 'not_configured'
  | 'csrf_invalid'
  | 'validation'
  | 'unauthenticated'
  | 'session_expired'
  | 'network'
  | 'server'
  | 'unknown'

/** i18n 键表：每个归一化 code 一份两语言对等的文案（i18n-check 会校验键存在）。 */
const I18N_KEYS: Record<AuthErrorCode, string> = {
  invalid_credentials: 'error.auth.invalidCredentials',
  invalid_current_password: 'error.auth.invalidCurrentPassword',
  rate_limited: 'error.auth.rateLimited',
  already_configured: 'error.auth.alreadyConfigured',
  not_configured: 'error.auth.notConfigured',
  csrf_invalid: 'error.auth.csrfInvalid',
  validation: 'error.auth.validation',
  unauthenticated: 'error.auth.unauthenticated',
  session_expired: 'error.auth.sessionExpired',
  network: 'error.auth.network',
  server: 'error.auth.server',
  unknown: 'error.auth.unknown'
}

/** 后端稳定码 -> 归一化类别（后端码优先于状态码）。 */
const CODE_MAP: Record<string, AuthErrorCode> = {
  invalid_credentials: 'invalid_credentials',
  bad_credentials: 'invalid_credentials',
  wrong_password: 'invalid_credentials',
  invalid_password: 'validation',
  password_mismatch: 'validation',
  invalid_request: 'validation',
  validation_failed: 'validation',
  already_configured: 'already_configured',
  not_configured: 'not_configured',
  rate_limited: 'rate_limited',
  csrf_invalid: 'csrf_invalid',
  csrf_missing: 'csrf_invalid',
  invalid_csrf: 'csrf_invalid',
  session_expired: 'session_expired',
  not_authenticated: 'unauthenticated',
  unauthorized: 'unauthenticated',
  missing_session: 'unauthenticated',
  invalid_response: 'server',
  network_error: 'network',
  request_timeout: 'network',
  internal_error: 'server',
  server_error: 'server'
}

export interface AuthErrorInfo {
  /** 归一化类别（UI 分支用）。 */
  code: AuthErrorCode
  /** 主文案 i18n 键（两种语言都有）。 */
  i18nKey: string
  /** HTTP 状态码；0 = 网络层失败。 */
  status: number
  /** 后端原始 message（仅详情展示，不直接当主文案）。 */
  detail?: string
  /** 429 时的 Retry-After 秒数。 */
  retryAfterSeconds?: number
  /** 后端原始稳定码（便于日志；没有则为 undefined）。 */
  rawCode?: string
}

function build(code: AuthErrorCode, status: number, detail?: string, retryAfterSeconds?: number, rawCode?: string): AuthErrorInfo {
  const info: AuthErrorInfo = { code, i18nKey: I18N_KEYS[code], status }
  if (detail !== undefined && detail.length > 0) {
    info.detail = detail
  }
  if (retryAfterSeconds !== undefined) {
    info.retryAfterSeconds = retryAfterSeconds
  }
  if (rawCode !== undefined) {
    info.rawCode = rawCode
  }
  return info
}

/** 状态码归类（后端没有给稳定码、或稳定码不认识时的兜底）。 */
function fromStatus(status: number, context: AuthErrorContext): AuthErrorCode {
  if (status === 0) {
    return 'network'
  }
  if (status === 429) {
    return 'rate_limited'
  }
  if (status === 409) {
    return 'already_configured'
  }
  if (status === 403) {
    return 'csrf_invalid'
  }
  if (status === 401) {
    if (context === 'changePassword') {
      return 'invalid_current_password'
    }
    if (context === 'login') {
      return 'invalid_credentials'
    }
    if (context === 'setup') {
      return 'validation'
    }
    return 'unauthenticated'
  }
  if (status === 400 || status === 422) {
    return 'validation'
  }
  if (status >= 500) {
    return 'server'
  }
  return 'unknown'
}

/**
 * 归类一次登录/初始化/改密/通用请求错误。
 * 传入非 ApiError（例如代码 bug）时归为 unknown，detail 只保留 message。
 */
export function classifyAuthError(error: unknown, context: AuthErrorContext = 'generic'): AuthErrorInfo {
  if (!isApiError(error)) {
    const detail = error instanceof Error ? error.message : undefined
    return build('unknown', 0, detail)
  }

  const mapped = CODE_MAP[error.code]
  let code = mapped ?? fromStatus(error.status, context)

  // 401 在改密语境下表示旧密码错误，而不是"无效凭据/会话失效"。
  if (code === 'invalid_credentials' && context === 'changePassword') {
    code = 'invalid_current_password'
  }
  // 通用语境下的 401 说明会话失效，而不是"密码猜错"。
  if (code === 'invalid_credentials' && context === 'generic') {
    code = 'unauthenticated'
  }

  const retryAfterSeconds = error.retryAfterSeconds === null ? undefined : error.retryAfterSeconds
  return build(code, error.status, error.message, retryAfterSeconds, error.code)
}
