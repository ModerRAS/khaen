<script setup lang="ts">
/**
 * 日志页（任务 T47，导航「日志」）——独立的日志查看器全页，复用系统页的 SystemLogsPanel。
 *
 * 数据来自 GET /api/system/logs（daemon / rt-host 的 journalctl 最近 N 行，只读）。
 * 401 走共享会话处理；日志不可用（503 logs_unavailable）如实显示「未接入」+ 原因。
 */
import { onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { ArrowLeft } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import SystemLogsPanel from '@/components/system/SystemLogsPanel.vue'
import { loadSystemInfo } from '@/stores/system'

const { t } = useI18n({ useScope: 'global' })
const router = useRouter()

onMounted(() => {
  // 日志页也可从深链直接进入：不阻塞，仅让顶栏/系统信息语境的缓存保持新鲜。
  void loadSystemInfo()
})
</script>

<template>
  <div class="logs-page">
    <PageHeader title-key="system.logsPageTitle" subtitle-key="system.logsSubtitle">
      <template #actions>
        <el-button plain @click="router.push({ name: 'system' })">
          <el-icon><ArrowLeft /></el-icon>
          <span>{{ t('system.actions.backToSystem') }}</span>
        </el-button>
      </template>
    </PageHeader>
    <SystemLogsPanel />
  </div>
</template>

<style scoped>
.logs-page {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}
</style>
