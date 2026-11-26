<script setup lang="ts">
/**
 * 校准 job 结果面板（PLAN §6.7 / §10.2 第 6–7 步）：verdict + 指标 + 1/3-oct 表 + 设计残留 + 产物。
 *
 * 所有数值来自 daemon 的 job result；缺失显示「—」，不编造。
 * mock/fixture 引擎（engine=synthetic/mock/fixture）的数据标「示意」，不冒充实测。
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus'
import { Download } from '@element-plus/icons-vue'
import TruthTag from '@/components/TruthTag.vue'
import VerdictBadge from './VerdictBadge.vue'
import {
  resultTruth,
  type CalibrationArtifact,
  type CalibrationDesign,
  type CalibrationJob,
  type CalibrationJobResult
} from '@/api/calibration'
import { downloadCalibrationArtifact } from '@/stores/calibration'
import { formatDecimal, formatInteger, shortenId, toByteUnit } from '@/utils/format'
import type { TruthKind } from '@/types/ui'

// 注意：Vue 会把缺省的 Boolean prop 归一为 false，必须显式给默认 true，
// 否则 job 详情页不传 showDesign 时设计摘要会被静默隐藏。
const props = withDefaults(
  defineProps<{
    job: CalibrationJob
    showDesign?: boolean
  }>(),
  { showDesign: true }
)

const { t, locale } = useI18n({ useScope: 'global' })

const localeId = computed(() => (locale.value === 'en-US' ? 'en-US' : 'zh-CN'))
const result = computed<CalibrationJobResult | null>(() => props.job.result)
/**
 * 结果真实性：优先用 result.measured / result.simulated（合同 §7【加法】），
 * 缺失时按 engine 判定；synthetic 永远不写成实测。
 */
const truth = computed<TruthKind>(() => resultTruth(result.value, props.job.engine))

const downloadingArtifact = ref<string | null>(null)

/**
 * 产物下载必须走带认证的 API（getRaw：Cookie 会话 + 统一 401/错误体），
 * 不能直接把 artifacts 路径放进 <a href>（同源 Cookie 虽在，但无法处理 401/错误体）。
 * 拿到 text/filename 后用 Blob + object URL 保存（浏览器侧）。
 */
async function downloadArtifact(artifact: CalibrationArtifact): Promise<void> {
  if (downloadingArtifact.value !== null) {
    return
  }
  downloadingArtifact.value = artifact.name
  try {
    const download = await downloadCalibrationArtifact(props.job.id, artifact.name)
    if (download.ok !== true) {
      const detail = download.error.detail ?? download.error.code
      ElMessage.error(t('calibrationEdit.artifact.failed') + ': ' + t(download.error.i18nKey) + ' — ' + detail)
      return
    }
    // 无损字节（getRawBytes）：二进制产物（plan.bin）也可以直接 Blob 保存。
    const blob = new Blob([download.value.data], {
      type: download.value.content_type ?? artifact.content_type ?? 'application/octet-stream'
    })
    const url = URL.createObjectURL(blob)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = download.value.filename
    document.body.appendChild(anchor)
    anchor.click()
    document.body.removeChild(anchor)
    URL.revokeObjectURL(url)
    ElMessage.success(t('calibrationEdit.artifact.done', { filename: download.value.filename }))
  } finally {
    downloadingArtifact.value = null
  }
}

function num(value: number | null, digits = 2): string {
  return formatDecimal(value, digits, localeId.value) ?? t('common.dash')
}

function int(value: number | null): string {
  return formatInteger(value, localeId.value) ?? t('common.dash')
}

function bytes(value: number | null): string {
  const unit = toByteUnit(value)
  if (unit === null) {
    return t('common.dash')
  }
  const digits = unit.unitKey === 'unit.bytes' ? 0 : 2
  return (formatDecimal(unit.value, digits, localeId.value) ?? '') + ' ' + t(unit.unitKey)
}

function residualTone(design: CalibrationDesign | null): string {
  if (design === null) {
    return ''
  }
  if (design.residual === 'FULL') {
    return 'residual--ok'
  }
  if (design.residual === 'PARTIAL') {
    return 'residual--warn'
  }
  return 'residual--bad'
}
</script>

