// KHAEN RT-first DSP 控制台 —— REST 契约类型（依据 control/src/rest.rs 与实测响应）。
// 诚实原则：engine/synthetic/actual 字段原样保留，页面绝不自造 actual。

/** 错误响应：{"error":{"code":"...","message":"..."}} */
export interface ApiErrorBody {
  error: { code: string; message: string }
}

export interface AuthStatus {
  password_configured: boolean
  setup_required: boolean
  token_fallback: boolean
  mock: boolean
  min_password_len: number
  recommended_password_len: number
}

export interface AuthSession {
  authenticated: boolean
  auth_kind: 'session' | 'token' | string
  csrf_token: string
  expires_in: number
  created_unix: number
}

export interface CoeffRef {
  version: string
  sha256: string
}

export interface RoleDac {
  role: string
  dac: number
}

export interface TargetSpec {
  layout: string
  channels: number
  profile: string
  roles: RoleDac[]
  low_band_dacs: number[]
  /** GET 响应里带 target_hash（PUT 请求体里不需要） */
  target_hash?: string
}

export interface PendingPlan {
  prepare_id: string
  planned_version: string
  admission: string
  created_unix: number
  invalidated: boolean
}

/** task-313 HostEngine 透传的 host 快照（engine=rt-host 时出现；旧 daemon 无此字段）。 */
export interface HostSnapshot {
  blocks?: number
  frames?: number
  xruns?: number
  underruns?: number
  overruns?: number
  recoveries?: number
  faults?: number
  muted_blocks?: number
  silence_blocks?: number
  safety_mute_blocks?: number
  deadline_misses?: number
  forced_mute?: boolean
  forced_mute_blocks?: number
  sample_rate?: number
  block_size?: number
  graph?: string
  safe_mode?: string
  silent?: boolean
  safety_mute?: boolean
  runtime_state?: string
  wired?: string[]
  not_wired?: string[]
}

export interface RuntimeStatus {
  schema: string
  api_version: number
  engine: string
  synthetic: boolean
  actual: boolean
  mode_controller: boolean
  mode_services: string
  runtime_state: string
  device_owner: string
  epoch: number
  actual_version: string
  graph_id: string
  coeff: CoeffRef | null
  last_good_coeff: CoeffRef | null
  available_coeffs: CoeffRef[]
  target: TargetSpec
  target_hash: string
  mode: string
  mode_label: string
  mode_profile: string | null
  sample_rate: number
  chunksize: number
  volume_db: number | null
  mute: boolean
  xruns: number
  deadline_misses: number
  clipped_samples: number
  calibration_owner: boolean
  committed_unix: number
  pending_prepare: string | null
  pending_plan: PendingPlan | null
  /** engine=rt-host 时由 daemon 原样透传的 host 运行时快照。 */
  host?: HostSnapshot | null
}

export interface VolumeBody {
  schema: string
  engine: string
  synthetic: boolean
  actual: boolean
  volume_db: number
  mute: boolean
  actual_version: string
  epoch: number
  runtime_state: string
  ok?: boolean
  requested_volume_db?: number
}

export interface MuteBody {
  schema: string
  engine: string
  synthetic: boolean
  actual: boolean
  mute: boolean
  volume_db: number
  actual_version: string
  epoch: number
  runtime_state: string
  ok?: boolean
  requested_mute?: boolean
}

export interface ModeEvidence {
  epoch: number
  switch_seq: number
  servo_owner: string
  servo_claims: unknown
  rolled_back: boolean
  last_exit_code: number
  fallback: string
}

export interface ModeProfile {
  mode: string
  label: string
  input_format: string
  output_format: string
  default_chunksize: number
  default_target_level: number | null
}

export interface ModeStatus {
  schema: string
  engine: string
  synthetic: boolean
  actual: boolean
  mode_controller: boolean
  mode_services: string
  mode_actual: boolean
  mode_evidence: ModeEvidence | null
  mode: string
  mode_label: string
  target: string | null
  target_label: string | null
  state: string
  step: string | null
  step_label: string | null
  progress: number
  steps: unknown[]
  last_error: string | null
  warnings: string[]
  started_unix: number | null
  finished_unix: number | null
  enabled: boolean
  restore_on_start: boolean
  switch_script: string
  strategy: string
  profiles: ModeProfile[]
  switch_state?: string
}

export interface RuntimeConfigBody {
  schema: string
  config: {
    mode: string
    sample_rate: number
    chunksize: number
    target: TargetSpec
    coeff: CoeffRef | null
    config_hash: string
  }
  config_hash: string
  actual_config: unknown
  actual_version: string
  graph_id: string
  epoch: number
  requires_commit: boolean
  pending_prepare: string | null
}

export interface TargetBody {
  schema: string
  target: TargetSpec
  target_hash: string
  actual_target: TargetSpec
  actual_target_hash: string
  config_hash: string
  actual_version: string
  requires_commit: boolean
  pending_prepare: string | null
}

export interface CoeffBody {
  schema: string
  coeff: CoeffRef | null
  actual_coeff: CoeffRef | null
  last_good_coeff: CoeffRef | null
  available_coeffs: CoeffRef[]
  config_hash: string
  actual_version: string
  requires_commit: boolean
  pending_prepare: string | null
}

export interface PrepareBody {
  schema: string
  prepared: boolean
  engine: string
  synthetic: boolean
  actual: boolean
  prepare_id: string
  planned_version: string
  planned_target_hash: string
  planned_config_hash: string
  planned_coeff: CoeffRef | null
  admission: string
  prepared_plan_version: number
  prepared_graph_id: string
  expected_workers: number
  admitted_bytes: number
  actual_version: string
  requires_commit: boolean
}

