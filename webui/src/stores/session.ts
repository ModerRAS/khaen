/**
 * 会话 store（PLAN §4.2 / §5.2 / §6.1）。
 *
 * 职责：
 * - 状态：configured（服务端是否已设密码）、authenticated、csrfToken、expiresAt、checked、busy、lastError；
 * - ensureSession()：初始化时 GET /api/auth/status；若 status.authenticated 再 GET /api/auth/session
 *   拿 csrf / expires_at（并发调用去重、结果缓存，force=true 强制重查）；
 * - login/setup：成功后保存 csrf 与过期时间，写操作由 ApiClient 自动带 X-KHAEN-CSRF；
 * - logout：POST /api/auth/logout（失败也清本地态，避免"假在线"），检查标记复位以便下次重新探测；
 * - handleUnauthorized：任意受保护 API 的 401 -> 清态 + 通过 onUnauthorized 回调跳登录（只通知一次）。
 *
 * 可测试性：createSessionController(state, deps) 是纯逻辑工厂（不依赖路由/DOM）；
 * 模块底部用 reactive state + 共享 ApiClient 组装单例，供 Vue 组件与路由守卫使用。
 */
import { reactive } from 'vue'
import { ApiClient, DEFAULT_API_VERSION, isApiError } from '../api/client.ts'
import { createAuthApi, type AuthApi, type AuthTokenResponse } from '../api/auth.ts'
import { classifyAuthError, type AuthErrorCode } from '../api/errors.ts'
import type { SessionStatus } from '../types/auth.ts'

export type { SessionStatus }

export interface SessionState {
  /** 会话判定：unknown = 尚未/无法判定（后端不可达时不得当成已登录）。 */
  status: SessionStatus
  /** 是否已认证（status 的布尔镜像，组件模板直接绑定；只由本模块写入）。 */
  authenticated: boolean
  /** 服务端是否已配置管理员密码：null = 未知（后端不可达/未查询）。 */
  configured: boolean | null
  /** 写请求所需 CSRF token：null = 未登录或未知。 */
  csrfToken: string | null
  /** 会话过期时间（RFC3339，来自 /api/auth/session；登录响应按 expires_in 推算）。 */
  expiresAt: string | null
  /** 是否已完成过一次服务端探测。 */
  checked: boolean
  /** 登录/登出等认证请求进行中（按钮 loading/禁用）。 */
  busy: boolean
  /** 最近一次认证错误（稳定 code，如 invalid_credentials / rate_limited / network_error）。 */
  lastError: string | null
}

export function createInitialSessionState(): SessionState {
  return {
    status: 'unknown',
    authenticated: false,
    configured: null,
    csrfToken: null,
    expiresAt: null,
    checked: false,
    busy: false,
    lastError: null
  }
}

/** 认证动作失败：error 原样带出（LoginView 用 classifyAuthError 映射文案）。 */
export interface AuthActionFailure {
  ok: false
  error: unknown
  /** 归一化错误类别（来自 classifyAuthError）。 */
  code: AuthErrorCode
  /** 主文案 i18n 键（来自 classifyAuthError）。 */
  i18nKey: string
}

export type AuthActionResult = { ok: true } | AuthActionFailure

export interface SessionDeps {
  /** 认证端点（单测注入假实现）。 */
  auth: Pick<AuthApi, 'status' | 'session' | 'login' | 'setup' | 'logout'>
  /** 401 清态后跳登录的回调（由 router 注册；登录页自跳由路由守卫负责）。 */
  onUnauthorized?: () => void
  /** 可注入时钟（测过期/通知去重，缺省 Date.now）。 */
  now?: () => number
}

export interface SessionController {
  ensureSession(options?: { force?: boolean }): Promise<SessionStatus>
  login(password: string): Promise<AuthActionResult>
  setup(password: string, confirm: string): Promise<AuthActionResult>
  logout(): Promise<void>
  handleUnauthorized(): void
  clear(): void
  isExpired(): boolean
}

