import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  SYSTEM_LOGS_INTERVAL_MS,
  createInitialSystemInfoState,
  createInitialSystemLogsState,
  createSystemInfoController,
  createSystemLogsController
} from '../src/stores/system.ts'
import {
  createInitialReleaseVersionState,
  createReleaseVersionController
} from '../src/stores/version.ts'
import { buildSystemInfoFixture, buildSystemLogsFixture } from '../scripts/system-fixtures.mjs'
import { ApiError } from '../src/api/client.ts'

function infoBody(): any {
  return buildSystemInfoFixture('mismatch')
}

function logsBody(unit = 'daemon', lines = 5): any {
  return buildSystemLogsFixture('mismatch', unit, lines).body
}

test('system info controller loads once and exposes data', async () => {
  const state = createInitialSystemInfoState()
  let calls = 0
  const controller = createSystemInfoController(state, {
    fetchInfo: async () => {
      calls += 1
      return infoBody()
    },
    now: () => 1234
  })
  assert.equal(state.phase, 'loading')
  await controller.load()
  assert.equal(calls, 1)
  assert.equal(state.phase, 'ready')
  assert.equal(state.data?.crate_version, '0.1.0')
  assert.equal(state.loadedAt, 1234)
  assert.equal(state.error, null)
  assert.equal(state.refreshing, false)
})

test('system info controller dedupes concurrent loads', async () => {
  const state = createInitialSystemInfoState()
  let calls = 0
  let resolve: (value: any) => void = () => {}
  const pending = new Promise((done) => {
    resolve = done
  })
  const controller = createSystemInfoController(state, {
    fetchInfo: async () => {
      calls += 1
      return pending
    }
  })
  const first = controller.load()
  const second = controller.refresh()
  assert.equal(first, second)
  resolve(infoBody())
  await first
  assert.equal(calls, 1)
})

test('system info refresh failure keeps the last good data and records the error', async () => {
  const state = createInitialSystemInfoState()
  let fail = false
  const controller = createSystemInfoController(state, {
    fetchInfo: async () => {
      if (fail) {
        throw new ApiError({ status: 503, code: 'logs_unavailable', message: 'down' })
      }
      return infoBody()
    }
  })
  await controller.load()
  assert.equal(state.phase, 'ready')
  fail = true
  await controller.refresh()
  assert.equal(state.phase, 'ready')
  assert.equal(state.data?.crate_version, '0.1.0')
  assert.equal(state.error?.category, 'unavailable')
})

test('system info first failure enters error phase and retry recovers', async () => {
  const state = createInitialSystemInfoState()
  let fail = true
  const controller = createSystemInfoController(state, {
    fetchInfo: async () => {
      if (fail) {
        throw new ApiError({ status: 0, code: 'network_error', message: 'offline' })
      }
      return infoBody()
    }
  })
  await controller.load()
  assert.equal(state.phase, 'error')
  assert.equal(state.data, null)
  assert.equal(state.error?.category, 'network')
  fail = false
  await controller.refresh()
  assert.equal(state.phase, 'ready')
  assert.equal(state.error, null)
})

test('system logs controller starts immediately and exposes lines', async () => {
  const state = createInitialSystemLogsState()
  assert.equal(state.unit, 'daemon')
  assert.equal(state.lines, 200)
  assert.equal(state.autoRefresh, false)
  const controller = createSystemLogsController(state, {
    fetchLogs: async () => logsBody('daemon', 100)
  })
  controller.start()
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(state.phase, 'ready')
  assert.equal(state.data?.lines.length, 100)
  assert.equal(state.loading, false)
  controller.stop()
})

test('system logs controller switches unit and lines, clearing stale data', async () => {
  const state = createInitialSystemLogsState()
  const calls: Array<{ unit: string; lines: number }> = []
  const controller = createSystemLogsController(state, {
    fetchLogs: async (params) => {
      calls.push({ unit: params.unit, lines: params.lines })
      return logsBody(params.unit, params.lines)
    }
  })
  controller.start()
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(calls[0].unit, 'daemon')

  controller.setUnit('rt-host')
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(state.unit, 'rt-host')
  assert.equal(calls[1].unit, 'rt-host')

  controller.setLines(500)
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(state.lines, 500)
  assert.equal(calls[2].lines, 500)

  // 非法行数规整为默认值（不发送畸形查询）。
  controller.setLines(0)
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(state.lines, 200)

  controller.stop()
})

