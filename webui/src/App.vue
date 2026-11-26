<script setup lang="ts">
/**
 * 应用根组件：
 * - 注入 Element Plus 的本地化（组件内建文案跟随 khaen 语言）；
 * - 维护 document.title：跟随路由 meta.titleKey + 当前语言（语言切换不重新导航，必须响应式更新）。
 * 首帧主题由 public/theme-init.js 提前应用；运行时状态在 main.ts 初始化。
 */
import { computed, watchEffect } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute } from 'vue-router'
import zhCn from 'element-plus/es/locale/lang/zh-cn'
import en from 'element-plus/es/locale/lang/en'

const { t, locale } = useI18n({ useScope: 'global' })
const route = useRoute()

/** Element Plus 内建文案（日期选择、分页、空态等）跟随当前语言。 */
const elementLocale = computed(() => (locale.value === 'en-US' ? en : zhCn))

watchEffect(() => {
  const titleKey = route.meta.titleKey ?? 'nav.overview'
  document.title = t(titleKey) + ' · ' + t('app.name')
})
</script>

<template>
  <el-config-provider :locale="elementLocale">
    <router-view />
  </el-config-provider>
</template>