<template>
  <div class="job-result">
    <p v-if="truth === 'demo'" class="banner banner--warn" role="status">
      {{ t('calibration.result.syntheticHint', { engine: props.job.engine ?? '' }) }}
    </p>

    <template v-if="result">
      <div class="job-result__verdict">
        <VerdictBadge :verdict="result.verdict" :engine="props.job.engine" :truth="truth" large />
        <span v-if="props.job.verdict !== result.verdict" class="aux-text">
          {{ t('calibration.result.jobVerdictMismatch', { job: props.job.verdict ?? t('common.dash'), result: result.verdict ?? t('common.dash') }) }}
        </span>
      </div>

      <dl class="job-result__metrics">
        <div class="job-result__metric">
          <dt>{{ t('calibration.result.maxDelta') }}</dt>
          <dd class="mono">{{ num(result.max_delta_db) }} <span class="aux-text">dB</span></dd>
        </div>
        <div class="job-result__metric">
          <dt>{{ t('calibration.result.comparedBands') }}</dt>
          <dd class="mono">{{ int(result.compared_bands) }}</dd>
        </div>
        <div class="job-result__metric">
          <dt>{{ t('calibration.result.missingBands') }}</dt>
          <dd class="mono">{{ int(result.missing_bands) }}</dd>
        </div>
      </dl>

      <section v-if="result.violations.length > 0" class="job-result__violations">
        <h3 class="job-result__subtitle">{{ t('calibration.result.violations') }}</h3>
        <ul class="job-result__violation-list">
          <li v-for="violation in result.violations" :key="violation" class="job-result__violation">{{ violation }}</li>
        </ul>
      </section>

      <section class="job-result__section">
        <h3 class="job-result__subtitle">{{ t('calibration.result.bands') }} <TruthTag :kind="truth" /></h3>
        <div v-if="result.bands.length === 0" class="aux-text">{{ t('calibration.result.noBands') }}</div>
        <div v-else class="job-result__table-wrap">
          <table class="job-result__table">
            <caption class="sr-only">{{ t('calibration.result.bands') }}</caption>
            <thead>
              <tr>
                <th scope="col">{{ t('calibration.result.bandHz') }}</th>
                <th scope="col">{{ t('calibration.result.predicted') }}</th>
                <th scope="col">{{ t('calibration.result.observed') }}</th>
                <th scope="col">{{ t('calibration.result.delta') }}</th>
                <th scope="col">{{ t('calibration.result.missing') }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="band in result.bands" :key="band.hz">
                <td class="mono">{{ num(band.hz, 1) }}</td>
                <td class="mono">{{ num(band.predicted_db) }}</td>
                <td class="mono">{{ num(band.observed_db) }}</td>
                <td class="mono">{{ num(band.delta_db) }}</td>
                <td>{{ band.missing ? t('common.yes') : t('common.no') }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <section v-if="props.showDesign !== false" class="job-result__section">
        <h3 class="job-result__subtitle">{{ t('calibration.design.title') }}</h3>
        <dl class="job-result__kv">
          <div class="job-result__kv-row">
            <dt>{{ t('calibration.design.residual') }}</dt>
            <dd>
              <span v-if="result.design" class="residual" :class="residualTone(result.design)">
                {{ result.design.residual }}
              </span>
              <span v-else class="aux-text">{{ t('common.dash') }}</span>
            </dd>
          </div>
          <div class="job-result__kv-row">
            <dt>{{ t('calibration.design.admission') }}</dt>
            <dd>
              <template v-if="result.design?.admission">
                <span :class="result.design.admission.accepted ? 'tone-ok' : 'tone-bad'">
                  {{ result.design.admission.accepted ? t('calibration.design.admitted') : t('calibration.design.rejected') }}
                </span>
                <span v-if="result.design.admission.code" class="mono"> · {{ result.design.admission.code }}</span>
                <span v-if="result.design.admission.reason" class="aux-text"> · {{ result.design.admission.reason }}</span>
              </template>
              <span v-else class="aux-text">{{ t('common.dash') }}</span>
            </dd>
          </div>
          <div class="job-result__kv-row">
            <dt>{{ t('calibration.design.planSha') }}</dt>
            <dd class="mono" :title="result.plan?.sha256 ?? undefined">{{ shortenId(result.plan?.sha256 ?? null, 16, 12) ?? t('common.dash') }}</dd>
          </div>
          <div class="job-result__kv-row">
            <dt>{{ t('calibration.design.graphId') }}</dt>
            <dd class="mono">{{ result.plan?.graph_id ?? t('common.dash') }}</dd>
          </div>
        </dl>
      </section>

      <section class="job-result__section">
        <h3 class="job-result__subtitle">{{ t('calibration.job.artifacts') }}</h3>
        <div v-if="props.job.artifacts.length === 0" class="aux-text">{{ t('calibration.job.noArtifacts') }}</div>
        <ul v-else class="job-result__artifacts">
          <li v-for="artifact in props.job.artifacts" :key="artifact.name" class="job-result__artifact">
            <span class="mono job-result__artifact-name">{{ artifact.name }}</span>
            <span class="aux-text mono" :title="artifact.sha256 ?? undefined">
              sha256: {{ shortenId(artifact.sha256, 12, 8) ?? t('common.dash') }}
            </span>
            <span class="aux-text mono">{{ bytes(artifact.bytes) }}</span>
            <el-button
              class="job-result__download"
              size="small"
              plain
              :loading="downloadingArtifact === artifact.name"
              :disabled="downloadingArtifact !== null && downloadingArtifact !== artifact.name"
              :aria-label="t('calibrationEdit.artifact.download') + ': ' + artifact.name"
              @click="downloadArtifact(artifact)"
            >
              <el-icon :size="13"><Download /></el-icon>
              <span>{{ t('calibrationEdit.artifact.download') }}</span>
            </el-button>
          </li>
        </ul>
      </section>
    </template>

    <div v-else class="job-result__empty">
      <p>{{ t('calibration.result.empty') }}</p>
      <p v-if="props.job.reason" class="aux-text">{{ props.job.reason }}</p>
    </div>
  </div>
</template>

<style scoped>
.job-result {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-3);
  min-width: 0;
}

.job-result__verdict {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-3);
  flex-wrap: wrap;
}

