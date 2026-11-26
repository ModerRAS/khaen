#!/usr/bin/env node
/**
 * Contract mock for the khaen web console — DEV / TEST ONLY (scripts/ is never bundled into dist).
 *
 * 对齐 docs/API.md v1.2（唯一 API 合同）+ crates/daemon 实际形态；所有写端点遵守 M1 错误体
 * {error:{code,message,details}}，前端兼容的 host_link 转发形态只保留在旧转发路径。
 *
 * 认证（scripts 级 mock，不是真实安全实现）：
 *   GET  /api/auth/status   -> {configured, authenticated}            (anonymous)
 *   POST /api/auth/setup    -> 201 {csrf, expires_in} + Set-Cookie   (409 when configured)
 *   POST /api/auth/login    -> 200 {csrf, expires_in} + Set-Cookie   (401 wrong, 429 rate limited)
 *   GET  /api/auth/session  -> {authenticated, csrf, expires_at}     (401 when anonymous)
 *   POST /api/auth/logout   -> clears session (idempotent)
 *   POST /api/auth/password -> {old, new}, requires session + CSRF
 *
 * 只读视图：
 *   GET  /api/status        -> 401 without session; khaen-status/v1 fixture（present | stale | missing，
 *                              scripts/status-fixtures.mjs）+ 运行时叠加（mute/format/plan）+ §12.2 加法 speaker_test
 *   GET  /api/meta          -> {ok, api_version, web_version, ops, surfaces, calibration:{wired,reason},
 *                              host_link, not_wired}（能力清单来自当前 fixture，页面先读能力再决定入口）
 *   GET  /api/motu          -> khaen-motu/v1 设备快照（fixtures: wired | degraded，scripts/motu-fixtures 逻辑内联）
 *   GET  /api/channels      -> khaen-channels/v1（恒 200；fixtures: wired | partial | degraded | unavailable，
 *                              scripts/channels-fixtures.mjs）+ §12.2 加法 speaker_roles/input_roles
 *   GET  /api/device/inputs -> 声卡输入能力（模拟 + 光口；scripts/audio-profile-fixtures.mjs）
 *   GET  /api/audio-profile -> 音频通道 profile 快照（fixtures: wired | not-wired，scripts/audio-profile-fixtures.mjs）
 *   GET  /api/plan/history、POST /api/plan/upload|inspect   （T60 起按 daemon 真实形态：
 *                              upload 回执带 segments:[区段名]/segment_count/stored_path；inspect 是
 *                              POST {plan_path, plan_sha256?}（plan_path = 上传回执的 stored_path）；
 *                              history 是 {current:{version,graph_id}, entries:[{recorded_at,...}]}）
 *   GET  /healthz           -> {ok, version, uptime_s}（匿名；version = release 版本，缺失回退 crate 版本）
 *
 * 两阶段写操作（全部要求会话 + X-KHAEN-CSRF；CSRF 失败 403 csrf_invalid）：
 *   POST /api/surface       -> 五个语义界面 volume|routing|layout|mode|format 的 prepare
 *                              （volume/routing/layout: engine_token/planned_version/graph_id/admission；
 *                               mode/format 另带 changed/applies_to/device_restart_required）
 *   POST /api/format        -> format prepare 的独立入口（与 surface format 同一套草稿）
 *   POST /api/prepare|commit|abort            -> 方案/回滚草稿两阶段（单一 pending 草稿槽）
 *   POST /api/rollback      -> 历史旧版本准备成草稿（token 递增，commit 后生效）
 *   POST /api/mute          -> 静音立即生效（回执带真实回读 mute/muted）
 *   GET/POST/PUT /api/motu/volume -> MOTU 音量两阶段（prepare 只签 token、commit 才写设备 + 真值回读；
 *                              没有 abort，草稿由 TTL 过期；>2 通道 400 motu_batch_rejected）
 *   PUT  /api/channels      -> 数据面热重配两阶段 {stage:"prepare"|"commit"|"abort"}：
 *                              prepare 回 changed/diff/current/target/engine_token/commit_supported/blockers/
 *                              admission/graph；commit 回 applied/changed/generation/gap_ms/format/
 *                              graph_dropped/routing（失败 502 ERR_RECONFIGURE_* + details.rolled_back）；
 *                              mapping/dac_map 属方案 routing：只改 mapping 时 commit_supported=false 且不签 token
 *   PUT  /api/audio-profile -> 音频 profile 两步保存 {stage:"prepare"|"commit"|"abort"}：
 *                              prepare 回 profile/validation/diff/token/expires_in_s；commit 回
 *                              applied/saved/revision/profile；abort 回 aborted
 *   POST /api/audio-profile/plan -> profile -> 方案文件（plan_path/sha256/lane_count/inspect/
 *                              folds_preview/downmix_applied），之后走现有 /api/plan/inspect + prepare + commit
 *   POST /api/speaker-test、POST /api/speaker-test/stop -> 通道识别测试音（必须 audible=true 且
 *                              confirm=true；dac 0..21、ms 500..10000、db -60..-12 默认 -30；
 *                              fixtures: enabled | sound-window-closed(403) | not-wired(503)）；
 *                              stop 幂等
 *   POST /api/device/retry  -> 设备监督器重试回执
 *
 * 校准 REST（实际 canonical 形状，docs/API.md §7 裁决 H；写操作要求 CSRF）：
 *   POST /api/calibration/uploads|null -> 原始字节 WAV 上传（201 首次 / 200 同 sha 去重；
 *                              {upload_id,sha256,bytes,created}，无 schema）
 *   GET/POST /api/calibration/jobs -> 列表 {schema,available,reason?,engine?,active_job_id?,total,limit,
 *                              offset,jobs,reasons}；创建 202 {ok,job_id,state,location}
 *   GET  /api/calibration/jobs/{id} -> {ok,schema,job(id/status canonical),result,artifacts,error,reasons}
 *   POST /api/calibration/jobs/{id}/cancel -> {ok,job_id,status,state}
 *   POST /api/calibration/jobs/{id}/apply|rollback -> 旧单发 {} 或 staged
 *                              {stage:"prepare"|"commit"|"abort",engine_token?}
 *   GET  /api/calibration/jobs/{id}/artifacts/{name} -> 二进制/JSON 产物（Content-Disposition +
 *                              X-KHAEN-Artifact-SHA256；含二进制 plan.bin）
 *   GET/PUT /api/calibration/wizard、GET /api/calibration/wizard/state、POST /api/calibration/wizard/apply
 *                              （apply 返回 apply 回执；无可用 job 409 no_applicable_job）
 *   fixtures: empty | success | failed | unavailable（scripts/calibration-fixtures.mjs）
 *
 * 系统 REST（fixtures: mismatch | consistent | degraded，scripts/system-fixtures.mjs）：
 *   GET /api/system/info|logs|diagnostics、POST /api/system/password|restart
 *
 * Dev-only fixture 切换（帮助浏览器验收，真实 daemon 没有这些端点）：
 *   GET  /__mock/status|calibration|system|control|channels|motu|audio-profile|speaker-test
 *   POST /__mock/{status,calibration,system,control,channels,motu,audio-profile,speaker-test}-fixture {fixture}
 *   （channels 除内置 4 个 fixture 外还接受 reconfigure-failure | host-link-unavailable | mapping-only）
 *
 * Cookie: khaen_session=<opaque>; HttpOnly; SameSite=Strict; Path=/; Max-Age=28800
 * CSRF: X-KHAEN-CSRF must equal the session csrf for session-bound writes.
 * State is in-memory only (restart = fresh setup flow). It never replaces the real backend:
 * the real one is Rust/axum and must be validated separately; this mock 不假成功：
 * 未接线的能力一律 null + reason / 503 not_wired，写失败不返回成功回执。
 *
 * Usage:
 *   node scripts/mock-auth-server.mjs                          # 127.0.0.1:8790
 *   node scripts/mock-auth-server.mjs --port 8730
 *   node scripts/mock-auth-server.mjs --status-fixture stale   # present | stale | missing
 *   node scripts/mock-auth-server.mjs --calibration-fixture success # empty | success | failed | unavailable
 *   node scripts/mock-auth-server.mjs --system-fixture degraded    # mismatch | consistent | degraded
 *   node scripts/mock-auth-server.mjs --control-fixture wired --channels-fixture wired
 *                              # control: wired | unavailable; channels: 见上（含三个扩展 fixture）
 *   node scripts/mock-auth-server.mjs --motu-fixture degraded --audio-profile-fixture not-wired \
 *        --speaker-test-fixture sound-window-closed
 *   KHAEN_DEV_API=http://127.0.0.1:8790 pnpm dev               # vite proxies /api to the mock
 */
import { createHash, randomBytes } from 'node:crypto'
import { createServer } from 'node:http'
import { pathToFileURL } from 'node:url'
import {
  DEFAULT_STATUS_FIXTURE,
  STATUS_FIXTURES,
  buildStatusFixture,
  isStatusFixture
} from './status-fixtures.mjs'
import {
  CALIBRATION_FIXTURES,
  DEFAULT_CALIBRATION_FIXTURE,
  buildCalibrationSeed,
  buildWizardDefaults,
  createQueuedJobRecord,
  finalizeJobFailure,
  isCalibrationFixture,
  tickJobRecord
} from './calibration-fixtures.mjs'
import {
  DEFAULT_SYSTEM_FIXTURE,
  LOG_UNITS,
  LOG_UNIT_NAMES,
  SYSTEM_FIXTURES,
  buildDiagnosticsFilename,
  buildSystemInfoFixture,
  buildSystemLogsFixture,
  isLogUnit,
  isSystemFixture
} from './system-fixtures.mjs'
import {
  CONTROL_FIXTURES,
  DEFAULT_CONTROL_FIXTURE,
  buildControlSeed,
  graphIdForVersion,
  isControlFixture
} from './control-fixtures.mjs'
import {
  CHANNELS_FIXTURES,
  DEFAULT_CHANNELS_FIXTURE,
  MAPPING_PLAN_OWNED_REASON,
  buildChannelsAbortAck,
  buildChannelsCommitAck,
  buildChannelsCommitFailureDetails,
  buildChannelsCurrent,
  buildChannelsMappingOnlyAck,
  buildChannelsPrepareAck,
  buildChannelsSeed,
  isChannelsFixture
} from './channels-fixtures.mjs'
import {
  AES67_CAPABILITY,
  AUDIO_PROFILE_DEVICE_CHANNELS,
  AUDIO_PROFILE_FIXED_AT,
  AUDIO_PROFILE_FIXTURES,
  AUDIO_PROFILE_GRAPH_ID,
  AUDIO_PROFILE_NOT_WIRED_REASON,
  AUDIO_PROFILE_PREPARE_TTL_S,
  AUDIO_PROFILE_SCHEMA,
  DEFAULT_AUDIO_PROFILE_FIXTURE,
  buildAudioProfileSeed,
  buildDefaultAudioProfile,
  buildDeviceInputsBody,
  buildGeneratedPlan,
  buildPlanSummary,
  enabledLaneOrder,
  isAudioProfileFixture,
  profileDiff,
  validateAudioProfile
} from './audio-profile-fixtures.mjs'
import {
  DEFAULT_SPEAKER_TEST_FIXTURE,
  SPEAKER_TEST_DAC_MAX,
  SPEAKER_TEST_FIXED_AT,
  SPEAKER_TEST_FIXTURES,
  SPEAKER_TEST_NOT_WIRED_REASON,
  SPEAKER_TEST_WINDOW_CLOSED_REASON,
  buildSpeakerTestAck,
  buildSpeakerTestSeed,
  buildSpeakerTestStatus,
  buildSpeakerTestStopAck,
  isSpeakerTestFixture,
  validateSpeakerTestRequest
} from './speaker-test-fixtures.mjs'

export const DEFAULT_MOCK_PORT = 8790

/** MOTU mock fixture（wired | degraded；degraded = detected:false + reason motu_not_detected，写 503）。 */
export const MOTU_FIXTURES = ['wired', 'degraded']
const MOTU_FIXED_CREATED_AT = '2026-11-26T07:20:00.000Z'
const MOTU_DB_FLOOR = -144.49
const MOTU_WRITE_APPLIES_TO = 'MOTU Main 1/Main 2 koBusFader (single-key write + readback, device write at commit only)'

export function isMotuFixture(value) {
  return typeof value === 'string' && MOTU_FIXTURES.includes(value)
}

const SESSION_TTL_MS = 8 * 60 * 60 * 1000
const SESSION_TTL_S = SESSION_TTL_MS / 1000
const MIN_PASSWORD_LENGTH = 8
const RATE_LIMIT_MAX_FAILURES = 5
const RATE_LIMIT_WINDOW_MS = 60 * 1000
const MAX_BODY_BYTES = 64 * 1024
const COOKIE_NAME = 'khaen_session'

function hashPassword(password) {
  return createHash('sha256').update('khaen-mock:' + password).digest('hex')
}

function randomToken() {
  return randomBytes(32).toString('hex')
}

