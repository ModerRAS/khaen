/**
 * 音频通道 profile / Speaker Layout REST 客户端（docs/API.md §12 v1.2；设计 docs/SPEAKER-MAPPING.md）。
 *
 * 端点：
 * - GET  /api/audio-profile         只读：{ok, schema, profile, validation, capability, plan}（后端未接线时 200+null+reasons）
 * - PUT  /api/audio-profile         两阶段：{stage:"prepare", profile} → 草稿/校验/diff/token；
 *                                   {stage:"commit", token} 保存；{stage:"abort", token} 丢弃
 * - POST /api/audio-profile/plan    profile → 方案生成（不自动提交；之后走 /api/plan/inspect + /api/prepare + /api/commit）
 * - GET  /api/device/inputs         声卡输入能力（只读；取不到 available:false + reason）
 * - POST /api/speaker-test          通道识别测试音（必须 audible:true + confirm:true；≤10s；轻音量）
 * - POST /api/speaker-test/stop     立即停止（幂等）
 *
 * 纪律：
 * - 角色 canonical（19 个）只存在于配置层/UI；代码里不用中文，显示名走 i18n（speakerRole.*）；
 * - 不做假数据：拿不到的字段 null + reason；写端点 501/503 如实归类，不假成功；
 * - 结构校验以前端 advisory + 后端 validation 为准（后端返回的 errors/warnings 原样展示）；
 * - CueMix 边界：本页只生成 profile/方案与测试音，绝不 per-channel 写 CueMix（未使用 DAC 不静音）。
 *
 * 纯函数（默认 profile/预设/校验/组统计/解析/错误归类）不依赖 Vue，Node 单测直接覆盖。
 */
import { ApiClient, ApiError } from './client.ts'

/** 音频 profile schema（docs/API.md §12.1）。 */
export const AUDIO_PROFILE_SCHEMA = 'khaen-audio-profile/v1'

/** 端点路径（mock/契约测试共用）。 */
export const AUDIO_PROFILE_PATHS = {
  profile: '/api/audio-profile',
  plan: '/api/audio-profile/plan',
  deviceInputs: '/api/device/inputs',
  speakerTest: '/api/speaker-test',
  speakerTestStop: '/api/speaker-test/stop',
  // 方案生成后走现有方案接口（不自动提交）
  planInspect: '/api/plan/inspect',
  planPrepare: '/api/prepare',
  planCommit: '/api/commit',
  planAbort: '/api/abort'
} as const

/** 请求超时（毫秒）：profile 读取/保存是管理操作，不轮询。 */
export const AUDIO_PROFILE_FETCH_TIMEOUT_MS = 10000
/** 测试音确认最长时长（毫秒）：服务端硬上限 10s，UI 上限 3s（用户可点立即停止）。 */
export const SPEAKER_TEST_MAX_MS = 10000
export const SPEAKER_TEST_UI_MAX_MS = 3000
export const SPEAKER_TEST_UI_MIN_MS = 500
/** 测试音默认参数（docs §12.6）：1s、-30 dBFS、带限粉噪、首尾 20ms 淡入淡出由服务端保证。 */
export const SPEAKER_TEST_DEFAULT_MS = 1000
export const SPEAKER_TEST_DEFAULT_DB = -30
export const SPEAKER_TEST_LEVEL_OPTIONS = [-40, -30, -20] as const
export const SPEAKER_TEST_HARD_MAX_DB = -12
export const SPEAKER_TEST_MIN_DB = -60
export type SpeakerTestKind = 'noise' | 'sine'

/** 设备通道数（MOTU 22；profile.device.channels 来自后端，缺省按 22 渲染）。 */
export const DEFAULT_DEVICE_CHANNELS = 22

/** 角色 canonical（19 个，顺序即方案 lane 排序：地面 → 低频 → 天空 → 其它）。 */
export const SPEAKER_ROLES = [
  'L',
  'R',
  'C',
  'Lw',
  'Rw',
  'Ls',
  'Rs',
  'Lb',
  'Rb',
  'LFE',
  'Sub',
  'Tfl',
  'Tfr',
  'Tml',
  'Tmr',
  'Trl',
  'Trr',
  'Custom',
  'None'
] as const
export type SpeakerRole = (typeof SPEAKER_ROLES)[number]

/** 角色校验：未知字符串不被接受（fail-closed，代码只用 canonical）。 */
export function isSpeakerRole(value: unknown): value is SpeakerRole {
  return typeof value === 'string' && (SPEAKER_ROLES as readonly string[]).includes(value)
}

/** 角色分组（UI 按组展示；canonical 顺序与上表一致）。 */
export const SPEAKER_ROLE_GROUPS = {
  ground: ['L', 'R', 'C', 'Lw', 'Rw', 'Ls', 'Rs', 'Lb', 'Rb'],
  low: ['LFE', 'Sub'],
  height: ['Tfl', 'Tfr', 'Tml', 'Tmr', 'Trl', 'Trr'],
  other: ['Custom']
} as const satisfies Record<string, readonly SpeakerRole[]>

/** 布局能力上限（9.1.6）：地面最多 9、天空最多 6；LFE/Sub 可多只；同角色可重复任意多只。 */
export const MAX_GROUND_ROLES = 9
export const MAX_HEIGHT_ROLES = 6

/** 一个物理输出（22 行固定）。 */
export interface AudioOutput {
  dac: number
  enabled: boolean
  role: SpeakerRole
  gain_db: number
  custom_label: string | null
}

/** 一个输入通道的角色标签（channel = 该源内的通道序号）。 */
export interface AudioInputChannel {
  channel: number
  role: SpeakerRole
}

/** 输入源：aes67（RTP）与 device（声卡采集，模拟+光口）。 */
export type AudioSourceKind = 'aes67' | 'device'

export interface AudioInputSource {
  kind: AudioSourceKind
  enabled: boolean
  gain_db: number
  channels: AudioInputChannel[]
  preset: string | null
}

/** 缺 C / 缺 LFE 的下混配置。 */
export interface AudioDownmix {
  center: { mode: 'auto' | 'none'; gain_db: number }
  lfe: { mode: 'auto' | 'none'; gain_db: number; include_heights: boolean }
}

export interface AudioFanout {
  mode: 'unity' | 'group'
}

