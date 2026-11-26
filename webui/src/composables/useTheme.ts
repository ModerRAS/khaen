/**
 * 深/浅主题（PLAN §11.7 / §12.1）。
 *
 * - 三态模式：'system'（跟随系统，默认）| 'dark' | 'light'，持久化到 localStorage['khaen.theme']；
 * - 解析结果写到 <html data-theme="dark|light" data-theme-mode="..." class="dark?">，
 *   同时同步 style.color-scheme 与 <meta name="theme-color">；
 * - 首帧由 public/theme-init.js 在挂载前应用（防闪烁），这里只负责运行时状态与切换。
 */
import { readonly, ref } from 'vue'

export type ThemeMode = 'system' | 'dark' | 'light'
export type ResolvedTheme = 'dark' | 'light'

const STORAGE_KEY = 'khaen.theme'
const BACKGROUND: Record<ResolvedTheme, string> = {
  dark: '#0f1115',
  light: '#f5f7fb'
}

const mode = ref<ThemeMode>('system')
const resolved = ref<ResolvedTheme>('dark')

let mediaQuery: MediaQueryList | null = null
let initialized = false

function isThemeMode(value: unknown): value is ThemeMode {
  return value === 'system' || value === 'dark' || value === 'light'
}

function readStoredMode(): ThemeMode {
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY)
    if (isThemeMode(stored)) {
      return stored
    }
  } catch {
    // localStorage 不可用：退回跟随系统。
  }
  return 'system'
}

function prefersDark(): boolean {
  try {
    return window.matchMedia('(prefers-color-scheme: dark)').matches
  } catch {
    return true
  }
}

function resolveTheme(nextMode: ThemeMode): ResolvedTheme {
  if (nextMode === 'system') {
    return prefersDark() ? 'dark' : 'light'
  }
  return nextMode
}

/** 把解析后的主题写到 DOM（Element Plus 暗色变量靠 html.dark）。 */
function applyDom(nextMode: ThemeMode, nextTheme: ResolvedTheme): void {
  const root = document.documentElement
  root.setAttribute('data-theme', nextTheme)
  root.setAttribute('data-theme-mode', nextMode)
  root.classList.toggle('dark', nextTheme === 'dark')
  root.style.colorScheme = nextTheme
  const meta = document.getElementById('khaen-theme-color')
  if (meta) {
    meta.setAttribute('content', BACKGROUND[nextTheme])
  }
}

function apply(nextMode: ThemeMode): void {
  mode.value = nextMode
  resolved.value = resolveTheme(nextMode)
  applyDom(nextMode, resolved.value)
}

/** 设置为指定模式并持久化。 */
export function setThemeMode(nextMode: ThemeMode): void {
  apply(nextMode)
  try {
    window.localStorage.setItem(STORAGE_KEY, nextMode)
  } catch {
    // 持久化失败不影响本次会话。
  }
}

/** 顶栏一键切换：在当前解析结果的反面之间切换（显式 dark/light，覆盖 system）。 */
export function toggleTheme(): void {
  setThemeMode(resolved.value === 'dark' ? 'light' : 'dark')
}

/**
 * 应用启动时调用一次：同步运行时状态、监听 prefers-color-scheme 变化。
 * 幂等（HMR / 重复调用安全）。
 */
export function initTheme(): void {
  if (initialized) {
    return
  }
  initialized = true
  apply(readStoredMode())
  try {
    mediaQuery = window.matchMedia('(prefers-color-scheme: dark)')
    mediaQuery.addEventListener('change', () => {
      if (mode.value === 'system') {
        apply('system')
      }
    })
  } catch {
    mediaQuery = null
  }
}

/** 组件内使用：只读状态 + 操作函数。 */
export function useTheme() {
  return {
    mode: readonly(mode),
    resolved: readonly(resolved),
    setThemeMode,
    toggleTheme
  }
}
