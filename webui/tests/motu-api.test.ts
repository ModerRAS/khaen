import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, ApiError } from '../src/api/client.ts'
import {
  MOTU_DETECTION_REASON_KEYS,
  MOTU_MAIN_CHANNELS,
  MOTU_STATUS_PATH,
  MOTU_STATUS_SCHEMA,
  MOTU_VOLUME_DB_MAX,
  MOTU_VOLUME_DB_MIN,
  MOTU_VOLUME_PATH,
  MOTU_VOLUME_SCHEMA,
  buildMotuVolumeCommitBody,
  buildMotuVolumePrepareBody,
  classifyMotuError,
  createMotuApi,
  motuDetectionReasonKey,
  motuIsWritable,
  motuReasonEntries,
  motuReasonFor,
  motuVolumeFor,
  parseMotuStatus,
  parseMotuVolumeCommitAck,
  parseMotuVolumeDraft,
  validateMotuChannelSelection,
  validateMotuVolumeDb
} from '../src/api/motu.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

const DATASTORE_0 = { key: 'koBusFader', index: 0 }
const DATASTORE_1 = { key: 'koBusFader', index: 1 }

const STATUS_WIRED = {
  schema: MOTU_STATUS_SCHEMA,
  detected: true,
  model: 'UltraLite-mk5',
  card_id: 'UltraLitemk5',
  card_index: 0,
  control_reachable: true,
  reason: 'detected',
  reason_detail: 'ready',
  channel_count: 2,
  channels: [
    { channel: 1, name: 'Main 1', datastore: DATASTORE_0 },
    { channel: 2, name: 'Main 2', datastore: DATASTORE_1 }
  ],
  mute: false,
  vol: [
    { channel: 1, name: 'Main 1', datastore: DATASTORE_0, linear: 1.0, db: 0 },
    { channel: 2, name: 'Main 2', datastore: DATASTORE_1, linear: 0.5, db: -6.020599913279624 }
  ],
  state: {
    sample_rate_hz: 48000,
    main_trim: 31,
    main_bus_mute: false,
    device: { model_id: 'UL5L', name: 'UltraLite-mk5', version: '1.0.0', api_version: 10 },
    missing: []
  },
  read_error: null,
  write: {
    status: 'wired',
    single_key: true,
    readback: true,
    two_phase: true,
    batch: false,
    max_channels_per_request: 2,
    reason: null
  },
  not_wired: [],
  reasons: {}
}

const TARGET_DEVICE = {
  detected: true,
  model: 'UltraLite-mk5',
  card_id: 'UltraLitemk5',
  card_index: 0,
  control_reachable: true,
  reason: 'detected',
  reason_detail: 'ready'
}

const PREPARE_ACK = {
  ok: true,
  schema: MOTU_VOLUME_SCHEMA,
  stage: 'prepare',
  engine_token: '0011223344556677',
  token: '0011223344556677',
  created_at: '2026-11-26T12:00:00.000Z',
  expires_in_s: 120,
  target: {
    channels: [
      { channel: 1, name: 'Main 1', datastore: DATASTORE_0 },
      { channel: 2, name: 'Main 2', datastore: DATASTORE_1 }
    ],
    linear: 0.5,
    db: -6.020599913279624
  },
  write_plan: {
    kind: 'single-key+readback',
    batch: false,
    writes_per_channel: 1,
    writes: 2,
    steps: [
      { channel: 1, key: 'koBusFader', index: 0, frames: 1 },
      { channel: 2, key: 'koBusFader', index: 1, frames: 1 }
    ],
    verified_by: 'device datastore readback'
  },
  commit_supported: true,
  two_phase: true,
  writes_device: false,
  applies_to: 'MOTU CueMix datastore',
  target_device: TARGET_DEVICE
}

