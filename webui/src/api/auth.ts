/**
 * /api/auth/* 端点封装（PLAN §4.2；前端 T34b 契约）。
 *
 * 端点：
 * - GET  /api/auth/status       匿名，{configured, authenticated}
 * - POST /api/auth/setup        {password, confirm} -> 201 {csrf, expires_in} + Set-Cookie；已配置 409
 * - POST /api/auth/login        {password} -> 200 {csrf, expires_in} + Set-Cookie；失败 401，限速 429
 * - GET  /api/auth/session      {authenticated, csrf, expires_at}；未登录 401
 * - POST /api/auth/logout       清会话（幂等，可能 204/200 空体）
 * - POST /api/auth/password     {old, new}（登录 + CSRF）
 *
 * 说明：Cookie 由浏览器持有（HttpOnly），这里只消费响应体里的 csrf / expires_*。
 * 响应体做最小运行时校验：字段缺失/类型漂移时抛 code=invalid_response 的 ApiError，
 * 不把畸形响应当成成功（避免"假登录"）。
 */
import { ApiClient, ApiError } from './client.ts'

export interface AuthStatusResponse {
  /** 服务端是否已配置管理员密码。 */
  configured: boolean
  /** 当前请求是否已带有效会话。 */
  authenticated: boolean
}

export interface AuthSessionResponse {
  authenticated: boolean
  /** 写请求所需的 CSRF token（绑定当前会话）。 */
  csrf: string
  /** 会话过期时间（RFC3339 UTC）。 */
  expires_at: string
}

export interface AuthTokenResponse {
  /** 写请求所需的 CSRF token。 */
  csrf: string
  /** 会话剩余秒数。 */
  expires_in: number
}

export interface AuthApi {
  status(): Promise<AuthStatusResponse>
  setup(password: string, confirm: string): Promise<AuthTokenResponse>
  login(password: string): Promise<AuthTokenResponse>
  session(): Promise<AuthSessionResponse>
  logout(): Promise<void>
  changePassword(oldPassword: string, newPassword: string): Promise<void>
}

function invalidResponse(endpoint: string, detail: string): ApiError {
  return new ApiError({
    status: 200,
    code: 'invalid_response',
    message: 'unexpected response from ' + endpoint + ': ' + detail
  })
}

function requireBoolean(value: unknown, endpoint: string, field: string): boolean {
  if (typeof value !== 'boolean') {
    throw invalidResponse(endpoint, field + ' must be a boolean')
  }
  return value
}

function requireString(value: unknown, endpoint: string, field: string): string {
  if (typeof value !== 'string' || value.length === 0) {
    throw invalidResponse(endpoint, field + ' must be a non-empty string')
  }
  return value
}

function requireFiniteNumber(value: unknown, endpoint: string, field: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) {
    throw invalidResponse(endpoint, field + ' must be a positive number')
  }
  return value
}

function asRecord(body: unknown, endpoint: string): Record<string, unknown> {
  if (typeof body !== 'object' || body === null || Array.isArray(body)) {
    throw invalidResponse(endpoint, 'body must be a JSON object')
  }
  return body as Record<string, unknown>
}

function parseToken(body: unknown, endpoint: string): AuthTokenResponse {
  const record = asRecord(body, endpoint)
  return {
    csrf: requireString(record.csrf, endpoint, 'csrf'),
    expires_in: requireFiniteNumber(record.expires_in, endpoint, 'expires_in')
  }
}

/** 用共享 client 构造认证 API（单测注入 baseUrl/fetchImpl 即可脱离网络）。 */
export function createAuthApi(client: ApiClient): AuthApi {
  return {
    async status(): Promise<AuthStatusResponse> {
      const endpoint = '/api/auth/status'
      const body = asRecord(await client.get<unknown>(endpoint, { auth: 'probe' }), endpoint)
      return {
        configured: requireBoolean(body.configured, endpoint, 'configured'),
        authenticated: requireBoolean(body.authenticated, endpoint, 'authenticated')
      }
    },

    async setup(password: string, confirm: string): Promise<AuthTokenResponse> {
      const endpoint = '/api/auth/setup'
      const body = await client.post<unknown>(endpoint, { password, confirm }, { csrf: 'omit', auth: 'probe' })
      return parseToken(body, endpoint)
    },

    async login(password: string): Promise<AuthTokenResponse> {
      const endpoint = '/api/auth/login'
      const body = await client.post<unknown>(endpoint, { password }, { csrf: 'omit', auth: 'probe' })
      return parseToken(body, endpoint)
    },

    async session(): Promise<AuthSessionResponse> {
      const endpoint = '/api/auth/session'
      const body = asRecord(await client.get<unknown>(endpoint, { auth: 'probe' }), endpoint)
      return {
        authenticated: requireBoolean(body.authenticated, endpoint, 'authenticated'),
        csrf: requireString(body.csrf, endpoint, 'csrf'),
        expires_at: requireString(body.expires_at, endpoint, 'expires_at')
      }
    },

    async logout(): Promise<void> {
      // 幂等：会话已失效时后端可能回 401/204；这条路径不清态跳转，由 store 统一收尾。
      await client.post<unknown>('/api/auth/logout', undefined, { auth: 'probe' })
    },

    async changePassword(oldPassword: string, newPassword: string): Promise<void> {
      // 改密失败(401) 与"会话失效"共用 401：交给调用方按上下文映射，不在这里触发全局跳转。
      await client.post<unknown>(
        '/api/auth/password',
        { old: oldPassword, new: newPassword },
        { auth: 'probe' }
      )
    }
  }
}
