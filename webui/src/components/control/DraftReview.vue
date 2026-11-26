<script setup lang="ts">
/**
 * 共享两阶段草稿审查卡（prepare 回执）—— volume / routing / layout / mode / format 共用。
 *
 * 纪律（docs/API.md §1 / §9）：
 * - 只展示后端真实回执字段；changed/diff/current/target/graph 是**可选**字段：
 *   缺失（null）显示「服务端未上报」，不臆造；
 * - commit/abort 只发事件，实际请求由页面调用 control store（401 交给会话 store）；
 * - graph_id / planned_version / admission / blockers 如实展示，便于用户确认风险。
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { ControlDraft } from '@/api/control'
import { shortenId } from '@/utils/format'

const props = defineProps<{
  draft: ControlDraft
  /** 控制面操作进行中（禁用两个按钮）。 */
  busy?: boolean
  /** commit 按钮文案键（缺省 draftReview.commit）。 */
  commitLabelKey?: string
  /** abort 按钮文案键（缺省 draftReview.abort）。 */
  abortLabelKey?: string
}>()

const emit = defineEmits<{
  (event: 'commit'): void
  (event: 'abort'): void
}>()

const { t } = useI18n({ useScope: 'global' })

function textOrNull(value: string | null): string | null {
  return value === null || value.length === 0 ? null : value
}

/** changed 三态：true/false/未上报（null）。 */
const changedText = computed(() => {
  if (props.draft.changed === true) {
    return t('draftReview.changedYes')
  }
  if (props.draft.changed === false) {
    return t('draftReview.changedNo')
  }
  return t('draftReview.notReported')
})

const surfaceText = computed(() => {
  const surface = props.draft.surface
  return surface === null ? null : t('draftReview.surface') + ' · ' + surface
})

const graphIdText = computed(() => shortenId(props.draft.graph_id, 10))

function jsonText(value: unknown): string | null {
  if (value === null || value === undefined) {
    return null
  }
  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return String(value)
  }
}

const diffText = computed(() => jsonText(props.draft.diff))
const currentText = computed(() => jsonText(props.draft.current))
const targetText = computed(() => jsonText(props.draft.target))
const graphText = computed(() => jsonText(props.draft.graph))
const admissionText = computed(() => jsonText(props.draft.admission))
const blockers = computed(() => props.draft.blockers ?? [])
</script>

