import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiError } from '../src/api/client.ts'
import type { MotuStatus, MotuVolumeCommitAck, MotuVolumeDraft } from '../src/api/motu.ts'
import {
  MOTU_POLL_INTERVAL_MS,
  MOTU_VOLUME_HISTORY_LIMIT,
  createInitialMotuPollState,
  createInitialMotuVolumeState,
  createMotuPollController,
  createMotuVolumeController
} from '../src/stores/motu.ts'

const NOW = Date.parse('2026-11-26T12:00:00.000Z')

function statusFixture(overrides: Partial<MotuStatus> = {}): MotuStatus {
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
      { channel: 1, name: 'Main 1', datastore: { key: 'koBusFader', index: 0 } },
      { channel: 2, name: 'Main 2', datastore: { key: 'koBusFader', index: 1 } }
    ],
    mute: false,
    vol: [
      { channel: 1, name: 'Main 1', datastore: { key: 'koBusFader', index: 0 }, linear: 1, db: 0 },
      { channel: 2, name: 'Main 2', datastore: { key: 'koBusFader', index: 1 }, linear: 0.5, db: -6.0206 }
    ],
    state: null,
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
    cache: null,
    ...overrides
  }
}

function draftFixture(overrides: Partial<MotuVolumeDraft> = {}): MotuVolumeDraft {
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
        { channel: 1, name: 'Main 1', datastore: { key: 'koBusFader', index: 0 } },
        { channel: 2, name: 'Main 2', datastore: { key: 'koBusFader', index: 1 } }
      ],
      linear: 0.5,
      db: -6.0206
    },
    write_plan: null,
    commit_supported: true,
    two_phase: true,
    writes_device: false,
    applies_to: 'MOTU CueMix datastore',
    target_device: null,
    ...overrides
  }
}

function ackFixture(overrides: Partial<MotuVolumeCommitAck> = {}): MotuVolumeCommitAck {
  return {
    ok: true,
    schema: 'khaen-motu-volume/v1',
    stage: 'commit',
    engine_token: 'tok-1',
    applied: true,
    idempotent: false,
    created_at: '2026-11-26T12:00:00.000Z',
    linear: 0.5,
    db: -6.0206,
    channels: [
      {
        channel: 1,
        name: 'Main 1',
        key: 'koBusFader',
        index: 0,
        before: 1,
        wrote: 0.5,
        readback: 0.5,
        changed: true,
        frame_sent: true,
        verified: true
      }
    ],
    write_plan: null,
    writes_device: true,
    applies_to: 'MOTU CueMix datastore',
    target_device: null,
    ...overrides
  }
}

/* ------------------------------------------------------------------ 轮询 */

interface DeferredCall {
  signal?: AbortSignal
  resolve: (value: MotuStatus) => void
  reject: (error: unknown) => void
}

interface TimerTask {
  callback: () => void
  delayMs: number
  cancelled: boolean
}

function flush(): Promise<void> {
  return new Promise((resolve) => setImmediate(resolve))
}

function createFetchQueue() {
  const calls: DeferredCall[] = []
  const fetchMotu = (options: { signal?: AbortSignal } = {}): Promise<MotuStatus> =>
    new Promise<MotuStatus>((resolve, reject) => {
      calls.push({ signal: options.signal, resolve, reject })
    })
  return { fetchMotu, calls }
}

function createTimers() {
  const tasks: TimerTask[] = []
  return {
    schedule(callback: () => void, delayMs: number): TimerTask {
      const task: TimerTask = { callback, delayMs, cancelled: false }
      tasks.push(task)
      return task
    },
    cancel(handle: unknown): void {
      ;(handle as TimerTask).cancelled = true
    },
    pending(): TimerTask[] {
      return tasks.filter((task) => !task.cancelled)
    },
    async runNext(): Promise<void> {
      const task = tasks.find((candidate) => !candidate.cancelled)
      assert.ok(task, 'expected a pending timer')
      task.cancelled = true
      task.callback()
      await flush()
    }
  }
}

function createPollHarness() {
  const state = createInitialMotuPollState()
  const queue = createFetchQueue()
  const timers = createTimers()
  let clock = NOW
  const controller = createMotuPollController(state, {
    fetchMotu: queue.fetchMotu,
    now: () => clock,
    schedule: timers.schedule,
    cancel: timers.cancel
  })
  return {
    state,
    queue,
    timers,
    controller,
    advanceClock(ms: number) {
      clock += ms
    }
  }
}

test('default MOTU poll interval is 5 seconds', () => {
  assert.equal(MOTU_POLL_INTERVAL_MS, 5000)
})

