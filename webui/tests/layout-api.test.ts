/**
 * Layout 页 API 契约测试（node --test，无 DOM / 无网络）。
 *
 * 契约来源：docs/API.md v1.1 §4（surface layout 两阶段）与 §9.8（每个新页面/交互必须有 mock
 * 契约测试覆盖 prepare/commit/abort/401/503/失败回执；断言 path/method/body）。
 *
 * 用 scripts/mock-test-support.mjs 的 createRecordingFetch/jsonResponse 注入 ApiClient：
 * 所有 mock promise 都会 resolve/reject（不会挂住 node --test 的门禁）。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient, isApiError } from '../src/api/client.ts'
import {
  CONTROL_PATHS,
  classifyControlError,
  createControlApi,
  type ControlCommitAck,
  type LayoutPatch
} from '../src/api/control.ts'
import { createControlController, createInitialControlState } from '../src/stores/control.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

/**
 * 与 crates/host/src/control/mod.rs prepare_surface_json 的真实回执一致：
 * layout 是方案补丁，回执只有 surface / engine_token / planned_version / graph_id / admission，
 * 没有 changed/diff/current/target（页面必须显示「服务端未上报」，不得臆造）。
 */
const LAYOUT_PREPARE = {
  ok: true,
  surface: 'layout',
  engine_token: 'layout-token-1',
  planned_version: 13,
  graph_id: 'ab'.repeat(32),
  admission: { accepted: true, budget: { folds: 2 } }
}

const LAYOUT_COMMIT = {
  ok: true,
  applied_block: 1665,
  version: 13,
  all_ack: true,
  graph_id: 'ab'.repeat(32)
}

const LAYOUT_ABORT = { ok: true, aborted: true, aborted_surface: 'layout' }

function makeHarness(handler) {
  const recording = createRecordingFetch(handler)
  const unauthorized = []
  const client = new ApiClient({
    baseUrl: 'http://mock.local',
    fetchImpl: recording.fetch,
    csrfToken: () => 'csrf-layout',
    onUnauthorized: () => unauthorized.push('client-401')
  })
  return { recording, unauthorized, client, api: createControlApi(client) }
}

function bodyOf(call) {
  return JSON.parse(String(call.body))
}

async function expectApiError(run) {
  try {
    await run()
  } catch (error) {
    assert.ok(isApiError(error), 'expected ApiError, got: ' + String(error))
    return error
  }
  return assert.fail('expected the request to reject')
}

test('layout prepare posts POST /api/surface with the exact [src, dst, gain] body', async () => {
  const h = makeHarness(() => jsonResponse(200, LAYOUT_PREPARE))
  const patch: LayoutPatch = {
    in_channels: 2,
    out_lanes: 4,
    folds: [
      [0, 0, 1],
      [1, 2, -3.5]
    ]
  }
  const draft = await h.api.prepareLayout(patch)

  assert.equal(h.recording.calls.length, 1)
  const call = h.recording.calls[0]
  assert.equal(String(call.url), 'http://mock.local' + CONTROL_PATHS.surface)
  assert.equal(call.method, 'POST')
  assert.equal(call.credentials, 'include')
  assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-layout')
  assert.deepEqual(bodyOf(call), {
    surface: 'layout',
    data: {
      in_channels: 2,
      out_lanes: 4,
      folds: [
        [0, 0, 1],
        [1, 2, -3.5]
      ]
    }
  })

  assert.equal(draft.surface, 'layout')
  assert.equal(draft.engine_token, 'layout-token-1')
  assert.equal(draft.planned_version, 13)
  assert.equal(draft.graph_id, 'ab'.repeat(32))
  // 可选回执字段缺失 = null（页面显示「服务端未上报」，不臆造）。
  assert.equal(draft.changed, null)
  assert.equal(draft.diff, null)
  assert.equal(draft.current, null)
  assert.equal(draft.target, null)
})

test('layout commit and abort post exactly {engine_token}', async () => {
  const responses = [jsonResponse(200, LAYOUT_COMMIT), jsonResponse(200, LAYOUT_ABORT)]
  const h = makeHarness((_call, index) => responses[index])

  const ack: ControlCommitAck = await h.api.commit('layout-token-1')
  const aborted = await h.api.abort('layout-token-1')

  assert.equal(h.recording.calls.length, 2)
  assert.equal(String(h.recording.calls[0].url), 'http://mock.local' + CONTROL_PATHS.commit)
  assert.equal(h.recording.calls[0].method, 'POST')
  assert.deepEqual(bodyOf(h.recording.calls[0]), { engine_token: 'layout-token-1' })
  assert.equal(String(h.recording.calls[1].url), 'http://mock.local' + CONTROL_PATHS.abort)
  assert.deepEqual(bodyOf(h.recording.calls[1]), { engine_token: 'layout-token-1' })

  assert.equal(ack.version, 13)
  assert.equal(ack.applied_block, 1665)
  assert.equal(aborted.aborted, true)
  assert.equal(aborted.aborted_surface, 'layout')
})