/** 完整 profile（schema 见 AUDIO_PROFILE_SCHEMA）。 */
export interface AudioProfile {
  schema: string
  device: { channels: number }
  outputs: AudioOutput[]
  inputs: { sources: AudioInputSource[] }
  downmix: AudioDownmix
  fanout: AudioFanout
}

/** 校验问题（后端 errors/warnings 文案原样保留；前端本地校验也产出同一形状）。 */
export interface ProfileIssue {
  /** 稳定码（后端给的或本地归类）；没有则为 null。 */
  code: string | null
  /** 字段路径（如 outputs[3].role）；没有则为 null。 */
  path: string | null
  /** 人读说明（后端原样）。 */
  message: string
}

export interface ProfileValidation {
  ok: boolean
  errors: ProfileIssue[]
  warnings: ProfileIssue[]
}

/** GET /api/audio-profile 的 capability（后端未接线时整组 null + reason）。 */
export interface Aes67Capability {
  channels: number | null
  map: number[] | null
  channel_order: string | null
  channel_order_source: 'sdp' | 'manual' | 'none' | null
}

export interface DeviceInputChannel {
  index: number
  label: string | null
  kind: 'analog' | 'optical' | 'unknown'
}

export interface DeviceInputsCapability {
  available: boolean | null
  optical_mode: 'spdif' | 'adat' | null
  sample_rate: number | null
  channels: DeviceInputChannel[] | null
  source: string | null
  reason: string | null
}

export interface ProfileCapability {
  aes67: Aes67Capability
  device_inputs: DeviceInputsCapability
}

export interface PlanSummary {
  lanes: number | null
  enabled_outputs: number | null
  dac_channels: number | null
  graph_id: string | null
}

/** GET /api/audio-profile 归一化快照。 */
export interface AudioProfileSnapshot {
  schema: string | null
  /** null = profile 不可读/未保存（read-only 200 + reasons）。 */
  profile: AudioProfile | null
  validation: ProfileValidation
  capability: ProfileCapability
  plan: PlanSummary
  /** 字段路径 -> 缺失原因（后端 reasons/reason 归一化）。 */
  reasons: Record<string, string>
}

/* ------------------------------------------------------------------ 默认 profile / 预设 */

/** 启用输出（按 DAC 顺序）。 */
export function enabledOutputs(profile: AudioProfile): AudioOutput[] {
  return profile.outputs.filter((output) => output.enabled && output.role !== 'None')
}

/** 构造默认 2.0 profile（L=DAC0、R=DAC1，其余禁用；输入只启用 AES67 ch0=L/ch1=R）。 */
export function createDefaultProfile(deviceChannels: number = DEFAULT_DEVICE_CHANNELS): AudioProfile {
  const channels = Number.isInteger(deviceChannels) && deviceChannels > 0 ? deviceChannels : DEFAULT_DEVICE_CHANNELS
  const outputs: AudioOutput[] = []
  for (let dac = 0; dac < channels; dac += 1) {
    const role: SpeakerRole = dac === 0 ? 'L' : dac === 1 ? 'R' : 'None'
    outputs.push({ dac, enabled: dac < 2, role, gain_db: 0, custom_label: null })
  }
  return {
    schema: AUDIO_PROFILE_SCHEMA,
    device: { channels },
    outputs,
    inputs: {
      sources: [
        {
          kind: 'aes67',
          enabled: true,
          gain_db: 0,
          channels: [
            { channel: 0, role: 'L' },
            { channel: 1, role: 'R' }
          ],
          preset: 'stereo'
        },
        {
          kind: 'device',
          enabled: false,
          gain_db: 0,
          channels: [
            { channel: 0, role: 'None' },
            { channel: 1, role: 'None' }
          ],
          preset: null
        }
      ]
    },
    downmix: {
      center: { mode: 'auto', gain_db: -3 },
      lfe: { mode: 'auto', gain_db: -3, include_heights: false }
    },
    fanout: { mode: 'unity' }
  }
}

/** 快捷预设（仅填充器，不写入方案、不构成约束）。 */
export interface LayoutPreset {
  id: string
  /** 角色顺序（按建议 DAC 顺序，从 0 开始依次启用）。 */
  roles: SpeakerRole[]
}

export const LAYOUT_PRESETS: readonly LayoutPreset[] = [
  { id: '2.0', roles: ['L', 'R'] },
  { id: '5.1', roles: ['L', 'R', 'C', 'LFE', 'Ls', 'Rs'] },
  { id: '7.1', roles: ['L', 'R', 'C', 'LFE', 'Ls', 'Rs', 'Lb', 'Rb'] },
  { id: '5.1.2', roles: ['L', 'R', 'C', 'LFE', 'Ls', 'Rs', 'Tfl', 'Tfr'] },
  { id: '5.1.4', roles: ['L', 'R', 'C', 'LFE', 'Ls', 'Rs', 'Tfl', 'Tfr', 'Tml', 'Tmr'] },
  { id: '7.1.4', roles: ['L', 'R', 'C', 'LFE', 'Ls', 'Rs', 'Lb', 'Rb', 'Tfl', 'Tfr', 'Tml', 'Tmr'] },
  {
    id: '9.1.6',
    roles: ['L', 'R', 'C', 'Lw', 'Rw', 'Ls', 'Rs', 'Lb', 'Rb', 'LFE', 'Tfl', 'Tfr', 'Tml', 'Tmr', 'Trl', 'Trr']
  }
]

/** AES67 输入的布局预设（通道角色顺序；5.1/7.1 与 ST 2110-30 一致）。 */
export const AES67_PRESETS: readonly { id: string; roles: SpeakerRole[] }[] = [
  { id: 'stereo', roles: ['L', 'R'] },
  { id: '5.1', roles: ['L', 'R', 'C', 'LFE', 'Ls', 'Rs'] },
  { id: '7.1', roles: ['L', 'R', 'C', 'LFE', 'Ls', 'Rs', 'Lb', 'Rb'] },
  { id: '7.1.4', roles: ['L', 'R', 'C', 'LFE', 'Ls', 'Rs', 'Lb', 'Rb', 'Tfl', 'Tfr', 'Tml', 'Tmr'] },
  {
    id: '9.1.6',
    roles: ['L', 'R', 'C', 'Lw', 'Rw', 'Ls', 'Rs', 'Lb', 'Rb', 'LFE', 'Tfl', 'Tfr', 'Tml', 'Tmr', 'Trl', 'Trr']
  }
]

