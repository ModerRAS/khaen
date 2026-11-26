/**
 * Mode 页 API 契约测试（node --test，无 DOM / 无网络）。
 *
 * 契约来源：docs/API.md v1.1 §4（两阶段控制面）与 §9.8（每个新页面/交互必须有 mock 契约测试覆盖
 * prepare/commit/abort/401/503/失败回执；断言 path/method/body）。
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
  type ControlDraft
} from '../src/api/control.ts'
import {
  MODE_CHOICES,
  MODE_WRITE_BLOCK_KEYS,
  canonicalMode,
  commitChangedNoop,
  draftChangedNoop,
  foreignDraftSurface,
  isSelectableMode,
  modeCommitBlocked,
  modeDraftOf,
  modeLabelKey,
  modeWriteBlock
} from '../src/components/mode/modeDraft.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

/** 与 crates/host/src/control/mod.rs prepare_mode_json 的真实回执字段一致。 */
const MODE_PREPARE = {
  ok: true,
  surface: 'mode',
  engine_token: 'mode-token-1',
  planned_version: 12,
  mode: 'aes67',
  changed: true,
  applies_to: 'control-plane engine (output enable)',
  note: 'real capture-source switching belongs to rt-host/device supervisor; toslink/dante are not implemented'
}

/** 与 commit_mode 的真实回执一致（changed=true 分支）。 */
const MODE_COMMIT = { ok: true, changed: true, applied_block: 1537, version: 12, mode: 'aes67' }

/** 与 abort 的真实回执一致。 */
const MODE_ABORT = { ok: true, aborted: true, aborted_surface: 'mode' }

function makeHarness(handler) {
  const recording = createRecordingFetch(handler)
  const unauthorized = []
  const client = new ApiClient({
    baseUrl: 'http://mock.local',
    fetchImpl: recording.fetch,
    csrfToken: () => 'csrf-mode',
    onUnauthorized: () => unauthorized.push('client-401')
  })
  return { recording, unauthorized, client, api: createControlApi(client) }
}

function bodyOf(call) {
  return JSON.parse(String(call.body))
}

/** 断言失败请求最终以 ApiError 拒绝，并返回该错误（不会挂起：handler 已同步给出响应）。 */
async function expectApiError(run) {
  try {
    await run()
  } catch (error) {
    assert.ok(isApiError(error), 'expected ApiError, got: ' + String(error))
    return error
  }
  return assert.fail('expected the request to reject')
}

test('mode prepare posts POST /api/surface with the exact contract body', async () => {
  const h = makeHarness(() => jsonResponse(200, MODE_PREPARE))
  const draft = await h.api.prepareMode('aes67')

  assert.equal(h.recording.calls.length, 1)
  const call = h.recording.calls[0]
  assert.equal(String(call.url), 'http://mock.local' + CONTROL_PATHS.surface)
  assert.equal(call.method, 'POST')
  assert.equal(call.credentials, 'include')
  assert.equal(call.headers['Content-Type'], 'application/json')
  assert.equal(call.headers['X-KHAEN-CSRF'], 'csrf-mode')
  assert.deepEqual(bodyOf(call), { surface: 'mode', data: { mode: 'aes67' } })

  assert.equal(draft.ok, true)
  assert.equal(draft.surface, 'mode')
  assert.equal(draft.engine_token, 'mode-token-1')
  assert.equal(draft.planned_version, 12)
  assert.equal(draft.mode, 'aes67')
  assert.equal(draft.changed, true)
  assert.equal(draft.applies_to, 'control-plane engine (output enable)')
  assert.match(String(draft.note), /toslink\/dante are not implemented/)
})

