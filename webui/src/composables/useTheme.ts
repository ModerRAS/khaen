import { reactive } from 'vue'

const THEME_KEY = 'khaen.theme'

function initial(): 'dark' | 'light' {
  try {
    const saved = localStorage.getItem(THEME_KEY)
    if (saved === 'light' || saved === 'dark') { return saved }
  } catch { /* ignore */ }
  return 'dark'
}

export const themeState = reactive({ theme: initial() as 'dark' | 'light' })

function apply(): void {
  document.documentElement.classList.toggle('dark', themeState.theme === 'dark')
}

export function initTheme(): void {
  apply()
}

export function toggleTheme(): void {
  themeState.theme = themeState.theme === 'dark' ? 'light' : 'dark'
  try { localStorage.setItem(THEME_KEY, themeState.theme) } catch { /* ignore */ }
  apply()
}
