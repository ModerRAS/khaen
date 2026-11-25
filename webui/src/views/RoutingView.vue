<script setup lang="ts">
// 路由矩阵：role → DAC 编辑 + prepare/commit 实际生效流程；不可用/未接线必须显式说明。
import { computed, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { Plus, Delete, Refresh, Upload } from '@element-plus/icons-vue'
import PageHeader from '@/components/PageHeader.vue'
import PanelCard from '@/components/PanelCard.vue'
import AckPanel from '@/components/AckPanel.vue'
import CopyText from '@/components/CopyText.vue'
import TruthTag from '@/components/TruthTag.vue'
import ContentLayoutPanel from '@/components/ContentLayoutPanel.vue'
import { api, ApiError, errorText } from '@/api/client'
import { refreshRuntime, runtimeState } from '@/stores/runtime'
import type { AckView, ChannelMapBody, CommitBody, PrepareBody, RoleDac, TargetBody } from '@/api/types'
import { shortHash } from '@/utils/format'

const ALLOWED_ROLES = ['FL', 'FR', 'C', 'SL', 'SR', 'SUB1', 'SUB2', 'SUB3', 'HFL', 'HFR']
const LAYOUT_PRESETS = ['4.1.2']

const target = ref<TargetBody | null>(null)
const inputMode = ref<ChannelMapBody | null>(null)
const inputModeError = ref('')

const layout = ref('4.1.2')
const profile = ref('p1-412')
const channels = ref(8)
const roles = ref<RoleDac[]>([])
const lowBandDacs = ref<number[]>([])
const loading = ref(false)
const applying = ref(false)
const blockedReason = ref('')
const ack = ref<AckView | null>(null)
const prepareInfo = ref<PrepareBody | null>(null)
const commitInfo = ref<CommitBody | null>(null)
const flowStep = ref(0)

const status = computed(() => runtimeState.status)
const notWired = computed<string[]>(() => status.value?.host?.not_wired ?? [])
const planWired = computed(() => !notWired.value.some((item) => ['prepare', 'commit', 'target'].includes(item)))
const planDisabledReason = computed(() =>
  planWired.value ? '' : '后端 host.not_wired 含 ' + notWired.value.filter((item) => ['prepare', 'commit', 'target'].includes(item)).join('/') +
    '（engine=' + (status.value?.engine ?? '?') + '）：prepare/commit 会返回 unsupported，控件已禁用；本页仍可只读查看当前 target。'
)
const requiresCommit = computed(() => target.value?.requires_commit === true || dirty())
const pendingPrepare = computed(() => target.value?.pending_prepare ?? null)

function dirty(): boolean {
  if (!target.value) { return false }
  const current = target.value.target
  if (layout.value !== current.layout || profile.value !== current.profile || channels.value !== current.channels) { return true }
  if (current.roles.length !== roles.value.length) { return true }
  return current.roles.some((role, index) => role.role !== roles.value[index]?.role || role.dac !== roles.value[index]?.dac)
}

function applyTarget(body: TargetBody): void {
  target.value = body
  layout.value = body.target.layout
  profile.value = body.target.profile
  channels.value = body.target.channels
  roles.value = body.target.roles.map((role) => ({ role: role.role, dac: role.dac }))
  lowBandDacs.value = [...body.target.low_band_dacs]
}

async function loadTarget(): Promise<void> {
  loading.value = true
  try {
    applyTarget(await api.targetGet())
    blockedReason.value = ''
  } catch (error) {
    blockedReason.value = `路由端点不可用：${errorText(error)}`
  } finally {
    loading.value = false
  }
}

async function loadInputMode(): Promise<void> {
  try {
    inputMode.value = await api.channelMapGet()
  } catch (error) {
    inputMode.value = null
    inputModeError.value = errorText(error)
  }
}

onMounted(async () => {
  await Promise.all([loadTarget(), loadInputMode()])
})

function addRole(): void {
  const used = new Set(roles.value.map((role) => role.role))
  const next = ALLOWED_ROLES.find((role) => !used.has(role))
  if (!next) { ElMessage.warning('没有可添加的角色（白名单已用尽）'); return }
  roles.value.push({ role: next, dac: 0 })
}

function removeRole(index: number): void {
  roles.value.splice(index, 1)
}

function ensureUniqueDac(): boolean {
  const dacs = roles.value.map((role) => role.dac)
  if (new Set(dacs).size !== dacs.length) {
    blockedReason.value = '同一 DAC 通道不能映射多个角色（后端会拒绝）；请先修正。'
    return false
  }
  return true
}

async function applyRouting(): Promise<void> {
  blockedReason.value = ''
  ack.value = null
  commitInfo.value = null
  prepareInfo.value = null
  if (!ensureUniqueDac()) { return }
  applying.value = true
  flowStep.value = 1
  try {
    await api.targetPut({
      layout: layout.value.trim(),
      profile: profile.value.trim(),
      channels: Number(channels.value),
      roles: roles.value.map((role) => ({ role: role.role, dac: Number(role.dac) })),
      low_band_dacs: lowBandDacs.value
    })
    flowStep.value = 2
    const prepared = await api.prepare()
    prepareInfo.value = prepared
    flowStep.value = 3
    const committed = await api.commit(prepared.prepare_id)
    commitInfo.value = committed
    flowStep.value = 4
    ack.value = {
      kind: committed.synthetic ? 'synthetic' : committed.actual ? 'actual' : 'info',
      engine: committed.engine,
      synthetic: committed.synthetic,
      actual: committed.actual,
      detail: `prepare→commit 完成：actual_version=${committed.actual_version} · plan_version=${committed.plan_version} · blocks_committed=${committed.blocks_committed} · ack_mask=${committed.ack_mask} · graph=${shortHash(committed.graph_id)} · epoch=${committed.epoch}`,
      raw: committed
    }
    await Promise.all([loadTarget(), refreshRuntime()])
    ElMessage.success(committed.synthetic ? '已提交（synthetic 回执，非 actual）' : '路由已生效（actual）')
  } catch (error) {
    flowStep.value = -1
    const code = error instanceof ApiError ? error.code : 'error'
    const unsupported = ['unsupported', 'not_implemented', 'not_supported', 'unsupported_api_version'].includes(code)
    ack.value = {
      kind: unsupported ? 'unsupported' : 'error',
      engine: null,
      synthetic: null,
      actual: null,
      detail: unsupported
        ? `后端明确不支持该路由操作（${code}）：${errorText(error)}；控件保持可编辑但不会静默失败。`
        : `路由应用失败（未生效）：${errorText(error)}`,
      raw: error instanceof ApiError ? error.body : String(error)
    }
    if (unsupported) { blockedReason.value = errorText(error) }
  } finally {
    applying.value = false
  }
}

async function commitPending(): Promise<void> {
  const id = pendingPrepare.value
  if (!id) { return }
  applying.value = true
  try {
    const committed = await api.commit(id)
    commitInfo.value = committed
    ack.value = {
      kind: committed.synthetic ? 'synthetic' : committed.actual ? 'actual' : 'info',
      engine: committed.engine,
      synthetic: committed.synthetic,
      actual: committed.actual,
      detail: `已提交待生效 prepare：actual_version=${committed.actual_version} · plan_version=${committed.plan_version}`,
      raw: committed
    }
    await Promise.all([loadTarget(), refreshRuntime()])
  } catch (error) {
    ack.value = {
      kind: 'error', engine: null, synthetic: null, actual: null,
      detail: `提交失败：${errorText(error)}`,
      raw: error instanceof ApiError ? error.body : String(error)
    }
  } finally {
    applying.value = false
  }
}

async function abortPending(): Promise<void> {
  const id = pendingPrepare.value
  if (!id) { return }
  try {
    await api.abort(id)
    await loadTarget()
    ElMessage.info('已丢弃待生效 prepare')
  } catch (error) {
    ack.value = {
      kind: 'error', engine: null, synthetic: null, actual: null,
      detail: `丢弃失败：${errorText(error)}`,
      raw: error instanceof ApiError ? error.body : String(error)
    }
  }
}

const flowSteps = [
  { key: 1, title: 'PUT /api/runtime/target', hint: '写入期望 target（draft）' },
  { key: 2, title: 'POST /api/runtime/prepare', hint: 'engine admission，返回 prepare_id（未生效）' },
  { key: 3, title: 'POST /api/runtime/commit', hint: '提交后实际生效（以 actual_version 回读为准）' }
]
</script>

<template>
  <div class="page">
    <PageHeader
      title="路由矩阵"
      subtitle="role → DAC 由 target 显式给出；编辑后必须 prepare → commit 才实际生效（synthetic 回执会标注）。"
    >
      <template #right>
        <el-button :icon="Refresh" :loading="loading" @click="loadTarget">读取实际路由</el-button>
      </template>
    </PageHeader>

    <div v-if="blockedReason" class="banner banner--warn">
      <div><div class="banner__title">路由操作受限</div><div class="banner__body">{{ blockedReason }}</div></div>
    </div>
    <div v-if="status?.synthetic" class="banner banner--warn">
      <div>
        <div class="banner__title">engine={{ status.engine }}（synthetic）— commit 回执不是板端 actual</div>
        <div class="banner__body">prepare/commit 走内存 engine 模型；页面会按回执 synthetic 标注，不会显示成「已生效（actual）」。</div>
      </div>
    </div>

    <div v-if="!planWired" class="banner banner--info">
      <div>
        <div class="banner__title">路由写入未接线（unsupported）— prepare/commit 已禁用</div>
        <div class="banner__body">{{ planDisabledReason }}</div>
      </div>
    </div>

    <div v-if="pendingPrepare" class="banner banner--info">
      <div style="flex: 1; min-width: 0">
        <div class="banner__title">存在待提交的 prepare：<span class="mono">{{ pendingPrepare }}</span></div>
        <div class="banner__body">prepare 只是 admission 结果，尚未生效；必须 commit 才会切换实际图。</div>
      </div>
      <div class="actions">
        <el-button type="primary" size="small" :loading="applying" :disabled="!planWired" @click="commitPending">提交生效</el-button>
        <el-button size="small" @click="abortPending">丢弃</el-button>
      </div>
    </div>

    <div class="grid-2">
      <PanelCard title="目标配置" subtitle="layout / profile / 容器宽度">
        <template #extra>
          <TruthTag :engine="status?.engine" :synthetic="status?.synthetic" :actual="status?.actual" compact />
        </template>
        <div class="form-grid">
          <div class="form-field">
            <label>layout</label>
            <el-select v-model="layout" filterable allow-create default-first-option placeholder="4.1.2">
              <el-option v-for="item in LAYOUT_PRESETS" :key="item" :label="item" :value="item" />
            </el-select>
          </div>
          <div class="form-field">
            <label>profile</label>
            <el-input v-model="profile" placeholder="p1-412" />
          </div>
          <div class="form-field">
            <label>channels（容器宽度）</label>
            <el-input-number v-model="channels" :min="2" :max="8" :step="1" style="width: 100%" />
          </div>
          <div class="form-field">
            <label>低带 dense DAC（由 T57 overlay 解析，本页原样回传）</label>
            <el-input :model-value="lowBandDacs.join(', ')" disabled />
          </div>
        </div>
        <div class="actions" style="margin-top: 14px">
          <el-button type="primary" :icon="Upload" :loading="applying" :disabled="!planWired" @click="applyRouting">
            应用路由（PUT target → prepare → commit）
          </el-button>
          <el-button :disabled="applying" @click="loadTarget">还原实际路由</el-button>
        </div>
        <p class="hint" style="margin-top: 10px">
          layout=4.1.2 时后端按 P1 固定表逐项校验（FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7，ch6 空）；
          自定义映射会被后端拒绝并在此处如实回显错误，不会静默失败。
        </p>
      </PanelCard>

      <PanelCard title="prepare / commit 流程" subtitle="只有 commit 的 actual ack 才算生效">
        <ol class="timeline">
          <li v-for="step in flowSteps" :key="step.key" class="timeline__step"
              :class="{ 'is-done': flowStep > step.key || flowStep === 4, 'is-current': flowStep === step.key, 'is-failed': flowStep === -1 }">
            <div class="timeline__rail">
              <span class="timeline__dot" />
              <span v-if="step.key < 3" class="timeline__line" />
            </div>
            <div class="timeline__label">
              {{ step.title }}
              <div class="timeline__meta">{{ step.hint }}</div>
            </div>
          </li>
        </ol>
        <div v-if="prepareInfo" class="kv" style="margin-top: 8px">
          <div class="kv__item"><span class="kv__label">prepare_id</span>
            <CopyText :value="prepareInfo.prepare_id" :short="18" /></div>
          <div class="kv__item"><span class="kv__label">planned_version</span>
            <span class="kv__value">{{ prepareInfo.planned_version }}</span></div>
          <div class="kv__item"><span class="kv__label">admission</span>
            <span class="kv__value">{{ prepareInfo.admission }}</span></div>
          <div class="kv__item"><span class="kv__label">expected_workers / admitted</span>
            <span class="kv__value">{{ prepareInfo.expected_workers }} / {{ prepareInfo.admitted_bytes }} B</span></div>
        </div>
        <div v-if="commitInfo" class="kv" style="margin-top: 8px">
          <div class="kv__item"><span class="kv__label">blocks_committed / ack_mask</span>
            <span class="kv__value">{{ commitInfo.blocks_committed }} / {{ commitInfo.ack_mask }}</span></div>
          <div class="kv__item"><span class="kv__label">plan_version</span>
            <span class="kv__value">{{ commitInfo.plan_version }}（prev {{ commitInfo.previous_plan_version }}）</span></div>
        </div>
      </PanelCard>
    </div>

    <PanelCard title="输出矩阵（role → DAC）" subtitle="DAC 通道号 0-based；同通道不可重复">
      <div class="table-scroll">
        <table class="data-table">
          <thead>
            <tr><th>role</th><th>DAC 通道</th><th style="width: 80px">操作</th></tr>
          </thead>
          <tbody>
            <tr v-for="(role, index) in roles" :key="index">
              <td>
                <el-select v-model="role.role" style="min-width: 110px">
                  <el-option v-for="item in ALLOWED_ROLES" :key="item" :label="item" :value="item" />
                </el-select>
              </td>
              <td>
                <el-select v-model="role.dac" style="min-width: 100px">
                  <el-option v-for="dac in channels" :key="dac" :label="'DAC ' + (dac - 1)" :value="dac - 1" />
                </el-select>
              </td>
              <td>
                <el-button link type="danger" :icon="Delete" @click="removeRole(index)">删除</el-button>
              </td>
            </tr>
            <tr v-if="!roles.length">
              <td colspan="3" class="hint">没有角色映射（读取失败或 target 为空）。</td>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="actions" style="margin-top: 12px">
        <el-button :icon="Plus" @click="addRole">添加角色</el-button>
      </div>
      <div class="kv" style="margin-top: 12px">
        <div class="kv__item"><span class="kv__label">目标 target_hash</span>
          <CopyText :value="target?.target_hash" :short="20" /></div>
        <div class="kv__item"><span class="kv__label">实际 target_hash</span>
          <CopyText :value="target?.actual_target_hash" :short="20" /></div>
        <div class="kv__item"><span class="kv__label">requires_commit</span>
          <span class="kv__value">{{ target?.requires_commit ?? false }}{{ dirty() ? '（本地有未提交修改）' : '' }}</span></div>
      </div>
    </PanelCard>

    <PanelCard
      title="输入 / 内容矩阵"
      subtitle="后端 /api/channelmap/input-mode 的接线事实（未接线就明确显示未支持，不放假数据）"
    >
      <div v-if="inputMode?.status === 'not_supported'" class="banner banner--info">
        <div>
          <div class="banner__title">unsupported：输入内容矩阵未接线</div>
          <div class="banner__body">
            status={{ inputMode.status }} · reason={{ inputMode.reason ?? '—' }}。
            P1 不做内容上混/下混（无 stereo→multi 自动变换）；输入侧不提供可编辑矩阵，本页不伪造。
          </div>
        </div>
      </div>
      <div v-else-if="inputModeError" class="banner banner--warn">
        <div><div class="banner__title">输入矩阵端点不可用</div><div class="banner__body">{{ inputModeError }}</div></div>
      </div>
      <div v-else class="hint">输入矩阵端点返回：{{ JSON.stringify(inputMode) }}</div>
    </PanelCard>

    <ContentLayoutPanel />

    <PanelCard title="操作回执">
      <AckPanel :ack="ack" title="路由操作回执" />
      <div v-if="!ack" class="hint">尚无路由操作回执。</div>
    </PanelCard>
  </div>
</template>