test('mode prepare off posts data.mode="off" (no analog/toslink fake options)', async () => {
  const h = makeHarness(() => jsonResponse(200, { ...MODE_PREPARE, mode: 'off', changed: false }))
  const draft: ControlDraft = await h.api.prepareMode('off')
  assert.deepEqual(bodyOf(h.recording.calls[0]), { surface: 'mode', data: { mode: 'off' } })
  assert.equal(draft.changed, false)

  // UI 可选项只有后端已接通的 off / aes67（on 是 aes67 别名，不单独渲染；analog 等未实现不渲染）。
  assert.deepEqual(
    MODE_CHOICES.map((choice) => choice.value),
    ['off', 'aes67']
  )
  assert.equal(isSelectableMode('on'), false)
  assert.equal(isSelectableMode('analog'), false)
})

test('mode commit and abort post exactly {engine_token} to /api/commit and /api/abort', async () => {
  const responses = [jsonResponse(200, MODE_COMMIT), jsonResponse(200, MODE_ABORT)]
  const h = makeHarness((_call, index) => responses[index])

  const ack: ControlCommitAck = await h.api.commit('mode-token-1')
  const aborted = await h.api.abort('mode-token-1')

  assert.equal(h.recording.calls.length, 2)
  const commitCall = h.recording.calls[0]
  assert.equal(String(commitCall.url), 'http://mock.local' + CONTROL_PATHS.commit)
  assert.equal(commitCall.method, 'POST')
  assert.equal(commitCall.headers['X-KHAEN-CSRF'], 'csrf-mode')
  assert.deepEqual(bodyOf(commitCall), { engine_token: 'mode-token-1' })

  const abortCall = h.recording.calls[1]
  assert.equal(String(abortCall.url), 'http://mock.local' + CONTROL_PATHS.abort)
  assert.equal(abortCall.method, 'POST')
  assert.deepEqual(bodyOf(abortCall), { engine_token: 'mode-token-1' })

  assert.equal(ack.version, 12)
  assert.equal(ack.applied_block, 1537)
  assert.equal(ack.mode, 'aes67')
  assert.equal(ack.changed, true)
  assert.equal(aborted.aborted, true)
  assert.equal(aborted.aborted_surface, 'mode')
})

test('mode prepare 401 goes to the shared onUnauthorized hook and is classified honestly', async () => {
  const h = makeHarness(() =>
    jsonResponse(401, { error: { code: 'unauthorized', message: 'session expired' } })
  )
  const error = await expectApiError(() => h.api.prepareMode('aes67'))

  assert.equal(error.status, 401)
  assert.equal(error.code, 'unauthorized')
  assert.deepEqual(h.unauthorized, ['client-401'])

  const info = classifyControlError(error)
  assert.equal(info.category, 'unauthorized')
  assert.equal(info.i18nKey, 'error.control.unauthorized')
  assert.equal(info.retryable, false)
})

test('mode prepare 503 not_wired is classified as not-wired (never treated as success)', async () => {
  const h = makeHarness(() => jsonResponse(503, { error: { code: 'not_wired', message: 'mode surface is not wired' } }))
  const error = await expectApiError(() => h.api.prepareMode('off'))

  assert.equal(error.status, 503)
  assert.equal(error.code, 'not_wired')
  const info = classifyControlError(error)
  assert.equal(info.category, 'not_wired')
  assert.equal(info.i18nKey, 'error.control.notWired')
  assert.equal(info.retryable, true)
})

test('mode commit 501 unsupported is classified as unsupported (no fake success)', async () => {
  const h = makeHarness(() =>
    jsonResponse(501, { error: { code: 'unsupported', message: 'mode surface not implemented' } })
  )
  const error = await expectApiError(() => h.api.commit('mode-token-1'))
  const info = classifyControlError(error)
  assert.equal(info.category, 'unsupported')
  assert.equal(info.i18nKey, 'error.control.unsupported')
})

test('mode commit 409 engine_token_invalid keeps the stable code for the UI', async () => {
  const h = makeHarness(() =>
    jsonResponse(409, { error: { code: 'engine_token_invalid', message: 'no prepared mode draft' } })
  )
  const error = await expectApiError(() => h.api.commit('stale-token'))
  assert.equal(error.status, 409)
  const info = classifyControlError(error)
  assert.equal(info.category, 'token_invalid')
  assert.equal(info.i18nKey, 'error.control.tokenInvalid')
  assert.equal(info.code, 'engine_token_invalid')
})

