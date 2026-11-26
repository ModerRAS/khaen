/**
 * 通道识别测试音（speaker test）契约 fixture（DEV / TEST ONLY；scripts/ 不进 dist）。
 *
 * 契约来源：docs/API.md v1.2 §12.2/§12.6（测试音：带限粉噪 500 Hz–4 kHz、默认 -30 dBFS、
 * 硬上限 10 s / -12 dBFS、必须 audible+confirm、KHAEN_SPEAKER_TEST=on 才启用、窗口外 403）。
 *
 * 覆盖：
 * - POST /api/speaker-test       校验 + 生效（回执 ok/dac/ms/kind/level_db/remaining_ms）
 * - POST /api/speaker-test/stop  幂等停止（{ok, stopped}）
 * - GET  /api/status 加法字段 speaker_test:{active, dac, remaining_ms, kind}
 *
 * fixture（确定性；不依赖当前时间/随机）：
 * - enabled：测试音可用、出声窗口开放；
 * - sound-window-closed：配置了 KHAEN_SOUND_WINDOW 且当前在窗口外 → 403 sound_window_closed；
 * - not-wired：后端/rt-host 未接线 → 503 not_wired。
 *
 * 纪律：mock 不出声（不存在声卡/设备）；回执只是"服务端已接受请求"的事实。
 * 页面（SpeakerLayoutView）必须如实显示这些状态，失败不得显示成已发声。
 */

export const SPEAKER_TEST_FIXTURES = ['enabled', 'sound-window-closed', 'not-wired']
export const DEFAULT_SPEAKER_TEST_FIXTURE = 'enabled'

export function isSpeakerTestFixture(value) {
  return typeof value === 'string' && SPEAKER_TEST_FIXTURES.includes(value)
}

/** 服务端硬边界（docs/API.md §12.2：dac 0..21、ms 500..10000、db -60..-12，默认 -30）。 */
export const SPEAKER_TEST_DAC_MIN = 0
export const SPEAKER_TEST_DAC_MAX = 21
export const SPEAKER_TEST_MS_MIN = 500
export const SPEAKER_TEST_MS_MAX = 10000
export const SPEAKER_TEST_DB_MIN = -60
export const SPEAKER_TEST_DB_MAX = -12
export const SPEAKER_TEST_DEFAULT_DB = -30
export const SPEAKER_TEST_DEFAULT_KIND = 'noise'
export const SPEAKER_TEST_KINDS = ['noise', 'sine']

/** 固定时间戳（mock 可复现）。 */
export const SPEAKER_TEST_FIXED_AT = '2026-11-26T07:50:00.000Z'

/** 稳定原因文案（403/503 与 GET /api/status.not_wired/reasons 共用）。 */
export const SPEAKER_TEST_WINDOW_CLOSED_REASON =
  'the speaker test sound window is closed (KHAEN_SOUND_WINDOW is configured and the current time is outside it): no test tone is played'
export const SPEAKER_TEST_NOT_WIRED_REASON =
  'the speaker test output is not wired on this daemon: no device/rt-host test-tone path is available (no sound is played)'

/** 构造 fixture 种子：{fixture, configured, windowOpen, wired, state}（state 会被 mock 修改）。 */
export function buildSpeakerTestSeed(fixture) {
  const name = isSpeakerTestFixture(fixture) ? fixture : DEFAULT_SPEAKER_TEST_FIXTURE
  const base = {
    fixture: name,
    /** KHAEN_SPEAKER_TEST=on（未启用时测试音不可用）。 */
    configured: true,
    windowOpen: true,
    wired: true,
    reason: null,
    /** 当前测试音运行态（null = 没有在跑；GET /api/status 的 speaker_test 加法字段由此派生）。 */
    state: { active: false, dac: null, ms: null, kind: null, level_db: null, remaining_ms: 0, started_at: null }
  }
  if (name === 'sound-window-closed') {
    return { ...base, windowOpen: false, reason: SPEAKER_TEST_WINDOW_CLOSED_REASON }
  }
  if (name === 'not-wired') {
    return { ...base, wired: false, reason: SPEAKER_TEST_NOT_WIRED_REASON }
  }
  return base
}

