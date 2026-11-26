<script setup lang="ts">
/**
 * Dirac 式校准向导（PLAN §10.2 8 步；任务 T37）。
 *
 * 数据纪律：
 * - 每步能否继续**只认后端** GET /api/calibration/wizard/state 的 steps[].status（passed/blocked/not_available）；
 *   缺失 gate 一律 fail closed（下一步禁用），前端不自行判过；
 * - 第 2 步（/api/channels 属 M4）、第 4 步多点/ART（P3）、第 5 步实时测量（T42/P2）是**声明式能力缺口**：
 *   灰态 + NOT_AVAILABLE + 原因 + 任务号，不产生任何数据，也不阻塞向导（不伪造映射/采集/进度）；
 * - 进度全部来自真实 job 状态轮询（stores/calibration.ts），无定时器假进度；
 * - 刷新恢复：配置/上传草稿/job 状态来自后端（?step= 记住当前步）；失败/中断保留已完成步骤；
 * - mock/fixture 引擎的数据由 VerdictBadge/JobResultPanel 标「示意」，不冒充实测。
 */
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { ElMessage, ElMessageBox } from 'element-plus'
import { Back, Refresh } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import TruthTag from '@/components/TruthTag.vue'
import WizardStepper, { type WizardStepperItem } from '@/components/calibration/WizardStepper.vue'
import GateCard from '@/components/calibration/GateCard.vue'
import UploadCard from '@/components/calibration/UploadCard.vue'
import JobTimeline from '@/components/calibration/JobTimeline.vue'
import JobResultPanel from '@/components/calibration/JobResultPanel.vue'
import ApplyPanel, { type BeforePlan } from '@/components/calibration/ApplyPanel.vue'
import { useStatusPolling } from '@/composables/useStatusPolling'
import {
  DEFAULT_CENTERS_HZ,
  WIZARD_STEP_COUNT,
  WIZARD_STEP_KEYS,
  blockReason,
  canAdvance,
  gateForStep,
  isJobActive,
  isJobTerminal,
  maxUnlockedStep,
  resultTruth,
  stepDisplayState,
  type WizardConfig,
  type WizardOrientation,
  type WizardTargetMode
} from '@/api/calibration'
import {
  calibrationState,
  cancelCalibrationJob,
  createCalibrationJob,
  loadCalibrationJob,
  loadCalibrationWizard,
  loadCalibrationWizardState,
  saveCalibrationWizard,
  startCalibrationJobPolling,
  stopCalibrationJobPolling,
  pauseCalibrationJobPolling,
  resumeCalibrationJobPolling,
  uploadCalibrationMeasurement,
  uploadCalibrationMicCal
} from '@/stores/calibration'
import { reasonForPath, runStateLabelKey } from '@/api/status'
import {
  MAX_WAV_BYTES,
  checkCalibrationWavFormat,
  checkWavPairAlignment,
  parseWav,
  type WavInfo
} from '@/utils/wav'
import { formatDecimal, formatTimestamp, shortenId, toRateUnit } from '@/utils/format'
import type { TruthKind } from '@/types/ui'

/** 麦克风校准文件上限（UMIK txt 远小于此；daemon 上传端点上限 64 MiB）。 */
const MIC_CAL_MAX_BYTES = 1024 * 1024

const { t, locale } = useI18n({ useScope: 'global' })
const route = useRoute()
const router = useRouter()
const { state: statusState, refresh: refreshStatus } = useStatusPolling()

const state = calibrationState
const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const phase = computed(() => state.wizardPhase)
const wizard = computed(() => state.wizard)
const wizardState = computed(() => state.wizardState)
const job = computed(() => state.job)
const currentStep = ref(1)

const stepKeys = WIZARD_STEP_KEYS
const stepLabels = computed(() => stepKeys.map((key, index) => ({ step: index + 1, key, label: t('calibration.step.names.' + key) })))
const maxUnlocked = computed(() => maxUnlockedStep(wizardState.value))
const gate = computed(() => gateForStep(wizardState.value, currentStep.value))
const gateStepLabel = computed(() => t('calibration.step.names.' + stepKeys[currentStep.value - 1]))
const stepperItems = computed<WizardStepperItem[]>(() =>
  stepLabels.value.map((item) => ({
    step: item.step,
    key: item.key,
    label: item.label,
    state: stepDisplayState(wizardState.value, item.step, currentStep.value)
  }))
)
const nextBlockedReason = computed(() => blockReason(wizardState.value, currentStep.value))
const canGoNext = computed(() => currentStep.value >= WIZARD_STEP_COUNT || canAdvance(wizardState.value, currentStep.value))
const nextLabel = computed(() => {
  if (currentStep.value >= WIZARD_STEP_COUNT) {
    return t('calibration.wizard.finish')
  }
  return gate.value?.status === 'not_available' ? t('calibration.wizard.nextSkip') : t('calibration.wizard.next')
})
const nextTitle = computed(() => (canGoNext.value ? '' : (nextBlockedReason.value ?? t('calibration.gate.failClosed'))))
/** 未接入原因一律用 daemon 上报的 reason/detail（不硬编码里程碑号）。 */
const wizardUnavailableReason = computed(
  () => state.wizardError?.detail ?? t('calibrationEdit.list.notAvailableNoReason')
)

/* ------------------------------------------------------------------ 状态数据 */

const statusData = computed(() => statusState.data)
const statusError = computed(() => statusState.error)
const planSha = computed(() => statusData.value?.plan.sha256 ?? null)
const currentPlanVersion = computed(() => statusData.value?.plan.version ?? null)
const currentGraphId = computed(() => statusData.value?.plan.graph_id ?? null)
const beforePlan = computed<BeforePlan | null>(() => {
  const snapshot = wizardState.value?.snapshot ?? null
  if (snapshot !== null) {
    return { version: snapshot.version, graph_id: snapshot.graph_id, sha256: snapshot.sha256 }
  }
  if (currentPlanVersion.value === null && currentGraphId.value === null) {
    return null
  }
  return { version: currentPlanVersion.value, graph_id: currentGraphId.value, sha256: planSha.value }
})
const measurementIds = computed(() => ({
  baseline: wizardState.value?.measurement.baseline_upload_id ?? state.measurementDraft.baseline_upload_id,
  applied: wizardState.value?.measurement.applied_upload_id ?? state.measurementDraft.applied_upload_id
}))
const measurementShas = computed(() => ({
  baseline: wizardState.value?.measurement.baseline_sha256 ?? state.uploads.baseline?.sha256 ?? null,
  applied: wizardState.value?.measurement.applied_sha256 ?? state.uploads.applied?.sha256 ?? null
}))
const singlePoint = computed(() => wizard.value?.config.single_point ?? null)
const settingsSaved = computed(() => wizard.value?.saved === true)
const jobTruth = computed(() => resultTruth(job.value?.result ?? null, job.value?.engine ?? null))