const COMMIT_ACK = {
  ok: true,
  schema: MOTU_VOLUME_SCHEMA,
  stage: 'commit',
  engine_token: '0011223344556677',
  applied: true,
  idempotent: false,
  created_at: '2026-11-26T12:00:00.000Z',
  linear: 0.5,
  db: -6.020599913279624,
  channels: [
    {
      channel: 1,
      name: 'Main 1',
      key: 'koBusFader',
      index: 0,
      before: 1.0,
      wrote: 0.5,
      readback: 0.5,
      changed: true,
      frame_sent: true,
      verified: true
    },
    {
      channel: 2,
      name: 'Main 2',
      key: 'koBusFader',
      index: 1,
      before: 0.5,
      wrote: 0.5,
      readback: 0.5,
      changed: false,
      frame_sent: false,
      verified: true
    }
  ],
  write_plan: PREPARE_ACK.write_plan,
  writes_device: true,
  applies_to: 'MOTU CueMix datastore',
  target_device: TARGET_DEVICE
}

async function captureError(run: () => Promise<unknown>): Promise<ApiError> {
  try {
    await run()
  } catch (error) {
    assert.ok(error instanceof ApiError, 'expected an ApiError, got: ' + String(error))
    return error
  }
  return assert.fail('expected the call to reject') as never
}

function createClient(handler: (call: any, index: number) => Response, csrf = 'csrf-test') {
  const recording = createRecordingFetch(handler)
  const client = new ApiClient({
    baseUrl: 'http://mock.local',
    fetchImpl: recording.fetch,
    csrfToken: () => csrf
  })
  return { client, calls: recording.calls, api: createMotuApi(client) }
}

test('parseMotuStatus keeps the wired device snapshot verbatim (no invented values)', () => {
  const status = parseMotuStatus(STATUS_WIRED)
  assert.equal(status.schema, MOTU_STATUS_SCHEMA)
  assert.equal(status.detected, true)
  assert.equal(status.model, 'UltraLite-mk5')
  assert.equal(status.card_id, 'UltraLitemk5')
  assert.equal(status.card_index, 0)
  assert.equal(status.control_reachable, true)
  assert.equal(status.reason, 'detected')
  assert.equal(status.reason_detail, 'ready')
  assert.equal(status.channel_count, 2)
  assert.deepEqual(
    status.channels.map((channel) => [channel.channel, channel.name, channel.datastore?.index]),
    [
      [1, 'Main 1', 0],
      [2, 'Main 2', 1]
    ]
  )
  assert.equal(status.mute, false)
  // vol 是设备真值回读（linear + 等效 dB），不是控制面回执。
  assert.equal(status.vol?.[0]?.linear, 1)
  assert.equal(status.vol?.[0]?.db, 0)
  assert.equal(status.vol?.[1]?.linear, 0.5)
  assert.ok(Math.abs((status.vol?.[1]?.db ?? 0) - -6.020599913279624) < 1e-9)
  assert.equal(status.state?.sample_rate_hz, 48000)
  assert.equal(status.state?.main_trim, 31)
  assert.equal(status.state?.main_bus_mute, false)
  assert.equal(status.state?.device?.model_id, 'UL5L')
  assert.equal(status.state?.device?.api_version, 10)
  assert.deepEqual(status.state?.missing, [])
  assert.equal(status.read_error, null)
  assert.equal(status.write?.status, 'wired')
  assert.equal(status.write?.batch, false)
  assert.equal(status.write?.max_channels_per_request, 2)
  assert.deepEqual(status.not_wired, [])
  assert.deepEqual(status.reasons, {})
  assert.equal(status.cache, null)
})

test('parseMotuStatus keeps cache age stale and last_error without clearing device fields', () => {
  const status = parseMotuStatus({
    ...STATUS_WIRED,
    cache: { age_ms: 1500, stale: true, last_error: 'timed out' }
  })
  assert.equal(status.mute, false)
  assert.equal(status.cache?.age_ms, 1500)
  assert.equal(status.cache?.stale, true)
  assert.equal(status.cache?.last_error, 'timed out')

  assert.equal(
    parseMotuStatus({ ...STATUS_WIRED, cache: { age_ms: 0, stale: false, last_error: null } }).cache?.last_error,
    null
  )
  assert.throws(() => parseMotuStatus({ ...STATUS_WIRED, cache: 'fresh' }), /cache must be an object/)
})

