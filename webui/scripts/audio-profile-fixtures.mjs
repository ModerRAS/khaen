/**
 * 音频通道 profile / Speaker Layout 契约 fixture（DEV / TEST ONLY；scripts/ 不进 dist）。
 *
 * 契约来源：docs/API.md v1.2 §12（音频通道 profile 合同）+ docs/SPEAKER-MAPPING.md。
 *
 * 覆盖：
 * - GET  /api/audio-profile      快照（model/capability/plan/reasons；不可用 = profile:null + reasons）
 * - PUT  /api/audio-profile      stage=prepare（校验 + diff + token）| commit（保存 + revision）| abort
 * - POST /api/audio-profile/plan profile -> 方案字节（之后走现有 /api/plan/inspect + /api/prepare + /api/commit）
 * - GET  /api/device/inputs      声卡输入能力（模拟 + 光口都算声卡输入；拿不到 available:false + reason）
 *
 * fixture：
 * - wired：默认 2.0（L=DAC0、R=DAC1，其余 22 路中禁用）、AES67 输入启用、声卡输入关闭；
 *           capability 里有 AES67（SDP 顺序）与声卡输入（spdif、48 kHz、2 模拟 + 2 光口）真实形态；
 * - not-wired：后端/设备监督器未接线：GET 200 但 profile=null + capability 全 null + reasons，
 *          写端点必须 503 not_wired（不假成功）。
 *
 * 纪律：
 * - 所有时间戳/角色/lane 顺序固定（不依赖当前时间/随机），测试断言与浏览器截图可复现；
 * - 本文件的产物是 **mock 数据**（engine/device 都不存在），页面必须按 §12.5/§9 显示“未接线的部分为 null + 原因”。
 * - 角色 canonical 与排序规则见 §12.1/§12.3：代码只用 canonical 角色，不维护中文映射。
 */
import { createHash } from 'node:crypto'

export const AUDIO_PROFILE_SCHEMA = 'khaen-audio-profile/v1'

/** 物理输出通道数（MOTU UltraLite-mk5：22；profile.device.channels）。 */
export const AUDIO_PROFILE_DEVICE_CHANNELS = 22

export const AUDIO_PROFILE_FIXTURES = ['wired', 'not-wired']
export const DEFAULT_AUDIO_PROFILE_FIXTURE = 'wired'

export function isAudioProfileFixture(value) {
  return typeof value === 'string' && AUDIO_PROFILE_FIXTURES.includes(value)
}

/** 固定时间戳（mock 可复现；与 status/control fixture 的 2026-11-26 基线一致）。 */
export const AUDIO_PROFILE_FIXED_AT = '2026-11-26T07:40:00.000Z'
/** 固定 graph_id（默认 profile 第一次生成的方案图；32 字节十六进制）。 */
export const AUDIO_PROFILE_GRAPH_ID = '9d9d9d9d0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c'
/** prepare 草稿有效期（秒；仅 UI 提示，mock 不做 TTL 淘汰）。 */
export const AUDIO_PROFILE_PREPARE_TTL_S = 120
/** mock 方案的文件名（POST /api/audio-profile/plan 回执的 stored_path 末段）。 */
export const AUDIO_PROFILE_PLAN_FILENAME = 'audio-profile.plan'

/** canonical 角色（19 个；顺序即 §12.3 的 lane 排序：地面 -> 低频 -> 天空 -> 其它）。 */
export const SPEAKER_ROLE_ORDER = [
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
]

/** 地面角色（9.1.6 上限 9）。 */
export const GROUND_ROLES = ['L', 'R', 'C', 'Lw', 'Rw', 'Ls', 'Rs', 'Lb', 'Rb']
/** 天空角色（9.1.6 上限 6）。 */
export const HEIGHT_ROLES = ['Tfl', 'Tfr', 'Tml', 'Tmr', 'Trl', 'Trr']
export const MAX_GROUND_ROLES = 9
export const MAX_HEIGHT_ROLES = 6

export function isSpeakerRole(value) {
  return typeof value === 'string' && SPEAKER_ROLE_ORDER.includes(value)
}

/** 声卡输入通道（模拟 + 光口；§12.1/§12.2：都算声卡输入）。 */
export const DEVICE_INPUT_CHANNELS = [
  { index: 0, label: 'Analog 1', kind: 'analog' },
  { index: 1, label: 'Analog 2', kind: 'analog' },
  { index: 2, label: 'Optical 1', kind: 'optical' },
  { index: 3, label: 'Optical 2', kind: 'optical' }
]

