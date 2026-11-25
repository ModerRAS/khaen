<script setup lang="ts">
// 真实性徽标：engine + synthetic/actual + real_audio，全部原样来自后端，UI 不做推断。
import { computed } from 'vue'

const props = withDefaults(defineProps<{
  engine?: string | null
  synthetic?: boolean | null
  actual?: boolean | null
  realAudio?: boolean | null
  modeServices?: string | null
  sourceLabel?: string
  compact?: boolean
}>(), {
  engine: null,
  synthetic: null,
  actual: null,
  realAudio: null,
  modeServices: null,
  sourceLabel: '',
  compact: false
})

const truth = computed<'synthetic' | 'actual' | 'unknown'>(() => {
  if (props.synthetic === true) { return 'synthetic' }
  if (props.actual === true) { return 'actual' }
  if (props.synthetic === false && props.actual === false) { return 'unknown' }
  return 'unknown'
})

const syntheticTip = computed(() =>
  'synthetic=true 表示回执来自内存/离线合成模型（engine=' + (props.engine ?? 'unknown') +
  '），没有真实 host/MOTU/4-worker actual ack。本界面不会把它显示成「已生效（actual）」。'
)

const actualTip = 'actual=true 表示回执来自真实 host engine（真实 block 与 worker all-ack）。'

const realAudioTip = computed(() =>
  props.realAudio === true
    ? 'real_audio=true：本次链路经过真实声卡（MOTU/UMIK）。'
    : 'real_audio=false：离线合成链路，未经过真实声卡。'
)
</script>

<template>
  <span class="truth-tags">
    <span v-if="engine" class="chip chip--mono" :title="'engine=' + engine">engine={{ engine }}</span>
    <el-tooltip v-if="truth === 'synthetic'" :content="syntheticTip" placement="top">
      <span class="chip chip--warn">synthetic · 非 actual</span>
    </el-tooltip>
    <el-tooltip v-else-if="truth === 'actual'" :content="actualTip" placement="top">
      <span class="chip chip--ok">actual</span>
    </el-tooltip>
    <span v-else class="chip">真实性未知</span>
    <el-tooltip v-if="realAudio !== null && !compact" :content="realAudioTip" placement="top">
      <span class="chip" :class="realAudio ? 'chip--info' : ''">real_audio={{ realAudio ? 'true' : 'false' }}</span>
    </el-tooltip>
    <span v-if="modeServices && !compact" class="chip" :title="'mode_services=' + modeServices">
      mode_services={{ modeServices }}
    </span>
    <span v-if="sourceLabel && !compact" class="chip">{{ sourceLabel }}</span>
  </span>
</template>

<style scoped>
.truth-tags { display: inline-flex; flex-wrap: wrap; gap: 6px; align-items: center; max-width: 100%; }
</style>
