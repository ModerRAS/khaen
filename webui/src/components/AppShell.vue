<script setup lang="ts">
/**
 * 全局外壳（PLAN §11.2）：左侧导航 + 顶栏 + 内容区。
 *
 * - >=1280 侧栏展开 224px；<1280 强制折叠 64px（只留图标）；<768 变抽屉。
 * - 顶栏：页面标题 + 状态徽标组（接 /api/status 真实数据，缺失显示「—」）+ 刷新/主题/用户菜单。
 * - 内容区最大 1440px 居中，左右 24px。
 * - 全部导航项均已落地（无灰显/占位项）；页面能力差异由各页面按 /api/meta 自行如实标注。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch, type Component } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import {
  ArrowLeft,
  ArrowRight,
  Check,
  Connection,
  DataLine,
  Document,
  Files,
  Grid,
  Headset,
  Menu,
  Monitor,
  Moon,
  Loading,
  Odometer,
  Refresh,
  Share,
  Sunny,
  SwitchButton,
  Timer,
  User
} from '@element-plus/icons-vue'
import { navGroups, type NavItem } from '@/router'
import { ensureMeta } from '@/stores/meta'
import { useTheme, type ThemeMode } from '@/composables/useTheme'
import { useViewport } from '@/composables/useViewport'
import { setLocale } from '@/i18n'
import { logout } from '@/stores/session'
import { refreshStatus, statusState } from '@/stores/status'
import { isStatusStale, runStateKind, runStateLabelKey } from '@/api/status'
import { loadReleaseVersion, releaseVersionState } from '@/stores/version'
import { formatDecimal, toRateUnit } from '@/utils/format'
import StatusBadge from './StatusBadge.vue'
import type { StatusKind } from '@/types/ui'

const { t, locale } = useI18n({ useScope: 'global' })
const route = useRoute()
const router = useRouter()
const { isMobile, isNarrow } = useViewport()
const { mode: themeMode, resolved: themeResolved, setThemeMode, toggleTheme } = useTheme()

/** 菜单图标名 → 图标组件（不引第三方图标包）。 */
const ICONS: Record<string, Component> = {
  Odometer,
  Headset,
  Connection,
  SwitchButton,
  Files,
  Share,
  Grid,
  DataLine,
  Timer,
  Monitor,
  Document
}

const COLLAPSE_KEY = 'khaen.sidebar'

function readUserCollapsed(): boolean {
  try {
    return window.localStorage.getItem(COLLAPSE_KEY) === 'collapsed'
  } catch {
    return false
  }
}

const userCollapsed = ref(readUserCollapsed())
const drawerOpen = ref(false)
/** 登出请求进行中：菜单项禁用，避免重复触发。 */
const signingOut = ref(false)

/** <1280 自动折叠；用户显式折叠优先。 */
const collapsed = computed(() => !isMobile.value && (isNarrow.value || userCollapsed.value))

const pageTitle = computed(() => t(route.meta.titleKey ?? 'nav.overview'))

/* ------------------------------------------ 顶栏徽标 / 侧栏链路：接真实 /api/status */

const runStateBadge = computed<{ kind: StatusKind; value: string | undefined }>(() => {
  const current = statusState.data?.state ?? null
  const key = runStateLabelKey(current)
  const fallback = current !== null && current.value !== null ? current.value : undefined
  return { kind: runStateKind(current), value: key !== null ? t(key) : fallback }
})

const planBadge = computed<string | undefined>(() => {
  const version = statusState.data?.plan.version ?? null
  return version === null ? undefined : 'v' + String(version)
})

const planBadgeKind = computed<StatusKind>(() => (planBadge.value === undefined ? 'unknown' : 'safe'))

const deviceRateBadge = computed<string | undefined>(() => {
  const unit = toRateUnit(statusState.data?.format.device_rate ?? null)
  if (unit === null) {
    return undefined
  }
  const value = formatDecimal(unit.value, 2, locale.value === 'en-US' ? 'en-US' : 'zh-CN')
  return value === null ? undefined : value + ' ' + t(unit.unitKey)
})

const deviceRateBadgeKind = computed<StatusKind>(() => {
  if (deviceRateBadge.value === undefined) {
    return 'unknown'
  }
  return statusState.data?.format.rate_asserted === false ? 'muted' : 'safe'
})

const hostLinkHealth = computed<'ok' | 'stale' | 'unknown'>(() => {
  const current = statusState.data
  if (current === null) {
    return 'unknown'
  }
  if (isStatusStale(current)) {
    return 'stale'
  }
  return current.host_link.connected === true ? 'ok' : 'unknown'
})

