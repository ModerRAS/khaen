<script setup lang="ts">
/** 页面头部：标题 + 说明 + 右侧操作区（PLAN §11.5 PageHeader）。 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  /** 标题 i18n 键。 */
  titleKey: string
  /** 说明 i18n 键（可选）。 */
  subtitleKey?: string
}>()

const { t } = useI18n({ useScope: 'global' })

const title = computed(() => t(props.titleKey))
const subtitle = computed(() => (props.subtitleKey ? t(props.subtitleKey) : ''))
</script>

<template>
  <div class="page-header">
    <div>
      <h1 class="page-header__title">{{ title }}</h1>
      <p v-if="subtitle" class="page-header__subtitle">{{ subtitle }}</p>
    </div>
    <div class="page-header__actions">
      <slot name="actions" />
    </div>
  </div>
</template>

<style scoped>
.page-header__actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}
</style>