function rateText(hz: number | null): string | null {
  const unit = toRateUnit(hz)
  if (unit === null) {
    return null
  }
  const value = formatDecimal(unit.value, 2, localeId.value)
  return value === null ? null : value + ' ' + t(unit.unitKey)
}

function msText(ms: number | null): string | null {
  if (ms === null) {
    return null
  }
  return (formatDecimal(ms, 0, localeId.value) ?? String(ms)) + ' ' + t('unit.ms')
}

function boolText(value: boolean | null): string | null {
  if (value === null) {
    return null
  }
  return t(value ? 'common.yes' : 'common.no')
}

function timeText(iso: string | null): string {
  return formatTimestamp(iso, localeId.value) ?? t('common.dash')
}

function stateText(): string | null {
  const current = statusData.value?.state ?? null
  if (current === null) {
    return null
  }
  const key = runStateLabelKey(current)
  return key !== null ? t(key) : current.value
}

interface FactRow {
  label: string
  value: string | null
  reason: string | null
  truth: TruthKind
}

function fact(label: string, value: string | null, reason: string | null): FactRow {
  return { label, value, reason, truth: value === null ? 'notWired' : 'actual' }
}

const deviceFacts = computed<FactRow[]>(() => {
  const current = statusData.value
  if (current === null) {
    return []
  }
  return [
    fact(t('calibration.wizard.device.device'), current.device.id, reasonForPath(current, 'device.id')),
    fact(t('calibration.wizard.device.deviceState'), current.device.state, reasonForPath(current, 'device.state')),
    fact(t('calibration.wizard.device.deviceCard'), current.device.card, reasonForPath(current, 'device.card')),
    fact(t('calibration.wizard.device.deviceRate'), rateText(current.format.device_rate), reasonForPath(current, 'format.device_rate')),
    fact(t('calibration.wizard.device.rateAsserted'), boolText(current.format.rate_asserted), reasonForPath(current, 'format.rate_asserted')),
    fact(t('calibration.wizard.device.hostLink'), boolText(current.host_link.connected), reasonForPath(current, 'host_link.connected')),
    fact(t('calibration.wizard.device.hostLinkAge'), msText(current.host_link.status_age_ms), reasonForPath(current, 'host_link.status_age_ms')),
    fact(t('calibration.wizard.device.runState'), stateText(), reasonForPath(current, 'state.value')),
    fact(t('calibration.wizard.device.channels'), current.format.channels === null ? null : String(current.format.channels), reasonForPath(current, 'format.channels')),
    fact(t('calibration.wizard.device.encoding'), current.format.encoding, reasonForPath(current, 'format.encoding'))
  ]
})

const channelFacts = computed<FactRow[]>(() => {
  const current = statusData.value
  if (current === null) {
    return []
  }
  const aes67 = current.format.aes67
  return [
    fact(t('calibration.wizard.channels.planRate'), rateText(current.format.plan_rate), reasonForPath(current, 'format.plan_rate')),
    fact(t('calibration.wizard.channels.deviceRate'), rateText(current.format.device_rate), reasonForPath(current, 'format.device_rate')),
    fact(t('calibration.wizard.channels.channels'), current.format.channels === null ? null : String(current.format.channels), reasonForPath(current, 'format.channels')),
    fact(t('calibration.wizard.channels.encoding'), current.format.encoding, reasonForPath(current, 'format.encoding')),
    fact(t('calibration.wizard.channels.aes67Channels'), aes67.channels === null ? null : String(aes67.channels), reasonForPath(current, 'format.aes67.channels')),
    fact(t('calibration.wizard.channels.aes67Map'), aes67.map === null ? null : aes67.map.join(','), reasonForPath(current, 'format.aes67.map')),
    fact(t('calibration.wizard.channels.aes67Group'), aes67.group, reasonForPath(current, 'format.aes67.group')),
    fact(t('calibration.wizard.channels.aes67Port'), aes67.port === null ? null : String(aes67.port), reasonForPath(current, 'format.aes67.port'))
  ]
})

/* -------------------------------------------------------------- 第 3 步表单 */

interface SettingsForm {
  micCalSha256: string | null
  micCalFilename: string | null
  /** '' = 未选择（Element Plus 的 radio-group 模型不接受 null）。 */
  orientation: WizardOrientation | ''
  targetMode: WizardTargetMode | ''
  centersText: string
  toleranceDb: number
  minBands: number
}

const form = reactive<SettingsForm>({
  micCalSha256: null,
  micCalFilename: null,
  orientation: '',
  targetMode: '',
  centersText: DEFAULT_CENTERS_HZ.join(', '),
  toleranceDb: 2,
  minBands: 3
})
const formDirty = ref(false)
const formError = ref<string | null>(null)
const micError = ref<string | null>(null)

function resetForm(config: WizardConfig): void {
  form.micCalSha256 = config.mic_cal_sha256
  form.micCalFilename = config.mic_cal_filename
  form.orientation = config.orientation ?? ''
  form.targetMode = config.target_mode ?? ''
  form.centersText = config.single_point.centers_hz.map((value) => String(value)).join(', ')
  form.toleranceDb = config.single_point.tolerance_db
  form.minBands = config.single_point.min_bands
  formDirty.value = false
  formError.value = null
}

watch(
  () => wizard.value?.config,
  (config) => {
    if (config && !formDirty.value) {
      resetForm(config)
    }
  },
  { immediate: true, deep: true }
)

function markDirty(): void {
  formDirty.value = true
}

const savedShaText = computed(() => shortenId(form.micCalSha256, 16, 12) ?? t('common.dash'))
const micUploadResult = computed(() => state.uploads.micCal)

async function onMicFile(file: File): Promise<void> {
  micError.value = null
  if (file.size > MIC_CAL_MAX_BYTES) {
    micError.value = t('calibration.wizard.mic.fileTooLarge')
    return
  }
  const buffer = await file.arrayBuffer()
  const result = await uploadCalibrationMicCal({
    bytes: buffer,
    filename: file.name,
    contentType: 'application/octet-stream'
  })
  if (result.ok === true) {
    form.micCalSha256 = result.value.sha256
    form.micCalFilename = file.name
    markDirty()
    ElMessage.success(t('calibration.wizard.toast.uploaded'))
  } else {
    micError.value = t(result.error.i18nKey) + (result.error.detail ? ' — ' + result.error.detail : '')
  }
}

