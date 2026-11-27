<script setup lang="ts">
/**
 * 音频映射 / Speaker Layout 页（task-73；docs/API.md §12 v1.2）。
 *
 * 本页负责「物理输出 ↔ 音箱角色」与「输入源逐通道角色标注」，与 /layout（方案内容布局 folds）区分。
 * 两步保存：① 保存草稿（PUT /api/audio-profile prepare，展示 validation/diff/token）
 *          ② 应用（POST /api/audio-profile/plan → /api/plan/inspect → /api/prepare → /api/commit）。
 *
 * 诚实边界（不假数据、不假成功）：
 * - 后端 profile 不可读时 GET 返回 profile=null + reasons（storage 损坏/未初始化）：页面显示「未接线」+原因，
 *   编辑区退化为本地草稿（明示），写操作 503 not_wired 如实展示；
 * - 声卡输入能力来自 GET /api/device/inputs，取不到 available:false + 原因，不编通道数；
 * - 每个 enable 项都要显式操作；未使用 DAC 保持无信号（本页不做任何 CueMix 写入）；
 * - 测试音必须用户在确认框确认（audible+confirm 显式 true），默认 -30 dBFS / 1 s，可立即停止。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage, ElMessageBox } from 'element-plus'
import { Refresh } from '@element-plus/icons-vue'
import { annotateGapById, gapLocaleOf } from '@/api/capabilityGaps'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import type { TruthKind } from '@/types/ui'
import {
  AES67_PRESETS,
  LAYOUT_PRESETS,
  SPEAKER_ROLE_GROUPS,
  SPEAKER_TEST_DEFAULT_DB,
  SPEAKER_TEST_DEFAULT_MS,
  SPEAKER_TEST_LEVEL_OPTIONS,
  SPEAKER_TEST_UI_MAX_MS,
  SPEAKER_TEST_UI_MIN_MS,
  applyInputPreset,
  applyLayoutPreset,
  buildDownmixHints,
  buildRoleFanout,
  deriveLayoutLabel,
  enabledOutputs,
  isSpeakerRole,
  summarizeRoleGroups,
  unusedDacs,
  type AudioInputSource,
  type AudioProfile,
  type AudioSourceKind,
  type SpeakerRole,
  type SpeakerTestKind
} from '@/api/audioProfile.ts'
import {
  abortAudioProfile,
  abortAudioProfileApply,
  applyAudioProfile,
  audioProfileState,
  clearSpeakerTestState,
  commitAudioProfile,
  editingProfile,
  loadAudioProfile,
  loadDeviceInputs,
  mergedValidation,
  prepareAudioProfile,
  speakerTestRemainingMs,
  startSpeakerTest,
  stopSpeakerTest
} from '@/stores/audioProfile.ts'

const { t, locale } = useI18n({ useScope: 'global' })

/* ------------------------------------------------------------------ 只读快照 / 本地草稿 */

const snapshot = computed(() => audioProfileState.snapshot)
const deviceInputs = computed(() => audioProfileState.deviceInputs)
const busy = computed(() => audioProfileState.busy)
const error = computed(() => audioProfileState.error)

/** 后端未接线：profile null 但请求成功（或有 not_wired 原因）。 */
const profileNotWired = computed(() => {
  const current = snapshot.value
  if (current === null) {
    return false
  }
  if (current.profile !== null) {
    return false
  }
  return current.reasons['profile'] !== undefined || current.reasons['reason'] !== undefined
})
const notWiredReason = computed<string | null>(() => {
  const current = snapshot.value
  if (current === null) {
    return null
  }
  return current.reasons['profile'] ?? current.reasons['reason'] ?? null
})

const profileTruth = computed<TruthKind>(() => (snapshot.value?.profile != null ? 'actual' : 'notWired'))
const inputsTruth = computed<TruthKind>(() => (deviceInputs.value?.available === true ? 'actual' : 'notWired'))
const deviceInputsText = computed(() => {
  const raw = deviceInputs.value?.reason ?? t('speakerLayout.deviceInputs.unavailable')
  if (deviceInputs.value?.available === true) {
    return raw
  }
  return annotateGapById(raw, 'deviceInputs', gapLocaleOf(locale.value))
})
const speakerTestGap = computed(() =>
  annotateGapById(t('speakerLayout.speakerTest.notWired'), 'speakerTest', gapLocaleOf(locale.value))
)

/** 本地编辑草稿：有快照 profile 用快照，否则退化为默认 2.0（页面明示）。 */
const edit = ref<AudioProfile>(editingProfile(null))
const dirty = ref(false)
const baselineText = ref(JSON.stringify(editingProfile(null)))

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

function resetEdit(profile: AudioProfile): void {
  edit.value = clone(profile)
  baselineText.value = JSON.stringify(edit.value)
  dirty.value = false
}

