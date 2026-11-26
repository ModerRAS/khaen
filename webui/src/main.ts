/**
 * 应用入口。
 *
 * 顺序很重要：
 * 1) Element Plus 样式 → 暗色 css-vars → khaen tokens/main（后导入才能覆盖 EP 变量）；
 * 2) initTheme() 同步首帧已由 public/theme-init.js 应用的 DOM 状态（防闪），并挂 prefers-color-scheme 监听；
 * 3) initLocale() 同步 <html lang>；
 * 4) 安装 i18n 后再装 router（router.afterEach 用 i18n.global.t 写 document.title）。
 */
import { createApp } from 'vue'
import ElementPlus from 'element-plus'
import 'element-plus/dist/index.css'
import 'element-plus/theme-chalk/dark/css-vars.css'
import '@/styles/tokens.css'
import '@/styles/main.css'

import App from '@/App.vue'
import { router } from '@/router'
import { i18n, initLocale } from '@/i18n'
import { initTheme } from '@/composables/useTheme'

initTheme()
initLocale()

const app = createApp(App)
app.use(ElementPlus)
app.use(i18n)
app.use(router)
app.mount('#app')
