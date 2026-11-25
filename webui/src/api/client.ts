// 极薄的 REST 客户端：同源 Cookie 会话 + CSRF 头；错误统一成 ApiError。
import type {
  ApiErrorBody, AudioFormatBody, AuthSession, AuthStatus, CalJobActionBody,
  CalJobsBody, ChannelMapBody, CoeffBody, CommitBody, ContentLayoutAck, ContentLayoutBody,
  ContentLayoutDraft, FormatApplyBody, HealthBody, ModeStatus, MuteBody, PrepareBody,
  RuntimeConfigBody, RuntimeStatus, TargetSpec, TargetBody, VolumeBody
} from './types'

export class ApiError extends Error {
  status: number
  code: string
  body: unknown

  constructor(status: number, code: string, message: string, body: unknown) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.code = code
    this.body = body
  }
}

let csrfToken = ''
let apiToken = ''
const TOKEN_KEY = 'khaen.token'

export function loadStoredToken(): string {
  try { return localStorage.getItem(TOKEN_KEY) ?? '' } catch { return '' }
}

export function setApiToken(token: string | null): void {
  apiToken = token ?? ''
  try {
    if (apiToken) { localStorage.setItem(TOKEN_KEY, apiToken) } else { localStorage.removeItem(TOKEN_KEY) }
  } catch { /* 忽略存储失败（隐私模式） */ }
}

export function setCsrfToken(token: string | null): void {
  csrfToken = token ?? ''
}

export function getCsrfToken(): string {
  return csrfToken
}

apiToken = loadStoredToken()

interface RequestOptions {
  timeoutMs?: number
}

async function request<T>(method: string, path: string, body?: unknown, options: RequestOptions = {}): Promise<T> {
  const headers: Record<string, string> = { Accept: 'application/json' }
  if (body !== undefined) { headers['Content-Type'] = 'application/json' }
  if (apiToken) { headers['X-KHAEN-Token'] = apiToken }
  if (method !== 'GET' && csrfToken) { headers['X-KHAEN-CSRF'] = csrfToken }
  const controller = new AbortController()
  const timeout = window.setTimeout(() => controller.abort(), options.timeoutMs ?? 15000)
  let response: Response
  try {
    response = await fetch(path, {
      method,
      headers,
      credentials: 'same-origin',
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: controller.signal
    })
  } catch (error) {
    window.clearTimeout(timeout)
    const reason = error instanceof DOMException && error.name === 'AbortError' ? '请求超时' : '无法连接控制面'
    throw new ApiError(0, 'network_error', `${reason}（${method} ${path}）`, null)
  }
  window.clearTimeout(timeout)
  const text = await response.text()
  let json: unknown = null
  if (text) {
    try { json = JSON.parse(text) } catch { json = null }
  }
  if (!response.ok) {
    const errBody = json as ApiErrorBody | null
    const code = errBody?.error?.code ?? `http_${response.status}`
    const message = errBody?.error?.message ?? (typeof json === 'string' ? json : text || response.statusText)
    throw new ApiError(response.status, code, message, json ?? text)
  }
  if (json === null) { return {} as T }
  return json as T
}

/** 读取响应头（系统信息页需要 X-KHAEN-API-Version / X-KHAEN-Web-Version）。 */
async function requestWithHeaders<T>(method: string, path: string): Promise<{ data: T; headers: Record<string, string> }> {
  const response = await fetch(path, { method, credentials: 'same-origin', headers: { Accept: 'application/json' } })
  const text = await response.text()
  const headers: Record<string, string> = {}
  response.headers.forEach((value, key) => { headers[key.toLowerCase()] = value })
  if (!response.ok) {
    let code = `http_${response.status}`
    let message = response.statusText
    try {
      const parsed = JSON.parse(text) as ApiErrorBody
      code = parsed.error?.code ?? code
      message = parsed.error?.message ?? message
    } catch { /* 保留状态码信息 */ }
    throw new ApiError(response.status, code, message, text)
  }
  return { data: (text ? JSON.parse(text) : {}) as T, headers }
}

