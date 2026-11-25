<script setup lang="ts">
// 内容布局 / 中置下混：接入 task-316 真实契约 GET/PUT /api/content/layout（khaen-host-content/v1）。
// 501 not_wired → 「待接线（host 需 plan-arena 图）」+ 控件禁用；预设/忙/超时/主机不可用各自给原因；不伪造 actual。
import { computed, onMounted, ref } from 'vue'
import PanelCard from '@/components/PanelCard.vue'
import AckPanel from '@/components/AckPanel.vue'
import {
  contentState, contentPhaseText, CONTENT_NOT_WIRED_HINT, draftFromCurrent,
  loadContentLayout, presetInputList, presetOutputError, presetOutputList, saveContentLayout
} from '@/stores/content'
import type { AckView, ContentLayoutDraft } from '@/api/types'

const draft = ref<ContentLayoutDraft>(draftFromCurrent(null))
const ack = ref<AckView | null>(null)
const initialized = ref(false)

const current = computed(() => contentState.current)
const phase = computed(() => contentState.phase)
const notWired = computed(() => phase.value === 'not_wired')
const wired = computed(() => phase.value === 'ok')
/** busy / timeout 是瞬态：保留控件可重试，不把表单锁死。 */
const transientError = computed(() => phase.value === 'busy' || phase.value === 'timeout')
const presetsAvailable = computed(() => {
  const body = contentState.body
  return body?.presets?.available === true && !presetOutputError(body)
})
const inputList = computed(() => presetInputList(contentState.body))
const outputList = computed(() => presetOutputList(contentState.body))
const outputError = computed(() => presetOutputError(contentState.body))
const controlsDisabled = computed(() =>
  !(phase.value === 'ok' || transientError.value) || !presetsAvailable.value || contentState.saving
)
const presetsErrorText = computed(() => {
  const presets = contentState.body?.presets
  if (!presets) { return '' }
  const output = presets.output
  if (output && !Array.isArray(output) && output.available === false) { return output.error }
  return presets.error ?? ''
})
const chipClass = computed(() => {
  if (notWired.value) { return 'chip--info' }
  if (wired.value) { return 'chip--ok' }
  if (phase.value === 'loading') { return '' }
  return 'chip--warn'
})

function preferredDraft(): ContentLayoutDraft {
  const inputs = inputList.value
  const outputs = outputList.value
  return {
    input_layout: inputs.find((value) => value === 'nsp-5.1.2') ?? inputs[0] ?? '',
    output_layout: outputs.find((value) => value === '4.1.2') ?? outputs[0] ?? '',
    center: 'auto',
    program_peak_dbfs: -18
  }
}

function syncDraft(): void {
  // 停用（enabled:false）后后端 current.input_layout/output_layout 会变空字符串；
  // 此时保留用户刚选过的布局值，避免「停用后无法再启用」。
  const next = draftFromCurrent(contentState.current)
  const fallback = preferredDraft()
  next.input_layout = next.input_layout || draft.value.input_layout || fallback.input_layout
  next.output_layout = next.output_layout || draft.value.output_layout || fallback.output_layout
  next.center = next.center || draft.value.center || 'auto'
  if (next.program_peak_dbfs === undefined || Number.isNaN(next.program_peak_dbfs)) {
    next.program_peak_dbfs = draft.value.program_peak_dbfs ?? -18
  }
  draft.value = next
}

onMounted(async () => {
  await loadContentLayout()
  syncDraft()
  initialized.value = true
})

function ackFromAck(): AckView {
  const ackBody = contentState.lastAck
  const applied = ackBody?.applied === true
  return {
    kind: ackBody?.actual === true ? 'actual' : 'synthetic',
    engine: contentState.body?.engine ?? null,
    synthetic: ackBody?.actual === true ? false : true,
    actual: ackBody?.actual === true,
    detail: `内容布局回执：actual=${ackBody?.actual ?? '—'} · applied=${ackBody?.applied ?? '—'}` +
      ` · applied_block=${ackBody?.applied_block ?? '—'} · adapter_status=${ackBody?.current?.adapter_status ?? '—'}` +
      ` · ${ackBody?.current?.input_layout ?? '—'} → ${ackBody?.current?.output_layout ?? '—'}` +
      `（center=${ackBody?.current?.center ?? '—'}）`,
    raw: ackBody
  }
}

