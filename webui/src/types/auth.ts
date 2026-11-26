/**
 * 认证相关的纯类型（不依赖 Vue / router / fetch，Node 单测可直接 import type）。
 */

/** 会话判定结果：unknown = 无法判定（例如后端不可达，不得当成已登录）。 */
export type SessionStatus = 'unknown' | 'authenticated' | 'unauthenticated'

/** 登录页两态：首次初始化（setup）/ 已初始化后的登录（signin）。 */
export type AuthMode = 'setup' | 'signin'
