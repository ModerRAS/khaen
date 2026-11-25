<script setup lang="ts">
// 登录 / 首次设置：沿用现有 REST 接口；错误回显；token 登录折叠为高级选项。
import { computed, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { Lock, Key, Connection, InfoFilled, Warning } from '@element-plus/icons-vue'
import { api } from '@/api/client'
import {
  clearSessionError, login, refreshAuthStatus, sessionState, setupPassword, useToken, verifySession
} from '@/stores/session'
import { themeState, toggleTheme } from '@/composables/useTheme'
import { Moon, Sunny } from '@element-plus/icons-vue'
import type { HealthBody } from '@/api/types'

const route = useRoute()
const router = useRouter()
const password = ref('')
const confirm = ref('')
const token = ref(sessionState.token)
const advancedOpen = ref<string[]>([])
const health = ref<HealthBody | null>(null)
const healthError = ref('')

const setupMode = computed(() => !!sessionState.authStatus?.setup_required)
const minLen = computed(() => sessionState.authStatus?.min_password_len ?? 8)
const recommendedLen = computed(() => sessionState.authStatus?.recommended_password_len ?? 24)
const tokenFallback = computed(() => sessionState.authStatus?.token_fallback === true)

function redirectTarget(): string {
  const raw = route.query.redirect
  if (typeof raw === 'string' && raw.startsWith('/') && !raw.startsWith('//')) { return raw }
  return '/'
}

onMounted(async () => {
  clearSessionError()
  await refreshAuthStatus()
  const authenticated = await verifySession()
  if (authenticated) {
    void router.replace(redirectTarget())
    return
  }
  try {
    health.value = (await api.healthz()).data
  } catch (error) {
    healthError.value = error instanceof Error ? error.message : String(error)
  }
})

async function submitPassword(): Promise<void> {
  clearSessionError()
  const value = password.value
  if (!value) { ElMessage.warning('请输入管理员密码'); return }
  if (setupMode.value) {
    if (value.length < minLen.value) { ElMessage.warning(`密码至少 ${minLen.value} 位`); return }
    if (value !== confirm.value) { ElMessage.warning('两次输入的密码不一致'); return }
    const ok = await setupPassword(value, confirm.value)
    if (ok) {
      ElMessage.success('管理员密码已设置并登录')
      password.value = ''
      confirm.value = ''
      void router.replace(redirectTarget())
    }
    return
  }
  const ok = await login(value)
  if (ok) {
    ElMessage.success('登录成功')
    password.value = ''
    void router.replace(redirectTarget())
  }
}

async function submitToken(): Promise<void> {
  clearSessionError()
  if (!token.value.trim()) { ElMessage.warning('请输入 API token'); return }
  const ok = await useToken(token.value)
  if (ok) {
    ElMessage.success('token 会话验证通过')
    void router.replace(redirectTarget())
  }
}
</script>

<template>
  <div class="login-page">
    <div class="login-card">
      <div class="login-brand">
        <span class="login-brand__mark">KHAEN</span>
        <div style="min-width: 0">
          <h1 class="login-title">RT-first DSP 控制台</h1>
          <p class="login-sub">Vue 3 · Element Plus · 同源 REST 会话（无外部资源）</p>
        </div>
        <el-button text class="right" :aria-label="themeState.theme === 'dark' ? '切换浅色' : '切换深色'" @click="toggleTheme">
          <el-icon :size="18"><Sunny v-if="themeState.theme === 'dark'" /><Moon v-else /></el-icon>
        </el-button>
      </div>

      <div v-if="setupMode" class="banner banner--warn" style="font-size: 12.5px; margin-bottom: 10px">
        <el-icon style="margin-top: 2px"><Warning /></el-icon>
        <div>
          <div class="banner__title">注意：后端当前 setup_required=true（未配置管理员密码）</div>
          <div class="banner__body">
            若设备此前已设置过密码，出现本提示通常说明后端 auth 文件被外部重启/部署变更（不是本页面造成的）。
            请先核对部署状态再设置密码——在本页提交会写入后端 auth（生产设备等同重设管理员密码）。
          </div>
        </div>
      </div>

      <div class="banner banner--info" style="font-size: 12.5px">
        <el-icon style="margin-top: 2px"><InfoFilled /></el-icon>
        <div>
          <div class="banner__title">{{ setupMode ? '首次使用：设置管理员密码' : '管理员密码登录' }}</div>
          <div class="banner__body">
            {{ setupMode
              ? `当前后端未设置管理员密码（最少 ${minLen} 位，建议 ${recommendedLen} 位以上）。设置后自动登录。`
              : '使用设备管理员密码登录；登录凭据只在本机浏览器表单中提交，界面不回显、不落盘。' }}
          </div>
        </div>
      </div>

      <form class="login-form" @submit.prevent="submitPassword">
        <el-input
          v-model="password"
          type="password"
          size="large"
          :placeholder="setupMode ? '设置管理员密码' : '管理员密码'"
          show-password
          autocomplete="current-password"
          :prefix-icon="Lock"
          clearable
        />
        <el-input
          v-if="setupMode"
          v-model="confirm"
          type="password"
          size="large"
          placeholder="再次输入密码确认"
          show-password
          autocomplete="new-password"
          :prefix-icon="Lock"
        />
        <el-button
          type="primary"
          size="large"
          native-type="submit"
          :loading="sessionState.busy"
          style="width: 100%"
        >
          {{ setupMode ? '设置密码并登录' : '登录' }}
        </el-button>
      </form>

      <el-alert
        v-if="sessionState.error"
        type="error"
        :closable="false"
        show-icon
        style="margin-top: 14px"
        :title="sessionState.error"
      >
        <template #default>
          <span v-if="sessionState.errorCode" class="mono">code={{ sessionState.errorCode }}</span>
        </template>
      </el-alert>

      <div class="login-meta">
        <span class="chip" :class="sessionState.authStatus?.password_configured ? 'chip--ok' : 'chip--warn'">
          密码{{ sessionState.authStatus?.password_configured ? '已设置' : '未设置' }}
        </span>
        <span class="chip">token_fallback={{ tokenFallback }}</span>
        <span class="chip" :class="sessionState.authStatus?.mock ? 'chip--warn' : ''">
          mock={{ sessionState.authStatus?.mock ?? '—' }}
        </span>
        <span v-if="health" class="chip chip--ok">daemon {{ health.version }} · up {{ Math.round(health.uptime_secs / 60) }} min</span>
        <span v-else-if="healthError" class="chip chip--warn">/healthz 不可达</span>
      </div>

      <el-collapse v-model="advancedOpen" style="margin-top: 14px">
        <el-collapse-item name="token">
          <template #title>
            <span class="hint" style="display: inline-flex; align-items: center; gap: 6px">
              <el-icon><Key /></el-icon> 高级：API token 会话验证
            </span>
          </template>
          <div style="display: flex; flex-direction: column; gap: 10px">
            <div class="hint">
              当前后端 token_fallback={{ tokenFallback }}。token 仅用于脚本/调试；实现未启用时登录会被 401 拒绝，
              页面会如实显示错误，不做伪装。
            </div>
            <el-input v-model="token" type="password" placeholder="X-KHAEN-Token" show-password autocomplete="off" />
            <el-button :loading="sessionState.busy" style="width: 100%" @click="submitToken">用 token 验证会话</el-button>
          </div>
        </el-collapse-item>
      </el-collapse>

      <p class="login-foot">
        本页面所有资源随包本地提供，不访问 CDN 或外部字体。<br />
        公开探针：<a href="/healthz" target="_blank" rel="noopener">/healthz</a> ·
        WS：/ws（需同一登录会话；预览代理可能返回 501，页面自动降级为 REST）
      </p>
    </div>
  </div>
</template>

<style scoped>
.right { margin-left: auto; }
</style>