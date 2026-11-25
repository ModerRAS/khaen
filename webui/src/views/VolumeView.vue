<script setup lang="ts">
// 音量与静音：滑杆 + 静音开关 + 明确区分 synthetic / actual / unsupported 的回执区。
import { computed, onMounted, ref, watch } from 'vue'
import { ElMessage } from 'element-plus'
import { Mic, Mute as MuteIcon } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import AckPanel from '@/components/AckPanel.vue'
import TruthTag from '@/components/TruthTag.vue'
import { api, ApiError, errorText } from '@/api/client'
import { refreshRuntime, runtimeState } from '@/stores/runtime'
import { formatDb, formatClock } from '@/utils/format'
import type { AckView, MuteBody, VolumeBody } from '@/api/types'

const VOLUME_MIN = -150
const VOLUME_MAX = 50
const draft = ref(-12)
const dragging = ref(false)
const busy = ref(false)
const ack = ref<AckView | null>(null)
const lastAckAt = ref(0)

const status = computed(() => runtimeState.status)
const notWired = computed<string[]>(() => status.value?.host?.not_wired ?? [])
const volumeWired = computed(() => !notWired.value.includes('volume'))
const volumeDisabledReason = computed(() =>
  volumeWired.value ? '' : '后端 host.not_wired 含 volume（engine=' + (status.value?.engine ?? '?') + ' 阶段 1 只接线 status/mute）：音量写入会返回 unsupported，控件已禁用以免静默失败；mute 仍可用。'
)
const actualDb = computed(() => status.value?.volume_db ?? null)
const actualMute = computed(() => status.value?.mute ?? false)
const dirty = computed(() => actualDb.value !== null && Math.abs(draft.value - actualDb.value) > 0.01)

watch(() => status.value?.volume_db, (value) => {
  if (dragging.value) { return }
  if (value !== null && value !== undefined) { draft.value = value }
}, { immediate: true })

function ackFrom(payload: VolumeBody | MuteBody, detail: string): AckView {
  const kind: AckView['kind'] = payload.synthetic ? 'synthetic' : payload.actual ? 'actual' : 'info'
  return {
    kind,
    engine: payload.engine ?? null,
    actual: payload.actual ?? null,
    synthetic: payload.synthetic ?? null,
    detail,
    raw: payload
  }
}

function ackFromError(error: unknown): AckView {
  const code = error instanceof ApiError ? error.code : 'error'
  const unsupported = ['unsupported', 'not_implemented', 'not_supported', 'unsupported_api_version'].includes(code)
  return {
    kind: unsupported ? 'unsupported' : 'error',
    engine: null,
    actual: null,
    synthetic: null,
    detail: unsupported
      ? `后端明确不支持该操作（${code}）：${errorText(error)}`
      : errorText(error),
    raw: error instanceof ApiError ? error.body : String(error)
  }
}

async function applyVolume(): Promise<void> {
  busy.value = true
  try {
    const payload = await api.volumePut(Number(draft.value.toFixed(1)))
    ack.value = ackFrom(
      payload,
      `音量请求 ${formatDb(draft.value)} → 回读 ${formatDb(payload.volume_db)} / mute=${payload.mute} / actual_version=${payload.actual_version} / epoch=${payload.epoch}`
    )
    lastAckAt.value = Date.now()
    await refreshRuntime()
    ElMessage.success(payload.synthetic ? '已提交（synthetic 回执，非 actual）' : '音量已生效（actual）')
  } catch (error) {
    ack.value = ackFromError(error)
  } finally {
    busy.value = false
  }
}

async function toggleMute(): Promise<void> {
  busy.value = true
  const next = !actualMute.value
  try {
    const payload = await api.mutePut(next)
    ack.value = ackFrom(
      payload,
      `静音请求 mute=${next} → 回读 mute=${payload.mute} / volume=${formatDb(payload.volume_db)} / actual_version=${payload.actual_version}`
    )
    lastAckAt.value = Date.now()
    await refreshRuntime()
    ElMessage.success(payload.synthetic ? '已提交（synthetic 回执，非 actual）' : '静音状态已生效（actual）')
  } catch (error) {
    ack.value = ackFromError(error)
  } finally {
    busy.value = false
  }
}

async function refresh(): Promise<void> {
  await refreshRuntime()
  if (actualDb.value !== null) { draft.value = actualDb.value }
}

onMounted(() => { void refresh() })
</script>