test('system logs auto refresh schedules periodic silent refreshes', async () => {
  const state = createInitialSystemLogsState()
  const timers: Array<() => void> = []
  let calls = 0
  const controller = createSystemLogsController(
    state,
    {
      fetchLogs: async () => {
        calls += 1
        return logsBody()
      },
      schedule: (callback) => {
        timers.push(callback)
        return timers.length
      },
      cancel: () => {},
      intervalMs: SYSTEM_LOGS_INTERVAL_MS
    }
  )
  controller.start()
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(calls, 1)
  assert.equal(state.autoRefresh, false)
  // 自动刷新关闭时不应安排定时器。
  assert.equal(timers.length, 0)

  controller.setAutoRefresh(true)
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(calls, 2)
  assert.equal(timers.length, 1)

  // 定时器触发一次（silent refresh），完成后安排下一次。
  const next = timers[0]
  next()
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(calls, 3)
  assert.equal(timers.length, 2)
  controller.stop()
})

test('system logs auto refresh is paused while the tab is hidden', async () => {
  const state = createInitialSystemLogsState()
  const timers: Array<() => void> = []
  let calls = 0
  const controller = createSystemLogsController(state, {
    fetchLogs: async () => {
      calls += 1
      return logsBody()
    },
    schedule: (callback) => {
      timers.push(callback)
      return timers.length
    },
    cancel: () => {}
  })
  controller.start()
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(calls, 1)
  controller.setAutoRefresh(true)
  await new Promise((resolve) => setTimeout(resolve, 0))
  const before = timers.length
  assert.equal(before, 1)
  controller.pause()
  assert.equal(controller.isPaused(), true)
  assert.equal(timers.length, before)
  controller.resume()
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(calls, 3)
  assert.equal(controller.isPaused(), false)
  assert.equal(timers.length > before, true)
  controller.stop()
})

test('system logs failure keeps the last successful lines', async () => {
  const state = createInitialSystemLogsState()
  let fail = false
  const controller = createSystemLogsController(state, {
    fetchLogs: async () => {
      if (fail) {
        throw new ApiError({ status: 503, code: 'logs_unavailable', message: 'journalctl missing' })
      }
      return logsBody()
    }
  })
  controller.start()
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(state.phase, 'ready')
  fail = true
  await controller.refresh()
  assert.equal(state.phase, 'ready')
  assert.equal(state.data?.lines.length, 5)
  assert.equal(state.error?.category, 'unavailable')
  controller.stop()
})

test('system logs stop aborts and allows a restart', async () => {
  const state = createInitialSystemLogsState()
  let calls = 0
  const controller = createSystemLogsController(state, {
    fetchLogs: async () => {
      calls += 1
      return logsBody()
    }
  })
  controller.start()
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(calls, 1)
  controller.stop()
  assert.equal(controller.isStarted(), false)
  controller.start()
  await new Promise((resolve) => setTimeout(resolve, 0))
  assert.equal(calls, 2)
  controller.stop()
})

test('release version controller caches the healthz version and formats the label', async () => {
  const state = createInitialReleaseVersionState()
  let calls = 0
  const controller = createReleaseVersionController(state, {
    fetchHealthz: async () => {
      calls += 1
      return { ok: true, version: 'v0.1.2', uptime_s: 1 }
    },
    now: () => 99
  })
  await controller.load()
  assert.equal(calls, 1)
  assert.equal(state.version, 'v0.1.2')
  assert.equal(state.label, 'v0.1.2')
  assert.equal(state.phase, 'ready')
  assert.equal(state.loadedAt, 99)
  // 已有成功结果且未 force：不重复请求。
  await controller.load()
  assert.equal(calls, 1)
  await controller.load(true)
  assert.equal(calls, 2)
})

test('release version failure keeps the last known version and marks the error', async () => {
  const state = createInitialReleaseVersionState()
  let fail = false
  const controller = createReleaseVersionController(state, {
    fetchHealthz: async () => {
      if (fail) {
        throw new ApiError({ status: 0, code: 'network_error', message: 'offline' })
      }
      return { ok: true, version: '0.1.2', uptime_s: 2 }
    }
  })
  await controller.load()
  assert.equal(state.label, 'v0.1.2')
  fail = true
  await controller.load(true)
  assert.equal(state.version, '0.1.2')
  assert.equal(state.phase, 'error')
  assert.equal(state.error?.category, 'network')
})

test('release version controller starts with null and clear resets it', async () => {
  const state = createInitialReleaseVersionState()
  const controller = createReleaseVersionController(state, {
    fetchHealthz: async () => ({ ok: true, version: null, uptime_s: null })
  })
  await controller.load()
  assert.equal(state.version, null)
  assert.equal(state.label, null)
  assert.equal(state.phase, 'ready')

  controller.clear()
  assert.equal(state.phase, 'idle')
  assert.equal(state.checked, false)
  assert.equal(state.version, null)
})