/** 用布局预设填充输出（第 0..N-1 个 DAC 启用并分配角色；其余禁用 role=None；保留 gain/custom_label 之外的旧值清空）。 */
export function applyLayoutPreset(profile: AudioProfile, presetId: string): AudioProfile {
  const preset = LAYOUT_PRESETS.find((item) => item.id === presetId)
  const channels = profile.device.channels > 0 ? profile.device.channels : DEFAULT_DEVICE_CHANNELS
  const outputs: AudioOutput[] = []
  for (let dac = 0; dac < channels; dac += 1) {
    const role = preset !== undefined && dac < preset.roles.length ? preset.roles[dac] : 'None'
    const previous = profile.outputs.find((output) => output.dac === dac)
    outputs.push({
      dac,
      enabled: role !== 'None',
      role,
      gain_db: previous?.gain_db ?? 0,
      custom_label: role === 'Custom' ? previous?.custom_label ?? null : null
    })
  }
  return { ...profile, outputs }
}

/** 用 AES67 预设填充某个输入源的角色（通道数由源实际通道列表决定；不足补 None）。 */
export function applyInputPreset(source: AudioInputSource, presetId: string): AudioInputSource {
  const preset = AES67_PRESETS.find((item) => item.id === presetId)
  if (preset === undefined) {
    return source
  }
  const channels: AudioInputChannel[] = []
  for (let channel = 0; channel < source.channels.length; channel += 1) {
    channels.push({ channel, role: preset.roles[channel] ?? 'None' })
  }
  return { ...source, preset: presetId, channels }
}

/* ------------------------------------------------------------------ 本地校验（advisory） */

/** 结构校验：启用但角色 None / DAC 越界 / 全部禁用 / Custom 无名称（docs §12.1）。 */
export function validateProfileLocally(profile: AudioProfile): ProfileValidation {
  const errors: ProfileIssue[] = []
  const warnings: ProfileIssue[] = []
  const channels = profile.device.channels
  if (!Number.isInteger(channels) || channels <= 0) {
    errors.push({ code: 'bad_device_channels', path: 'device.channels', message: 'device.channels must be a positive integer' })
  }
  const seen = new Set<number>()
  for (const output of profile.outputs) {
    const path = 'outputs[' + String(output.dac) + ']'
    if (!Number.isInteger(output.dac) || output.dac < 0 || (channels > 0 && output.dac >= channels)) {
      errors.push({ code: 'dac_out_of_range', path, message: 'DAC ' + String(output.dac) + ' is outside 0..' + String(channels - 1) })
      continue
    }
    if (seen.has(output.dac)) {
      errors.push({ code: 'duplicate_dac', path, message: 'DAC ' + String(output.dac) + ' appears more than once' })
    }
    seen.add(output.dac)
    if (!isSpeakerRole(output.role)) {
      errors.push({ code: 'bad_role', path: path + '.role', message: 'unknown role ' + String(output.role) })
      continue
    }
    if (output.enabled && output.role === 'None') {
      errors.push({ code: 'enabled_without_role', path, message: 'an enabled output must have a role (not None)' })
    }
    if (output.role === 'Custom' && (output.custom_label === null || output.custom_label.trim().length === 0)) {
      errors.push({ code: 'custom_without_label', path: path + '.custom_label', message: 'Custom roles need a custom_label' })
    }
    if (!output.enabled && output.role !== 'None') {
      warnings.push({ code: 'disabled_with_role', path, message: 'a disabled output keeps its role label; it will not produce a lane' })
    }
  }
  if (profile.outputs.length > 0 && enabledOutputs(profile).length === 0) {
    errors.push({ code: 'no_enabled_outputs', path: 'outputs', message: 'all physical outputs are disabled: at least one speaker must be enabled' })
  }
  const enabledSources = profile.inputs.sources.filter((source) => source.enabled)
  if (enabledSources.length === 0) {
    warnings.push({ code: 'no_input_source', path: 'inputs.sources', message: 'no input source is enabled: the plan will carry no content' })
  }
  for (const source of enabledSources) {
    for (const channel of source.channels) {
      if (!isSpeakerRole(channel.role)) {
        errors.push({
          code: 'bad_input_role',
          path: 'inputs.sources[' + source.kind + '].channels[' + String(channel.channel) + ']',
          message: 'unknown input role ' + String(channel.role)
        })
      }
    }
  }
  // 缺少角色只做信息提示（不是错误）：缺 C → C 输入下混到 L/R；缺 LFE/Sub → LFE 下混到其它。
  const hints = buildDownmixHints(profile)
  if (hints.missingCenter) {
    warnings.push({
      code: 'missing_center',
      path: 'outputs',
      message: 'no C output: C input will be downmixed to L/R at ' + String(hints.centerGainDb) + ' dB each'
    })
  }
  if (hints.missingLfe) {
    warnings.push({
      code: 'missing_lfe',
      path: 'outputs',
      message:
        'no LFE/Sub output: LFE input will be distributed to ' + String(hints.lfeTargetCount) + ' speaker(s) at ' +
        String(hints.lfeGainDb) + ' dB each' + (hints.includeHeights ? ' (including heights)' : ' (heights excluded)')
    })
  }
  return { ok: errors.length === 0, errors, warnings }
}

/** 输入角色 -> 输出角色组的自动映射预览（fan-out 语义：同角色求和）。 */
export interface RoleFanoutRow {
  role: SpeakerRole
  /** 标注该角色的已启用输入通道（source/channel）。 */
  inputs: Array<{ source: AudioSourceKind; channel: number }>
  /** 该角色组的输出 DAC（升序）。 */
  dacs: number[]
}

/** 构造「输入角色 → 输出角色组」映射（缺 C 下混单独在 downmix hints 展示）。 */
export function buildRoleFanout(profile: AudioProfile): RoleFanoutRow[] {
  const outputsByRole = new Map<SpeakerRole, number[]>()
  for (const output of enabledOutputs(profile)) {
    const list = outputsByRole.get(output.role) ?? []
    list.push(output.dac)
    outputsByRole.set(output.role, list)
  }
  const rows: RoleFanoutRow[] = []
  for (const role of SPEAKER_ROLES) {
    if (role === 'None' || role === 'Custom') {
      continue
    }
    const inputs: Array<{ source: AudioSourceKind; channel: number }> = []
    for (const source of profile.inputs.sources) {
      if (!source.enabled) {
        continue
      }
      for (const channel of source.channels) {
        if (channel.role === role) {
          inputs.push({ source: source.kind, channel: channel.channel })
        }
      }
    }
    const dacs = (outputsByRole.get(role) ?? []).slice().sort((a, b) => a - b)
    if (inputs.length > 0 || dacs.length > 0) {
      rows.push({ role, inputs, dacs })
    }
  }
  return rows
}

