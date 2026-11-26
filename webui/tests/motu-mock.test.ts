import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient } from '../src/api/client.ts'
import {
  MOTU_VOLUME_PATH,
  classifyMotuError,
  createMotuApi,
  motuIsWritable,
  motuReasonFor,
  parseMotuStatus
} from '../src/api/motu.ts'
import type { MotuStatus } from '../src/api/motu.ts'
import {
  createInitialMotuPollState,
  createInitialMotuVolumeState,
  createMotuPollController,
  createMotuVolumeController
} from '../src/stores/motu.ts'
import {
  formatMotuDb,
  formatMotuTimestamp,
  linearToDb,
  motuCommitWritesDevice,
  motuDraftRemainingSeconds,
  motuReceiptViews,
  motuVerifiedCount,
  motuVolumeValueText
} from '../src/components/motu/format.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

const DATASTORE_0 = { key: 'koBusFader', index: 0 }
const DATASTORE_1 = { key: 'koBusFader', index: 1 }

function statusBody(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    schema: 'khaen-motu/v1',
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
    reasons: {},
    ...overrides
  }
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

function prepareBody(): Record<string, unknown> {
  return {
    ok: true,
    schema: 'khaen-motu-volume/v1',
    stage: 'prepare',
    engine_token: 'tok-1',
    token: 'tok-1',
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
}

const RECEIPT_1 = {
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

function commitBody(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    ok: true,
    schema: 'khaen-motu-volume/v1',
    stage: 'commit',
    engine_token: 'tok-1',
    applied: true,
    idempotent: false,
    created_at: '2026-11-26T12:00:00.000Z',
    linear: 0.5,
    db: -6.020599913279624,
    channels: [RECEIPT_1],
    write_plan: null,
    writes_device: true,
    applies_to: 'MOTU CueMix datastore',
    target_device: TARGET_DEVICE,
    ...overrides
  }
}

function errorBody(status: number, code: string, message: string, details?: unknown): Response {
  return jsonResponse(status, { ok: false, error: { code, message, details } })
}

function pathOf(call: any): string {
  return String(call.url).replace('http://mock.local', '')
}

function bodyOf(call: any): unknown {
  return call.body === undefined ? undefined : JSON.parse(String(call.body))
}

function createVolumeHarness(handler: (call: any, index: number) => Response) {
  const recording = createRecordingFetch(handler)
  const client = new ApiClient({
    baseUrl: 'http://mock.local',
    fetchImpl: recording.fetch,
    csrfToken: () => 'csrf-motu'
  })
  const api = createMotuApi(client)
  const state = createInitialMotuVolumeState()
  const controller = createMotuVolumeController(state, { api, now: () => 1000 })
  return { state, controller, api, calls: recording.calls, client }
}

function createPollHarness(handler: (call: any, index: number) => Response) {
  const recording = createRecordingFetch(handler)
  const client = new ApiClient({
    baseUrl: 'http://mock.local',
    fetchImpl: recording.fetch,
    csrfToken: () => 'csrf-motu',
    onUnauthorized: () => {
      notifications.push('401')
    }
  })
  const notifications: string[] = []
  const api = createMotuApi(client)
  const state = createInitialMotuPollState()
  const timers: Array<() => void> = []
  const controller = createMotuPollController(state, {
    fetchMotu: (options) => api.fetchMotu(options),
    now: () => 2000,
    schedule: (callback) => {
      timers.push(callback)
      return timers.length
    },
    cancel: () => {}
  })
  return { state, controller, api, calls: recording.calls, notifications, timers }
}

test('mock: GET snapshot renders device truth readback; a poll failure keeps the snapshot and marks it', async () => {
  let mode: 'wired' | 'fail' = 'wired'
  const h = createPollHarness(() =>
    mode === 'wired'
      ? jsonResponse(200, statusBody())
      : errorBody(503, 'motu_control_unreachable', 'control network down')
  )
  await h.controller.refresh()
  assert.equal(h.state.phase, 'ready')
  const status = h.state.status
  assert.ok(status !== null)
  assert.equal(h.calls.length, 1)
  assert.equal(pathOf(h.calls[0]), '/api/motu')
  assert.equal(h.calls[0].method, 'GET')
  assert.equal(h.notifications.length, 0)

  // 真值回读渲染：Main 1 = 1.0000 / 0 dB；Main 2 = 0.5000 / -6.02 dB。
  const main1 = motuVolumeValueText(status.vol?.[0] ?? null, 'zh-CN')
  assert.deepEqual(main1, { db: '0', linear: '1', silent: false })
  const main2 = motuVolumeValueText(status.vol?.[1] ?? null, 'zh-CN')
  assert.equal(main2.linear, '0.5')
  assert.equal(main2.db, '-6.02')
  assert.equal(main2.silent, false)
  assert.equal(formatMotuDb(linearToDb(status.vol?.[1]?.linear ?? null), 'zh-CN'), '-6.02')

  // 静音（linear 0）不是“没数据”：silent=true 且 dB 未上报。
  assert.deepEqual(motuVolumeValueText({ linear: 0, db: null }, 'zh-CN'), {
    db: null,
    linear: '0',
    silent: true
  })
  assert.equal(motuVolumeValueText(null, 'zh-CN').db, null)

  mode = 'fail'
  await h.controller.refresh()
  assert.equal(h.state.phase, 'ready')
  assert.equal(h.state.status?.detected, true, 'keeps the last successful snapshot')
  assert.equal(h.state.error?.category, 'control_unreachable')
  assert.equal(h.state.error?.i18nKey, 'motu.error.controlUnreachable')
})

test('mock: degraded snapshot (no device) stays honest and non-writable', async () => {
  const degraded = parseMotuStatus(
    statusBody({
      detected: false,
      model: null,
      card_id: null,
      card_index: null,
      control_reachable: false,
      reason: 'no_motu_card',
      reason_detail: 'no MOTU card was found in /proc/asound/cards',
      mute: null,
      vol: null,
      state: null,
      read_error: null,
      write: {
        status: 'not_detected',
        single_key: true,
        readback: true,
        two_phase: true,
        batch: false,
        max_channels_per_request: 2,
        reason: 'no MOTU card was found in /proc/asound/cards'
      },
      reasons: { mute: 'no card', vol: 'no card', state: 'no card' }
    })
  )
  assert.equal(degraded.detected, false)
  assert.equal(motuIsWritable(degraded), false)
  assert.equal(motuReasonFor(degraded, 'vol'), 'no card')
  assert.equal(degraded.write?.status, 'not_detected')
  // 设备不存在时 vol 为 null：页面显示「未上报 + 原因」，不编 0 dB。
  assert.equal(degraded.vol, null)
  assert.deepEqual(motuVolumeValueText(null, 'en-US'), { db: null, linear: null, silent: false })
})

test('mock: prepare -> commit writes the device with truth readback and never calls abort', async () => {
  const bodies: unknown[] = []
  const h = createVolumeHarness((call) => {
    bodies.push(bodyOf(call))
    if (call.method !== 'PUT') {
      return errorBody(405, 'method_not_allowed', 'PUT expected')
    }
    const body = bodyOf(call) as { stage?: string }
    if (body.stage === 'prepare') {
      return jsonResponse(200, prepareBody())
    }
    if (body.stage === 'commit') {
      return jsonResponse(200, commitBody())
    }
    return errorBody(400, 'invalid_request', 'unknown stage')
  })

  const draft = await h.controller.prepare('both', -6)
  assert.notEqual(draft, null)
  assert.equal(h.state.draft?.writes_device, false, 'prepare never writes the device')
  assert.equal(h.state.lastCommit, null)
  assert.equal(pathOf(h.calls[0]), MOTU_VOLUME_PATH)
  assert.equal(h.calls[0].method, 'PUT')
  assert.equal(h.calls[0].headers['X-KHAEN-CSRF'], 'csrf-motu')
  assert.deepEqual(bodies[0], { stage: 'prepare', channel: 'both', db: -6 })

  const ack = await h.controller.commit()
  assert.notEqual(ack, null)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.lastCommit?.applied, true)
  assert.equal(h.state.lastCommit?.writes_device, true)
  assert.deepEqual(bodies[1], { stage: 'commit', engine_token: 'tok-1' })
  assert.equal(pathOf(h.calls[1]), MOTU_VOLUME_PATH)
  assert.equal(h.calls[1].method, 'PUT')

  // 真值回读渲染：before 1.0 → wrote 0.5 (-6.02 dB) → readback 0.5 (-6.02 dB)，verified。
  const rows = motuReceiptViews(h.state.lastCommit?.channels ?? null, 'zh-CN')
  assert.equal(rows.length, 1)
  assert.equal(rows[0]?.name, 'Main 1')
  assert.equal(rows[0]?.beforeLinear, '1')
  assert.equal(rows[0]?.wroteLinear, '0.5')
  assert.equal(rows[0]?.readbackLinear, '0.5')
  assert.equal(rows[0]?.wroteDb, '-6.02')
  assert.equal(rows[0]?.readbackDb, '-6.02')
  assert.equal(rows[0]?.verified, true)
  assert.deepEqual(motuVerifiedCount(h.state.lastCommit), { verified: 1, total: 1 })
  assert.equal(motuCommitWritesDevice(h.state.lastCommit), true)

  // 端点没有 abort：整个流程只出现 GET(无)/PUT /api/motu/volume 两次，绝不出现 /abort。
  assert.equal(h.calls.length, 2)
  for (const call of h.calls) {
    assert.equal(pathOf(call), MOTU_VOLUME_PATH)
    assert.ok(!pathOf(call).includes('abort'))
  }
})

