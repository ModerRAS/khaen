/**
 * 会话状态（骨架）——**M1 之前没有真实认证**。
 *
 * TODO(M1)：接 GET /api/auth/status、GET /api/auth/session、POST /api/auth/{setup,login,logout}；
 *  - Cookie 会话 + X-KHAEN-CSRF（写操作）；401 时清会话并跳登录；
 *  - 错误密码 401、限速 429、已初始化 409 按 PLAN §6.1 映射成本地化文案。
 *
 * 现在：ensureSession() 不发任何请求、不伪造登录态，返回 'unknown'（checked = false），
 * 因此路由守卫不会拦截——这样骨架页面可以被 review，而 UI 上会明确标注「未接入」。
 */
import { reactive } from 'vue'

export type SessionStatus = 'unknown' | 'authenticated' | 'unauthenticated'
export type SessionAuthKind = 'cookie' | 'token'

export interface SessionState {
  /** 认证结果：unknown = 尚未/无法判定（API 未接入时恒为 unknown）。 */
  status: SessionStatus
  /** 服务端是否已配置管理员密码：null = 未知（未接 API，不猜）。 */
  configured: boolean | null
  /** 会话形式：null = 未知。 */
  authKind: SessionAuthKind | null
  /** CSRF token（写操作需要）：null = 未知。 */
  csrfToken: string | null
  /** 是否已真正查询过服务端：false = API 未接入（骨架）。 */
  checked: boolean
  /** 最近一次错误码（稳定字符串，前端再映射文案）：null = 无。 */
  lastError: string | null
}

export const sessionState = reactive<SessionState>({
  status: 'unknown',
  configured: null,
  authKind: null,
  csrfToken: null,
  checked: false,
  lastError: null
})

let inflight: Promise<SessionStatus> | null = null

/**
 * 路由守卫调用：确认会话有效性。
 * 骨架阶段返回 'unknown'（不请求、不拦截）；M1 落地后替换为真实请求。
 */
export function ensureSession(): Promise<SessionStatus> {
  if (inflight) {
    return inflight
  }
  inflight = Promise.resolve().then(() => {
    // TODO(M1): 换成真实请求：
    //   const response = await api.get('/api/auth/session')
    //   sessionState.checked = true
    //   sessionState.status = response.authenticated ? 'authenticated' : 'unauthenticated'
    console.info('[session] TODO(M1): auth API not wired; session status stays "unknown"')
    return sessionState.status
  })
  inflight.finally(() => {
    inflight = null
  })
  return inflight
}

/** 退出登录（骨架：只清本地状态，不发请求）。 TODO(M1)：POST /api/auth/logout。 */
export function logout(): void {
  sessionState.status = 'unknown'
  sessionState.checked = false
  sessionState.csrfToken = null
  sessionState.lastError = null
}
