/**
 * i18n 入口（vue-i18n，Composition API 模式）。
 *
 * PLAN §12.2：
 * - 源语言 zh-CN，en-US 对等；缺失键回退 zh-CN 并 console.warn；
 * - 默认跟随浏览器语言（zh* → zh-CN，其余 → en-US），可切换并持久化到 localStorage['khaen.locale']；
 * - 所有模板文案一律 useI18n().t()，不在 .vue 里硬编码中文/英文。
 */
import { createI18n } from 'vue-i18n'
import zhCN from './zh-CN'
import enUS from './en-US'

export const LOCALES = ['zh-CN', 'en-US'] as const
export type Locale = (typeof LOCALES)[number]

/** 回退语言（源语言）。 */
export const FALLBACK_LOCALE: Locale = 'zh-CN'

const STORAGE_KEY = 'khaen.locale'

export function isLocale(value: unknown): value is Locale {
  return typeof value === 'string' && (LOCALES as readonly string[]).includes(value)
}

/** 优先级：localStorage → 浏览器语言 → zh-CN。 */
export function detectLocale(): Locale {
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY)
    if (isLocale(stored)) {
      return stored
    }
  } catch {
    // localStorage 不可用（隐私模式等）：忽略，继续按浏览器语言判断。
  }
  const candidates: string[] =
    typeof navigator === 'undefined'
      ? []
      : navigator.languages && navigator.languages.length > 0
        ? [...navigator.languages]
        : [navigator.language]
  for (const candidate of candidates) {
    if (typeof candidate !== 'string') {
      continue
    }
    const lower = candidate.toLowerCase()
    if (lower.startsWith('zh')) {
      return 'zh-CN'
    }
    if (lower.startsWith('en')) {
      return 'en-US'
    }
  }
  return FALLBACK_LOCALE
}

export const i18n = createI18n({
  legacy: false,
  globalInjection: true,
  locale: detectLocale(),
  fallbackLocale: FALLBACK_LOCALE,
  messages: {
    'zh-CN': zhCN,
    'en-US': enUS
  },
  missingWarn: true,
  fallbackWarn: true,
  missing: (locale: string, key: string): string => {
    console.warn('[i18n] missing key:', locale, key)
    return key
  }
})

export function currentLocale(): Locale {
  const value: unknown = i18n.global.locale.value
  return isLocale(value) ? value : FALLBACK_LOCALE
}

/** 切换语言：立即生效（无需刷新），写 localStorage 与 <html lang>。 */
export function setLocale(next: Locale): void {
  i18n.global.locale.value = next
  try {
    window.localStorage.setItem(STORAGE_KEY, next)
  } catch {
    // 忽略持久化失败：本次会话内仍然生效。
  }
  document.documentElement.setAttribute('lang', next)
}

/** 应用启动时同步 <html lang>（首帧语言由 i18n 初始化决定，无需防闪脚本）。 */
export function initLocale(): void {
  document.documentElement.setAttribute('lang', currentLocale())
}