function parseCenters(text: string): number[] | null {
  const parts = text
    .split(/[,\uFF0C\s]+/)
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
  if (parts.length === 0) {
    return null
  }
  const values = parts.map((part) => Number(part))
  if (values.some((value) => !Number.isFinite(value) || value <= 0 || value > 24000)) {
    return null
  }
  return [...new Set(values)].sort((a, b) => a - b)
}

function validateForm(): boolean {
  if (form.micCalSha256 === null) {
    formError.value = t('calibration.wizard.mic.validation.micRequired')
    return false
  }
  if (form.orientation === '') {
    formError.value = t('calibration.wizard.mic.validation.orientationRequired')
    return false
  }
  if (form.targetMode === '') {
    formError.value = t('calibration.wizard.mic.validation.targetRequired')
    return false
  }
  const centers = parseCenters(form.centersText)
  if (centers === null) {
    formError.value = t('calibration.wizard.mic.validation.centersRange')
    return false
  }
  if (centers.length > 64) {
    formError.value = t('calibration.wizard.mic.validation.centersTooMany')
    return false
  }
  if (!Number.isFinite(form.toleranceDb) || form.toleranceDb < 0.1 || form.toleranceDb > 24) {
    formError.value = t('calibration.wizard.mic.validation.toleranceRange')
    return false
  }
  if (!Number.isInteger(form.minBands) || form.minBands < 1 || form.minBands > 64) {
    formError.value = t('calibration.wizard.mic.validation.minBandsRange')
    return false
  }
  formError.value = null
  return true
}

async function saveSettings(): Promise<void> {
  if (!validateForm()) {
    return
  }
  const centers = parseCenters(form.centersText) ?? []
  const config: WizardConfig = {
    mic_cal_sha256: form.micCalSha256,
    mic_cal_filename: form.micCalFilename,
    orientation: form.orientation === '' ? null : form.orientation,
    target_mode: form.targetMode === '' ? null : form.targetMode,
    single_point: { centers_hz: centers, tolerance_db: form.toleranceDb, min_bands: form.minBands }
  }
  const result = await saveCalibrationWizard(config, {
    baseline_upload_id: measurementIds.value.baseline,
    applied_upload_id: measurementIds.value.applied,
    plan_sha: planSha.value
  })
  if (result.ok === true) {
    formDirty.value = false
    ElMessage.success(t('calibration.wizard.toast.saved'))
  } else {
    formError.value = t(result.error.i18nKey) + (result.error.detail ? ' — ' + result.error.detail : '')
  }
}

/* -------------------------------------------------------------- 第 5 步测量 */

const baselineInfo = ref<WavInfo | null>(null)
const appliedInfo = ref<WavInfo | null>(null)
const baselineError = ref<string | null>(null)
const appliedError = ref<string | null>(null)

function wavErrorText(code: string): string {
  return t('calibration.wizard.measure.wavErrors.' + code)
}

function setMeasureError(role: 'baseline' | 'applied', message: string | null): void {
  if (role === 'baseline') {
    baselineError.value = message
  } else {
    appliedError.value = message
  }
}

function wavFormatText(info: WavInfo): string {
  return t('calibration.wizard.measure.formatValue', {
    rate: rateText(info.format.sampleRate) ?? String(info.format.sampleRate),
    bits: info.format.bitsPerSample,
    channels: info.format.channels
  })
}

function wavDurationText(info: WavInfo): string {
  return (formatDecimal(info.durationS, 3, localeId.value) ?? String(info.durationS)) + ' ' + t('unit.s')
}

function measurementDetails(info: WavInfo | null): Array<{ label: string; value: string }> {
  if (info === null) {
    return []
  }
  return [
    { label: t('calibration.wizard.measure.format'), value: wavFormatText(info) },
    { label: t('calibration.wizard.measure.duration'), value: wavDurationText(info) }
  ]
}

async function onMeasurementFile(role: 'baseline' | 'applied', file: File): Promise<void> {
  setMeasureError(role, null)
  if (file.size > MAX_WAV_BYTES) {
    setMeasureError(role, wavErrorText('file_too_large'))
    return
  }
  const buffer = await file.arrayBuffer()
  const parsed = parseWav(new Uint8Array(buffer))
  if (parsed.ok !== true) {
    setMeasureError(role, t('calibration.wizard.measure.parseError', { message: wavErrorText(parsed.code) }))
    return
  }
  const formatCheck = checkCalibrationWavFormat(parsed.info)
  if (formatCheck.ok !== true) {
    setMeasureError(
      role,
      t('calibration.wizard.measure.formatError', { message: wavErrorText(formatCheck.code ?? 'bad_data_length') })
    )
    return
  }
  const other = role === 'baseline' ? appliedInfo.value : baselineInfo.value
  if (other !== null) {
    const pair = checkWavPairAlignment(parsed.info, other)
    if (pair.ok !== true) {
      setMeasureError(role, t('calibration.wizard.measure.pairError', { message: wavErrorText(pair.code ?? 'format_mismatch') }))
      return
    }
  }
  if (role === 'baseline') {
    baselineInfo.value = parsed.info
  } else {
    appliedInfo.value = parsed.info
  }
  const result = await uploadCalibrationMeasurement({
    bytes: buffer,
    filename: file.name,
    contentType: 'audio/wav',
    role
  })
  if (result.ok === true) {
    ElMessage.success(t('calibration.wizard.toast.uploaded'))
  } else {
    setMeasureError(role, t(result.error.i18nKey) + (result.error.detail ? ' — ' + result.error.detail : ''))
  }
}

/* -------------------------------------------------------------- 第 6 步分析 */

const canCreateJob = computed(
  () =>
    planSha.value !== null &&
    measurementIds.value.baseline !== null &&
    measurementIds.value.applied !== null &&
    singlePoint.value !== null &&
    state.busy.createJob !== true &&
    !isJobActive(job.value?.status)
)

async function startAnalysis(): Promise<void> {
  if (!canCreateJob.value || planSha.value === null || singlePoint.value === null) {
    return
  }
  const baseline = measurementIds.value.baseline
  const applied = measurementIds.value.applied
  if (baseline === null || applied === null) {
    return
  }
  const result = await createCalibrationJob({
    plan_sha: planSha.value,
    baseline_upload_id: baseline,
    applied_upload_id: applied,
    centers_hz: singlePoint.value.centers_hz,
    tolerance_db: singlePoint.value.tolerance_db,
    min_bands: singlePoint.value.min_bands
  })
  if (result.ok === true) {
    ElMessage.success(t('calibration.wizard.toast.jobCreated'))
  }
}

