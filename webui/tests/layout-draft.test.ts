/**
 * Layout 页表单纯逻辑测试（node --test，无 DOM）：本地草稿 ↔ surface 补丁 ↔ 校验映射。
 *
 * 验收要求：
 * - in_channels / out_lanes / folds（[src,dst,gain] 行增删）本地校验复用 validateLayoutPatch；
 * - 本地草稿 → POST /api/surface body 与 docs/API.md §4 layout.data 完全一致；
 * - 当前值不可读、changed/diff/current/target 缺省显示「服务端未上报」等诚实语义由纯函数保证。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient } from '../src/api/client.ts'
import { CONTROL_PATHS, createControlApi, type LayoutPatch } from '../src/api/control.ts'
import {
  LAYOUT_U16_MAX,
  LAYOUT_WRITE_BLOCK_KEYS,
  addFoldRow,
  buildLayoutPatchFromDraft,
  changedStateKey,
  commitChangedNoop,
  createFoldDraft,
  createLayoutDraft,
  foreignDraftSurface,
  layoutCommitBlocked,
  layoutDraftOf,
  layoutValidationKey,
  layoutWriteBlock,
  optionalReceiptText,
  removeFoldRow,
  statusShowsNoBasePlan,
  validateLayoutDraft
} from '../src/components/layout/layoutDraft.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

const LAYOUT_PREPARE = {
  ok: true,
  surface: 'layout',
  engine_token: 'layout-token-form',
  planned_version: 21,
  graph_id: 'cd'.repeat(32),
  admission: { accepted: true }
}

test('layout draft starts from an explicit local draft (not a fake current layout)', () => {
  const draft = createLayoutDraft()
  assert.deepEqual(draft.folds, [{ src: 0, dst: 0, gain: 1 }])
  assert.equal(draft.inChannels, 2)
  assert.equal(draft.outLanes, 2)
  assert.equal(validateLayoutDraft(draft), null)
  // 默认值能被完整表达成契约补丁。
  assert.deepEqual(buildLayoutPatchFromDraft(draft), {
    in_channels: 2,
    out_lanes: 2,
    folds: [[0, 0, 1]]
  })
})

test('layout fold rows can be added and removed (empty table is a local validation error)', () => {
  const draft = createLayoutDraft()
  addFoldRow(draft)
  addFoldRow(draft)
  assert.equal(draft.folds.length, 3)
  assert.deepEqual(draft.folds[2], createFoldDraft())

  assert.equal(removeFoldRow(draft, 1), true)
  assert.equal(draft.folds.length, 2)
  assert.equal(removeFoldRow(draft, 9), false)
  assert.equal(draft.folds.length, 2)

  // 删到 0 行：本地校验给出 empty（后端同样拒绝空表）。
  assert.equal(removeFoldRow(draft, 1), true)
  assert.equal(removeFoldRow(draft, 0), true)
  assert.equal(draft.folds.length, 0)
  assert.equal(validateLayoutDraft(draft), 'empty')
  assert.equal(layoutValidationKey('empty'), 'layout.validation.empty')
  assert.equal(layoutValidationKey(null), null)
})

test('layout local validation reuses validateLayoutPatch error codes', () => {
  const draft = createLayoutDraft()
  assert.equal(validateLayoutDraft(draft), null)

  draft.inChannels = 0
  assert.equal(validateLayoutDraft(draft), 'channels')
  assert.equal(layoutValidationKey('channels'), 'layout.validation.channels')

  draft.inChannels = 2
  draft.outLanes = null
  assert.equal(validateLayoutDraft(draft), 'channels')

  draft.outLanes = 2
  const row = draft.folds[0]
  assert.ok(row !== undefined)
  row.gain = Number.NaN
  assert.equal(validateLayoutDraft(draft), 'entry')
  assert.equal(layoutValidationKey('entry'), 'layout.validation.entry')

  row.gain = 1
  row.src = null
  assert.equal(validateLayoutDraft(draft), 'entry')
})

test('layout write gate blocks not-wired > safe/faulted > busy > invalid form', () => {
  const base = { surface: 'wired', safe: false, faulted: false, busy: false, validation: null }
  assert.equal(layoutWriteBlock(base), null)
  assert.equal(layoutWriteBlock({ ...base, surface: 'not_wired' }), 'capability')
  assert.equal(layoutWriteBlock({ ...base, safe: true }), 'locked')
  assert.equal(layoutWriteBlock({ ...base, faulted: true, validation: 'empty' }), 'locked')
  assert.equal(layoutWriteBlock({ ...base, busy: true }), 'busy')
  assert.equal(layoutWriteBlock({ ...base, validation: 'entry' }), 'invalid')
  // 能力未知不是"未接通"：允许尝试，由后端 501/503 如实报错。
  assert.equal(layoutWriteBlock({ ...base, surface: 'unknown' }), null)
  for (const key of Object.values(LAYOUT_WRITE_BLOCK_KEYS)) {
    assert.match(key, /^layout\.block\./)
  }
})

test('layout draft builds the exact POST /api/surface body from the form', async () => {
  const draft = createLayoutDraft()
  draft.inChannels = 3
  draft.outLanes = 4
  draft.folds[0] = { src: 2, dst: 3, gain: 0.5 }
  addFoldRow(draft)
  draft.folds[1] = { src: 0, dst: 1, gain: -1 }

  const patch: LayoutPatch = buildLayoutPatchFromDraft(draft)
  assert.equal(validateLayoutDraft(draft), null)

  const recording = createRecordingFetch(() => jsonResponse(200, LAYOUT_PREPARE))
  const client = new ApiClient({
    baseUrl: 'http://mock.local',
    fetchImpl: recording.fetch,
    csrfToken: () => 'csrf-layout'
  })
  const api = createControlApi(client)
  await api.prepareLayout(patch)

  assert.equal(String(recording.calls[0].url), 'http://mock.local' + CONTROL_PATHS.surface)
  assert.equal(recording.calls[0].method, 'POST')
  assert.deepEqual(JSON.parse(String(recording.calls[0].body)), {
    surface: 'layout',
    data: {
      in_channels: 3,
      out_lanes: 4,
      folds: [
        [2, 3, 0.5],
        [0, 1, -1]
      ]
    }
  })
  assert.equal(LAYOUT_U16_MAX, 65535)
})

test('layout honest optional fields: missing changed/diff/current/target stays "not reported"', () => {
  // changed 三态：null → 未上报（不做默认值推断）。
  assert.equal(changedStateKey(true), 'draftReview.changedYes')
  assert.equal(changedStateKey(false), 'draftReview.changedNo')
  assert.equal(changedStateKey(null), 'draftReview.notReported')

  // diff/current/target 缺失返回 null（页面渲染「服务端未上报」），有值只做 JSON 序列化。
  assert.equal(optionalReceiptText(null), null)
  assert.equal(optionalReceiptText(undefined), null)
  assert.equal(optionalReceiptText({ folds: 2 }), JSON.stringify({ folds: 2 }, null, 2))

  const ack = { changed: false } as unknown as Parameters<typeof commitChangedNoop>[0]
  assert.equal(commitChangedNoop(ack), true)
  assert.equal(commitChangedNoop(null), false)

  // /api/status.plan.version 为空时才提前提示「先提交方案」。
  assert.equal(statusShowsNoBasePlan(null), true)
  assert.equal(statusShowsNoBasePlan(undefined), true)
  assert.equal(statusShowsNoBasePlan(7), false)
})

test('layout draft ownership helpers ignore drafts of other surfaces', () => {
  const layoutDraft = { surface: 'layout' }
  assert.equal(layoutDraftOf(layoutDraft), layoutDraft)
  assert.equal(layoutDraftOf({ surface: 'mode' }), null)
  assert.equal(layoutDraftOf(null), null)

  // 控制面单 pending 槽被其它界面占用时页面要提示（prepare 会替换它）。
  assert.equal(foreignDraftSurface({ surface: 'mode' }, 'layout'), 'mode')
  assert.equal(foreignDraftSurface({ surface: 'layout' }, 'layout'), null)
  assert.equal(foreignDraftSurface(null, 'layout'), null)
})
test('layout commit is blocked in read-only/capability states (abort stays reachable by design)', () => {
  assert.equal(layoutCommitBlocked({ surface: 'wired', safe: false, faulted: false }), false)
  assert.equal(layoutCommitBlocked({ surface: 'wired', safe: true, faulted: false }), true)
  assert.equal(layoutCommitBlocked({ surface: 'wired', safe: false, faulted: true }), true)
  assert.equal(layoutCommitBlocked({ surface: 'not_wired', safe: false, faulted: false }), true)
  assert.equal(layoutCommitBlocked({ surface: 'unknown', safe: false, faulted: false }), false)
})