test('parseMotuStatus degrades honestly: null fields + reasons + read_error + not_detected write', () => {
  const status = parseMotuStatus({
    schema: MOTU_STATUS_SCHEMA,
    detected: false,
    model: null,
    card_id: null,
    card_index: null,
    control_reachable: false,
    reason: 'no_motu_card',
    reason_detail: 'no MOTU card was found',
    channel_count: 2,
    channels: STATUS_WIRED.channels,
    mute: null,
    vol: null,
    state: null,
    read_error: { code: 'E_TIMEOUT', message: 'readback timed out', retryable: true },
    write: {
      status: 'not_detected',
      single_key: true,
      readback: true,
      two_phase: true,
      batch: false,
      max_channels_per_request: 2,
      reason: 'no MOTU card was found'
    },
    not_wired: [{ path: 'motu', reason: 'detection task failed' }],
    reasons: { mute: 'no card', vol: 'no card', state: 'no card' }
  })
  assert.equal(status.detected, false)
  assert.equal(status.mute, null)
  assert.equal(status.vol, null)
  assert.equal(status.state, null)
  assert.equal(status.read_error?.code, 'E_TIMEOUT')
  assert.equal(status.read_error?.retryable, true)
  assert.equal(status.write?.status, 'not_detected')
  assert.deepEqual(status.not_wired, [{ path: 'motu', reason: 'detection task failed' }])
  assert.deepEqual(motuReasonEntries(status), [
    { path: 'mute', reason: 'no card' },
    { path: 'state', reason: 'no card' },
    { path: 'vol', reason: 'no card' }
  ])
  assert.equal(motuReasonFor(status, 'vol'), 'no card')
  assert.equal(motuReasonFor(status, 'unknown-path'), null)
})

test('parseMotuStatus accepts detected=null (unknown != not detected) and missing optional maps', () => {
  const status = parseMotuStatus({
    schema: MOTU_STATUS_SCHEMA,
    detected: null,
    model: null,
    card_id: null,
    card_index: null,
    control_reachable: null,
    reason: null,
    reason_detail: null,
    channel_count: 2,
    channels: STATUS_WIRED.channels,
    mute: null,
    vol: null,
    state: null,
    read_error: null,
    write: { status: 'unknown', single_key: true, readback: true, two_phase: true, batch: false, max_channels_per_request: 2, reason: 'task failed' },
    not_wired: [],
    reasons: {}
  })
  assert.equal(status.detected, null)
  assert.equal(status.control_reachable, null)
  assert.deepEqual(status.reasons, {})
  assert.equal(motuIsWritable(status), false)
})

test('parseMotuStatus rejects malformed structures instead of faking a device', () => {
  assert.throws(() => parseMotuStatus(null), /body must be an object/)
  assert.throws(() => parseMotuStatus({ ...STATUS_WIRED, schema: 'khaen-motu/v2' }), /schema/)
  assert.throws(() => parseMotuStatus({ ...STATUS_WIRED, vol: 'loud' }), /vol/)
  assert.throws(
    () => parseMotuStatus({ ...STATUS_WIRED, vol: [{ channel: 1, linear: 'unity' }] }),
    /linear/
  )
  assert.throws(
    () => parseMotuStatus({ ...STATUS_WIRED, write: { ...STATUS_WIRED.write, status: 'maybe' } }),
    /write\.status/
  )
  assert.throws(() => parseMotuStatus({ ...STATUS_WIRED, detected: 'yes' }), /detected/)
})