async function onCancelJob(): Promise<void> {
  const current = job.value
  if (current === null) {
    return
  }
  try {
    await ElMessageBox.confirm(
      t('calibration.wizard.analysis.cancelConfirm.message', { id: current.id }),
      t('calibration.wizard.analysis.cancelConfirm.title'),
      {
        confirmButtonText: t('calibration.wizard.analysis.cancel'),
        cancelButtonText: t('common.cancel'),
        type: 'warning'
      }
    )
  } catch {
    return
  }
  const result = await cancelCalibrationJob(current.id)
  if (result.ok === true) {
    ElMessage.success(t('calibration.wizard.toast.jobCancelled'))
  }
}

/* ------------------------------------------------------------- 第 8 步应用 */

async function onApplyChanged(): Promise<void> {
  await refreshStatus()
  if (job.value !== null) {
    await loadCalibrationJob(job.value.id)
  }
  await loadCalibrationWizardState()
}

/* ------------------------------------------------------------------ 导航 */

function clampStep(step: number): number {
  return Math.min(Math.max(step, 1), Math.max(maxUnlocked.value, 1))
}

function setStep(step: number, updateQuery = true): void {
  currentStep.value = clampStep(step)
  if (updateQuery) {
    void router.replace({ query: { ...route.query, step: String(currentStep.value) } })
  }
}

function applyInitialStep(): void {
  const raw = Number(route.query.step)
  const fromQuery = Number.isInteger(raw) && raw >= 1 && raw <= WIZARD_STEP_COUNT ? raw : 0
  const fallback = wizardState.value?.current_step ?? 1
  setStep(fromQuery > 0 ? fromQuery : fallback, false)
}

function onSelectStep(step: number): void {
  if (step > maxUnlocked.value && step !== currentStep.value) {
    return
  }
  setStep(step)
}

function goNext(): void {
  if (currentStep.value >= WIZARD_STEP_COUNT) {
    void router.push({ name: 'calibration' })
    return
  }
  if (!canGoNext.value) {
    return
  }
  setStep(currentStep.value + 1)
}

function goPrev(): void {
  if (currentStep.value <= 1) {
    return
  }
  setStep(currentStep.value - 1)
}

async function cancelWizard(): Promise<void> {
  try {
    await ElMessageBox.confirm(
      t('calibration.wizard.confirmCancel.message'),
      t('calibration.wizard.confirmCancel.title'),
      {
        confirmButtonText: t('calibration.wizard.confirmCancel.ok'),
        cancelButtonText: t('common.cancel'),
        type: 'info'
      }
    )
  } catch {
    return
  }
  void router.push({ name: 'calibration' })
}

function backToList(): void {
  void router.push({ name: 'calibration' })
}

/**
 * 刷新恢复：wizard 配置 + gate state 来自后端；最近 job 也要重新取回（否则刷新后第 6/7/8 步
 * 会误以为"尚未创建 job"）。active job 继续轮询；已完成/失败的 job 只加载一次详情。
 */
async function refreshAll(): Promise<void> {
  await Promise.all([loadCalibrationWizard(), refreshStatus()])
  const activeJobId = state.wizardState?.active_job_id ?? null
  if (activeJobId !== null) {
    await loadCalibrationJob(activeJobId)
    startCalibrationJobPolling(activeJobId)
    return
  }
  const lastJobId = state.wizardState?.last_job_id ?? null
  if (lastJobId !== null && (state.job === null || state.job.id !== lastJobId)) {
    await loadCalibrationJob(lastJobId)
  }
}

function onVisibilityChange(): void {
  if (document.visibilityState === 'hidden') {
    pauseCalibrationJobPolling()
  } else {
    resumeCalibrationJobPolling()
  }
}

/* -------------------------------------------------------------- gate 事实 */

const gateFacts = computed<Array<{ label: string; value: string }>>(() => {
  const facts: Array<{ label: string; value: string }> = []
  if (currentStep.value === 1) {
    facts.push({ label: t('calibration.wizard.device.deviceState'), value: statusData.value?.device.state ?? t('common.dash') })
    facts.push({ label: t('calibration.wizard.device.deviceRate'), value: rateText(statusData.value?.format.device_rate ?? null) ?? t('common.dash') })
    facts.push({ label: t('calibration.wizard.device.hostLink'), value: boolText(statusData.value?.host_link.connected ?? null) ?? t('common.dash') })
  } else if (currentStep.value === 2) {
    facts.push({ label: t('calibration.wizard.channels.channels'), value: statusData.value?.format.channels === null || statusData.value?.format.channels === undefined ? t('common.dash') : String(statusData.value?.format.channels) })
    facts.push({ label: t('calibration.wizard.channels.task'), value: 'M4' })
  } else if (currentStep.value === 3) {
    facts.push({ label: t('calibration.wizard.mic.savedSha'), value: form.micCalSha256 === null ? t('common.dash') : shortenId(form.micCalSha256, 12, 8) ?? t('common.dash') })
    facts.push({ label: t('calibration.wizard.mic.orientation'), value: form.orientation === '' ? t('common.dash') : t('calibration.wizard.mic.orientation' + (form.orientation === '0deg' ? '0' : '90')) })
  } else if (currentStep.value === 4) {
    facts.push({ label: t('calibration.wizard.positions.single'), value: gate.value?.status === 'passed' ? t('calibration.wizard.positions.selected') : t('calibration.wizard.positions.notSelected') })
  } else if (currentStep.value === 5) {
    facts.push({ label: t('calibration.wizard.measure.baseline'), value: shortenId(measurementShas.value.baseline, 12, 8) ?? t('common.dash') })
    facts.push({ label: t('calibration.wizard.measure.applied'), value: shortenId(measurementShas.value.applied, 12, 8) ?? t('common.dash') })
  } else if (currentStep.value === 6) {
    facts.push({ label: t('calibration.wizard.analysis.jobId'), value: job.value === null ? t('common.dash') : shortenId(job.value.id, 12, 6) ?? t('common.dash') })
    facts.push({ label: t('calibration.job.status'), value: job.value === null ? t('common.dash') : t('calibration.jobStatus.' + job.value.status) })
  } else if (currentStep.value === 7) {
    facts.push({ label: t('calibration.design.residual'), value: job.value?.result?.design?.residual ?? t('common.dash') })
    facts.push({ label: t('calibration.design.planSha'), value: shortenId(job.value?.result?.plan?.sha256 ?? null, 12, 8) ?? t('common.dash') })
  } else {
    facts.push({ label: t('calibration.review.applyState'), value: job.value?.apply_state ? t('calibration.applyState.' + job.value.apply_state) : t('common.dash') })
    const beforeVersion = beforePlan.value === null ? null : beforePlan.value.version
    facts.push({ label: t('calibration.review.version'), value: beforeVersion === null ? t('common.dash') : 'v' + String(beforeVersion) })
  }
  return facts
})