function markDirty(): void {
  dirty.value = true
}

watch(
  () => snapshot.value?.profile,
  (profile) => {
    if (profile == null) {
      return
    }
    // 用户已有未保存本地编辑时不覆盖（避免轮询吞掉输入）；刷新按钮会显式重置。
    if (!dirty.value) {
      resetEdit(profile)
    }
  }
)

/* ------------------------------------------------------------------ 布局摘要 / 校验 */

const layoutLabel = computed(() => {
  const label = deriveLayoutLabel(edit.value)
  if (!label.startsWith('custom:')) {
    return label
  }
  const [, ground, height, low] = label.split(':')
  return t('speakerLayout.layoutCustom', { ground, height, low })
})
const enabledCount = computed(() => enabledOutputs(edit.value).length)
const roles = computed(() => summarizeRoleGroups(edit.value))
const unused = computed(() => unusedDacs(edit.value))
const hints = computed(() => buildDownmixHints(edit.value))
const fanout = computed(() => buildRoleFanout(edit.value))
const localValidation = computed(() => mergedValidation(audioProfileState.draft))
const validation = computed(() => (audioProfileState.draft !== null ? localValidation.value : { ok: true, errors: [], warnings: [] as typeof localValidation.value.warnings }))
const planSummary = computed(() => snapshot.value?.plan ?? { lanes: null, enabled_outputs: null, dac_channels: null, graph_id: null })

const groundCount = computed(() => roles.value.filter((group) => (SPEAKER_ROLE_GROUPS.ground as readonly string[]).includes(group.role)).reduce((sum, group) => sum + group.count, 0))
const heightCount = computed(() => roles.value.filter((group) => (SPEAKER_ROLE_GROUPS.height as readonly string[]).includes(group.role)).reduce((sum, group) => sum + group.count, 0))

function roleLabel(role: SpeakerRole): string {
  return t('speakerRole.' + role)
}

function groupLabel(key: 'ground' | 'low' | 'height' | 'other'): string {
  return t('speakerRole.group.' + key)
}

const roleCount = computed(() => {
  const counts = new Map<SpeakerRole, number>()
  for (const group of roles.value) {
    counts.set(group.role, group.count)
  }
  return counts
})

function countOf(role: SpeakerRole): number {
  return roleCount.value.get(role) ?? 0
}

/* ------------------------------------------------------------------ 编辑操作 */

function firstFreeDac(): number | null {
  const output = edit.value.outputs.find((item) => !item.enabled || item.role === 'None')
  return output === undefined ? null : output.dac
}

function addRole(role: SpeakerRole): void {
  if (role === 'None') {
    return
  }
  if ((SPEAKER_ROLE_GROUPS.ground as readonly string[]).includes(role) && groundCount.value >= 9) {
    return
  }
  if ((SPEAKER_ROLE_GROUPS.height as readonly string[]).includes(role) && heightCount.value >= 6) {
    return
  }
  const dac = firstFreeDac()
  if (dac === null) {
    return
  }
  const output = edit.value.outputs[dac]
  output.enabled = true
  output.role = role
  output.custom_label = role === 'Custom' ? 'Custom' : null
  markDirty()
}

function removeRole(role: SpeakerRole): void {
  for (let index = edit.value.outputs.length - 1; index >= 0; index -= 1) {
    const output = edit.value.outputs[index]
    if (output.enabled && output.role === role) {
      output.enabled = false
      output.role = 'None'
      output.custom_label = null
      markDirty()
      return
    }
  }
}

function applyPreset(presetId: string): void {
  edit.value = applyLayoutPreset(edit.value, presetId)
  markDirty()
}

function setOutputDac(output: { dac: number }, nextDac: number | undefined): void {
  if (typeof nextDac !== 'number') {
    return
  }
  if (nextDac < 0 || nextDac >= edit.value.outputs.length || nextDac === output.dac) {
    return
  }
  // 交换两个 DAC 行的配置（保证 DAC 序号稳定、不产生重复）。
  const current = edit.value.outputs[output.dac]
  const target = edit.value.outputs[nextDac]
  edit.value.outputs[output.dac] = { ...target, dac: current.dac }
  edit.value.outputs[nextDac] = { ...current, dac: target.dac }
  markDirty()
}

function setOutputRole(output: { dac: number }, role: unknown): void {
  if (!isSpeakerRole(role)) {
    return
  }
  const row = edit.value.outputs[output.dac]
  row.role = role
  row.enabled = role !== 'None'
  row.custom_label = role === 'Custom' ? row.custom_label ?? 'Custom' : null
  markDirty()
}

function setOutputEnabled(output: { dac: number }, enabled: boolean): void {
  const row = edit.value.outputs[output.dac]
  row.enabled = enabled
  if (!enabled) {
    row.role = 'None'
    row.custom_label = null
  } else if (row.role === 'None') {
    row.role = 'L'
  }
  markDirty()
}

