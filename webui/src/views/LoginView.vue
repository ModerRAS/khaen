<script setup lang="ts">
/**
 * 登录 / 初始化（PLAN §6.1 / §11.4）——M1 起接真实认证 API。
 *
 * 启动：GET /api/auth/status（经 stores/session.ensureSession）决定形态：
 * - configured=false                         -> 初始化表单（密码 + 确认，>= MIN_PASSWORD_LENGTH 且一致）；
 * - configured=true && authenticated=false    -> 登录表单（密码）；
 * - authenticated=true                        -> 直接回跳 next（只接受站内相对路径）；
 * - 后端不可达（configured=null）              -> 明确的不可达提示 + 重试（不假装任何状态）。
 *
 * 交互：回车提交；提交中按钮 loading 且表单禁用；错误走 role=alert/aria-live；
 * 401 密码错误、429 限速、409 已初始化（切到登录态并提示）按 PLAN §6.1 映射成本地化文案。
 * 底部固定展示 **release 版本** + 前端构建标识（release 版本来自匿名 /healthz，未知显示「—」；
 * 构建标识来自 KHAEN_WEBUI_BUILD_ID，无时间戳）。
 */
import { computed, nextTick, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { Loading } from '@element-plus/icons-vue'
import { APP_BUILD_ID, MIN_PASSWORD_LENGTH } from '@/config'
import { setLocale } from '@/i18n'
import { validateCredentials } from '@/auth/validation'
import { classifyAuthError } from '@/api/errors'
import { ensureSession, login, sessionState, setup } from '@/stores/session'
import { loadReleaseVersion, releaseVersionState } from '@/stores/version'
import { postLoginPath } from '@/router/guard'

type Phase = 'checking' | 'setup' | 'signin' | 'unavailable'

const { t, locale } = useI18n({ useScope: 'global' })
const route = useRoute()
const router = useRouter()

const phase = ref<Phase>('checking')
const password = ref('')
const confirm = ref('')
const loading = ref(false)
const errorKey = ref<string | null>(null)
const errorDetail = ref('')
const noticeKey = ref<string | null>(null)
const passwordInput = ref<{ focus: () => void } | null>(null)

const isSetup = computed(() => phase.value === 'setup')
const showForm = computed(() => phase.value === 'setup' || phase.value === 'signin')

const headingTitle = computed(() => {
  if (isSetup.value) {
    return t('login.title.setup')
  }
  if (phase.value === 'signin') {
    return t('login.title.login')
  }
  return t('app.name')
})

const headingDesc = computed(() => {
  if (isSetup.value) {
    return t('login.desc.setup', { min: MIN_PASSWORD_LENGTH })
  }
  if (phase.value === 'signin') {
    return t('login.desc.login')
  }
  return t('app.tagline')
})

const submitLabel = computed(() => t(isSetup.value ? 'login.action.submitSetup' : 'login.action.submitLogin'))
const errorText = computed(() => (errorKey.value !== null ? t(errorKey.value, { min: MIN_PASSWORD_LENGTH }) : ''))
const noticeText = computed(() => (noticeKey.value !== null ? t(noticeKey.value) : ''))
const localeLabel = computed(() => (locale.value === 'en-US' ? t('locale.enUS') : t('locale.zhCN')))
/** 页脚版本 = release 版本（来自 /healthz；未知显示「—」，不拿 webui 版本冒充）。 */
const releaseVersionText = computed(() => releaseVersionState.label ?? t('common.dash'))

function toggleLocale(): void {
  setLocale(locale.value === 'en-US' ? 'zh-CN' : 'en-US')
}

async function focusPassword(): Promise<void> {
  await nextTick()
  passwordInput.value?.focus()
}

/** 拉取认证状态并决定页面形态；force=true 用于"重试"按钮（绕过缓存）。 */
async function bootstrap(force = false): Promise<void> {
  phase.value = 'checking'
  errorKey.value = null
  errorDetail.value = ''
  const status = await ensureSession(force ? { force: true } : undefined)
  if (status === 'authenticated') {
    await router.replace(postLoginPath(route.query.next))
    return
  }
  if (sessionState.configured === false) {
    phase.value = 'setup'
  } else if (sessionState.configured === true) {
    phase.value = 'signin'
  } else {
    phase.value = 'unavailable'
  }
  if (showForm.value) {
    await focusPassword()
  }
}

function retry(): void {
  if (phase.value === 'checking') {
    return
  }
  void bootstrap(true)
}

async function submit(): Promise<void> {
  if (loading.value || !showForm.value) {
    return
  }
  errorKey.value = null
  errorDetail.value = ''
  noticeKey.value = null

  const validation = validateCredentials(
    isSetup.value ? 'setup' : 'signin',
    password.value,
    confirm.value,
    MIN_PASSWORD_LENGTH
  )
  if (!validation.ok) {
    errorKey.value = validation.key
    return
  }

  loading.value = true
  try {
    const result = isSetup.value ? await setup(password.value, confirm.value) : await login(password.value)
    if (result.ok) {
      password.value = ''
      confirm.value = ''
      await router.replace(postLoginPath(route.query.next))
      return
    }

    const info = classifyAuthError(result.error, isSetup.value ? 'setup' : 'login')
    if (info.code === 'already_configured') {
      // 409：服务端已经有管理员密码（例如另一个窗口刚完成初始化）——直接切到登录态。
      phase.value = 'signin'
      noticeKey.value = 'login.notice.alreadyConfigured'
      confirm.value = ''
      await focusPassword()
      return
    }
    if (info.code === 'not_configured') {
      phase.value = 'setup'
      errorKey.value = info.i18nKey
      return
    }
    errorKey.value = info.i18nKey
    errorDetail.value = info.detail ?? ''
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  if (route.query.changed === '1') {
    noticeKey.value = 'login.notice.passwordChanged'
  } else if (route.query.expired === '1') {
    noticeKey.value = 'login.notice.sessionExpired'
  }
  void loadReleaseVersion()
  void bootstrap()
})
</script>

<template>
  <div class="login-page">
    <section class="login-card" :aria-busy="loading">
      <div class="login-card__brand">
        <span class="login-card__mark" aria-hidden="true">KH</span>
        <div>
          <h1 class="login-card__title">{{ headingTitle }}</h1>
          <p class="login-card__desc">{{ headingDesc }}</p>
        </div>
      </div>

      <div v-if="phase === 'checking'" class="login-card__status" role="status" aria-live="polite">
        <el-icon class="is-loading" :size="16"><Loading /></el-icon>
        <span>{{ t('login.state.checking') }}</span>
      </div>

      <div v-else-if="phase === 'unavailable'" class="login-card__status login-card__status--error" role="alert" aria-live="assertive">
        <p class="login-card__status-title">{{ t('login.state.unavailable') }}</p>
        <p class="login-card__status-detail">{{ t('error.auth.network') }}</p>
        <el-button type="primary" class="login-card__submit touch-target" @click="retry">
          {{ t('login.action.retry') }}
        </el-button>
      </div>

      <el-form v-else-if="showForm" label-position="top" class="login-card__form" @submit.prevent="submit">
        <p v-if="noticeText" class="login-card__notice" role="status" aria-live="polite">{{ noticeText }}</p>

        <el-form-item :label="t('login.field.password')">
          <el-input
            ref="passwordInput"
            v-model="password"
            type="password"
            show-password
            :disabled="loading"
            :placeholder="t('login.placeholder.password')"
            :autocomplete="isSetup ? 'new-password' : 'current-password'"
            :aria-label="t('login.field.password')"
          />
        </el-form-item>

        <el-form-item v-if="isSetup" :label="t('login.field.confirm')">
          <el-input
            v-model="confirm"
            type="password"
            show-password
            :disabled="loading"
            :placeholder="t('login.placeholder.confirm')"
            autocomplete="new-password"
            :aria-label="t('login.field.confirm')"
          />
        </el-form-item>

        <p class="login-card__error" role="alert" aria-live="assertive">{{ errorText }}</p>
        <p v-if="errorDetail" class="login-card__detail">{{ errorDetail }}</p>

        <el-button
          type="primary"
          native-type="submit"
          class="login-card__submit touch-target"
          :loading="loading"
          :disabled="loading"
        >
          {{ submitLabel }}
        </el-button>
      </el-form>

      <div class="login-card__footer">
        <span>{{ t('login.footer.version', { version: releaseVersionText }) }}</span>
        <span>{{ t('login.footer.build', { id: APP_BUILD_ID }) }}</span>
        <button type="button" class="login-card__locale" @click="toggleLocale">{{ localeLabel }}</button>
      </div>
    </section>
  </div>
</template>

<style scoped>
.login-card__status {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--khaen-space-2);
  margin: var(--khaen-space-4) 0;
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  text-align: center;
}