/* --------------------------------------------------------------- 生命周期 */

onMounted(async () => {
  document.addEventListener('visibilitychange', onVisibilityChange)
  await refreshAll()
  applyInitialStep()
})

onBeforeUnmount(() => {
  document.removeEventListener('visibilitychange', onVisibilityChange)
  stopCalibrationJobPolling()
})

watch(
  () => wizardState.value?.current_step,
  () => {
    if (wizardState.value === null) {
      return
    }
    const target = clampStep(currentStep.value)
    if (target !== currentStep.value) {
      setStep(target)
    }
  }
)

watch(
  () => job.value?.status,
  (status) => {
    if (status !== undefined && isJobTerminal(status)) {
      void refreshStatus()
    }
  }
)

watch(
  () => state.wizardState?.active_job_id,
  (jobId) => {
    if (jobId) {
      startCalibrationJobPolling(jobId)
    }
  }
)

watch(
  () => state.wizardState?.last_job_id,
  (jobId) => {
    if (jobId && (state.job === null || state.job.id !== jobId)) {
      void loadCalibrationJob(jobId)
    }
  }
)
</script>

<template>
  <div class="wizard-page">
    <PageHeader title-key="calibration.wizard.pageTitle" subtitle-key="calibration.wizard.subtitle">
      <template #actions>
        <el-button plain @click="backToList">
          <el-icon><Back /></el-icon>
          <span>{{ t('calibration.wizard.backToList') }}</span>
        </el-button>
        <el-button plain :loading="state.busy.wizard" :disabled="state.busy.wizard" @click="refreshAll">
          <el-icon v-if="!state.busy.wizard"><Refresh /></el-icon>
          <span>{{ t('common.refresh') }}</span>
        </el-button>
      </template>
    </PageHeader>

    <div v-if="phase === 'loading'" class="wizard-loading" aria-busy="true">
      <el-skeleton :rows="6" animated />
    </div>

    <div v-else-if="phase === 'unavailable'" class="banner banner--warn" role="status">
      <div class="wizard-notice">
        <strong>NOT_AVAILABLE · {{ t('calibration.wizard.unavailable.title') }}</strong>
        <span>{{ wizardUnavailableReason }}</span>
        <TruthTag kind="notWired" />
      </div>
      <el-button size="small" @click="refreshAll">{{ t('common.retry') }}</el-button>
    </div>

    <div v-else-if="phase === 'error'" class="banner banner--danger" role="alert">
      <div class="wizard-notice">
        <strong>{{ t('calibration.wizard.error.title') }}</strong>
        <span v-if="state.wizardError">{{ t(state.wizardError.i18nKey) }}</span>
        <span v-if="state.wizardError?.detail" class="aux-text">{{ state.wizardError.detail }}</span>
      </div>
      <el-button size="small" @click="refreshAll">{{ t('common.retry') }}</el-button>
    </div>

    <template v-else>
      <div class="wizard-stepper-bar">
        <WizardStepper :steps="stepperItems" :current="currentStep" @select="onSelectStep" />
      </div>

      <div class="wizard-body">
        <div class="wizard-main">
          <!-- 第 1 步：设备检查 -->
          <section v-if="currentStep === 1" class="step-panel">
            <h2 class="step-panel__title">{{ t('calibration.step.names.device_check') }}</h2>
            <p class="step-panel__desc">{{ t('calibration.wizard.device.desc') }}</p>
            <div v-if="statusData === null" class="banner banner--info" role="status">
              {{ t('calibration.wizard.device.noStatus') }}
            </div>
            <dl v-else class="fact-list">
              <div v-for="row in deviceFacts" :key="row.label" class="fact-list__row">
                <dt class="fact-list__label">{{ row.label }}</dt>
                <dd class="fact-list__value">
                  <span class="mono">{{ row.value ?? t('common.dash') }}</span>
                  <TruthTag :kind="row.truth" :reason="row.reason" />
                </dd>
              </div>
            </dl>
            <div v-if="statusError" class="banner banner--warn" role="status">
              {{ t('calibration.wizard.device.statusError', { message: t(statusError.i18nKey) }) }}
            </div>
          </section>

          <!-- 第 2 步：布局与声道（M4 未接：只读 + NOT_AVAILABLE） -->
          <section v-else-if="currentStep === 2" class="step-panel">
            <h2 class="step-panel__title">{{ t('calibration.step.names.layout_channels') }}</h2>
            <p class="step-panel__desc">{{ t('calibration.wizard.channels.desc') }}</p>
            <div class="banner banner--warn" role="status">
              <div class="wizard-notice">
                <strong>NOT_AVAILABLE</strong>
                <span>{{ gate?.reason ?? t('calibration.wizard.channels.notAvailable') }}</span>
                <span class="aux-text">{{ t('calibration.gate.task', { task: 'M4' }) }}</span>
              </div>
              <TruthTag kind="notWired" />
            </div>
            <dl v-if="channelFacts.length > 0" class="fact-list">
              <div v-for="row in channelFacts" :key="row.label" class="fact-list__row">
                <dt class="fact-list__label">{{ row.label }}</dt>
                <dd class="fact-list__value">
                  <span class="mono">{{ row.value ?? t('common.dash') }}</span>
                  <TruthTag :kind="row.truth" :reason="row.reason" />
                </dd>
              </div>
            </dl>
            <p v-else class="aux-text">{{ t('calibration.wizard.device.noStatus') }}</p>
            <p class="aux-text">{{ t('calibration.wizard.channels.noMapping') }}</p>
            <p class="aux-text">{{ t('calibration.wizard.channels.readOnly') }}</p>
          </section>

          <!-- 第 3 步：麦克风与校准设置 -->
          <section v-else-if="currentStep === 3" class="step-panel">
            <h2 class="step-panel__title">{{ t('calibration.step.names.mic_settings') }}</h2>
            <p class="step-panel__desc">{{ t('calibration.wizard.mic.desc') }}</p>

            <UploadCard
              :label="t('calibration.wizard.mic.file')"
              :hint="t('calibration.wizard.mic.fileHint')"
              accept=".txt,.json,.cal"
              :action-label="t('calibration.wizard.mic.fileSelect')"
              :busy="state.busy.upload"
              :result="micUploadResult"
              :error-text="micError"
              @select="onMicFile"
            />
            <dl class="fact-list fact-list--compact">
              <div class="fact-list__row">
                <dt class="fact-list__label">{{ t('calibration.wizard.mic.savedSha') }}</dt>
                <dd class="fact-list__value mono">{{ savedShaText }}</dd>
              </div>
              <div class="fact-list__row">
                <dt class="fact-list__label">{{ t('calibration.wizard.mic.filename') }}</dt>
                <dd class="fact-list__value mono">{{ form.micCalFilename ?? t('common.dash') }}</dd>
              </div>
            </dl>

            <div class="form-grid">
              <div class="form-field">
                <span class="form-field__label">{{ t('calibration.wizard.mic.orientation') }}</span>
                <el-radio-group v-model="form.orientation" @change="markDirty">
                  <el-radio value="0deg">{{ t('calibration.wizard.mic.orientation0') }}</el-radio>
                  <el-radio value="90deg">{{ t('calibration.wizard.mic.orientation90') }}</el-radio>
                </el-radio-group>
              </div>
              <div class="form-field">
                <span class="form-field__label">{{ t('calibration.wizard.mic.targetMode') }}</span>
                <el-radio-group v-model="form.targetMode" @change="markDirty">
                  <el-radio value="flat">{{ t('calibration.wizard.mic.targetFlat') }}</el-radio>
                  <el-radio value="tilt10">{{ t('calibration.wizard.mic.targetTilt10') }}</el-radio>
                </el-radio-group>
              </div>
              <div class="form-field form-field--wide">
                <label class="form-field__label" for="wizard-centers">{{ t('calibration.wizard.mic.centers') }}</label>
                <el-input id="wizard-centers" v-model="form.centersText" @input="markDirty" />
                <span class="form-field__hint">{{ t('calibration.wizard.mic.centersHint') }}</span>
              </div>
              <div class="form-field">
                <span class="form-field__label">{{ t('calibration.wizard.mic.tolerance') }}</span>
                <el-input-number v-model="form.toleranceDb" :min="0.1" :max="24" :step="0.1" :precision="2" @change="markDirty" />
              </div>
              <div class="form-field">
                <span class="form-field__label">{{ t('calibration.wizard.mic.minBands') }}</span>
                <el-input-number v-model="form.minBands" :min="1" :max="64" :step="1" :precision="0" @change="markDirty" />
              </div>
            </div>

            <p v-if="formError" class="form-error" role="alert">{{ formError }}</p>
            <div class="step-actions">
              <el-button type="primary" :loading="state.busy.saveWizard" :disabled="state.busy.saveWizard" @click="saveSettings">
                {{ state.busy.saveWizard ? t('calibration.wizard.mic.saving') : t('calibration.wizard.mic.save') }}
              </el-button>
              <span class="aux-text">
                {{ settingsSaved ? t('calibration.wizard.mic.savedAt', { time: timeText(wizard?.updated_at ?? null) }) : t('calibration.wizard.mic.notSaved') }}
              </span>
            </div>
          </section>

          <!-- 第 4 步：测点 -->
          <section v-else-if="currentStep === 4" class="step-panel">
            <h2 class="step-panel__title">{{ t('calibration.step.names.positions') }}</h2>
            <p class="step-panel__desc">{{ t('calibration.wizard.positions.desc') }}</p>
            <div class="position-grid">
              <article class="position-card" :class="gate?.status === 'passed' ? 'position-card--selected' : ''">
                <header class="position-card__head">
                  <h3 class="position-card__title">{{ t('calibration.wizard.positions.single') }}</h3>
                  <TruthTag :kind="gate?.status === 'passed' ? 'actual' : 'notWired'" />
                </header>
                <p class="aux-text">{{ t('calibration.wizard.positions.singleDesc') }}</p>
                <p class="position-card__state">
                  {{ gate?.status === 'passed' ? t('calibration.wizard.positions.selected') : t('calibration.wizard.positions.notSelected') }}
                </p>
              </article>
              <article class="position-card position-card--disabled">
                <header class="position-card__head">
                  <h3 class="position-card__title">{{ t('calibration.wizard.positions.multi') }}</h3>
                  <TruthTag kind="notWired" />
                </header>
                <p class="aux-text">{{ t('calibration.wizard.positions.multiReason') }}</p>
                <p class="position-card__task">{{ t('calibration.gate.task', { task: 'P3' }) }}</p>
              </article>
              <article class="position-card position-card--disabled">
                <header class="position-card__head">
                  <h3 class="position-card__title">{{ t('calibration.wizard.positions.art') }}</h3>
                  <TruthTag kind="notWired" />
                </header>
                <p class="aux-text">{{ t('calibration.wizard.positions.artReason') }}</p>
                <p class="position-card__task">{{ t('calibration.gate.task', { task: 'P3' }) }}</p>
              </article>
            </div>
            <p v-if="gate?.reason" class="aux-text">{{ t('calibration.gate.reason', { reason: gate.reason }) }}</p>
          </section>

          <!-- 第 5 步：测量（P1 仅离线；实时测量 NOT_AVAILABLE） -->
          <section v-else-if="currentStep === 5" class="step-panel">
            <h2 class="step-panel__title">{{ t('calibration.step.names.measurement') }}</h2>
            <p class="step-panel__desc">{{ t('calibration.wizard.measure.desc') }}</p>
            <div class="measure-grid">
              <UploadCard
                :label="t('calibration.wizard.measure.baseline')"
                :hint="t('calibration.wizard.measure.hint')"
                accept=".wav,audio/wav"
                :action-label="t('calibration.wizard.measure.select')"
                :busy="state.busy.upload"
                :result="state.uploads.baseline"
                :details="measurementDetails(baselineInfo)"
                :error-text="baselineError"
                @select="(file) => onMeasurementFile('baseline', file)"
              />
              <UploadCard
                :label="t('calibration.wizard.measure.applied')"
                :hint="t('calibration.wizard.measure.hint')"
                accept=".wav,audio/wav"
                :action-label="t('calibration.wizard.measure.select')"
                :busy="state.busy.upload"
                :result="state.uploads.applied"
                :details="measurementDetails(appliedInfo)"
                :error-text="appliedError"
                @select="(file) => onMeasurementFile('applied', file)"
              />
            </div>
            <div class="banner banner--info" role="status">
              <div class="wizard-notice">
                <strong>NOT_AVAILABLE · {{ t('calibration.wizard.measure.realtime') }}</strong>
                <span>{{ t('calibration.wizard.measure.realtimeReason') }}</span>
                <span class="aux-text">{{ t('calibration.gate.task', { task: t('calibration.wizard.measure.realtimeTask') }) }}</span>
              </div>
              <TruthTag kind="notWired" />
            </div>
            <dl v-if="measurementShas.baseline || measurementShas.applied" class="fact-list fact-list--compact">
              <div class="fact-list__row">
                <dt class="fact-list__label">{{ t('calibration.wizard.measure.savedBaseline') }}</dt>
                <dd class="fact-list__value mono">{{ shortenId(measurementShas.baseline, 16, 12) ?? t('common.dash') }}</dd>
              </div>
              <div class="fact-list__row">
                <dt class="fact-list__label">{{ t('calibration.wizard.measure.savedApplied') }}</dt>
                <dd class="fact-list__value mono">{{ shortenId(measurementShas.applied, 16, 12) ?? t('common.dash') }}</dd>
              </div>
            </dl>
          </section>

          <!-- 第 6 步：分析 -->
          <section v-else-if="currentStep === 6" class="step-panel">
            <h2 class="step-panel__title">{{ t('calibration.step.names.analysis') }}</h2>
            <p class="step-panel__desc">{{ t('calibration.wizard.analysis.desc') }}</p>

            <ul class="check-list">
              <li class="check-list__item" :class="planSha ? 'is-ok' : 'is-missing'">
                <span class="check-list__mark" aria-hidden="true">{{ planSha ? '✓' : '!' }}</span>
                <span>{{ t('calibration.wizard.analysis.planSha') }}</span>
                <span class="mono aux-text">{{ shortenId(planSha, 16, 12) ?? t('calibration.wizard.analysis.planMissing') }}</span>
              </li>
              <li class="check-list__item" :class="measurementIds.baseline && measurementIds.applied ? 'is-ok' : 'is-missing'">
                <span class="check-list__mark" aria-hidden="true">{{ measurementIds.baseline && measurementIds.applied ? '✓' : '!' }}</span>
                <span>{{ t('calibration.wizard.analysis.uploadsMissing') }}</span>
                <span class="mono aux-text">
                  {{ shortenId(measurementIds.baseline, 8, 6) ?? t('common.dash') }} / {{ shortenId(measurementIds.applied, 8, 6) ?? t('common.dash') }}
                </span>
              </li>
              <li class="check-list__item" :class="settingsSaved ? 'is-ok' : 'is-missing'">
                <span class="check-list__mark" aria-hidden="true">{{ settingsSaved ? '✓' : '!' }}</span>
                <span>{{ t('calibration.wizard.analysis.settingsMissing') }}</span>
              </li>
            </ul>

            <div class="step-actions">
              <el-button
                v-if="job === null || (job !== null && isJobTerminal(job.status))"
                type="primary"
                :disabled="!canCreateJob"
                :loading="state.busy.createJob"
                @click="startAnalysis"
              >
                {{
                  state.busy.createJob
                    ? t('calibration.wizard.analysis.starting')
                    : job === null
                      ? t('calibration.wizard.analysis.start')
                      : t('calibration.wizard.analysis.retry')
                }}
              </el-button>
              <el-button
                v-if="job !== null && isJobActive(job.status)"
                type="warning"
                plain
                :loading="state.busy.cancelJob"
                @click="onCancelJob"
              >
                {{ t('calibration.wizard.analysis.cancel') }}
              </el-button>
              <span v-if="job !== null" class="aux-text">
                {{ t('calibration.wizard.analysis.jobId') }}: <span class="mono">{{ shortenId(job.id, 12, 6) }}</span>
                · {{ t('calibration.jobStatus.' + job.status) }}
              </span>
            </div>

            <div v-if="job === null" class="banner banner--info" role="status">
              {{ t('calibration.wizard.analysis.noJob') }}
            </div>

            <template v-else>
              <JobTimeline :steps="job.steps" :truth="jobTruth" />
              <div v-if="job.status === 'failed'" class="banner banner--danger" role="alert">
                <div class="wizard-notice">
                  <strong>{{ t('calibration.wizard.analysis.failedTitle') }}</strong>
                  <span class="mono">{{ t('calibration.wizard.analysis.errorCode') }}: {{ job.error?.code ?? t('common.dash') }}</span>
                  <span v-if="job.error?.message" class="aux-text">{{ job.error.message }}</span>
                </div>
              </div>
              <div v-else-if="job.status === 'cancelled'" class="banner banner--warn" role="status">
                <div class="wizard-notice">
                  <strong>{{ t('calibration.wizard.analysis.cancelledTitle') }}</strong>
                  <span>{{ t('calibration.wizard.analysis.cancelledDesc') }}</span>
                </div>
              </div>
              <JobResultPanel v-if="job.status === 'succeeded'" :job="job" />
            </template>
          </section>

          <!-- 第 7 步：设计 -->
          <section v-else-if="currentStep === 7" class="step-panel">
            <h2 class="step-panel__title">{{ t('calibration.step.names.design') }}</h2>
            <p class="step-panel__desc">{{ t('calibration.wizard.design.desc') }}</p>
            <template v-if="job?.result">
              <dl class="fact-list">
                <div class="fact-list__row">
                  <dt class="fact-list__label">{{ t('calibration.design.residual') }}</dt>
                  <dd class="fact-list__value">
                    <span class="mono">{{ job.result.design?.residual ?? t('common.dash') }}</span>
                    <TruthTag :kind="jobTruth" />
                  </dd>
                </div>
                <div class="fact-list__row">
                  <dt class="fact-list__label">{{ t('calibration.design.admission') }}</dt>
                  <dd class="fact-list__value">
                    <span>{{ job.result.design?.admission?.accepted ? t('calibration.design.admitted') : t('calibration.design.rejected') }}</span>
                    <span v-if="job.result.design?.admission?.code" class="mono">{{ job.result.design.admission.code }}</span>
                    <span v-if="job.result.design?.admission?.reason" class="aux-text">{{ job.result.design.admission.reason }}</span>
                  </dd>
                </div>
                <div class="fact-list__row">
                  <dt class="fact-list__label">{{ t('calibration.design.planSha') }}</dt>
                  <dd class="fact-list__value mono" :title="job.result.plan?.sha256 ?? undefined">
                    {{ shortenId(job.result.plan?.sha256 ?? null, 16, 12) ?? t('common.dash') }}
                  </dd>
                </div>
                <div class="fact-list__row">
                  <dt class="fact-list__label">{{ t('calibration.design.graphId') }}</dt>
                  <dd class="fact-list__value mono">{{ job.result.plan?.graph_id ?? t('common.dash') }}</dd>
                </div>
              </dl>
              <div v-if="gate?.status === 'blocked'" class="banner banner--warn" role="status">
                <div class="wizard-notice">
                  <strong>{{ t('calibration.wizard.design.nextBlocked') }}</strong>
                  <span v-if="gate.reason">{{ t('calibration.gate.reason', { reason: gate.reason }) }}</span>
                </div>
              </div>
            </template>
            <div v-else class="banner banner--info" role="status">
              {{ job === null ? t('calibration.wizard.analysis.noJob') : t('calibration.wizard.design.noResult') }}
            </div>
          </section>

          <!-- 第 8 步：回顾 / 应用 / 回滚 -->
          <section v-else class="step-panel">
            <h2 class="step-panel__title">{{ t('calibration.step.names.review_apply') }}</h2>
            <p class="step-panel__desc">{{ t('calibration.wizard.review.desc') }}</p>
            <ApplyPanel :job="job" :before-plan="beforePlan" @changed="onApplyChanged" />
          </section>
        </div>

        <aside class="wizard-side">
          <GateCard :step-label="gateStepLabel" :gate="gate">
            <dl class="fact-list fact-list--compact">
              <div v-for="item in gateFacts" :key="item.label" class="fact-list__row">
                <dt class="fact-list__label">{{ item.label }}</dt>
                <dd class="fact-list__value mono">{{ item.value }}</dd>
              </div>
            </dl>
          </GateCard>
          <div v-if="state.actionError" class="banner banner--danger" role="alert">
            {{ t('calibration.wizard.actionError', { message: t(state.actionError.i18nKey) }) }}
          </div>
        </aside>
      </div>

      <footer class="wizard-footer">
        <span class="wizard-footer__step">
          {{ t('calibration.wizard.stepOf', { current: currentStep, total: WIZARD_STEP_COUNT }) }}
        </span>
        <div class="wizard-footer__actions">
          <el-button :disabled="currentStep <= 1" @click="goPrev">{{ t('calibration.wizard.previous') }}</el-button>
          <el-button plain @click="cancelWizard">{{ t('calibration.wizard.cancelWizard') }}</el-button>
          <el-button
            type="primary"
            :disabled="currentStep < WIZARD_STEP_COUNT && !canGoNext"
            :title="nextTitle"
            @click="goNext"
          >
            {{ nextLabel }}
          </el-button>
        </div>
      </footer>
    </template>
  </div>