/** GET /api/status 的加法字段 speaker_test（inactive 也上报，便于页面区分"没有测试音"与"未上报"）。 */
export function buildSpeakerTestStatus(state) {
  const active = state !== null && state !== undefined && state.active === true
  return {
    active,
    dac: active ? state.dac ?? null : null,
    remaining_ms: active ? state.remaining_ms ?? 0 : 0,
    kind: active ? state.kind ?? null : null
  }
}

/** 测试音请求校验（返回 {ok:true, request} 或 {ok:false, status, code, message, details}）。 */
export function validateSpeakerTestRequest(body, limits = {}) {
  const report = (status, code, message, details) => ({ ok: false, status, code, message, details })
  if (typeof body !== 'object' || body === null || Array.isArray(body)) {
    return report(400, 'bad_request', 'request body must be a JSON object', { field: '<body>' })
  }
  const missing = []
  if (body.audible !== true) {
    missing.push('audible')
  }
  if (body.confirm !== true) {
    missing.push('confirm')
  }
  if (missing.length > 0) {
    return report(
      400,
      'bad_request',
      'explicit confirmation is required to play a test tone: send audible=true and confirm=true',
      { fields: missing, reason: 'test tone requires audible=true and confirm=true (docs/API.md §12.2)' }
    )
  }
  const dac = body.dac
  const dacMax = limits.dacMax ?? SPEAKER_TEST_DAC_MAX
  if (!Number.isInteger(dac) || dac < SPEAKER_TEST_DAC_MIN || dac > dacMax) {
    return report(
      400,
      'bad_request',
      'field dac must be an integer in ' + String(SPEAKER_TEST_DAC_MIN) + '..' + String(dacMax),
      { field: 'dac', reason: 'dac out of range' }
    )
  }
  const ms = body.ms
  if (typeof ms !== 'number' || !Number.isFinite(ms) || ms < SPEAKER_TEST_MS_MIN || ms > SPEAKER_TEST_MS_MAX) {
    return report(
      400,
      'bad_request',
      'field ms must be a number in ' + String(SPEAKER_TEST_MS_MIN) + '..' + String(SPEAKER_TEST_MS_MAX) + ' milliseconds',
      { field: 'ms', reason: 'duration out of range' }
    )
  }
  const kind = body.kind === undefined || body.kind === null ? SPEAKER_TEST_DEFAULT_KIND : body.kind
  if (typeof kind !== 'string' || !SPEAKER_TEST_KINDS.includes(kind)) {
    return report(400, 'bad_request', 'field kind must be "noise" or "sine"', { field: 'kind' })
  }
  const db = body.db === undefined || body.db === null ? SPEAKER_TEST_DEFAULT_DB : body.db
  if (typeof db !== 'number' || !Number.isFinite(db) || db < SPEAKER_TEST_DB_MIN || db > SPEAKER_TEST_DB_MAX) {
    return report(
      400,
      'bad_request',
      'field db must be a number in ' + String(SPEAKER_TEST_DB_MIN) + '..' + String(SPEAKER_TEST_DB_MAX) + ' dBFS (default ' +
        String(SPEAKER_TEST_DEFAULT_DB) + ')',
      { field: 'db', reason: 'level out of range' }
    )
  }
  return { ok: true, request: { dac, ms, kind, db } }
}

/** 开始回执（remaining_ms 是服务端回执的原值；mock 不做实时倒计时）。 */
export function buildSpeakerTestAck(request) {
  return {
    ok: true,
    dac: request.dac,
    ms: request.ms,
    kind: request.kind,
    level_db: request.db,
    remaining_ms: request.ms
  }
}

/** 停止回执（幂等：没有在跑的测试音时 stopped:false，不是错误）。 */
export function buildSpeakerTestStopAck(stopped) {
  return { ok: true, stopped: stopped === true }
}