/** 下混提示（缺 C / 缺 LFE；不改变 profile，只用于 UI 提示与方案生成预览）。 */
export interface DownmixHints {
  missingCenter: boolean
  missingLfe: boolean
  centerGainDb: number
  lfeGainDb: number
  includeHeights: boolean
  /** LFE 下混的目标音箱数（不含被排除的天空时也照实给）。 */
  lfeTargetCount: number
  /** LFE 下混目标 DAC（升序）。 */
  lfeTargetDacs: number[]
}

export function buildDownmixHints(profile: AudioProfile): DownmixHints {
  const outputs = enabledOutputs(profile)
  const hasCenter = outputs.some((output) => output.role === 'C')
  const hasLfe = outputs.some((output) => output.role === 'LFE' || output.role === 'Sub')
  const heights = new Set<SpeakerRole>(SPEAKER_ROLE_GROUPS.height)
  const includeHeights = profile.downmix.lfe.include_heights
  const lfeTargets = outputs.filter((output) => !includeHeights || !heights.has(output.role))
  return {
    missingCenter: !hasCenter,
    missingLfe: !hasLfe,
    centerGainDb: profile.downmix.center.gain_db,
    lfeGainDb: profile.downmix.lfe.gain_db,
    includeHeights,
    lfeTargetCount: lfeTargets.length,
    lfeTargetDacs: lfeTargets.map((output) => output.dac).sort((a, b) => a - b)
  }
}

/** 角色组统计（重复角色合法；显示如 L ×2 / LFE ×3）。 */
export interface RoleGroupStat {
  role: SpeakerRole
  count: number
  dacs: number[]
}

export function summarizeRoleGroups(profile: AudioProfile): RoleGroupStat[] {
  const byRoles = new Map<SpeakerRole, number[]>()
  for (const output of enabledOutputs(profile)) {
    const list = byRoles.get(output.role) ?? []
    list.push(output.dac)
    byRoles.set(output.role, list)
  }
  const stats: RoleGroupStat[] = []
  for (const role of SPEAKER_ROLES) {
    const dacs = byRoles.get(role)
    if (dacs === undefined) {
      continue
    }
    stats.push({ role, count: dacs.length, dacs: dacs.slice().sort((a, b) => a - b) })
  }
  return stats
}

/** 派生布局标签：命中标准预设 → 预设 id；否则 custom 摘要（地面/天空/低音炮计数）。 */
export function deriveLayoutLabel(profile: AudioProfile): string {
  const roles = enabledOutputs(profile).map((output) => output.role)
  const multiset = new Map<SpeakerRole, number>()
  for (const role of roles) {
    multiset.set(role, (multiset.get(role) ?? 0) + 1)
  }
  for (const preset of LAYOUT_PRESETS) {
    if (preset.roles.length !== roles.length) {
      continue
    }
    const presetCount = new Map<SpeakerRole, number>()
    for (const role of preset.roles) {
      presetCount.set(role, (presetCount.get(role) ?? 0) + 1)
    }
    let match = true
    for (const [role, count] of presetCount) {
      if ((multiset.get(role) ?? 0) !== count) {
        match = false
        break
      }
    }
    if (match) {
      return preset.id
    }
  }
  let ground = 0
  let height = 0
  let low = 0
  for (const role of roles) {
    if ((SPEAKER_ROLE_GROUPS.ground as readonly SpeakerRole[]).includes(role)) {
      ground += 1
    } else if ((SPEAKER_ROLE_GROUPS.height as readonly SpeakerRole[]).includes(role)) {
      height += 1
    } else if ((SPEAKER_ROLE_GROUPS.low as readonly SpeakerRole[]).includes(role)) {
      low += 1
    }
  }
  return 'custom:' + String(ground) + ':' + String(height) + ':' + String(low)
}

/** 未使用 DAC 列表（保持无信号；不做任何 CueMix 写入）。 */
export function unusedDacs(profile: AudioProfile): number[] {
  return profile.outputs.filter((output) => !output.enabled || output.role === 'None').map((output) => output.dac).sort((a, b) => a - b)
}

/* ------------------------------------------------------------------ 运行时校验 / 解析 */

function invalidResponse(endpoint: string, detail: string): ApiError {
  return new ApiError({ status: 200, code: 'invalid_response', message: 'unexpected response from ' + endpoint + ': ' + detail })
}

function asRecord(value: unknown, path: string): Record<string, unknown> | null {
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'object' || Array.isArray(value)) {
    throw invalidResponse(path, 'must be an object or null')
  }
  return value as Record<string, unknown>
}

function readString(record: Record<string, unknown>, field: string, path: string): string | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'string') {
    throw invalidResponse(path, field + ' must be a string or null')
  }
  return value
}

function readNumber(record: Record<string, unknown>, field: string, path: string): number | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw invalidResponse(path, field + ' must be a finite number or null')
  }
  return value
}

function readBoolean(record: Record<string, unknown>, field: string, path: string): boolean | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (typeof value !== 'boolean') {
    throw invalidResponse(path, field + ' must be a boolean or null')
  }
  return value
}

function readNumberArray(record: Record<string, unknown>, field: string, path: string): number[] | null {
  const value = record[field]
  if (value === null || value === undefined) {
    return null
  }
  if (!Array.isArray(value)) {
    throw invalidResponse(path, field + ' must be an array or null')
  }
  return value.map((item) => {
    if (typeof item !== 'number' || !Number.isFinite(item)) {
      throw invalidResponse(path, field + ' entries must be finite numbers')
    }
    return item
  })
}

function readRole(record: Record<string, unknown>, field: string, path: string): SpeakerRole {
  const value = readString(record, field, path)
  if (value === null) {
    return 'None'
  }
  if (!isSpeakerRole(value)) {
    throw invalidResponse(path, field + ' must be a canonical role')
  }
  return value
}

