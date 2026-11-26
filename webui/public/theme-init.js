/* khaen 首帧主题引导（外置脚本以满足 daemon CSP script-src 'self'；不访问网络）。
 *
 * - 持久化：localStorage['khaen.theme'] = 'system' | 'dark' | 'light'（缺省 system）
 * - 解析：system 时读 prefers-color-scheme；其余按显式选择
 * - 结果写到 <html data-theme data-theme-mode class>，并同步 color-scheme 与 theme-color
 * - 必须在 Vue 挂载前执行（index.html <head> 里的同步 <script src>），否则首帧会闪。
 */
(function () {
  var STORAGE_KEY = 'khaen.theme'
  var BACKGROUND = { dark: '#0f1115', light: '#f5f7fb' }
  try {
    var root = document.documentElement
    var stored = null
    try {
      stored = window.localStorage.getItem(STORAGE_KEY)
    } catch (error) {
      stored = null
    }
    var mode = stored === 'dark' || stored === 'light' || stored === 'system' ? stored : 'system'
    var prefersDark = true
    try {
      prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches
    } catch (error) {
      prefersDark = true
    }
    var theme = mode === 'system' ? (prefersDark ? 'dark' : 'light') : mode
    root.setAttribute('data-theme', theme)
    root.setAttribute('data-theme-mode', mode)
    root.classList.toggle('dark', theme === 'dark')
    root.style.colorScheme = theme
    var meta = document.getElementById('khaen-theme-color')
    if (meta) {
      meta.setAttribute('content', BACKGROUND[theme])
    }
  } catch (error) {
    document.documentElement.classList.add('dark')
  }
})()