</template>

<style scoped>
.wizard-page {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-4);
  min-width: 0;
}

.wizard-loading {
  padding: var(--khaen-space-4);
  border: 1px dashed var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
}

.wizard-notice {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
  overflow-wrap: anywhere;
}

.wizard-stepper-bar {
  position: sticky;
  top: 0;
  z-index: 5;
  padding: var(--khaen-space-2) 0;
  background: var(--khaen-bg);
  border-bottom: 1px solid var(--khaen-border);
}

.wizard-body {
  display: grid;
  grid-template-columns: minmax(0, 8fr) minmax(0, 4fr);
  gap: var(--khaen-space-4);
  align-items: start;
}

.wizard-main,
.wizard-side {
  min-width: 0;
}

.wizard-side {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-3);
  position: sticky;
  top: 72px;
}

.step-panel {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-3);
  padding: var(--khaen-space-4);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
  box-shadow: var(--khaen-shadow-card);
  min-width: 0;
}

.step-panel__title {
  font-size: 18px;
  font-weight: 600;
}

.step-panel__desc {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.fact-list {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  margin: 0;
}

.fact-list--compact {
  gap: var(--khaen-space-1);
}

.fact-list__row {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  border-bottom: 1px dashed var(--khaen-border);
  padding-bottom: 4px;
  flex-wrap: wrap;
}

.fact-list__row:last-child {
  border-bottom: 0;
  padding-bottom: 0;
}

.fact-list__label {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.fact-list__value {
  display: inline-flex;
  align-items: center;
  gap: var(--khaen-space-1);
  margin: 0;
  font-size: var(--khaen-font-aux);
  text-align: right;
  overflow-wrap: anywhere;
  max-width: 100%;
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--khaen-space-3);
}

.form-field {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  min-width: 0;
}

.form-field--wide {
  grid-column: 1 / -1;
}

.form-field__label {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.form-field__hint {
  color: var(--khaen-text-muted);
  font-size: 11px;
}

.form-error {
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
}

.step-actions {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.measure-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--khaen-space-3);
}

.position-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: var(--khaen-space-3);
}