/** 纯逻辑会话控制器：所有状态写入都走 state（Vue reactive 或普通对象都可）。 */
export function createSessionController(state: SessionState, deps: SessionDeps): SessionController {
  const now = deps.now ?? (() => Date.now())
  let inflight: Promise<SessionStatus> | null = null
  let unauthorizedNotified = false

  function setUnauthenticated(lastError: string | null): void {
    state.status = 'unauthenticated'
    state.authenticated = false
    state.csrfToken = null
    state.expiresAt = null
    state.lastError = lastError
  }

  function setAuthenticated(csrf: string, expiresAt: string | null): void {
    state.status = 'authenticated'
    state.authenticated = true
    state.csrfToken = csrf
    state.expiresAt = expiresAt
    state.lastError = null
    unauthorizedNotified = false
  }

  function expiresAtFrom(expiresIn: number): string | null {
    if (!Number.isFinite(expiresIn) || expiresIn <= 0) {
      return null
    }
    return new Date(now() + expiresIn * 1000).toISOString()
  }

  function errorCode(error: unknown): string {
    return isApiError(error) ? error.code : 'unknown_error'
  }

  function isExpired(): boolean {
    if (state.expiresAt === null) {
      return false
    }
    const timestamp = Date.parse(state.expiresAt)
    if (!Number.isFinite(timestamp)) {
      return false
    }
    return timestamp <= now()
  }

  async function probe(): Promise<SessionStatus> {
    try {
      const status = await deps.auth.status()
      state.configured = status.configured
      if (!status.authenticated) {
        setUnauthenticated(null)
        return state.status
      }
      try {
        const session = await deps.auth.session()
        setAuthenticated(session.csrf, session.expires_at)
      } catch (error) {
        if (isApiError(error) && error.status === 401) {
          // status 说已登录但 session 401：会话恰好过期/被清除，按未登录处理。
          setUnauthenticated('session_expired')
        } else {
          state.status = 'unknown'
          state.authenticated = false
          state.configured = null
          state.csrfToken = null
          state.expiresAt = null
          state.lastError = errorCode(error)
        }
      }
      return state.status
    } catch (error) {
      // 后端不可达：保持 unknown + 记录错误码，由登录页展示"不可达 + 重试"，绝不冒充已登录。
      state.status = 'unknown'
      state.authenticated = false
      state.configured = null
      state.csrfToken = null
      state.expiresAt = null
      state.lastError = errorCode(error)
      return state.status
    } finally {
      state.checked = true
    }
  }

  async function ensureSession(options: { force?: boolean } = {}): Promise<SessionStatus> {
    if (inflight !== null) {
      return inflight
    }
    const force = options.force === true
    if (!force && state.checked && !(state.status === 'authenticated' && isExpired())) {
      return state.status
    }
    const started = probe().then(
      (status) => {
        inflight = null
        return status
      },
      (error: unknown) => {
        inflight = null
        state.checked = true
        state.status = 'unknown'
        state.authenticated = false
        state.configured = null
        state.lastError = errorCode(error)
        return state.status
      }
    )
    inflight = started
    return started
  }

  async function runAuth(
    operation: () => Promise<AuthTokenResponse>,
    context: 'login' | 'setup',
    markConfigured: boolean
  ): Promise<AuthActionResult> {
    state.busy = true
    state.lastError = null
    try {
      const token = await operation()
      setAuthenticated(token.csrf, expiresAtFrom(token.expires_in))
      if (markConfigured) {
        state.configured = true
      }
      state.checked = true
      return { ok: true }
    } catch (error) {
      const info = classifyAuthError(error, context)
      state.lastError = info.code
      if (info.code === 'already_configured') {
        state.configured = true
      }
      if (info.code === 'not_configured') {
        state.configured = false
      }
      return { ok: false, error, code: info.code, i18nKey: info.i18nKey }
    } finally {
      state.busy = false
    }
  }

  async function logout(): Promise<void> {
    state.busy = true
    try {
      await deps.auth.logout()
    } catch {
      // 登出请求失败也要清本地态：宁可多一次登录，也不显示"假在线"。
    } finally {
      state.busy = false
      setUnauthenticated(null)
      state.checked = false
      unauthorizedNotified = false
    }
  }

  function handleUnauthorized(): void {
    const alreadyHandled = unauthorizedNotified
    setUnauthenticated('session_expired')
    state.checked = true
    if (alreadyHandled) {
      return
    }
    unauthorizedNotified = true
    if (deps.onUnauthorized) {
      deps.onUnauthorized()
    }
  }

  function clear(): void {
    setUnauthenticated(null)
    state.checked = false
    state.busy = false
    unauthorizedNotified = false
  }

  return {
    ensureSession,
    login: (password: string) => runAuth(() => deps.auth.login(password), 'login', true),
    setup: (password: string, confirm: string) => runAuth(() => deps.auth.setup(password, confirm), 'setup', true),
    logout,
    handleUnauthorized,
    clear,
    isExpired
  }
}

/* ---------------------------------------------------------------- 单例装配 */

export const sessionState = reactive<SessionState>(createInitialSessionState())

let unauthorizedHandler: (() => void) | null = null

/** router 初始化时注册：401 清态后跳 /login（保留原路径 + 过期提示）。 */
export function setUnauthorizedHandler(handler: (() => void) | null): void {
  unauthorizedHandler = handler
}

const apiClient = new ApiClient({
  csrfToken: () => sessionState.csrfToken,
  onUnauthorized: () => {
    controller.handleUnauthorized()
  },
  onVersionMismatch: (serverVersion: string) => {
    console.warn('[api] daemon API version mismatch: expected ' + DEFAULT_API_VERSION + ', got ' + serverVersion)
  }
})

const controller = createSessionController(sessionState, {
  auth: createAuthApi(apiClient),
  onUnauthorized: () => {
    if (unauthorizedHandler !== null) {
      unauthorizedHandler()
    }
  }
})

/** 供后续模块（M2 状态页等）复用的同源 REST 客户端。 */
export { apiClient }

export function ensureSession(options?: { force?: boolean }): Promise<SessionStatus> {
  return controller.ensureSession(options)
}

export function login(password: string): Promise<AuthActionResult> {
  return controller.login(password)
}

export function setup(password: string, confirm: string): Promise<AuthActionResult> {
  return controller.setup(password, confirm)
}

export function logout(): Promise<void> {
  return controller.logout()
}

export function isSessionExpired(): boolean {
  return controller.isExpired()
}