.login-card__status--error {
  align-items: stretch;
  padding: var(--khaen-space-3);
  border: 1px solid color-mix(in srgb, var(--khaen-danger) 45%, transparent);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-danger-soft);
  color: var(--khaen-text);
  text-align: left;
}

.login-card__status-title {
  font-weight: 600;
}

.login-card__status-detail {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.login-card__form {
  margin-top: var(--khaen-space-2);
}

.login-card__notice {
  margin-bottom: var(--khaen-space-3);
  padding: var(--khaen-space-2) var(--khaen-space-3);
  border: 1px solid color-mix(in srgb, var(--khaen-accent) 40%, transparent);
  border-radius: var(--khaen-radius-button);
  background: var(--khaen-accent-soft);
  color: var(--khaen-text);
  font-size: var(--khaen-font-aux);
}

.login-card__detail {
  margin-top: var(--khaen-space-1);
  color: var(--khaen-text-muted);
  font-size: 11px;
  overflow-wrap: anywhere;
}

.login-card__locale {
  margin-left: auto;
  padding: 0;
  border: 0;
  background: transparent;
  color: var(--khaen-accent);
  font-family: var(--khaen-font-sans);
  font-size: 11px;
  cursor: pointer;
}

.login-card__locale:hover {
  text-decoration: underline;
}
</style>