const hostLinkDotClass = computed(() => {
  if (hostLinkHealth.value === 'ok') {
    return 'connection-dot--ok'
  }
  return hostLinkHealth.value === 'stale' ? 'connection-dot--warn' : ''
})

const hostLinkText = computed(() => {
  if (hostLinkHealth.value === 'ok') {
    return t('shell.hostLinkConnected')
  }
  return hostLinkHealth.value === 'stale' ? t('shell.hostLinkStale') : t('shell.hostLinkNotWired')
})

function onRefresh(): void {
  void refreshStatus()
}

function toggleCollapsed(): void {
  userCollapsed.value = !userCollapsed.value
  try {
    window.localStorage.setItem(COLLAPSE_KEY, userCollapsed.value ? 'collapsed' : 'expanded')
  } catch {
    // 持久化失败不影响本次会话。
  }
}

function navTitle(item: NavItem): string {
  return t(item.titleKey)
}

function navigate(item: NavItem): void {
  drawerOpen.value = false
  void router.push(item.path)
}

/** 登出：调 POST /api/auth/logout（store 负责清态），随后回登录页。 */
async function signOut(): Promise<void> {
  if (signingOut.value) {
    return
  }
  signingOut.value = true
  try {
    await logout()
  } finally {
    signingOut.value = false
    drawerOpen.value = false
    await router.replace({ name: 'login' })
  }
}

function onMenuCommand(command: unknown): void {
  if (typeof command !== 'string') {
    return
  }
  if (command === 'auth:logout') {
    void signOut()
    return
  }
  if (command === 'theme:system' || command === 'theme:dark' || command === 'theme:light') {
    const next: ThemeMode = command === 'theme:system' ? 'system' : command === 'theme:dark' ? 'dark' : 'light'
    setThemeMode(next)
    return
  }
  if (command === 'locale:zh-CN') {
    setLocale('zh-CN')
    return
  }
  if (command === 'locale:en-US') {
    setLocale('en-US')
  }
  if (command === 'system:password') {
    drawerOpen.value = false
    void router.push({ name: 'system', hash: '#system-password' })
  }
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') {
    drawerOpen.value = false
  }
}

watch(
  () => route.fullPath,
  () => {
    drawerOpen.value = false
  }
)

onMounted(() => {
  window.addEventListener('keydown', onKeydown)
  // 顶栏/侧栏版本号 = release 版本（来自匿名 /healthz；失败保持「—」，绝不拿 webui 版本冒充）。
  void loadReleaseVersion()
  // 能力快照预热（GET /api/meta）：页面首屏即可按真实能力渲染入口；失败保持 unknown 由页面标注。
  void ensureMeta()
})
onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown))
</script>