test('start fetches immediately then schedules the next tick after 5s', async () => {
  const h = createPollHarness()
  h.controller.start()
  assert.equal(h.queue.calls.length, 1)
  assert.equal(h.state.phase, 'loading')
  assert.equal(h.state.refreshing, true)

  h.queue.calls[0].resolve(statusFixture())
  await flush()

  assert.equal(h.state.phase, 'ready')
  assert.equal(h.state.status?.detected, true)
  assert.equal(h.state.status?.vol?.[1]?.linear, 0.5)
  assert.equal(h.state.refreshing, false)
  assert.equal(h.state.loadedAt, NOW)
  const pending = h.timers.pending()
  assert.equal(pending.length, 1)
  assert.equal(pending[0].delayMs, MOTU_POLL_INTERVAL_MS)
})

test('start is idempotent and a tick refetches while keeping a single timer', async () => {
  const h = createPollHarness()
  h.controller.start()
  h.controller.start()
  assert.equal(h.queue.calls.length, 1)
  h.queue.calls[0].resolve(statusFixture())
  await flush()
  assert.equal(h.timers.pending().length, 1)

  h.advanceClock(5000)
  await h.timers.runNext()
  assert.equal(h.queue.calls.length, 2)
  h.queue.calls[1].resolve(statusFixture({ detected: false }))
  await flush()
  assert.equal(h.state.status?.detected, false)
  assert.equal(h.state.loadedAt, NOW + 5000)
  assert.equal(h.timers.pending().length, 1)
})

test('manual refresh dedupes into the in-flight request', async () => {
  const h = createPollHarness()
  h.controller.start()
  const first = h.controller.refresh()
  const second = h.controller.refresh()
  assert.equal(h.queue.calls.length, 1)
  h.queue.calls[0].resolve(statusFixture())
  await first
  await second
  assert.equal(h.state.phase, 'ready')
  assert.equal(h.timers.pending().length, 1)
})

test('pause aborts in-flight work, ignores the abort and stops polling; resume refreshes at once', async () => {
  const h = createPollHarness()
  h.controller.start()
  h.controller.pause()
  assert.equal(h.queue.calls[0].signal?.aborted, true)
  assert.equal(h.timers.pending().length, 0)

  h.queue.calls[0].reject(new ApiError({ status: 0, code: 'network_error', message: 'aborted by pause' }))
  await flush()
  assert.equal(h.state.error, null)
  assert.equal(h.state.phase, 'loading')
  assert.equal(h.state.status, null)
  assert.equal(h.controller.isPaused(), true)

  h.controller.resume()
  assert.equal(h.queue.calls.length, 2)
  h.queue.calls[1].resolve(statusFixture())
  await flush()
  assert.equal(h.state.phase, 'ready')
  assert.equal(h.timers.pending().length, 1)
})

test('a failed background poll keeps the last snapshot and records the error', async () => {
  const h = createPollHarness()
  h.controller.start()
  h.queue.calls[0].resolve(statusFixture())
  await flush()
  const loadedAt = h.state.loadedAt

  await h.timers.runNext()
  h.queue.calls[1].reject(new ApiError({ status: 503, code: 'motu_not_detected', message: 'no device' }))
  await flush()

  assert.equal(h.state.phase, 'ready')
  assert.equal(h.state.status?.detected, true)
  assert.equal(h.state.loadedAt, loadedAt)
  assert.equal(h.state.error?.category, 'not_detected')
  assert.equal(h.state.error?.i18nKey, 'motu.error.notDetected')
  assert.equal(h.timers.pending().length, 1)

  // 恢复后错误清空
  await h.timers.runNext()
  h.queue.calls[2].resolve(statusFixture())
  await flush()
  assert.equal(h.state.error, null)
})

test('an initial failure enters the error phase but keeps polling', async () => {
  const h = createPollHarness()
  h.controller.start()
  h.queue.calls[0].reject(new ApiError({ status: 500, code: 'internal_error', message: 'boom' }))
  await flush()
  assert.equal(h.state.phase, 'error')
  assert.equal(h.state.status, null)
  assert.equal(h.state.error?.category, 'server')
  assert.equal(h.state.refreshing, false)
  assert.equal(h.timers.pending().length, 1)
})

test('401 during polling is classified as unauthorized (session store handles the redirect)', async () => {
  const h = createPollHarness()
  h.controller.start()
  h.queue.calls[0].reject(new ApiError({ status: 401, code: 'not_authenticated', message: 'expired' }))
  await flush()
  assert.equal(h.state.error?.category, 'unauthorized')
  assert.equal(h.state.error?.i18nKey, 'motu.error.unauthorized')
  assert.equal(h.state.error?.retryable, false)
})