export const api = {
  // 认证（R03/R04；密码 Cookie 会话 + 可选 token 回退）
  authStatus: () => request<AuthStatus>('GET', '/api/auth/status'),
  authSetup: (password: string, confirm: string) =>
    request<AuthSession>('POST', '/api/auth/setup', { password, confirm }),
  authLogin: (password: string) => request<AuthSession>('POST', '/api/auth/login', { password }),
  authSession: () => request<AuthSession>('GET', '/api/auth/session'),
  authLogout: () => request<{ ok: boolean }>('POST', '/api/auth/logout', {}),

  // 运行时（T42）
  runtimeStatus: () => request<RuntimeStatus>('GET', '/api/runtime/status'),
  runtimeConfig: () => request<RuntimeConfigBody>('GET', '/api/runtime/config'),
  prepare: () => request<PrepareBody>('POST', '/api/runtime/prepare', {}),
  commit: (prepareId: string) => request<CommitBody>('POST', '/api/runtime/commit', { prepare_id: prepareId }),
  abort: (prepareId: string) => request<{ aborted: boolean }>('POST', '/api/runtime/abort', { prepare_id: prepareId }),

  volumeGet: () => request<VolumeBody>('GET', '/api/runtime/volume'),
  volumePut: (db: number) => request<VolumeBody>('PUT', '/api/runtime/volume', { db }),
  muteGet: () => request<MuteBody>('GET', '/api/runtime/mute'),
  mutePut: (mute: boolean) => request<MuteBody>('PUT', '/api/runtime/mute', { mute }),

  modeGet: () => request<ModeStatus>('GET', '/api/runtime/mode'),
  modePut: (mode: string) => request<ModeStatus>('PUT', '/api/runtime/mode', { mode }),

  targetGet: () => request<TargetBody>('GET', '/api/runtime/target'),
  targetPut: (target: TargetSpec) => request<{ ok?: boolean; target?: TargetSpec; requires_commit?: boolean }>(
    'PUT', '/api/runtime/target', { target }
  ),
  coeffGet: () => request<CoeffBody>('GET', '/api/runtime/coeff'),

  audioFormatGet: () => request<AudioFormatBody>('GET', '/api/audio/format'),
  audioFormatPut: (sampleRate: number, chunksize: number) =>
    request<FormatApplyBody>('PUT', '/api/audio/format', { sample_rate: sampleRate, chunksize }),

  channelMapGet: () => request<ChannelMapBody>('GET', '/api/channelmap/input-mode'),

  // 内容布局 / 中置下混（task-316：GET/PUT /api/content/layout，khaen-host-content/v1）
  contentLayoutGet: () => request<ContentLayoutBody>('GET', '/api/content/layout'),
  contentLayoutPut: (body: ContentLayoutDraft) => request<ContentLayoutAck>('PUT', '/api/content/layout', body),

  // 校准 job（R48）
  calJobsList: () => request<CalJobsBody>('GET', '/api/calibration/jobs'),
  calJobGet: (id: string) => request<CalJobActionBody>('GET', `/api/calibration/jobs/${encodeURIComponent(id)}`),
  calJobStart: (kind: string, extra: Record<string, unknown> = {}) =>
    request<CalJobActionBody>('POST', '/api/calibration/jobs', { kind, ...extra }),
  calJobCancel: (id: string) => request<CalJobActionBody>('POST', `/api/calibration/jobs/${encodeURIComponent(id)}/cancel`, {}),
  calJobApply: (id: string) => request<CalJobActionBody>('POST', `/api/calibration/jobs/${encodeURIComponent(id)}/apply`, {}),
  calJobRetest: (id: string) => request<CalJobActionBody>('POST', `/api/calibration/jobs/${encodeURIComponent(id)}/retest`, {}),
  calJobRollback: (id: string) => request<CalJobActionBody>('POST', `/api/calibration/jobs/${encodeURIComponent(id)}/rollback`, {}),

  // 公开探针（无鉴权）
  healthz: () => requestWithHeaders<HealthBody>('GET', '/healthz')
}

export function errorText(error: unknown): string {
  if (error instanceof ApiError) { return `${error.code}: ${error.message}` }
  if (error instanceof Error) { return error.message }
  return String(error)
}