test('mock: 503 motu_not_detected on prepare shows the reason and sends no device write', async () => {
  const h = createVolumeHarness(() =>
    errorBody(503, 'motu_not_detected', 'no MOTU card was found', {
      reason: 'no_motu_card',
      detection: { ...TARGET_DEVICE, detected: false, control_reachable: false, reason: 'no_motu_card' }
    })
  )
  assert.equal(await h.controller.prepare(1, -6), null)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.error?.category, 'not_detected')
  assert.equal(h.state.error?.retryable, true)
  assert.equal(h.calls.length, 1)
  assert.equal(h.calls[0].method, 'PUT')
  assert.equal(h.state.lastCommit, null)
})

test('mock: 400 motu_batch_rejected is surfaced with the stable code and details', async () => {
  const h = createVolumeHarness(() =>
    errorBody(400, 'motu_batch_rejected', 'refusing a batch MOTU volume write of 3 channels', {
      limit: 2,
      requested: 3
    })
  )
  await h.controller.prepare('both', -6)
  assert.equal(h.state.error?.category, 'batch_rejected')
  assert.equal(h.state.error?.i18nKey, 'motu.error.batchRejected')
  assert.equal(h.state.error?.retryable, false)
  assert.deepEqual(h.state.error?.details, { limit: 2, requested: 3 })
  assert.equal(h.state.draft, null)
})