test('stop cancels the timer, aborts in-flight work and can be restarted', async () => {
  const h = createPollHarness()
  h.controller.start()
  h.controller.stop()
  assert.equal(h.queue.calls[0].signal?.aborted, true)
  assert.equal(h.timers.pending().length, 0)
  assert.equal(h.controller.isPolling(), false)

  h.queue.calls[0].resolve(statusFixture())
  await flush()
  assert.equal(h.state.phase, 'loading')

  h.controller.start()
  assert.equal(h.queue.calls.length, 2)
  h.queue.calls[1].resolve(statusFixture())
  await flush()
  assert.equal(h.state.phase, 'ready')
  assert.equal(h.controller.isPolling(), true)
})

/* -------------------------------------------------------------- 音量两阶段 */

interface VolumeCalls {
  prepare: Array<{ selection: string; db: number }>
  commit: string[]
}

function createVolumeHarness(overrides: {
  prepareVolume?: (selection: unknown, db: number) => Promise<MotuVolumeDraft>
  commitVolume?: (engineToken: string) => Promise<MotuVolumeCommitAck>
} = {}) {
  const calls: VolumeCalls = { prepare: [], commit: [] }
  const api = {
    async prepareVolume(selection: unknown, db: number): Promise<MotuVolumeDraft> {
      calls.prepare.push({ selection: String(selection), db })
      if (overrides.prepareVolume) {
        return await overrides.prepareVolume(selection, db)
      }
      return draftFixture()
    },
    async commitVolume(engineToken: string): Promise<MotuVolumeCommitAck> {
      calls.commit.push(engineToken)
      if (overrides.commitVolume) {
        return await overrides.commitVolume(engineToken)
      }
      return ackFixture()
    }
  }
  const state = createInitialMotuVolumeState()
  let clock = NOW
  const controller = createMotuVolumeController(state, { api, now: () => clock })
  return {
    state,
    calls,
    controller,
    advanceClock(ms: number) {
      clock += ms
    }
  }
}

test('prepare stores the draft (writes_device=false) and records history', async () => {
  const h = createVolumeHarness()
  const draft = await h.controller.prepare('both', -6)
  assert.equal(draft?.engine_token, 'tok-1')
  assert.deepEqual(h.calls.prepare, [{ selection: 'both', db: -6 }])
  assert.equal(h.state.draft?.writes_device, false)
  assert.equal(h.state.busy, null)
  assert.equal(h.state.history.length, 1)
  assert.equal(h.state.history[0].action, 'prepare')
  assert.equal(h.state.history[0].ok, true)
  assert.deepEqual(h.state.history[0].channels, [1, 2])
})

test('prepare rejects invalid local input without sending a request', async () => {
  const h = createVolumeHarness()
  assert.equal(await h.controller.prepare(3 as never, -6), null)
  assert.equal(await h.controller.prepare('both', 1), null)
  assert.equal(h.calls.prepare.length, 0)
  assert.equal(h.state.draft, null)
})

test('prepare failure keeps no draft and records the stable error', async () => {
  const h = createVolumeHarness({
    async prepareVolume(): Promise<MotuVolumeDraft> {
      throw new ApiError({ status: 503, code: 'motu_control_unreachable', message: 'link down' })
    }
  })
  assert.equal(await h.controller.prepare(1, -6), null)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.error?.category, 'control_unreachable')
  assert.equal(h.state.error?.i18nKey, 'motu.error.controlUnreachable')
  assert.equal(h.state.history[0].ok, false)
  assert.equal(h.state.history[0].code, 'motu_control_unreachable')
})

test('commit applies the truth readback, clears the draft and keeps lastCommit', async () => {
  const h = createVolumeHarness()
  await h.controller.prepare(1, -6)
  h.advanceClock(1000)
  const ack = await h.controller.commit()
  assert.equal(ack?.applied, true)
  assert.equal(ack?.writes_device, true)
  assert.deepEqual(h.calls.commit, ['tok-1'])
  assert.equal(h.state.lastCommit?.channels?.[0]?.readback, 0.5)
  assert.equal(h.state.lastCommitAt, NOW + 1000)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.error, null)
  assert.equal(h.state.history[0].action, 'commit')
  assert.equal(h.state.history[0].ok, true)
})

test('commit without a draft is a no-op; a 409 token clears the stale draft', async () => {
  const h = createVolumeHarness({
    async commitVolume(): Promise<MotuVolumeCommitAck> {
      throw new ApiError({ status: 409, code: 'engine_token_invalid', message: 'expired' })
    }
  })
  assert.equal(await h.controller.commit(), null)
  assert.equal(h.calls.commit.length, 0)

  await h.controller.prepare(1, -6)
  assert.equal(await h.controller.commit(), null)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.error?.category, 'token_invalid')
  assert.equal(h.state.lastCommit, null)
})