export const DEVICE_INPUTS_SOURCE = 'motu-alsa'
export const DEVICE_INPUTS_OPTICAL_MODE = 'spdif'
export const DEVICE_INPUTS_SAMPLE_RATE = 48000

/** AES67 能力（mock：SDP 已发 channel-order，2ch）。 */
export const AES67_CAPABILITY = {
  channels: 2,
  map: [0, 1],
  channel_order: 'SMPTE2110.(M,ST)',
  channel_order_source: 'sdp'
}

/** 未接线原因（GET 的 reason / reasons 与写端点 503 的 message 用同一段文字）。 */
export const AUDIO_PROFILE_NOT_WIRED_REASON =
  'the audio profile backend is not wired on this daemon: no device supervisor / MOTU control link is available, so the profile, capability and generated plan are not reported (not fabricated)'

/** 深拷贝（fixture 每次返回新对象，测试可安全修改）。 */
function clone(value) {
  return JSON.parse(JSON.stringify(value))
}

/** 角色排序权重（未知角色排最后；'None' 永远不参与 lane）。 */
function roleRank(role) {
  const index = SPEAKER_ROLE_ORDER.indexOf(role)
  return index < 0 ? SPEAKER_ROLE_ORDER.length : index
}

/** 构造默认 2.0 profile（L=DAC0、R=DAC1；其余禁用；输入只启用 AES67 ch0=L/ch1=R）。 */
export function buildDefaultAudioProfile(deviceChannels = AUDIO_PROFILE_DEVICE_CHANNELS) {
  const channels =
    Number.isInteger(deviceChannels) && deviceChannels > 0 ? deviceChannels : AUDIO_PROFILE_DEVICE_CHANNELS
  const outputs = []
  for (let dac = 0; dac < channels; dac += 1) {
    const role = dac === 0 ? 'L' : dac === 1 ? 'R' : 'None'
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
            { channel: 1, role: 'None' },
            { channel: 2, role: 'None' },
            { channel: 3, role: 'None' }
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

/** 设备输入能力（GET /api/device/inputs 的正文；不含 ok）。 */
export function buildDeviceInputsCapability() {
  return {
    available: true,
    optical_mode: DEVICE_INPUTS_OPTICAL_MODE,
    sample_rate: DEVICE_INPUTS_SAMPLE_RATE,
    channels: clone(DEVICE_INPUT_CHANNELS),
    source: DEVICE_INPUTS_SOURCE,
    reason: null
  }
}

/** 设备输入不可用形态（available:false + reason，不编数值）。 */
export function buildDeviceInputsUnavailable(reason = AUDIO_PROFILE_NOT_WIRED_REASON) {
  return {
    available: false,
    optical_mode: null,
    sample_rate: null,
    channels: null,
    source: null,
    reason
  }
}

/** GET /api/device/inputs 的完整回执（ok + capability）。 */
export function buildDeviceInputsBody(fixture) {
  const name = isAudioProfileFixture(fixture) ? fixture : DEFAULT_AUDIO_PROFILE_FIXTURE
  if (name === 'not-wired') {
    return { ok: true, ...buildDeviceInputsUnavailable() }
  }
  return { ok: true, ...buildDeviceInputsCapability() }
}

/** profile 校验（§12.1 只有四类结构性错误；重复角色/缺少角色不是错误）。 */
export function validateAudioProfile(profile) {
  const errors = []
  const warnings = []
  if (typeof profile !== 'object' || profile === null || Array.isArray(profile)) {
    return { ok: false, errors: [{ code: 'bad_profile', path: 'profile', message: 'profile must be a JSON object' }], warnings }
  }
  const channels = profile.device === undefined ? null : profile.device.channels
  if (!Number.isInteger(channels) || channels <= 0) {
    errors.push({ code: 'bad_device_channels', path: 'device.channels', message: 'device.channels must be a positive integer' })
  }
  const outputs = Array.isArray(profile.outputs) ? profile.outputs : []
  const seenDacs = new Set()
  const enabledRoles = []
  const groundCount = { value: 0 }
  const heightCount = { value: 0 }
  for (let index = 0; index < outputs.length; index += 1) {
    const output = outputs[index]
    const path = 'outputs[' + String(index) + ']'
    if (typeof output !== 'object' || output === null || Array.isArray(output)) {
      errors.push({ code: 'bad_output', path, message: 'output entry must be an object' })
      continue
    }
    const dac = output.dac
    if (!Number.isInteger(dac) || dac < 0 || (Number.isInteger(channels) && dac >= channels)) {
      errors.push({
        code: 'dac_out_of_range',
        path: path + '.dac',
        message: 'DAC ' + String(dac) + ' is outside 0..' + String(Number.isInteger(channels) ? channels - 1 : '?')
      })
      continue
    }
    if (seenDacs.has(dac)) {
      errors.push({ code: 'duplicate_dac', path, message: 'DAC ' + String(dac) + ' appears more than once' })
    }
    seenDacs.add(dac)
    const role = output.role
    if (!isSpeakerRole(role)) {
      errors.push({ code: 'bad_role', path: path + '.role', message: 'unknown role ' + JSON.stringify(role) })
      continue
    }
    const enabled = output.enabled === true
    if (enabled && role === 'None') {
      errors.push({ code: 'enabled_without_role', path, message: 'an enabled output must have a role (not None)' })
    }
    if (role === 'Custom' && (typeof output.custom_label !== 'string' || output.custom_label.trim().length === 0)) {
      errors.push({ code: 'custom_without_label', path: path + '.custom_label', message: 'Custom roles need a custom_label' })
    }
    if (!enabled && role !== 'None') {
      warnings.push({
        code: 'disabled_with_role',
        path,
        message: 'a disabled output keeps its role label; it will not produce a lane'
      })
    }
    if (enabled) {
      enabledRoles.push(role)
    }
  }
  if (outputs.length > 0 && enabledRoles.length === 0) {
    errors.push({ code: 'no_enabled_outputs', path: 'outputs', message: 'all physical outputs are disabled: at least one speaker must be enabled' })
  }
  // 9.1.6 上限：地面最多 9、天空最多 6（同角色可重复；重复不是错误）。
  for (const role of enabledRoles) {
    if (GROUND_ROLES.includes(role)) {
      groundCount.value += 1
    }
    if (HEIGHT_ROLES.includes(role)) {
      heightCount.value += 1
    }
  }
  if (groundCount.value > MAX_GROUND_ROLES) {
    errors.push({
      code: 'too_many_ground',
      path: 'outputs',
      message: 'ground speakers exceed the 9.1.6 limit: ' + String(groundCount.value) + ' > ' + String(MAX_GROUND_ROLES)
    })
  }
  if (heightCount.value > MAX_HEIGHT_ROLES) {
    errors.push({
      code: 'too_many_height',
      path: 'outputs',
      message: 'height speakers exceed the 9.1.6 limit: ' + String(heightCount.value) + ' > ' + String(MAX_HEIGHT_ROLES)
    })
  }
  const sources = profile.inputs === undefined || profile.inputs === null ? [] : profile.inputs.sources
  const sourceList = Array.isArray(sources) ? sources : []
  for (let index = 0; index < sourceList.length; index += 1) {
    const source = sourceList[index]
    const path = 'inputs.sources[' + String(index) + ']'
    if (typeof source !== 'object' || source === null || Array.isArray(source)) {
      errors.push({ code: 'bad_source', path, message: 'input source must be an object' })
      continue
    }
    if (source.kind !== 'aes67' && source.kind !== 'device') {
      errors.push({ code: 'bad_source_kind', path: path + '.kind', message: 'kind must be aes67 or device' })
    }
    const sourceChannels = Array.isArray(source.channels) ? source.channels : []
    const seenChannels = new Set()
    for (let channelIndex = 0; channelIndex < sourceChannels.length; channelIndex += 1) {
      const entry = sourceChannels[channelIndex]
      const channelPath = path + '.channels[' + String(channelIndex) + ']'
      if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) {
        errors.push({ code: 'bad_input_channel', path: channelPath, message: 'input channel must be an object' })
        continue
      }
      if (!Number.isInteger(entry.channel) || entry.channel < 0) {
        errors.push({ code: 'bad_input_channel', path: channelPath + '.channel', message: 'channel must be a non-negative integer' })
      } else if (seenChannels.has(entry.channel)) {
        errors.push({ code: 'duplicate_input_channel', path: channelPath, message: 'channel ' + String(entry.channel) + ' appears twice' })
      } else {
        seenChannels.add(entry.channel)
      }
      if (!isSpeakerRole(entry.role)) {
        errors.push({ code: 'bad_role', path: channelPath + '.role', message: 'unknown role ' + JSON.stringify(entry.role) })
      }
    }
  }
  // informational：缺 C / 缺 LFE 走下混（§12.3 第 2 条），页面显示提示与实际增益。
  if (enabledRoles.length > 0 && !enabledRoles.includes('C')) {
    const hasCenterInput = sourceList.some(
      (source) =>
        typeof source === 'object' &&
        source !== null &&
        source.enabled === true &&
        Array.isArray(source.channels) &&
        source.channels.some((entry) => typeof entry === 'object' && entry !== null && entry.role === 'C')
    )
    if (hasCenterInput) {
      warnings.push({
        code: 'downmix_center',
        path: 'downmix.center',
        message: 'no C lane is enabled: the C input is folded into L/R with downmix.center.gain_db'
      })
    }
  }
  if (enabledRoles.length > 0 && !enabledRoles.includes('LFE') && !enabledRoles.includes('Sub')) {
    const hasLfeInput = sourceList.some(
      (source) =>
        typeof source === 'object' &&
        source !== null &&
        source.enabled === true &&
        Array.isArray(source.channels) &&
        source.channels.some((entry) => typeof entry === 'object' && entry !== null && (entry.role === 'LFE' || entry.role === 'Sub'))
    )
    if (hasLfeInput) {
      warnings.push({
        code: 'downmix_lfe',
        path: 'downmix.lfe',
        message: 'no LFE/Sub lane is enabled: the LFE input is folded into the other lanes with downmix.lfe.gain_db'
      })
    }
  }
  return { ok: errors.length === 0, errors, warnings }
}

/** outputs / inputs / downmix / fanout 四块是否有变化（PUT prepare 的 diff）。 */
export function profileDiff(current, next) {
  const normalized = (profile) => ({
    outputs: profile === null || profile === undefined ? null : profile.outputs ?? null,
    inputs: profile === null || profile === undefined ? null : profile.inputs ?? null,
    downmix: profile === null || profile === undefined ? null : profile.downmix ?? null,
    fanout: profile === null || profile === undefined ? null : profile.fanout ?? null
  })
  const left = normalized(current)
  const right = normalized(next)
  const equal = (a, b) => JSON.stringify(a) === JSON.stringify(b)
  return {
    outputs_changed: !equal(left.outputs, right.outputs),
    inputs_changed: !equal(left.inputs, right.inputs),
    downmix_changed: !equal(left.downmix, right.downmix),
    fanout_changed: !equal(left.fanout, right.fanout)
  }
}

/** 启用输出按 §12.3 排序为 lane 0..N-1（角色 canonical 顺序；同角色按 DAC 升序）。 */
export function enabledLaneOrder(profile) {
  const outputs = Array.isArray(profile?.outputs) ? profile.outputs : []
  const enabled = outputs.filter(
    (output) => output !== null && typeof output === 'object' && output.enabled === true && output.role !== 'None' && isSpeakerRole(output.role)
  )
  enabled.sort((left, right) => roleRank(left.role) - roleRank(right.role) || left.dac - right.dac)
  return enabled.map((output, lane) => ({ lane, dac: output.dac, role: output.role, gain_db: typeof output.gain_db === 'number' ? output.gain_db : 0 }))
}

/** 线性增益（dB -> amplitude；0 dB = 1.0）。 */
export function dbToGain(db) {
  return typeof db === 'number' && Number.isFinite(db) ? Math.pow(10, db / 20) : 1
}

/** profile 方案摘要（GET 快照的 plan 段；不可用整组 null）。 */
export function buildPlanSummary(profile, graphId = AUDIO_PROFILE_GRAPH_ID) {
  if (profile === null || profile === undefined) {
    return { lanes: null, enabled_outputs: null, dac_channels: null, graph_id: null }
  }
  const lanes = enabledLaneOrder(profile)
  return {
    lanes: lanes.length,
    enabled_outputs: lanes.length,
    dac_channels: Number.isInteger(profile.device?.channels) ? profile.device.channels : AUDIO_PROFILE_DEVICE_CHANNELS,
    graph_id: graphId
  }
}

/**
 * 由 profile 生成方案字节 + 回执（§12.3：lane 排序 / folds / 下混 / fanout）。
 * 返回的 plan_path 与 sha256 可用于现有 POST /api/plan/inspect 与 /api/prepare。
 */
export function buildGeneratedPlan(profile) {
  const lanes = enabledLaneOrder(profile)
  const enabledRoles = lanes.map((entry) => entry.role)
  const laneIndexByRole = new Map()
  for (const entry of lanes) {
    if (!laneIndexByRole.has(entry.role)) {
      laneIndexByRole.set(entry.role, [])
    }
    laneIndexByRole.get(entry.role).push(entry.lane)
  }
  const fanoutMode = profile.fanout?.mode === 'group' ? 'group' : 'unity'
  const folds = []
  const foldsPreview = []
  const sources = Array.isArray(profile.inputs?.sources) ? profile.inputs.sources : []
  for (const source of sources) {
    if (source === null || typeof source !== 'object' || source.enabled !== true) {
      continue
    }
    const sourceGain = dbToGain(source.gain_db)
    const channels = Array.isArray(source.channels) ? source.channels : []
    for (const channel of channels) {
      if (channel === null || typeof channel !== 'object' || !isSpeakerRole(channel.role) || channel.role === 'None') {
        continue
      }
      const targets = laneIndexByRole.get(channel.role) ?? []
      const groupGain = fanoutMode === 'group' && targets.length > 1 ? 1 / Math.sqrt(targets.length) : 1
      for (const lane of targets) {
        const gain = sourceGain * groupGain
        folds.push([channel.channel, lane, gain])
        foldsPreview.push({ source: source.kind, channel: channel.channel, role: channel.role, lane, gain })
      }
    }
  }
  const downmixApplied = []
  // 缺 C：C 输入额外 fold 到 L/R（每 lane 用 downmix.center.gain_db）。
  const centerMode = profile.downmix?.center?.mode === 'none' ? 'none' : 'auto'
  if (centerMode === 'auto' && !enabledRoles.includes('C')) {
    const centerGain = dbToGain(profile.downmix?.center?.gain_db ?? -3)
    const lanesToFold = []
    for (const source of sources) {
      if (source === null || typeof source !== 'object' || source.enabled !== true) {
        continue
      }
      for (const channel of Array.isArray(source.channels) ? source.channels : []) {
        if (channel !== null && typeof channel === 'object' && channel.role === 'C') {
          for (const lane of laneIndexByRole.get('L') ?? []) {
            folds.push([channel.channel, lane, dbToGain(source.gain_db) * centerGain])
            lanesToFold.push(lane)
          }
          for (const lane of laneIndexByRole.get('R') ?? []) {
            folds.push([channel.channel, lane, dbToGain(source.gain_db) * centerGain])
            lanesToFold.push(lane)
          }
        }
      }
    }
    if (lanesToFold.length > 0) {
      downmixApplied.push({ kind: 'center', gain_db: profile.downmix?.center?.gain_db ?? -3, lanes: [...new Set(lanesToFold)].sort((a, b) => a - b) })
    }
  }
  // 缺 LFE/Sub：LFE 输入额外 fold 到其它所有启用 lane（include_heights 默认 false）。
  const lfeMode = profile.downmix?.lfe?.mode === 'none' ? 'none' : 'auto'
  if (lfeMode === 'auto' && !enabledRoles.includes('LFE') && !enabledRoles.includes('Sub')) {
    const lfeGain = dbToGain(profile.downmix?.lfe?.gain_db ?? -3)
    const includeHeights = profile.downmix?.lfe?.include_heights === true
    const targets = lanes
      .filter((entry) => !HEIGHT_ROLES.includes(entry.role) || includeHeights)
      .filter((entry) => entry.role !== 'LFE' && entry.role !== 'Sub')
      .map((entry) => entry.lane)
    const foldedLanes = []
    for (const source of sources) {
      if (source === null || typeof source !== 'object' || source.enabled !== true) {
        continue
      }
      for (const channel of Array.isArray(source.channels) ? source.channels : []) {
        if (channel !== null && typeof channel === 'object' && (channel.role === 'LFE' || channel.role === 'Sub')) {
          for (const lane of targets) {
            folds.push([channel.channel, lane, dbToGain(source.gain_db) * lfeGain])
            foldedLanes.push(lane)
          }
        }
      }
    }
    if (foldedLanes.length > 0) {
      downmixApplied.push({
        kind: 'lfe',
        gain_db: profile.downmix?.lfe?.gain_db ?? -3,
        lanes: [...new Set(foldedLanes)].sort((a, b) => a - b)
      })
    }
  }
  const planObject = {
    schema: 'khaen-plan/v1',
    source: 'audio-profile',
    profile_schema: AUDIO_PROFILE_SCHEMA,
    device: { channels: profile.device?.channels ?? AUDIO_PROFILE_DEVICE_CHANNELS },
    content: {
      in_channels: AUDIO_PROFILE_DEVICE_CHANNELS,
      out_lanes: lanes.length,
      folds,
      lane_roles: lanes.map((entry) => entry.role),
      dac_order: lanes.map((entry) => entry.dac)
    },
    routing: {
      dac_channels: profile.device?.channels ?? AUDIO_PROFILE_DEVICE_CHANNELS,
      routes: lanes.map((entry) => [entry.lane, entry.dac])
    },
    fanout: { mode: fanoutMode },
    downmix: {
      center: { mode: centerMode, gain_db: profile.downmix?.center?.gain_db ?? -3 },
      lfe: {
        mode: lfeMode,
        gain_db: profile.downmix?.lfe?.gain_db ?? -3,
        include_heights: profile.downmix?.lfe?.include_heights === true
      }
    }
  }
  const text = JSON.stringify(planObject)
  const bytes = Buffer.from(text, 'utf8')
  const sha256 = createHash('sha256').update(bytes).digest('hex')
  const storedPath = '/var/lib/khaen/plans/' + sha256 + '.plan'
  const planFacts = {
    sha256,
    graph_id: sha256,
    size_bytes: bytes.length,
    sample_rate: 48000,
    block_size: 128,
    segments: ['content', 'crossover', 'matrix', 'protection', 'routing'],
    segment_count: 5,
    latency_samples: 1023,
    stored_path: storedPath,
    recorded_at: AUDIO_PROFILE_FIXED_AT,
    filename: AUDIO_PROFILE_PLAN_FILENAME
  }
  const inspect = {
    ok: true,
    staged: false,
    sha256,
    graph_id: sha256,
    plan: {
      block_size: planFacts.block_size,
      sample_rate: planFacts.sample_rate,
      in_channels: AUDIO_PROFILE_DEVICE_CHANNELS,
      lanes: lanes.length,
      dac_channels: planObject.routing.dac_channels
    },
    admission: { accepted: true, rejections: [], budget: {} },
    compile: { compiled: true, latency_samples: planFacts.latency_samples }
  }
  return {
    bytes,
    sha256,
    plan_path: storedPath,
    lane_count: lanes.length,
    plan_facts: planFacts,
    inspect,
    folds_preview: foldsPreview,
    downmix_applied: downmixApplied
  }
}

/** 构造 profile fixture 种子（调用方可直接修改；每次返回新对象）。 */
export function buildAudioProfileSeed(fixture) {
  const name = isAudioProfileFixture(fixture) ? fixture : DEFAULT_AUDIO_PROFILE_FIXTURE
  if (name === 'not-wired') {
    return {
      fixture: name,
      available: false,
      reason: AUDIO_PROFILE_NOT_WIRED_REASON,
      profile: null,
      capability: {
        aes67: { channels: null, map: null, channel_order: null, channel_order_source: null },
        device_inputs: buildDeviceInputsUnavailable()
      },
      plan: { lanes: null, enabled_outputs: null, dac_channels: null, graph_id: null },
      reasons: {
        profile: AUDIO_PROFILE_NOT_WIRED_REASON,
        capability: AUDIO_PROFILE_NOT_WIRED_REASON,
        'capability.aes67': AUDIO_PROFILE_NOT_WIRED_REASON,
        'capability.device_inputs': AUDIO_PROFILE_NOT_WIRED_REASON,
        plan: AUDIO_PROFILE_NOT_WIRED_REASON
      },
      revision: null
    }
  }
  const profile = buildDefaultAudioProfile()
  return {
    fixture: name,
    available: true,
    reason: null,
    profile,
    capability: {
      aes67: clone(AES67_CAPABILITY),
      device_inputs: buildDeviceInputsCapability()
    },
    plan: buildPlanSummary(profile, AUDIO_PROFILE_GRAPH_ID),
    reasons: {},
    revision: 1
  }
}
