<script setup lang="ts">
withDefaults(defineProps<{ title: string; subtitle?: string; badge?: string }>(), {
  subtitle: '',
  badge: ''
})
</script>

<template>
  <header class="page-header">
    <div style="min-width: 0">
      <h1 class="page-title">
        {{ title }}
        <span v-if="badge" class="chip chip--info">{{ badge }}</span>
      </h1>
      <p v-if="subtitle" class="page-sub">{{ subtitle }}</p>
    </div>
    <div v-if="$slots.right" class="page-actions">
      <slot name="right" />
    </div>
  </header>
</template>

<style scoped>
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; flex-wrap: wrap; }
.page-title { margin: 0; font-size: 20px; font-weight: 650; display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.page-sub { margin: 4px 0 0; color: var(--khaen-text-muted); font-size: 12.5px; max-width: 900px; }
.page-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
@media (max-width: 767px) {
  .page-title { font-size: 18px; }
  .page-actions { width: 100%; }
  .page-actions :deep(.el-button) { flex: 1; }
}
</style>