test('mode alias/normalization helpers never invent a current mode', () => {
  // CONTRACTS B.1.5："on" 是 aes67 的别名。
  assert.equal(canonicalMode('on'), 'aes67')
  assert.equal(canonicalMode('aes67'), 'aes67')
  assert.equal(canonicalMode('off'), 'off')
  assert.equal(canonicalMode('analog'), null)
  assert.equal(canonicalMode(null), null)
  assert.equal(modeLabelKey('on'), 'mode.target.aes67')
  assert.equal(modeLabelKey('off'), 'mode.target.off')
  assert.equal(modeLabelKey('weird'), null)
})

test('mode write gate blocks not-wired > safe/faulted > busy > no target', () => {
  const base = { surface: 'wired', safe: false, faulted: false, busy: false, target: 'off' }
  assert.equal(modeWriteBlock(base), null)
  assert.equal(modeWriteBlock({ ...base, surface: 'not_wired' }), 'capability')
  assert.equal(modeWriteBlock({ ...base, safe: true, target: null }), 'locked')
  assert.equal(modeWriteBlock({ ...base, faulted: true }), 'locked')
  assert.equal(modeWriteBlock({ ...base, busy: true }), 'busy')
  assert.equal(modeWriteBlock({ ...base, target: null }), 'no-target')
  // 能力未知不是"未接通"：允许尝试，由后端 501/503 如实报错。
  assert.equal(modeWriteBlock({ ...base, surface: 'unknown' }), null)
  for (const key of Object.values(MODE_WRITE_BLOCK_KEYS)) {
    assert.match(key, /^mode\.block\./)
  }
})

test('changed=false is surfaced as a no-op (prepare draft and commit receipt)', () => {
  const draftNoop = { changed: false } as unknown as ControlDraft
  assert.equal(draftChangedNoop(draftNoop), true)
  assert.equal(draftChangedNoop({ changed: true } as unknown as ControlDraft), false)
  assert.equal(draftChangedNoop(null), false)

  const ackNoop = { changed: false } as unknown as ControlCommitAck
  assert.equal(commitChangedNoop(ackNoop), true)
  assert.equal(commitChangedNoop({ changed: true } as unknown as ControlCommitAck), false)
  assert.equal(commitChangedNoop(null), false)
})
test('mode commit is blocked in read-only/capability states (abort stays reachable by design)', () => {
  assert.equal(modeCommitBlocked({ surface: 'wired', safe: false, faulted: false }), false)
  assert.equal(modeCommitBlocked({ surface: 'wired', safe: true, faulted: false }), true)
  assert.equal(modeCommitBlocked({ surface: 'wired', safe: false, faulted: true }), true)
  assert.equal(modeCommitBlocked({ surface: 'not_wired', safe: false, faulted: false }), true)
  // 能力未知不是"未接通"：允许尝试（后端 501/503 会如实报错）。
  assert.equal(modeCommitBlocked({ surface: 'unknown', safe: false, faulted: false }), false)
})

test('mode draft ownership helpers ignore drafts of other surfaces', () => {
  const draft = { surface: 'mode' }
  assert.equal(modeDraftOf(draft), draft)
  assert.equal(modeDraftOf({ surface: 'layout' }), null)
  assert.equal(modeDraftOf(null), null)
  // parseControlDraft 在缺 surface 时用 mode 字段推断，这里保持同样兜底口径。
  assert.notEqual(modeDraftOf({ surface: null, mode: 'off' }), null)

  assert.equal(foreignDraftSurface({ surface: 'layout' }, 'mode'), 'layout')
  assert.equal(foreignDraftSurface({ surface: 'mode' }, 'mode'), null)
  assert.equal(foreignDraftSurface(null, 'mode'), null)
})
