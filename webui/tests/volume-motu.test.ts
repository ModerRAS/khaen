import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import zhMotu from '../src/i18n/messages/motu.zh-CN.ts'
import enMotu from '../src/i18n/messages/motu.en-US.ts'
import zhBase from '../src/i18n/zh-CN.ts'
import enBase from '../src/i18n/en-US.ts'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

function flatten(value: unknown, prefix = ''): Map<string, string> {
  const entries = new Map<string, string>()
  if (typeof value !== 'object' || value === null) {
    return entries
  }
  for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
    const full = prefix === '' ? key : prefix + '.' + key
    if (typeof entry === 'string') {
      entries.set(full, entry)
    } else if (typeof entry === 'object' && entry !== null) {
      for (const [childKey, childValue] of flatten(entry, full)) {
        entries.set(childKey, childValue)
      }
    }
  }
  return entries
}

function read(relativePath: string): string {
  return readFileSync(join(ROOT, relativePath), 'utf8')
}

const zhKeys = flatten(zhMotu)
const enKeys = flatten(enMotu)

test('motu i18n shards exist for both locales with identical key sets', () => {
  assert.ok(zhKeys.size > 80, 'expected a substantial MOTU key set, got ' + zhKeys.size)
  assert.deepEqual([...enKeys.keys()].sort(), [...zhKeys.keys()].sort())
  for (const [key, value] of zhKeys) {
    assert.ok(key.startsWith('motu.'), 'shard key outside the motu namespace: ' + key)
    assert.ok(value.trim().length > 0, 'empty zh-CN value for ' + key)
  }
  for (const [key, value] of enKeys) {
    assert.ok(value.trim().length > 0, 'empty en-US value for ' + key)
  }
})

test('motu shard never redefines a base i18n key', () => {
  const zhBaseKeys = flatten(zhBase)
  const enBaseKeys = flatten(enBase)
  for (const key of zhKeys.keys()) {
    assert.ok(!zhBaseKeys.has(key), 'zh-CN shard duplicates base key ' + key)
  }
  for (const key of enKeys.keys()) {
    assert.ok(!enBaseKeys.has(key), 'en-US shard duplicates base key ' + key)
  }
})