test('a 502 device failure never reports success and clears a consumed draft', async () => {
  const details = {
    phase: 'motu_volume_commit',
    channel: 2,
    requested_channels: [1, 2],
    requested_linear: 0.5,
    completed: [
      {
        channel: 1,
        name: 'Main 1',
        key: 'koBusFader',
        index: 0,
        before: 1,
        wrote: 0.5,
        readback: 0.5,
        changed: true,
        frame_sent: true,
        verified: true
      }
    ],
    writes_device: true,
    draft_consumed: true,
    motu: { code: 'E_READBACK_MISMATCH', message: 'readback mismatch', retryable: false }
  }
  const h = createVolumeHarness({
    async commitVolume(): Promise<MotuVolumeCommitAck> {
      throw new ApiError({ status: 502, code: 'E_READBACK_MISMATCH', message: 'readback mismatch', details })
    }
  })
  await h.controller.prepare('both', -6)
  assert.equal(await h.controller.commit(), null)
  assert.equal(h.state.lastCommit, null, 'a failed commit must never become a success receipt')
  assert.equal(h.state.error?.category, 'device_error')
  assert.equal(h.state.error?.deviceCode, 'E_READBACK_MISMATCH')
  assert.deepEqual(h.state.error?.completedChannels, [1])
  assert.equal(h.state.error?.partialWrites, true)
  assert.equal(h.state.draft, null, 'draft_consumed=true means the token is gone')
  assert.equal(h.state.history[0].ok, false)
  assert.equal(h.state.history[0].code, 'E_READBACK_MISMATCH')
})

test('a retryable commit failure keeps the draft for a retry (device temporarily unavailable)', async () => {
  const h = createVolumeHarness({
    async commitVolume(): Promise<MotuVolumeCommitAck> {
      throw new ApiError({ status: 503, code: 'motu_control_unreachable', message: 'link down' })
    }
  })
  await h.controller.prepare(1, -6)
  assert.equal(await h.controller.commit(), null)
  assert.notEqual(h.state.draft, null, 'backend keeps the draft when the device is not ready')
  assert.equal(h.state.error?.category, 'control_unreachable')
})

test('local cancel drops the draft with a TTL notice and sends no request (no abort endpoint)', async () => {
  const h = createVolumeHarness()
  assert.equal(h.controller.cancelDraft(), null)
  await h.controller.prepare('both', -6)
  h.advanceClock(2000)
  const notice = h.controller.cancelDraft()
  assert.notEqual(notice, null)
  assert.equal(notice?.token, 'tok-1')
  assert.equal(notice?.ttlSeconds, 120)
  assert.equal(notice?.cancelledAt, NOW + 2000)
  assert.deepEqual(notice?.channels, [1, 2])
  assert.equal(h.state.draft, null)
  assert.deepEqual(h.state.localCancel, notice)
  assert.equal(h.calls.commit.length, 0, 'cancel must not call commit/abort')
  assert.ok(h.calls.prepare.length === 1)
  assert.equal(h.state.history[0].action, 'cancel')
  assert.equal(h.state.history[0].ok, true)
})

test('busy guard serialises prepare/commit (single draft slot semantics)', async () => {
  let release: (value: MotuVolumeDraft) => void = () => {}
  const pending = new Promise<MotuVolumeDraft>((resolve) => {
    release = resolve
  })
  const h = createVolumeHarness({ prepareVolume: () => pending })
  const first = h.controller.prepare('both', -6)
  assert.equal(h.state.busy, 'prepare')
  assert.equal(await h.controller.commit(), null)
  assert.equal(await h.controller.prepare(1, -3), null)
  assert.equal(h.calls.prepare.length, 1)
  release(draftFixture())
  assert.equal((await first)?.engine_token, 'tok-1')
  assert.equal(h.state.busy, null)
})

test('dismissError and reset clear the volume state', async () => {
  const h = createVolumeHarness({
    async prepareVolume(): Promise<MotuVolumeDraft> {
      throw new ApiError({ status: 503, code: 'motu_not_detected', message: 'no device' })
    }
  })
  await h.controller.prepare(1, -6)
  assert.notEqual(h.state.error, null)
  h.controller.dismissError()
  assert.equal(h.state.error, null)

  await h.controller.prepare(1, -6)
  h.controller.reset()
  assert.deepEqual(h.state, createInitialMotuVolumeState())
})

test('history is capped at MOTU_VOLUME_HISTORY_LIMIT entries', async () => {
  const h = createVolumeHarness()
  for (let index = 0; index < MOTU_VOLUME_HISTORY_LIMIT + 2; index += 1) {
    await h.controller.prepare(1, -index)
    h.controller.cancelDraft()
  }
  assert.equal(h.state.history.length, MOTU_VOLUME_HISTORY_LIMIT)
})