test('mock: 409 token invalid at commit clears the draft and keeps no success receipt', async () => {
  const h = createVolumeHarness((call) => {
    const body = bodyOf(call) as { stage?: string }
    if (body.stage === 'prepare') {
      return jsonResponse(200, prepareBody())
    }
    return errorBody(409, 'engine_token_invalid', 'the MOTU volume draft expired')
  })
  await h.controller.prepare('both', -6)
  assert.equal(await h.controller.commit(), null)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.lastCommit, null)
  assert.equal(h.state.error?.category, 'token_invalid')
  assert.equal(h.state.error?.retryable, true)
  assert.equal(h.state.history[0].action, 'commit')
  assert.equal(h.state.history[0].ok, false)
})

test('mock: 502 device failure lists completed channels and never claims full success', async () => {
  const details = {
    phase: 'motu_volume_commit',
    channel: 2,
    requested_channels: [1, 2],
    requested_linear: 0.5,
    completed: [RECEIPT_1],
    writes_device: true,
    draft_consumed: true,
    motu: { code: 'E_READBACK_MISMATCH', message: 'device read back 0.7, expected 0.5', retryable: false }
  }
  const h = createVolumeHarness((call) => {
    const body = bodyOf(call) as { stage?: string }
    if (body.stage === 'prepare') {
      return jsonResponse(200, prepareBody())
    }
    return errorBody(502, 'E_READBACK_MISMATCH', 'device readback mismatch', details)
  })
  await h.controller.prepare('both', -6)
  assert.equal(await h.controller.commit(), null)
  assert.equal(h.state.lastCommit, null, 'a 502 must not produce a success receipt')
  assert.equal(h.state.error?.category, 'device_error')
  assert.equal(h.state.error?.deviceCode, 'E_READBACK_MISMATCH')
  assert.equal(h.state.error?.deviceMessage, 'device read back 0.7, expected 0.5')
  assert.deepEqual(h.state.error?.completedChannels, [1])
  assert.equal(h.state.error?.partialWrites, true)
  assert.equal(h.state.error?.draftConsumed, true)
  assert.equal(h.state.draft, null, 'draft_consumed=true -> the token is gone')
  assert.equal(motuCommitWritesDevice(h.state.lastCommit), false)
  assert.equal(motuVerifiedCount(h.state.lastCommit), null)
  // completed 通道可渲染（已写 Main 1，未写 Main 2）。
  const completedRows = motuReceiptViews(h.state.error?.completed ?? null, 'zh-CN')
  assert.equal(completedRows.length, 1)
  assert.equal(completedRows[0]?.channel, 1)
  assert.equal(completedRows[0]?.verified, true)
})