test('GET /api/motu uses GET + credentials and no CSRF (read-only)', async () => {
  const { api, calls } = createClient(() => jsonResponse(200, STATUS_WIRED))
  const status = await api.fetchMotu()
  assert.equal(status.detected, true)
  assert.equal(calls.length, 1)
  const call = calls[0]
  assert.equal(String(call.url).replace('http://mock.local', ''), MOTU_STATUS_PATH)
  assert.equal(call.method, 'GET')
  assert.equal(call.credentials, 'include')
  assert.equal(call.body, undefined)
  assert.equal(call.headers['X-KHAEN-CSRF'], undefined)
})

test('PUT /api/motu/volume prepare sends the exact two-phase shape with CSRF', async () => {
  const bodies: unknown[] = []
  const { api, calls } = createClient((call) => {
    bodies.push(JSON.parse(String(call.body)))
    return jsonResponse(200, PREPARE_ACK)
  })

  const draft = await api.prepareVolume('both', -6)
  assert.equal(calls.length, 1)
  const call = calls[0]
  assert.equal(String(call.url).replace('http://mock.local', ''), MOTU_VOLUME_PATH)
  assert.equal(call.method, 'PUT')
  assert.equal(call.credentials, 'include')
  assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-test')
  assert.equal(call.headers['Content-Type'], 'application/json')
  assert.deepEqual(bodies[0], { stage: 'prepare', channel: 'both', db: -6 })

  // 回执：engine_token/token/TTL/target/write_plan/commit_supported/two_phase/writes_device=false。
  assert.equal(draft.engine_token, '0011223344556677')
  assert.equal(draft.token, '0011223344556677')
  assert.equal(draft.created_at, '2026-11-26T12:00:00.000Z')
  assert.equal(draft.expires_in_s, 120)
  assert.deepEqual(
    draft.target?.channels.map((channel) => channel.channel),
    [1, 2]
  )
  assert.equal(draft.target?.linear, 0.5)
  assert.equal(draft.write_plan?.writes, 2)
  assert.equal(draft.write_plan?.steps?.[0]?.key, 'koBusFader')
  assert.equal(draft.commit_supported, true)
  assert.equal(draft.two_phase, true)
  assert.equal(draft.writes_device, false)
  assert.equal(draft.applies_to, 'MOTU CueMix datastore')
  assert.equal(draft.target_device?.detected, true)

  await api.prepareVolume(1, 0)
  assert.deepEqual(bodies[1], { stage: 'prepare', channel: 1, db: 0 })
  await api.prepareVolume(2, -12.5)
  assert.deepEqual(bodies[2], { stage: 'prepare', channel: 2, db: -12.5 })
})

test('prepare rejects a contract drift where writes_device claims the device was written', async () => {
  const { api } = createClient(() => jsonResponse(200, { ...PREPARE_ACK, writes_device: true }))
  const error = await captureError(() => api.prepareVolume('both', -6))
  assert.equal(error.code, 'invalid_response')
  assert.match(error.message, /writes_device/)
})

test('PUT /api/motu/volume commit sends {stage, engine_token} and returns truth readback', async () => {
  const bodies: unknown[] = []
  const { api, calls } = createClient((call) => {
    bodies.push(JSON.parse(String(call.body)))
    return jsonResponse(200, COMMIT_ACK)
  })

  const ack = await api.commitVolume('0011223344556677')
  assert.equal(calls.length, 1)
  const call = calls[0]
  assert.equal(String(call.url).replace('http://mock.local', ''), MOTU_VOLUME_PATH)
  assert.equal(call.method, 'PUT')
  assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-test')
  assert.deepEqual(bodies[0], { stage: 'commit', engine_token: '0011223344556677' })

  assert.equal(ack.applied, true)
  assert.equal(ack.idempotent, false)
  assert.equal(ack.linear, 0.5)
  assert.ok(Math.abs((ack.db ?? 0) - -6.020599913279624) < 1e-9)
  assert.equal(ack.writes_device, true)
  assert.equal(ack.channels?.length, 2)
  assert.equal(ack.channels?.[0]?.before, 1)
  assert.equal(ack.channels?.[0]?.wrote, 0.5)
  assert.equal(ack.channels?.[0]?.readback, 0.5)
  assert.equal(ack.channels?.[0]?.verified, true)
  // idempotent 的第二通道：值本来就在位（未发写帧但仍 verified）。
  assert.equal(ack.channels?.[1]?.frame_sent, false)
  assert.equal(ack.channels?.[1]?.verified, true)
})