<template>
  <section class="draft-review" data-testid="draft-review">
    <header class="draft-review__head">
      <h3 class="draft-review__title">{{ t('draftReview.title') }}</h3>
      <span v-if="surfaceText !== null" class="draft-review__surface mono">{{ surfaceText }}</span>
    </header>
    <p class="aux-text">{{ t('draftReview.desc') }}</p>

    <dl class="draft-review__grid">
      <div class="draft-review__row">
        <dt>{{ t('draftReview.changed') }}</dt>
        <dd>{{ changedText }}</dd>
      </div>
      <div class="draft-review__row">
        <dt>{{ t('draftReview.plannedVersion') }}</dt>
        <dd>{{ draft.planned_version === null ? t('common.dash') : String(draft.planned_version) }}</dd>
      </div>
      <div class="draft-review__row">
        <dt>{{ t('draftReview.graphId') }}</dt>
        <dd class="mono">{{ graphIdText ?? t('common.dash') }}</dd>
      </div>
      <div v-if="draft.mode !== null" class="draft-review__row">
        <dt>{{ t('draftReview.mode') }}</dt>
        <dd>{{ draft.mode }}</dd>
      </div>
      <div v-if="draft.format !== null" class="draft-review__row">
        <dt>{{ t('draftReview.format') }}</dt>
        <dd class="mono">
          {{ draft.format.sample_rate ?? t('common.dash') }} Hz /
          {{ draft.format.block_size ?? t('common.dash') }} /
          {{ draft.format.channels ?? t('common.dash') }}
        </dd>
      </div>
      <div v-if="draft.device_restart_required !== null" class="draft-review__row">
        <dt>{{ t('draftReview.deviceRestart') }}</dt>
        <dd>{{ draft.device_restart_required ? t('common.yes') : t('common.no') }}</dd>
      </div>
      <div v-if="textOrNull(draft.applies_to) !== null" class="draft-review__row">
        <dt>{{ t('draftReview.appliesTo') }}</dt>
        <dd>{{ draft.applies_to }}</dd>
      </div>
      <div v-if="blockers.length > 0" class="draft-review__row draft-review__row--warn">
        <dt>{{ t('draftReview.blockers') }}</dt>
        <dd>
          <ul class="draft-review__list">
            <li v-for="item in blockers" :key="item">{{ item }}</li>
          </ul>
        </dd>
      </div>
      <div v-if="textOrNull(draft.note) !== null" class="draft-review__row">
        <dt>{{ t('draftReview.note') }}</dt>
        <dd>{{ draft.note }}</dd>
      </div>
    </dl>

    <details v-if="diffText !== null" class="draft-review__block">
      <summary>{{ t('draftReview.diff') }}</summary>
      <pre class="draft-review__json mono">{{ diffText }}</pre>
    </details>
    <details v-if="currentText !== null" class="draft-review__block">
      <summary>{{ t('draftReview.current') }}</summary>
      <pre class="draft-review__json mono">{{ currentText }}</pre>
    </details>
    <details v-if="targetText !== null" class="draft-review__block">
      <summary>{{ t('draftReview.target') }}</summary>
      <pre class="draft-review__json mono">{{ targetText }}</pre>
    </details>
    <details v-if="graphText !== null" class="draft-review__block">
      <summary>{{ t('draftReview.graph') }}</summary>
      <pre class="draft-review__json mono">{{ graphText }}</pre>
    </details>
    <details v-if="admissionText !== null" class="draft-review__block">
      <summary>{{ t('draftReview.admission') }}</summary>
      <pre class="draft-review__json mono">{{ admissionText }}</pre>
    </details>

    <p class="aux-text">{{ t('draftReview.changedSourceHint') }}</p>
    <p class="draft-review__token aux-text">
      {{ t('draftReview.engineToken') }}:
      <span class="mono">{{ shortenId(draft.engine_token, 8) ?? t('common.dash') }}</span>
    </p>

    <div class="draft-review__actions">
      <el-button :disabled="props.busy === true" data-testid="draft-review-abort" @click="emit('abort')">
        {{ t(props.abortLabelKey ?? 'draftReview.abort') }}
      </el-button>
      <el-button
        type="primary"
        :loading="props.busy === true"
        :disabled="props.busy === true"
        data-testid="draft-review-commit"
        @click="emit('commit')"
      >
        {{ t(props.commitLabelKey ?? 'draftReview.commit') }}
      </el-button>
    </div>
  </section>
</template>

<style scoped>
.draft-review {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-3);
  border: 1px dashed var(--khaen-border-strong);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
}

.draft-review__head {
  display: flex;
  align-items: baseline;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.draft-review__title {
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
}

.draft-review__surface {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

.draft-review__grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  gap: var(--khaen-space-2);
}

.draft-review__row {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.draft-review__row dt {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

.draft-review__row dd {
  margin: 0;
  overflow-wrap: anywhere;
}

.draft-review__row--warn dd {
  color: var(--khaen-warn, var(--khaen-text));
}

.draft-review__list {
  margin: 0;
  padding-left: 18px;
}

.draft-review__block summary {
  cursor: pointer;
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

.draft-review__json {
  max-height: 220px;
  overflow: auto;
  margin: var(--khaen-space-1) 0 0;
  padding: var(--khaen-space-2);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-bg-subtle, rgba(127, 127, 127, 0.08));
  font-size: 12px;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.draft-review__actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--khaen-space-2);
}
</style>
