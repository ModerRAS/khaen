import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiError } from '../src/api/client.ts'
import {
  MIC_PROFILE_SCHEMA,
  type MicApi,
  type MicCalUploadResponse,
  type MicProfile,
  type MicProfileResponse,
  type MicStatusResponse
} from '../src/api/mic.ts'
import { createInitialMicState, createMicController } from '../src/stores/mic.ts'

const SHA = 'a'.repeat(64)
const PROFILE: MicProfile = {
  schema: MIC_PROFILE_SCHEMA,
  device: 'hw:CARD=U18dB,DEV=0',
  serial: '718-7696',
  orientation: '90deg',
  cal_sha256: SHA,
  cal_filename: 'umik1_718-7696_90deg.txt',
  cal_path: '/var/lib/khaen/calibration/mics/' + SHA + '.txt',
  parsed: { serno: '718-7696', sens_factor_db: -0.01, points: 1000 },
  updated_at: '2026-11-27T09:30:00Z'
}

function profileResponse(profile: MicProfile | null, reasons: string[] = []): MicProfileResponse {
  return { ok: true, schema: MIC_PROFILE_SCHEMA, profile, reasons }
}

const STATUS: MicStatusResponse = {
  ...profileResponse(PROFILE),
  rt_host: { available: false, reason: 'rt-host calibration bridge is not wired yet (C2)' }
}

const UPLOAD: MicCalUploadResponse = {
  ok: true,
  sha256: SHA,
  filename: 'umik.txt',
  bytes: 2048,
  serial: '718-7696',
  parsed: { serno: '718-7696', sens_factor_db: null, points: 512 }
}

interface Calls {
  order: string[]
}

function fakeApi(calls: Calls, overrides: Partial<MicApi> = {}): MicApi {
  const base: MicApi = {
    getProfile: async () => {
      calls.order.push('get')
      return profileResponse(null, ['no microphone profile saved yet'])
    },
    putProfile: async (update) => {
      calls.order.push('put:' + JSON.stringify(update))
      return profileResponse(PROFILE)
    },
    uploadCalibration: async (body, options) => {
      calls.order.push('upload:' + options?.filename)
      return UPLOAD
    },
    getStatus: async () => {
      calls.order.push('status')
      return STATUS
    }
  }
  return { ...base, ...overrides }
}

test('load exposes a missing profile with its reason instead of inventing one', async () => {
  const state = createInitialMicState()
  const calls: Calls = { order: [] }
  const controller = createMicController(state, { api: fakeApi(calls) })
  const result = await controller.load()
  assert.equal(result.ok, true)
  assert.equal(state.phase, 'ready')
  assert.equal(state.profile, null)
  assert.deepEqual(state.reasons, ['no microphone profile saved yet'])
  assert.deepEqual(calls.order, ['get'])
})

test('load failure keeps phase=error and classifies the API error', async () => {
  const state = createInitialMicState()
  const controller = createMicController(state, {
    api: fakeApi({ order: [] }, { getProfile: async () => { throw new ApiError({ status: 503, code: 'mic_store_unavailable', message: 'no state dir' }) } })
  })
  const result = await controller.load()
  assert.equal(result.ok, false)
  assert.equal(state.phase, 'error')
  assert.equal(state.error?.code, 'mic_store_unavailable')
  assert.equal(state.busy.load, false)
})

test('save requires the server to return the persisted profile', async () => {
  const state = createInitialMicState()
  const calls: Calls = { order: [] }
  const controller = createMicController(state, { api: fakeApi(calls) })
  const result = await controller.save({ serial: '718-7696', orientation: '90deg', cal_sha256: SHA })
  assert.equal(result.ok, true)
  assert.deepEqual(state.profile, PROFILE)
  assert.ok(calls.order[0]?.startsWith('put:'))
  assert.ok(calls.order[0]?.includes('718-7696'))
})

test('save rejects a null profile response instead of reporting success', async () => {
  const state = createInitialMicState()
  const controller = createMicController(state, {
    api: fakeApi({ order: [] }, { putProfile: async () => profileResponse(null, ['rejected']) })
  })
  const result = await controller.save({ cal_sha256: SHA })
  assert.equal(result.ok, false)
  assert.equal(state.profile, null)
  assert.equal(state.busy.save, false)
})

test('upload stores the server receipt and passes the filename hint', async () => {
  const state = createInitialMicState()
  const calls: Calls = { order: [] }
  const controller = createMicController(state, { api: fakeApi(calls) })
  const result = await controller.upload({ bytes: 'SERNO 718-7696\n20 0.0\n', filename: 'umik.txt' })
  assert.equal(result.ok, true)
  assert.deepEqual(state.upload, UPLOAD)
  assert.deepEqual(calls.order, ['upload:umik.txt'])
  assert.equal(state.busy.upload, false)
})

test('loadStatus keeps rt-host unavailability as data, not as an error', async () => {
  const state = createInitialMicState()
  const calls: Calls = { order: [] }
  const controller = createMicController(state, { api: fakeApi(calls) })
  const result = await controller.loadStatus()
  assert.equal(result.ok, true)
  assert.equal(state.status?.rt_host.available, false)
  assert.equal(state.status?.rt_host.reason, 'rt-host calibration bridge is not wired yet (C2)')
  assert.equal(state.statusError, null)
  assert.deepEqual(calls.order, ['status'])
})

test('loadStatus records a transport error separately from rt-host unavailable', async () => {
  const state = createInitialMicState()
  const controller = createMicController(state, {
    api: fakeApi({ order: [] }, { getStatus: async () => { throw new ApiError({ status: 0, code: 'network_error', message: 'offline' }) } })
  })
  const result = await controller.loadStatus()
  assert.equal(result.ok, false)
  assert.equal(state.status, null)
  assert.equal(state.statusError?.category, 'network')
  assert.equal(state.busy.status, false)
})