test('commit parser tolerates missing channels but rejects malformed receipts', () => {
  const noChannels = parseMotuVolumeCommitAck({ ...COMMIT_ACK, channels: null })
  assert.equal(noChannels.channels, null)
  assert.throws(
    () => parseMotuVolumeCommitAck({ ...COMMIT_ACK, channels: [{ channel: 1, readback: 'loud' }] }),
    /readback/
  )
  assert.throws(() => parseMotuVolumeCommitAck({ ...COMMIT_ACK, ok: false }), /ok must be true/)
  assert.throws(() => parseMotuVolumeDraft({ ...PREPARE_ACK, engine_token: '' }), /engine_token/)
})

test('classifyMotuError maps every stable MOTU error code honestly', () => {
  const cases: Array<[ApiError, string, string, boolean]> = [
    [
      new ApiError({ status: 503, code: 'motu_not_detected', message: 'no card' }),
      'not_detected',
      'motu.error.notDetected',
      true
    ],
    [
      new ApiError({ status: 503, code: 'motu_control_unreachable', message: 'link down' }),
      'control_unreachable',
      'motu.error.controlUnreachable',
      true
    ],
    [
      new ApiError({ status: 503, code: 'motu_control_disabled', message: 'disabled' }),
      'control_disabled',
      'motu.error.controlDisabled',
      true
    ],
    [
      new ApiError({ status: 400, code: 'motu_batch_rejected', message: 'too many' }),
      'batch_rejected',
      'motu.error.batchRejected',
      false
    ],
    [
      new ApiError({ status: 400, code: 'invalid_request', message: 'bad db' }),
      'invalid_request',
      'motu.error.invalidRequest',
      false
    ],
    [
      new ApiError({ status: 409, code: 'engine_token_invalid', message: 'expired' }),
      'token_invalid',
      'motu.error.tokenInvalid',
      true
    ],
    [
      new ApiError({ status: 502, code: 'E_READBACK_MISMATCH', message: 'readback mismatch' }),
      'device_error',
      'motu.error.device',
      true
    ],
    [
      new ApiError({ status: 501, code: 'unsupported', message: 'not implemented' }),
      'unsupported',
      'motu.error.unsupported',
      true
    ],
    [
      new ApiError({ status: 401, code: 'not_authenticated', message: 'expired' }),
      'unauthorized',
      'motu.error.unauthorized',
      false
    ],
    [new ApiError({ status: 500, code: 'internal_error', message: 'boom' }), 'server', 'motu.error.server', true],
    [new ApiError({ status: 0, code: 'network_error', message: 'down' }), 'network', 'motu.error.network', true],
    [new ApiError({ status: 0, code: 'request_timeout', message: 'slow' }), 'timeout', 'motu.error.timeout', true],
    [
      new ApiError({ status: 200, code: 'invalid_response', message: 'bad json' }),
      'invalid_response',
      'motu.error.invalidResponse',
      false
    ]
  ]
  for (const [error, category, i18nKey, retryable] of cases) {
    const info = classifyMotuError(error)
    assert.equal(info.category, category)
    assert.equal(info.i18nKey, i18nKey)
    assert.equal(info.retryable, retryable)
    assert.equal(info.code, error.code)
    assert.equal(info.status, error.status)
  }
  const unknown = classifyMotuError(new Error('boom'))
  assert.equal(unknown.category, 'unknown')
  assert.equal(unknown.i18nKey, 'motu.error.unknown')
})

