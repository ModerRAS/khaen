<script setup lang="ts">
/**
 * 登录 / 初始化静态壳（PLAN §6.1 / §11.4）。
 *
 * 本页现在**不接认证 API**：
 * - setup / login 两态可切换预览（M1 落地后由 GET /api/auth/status 决定，移除预览开关）；
 * - 表单做本地校验（必填 / 最短长度 / 两次一致），提交按钮只提示「M1 未接入」，不伪造登录成功。
 * TODO(M1)：GET /api/auth/status 决定 setup|login；POST /api/auth/setup|login；成功保存 CSRF 并跳 next。
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import { APP_BUILD_ID, APP_VERSION, MIN_PASSWORD_LENGTH } from '@/config'
import { setLocale } from '@/i18n'

type LoginMode = 'setup' | 'login'

const { t, locale } = useI18n({ useScope: 'global' })

const mode = ref<LoginMode>('setup')
const password = ref('')
const confirm = ref('')
const token = ref('')
const error = ref('')

const title = computed(() => t(mode.value === 'setup' ? 'login.title.setup' : 'login.title.login'))
const desc = computed(() =>
  t(mode.value === 'setup' ? 'login.desc.setup' : 'login.desc.login', { min: MIN_PASSWORD_LENGTH })
)
const submitLabel = computed(() =>
  t(mode.value === 'setup' ? 'login.action.submitSetup' : 'login.action.submitLogin')
)
const localeLabel = computed(() => (locale.value === 'en-US' ? t('locale.enUS') : t('locale.zhCN')))

function toggleLocale(): void {
  setLocale(locale.value === 'en-US' ? 'zh-CN' : 'en-US')
}

/** 仅本地校验（与后端 M1 规则保持一致：至少 MIN_PASSWORD_LENGTH 个字符、两次一致）。 */
function validate(): boolean {
  if (password.value.length === 0) {
    error.value = t('login.validation.required')
    return false
  }
  if (mode.value === 'setup') {
    if (password.value.length < MIN_PASSWORD_LENGTH) {
      error.value = t('login.validation.tooShort', { min: MIN_PASSWORD_LENGTH })
      return false
    }
    if (confirm.value !== password.value) {
      error.value = t('login.validation.mismatch')
      return false
    }
  }
  return true
}

/** TODO(M1)：替换为真实认证请求；现在只做本地校验 + 明确的未接入提示。 */
function submit(): void {
  error.value = ''
  if (!validate()) {
    return
  }
  ElMessage.warning(t('login.stub.submitBlocked'))
}
</script>

<template>
  <div class="login-page">
    <section class="login-card">
      <div class="login-card__brand">
        <span class="login-card__mark" aria-hidden="true">KH</span>
        <div>
          <h1 class="login-card__title">{{ title }}</h1>
          <p class="login-card__desc">{{ desc }}</p>
        </div>
      </div>

      <div class="login-card__mode">
        <span class="login-card__mode-label">{{ t('login.stub.previewLabel') }}</span>
        <el-radio-group v-model="mode" size="small">
          <el-radio-button value="setup">{{ t('login.stub.previewSetup') }}</el-radio-button>
          <el-radio-button value="login">{{ t('login.stub.previewLogin') }}</el-radio-button>
        </el-radio-group>
      </div>

      <el-form label-position="top" @submit.prevent="submit">
        <el-form-item :label="t('login.field.password')">
          <el-input
            v-model="password"
            type="password"
            show-password
            :placeholder="t('login.placeholder.password')"
            :autocomplete="mode === 'setup' ? 'new-password' : 'current-password'"
          />
        </el-form-item>

        <el-form-item v-if="mode === 'setup'" :label="t('login.field.confirm')">
          <el-input
            v-model="confirm"
            type="password"
            show-password
            :placeholder="t('login.placeholder.confirm')"
            autocomplete="new-password"
          />
        </el-form-item>

        <el-form-item v-if="mode === 'login'" :label="t('login.field.token')">
          <el-input
            v-model="token"
            type="password"
            show-password
            :placeholder="t('login.placeholder.token')"
            autocomplete="off"
          />
        </el-form-item>

        <div class="login-card__error" role="alert">{{ error }}</div>

        <el-button type="primary" native-type="submit" class="login-card__submit">
          {{ submitLabel }}
        </el-button>
      </el-form>

      <p class="login-card__note">{{ t('login.stub.note') }}</p>

      <div class="login-card__footer">
        <span>{{ t('login.footer.version', { version: APP_VERSION }) }}</span>
        <span>{{ t('login.footer.build', { id: APP_BUILD_ID }) }}</span>
        <button type="button" class="login-card__locale" @click="toggleLocale">{{ localeLabel }}</button>
      </div>
    </section>
  </div>
</template>

<style scoped>
.login-card__note {
  margin-top: var(--khaen-space-3);
  color: var(--khaen-text-muted);
  font-size: 11px;
  line-height: 1.5;
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
