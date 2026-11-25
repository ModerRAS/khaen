import { createRouter, createWebHistory } from 'vue-router'
import AppShell from '@/components/AppShell.vue'
import LoginView from '@/views/LoginView.vue'
import OverviewView from '@/views/OverviewView.vue'
import VolumeView from '@/views/VolumeView.vue'
import RoutingView from '@/views/RoutingView.vue'
import FormatView from '@/views/FormatView.vue'
import CalibrationView from '@/views/CalibrationView.vue'
import SystemView from '@/views/SystemView.vue'
import { ensureSession, sessionState } from '@/stores/session'

export interface NavMeta {
  title: string
  short: string
  icon: string
  primary?: boolean
}

export const navRoutes: Array<{ name: string; path: string; meta: NavMeta }> = [
  { name: 'overview', path: '/', meta: { title: '概览', short: '概览', icon: 'Odometer', primary: true } },
  { name: 'volume', path: '/volume', meta: { title: '音量与静音', short: '音量', icon: 'Headset', primary: true } },
  { name: 'routing', path: '/routing', meta: { title: '路由矩阵', short: '路由', icon: 'Share', primary: true } },
  { name: 'format', path: '/format', meta: { title: '音频格式', short: '格式', icon: 'DataLine' } },
  { name: 'calibration', path: '/calibration', meta: { title: '校准', short: '校准', icon: 'Timer', primary: true } },
  { name: 'system', path: '/system', meta: { title: '系统信息', short: '系统', icon: 'Monitor' } }
]

export const router = createRouter({
  history: createWebHistory(),
  scrollBehavior: () => ({ top: 0 }),
  routes: [
    {
      path: '/login',
      name: 'login',
      component: LoginView,
      meta: { public: true, title: '登录' }
    },
    {
      path: '/',
      component: AppShell,
      children: navRoutes.map((route) => ({
        path: route.path === '/' ? '' : route.path.slice(1),
        name: route.name,
        component: {
          overview: OverviewView,
          volume: VolumeView,
          routing: RoutingView,
          format: FormatView,
          calibration: CalibrationView,
          system: SystemView
        }[route.name],
        meta: { title: route.meta.title }
      }))
    },
    { path: '/:pathMatch(.*)*', redirect: '/' }
  ]
})

let bootstrap: Promise<boolean> | null = null

router.beforeEach(async (to) => {
  document.title = to.meta.title ? `${String(to.meta.title)} · KHAEN 控制台` : 'KHAEN 控制台'
  if (to.meta.public) {
    if (to.name === 'login' && sessionState.authenticated) { return { path: '/' } }
    return true
  }
  if (!sessionState.ready) {
    if (!bootstrap) { bootstrap = ensureSession().finally(() => { bootstrap = null }) }
    await bootstrap
  }
  if (!sessionState.authenticated) {
    return { name: 'login', query: { redirect: to.fullPath } }
  }
  return true
})