function setOutputGain(output: { dac: number }, gain: number | undefined): void {
  const value = typeof gain === 'number' && Number.isFinite(gain) ? gain : 0
  edit.value.outputs[output.dac].gain_db = value
  markDirty()
}

function setOutputLabel(output: { dac: number }, label: string): void {
  edit.value.outputs[output.dac].custom_label = label
  markDirty()
}

/* ------------------------------------------------------------ 输入源编辑 */

const sourceOrder = computed<AudioInputSource[]>(() => edit.value.inputs.sources)

function capabilityChannels(kind: AudioSourceKind): number {
  if (kind === 'aes67') {
    return snapshot.value?.capability.aes67.channels ?? edit.value.inputs.sources.find((source) => source.kind === 'aes67')?.channels.length ?? 2
  }
  const channels = deviceInputs.value?.channels
  return channels !== null && channels !== undefined ? channels.length : edit.value.inputs.sources.find((source) => source.kind === 'device')?.channels.length ?? 2
}

function syncSourceChannels(source: AudioInputSource, count: number): void {
  const width = Number.isInteger(edit.value.inputs.channels) && edit.value.inputs.channels > 0 ? edit.value.inputs.channels : count
  const bounded = Math.min(count, width)
  const next: typeof source.channels = []
  for (let channel = 0; channel < bounded; channel += 1) {
    next.push(source.channels[channel] ?? { channel, role: 'None' })
  }
  source.channels = next
}

function setInputChannels(value: number | undefined): void {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return
  }
  const next = Math.trunc(value)
  edit.value.inputs.channels = Math.min(256, Math.max(1, next))
  markDirty()
}

function setSourceEnabled(index: number, value: boolean): void {
  const source = sourceOrder.value[index]
  if (source === undefined) {
    return
  }
  source.enabled = value
  syncSourceChannels(source, capabilityChannels(source.kind))
  markDirty()
}

function setSourceGain(index: number, value: number | undefined): void {
  const source = sourceOrder.value[index]
  if (source === undefined) {
    return
  }
  source.gain_db = typeof value === 'number' && Number.isFinite(value) ? value : 0
  markDirty()
}

function setInputRole(index: number, channelIndex: number, role: unknown): void {
  const source = sourceOrder.value[index]
  if (source === undefined || !isSpeakerRole(role)) {
    return
  }
  const channel = source.channels[channelIndex]
  if (channel === undefined) {
    return
  }
  channel.role = role
  source.preset = null
  markDirty()
}

function applySourcePreset(index: number, presetId: string): void {
  const source = sourceOrder.value[index]
  if (source === undefined) {
    return
  }
  syncSourceChannels(source, capabilityChannels(source.kind))
  sourceOrder.value[index] = applyInputPreset(source, presetId)
  markDirty()
}

const orderSourceText = computed<string>(() => {
  const source = snapshot.value?.capability.aes67.channel_order_source ?? null
  if (source === 'sdp') {
    return t('speakerLayout.source.orderSdp')
  }
  if (source === 'manual') {
    return t('speakerLayout.source.orderManual')
  }
  return t('speakerLayout.source.orderNone')
})

const deviceInputList = computed(() => {
  const channels = deviceInputs.value?.channels ?? null
  if (channels !== null) {
    return channels
  }
  return []
})

/* ------------------------------------------------------------------ 保存 / 应用 */

const draft = computed(() => audioProfileState.draft)
const apply = computed(() => audioProfileState.apply)
const lastCommit = computed(() => audioProfileState.lastCommit)

function diffText(value: boolean | null): string {
  if (value === true) {
    return t('speakerLayout.draft.changed')
  }
  if (value === false) {
    return t('speakerLayout.draft.unchanged')
  }
  return t('speakerLayout.draft.notReported')
}

async function onPrepare(): Promise<void> {
  const result = await prepareAudioProfile(edit.value)
  if (result !== null) {
    ElMessage.success(t('speakerLayout.draft.title'))
  }
}

async function onCommit(): Promise<void> {
  const ack = await commitAudioProfile()
  if (ack !== null) {
    ElMessage.success(t('speakerLayout.saved'))
    resetEdit(editingProfile(snapshot.value))
  }
}

async function onAbort(): Promise<void> {
  const ok = await abortAudioProfile()
  if (ok) {
    ElMessage.info(t('speakerLayout.draft.abort'))
  }
}

async function onApply(): Promise<void> {
  const outcome = await applyAudioProfile()
  if (outcome.ok) {
    ElMessage.success(
      t('speakerLayout.apply.success', {
        graph: outcome.commit?.graph_id ?? t('common.dash'),
        version: outcome.commit?.version ?? t('common.dash')
      })
    )
  }
}

async function onAbortApply(): Promise<void> {
  await abortAudioProfileApply()
}

