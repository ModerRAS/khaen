/**
 * REST 客户端（PLAN §4.1 / §4.2）——浏览器同源调用 khaen-daemon 的唯一出口。
 *
 * 纪律：
 * - Cookie 会话由浏览器管理（HttpOnly / SameSite=Strict），前端不读、不写 Cookie：
 *   每个请求统一 credentials: 'include'，登录态只认 /api/auth/session 的响应体。
 * - 写方法（POST/PUT/PATCH/DELETE）在拿到会话 csrf 后自动附加 X-KHAEN-CSRF 头；
 *   登录/初始化这类匿名接口用 csrf: 'omit' 显式关闭（此时还没有 csrf）。
 * - 错误体固定 {"error":{"code","message","details"}}，统一解析成 ApiError；
 *   网络层失败（fetch 抛出 / 超时）归为 status=0（code=network_error / request_timeout）。
 * - 任意 401：先构造 ApiError，再回调 onUnauthorized（会话 store 负责清态 + 跳登录）；
 *   会话探测类请求（auth: 'probe'）不触发该回调，避免登录页自跳。
 * - 可选 API 版本校验：响应头 X-KHAEN-API-Version 存在且与期望不符时只回调一次
 *   onVersionMismatch（不阻断请求；是否提示由上层决定）。
 *
 * 该模块不依赖 Vue / DOM 全局（除 fetch/Response 标准类型），可在 Node 单测中直接使用。
 */

/** 当前前端对应的 API 版本（PLAN §4.1：破坏性变更升版本，SPA 检测到不匹配提示刷新）。 */
export const DEFAULT_API_VERSION = '1'

/** 默认请求超时（毫秒）；0 表示不超时。 */
export const DEFAULT_TIMEOUT_MS = 15000

export type HttpMethod = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE'

const WRITE_METHODS: ReadonlySet<HttpMethod> = new Set<HttpMethod>(['POST', 'PUT', 'PATCH', 'DELETE'])

/** 查询参数值（null/undefined 会被跳过）。 */
export type QueryValue = string | number | boolean | null | undefined

export type QueryParams = Record<string, QueryValue>

/** fetch 的最小可注入签名（单测注入假实现，避免网络）。 */
export type ApiFetch = (input: string, init?: RequestInit) => Promise<Response>

export interface ApiErrorOptions {
  /** HTTP 状态码；0 = 网络层失败（未拿到响应）。 */
  status: number
  /** 稳定错误码（后端 error.code，或本地归类码如 network_error）。 */
  code: string
  /** 人类可读消息（后端 message；网络错误为本地英文描述）。 */
  message: string
  /** 后端 error.details 原样保留。 */
  details?: unknown
  /** 429 限速时 Retry-After 解析出的秒数（没有则为 null）。 */
  retryAfterSeconds?: number | null
}

/** 统一的 API 错误对象。 */
export class ApiError extends Error {
  readonly status: number
  readonly code: string
  readonly details: unknown
  readonly retryAfterSeconds: number | null

  constructor(options: ApiErrorOptions) {
    super(options.message)
    this.name = 'ApiError'
    this.status = options.status
    this.code = options.code
    this.details = options.details
    this.retryAfterSeconds = options.retryAfterSeconds ?? null
  }

  get isUnauthorized(): boolean {
    return this.status === 401
  }

  get isNetworkError(): boolean {
    return this.status === 0
  }
}