.position-card {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-3);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel-hover);
  min-width: 0;
}

.position-card--selected {
  border-color: color-mix(in srgb, var(--khaen-success) 45%, transparent);
}

.position-card--disabled {
  border-style: dashed;
  opacity: 0.85;
}

.position-card__head {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

.position-card__title {
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
}

.position-card__state {
  font-size: var(--khaen-font-aux);
  font-weight: 600;
}

.position-card--selected .position-card__state {
  color: var(--khaen-success);
}

.position-card__task {
  color: var(--khaen-warning);
  font-size: 11px;
  font-family: var(--khaen-font-mono);
}

.check-list {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  margin: 0;
  padding: 0;
  list-style: none;
}

.check-list__item {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  padding: var(--khaen-space-1) var(--khaen-space-2);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-button);
  font-size: var(--khaen-font-aux);
  flex-wrap: wrap;
}

.check-list__item.is-ok {
  border-color: color-mix(in srgb, var(--khaen-success) 45%, transparent);
}

.check-list__item.is-missing {
  border-color: color-mix(in srgb, var(--khaen-warning) 45%, transparent);
}

.check-list__mark {
  display: inline-grid;
  place-items: center;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  font-size: 11px;
  font-weight: 700;
  flex: none;
}

.check-list__item.is-ok .check-list__mark {
  color: var(--khaen-success);
  background: var(--khaen-success-soft);
}

