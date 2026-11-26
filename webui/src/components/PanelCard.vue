<script setup lang="ts">
/** 面板卡片：标题行（标题 + TruthTag + 可选「详情」）+ 内容（PLAN §11.4/§11.5）。 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import TruthTag from './TruthTag.vue'
import type { TruthKind } from '@/types/ui'

const props = defineProps<{
  /** 标题 i18n 键。 */
  titleKey: string
  /** 说明 i18n 键（可选，显示在标题下）。 */
  descKey?: string
  /** 数据真实性标签。 */
  truth: TruthKind
  /** 右侧「详情」链接文案键（可选；未落地页面不传）。 */
  detailKey?: string
}>()

const { t } = useI18n({ useScope: 'global' })

const title = computed(() => t(props.titleKey))
const desc = computed(() => (props.descKey ? t(props.descKey) : ''))
</script>

<template>
  <section class="panel-card" :class="{ 'panel-card--not-wired': props.truth === 'notWired' }">
    <header class="panel-card__head">
      <div class="panel-card__heading">
        <h2 class="panel-card__title">{{ title }}</h2>
        <TruthTag :kind="props.truth" />
      </div>
      <div class="panel-card__head-actions">
        <slot name="actions" />
      </div>
    </header>
    <p v-if="desc" class="panel-card__desc">{{ desc }}</p>
    <div class="panel-card__body">
      <slot />
    </div>
  </section>
</template>

<style scoped>
.panel-card {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  min-height: 150px;
  padding: var(--khaen-space-4);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
  box-shadow: var(--khaen-shadow-card);
  transition: border-color var(--khaen-transition), background var(--khaen-transition);
}

.panel-card:hover {
  border-color: var(--khaen-border-strong);
}

.panel-card__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--khaen-space-2);
  min-height: 24px;
}

.panel-card__heading {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  min-width: 0;
}

.panel-card__title {
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.panel-card__head-actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-1);
  flex: none;
}

.panel-card__desc {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

.panel-card__body {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
}

.panel-card--not-wired {
  border-style: dashed;
}
</style>