export function createMockAuthServer(options = {}) {
  const sessions = new Map()
  const failuresByIp = new Map()
  let passwordHash = null
  let configured = false
  /** /api/status fixture name（present | stale | missing）；未知值回退 present。 */
  let statusFixture = isStatusFixture(options.statusFixture) ? options.statusFixture : DEFAULT_STATUS_FIXTURE
  /** /api/system fixture name（mismatch | consistent | degraded）；未知值回退 mismatch。 */
  let systemFixture = isSystemFixture(options.systemFixture) ? options.systemFixture : DEFAULT_SYSTEM_FIXTURE
  /** POST /api/system/restart 成功后：下一次 /info 把 rt-host 视作已被重启拉起（mock 状态，可复现）。 */
  let systemRestartState = { restartedAt: null, rtHostStarted: false }

  /* ------------------------------------------------------- 校准 REST mock 状态 */
  // DEV/TEST ONLY：内存态校准后端，用于前端联调（真实实现是 daemon + calibration backend，T36/T70h-b）。
  const mockNow = typeof options.now === 'function' ? options.now : () => Date.now()
  const maxUploadBytes = typeof options.maxUploadBytes === 'number' ? options.maxUploadBytes : 64 * 1024 * 1024
  let calibrationFixture = isCalibrationFixture(options.calibrationFixture)
    ? options.calibrationFixture
    : DEFAULT_CALIBRATION_FIXTURE
  /** true = 模拟 daemon 的 NotWired 后端（job/apply/artifact 503；wizard 真实部分可用）。 */
  let calibrationUnavailable = false
  /** upload_id(==sha256) -> {upload_id,sha256,bytes,content_type,role,created_at} */
  let calibrationUploads = new Map()
  /** job_id -> job record（含 mock 内部 artifactContent/ticks 字段，序列化时剔除）。 */
  let calibrationJobs = new Map()
  let calibrationJobSeq = 0
  /** 已保存的向导配置（null = 未保存）。 */
  let wizardConfig = null
  let wizardConfigUpdatedAt = null
  /** PUT /wizard 的可选 measurement 草稿（刷新后恢复第 5 步）。 */
  let wizardMeasurement = { baseline_upload_id: null, applied_upload_id: null, plan_sha: null }
  let appliedJobId = null
  let appliedSnapshot = null
  /**
   * apply/rollback 的两阶段 staging（docs/API.md §7：stage=prepare 只签一次性 token，commit 才生效）。
   * token -> {job_id, kind:'apply'|'rollback', plan_sha256, previous_version, target_version}
   */
  let calibrationStagedOps = new Map()

  function seedCalibration(fixture) {
    const seed = buildCalibrationSeed(fixture)
    calibrationFixture = fixture
    calibrationUnavailable = seed.unavailable === true
    calibrationUploads = new Map(seed.uploads.map((upload) => [upload.upload_id, { ...upload }]))
    calibrationJobs = new Map(seed.jobs.map((job) => [job.id, job]))
    calibrationJobSeq = seed.jobs.length
    wizardConfig = seed.config === null ? null : JSON.parse(JSON.stringify(seed.config))
    wizardConfigUpdatedAt = seed.config === null ? null : new Date(Date.parse('2026-11-26T06:03:00.000Z')).toISOString()
    const baseline = seed.uploads.find((upload) => upload.role === 'baseline') ?? null
    const applied = seed.uploads.find((upload) => upload.role === 'applied') ?? null
    wizardMeasurement = {
      baseline_upload_id: baseline === null ? null : baseline.upload_id,
      applied_upload_id: applied === null ? null : applied.upload_id,
      plan_sha: null
    }
    appliedJobId = seed.appliedJobId ?? null
    appliedSnapshot = null
    calibrationStagedOps = new Map()
  }
  seedCalibration(calibrationFixture)

  /* ------------------------------------------------------- M3 控制面 mock 状态 */
  // DEV/TEST ONLY：内存态控制面（mute / surface volume|mode|format / commit / abort / device retry），
  // 语义镜像 crates/host/src/control/mod.rs（T70f-b 之后 daemon 转发 rt-host 的同一套 op）。
  // wired=false 时所有写操作返回 503 host_link_unavailable（页面必须如实显示，不得假成功）。
  let controlFixture = isControlFixture(options.controlFixture) ? options.controlFixture : DEFAULT_CONTROL_FIXTURE
  let control = buildControlSeed(controlFixture)
  /** 单 pending 草稿槽（prepare 覆盖旧草稿；commit/abort 消费）。 */
  let controlPending = null
  /** 控制面版本号递增（与 rt-host 的 actual_version 语义一致）。 */
  let controlOperationSeq = 0

  function seedControl(fixture) {
    controlFixture = fixture
    control = buildControlSeed(fixture)
    controlPending = null
    controlOperationSeq = 0
  }
  seedControl(controlFixture)

  /* ------------------------------------------- M4 方案/声道 mock 状态 */
  // DEV/TEST ONLY：/api/channels（读 + 两阶段写）、/api/plan/upload|prepare|history|inspect、/api/rollback。
  // fixture：
  // - 内置 4 个（wired | partial | degraded | unavailable，scripts/channels-fixtures.mjs）；
  // - 扩展 3 个（仅 mock 接受，用于浏览器演练 daemon 的真实失败/降级形态，见 docs/API.md §5/§1）：
  //   mapping-only：prepare 一律返回 commit_supported=false 的 mapping 草稿（mapping 属方案 routing，
  //                 channels_commit 不能改，正确行为是禁用 commit + 逐条 blockers）；
  //   reconfigure-failure：commit 返回 502 ERR_RECONFIGURE_DEVICE + details.rolled_back=true（不假成功）；
  //   host-link-unavailable：GET 200 但现场字段 null + reasons，PUT 任何 stage 503 host_link_unavailable。
  let channelsFixture = DEFAULT_CHANNELS_FIXTURE
  let channelsSeed = buildChannelsSeed(DEFAULT_CHANNELS_FIXTURE)
  /** 扩展 fixture 的运行模式：normal | mapping-only | reconfigure-failure | host-link-unavailable。 */
  let channelsMode = 'normal'
  /** PUT /api/channels 的 pending 草稿槽（rt-host channels 两阶段自己的 token，与控制面草稿槽分开）。 */
  let channelsPending = null
  /** 数据面重配 generation（commit 成功后 +1；null = 拿不到）。 */
  let channelsGeneration = null
  /** sha256(plan bytes) -> 上传记录（幂等）。 */
  let planUploads = new Map()

  const CHANNELS_EXTRA_FIXTURES = ['mapping-only', 'reconfigure-failure', 'host-link-unavailable']

  function resolveChannelsFixture(name) {
    if (isChannelsFixture(name)) {
      return { fixture: name, base: name, mode: 'normal' }
    }
    if (typeof name === 'string' && CHANNELS_EXTRA_FIXTURES.includes(name)) {
      return { fixture: name, base: 'wired', mode: name }
    }
    return null
  }

  function seedM4(name) {
    const resolved = resolveChannelsFixture(name) ?? resolveChannelsFixture(DEFAULT_CHANNELS_FIXTURE)
    channelsFixture = resolved.fixture
    channelsMode = resolved.mode
    channelsSeed = buildChannelsSeed(resolved.base)
    channelsGeneration = channelsSeed.channels === null ? null : channelsSeed.channels.generation
    channelsPending = null
    planUploads = new Map()
    controlPending = null
  }
  seedM4(options.channelsFixture)

  /* ------------------------------------------------------- MOTU 设备 mock 状态 */
  // DEV/TEST ONLY：GET /api/motu（恒 200）+ PUT/POST /api/motu/volume 两阶段（prepare -> commit；**没有 abort**）。
  // fixture：
  // - wired：检测到 UltraLite-mk5、USB-CDC 控制网可达、Main 1/2 真值回读、逐通道单键写 + 回读可用；
  // - degraded：未检测到（reason=motu_not_detected）：GET 200 + mute/vol/state=null + reasons，
  //   写端点 503 motu_not_detected（不尝试写、不假成功）。
  // 所有时间戳/初始音量固定（不依赖当前时间/随机）。
  /** fader linear -> dB（0/-inf -> null；与 crates/motu-cuemix/src/fader.rs 一致）。 */
  function motuFaderToDb(linear) {
    if (typeof linear !== 'number' || !Number.isFinite(linear) || linear <= 0) {
      return null
    }
    return 20 * Math.log10(linear)
  }

  /** 8.24 量化（与设备一致：写入值 == 回读值）。 */
  function motuQuantizeLinear(linear) {
    return Math.round(linear * 16777216) / 16777216
  }

  function motuMainChannels() {
    return [
      { channel: 1, name: 'Main 1', datastore: { key: 'koBusFader', index: 0 } },
      { channel: 2, name: 'Main 2', datastore: { key: 'koBusFader', index: 1 } }
    ]
  }

  function buildMotuSeed(fixture) {
    const name = isMotuFixture(fixture) ? fixture : 'wired'
    const channels = motuMainChannels()
    if (name === 'degraded') {
      return {
        fixture: name,
        detected: false,
        model: null,
        card_id: null,
        card_index: null,
        control_reachable: false,
        reason: 'motu_not_detected',
        reason_detail:
          'no MOTU card was found in /proc/asound/cards: MOTU control is unavailable on this host (this is not an error)',
        mute: null,
        vol: null,
        state: null,
        draft: null,
        write_seq: 0
      }
    }
    return {
      fixture: name,
      detected: true,
      model: 'UltraLite-mk5',
      card_id: 'hw:CARD=UltraLitemk5,DEV=0',
      card_index: 1,
      control_reachable: true,
      reason: 'detected',
      reason_detail:
        'MOTU control is available: a MOTU card is present and the USB-CDC control network answered the TCP probe',
      mute: false,
      vol: [
        { ...channels[0], linear: 1, db: 0 },
        { ...channels[1], linear: 0.5, db: motuFaderToDb(0.5) }
      ],
      state: {
        sample_rate_hz: 48000,
        main_trim: 0,
        main_bus_mute: false,
        device: {
          model_id: 'UltraLite-mk5',
          name: 'MOTU UltraLite mk5',
          version: '1.6.6',
          api_version: 2
        },
        missing: []
      },
      draft: null,
      write_seq: 0
    }
  }

  let motuFixture = isMotuFixture(options.motuFixture) ? options.motuFixture : 'wired'
  let motu = buildMotuSeed(motuFixture)

  function seedMotu(fixture) {
    motuFixture = isMotuFixture(fixture) ? fixture : 'wired'
    motu = buildMotuSeed(motuFixture)
  }

  /* --------------------------------------------- 音频 profile / 测试音 mock 状态 */
  // DEV/TEST ONLY：/api/audio-profile（GET/PUT 两阶段）、/api/audio-profile/plan、/api/device/inputs、
  // /api/speaker-test(+stop)。fixture 语义见 scripts/audio-profile-fixtures.mjs / speaker-test-fixtures.mjs。
  let audioProfileFixture = isAudioProfileFixture(options.audioProfileFixture)
    ? options.audioProfileFixture
    : DEFAULT_AUDIO_PROFILE_FIXTURE
  let audioProfileSeed = buildAudioProfileSeed(audioProfileFixture)
  /** 已保存的 profile（null = 未接线/未保存）与其 revision。 */
  let audioProfileSaved = audioProfileSeed.profile
  let audioProfileRevision = audioProfileSeed.revision
  /** 当前**已生效**方案的摘要（GET 快照 plan 段；只有 profile 方案经 /api/commit 生效后才更新）。 */
  let audioProfileAppliedPlan = { ...audioProfileSeed.plan }
  /** 最近一次 POST /api/audio-profile/plan 生成的方案（等 /api/commit 后并入 appliedPlan）。 */
  let audioProfileGeneratedPlan = null
  /** PUT /api/audio-profile 的 pending 草稿槽（单槽；commit/abort 消费）。 */
  let audioProfilePending = null

  function seedAudioProfile(fixture) {
    audioProfileFixture = isAudioProfileFixture(fixture) ? fixture : DEFAULT_AUDIO_PROFILE_FIXTURE
    audioProfileSeed = buildAudioProfileSeed(audioProfileFixture)
    audioProfileSaved = audioProfileSeed.profile
    audioProfileRevision = audioProfileSeed.revision
    audioProfileAppliedPlan = { ...audioProfileSeed.plan }
    audioProfileGeneratedPlan = null
    audioProfilePending = null
  }

  let speakerTestFixture = isSpeakerTestFixture(options.speakerTestFixture)
    ? options.speakerTestFixture
    : DEFAULT_SPEAKER_TEST_FIXTURE
  let speakerTest = buildSpeakerTestSeed(speakerTestFixture)

  function seedSpeakerTest(fixture) {
    speakerTestFixture = isSpeakerTestFixture(fixture) ? fixture : DEFAULT_SPEAKER_TEST_FIXTURE
    speakerTest = buildSpeakerTestSeed(speakerTestFixture)
  }

  function controlUnavailable(res) {
    sendError(
      res,
      503,
      'host_link_unavailable',
      'rt-host control channel is unavailable (mock fixture: ' + controlFixture + ')',
      { reason: control.reason }
    )
  }

  /** 稳定 token（mock 用；真实 daemon 用版本号 + 方案 sha 前 8 字节，见 host control::engine_token）。 */
  function stableControlToken(kind, version, payload) {
    return createHash('sha256')
      .update('khaen-mock-control:' + kind + ':' + String(version) + ':' + JSON.stringify(payload))
      .digest('hex')
      .slice(0, 32)
  }

  function prepareVolumeSurface(body) {
    const data = body.data === undefined ? {} : body.data
    if (typeof data !== 'object' || data === null || Array.isArray(data)) {
      return { status: 400, code: 'bad_request', message: 'field data must be a json object' }
    }
    if (control.basePlan === null) {
      return {
        status: 409,
        code: 'no_base_plan',
        message: 'no committed plan to patch: prepare + commit a plan first'
      }
    }
    const gains = data.gains_db
    if (!Array.isArray(gains) || gains.length === 0) {
      return {
        status: 400,
        code: 'bad_request',
        message: 'field data.gains_db must be an array of {lane, gain_db}'
      }
    }
    const parsed = []
    for (const item of gains) {
      if (typeof item !== 'object' || item === null || Array.isArray(item)) {
        return { status: 400, code: 'bad_request', message: 'data.gains_db entries must be objects' }
      }
      if (!Number.isInteger(item.lane) || item.lane < 0) {
        return { status: 400, code: 'bad_request', message: 'field lane must be a non-negative integer' }
      }
      if (typeof item.gain_db !== 'number' || !Number.isFinite(item.gain_db)) {
        return { status: 400, code: 'bad_request', message: 'field gain_db must be a finite number' }
      }
      if (item.lane >= control.format.channels) {
        return {
          status: 400,
          code: 'bad_request',
          message: 'protection has no lane ' + String(item.lane)
        }
      }
      parsed.push({ lane: item.lane, gain_db: item.gain_db })
    }
    const version = control.version + 1
    const nextGains = control.gains_db.slice()
    for (const item of parsed) {
      nextGains[item.lane] = item.gain_db
    }
    const graphId = graphIdForVersion(version)
    const token = stableControlToken('volume', version, parsed)
    const response = {
      ok: true,
      surface: 'volume',
      engine_token: token,
      planned_version: version,
      graph_id: graphId,
      admission: { accepted: true, budget: {} }
    }
    if (controlPending !== null && controlPending.token === token) {
      return { response: controlPending.response }
    }
    controlPending = {
      kind: 'volume',
      token,
      version,
      response,
      gains_db: nextGains,
      graph_id: graphId
    }
    return { response }
  }

  /** 语义界面 routing：routes=[[lane,dac],...] 完整替换表 + 可选 dac_channels（CONTRACTS B.1.2）。 */
  function prepareRoutingSurface(body) {
    const data = body.data === undefined ? {} : body.data
    if (typeof data !== 'object' || data === null || Array.isArray(data)) {
      return { status: 400, code: 'bad_request', message: 'field data must be a json object' }
    }
    if (control.basePlan === null) {
      return {
        status: 409,
        code: 'no_base_plan',
        message: 'no committed plan to patch: prepare + commit a plan first'
      }
    }
    const routes = data.routes
    if (!Array.isArray(routes) || routes.length === 0) {
      return { status: 400, code: 'bad_request', message: 'field data.routes must be a non-empty array of [lane, dac]' }
    }
    const parsed = []
    const seenLanes = new Set()
    for (const item of routes) {
      if (!Array.isArray(item) || item.length !== 2) {
        return { status: 400, code: 'bad_request', message: 'data.routes entries must be [lane, dac] arrays' }
      }
      const lane = item[0]
      const dac = item[1]
      if (!Number.isInteger(lane) || lane < 0) {
        return { status: 400, code: 'bad_request', message: 'route lane must be a non-negative integer' }
      }
      if (!Number.isInteger(dac) || dac < 0) {
        return { status: 400, code: 'bad_request', message: 'route dac must be a non-negative integer' }
      }
      if (seenLanes.has(lane)) {
        return { status: 400, code: 'bad_request', message: 'data.routes must not route the same lane twice' }
      }
      seenLanes.add(lane)
      parsed.push([lane, dac])
    }
    let dacChannels = control.format.channels ?? 22
    if (data.dac_channels !== undefined) {
      if (!Number.isInteger(data.dac_channels) || data.dac_channels <= 0 || data.dac_channels > 32) {
        return { status: 400, code: 'bad_request', message: 'field data.dac_channels must be an integer in 1..32' }
      }
      dacChannels = data.dac_channels
    }
    for (const [lane, dac] of parsed) {
      if (dac >= dacChannels) {
        return {
          status: 400,
          code: 'bad_request',
          message: 'route lane ' + String(lane) + ' -> dac ' + String(dac) + ' is outside dac_channels ' + String(dacChannels)
        }
      }
    }
    const version = control.version + 1
    const graphId = graphIdForVersion(version)
    const token = stableControlToken('routing', version, { parsed, dacChannels })
    const response = {
      ok: true,
      surface: 'routing',
      engine_token: token,
      planned_version: version,
      graph_id: graphId,
      admission: { accepted: true, budget: {} }
    }
    if (controlPending !== null && controlPending.token === token) {
      return { response: controlPending.response }
    }
    controlPending = { kind: 'routing', token, version, response, routes: parsed, dacChannels, graph_id: graphId }
    return { response }
  }

  /** 语义界面 layout：{in_channels, out_lanes, folds:[[src,dst,gain],...]} 完整替换表。 */
  function prepareLayoutSurface(body) {
    const data = body.data === undefined ? {} : body.data
    if (typeof data !== 'object' || data === null || Array.isArray(data)) {
      return { status: 400, code: 'bad_request', message: 'field data must be a json object' }
    }
    if (control.basePlan === null) {
      return {
        status: 409,
        code: 'no_base_plan',
        message: 'no committed plan to patch: prepare + commit a plan first'
      }
    }
    const inChannels = data.in_channels
    const outLanes = data.out_lanes
    if (!Number.isInteger(inChannels) || inChannels < 1 || inChannels > 64) {
      return { status: 400, code: 'bad_request', message: 'field data.in_channels must be an integer in 1..64' }
    }
    if (!Number.isInteger(outLanes) || outLanes < 1 || outLanes > 32) {
      return { status: 400, code: 'bad_request', message: 'field data.out_lanes must be an integer in 1..32' }
    }
    const folds = data.folds
    if (!Array.isArray(folds) || folds.length === 0) {
      return { status: 400, code: 'bad_request', message: 'field data.folds must be a non-empty array of [src, dst, gain]' }
    }
    const parsed = []
    for (const item of folds) {
      if (!Array.isArray(item) || item.length !== 3) {
        return { status: 400, code: 'bad_request', message: 'data.folds entries must be [src, dst, gain] arrays' }
      }
      const src = item[0]
      const dst = item[1]
      const gain = item[2]
      if (!Number.isInteger(src) || src < 0 || !Number.isInteger(dst) || dst < 0) {
        return { status: 400, code: 'bad_request', message: 'fold src/dst must be non-negative integers' }
      }
      if (typeof gain !== 'number' || !Number.isFinite(gain)) {
        return { status: 400, code: 'bad_request', message: 'fold gain must be a finite number' }
      }
      if (src >= inChannels) {
        return { status: 400, code: 'bad_request', message: 'fold src ' + String(src) + ' is outside in_channels ' + String(inChannels) }
      }
      if (dst >= outLanes) {
        return { status: 400, code: 'bad_request', message: 'fold dst ' + String(dst) + ' is outside out_lanes ' + String(outLanes) }
      }
      parsed.push([src, dst, gain])
    }
    const version = control.version + 1
    const graphId = graphIdForVersion(version)
    const token = stableControlToken('layout', version, { inChannels, outLanes, parsed })
    const response = {
      ok: true,
      surface: 'layout',
      engine_token: token,
      planned_version: version,
      graph_id: graphId,
      admission: { accepted: true, budget: {} }
    }
    if (controlPending !== null && controlPending.token === token) {
      return { response: controlPending.response }
    }
    controlPending = {
      kind: 'layout',
      token,
      version,
      response,
      in_channels: inChannels,
      out_lanes: outLanes,
      folds: parsed,
      graph_id: graphId
    }
    return { response }
  }

  function prepareModeSurface(body) {
    const data = body.data === undefined ? {} : body.data
    if (typeof data !== 'object' || data === null || Array.isArray(data)) {
      return { status: 400, code: 'bad_request', message: 'field data must be a json object' }
    }
    const raw = data.mode
    const target = raw === 'on' ? 'aes67' : raw
    if (target !== 'off' && target !== 'aes67') {
      return {
        status: 400,
        code: 'bad_request',
        message:
          'mode ' + JSON.stringify(raw) +
          ' is not wired: supported are off/on/aes67 (toslink/dante/analog input sources are not implemented)'
      }
    }
    const version = control.version + 1
    const changed = target !== control.mode
    const token = stableControlToken('mode', version, target)
    const response = {
      ok: true,
      surface: 'mode',
      engine_token: token,
      planned_version: version,
      mode: target,
      changed,
      applies_to: 'control-plane engine (output enable)',
      note: 'real capture-source switching belongs to rt-host/device supervisor; toslink/dante are not implemented'
    }
    if (controlPending !== null && controlPending.token === token) {
      return { response: controlPending.response }
    }
    controlPending = { kind: 'mode', token, version, response, mode: target, changed }
    return { response }
  }

  function prepareFormatBody(body) {
    if (typeof body !== 'object' || body === null || Array.isArray(body)) {
      return { status: 400, code: 'bad_request', message: 'body must be a json object' }
    }
    const target = { ...control.format }
    let requested = false
    if (body.sample_rate !== undefined) {
      if (!Number.isInteger(body.sample_rate) || ![48000, 96000].includes(body.sample_rate)) {
        return { status: 400, code: 'bad_request', message: 'sample_rate must be one of [48000, 96000]' }
      }
      target.sample_rate = body.sample_rate
      requested = true
    }
    if (body.chunksize !== undefined) {
      if (!Number.isInteger(body.chunksize) || ![64, 128].includes(body.chunksize)) {
        return { status: 400, code: 'bad_request', message: 'chunksize must be one of [64, 128]' }
      }
      target.block_size = body.chunksize
      requested = true
    }
    if (!requested) {
      return { status: 400, code: 'bad_request', message: 'at least one of sample_rate / chunksize is required' }
    }
    const version = control.version + 1
    const changed =
      target.sample_rate !== control.format.sample_rate || target.block_size !== control.format.block_size
    const token = stableControlToken('format', version, target)
    const response = {
      ok: true,
      engine_token: token,
      planned_version: version,
      format: target,
      changed,
      applies_to: 'control-plane engine',
      device_restart_required: true,
      note:
        'format change rebuilds the control-plane engine and drops the running graph; a plan matching the new format must be prepared again (a real device requires reopening the PCM stream)'
    }
    if (controlPending !== null && controlPending.token === token) {
      return { response: controlPending.response }
    }
    controlPending = { kind: 'format', token, version, response, target, changed }
    return { response }
  }

  /** 消费 pending 草稿并返回 commit 回执（volume / routing / layout / mode / format / plan / rollback）。 */
  function commitControlDraft() {
    const pending = controlPending
    if (pending === null) {
      return { status: 409, code: 'engine_token_invalid', message: 'no prepared draft: call prepare before commit' }
    }
    controlPending = null
    controlOperationSeq += 1
    control.version = pending.version
    if (pending.kind === 'volume') {
      control.gains_db = pending.gains_db
      control.basePlan = {
        ...control.basePlan,
        graph_id: pending.graph_id,
        applied_at: '2026-11-26T07:10:00.000Z'
      }
      control.history.push({
        version: pending.version,
        graph_id: pending.graph_id,
        sha256: null,
        latency_samples: control.basePlan.latency_samples,
        applied_at: control.basePlan.applied_at,
        source: 'upload'
      })
      if (control.history.length > 16) {
        control.history.shift()
      }
      return {
        response: {
          ok: true,
          applied_block: pending.version * 128 + 1,
          version: pending.version,
          all_ack: true,
          graph_id: pending.graph_id
        }
      }
    }
    if (pending.kind === 'mode') {
      control.mode = pending.mode
      control.muted = pending.mode === 'off'
      return {
        response: {
          ok: true,
          changed: pending.changed,
          applied_block: pending.changed ? pending.version * 128 + 1 : null,
          version: pending.version,
          mode: control.mode
        }
      }
    }
    if (pending.kind === 'format') {
      const wasMuted = control.muted
      let droppedHistory = false
      if (pending.changed) {
        control.format = { ...pending.target }
        // daemon 语义（crates/host/src/control/mod.rs commit_format）：格式变化重建执行器并丢弃旧格式下的
        // 方案与历史；副本格式的旧方案对新格式不再合法，如实暴露给页面（后续 surface 补丁会 409 no_base_plan）。
        control.basePlan = null
        droppedHistory = control.history.length > 0
        control.history = []
      }
      return {
        response: {
          ok: true,
          changed: pending.changed,
          applied_block: pending.changed ? pending.version * 128 + 1 : null,
          version: pending.version,
          format: control.format,
          all_ack: true,
          graph_dropped: pending.changed,
          history_cleared: droppedHistory,
          mute_preserved: wasMuted,
          device_restart_required: true
        }
      }
    }
    if (pending.kind === 'routing') {
      const appliedAt = '2026-11-26T07:15:00.000Z'
      const graphId = pending.graph_id
      control.basePlan = {
        graph_id: graphId,
        sha256: null,
        latency_samples: 1023,
        applied_at: appliedAt,
        source: 'surface:routing'
      }
      control.history.push({
        version: pending.version,
        graph_id: graphId,
        sha256: null,
        latency_samples: 1023,
        applied_at: appliedAt,
        source: 'surface:routing'
      })
      if (control.history.length > 16) {
        control.history.shift()
      }
      // 方案 routing 已生效：/api/channels 的 mapping / output.dac_map 跟随（daemon 的方案 routing 镜像）。
      if (channelsSeed.channels !== null) {
        const channels = channelsSeed.channels
        const mapping = pending.routes
          .map(([lane, dac]) => ({ lane, dac }))
          .sort((left, right) => left.lane - right.lane || left.dac - right.dac)
        const dacMap = new Array(pending.dacChannels).fill(null)
        for (const [lane, dac] of pending.routes) {
          if (dac < dacMap.length) {
            dacMap[dac] = lane
          }
        }
        channels.mapping = mapping
        channels.output = {
          channels: pending.dacChannels,
          dac_map: dacMap,
          channels_source: 'plan-routing'
        }
        channels.plan = {
          ...channels.plan,
          version: pending.version,
          graph_id: graphId,
          routing_known: true,
          routing_invalidated: null
        }
        if (channels.format !== null) {
          channels.format = { ...channels.format, channels: pending.dacChannels }
        }
        if (channels.current !== null) {
          channels.current = { ...channels.current, channels: pending.dacChannels }
        }
        channels.reasons = { ...channels.reasons }
        delete channels.reasons.mapping
        delete channels.reasons['output.dac_map']
        channels.not_wired = (Array.isArray(channels.not_wired) ? channels.not_wired : []).filter(
          (entry) => entry.path !== 'channels.hot_reconfigure.routing'
        )
      }
      return {
        response: {
          ok: true,
          applied_block: pending.version * 128 + 1,
          version: pending.version,
          all_ack: true,
          graph_id: graphId
        }
      }
    }
    if (pending.kind === 'layout') {
      const appliedAt = '2026-11-26T07:16:00.000Z'
      const graphId = pending.graph_id
      control.basePlan = {
        graph_id: graphId,
        sha256: null,
        latency_samples: 1023,
        applied_at: appliedAt,
        source: 'surface:layout'
      }
      control.history.push({
        version: pending.version,
        graph_id: graphId,
        sha256: null,
        latency_samples: 1023,
        applied_at: appliedAt,
        source: 'surface:layout'
      })
      if (control.history.length > 16) {
        control.history.shift()
      }
      return {
        response: {
          ok: true,
          applied_block: pending.version * 128 + 1,
          version: pending.version,
          all_ack: true,
          graph_id: graphId
        }
      }
    }
    // plan / rollback：换入方案（版本号递增；rollback 也是新版本，历史不改写）。
    control.basePlan = {
      graph_id: pending.graph_id,
      sha256: pending.sha256,
      latency_samples: 1023,
      applied_at: '2026-11-26T07:20:00.000Z',
      source: pending.kind === 'rollback' ? 'rollback' : 'upload'
    }
    // audio-profile 生成的方案真正 commit 后，GET /api/audio-profile 的 plan 摘要才更新（生效以回执为准）。
    if (audioProfileGeneratedPlan !== null && pending.sha256 === audioProfileGeneratedPlan.sha256) {
      audioProfileAppliedPlan = {
        lanes: audioProfileGeneratedPlan.lanes,
        enabled_outputs: audioProfileGeneratedPlan.lanes,
        dac_channels: audioProfileGeneratedPlan.dac_channels,
        graph_id: audioProfileGeneratedPlan.graph_id
      }
      audioProfileGeneratedPlan = null
    }
    control.history.push({
      version: pending.version,
      graph_id: pending.graph_id,
      sha256: pending.sha256,
      latency_samples: 1023,
      applied_at: control.basePlan.applied_at,
      source: control.basePlan.source
    })
    if (control.history.length > 16) {
      control.history.shift()
    }
    if (channelsSeed !== null) {
      // 已生效方案变化后，inspect 预览跟随（真实 daemon 的 /api/plan/inspect 读当前已生效方案）。
      channelsSeed.inspect = {
        ok: true,
        sha256: pending.sha256,
        graph_id: pending.graph_id,
        plan: {
          block_size: control.format.block_size,
          sample_rate: control.format.sample_rate,
          in_channels: 2,
          lanes: 22,
          dac_channels: channelsSeed.channels === null ? null : channelsSeed.channels.output.channels
        },
        admission: { accepted: true, rejections: [], budget: {} },
        compile: { compiled: true, latency_samples: 1023 }
      }
    }
    return {
      response: {
        ok: true,
        applied_block: pending.version * 128 + 1,
        version: pending.version,
        all_ack: true,
        graph_id: pending.graph_id
      }
    }
  }

  /* ----------------------------------------------------- M4 方案/声道 handler */

  function m4Unavailable(res) {
    sendError(res, 501, 'not_wired', channelsSeed.reason, {
      reason: channelsSeed.reason,
      task: 'T55'
    })
  }

  /** ABI v3 的五个可选区段名（crates/daemon/src/plan_store.rs section_names）。 */
  const PLAN_SECTIONS = ['content', 'crossover', 'matrix', 'protection', 'routing']

  /** 上传方案的 mock 落盘路径（真实 daemon 是 <KHAEN_STATE_DIR>/plans/<sha256>.plan）。 */
  function planStoredPath(sha) {
    return '/var/lib/khaen/plans/' + sha + '.plan'
  }

  /** 从上传字节派生稳定的方案元数据（mock 用；真实 daemon 解码 ABI v3）。 */
  function planFacts(sha, bytes) {
    const segmentCount = 1 + (bytes.length % PLAN_SECTIONS.length)
    return {
      graph_id: sha,
      segments: PLAN_SECTIONS.slice(0, segmentCount),
      latency_samples: 1023 + (Number.parseInt(sha.slice(0, 2), 16) % 17),
      sample_rate: control.format.sample_rate,
      block_size: control.format.block_size
    }
  }

  /** 按 stored_path 查已上传的 plan（POST /api/plan/inspect 与 /api/prepare 的 plan_path）。 */
  function findUploadByStoredPath(storedPath) {
    for (const upload of planUploads.values()) {
      if (upload.stored_path === storedPath) {
        return upload
      }
    }
    return undefined
  }

  /** GET /api/plan/history：daemon 真实形态（current 对象 + entries，最新在前，时间字段 recorded_at）。 */
  function planHistoryJson() {
    const entries = control.history
      .map((entry) => ({
        version: entry.version,
        graph_id: entry.graph_id ?? null,
        applied_block: entry.applied_block ?? null,
        recorded_at: entry.applied_at ?? null,
        source: entry.source ?? null,
        sha256: entry.sha256 ?? null,
        latency_samples: entry.latency_samples ?? null,
        // daemon 亲自记录过的 commit 回执才有 observed:true（plan_api.rs commit_entry）。
        observed: true
      }))
      .sort((left, right) => right.version - left.version)
    return {
      schema: 'khaen-plan-history/v1',
      source: 'daemon-process',
      current: {
        version: control.version,
        graph_id: control.basePlan === null ? null : control.basePlan.graph_id
      },
      entries,
      pagination: { limit: 20, offset: 0, total: entries.length, has_more: false },
      reasons: {}
    }
  }

  /* ------------------------------------------------ /api/channels 两阶段（rt-host） */

  /** rt-host 图被格式变化丢弃时的稳定原因（与 crates/daemon/src/channels.rs 同义，不编造）。 */
  const CHANNELS_GRAPH_DROPPED_REASON =
    'the engine format (channels/sample rate/block) changes, so the running graph is compiled for the old format; it is dropped at commit and a matching plan must be prepared+committed again'
  const CHANNELS_HOST_LINK_REASON =
    'the daemon -> rt-host control channel is unavailable (mock fixture: host-link-unavailable): the channels status cannot be read and no reconfiguration can be prepared or committed'

  /** §12.2 加法字段：dac -> canonical 角色（来源 = GET /api/audio-profile 的已保存 profile）。 */
  function channelsSpeakerRoles() {
    if (audioProfileSaved === null) {
      return null
    }
    return audioProfileSaved.outputs.map((output) => ({
      dac: output.dac,
      role: output.role,
      enabled: output.enabled === true
    }))
  }

  /** §12.2 加法字段：输入源每通道的角色（来源同上；未接线为 null，不编造）。 */
  function channelsInputRoles() {
    if (audioProfileSaved === null) {
      return null
    }
    const roles = []
    const sources = Array.isArray(audioProfileSaved.inputs?.sources) ? audioProfileSaved.inputs.sources : []
    for (const source of sources) {
      if (source === null || typeof source !== 'object') {
        continue
      }
      for (const channel of Array.isArray(source.channels) ? source.channels : []) {
        if (channel === null || typeof channel !== 'object') {
          continue
        }
        roles.push({ source: source.kind, channel: channel.channel, role: channel.role })
      }
    }
    return roles
  }

  /** GET /api/channels 的正文（恒 200；host-link-unavailable 时现场字段 null + reasons）。 */
  function buildChannelsView() {
    if (channelsMode === 'host-link-unavailable') {
      const document = buildChannelsSeed('degraded').channels
      document.reasons = {
        ...document.reasons,
        'channels.hot_reconfigure': CHANNELS_HOST_LINK_REASON,
        'output.channels': CHANNELS_HOST_LINK_REASON
      }
      document.not_wired = [{ path: 'channels.hot_reconfigure', reason: CHANNELS_HOST_LINK_REASON }]
      document.speaker_roles = channelsSpeakerRoles()
      document.input_roles = channelsInputRoles()
      return document
    }
    const document = JSON.parse(JSON.stringify(channelsSeed.channels))
    document.speaker_roles = channelsSpeakerRoles()
    document.input_roles = channelsInputRoles()
    return document
  }

  const CHANNELS_PATCH_FIELDS = [
    'channels',
    'sample_rate',
    'aes67_group',
    'aes67_port',
    'aes67_payload_type',
    'aes67_channels',
    'aes67_map',
    'input_channels',
    'input_map',
    'jitter_frames',
    'mapping',
    'dac_map',
    'output',
    'input'
  ]
  const CHANNELS_SAMPLE_RATES = [44100, 48000, 96000]
  const CHANNELS_MAX_OUTPUT = 22
  const CHANNELS_MAX_AES67 = 8
  const CHANNELS_MAX_MAPPING_ENTRIES = 128

  function channelsFail(field, message, code = 'bad_request') {
    return { ok: false, status: 400, code, message, details: { field } }
  }

  function isMulticastIpv4(value) {
    if (typeof value !== 'string') {
      return false
    }
    const parts = value.trim().split('.')
    if (parts.length !== 4) {
      return false
    }
    const octets = []
    for (const part of parts) {
      if (!/^[0-9]{1,3}$/.test(part)) {
        return false
      }
      const octet = Number(part)
      if (!Number.isInteger(octet) || octet < 0 || octet > 255) {
        return false
      }
      octets.push(octet)
    }
    return octets[0] >= 224 && octets[0] <= 239
  }

  /** 解析 PUT /api/channels 的 prepare 补丁（daemon 字段 + 旧 output/input 嵌套别名）。 */
  function parseChannelsPatch(body) {
    for (const key of Object.keys(body)) {
      if (key === 'stage' || key === 'action' || key === 'engine_token') {
        continue
      }
      if (!CHANNELS_PATCH_FIELDS.includes(key)) {
        return channelsFail(key, 'unknown field: ' + key)
      }
    }
    const patch = {}
    if (body.output !== undefined) {
      if (typeof body.output !== 'object' || body.output === null || Array.isArray(body.output)) {
        return channelsFail('output', 'field output must be an object')
      }
      if (body.output.channels !== undefined) {
        patch.channels = body.output.channels
      }
      if (body.output.dac_map !== undefined) {
        patch.dac_map = body.output.dac_map
      }
    }
    if (body.input !== undefined) {
      if (typeof body.input !== 'object' || body.input === null || Array.isArray(body.input)) {
        return channelsFail('input', 'field input must be an object')
      }
      if (body.input.channels !== undefined) {
        patch.input_channels = body.input.channels
      }
      if (body.input.map !== undefined) {
        patch.input_map = body.input.map
      }
    }
    for (const field of [
      'channels',
      'sample_rate',
      'aes67_group',
      'aes67_port',
      'aes67_payload_type',
      'aes67_channels',
      'aes67_map',
      'input_channels',
      'input_map',
      'jitter_frames',
      'mapping',
      'dac_map'
    ]) {
      if (body[field] !== undefined) {
        patch[field] = body[field]
      }
    }
    const targetChannels = patch.channels === undefined ? null : patch.channels
    if (targetChannels !== null) {
      if (!Number.isInteger(targetChannels) || targetChannels < 1 || targetChannels > CHANNELS_MAX_OUTPUT) {
        return channelsFail('channels', 'output channels must be an integer in 1..' + String(CHANNELS_MAX_OUTPUT))
      }
    }
    if (patch.sample_rate !== undefined && !CHANNELS_SAMPLE_RATES.includes(patch.sample_rate)) {
      return channelsFail('sample_rate', 'sample_rate must be one of ' + JSON.stringify(CHANNELS_SAMPLE_RATES))
    }
    if (patch.aes67_group !== undefined && !isMulticastIpv4(patch.aes67_group)) {
      return channelsFail('aes67_group', 'aes67_group must be an IPv4 multicast address (224.0.0.0/4)')
    }
    if (patch.aes67_port !== undefined) {
      if (!Number.isInteger(patch.aes67_port) || patch.aes67_port < 1 || patch.aes67_port > 65535) {
        return channelsFail('aes67_port', 'aes67_port must be an integer in 1..65535')
      }
    }
    if (patch.aes67_payload_type !== undefined) {
      if (!Number.isInteger(patch.aes67_payload_type) || patch.aes67_payload_type < 96 || patch.aes67_payload_type > 127) {
        return channelsFail('aes67_payload_type', 'aes67_payload_type must be an integer in 96..127')
      }
    }
    if (patch.aes67_channels !== undefined) {
      if (!Number.isInteger(patch.aes67_channels) || patch.aes67_channels < 1 || patch.aes67_channels > CHANNELS_MAX_AES67) {
        return channelsFail('aes67_channels', 'aes67_channels must be an integer in 1..' + String(CHANNELS_MAX_AES67))
      }
    }
    if (patch.aes67_map !== undefined) {
      if (!Array.isArray(patch.aes67_map)) {
        return channelsFail('aes67_map', 'aes67_map must be an array of output channels')
      }
      const expected =
        patch.aes67_channels ?? (channelsSeed.channels === null ? null : channelsSeed.channels.current?.aes67_channels ?? null)
      if (expected !== null && patch.aes67_map.length !== expected) {
        return channelsFail('aes67_map', 'aes67_map length must equal aes67_channels')
      }
      const seen = new Set()
      for (const slot of patch.aes67_map) {
        const upper = targetChannels === null ? Number.POSITIVE_INFINITY : targetChannels - 1
        if (!Number.isInteger(slot) || slot < 0 || slot > upper || seen.has(slot)) {
          return channelsFail('aes67_map', 'aes67_map values must be unique integers 0..channels-1')
        }
        seen.add(slot)
      }
    }
    if (patch.jitter_frames !== undefined) {
      if (!Number.isInteger(patch.jitter_frames) || patch.jitter_frames < 0) {
        return channelsFail('jitter_frames', 'jitter_frames must be a non-negative integer')
      }
    }
    if (patch.input_channels !== undefined) {
      if (!Number.isInteger(patch.input_channels) || patch.input_channels < 1 || patch.input_channels > CHANNELS_MAX_AES67) {
        return channelsFail('input_channels', 'input channels must be an integer in 1..' + String(CHANNELS_MAX_AES67))
      }
    }
    if (patch.input_map !== undefined) {
      if (!Array.isArray(patch.input_map)) {
        return channelsFail('input_map', 'input_map must be an array of capture channels')
      }
      const expected =
        patch.input_channels ??
        (channelsSeed.channels === null ? null : channelsSeed.channels.input.channels ?? null)
      if (expected !== null && patch.input_map.length !== expected) {
        return channelsFail('input_map', 'input_map length must equal the input channel count')
      }
      const seen = new Set()
      for (const channel of patch.input_map) {
        if (!Number.isInteger(channel) || channel < 0 || channel > 63 || seen.has(channel)) {
          return channelsFail('input_map', 'input_map values must be unique integers 0..63')
        }
        seen.add(channel)
      }
    }
    if (patch.dac_map !== undefined) {
      if (!Array.isArray(patch.dac_map)) {
        return channelsFail('dac_map', 'dac_map must be an array of lane numbers (null = no lane)')
      }
      const expected =
        targetChannels ?? (channelsSeed.channels === null ? null : channelsSeed.channels.output.channels ?? null)
      if (expected !== null && patch.dac_map.length !== expected) {
        return channelsFail('dac_map', 'dac_map length must equal the output channel count')
      }
      const seen = new Set()
      for (const lane of patch.dac_map) {
        if (lane === null) {
          continue
        }
        if (!Number.isInteger(lane) || lane < 0 || seen.has(lane)) {
          return channelsFail('dac_map', 'dac_map values must be unique non-negative lane numbers or null')
        }
        seen.add(lane)
      }
    }
    if (patch.mapping !== undefined) {
      if (!Array.isArray(patch.mapping)) {
        return channelsFail('mapping', 'mapping must be an array of {lane, dac}')
      }
      if (patch.mapping.length > CHANNELS_MAX_MAPPING_ENTRIES) {
        return channelsFail('mapping', 'mapping has too many entries (max ' + String(CHANNELS_MAX_MAPPING_ENTRIES) + ')')
      }
      const lanes = new Set()
      const dacs = new Set()
      for (const entry of patch.mapping) {
        if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) {
          return channelsFail('mapping', 'mapping entries must be {lane, dac} objects')
        }
        if (!Number.isInteger(entry.lane) || entry.lane < 0 || !Number.isInteger(entry.dac) || entry.dac < 0) {
          return channelsFail('mapping', 'mapping lane/dac must be non-negative integers')
        }
        if (lanes.has(entry.lane) || dacs.has(entry.dac)) {
          return channelsFail('mapping', 'mapping must not repeat lanes or DACs')
        }
        lanes.add(entry.lane)
        dacs.add(entry.dac)
      }
    }
    const rtFields = [
      'channels',
      'sample_rate',
      'aes67_group',
      'aes67_port',
      'aes67_payload_type',
      'aes67_channels',
      'aes67_map',
      'input_channels',
      'input_map',
      'jitter_frames'
    ]
    const hasRtFields = rtFields.some((field) => patch[field] !== undefined)
    const hasMappingPatch = patch.mapping !== undefined || patch.dac_map !== undefined
    if (!hasRtFields && !hasMappingPatch) {
      return channelsFail(
        'stage',
        'empty channels patch: provide at least one of channels/sample_rate/aes67_*/jitter_frames/mapping/dac_map'
      )
    }
    return { ok: true, patch, hasRtFields, hasMappingPatch }
  }

  /** mapping/dac_map 补丁 -> lane -> dac（供 mapping-only 回执的 target 展示）。 */
  function mappingFromPatch(patch) {
    if (Array.isArray(patch.mapping)) {
      return patch.mapping.map((entry) => ({ lane: entry.lane, dac: entry.dac }))
    }
    if (Array.isArray(patch.dac_map)) {
      const rows = []
      patch.dac_map.forEach((lane, dac) => {
        if (lane !== null) {
          rows.push({ lane, dac })
        }
      })
      return rows.sort((left, right) => left.lane - right.lane || left.dac - right.dac)
    }
    return null
  }

  /** 只有 mapping 改动的 prepare 回执（commit_supported=false；不签发假 token）。 */
  function channelsMappingOnlyAck(patch) {
    const current = channelsSeed.channels
    const ack = buildChannelsMappingOnlyAck({
      current: current === null ? null : JSON.parse(JSON.stringify(current.current)),
      target: mappingFromPatch(patch),
      reasons: {
        'mapping.current': 'the rt-host control status reports no applied graph_id, so the current routing (and the planned diff) is unknown'
      }
    })
    return ack
  }

  /** 解析 prepare 的目标值（rt-host 字段）并计算 diff。 */
  function channelsPrepareTarget(patch) {
    const document = channelsSeed.channels
    const live = document.current === null ? {} : document.current
    const currentChannels = document.output.channels
    const target = {
      channels: patch.channels ?? currentChannels,
      sample_rate: patch.sample_rate ?? live.sample_rate ?? document.format?.sample_rate ?? null,
      aes67_group: patch.aes67_group ?? live.aes67_group ?? null,
      aes67_port: patch.aes67_port ?? live.aes67_port ?? null,
      aes67_payload_type: patch.aes67_payload_type ?? live.aes67_payload_type ?? null,
      aes67_channels: patch.aes67_channels ?? live.aes67_channels ?? document.input.channels ?? null,
      aes67_map: patch.aes67_map ?? live.aes67_map ?? document.input.map ?? null,
      jitter_frames: patch.jitter_frames ?? live.jitter_frames ?? null,
      input_channels: patch.input_channels ?? document.input.channels ?? null,
      input_map: patch.input_map ?? document.input.map ?? null
    }
    const diff = {}
    if (target.channels !== currentChannels) {
      diff.channels = { from: currentChannels, to: target.channels }
    }
    const compare = [
      'sample_rate',
      'aes67_group',
      'aes67_port',
      'aes67_payload_type',
      'aes67_channels',
      'aes67_map',
      'jitter_frames'
    ]
    for (const field of compare) {
      if (patch[field] !== undefined && JSON.stringify(live[field] ?? null) !== JSON.stringify(target[field])) {
        diff[field] = { from: live[field] ?? null, to: target[field] }
      }
    }
    if (target.input_channels !== document.input.channels || JSON.stringify(target.input_map) !== JSON.stringify(document.input.map)) {
      diff.input = { channels: target.input_channels, map: target.input_map }
    }
    const changed = Object.keys(diff).length > 0
    const changedFormat =
      target.channels !== currentChannels ||
      (patch.sample_rate !== undefined && target.sample_rate !== (live.sample_rate ?? document.format?.sample_rate ?? null))
    return { target, diff, changed, changedFormat, live, currentChannels }
  }

  /** PUT /api/channels stage=prepare：只生成草稿，不改变运行态。 */
  function prepareChannelsJson(body) {
    if (channelsMode === 'host-link-unavailable') {
      return {
        status: 503,
        code: 'host_link_unavailable',
        message: CHANNELS_HOST_LINK_REASON,
        details: { target: 'rt-host', phase: 'channels_prepare', reason: CHANNELS_HOST_LINK_REASON }
      }
    }
    if (channelsSeed.unavailable) {
      return { status: 501, code: 'not_wired', message: channelsSeed.reason, details: { reason: channelsSeed.reason } }
    }
    if (typeof body !== 'object' || body === null || Array.isArray(body)) {
      return { status: 400, code: 'bad_request', message: 'body must be a json object' }
    }
    const current = channelsSeed.channels
    if (current === null || current.output.channels === null) {
      return {
        status: 409,
        code: 'channel_layout_unavailable',
        message: 'current channel layout is not readable (rt-host status unavailable): refusing to prepare a reconfiguration'
      }
    }
    const parsed = parseChannelsPatch(body)
    if (parsed.ok !== true) {
      return parsed
    }
    if (channelsMode === 'mapping-only' || (parsed.hasMappingPatch && !parsed.hasRtFields)) {
      const ack = channelsMappingOnlyAck(parsed.patch)
      return { response: ack }
    }
    const prepared = channelsPrepareTarget(parsed.patch)
    const planGeneration = (channelsGeneration ?? 0) + 1
    const token = stableControlToken('channels', planGeneration, {
      target: prepared.target,
      mapping: mappingFromPatch(parsed.patch)
    })
    const mappingSection = parsed.hasMappingPatch
      ? {
          requested: true,
          source: 'mapping',
          current: current.mapping,
          target: mappingFromPatch(parsed.patch),
          changed: true,
          diff: null,
          applied_by: 'plan routing section (plan prepare + commit)',
          applies_with_this_commit: false,
          reason: MAPPING_PLAN_OWNED_REASON
        }
      : {
          requested: false,
          reason: 'no lane -> DAC mapping change was requested; the running plan routing is untouched'
        }
    const response = buildChannelsPrepareAck({
      token,
      engine_token: token,
      planned_generation: planGeneration,
      changed: prepared.changed,
      diff: prepared.diff,
      current: JSON.parse(JSON.stringify(prepared.live)),
      format: {
        channels: prepared.target.channels,
        sample_rate: prepared.target.sample_rate,
        block_frames: prepared.live.block_frames ?? current.format?.block_frames ?? null,
        plan_rate: prepared.live.plan_rate ?? null,
        device: prepared.live.device ?? null,
        source: 'rt-host-channels-status'
      },
      device: current.device === null ? null : JSON.parse(JSON.stringify(current.device)),
      graph: prepared.changedFormat
        ? { action: 'dropped', reason: CHANNELS_GRAPH_DROPPED_REASON }
        : {
            action: 'kept',
            reason: 'the data-plane format (channels/sample rate/block) is unchanged: the running graph stays valid'
          },
      admission: {
        accepted: true,
        limits: {
          max_channels: CHANNELS_MAX_OUTPUT,
          max_aes67_channels: CHANNELS_MAX_AES67,
          supported_sample_rates: CHANNELS_SAMPLE_RATES,
          payload_type_range: [96, 127]
        }
      },
      mapping: mappingSection,
      preview: {
        old: { channels: prepared.currentChannels, input_channels: current.input.channels },
        new: { channels: prepared.target.channels, input_channels: prepared.target.input_channels },
        audio_gap_ms_estimate: null,
        audio_gap_ms_estimate_reason: 'the real mute-to-resume gap is measured at commit (gap_ms); prepare does not estimate it'
      },
      warnings:
        parsed.hasMappingPatch
          ? [
              'the lane -> DAC mapping is the plan routing section: it is NOT applied by channels_commit (prepare the matching plan and commit it separately)'
            ]
          : [],
      blockers: [],
      not_wired: [],
      reasons: {}
    })
    if (channelsPending !== null && channelsPending.token === token) {
      return { response: channelsPending.response }
    }
    channelsPending = {
      kind: 'channels',
      token,
      planGeneration,
      target: prepared.target,
      changed: prepared.changed,
      changedFormat: prepared.changedFormat,
      response
    }
    return { response }
  }

  /** PUT /api/channels stage=commit：块边界生效（失败 502 + details.rolled_back，不假成功）。 */
  function commitChannelsJson(body) {
    if (channelsMode === 'host-link-unavailable') {
      return {
        status: 503,
        code: 'host_link_unavailable',
        message: CHANNELS_HOST_LINK_REASON,
        details: { target: 'rt-host', phase: 'channels_commit', reason: CHANNELS_HOST_LINK_REASON }
      }
    }
    if (channelsSeed.unavailable) {
      return { status: 501, code: 'not_wired', message: channelsSeed.reason, details: { reason: channelsSeed.reason } }
    }
    const token = body.engine_token === undefined ? body.token : body.engine_token
    if (typeof token !== 'string' || token.length === 0) {
      return { status: 400, code: 'bad_request', message: 'field engine_token must be a non-empty string', details: { field: 'engine_token' } }
    }
    if (channelsPending === null || channelsPending.token !== token) {
      return {
        status: 409,
        code: 'engine_token_invalid',
        message: 'engine_token mismatch or no prepared channels draft: call PUT /api/channels {stage:"prepare"} again'
      }
    }
    const draft = channelsPending
    channelsPending = null
    if (channelsMode === 'reconfigure-failure') {
      const failure = buildChannelsCommitFailureDetails({
        stage: 'device_open',
        generation: channelsGeneration ?? 0
      })
      return {
        status: 502,
        code: failure.error,
        message: failure.message,
        details: failure
      }
    }
    // 数据面生效：更新 mock 的 channels 视图（rt-host 真值回读形态）。
    const nextGeneration = (channelsGeneration ?? 0) + 1
    const document = JSON.parse(JSON.stringify(channelsSeed.channels))
    const fixedAt = '2026-11-26T07:30:00.000Z'
    document.generation = nextGeneration
    document.busy = false
    document.pending = null
    document.last_gap_ms = 37
    document.last_result = { ok: true, applied: true, changed: draft.changed, generation: nextGeneration, gap_ms: 37 }
    document.current = {
      ...(document.current ?? {}),
      channels: draft.target.channels,
      sample_rate: draft.target.sample_rate,
      aes67_group: draft.target.aes67_group,
      aes67_port: draft.target.aes67_port,
      aes67_payload_type: draft.target.aes67_payload_type,
      aes67_channels: draft.target.aes67_channels,
      aes67_map: draft.target.aes67_map,
      jitter_frames: draft.target.jitter_frames,
      block_frames: document.current?.block_frames ?? null,
      plan_rate: document.current?.plan_rate ?? null,
      device: document.current?.device ?? null
    }
    document.format = {
      ...(document.format ?? {}),
      channels: draft.target.channels,
      sample_rate: draft.target.sample_rate,
      block_frames: document.format?.block_frames ?? null,
      plan_rate: document.format?.plan_rate ?? null,
      device: document.format?.device ?? null,
      source: 'rt-host-channels-status'
    }
    document.input = {
      ...document.input,
      channels: draft.target.input_channels,
      map: draft.target.input_map
    }
    document.output = { ...document.output, channels: draft.target.channels }
    if (draft.changedFormat) {
      // 引擎格式变化：运行中的方案图被丢弃，方案 routing（mapping/dac_map）不再可信（如实置 null + reasons）。
      document.plan = {
        ...document.plan,
        routing_known: false,
        routing_invalidated: {
          generation: nextGeneration,
          recorded_at: fixedAt,
          reason: 'the running graph was dropped by the channels hot reconfiguration: the plan routing no longer describes the data plane'
        }
      }
      document.mapping = null
      document.output = { ...document.output, dac_map: null, channels_source: 'rt-host-channels-status' }
      document.reasons = {
        ...document.reasons,
        mapping: 'the running plan graph was dropped by the channels hot reconfiguration (graph_dropped=true): the plan routing mirror is no longer trusted',
        'output.dac_map': 'the running plan graph was dropped by the channels hot reconfiguration (graph_dropped=true): the plan routing mirror is no longer trusted',
        'plan.routing_known': 'the running graph was dropped by the channels hot reconfiguration'
      }
      document.not_wired = [
        ...(Array.isArray(document.not_wired) ? document.not_wired : []),
        {
          path: 'channels.hot_reconfigure.routing',
          reason: 'a plan whose routing matches the new format must be prepared and committed again'
        }
      ]
    }
    document.applied = {
      generation: nextGeneration,
      recorded_at: fixedAt,
      format: {
        channels: draft.target.channels,
        sample_rate: draft.target.sample_rate,
        block_frames: document.format.block_frames
      }
    }
    channelsSeed.channels = document
    channelsGeneration = nextGeneration
    const ack = buildChannelsCommitAck({
      changed: draft.changed,
      idempotent: false,
      generation: nextGeneration,
      gap_ms: 37,
      format: {
        channels: draft.target.channels,
        sample_rate: draft.target.sample_rate,
        block_frames: document.format.block_frames,
        plan_rate: document.format.plan_rate,
        device: document.format.device,
        source: document.format.source
      },
      channels: JSON.parse(JSON.stringify(document.current)),
      graph_dropped: draft.changedFormat,
      rolled_back: false,
      routing: {
        requested: false,
        applied: false,
        graph_dropped: draft.changedFormat,
        plan_recommit_required: draft.changedFormat,
        reason: draft.changedFormat
          ? 'the running graph was dropped by the format change: a plan whose routing matches the new format must be prepared and committed separately'
          : 'no lane -> DAC routing draft is known to this daemon; channels_commit never changes the plan routing section'
      },
      local: { mirror_updated: true, pending_routing_known: false, recorded_at: fixedAt }
    })
    return { response: ack }
  }

  /** PUT /api/channels stage=abort：丢弃草稿（运行态不变）。 */
  function abortChannelsJson(body) {
    if (channelsMode === 'host-link-unavailable') {
      return {
        status: 503,
        code: 'host_link_unavailable',
        message: CHANNELS_HOST_LINK_REASON,
        details: { target: 'rt-host', phase: 'channels_abort', reason: CHANNELS_HOST_LINK_REASON }
      }
    }
    if (channelsSeed.unavailable) {
      return { status: 501, code: 'not_wired', message: channelsSeed.reason, details: { reason: channelsSeed.reason } }
    }
    const token = body.engine_token === undefined ? body.token : body.engine_token
    if (typeof token !== 'string' || token.length === 0) {
      return { status: 400, code: 'bad_request', message: 'field engine_token must be a non-empty string', details: { field: 'engine_token' } }
    }
    if (channelsPending === null || channelsPending.token !== token) {
      return {
        status: 409,
        code: 'engine_token_invalid',
        message: 'engine_token mismatch or no prepared channels draft to abort'
      }
    }
    channelsPending = null
    return {
      response: buildChannelsAbortAck({
        aborted_token: token,
        local: { pending_routing_discarded: true, recorded_at: '2026-11-26T07:31:00.000Z' }
      })
    }
  }

  /** PUT /api/channels 入口：stage=prepare|commit|abort（兼容 action 字段）。 */
  function handleChannelsWrite(body) {
    const stage = body.stage === undefined ? body.action : body.stage
    if (stage === 'prepare') {
      return prepareChannelsJson(body)
    }
    if (stage === 'commit') {
      return commitChannelsJson(body)
    }
    if (stage === 'abort') {
      return abortChannelsJson(body)
    }
    if (stage === undefined) {
      return {
        status: 400,
        code: 'bad_request',
        message: 'missing field: stage (prepare|commit|abort)',
        details: { field: 'stage' }
      }
    }
    return {
      status: 400,
      code: 'bad_request',
      message: 'stage must be prepare, commit or abort (got ' + JSON.stringify(stage) + ')',
      details: { field: 'stage' }
    }
  }

  async function handleM4(req, res, path, method) {
    if (channelsSeed.unavailable) {
      m4Unavailable(res)
      return
    }
    if (method === 'GET' && path === '/api/channels') {
      sendJson(res, 200, buildChannelsView())
      return
    }
    if (method === 'PUT' && path === '/api/channels') {
      const body = await readJsonBody(req)
      const outcome = handleChannelsWrite(body)
      if (outcome.status !== undefined) {
        sendError(res, outcome.status, outcome.code, outcome.message, outcome.details)
        return
      }
      sendJson(res, 200, outcome.response)
      return
    }
    if (method === 'GET' && path === '/api/plan/history') {
      sendJson(res, 200, planHistoryJson())
      return
    }
    if (method === 'POST' && path === '/api/plan/inspect') {
      // daemon 真实契约：POST {plan_path, plan_sha256?}（只读试运行，读的是已落盘的方案文件；
      // plan_path 来自上传回执的 stored_path）。GET 在本 mock 里落到 405（与 axum 的 POST-only 路由一致）。
      const body = await readJsonBody(req)
      if (typeof body.plan_path !== 'string' || body.plan_path.length === 0) {
        sendError(res, 400, 'bad_request', 'missing field: plan_path')
        return
      }
      if (body.plan_sha256 !== undefined && typeof body.plan_sha256 !== 'string') {
        sendError(res, 400, 'bad_request', 'field plan_sha256 must be a string')
        return
      }
      const upload = findUploadByStoredPath(body.plan_path)
      if (upload === undefined) {
        sendError(
          res,
          400,
          'bad_request',
          'cannot read plan file ' + body.plan_path + ': no such uploaded plan (upload it first)'
        )
        return
      }
      if (typeof body.plan_sha256 === 'string' && body.plan_sha256 !== upload.sha256) {
        sendError(
          res,
          400,
          'bad_request',
          'plan sha256 mismatch: request ' + body.plan_sha256 + ', file ' + body.plan_path + ' is ' + upload.sha256
        )
        return
      }
      const fixtureInspect = channelsSeed.inspect
      if (fixtureInspect !== null && fixtureInspect.admission && fixtureInspect.admission.accepted === false) {
        // degraded fixture：如实报告试运行不可用（rejection + message），不编造已编译结果。
        sendJson(res, 200, { ...fixtureInspect, sha256: upload.sha256, graph_id: upload.graph_id })
        return
      }
      sendJson(res, 200, {
        ok: true,
        staged: false,
        sha256: upload.sha256,
        graph_id: upload.graph_id,
        plan: {
          block_size: upload.block_size,
          sample_rate: upload.sample_rate,
          in_channels: 2,
          lanes: 22,
          dac_channels: channelsSeed.channels === null ? null : channelsSeed.channels.output.channels
        },
        admission: { accepted: true, rejections: [], budget: {} },
        compile: { compiled: true, latency_samples: upload.latency_samples }
      })
      return
    }
    if (method === 'POST' && path === '/api/plan/upload') {
      let bytes
      try {
        bytes = await readRawBody(req, 4 * 1024 * 1024)
      } catch (error) {
        if (error instanceof Error && error.message === 'too_large') {
          sendError(res, 413, 'upload_too_large', 'plan file exceeds the 4 MiB upload limit')
          return
        }
        throw error
      }
      if (bytes.length < 64) {
        sendError(res, 400, 'invalid_plan', 'plan file is too small to be a khaen plan (ABI v3)')
        return
      }
      if (bytes.subarray(0, 8).toString('latin1').includes('REJECT')) {
        sendError(res, 400, 'invalid_plan', 'plan rejected: mock fixture bytes contain REJECT (invalid ABI v3)')
        return
      }
      const sha = createHash('sha256').update(bytes).digest('hex')
      const existing = planUploads.get(sha)
      let upload = existing
      if (upload === undefined) {
        const facts = planFacts(sha, bytes)
        upload = {
          sha256: sha,
          graph_id: facts.graph_id,
          size_bytes: bytes.length,
          sample_rate: facts.sample_rate,
          block_size: facts.block_size,
          segments: facts.segments,
          segment_count: facts.segments.length,
          latency_samples: facts.latency_samples,
          stored_path: planStoredPath(sha),
          recorded_at: '2026-11-26T07:00:00.000Z',
          filename: typeof req.headers['x-khaen-filename'] === 'string' ? req.headers['x-khaen-filename'] : null
        }
        planUploads.set(sha, upload)
      }
      // daemon 真实回执（plan_api.rs upload）：record.to_json() + ok/schema/created/deduplicated。
      // 首次写入 201、同内容幂等复用 200（不假装重新解码）。mock 不解码 mock 字节：routing/admission 给 null。
      sendJson(res, existing === undefined ? 201 : 200, {
        ok: true,
        schema: 'khaen-plan/v1',
        created: existing === undefined,
        deduplicated: existing !== undefined,
        sha256: upload.sha256,
        graph_id: upload.graph_id,
        size_bytes: upload.size_bytes,
        sample_rate: upload.sample_rate,
        block_size: upload.block_size,
        segments: upload.segments,
        segment_count: upload.segment_count,
        latency_samples: upload.latency_samples,
        routing: null,
        layout: { in_channels: null, out_lanes: null },
        admission: { accepted: null, rejections: [] },
        source: 'upload',
        recorded_at: upload.recorded_at,
        stored_path: upload.stored_path
      })
      return
    }
    if (method === 'POST' && path === '/api/prepare') {
      // daemon 真实契约（lib.rs prepare → 控制面 prepare_json）：{plan_path, plan_sha256}
      // 两个字符串；读文件 + 校验 sha + 解码 + 编译图，返回草稿（不含 preview/版本递增语义）。
      const body = await readJsonBody(req)
      if (typeof body.plan_path !== 'string' || typeof body.plan_sha256 !== 'string') {
        sendError(res, 400, 'bad_request', 'fields plan_path and plan_sha256 must be strings')
        return
      }
      const upload = findUploadByStoredPath(body.plan_path)
      if (upload === undefined) {
        sendError(
          res,
          400,
          'bad_request',
          'cannot read plan file ' + body.plan_path + ': no such uploaded plan (upload it first)'
        )
        return
      }
      const sha = body.plan_sha256.trim().toLowerCase()
      if (upload.sha256 !== sha) {
        sendError(
          res,
          400,
          'bad_request',
          'plan sha256 mismatch: request ' + body.plan_sha256 + ', file ' + body.plan_path + ' is ' + upload.sha256
        )
        return
      }
      const version = control.version + 1
      const token = stableControlToken('plan', version, sha)
      const response = {
        ok: true,
        surface: 'plan',
        engine_token: token,
        planned_version: version,
        graph_id: upload.graph_id,
        admission: { accepted: true, budget: {} }
      }
      if (controlPending !== null && controlPending.token === token) {
        sendJson(res, 200, controlPending.response)
        return
      }
      controlPending = {
        kind: 'plan',
        token,
        version,
        graph_id: upload.graph_id,
        sha256: sha,
        response
      }
      sendJson(res, 200, response)
      return
    }
    if (method === 'POST' && path === '/api/rollback') {
      const body = await readJsonBody(req)
      if (control.history.length === 0) {
        sendError(res, 409, 'no_history', 'no committed plan history to roll back to')
        return
      }
      const current = control.history[control.history.length - 1]
      let target
      if (body.target === undefined || body.target === null) {
        if (control.history.length < 2) {
          sendError(res, 400, 'bad_request', 'nothing to roll back: only one committed plan version exists')
          return
        }
        target = control.history[control.history.length - 2].version
      } else {
        if (!Number.isInteger(body.target)) {
          sendError(res, 400, 'bad_request', 'field target must be a non-negative integer version')
          return
        }
        target = body.target
      }
      if (target === current.version) {
        sendError(res, 400, 'bad_request', 'target version is already the running graph; rollback target must be older')
        return
      }
      const entry = control.history.find((item) => item.version === target)
      if (entry === undefined) {
        sendError(res, 400, 'bad_request', 'target version is not in rollback history')
        return
      }
      const version = control.version + 1
      const token = stableControlToken('rollback', version, target)
      const response = {
        ok: true,
        rolled_back_to: target,
        engine_token: token,
        planned_version: version,
        graph_id: entry.graph_id,
        admission: { accepted: true, budget: {} }
      }
      if (controlPending !== null && controlPending.token === token) {
        sendJson(res, 200, controlPending.response)
        return
      }
      controlPending = {
        kind: 'rollback',
        token,
        version,
        graph_id: entry.graph_id,
        sha256: entry.sha256 ?? null,
        rolled_back_to: target,
        response
      }
      sendJson(res, 200, response)
      return
    }
    sendError(res, 405, 'method_not_allowed', 'unsupported method/path: ' + method + ' ' + path)
  }

  async function handleControl(req, res, path, method) {
    if (!control.wired) {
      controlUnavailable(res)
      return
    }
    if (method === 'POST' && path === '/api/mute') {
      const body = await readJsonBody(req)
      if (typeof body.mute !== 'boolean') {
        sendError(res, 400, 'bad_request', 'field mute must be a boolean')
        return
      }
      control.muted = body.mute
      sendJson(res, 200, {
        ok: true,
        mute: control.muted,
        applied_block: control.version * 128 + 1,
        idempotent: false
      })
      return
    }
    if (method === 'POST' && path === '/api/surface') {
      const body = await readJsonBody(req)
      if (typeof body.surface !== 'string' || body.surface.length === 0) {
        sendError(res, 400, 'bad_request', 'missing field: surface')
        return
      }
      // 五个界面都走同一套 prepare -> commit/abort（docs/API.md §4 与 crates/host/src/control/mod.rs）：
      // volume/routing/layout 是方案补丁（需要已生效方案基线，否则 409 no_base_plan）；
      // mode/format 是执行器配置（不依赖方案，另带 changed/applies_to/device_restart_required）。
      const prepared =
        body.surface === 'volume'
          ? prepareVolumeSurface(body)
          : body.surface === 'routing'
            ? prepareRoutingSurface(body)
            : body.surface === 'layout'
              ? prepareLayoutSurface(body)
              : body.surface === 'mode'
                ? prepareModeSurface(body)
                : body.surface === 'format'
                  ? prepareFormatBody(body.data === undefined ? {} : body.data)
                  : { status: 400, code: 'bad_request', message: 'unknown surface: ' + body.surface }
      if (prepared.status !== undefined) {
        sendError(res, prepared.status, prepared.code, prepared.message)
        return
      }
      sendJson(res, 200, prepared.response)
      return
    }
    if (method === 'POST' && path === '/api/format') {
      const body = await readJsonBody(req)
      const prepared = prepareFormatBody(body)
      if (prepared.status !== undefined) {
        sendError(res, prepared.status, prepared.code, prepared.message)
        return
      }
      sendJson(res, 200, prepared.response)
      return
    }
    if (method === 'POST' && path === '/api/commit') {
      const body = await readJsonBody(req)
      if (typeof body.engine_token !== 'string' || body.engine_token.length === 0) {
        sendError(res, 400, 'bad_request', 'field engine_token must be a string')
        return
      }
      if (controlPending === null || controlPending.token !== body.engine_token) {
        sendError(
          res,
          409,
          'engine_token_invalid',
          'engine_token mismatch or no prepared draft: prepare again'
        )
        return
      }
      const committed = commitControlDraft()
      if (committed.status !== undefined) {
        sendError(res, committed.status, committed.code, committed.message)
        return
      }
      sendJson(res, 200, committed.response)
      return
    }
    if (method === 'POST' && path === '/api/abort') {
      const body = await readJsonBody(req)
      if (typeof body.engine_token !== 'string' || body.engine_token.length === 0) {
        sendError(res, 400, 'bad_request', 'field engine_token must be a string')
        return
      }
      if (controlPending === null || controlPending.token !== body.engine_token) {
        sendError(res, 409, 'engine_token_invalid', 'engine_token mismatch or no prepared draft')
        return
      }
      const abortedSurface = controlPending.kind
      controlPending = null
      sendJson(res, 200, {
        ok: true,
        aborted: true,
        ...(abortedSurface === 'mode' || abortedSurface === 'format'
          ? { aborted_surface: abortedSurface }
          : {})
      })
      return
    }
    if (method === 'POST' && path === '/api/device/retry') {
      sendJson(res, 200, {
        wired: true,
        state: 'streaming',
        plan_rate: control.format.sample_rate,
        device_rate: control.device_rate,
        rate_asserted: control.rate_asserted,
        attempts: controlOperationSeq + 1,
        since_ms: 12,
        last_error: null,
        safety: {
          state: 'ok',
          requires_mute: false,
          user_mute: control.muted,
          blocks: 0,
          total_faults: 0
        }
      })
      return
    }
    sendError(res, 405, 'method_not_allowed', 'unsupported method/path: ' + method + ' ' + path)
  }

  /* ------------------------------------------------------- MOTU handler（T57） */

  function motuFlattenedChannels() {
    return motuMainChannels()
  }

  function motuDetectionJson() {
    return {
      detected: motu.detected,
      model: motu.model,
      card_id: motu.card_id,
      card_index: motu.card_index,
      control_reachable: motu.control_reachable,
      reason: motu.reason,
      reason_detail: motu.reason_detail
    }
  }

  function motuWriteCapabilityJson() {
    if (motu.detected === true && motu.control_reachable === true) {
      return {
        status: 'wired',
        single_key: true,
        readback: true,
        two_phase: true,
        batch: false,
        max_channels_per_request: 2,
        reason: null
      }
    }
    return {
      status: motu.detected === true ? 'unreachable' : 'not_detected',
      single_key: true,
      readback: true,
      two_phase: true,
      batch: false,
      max_channels_per_request: 2,
      reason: motu.reason_detail
    }
  }

  /** GET /api/motu（恒 200；拿不到的字段 null + reasons，不编数值）。 */
  function motuStatusJson() {
    const reasons = {}
    if (!(motu.detected === true && motu.control_reachable === true)) {
      for (const path of ['mute', 'vol', 'state']) {
        reasons[path] = motu.reason_detail
      }
    }
    return {
      schema: 'khaen-motu/v1',
      ...motuDetectionJson(),
      channel_count: 2,
      channels: motuFlattenedChannels(),
      mute: motu.mute,
      vol: motu.vol === null ? null : JSON.parse(JSON.stringify(motu.vol)),
      state: motu.state === null ? null : JSON.parse(JSON.stringify(motu.state)),
      read_error: null,
      write: motuWriteCapabilityJson(),
      not_wired:
        motu.detected === true && motu.control_reachable === true
          ? []
          : [{ path: 'motu.write', reason: motu.reason_detail }],
      reasons
    }
  }

  /** 写前门禁：设备必须 detected 且控制网可达（否则 503 + 稳定码，不尝试写）。 */
  function motuGateError() {
    return {
      status: 503,
      code: 'motu_not_detected',
      message: motu.reason_detail,
      details: { reason: motu.reason, detection: motuDetectionJson() }
    }
  }

  function motuWritePlanJson(channels) {
    return {
      kind: 'single-key+readback',
      batch: false,
      writes_per_channel: 1,
      writes: channels.length,
      steps: channels.map((channel) => ({
        channel,
        key: 'koBusFader',
        index: channel - 1,
        frames: 1
      })),
      verified_by: 'device datastore readback: a write only counts as done when the device streams the same value back'
    }
  }

  /** 解析音量目标（channel/channels 二选一；db/linear 二选一；>2 通道 400 motu_batch_rejected）。 */
  function motuParseVolumeTarget(body) {
    const invalid = (field, message) => ({
      status: 400,
      code: 'invalid_request',
      message,
      details: { field }
    })
    if (typeof body !== 'object' || body === null || Array.isArray(body)) {
      return invalid('<body>', 'request body must be a JSON object')
    }
    const hasChannel = body.channel !== undefined
    const hasChannels = body.channels !== undefined
    if (hasChannel === hasChannels) {
      return invalid(
        'channel',
        'give exactly one of channel (1|2|"both") or channels ([1], [2] or [1,2])'
      )
    }
    let channels = []
    if (hasChannel) {
      if (typeof body.channel === 'string') {
        if (body.channel.trim().toLowerCase() !== 'both') {
          return invalid('channel', 'field channel must be 1, 2 or "both" (got ' + JSON.stringify(body.channel) + ')')
        }
        channels = [1, 2]
      } else {
        const channel = body.channel
        if (!Number.isInteger(channel) || channel < 1 || channel > 2) {
          return invalid('channel', 'field channel must be a main channel number (1..=2)')
        }
        channels = [channel]
      }
    } else {
      if (!Array.isArray(body.channels) || body.channels.length === 0) {
        return invalid('channels', 'field channels must be a non-empty array of main channel numbers')
      }
      if (body.channels.length > 2) {
        return {
          status: 400,
          code: 'motu_batch_rejected',
          message:
            'refusing a batch MOTU volume write of ' +
            String(body.channels.length) +
            ' channels: at most the two main channels (Main 1/Main 2) are addressable, each with its own single-key write and readback',
          details: { limit: 2, requested: body.channels.length }
        }
      }
      const seen = new Set()
      for (const entry of body.channels) {
        if (!Number.isInteger(entry) || entry < 1 || entry > 2) {
          return invalid('channels', 'field channels entries must be main channel numbers (1..=2)')
        }
        if (seen.has(entry)) {
          return invalid('channels', 'channels lists channel ' + String(entry) + ' more than once')
        }
        seen.add(entry)
      }
      channels = [...body.channels].sort((left, right) => left - right)
    }
    const hasDb = body.db !== undefined
    const hasLinear = body.linear !== undefined
    if (hasDb === hasLinear) {
      return invalid('db', 'give exactly one of db or linear, not both')
    }
    let linear
    if (hasDb) {
      const db = body.db
      if (typeof db !== 'number' || !Number.isFinite(db) || db > 0) {
        return invalid('db', 'field db must be a finite number <= 0 dB')
      }
      if (db < MOTU_DB_FLOOR) {
        linear = 0
      } else {
        linear = Math.min(Math.max(10 ** (db / 20), 0), 1)
      }
    } else {
      const value = body.linear
      if (typeof value !== 'number' || !Number.isFinite(value) || value < 0 || value > 1) {
        return invalid('linear', 'field linear must be a finite number in 0.0..=1.0')
      }
      linear = value
    }
    linear = motuQuantizeLinear(linear)
    return { ok: true, channels, linear, db: motuFaderToDb(linear) }
  }

  /** PUT/POST /api/motu/volume stage=prepare：只签一次性 token + 目标/写计划，**不写设备**。 */
  function motuVolumePrepareJson(body) {
    const parsed = motuParseVolumeTarget(body)
    if (parsed.ok !== true) {
      return parsed
    }
    if (!(motu.detected === true && motu.control_reachable === true)) {
      return motuGateError()
    }
    const key = JSON.stringify({ channels: parsed.channels, linear: parsed.linear })
    let draft = motu.draft
    if (draft === null || draft.key !== key) {
      motu.write_seq += 1
      const token = createHash('sha256')
        .update('khaen-motu-volume/v1|' + MOTU_FIXED_CREATED_AT + '|' + String(motu.write_seq) + '|' + key)
        .digest('hex')
        .slice(0, 16)
      draft = {
        token,
        key,
        channels: parsed.channels,
        linear: parsed.linear,
        db: parsed.db,
        created_at: MOTU_FIXED_CREATED_AT
      }
      motu.draft = draft
    }
    return {
      response: {
        ok: true,
        schema: 'khaen-motu-volume/v1',
        stage: 'prepare',
        engine_token: draft.token,
        token: draft.token,
        created_at: draft.created_at,
        expires_in_s: 120,
        target: {
          channels: draft.channels.map((channel) => ({
            channel,
            name: 'Main ' + String(channel),
            datastore: { key: 'koBusFader', index: channel - 1 }
          })),
          linear: draft.linear,
          db: draft.db
        },
        write_plan: motuWritePlanJson(draft.channels),
        commit_supported: true,
        two_phase: true,
        writes_device: false,
        applies_to: MOTU_WRITE_APPLIES_TO,
        target_device: motuDetectionJson()
      }
    }
  }

  /** PUT/POST /api/motu/volume stage=commit：逐通道单键写 + 真值回读（一次 commit 消费草稿）。 */
  function motuVolumeCommitJson(body) {
    const token = body.engine_token === undefined ? body.token : body.engine_token
    if (typeof token !== 'string' || token.trim().length === 0) {
      return {
        status: 400,
        code: 'invalid_request',
        message: 'missing field: engine_token (commit the token returned by stage=prepare)',
        details: { field: 'engine_token' }
      }
    }
    // 设备不可用时保留草稿（设备恢复后同一 token 还能 commit），如实 503。
    if (!(motu.detected === true && motu.control_reachable === true)) {
      return motuGateError()
    }
    const draft = motu.draft
    if (draft === null || draft.token !== token) {
      return {
        status: 409,
        code: 'engine_token_invalid',
        message: 'engine_token mismatch or no prepared MOTU volume draft: call stage=prepare again'
      }
    }
    motu.draft = null
    const receipts = []
    let idempotent = true
    for (const channel of draft.channels) {
      const index = Array.isArray(motu.vol) ? motu.vol.findIndex((entry) => entry.channel === channel) : -1
      const before = index >= 0 ? motu.vol[index].linear : null
      const changed = before !== draft.linear
      if (changed) {
        idempotent = false
      }
      const receipt = {
        channel,
        name: 'Main ' + String(channel),
        key: 'koBusFader',
        index: channel - 1,
        before,
        wrote: draft.linear,
        readback: draft.linear,
        changed,
        frame_sent: changed,
        verified: true
      }
      if (index >= 0) {
        motu.vol[index] = { ...motu.vol[index], linear: draft.linear, db: draft.db }
      }
      receipts.push(receipt)
    }
    motu.write_seq += 1
    return {
      response: {
        ok: true,
        schema: 'khaen-motu-volume/v1',
        stage: 'commit',
        engine_token: token,
        applied: true,
        idempotent,
        created_at: draft.created_at,
        linear: draft.linear,
        db: draft.db,
        channels: receipts,
        write_plan: motuWritePlanJson(draft.channels),
        writes_device: true,
        applies_to: MOTU_WRITE_APPLIES_TO,
        target_device: motuDetectionJson()
      }
    }
  }

  /** /api/motu 与 /api/motu/volume 路由（没有 abort；未提交草稿由 TTL 过期）。 */
  async function handleMotu(req, res, path, method) {
    if (method === 'GET' && path === '/api/motu') {
      sendJson(res, 200, motuStatusJson())
      return
    }
    if ((method === 'PUT' || method === 'POST') && path === '/api/motu/volume') {
      const body = await readJsonBody(req)
      const stage = body.stage === undefined ? (body.action === undefined ? 'prepare' : body.action) : body.stage
      let outcome
      if (stage === 'prepare') {
        outcome = motuVolumePrepareJson(body)
      } else if (stage === 'commit') {
        outcome = motuVolumeCommitJson(body)
      } else {
        outcome = {
          status: 400,
          code: 'invalid_request',
          message: 'stage must be prepare or commit (got ' + JSON.stringify(stage) + ')',
          details: { field: 'stage' }
        }
      }
      if (outcome.status !== undefined) {
        sendError(res, outcome.status, outcome.code, outcome.message, outcome.details)
        return
      }
      sendJson(res, 200, outcome.response)
      return
    }
    sendError(res, 405, 'method_not_allowed', 'unsupported method/path: ' + method + ' ' + path)
  }

  /* ------------------------------------------- 音频 profile / 设备输入 / 测试音 handler */

  /** GET /api/audio-profile：read-only 恒 200；不可用 = profile:null + capability 全 null + reasons。 */
  function audioProfileSnapshotJson() {
    const profile = audioProfileSaved
    return {
      ok: true,
      schema: AUDIO_PROFILE_SCHEMA,
      profile: profile === null ? null : JSON.parse(JSON.stringify(profile)),
      validation: profile === null ? { ok: true, errors: [], warnings: [] } : validateAudioProfile(profile),
      capability: JSON.parse(JSON.stringify(audioProfileSeed.capability)),
      plan: JSON.parse(JSON.stringify(audioProfileAppliedPlan)),
      reasons: { ...audioProfileSeed.reasons }
    }
  }

  function audioProfileNotWiredError() {
    return {
      status: 503,
      code: 'not_wired',
      message: AUDIO_PROFILE_NOT_WIRED_REASON,
      details: { reason: AUDIO_PROFILE_NOT_WIRED_REASON, target: 'rt-host' }
    }
  }

  /** PUT /api/audio-profile stage=prepare：校验 + diff + 一次性 token（不碰引擎）。 */
  function audioProfilePrepareJson(body) {
    if (audioProfileSeed.available !== true) {
      return audioProfileNotWiredError()
    }
    if (typeof body.profile !== 'object' || body.profile === null || Array.isArray(body.profile)) {
      return {
        status: 400,
        code: 'bad_request',
        message: 'missing field: profile (a khaen-audio-profile/v1 JSON object)',
        details: { field: 'profile' }
      }
    }
    const profile = JSON.parse(JSON.stringify(body.profile))
    const validation = validateAudioProfile(profile)
    const diff = profileDiff(audioProfileSaved, profile)
    const token = createHash('sha256')
      .update('khaen-audio-profile/v1|prepare|' + JSON.stringify(profile))
      .digest('hex')
      .slice(0, 32)
    const response = {
      ok: true,
      stage: 'prepare',
      profile: JSON.parse(JSON.stringify(profile)),
      validation,
      diff,
      token,
      expires_in_s: AUDIO_PROFILE_PREPARE_TTL_S
    }
    if (audioProfilePending !== null && audioProfilePending.token === token) {
      return { response: audioProfilePending.response }
    }
    audioProfilePending = { token, profile, response }
    return { response }
  }

  /** PUT /api/audio-profile stage=commit：保存 profile（revision 递增）。 */
  function audioProfileCommitJson(body) {
    if (audioProfileSeed.available !== true) {
      return audioProfileNotWiredError()
    }
    const token = body.token === undefined ? body.engine_token : body.token
    if (typeof token !== 'string' || token.length === 0) {
      return { status: 400, code: 'bad_request', message: 'field token must be a non-empty string', details: { field: 'token' } }
    }
    if (audioProfilePending === null || audioProfilePending.token !== token) {
      return {
        status: 409,
        code: 'engine_token_invalid',
        message: 'token mismatch or no prepared profile draft: PUT /api/audio-profile {stage:"prepare"} again'
      }
    }
    const draft = audioProfilePending
    audioProfilePending = null
    audioProfileSaved = JSON.parse(JSON.stringify(draft.profile))
    audioProfileRevision = (typeof audioProfileRevision === 'number' ? audioProfileRevision : 0) + 1
    return {
      response: {
        ok: true,
        applied: true,
        saved: true,
        profile: JSON.parse(JSON.stringify(audioProfileSaved)),
        revision: audioProfileRevision
      }
    }
  }

  /** PUT /api/audio-profile stage=abort：丢弃草稿（不改变已保存 profile）。 */
  function audioProfileAbortJson(body) {
    if (audioProfileSeed.available !== true) {
      return audioProfileNotWiredError()
    }
    const token = body.token === undefined ? body.engine_token : body.token
    if (typeof token !== 'string' || token.length === 0) {
      return { status: 400, code: 'bad_request', message: 'field token must be a non-empty string', details: { field: 'token' } }
    }
    if (audioProfilePending === null || audioProfilePending.token !== token) {
      return { status: 409, code: 'engine_token_invalid', message: 'token mismatch or no prepared profile draft to abort' }
    }
    audioProfilePending = null
    return { response: { ok: true, aborted: true } }
  }

  function handleAudioProfileWrite(body) {
    const stage = body.stage
    if (stage === 'prepare') {
      return audioProfilePrepareJson(body)
    }
    if (stage === 'commit') {
      return audioProfileCommitJson(body)
    }
    if (stage === 'abort') {
      return audioProfileAbortJson(body)
    }
    return {
      status: 400,
      code: 'bad_request',
      message: 'stage must be prepare, commit or abort (got ' + JSON.stringify(stage) + ')',
      details: { field: 'stage' }
    }
  }

  /** POST /api/audio-profile/plan：profile -> 方案文件（不自动提交；之后走 inspect + prepare + commit）。 */
  function audioProfilePlanJson() {
    if (audioProfileSeed.available !== true) {
      return audioProfileNotWiredError()
    }
    if (audioProfileSaved === null) {
      return {
        status: 409,
        code: 'no_applicable_profile',
        message: 'no saved audio profile: PUT /api/audio-profile {stage:"prepare"} then commit first'
      }
    }
    const generated = buildGeneratedPlan(audioProfileSaved)
    // 与 /api/plan/upload 同一份内容寻址表：后续 POST /api/plan/inspect 与 /api/prepare 用同一个 stored_path。
    planUploads.set(generated.sha256, generated.plan_facts)
    audioProfileGeneratedPlan = {
      sha256: generated.sha256,
      graph_id: generated.sha256,
      lanes: generated.lane_count,
      dac_channels: generated.plan_facts.routing?.dac_channels ?? AUDIO_PROFILE_DEVICE_CHANNELS
    }
    return {
      response: {
        ok: true,
        plan_path: generated.plan_path,
        sha256: generated.sha256,
        lane_count: generated.lane_count,
        inspect: generated.inspect,
        folds_preview: generated.folds_preview,
        downmix_applied: generated.downmix_applied
      }
    }
  }

  /** /api/audio-profile、/api/audio-profile/plan、/api/device/inputs 路由。 */
  async function handleAudioProfile(req, res, path, method) {
    if (method === 'GET' && path === '/api/audio-profile') {
      sendJson(res, 200, audioProfileSnapshotJson())
      return
    }
    if (method === 'PUT' && path === '/api/audio-profile') {
      const body = await readJsonBody(req)
      const outcome = handleAudioProfileWrite(body)
      if (outcome.status !== undefined) {
        sendError(res, outcome.status, outcome.code, outcome.message, outcome.details)
        return
      }
      sendJson(res, 200, outcome.response)
      return
    }
    if (method === 'POST' && path === '/api/audio-profile/plan') {
      const outcome = audioProfilePlanJson()
      if (outcome.status !== undefined) {
        sendError(res, outcome.status, outcome.code, outcome.message, outcome.details)
        return
      }
      sendJson(res, 200, outcome.response)
      return
    }
    if (method === 'GET' && path === '/api/device/inputs') {
      sendJson(res, 200, buildDeviceInputsBody(audioProfileFixture))
      return
    }
    sendError(res, 405, 'method_not_allowed', 'unsupported method/path: ' + method + ' ' + path)
  }

  /** POST /api/speaker-test /api/speaker-test/stop（必须 audible+confirm；403 窗口 / 503 未接线 / 409 busy）。 */
  async function handleSpeakerTest(req, res, path, method) {
    if (method === 'POST' && path === '/api/speaker-test') {
      const body = await readJsonBody(req)
      if (speakerTest.wired !== true) {
        sendError(res, 503, 'not_wired', SPEAKER_TEST_NOT_WIRED_REASON, { reason: SPEAKER_TEST_NOT_WIRED_REASON })
        return
      }
      if (speakerTest.windowOpen !== true) {
        sendError(res, 403, 'sound_window_closed', SPEAKER_TEST_WINDOW_CLOSED_REASON, {
          reason: SPEAKER_TEST_WINDOW_CLOSED_REASON,
          fixture: speakerTestFixture
        })
        return
      }
      const parsed = validateSpeakerTestRequest(body, { dacMax: SPEAKER_TEST_DAC_MAX })
      if (parsed.ok !== true) {
        sendError(res, parsed.status, parsed.code, parsed.message, parsed.details)
        return
      }
      const active = speakerTest.state.active === true
      if (active) {
        sendError(res, 409, 'busy', 'a speaker test tone is already playing: stop it before starting another one', {
          active: true,
          dac: speakerTest.state.dac,
          remaining_ms: speakerTest.state.remaining_ms
        })
        return
      }
      // 校准/热重配进行中时不出声（docs/API.md §12.2 busy 409）。
      const running = calibrationUnavailable ? null : activeJob()
      if (running !== null) {
        sendError(res, 409, 'busy', 'a calibration job is active: wait for it to finish or cancel it before playing a test tone', {
          active_job_id: running.id
        })
        return
      }
      speakerTest.state = {
        active: true,
        dac: parsed.request.dac,
        ms: parsed.request.ms,
        kind: parsed.request.kind,
        level_db: parsed.request.db,
        remaining_ms: parsed.request.ms,
        started_at: SPEAKER_TEST_FIXED_AT
      }
      sendJson(res, 200, buildSpeakerTestAck(parsed.request))
      return
    }
    if (method === 'POST' && path === '/api/speaker-test/stop') {
      const stopped = speakerTest.state.active === true
      speakerTest.state = {
        active: false,
        dac: null,
        ms: null,
        kind: null,
        level_db: null,
        remaining_ms: 0,
        started_at: null
      }
      sendJson(res, 200, buildSpeakerTestStopAck(stopped))
      return
    }
    sendError(res, 405, 'method_not_allowed', 'unsupported method/path: ' + method + ' ' + path)
  }

  /* ------------------------------------------------------- /api/meta 能力文档 */

  /** GET /api/meta：{api_version, web_version, ops, surfaces, calibration, host_link, not_wired}。 */
  function buildMetaJson() {
    const notWired = []
    if (!control.wired) {
      for (const op of ['mute', 'prepare', 'commit', 'abort', 'surface', 'format', 'inspect', 'rollback']) {
        notWired.push({ op, reason: control.reason })
      }
    }
    if (calibrationUnavailable) {
      notWired.push({
        op: 'calibration.jobs',
        reason: 'calibration job backend not wired (mock fixture: unavailable)'
      })
    }
    if (channelsSeed.unavailable) {
      notWired.push({ op: 'channels', reason: channelsSeed.reason })
    }
    if (channelsMode === 'host-link-unavailable') {
      notWired.push({ op: 'channels', reason: CHANNELS_HOST_LINK_REASON })
    }
    if (audioProfileSeed.available !== true) {
      notWired.push({ op: 'audio_profile', reason: AUDIO_PROFILE_NOT_WIRED_REASON })
      notWired.push({ op: 'device.inputs', reason: AUDIO_PROFILE_NOT_WIRED_REASON })
    }
    if (speakerTest.wired !== true) {
      notWired.push({ op: 'speaker_test', reason: SPEAKER_TEST_NOT_WIRED_REASON })
    }
    if (motu.detected !== true || motu.control_reachable !== true) {
      notWired.push({ op: 'motu.write', reason: motu.reason_detail })
    }
    return {
      ok: true,
      api_version: 1,
      web_version: 'mock',
      ops: control.wired
        ? ['status', 'mute', 'prepare', 'commit', 'abort', 'coeff', 'surface', 'inspect', 'rollback', 'format']
        : ['status'],
      surfaces: control.wired ? ['volume', 'routing', 'layout', 'format', 'mode'] : [],
      calibration: {
        wired: calibrationUnavailable !== true,
        reason: calibrationUnavailable
          ? 'calibration job backend not wired (mock fixture: unavailable)'
          : null
      },
      host_link: {
        target: control.wired ? 'rt-host' : 'none',
        mode: control.wired ? 'socket' : 'none',
        connected: control.wired,
        socket: control.wired ? '/run/khaen/rt-host.sock' : null,
        error: control.wired ? null : control.reason
      },
      not_wired: notWired
    }
  }

  function serializeJob(job) {
    const clone = {}
    for (const [key, value] of Object.entries(job)) {
      if (key === 'artifactContent' || key === 'ticks') {
        continue
      }
      clone[key] = value
    }
    return JSON.parse(JSON.stringify(clone))
  }

  function sortedJobs() {
    return [...calibrationJobs.values()].sort((a, b) => {
      const at = String(a.created_at ?? '')
      const bt = String(b.created_at ?? '')
      if (at === bt) {
        return String(b.id).localeCompare(String(a.id))
      }
      return bt.localeCompare(at)
    })
  }

  function activeJob() {
    return [...calibrationJobs.values()].find((job) => job.status === 'queued' || job.status === 'running') ?? null
  }

  function latestJob() {
    const jobs = sortedJobs()
    return jobs.length > 0 ? jobs[0] : null
  }

  function calibrationStatus() {
    return buildStatusFixture(statusFixture)
  }

  /* ------------------------------------------------------- 系统信息 REST mock */

  /** /api/system/info：fixture + mock 内部重启状态（restart 后 rt-host 视为已拉起）。 */
  function systemInfo() {
    const info = buildSystemInfoFixture(systemFixture)
    if (systemRestartState.rtHostStarted && info.services !== null && info.services !== undefined) {
      info.services.rt_host = { active: true, enabled: info.services.rt_host?.enabled ?? true, pid: 4247, reason: null }
      info.reasons = { ...(info.reasons ?? {}) }
      delete info.reasons['services.rt_host.pid']
      delete info.reasons['services.rt_host.active']
    }
    if (systemRestartState.restartedAt !== null) {
      info.restart = { ...(info.restart ?? {}), last_restarted_at: systemRestartState.restartedAt }
    }
    return info
  }

  /** /api/system/logs：fixture 决定 journalctl 是否可用；参数校验在路由层做。 */
  function systemLogs(unit, lines) {
    return buildSystemLogsFixture(systemFixture, unit, lines)
  }

  /** /api/system/diagnostics：info + status + 设备 + 最近日志摘要合并；绝不包含密码/凭据。 */
  function systemDiagnostics() {
    const info = systemInfo()
    const status = calibrationStatus()
    const generatedAt = new Date(mockNow()).toISOString()
    const logs = {}
    for (const unit of LOG_UNITS) {
      const built = systemLogs(unit, 20)
      logs[unit] = built.available
        ? { available: true, unit_name: LOG_UNIT_NAMES[unit], lines: built.body.lines }
        : { available: false, unit_name: LOG_UNIT_NAMES[unit], reason: built.reason, lines: [] }
    }
    return {
      schema: 'khaen-diagnostics/v1',
      generated_at: generatedAt,
      build: {
        crate_version: info.crate_version,
        release_version: info.release_version,
        commit: info.commit,
        build_profile: info.build_profile
      },
      host: {
        hostname: info.hostname,
        kernel: info.kernel,
        os: info.os,
        state_dir: info.state_dir,
        rt_status_file: info.rt_status_file,
        bind: info.bind
      },
      info,
      status,
      devices: info.devices,
      logs
    }
  }

  function parseLinesParam(raw) {
    if (raw === null || raw === undefined || raw === '') {
      return { ok: true, lines: 200 }
    }
    if (!/^[0-9]+$/.test(raw)) {
      return { ok: false, lines: 0 }
    }
    const parsed = Number.parseInt(raw, 10)
    if (!Number.isInteger(parsed) || parsed < 1 || parsed > 500) {
      return { ok: false, lines: parsed }
    }
    return { ok: true, lines: parsed }
  }

  /** /api/system/* 路由；返回 true 表示已处理。鉴权/CSRF 由调用方先行检查。 */
  async function handleSystem(req, res, path, method, url) {
    if (path === '/api/system/info' && method === 'GET') {
      sendJson(res, 200, systemInfo())
      return true
    }

    if (path === '/api/system/logs' && method === 'GET') {
      const unit = url.searchParams.get('unit') ?? 'daemon'
      if (!isLogUnit(unit)) {
        sendError(res, 400, 'invalid_request', 'unit must be daemon or rt-host', {
          field: 'unit',
          reason: 'must be "daemon" or "rt-host"'
        })
        return true
      }
      const parsed = parseLinesParam(url.searchParams.get('lines'))
      if (parsed.ok !== true) {
        sendError(res, 400, 'invalid_request', 'lines must be an integer in [1, 500]', {
          field: 'lines',
          reason: 'must be an integer in [1, 500]'
        })
        return true
      }
      const built = systemLogs(unit, parsed.lines)
      if (built.available !== true) {
        sendError(res, 503, 'logs_unavailable', built.reason, { unit, reason: built.reason })
        return true
      }
      sendJson(res, 200, built.body)
      return true
    }

    if (path === '/api/system/diagnostics' && method === 'GET') {
      const body = systemDiagnostics()
      const filename = buildDiagnosticsFilename(body.info, body.generated_at)
      sendJson(res, 200, body, { 'Content-Disposition': 'attachment; filename="' + filename + '"' })
      return true
    }

    if (path === '/api/system/password' && method === 'POST') {
      const found = sessionFromRequest(req)
      if (!found) {
        sendError(res, 401, 'not_authenticated', 'no valid session')
        return true
      }
      if (!hasValidCsrf(req, found.session)) {
        sendError(res, 403, 'csrf_invalid', 'missing or invalid CSRF token')
        return true
      }
      const body = await readJsonBody(req)
      if (typeof body.old !== 'string' || passwordHash !== hashPassword(body.old)) {
        sendError(res, 401, 'invalid_credentials', 'wrong current password')
        return true
      }
      if (typeof body.new !== 'string' || body.new.length < MIN_PASSWORD_LENGTH) {
        sendError(res, 400, 'invalid_password', 'new password must be at least ' + String(MIN_PASSWORD_LENGTH) + ' characters', {
          min_length: MIN_PASSWORD_LENGTH
        })
        return true
      }
      passwordHash = hashPassword(body.new)
      // 与 /api/auth/password 语义一致：改密后使所有旧会话失效，并下发新会话。
      sessions.clear()
      const created = createSession()
      setSessionCookie(res, created.token)
      sendJson(res, 200, { csrf: created.session.csrf, expires_in: SESSION_TTL_S })
      return true
    }

    if (path === '/api/system/restart' && method === 'POST') {
      const body = await readJsonBody(req)
      const target = body.target === undefined ? 'rt-host' : body.target
      if (target !== 'rt-host') {
        sendError(res, 400, 'invalid_request', 'unsupported restart target', {
          field: 'target',
          reason: 'only "rt-host" is supported'
        })
        return true
      }
      const info = systemInfo()
      if (info.restart?.allowed !== true) {
        const reason = info.restart?.reason ?? 'restart is disabled on this daemon'
        sendError(res, 403, 'restart_disabled', reason, { target, reason })
        return true
      }
      systemRestartState = { restartedAt: new Date(mockNow()).toISOString(), rtHostStarted: true }
      sendJson(res, 200, {
        ok: true,
        target: 'rt-host',
        restarted: true,
        restarted_at: systemRestartState.restartedAt,
        reason: null
      })
      return true
    }

    sendError(res, 405, 'method_not_allowed', 'unsupported method/path: ' + method + ' ' + path)
    return true
  }

  function calibrationGate(step, key, status, reason = null, task = null) {
    return { step, key, status, reason, task }
  }

  /** 由 status / wizard 配置 / uploads / 最近 job 推导 8 步 gate（与 T36 的推导口径一致）。 */
  function buildWizardState() {
    const status = calibrationStatus()
    const plan = status.plan ?? {}
    const reasons = {}
    const config = wizardConfig ?? buildWizardDefaults()
    const saved = wizardConfig !== null
    const measurementReady =
      wizardMeasurement.baseline_upload_id !== null &&
      wizardMeasurement.applied_upload_id !== null &&
      calibrationUploads.has(wizardMeasurement.baseline_upload_id) &&
      calibrationUploads.has(wizardMeasurement.applied_upload_id)

    // 1. 设备检查：device present + rate_asserted + host_link connected（全部来自 /api/status）。
    const devicePresent = status.device?.state === 'present'
    const rateAsserted = status.format?.rate_asserted === true
    const hostLinkConnected = status.host_link?.connected === true
    let step1
    if (devicePresent && rateAsserted && hostLinkConnected) {
      step1 = calibrationGate(1, 'device_check', 'passed')
    } else {
      const missing = []
      if (!devicePresent) {
        missing.push('device not present')
      }
      if (!rateAsserted) {
        missing.push('device rate not asserted')
      }
      if (!hostLinkConnected) {
        missing.push('control link not connected')
      }
      step1 = calibrationGate(1, 'device_check', 'blocked', 'device check failed: ' + missing.join(', '))
    }

    // 2. 布局与声道：/api/channels 属 M4，mock 同样未实现 -> 显式 not_available（不伪造映射）。
    const step2 = calibrationGate(
      2,
      'layout_channels',
      'not_available',
      '/api/channels is not implemented in the mock (M4)',
      'M4'
    )

    // 3. 麦克风与校准设置：配置已保存且关键字段齐全。
    const settingsReady =
      saved &&
      config.mic_cal_sha256 !== null &&
      config.orientation !== null &&
      config.target_mode !== null &&
      Array.isArray(config.single_point?.centers_hz) &&
      config.single_point.centers_hz.length > 0
    const step3 = settingsReady
      ? calibrationGate(3, 'mic_settings', 'passed')
      : calibrationGate(3, 'mic_settings', 'blocked', saved ? 'wizard settings incomplete' : 'wizard settings not saved')

    // 4. 测点：khaen P1 只有单点（皇帝位固定）；多点/ART 在 UI 灰态声明 NOT_AVAILABLE。
    const step4 = settingsReady
      ? calibrationGate(4, 'positions', 'passed')
      : calibrationGate(4, 'positions', 'blocked', 'single listening position not confirmed')

    // 5. 测量（离线）：两段 WAV 已上传；实时测量在本版本 NOT_AVAILABLE（无采集服务，T42）。
    const step5 = measurementReady
      ? calibrationGate(5, 'measurement', 'passed')
      : calibrationGate(5, 'measurement', 'blocked', 'baseline/applied WAV not uploaded yet')

    // 6/7/8 依赖 job backend。
    const job = latestJob()
    let step6
    let step7
    let step8
    if (calibrationUnavailable) {
      const reason = 'calibration job backend not wired (T70h-b)'
      step6 = calibrationGate(6, 'analysis', 'blocked', reason, 'T70h-b')
      step7 = calibrationGate(7, 'design', 'blocked', reason, 'T70h-b')
      step8 = calibrationGate(8, 'review_apply', 'blocked', reason, 'T70h-b')
      reasons.job = reason
    } else if (job === null) {
      step6 = calibrationGate(6, 'analysis', 'blocked', 'no analysis job yet')
      step7 = calibrationGate(7, 'design', 'blocked', 'analysis job has not succeeded')
      step8 = calibrationGate(8, 'review_apply', 'blocked', 'no job to apply')
    } else if (job.status === 'succeeded') {
      step6 = calibrationGate(6, 'analysis', 'passed')
      const admission = job.result?.design?.admission ?? null
      if (job.result?.plan != null && admission !== null && admission.accepted === true) {
        step7 = calibrationGate(7, 'design', 'passed')
      } else {
        const reason =
          job.result?.design?.residual === 'NONE'
            ? 'design residual NONE: plan not produced'
            : 'plan admission not accepted'
        step7 = calibrationGate(7, 'design', 'blocked', reason)
      }
      if (job.apply_state === 'applied') {
        step8 = calibrationGate(8, 'review_apply', 'passed')
      } else if (job.apply_state === 'rolled_back') {
        step8 = calibrationGate(8, 'review_apply', 'blocked', 'plan rolled back to the previous version')
      } else {
        step8 = calibrationGate(8, 'review_apply', 'blocked', 'plan not applied yet')
      }
    } else if (job.status === 'failed') {
      const code = job.error?.code ?? 'E_JOB_FAILED'
      const message = job.error?.message ?? 'calibration job failed'
      step6 = calibrationGate(6, 'analysis', 'blocked', code + ': ' + message)
      step7 = calibrationGate(7, 'design', 'blocked', 'analysis job failed: ' + code)
      step8 = calibrationGate(8, 'review_apply', 'blocked', 'no succeeded job to apply')
    } else {
      step6 = calibrationGate(6, 'analysis', 'blocked', 'job ' + job.id + ' is ' + job.status)
      step7 = calibrationGate(7, 'design', 'blocked', 'analysis job is not succeeded')
      step8 = calibrationGate(8, 'review_apply', 'blocked', 'no succeeded job to apply')
    }

    const steps = [step1, step2, step3, step4, step5, step6, step7, step8]
    const firstBlocked = steps.find((step) => step.status === 'blocked')
    const currentStep = firstBlocked === undefined ? 8 : firstBlocked.step

    const baselineUpload =
      wizardMeasurement.baseline_upload_id === null ? null : calibrationUploads.get(wizardMeasurement.baseline_upload_id) ?? null
    const appliedUpload =
      wizardMeasurement.applied_upload_id === null ? null : calibrationUploads.get(wizardMeasurement.applied_upload_id) ?? null

    const snapshot =
      plan.version !== null && plan.version !== undefined && plan.graph_id !== null && plan.graph_id !== undefined
        ? { version: plan.version, graph_id: plan.graph_id, sha256: plan.sha256 ?? null }
        : null
    if (snapshot === null) {
      reasons.snapshot = 'no applied plan to snapshot (status.plan.version/graph_id are null)'
    }
    if (job !== null && job.result === null) {
      reasons.result = 'job has no result yet'
    }

    return {
      schema: 'khaen-calibration-wizard-state/v1',
      current_step: currentStep,
      steps,
      active_job_id: activeJob() === null ? null : activeJob().id,
      last_job_id: job === null ? null : job.id,
      measurement: {
        baseline_upload_id: baselineUpload === null ? null : baselineUpload.upload_id,
        applied_upload_id: appliedUpload === null ? null : appliedUpload.upload_id,
        baseline_sha256: baselineUpload === null ? null : baselineUpload.sha256,
        applied_sha256: appliedUpload === null ? null : appliedUpload.sha256
      },
      snapshot,
      updated_at: new Date(mockNow()).toISOString(),
      reasons
    }
  }

  function sendBinary(res, status, buffer, contentType, extraHeaders) {
    const headers = {
      'Content-Type': contentType,
      'Content-Length': String(buffer.length),
      'Cache-Control': 'no-store',
      'X-KHAEN-API-Version': '1'
    }
    if (extraHeaders) {
      Object.assign(headers, extraHeaders)
    }
    res.writeHead(status, headers)
    res.end(buffer)
  }

  /**
   * 读原始字节 body（上传用；超限 reject('too_large')，由调用方回 413）。
   * 注意：不 destroy 请求——先 resume() 丢弃剩余 body，再让上层把 413 写回同一连接，
   * 否则浏览器/Node fetch 只会看到连接被重置（network error），拿不到稳定错误码。
   */
  function readRawBody(req, maxBytes) {
    return new Promise((resolve, reject) => {
      const declared = Number(req.headers['content-length'])
      if (Number.isFinite(declared) && declared > maxBytes) {
        req.resume()
        reject(new Error('too_large'))
        return
      }
      const chunks = []
      let size = 0
      let failed = false
      req.on('data', (chunk) => {
        if (failed) {
          return
        }
        size += chunk.length
        if (size > maxBytes) {
          failed = true
          chunks.length = 0
          req.resume()
          reject(new Error('too_large'))
          return
        }
        chunks.push(chunk)
      })
      req.on('end', () => {
        if (!failed) {
          resolve(Buffer.concat(chunks))
        }
      })
      req.on('error', (error) => {
        if (!failed) {
          reject(error)
        }
      })
    })
  }

  function normalizeContentType(raw) {
    if (typeof raw !== 'string') {
      return ''
    }
    return raw.split(';')[0].trim().toLowerCase()
  }

  function parseWizardConfigPut(body) {
    const allowed = new Set([
      'mic_cal_sha256',
      'mic_cal_filename',
      'orientation',
      'target_mode',
      'single_point',
      'measurement'
    ])
    for (const key of Object.keys(body)) {
      if (!allowed.has(key)) {
        return { ok: false, field: key, reason: 'unknown field' }
      }
    }
    const defaults = buildWizardDefaults()
    const micSha = body.mic_cal_sha256 === undefined ? null : body.mic_cal_sha256
    if (micSha !== null && (typeof micSha !== 'string' || !/^[0-9a-f]{64}$/.test(micSha))) {
      return { ok: false, field: 'mic_cal_sha256', reason: 'must be null or a 64-char lowercase hex sha256' }
    }
    const micFilename = body.mic_cal_filename === undefined ? null : body.mic_cal_filename
    if (micFilename !== null && (typeof micFilename !== 'string' || micFilename.length === 0 || micFilename.length > 255)) {
      return { ok: false, field: 'mic_cal_filename', reason: 'must be null or a 1..255 char string' }
    }
    const orientation = body.orientation === undefined ? null : body.orientation
    if (orientation !== null && orientation !== '0deg' && orientation !== '90deg') {
      return { ok: false, field: 'orientation', reason: 'must be null, "0deg" or "90deg"' }
    }
    const targetMode = body.target_mode === undefined ? null : body.target_mode
    if (targetMode !== null && targetMode !== 'flat' && targetMode !== 'tilt10') {
      return { ok: false, field: 'target_mode', reason: 'must be null, "flat" or "tilt10"' }
    }
    const singlePointRaw = body.single_point === undefined ? defaults.single_point : body.single_point
    if (typeof singlePointRaw !== 'object' || singlePointRaw === null || Array.isArray(singlePointRaw)) {
      return { ok: false, field: 'single_point', reason: 'must be an object' }
    }
    const centers = singlePointRaw.centers_hz
    if (!Array.isArray(centers) || centers.length === 0 || centers.length > 64) {
      return { ok: false, field: 'single_point.centers_hz', reason: 'must be an array of 1..64 numbers' }
    }
    for (const hz of centers) {
      if (typeof hz !== 'number' || !Number.isFinite(hz) || hz <= 0 || hz > 24000) {
        return { ok: false, field: 'single_point.centers_hz', reason: 'each center must be a finite number in (0, 24000]' }
      }
    }
    const tolerance = singlePointRaw.tolerance_db
    if (typeof tolerance !== 'number' || !Number.isFinite(tolerance) || tolerance < 0.1 || tolerance > 24) {
      return { ok: false, field: 'single_point.tolerance_db', reason: 'must be a number in [0.1, 24]' }
    }
    const minBands = singlePointRaw.min_bands
    if (!Number.isInteger(minBands) || minBands < 1 || minBands > 64) {
      return { ok: false, field: 'single_point.min_bands', reason: 'must be an integer in [1, 64]' }
    }
    let measurement = null
    if (body.measurement !== undefined && body.measurement !== null) {
      const raw = body.measurement
      if (typeof raw !== 'object' || Array.isArray(raw)) {
        return { ok: false, field: 'measurement', reason: 'must be an object' }
      }
      for (const key of Object.keys(raw)) {
        if (key !== 'baseline_upload_id' && key !== 'applied_upload_id' && key !== 'plan_sha') {
          return { ok: false, field: 'measurement.' + key, reason: 'unknown field' }
        }
      }
      const baseline = raw.baseline_upload_id ?? null
      const applied = raw.applied_upload_id ?? null
      const planSha = raw.plan_sha ?? null
      for (const [field, value] of [
        ['baseline_upload_id', baseline],
        ['applied_upload_id', applied]
      ]) {
        if (value !== null && typeof value !== 'string') {
          return { ok: false, field: 'measurement.' + field, reason: 'must be null or a string' }
        }
        if (value !== null && !calibrationUploads.has(value)) {
          return { ok: false, field: 'measurement.' + field, reason: 'upload not found: ' + value }
        }
      }
      if (planSha !== null && typeof planSha !== 'string') {
        return { ok: false, field: 'measurement.plan_sha', reason: 'must be null or a string' }
      }
      measurement = { baseline_upload_id: baseline, applied_upload_id: applied, plan_sha: planSha }
    }
    return {
      ok: true,
      config: {
        mic_cal_sha256: micSha,
        mic_cal_filename: micFilename,
        orientation,
        target_mode: targetMode,
        single_point: { centers_hz: [...centers], tolerance_db: tolerance, min_bands: minBands }
      },
      measurement
    }
  }

  function applyJobRecord(job) {
    const status = calibrationStatus()
    const before = status.plan ?? {}
    const snapshot =
      before.version !== null && before.version !== undefined && before.graph_id !== null && before.graph_id !== undefined
        ? { version: before.version, graph_id: before.graph_id, sha256: before.sha256 ?? null }
        : null
    const appliedVersion = (typeof before.version === 'number' ? before.version : 0) + 1
    appliedJobId = job.id
    appliedSnapshot = snapshot
    job.apply_state = 'applied'
    job.applied_version = appliedVersion
    return {
      ok: true,
      applied: true,
      job_id: job.id,
      applied_version: appliedVersion,
      graph_id: job.result.plan.graph_id,
      sha256: job.result.plan.sha256 ?? null,
      snapshot,
      ack: { synthetic: true, actual: false },
      readback: { mock: true, applied_version: appliedVersion, graph_id: job.result.plan.graph_id },
      applied_at: new Date(mockNow()).toISOString()
    }
  }

  /* ------------------------------------- apply/rollback 两阶段（docs/API.md §7 目标形状） */

  function calibrationStagedToken(kind, jobId) {
    return createHash('sha256').update('khaen-calibration-staged:v1:' + kind + ':' + jobId).digest('hex').slice(0, 32)
  }

  /** 解析 staged 请求：无 stage = 旧单发；prepare/commit/abort = 两阶段。 */
  function calibrationParseStaged(body) {
    if (typeof body !== 'object' || body === null || Array.isArray(body) || body.stage === undefined) {
      return { kind: 'legacy' }
    }
    const stage = body.stage
    if (stage === 'prepare') {
      return { kind: 'prepare' }
    }
    if (stage !== 'commit' && stage !== 'abort') {
      return {
        error: {
          status: 400,
          code: 'invalid_request',
          message: 'stage must be prepare, commit or abort (got ' + JSON.stringify(stage) + ')',
          details: { field: 'stage' }
        }
      }
    }
    const token = body.engine_token
    if (typeof token !== 'string' || token.length === 0) {
      return {
        error: {
          status: 400,
          code: 'invalid_request',
          message: 'field engine_token must be a non-empty string for stage=' + stage,
          details: { field: 'engine_token' }
        }
      }
    }
    return { kind: stage, token }
  }

  function calibrationStagedTokenError() {
    return {
      status: 409,
      code: 'engine_token_invalid',
      message: 'engine_token mismatch, already consumed, or does not belong to this job'
    }
  }

  /** staged prepare：只签一次性 token（不改变运行态）。 */
  function calibrationStagedPrepare(job, kind) {
    const status = calibrationStatus()
    const planVersion = status.plan === undefined || status.plan === null ? null : status.plan.version
    const previousVersion = typeof planVersion === 'number' ? planVersion : null
    const targetVersion =
      kind === 'apply' ? (previousVersion === null ? 1 : previousVersion + 1) : appliedSnapshot === null ? null : appliedSnapshot.version
    const planSha =
      job.result === null || job.result === undefined || job.result.plan === undefined || job.result.plan === null
        ? null
        : job.result.plan.sha256 ?? null
    const token = calibrationStagedToken(kind, job.id)
    calibrationStagedOps.set(token, {
      job_id: job.id,
      kind,
      plan_sha256: planSha,
      previous_version: previousVersion,
      target_version: targetVersion
    })
    return {
      ok: true,
      stage: 'prepare',
      job_id: job.id,
      engine_token: token,
      plan_sha256: planSha,
      previous_version: previousVersion,
      target_version: targetVersion,
      job: { schema: 'khaen-calibration-job/v1', job: serializeJob(job) },
      reasons: {}
    }
  }

  /** staged abort：丢弃草稿（运行态不变；token 一次性消费）。 */
  function calibrationStagedAbort(job, op) {
    return {
      ok: true,
      stage: 'abort',
      aborted: true,
      job_id: job.id,
      plan_sha256: op.plan_sha256,
      target_version: op.target_version,
      control: { ok: true, aborted: true, note: 'the mock has no real engine draft to discard; the token is consumed' },
      reasons: {}
    }
  }

  /** staged apply commit：真生效并回写 job（与旧单发同一套 applyJobRecord）。 */
  function calibrationStagedApplyCommit(job, op) {
    const receipt = applyJobRecord(job)
    return {
      ok: true,
      stage: 'commit',
      applied: true,
      job_id: job.id,
      applied_version: receipt.applied_version,
      previous_version: op.previous_version,
      applied_block: receipt.applied_version * 128 + 1,
      graph_id: receipt.graph_id,
      sha256: op.plan_sha256,
      snapshot: receipt.snapshot,
      ack: receipt.ack,
      readback: receipt.readback,
      applied_at: receipt.applied_at,
      job: { schema: 'khaen-calibration-job/v1', job: serializeJob(job) },
      reasons: {}
    }
  }

  /** staged rollback commit：回滚到 apply 前的快照并回读。 */
  function calibrationStagedRollbackCommit(job, op) {
    const snapshot = appliedSnapshot
    job.apply_state = 'rolled_back'
    job.applied_version = null
    appliedJobId = null
    appliedSnapshot = null
    return {
      ok: true,
      stage: 'commit',
      rolled_back: true,
      job_id: job.id,
      restored_version: snapshot === null ? null : snapshot.version,
      applied_version: op.target_version,
      graph_id: snapshot === null ? null : snapshot.graph_id,
      sha256: snapshot === null ? null : snapshot.sha256 ?? null,
      readback: snapshot === null ? null : { mock: true, restored_version: snapshot.version, graph_id: snapshot.graph_id },
      rolled_back_at: new Date(mockNow()).toISOString(),
      job: { schema: 'khaen-calibration-job/v1', job: serializeJob(job) },
      reasons: {}
    }
  }

  /** 处理 staged 分支；返回 null = 旧单发（交给调用方）。 */
  function calibrationStagedOutcome(job, kind, staged) {
    if (staged.kind === 'legacy') {
      return null
    }
    if (staged.kind === 'prepare') {
      return { response: calibrationStagedPrepare(job, kind) }
    }
    const op = calibrationStagedOps.get(staged.token)
    if (op === undefined || op.job_id !== job.id || op.kind !== kind) {
      return { error: calibrationStagedTokenError() }
    }
    if (staged.kind === 'abort') {
      calibrationStagedOps.delete(staged.token)
      return { response: calibrationStagedAbort(job, op) }
    }
    if (kind === 'rollback' && appliedSnapshot === null) {
      return { error: { status: 409, code: 'rollback_unavailable', message: 'no previous plan snapshot is available' } }
    }
    calibrationStagedOps.delete(staged.token)
    return {
      response: kind === 'apply' ? calibrationStagedApplyCommit(job, op) : calibrationStagedRollbackCommit(job, op)
    }
  }

  /** 校准路由；返回 true 表示已处理。鉴权/CSRF 由调用方先行检查。 */
  async function handleCalibration(req, res, path, method, url) {
    const prefix = '/api/calibration/'
    if (!path.startsWith(prefix)) {
      return false
    }
    const rest = path.slice(prefix.length)
    const segments = rest.split('/').filter((part) => part.length > 0).map((part) => decodeURIComponent(part))

    // 与 T36 的 NotWired 默认实现一致：GET /jobs 返回 200 {available:false,jobs:[],reason}，
    // 其它 job/apply/artifact 接口 503 not_wired（向导配置与 state 的真实部分照常可用）。
    const isJobListGet = segments.length === 1 && segments[0] === 'jobs' && method === 'GET'
    if (calibrationUnavailable && segments[0] === 'jobs' && !isJobListGet) {
      sendError(res, 503, 'not_wired', 'calibration job backend not wired (T70h-b)')
      return true
    }

    // POST /api/calibration/uploads（原始字节；不依赖 job backend）
    if (segments.length === 1 && segments[0] === 'uploads' && method === 'POST') {
      const contentType = normalizeContentType(req.headers['content-type'])
      if (contentType !== 'application/octet-stream' && contentType !== 'audio/wav') {
        sendError(res, 415, 'unsupported_format', 'upload Content-Type must be application/octet-stream or audio/wav')
        return true
      }
      let buffer
      try {
        buffer = await readRawBody(req, maxUploadBytes)
      } catch (error) {
        if (error instanceof Error && error.message === 'too_large') {
          sendError(res, 413, 'upload_too_large', 'upload exceeds ' + String(maxUploadBytes) + ' bytes')
          return true
        }
        throw error
      }
      if (buffer.length === 0) {
        sendError(res, 400, 'invalid_upload', 'upload body is empty')
        return true
      }
      const sha = createHash('sha256').update(buffer).digest('hex')
      const existing = calibrationUploads.get(sha)
      const role = typeof req.headers['x-khaen-upload-role'] === 'string' ? req.headers['x-khaen-upload-role'] : null
      if (existing !== undefined) {
        sendJson(res, 200, {
          upload_id: existing.upload_id,
          sha256: existing.sha256,
          bytes: existing.bytes,
          created: false
        })
        return true
      }
      const record = {
        upload_id: sha,
        sha256: sha,
        bytes: buffer.length,
        content_type: contentType,
        role,
        created_at: new Date(mockNow()).toISOString()
      }
      calibrationUploads.set(sha, record)
      // dev 便利：带 role=baseline/applied 的 WAV 自动进入向导 measurement 草稿。
      if (role === 'baseline') {
        wizardMeasurement.baseline_upload_id = sha
      } else if (role === 'applied') {
        wizardMeasurement.applied_upload_id = sha
      }
      sendJson(
        res,
        201,
        { upload_id: record.upload_id, sha256: record.sha256, bytes: record.bytes, created: true },
        { 'X-KHAEN-Upload-Role': role ?? '' }
      )
      return true
    }

    // /api/calibration/jobs[...]
    if (segments[0] === 'jobs') {
      const jobId = segments.length >= 2 ? segments[1] : null
      const action = segments.length >= 3 ? segments[2] : null

      if (jobId === null && method === 'GET') {
        const limit = Math.max(1, Math.min(100, Number.parseInt(url.searchParams.get('limit') ?? '20', 10) || 20))
        const offset = Math.max(0, Number.parseInt(url.searchParams.get('offset') ?? '0', 10) || 0)
        const all = sortedJobs()
        const page = all.slice(offset, offset + limit)
        sendJson(res, 200, {
          ok: true,
          schema: 'khaen-calibration-jobs/v1',
          available: !calibrationUnavailable,
          reason: calibrationUnavailable ? 'calibration job backend not wired (T70h-b)' : null,
          engine: null,
          active_job_id: activeJob() === null ? null : activeJob().id,
          jobs: page.map(serializeJob),
          total: all.length,
          limit,
          offset,
          reasons: {}
        })
        return true
      }

      if (jobId === null && method === 'POST') {
        const body = await readJsonBody(req)
        if (body.kind !== 'single_point_offline') {
          sendError(res, 400, 'invalid_request', 'unsupported job kind', { field: 'kind', reason: 'must be single_point_offline' })
          return true
        }
        const inputs = body.inputs
        if (typeof inputs !== 'object' || inputs === null || Array.isArray(inputs)) {
          sendError(res, 400, 'invalid_request', 'inputs must be an object', { field: 'inputs', reason: 'must be an object' })
          return true
        }
        if (typeof inputs.plan_sha !== 'string' || inputs.plan_sha.length === 0) {
          sendError(res, 400, 'invalid_request', 'plan_sha is required', { field: 'inputs.plan_sha', reason: 'must be a non-empty string' })
          return true
        }
        for (const field of ['baseline_upload_id', 'applied_upload_id']) {
          const value = inputs[field]
          if (typeof value !== 'string' || value.length === 0) {
            sendError(res, 400, 'invalid_request', field + ' is required', { field: 'inputs.' + field, reason: 'must be a non-empty string' })
            return true
          }
          if (!calibrationUploads.has(value)) {
            sendError(res, 404, 'upload_not_found', 'upload not found: ' + value, { field: 'inputs.' + field })
            return true
          }
        }
        if (!Array.isArray(inputs.centers_hz) || inputs.centers_hz.length === 0 || inputs.centers_hz.length > 64) {
          sendError(res, 400, 'invalid_request', 'centers_hz must be a non-empty array', { field: 'inputs.centers_hz' })
          return true
        }
        for (const hz of inputs.centers_hz) {
          if (typeof hz !== 'number' || !Number.isFinite(hz) || hz <= 0 || hz > 24000) {
            sendError(res, 400, 'invalid_request', 'centers_hz entries must be finite numbers in (0, 24000]', {
              field: 'inputs.centers_hz'
            })
            return true
          }
        }
        if (typeof inputs.tolerance_db !== 'number' || !Number.isFinite(inputs.tolerance_db) || inputs.tolerance_db <= 0) {
          sendError(res, 400, 'invalid_request', 'tolerance_db is required', { field: 'inputs.tolerance_db' })
          return true
        }
        if (!Number.isInteger(inputs.min_bands) || inputs.min_bands < 1) {
          sendError(res, 400, 'invalid_request', 'min_bands is required', { field: 'inputs.min_bands' })
          return true
        }
        if (activeJob() !== null) {
          sendError(res, 409, 'switch_busy', 'another calibration job is active: ' + activeJob().id)
          return true
        }
        calibrationJobSeq += 1
        const id = 'job-mock-' + String(calibrationJobSeq).padStart(4, '0')
        const job = createQueuedJobRecord({
          id,
          now: mockNow(),
          inputs: {
            plan_sha: inputs.plan_sha,
            baseline_upload_id: inputs.baseline_upload_id,
            applied_upload_id: inputs.applied_upload_id,
            centers_hz: [...inputs.centers_hz],
            tolerance_db: inputs.tolerance_db,
            min_bands: inputs.min_bands
          }
        })
        calibrationJobs.set(id, job)
        sendJson(res, 202, {
          ok: true,
          job_id: id,
          state: 'queued',
          location: '/api/calibration/jobs/' + id
        })
        return true
      }

      if (jobId !== null && action === null && method === 'GET') {
        const job = calibrationJobs.get(jobId)
        if (job === undefined) {
          sendError(res, 404, 'job_not_found', 'unknown job: ' + jobId)
          return true
        }
        // mock 的进度推进：每次 GET 详情推进一步（真实 daemon 由 worker 推进；进度仍来自真实状态）。
        tickJobRecord(job, mockNow())
        const detailReasons = {}
        if (job.result === null) {
          detailReasons.result = 'job is ' + job.status + ': analysis results exist after the closed_loop and bundle stages'
        }
        if (job.engine === null) {
          detailReasons.engine =
            'offline single-point jobs do not open an audio engine in the daemon process; engine is reported by the job backend when it has one'
        }
        if (job.apply_state === 'not_applied') {
          detailReasons['apply.applied_version'] = 'job has not been applied to the control plane (apply_state=not_applied)'
        }
        const serialized = serializeJob(job)
        sendJson(res, 200, {
          ok: true,
          schema: 'khaen-calibration-job/v1',
          job: serialized,
          // docs/API.md §7【加法】：详情顶层也暴露 result/artifacts/error（与 job 内同源）。
          result: serialized.result ?? null,
          artifacts: serialized.artifacts ?? [],
          error: serialized.error ?? null,
          reasons: detailReasons
        })
        return true
      }

      if (jobId !== null && action === 'cancel' && method === 'POST') {
        const job = calibrationJobs.get(jobId)
        if (job === undefined) {
          sendError(res, 404, 'job_not_found', 'unknown job: ' + jobId)
          return true
        }
        if (job.status !== 'queued' && job.status !== 'running') {
          sendError(res, 409, 'job_not_active', 'job is not active: ' + job.status)
          return true
        }
        job.status = 'cancelled'
        job.finished_at = new Date(mockNow()).toISOString()
        job.steps = job.steps.map((step) =>
          step.status === 'pending' || step.status === 'running' ? { ...step, status: 'cancelled' } : step
        )
        sendJson(res, 200, { ok: true, job_id: job.id, status: job.status, state: job.status })
        return true
      }

      if (jobId !== null && action === 'apply' && method === 'POST') {
        const job = calibrationJobs.get(jobId)
        if (job === undefined) {
          sendError(res, 404, 'job_not_found', 'unknown job: ' + jobId)
          return true
        }
        if (job.status !== 'succeeded') {
          sendError(res, 409, 'job_not_succeeded', 'job is not succeeded: ' + job.status)
          return true
        }
        if (job.result === null || job.result.plan == null) {
          sendError(res, 409, 'plan_unavailable', 'job has no plan artifact to apply')
          return true
        }
        const staged = calibrationParseStaged(await readJsonBody(req))
        if (staged.error !== undefined) {
          sendError(res, staged.error.status, staged.error.code, staged.error.message, staged.error.details)
          return true
        }
        const outcome = calibrationStagedOutcome(job, 'apply', staged)
        if (outcome === null) {
          sendJson(res, 200, applyJobRecord(job))
          return true
        }
        if (outcome.error !== undefined) {
          sendError(res, outcome.error.status, outcome.error.code, outcome.error.message, outcome.error.details)
          return true
        }
        sendJson(res, 200, outcome.response)
        return true
      }

      if (jobId !== null && action === 'rollback' && method === 'POST') {
        const job = calibrationJobs.get(jobId)
        if (job === undefined) {
          sendError(res, 404, 'job_not_found', 'unknown job: ' + jobId)
          return true
        }
        if (job.apply_state !== 'applied' || appliedJobId !== job.id) {
          sendError(res, 409, 'rollback_unavailable', 'job has no applied version to roll back')
          return true
        }
        if (appliedSnapshot === null) {
          sendError(res, 409, 'rollback_unavailable', 'no previous plan snapshot is available')
          return true
        }
        const staged = calibrationParseStaged(await readJsonBody(req))
        if (staged.error !== undefined) {
          sendError(res, staged.error.status, staged.error.code, staged.error.message, staged.error.details)
          return true
        }
        const outcome = calibrationStagedOutcome(job, 'rollback', staged)
        if (outcome !== null) {
          if (outcome.error !== undefined) {
            sendError(res, outcome.error.status, outcome.error.code, outcome.error.message, outcome.error.details)
            return true
          }
          sendJson(res, 200, outcome.response)
          return true
        }
        const snapshot = appliedSnapshot
        job.apply_state = 'rolled_back'
        job.applied_version = null
        appliedJobId = null
        appliedSnapshot = null
        sendJson(res, 200, {
          ok: true,
          rolled_back: true,
          restored_version: snapshot.version,
          graph_id: snapshot.graph_id,
          sha256: snapshot.sha256 ?? null,
          readback: { mock: true, restored_version: snapshot.version, graph_id: snapshot.graph_id },
          rolled_back_at: new Date(mockNow()).toISOString()
        })
        return true
      }

      if (jobId !== null && action === 'artifacts' && segments.length >= 4 && method === 'GET') {
        const job = calibrationJobs.get(jobId)
        if (job === undefined) {
          sendError(res, 404, 'job_not_found', 'unknown job: ' + jobId)
          return true
        }
        const name = segments[3]
        const artifact = (job.artifacts ?? []).find((item) => item.name === name)
        const content = (job.artifactContent ?? []).find((item) => item.name === name)
        if (artifact === undefined || content === undefined) {
          sendError(res, 404, 'artifact_not_found', 'unknown artifact: ' + name)
          return true
        }
        // daemon 真实形态（crates/daemon/src/calibration/api.rs）：JSON/二进制产物都带
        // Content-Disposition: attachment; filename="<name>"，浏览器/客户端按文件名保存。
        sendBinary(res, 200, content.content, content.content_type, {
          'X-KHAEN-Artifact-SHA256': artifact.sha256 ?? '',
          'Content-Disposition': 'attachment; filename="' + name + '"'
        })
        return true
      }

      sendError(res, 405, 'method_not_allowed', 'unsupported method/path: ' + method + ' ' + path)
      return true
    }

    // /api/calibration/wizard[...]
    if (segments[0] === 'wizard') {
      if (segments.length === 1 && method === 'GET') {
        const defaults = buildWizardDefaults()
        sendJson(res, 200, {
          schema: 'khaen-calibration-wizard/v1',
          config: wizardConfig ?? defaults,
          defaults,
          saved: wizardConfig !== null,
          updated_at: wizardConfigUpdatedAt,
          reasons: {}
        })
        return true
      }
      if (segments.length === 1 && method === 'PUT') {
        const body = await readJsonBody(req)
        const parsed = parseWizardConfigPut(body)
        if (parsed.ok !== true) {
          sendError(res, 400, 'invalid_request', parsed.reason, { field: parsed.field, reason: parsed.reason })
          return true
        }
        wizardConfig = parsed.config
        wizardConfigUpdatedAt = new Date(mockNow()).toISOString()
        if (parsed.measurement !== null) {
          wizardMeasurement = parsed.measurement
        }
        const defaults = buildWizardDefaults()
        sendJson(res, 200, {
          schema: 'khaen-calibration-wizard/v1',
          config: wizardConfig,
          defaults,
          saved: true,
          updated_at: wizardConfigUpdatedAt,
          reasons: {}
        })
        return true
      }
      if (segments.length === 2 && segments[1] === 'state' && method === 'GET') {
        sendJson(res, 200, buildWizardState())
        return true
      }
      if (segments.length === 2 && segments[1] === 'apply' && method === 'POST') {
        if (calibrationUnavailable) {
          sendError(res, 503, 'not_wired', 'calibration job backend not wired (T70h-b)')
          return true
        }
        const job = latestJob()
        if (job === null) {
          // docs/API.md §7（裁决 H）：无可用 job 409 no_applicable_job。
          sendError(res, 409, 'no_applicable_job', 'no calibration job to apply: no calibration job has run yet')
          return true
        }
        if (job.status !== 'succeeded') {
          sendError(res, 409, 'job_not_succeeded', 'latest job is not succeeded: ' + job.status)
          return true
        }
        if (job.result === null || job.result.plan == null) {
          sendError(res, 409, 'plan_unavailable', 'job has no plan artifact to apply')
          return true
        }
        sendJson(res, 200, applyJobRecord(job))
        return true
      }
      sendError(res, 405, 'method_not_allowed', 'unsupported method/path: ' + method + ' ' + path)
      return true
    }

    sendError(res, 404, 'not_found', 'unknown calibration path: ' + path)
    return true
  }

  function sendJson(res, status, body, extraHeaders) {
    const headers = {
      'Content-Type': 'application/json; charset=utf-8',
      'Cache-Control': 'no-store',
      'X-KHAEN-API-Version': '1',
      'X-KHAEN-Web-Version': 'mock'
    }
    if (extraHeaders) {
      Object.assign(headers, extraHeaders)
    }
    res.writeHead(status, headers)
    res.end(JSON.stringify(body))
  }

  function sendError(res, status, code, message, details, extraHeaders) {
    sendJson(res, status, { error: { code, message, details: details === undefined ? {} : details } }, extraHeaders)
  }

  function readJsonBody(req) {
    return new Promise((resolve, reject) => {
      const chunks = []
      let size = 0
      req.on('data', (chunk) => {
        size += chunk.length
        if (size > MAX_BODY_BYTES) {
          reject(new Error('body too large'))
          req.destroy()
          return
        }
        chunks.push(chunk)
      })
      req.on('end', () => {
        const text = Buffer.concat(chunks).toString('utf8')
        if (text.length === 0) {
          resolve({})
          return
        }
        try {
          const parsed = JSON.parse(text)
          resolve(typeof parsed === 'object' && parsed !== null ? parsed : {})
        } catch {
          reject(new Error('invalid JSON body'))
        }
      })
      req.on('error', reject)
    })
  }

  function parseCookies(header) {
    const cookies = {}
    if (typeof header !== 'string') {
      return cookies
    }
    for (const part of header.split(';')) {
      const index = part.indexOf('=')
      if (index < 0) {
        continue
      }
      cookies[part.slice(0, index).trim()] = decodeURIComponent(part.slice(index + 1).trim())
    }
    return cookies
  }

  function sessionFromRequest(req) {
    const cookies = parseCookies(req.headers.cookie)
    let token = cookies[COOKIE_NAME]
    if ((typeof token !== 'string' || token.length === 0) && typeof req.headers['x-khaen-token'] === 'string') {
      token = req.headers['x-khaen-token']
    }
    if (typeof token !== 'string' || token.length === 0) {
      return null
    }
    const session = sessions.get(token)
    if (!session) {
      return null
    }
    if (session.expiresAt <= Date.now()) {
      sessions.delete(token)
      return null
    }
    return { token, session }
  }

  function createSession() {
    const token = randomToken()
    const session = { csrf: randomToken(), expiresAt: Date.now() + SESSION_TTL_MS }
    sessions.set(token, session)
    return { token, session }
  }

  function setSessionCookie(res, token) {
    res.setHeader(
      'Set-Cookie',
      COOKIE_NAME + '=' + token + '; Path=/; HttpOnly; SameSite=Strict; Max-Age=' + String(SESSION_TTL_S)
    )
  }

  function clearSessionCookie(res) {
    res.setHeader('Set-Cookie', COOKIE_NAME + '=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0')
  }

  function hasValidCsrf(req, session) {
    const sent = req.headers['x-khaen-csrf']
    return typeof sent === 'string' && sent === session.csrf
  }

  function clientIp(req) {
    return req.socket.remoteAddress || 'unknown'
  }

  function isRateLimited(ip) {
    const now = Date.now()
    const recent = (failuresByIp.get(ip) || []).filter((timestamp) => now - timestamp < RATE_LIMIT_WINDOW_MS)
    failuresByIp.set(ip, recent)
    return recent.length >= RATE_LIMIT_MAX_FAILURES
  }

  function recordFailure(ip) {
    const list = failuresByIp.get(ip) || []
    list.push(Date.now())
    failuresByIp.set(ip, list)
  }

  async function handle(req, res) {
    const url = new URL(req.url || '/', 'http://mock.invalid')
    const path = url.pathname
    const method = req.method || 'GET'

    if (method === 'GET' && path === '/healthz') {
      // 匿名健康检查：version = release 版本（缺失时如实回退 crate 版本；T46 契约）。
      const info = buildSystemInfoFixture(systemFixture)
      sendJson(res, 200, {
        ok: true,
        version: info.release_version ?? info.crate_version,
        uptime_s: info.uptime_s
      })
      return
    }

    // Dev-only helper: 在运行中切换 /api/status fixture（浏览器验收时演练 stale/missing）。
    if (method === 'GET' && path === '/__mock/status') {
      sendJson(res, 200, { fixture: statusFixture, fixtures: STATUS_FIXTURES })
      return
    }
    if (method === 'POST' && path === '/__mock/status-fixture') {
      const body = await readJsonBody(req)
      if (!isStatusFixture(body.fixture)) {
        sendError(res, 400, 'invalid_fixture', 'unknown status fixture: ' + String(body.fixture))
        return
      }
      statusFixture = body.fixture
      sendJson(res, 200, { fixture: statusFixture })
      return
    }

    // Dev-only helper: 在运行中切换校准 fixture（browser QA 演练 empty/success/failed/unavailable）。
    if (method === 'GET' && path === '/__mock/calibration') {
      sendJson(res, 200, {
        fixture: calibrationFixture,
        fixtures: CALIBRATION_FIXTURES,
        jobs: calibrationJobs.size,
        uploads: calibrationUploads.size,
        applied_job_id: appliedJobId,
        unavailable: calibrationUnavailable
      })
      return
    }
    if (method === 'POST' && path === '/__mock/calibration-fixture') {
      const body = await readJsonBody(req)
      if (!isCalibrationFixture(body.fixture)) {
        sendError(res, 400, 'invalid_fixture', 'unknown calibration fixture: ' + String(body.fixture))
        return
      }
      seedCalibration(body.fixture)
      sendJson(res, 200, { fixture: calibrationFixture })
      return
    }

    // Dev-only helper: 在运行中切换系统信息 fixture（browser QA 演练 mismatch/consistent/degraded）。
    if (method === 'GET' && path === '/__mock/system') {
      sendJson(res, 200, {
        fixture: systemFixture,
        fixtures: SYSTEM_FIXTURES,
        restart: systemInfo().restart ?? null,
        restarted_at: systemRestartState.restartedAt
      })
      return
    }
    if (method === 'POST' && path === '/__mock/system-fixture') {
      const body = await readJsonBody(req)
      if (!isSystemFixture(body.fixture)) {
        sendError(res, 400, 'invalid_fixture', 'unknown system fixture: ' + String(body.fixture))
        return
      }
      systemFixture = body.fixture
      systemRestartState = { restartedAt: null, rtHostStarted: false }
      sendJson(res, 200, { fixture: systemFixture })
      return
    }

    // Dev-only helper: 在运行中切换 M3 控制面 fixture（browser QA 演练 host_link_unavailable）。
    if (method === 'GET' && path === '/__mock/control') {
      sendJson(res, 200, {
        fixture: controlFixture,
        fixtures: CONTROL_FIXTURES,
        wired: control.wired,
        muted: control.muted,
        mode: control.mode,
        format: control.format,
        version: control.version,
        gains_db: control.gains_db.slice(),
        pending: controlPending === null ? null : controlPending.kind
      })
      return
    }
    if (method === 'POST' && path === '/__mock/control-fixture') {
      const body = await readJsonBody(req)
      if (!isControlFixture(body.fixture)) {
        sendError(res, 400, 'invalid_fixture', 'unknown control fixture: ' + String(body.fixture))
        return
      }
      seedControl(body.fixture)
      sendJson(res, 200, { fixture: controlFixture })
      return
    }

    // Dev-only helper: M4 方案/声道 fixture（browser QA 演练 degraded/unavailable 与三个失败形态）。
    if (method === 'GET' && path === '/__mock/channels') {
      sendJson(res, 200, {
        fixture: channelsFixture,
        fixtures: [...CHANNELS_FIXTURES, ...CHANNELS_EXTRA_FIXTURES],
        mode: channelsMode,
        unavailable: channelsSeed.unavailable,
        channels: channelsSeed.channels,
        pending: channelsPending === null ? null : channelsPending.kind,
        uploads: planUploads.size,
        current_version: control.version
      })
      return
    }
    if (method === 'POST' && path === '/__mock/channels-fixture') {
      const body = await readJsonBody(req)
      if (resolveChannelsFixture(body.fixture) === null) {
        sendError(res, 400, 'invalid_fixture', 'unknown channels fixture: ' + String(body.fixture))
        return
      }
      seedM4(body.fixture)
      sendJson(res, 200, { fixture: channelsFixture, mode: channelsMode })
      return
    }

    // Dev-only helper: MOTU fixture（wired | degraded）。
    if (method === 'GET' && path === '/__mock/motu') {
      sendJson(res, 200, {
        fixture: motuFixture,
        fixtures: MOTU_FIXTURES,
        detected: motu.detected,
        control_reachable: motu.control_reachable,
        reason: motu.reason,
        draft: motu.draft === null ? null : motu.draft.channels,
        vol: motu.vol
      })
      return
    }
    if (method === 'POST' && path === '/__mock/motu-fixture') {
      const body = await readJsonBody(req)
      if (!isMotuFixture(body.fixture)) {
        sendError(res, 400, 'invalid_fixture', 'unknown motu fixture: ' + String(body.fixture))
        return
      }
      seedMotu(body.fixture)
      sendJson(res, 200, { fixture: motuFixture })
      return
    }

    // Dev-only helper: 音频 profile fixture（wired | not-wired）。
    if (method === 'GET' && path === '/__mock/audio-profile') {
      sendJson(res, 200, {
        fixture: audioProfileFixture,
        fixtures: AUDIO_PROFILE_FIXTURES,
        available: audioProfileSeed.available,
        revision: audioProfileRevision,
        saved_outputs: audioProfileSaved === null ? null : audioProfileSaved.outputs.filter((output) => output.enabled).length,
        applied_plan: audioProfileAppliedPlan,
        generated_plan: audioProfileGeneratedPlan === null ? null : audioProfileGeneratedPlan.sha256,
        pending: audioProfilePending === null ? null : audioProfilePending.token
      })
      return
    }
    if (method === 'POST' && path === '/__mock/audio-profile-fixture') {
      const body = await readJsonBody(req)
      if (!isAudioProfileFixture(body.fixture)) {
        sendError(res, 400, 'invalid_fixture', 'unknown audio-profile fixture: ' + String(body.fixture))
        return
      }
      seedAudioProfile(body.fixture)
      sendJson(res, 200, { fixture: audioProfileFixture })
      return
    }

    // Dev-only helper: 测试音 fixture（enabled | sound-window-closed | not-wired）。
    if (method === 'GET' && path === '/__mock/speaker-test') {
      sendJson(res, 200, {
        fixture: speakerTestFixture,
        fixtures: SPEAKER_TEST_FIXTURES,
        wired: speakerTest.wired,
        window_open: speakerTest.windowOpen,
        state: buildSpeakerTestStatus(speakerTest.state)
      })
      return
    }
    if (method === 'POST' && path === '/__mock/speaker-test-fixture') {
      const body = await readJsonBody(req)
      if (!isSpeakerTestFixture(body.fixture)) {
        sendError(res, 400, 'invalid_fixture', 'unknown speaker-test fixture: ' + String(body.fixture))
        return
      }
      seedSpeakerTest(body.fixture)
      sendJson(res, 200, { fixture: speakerTestFixture })
      return
    }

    if (!path.startsWith('/api/')) {
      sendError(res, 404, 'not_found', 'unknown path: ' + path)
      return
    }

    if (method === 'GET' && path === '/api/auth/status') {
      sendJson(res, 200, { configured, authenticated: sessionFromRequest(req) !== null })
      return
    }

    if (method === 'POST' && path === '/api/auth/setup') {
      if (configured) {
        sendError(res, 409, 'already_configured', 'admin password is already configured')
        return
      }
      const body = await readJsonBody(req)
      if (typeof body.password !== 'string' || body.password.length < MIN_PASSWORD_LENGTH) {
        sendError(res, 400, 'invalid_password', 'password must be at least ' + String(MIN_PASSWORD_LENGTH) + ' characters', {
          min_length: MIN_PASSWORD_LENGTH
        })
        return
      }
      if (typeof body.confirm !== 'string' || body.confirm !== body.password) {
        sendError(res, 400, 'password_mismatch', 'confirm does not match password')
        return
      }
      passwordHash = hashPassword(body.password)
      configured = true
      const created = createSession()
      setSessionCookie(res, created.token)
      sendJson(res, 201, { csrf: created.session.csrf, expires_in: SESSION_TTL_S })
      return
    }

    if (method === 'POST' && path === '/api/auth/login') {
      if (!configured) {
        sendError(res, 409, 'not_configured', 'no admin password configured yet; run setup first')
        return
      }
      const ip = clientIp(req)
      if (isRateLimited(ip)) {
        sendError(res, 429, 'rate_limited', 'too many failed login attempts', { retry_after_s: 60 }, { 'Retry-After': '60' })
        return
      }
      const body = await readJsonBody(req)
      if (typeof body.password !== 'string' || passwordHash !== hashPassword(body.password)) {
        recordFailure(ip)
        sendError(res, 401, 'invalid_credentials', 'wrong password')
        return
      }
      const created = createSession()
      setSessionCookie(res, created.token)
      sendJson(res, 200, { csrf: created.session.csrf, expires_in: SESSION_TTL_S })
      return
    }

    if (method === 'GET' && path === '/api/auth/session') {
      const found = sessionFromRequest(req)
      if (!found) {
        sendError(res, 401, 'not_authenticated', 'no valid session')
        return
      }
      sendJson(res, 200, {
        authenticated: true,
        csrf: found.session.csrf,
        expires_at: new Date(found.session.expiresAt).toISOString()
      })
      return
    }

    if (method === 'POST' && path === '/api/auth/logout') {
      const found = sessionFromRequest(req)
      if (!found) {
        // Idempotent: logging out without a session just clears the cookie.
        clearSessionCookie(res)
        sendJson(res, 200, {})
        return
      }
      if (!hasValidCsrf(req, found.session)) {
        sendError(res, 403, 'csrf_invalid', 'missing or invalid CSRF token')
        return
      }
      sessions.delete(found.token)
      clearSessionCookie(res)
      sendJson(res, 200, {})
      return
    }

    if (method === 'POST' && path === '/api/auth/password') {
      const found = sessionFromRequest(req)
      if (!found) {
        sendError(res, 401, 'not_authenticated', 'no valid session')
        return
      }
      if (!hasValidCsrf(req, found.session)) {
        sendError(res, 403, 'csrf_invalid', 'missing or invalid CSRF token')
        return
      }
      const body = await readJsonBody(req)
      if (typeof body.old !== 'string' || passwordHash !== hashPassword(body.old)) {
        sendError(res, 401, 'invalid_credentials', 'wrong current password')
        return
      }
      if (typeof body.new !== 'string' || body.new.length < MIN_PASSWORD_LENGTH) {
        sendError(res, 400, 'invalid_password', 'new password must be at least ' + String(MIN_PASSWORD_LENGTH) + ' characters', {
          min_length: MIN_PASSWORD_LENGTH
        })
        return
      }
      passwordHash = hashPassword(body.new)
      // Invalidate every existing session, then issue a fresh one.
      sessions.clear()
      const created = createSession()
      setSessionCookie(res, created.token)
      sendJson(res, 200, { csrf: created.session.csrf, expires_in: SESSION_TTL_S })
      return
    }

    // /api/meta、MOTU、音频 profile / 设备输入 / 测试音：全部要求会话；写操作额外要求 CSRF。
    // （/api/meta 是页面能力入口；MOTU 两阶段没有 abort；测试音必须 audible+confirm。）
    if (
      path === '/api/meta' ||
      path === '/api/motu' ||
      path === '/api/motu/volume' ||
      path === '/api/audio-profile' ||
      path === '/api/audio-profile/plan' ||
      path === '/api/device/inputs' ||
      path === '/api/speaker-test' ||
      path === '/api/speaker-test/stop'
    ) {
      const found = sessionFromRequest(req)
      if (!found) {
        sendError(res, 401, 'not_authenticated', 'authentication required')
        return
      }
      if (method !== 'GET' && !hasValidCsrf(req, found.session)) {
        sendError(res, 403, 'csrf_invalid', 'missing or invalid CSRF token')
        return
      }
      if (path === '/api/meta' && method === 'GET') {
        sendJson(res, 200, buildMetaJson())
        return
      }
      if (path === '/api/motu' || path === '/api/motu/volume') {
        await handleMotu(req, res, path, method)
        return
      }
      if (path === '/api/audio-profile' || path === '/api/audio-profile/plan' || path === '/api/device/inputs') {
        await handleAudioProfile(req, res, path, method)
        return
      }
      await handleSpeakerTest(req, res, path, method)
      return
    }

    // M3 控制面（mute / surface volume|mode|format / commit / abort / device retry）：
    // 与真实 daemon 相同：全部要求会话；写操作额外要求 CSRF。wired=false 时 handler 内 503。
    if (
      path === '/api/mute' ||
      path === '/api/surface' ||
      path === '/api/format' ||
      path === '/api/commit' ||
      path === '/api/abort' ||
      path === '/api/device/retry'
    ) {
      const found = sessionFromRequest(req)
      if (!found) {
        sendError(res, 401, 'not_authenticated', 'authentication required')
        return
      }
      if (method !== 'GET' && !hasValidCsrf(req, found.session)) {
        sendError(res, 403, 'csrf_invalid', 'missing or invalid CSRF token')
        return
      }
      await handleControl(req, res, path, method)
      return
    }

    // M4 方案/声道（/api/channels、/api/plan/*、/api/prepare、/api/rollback）：同样要求会话 + CSRF；
    // fixture unavailable 时 handler 内 501 not_wired（T55 未实现）。
    if (
      path === '/api/channels' ||
      path === '/api/plan/upload' ||
      path === '/api/plan/history' ||
      path === '/api/plan/inspect' ||
      path === '/api/prepare' ||
      path === '/api/rollback'
    ) {
      const found = sessionFromRequest(req)
      if (!found) {
        sendError(res, 401, 'not_authenticated', 'authentication required')
        return
      }
      if (method !== 'GET' && !hasValidCsrf(req, found.session)) {
        sendError(res, 403, 'csrf_invalid', 'missing or invalid CSRF token')
        return
      }
      await handleM4(req, res, path, method)
      return
    }

    if (path.startsWith('/api/calibration/')) {
      const found = sessionFromRequest(req)
      if (!found) {
        sendError(res, 401, 'not_authenticated', 'authentication required')
        return
      }
      if (method !== 'GET' && !hasValidCsrf(req, found.session)) {
        sendError(res, 403, 'csrf_invalid', 'missing or invalid CSRF token')
        return
      }
      await handleCalibration(req, res, path, method, url)
      return
    }

    if (path.startsWith('/api/system/')) {
      const found = sessionFromRequest(req)
      if (!found) {
        sendError(res, 401, 'not_authenticated', 'authentication required')
        return
      }
      if (method !== 'GET' && !hasValidCsrf(req, found.session)) {
        sendError(res, 403, 'csrf_invalid', 'missing or invalid CSRF token')
        return
      }
      await handleSystem(req, res, path, method, url)
      return
    }

    if (method === 'GET' && path === '/api/status') {
      if (!sessionFromRequest(req)) {
        sendError(res, 401, 'not_authenticated', 'authentication required')
        return
      }
      const document = buildStatusFixture(statusFixture)
      // mock 控制面是"运行时"的真正状态来源之一：静音立即生效、commit 后的方案/格式要反映真实回读，
      // 否则页面在 mock 下会显示互相矛盾的旧值（真实 daemon 由 host_link 聚合，不存在这个问题）。
      // 注意：只在 present fixture 上叠加（stale/missing fixture 是给页面演练降级形态用的，必须保持原样）；
      // 方案叠加只在控制面真的 commit 过之后（fixture 契约测试断言初始 plan.version=3）。
      if (control.wired && statusFixture === 'present') {
        document.state.muted = control.muted
        document.format.block_frames = control.format.block_size
        if (controlOperationSeq > 0 && control.basePlan !== null) {
          document.plan.version = control.version
          document.plan.graph_id = control.basePlan.graph_id
          if (control.basePlan.sha256 !== undefined) {
            document.plan.sha256 = control.basePlan.sha256
          }
          document.plan.latency_samples = control.basePlan.latency_samples ?? null
          document.plan.applied_at = control.basePlan.applied_at ?? null
          document.plan.source = control.basePlan.source ?? null
        }
      }
      // §12.2 加法字段：测试音运行态（present/stale fixture 恒上报；missing 保持"全字段 null"语义）。
      if (statusFixture !== 'missing') {
        document.speaker_test = buildSpeakerTestStatus(speakerTest.state)
        if (speakerTest.wired !== true) {
          document.reasons = { ...(document.reasons ?? {}), speaker_test: SPEAKER_TEST_NOT_WIRED_REASON }
        }
      }
      sendJson(res, 200, document)
      return
    }

    sendError(res, 405, 'method_not_allowed', 'unsupported method/path: ' + method + ' ' + path)
  }

  const server = createServer((req, res) => {
    handle(req, res).catch((error) => {
      if (res.headersSent) {
        res.end()
        return
      }
      sendError(res, 500, 'internal_error', error instanceof Error ? error.message : 'mock internal error')
    })
  })

  return {
    server,
    /** Start listening; returns {host, port, baseUrl}. */
    listen({ host = '127.0.0.1', port = 0 } = {}) {
      return new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(port, host, () => {
          server.removeListener('error', reject)
          const address = server.address()
          const boundPort = typeof address === 'object' && address !== null ? address.port : port
          resolve({ host, port: boundPort, baseUrl: 'http://' + host + ':' + String(boundPort) })
        })
      })
    },
    close() {
      return new Promise((resolve) => {
        server.close(() => resolve())
        if (typeof server.closeAllConnections === 'function') {
          server.closeAllConnections()
        }
      })
    },
    get configured() {
      return configured
    },
    /** 切换 /api/status fixture（测试用；也便于开发时演练 stale/missing 形态）。 */
    setStatusFixture(next) {
      if (!isStatusFixture(next)) {
        throw new Error('unknown status fixture: ' + String(next) + ' (expected ' + STATUS_FIXTURES.join(' | ') + ')')
      }
      statusFixture = next
    },
    get statusFixture() {
      return statusFixture
    },
    /** 切换校准 fixture（测试/开发用；也会重置校准内存态）。 */
    get calibrationFixture() {
      return calibrationFixture
    },
    setCalibrationFixture(next) {
      if (!isCalibrationFixture(next)) {
        throw new Error(
          'unknown calibration fixture: ' + String(next) + ' (expected ' + CALIBRATION_FIXTURES.join(' | ') + ')'
        )
      }
      seedCalibration(next)
    },
    /** 切换 /api/system fixture（测试/开发用；同时清空 mock 的重启状态）。 */
    get systemFixture() {
      return systemFixture
    },
    setSystemFixture(next) {
      if (!isSystemFixture(next)) {
        throw new Error('unknown system fixture: ' + String(next) + ' (expected ' + SYSTEM_FIXTURES.join(' | ') + ')')
      }
      systemFixture = next
      systemRestartState = { restartedAt: null, rtHostStarted: false }
    },
    /** 切换 M3 控制面 fixture（测试/开发用；会重置控制面内存态）。 */
    get controlFixture() {
      return controlFixture
    },
    setControlFixture(next) {
      if (!isControlFixture(next)) {
        throw new Error(
          'unknown control fixture: ' + String(next) + ' (expected ' + CONTROL_FIXTURES.join(' | ') + ')'
        )
      }
      seedControl(next)
    },
    /** 切换 M4 方案/声道 fixture（测试/开发用；会重置上传/草稿内存态）。 */
    get channelsFixture() {
      return channelsFixture
    },
    setChannelsFixture(next) {
      if (resolveChannelsFixture(next) === null) {
        throw new Error(
          'unknown channels fixture: ' +
            String(next) +
            ' (expected ' +
            CHANNELS_FIXTURES.join(' | ') +
            ' | ' +
            CHANNELS_EXTRA_FIXTURES.join(' | ') +
            ')'
        )
      }
      seedM4(next)
    },
    /** 当前 channels fixture 的运行时模式（normal / mapping-only / reconfigure-failure / host-link-unavailable）。 */
    get channelsMode() {
      return channelsMode
    },
    /** 切换 MOTU fixture（测试/开发用；会重置设备音量/草稿内存态）。 */
    get motuFixture() {
      return motuFixture
    },
    setMotuFixture(next) {
      if (!isMotuFixture(next)) {
        throw new Error('unknown motu fixture: ' + String(next) + ' (expected ' + MOTU_FIXTURES.join(' | ') + ')')
      }
      seedMotu(next)
    },
    /** 切换音频 profile fixture（测试/开发用；会重置已保存 profile/草稿）。 */
    get audioProfileFixture() {
      return audioProfileFixture
    },
    setAudioProfileFixture(next) {
      if (!isAudioProfileFixture(next)) {
        throw new Error(
          'unknown audio-profile fixture: ' + String(next) + ' (expected ' + AUDIO_PROFILE_FIXTURES.join(' | ') + ')'
        )
      }
      seedAudioProfile(next)
    },
    /** 切换测试音 fixture（测试/开发用；会清掉正在跑的测试音）。 */
    get speakerTestFixture() {
      return speakerTestFixture
    },
    setSpeakerTestFixture(next) {
      if (!isSpeakerTestFixture(next)) {
        throw new Error(
          'unknown speaker-test fixture: ' + String(next) + ' (expected ' + SPEAKER_TEST_FIXTURES.join(' | ') + ')'
        )
      }
      seedSpeakerTest(next)
    },
    /** M4 方案/声道 mock 内部状态（测试用；不是对外 API）。 */
    m4: {
      get fixture() {
        return channelsFixture
      },
      get unavailable() {
        return channelsSeed.unavailable
      },
      get channels() {
        return channelsSeed.channels
      },
      get inspect() {
        return channelsSeed.inspect
      },
      get uploads() {
        return [...planUploads.values()].map((upload) => ({ ...upload }))
      },
      get planHistory() {
        return control.history.map((entry) => ({ ...entry }))
      }
    },
    /** MOTU mock 内部状态（测试用；不是对外 API）。 */
    motu: {
      get fixture() {
        return motuFixture
      },
      get detected() {
        return motu.detected
      },
      get vol() {
        return motu.vol === null ? null : JSON.parse(JSON.stringify(motu.vol))
      },
      get draft() {
        return motu.draft === null ? null : { ...motu.draft }
      }
    },
    /** 音频 profile mock 内部状态（测试用；不是对外 API）。 */
    audioProfile: {
      get fixture() {
        return audioProfileFixture
      },
      get saved() {
        return audioProfileSaved === null ? null : JSON.parse(JSON.stringify(audioProfileSaved))
      },
      get revision() {
        return audioProfileRevision
      },
      get appliedPlan() {
        return { ...audioProfileAppliedPlan }
      },
      get pendingToken() {
        return audioProfilePending === null ? null : audioProfilePending.token
      }
    },
    /** 测试音 mock 内部状态（测试用；不是对外 API）。 */
    speakerTest: {
      get fixture() {
        return speakerTestFixture
      },
      get active() {
        return speakerTest.state.active === true
      },
      get state() {
        return buildSpeakerTestStatus(speakerTest.state)
      }
    },
    /** 系统 mock 内部状态（测试用；不是对外 API）。 */
    system: {
      get info() {
        return systemInfo()
      },
      get logs() {
        return systemLogs()
      },
      get restart() {
        return systemInfo().restart ?? null
      },
      get restartedAt() {
        return systemRestartState.restartedAt
      },
      get rtHostStarted() {
        return systemRestartState.rtHostStarted
      },
      diagnostics() {
        return systemDiagnostics()
      }
    },
    /** M3 控制面 mock 内部状态（测试用；不是对外 API）。 */
    control: {
      get fixture() {
        return controlFixture
      },
      get wired() {
        return control.wired
      },
      get muted() {
        return control.muted
      },
      get mode() {
        return control.mode
      },
      get format() {
        return { ...control.format }
      },
      get version() {
        return control.version
      },
      get gainsDb() {
        return control.gains_db.slice()
      },
      get history() {
        return control.history.map((entry) => ({ ...entry }))
      },
      get pending() {
        return controlPending === null ? null : controlPending.kind
      }
    },
    /** 校准 mock 内部状态（测试用；不是对外 API）。 */
    calibration: {
      get fixture() {
        return calibrationFixture
      },
      get jobs() {
        return sortedJobs().map(serializeJob)
      },
      get uploads() {
        return [...calibrationUploads.values()]
      },
      get config() {
        return wizardConfig
      },
      get appliedJobId() {
        return appliedJobId
      },
      tickJob(jobId) {
        const job = calibrationJobs.get(jobId)
        if (job === undefined) {
          throw new Error('unknown job: ' + jobId)
        }
        tickJobRecord(job, mockNow())
        return serializeJob(job)
      },
      completeJob(jobId) {
        const job = calibrationJobs.get(jobId)
        if (job === undefined) {
          throw new Error('unknown job: ' + jobId)
        }
        while (!tickJobRecord(job, mockNow())) {
          // 推进到终态（mock 确定性；真实 daemon 由 worker 推进）。
        }
        return serializeJob(job)
      },
      failJob(jobId, code = 'E_MOCK_FAILURE', message = 'mock failure') {
        const job = calibrationJobs.get(jobId)
        if (job === undefined) {
          throw new Error('unknown job: ' + jobId)
        }
        finalizeJobFailure(job, mockNow(), { code, message })
        return serializeJob(job)
      }
    },
    setPasswordForTest(password) {
      passwordHash = hashPassword(password)
      configured = true
    },
    reset() {
      sessions.clear()
      failuresByIp.clear()
      passwordHash = null
      configured = false
      systemRestartState = { restartedAt: null, rtHostStarted: false }
      seedControl(DEFAULT_CONTROL_FIXTURE)
      seedM4(DEFAULT_CHANNELS_FIXTURE)
      seedMotu('wired')
      seedAudioProfile(DEFAULT_AUDIO_PROFILE_FIXTURE)
      seedSpeakerTest(DEFAULT_SPEAKER_TEST_FIXTURE)
    }
  }
}

