<script setup lang="ts">
// 音频格式：当前实际值 + 允许值 + 应用回执（synthetic 明确标注）。
import { computed, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import AckPanel from '@/components/AckPanel.vue'
import TruthTag from '@/components/TruthTag.vue'
import { api, ApiError, errorText } from '@/api/client'
import { refreshRuntime, runtimeState } from '@/stores/runtime'
import type { AckView, AudioFormatBody } from '@/api/types'

const format = ref<AudioFormatBody | null>(null)
const loading = ref(false)
const applying = ref(false)
const loadError = ref('')
const rate = ref(48000)
const chunksize = ref(128)
const ack = ref<AckView | null>(null)

const notWired = computed<string[]>(() => runtimeState.status?.host?.not_wired ?? [])
const formatWired = computed(() => !notWired.value.includes('commit'))
const formatDisabledReason = computed(() =>
  formatWired.value ? '' : '后端 host.not_wired 含 commit（engine=' + (runtimeState.status?.engine ?? '?') + '）：格式应用走 prepare+commit，会返回 unsupported；控件已禁用，避免静默失败。'
)
const limits = computed(() => format.value?.limits)
const rates = computed(() => limits.value?.sample_rates ?? [])
const chunks = computed(() => limits.value?.chunksizes ?? [])
const uiReferenceChunks = computed(() =>
  (limits.value?.ui_reference_chunksizes ?? []).filter((size) => !chunks.value.includes(size))
)
const dirty = computed(() =>
  !!format.value && (rate.value !== format.value.sample_rate || chunksize.value !== format.value.chunksize)
)

async function load(): Promise<void> {
  loading.value = true
  try {
    const body = await api.audioFormatGet()
    format.value = body
    rate.value = body.sample_rate
    chunksize.value = body.chunksize
    loadError.value = ''
  } catch (error) {
    loadError.value = errorText(error)
  } finally {
    loading.value = false
  }
}

async function apply(): Promise<void> {
  applying.value = true
  ack.value = null
  try {
    const body = await api.audioFormatPut(Number(rate.value), Number(chunksize.value))
    const outcome = body.outcome
    ack.value = {
      kind: outcome.synthetic ? 'synthetic' : outcome.actual ? 'actual' : 'info',
      engine: outcome.engine,
      synthetic: outcome.synthetic,
      actual: outcome.actual,
      detail: `格式应用完成：actual_version=${outcome.actual_version} · plan_version=${outcome.plan_version} · blocks_committed=${outcome.blocks_committed} · ack_mask=${outcome.ack_mask} · epoch=${outcome.epoch}`,
      raw: body
    }
    format.value = body.format
    rate.value = body.format.sample_rate
    chunksize.value = body.format.chunksize
    await refreshRuntime()
    ElMessage.success(outcome.synthetic ? '已应用（synthetic 回执，非 actual）' : '音频格式已生效（actual）')
  } catch (error) {
    const code = error instanceof ApiError ? error.code : 'error'
    const unsupported = ['unsupported', 'not_implemented', 'not_supported', 'unsupported_api_version'].includes(code)
    ack.value = {
      kind: unsupported ? 'unsupported' : 'error',
      engine: null,
      synthetic: null,
      actual: null,
      detail: `格式应用失败（未生效）：${errorText(error)}`,
      raw: error instanceof ApiError ? error.body : String(error)
    }
  } finally {
    applying.value = false
  }
}

onMounted(load)
</script>

<template>
  <div class="page">
    <PageHeader
      title="音频格式"
      subtitle="runtime v1 只接受采样率 48k/96k、chunksize 64/128；其余值为 UI 参考，后端会显式 400。"
    >
      <template #right>
        <el-button :icon="Refresh" :loading="loading" @click="load">刷新</el-button>
      </template>
    </PageHeader>

    <div v-if="loadError" class="banner banner--danger">
      <div><div class="banner__title">读取失败</div><div class="banner__body">{{ loadError }}</div></div>
    </div>

    <div v-if="!formatWired" class="banner banner--info">
      <div>
        <div class="banner__title">格式应用未接线（unsupported）— 控件已禁用</div>
        <div class="banner__body">{{ formatDisabledReason }}</div>
      </div>
    </div>

    <div v-if="format?.synthetic" class="banner banner--warn">
      <div>
        <div class="banner__title">engine={{ format.engine }}（synthetic）— source: {{ format.source }}</div>
        <div class="banner__body">格式应用回执来自内存 engine 模型；页面不会显示成板端 actual。</div>
      </div>
    </div>

    <div class="grid-2">
      <PanelCard title="当前实际格式" subtitle="REST /api/audio/format 回读">
        <template #extra>
          <TruthTag :engine="format?.engine" :synthetic="format?.synthetic" :actual="format?.actual" compact />
        </template>
        <div v-if="!format" class="hint">未加载</div>
        <div v-else class="kv">
          <div class="kv__item"><span class="kv__label">input_format</span>
            <span class="kv__value mono">{{ format.input_format }}</span></div>
          <div class="kv__item"><span class="kv__label">output_format</span>
            <span class="kv__value mono">{{ format.output_format }}</span></div>
          <div class="kv__item"><span class="kv__label">sample_rate</span>
            <span class="kv__value">{{ format.sample_rate.toLocaleString() }} Hz</span></div>
          <div class="kv__item"><span class="kv__label">chunksize</span>
            <span class="kv__value">{{ format.chunksize }} frames</span></div>
          <div class="kv__item"><span class="kv__label">bit_depth / channels</span>
            <span class="kv__value">{{ format.bit_depth }} bit / {{ format.channels }} ch</span></div>
          <div class="kv__item"><span class="kv__label">mode</span>
            <span class="kv__value">{{ format.mode }}（{{ format.mode_label }}）</span></div>
          <div class="kv__item"><span class="kv__label">source</span>
            <span class="kv__value">{{ format.source }}</span></div>
          <div class="kv__item"><span class="kv__label">actual_version / epoch</span>
            <span class="kv__value">{{ format.actual_version }} / {{ format.epoch }}</span></div>
          <div class="kv__item"><span class="kv__label">xruns / deadline / clip</span>
            <span class="kv__value">{{ format.xrun.xruns }} / {{ format.xrun.deadline_misses }} / {{ format.xrun.clipped_samples }}</span></div>
          <div class="kv__item"><span class="kv__label">config_path</span>
            <span class="kv__value mono">{{ format.config_path ?? '—（无外部配置；synthetic）' }}</span></div>
        </div>
      </PanelCard>

      <PanelCard title="应用格式" subtitle="PUT /api/audio/format（内部 prepare+commit）">
        <div class="form-grid">
          <div class="form-field">
            <label>sample_rate（后端允许值）</label>
            <el-select v-model="rate">
              <el-option v-for="item in rates" :key="item" :label="item.toLocaleString() + ' Hz'" :value="item" />
            </el-select>
          </div>
          <div class="form-field">
            <label>chunksize（后端允许值）</label>
            <el-select v-model="chunksize">
              <el-option v-for="item in chunks" :key="item" :label="item + ' frames'" :value="item" />
            </el-select>
          </div>
        </div>
        <div class="actions" style="margin-top: 14px">
          <el-button type="primary" :loading="applying" :disabled="!format || !formatWired" @click="apply">应用格式</el-button>
          <el-button :disabled="!format || !dirty" @click="format && (rate = format.sample_rate, chunksize = format.chunksize)">还原实际值</el-button>
        </div>
        <div class="kv" style="margin-top: 14px">
          <div class="kv__item"><span class="kv__label">允许 sample_rate</span>
            <span class="kv__value mono">{{ rates.join(' / ') }}</span></div>
          <div class="kv__item"><span class="kv__label">允许 chunksize</span>
            <span class="kv__value mono">{{ chunks.join(' / ') }}</span></div>
          <div class="kv__item"><span class="kv__label">UI 参考 chunksize（不可选）</span>
            <span class="kv__value mono">{{ uiReferenceChunks.join(' / ') || '—' }}</span></div>
        </div>
        <el-alert
          v-for="(warning, index) in format?.warnings ?? []"
          :key="index"
          :title="warning"
          type="warning"
          :closable="false"
          show-icon
          style="margin-top: 10px"
        />
      </PanelCard>
    </div>

    <PanelCard title="应用回执">
      <AckPanel :ack="ack" title="格式回执" />
      <div v-if="!ack" class="hint">尚无格式应用回执。</div>
    </PanelCard>
  </div>
</template>