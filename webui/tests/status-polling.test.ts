import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiError } from '../src/api/client.ts'
import { parseStatusResponse, type StatusResponse } from '../src/api/status.ts'
import {
  STATUS_POLL_INTERVAL_MS,
  createInitialStatusState,
  createStatusController
} from '../src/stores/status.ts'
import { buildStatusFixture } from '../scripts/status-fixtures.mjs'

const NOW = Date.parse('2026-11-26T12:00:00.000Z')

function fixture(name: 'present' | 'stale' | 'missing' = 'present'): StatusResponse {
  return parseStatusResponse(buildStatusFixture(name))
}

interface DeferredCall {
  signal?: AbortSignal
  resolve: (value: StatusResponse) => void
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
  const fetchStatus = (options: { signal?: AbortSignal } = {}): Promise<StatusResponse> =>
    new Promise<StatusResponse>((resolve, reject) => {
      calls.push({ signal: options.signal, resolve, reject })
    })
  return { fetchStatus, calls }
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

function createHarness() {
  const state = createInitialStatusState()
  const queue = createFetchQueue()
  const timers = createTimers()
  let clock = NOW
  const controller = createStatusController(state, {
    fetchStatus: queue.fetchStatus,
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

test('default poll interval is 5 seconds', () => {
  assert.equal(STATUS_POLL_INTERVAL_MS, 5000)
})

test('start fetches immediately and schedules the next tick after the interval', async () => {
  const h = createHarness()
  h.controller.start()
  assert.equal(h.queue.calls.length, 1)
  assert.equal(h.state.phase, 'loading')
  assert.equal(h.state.refreshing, true)

  h.queue.calls[0].resolve(fixture('present'))
  await flush()

  assert.equal(h.state.phase, 'ready')
  assert.equal(h.state.data?.state.value, 'running')
  assert.equal(h.state.refreshing, false)
  assert.equal(h.state.loadedAt, NOW)
  const pending = h.timers.pending()
  assert.equal(pending.length, 1)
  assert.equal(pending[0].delayMs, STATUS_POLL_INTERVAL_MS)
})

test('start is idempotent (no duplicate timers or requests)', async () => {
  const h = createHarness()
  h.controller.start()
  h.controller.start()
  assert.equal(h.queue.calls.length, 1)
  h.queue.calls[0].resolve(fixture())
  await flush()
  assert.equal(h.timers.pending().length, 1)
})

test('a timer tick fetches again and refresh updates loadedAt', async () => {
  const h = createHarness()
  h.controller.start()
  h.queue.calls[0].resolve(fixture('present'))
  await flush()

  h.advanceClock(5000)
  await h.timers.runNext()
  assert.equal(h.queue.calls.length, 2)
  assert.equal(h.timers.pending().length, 0)

  h.queue.calls[1].resolve(fixture('stale'))
  await flush()
  assert.equal(h.state.data?.state.value, 'faulted')
  assert.equal(h.state.loadedAt, NOW + 5000)
  assert.equal(h.timers.pending().length, 1)
})

test('manual refresh dedupes into the in-flight request', async () => {
  const h = createHarness()
  h.controller.start()
  const first = h.controller.refresh()
  const second = h.controller.refresh()
  assert.equal(h.queue.calls.length, 1)

  h.queue.calls[0].resolve(fixture())
  await first
  await second
  assert.equal(h.state.phase, 'ready')
  assert.equal(h.timers.pending().length, 1)
})

test('pause aborts the in-flight request, ignores the abort and stops polling', async () => {
  const h = createHarness()
  h.controller.start()
  h.controller.pause()
  assert.equal(h.queue.calls[0].signal?.aborted, true)
  assert.equal(h.timers.pending().length, 0)

  h.queue.calls[0].reject(new ApiError({ status: 0, code: 'network_error', message: 'aborted by test' }))
  await flush()
  assert.equal(h.state.error, null)
  assert.equal(h.state.phase, 'loading')
  assert.equal(h.state.data, null)

  h.controller.resume()
  assert.equal(h.queue.calls.length, 2)
  h.queue.calls[1].resolve(fixture())
  await flush()
  assert.equal(h.state.phase, 'ready')
  assert.equal(h.timers.pending().length, 1)
})

test('pause after a successful load clears the timer; resume refreshes immediately', async () => {
  const h = createHarness()
  h.controller.start()
  h.queue.calls[0].resolve(fixture())
  await flush()
  assert.equal(h.timers.pending().length, 1)

  h.controller.pause()
  assert.equal(h.timers.pending().length, 0)

  h.controller.resume()
  assert.equal(h.queue.calls.length, 2)
  h.queue.calls[1].resolve(fixture('stale'))
  await flush()
  assert.equal(h.timers.pending().length, 1)
})

test('an initial failure enters error phase but keeps polling and recovers', async () => {
  const h = createHarness()
  h.controller.start()
  h.queue.calls[0].reject(new ApiError({ status: 0, code: 'network_error', message: 'no daemon' }))
  await flush()

  assert.equal(h.state.phase, 'error')
  assert.equal(h.state.error?.category, 'network')
  assert.equal(h.state.data, null)
  assert.equal(h.state.refreshing, false)
  assert.equal(h.timers.pending().length, 1)

  await h.timers.runNext()
  assert.equal(h.queue.calls.length, 2)
  h.queue.calls[1].resolve(fixture())
  await flush()
  assert.equal(h.state.phase, 'ready')
  assert.equal(h.state.error, null)
  assert.equal(h.state.data?.state.value, 'running')
})

test('a failed background refresh keeps the last good data but records the error', async () => {
  const h = createHarness()
  h.controller.start()
  h.queue.calls[0].resolve(fixture())
  await flush()
  const loadedAt = h.state.loadedAt
  assert.ok(loadedAt !== null)

  await h.timers.runNext()
  h.queue.calls[1].reject(new ApiError({ status: 503, code: 'internal_error', message: 'rt-host down' }))
  await flush()

  assert.equal(h.state.phase, 'ready')
  assert.equal(h.state.data?.state.value, 'running')
  assert.equal(h.state.loadedAt, loadedAt)
  assert.equal(h.state.error?.category, 'server')
  assert.equal(h.timers.pending().length, 1)
})

test('401 is classified as unauthorized and marked non-retryable', async () => {
  const h = createHarness()
  h.controller.start()
  h.queue.calls[0].reject(new ApiError({ status: 401, code: 'not_authenticated', message: 'expired' }))
  await flush()
  assert.equal(h.state.error?.category, 'unauthorized')
  assert.equal(h.state.error?.i18nKey, 'error.status.unauthorized')
  assert.equal(h.state.error?.retryable, false)
  assert.equal(h.state.data, null)
})

test('stop cancels the timer, aborts in-flight work and can be restarted', async () => {
  const h = createHarness()
  h.controller.start()
  h.controller.stop()
  assert.equal(h.queue.calls[0].signal?.aborted, true)
  assert.equal(h.timers.pending().length, 0)
  assert.equal(h.controller.isPolling(), false)

  h.queue.calls[0].resolve(fixture())
  await flush()
  assert.equal(h.state.phase, 'loading')

  h.controller.pause()
  h.controller.resume()
  assert.equal(h.queue.calls.length, 1)

  h.controller.start()
  assert.equal(h.queue.calls.length, 2)
  h.queue.calls[1].resolve(fixture())
  await flush()
  assert.equal(h.state.phase, 'ready')
  assert.equal(h.controller.isPolling(), true)
})

test('an abort during a background refresh keeps the previous snapshot', async () => {
  const h = createHarness()
  h.controller.start()
  h.queue.calls[0].resolve(fixture())
  await flush()

  await h.timers.runNext()
  h.controller.pause()
  assert.equal(h.queue.calls[1].signal?.aborted, true)
  h.queue.calls[1].reject(new ApiError({ status: 0, code: 'network_error', message: 'aborted' }))
  await flush()

  assert.equal(h.state.data?.state.value, 'running')
  assert.equal(h.state.error, null)
  assert.equal(h.state.phase, 'ready')
  assert.equal(h.controller.isPaused(), true)
})