async function save(): Promise<void> {
  if (!draft.value.input_layout || !draft.value.output_layout) {
    ack.value = {
      kind: 'error', engine: null, synthetic: null, actual: null,
      detail: '请先选择输入布局与输出布局再保存（后端会以 invalid_field 拒绝空布局；本页不发空请求）。',
      raw: { draft: { ...draft.value } }
    }
    return
  }
  const result = await saveContentLayout({ ...draft.value })
  if (result === 'unsupported') {
    ack.value = {
      kind: 'unsupported', engine: null, synthetic: null, actual: null,
      detail: `保存未生效：${CONTENT_NOT_WIRED_HINT}。后端返回 ${contentState.errorCode || 'not_wired'}：${contentState.message}`,
      raw: { error: contentState.message, code: contentState.errorCode }
    }
    return
  }
  if (result === 'error') {
    ack.value = {
      kind: 'error', engine: null, synthetic: null, actual: null,
      detail: `保存失败（${contentPhaseText(phase.value)}）：${contentState.message}`,
      raw: { error: contentState.message, code: contentState.errorCode }
    }
    return
  }
  ack.value = ackFromAck()
  syncDraft()
}

async function disable(): Promise<void> {
  const result = await saveContentLayout({ enabled: false })
  if (result === 'unsupported') {
    ack.value = {
      kind: 'unsupported', engine: null, synthetic: null, actual: null,
      detail: `停用未生效：${CONTENT_NOT_WIRED_HINT}。${contentState.message}`,
      raw: { error: contentState.message, code: contentState.errorCode }
    }
    return
  }
  if (result === 'error') {
    ack.value = {
      kind: 'error', engine: null, synthetic: null, actual: null,
      detail: `停用失败：${contentState.message}`,
      raw: { error: contentState.message, code: contentState.errorCode }
    }
    return
  }
  ack.value = ackFromAck()
  void 0
}

async function reload(): Promise<void> {
  await loadContentLayout()
  syncDraft()
}
</script>