.check-list__item.is-missing .check-list__mark {
  color: var(--khaen-warning);
  background: var(--khaen-warning-soft);
}

.wizard-footer {
  position: sticky;
  bottom: 0;
  z-index: 6;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
  min-height: 64px;
  padding: var(--khaen-space-2) var(--khaen-space-4);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-panel);
  background: var(--khaen-panel);
  box-shadow: 0 -2px 8px rgba(0, 0, 0, 0.18);
}

.wizard-footer__step {
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
  font-family: var(--khaen-font-mono);
}

.wizard-footer__actions {
  display: flex;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
}

@media (max-width: 1279px) {
  .wizard-body {
    grid-template-columns: minmax(0, 1fr);
  }

  .wizard-side {
    position: static;
    order: -1;
  }

  .position-grid {
    grid-template-columns: minmax(0, 1fr);
  }
}

@media (max-width: 767px) {
  .form-grid,
  .measure-grid {
    grid-template-columns: minmax(0, 1fr);
  }

  .wizard-footer {
    flex-direction: column;
    align-items: stretch;
  }

  .wizard-footer__actions {
    flex-direction: column;
  }

  .wizard-footer__actions .el-button {
    width: 100%;
    margin-left: 0;
  }

  .step-panel {
    padding: var(--khaen-space-3);
  }
}
</style>
