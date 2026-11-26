import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient } from '../src/api/client.ts'
import {
  META_PATH,
  capabilityReason,
  createMetaApi,
  opState,
  parseMeta,
  surfaceState
} from '../src/api/meta.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

const META = {
  api_version: '1',
  web_version: '0.3.0',
  ops: ['status', 'mute', 'prepare', 'commit', 'abort', 'coeff', 'surface', 'inspect', 'rollback'],
  surfaces: ['volume', 'routing', 'layout', 'format', 'mode'],
  calibration: { wired: false, reason: 'calibration job backend is not wired' },
  host_link: { target: 'rt-host', mode: 'socket', connected: true, socket: '/run/khaen/host.sock' },
  not_wired: [
    { op: 'calibration.jobs', reason: 'calibration job backend is not wired' }
  ]
}

test('parseMeta reads the real capability document (no ok field required)', () => {
  const meta = parseMeta(META)
  assert.equal(meta.apiVersion, '1')
  assert.equal(meta.webVersion, '0.3.0')
  assert.deepEqual(meta.surfaces, ['volume', 'routing', 'layout', 'format', 'mode'])
  assert.deepEqual(meta.surfaceStates, {})
  assert.equal(meta.calibration.wired, false)
  assert.equal(meta.calibration.reason, META.calibration.reason)
  assert.equal(meta.hostLink?.connected, true)
  assert.equal(meta.notWired.length, 1)
  assert.equal(meta.notWired[0].op, 'calibration.jobs')
  assert.equal(meta.notWired[0].surface, null)
  assert.equal(meta.notWired[0].reason, META.not_wired[0].reason)
  for (const surface of ['volume', 'routing', 'layout', 'format', 'mode']) {
    assert.equal(surfaceState(meta, surface), 'wired', surface)
  }
  assert.equal(opState(meta, 'surface'), 'wired')
  assert.equal(opState(meta, 'calibration.jobs'), 'not_wired')
})

test('parseMeta accepts the object surfaces shape and surface-scoped not_wired entries', () => {
  const meta = parseMeta({
    surfaces: { volume: true, routing: false, layout: true },
    ops: [],
    not_wired: [{ surface: 'routing', reason: 'plan graph editor is not wired' }]
  })
  assert.deepEqual(meta.surfaces, ['volume', 'layout'])
  assert.equal(meta.surfaceStates.routing, false)
  assert.equal(surfaceState(meta, 'routing'), 'not_wired')
  assert.equal(surfaceState(meta, 'layout'), 'wired')
  assert.equal(surfaceState(meta, 'mode'), 'unknown')
  assert.equal(capabilityReason(meta, { surface: 'routing' }), 'plan graph editor is not wired')
  assert.equal(capabilityReason(meta, { op: 'surface' }), null)
})

test('capability states are unknown (not wired) when the snapshot is missing', () => {
  assert.equal(surfaceState(null, 'volume'), 'unknown')
  assert.equal(opState(null, 'surface'), 'unknown')
  assert.equal(capabilityReason(null, { surface: 'volume' }), null)
})

test('parseMeta rejects type drift instead of treating it as full capability', () => {
  assert.throws(() => parseMeta(null), /body must be an object/)
  assert.throws(() => parseMeta({ surfaces: [1] }), /surfaces entries must be strings/)
  assert.throws(() => parseMeta({ surfaces: { volume: 'yes' } }), /must be a boolean/)
  assert.throws(() => parseMeta({ not_wired: [{ reason: 'no key' }] }), /must carry op or surface/)
  assert.throws(() => parseMeta({ ops: 'status' }), /ops must be an array/)
})

test('meta API fetches GET /api/meta with credentials and no CSRF', async () => {
  const recording = createRecordingFetch(() => jsonResponse(200, META))
  const client = new ApiClient({ baseUrl: 'http://mock.local', fetchImpl: recording.fetch, csrfToken: () => 'csrf-test' })
  const api = createMetaApi(client)
  const meta = await api.fetch()
  assert.equal(meta.apiVersion, '1')
  assert.equal(recording.calls.length, 1)
  const call = recording.calls[0]
  assert.equal(String(call.url), 'http://mock.local' + META_PATH)
  assert.equal(call.method, 'GET')
  assert.equal(call.credentials, 'include')
  assert.equal(call.headers['X-KHAEN-CSRF'], undefined)
})
