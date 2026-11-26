<script setup lang="ts">
/**
 * 状态明细行（概览卡片用）：label + 值（可单位/语义色）+ 可选复制按钮。
 *
 * - value 为 null/空：显示「—」+ 「未接入」TruthTag（tooltip 带 reasons 里的原因），不显示假值；
 * - copy 非空：值右侧出现复制按钮（graph_id / sha256 / job id 等），复制失败给出本地化提示；
 * - tone：数值语义色（ok 绿 / warn 黄 / bad 红），用于欠载率、xrun 之类需要分级的字段。
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import { DocumentCopy } from '@element-plus/icons-vue'
import TruthTag from './TruthTag.vue'
import type { TruthKind } from '@/types/ui'

const props = defineProps<{
  /** 行标签（已本地化）。 */
  label: string
  /** 显示值；null/undefined/空串按缺失处理。 */
  value?: string | null
  /** 附加单位/后缀（已本地化）。 */
  suffix?: string | null
  /** 复制按钮要复制的原始值（不传则不显示按钮）。 */
  copy?: string | null
  /** 缺失原因（TruthTag tooltip）。 */
  reason?: string | null
  /** 数值语义色。 */
  tone?: 'ok' | 'warn' | 'bad' | null
  /** 非空值时强制显示的真实性标签（缺省：非空不显示，空值显示「未接入」）。 */
  truth?: TruthKind | null
}>()

const { t } = useI18n({ useScope: 'global' })

const hasValue = computed(() => {
  const value = props.value
  return value !== null && value !== undefined && value.length > 0
})

const truthKind = computed<TruthKind | null>(() => {
  if (props.truth) {
    return props.truth
  }
  return hasValue.value ? null : 'notWired'
})

const toneClass = computed(() => (props.tone ? 'stat-row__value--' + props.tone : ''))
const copyable = computed(() => typeof props.copy === 'string' && props.copy.length > 0)
const copied = ref(false)

async function copyValue(): Promise<void> {
  if (!copyable.value || props.copy === null || props.copy === undefined) {
    return
  }
  const clipboard = typeof navigator !== 'undefined' ? navigator.clipboard : undefined
  if (!clipboard || typeof clipboard.writeText !== 'function') {
    ElMessage.error(t('overview.copy.failed'))
    return
  }
  try {
    await clipboard.writeText(props.copy)
    copied.value = true
    ElMessage.success(t('overview.copy.copied'))
    window.setTimeout(() => {
      copied.value = false
    }, 1200)
  } catch {
    ElMessage.error(t('overview.copy.failed'))
  }
}
</script>

<template>
  <li class="kv-list__row stat-row">
    <span class="kv-list__label stat-row__label">{{ props.label }}</span>
    <span class="stat-row__value-wrap">
      <template v-if="hasValue">
        <span class="kv-list__value stat-row__value" :class="toneClass">{{ props.value }}</span>
        <span v-if="props.suffix" class="stat-row__suffix">{{ props.suffix }}</span>
      </template>
      <span v-else class="kv-list__value kv-list__value--muted stat-row__value">{{ t('common.dash') }}</span>
      <TruthTag v-if="truthKind" :kind="truthKind" :reason="props.reason" />
      <button
        v-if="copyable"
        type="button"
        class="stat-row__copy"
        :class="{ 'is-copied': copied }"
        :title="t('overview.copy.action')"
        :aria-label="t('overview.copy.action')"
        @click="copyValue"
      >
        <el-icon :size="13"><DocumentCopy /></el-icon>
      </button>
    </span>
  </li>
</template>

<style scoped>
.stat-row {
  align-items: center;
  flex-wrap: wrap;
  row-gap: 2px;
}

.stat-row__label {
  flex: 1 1 auto;
  min-width: 0;
}

.stat-row__value-wrap {
  display: inline-flex;
  align-items: center;
  justify-content: flex-end;
  flex-wrap: wrap;
  gap: var(--khaen-space-1);
  min-width: 0;
  max-width: 100%;
}

.stat-row__value {
  overflow-wrap: anywhere;
}

.stat-row__value--ok {
  color: var(--khaen-success);
}

.stat-row__value--warn {
  color: var(--khaen-warning);
}

.stat-row__value--bad {
  color: var(--khaen-danger);
}

.stat-row__suffix {
  color: var(--khaen-text-secondary);
  font-family: var(--khaen-font-mono);
  font-size: var(--khaen-font-aux);
}

.stat-row__copy {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  padding: 0;
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-tag);
  background: transparent;
  color: var(--khaen-text-secondary);
  cursor: pointer;
  transition: color var(--khaen-transition), border-color var(--khaen-transition);
  flex: none;
}

.stat-row__copy:hover {
  color: var(--khaen-accent);
  border-color: var(--khaen-accent);
}

.stat-row__copy.is-copied {
  color: var(--khaen-success);
  border-color: var(--khaen-success);
}
</style>