export function isApiError(error: unknown): error is ApiError {
  return error instanceof ApiError
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/** Retry-After 可能是秒数或 HTTP 日期；只解析秒数（后端契约用秒）。 */
export function parseRetryAfter(raw: string | null): number | null {
  if (raw === null) {
    return null
  }
  const seconds = Number.parseFloat(raw.trim())
  if (!Number.isFinite(seconds) || seconds < 0) {
    return null
  }
  return Math.ceil(seconds)
}

export interface ApiClientOptions {
  /** 同源部署为空字符串；dev 代理或单测可传 origin（如 http://127.0.0.1:8730）。 */
  baseUrl?: string
  /** 可注入 fetch（缺省 globalThis.fetch）。 */
  fetchImpl?: ApiFetch
  /** 读取当前会话 CSRF（写请求自动附加）；未登录时返回 null。 */
  csrfToken?: () => string | null
  /** 非探测类请求收到 401 时回调（会话 store 用它清态 + 跳登录）。 */
  onUnauthorized?: (error: ApiError) => void
  /** API 版本头与期望不一致时回调（只回调一次）。 */
  onVersionMismatch?: (serverVersion: string) => void
  /** 单请求默认超时（毫秒，0 = 不超时）。 */
  timeoutMs?: number
  /** 期望的 API 版本，缺省 DEFAULT_API_VERSION。 */
  expectedApiVersion?: string
}

export interface RequestOptions {
  /** JSON 请求体；GET 忽略。 */
  body?: unknown
  /** 查询参数（自动 URL 编码）。 */
  query?: QueryParams
  /** 附加/覆盖请求头。 */
  headers?: Record<string, string>
  /** 写请求是否自动带 CSRF：'omit' = 不带（登录/初始化等匿名写接口）。 */
  csrf?: 'auto' | 'omit'
  /**
   * 'required'（默认）= 401 交给 onUnauthorized 统一处理；
   * 'probe' = 会话探测（如 /api/auth/session），401 由调用方自己解释，不触发跳转。
   */
  auth?: 'required' | 'probe'
  /** 外部取消信号（传入时不再加内部超时定时器）。 */
  signal?: AbortSignal
  /** 覆盖本次请求超时（毫秒，0 = 不超时）。 */
  timeoutMs?: number
}

/** 同源 REST 客户端。 */
export class ApiClient {
  private readonly baseUrl: string
  private readonly fetchImpl: ApiFetch
  private readonly csrfToken: (() => string | null) | null
  private readonly onUnauthorized: ((error: ApiError) => void) | null
  private readonly onVersionMismatch: ((serverVersion: string) => void) | null
  private readonly timeoutMs: number
  private readonly expectedApiVersion: string
  private versionChecked = false
  private serverVersion: string | null = null

  constructor(options: ApiClientOptions = {}) {
    this.baseUrl = (options.baseUrl ?? '').replace(/\/+$/, '')
    // 注意：浏览器里 fetch 是 window 的方法，解构后直接当普通函数调用会 Illegal invocation，
    // 必须用箭头包一层，让 this 保持 globalThis（Node 单测注入的 fetchImpl 不受影响）。
    const fetchImpl: ApiFetch =
      options.fetchImpl ??
      ((input: string, init?: RequestInit) => {
        if (typeof globalThis.fetch !== 'function') {
          return Promise.reject(new Error('ApiClient: no fetch implementation available'))
        }
        return globalThis.fetch(input, init)
      })
    this.fetchImpl = fetchImpl
    this.csrfToken = options.csrfToken ?? null
    this.onUnauthorized = options.onUnauthorized ?? null
    this.onVersionMismatch = options.onVersionMismatch ?? null
    this.timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS
    this.expectedApiVersion = options.expectedApiVersion ?? DEFAULT_API_VERSION
  }

  /** 最近一次成功响应里看到的服务端 API 版本（没有该头时为 null）。 */
  get serverApiVersion(): string | null {
    return this.serverVersion
  }

  async request<T>(method: HttpMethod, path: string, options: RequestOptions = {}): Promise<T> {
    const upper = method.toUpperCase() as HttpMethod
    const url = this.buildUrl(path, options.query)
    const headers: Record<string, string> = { Accept: 'application/json' }

    if (options.body !== undefined) {
      headers['Content-Type'] = 'application/json'
    }
    if (WRITE_METHODS.has(upper) && options.csrf !== 'omit' && this.csrfToken !== null) {
      const token = this.csrfToken()
      if (typeof token === 'string' && token.length > 0) {
        headers['X-KHAEN-CSRF'] = token
      }
    }
    if (options.headers) {
      Object.assign(headers, options.headers)
    }

    const init: RequestInit = {
      method: upper,
      headers,
      credentials: 'include'
    }
    if (options.body !== undefined) {
      init.body = JSON.stringify(options.body)
    }

    let response: Response
    try {
      response = await this.dispatch(url, init, options)
    } catch (error) {
      if (isApiError(error)) {
        throw error
      }
      const message = error instanceof Error ? error.message : 'network request failed'
      throw new ApiError({ status: 0, code: 'network_error', message })
    }

    this.checkApiVersion(response)

    if (!response.ok) {
      const apiError = await this.toApiError(response)
      if (apiError.status === 401 && options.auth !== 'probe' && this.onUnauthorized !== null) {
        this.onUnauthorized(apiError)
      }
      throw apiError
    }

    return await this.parseSuccess<T>(response)
  }

  get<T>(path: string, options: Omit<RequestOptions, 'body'> = {}): Promise<T> {
    return this.request<T>('GET', path, options)
  }

  post<T>(path: string, body?: unknown, options: RequestOptions = {}): Promise<T> {
    return this.request<T>('POST', path, { ...options, body })
  }

  put<T>(path: string, body?: unknown, options: RequestOptions = {}): Promise<T> {
    return this.request<T>('PUT', path, { ...options, body })
  }

  patch<T>(path: string, body?: unknown, options: RequestOptions = {}): Promise<T> {
    return this.request<T>('PATCH', path, { ...options, body })
  }

  delete<T>(path: string, options: RequestOptions = {}): Promise<T> {
    return this.request<T>('DELETE', path, options)
  }

  private buildUrl(path: string, query?: QueryParams): string {
    if (typeof path !== 'string' || !path.startsWith('/')) {
      throw new Error('ApiClient: path must start with "/": ' + String(path))
    }
    let url = this.baseUrl + path
    if (query) {
      const params = new URLSearchParams()
      for (const [key, value] of Object.entries(query)) {
        if (value === null || value === undefined) {
          continue
        }
        params.append(key, String(value))
      }
      const queryString = params.toString()
      if (queryString.length > 0) {
        url += (url.includes('?') ? '&' : '?') + queryString
      }
    }
    return url
  }

  private async dispatch(url: string, init: RequestInit, options: RequestOptions): Promise<Response> {
    if (options.signal) {
      return await this.fetchImpl(url, { ...init, signal: options.signal })
    }
    const timeoutMs = options.timeoutMs ?? this.timeoutMs
    if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
      return await this.fetchImpl(url, init)
    }
    const controller = new AbortController()
    let timedOut = false
    const timer = setTimeout(() => {
      timedOut = true
      controller.abort()
    }, timeoutMs)
    try {
      return await this.fetchImpl(url, { ...init, signal: controller.signal })
    } catch (error) {
      if (timedOut) {
        throw new ApiError({ status: 0, code: 'request_timeout', message: 'request timed out after ' + timeoutMs + 'ms' })
      }
      throw error
    } finally {
      clearTimeout(timer)
    }
  }

  private checkApiVersion(response: Response): void {
    const version = response.headers.get('X-KHAEN-API-Version')
    if (version === null) {
      return
    }
    this.serverVersion = version
    if (this.versionChecked) {
      return
    }
    this.versionChecked = true
    if (version !== this.expectedApiVersion && this.onVersionMismatch !== null) {
      this.onVersionMismatch(version)
    }
  }

  private async toApiError(response: Response): Promise<ApiError> {
    const retryAfterSeconds = parseRetryAfter(response.headers.get('Retry-After'))
    let text = ''
    try {
      text = await response.text()
    } catch {
      text = ''
    }
    let code = 'http_' + String(response.status)
    let message = response.statusText.length > 0 ? response.statusText : 'HTTP ' + String(response.status)
    let details: unknown
    if (text.length > 0) {
      try {
        const parsed: unknown = JSON.parse(text)
        if (isRecord(parsed) && isRecord(parsed.error)) {
          const body = parsed.error
          if (typeof body.code === 'string' && body.code.length > 0) {
            code = body.code
          }
          if (typeof body.message === 'string' && body.message.length > 0) {
            message = body.message
          }
          details = body.details
        }
      } catch {
        // 非 JSON 错误体（反代 HTML 等）：保留 http_<status> 归类码，便于上层提示刷新。
      }
    }
    return new ApiError({ status: response.status, code, message, details, retryAfterSeconds })
  }

  private async parseSuccess<T>(response: Response): Promise<T> {
    if (response.status === 204 || response.status === 205) {
      return undefined as T
    }
    let text = ''
    try {
      text = await response.text()
    } catch {
      text = ''
    }
    if (text.length === 0) {
      return undefined as T
    }
    try {
      return JSON.parse(text) as T
    } catch {
      throw new ApiError({
        status: response.status,
        code: 'invalid_response',
        message: 'response is not valid JSON',
        details: { contentType: response.headers.get('Content-Type') }
      })
    }
  }
}
