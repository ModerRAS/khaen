// 会话状态：密码登录（Cookie 会话 + CSRF）；token 登录作为折叠的高级选项。
import { reactive } from 'vue'
import { api, ApiError, errorText, loadStoredToken, setApiToken, setCsrfToken } from '@/api/client'
import type { AuthSession, AuthStatus } from '@/api/types'

export interface SessionState {
  authStatus: AuthStatus | null
  session: AuthSession | null
  authenticated: boolean
  ready: boolean
  busy: boolean
  error: string | null
  errorCode: string
  token: string
}

export const sessionState = reactive<SessionState>({
  authStatus: null,
  session: null,
  authenticated: false,
  ready: false,
  busy: false,
  error: null,
  errorCode: '',
  token: loadStoredToken()
})

function setError(error: unknown): void {
  sessionState.error = error ? errorText(error) : null
  sessionState.errorCode = error instanceof ApiError ? error.code : ''
}

export function clearSessionError(): void {
  sessionState.error = null
  sessionState.errorCode = ''
}

export async function refreshAuthStatus(): Promise<AuthStatus | null> {
  try {
    sessionState.authStatus = await api.authStatus()
  } catch (error) {
    setError(error)
  }
  return sessionState.authStatus
}

/** 用现有 Cookie 会话做一次校验；401 视为未登录（不是错误）。 */
export async function verifySession(): Promise<boolean> {
  try {
    const current = await api.authSession()
    setCsrfToken(current.csrf_token)
    sessionState.session = current
    sessionState.authenticated = true
    setError(null)
    return true
  } catch (error) {
    setCsrfToken(null)
    sessionState.session = null
    sessionState.authenticated = false
    if (error instanceof ApiError && error.status === 401) {
      sessionState.error = null
      sessionState.errorCode = ''
    } else {
      setError(error)
    }
    return false
  }
}

export async function ensureSession(): Promise<boolean> {
  if (sessionState.ready) { return sessionState.authenticated }
  await refreshAuthStatus()
  const ok = await verifySession()
  sessionState.ready = true
  return ok
}

export async function login(password: string): Promise<boolean> {
  sessionState.busy = true
  clearSessionError()
  try {
    const session = await api.authLogin(password)
    setCsrfToken(session.csrf_token)
    sessionState.session = session
    const ok = await verifySession()
    sessionState.authenticated = ok
    return ok
  } catch (error) {
    setError(error)
    sessionState.authenticated = false
    return false
  } finally {
    sessionState.busy = false
    sessionState.ready = true
  }
}

export async function setupPassword(password: string, confirm: string): Promise<boolean> {
  sessionState.busy = true
  clearSessionError()
  try {
    const session = await api.authSetup(password, confirm)
    setCsrfToken(session.csrf_token)
    sessionState.session = session
    const ok = await verifySession()
    sessionState.authenticated = ok
    if (ok) { await refreshAuthStatus() }
    return ok
  } catch (error) {
    setError(error)
    sessionState.authenticated = false
    return false
  } finally {
    sessionState.busy = false
    sessionState.ready = true
  }
}

/** token 校验（高级选项）。当前后端 token_fallback=false 时会返回 401，UI 要如实显示。 */
export async function useToken(token: string): Promise<boolean> {
  sessionState.busy = true
  clearSessionError()
  setApiToken(token.trim())
  sessionState.token = token.trim()
  try {
    const ok = await verifySession()
    if (!ok && !sessionState.error) {
      sessionState.error = 'token 未通过会话校验（后端 token 回退可能未启用）'
      sessionState.errorCode = 'unauthorized'
    }
    return ok
  } finally {
    sessionState.busy = false
    sessionState.ready = true
  }
}

export async function logout(): Promise<void> {
  try { await api.authLogout() } catch { /* 退出不因 401 报错 */ }
  setCsrfToken(null)
  setApiToken(null)
  sessionState.token = ''
  sessionState.session = null
  sessionState.authenticated = false
  await refreshAuthStatus()
}
