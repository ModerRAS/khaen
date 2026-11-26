<script setup lang="ts">
/**
 * 修改密码卡片（任务 T47）：旧 / 新 / 确认；成功后所有会话失效，父组件负责跳回登录页。
 *
 * 端点：POST /api/system/password {old,new}（登录 + CSRF，复用 auth password 语义）。
 * 错误：401 在改密语境下是「当前密码错误」，用 classifyAuthError(context='changePassword') 映射，
 * 不触发全局 401 跳转（端点用 auth:'probe'）。
 */
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import PanelCard from '@/components/PanelCard.vue'
import { MIN_PASSWORD_LENGTH } from '@/config'
import { classifyAuthError } from '@/api/errors'
import { validatePasswordChange } from '@/api/system'
import { systemApi } from '@/stores/system'

const emit = defineEmits<{ changed: [] }>()

const { t } = useI18n({ useScope: 'global' })

const oldPassword = ref('')
const newPassword = ref('')
const confirm = ref('')
const busy = ref(false)
const errorKey = ref<string | null>(null)
const errorDetail = ref('')

function reset(): void {
  oldPassword.value = ''
  newPassword.value = ''
  confirm.value = ''
  errorKey.value = null
  errorDetail.value = ''
}

async function submit(): Promise<void> {
  if (busy.value) {
    return
  }
  errorKey.value = null
  errorDetail.value = ''
  const validation = validatePasswordChange(
    oldPassword.value,
    newPassword.value,
    confirm.value,
    MIN_PASSWORD_LENGTH
  )
  if (!validation.ok) {
    errorKey.value = validation.key
    return
  }
  busy.value = true
  try {
    await systemApi.changePassword(oldPassword.value, newPassword.value)
    reset()
    ElMessage.success(t('system.password.success'))
    emit('changed')
  } catch (error) {
    const info = classifyAuthError(error, 'changePassword')
    errorKey.value = info.i18nKey
    errorDetail.value = info.detail ?? ''
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div id="system-password" class="password-card">
    <PanelCard title-key="system.password.title" desc-key="system.password.desc" truth="actual">
      <el-form label-position="top" class="password-card__form" @submit.prevent="submit">
        <el-form-item :label="t('system.password.field.old')">
          <el-input
            v-model="oldPassword"
            type="password"
            show-password
            autocomplete="current-password"
            :disabled="busy"
            :aria-label="t('system.password.field.old')"
          />
        </el-form-item>
        <el-form-item :label="t('system.password.field.new')">
          <el-input
            v-model="newPassword"
            type="password"
            show-password
            autocomplete="new-password"
            :disabled="busy"
            :aria-label="t('system.password.field.new')"
          />
        </el-form-item>
        <el-form-item :label="t('system.password.field.confirm')">
          <el-input
            v-model="confirm"
            type="password"
            show-password
            autocomplete="new-password"
            :disabled="busy"
            :aria-label="t('system.password.field.confirm')"
          />
        </el-form-item>
        <p v-if="errorKey" class="password-card__error" role="alert" aria-live="assertive">
          {{ t(errorKey, { min: MIN_PASSWORD_LENGTH }) }}
        </p>
        <p v-if="errorDetail" class="aux-text password-card__detail">{{ errorDetail }}</p>
        <div class="password-card__actions">
          <el-button
            type="primary"
            native-type="submit"
            :loading="busy"
            :disabled="busy"
            class="touch-target"
          >
            {{ busy ? t('system.password.busy') : t('system.password.submit') }}
          </el-button>
        </div>
      </el-form>
    </PanelCard>
  </div>
</template>

<style scoped>
.password-card {
  min-width: 0;
}

.password-card__form {
  max-width: 420px;
}

.password-card__form .el-form-item {
  margin-bottom: var(--khaen-space-3);
}

.password-card__error {
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.password-card__detail {
  margin-top: var(--khaen-space-1);
  overflow-wrap: anywhere;
}

.password-card__actions {
  display: flex;
  justify-content: flex-start;
}
</style>
