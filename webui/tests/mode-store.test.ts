/**
 * Mode 页 control store 集成测试（node --test，无 DOM / 无网络）。
 *
 * 页面不直接发请求，而是走 stores/control.ts 的两阶段状态机；这里用真实 createControlApi +
 * 注入 fetch 的 ApiClient 驱动 store，断言：
 * - prepare 成功持有草稿、prepare 失败清空旧草稿（避免把上一次意图当成这一次提交）；
 * - engine_token 只能消费一次（commit 成功后草稿清空，重复 commit 不再发请求）；
 * - abort 丢弃草稿并记录回执；
 * - 401 触达共享 onUnauthorized；503 not_wired 与 409 token 失效进入稳定类别。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient } from '../src/api/client.ts'
import { createControlApi } from '../src/api/control.ts'
import { createControlController, createInitialControlState } from '../src/stores/control.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

const MODE_PREPARE = {
  ok: true,
  surface: 'mode',
  engine_token: 'mode-token-1',
  planned_version: 12,
  mode: 'aes67',
  changed: true,
  applies_to: 'control-plane engine (output enable)',
  note: 'mode draft'
}

const MODE_COMMIT = { ok: true, changed: true, applied_block: 1537, version: 12, mode: 'aes67' }
const MODE_ABORT = { ok: true, aborted: true, aborted_surface: 'mode' }

function makeFlow(handler) {
  const recording = createRecordingFetch(handler)
  const unauthorized = []
  const client = new ApiClient({
    baseUrl: 'http://mock.local',
    fetchImpl: recording.fetch,
    csrfToken: () => 'csrf-mode',
    onUnauthorized: () => unauthorized.push('client-401')
  })
  const state = createInitialControlState()
  const controller = createControlController(state, {
    api: createControlApi(client),
    now: () => 1000
  })
  return { recording, unauthorized, state, controller }
}

test('mode flow: prepare holds a draft, commit consumes the token once and records the receipt', async () => {
  const responses = [
    jsonResponse(200, MODE_PREPARE),
    jsonResponse(200, MODE_COMMIT),
    jsonResponse(409, { error: { code: 'engine_token_invalid', message: 'token already consumed' } })
  ]
  const h = makeFlow((_call, index) => responses[index])

  const prepared = await h.controller.prepareMode('aes67')
  assert.equal(prepared?.engine_token, 'mode-token-1')
  assert.equal(h.state.draft?.surface, 'mode')
  assert.equal(h.state.busy, null)
  assert.equal(h.state.error, null)

  const ack = await h.controller.commit()
  assert.equal(ack?.version, 12)
  assert.equal(ack?.applied_block, 1537)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.lastCommit?.mode, 'aes67')
  assert.equal(h.state.appliedMode, 'aes67')

  // 第二次 commit：草稿已被消费，store 不会再发请求（token 只能消费一次）。
  const callsAfterCommit = h.recording.calls.length
  const second = await h.controller.commit()
  assert.equal(second, null)
  assert.equal(h.recording.calls.length, callsAfterCommit)
  assert.equal(h.recording.calls[1].url, 'http://mock.local/api/commit')
  assert.deepEqual(JSON.parse(String(h.recording.calls[1].body)), { engine_token: 'mode-token-1' })
})

test('mode flow: a failed prepare clears the previously staged draft', async () => {
  const responses = [
    jsonResponse(200, MODE_PREPARE),
    jsonResponse(409, { error: { code: 'engine_token_invalid', message: 'draft replaced concurrently' } })
  ]
  const h = makeFlow((_call, index) => responses[index])

  assert.notEqual(await h.controller.prepareMode('aes67'), null)
  assert.notEqual(h.state.draft, null)

  const failed = await h.controller.prepareMode('off')
  assert.equal(failed, null)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.error?.category, 'token_invalid')
  assert.equal(h.state.error?.code, 'engine_token_invalid')
  assert.equal(h.state.history[0].ok, false)
  assert.equal(h.state.history[0].action, 'prepare')
})

test('mode flow: abort discards the draft and records the abort receipt', async () => {
  const responses = [jsonResponse(200, MODE_PREPARE), jsonResponse(200, MODE_ABORT)]
  const h = makeFlow((_call, index) => responses[index])

  await h.controller.prepareMode('off')
  assert.notEqual(h.state.draft, null)
  const aborted = await h.controller.abort()
  assert.equal(aborted?.aborted, true)
  assert.equal(aborted?.aborted_surface, 'mode')
  assert.equal(h.state.draft, null)
  assert.deepEqual(JSON.parse(String(h.recording.calls[1].body)), { engine_token: 'mode-token-1' })
})

test('mode flow: 401 on prepare clears the draft path and reaches the shared hook', async () => {
  const h = makeFlow(() =>
    jsonResponse(401, { error: { code: 'unauthorized', message: 'session expired' } })
  )
  const prepared = await h.controller.prepareMode('aes67')
  assert.equal(prepared, null)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.error?.category, 'unauthorized')
  assert.deepEqual(h.unauthorized, ['client-401'])
})

test('mode flow: 503 not_wired keeps a stable category instead of a fake draft', async () => {
  const h = makeFlow(() =>
    jsonResponse(503, { error: { code: 'not_wired', message: 'mode surface is not wired' } })
  )
  const prepared = await h.controller.prepareMode('aes67')
  assert.equal(prepared, null)
  assert.equal(h.state.draft, null)
  assert.equal(h.state.error?.category, 'not_wired')
  assert.equal(h.state.error?.i18nKey, 'error.control.notWired')
})
