import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiError } from '../src/api/client.ts'
import { createInitialRoutingState, createRoutingController } from '../src/stores/routing.ts'
import { parseRoutingChannels, parseRoutingPlanHistory } from '../src/api/routing.ts'
import { buildRealChannelsDocument, buildRealHistoryDocument } from './routing-fixtures.ts'

function channelsInfo(kind = 'wired') {
  return parseRoutingChannels(buildRealChannelsDocument(kind))
}

function historyInfo() {
  return parseRoutingPlanHistory(buildRealHistoryDocument())
}

/** 可注入 fetch 行为的假 api：记录调用次数；throw 时模拟网络/HTTP 失败。 */
function createFakeApi(options = {}) {
  const calls = { channels: 0, history: 0 }
  const api = {
    async fetchChannels() {
      calls.channels += 1
      if (options.channelsError) {
        throw options.channelsError
      }
      return options.channels ?? channelsInfo()
    },
    async fetchPlanHistory() {
      calls.history += 1
      if (options.historyError) {
        throw options.historyError
      }
      return options.history ?? historyInfo()
    }
  }
  return { api, calls }
}

function networkError() {
  return new ApiError({ status: 0, code: 'network_error', message: 'fetch failed' })
}

function notWiredError() {
  return new ApiError({ status: 501, code: 'not_wired', message: 'M4 API is not implemented (T55)' })
}

function unauthorizedError() {
  return new ApiError({ status: 401, code: 'not_authenticated', message: 'login required' })
}

test('routing store: initial state is empty and visibly loading (no fabricated routing)', () => {
  const state = createInitialRoutingState()
  assert.equal(state.channels, null)
  assert.equal(state.history, null)
  assert.equal(state.channelsPhase, 'loading')
  assert.equal(state.channelsError, null)
  assert.equal(state.historyError, null)
  assert.equal(state.refreshing, false)
  assert.equal(state.loadedAt, null)
})

test('routing store: load fills channels + history and stamps loadedAt', async () => {
  const state = createInitialRoutingState()
  const { api, calls } = createFakeApi()
  const controller = createRoutingController(state, { api, now: () => 1234 })
  await controller.load()
  assert.equal(calls.channels, 1)
  assert.equal(calls.history, 1)
  assert.equal(state.channels?.output.channels, 22)
  assert.equal(state.history?.current_version, 7)
  assert.equal(state.channelsPhase, 'ready')
  assert.equal(state.channelsError, null)
  assert.equal(state.historyError, null)
  assert.equal(state.refreshing, false)
  assert.equal(state.loadedAt, 1234)
})

test('routing store: both endpoints failing keeps nulls, records classified errors, no fake loadedAt', async () => {
  const state = createInitialRoutingState()
  const { api } = createFakeApi({ channelsError: notWiredError(), historyError: networkError() })
  const controller = createRoutingController(state, { api, now: () => 99 })
  await controller.load()
  assert.equal(state.channels, null)
  assert.equal(state.history, null)
  assert.equal(state.channelsPhase, 'error')
  assert.equal(state.channelsError?.category, 'not_wired')
  assert.equal(state.channelsError?.code, 'not_wired')
  assert.equal(state.historyError?.category, 'network')
  assert.equal(state.loadedAt, null)
  assert.equal(state.refreshing, false)
})

test('routing store: channels outage does not hide a usable history (and vice versa)', async () => {
  const state = createInitialRoutingState()
  const { api } = createFakeApi({ channelsError: notWiredError() })
  const controller = createRoutingController(state, { api, now: () => 7 })
  await controller.load()
  assert.equal(state.channels, null)
  assert.equal(state.channelsError?.category, 'not_wired')
  assert.equal(state.history?.current_version, 7)
  assert.equal(state.loadedAt, 7)
})

test('routing store: history failure clears history but keeps channels (no stale version passed off as current)', async () => {
  const state = createInitialRoutingState()
  let failHistory = false
  const { api } = createFakeApi({
    get historyError() {
      return failHistory ? networkError() : undefined
    }
  })
  const controller = createRoutingController(state, { api, now: () => 5 })
  await controller.load()
  assert.equal(state.history?.current_version, 7)

  failHistory = true
  await controller.refresh()
  assert.equal(state.history, null)
  assert.equal(state.historyError?.category, 'network')
  assert.equal(state.channels?.output.channels, 22)
  assert.equal(state.channelsPhase, 'ready')
})

test('routing store: refresh failure after success keeps the last channels snapshot and flags it', async () => {
  const state = createInitialRoutingState()
  let failing = false
  const { api, calls } = createFakeApi()
  const flaky = {
    async fetchChannels() {
      calls.channels += 1
      if (failing) {
        throw networkError()
      }
      return channelsInfo()
    },
    fetchPlanHistory: api.fetchPlanHistory
  }
  const controller = createRoutingController(state, { api: flaky, now: () => 42 })
  await controller.load()
  failing = true
  await controller.refresh()
  assert.equal(calls.channels, 2)
  assert.equal(state.channels?.output.channels, 22)
  assert.equal(state.channelsPhase, 'ready')
  assert.equal(state.channelsError?.category, 'network')
  assert.equal(state.history?.current_version, 7)
  // 有数据仍在展示：loadedAt 不应被失败推进（这里是成功读取的那次时间）。
  assert.equal(state.loadedAt, 42)
})

test('routing store: concurrent load/refresh calls share one in-flight read', async () => {
  const state = createInitialRoutingState()
  const { api, calls } = createFakeApi()
  const controller = createRoutingController(state, { api })
  const first = controller.load()
  const second = controller.refresh()
  assert.equal(first, second)
  await Promise.all([first, second])
  assert.equal(calls.channels, 1)
  assert.equal(calls.history, 1)
})

test('routing store: 401 is classified as unauthorized and surfaces the same state', async () => {
  const state = createInitialRoutingState()
  const { api } = createFakeApi({ channelsError: unauthorizedError(), historyError: unauthorizedError() })
  const controller = createRoutingController(state, { api })
  await controller.load()
  assert.equal(state.channelsError?.category, 'unauthorized')
  assert.equal(state.channelsError?.retryable, false)
  assert.equal(state.channelsPhase, 'error')
  assert.equal(state.historyError?.category, 'unauthorized')
})

test('routing store: reset returns to the initial state', async () => {
  const state = createInitialRoutingState()
  const { api } = createFakeApi()
  const controller = createRoutingController(state, { api })
  await controller.load()
  controller.reset()
  assert.deepEqual(state, createInitialRoutingState())
})