export function parseAudioOutput(value: unknown, index: number): AudioOutput {
  const path = 'outputs[' + String(index) + ']'
  const record = asRecord(value, path)
  if (record === null) {
    throw invalidResponse(path, 'must be an object')
  }
  const dac = readNumber(record, 'dac', path)
  const enabled = readBoolean(record, 'enabled', path)
  if (dac === null || enabled === null) {
    throw invalidResponse(path, 'dac/enabled are required')
  }
  return {
    dac,
    enabled,
    role: readRole(record, 'role', path),
    gain_db: readNumber(record, 'gain_db', path) ?? 0,
    custom_label: readString(record, 'custom_label', path)
  }
}

export function parseAudioSource(value: unknown, index: number): AudioInputSource {
  const path = 'inputs.sources[' + String(index) + ']'
  const record = asRecord(value, path)
  if (record === null) {
    throw invalidResponse(path, 'must be an object')
  }
  const kindValue = readString(record, 'kind', path)
  const kind: AudioSourceKind = kindValue === 'device' ? 'device' : 'aes67'
  const channels: AudioInputChannel[] = []
  const channelValues = record.channels
  if (Array.isArray(channelValues)) {
    channelValues.forEach((item, channelIndex) => {
      const channelPath = path + '.channels[' + String(channelIndex) + ']'
      const channelRecord = asRecord(item, channelPath)
      if (channelRecord === null) {
        throw invalidResponse(channelPath, 'must be an object')
      }
      const channel = readNumber(channelRecord, 'channel', channelPath)
      if (channel === null) {
        throw invalidResponse(channelPath, 'channel is required')
      }
      channels.push({ channel, role: readRole(channelRecord, 'role', channelPath) })
    })
  }
  return {
    kind,
    enabled: readBoolean(record, 'enabled', path) ?? false,
    gain_db: readNumber(record, 'gain_db', path) ?? 0,
    channels,
    preset: readString(record, 'preset', path)
  }
}

export function parseAudioProfile(value: unknown): AudioProfile {
  const record = asRecord(value, 'profile')
  if (record === null) {
    throw invalidResponse('profile', 'must be an object')
  }
  const device = asRecord(record.device, 'profile.device') ?? {}
  const channels = readNumber(device, 'channels', 'profile.device')
  const outputsValue = record.outputs
  const outputs: AudioOutput[] = Array.isArray(outputsValue)
    ? outputsValue.map((item, index) => parseAudioOutput(item, index))
    : []
  const inputs = asRecord(record.inputs, 'profile.inputs') ?? {}
  const sourcesValue = inputs.sources
  const sources: AudioInputSource[] = Array.isArray(sourcesValue)
    ? sourcesValue.map((item, index) => parseAudioSource(item, index))
    : []
  const downmix = asRecord(record.downmix, 'profile.downmix') ?? {}
  const center = asRecord(downmix.center, 'profile.downmix.center') ?? {}
  const lfe = asRecord(downmix.lfe, 'profile.downmix.lfe') ?? {}
  const fanout = asRecord(record.fanout, 'profile.fanout') ?? {}
  const fanoutMode = readString(fanout, 'mode', 'profile.fanout')
  return {
    schema: readString(record, 'schema', 'profile') ?? AUDIO_PROFILE_SCHEMA,
    device: { channels: channels ?? DEFAULT_DEVICE_CHANNELS },
    outputs,
    inputs: { sources },
    downmix: {
      center: {
        mode: readString(center, 'mode', 'profile.downmix.center') === 'none' ? 'none' : 'auto',
        gain_db: readNumber(center, 'gain_db', 'profile.downmix.center') ?? -3
      },
      lfe: {
        mode: readString(lfe, 'mode', 'profile.downmix.lfe') === 'none' ? 'none' : 'auto',
        gain_db: readNumber(lfe, 'gain_db', 'profile.downmix.lfe') ?? -3,
        include_heights: readBoolean(lfe, 'include_heights', 'profile.downmix.lfe') ?? false
      }
    },
    fanout: { mode: fanoutMode === 'group' ? 'group' : 'unity' }
  }
}

/** 解析 errors/warnings 数组：字符串或 {code,path,message} 两种形态都接受。 */
export function parseIssues(value: unknown): ProfileIssue[] {
  if (value === null || value === undefined) {
    return []
  }
  if (!Array.isArray(value)) {
    throw invalidResponse('validation', 'errors/warnings must be arrays or null')
  }
  return value.map((item, index) => {
    if (typeof item === 'string') {
      return { code: null, path: null, message: item }
    }
    const record = asRecord(item, 'validation[' + String(index) + ']')
    if (record === null) {
      throw invalidResponse('validation[' + String(index) + ']', 'must be a string or object')
    }
    return {
      code: readString(record, 'code', 'validation'),
      path: readString(record, 'path', 'validation'),
      message: readString(record, 'message', 'validation') ?? readString(record, 'detail', 'validation') ?? ''
    }
  })
}

export function parseValidation(value: unknown): ProfileValidation {
  const record = asRecord(value, 'validation')
  const errors = parseIssues(record === null ? null : record.errors)
  const warnings = parseIssues(record === null ? null : record.warnings)
  const okValue = record === null ? null : readBoolean(record, 'ok', 'validation')
  return { ok: okValue ?? errors.length === 0, errors, warnings }
}

function parseReasons(value: unknown): Record<string, string> {
  const record = asRecord(value, 'reasons')
  if (record === null) {
    return {}
  }
  const reasons: Record<string, string> = {}
  for (const [key, item] of Object.entries(record)) {
    if (typeof item === 'string') {
      reasons[key] = item
    }
  }
  return reasons
}

function parseAes67Capability(value: unknown): Aes67Capability {
  const record = asRecord(value, 'capability.aes67') ?? {}
  const source = readString(record, 'channel_order_source', 'capability.aes67')
  const orderSource: Aes67Capability['channel_order_source'] =
    source === 'sdp' ? 'sdp' : source === 'manual' ? 'manual' : source === 'none' ? 'none' : null
  return {
    channels: readNumber(record, 'channels', 'capability.aes67'),
    map: readNumberArray(record, 'map', 'capability.aes67'),
    channel_order: readString(record, 'channel_order', 'capability.aes67'),
    channel_order_source: orderSource
  }
}

