/**
 * 路由守卫的纯决策逻辑（PLAN §5.2）——不 import .vue / vue-router，可在 Node 单测中覆盖。
 *
 * 规则：
 * - 未登录访问受保护页 -> /login?next=<原路径>；
 * - 已登录访问 /login -> 回跳 next（只接受站内相对路径，拒绝开放重定向）；
 * - 公开页（/login）在未登录/未知状态放行（登录页自己展示后端不可达并给重试）；
 * - 未知会话状态（后端不可达）按未登录处理（fail closed），绝不因为"探测失败"放行受保护页。
 */
import type { SessionStatus } from '../types/auth.ts'

/** next 参数长度上限（防御异常长 URL；正常站内路径远小于此）。 */
const MAX_NEXT_LENGTH = 2048

/** 用于解析相对路径的虚拟 origin（不会发起请求）。 */
const INTERNAL_ORIGIN = 'http://khaen.invalid'

/**
 * 只允许站内相对路径作为登录后回跳目标（防开放重定向）。
 * 拒绝：绝对 URL、协议相对（//host）、反斜杠变体（/\host、\\host）、控制字符、超长值。
 * 返回规范化后的站内路径（含 query/hash），非法输入回退 '/'。
 */
export function safeNext(raw: unknown): string {
  if (typeof raw !== 'string') {
    return '/'
  }
  const value = raw.trim()
  if (value.length === 0 || value.length > MAX_NEXT_LENGTH) {
    return '/'
  }
  if (value[0] !== '/') {
    return '/'
  }
  if (value.startsWith('//')) {
    return '/'
  }
  if (value.includes('\\')) {
    return '/'
  }
  // 控制字符（含 \n \r \t）不允许出现在内部路径里。
  if (/[\u0000-\u001f\u007f]/.test(value)) {
    return '/'
  }
  let url: URL
  try {
    url = new URL(value, INTERNAL_ORIGIN)
  } catch {
    return '/'
  }
  if (url.origin !== INTERNAL_ORIGIN) {
    return '/'
  }
  const path = url.pathname + url.search + url.hash
  if (path[0] !== '/') {
    return '/'
  }
  return path
}

/** 登录成功后的落地路径：next 合法则用它，但不能指回登录页（否则会循环）。 */
export function postLoginPath(raw: unknown): string {
  const next = safeNext(raw)
  if (next === '/login' || next.startsWith('/login?') || next.startsWith('/login#') || next.startsWith('/login/')) {
    return '/'
  }
  return next
}

/** 守卫需要的最小路由信息（与 vue-router 的 RouteLocation 兼容的子集）。 */
export interface GuardTarget {
  /** 路由名（isLogin 判定用）。 */
  name?: string | null
  /** 完整路径（含 query/hash），用于 /login?next=。 */
  fullPath: string
  /** 查询参数（读取 next）。 */
  query?: Record<string, unknown>
  meta?: {
    /** true = 无需会话即可访问（仅登录页）。 */
    public?: boolean
    /** true = 需要会话。 */
    requiresAuth?: boolean
  }
}

export type GuardDecision =
  | { kind: 'allow' }
  | { kind: 'login'; next: string }
  | { kind: 'redirect'; path: string }

/** 纯函数：根据目标路由与会话状态给出导航决策。 */
export function resolveNavigation(target: GuardTarget, status: SessionStatus): GuardDecision {
  const meta = target.meta ?? {}

  if (meta.public === true) {
    if (target.name === 'login' && status === 'authenticated') {
      return { kind: 'redirect', path: postLoginPath(target.query ? target.query.next : undefined) }
    }
    return { kind: 'allow' }
  }

  if (meta.requiresAuth === true) {
    if (status === 'authenticated') {
      return { kind: 'allow' }
    }
    return { kind: 'login', next: safeNext(target.fullPath) }
  }

  return { kind: 'allow' }
}