/* ------------------------------------------------------------------ 测试音 */

const testMs = ref<number>(SPEAKER_TEST_DEFAULT_MS)
const testDb = ref<number>(SPEAKER_TEST_DEFAULT_DB)
const testKind = ref<SpeakerTestKind>('noise')
const nowTick = ref<number>(Date.now())
let testTimer: ReturnType<typeof setInterval> | null = null

const testState = computed(() => audioProfileState.test)
const testRemaining = computed(() => speakerTestRemainingMs(testState.value, nowTick.value))

function ensureTestTimer(): void {
  if (testTimer !== null) {
    return
  }
  testTimer = setInterval(() => {
    nowTick.value = Date.now()
    if (testState.value !== null && speakerTestRemainingMs(testState.value, nowTick.value) <= 0) {
      clearSpeakerTestState()
      stopTestTimer()
    }
  }, 100)
}

function stopTestTimer(): void {
  if (testTimer !== null) {
    clearInterval(testTimer)
    testTimer = null
  }
}

watch(
  () => testState.value?.active ?? false,
  (active) => {
    if (active) {
      ensureTestTimer()
    } else {
      stopTestTimer()
    }
  }
)

async function onIdentify(dac: number): Promise<void> {
  try {
    await ElMessageBox.confirm(
      t('speakerLayout.speakerTest.confirmBody', {
        dac,
        channel: dac + 1,
        ms: testMs.value,
        db: testDb.value
      }),
      t('speakerLayout.speakerTest.confirmTitle'),
      {
        confirmButtonText: t('speakerLayout.speakerTest.start'),
        cancelButtonText: t('common.cancel'),
        type: 'warning'
      }
    )
  } catch {
    return
  }
  const ack = await startSpeakerTest({
    dac,
    ms: testMs.value,
    kind: testKind.value,
    db: testDb.value,
    audible: true,
    confirm: true
  })
  if (ack !== null) {
    nowTick.value = Date.now()
    ensureTestTimer()
  }
}

async function onStopTest(): Promise<void> {
  const stopped = await stopSpeakerTest()
  if (stopped) {
    ElMessage.info(t('speakerLayout.speakerTest.stopped'))
  }
  stopTestTimer()
}

/* ------------------------------------------------------------------ 生命周期 */

async function reload(): Promise<void> {
  await Promise.all([loadAudioProfile(true), loadDeviceInputs(true)])
  resetEdit(editingProfile(snapshot.value))
}

onMounted(() => {
  void reload()
})

onBeforeUnmount(() => {
  stopTestTimer()
})
</script>