const entry = process.argv[1]
const isMain = typeof entry === 'string' && import.meta.url === pathToFileURL(entry).href
if (isMain) {
  const portIndex = process.argv.indexOf('--port')
  const portValue = portIndex >= 0 ? process.argv[portIndex + 1] : process.env.KHAEN_MOCK_PORT
  const port = portValue === undefined ? DEFAULT_MOCK_PORT : Number(portValue)
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    console.error('[mock-auth] invalid port: ' + String(portValue))
    process.exit(2)
  }
  const fixtureIndex = process.argv.indexOf('--status-fixture')
  const fixtureValue =
    fixtureIndex >= 0 ? process.argv[fixtureIndex + 1] : process.env.KHAEN_MOCK_STATUS_FIXTURE
  if (fixtureValue !== undefined && !isStatusFixture(fixtureValue)) {
    console.error('[mock-auth] invalid status fixture: ' + String(fixtureValue) + ' (expected ' + STATUS_FIXTURES.join(' | ') + ')')
    process.exit(2)
  }
  const calibrationIndex = process.argv.indexOf('--calibration-fixture')
  const calibrationValue =
    calibrationIndex >= 0 ? process.argv[calibrationIndex + 1] : process.env.KHAEN_MOCK_CALIBRATION_FIXTURE
  if (calibrationValue !== undefined && !isCalibrationFixture(calibrationValue)) {
    console.error(
      '[mock-auth] invalid calibration fixture: ' +
        String(calibrationValue) +
        ' (expected ' +
        CALIBRATION_FIXTURES.join(' | ') +
        ')'
    )
    process.exit(2)
  }
  const systemIndex = process.argv.indexOf('--system-fixture')
  const systemValue =
    systemIndex >= 0 ? process.argv[systemIndex + 1] : process.env.KHAEN_MOCK_SYSTEM_FIXTURE
  if (systemValue !== undefined && !isSystemFixture(systemValue)) {
    console.error(
      '[mock-auth] invalid system fixture: ' +
        String(systemValue) +
        ' (expected ' +
        SYSTEM_FIXTURES.join(' | ') +
        ')'
    )
    process.exit(2)
  }
  const controlIndex = process.argv.indexOf('--control-fixture')
  const controlValue =
    controlIndex >= 0 ? process.argv[controlIndex + 1] : process.env.KHAEN_MOCK_CONTROL_FIXTURE
  if (controlValue !== undefined && !isControlFixture(controlValue)) {
    console.error(
      '[mock-auth] invalid control fixture: ' +
        String(controlValue) +
        ' (expected ' +
        CONTROL_FIXTURES.join(' | ') +
        ')'
    )
    process.exit(2)
  }
  const channelsIndex = process.argv.indexOf('--channels-fixture')
  const channelsValue =
    channelsIndex >= 0 ? process.argv[channelsIndex + 1] : process.env.KHAEN_MOCK_CHANNELS_FIXTURE
  if (channelsValue !== undefined && !isChannelsFixture(channelsValue) && channelsValue !== 'mapping-only' && channelsValue !== 'reconfigure-failure' && channelsValue !== 'host-link-unavailable') {
    console.error(
      '[mock-auth] invalid channels fixture: ' +
        String(channelsValue) +
        ' (expected ' +
        CHANNELS_FIXTURES.join(' | ') +
        ' | mapping-only | reconfigure-failure | host-link-unavailable)'
    )
    process.exit(2)
  }
  const readFixtureFlag = (flag, envName, validate, fixtures) => {
    const index = process.argv.indexOf(flag)
    const value = index >= 0 ? process.argv[index + 1] : process.env[envName]
    if (value !== undefined && !validate(value)) {
      console.error('[mock-auth] invalid ' + flag + ': ' + String(value) + ' (expected ' + fixtures.join(' | ') + ')')
      process.exit(2)
    }
    return value
  }
  const motuValue = readFixtureFlag('--motu-fixture', 'KHAEN_MOCK_MOTU_FIXTURE', isMotuFixture, MOTU_FIXTURES)
  const audioProfileValue = readFixtureFlag(
    '--audio-profile-fixture',
    'KHAEN_MOCK_AUDIO_PROFILE_FIXTURE',
    isAudioProfileFixture,
    AUDIO_PROFILE_FIXTURES
  )
  const speakerTestValue = readFixtureFlag(
    '--speaker-test-fixture',
    'KHAEN_MOCK_SPEAKER_TEST_FIXTURE',
    isSpeakerTestFixture,
    SPEAKER_TEST_FIXTURES
  )
  const mock = createMockAuthServer({
    statusFixture: fixtureValue,
    calibrationFixture: calibrationValue,
    systemFixture: systemValue,
    controlFixture: controlValue,
    channelsFixture: channelsValue,
    motuFixture: motuValue,
    audioProfileFixture: audioProfileValue,
    speakerTestFixture: speakerTestValue
  })
  const address = await mock.listen({ port })
  console.log('[mock-auth] contract mock listening on ' + address.baseUrl)
  console.log('[mock-auth] /api/status fixture: ' + mock.statusFixture + ' (' + STATUS_FIXTURES.join(' | ') + ')')
  console.log(
    '[mock-auth] /api/calibration fixture: ' +
      mock.calibrationFixture +
      ' (' +
      CALIBRATION_FIXTURES.join(' | ') +
      ')'
  )
  console.log(
    '[mock-auth] /api/system fixture: ' + mock.systemFixture + ' (' + SYSTEM_FIXTURES.join(' | ') + ')'
  )
  console.log(
    '[mock-auth] /api control fixture: ' + mock.controlFixture + ' (' + CONTROL_FIXTURES.join(' | ') + ')'
  )
  console.log(
    '[mock-auth] /api/channels fixture: ' +
      mock.channelsFixture +
      ' (' +
      CHANNELS_FIXTURES.join(' | ') +
      ' | mapping-only | reconfigure-failure | host-link-unavailable)'
  )
  console.log(
    '[mock-auth] /api/motu fixture: ' + mock.motuFixture + ' (' + MOTU_FIXTURES.join(' | ') + ')'
  )
  console.log(
    '[mock-auth] /api/audio-profile fixture: ' +
      mock.audioProfileFixture +
      ' (' +
      AUDIO_PROFILE_FIXTURES.join(' | ') +
      ')'
  )
  console.log(
    '[mock-auth] /api/speaker-test fixture: ' +
      mock.speakerTestFixture +
      ' (' +
      SPEAKER_TEST_FIXTURES.join(' | ') +
      ')'
  )
  console.log('[mock-auth] dev: KHAEN_DEV_API=' + address.baseUrl + ' pnpm dev')
  console.log('[mock-auth] in-memory state; restart resets the setup flow')
}
