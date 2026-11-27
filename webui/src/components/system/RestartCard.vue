<script setup lang="ts">
/**
 * 重启 rt-host 卡片（任务 T47）：二次确认（输入确认词）后才提交。
 *
 * 契约（T46）：
 * - POST /api/system/restart {target:"rt-host"}；仅在 daemon 以 KHAEN_ALLOW_RESTART=1 启动时允许，
 *   未启用返回 403/501 + reason；
 * - 前端只在 info.restart.allowed=true 时显示操作按钮（能力未允许 -> NOT_AVAILABLE + 原因），
 *   二次确认由前端负责（输入确认词），不把危险操作做成一次点击。
 * - daemon 自身重启不实现（T46 返回 501）：本卡始终以 NOT_AVAILABLE 明示，并给出 systemd 手动方式。
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import PanelCard from '@/components/PanelCard.vue'
import TruthTag from '@/components/TruthTag.vue'
import { annotateGapById, gapLocaleOf } from '@/api/capabilityGaps'
import { classifySystemError, isRestartAvailable, type SystemInfo } from '@/api/system'
import { formatTimestamp } from '@/utils/format'
import { systemApi } from '@/stores/system'

const props = defineProps<{
  /** /api/system/info（null = 尚未加载）。 */
  info: SystemInfo | null
}>()

const emit = defineEmits<{ restarted: [] }>()

const { t, locale } = useI18n({ useScope: 'global' })
const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))

const available = computed(() => isRestartAvailable(props.info))
const disabledReason = computed(
  () => props.info?.restart.reason ?? t('system.restart.disabledTitle')
)
const lastRestartedAt = computed(() => {
  const value = props.info?.restart.last_restarted_at ?? null
  return value === null ? null : formatTimestamp(value, localeId.value)
})

const dialogOpen = ref(false)
const confirmWord = ref('')
const submitting = ref(false)
const formError = ref<string | null>(null)
const errorDetail = ref('')

const truth = computed(() => (available.value ? 'actual' : 'notWired'))

function openDialog(): void {
  confirmWord.value = ''
  formError.value = null
  errorDetail.value = ''
  dialogOpen.value = true
}

function onDialogClosed(): void {
  confirmWord.value = ''
  formError.value = null
  errorDetail.value = ''
}

async function submit(): Promise<void> {
  if (submitting.value) {
    return
  }
  formError.value = null
  errorDetail.value = ''
  if (confirmWord.value.trim() !== t('system.restart.confirmWord')) {
    formError.value = t('system.restart.confirmMismatch')
    return
  }
  submitting.value = true
  try {
    await systemApi.restartRtHost()
    dialogOpen.value = false
    ElMessage.success(t('system.restart.success'))
    emit('restarted')
  } catch (error) {
    const info = classifySystemError(error)
    formError.value = t(info.i18nKey)
    errorDetail.value = info.reason ?? info.detail ?? ''
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <div id="system-restart" class="restart-card">
    <PanelCard title-key="system.restart.title" desc-key="system.restart.desc" :truth="truth">
      <div v-if="available" class="restart-card__body">
        <p v-if="lastRestartedAt" class="aux-text">
          {{ t('system.restart.lastRestartedAt') }}: <span class="mono">{{ lastRestartedAt }}</span>
        </p>
        <el-button type="warning" plain class="touch-target" @click="openDialog">
          {{ t('system.restart.button') }}
        </el-button>
      </div>
      <div v-else class="restart-card__blocked">
        <strong>{{ t('system.restart.disabledTitle') }}</strong>
        <span>{{ annotateGapById(t('system.restart.disabledReason', { reason: disabledReason }), 'systemRestart', gapLocaleOf(locale)) }}</span>
      </div>

      <div class="restart-card__daemon">
        <div class="restart-card__daemon-head">
          <strong>NOT_AVAILABLE · {{ t('system.restart.daemonTitle') }}</strong>
          <TruthTag kind="notWired" />
        </div>
        <p class="aux-text">{{ annotateGapById(t('system.restart.daemonReason'), 'systemDaemonRestart', gapLocaleOf(locale)) }}</p>
      </div>
    </PanelCard>

    <el-dialog
      v-model="dialogOpen"
      :title="t('system.restart.confirmTitle')"
      width="460px"
      class="restart-dialog"
      :close-on-click-modal="false"
      @closed="onDialogClosed"
    >
      <p class="restart-card__dialog-desc">{{ t('system.restart.confirmDesc') }}</p>
      <p class="restart-card__word">
        {{ t('system.restart.confirmWordLabel') }}: <code class="mono">{{ t('system.restart.confirmWord') }}</code>
      </p>
      <el-input
        v-model="confirmWord"
        :placeholder="t('system.restart.confirmWordPlaceholder', { word: t('system.restart.confirmWord') })"
        :aria-label="t('system.restart.confirmWordLabel')"
        :disabled="submitting"
        @keyup.enter="submit"
      />
      <p v-if="formError" class="restart-card__error" role="alert" aria-live="assertive">{{ formError }}</p>
      <p v-if="errorDetail" class="aux-text restart-card__detail">{{ errorDetail }}</p>
      <template #footer>
        <el-button :disabled="submitting" @click="dialogOpen = false">{{ t('system.restart.cancel') }}</el-button>
        <el-button type="danger" :loading="submitting" :disabled="submitting" @click="submit">
          {{ submitting ? t('system.restart.running') : t('system.restart.confirm') }}
        </el-button>
      </template>
    </el-dialog>
  </div>
</template>

<style scoped>
.restart-card {
  min-width: 0;
}

.restart-card__body {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: var(--khaen-space-2);
}

.restart-card__blocked,
.restart-card__daemon {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  padding: var(--khaen-space-2) var(--khaen-space-3);
  border: 1px dashed var(--khaen-border-strong);
  border-radius: var(--khaen-radius-button);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.restart-card__daemon {
  border-color: var(--khaen-border);
  color: var(--khaen-text-muted);
}

.restart-card__daemon-head {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.restart-card__dialog-desc {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.restart-card__word {
  margin: var(--khaen-space-3) 0 var(--khaen-space-2);
  font-size: var(--khaen-font-aux);
  color: var(--khaen-text-secondary);
}

.restart-card__error {
  margin-top: var(--khaen-space-2);
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
  overflow-wrap: anywhere;
}

.restart-card__detail {
  margin-top: var(--khaen-space-1);
  overflow-wrap: anywhere;
}
</style>