test('every motu.* key literal in the MOTU sources exists in both shards', () => {
  const sources = [
    'src/views/VolumeView.vue',
    'src/api/motu.ts',
    'src/stores/motu.ts',
    'src/components/motu/format.ts',
    'src/components/motu/useMotuPolling.ts',
    'src/components/motu/MotuDeviceCard.vue',
    'src/components/motu/MotuVolumeReadback.vue',
    'src/components/motu/MotuVolumeControl.vue',
    'src/components/motu/MotuDraftReceipt.vue',
    'src/components/motu/MotuWriteReceipt.vue',
    'src/components/motu/MotuErrorNotice.vue'
  ]
  const referenced = new Set<string>()
  for (const source of sources) {
    const text = read(source)
    for (const match of text.matchAll(/['"](motu\.[A-Za-z0-9_.]+)['"]/g)) {
      referenced.add(match[1])
    }
    for (const match of text.matchAll(/'(motu\.[A-Za-z0-9_]+)\.' \+/g)) {
      // 动态前缀（本区域不使用；显式失败以便人工处理）
      assert.fail(source + ' uses a dynamic motu key prefix: ' + match[1])
    }
  }
  assert.ok(referenced.size > 40, 'expected many MOTU key references, got ' + referenced.size)
  for (const key of referenced) {
    assert.ok(zhKeys.has(key), 'zh-CN shard misses referenced key ' + key)
    assert.ok(enKeys.has(key), 'en-US shard misses referenced key ' + key)
  }
})

test('VolumeView no longer renders the mode card (ModeView owns it) and wires the MOTU cards', () => {
  const view = read('src/views/VolumeView.vue')
  // mode 卡已移除：不再引用 volume.mode.*，也不再准备 mode（不碰共享 control store 行为）。
  assert.ok(!view.includes('volume.mode.'), 'VolumeView must not reference volume.mode.* anymore')
  assert.ok(!view.includes('prepareMode'), 'VolumeView must not prepare mode anymore')
  assert.ok(!view.includes('modeTarget'), 'VolumeView must not keep mode state')
  // MOTU 设备卡/回读/两阶段控制 + 5s 轮询 + 共享 DraftReview 都在页面上。
  assert.ok(view.includes('<MotuDeviceCard'), 'missing MOTU device card')
  assert.ok(view.includes('<MotuVolumeReadback'), 'missing MOTU readback card')
  assert.ok(view.includes('<MotuVolumeControl'), 'missing MOTU volume control card')
  assert.ok(view.includes('useMotuPolling'), 'missing MOTU polling composable')
  assert.ok(view.includes('<DraftReview'), 'per-lane volume draft must use the shared DraftReview')
  assert.ok(view.includes("motu.gainsSeparation"), 'must separate MOTU device volume from lane protection gain')
  // 原有能力保留：静音 + 逐 lane 两阶段 + 两阶段回执区。
  assert.ok(view.includes('setControlMute'), 'mute must stay wired')
  assert.ok(view.includes('prepareVolume'), 'per-lane two-phase volume must stay wired')
  assert.ok(view.includes('@refresh="onRefreshMotu"'), 'MOTU refresh must be wired')
  assert.ok(view.includes('void refreshMotu()'), 'header refresh must refresh MOTU too')
})

test('MOTU volume control only exposes 1/2/both (no >2 channel batch) and uses local cancel + TTL copy', () => {
  const control = read('src/components/motu/MotuVolumeControl.vue')
  assert.ok(control.includes(':value="1"'))
  assert.ok(control.includes(':value="2"'))
  assert.ok(control.includes('value="both"'))
  // 没有“取消/abort 请求”：取消是本地动作，文案明确服务端草稿按 TTL 过期。
  assert.ok(control.includes('cancelMotuVolumeDraft'))
  assert.ok(control.includes('motu.draft.cancelNote'), 'cancel note explains the server-side TTL')
  assert.ok(control.includes('localCancelText'), 'the local cancel notice must be rendered')
  const draft = read('src/components/motu/MotuDraftReceipt.vue')
  assert.ok(draft.includes('motu.draft.cancelNote'))
  assert.ok(draft.includes('motu.draft.noAbortNote'))
  assert.ok(draft.includes('writes_device'), 'draft must show writes_device honestly')
})

test('MOTU API module is PUT-only for the two stages and has no abort endpoint', () => {
  const api = read('src/api/motu.ts')
  assert.ok(api.includes("client.put<unknown>(MOTU_VOLUME_PATH, buildMotuVolumePrepareBody(selection, db))"))
  assert.ok(api.includes("client.put<unknown>(MOTU_VOLUME_PATH, buildMotuVolumeCommitBody(engineToken))"))
  assert.ok(!api.includes('/api/motu/abort'), 'the MOTU endpoint has no abort')
  assert.ok(!/abort\s*\(/.test(api), 'the MOTU API must not expose abort()')
  const store = read('src/stores/motu.ts')
  assert.ok(!store.includes('/api/motu/abort'), 'the MOTU store must not call an abort endpoint')
  // 轮询间隔 5s + visibilitychange 由 composable 接线。
  assert.ok(store.includes('MOTU_POLL_INTERVAL_MS = 5000'))
  const polling = read('src/components/motu/useMotuPolling.ts')
  assert.ok(polling.includes('visibilitychange'))
  assert.ok(polling.includes('onBeforeUnmount'))
  assert.ok(polling.includes('stopMotuPolling'))
})

test('MOTU error handling shows the stable codes / details / completed channels and never success on failure', () => {
  const notice = read('src/components/motu/MotuErrorNotice.vue')
  assert.ok(notice.includes('motu.error.completed'))
  assert.ok(notice.includes('motu.error.deviceCode'))
  assert.ok(notice.includes('motu.error.details'))
  assert.ok(notice.includes('motu.error.partial'))
  const api = read('src/api/motu.ts')
  assert.ok(api.includes('motu_not_detected'))
  assert.ok(api.includes('motu_control_unreachable'))
  assert.ok(api.includes('motu_control_disabled'))
  assert.ok(api.includes('motu_batch_rejected'))
  assert.ok(api.includes('engine_token_invalid'))
  const store = read('src/stores/motu.ts')
  assert.ok(store.includes("info.category === 'token_invalid' || info.draftConsumed === true"))
  assert.ok(store.includes('state.lastCommit = ack'), 'lastCommit only set on success')
})
