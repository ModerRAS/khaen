import { test } from 'node:test'
import assert from 'node:assert/strict'
import { parseChannelsInfo } from '../src/api/channels.ts'
import { aes67FromChannels } from '../src/api/formatAes67.ts'
import { buildChannelsSeed } from '../scripts/channels-fixtures.mjs'

test('aes67FromChannels uses channels.current and ignores a missing status aes67', () => {
  const info = parseChannelsInfo(buildChannelsSeed('wired').channels)
  const view = aes67FromChannels(info, 'channels down')
  assert.equal(view.unavailable, false)
  assert.equal(view.fields.group, '239.69.1.1')
  assert.equal(view.fields.port, 5004)
  assert.equal(view.fields.pt, 98)
  assert.equal(view.fields.channels, 2)
  assert.deepEqual(view.fields.map, [0, 1])
  assert.equal(view.reasons.group, null)
  assert.equal(view.reasons.port, null)
  assert.equal(view.reasons.pt, null)
})

test('aes67FromChannels does not invent values when channels is unavailable', () => {
  const view = aes67FromChannels(null, 'channels unavailable')
  assert.equal(view.unavailable, true)
  assert.equal(view.fields.group, null)
  assert.equal(view.fields.port, null)
  assert.equal(view.fields.pt, null)
  assert.equal(view.fields.channels, null)
  assert.equal(view.fields.map, null)
  assert.equal(view.reasons.group, 'channels unavailable')
  assert.equal(view.reasons.port, 'channels unavailable')
})

test('aes67FromChannels keeps a per-field reason when current omits one AES67 field', () => {
  const doc = buildChannelsSeed('wired').channels
  doc.current.aes67_group = null
  doc.reasons['current.aes67_group'] = 'group was not reported'
  const info = parseChannelsInfo(doc)
  const view = aes67FromChannels(info, null)
  assert.equal(view.fields.group, null)
  assert.equal(view.fields.port, 5004)
  assert.equal(view.reasons.group, 'group was not reported')
  assert.equal(view.reasons.port, null)
})