<template>
  <div class="page speaker-layout">
    <PageHeader title-key="nav.speakers" subtitle-key="speakerLayout.desc">
      <template #actions>
        <el-button :loading="busy === 'profile'" @click="reload">
          <el-icon><Refresh /></el-icon>
          {{ t('speakerLayout.reload') }}
        </el-button>
      </template>
    </PageHeader>

    <div v-if="profileNotWired" class="banner banner--warn" role="status" data-testid="speaker-not-wired">
      <strong>{{ t('speakerLayout.notWiredTitle') }}</strong>
      <span>{{ notWiredReason ?? t('speakerLayout.notWiredDesc') }}</span>
    </div>
    <div v-if="dirty" class="banner banner--info" role="status">
      {{ t('speakerLayout.localDraftHint') }}
    </div>
    <div v-if="error !== null" class="banner banner--danger" role="alert" data-testid="speaker-error">
      <strong>{{ t(error.i18nKey) }}</strong>
      <span class="mono">{{ error.code }}</span>
      <span v-if="error.detail !== null">{{ error.detail }}</span>
      <span v-if="error.reason !== null">{{ error.reason }}</span>
    </div>

    <!-- 顶部摘要 -->
    <PanelCard title-key="speakerLayout.layoutLabel" :truth="profileTruth">
      <dl class="summary-grid">
        <div><dt>{{ t('speakerLayout.inputChannels') }}</dt><dd>{{ edit.inputs.channels }}</dd></div>
        <div><dt>{{ t('speakerLayout.deviceChannels') }}</dt><dd>{{ edit.device.channels }}</dd></div>
        <div><dt>{{ t('speakerLayout.planLanes') }}</dt><dd>{{ planSummary.lanes ?? t('common.dash') }}</dd></div>
        <div><dt>{{ t('speakerLayout.graphId') }}</dt><dd class="mono">{{ planSummary.graph_id ?? t('common.dash') }}</dd></div>
        <div><dt>{{ t('speakerLayout.layoutLabel') }}</dt><dd>{{ layoutLabel }}</dd></div>
      </dl>
      <p class="aux-text">{{ t('speakerLayout.lanePreview', { lanes: enabledCount }) }}</p>
    </PanelCard>

    <!-- 输入区 -->
    <PanelCard title-key="speakerLayout.source.aes67" :truth="inputsTruth">
      <div class="inline-fields">
        <label>
          {{ t('speakerLayout.inputChannels') }}
          <el-input-number
            :model-value="edit.inputs.channels"
            :min="1"
            :max="256"
            size="small"
            :disabled="busy !== null"
            @update:model-value="(value: number | undefined) => setInputChannels(value)"
          />
        </label>
        <span class="aux-text">{{ t('speakerLayout.inputChannelsHint') }}</span>
      </div>
      <div v-for="(source, index) in sourceOrder" :key="source.kind" class="source-block">
        <header class="source-block__head">
          <strong>{{ source.kind === 'aes67' ? t('speakerLayout.source.aes67') : t('speakerLayout.source.device') }}</strong>
          <el-switch
            :model-value="source.enabled"
            :disabled="busy !== null"
            :active-text="t('speakerLayout.source.enabled')"
            @update:model-value="(value: boolean | string | number) => setSourceEnabled(index, value === true)"
          />
        </header>
        <div class="inline-fields">
          <label>
            {{ t('speakerLayout.source.gainDb') }}
            <el-input-number
              :model-value="source.gain_db"
              :step="0.5"
              :min="-60"
              :max="12"
              size="small"
              @update:model-value="(value: number | undefined) => setSourceGain(index, value)"
            />
          </label>
          <template v-if="source.kind === 'aes67'">
            <span class="aux-text">{{ t('speakerLayout.source.orderSource') }}: {{ orderSourceText }}</span>
            <el-button-group>
              <el-button
                v-for="preset in AES67_PRESETS"
                :key="preset.id"
                size="small"
                @click="applySourcePreset(index, preset.id)"
              >
                {{ preset.id }}
              </el-button>
            </el-button-group>
          </template>
        </div>
        <table class="compact-table" data-testid="speaker-input-roles">
          <thead>
            <tr>
              <th>{{ t('speakerLayout.source.channel', { channel: '' }) }}</th>
              <th>{{ t('speakerLayout.outputs.role') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(channel, channelIndex) in source.channels" :key="channel.channel">
              <td class="mono">{{ t('speakerLayout.source.channel', { channel: channel.channel }) }}</td>
              <td>
                <el-select
                  :model-value="channel.role"
                  size="small"
                  @update:model-value="(value: unknown) => setInputRole(index, channelIndex, value)"
                >
                  <el-option v-for="role in ['None', ...SPEAKER_ROLE_GROUPS.ground, ...SPEAKER_ROLE_GROUPS.low, ...SPEAKER_ROLE_GROUPS.height] " :key="role" :value="role" :label="roleLabel(role as SpeakerRole)" />
                </el-select>
              </td>
            </tr>
          </tbody>
        </table>
        <p class="aux-text">{{ t('speakerLayout.source.sumHint') }}</p>
      </div>
    </PanelCard>

    <PanelCard title-key="speakerLayout.deviceInputs.title" :truth="inputsTruth">
      <div v-if="deviceInputs === null || deviceInputs.available !== true" class="banner banner--warn" role="status">
        {{ deviceInputsText }}
      </div>
      <dl v-else class="summary-grid">
        <div><dt>{{ t('speakerLayout.deviceInputs.opticalMode') }}</dt><dd>{{ deviceInputs.optical_mode ?? t('common.dash') }}</dd></div>
        <div><dt>{{ t('speakerLayout.deviceInputs.sampleRate') }}</dt><dd>{{ deviceInputs.sample_rate ?? t('common.dash') }}</dd></div>
        <div><dt>{{ t('speakerLayout.deviceInputs.channels') }}</dt><dd>{{ deviceInputList.length }}</dd></div>
      </dl>
      <ul v-if="deviceInputList.length > 0" class="plain-list mono">
        <li v-for="channel in deviceInputList" :key="channel.index">
          {{ channel.index + 1 }} · {{ channel.label ?? t('common.dash') }} · {{ channel.kind }}
        </li>
      </ul>
    </PanelCard>

    <!-- 输出区 -->
    <PanelCard title-key="speakerLayout.outputs.title" :truth="profileTruth">
      <h3>{{ t('speakerLayout.outputs.step1') }}</h3>
      <div class="role-groups">
        <div v-for="groupKey in ['ground', 'low', 'height'] as const" :key="groupKey" class="role-group">
          <h4>{{ groupLabel(groupKey) }}</h4>
          <div class="role-grid">
            <div v-for="role in SPEAKER_ROLE_GROUPS[groupKey]" :key="role" class="role-chip">
              <span>{{ roleLabel(role) }}</span>
              <span class="role-chip__count">{{ countOf(role) }}</span>
              <el-button size="small" text :disabled="busy !== null" @click="addRole(role)">+</el-button>
              <el-button size="small" text :disabled="busy !== null || countOf(role) === 0" @click="removeRole(role)">−</el-button>
            </div>
          </div>
        </div>
      </div>
      <p class="aux-text">{{ t('speakerLayout.outputs.maxGround') }} / {{ t('speakerLayout.outputs.maxHeight') }}</p>

      <h3>{{ t('speakerLayout.outputs.step2') }}</h3>
      <div class="inline-fields">
        <span class="aux-text">{{ t('speakerLayout.outputs.preset') }}</span>
        <el-button-group>
          <el-button v-for="preset in LAYOUT_PRESETS" :key="preset.id" size="small" @click="applyPreset(preset.id)">
            {{ preset.id }}
          </el-button>
        </el-button-group>
      </div>
      <p class="aux-text">{{ t('speakerLayout.outputs.presetHint') }}</p>

      <table class="compact-table" data-testid="speaker-output-table">
        <thead>
          <tr>
            <th>{{ t('speakerLayout.outputs.dac') }}</th>
            <th>{{ t('speakerLayout.outputs.enabled') }}</th>
            <th>{{ t('speakerLayout.outputs.role') }}</th>
            <th>{{ t('speakerLayout.outputs.customLabel') }}</th>
            <th>{{ t('speakerLayout.outputs.gainDb') }}</th>
            <th>{{ t('speakerLayout.speakerTest.identify') }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="output in edit.outputs" :key="output.dac">
            <td>
              <el-select :model-value="output.dac" size="small" @update:model-value="(value: unknown) => setOutputDac(output, typeof value === 'number' ? value : undefined)">
                <el-option v-for="candidate in edit.outputs" :key="candidate.dac" :value="candidate.dac" :label="String(candidate.dac + 1)" />
              </el-select>
            </td>
            <td>
              <el-switch
                :model-value="output.enabled"
                size="small"
                :disabled="busy !== null"
                @update:model-value="(value: boolean | string | number) => setOutputEnabled(output, value === true)"
              />
            </td>
            <td>
              <el-select :model-value="output.role" size="small" @update:model-value="(value: unknown) => setOutputRole(output, value)">
                <el-option v-for="role in ['None', ...SPEAKER_ROLE_GROUPS.ground, ...SPEAKER_ROLE_GROUPS.low, ...SPEAKER_ROLE_GROUPS.height, 'Custom'] " :key="role" :value="role" :label="roleLabel(role as SpeakerRole)" />
              </el-select>
            </td>
            <td>
              <el-input
                v-if="output.role === 'Custom'"
                :model-value="output.custom_label ?? ''"
                size="small"
                @update:model-value="(value: string) => setOutputLabel(output, value)"
              />
              <span v-else class="aux-text">{{ t('common.dash') }}</span>
            </td>
            <td>
              <el-input-number
                :model-value="output.gain_db"
                size="small"
                :step="0.5"
                :min="-60"
                :max="12"
                @update:model-value="(value: number | undefined) => setOutputGain(output, value)"
              />
            </td>
            <td>
              <el-button size="small" :disabled="busy !== null" @click="onIdentify(output.dac)">
                {{ t('speakerLayout.speakerTest.identify') }}
              </el-button>
            </td>
          </tr>
        </tbody>
      </table>
      <p class="aux-text">{{ t('speakerLayout.outputs.unused') }}: {{ unused.join(', ') }}</p>
    </PanelCard>

    <!-- 统计 / 校验 / 下混 / fanout -->
    <PanelCard title-key="speakerLayout.validation.title" truth="actual">
      <p v-if="validation.errors.length === 0" class="aux-text">{{ t('speakerLayout.validation.ok') }}</p>
      <div v-if="validation.errors.length > 0" class="banner banner--danger" role="alert">
        <strong>{{ t('speakerLayout.validation.errors') }}</strong>
        <ul>
          <li v-for="issue in validation.errors" :key="(issue.code ?? '') + (issue.path ?? '')">{{ issue.message }}</li>
        </ul>
      </div>
      <div v-if="validation.warnings.length > 0" class="banner banner--warn" role="status">
        <strong>{{ t('speakerLayout.validation.warnings') }}</strong>
        <ul>
          <li v-for="issue in validation.warnings" :key="(issue.code ?? '') + (issue.path ?? '')">{{ issue.message }}</li>
        </ul>
      </div>
      <p class="aux-text">{{ t('speakerLayout.validation.structuralHint') }}</p>
      <h3>{{ t('speakerLayout.outputs.roleSummary') }}</h3>
      <ul class="plain-list">
        <li v-for="group in roles" :key="group.role">
          {{ t('speakerLayout.outputs.roleCount', { role: roleLabel(group.role), count: group.count }) }} — DAC {{ group.dacs.join(', ') }}
        </li>
      </ul>
    </PanelCard>

    <PanelCard title-key="speakerLayout.downmix.title" truth="actual">
      <p v-if="hints.missingCenter" data-testid="speaker-downmix-center">
        {{ t('speakerLayout.downmix.centerMissing', { gain: hints.centerGainDb }) }}
      </p>
      <p v-if="hints.missingLfe" data-testid="speaker-downmix-lfe">
        {{ t('speakerLayout.downmix.lfeMissing', { count: hints.lfeTargetCount, gain: hints.lfeGainDb }) }}
      </p>
      <div class="inline-fields">
        <label>
          {{ t('speakerLayout.downmix.centerGain') }}
          <el-input-number v-model="edit.downmix.center.gain_db" size="small" :step="0.5" :min="-60" :max="0" />
        </label>
        <label>
          {{ t('speakerLayout.downmix.lfeGain') }}
          <el-input-number v-model="edit.downmix.lfe.gain_db" size="small" :step="0.5" :min="-60" :max="0" />
        </label>
        <el-checkbox v-model="edit.downmix.lfe.include_heights">
          {{ t('speakerLayout.downmix.includeHeights') }}
        </el-checkbox>
      </div>
    </PanelCard>

    <PanelCard title-key="speakerLayout.fanout.title" :truth="profileTruth">
      <p v-if="fanout.length === 0" class="aux-text">{{ t('speakerLayout.fanout.empty') }}</p>
      <table v-else class="compact-table">
        <thead>
          <tr>
            <th>{{ t('speakerLayout.outputs.role') }}</th>
            <th>{{ t('speakerLayout.fanout.roleInputs') }}</th>
            <th>{{ t('speakerLayout.fanout.roleOutputs') }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="row in fanout" :key="row.role">
            <td>{{ roleLabel(row.role) }}</td>
            <td class="mono">{{ row.inputs.map((input) => input.source + ':' + input.channel).join(', ') }}</td>
            <td class="mono">{{ row.dacs.join(', ') }}</td>
          </tr>
        </tbody>
      </table>
    </PanelCard>

    <!-- 保存草稿 -->
    <PanelCard title-key="speakerLayout.draft.title" truth="actual">
      <div class="inline-fields">
        <el-button type="primary" :loading="busy === 'prepare'" :disabled="busy !== null" data-testid="speaker-prepare" @click="onPrepare">
          {{ t('speakerLayout.draft.prepare') }}
        </el-button>
        <span v-if="lastCommit !== null" class="aux-text">
          {{ t('speakerLayout.saved') }} · revision {{ lastCommit.revision ?? t('common.dash') }}
        </span>
      </div>
      <div v-if="draft !== null" class="draft-box" data-testid="speaker-draft">
        <dl class="summary-grid">
          <div><dt>{{ t('speakerLayout.draft.token') }}</dt><dd class="mono">{{ draft.token.slice(0, 8) }}…</dd></div>
          <div><dt>{{ t('speakerLayout.draft.expires') }}</dt><dd>{{ draft.expires_in_s ?? t('common.dash') }}</dd></div>
          <div><dt>{{ t('speakerLayout.draft.diffOutputs') }}</dt><dd>{{ diffText(draft.diff.outputs_changed) }}</dd></div>
          <div><dt>{{ t('speakerLayout.draft.diffInputs') }}</dt><dd>{{ diffText(draft.diff.inputs_changed) }}</dd></div>
          <div><dt>{{ t('speakerLayout.draft.diffDownmix') }}</dt><dd>{{ diffText(draft.diff.downmix_changed) }}</dd></div>
          <div><dt>{{ t('speakerLayout.draft.diffFanout') }}</dt><dd>{{ diffText(draft.diff.fanout_changed) }}</dd></div>
        </dl>
        <div class="inline-fields">
          <el-button type="primary" :loading="busy === 'commit'" :disabled="busy !== null" data-testid="speaker-commit" @click="onCommit">
            {{ t('speakerLayout.draft.commit') }}
          </el-button>
          <el-button :loading="busy === 'abort'" :disabled="busy !== null" data-testid="speaker-abort" @click="onAbort">
            {{ t('speakerLayout.draft.abort') }}
          </el-button>
        </div>
      </div>
    </PanelCard>

    <!-- 应用到方案 -->
    <PanelCard title-key="speakerLayout.apply.title" truth="actual">
      <p class="aux-text">{{ t('speakerLayout.apply.desc') }}</p>
      <div class="inline-fields">
        <el-button type="primary" :loading="busy === 'apply'" :disabled="busy !== null" data-testid="speaker-apply" @click="onApply">
          {{ t('speakerLayout.apply.button') }}
        </el-button>
        <el-button
          v-if="apply?.planDraft != null"
          :disabled="busy !== null"
          @click="onAbortApply"
        >
          {{ t('speakerLayout.apply.abortApply') }}
        </el-button>
      </div>
      <ol v-if="apply !== null" class="step-list">
        <li v-for="step in apply.steps" :key="step.step" :class="step.ok ? 'is-ok' : 'is-fail'">
          {{
            step.step === 'plan'
              ? t('speakerLayout.apply.stepPlan')
              : step.step === 'inspect'
                ? t('speakerLayout.apply.stepInspect')
                : step.step === 'prepare'
                  ? t('speakerLayout.apply.stepPrepare')
                  : t('speakerLayout.apply.stepCommit')
          }}
          <span class="mono">{{ step.ok ? 'OK' : step.detail }}</span>
        </li>
      </ol>
      <div v-if="apply?.ok !== true && apply?.error != null" class="banner banner--danger" role="alert">
        {{ t('speakerLayout.apply.failedAt', { step: apply.steps[apply.steps.length - 1]?.step ?? '', code: apply.error.code }) }}
      </div>
      <dl v-if="apply?.plan != null" class="summary-grid">
        <div><dt>{{ t('speakerLayout.apply.planPath') }}</dt><dd class="mono">{{ apply.plan.plan_path }}</dd></div>
        <div><dt>{{ t('speakerLayout.apply.laneCount') }}</dt><dd>{{ apply.plan.lane_count ?? t('common.dash') }}</dd></div>
      </dl>
      <div v-if="apply?.commit != null" class="banner banner--info" role="status">
        {{ t('speakerLayout.apply.success', { graph: apply.commit.graph_id ?? t('common.dash'), version: apply.commit.version ?? t('common.dash') }) }}
      </div>
    </PanelCard>

    <!-- 测试音设置 -->
    <PanelCard title-key="speakerLayout.speakerTest.confirmTitle" truth="actual">
      <div class="inline-fields">
        <label>
          {{ t('speakerLayout.speakerTest.duration') }}
          <el-input-number v-model="testMs" size="small" :step="500" :min="SPEAKER_TEST_UI_MIN_MS" :max="SPEAKER_TEST_UI_MAX_MS" />
        </label>
        <label>
          {{ t('speakerLayout.speakerTest.level') }}
          <el-select v-model="testDb" size="small">
            <el-option v-for="level in SPEAKER_TEST_LEVEL_OPTIONS" :key="level" :value="level" :label="String(level) + ' dBFS'" />
          </el-select>
        </label>
        <label>
          {{ t('speakerLayout.speakerTest.kind') }}
          <el-select v-model="testKind" size="small">
            <el-option value="noise" :label="t('speakerLayout.speakerTest.kindNoise')" />
            <el-option value="sine" :label="t('speakerLayout.speakerTest.kindSine')" />
          </el-select>
        </label>
      </div>
      <div v-if="testState !== null" class="banner banner--warn" role="status" data-testid="speaker-test-running">
        {{
          t('speakerLayout.speakerTest.running', {
            dac: testState.dac,
            seconds: (testRemaining / 1000).toFixed(1)
          })
        }}
        <el-button size="small" type="danger" :loading="busy === 'stop'" @click="onStopTest">
          {{ t('speakerLayout.speakerTest.stop') }}
        </el-button>
      </div>
      <p class="aux-text">{{ speakerTestGap }}</p>
      <p class="aux-text">{{ t('speakerLayout.speakerTest.mockHint') }}</p>
    </PanelCard>
  </div>
</template>

<style scoped>
.speaker-layout {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-3);
}

.summary-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: var(--khaen-space-2);
}

.summary-grid dt {
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

.summary-grid dd {
  margin: 0;
}

.source-block {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-2) 0;
  border-bottom: 1px solid var(--khaen-border);
}

.source-block__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--khaen-space-2);
}

.inline-fields {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.inline-fields label {
  display: inline-flex;
  align-items: center;
  gap: var(--khaen-space-1);
  color: var(--khaen-text-muted);
  font-size: var(--khaen-font-aux);
}

.compact-table {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--khaen-font-aux);
}

.compact-table th,
.compact-table td {
  padding: 4px 6px;
  border-bottom: 1px solid var(--khaen-border);
  text-align: left;
  vertical-align: middle;
}

.role-groups {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  gap: var(--khaen-space-2);
}

.role-grid {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.role-chip {
  display: inline-flex;
  align-items: center;
  gap: var(--khaen-space-1);
}

.role-chip__count {
  color: var(--khaen-text-muted);
  min-width: 18px;
  text-align: right;
}

.plain-list {
  margin: 0;
  padding-left: 18px;
}

.draft-box {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-2);
  border: 1px dashed var(--khaen-border-strong);
  border-radius: var(--khaen-radius-panel);
}

.step-list {
  margin: 0;
  padding-left: 18px;
}

.step-list .is-ok {
  color: var(--khaen-text);
}

.step-list .is-fail {
  color: var(--khaen-danger, #d9534f);
}
</style>
