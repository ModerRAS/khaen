<script setup lang="ts">
/**
 * 全局外壳（PLAN §11.2）：左侧导航 + 顶栏 + 内容区。
 *
 * - >=1280 侧栏展开 224px；<1280 强制折叠 64px（只留图标）；<768 变抽屉。
 * - 顶栏：页面标题 + 状态徽标组（M2 起接真实数据，现在一律「未接入」）+ 刷新/主题/用户菜单。
 * - 内容区最大 1440px 居中，左右 24px；顶部常驻 M0/M1 骨架提示条。
 * - 未落地的导航项灰显并标注计划里程碑，不做假跳转。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch, type Component } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import {
  ArrowLeft,
  ArrowRight,
  Check,
  DataLine,
  Document,
  Files,
  Headset,
  Menu,
  Monitor,
  Moon,
  Loading,
  Odometer,
  Refresh,
  Share,
  Sunny,
  Timer,
  User
} from '@element-plus/icons-vue'
import { navGroups, type NavItem } from '@/router'
import { useTheme, type ThemeMode } from '@/composables/useTheme'
import { useViewport } from '@/composables/useViewport'
import { setLocale } from '@/i18n'
import { logout } from '@/stores/session'
import { APP_VERSION } from '@/config'
import StatusBadge from './StatusBadge.vue'

const { t, locale } = useI18n({ useScope: 'global' })
const route = useRoute()
const router = useRouter()
const { isMobile, isNarrow } = useViewport()
const { mode: themeMode, resolved: themeResolved, setThemeMode, toggleTheme } = useTheme()

/** 菜单图标名 → 图标组件（不引第三方图标包）。 */
const ICONS: Record<string, Component> = {
  Odometer,
  Headset,
  Files,
  Share,
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


function toggleCollapsed(): void {
  userCollapsed.value = !userCollapsed.value
  try {
    window.localStorage.setItem(COLLAPSE_KEY, userCollapsed.value ? 'collapsed' : 'expanded')
  } catch {
    // 持久化失败不影响本次会话。
  }
}

function navTitle(item: NavItem): string {
  return item.implemented ? t(item.titleKey) : t('shell.navDisabled', { milestone: item.milestone ?? 'M?' })
}

function navigate(item: NavItem): void {
  if (!item.implemented) {
    return
  }
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

onMounted(() => window.addEventListener('keydown', onKeydown))
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
      <span class="topbar__spacer" />

      <div class="topbar__badges">
        <StatusBadge kind="unknown" :label="t('topbar.runState')" />
        <StatusBadge kind="unknown" :label="t('topbar.plan')" />
        <StatusBadge kind="unknown" :label="t('topbar.deviceRate')" />
      </div>

      <div class="topbar__actions">
        <el-tooltip :content="t('topbar.refreshTodo', { milestone: 'M2' })" placement="bottom">
          <el-button text class="touch-target" :aria-label="t('common.refresh')" disabled>
            <el-icon :size="18"><Refresh /></el-icon>
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
              <el-dropdown-item divided disabled>
                <span class="menu-line">
                  <span>{{ t('topbar.changePassword') }}</span>
                  <span class="menu-line__flag">{{ t('topbar.comingSoon', { milestone: 'M6' }) }}</span>
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
          <div class="brand__version">{{ t('app.version', { version: APP_VERSION }) }}</div>
        </div>
      </div>

      <template v-for="group in navGroups" :key="group.key">
        <div v-if="!collapsed" class="nav-group__label">{{ t(group.labelKey) }}</div>
        <nav class="nav">
          <component
            :is="item.implemented ? 'a' : 'div'"
            v-for="item in group.items"
            :key="item.name"
            class="nav-item"
            :class="{
              'is-active': item.implemented && route.name === item.name,
              'is-disabled': !item.implemented
            }"
            :href="item.implemented ? item.path : undefined"
            :title="navTitle(item)"
            @click.prevent="navigate(item)"
          >
            <el-icon :size="18"><component :is="ICONS[item.icon]" /></el-icon>
            <span v-if="!collapsed" class="nav-item__label">{{ t(item.titleKey) }}</span>
            <span v-if="!collapsed && !item.implemented" class="nav-item__flag">
              {{ item.milestone }}
            </span>
          </component>
        </nav>
      </template>

      <div class="sidebar-footer">
        <span class="connection-dot" aria-hidden="true" />
        <span v-if="!collapsed">{{ t('shell.hostLinkNotWired') }}</span>
      </div>
    </aside>

    <div v-if="drawerOpen" class="drawer-backdrop" @click="drawerOpen = false" />

    <main class="app-main">
      <div class="app-content">
        <div class="banner banner--info" role="status">{{ t('shell.stubBanner') }}</div>
        <router-view />
      </div>
    </main>
  </div>
</template>

<style scoped>
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
