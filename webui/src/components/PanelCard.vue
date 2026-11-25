<script setup lang="ts">
withDefaults(defineProps<{
  title?: string
  subtitle?: string
  flat?: boolean
  dense?: boolean
}>(), {
  title: '',
  subtitle: '',
  flat: false,
  dense: false
})
</script>

<template>
  <section class="panel" :class="{ 'panel--flat': flat, 'panel--dense': dense }">
    <header v-if="title || $slots.extra" class="panel__head">
      <div>
        <h2 v-if="title" class="panel__title">{{ title }}</h2>
        <p v-if="subtitle" class="panel__sub">{{ subtitle }}</p>
      </div>
      <div v-if="$slots.extra" class="panel__extra">
        <slot name="extra" />
      </div>
    </header>
    <slot />
  </section>
</template>

<style scoped>
.panel--dense { padding: 12px; }
</style>