export function parseDeviceInputs(value: unknown): DeviceInputsCapability {
  const record = asRecord(value, 'device_inputs') ?? {}
  const optical = readString(record, 'optical_mode', 'device_inputs')
  const channels: DeviceInputChannel[] | null = Array.isArray(record.channels)
    ? record.channels.map((item, index) => {
        const path = 'device_inputs.channels[' + String(index) + ']'
        const channelRecord = asRecord(item, path)
        if (channelRecord === null) {
          throw invalidResponse(path, 'must be an object')
        }
        const kindValue = readString(channelRecord, 'kind', path)
        return {
          index: readNumber(channelRecord, 'index', path) ?? index,
          label: readString(channelRecord, 'label', path),
          kind: kindValue === 'optical' ? 'optical' : kindValue === 'analog' ? 'analog' : 'unknown'
        }
      })
    : null
  return {
    available: readBoolean(record, 'available', 'device_inputs'),
    optical_mode: optical === 'spdif' || optical === 'adat' ? optical : null,
    sample_rate: readNumber(record, 'sample_rate', 'device_inputs'),
    channels,
    source: readString(record, 'source', 'device_inputs'),
    reason: readString(record, 'reason', 'device_inputs')
  }
}

/** 解析 GET /api/audio-profile（后端未接线时 profile/capability 为 null + reasons）。 */
export function parseAudioProfileSnapshot(body: unknown): AudioProfileSnapshot {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('body', 'must be an object')
  }
  const capability = asRecord(record.capability, 'capability')
  const plan = asRecord(record.plan, 'plan')
  const reasons = parseReasons(record.reasons)
  // 后端未接线：{ok:false?} / profile=null + reason 都是诚实回执；有 reason 时把通用原因放 reasons 兜底。
  const genericReason = readString(record, 'reason', 'body')
  if (genericReason !== null && Object.keys(reasons).length === 0) {
    reasons['reason'] = genericReason
  }
  return {
    schema: readString(record, 'schema', 'body'),
    profile: record.profile === null || record.profile === undefined ? null : parseAudioProfile(record.profile),
    validation: parseValidation(record.validation),
    capability: {
      aes67: parseAes67Capability(capability === null ? null : capability.aes67),
      device_inputs: parseDeviceInputs(capability === null ? null : capability.device_inputs)
    },
    plan: {
      lanes: plan === null ? null : readNumber(plan, 'lanes', 'plan'),
      enabled_outputs: plan === null ? null : readNumber(plan, 'enabled_outputs', 'plan'),
      dac_channels: plan === null ? null : readNumber(plan, 'dac_channels', 'plan'),
      graph_id: plan === null ? null : readString(plan, 'graph_id', 'plan')
    },
    reasons
  }
}

/** PUT prepare 回执（草稿）。 */
export interface AudioProfileDraft {
  stage: 'prepare'
  token: string
  profile: AudioProfile
  validation: ProfileValidation
  diff: { outputs_changed: boolean | null; inputs_changed: boolean | null; downmix_changed: boolean | null; fanout_changed: boolean | null }
  expires_in_s: number | null
}

export function parsePrepareResponse(body: unknown): AudioProfileDraft {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('body', 'must be an object')
  }
  const token = readString(record, 'token', 'body') ?? readString(record, 'engine_token', 'body')
  if (token === null || token.length === 0) {
    throw invalidResponse('body', 'prepare must return a non-empty token')
  }
  const diff = asRecord(record.diff, 'diff') ?? {}
  return {
    stage: 'prepare',
    token,
    profile: parseAudioProfile(record.profile),
    validation: parseValidation(record.validation),
    diff: {
      outputs_changed: readBoolean(diff, 'outputs_changed', 'diff'),
      inputs_changed: readBoolean(diff, 'inputs_changed', 'diff'),
      downmix_changed: readBoolean(diff, 'downmix_changed', 'diff'),
      fanout_changed: readBoolean(diff, 'fanout_changed', 'diff')
    },
    expires_in_s: readNumber(record, 'expires_in_s', 'body')
  }
}

export interface AudioProfileCommitAck {
  ok: boolean
  applied: boolean
  saved: boolean
  revision: number | null
  profile: AudioProfile | null
}

export function parseCommitResponse(body: unknown): AudioProfileCommitAck {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('body', 'must be an object')
  }
  return {
    ok: readBoolean(record, 'ok', 'body') ?? true,
    applied: readBoolean(record, 'applied', 'body') ?? false,
    saved: readBoolean(record, 'saved', 'body') ?? false,
    revision: readNumber(record, 'revision', 'body'),
    profile: record.profile === null || record.profile === undefined ? null : parseAudioProfile(record.profile)
  }
}

export interface AudioProfileAbortAck {
  ok: boolean
  aborted: boolean
}

export function parseAbortResponse(body: unknown): AudioProfileAbortAck {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('body', 'must be an object')
  }
  return {
    ok: readBoolean(record, 'ok', 'body') ?? true,
    aborted: readBoolean(record, 'aborted', 'body') === true
  }
}

/** POST /api/audio-profile/plan 回执（生成方案但不提交）。 */
export interface GeneratedPlan {
  ok: boolean
  plan_path: string
  sha256: string
  lane_count: number | null
  inspect: unknown
  folds_preview: Array<{ source: string; channel: number | null; role: SpeakerRole | null; lane: number | null }>
  downmix_applied: Array<{ kind: 'center' | 'lfe'; gain_db: number | null; lanes: number[] }>
}

export function parseGeneratedPlan(body: unknown): GeneratedPlan {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('body', 'must be an object')
  }
  const planPath = readString(record, 'plan_path', 'body')
  const sha = readString(record, 'sha256', 'body')
  if (planPath === null || planPath.length === 0) {
    throw invalidResponse('body', 'plan_path must be a non-empty string')
  }
  if (sha === null || sha.length === 0) {
    throw invalidResponse('body', 'sha256 must be a non-empty string')
  }
  const foldsPreview = Array.isArray(record.folds_preview)
    ? record.folds_preview.map((item, index) => {
        const path = 'folds_preview[' + String(index) + ']'
        const entry = asRecord(item, path) ?? {}
        const roleValue = readString(entry, 'role', path)
        return {
          source: readString(entry, 'source', path) ?? '',
          channel: readNumber(entry, 'channel', path),
          role: roleValue !== null && isSpeakerRole(roleValue) ? roleValue : null,
          lane: readNumber(entry, 'lane', path)
        }
      })
    : []
  const downmix = Array.isArray(record.downmix_applied)
    ? record.downmix_applied.map((item, index) => {
        const path = 'downmix_applied[' + String(index) + ']'
        const entry = asRecord(item, path) ?? {}
        const kindValue = readString(entry, 'kind', path)
        return {
          kind: (kindValue === 'center' ? 'center' : 'lfe') as 'center' | 'lfe',
          gain_db: readNumber(entry, 'gain_db', path),
          lanes: readNumberArray(entry, 'lanes', path) ?? []
        }
      })
    : []
  return {
    ok: readBoolean(record, 'ok', 'body') ?? true,
    plan_path: planPath,
    sha256: sha,
    lane_count: readNumber(record, 'lane_count', 'body'),
    inspect: record.inspect ?? null,
    folds_preview: foldsPreview,
    downmix_applied: downmix
  }
}