.job-result__metrics {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: var(--khaen-space-2);
  margin: 0;
}

.job-result__metric {
  padding: var(--khaen-space-2);
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-button);
  background: var(--khaen-panel-hover);
}

.job-result__metric dt {
  color: var(--khaen-text-secondary);
  font-size: 11px;
}

.job-result__metric dd {
  margin: 0;
  font-size: 20px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}

.job-result__subtitle {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  font-size: var(--khaen-font-card-title);
  font-weight: 600;
  margin-bottom: var(--khaen-space-2);
}

.job-result__section {
  min-width: 0;
}

.job-result__violation-list {
  margin: 0;
  padding-left: var(--khaen-space-4);
  color: var(--khaen-danger);
  font-size: var(--khaen-font-aux);
}

.job-result__table-wrap {
  overflow-x: auto;
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-button);
}

.job-result__table {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--khaen-font-aux);
}

.job-result__table th,
.job-result__table td {
  padding: 6px 10px;
  text-align: right;
  border-bottom: 1px solid var(--khaen-border);
  white-space: nowrap;
}

.job-result__table th:first-child,
.job-result__table td:first-child {
  text-align: left;
}

.job-result__table th {
  color: var(--khaen-text-secondary);
  font-weight: 500;
  background: var(--khaen-panel-hover);
  position: sticky;
  top: 0;
}

.job-result__table tbody tr:last-child td {
  border-bottom: 0;
}

.job-result__kv {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  margin: 0;
}

.job-result__kv-row {
  display: flex;
  justify-content: space-between;
  gap: var(--khaen-space-3);
  font-size: var(--khaen-font-aux);
  border-bottom: 1px dashed var(--khaen-border);
  padding-bottom: 4px;
  flex-wrap: wrap;
}

.job-result__kv-row dt {
  color: var(--khaen-text-secondary);
}

.job-result__kv-row dd {
  margin: 0;
  text-align: right;
  overflow-wrap: anywhere;
}

.residual {
  padding: 1px 8px;
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-tag);
  font-family: var(--khaen-font-mono);
  font-size: 11px;
}

.residual--ok {
  color: var(--khaen-success);
  border-color: color-mix(in srgb, var(--khaen-success) 45%, transparent);
}

.residual--warn {
  color: var(--khaen-warning);
  border-color: color-mix(in srgb, var(--khaen-warning) 45%, transparent);
}

.residual--bad {
  color: var(--khaen-danger);
  border-color: color-mix(in srgb, var(--khaen-danger) 45%, transparent);
}

.tone-ok {
  color: var(--khaen-success);
}

.tone-bad {
  color: var(--khaen-danger);
}

.job-result__artifacts {
  display: flex;
  flex-direction: column;
  gap: var(--khaen-space-1);
  margin: 0;
  padding: 0;
  list-style: none;
}

.job-result__artifact {
  display: flex;
  align-items: center;
  gap: var(--khaen-space-2);
  flex-wrap: wrap;
  padding: 6px 8px;
  border: 1px solid var(--khaen-border);
  border-radius: var(--khaen-radius-button);
  font-size: var(--khaen-font-aux);
}

.job-result__artifact-name {
  font-weight: 600;
}

.job-result__download {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-left: auto;
  color: var(--khaen-accent);
  min-height: 32px;
}

.job-result__empty {
  padding: var(--khaen-space-3);
  border: 1px dashed var(--khaen-border-strong);
  border-radius: var(--khaen-radius-panel);
  color: var(--khaen-text-secondary);
  font-size: var(--khaen-font-aux);
}

.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}

@media (max-width: 767px) {
  .job-result__metrics {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
