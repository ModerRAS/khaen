<script setup lang="ts">
import { onMounted, onUnmounted } from 'vue'
import { useRouter } from 'vue-router'
import { sessionState, verifySession } from '@/stores/session'

const router = useRouter()

function onUnauthorized(): void {
  if (!sessionState.authenticated) { return }
  sessionState.authenticated = false
  void verifySession().then(() => {
    if (!sessionState.authenticated) {
      void router.replace({ name: 'login', query: { redirect: router.currentRoute.value.fullPath } })
    }
  })
}

onMounted(() => { window.addEventListener('khaen:unauthorized', onUnauthorized) })
onUnmounted(() => { window.removeEventListener('khaen:unauthorized', onUnauthorized) })
</script>

<template>
  <router-view v-slot="{ Component }">
    <transition name="fade" mode="out-in">
      <component :is="Component" />
    </transition>
  </router-view>
</template>
