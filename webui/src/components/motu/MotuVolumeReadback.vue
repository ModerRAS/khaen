<script setup lang="ts">
/**
 * MOTU 当前音量回读（GET /api/motu 的 vol[]/mute 是设备真值，不是控制面回执）。
 *
 * - linear 是设备 datastore（koBusFader）回读值，dB 是 20*log10 换算；linear 0 = 静音（-inf dB）；
 * - 设备没上报回读（vol=null）时显示「未上报 + reasons 原文」，不编数值；
 * - 明确标注这不是逐 lane 保护增益（后者在下方增益卡，且没有读接口）。
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import PanelCard from '@/components/PanelCard.vue'
import TruthTag from '@/components/TruthTag.vue'
import { motuReasonFor } from '@/api/motu'
import type { MotuStatus } from '@/api/motu'
import { motuVolumeValueText } from '@/components/motu/format'

const props = defineProps<{
  status: MotuStatus | null
}>()

const { t, locale } = useI18n({ useScope: 'global' })

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))

const volumes = computed(() => props.status?.vol ?? null)

const volReason = computed(() => motuReasonFor(props.status, 'vol'))
const muteReason = computed(() => motuReasonFor(props.status, 'mute'))

const truth = computed(() => (volumes.value === null ? 'notWired' : 'actual'))

interface ReadbackRow {
  label: string
  db: string | null
  linear: string | null
  silent: boolean
}

const readbackRows = computed<ReadbackRow[]>(() => {
  const volumesValue = volumes.value
  if (volumesValue === null) {
    return []
  }
  return volumesValue.map((entry) => {
    const value = motuVolumeValueText({ linear: entry.linear, db: entry.db }, localeId.value)
    return {
      label: entry.name === null ? t('motu.readback.channel', { channel: entry.channel }) : entry.name,
      db: value.db,
      linear: value.linear,
      silent: value.silent
    }
  })
})

const muteText = computed(() => {
  const mute = props.status?.mute ?? null
  if (mute === null) {
    return null
  }
  return mute ? t('motu.readback.muteOn') : t('motu.readback.muteOff')
})
</script>

<template>
  <PanelCard title-key="motu.readback.title" desc-key="motu.readback.desc" :truth="truth">
    <p v-if="volumes === null" class="aux-text">
      {{ t('motu.readback.volNull') }}
      <TruthTag v-if="volReason !== null" kind="notWired" :reason="volReason" />
    </p>
    <p v-else-if="readbackRows.length === 0" class="aux-text">{{ t('motu.readback.noChannels') }}</p>
    <ul v-else class="kv-list motu-volumes">
      <li v-for="row in readbackRows" :key="row.label" class="kv-list__row motu-volumes__row">
        <span class="kv-list__label">{{ row.label }}</span>
        <span class="motu-volumes__value">
          <template v-if="row.db !== null">
            <span class="kv-list__value mono">{{ row.db }} {{ t('unit.db') }}</span>
          </template>
          <template v-else-if="row.silent">
            <span class="kv-list__value mono motu-volumes__silent">{{ t('motu.readback.minusInfinity') }}</span>
          </template>
          <template v-else>
            <span class="kv-list__value kv-list__value--muted">{{ t('common.dash') }}</span>
            <TruthTag kind="notWired" :reason="volReason" />
          </template>
          <span v-if="row.linear !== null" class="aux-text motu-volumes__linear">
            {{ t('motu.readback.linear') }} {{ row.linear }}
          </span>
        </span>
      </li>
    </ul>

    <div class="motu-volumes__mute">
      <span class="kv-list__label">{{ t('motu.readback.mute') }}</span>
      <span class="kv-list__value">
        {{ muteText ?? t('common.dash') }}
      </span>
      <TruthTag v-if="muteText === null" kind="notWired" :reason="muteReason" />
      <TruthTag v-else kind="actual" :reason="null" />
    </div>

    <p class="aux-text">{{ t('motu.readback.source') }}</p>
  </PanelCard>
</template>

<style scoped>
.motu-volumes {
  margin: 0;
}

.motu-volumes__row {
  align-items: center;
  flex-wrap: wrap;
}

.motu-volumes__value {
  display: inline-flex;
  align-items: baseline;
  gap: var(--khaen-space-1);
  flex-wrap: wrap;
}

.motu-volumes__linear {
  font-family: var(--khaen-font-mono);
  font-size: 11px;
}

.motu-volumes__silent {
  color: var(--khaen-text-muted);
}

.motu-volumes__mute {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}
</style>