<template>
  <div
    class="app-shell"
    :class="{
      'app-shell--collapsed': collapsed,
      'app-shell--mobile': isMobile,
      'is-drawer-open': drawerOpen
    }"
  >
    <header class="app-header">
      <el-button
        v-if="isMobile"
        text
        class="touch-target"
        :aria-label="t('shell.menu')"
        @click="drawerOpen = true"
      >
        <el-icon :size="20"><Menu /></el-icon>
      </el-button>
      <el-button
        v-else
        text
        class="touch-target"
        :aria-label="collapsed ? t('shell.expand') : t('shell.collapse')"
        @click="toggleCollapsed"
      >
        <el-icon :size="18"><ArrowRight v-if="collapsed" /><ArrowLeft v-else /></el-icon>
      </el-button>

      <span class="topbar__title">{{ pageTitle }}</span>
      <span
        class="topbar__version mono"
        :title="t('topbar.versionHint')"
        :aria-label="t('topbar.version') + ' ' + (releaseVersionState.label ?? t('common.dash'))"
      >
        <span class="topbar__version-label">{{ t('topbar.version') }}</span>
        <span>{{ releaseVersionState.label ?? t('common.dash') }}</span>
      </span>
      <span class="topbar__spacer" />

      <div class="topbar__badges">
        <StatusBadge :kind="runStateBadge.kind" :label="t('topbar.runState')" :value="runStateBadge.value" />
        <StatusBadge :kind="planBadgeKind" :label="t('topbar.plan')" :value="planBadge" />
        <StatusBadge :kind="deviceRateBadgeKind" :label="t('topbar.deviceRate')" :value="deviceRateBadge" />
      </div>

      <div class="topbar__actions">
        <el-tooltip :content="t('common.refresh')" placement="bottom">
          <el-button
            text
            class="touch-target"
            :aria-label="t('common.refresh')"
            :loading="statusState.refreshing"
            :disabled="statusState.refreshing"
            @click="onRefresh"
          >
            <el-icon v-if="!statusState.refreshing" :size="18"><Refresh /></el-icon>
          </el-button>
        </el-tooltip>

        <el-tooltip
          :content="themeResolved === 'dark' ? t('theme.toggleToLight') : t('theme.toggleToDark')"
          placement="bottom"
        >
          <el-button text class="touch-target" :aria-label="t('topbar.themeToggle')" @click="toggleTheme">
            <el-icon :size="18"><Sunny v-if="themeResolved === 'dark'" /><Moon v-else /></el-icon>
          </el-button>
        </el-tooltip>

        <el-dropdown trigger="click" @command="onMenuCommand">
          <el-button text class="touch-target" :aria-label="t('topbar.userMenu')">
            <el-icon :size="18"><User /></el-icon>
          </el-button>
          <template #dropdown>
            <el-dropdown-menu>
              <el-dropdown-item command="theme:system">
                <span class="menu-line">
                  <el-icon v-if="themeMode === 'system'"><Check /></el-icon>
                  <span>{{ t('theme.system') }}</span>
                </span>
              </el-dropdown-item>
              <el-dropdown-item command="theme:dark">
                <span class="menu-line">
                  <el-icon v-if="themeMode === 'dark'"><Check /></el-icon>
                  <span>{{ t('theme.dark') }}</span>
                </span>
              </el-dropdown-item>
              <el-dropdown-item command="theme:light">
                <span class="menu-line">
                  <el-icon v-if="themeMode === 'light'"><Check /></el-icon>
                  <span>{{ t('theme.light') }}</span>
                </span>
              </el-dropdown-item>
              <el-dropdown-item divided command="locale:zh-CN">
                <span class="menu-line">
                  <el-icon v-if="locale === 'zh-CN'"><Check /></el-icon>
                  <span>{{ t('locale.zhCN') }}</span>
                </span>
              </el-dropdown-item>
              <el-dropdown-item command="locale:en-US">
                <span class="menu-line">
                  <el-icon v-if="locale === 'en-US'"><Check /></el-icon>
                  <span>{{ t('locale.enUS') }}</span>
                </span>
              </el-dropdown-item>
              <el-dropdown-item divided command="system:password">
                <span class="menu-line">
                  <span>{{ t('topbar.changePassword') }}</span>
                </span>
              </el-dropdown-item>
              <el-dropdown-item divided command="auth:logout" :disabled="signingOut">
                <span class="menu-line">
                  <span>{{ t('topbar.logout') }}</span>
                  <el-icon v-if="signingOut" class="is-loading"><Loading /></el-icon>
                </span>
              </el-dropdown-item>
            </el-dropdown-menu>
          </template>
        </el-dropdown>
      </div>
    </header>

    <aside v-if="!isMobile || drawerOpen" class="app-sidebar">
      <div class="brand">
        <span class="brand__mark" aria-hidden="true">KH</span>
        <div v-if="!collapsed" class="brand__text">
          <div class="brand__name">{{ t('app.name') }}</div>
          <div class="brand__version">{{ t('app.version', { version: releaseVersionState.label ?? t('common.dash') }) }}</div>
        </div>
      </div>

      <template v-for="group in navGroups" :key="group.key">
        <div v-if="!collapsed" class="nav-group__label">{{ t(group.labelKey) }}</div>
        <nav class="nav">
          <a
            v-for="item in group.items"
            :key="item.name"
            class="nav-item"
            :class="{ 'is-active': route.name === item.name }"
            :href="item.path"
            :title="navTitle(item)"
            @click.prevent="navigate(item)"
          >
            <el-icon :size="18"><component :is="ICONS[item.icon]" /></el-icon>
            <span v-if="!collapsed" class="nav-item__label">{{ t(item.titleKey) }}</span>
          </a>
        </nav>
      </template>

      <div class="sidebar-footer">
        <span class="connection-dot" :class="hostLinkDotClass" aria-hidden="true" />
        <span v-if="!collapsed">{{ hostLinkText }}</span>
      </div>
    </aside>

    <div v-if="drawerOpen" class="drawer-backdrop" @click="drawerOpen = false" />

    <main class="app-main">
      <div class="app-content">
        <router-view />
      </div>
    </main>
  </div>
</template>

<style scoped>
.topbar__version {
  display: inline-flex;
  align-items: baseline;
  gap: var(--khaen-space-1);
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

/* 窄屏只留版本值（v0.1.2），避免「版本 v0…」被截断 */
@media (max-width: 767px) {
  .topbar__version-label {
    display: none;
  }
}

.menu-line {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.menu-line__flag {
  margin-left: auto;
  padding-left: 16px;
  color: var(--khaen-text-muted);
  font-size: 11px;
}
</style>
