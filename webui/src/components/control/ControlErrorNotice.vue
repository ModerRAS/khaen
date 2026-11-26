<script setup lang="ts">
/**
 * 控制面错误提示条（M3/M4 共用）：稳定错误码 + 本地化主文案 + 后端 message 详情 + 可选重试。
 * 文案全部走 i18n（error.control.* / error.channels.* 由调用方给出 i18nKey）。
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  /** classifyControlError / classifyChannelsError 的结果。 */
  error: { i18nKey: string; code: string; detail?: string; retryable: boolean } | null
  /** 是否显示重试按钮（默认显示；retryable=false 时始终不显示）。 */
  showRetry?: boolean
}>()

const emit = defineEmits<{ retry: [] }>()

const { t } = useI18n({ useScope: 'global' })

const visible = computed(() => props.error !== null)
const retryable = computed(() => props.error !== null && props.error.retryable && props.showRetry !== false)
</script>

<template>
  <div v-if="visible" class="banner banner--danger control-error" role="alert" aria-live="assertive">
    <div class="control-error__text">
      <span class="control-error__title">{{ t('control.error.title') }}</span>
      <span class="control-error__message">{{ t(error!.i18nKey) }}</span>
      <span class="control-error__code mono">{{ error!.code }}</span>
      <span v-if="error!.detail" class="aux-text control-error__detail">{{ error!.detail }}</span>
    </div>
    <el-button v-if="retryable" size="small" @click="emit('retry')">{{ t('control.actions.retry') }}</el-button>
  </div>
</template>

<style scoped>
.control-error {
  align-items: flex-start;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--khaen-space-3);
}

.control-error__text {
  display: flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  min-width: 0;
}

.control-error__title {
  font-weight: 600;
}

.control-error__message {
  overflow-wrap: anywhere;
}

.control-error__code {
  padding: 0 6px;
  border: 1px solid var(--khaen-border-strong);
  border-radius: var(--khaen-radius-tag);
  color: var(--khaen-text-secondary);
  font-size: 11px;
}

.control-error__detail {
  flex-basis: 100%;
  overflow-wrap: anywhere;
}
</style>
