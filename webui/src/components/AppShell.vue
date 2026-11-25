<script setup lang="ts">
// 应用外壳：桌面常驻侧边栏 / 平板窄栏 / 手机抽屉 + 底部导航；顶栏常驻引擎真实性徽标。
import { computed, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { ElMessageBox } from 'element-plus'
import {
  Odometer, Headset, Share, DataLine, Timer, Monitor, Menu, Close,
  Moon, Sunny, SwitchButton, Connection, Grid
} from '@element-plus/icons-vue'
import { navRoutes } from '@/router'
import { sessionState, logout } from '@/stores/session'
import { runtimeState, useRuntimePolling } from '@/stores/runtime'
import { themeState, toggleTheme } from '@/composables/useTheme'
import { useViewport } from '@/composables/useViewport'
import { wsState } from '@/stores/live'
import TruthTag from './TruthTag.vue'
import { APP_VERSION } from '@/utils/build'

const iconMap: Record<string, unknown> = { Odometer, Headset, Share, DataLine, Timer, Monitor, Connection, Grid }
const route = useRoute()
const router = useRouter()
const { isMobile, isTablet, isDesktop } = useViewport()
const drawerOpen = ref(false)

useRuntimePolling(5000)

const shellClass = computed(() => ({
  'shell--mobile': isMobile.value,
  'shell--tablet': isTablet.value
}))

const pageTitle = computed(() => String(route.meta.title ?? '控制台'))
const engineChip = computed(() => {
  const status = runtimeState.status
  if (!status) { return { text: '状态未加载', cls: '' } }
  if (status.synthetic) { return { text: `engine=${status.engine} · synthetic`, cls: 'chip--warn' } }
  if (status.actual) { return { text: `engine=${status.engine} · actual`, cls: 'chip--ok' } }
  return { text: `engine=${status.engine}`, cls: '' }
})
const wsChip = computed(() => {
  switch (wsState.phase) {
    case 'open': return { text: 'WS 已连接', cls: 'chip--ok' }
    case 'connecting': return { text: 'WS 连接中', cls: 'chip--warn' }
    case 'unsupported': return { text: 'WS 不可用（已降级 REST）', cls: 'chip--warn' }
    case 'error': return { text: 'WS 错误（已降级 REST）', cls: 'chip--warn' }
    case 'closed': return { text: 'WS 已断开', cls: '' }
    default: return { text: 'WS 未连接', cls: '' }
  }
})

const primaryRoutes = navRoutes.filter((item) => item.meta.primary)
const overflowRoutes = navRoutes.filter((item) => !item.meta.primary)

function navigate(name: string): void {
  drawerOpen.value = false
  void router.push({ name })
}

async function doLogout(): Promise<void> {
  try {
    await ElMessageBox.confirm('退出后将清除本机会话 Cookie，需要重新登录。', '退出登录', {
      confirmButtonText: '退出',
      cancelButtonText: '取消',
      type: 'warning'
    })
  } catch { return }
  await logout()
  drawerOpen.value = false
  void router.replace({ name: 'login' })
}

watch(() => route.fullPath, () => { drawerOpen.value = false })
</script>

<template>
  <div class="shell" :class="shellClass">
    <header class="app-header">
      <el-button v-if="isMobile" text class="hamburger" aria-label="打开导航" @click="drawerOpen = true">
        <el-icon :size="22"><Menu /></el-icon>
      </el-button>
      <div class="brand">
        <span class="brand__mark hide-mobile">KHAEN</span>
        <div style="min-width: 0">
          <div class="brand__title">{{ isMobile ? pageTitle : 'KHAEN RT-first 控制台' }}</div>
          <div class="brand__sub hide-mobile">
            {{ isDesktop ? '控制/校准 · REST + WS（同源）' : '控制/校准' }}
          </div>
        </div>
      </div>
      <span class="chip" :class="engineChip.cls" :title="'后端 engine 身份（原样展示）'">{{ engineChip.text }}</span>
      <span v-if="isDesktop" class="chip" :class="wsChip.cls">{{ wsChip.text }}</span>
      <el-button text class="icon-btn" :aria-label="themeState.theme === 'dark' ? '切换浅色' : '切换深色'" @click="toggleTheme">
        <el-icon :size="18"><Sunny v-if="themeState.theme === 'dark'" /><Moon v-else /></el-icon>
      </el-button>
      <el-button v-if="isDesktop" text class="icon-btn" aria-label="退出登录" @click="doLogout">
        <el-icon :size="18"><SwitchButton /></el-icon>
      </el-button>
    </header>

    <aside v-if="!isMobile" class="app-aside">
      <div class="nav-group__label">控制台</div>
      <nav class="nav">
        <a
          v-for="item in navRoutes"
          :key="item.name"
          class="nav-item"
          :class="{ 'is-active': route.name === item.name }"
          :href="item.path"
          :title="item.meta.title"
          @click.prevent="navigate(item.name)"
        >
          <el-icon class="nav-item__icon"><component :is="iconMap[item.meta.icon]" /></el-icon>
          <span class="nav-item__label">{{ item.meta.title }}</span>
        </a>
      </nav>
      <div style="margin-top: auto; display: flex; flex-direction: column; gap: 8px; padding-top: 16px">
        <div class="hint" style="padding: 0 10px">
          <div v-if="!isTablet">登录：{{ sessionState.session?.auth_kind === 'token' ? 'token' : '密码会话' }}</div>
          <div v-if="!isTablet">构建：v{{ APP_VERSION }}</div>
          <div v-if="runtimeState.status && !isTablet" class="mono" style="word-break: break-all">state={{ runtimeState.status.runtime_state }}</div>
        </div>
        <el-button v-if="!isDesktop" text size="small" @click="doLogout">
          <el-icon><SwitchButton /></el-icon>
          <span style="margin-left: 4px"></span>
        </el-button>
      </div>
    </aside>

    <main class="app-main">
      <router-view v-slot="{ Component }">
        <transition name="fade" mode="out-in">
          <component :is="Component" />
        </transition>
      </router-view>
    </main>

    <nav v-if="isMobile" class="app-bottom">
      <button
        v-for="item in primaryRoutes"
        :key="item.name"
        class="bottom-item"
        :class="{ 'is-active': route.name === item.name }"
        @click="navigate(item.name)"
      >
        <el-icon><component :is="iconMap[item.meta.icon]" /></el-icon>
        <span>{{ item.meta.short }}</span>
      </button>
      <button class="bottom-item" :class="{ 'is-active': overflowRoutes.some((r) => r.name === route.name) }" @click="drawerOpen = true">
        <el-icon><Grid /></el-icon>
        <span>更多</span>
      </button>
    </nav>

    <el-drawer v-model="drawerOpen" direction="ltr" size="272px" :with-header="false">
      <div style="display: flex; flex-direction: column; height: 100%">
        <div class="brand" style="margin-bottom: 14px">
          <span class="brand__mark">KHAEN</span>
          <div>
            <div class="brand__title">RT-first 控制台</div>
            <div class="brand__sub">v{{ APP_VERSION }} · Vue 3</div>
          </div>
          <el-button text class="icon-btn right" aria-label="关闭" @click="drawerOpen = false">
            <el-icon><Close /></el-icon>
          </el-button>
        </div>
        <div class="nav">
          <a
            v-for="item in navRoutes"
            :key="item.name"
            class="nav-item"
            :class="{ 'is-active': route.name === item.name }"
            :href="item.path"
            @click.prevent="navigate(item.name)"
          >
            <el-icon class="nav-item__icon"><component :is="iconMap[item.meta.icon]" /></el-icon>
            <span class="nav-item__label">{{ item.meta.title }}</span>
          </a>
        </div>
        <div style="margin-top: auto; display: flex; flex-direction: column; gap: 10px; padding-top: 18px">
          <div class="banner" style="font-size: 12px">
            <div>
              <div class="banner__title">后端真实性</div>
              <div class="banner__body">
                <TruthTag
                  :engine="runtimeState.status?.engine"
                  :synthetic="runtimeState.status?.synthetic"
                  :actual="runtimeState.status?.actual"
                  :mode-services="runtimeState.status?.mode_services"
                  compact
                />
              </div>
            </div>
          </div>
          <el-button style="width: 100%" @click="toggleTheme">
            <el-icon style="margin-right: 6px"><Sunny v-if="themeState.theme === 'dark'" /><Moon v-else /></el-icon>
            {{ themeState.theme === 'dark' ? '切换浅色主题' : '切换深色主题' }}
          </el-button>
          <el-button style="width: 100%" @click="doLogout">
            <el-icon style="margin-right: 6px"><SwitchButton /></el-icon>
            退出登录
          </el-button>
        </div>
      </div>
    </el-drawer>
  </div>
</template>

<style scoped>
.icon-btn { padding: 0 6px; min-height: 36px; }
.hamburger { padding: 0 4px; min-height: 40px; }
@media (max-width: 1199px) {
  .app-header :deep(.el-button) { min-height: 44px; }
  .icon-btn, .hamburger { min-height: 44px; min-width: 44px; }
}
</style>