import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiError } from '../src/api/client.ts'
import {
  createInitialRoutingEditorState,
  createInitialRoutingState,
  createRoutingController,
  createRoutingEditorController,
  resetRoutingState,
  routingCurrentSource,
  routingState
} from '../src/stores/routing.ts'
import {
  currentRoutingSource,
  parseRoutingChannels,
  parseRoutingPlanHistory
} from '../src/api/routing.ts'
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

/* ------------------------------------------------- routing editor 状态机 */

function sourceFrom(kind = 'wired') {
  return currentRoutingSource(parseRoutingChannels(buildRealChannelsDocument(kind)))
}

function createEditor(kind = 'wired', now = () => 1000) {
  const state = createInitialRoutingEditorState()
  const controller = createRoutingEditorController(state, { current: () => sourceFrom(kind), now })
  return { state, controller }
}

test('routing editor: prefill copies /api/channels.mapping rows and stamps the local time; unknown stays empty', () => {
  const wired = createEditor()
  assert.equal(wired.controller.prefill(), true)
  assert.equal(wired.state.rows.length, 22)
  assert.deepEqual(wired.state.rows[0], { lane: 0, dac: 0 })
  assert.equal(wired.state.dirty, false)
  assert.equal(wired.state.prefilledAt, 1000)
  assert.equal(wired.state.dacChannels, 22)

  const unknown = createEditor('degraded')
  assert.equal(unknown.controller.prefill(), true)
  assert.deepEqual(unknown.state.rows, [])
  assert.equal(unknown.state.prefilledAt, null)
  assert.equal(unknown.state.dacChannels, null)
})

test('routing editor: manual edits are never overwritten by refresh prefill unless forced', () => {
  const { state, controller } = createEditor()
  controller.prefill()
  controller.addRow()
  assert.equal(state.rows.length, 23)
  assert.equal(state.rows[22].lane, 22)
  assert.equal(state.dirty, true)

  controller.prefill()
  assert.equal(state.rows.length, 23)
  assert.equal(state.dirty, true)

  assert.equal(controller.prefill(true), true)
  assert.equal(state.rows.length, 22)
  assert.equal(state.dirty, false)
})

test('routing editor: add/remove/clear round-trip with the smallest unused lane', () => {
  const { state, controller } = createEditor()
  controller.prefill()
  controller.clear()
  assert.deepEqual(state.rows, [])
  assert.equal(controller.validate(), 'empty')

  controller.addRow()
  controller.addRow()
  assert.deepEqual(
    state.rows.map((row) => row.lane),
    [0, 1]
  )
  assert.deepEqual(
    state.rows.map((row) => row.dac),
    [0, 0]
  )
  controller.setLane(0, 5)
  controller.setDac(0, 3)
  assert.deepEqual(state.rows[0], { lane: 5, dac: 3 })

  controller.removeRow(0)
  assert.deepEqual(
    state.rows.map((row) => row.lane),
    [1]
  )
  controller.removeRow(99)
  controller.setLane(99, 0)
  assert.equal(state.rows.length, 1)
})

test('routing editor: validation reports duplicate/invalid/dac-range and dac_channels inherit vs explicit', () => {
  const partial = createEditor('partial')
  partial.controller.prefill()
  assert.deepEqual(partial.state.rows, [
    { lane: 0, dac: 0 },
    { lane: 1, dac: 2 }
  ])
  assert.equal(partial.controller.validate(), null)

  // 重复 lane（全量替换表不允许一个 lane 出现两次）。
  partial.controller.setLane(1, 0)
  assert.equal(partial.controller.validate(), 'duplicate')
  partial.controller.setLane(1, 1)

  // 非法 DAC（负整数）。
  partial.controller.setDac(0, -1)
  assert.equal(partial.controller.validate(), 'entry')
  partial.controller.setDac(0, 0)

  // 当前方案 DAC 声道数 = 4：DAC 4 越界（前端先拦，后端准入仍会再判）。
  partial.controller.setDac(0, 4)
  assert.equal(partial.controller.validate(), 'dac_range')
  partial.controller.setDac(0, 3)
  assert.equal(partial.controller.validate(), null)

  // 显式 dac_channels：空值 -> dac_channels；比 DAC 序号小 -> dac_range。
  partial.controller.setExplicitDacChannels(true)
  assert.equal(partial.state.dacChannels, 4)
  partial.controller.setDacChannels(null)
  assert.equal(partial.controller.validate(), 'dac_channels')
  partial.controller.setDacChannels(2)
  partial.controller.setDac(0, 2)
  assert.equal(partial.controller.validate(), 'dac_range')
  partial.controller.setDacChannels(22)
  assert.equal(partial.controller.validate(), null)
})

test('routing editor: buildPatch is a full replacement table; dac_channels only when explicit', () => {
  const { state, controller } = createEditor('partial')
  controller.prefill()
  assert.deepEqual(controller.buildPatch(), {
    routes: [
      [0, 0],
      [1, 2]
    ]
  })

  controller.setExplicitDacChannels(true)
  assert.deepEqual(controller.buildPatch(), {
    routes: [
      [0, 0],
      [1, 2]
    ],
    dac_channels: 4
  })
  state.dacChannels = 22
  assert.equal(controller.buildPatch().dac_channels, 22)
})

test('routing store: routingCurrentSource selector reads the singleton (mapping null -> unknown)', () => {
  resetRoutingState()
  assert.equal(routingCurrentSource().rows, null)
  assert.equal(routingCurrentSource().known, false)

  routingState.channels = parseRoutingChannels(buildRealChannelsDocument('wired'))
  const wired = routingCurrentSource()
  assert.equal(wired.rows?.length, 22)
  assert.equal(wired.source, 'mapping')
  assert.equal(wired.known, true)

  routingState.channels = null
  assert.equal(routingCurrentSource().rows, null)
  resetRoutingState()
})