<template>
  <div class="page">
    <PageHeader
      title="音量与静音"
      subtitle="-150.0 … +50.0 dB（后端校验）；mute 与音量独立。回执区分 synthetic / actual / unsupported。"
    >
      <template #right>
        <el-button :loading="runtimeState.loading" @click="refresh">刷新实际值</el-button>
      </template>
    </PageHeader>

    <div v-if="runtimeState.error" class="banner banner--danger">
      <div><div class="banner__title">读取失败</div><div class="banner__body">{{ runtimeState.error }}</div></div>
    </div>

    <div v-if="!volumeWired" class="banner banner--info">
      <div>
        <div class="banner__title">音量写入未接线（unsupported）— 控件已禁用</div>
        <div class="banner__body">{{ volumeDisabledReason }}</div>
      </div>
    </div>

    <div v-if="status?.synthetic" class="banner banner--warn">
      <div>
        <div class="banner__title">engine={{ status.engine }}（synthetic）— 回执不是板端 actual</div>
        <div class="banner__body">音量/静音写入走内存 engine 模型；页面按回执 synthetic 标注，不显示成「已生效（actual）」。</div>
      </div>
    </div>

    <div class="grid-2">
      <PanelCard title="音量" subtitle="拖动后点「应用音量」，以回读为准确认">
        <template #extra>
          <TruthTag
            :engine="status?.engine"
            :synthetic="status?.synthetic"
            :actual="status?.actual"
            compact
          />
        </template>
        <div class="volume-readout">{{ formatDb(draft) }}</div>
        <input
          v-model.number="draft"
          class="big-range"
          type="range"
          :min="VOLUME_MIN"
          :max="VOLUME_MAX"
          step="0.5"
          aria-label="音量 dB"
          :disabled="!volumeWired"
          @pointerdown="dragging = true"
          @pointerup="dragging = false"
          @keydown="dragging = true"
          @keyup="dragging = false"
        />
        <div class="hint" style="display: flex; justify-content: space-between">
          <span>{{ VOLUME_MIN }} dB</span>
          <span>0 dB</span>
          <span>+{{ VOLUME_MAX }} dB</span>
        </div>
        <div class="actions" style="margin-top: 14px">
          <el-input-number v-model="draft" :min="VOLUME_MIN" :max="VOLUME_MAX" :step="0.5" :precision="1" :disabled="!volumeWired" style="width: 150px" />
          <el-button type="primary" :loading="busy" :disabled="!volumeWired" @click="applyVolume">应用音量</el-button>
          <span v-if="!volumeWired" class="hint">未接线：{{ volumeDisabledReason }}</span>
          <el-button :disabled="!dirty" @click="draft = actualDb ?? draft">还原实际值</el-button>
        </div>
        <div class="kv" style="margin-top: 14px">
          <div class="kv__item"><span class="kv__label">实际音量（REST status）</span>
            <span class="kv__value">{{ formatDb(actualDb) }}</span></div>
          <div class="kv__item"><span class="kv__label">actual_version</span>
            <span class="kv__value">{{ status?.actual_version ?? '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">epoch</span>
            <span class="kv__value">{{ status?.epoch ?? '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">运行状态</span>
            <span class="kv__value">{{ status?.runtime_state ?? '—' }}</span></div>
        </div>
      </PanelCard>

      <PanelCard title="静音" subtitle="mute 切换后回读 mute/volume；与音量写入分开记录">
        <div class="mute-row">
          <div>
            <div class="stat__label">当前静音状态</div>
            <div class="stat__value" :class="{ 'is-warn': actualMute }">{{ actualMute ? 'ON（静音）' : 'OFF（出声）' }}</div>
          </div>
          <el-switch
            :model-value="actualMute"
            size="large"
            :disabled="busy"
            active-text="静音"
            inactive-text="取消"
            style="min-height: 44px"
            @change="toggleMute"
          />
        </div>
        <p class="hint" style="margin-top: 10px">
          静音是独立标志位：mute=true 时音量值仍保留，取消静音后恢复到原音量（以回读为准）。
        </p>
        <div class="actions" style="margin-top: 10px">
          <el-button :loading="busy" @click="toggleMute">
            <el-icon style="margin-right: 6px"><component :is="actualMute ? Mic : MuteIcon" /></el-icon>
            {{ actualMute ? '取消静音' : '静音' }}
          </el-button>
        </div>
        <div class="kv" style="margin-top: 14px">
          <div class="kv__item"><span class="kv__label">回执时间</span>
            <span class="kv__value">{{ lastAckAt ? formatClock(lastAckAt) : '—' }}</span></div>
          <div class="kv__item"><span class="kv__label">engine / synthetic</span>
            <span class="kv__value mono">{{ status?.engine ?? '—' }} / {{ status?.synthetic ?? '—' }}</span></div>
        </div>
      </PanelCard>
    </div>

    <PanelCard title="操作回执" subtitle="synthetic = 合成回执（非 actual）；unsupported = 后端明确未支持">
      <AckPanel :ack="ack" />
      <div v-if="!ack" class="hint">尚无操作。点击「应用音量」或切换静音后，此处显示回执类型与原始 JSON。</div>
    </PanelCard>
  </div>
</template>

<style scoped>
.mute-row { display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-wrap: wrap; }
.stat__value.is-warn { color: var(--khaen-warn); }
@media (max-width: 767px) {
  .mute-row { flex-direction: column; align-items: flex-start; }
}
</style>