/** POST /api/speaker-test 回执。 */
export interface SpeakerTestAck {
  ok: boolean
  dac: number
  ms: number
  kind: SpeakerTestKind
  level_db: number | null
  remaining_ms: number | null
}

export function parseSpeakerTestAck(body: unknown): SpeakerTestAck {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('body', 'must be an object')
  }
  const dac = readNumber(record, 'dac', 'body')
  const ms = readNumber(record, 'ms', 'body')
  if (dac === null || ms === null) {
    throw invalidResponse('body', 'speaker-test must echo dac/ms')
  }
  return {
    ok: readBoolean(record, 'ok', 'body') ?? true,
    dac,
    ms,
    kind: readString(record, 'kind', 'body') === 'sine' ? 'sine' : 'noise',
    level_db: readNumber(record, 'level_db', 'body'),
    remaining_ms: readNumber(record, 'remaining_ms', 'body')
  }
}

/** POST /api/speaker-test/stop 回执（幂等）。 */
export function parseSpeakerTestStopAck(body: unknown): { ok: boolean; stopped: boolean } {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('body', 'must be an object')
  }
  return {
    ok: readBoolean(record, 'ok', 'body') ?? true,
    stopped: readBoolean(record, 'stopped', 'body') === true
  }
}

/* ------------------------------------------------------------------ 端点封装 */

export interface SpeakerTestRequest {
  dac: number
  /** 毫秒；UI 上限 3s，服务端硬上限 10s。 */
  ms: number
  kind: SpeakerTestKind
  /** dBFS；默认 -30，可选 -40/-30/-20，服务端硬上限 -12、下限 -60。 */
  db: number
  /** 必须显式 true（防误触自动出声）。 */
  audible: true
  /** 必须显式 true（用户在确认框确认过）。 */
  confirm: true
}

export interface PreparePlanDraft {
  engine_token: string
  planned_version: number | null
  graph_id: string | null
}

export interface CommitPlanAck {
  version: number | null
  graph_id: string | null
  applied_block: number | null
}

export interface AudioProfileApi {
  fetchProfile(options?: { signal?: AbortSignal }): Promise<AudioProfileSnapshot>
  fetchDeviceInputs(options?: { signal?: AbortSignal }): Promise<DeviceInputsCapability>
  prepareProfile(profile: AudioProfile): Promise<AudioProfileDraft>
  commitProfile(token: string): Promise<AudioProfileCommitAck>
  abortProfile(token: string): Promise<AudioProfileAbortAck>
  generatePlan(): Promise<GeneratedPlan>
  inspectPlan(planPath: string, planSha256: string): Promise<unknown>
  preparePlan(planPath: string, planSha256: string): Promise<PreparePlanDraft>
  commitPlan(engineToken: string): Promise<CommitPlanAck>
  abortPlan(engineToken: string): Promise<{ ok: boolean; aborted: boolean }>
  startSpeakerTest(request: SpeakerTestRequest): Promise<SpeakerTestAck>
  stopSpeakerTest(): Promise<{ ok: boolean; stopped: boolean }>
}

function parsePreparePlanDraft(body: unknown): PreparePlanDraft {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('/api/prepare', 'body must be an object')
  }
  const token = readString(record, 'engine_token', 'body')
  if (token === null || token.length === 0) {
    throw invalidResponse('/api/prepare', 'engine_token must be a non-empty string')
  }
  return {
    engine_token: token,
    planned_version: readNumber(record, 'planned_version', 'body'),
    graph_id: readString(record, 'graph_id', 'body')
  }
}

function parseCommitPlanAck(body: unknown): CommitPlanAck {
  const record = asRecord(body, 'body')
  if (record === null) {
    throw invalidResponse('/api/commit', 'body must be an object')
  }
  return {
    version: readNumber(record, 'version', 'body'),
    graph_id: readString(record, 'graph_id', 'body'),
    applied_block: readNumber(record, 'applied_block', 'body')
  }
}

/** 用共享 ApiClient 构造音频 profile API（单测注入 fetchImpl 即可脱离网络）。 */
export function createAudioProfileApi(client: ApiClient): AudioProfileApi {
  return {
    async fetchProfile(options = {}) {
      const body = await client.get<unknown>(AUDIO_PROFILE_PATHS.profile, {
        signal: options.signal,
        timeoutMs: AUDIO_PROFILE_FETCH_TIMEOUT_MS
      })
      return parseAudioProfileSnapshot(body)
    },
    async fetchDeviceInputs(options = {}) {
      const body = await client.get<unknown>(AUDIO_PROFILE_PATHS.deviceInputs, {
        signal: options.signal,
        timeoutMs: AUDIO_PROFILE_FETCH_TIMEOUT_MS
      })
      const record = asRecord(body, 'body')
      if (record === null) {
        throw invalidResponse(AUDIO_PROFILE_PATHS.deviceInputs, 'body must be an object')
      }
      return parseDeviceInputs(record)
    },
    async prepareProfile(profile) {
      const body = await client.put<unknown>(AUDIO_PROFILE_PATHS.profile, { stage: 'prepare', profile })
      return parsePrepareResponse(body)
    },
    async commitProfile(token) {
      const body = await client.put<unknown>(AUDIO_PROFILE_PATHS.profile, { stage: 'commit', token })
      return parseCommitResponse(body)
    },
    async abortProfile(token) {
      const body = await client.put<unknown>(AUDIO_PROFILE_PATHS.profile, { stage: 'abort', token })
      return parseAbortResponse(body)
    },
    async generatePlan() {
      const body = await client.post<unknown>(AUDIO_PROFILE_PATHS.plan, {})
      return parseGeneratedPlan(body)
    },
    async inspectPlan(planPath, planSha256) {
      return await client.post<unknown>(AUDIO_PROFILE_PATHS.planInspect, { plan_path: planPath, plan_sha256: planSha256 })
    },
    async preparePlan(planPath, planSha256) {
      const body = await client.post<unknown>(AUDIO_PROFILE_PATHS.planPrepare, { plan_path: planPath, plan_sha256: planSha256 })
      return parsePreparePlanDraft(body)
    },
    async commitPlan(engineToken) {
      const body = await client.post<unknown>(AUDIO_PROFILE_PATHS.planCommit, { engine_token: engineToken })
      return parseCommitPlanAck(body)
    },
    async abortPlan(engineToken) {
      const body = await client.post<unknown>(AUDIO_PROFILE_PATHS.planAbort, { engine_token: engineToken })
      const record = asRecord(body, 'body') ?? {}
      return { ok: readBoolean(record, 'ok', 'body') ?? true, aborted: readBoolean(record, 'aborted', 'body') === true }
    },
    async startSpeakerTest(request) {
      const body = await client.post<unknown>(AUDIO_PROFILE_PATHS.speakerTest, { ...request })
      return parseSpeakerTestAck(body)
    },
    async stopSpeakerTest() {
      const body = await client.post<unknown>(AUDIO_PROFILE_PATHS.speakerTestStop, {})
      return parseSpeakerTestStopAck(body)
    }
  }
}