test('layout prepare 401 reaches the shared onUnauthorized hook', async () => {
  const h = makeHarness(() =>
    jsonResponse(401, { error: { code: 'unauthorized', message: 'session expired' } })
  )
  const error = await expectApiError(() =>
    h.api.prepareLayout({ in_channels: 2, out_lanes: 2, folds: [[0, 0, 1]] })
  )
  assert.equal(error.status, 401)
  assert.deepEqual(h.unauthorized, ['client-401'])
  assert.equal(classifyControlError(error).category, 'unauthorized')
})

test('layout prepare 503 not_wired is classified as not-wired', async () => {
  const h = makeHarness(() =>
    jsonResponse(503, { error: { code: 'not_wired', message: 'layout surface is not wired' } })
  )
  const error = await expectApiError(() =>
    h.api.prepareLayout({ in_channels: 2, out_lanes: 2, folds: [[0, 0, 1]] })
  )
  assert.equal(error.code, 'not_wired')
  const info = classifyControlError(error)
  assert.equal(info.category, 'not_wired')
  assert.equal(info.i18nKey, 'error.control.notWired')
})

test('layout prepare 409 no_base_plan keeps the stable code and a localized guidance key', async () => {
  const h = makeHarness(() =>
    jsonResponse(409, {
      error: {
        code: 'no_base_plan',
        message: 'no committed plan to patch: prepare + commit a plan first'
      }
    })
  )
  const error = await expectApiError(() =>
    h.api.prepareLayout({ in_channels: 2, out_lanes: 2, folds: [[0, 0, 1]] })
  )
  assert.equal(error.status, 409)
  assert.equal(error.code, 'no_base_plan')
  const info = classifyControlError(error)
  assert.equal(info.category, 'no_base_plan')
  assert.equal(info.i18nKey, 'error.control.noBasePlan')
  assert.equal(info.detail, 'no committed plan to patch: prepare + commit a plan first')
})

test('layout store flow: prepare failure clears the draft and exposes no_base_plan', async () => {
  const recording = createRecordingFetch(() =>
    jsonResponse(409, {
      error: { code: 'no_base_plan', message: 'no committed plan to patch: prepare + commit a plan first' }
    })
  )
  const client = new ApiClient({
    baseUrl: 'http://mock.local',
    fetchImpl: recording.fetch,
    csrfToken: () => 'csrf-layout'
  })
  const state = createInitialControlState()
  const controller = createControlController(state, { api: createControlApi(client), now: () => 2000 })

  const prepared = await controller.prepareLayout({ in_channels: 2, out_lanes: 2, folds: [[0, 0, 1]] })
  assert.equal(prepared, null)
  assert.equal(state.draft, null)
  assert.equal(state.error?.category, 'no_base_plan')
  assert.equal(state.error?.i18nKey, 'error.control.noBasePlan')
  assert.equal(state.history[0].action, 'prepare')
  assert.equal(state.history[0].code, 'no_base_plan')
  assert.deepEqual(bodyOf(recording.calls[0]), {
    surface: 'layout',
    data: { in_channels: 2, out_lanes: 2, folds: [[0, 0, 1]] }
  })
})

test('layout store flow: commit consumes the token once, abort stays reachable', async () => {
  const responses = [
    jsonResponse(200, LAYOUT_PREPARE),
    jsonResponse(200, LAYOUT_COMMIT),
    jsonResponse(200, LAYOUT_PREPARE),
    jsonResponse(200, LAYOUT_ABORT)
  ]
  const recording = createRecordingFetch((_call, index) => responses[index])
  const client = new ApiClient({
    baseUrl: 'http://mock.local',
    fetchImpl: recording.fetch,
    csrfToken: () => 'csrf-layout'
  })
  const state = createInitialControlState()
  const controller = createControlController(state, { api: createControlApi(client), now: () => 3000 })

  await controller.prepareLayout({ in_channels: 2, out_lanes: 2, folds: [[0, 0, 1]] })
  assert.equal(state.draft?.surface, 'layout')
  const ack = await controller.commit()
  assert.equal(ack?.version, 13)
  assert.equal(state.draft, null)
  const callsAfterCommit = recording.calls.length
  assert.equal(await controller.commit(), null)
  assert.equal(recording.calls.length, callsAfterCommit)

  // abort 必须可达：重新 prepare 一份草稿后可以丢弃。
  await controller.prepareLayout({ in_channels: 3, out_lanes: 3, folds: [[0, 0, 1]] })
  assert.notEqual(state.draft, null)
  const aborted = await controller.abort()
  assert.equal(aborted?.aborted, true)
  assert.equal(state.draft, null)
  assert.equal(String(recording.calls.at(-1).url), 'http://mock.local/api/abort')
})