test('classifyMotuError extracts details.motu / details.completed for a 502 device failure', () => {
  const completedReceipt = {
    channel: 1,
    name: 'Main 1',
    key: 'koBusFader',
    index: 0,
    before: 1.0,
    wrote: 0.5,
    readback: 0.5,
    changed: true,
    frame_sent: true,
    verified: true
  }
  const details = {
    phase: 'motu_volume_commit',
    channel: 2,
    requested_channels: [1, 2],
    requested_linear: 0.5,
    completed: [completedReceipt],
    writes_device: true,
    draft_consumed: true,
    motu: { code: 'E_READBACK_MISMATCH', message: 'device read back 0.7, expected 0.5', retryable: false }
  }
  const info = classifyMotuError(
    new ApiError({ status: 502, code: 'E_READBACK_MISMATCH', message: 'readback mismatch', details })
  )
  assert.equal(info.category, 'device_error')
  assert.equal(info.deviceCode, 'E_READBACK_MISMATCH')
  assert.equal(info.deviceMessage, 'device read back 0.7, expected 0.5')
  assert.equal(info.deviceRetryable, false)
  assert.equal(info.failedChannel, 2)
  assert.deepEqual(info.requestedChannels, [1, 2])
  assert.equal(info.partialWrites, true)
  assert.equal(info.draftConsumed, true)
  assert.equal(info.completed?.length, 1)
  assert.deepEqual(info.completedChannels, [1])
  // details 原样保留（页面原样展示）。
  assert.deepEqual(info.details, details)
})

test('write gate and reason helpers mirror the backend ensure_ready semantics', () => {
  const wired = parseMotuStatus(STATUS_WIRED)
  assert.equal(motuIsWritable(wired), true)
  assert.equal(motuIsWritable(null), false)

  const unreachable = parseMotuStatus({
    ...STATUS_WIRED,
    control_reachable: false,
    write: { ...STATUS_WIRED.write, status: 'unreachable', reason: 'probe failed' }
  })
  assert.equal(motuIsWritable(unreachable), false)
  assert.equal(unreachable.write?.reason, 'probe failed')

  assert.equal(motuVolumeFor(wired, 1)?.linear, 1)
  assert.equal(motuVolumeFor(wired, 2)?.linear, 0.5)
  assert.equal(motuVolumeFor(wired, 7), null)
  assert.equal(motuVolumeFor(null, 1), null)

  assert.equal(motuDetectionReasonKey('detected'), 'motu.detectReason.detected')
  assert.equal(motuDetectionReasonKey('no_motu_card'), MOTU_DETECTION_REASON_KEYS.no_motu_card)
  assert.equal(motuDetectionReasonKey('E_SOMETHING_NEW'), null)
  assert.equal(motuDetectionReasonKey(null), null)
})

test('channel/db validation and request builders match the UI contract (1/2/both, one dB target)', () => {
  assert.deepEqual([...MOTU_MAIN_CHANNELS], [1, 2])
  assert.equal(validateMotuChannelSelection(1), null)
  assert.equal(validateMotuChannelSelection(2), null)
  assert.equal(validateMotuChannelSelection('both'), null)
  assert.equal(validateMotuChannelSelection(3), 'channel')
  assert.equal(validateMotuChannelSelection([1, 2]), 'channel')

  assert.equal(MOTU_VOLUME_DB_MAX, 0)
  assert.equal(MOTU_VOLUME_DB_MIN, -144.5)
  assert.equal(validateMotuVolumeDb(0), null)
  assert.equal(validateMotuVolumeDb(-6), null)
  assert.equal(validateMotuVolumeDb(MOTU_VOLUME_DB_MIN), null)
  assert.equal(validateMotuVolumeDb(0.5), 'db')
  assert.equal(validateMotuVolumeDb(-200), 'db')
  assert.equal(validateMotuVolumeDb(Number.NaN), 'db')
  assert.equal(validateMotuVolumeDb('loud'), 'db')

  assert.deepEqual(buildMotuVolumePrepareBody('both', -6), { stage: 'prepare', channel: 'both', db: -6 })
  assert.deepEqual(buildMotuVolumeCommitBody('tok'), { stage: 'commit', engine_token: 'tok' })
})