/* ------------------------------------------------------------------ 测试音参数校验 */

export type SpeakerTestValidationKey = 'dac' | 'ms' | 'db' | 'confirm' | 'busy'

/** 测试音参数校验（前端快速反馈；服务端仍是权威）。 */
export function validateSpeakerTestRequest(request: Partial<SpeakerTestRequest>): SpeakerTestValidationKey | null {
  if (request.audible !== true || request.confirm !== true) {
    return 'confirm'
  }
  if (typeof request.dac !== 'number' || !Number.isInteger(request.dac) || request.dac < 0) {
    return 'dac'
  }
  if (typeof request.ms !== 'number' || !Number.isFinite(request.ms)) {
    return 'ms'
  }
  if (request.ms < SPEAKER_TEST_UI_MIN_MS || request.ms > SPEAKER_TEST_MAX_MS) {
    return 'ms'
  }
  if (request.db !== undefined) {
    if (typeof request.db !== 'number' || !Number.isFinite(request.db)) {
      return 'db'
    }
    if (request.db < SPEAKER_TEST_MIN_DB || request.db > SPEAKER_TEST_HARD_MAX_DB) {
      return 'db'
    }
  }
  return null
}

/* ------------------------------------------------------------------ 错误归类 */

export type AudioProfileErrorCategory =
  | 'network'
  | 'timeout'
  | 'unauthorized'
  | 'not_wired'
  | 'unsupported'
  | 'busy'
  | 'sound_window_closed'
  | 'validation'
  | 'admission_rejected'
  | 'token_invalid'
  | 'conflict'
  | 'not_found'
  | 'server'
  | 'invalid_response'
  | 'unknown'

export interface AudioProfileErrorInfo {
  category: AudioProfileErrorCategory
  status: number
  code: string
  /** 主文案 i18n 键（speakerLayout.error.*）。 */
  i18nKey: string
  detail: string | null
  /** 后端 details.reason（如有）。 */
  reason: string | null
  retryable: boolean
}

const AUDIO_PROFILE_ERROR_KEYS: Record<AudioProfileErrorCategory, string> = {
  network: 'speakerLayout.error.network',
  timeout: 'speakerLayout.error.timeout',
  unauthorized: 'speakerLayout.error.unauthorized',
  not_wired: 'speakerLayout.error.notWired',
  unsupported: 'speakerLayout.error.unsupported',
  busy: 'speakerLayout.error.busy',
  sound_window_closed: 'speakerLayout.error.soundWindowClosed',
  validation: 'speakerLayout.error.validation',
  admission_rejected: 'speakerLayout.error.admissionRejected',
  token_invalid: 'speakerLayout.error.tokenInvalid',
  conflict: 'speakerLayout.error.conflict',
  not_found: 'speakerLayout.error.notFound',
  server: 'speakerLayout.error.server',
  invalid_response: 'speakerLayout.error.invalidResponse',
  unknown: 'speakerLayout.error.unknown'
}

function detailReason(details: unknown): string | null {
  if (typeof details !== 'object' || details === null || Array.isArray(details)) {
    return null
  }
  const record = details as Record<string, unknown>
  if (typeof record.reason === 'string' && record.reason.length > 0) {
    return record.reason
  }
  return null
}

export function classifyAudioProfileError(error: unknown): AudioProfileErrorInfo {
  if (!(error instanceof ApiError)) {
    const detail = error instanceof Error ? error.message : null
    return {
      category: 'unknown',
      status: 0,
      code: 'unknown_error',
      i18nKey: AUDIO_PROFILE_ERROR_KEYS.unknown,
      detail,
      reason: null,
      retryable: true
    }
  }
  let category: AudioProfileErrorCategory
  if (error.code === 'request_timeout') {
    category = 'timeout'
  } else if (error.code === 'network_error' || error.status === 0) {
    category = 'network'
  } else if (error.status === 401) {
    category = 'unauthorized'
  } else if (error.code === 'invalid_response') {
    category = 'invalid_response'
  } else if (error.code === 'sound_window_closed' || error.status === 403) {
    category = 'sound_window_closed'
  } else if (error.code === 'not_wired') {
    category = 'not_wired'
  } else if (error.code === 'unsupported' || error.code === 'not_implemented') {
    category = 'unsupported'
  } else if (error.code === 'busy' || error.status === 409) {
    category = (error.code === 'admission_rejected' ? 'admission_rejected' : error.code === 'engine_token_invalid' ? 'token_invalid' : 'busy')
  } else if (error.status === 400 || error.status === 422) {
    category = 'validation'
  } else if (error.status === 404) {
    category = 'not_found'
  } else if (error.status >= 500) {
    category = 'server'
  } else {
    category = 'conflict'
  }
  return {
    category,
    status: error.status,
    code: error.code,
    i18nKey: AUDIO_PROFILE_ERROR_KEYS[category],
    detail: error.message.length > 0 ? error.message : null,
    reason: detailReason(error.details),
    retryable: category === 'network' || category === 'timeout' || category === 'server' || category === 'busy'
  }
}
