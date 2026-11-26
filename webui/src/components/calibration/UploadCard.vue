<script setup lang="ts">
/**
 * 校准上传卡（第 3 步麦克风文件 / 第 5 步两段 WAV）：选择文件 -> 父组件校验/上传 -> 展示服务端事实。
 * 只负责展示与选择；WAV 解析/格式校验/上传都在调用方（向导视图）+ store 完成。
 */
import { ref, useId } from 'vue'
import { useI18n } from 'vue-i18n'
import { UploadFilled } from '@element-plus/icons-vue'
import TruthTag from '@/components/TruthTag.vue'
import { shortenId } from '@/utils/format'
import type { CalibrationUploadResult } from '@/api/calibration'

export interface UploadDetail {
  label: string
  value: string
}

const props = defineProps<{
  label: string
  hint?: string
  accept?: string
  actionLabel: string
  busy?: boolean
  disabled?: boolean
  result?: CalibrationUploadResult | null
  details?: UploadDetail[]
  errorText?: string | null
}>()

const emit = defineEmits<{ (event: 'select', file: File): void }>()

const { t } = useI18n({ useScope: 'global' })
const inputId = useId()
const input = ref<HTMLInputElement | null>(null)

function openPicker(): void {
  if (props.disabled === true || props.busy === true) {
    return
  }
  input.value?.click()
}

function onChange(event: Event): void {
  const target = event.target as HTMLInputElement
  const file = target.files && target.files.length > 0 ? target.files[0] : null
  target.value = ''
  if (file === null) {
    return
  }
  emit('select', file)
}
</script>

<template>
  <div class="upload-card" :class="{ 'upload-card--error': Boolean(props.errorText) }">
    <div class="upload-card__head">
      <label class="upload-card__label" :for="inputId">{{ props.label }}</label>
      <TruthTag :kind="props.result ? 'actual' : 'notWired'" />
    </div>
    <p v-if="props.hint" class="upload-card__hint">{{ props.hint }}</p>
    <input
      :id="inputId"
      ref="input"
      class="upload-card__input"
      type="file"
      :accept="props.accept ?? undefined"
      :disabled="props.disabled === true || props.busy === true"
      @change="onChange"
    />
    <div class="upload-card__actions">
      <el-button
        :type="props.result ? 'default' : 'primary'"
        plain
        :loading="props.busy === true"
        :disabled="props.disabled === true || props.busy === true"
        @click="openPicker"
      >
        <el-icon v-if="!props.busy"><UploadFilled /></el-icon>
        <span>{{ props.result ? t('calibration.upload.replace') : props.actionLabel }}</span>
      </el-button>
      <span v-if="props.busy" class="upload-card__busy">{{ t('calibration.upload.uploading') }}</span>
    </div>
    <dl v-if="props.result" class="upload-card__facts">
      <div class="upload-card__fact">
        <dt>{{ t('calibration.upload.sha256') }}</dt>
        <dd class="mono" :title="props.result.sha256">{{ shortenId(props.result.sha256, 16, 12) }}</dd>
      </div>
      <div class="upload-card__fact">
        <dt>{{ t('calibration.upload.bytes') }}</dt>
        <dd class="mono">{{ props.result.bytes }}</dd>
      </div>
      <div v-for="detail in props.details ?? []" :key="detail.label" class="upload-card__fact">
        <dt>{{ detail.label }}</dt>
        <dd class="mono">{{ detail.value }}</dd>
      </div>
    </dl>
    <p v-if="props.errorText" class="upload-card__error" role="alert">{{ props.errorText }}</p>
  </div>
</template>

<style scoped>
.upload-card {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-3);
  border: 1px dashed var(--khaen-border-strong);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
  min-width: 0;
}

.upload-card--error {
  border-color: color-mix(in srgb, var(--khaen-danger) 55%, transparent);
}

.upload-card__head {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.upload-card__label {
  font-weight: 600;
  font-size: var(--khaen-font-card-title);
}

.upload-card__hint {
  color: var(--khaen-text-muted);
  font-size: 11px;
}

.upload-card__input {
  position: absolute;
  width: 1px;
  height: 1px;
  opacity: 0;
  overflow: hidden;
  clip: rect(0 0 0 0);
}

.upload-card__actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.upload-card__busy {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.upload-card__facts {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  margin: 0;
}

.upload-card__fact {
  display: flex;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  font-size: var(--khaen-font-aux);
  border-bottom: 1px dashed var(--khaen-border);
  padding-bottom: 2px;
}

.upload-card__fact:last-child {
  border-bottom: 0;
}

.upload-card__fact dt {
  color: var(--khaen-text-secondary);
}

.upload-card__fact dd {
  margin: 0;
  text-align: right;
  overflow-wrap: anywhere;
}

.upload-card__error {
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}
</style>