<template>
  <PanelCard
    title="内容布局 / 中置下混"
    subtitle="GET/PUT /api/content/layout（khaen-host-content/v1）；未接线/预设不可用/忙/超时都会显式标注，不伪造 actual"
  >
    <template #extra>
      <span class="chip" :class="chipClass">{{ contentPhaseText(phase) }}</span>
      <span v-if="wired" class="chip chip--mono">engine={{ contentState.body?.engine ?? '—' }}</span>
      <span v-if="wired" class="chip" :class="contentState.body?.actual ? 'chip--ok' : 'chip--warn'">
        actual={{ contentState.body?.actual ?? '—' }}
      </span>
    </template>

    <div v-if="notWired" class="banner banner--info" style="margin-bottom: 12px">
      <div>
        <div class="banner__title">{{ CONTENT_NOT_WIRED_HINT }}</div>
        <div class="banner__body">
          后端返回 {{ contentState.errorCode || 'not_wired' }}：{{ contentState.message || 'host 当前无 content adapter' }}。<br />
          当前生产 host graph=stereo_identity，该 501 属预期；接线（host plan-arena 图）后本面板自动可用，控件暂禁用。
        </div>
      </div>
    </div>
    <div v-else-if="phase === 'presets_unavailable'" class="banner banner--warn" style="margin-bottom: 12px">
      <div>
        <div class="banner__title">布局预设不可用（layout_presets_unavailable）</div>
        <div class="banner__body">
          {{ contentState.message || presetsErrorText }}<template v-if="outputError && outputError !== contentState.message"> · {{ outputError }}</template>；
          布局下拉与保存已禁用（无法确认可用布局）。presets_path 见下方状态卡。
        </div>
      </div>
    </div>
    <div v-else-if="phase === 'busy'" class="banner banner--warn" style="margin-bottom: 12px">
      <div><div class="banner__title">内容路径忙（content_busy）</div><div class="banner__body">{{ contentState.message }}；请稍后重试。</div></div>
    </div>
    <div v-else-if="phase === 'unavailable'" class="banner banner--danger" style="margin-bottom: 12px">
      <div><div class="banner__title">host 不可用（host_unavailable）</div><div class="banner__body">{{ contentState.message }}</div></div>
    </div>
    <div v-else-if="phase === 'timeout'" class="banner banner--warn" style="margin-bottom: 12px">
      <div><div class="banner__title">内容适配超时（content_timeout）</div><div class="banner__body">{{ contentState.message }}；后端可能仍在生效，请重新读取确认 applied_block。</div></div>
    </div>
    <div v-else-if="phase === 'error'" class="banner banner--danger" style="margin-bottom: 12px">
      <div><div class="banner__title">内容布局读取失败</div><div class="banner__body">{{ contentState.message }}</div></div>
    </div>

    <div class="form-grid">
      <div class="form-field">
        <label>输入布局（presets.input）</label>
        <el-select v-model="draft.input_layout" :disabled="controlsDisabled" filterable placeholder="—">
          <el-option v-for="item in inputList" :key="item" :label="item" :value="item" />
        </el-select>
      </div>
      <div class="form-field">
        <label>输出布局（presets.output）</label>
        <el-select v-model="draft.output_layout" :disabled="controlsDisabled" filterable placeholder="—">
          <el-option v-for="item in outputList" :key="item" :label="item" :value="item" />
        </el-select>
        <span v-if="outputError" class="hint" style="color: var(--khaen-warn)">presets 输出不可用：{{ outputError }}</span>
      </div>
      <div class="form-field">
        <label>中置模式 center</label>
        <el-radio-group v-model="draft.center" :disabled="controlsDisabled">
          <el-radio value="auto">auto</el-radio>
          <el-radio value="present">present</el-radio>
          <el-radio value="absent">absent</el-radio>
        </el-radio-group>
      </div>
      <div class="form-field">
        <label>节目峰值 program_peak_dbfs</label>
        <el-input-number
          v-model="draft.program_peak_dbfs"
          :min="-60"
          :max="0"
          :step="0.5"
          :precision="1"
          :disabled="controlsDisabled"
          style="width: 100%"
        />
      </div>
    </div>

    <div class="actions" style="margin-top: 12px">
      <el-button type="primary" :disabled="controlsDisabled" :loading="contentState.saving" @click="save">
        保存并应用
      </el-button>
      <el-button :disabled="controlsDisabled" :loading="contentState.saving" @click="disable">停用内容适配</el-button>
      <el-button :disabled="contentState.saving" @click="reload">重新读取</el-button>
      <span class="hint">PUT body: {input_layout, output_layout, center, program_peak_dbfs} 或 {enabled:false}</span>
    </div>

    <div class="kv" style="margin-top: 12px">
      <div class="kv__item"><span class="kv__label">已配置 configured</span>
        <span class="kv__value">{{ current?.configured ?? '—' }}</span></div>
      <div class="kv__item"><span class="kv__label">适配器状态 adapter_status</span>
        <span class="kv__value">{{ current?.adapter_status ?? '—' }}</span></div>
      <div class="kv__item"><span class="kv__label">已应用 applied</span>
        <span class="kv__value">{{ current?.applied ?? '—' }}</span></div>
      <div class="kv__item"><span class="kv__label">生效块号 applied_block</span>
        <span class="kv__value mono">{{ current?.applied_block ?? '—' }}</span></div>
      <div class="kv__item"><span class="kv__label">已应用块数 applied_blocks</span>
        <span class="kv__value mono">{{ current?.applied_blocks ?? '—' }}</span></div>
      <div class="kv__item"><span class="kv__label">源通道 / 中置通道</span>
        <span class="kv__value mono">{{ current?.source_channels ?? '—' }} / {{ current?.center_channel ?? '—' }}</span></div>
      <div class="kv__item"><span class="kv__label">program_peak_dbfs / measured</span>
        <span class="kv__value mono">{{ current?.program_peak_dbfs ?? '—' }} / {{ current?.program_peak_measured_dbfs ?? '—' }}</span></div>
      <div class="kv__item"><span class="kv__label">plan 输入/输出通道</span>
        <span class="kv__value mono">{{ contentState.body?.plan_input_channels ?? '—' }} / {{ contentState.body?.plan_output_channels ?? '—' }}</span></div>
      <div class="kv__item"><span class="kv__label">presets_path</span>
        <span class="kv__value mono">{{ contentState.body?.presets_path ?? '—' }}</span></div>
      <div class="kv__item"><span class="kv__label">schema</span>
        <span class="kv__value mono">{{ contentState.body?.schema ?? '—' }}</span></div>
    </div>

    <div v-if="contentState.body" class="truth-tags" style="margin-top: 10px">
      <span v-for="flag in contentState.body.wired ?? []" :key="'w-' + flag" class="chip chip--ok">wired: {{ flag }}</span>
      <span v-for="flag in contentState.body.not_wired ?? []" :key="'n-' + flag" class="chip chip--warn">not_wired: {{ flag }}</span>
    </div>

    <div class="banner banner--info" style="margin-top: 12px">
      <div>
        <div class="banner__title">语义（C 下混规则）</div>
        <div class="banner__body">
          C 以 −3 dB 折进 FL/FR（dual-mono 下混），随后 C 通道置零；只在「输入有 C 且输出无 C」时生效
          （adapter_status=applied）；输入无 C 或输出含 C 时整体 passthrough，不改变任何通道。
          program_peak_dbfs 为内容峰值参考（默认 −18 dBFS），measured 为实测值（后端未测得时为 null）。
        </div>
      </div>
    </div>

    <AckPanel :ack="ack" title="内容布局回执" style="margin-top: 12px" />
    <div v-if="!ack && initialized" class="hint" style="margin-top: 8px">尚无保存回执；「待接线」状态下保存会被后端拒绝并如实回显。</div>
  </PanelCard>
</template>