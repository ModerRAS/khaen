#!/usr/bin/env node
/**
 * Contract mock for /api/auth/* — DEV / TEST ONLY (scripts/ is never bundled into dist).
 *
 * It implements exactly the PLAN §4.1/§4.2 frontend contract:
 *   GET  /api/auth/status   -> {configured, authenticated}            (anonymous)
 *   POST /api/auth/setup    -> 201 {csrf, expires_in} + Set-Cookie   (409 when configured)
 *   POST /api/auth/login    -> 200 {csrf, expires_in} + Set-Cookie   (401 wrong, 429 rate limited)
 *   GET  /api/auth/session  -> {authenticated, csrf, expires_at}     (401 when anonymous)
 *   POST /api/auth/logout   -> clears session (idempotent)
 *   POST /api/auth/password -> {old, new}, requires session + CSRF
 *   GET  /api/status        -> 401 without session; khaen-status/v1 fixture with session
 *                              (fixtures: present | stale | missing, see scripts/status-fixtures.mjs)
 *   /api/calibration/*      -> 校准 REST（jobs/uploads/artifacts/wizard；fixtures: empty | success | failed |
 *                              unavailable，见 scripts/calibration-fixtures.mjs）；写操作要求 CSRF
 *   /api/system/*           -> 系统信息 REST（info/logs/diagnostics/password/restart；fixtures: mismatch |
 *                              consistent | degraded，见 scripts/system-fixtures.mjs）；写操作要求 CSRF
 *   M3 控制面                -> /api/mute、/api/surface（volume|mode）、/api/format、/api/commit、/api/abort、
 *                              /api/device/retry（prepare → commit/abort 单草稿槽；fixtures: wired | unavailable，
 *                              见 scripts/control-fixtures.mjs）；写操作要求 CSRF
 *   M4 方案与声道            -> /api/channels（GET/PUT）、/api/plan/upload|history|inspect、/api/prepare、
 *                              /api/rollback；channels commit 在 M3b/T52 完成前返回 501 host_link_unsupported
 *                              （fixtures: wired | degraded | unavailable，见 scripts/channels-fixtures.mjs）
 *   GET  /healthz           -> {ok, version, uptime_s}（匿名；version = release 版本，缺失回退 crate 版本）
 *   GET  /__mock/status     -> {fixture, fixtures}                     (dev-only switch helper)
 *   POST /__mock/status-fixture {fixture} -> switch /api/status fixture (dev-only)
 *   GET  /__mock/system     -> {fixture, fixtures, restart}            (dev-only switch helper)
 *   POST /__mock/system-fixture {fixture} -> switch /api/system fixture (dev-only)
 *   GET  /__mock/control    -> {fixture, fixtures, state, drafts}      (dev-only switch helper)
 *   POST /__mock/control-fixture {fixture} -> switch M3 control fixture (dev-only; clears drafts)
 *   GET  /__mock/channels   -> {fixture, fixtures, state, history}     (dev-only switch helper)
 *   POST /__mock/channels-fixture {fixture} -> switch M4 channels fixture (dev-only)
 *
 * Cookie: khaen_session=<opaque>; HttpOnly; SameSite=Strict; Path=/; Max-Age=28800
 * CSRF: X-KHAEN-CSRF must equal the session csrf for session-bound writes.
 * State is in-memory only (restart = fresh setup flow). It never replaces the real backend:
 * the real one is Rust/axum (task T34) and must be validated separately before M1 ships.
 *
 * Usage:
 *   node scripts/mock-auth-server.mjs                          # 127.0.0.1:8790
 *   node scripts/mock-auth-server.mjs --port 8730
 *   node scripts/mock-auth-server.mjs --status-fixture stale   # present | stale | missing
 *   node scripts/mock-auth-server.mjs --calibration-fixture success # empty | success | failed | unavailable
 *   node scripts/mock-auth-server.mjs --system-fixture degraded    # mismatch | consistent | degraded
 *   node scripts/mock-auth-server.mjs --control-fixture wired --channels-fixture wired
 *                              # control: wired | unavailable; channels: wired | degraded | unavailable
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
  buildChannelsSeed,
  isChannelsFixture
} from './channels-fixtures.mjs'

export const DEFAULT_MOCK_PORT = 8790

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

  /* --------------------------------------------------- M4 方案/声道 mock 状态 */
  // DEV/TEST ONLY：/api/channels（读 + prepare）、/api/plan/upload|prepare|history|inspect、/api/rollback。
  // fixture unavailable = daemon 尚未实现 M4（T55）：所有 M4 端点 501 not_wired（不编造映射/历史）。
  let channelsFixture = isChannelsFixture(options.channelsFixture) ? options.channelsFixture : DEFAULT_CHANNELS_FIXTURE
  let channelsSeed = buildChannelsSeed(channelsFixture)
  /** sha256(plan bytes) -> 上传记录（幂等）。 */
  let planUploads = new Map()

  function seedM4(fixture) {
    channelsFixture = fixture
    channelsSeed = buildChannelsSeed(fixture)
    planUploads = new Map()
    controlPending = null
  }
  seedM4(channelsFixture)

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

  /** 消费 pending 草稿并返回 commit 回执（volume / mode / format / plan / rollback / channels）。 */
  function commitControlDraft() {
    const pending = controlPending
    if (pending === null) {
      return { status: 409, code: 'engine_token_invalid', message: 'no prepared draft: call prepare before commit' }
    }
    if (pending.kind === 'channels') {
      // M3b / T52 未完成：commit 明确 501（保留草稿，可 abort），绝不假装成功。
      return {
        status: 501,
        code: 'host_link_unsupported',
        message:
          'rt-host does not support channel hot reconfiguration yet (M3b/T52): commit refused',
        details: { reason: 'channel hot reconfiguration (M3b/T52) is not implemented in rt-host' }
      }
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
      if (pending.changed) {
        control.format = { ...pending.target }
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
          history_cleared: false,
          mute_preserved: control.muted,
          device_restart_required: true
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

  /** 从上传字节派生稳定的 graph_id / 段数 / 延迟（mock 用；真实 daemon 解码 ABI v3）。 */
  function planFacts(sha, bytes) {
    return {
      graph_id: sha,
      segments: 1 + (bytes.length % 5),
      latency_samples: 1023 + (Number.parseInt(sha.slice(0, 2), 16) % 17)
    }
  }

  function planHistoryJson() {
    return {
      schema: 'khaen-plan-history/v1',
      current_version: control.version,
      versions: control.history.map((entry) => ({
        version: entry.version,
        graph_id: entry.graph_id,
        sha256: entry.sha256 ?? null,
        applied_at: entry.applied_at ?? null,
        source: entry.source ?? null
      })),
      reasons: {}
    }
  }

  function prepareChannelsBody(body) {
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
    const output = body.output
    const input = body.input
    if (output !== undefined) {
      if (typeof output !== 'object' || output === null || Array.isArray(output)) {
        return { status: 400, code: 'bad_request', message: 'field output must be an object' }
      }
      if (output.channels !== undefined) {
        if (!Number.isInteger(output.channels) || output.channels < 1 || output.channels > 32) {
          return { status: 400, code: 'bad_request', message: 'output.channels must be an integer between 1 and 32' }
        }
      }
      if (output.dac_map !== undefined) {
        if (!Array.isArray(output.dac_map)) {
          return { status: 400, code: 'bad_request', message: 'output.dac_map must be an array' }
        }
        const expected = output.channels ?? current.output.channels
        if (output.dac_map.length !== expected) {
          return { status: 400, code: 'bad_request', message: 'output.dac_map length must equal the channel count' }
        }
        const seen = new Set()
        for (const dac of output.dac_map) {
          if (!Number.isInteger(dac) || dac < 0 || dac > 31 || seen.has(dac)) {
            return { status: 400, code: 'bad_request', message: 'output.dac_map values must be unique integers 0..31' }
          }
          seen.add(dac)
        }
      }
    }
    if (input !== undefined) {
      if (typeof input !== 'object' || input === null || Array.isArray(input)) {
        return { status: 400, code: 'bad_request', message: 'field input must be an object' }
      }
      if (input.channels !== undefined) {
        if (!Number.isInteger(input.channels) || input.channels < 1 || input.channels > 8) {
          return { status: 400, code: 'bad_request', message: 'input.channels must be an integer between 1 and 8' }
        }
      }
      if (input.map !== undefined && input.map !== null) {
        if (!Array.isArray(input.map)) {
          return { status: 400, code: 'bad_request', message: 'input.map must be an array or null' }
        }
        const expected = input.channels ?? current.input.channels
        if (input.map.length !== expected) {
          return { status: 400, code: 'bad_request', message: 'input.map length must equal the input channel count' }
        }
        const seen = new Set()
        for (const channel of input.map) {
          if (!Number.isInteger(channel) || channel < 0 || channel > 63 || seen.has(channel)) {
            return { status: 400, code: 'bad_request', message: 'input.map values must be unique integers 0..63' }
          }
          seen.add(channel)
        }
      }
    }
    const newChannels = output?.channels ?? current.output.channels
    const newInput = input?.channels ?? current.input.channels
    const changed =
      newChannels !== current.output.channels || newInput !== current.input.channels
    const version = control.version + 1
    const token = stableControlToken('channels', version, { newChannels, newInput })
    const response = {
      ok: true,
      engine_token: token,
      planned_version: version,
      changed,
      preview: {
        old: { channels: current.output.channels, input_channels: current.input.channels },
        new: { channels: newChannels, input_channels: newInput },
        audio_gap_ms_estimate: 1200
      },
      warnings: ['audio output will be interrupted while the device is closed and reopened (M3b hot reconfiguration)'],
      blockers: []
    }
    if (controlPending !== null && controlPending.token === token) {
      return { response: controlPending.response }
    }
    controlPending = { kind: 'channels', token, version, response }
    return { response }
  }

  async function handleM4(req, res, path, method) {
    if (channelsSeed.unavailable) {
      m4Unavailable(res)
      return
    }
    if (method === 'GET' && path === '/api/channels') {
      sendJson(res, 200, channelsSeed.channels)
      return
    }
    if (method === 'PUT' && path === '/api/channels') {
      const body = await readJsonBody(req)
      const prepared = prepareChannelsBody(body)
      if (prepared.status !== undefined) {
        sendError(res, prepared.status, prepared.code, prepared.message)
        return
      }
      sendJson(res, 200, prepared.response)
      return
    }
    if (method === 'GET' && path === '/api/plan/history') {
      sendJson(res, 200, planHistoryJson())
      return
    }
    if (method === 'GET' && path === '/api/plan/inspect') {
      sendJson(res, 200, channelsSeed.inspect)
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
      let upload = planUploads.get(sha)
      if (upload === undefined) {
        const facts = planFacts(sha, bytes)
        upload = {
          upload_id: sha,
          sha256: sha,
          graph_id: facts.graph_id,
          version: control.version + 1,
          segments: facts.segments,
          latency_samples: facts.latency_samples,
          filename: typeof req.headers['x-khaen-filename'] === 'string' ? req.headers['x-khaen-filename'] : null,
          bytes: bytes.length
        }
        planUploads.set(sha, upload)
      }
      sendJson(res, 200, {
        upload_id: upload.upload_id,
        sha256: upload.sha256,
        graph_id: upload.graph_id,
        version: upload.version,
        segments: upload.segments,
        latency_samples: upload.latency_samples
      })
      return
    }
    if (method === 'POST' && path === '/api/prepare') {
      const body = await readJsonBody(req)
      if (typeof body.upload_id !== 'string' || body.upload_id.length === 0) {
        sendError(res, 400, 'bad_request', 'field upload_id must be a non-empty string')
        return
      }
      const sha = typeof body.sha256 === 'string' ? body.sha256 : typeof body.plan_sha256 === 'string' ? body.plan_sha256 : null
      if (sha === null || sha.length === 0) {
        sendError(res, 400, 'bad_request', 'field sha256 must be a non-empty string')
        return
      }
      const upload = planUploads.get(body.upload_id)
      if (upload === undefined) {
        sendError(res, 404, 'upload_not_found', 'uploaded plan does not exist: upload it again')
        return
      }
      if (upload.sha256 !== sha) {
        sendError(res, 409, 'sha_mismatch', 'plan sha256 mismatch: request does not match the uploaded bytes')
        return
      }
      const version = control.version + 1
      const token = stableControlToken('plan', version, sha)
      const response = {
        ok: true,
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
        upload_id: upload.upload_id,
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
      const prepared =
        body.surface === 'volume'
          ? prepareVolumeSurface(body)
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
        sendJson(res, 202, { job_id: id, status: 'queued' })
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
        sendJson(res, 200, {
          schema: 'khaen-calibration-job/v1',
          job: serializeJob(job),
          reasons: job.result === null ? { result: 'job has no result yet' } : {}
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
        sendJson(res, 200, { job_id: job.id, status: job.status })
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
        sendJson(res, 200, applyJobRecord(job))
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
        const snapshot = appliedSnapshot
        job.apply_state = 'rolled_back'
        job.applied_version = null
        appliedJobId = null
        appliedSnapshot = null
        sendJson(res, 200, {
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
        sendBinary(res, 200, content.content, content.content_type, {
          'X-KHAEN-Artifact-SHA256': artifact.sha256 ?? ''
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
          sendError(res, 409, 'job_not_succeeded', 'no calibration job to apply')
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

    // Dev-only helper: M4 方案/声道 fixture（browser QA 演练 degraded/unavailable）。
    if (method === 'GET' && path === '/__mock/channels') {
      sendJson(res, 200, {
        fixture: channelsFixture,
        fixtures: CHANNELS_FIXTURES,
        unavailable: channelsSeed.unavailable,
        channels: channelsSeed.channels,
        uploads: planUploads.size,
        current_version: control.version
      })
      return
    }
    if (method === 'POST' && path === '/__mock/channels-fixture') {
      const body = await readJsonBody(req)
      if (!isChannelsFixture(body.fixture)) {
        sendError(res, 400, 'invalid_fixture', 'unknown channels fixture: ' + String(body.fixture))
        return
      }
      seedM4(body.fixture)
      sendJson(res, 200, { fixture: channelsFixture })
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
      if (!isChannelsFixture(next)) {
        throw new Error(
          'unknown channels fixture: ' + String(next) + ' (expected ' + CHANNELS_FIXTURES.join(' | ') + ')'
        )
      }
      seedM4(next)
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
  if (channelsValue !== undefined && !isChannelsFixture(channelsValue)) {
    console.error(
      '[mock-auth] invalid channels fixture: ' +
        String(channelsValue) +
        ' (expected ' +
        CHANNELS_FIXTURES.join(' | ') +
        ')'
    )
    process.exit(2)
  }
  const mock = createMockAuthServer({
    statusFixture: fixtureValue,
    calibrationFixture: calibrationValue,
    systemFixture: systemValue,
    controlFixture: controlValue,
    channelsFixture: channelsValue
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
    '[mock-auth] /api/channels fixture: ' + mock.channelsFixture + ' (' + CHANNELS_FIXTURES.join(' | ') + ')'
  )
  console.log('[mock-auth] dev: KHAEN_DEV_API=' + address.baseUrl + ' pnpm dev')
  console.log('[mock-auth] in-memory state; restart resets the setup flow')
}