test('mock: 401 during prepare is classified as unauthorized and is not retryable', async () => {
  const h = createVolumeHarness(() => errorBody(401, 'not_authenticated', 'session expired'))
  // 401 的跳登录由共享 ApiClient onUnauthorized 负责；store 只归类为 unauthorized（不重试）。
  assert.equal(await h.controller.prepare(1, -6), null)
  assert.equal(h.state.error?.category, 'unauthorized')
  assert.equal(h.state.error?.retryable, false)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.lastCommit, null)
})

test('mock: 401 on the poll endpoint reaches the shared client hook', async () => {
  const h = createPollHarness(() => errorBody(401, 'not_authenticated', 'session expired'))
  await h.controller.refresh()
  assert.deepEqual(h.notifications, ['401'])
  assert.equal(h.state.error?.category, 'unauthorized')
  assert.equal(h.state.status, null)
})

test('mock: invalid success payloads are rejected with invalid_response (never faked as success)', async () => {
  const h = createVolumeHarness(() => jsonResponse(200, { ok: true, engine_token: 'tok', writes_device: true }))
  await h.controller.prepare(1, -6)
  assert.equal(h.state.error?.category, 'invalid_response')
  assert.equal(h.state.draft, null)

  const poll = createPollHarness(() => jsonResponse(200, { schema: 'khaen-motu/v0', detected: true }))
  await poll.controller.refresh()
  assert.equal(poll.state.error?.category, 'invalid_response')
  assert.equal(poll.state.status, null)
})

test('format helpers: TTL countdown, timestamps, commit write flag and error classification', () => {
  const parsed = parseMotuStatus(statusBody())
  assert.equal(motuIsWritable(parsed), true)

  const draftLike = {
    ok: true as const,
    schema: 'khaen-motu-volume/v1',
    stage: 'prepare',
    engine_token: 'tok',
    token: 'tok',
    created_at: '2026-11-26T12:00:00.000Z',
    expires_in_s: 120,
    target: null,
    write_plan: null,
    commit_supported: true,
    two_phase: true,
    writes_device: false,
    applies_to: null,
    target_device: null
  }
  const createdMs = Date.parse('2026-11-26T12:00:00.000Z')
  assert.equal(motuDraftRemainingSeconds(draftLike, createdMs), 120)
  assert.equal(motuDraftRemainingSeconds(draftLike, createdMs + 30_000), 90)
  assert.equal(motuDraftRemainingSeconds(draftLike, createdMs + 130_000), 0)
  assert.equal(motuDraftRemainingSeconds({ ...draftLike, expires_in_s: null }, createdMs), null)
  assert.equal(motuDraftRemainingSeconds({ ...draftLike, created_at: 'not-a-time' }, createdMs), null)
  assert.equal(formatMotuTimestamp('2026-11-26T12:00:00.000Z', 'zh-CN') !== null, true)
  assert.equal(formatMotuTimestamp('not-a-time', 'zh-CN'), null)

  const info = classifyMotuError(new Error('boom'))
  assert.equal(info.category, 'unknown')
  assert.equal(info.i18nKey, 'motu.error.unknown')
})
