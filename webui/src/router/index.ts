/**
 * 路由表与全局守卫（PLAN §5.1 / §5.2）。
 *
 * 已落地：/login（登录/初始化，公开）、/（概览）、/volume（音量与静音，M3）、/format（音频格式，M3）、
 * /channels（方案与声道，M4）、/calibration（校准列表）、/calibration/wizard（8 步向导）、
 * /calibration/jobs/:id（job 详情）、/system（系统信息）、/system/logs（日志）。
 * 其余页面（routing M3）在菜单里灰显并标注计划里程碑，
 * 尚未注册路由——深链由 catch-all 重定向回 /（后续里程碑逐个接入时再补 route record）。
 *
 * 守卫（M1 起真实生效）：
 * - 每次导航先 ensureSession()（结果缓存，401 由 store 清态）；
 * - 未登录访问受保护页 -> /login?next=<站内相对路径>；
 * - 已登录访问 /login -> 回跳 next（safeNext 拒绝开放重定向；不允许回跳 /login 自身）；
 * - 任意受保护 API 的 401 -> setUnauthorizedHandler 注册的跳转（/login?next=...&expired=1）。
 */
import { createRouter, createWebHistory, type RouteRecordRaw } from 'vue-router'
import AppShell from '@/components/AppShell.vue'
import LoginView from '@/views/LoginView.vue'
import OverviewView from '@/views/OverviewView.vue'
import VolumeView from '@/views/VolumeView.vue'
import FormatView from '@/views/FormatView.vue'
import ChannelsView from '@/views/ChannelsView.vue'
import CalibrationView from '@/views/CalibrationView.vue'
import CalibrationWizardView from '@/views/CalibrationWizardView.vue'
import CalibrationJobView from '@/views/CalibrationJobView.vue'
import SystemView from '@/views/SystemView.vue'
import SystemLogsView from '@/views/SystemLogsView.vue'
import { ensureSession, setUnauthorizedHandler } from '@/stores/session'
import { resolveNavigation } from './guard'

export { postLoginPath, safeNext } from './guard'

declare module 'vue-router' {
  interface RouteMeta {
    /** true = 无需会话即可访问（仅登录页）。 */
    public?: boolean
    /** true = 需要会话。 */
    requiresAuth?: boolean
    /** 页面标题 i18n 键（document.title / 顶栏标题）。 */
    titleKey?: string
  }
}

/** 侧栏导航项（未落地的项 implemented=false，灰显 + 里程碑提示）。 */
export interface NavItem {
  /** 路由名（落地后与 route name 一致）。 */
  name: string
  path: string
  /** 菜单文案 i18n 键。 */
  titleKey: string
  /** @element-plus/icons-vue 的组件名（AppShell 内映射）。 */
  icon: string
  group: 'main' | 'system'
  implemented: boolean
  /** 计划里程碑（未落地时展示，如 'M3'）。 */
  milestone?: string
}

export const navItems: NavItem[] = [
  { name: 'overview', path: '/', titleKey: 'nav.overview', icon: 'Odometer', group: 'main', implemented: true },
  { name: 'volume', path: '/volume', titleKey: 'nav.volume', icon: 'Headset', group: 'main', implemented: true },
  { name: 'channels', path: '/channels', titleKey: 'nav.channels', icon: 'Files', group: 'main', implemented: true },
  { name: 'routing', path: '/routing', titleKey: 'nav.routing', icon: 'Share', group: 'main', implemented: false, milestone: 'M3' },
  { name: 'format', path: '/format', titleKey: 'nav.format', icon: 'DataLine', group: 'main', implemented: true },
  { name: 'calibration', path: '/calibration', titleKey: 'nav.calibration', icon: 'Timer', group: 'main', implemented: true },
  { name: 'system', path: '/system', titleKey: 'nav.system', icon: 'Monitor', group: 'system', implemented: true },
  { name: 'logs', path: '/system/logs', titleKey: 'nav.logs', icon: 'Document', group: 'system', implemented: true }
]

/** 侧栏分组（与 PLAN §11.2 一致：主功能 / 系统）。 */
export const navGroups: Array<{ key: 'main' | 'system'; labelKey: string; items: NavItem[] }> = [
  { key: 'main', labelKey: 'navGroup.main', items: navItems.filter((item) => item.group === 'main') },
  { key: 'system', labelKey: 'navGroup.system', items: navItems.filter((item) => item.group === 'system') }
]

const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
    component: LoginView,
    meta: { public: true, titleKey: 'login.pageTitle' }
  },
  {
    path: '/',
    component: AppShell,
    children: [
      {
        path: '',
        name: 'overview',
        component: OverviewView,
        meta: { requiresAuth: true, titleKey: 'nav.overview' }
      },
      {
        path: 'volume',
        name: 'volume',
        component: VolumeView,
        meta: { requiresAuth: true, titleKey: 'nav.volume' }
      },
      {
        path: 'format',
        name: 'format',
        component: FormatView,
        meta: { requiresAuth: true, titleKey: 'nav.format' }
      },
      {
        path: 'channels',
        name: 'channels',
        component: ChannelsView,
        meta: { requiresAuth: true, titleKey: 'nav.channels' }
      },
      {
        path: 'calibration',
        name: 'calibration',
        component: CalibrationView,
        meta: { requiresAuth: true, titleKey: 'nav.calibration' }
      },
      {
        path: 'calibration/wizard',
        name: 'calibration-wizard',
        component: CalibrationWizardView,
        meta: { requiresAuth: true, titleKey: 'calibration.wizard.pageTitle' }
      },
      {
        path: 'calibration/jobs/:id',
        name: 'calibration-job',
        component: CalibrationJobView,
        meta: { requiresAuth: true, titleKey: 'calibration.job.pageTitle' }
      },
      {
        path: 'system',
        name: 'system',
        component: SystemView,
        meta: { requiresAuth: true, titleKey: 'nav.system' }
      },
      {
        path: 'system/logs',
        name: 'logs',
        component: SystemLogsView,
        meta: { requiresAuth: true, titleKey: 'nav.logs' }
      }
    ]
  },
  { path: '/:pathMatch(.*)*', redirect: '/' }
]

export const router = createRouter({
  history: createWebHistory(),
  scrollBehavior: () => ({ top: 0 }),
  routes
})

router.beforeEach(async (to) => {
  const status = await ensureSession()
  const decision = resolveNavigation(
    {
      name: typeof to.name === 'string' ? to.name : null,
      fullPath: to.fullPath,
      query: to.query as Record<string, unknown>,
      meta: to.meta
    },
    status
  )

  if (decision.kind === 'login') {
    return decision.next === '/' ? { name: 'login' } : { name: 'login', query: { next: decision.next } }
  }
  if (decision.kind === 'redirect') {
    return decision.path
  }
  return true
})

/**
 * 受保护 API 返回 401（会话过期/被清除）时的统一跳转。
 * 只在需要认证的页面上保留 next；已经在登录页则不重复导航（避免循环）。
 */
setUnauthorizedHandler(() => {
  const current = router.currentRoute.value
  if (current.name === 'login') {
    return
  }
  const query: Record<string, string> = { expired: '1' }
  if (current.meta.requiresAuth === true) {
    query.next = current.fullPath
  }
  void router.replace({ name: 'login', query })
})