export interface CommitBody {
  schema: string
  committed: boolean
  replayed: boolean
  engine: string
  synthetic: boolean
  actual: boolean
  prepare_id: string
  actual_version: string
  graph_id: string
  config_sha256: string
  plan_version: number
  previous_plan_version: number
  expected_workers: number
  blocks_committed: number
  ack_mask: number
  rolled_back: boolean
  coeff: CoeffRef | null
  target_hash: string
  epoch: number
  committed_unix: number
  mode: string
  sample_rate: number
  chunksize: number
  runtime_state: string
  device_owner: string
}

export interface AudioFormatBody {
  schema: string
  engine: string
  synthetic: boolean
  actual: boolean
  mode: string
  mode_label: string
  source: string
  sample_rate: number
  bit_depth: number
  channels: number
  chunksize: number
  target_level: number | null
  input_format: string
  output_format: string
  config_path: string | null
  limits: {
    sample_rates: number[]
    chunksizes: number[]
    ui_reference_chunksizes: number[]
    default_sample_rate: number
    default_chunksize: number
  }
  warnings: string[]
  xrun: { xruns: number; deadline_misses: number; clipped_samples: number }
  mock: boolean
  actual_version: string
  graph_id: string
  epoch: number
}

export interface FormatApplyBody {
  ok: boolean
  schema: string
  format: AudioFormatBody
  outcome: {
    engine: string
    synthetic: boolean
    actual: boolean
    actual_version: string
    graph_id: string
    plan_version: number
    blocks_committed: number
    ack_mask: number
    epoch: number
    committed_unix: number
    warnings: string[]
  }
}

export interface ChannelMapBody {
  status: string
  reason?: string
}

export interface CalEngineInfo {
  engine: string
  synthetic: boolean
  backend: string
  real_audio: boolean
  note: string
}

export interface CalJob {
  job_id: string
  kind: string
  state: string
  stage: string | null
  progress: number
  applied_version: string | null
  last_good_version: string | null
  failure: string | null
  created_unix: number
  updated_unix: number
}

export interface CalJobsBody {
  schema: string
  count: number
  jobs: CalJob[]
  engine: string
  synthetic: boolean
  real_audio: boolean
  cal_engine: CalEngineInfo
}

export interface CalJobBody {
  schema: string
  job: CalJob
  job_id: string
  state: string
  progress: number
  engine: string
  synthetic: boolean
  real_audio: boolean
  cal_engine: CalEngineInfo
  applied: boolean
  applied_to_audio: boolean
  not_applied_to_audio: boolean
  audio_status: string
  measured_audio: boolean
  production_calibration: boolean
  calibration_scope: string
}

/** apply/retest/cancel/rollback 的回执：不同 daemon 构建可能只带 {job} 而省略顶层字段。 */
export interface CalJobActionBody {
  schema?: string
  job: CalJob
  job_id?: string
  state?: string
  progress?: number
  engine?: string
  synthetic?: boolean
  real_audio?: boolean
  cal_engine?: CalEngineInfo
  applied?: boolean
  applied_to_audio?: boolean
  not_applied_to_audio?: boolean
  audio_status?: string
  measured_audio?: boolean
  production_calibration?: boolean
  calibration_scope?: string
  cancelled?: boolean
}

export interface HealthBody {
  status: string
  version: string
  mock: boolean
  uptime_secs: number
  now_unix: number
}

/** 操作回执的统一「真实性」投影（页面用；不含任何推断出的 actual）。 */
export interface AckView {
  kind: 'actual' | 'synthetic' | 'unsupported' | 'error' | 'info'
  engine: string | null
  actual: boolean | null
  synthetic: boolean | null
  detail: string
  raw: unknown
}
/** 内容布局 / 中置下混（GET/PUT /api/content/layout；task-316 真实契约 khaen-host-content/v1）。 */
export interface ContentLayoutCurrent {
  configured: boolean
  input_layout: string
  output_layout: string
  center: 'auto' | 'present' | 'absent' | string
  program_peak_dbfs: number
  program_peak_measured_dbfs: number | null
  /** applied | disabled_* | disabled（后端状态机原样输出） */
  adapter_status: string
  applied: boolean
  source_channels: number
  center_channel: number | null
  applied_blocks: number
  applied_block: number
}

export interface ContentLayoutPresets {
  available: boolean
  input?: string[]
  /** 后端在 presets 不可用时可能给对象 {available:false,error} */
  output?: string[] | { available: false; error: string }
  error?: string
}

export interface ContentLayoutBody {
  ok?: boolean
  schema?: string
  engine?: string
  actual?: boolean
  wired?: string[]
  not_wired?: string[]
  current: ContentLayoutCurrent
  presets_path?: string
  presets?: ContentLayoutPresets
  plan_input_channels?: number
  plan_output_channels?: number
}

/** PUT /api/content/layout 请求体（enabled:false 表示停用内容适配）。 */
export interface ContentLayoutDraft {
  input_layout?: string
  output_layout?: string
  center?: string
  program_peak_dbfs?: number
  enabled?: boolean
}

/** PUT 的 actual ack。 */
export interface ContentLayoutAck {
  ok: boolean
  actual: boolean
  applied: boolean
  applied_block?: number
  current: ContentLayoutCurrent
  schema?: string
  engine?: string
}
