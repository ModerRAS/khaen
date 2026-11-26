<script setup lang="ts">
/**
 * 概览占位壳（PLAN §6.2 / §11.4）。
 *
 * 布局按规划落地：顶部状态带 + 8 张卡片（运行状态 / 输入统计 / 输出统计 / 方案 / 格式与设备 /
 * AES67 / 最近校准 / 控制通道）。**本里程碑不接任何数据**：所有数值显示「—」并带「未接入」
 * 真相标签，M2 起接 daemon 的 GET /api/status 聚合，届时逐卡替换为真实字段（不允许假数据）。
 */
import { useI18n } from 'vue-i18n'
import { Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import StatusBadge from '@/components/StatusBadge.vue'

const { t } = useI18n({ useScope: 'global' })

interface OverviewCard {
  /** 卡片稳定标识（仅用作列表 key）。 */
  key: string
  /** 标题 i18n 键。 */
  titleKey: string
  /** 说明 i18n 键。 */
  descKey: string
  /** 明细行 i18n 键（值在 M2 之前恒为「未接入」）。 */
  fields: string[]
}

/** 8 张卡片与字段顺序按 PLAN §11.4；键全部在 zh-CN / en-US 两侧对等存在。 */
const cards: OverviewCard[] = [
  {
    key: 'runtime',
    titleKey: 'overview.card.runtime.title',
    descKey: 'overview.card.runtime.desc',
    fields: [
      'overview.card.runtime.field.state',
      'overview.card.runtime.field.muted',
      'overview.card.runtime.field.safe',
      'overview.card.runtime.field.blocks',
      'overview.card.runtime.field.uptime'
    ]
  },
  {
    key: 'input',
    titleKey: 'overview.card.input.title',
    descKey: 'overview.card.input.desc',
    fields: [
      'overview.card.input.field.packets',
      'overview.card.input.field.underruns',
      'overview.card.input.field.jitterFill',
      'overview.card.input.field.ssrc',
      'overview.card.input.field.queueFullDrops',
      'overview.card.input.field.recvErrors'
    ]
  },
  {
    key: 'output',
    titleKey: 'overview.card.output.title',
    descKey: 'overview.card.output.desc',
    fields: [
      'overview.card.output.field.blocks',
      'overview.card.output.field.xruns',
      'overview.card.output.field.errors',
      'overview.card.output.field.bytes'
    ]
  },
  {
    key: 'plan',
    titleKey: 'overview.card.plan.title',
    descKey: 'overview.card.plan.desc',
    fields: [
      'overview.card.plan.field.graphId',
      'overview.card.plan.field.sha256',
      'overview.card.plan.field.version',
      'overview.card.plan.field.latencySamples',
      'overview.card.plan.field.appliedAt',
      'overview.card.plan.field.source'
    ]
  },
  {
    key: 'format',
    titleKey: 'overview.card.format.title',
    descKey: 'overview.card.format.desc',
    fields: [
      'overview.card.format.field.planRate',
      'overview.card.format.field.deviceRate',
      'overview.card.format.field.blockFrames',
      'overview.card.format.field.channels',
      'overview.card.format.field.encoding'
    ]
  },
  {
    key: 'aoip',
    titleKey: 'overview.card.aoip.title',
    descKey: 'overview.card.aoip.desc',
    fields: [
      'overview.card.aoip.field.group',
      'overview.card.aoip.field.port',
      'overview.card.aoip.field.pt',
      'overview.card.aoip.field.channels',
      'overview.card.aoip.field.map'
    ]
  },
  {
    key: 'calibration',
    titleKey: 'overview.card.calibration.title',
    descKey: 'overview.card.calibration.desc',
    fields: ['overview.card.calibration.field.lastJob', 'overview.card.calibration.field.verdict']
  },
  {
    key: 'control',
    titleKey: 'overview.card.control.title',
    descKey: 'overview.card.control.desc',
    fields: ['overview.card.control.field.connected', 'overview.card.control.field.statusAgeMs']
  }
]
</script>

<template>
  <div class="overview">
    <PageHeader title-key="overview.title" subtitle-key="overview.subtitle">
      <template #actions>
        <el-tooltip :content="t('topbar.refreshTodo', { milestone: 'M2' })" placement="bottom">
          <el-button disabled>
            <el-icon><Refresh /></el-icon>
            <span>{{ t('common.refresh') }}</span>
          </el-button>
        </el-tooltip>
      </template>
    </PageHeader>

    <div class="status-band">
      <StatusBadge kind="unknown" :label="t('overview.band.runState')" />
      <span class="status-band__divider" aria-hidden="true" />
      <StatusBadge kind="unknown" :label="t('overview.band.lastError')" />
      <span class="status-band__divider" aria-hidden="true" />
      <StatusBadge kind="unknown" :label="t('overview.band.hostLink')" />
      <span class="status-band__meta">{{ t('overview.band.neverUpdated') }}</span>
    </div>

    <p class="aux-text">{{ t('overview.notWired.reason', { milestone: 'M2' }) }}</p>

    <div class="card-grid">
      <PanelCard
        v-for="card in cards"
        :key="card.key"
        :title-key="card.titleKey"
        :desc-key="card.descKey"
        truth="notWired"
      >
        <div class="metric">
          <span class="metric__value metric__value--muted">{{ t('common.dash') }}</span>
          <span class="metric__unit">{{ t('common.notWired') }}</span>
        </div>
        <ul class="kv-list">
          <li v-for="fieldKey in card.fields" :key="fieldKey" class="kv-list__row">
            <span class="kv-list__label">{{ t(fieldKey) }}</span>
            <span class="kv-list__value kv-list__value--muted">{{ t('common.dash') }}</span>
          </li>
        </ul>
      </PanelCard>
    </div>
  </div>
</template>

<style scoped>
.overview {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
}
</style>
