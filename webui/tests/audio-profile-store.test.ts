import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiError } from '../src/api/client.ts'
import {
  createDefaultProfile,
  applyLayoutPreset,
  type AudioProfileApi,
  type AudioProfileSnapshot,
  type DeviceInputsCapability
} from '../src/api/audioProfile.ts'
import {
  createAudioProfileController,
  createInitialAudioProfileState,
  editingProfile,
  mergedValidation,
  speakerTestRemainingMs
} from '../src/stores/audioProfile.ts'

const SNAPSHOT: AudioProfileSnapshot = {
  schema: 'khaen-audio-profile/v1',
  profile: createDefaultProfile(),
  validation: { ok: true, errors: [], warnings: [] },
  capability: {
    aes67: { channels: 2, map: [0, 1], channel_order: 'M', channel_order_source: 'sdp' },
    device_inputs: { available: true, optical_mode: 'spdif', sample_rate: 48000, channels: [], source: 'motu-alsa', alsa_capture: null, reason: null }
  },
  plan: { lanes: 2, enabled_outputs: 2, dac_channels: 22, graph_id: 'graph-1' },
  reasons: {}
}

const DEVICE_INPUTS: DeviceInputsCapability = {
  available: false,
  optical_mode: null,
  sample_rate: null,
  channels: null,
  source: null,
  alsa_capture: null,
  reason: 'not wired'
}

interface Calls {
  order: string[]
}

function createFakeApi(calls: Calls, overrides: Partial<AudioProfileApi> = {}): AudioProfileApi {
  const baseApi: AudioProfileApi = {
    fetchProfile: async () => {
      calls.order.push('profile')
      return SNAPSHOT
    },
    fetchDeviceInputs: async () => {
      calls.order.push('inputs')
      return DEVICE_INPUTS
    },
    fetchDeviceOutputs: async () => {
      calls.order.push('outputs')
      return { devices: [], reason: null }
    },
    prepareProfile: async (profile) => {
      calls.order.push('prepare')
      return {
        stage: 'prepare',
        token: 'draft-token',
        profile,
        validation: { ok: true, errors: [], warnings: [] },
        diff: { outputs_changed: true, inputs_changed: false, downmix_changed: false, fanout_changed: false },
        expires_in_s: 60
      }
    },
    commitProfile: async () => {
      calls.order.push('commit')
      return { ok: true, applied: true, saved: true, revision: 2, profile: null }
    },
    abortProfile: async () => {
      calls.order.push('abort')
      return { ok: true, aborted: true }
    },
    generatePlan: async () => {
      calls.order.push('plan')
      return {
        ok: true,
        plan_path: '/p.bin',
        sha256: 'c'.repeat(64),
        lane_count: 6,
        inspect: null,
        folds_preview: [],
        downmix_applied: []
      }
    },
    inspectPlan: async () => {
      calls.order.push('inspect')
      return { ok: true }
    },
    preparePlan: async () => {
      calls.order.push('plan-prepare')
      return { engine_token: 'engine-token', planned_version: 3, graph_id: 'g' }
    },
    commitPlan: async () => {
      calls.order.push('plan-commit')
      return { version: 3, graph_id: 'g', applied_block: 11 }
    },
    abortPlan: async () => {
      calls.order.push('plan-abort')
      return { ok: true, aborted: true }
    },
    startSpeakerTest: async () => {
      calls.order.push('test')
      return { ok: true, dac: 2, ms: 1000, kind: 'noise', level_db: -30, remaining_ms: 1000 }
    },
    stopSpeakerTest: async () => {
      calls.order.push('stop')
      return { ok: true, stopped: true }
    }
  }
  return { ...baseApi, ...overrides }
}

test('profile controller loads a snapshot and degrades honestly on failure', async () => {
  const calls: Calls = { order: [] }
  const state = createInitialAudioProfileState()
  const controller = createAudioProfileController(state, { api: createFakeApi(calls), now: () => 100 })
  const snapshot = await controller.loadProfile()
  assert.equal(snapshot?.plan.graph_id, 'graph-1')
  assert.equal(state.profilePhase, 'ready')
  assert.equal(state.profileLoadedAt, 100)
  // 缓存：再读不重复请求
  await controller.loadProfile()
  assert.equal(calls.order.filter((item) => item === 'profile').length, 1)
  // 失败保留上次快照
  const failing = createAudioProfileController(state, {
    api: createFakeApi(calls, {
      fetchProfile: async () => {
        throw new ApiError({ status: 503, code: 'not_wired', message: 'not wired' })
      }
    })
  })
  const result = await failing.loadProfile(true)
  assert.equal(result, null)
  assert.equal(state.profilePhase, 'error')
  assert.equal(state.profileError?.category, 'not_wired')
  assert.equal(state.snapshot?.plan.graph_id, 'graph-1')
})

test('device outputs load once and cache the real list', async () => {
  const calls: Calls = { order: [] }
  const state = createInitialAudioProfileState()
  const controller = createAudioProfileController(state, { api: createFakeApi(calls) })
  const outputs = await controller.loadDeviceOutputs()
  assert.deepEqual(outputs?.devices, [])
  assert.equal(outputs?.reason, null)
  assert.equal(state.deviceOutputsPhase, 'ready')
  await controller.loadDeviceOutputs()
  assert.equal(calls.order.filter((item) => item === 'outputs').length, 1)
})

test('two-step save: prepare -> draft, commit applies and clears the draft', async () => {
  const calls: Calls = { order: [] }
  const state = createInitialAudioProfileState()
  const controller = createAudioProfileController(state, { api: createFakeApi(calls), now: () => 1 })
  const draft = await controller.prepare(applyLayoutPreset(createDefaultProfile(), '5.1'))
  assert.equal(draft?.token, 'draft-token')
  assert.equal(state.draft?.diff.outputs_changed, true)
  const ack = await controller.commit()
  assert.equal(ack?.revision, 2)
  assert.equal(state.draft, null)
  assert.equal(state.lastCommit?.saved, true)
  assert.deepEqual(calls.order, ['prepare', 'commit'])
})

test('prepare failure clears the previous draft; abort discards it', async () => {
  const calls: Calls = { order: [] }
  const state = createInitialAudioProfileState()
  const controller = createAudioProfileController(state, { api: createFakeApi(calls), now: () => 1 })
  await controller.prepare(createDefaultProfile())
  assert.notEqual(state.draft, null)
  const failing = createAudioProfileController(state, {
    api: createFakeApi(calls, {
      prepareProfile: async () => {
        throw new ApiError({ status: 400, code: 'bad_request', message: 'bad profile' })
      }
    })
  })
  const failed = await failing.prepare(createDefaultProfile())
  assert.equal(failed, null)
  assert.equal(state.draft, null)
  assert.equal(state.error?.category, 'validation')

  await controller.prepare(createDefaultProfile())
  assert.equal(await controller.abort(), true)
  assert.equal(state.draft, null)
})

test('apply chain runs plan -> inspect -> prepare -> commit and records every step', async () => {
  const calls: Calls = { order: [] }
  const state = createInitialAudioProfileState()
  const controller = createAudioProfileController(state, { api: createFakeApi(calls), now: () => 7 })
  const outcome = await controller.apply()
  assert.equal(outcome.ok, true)
  assert.deepEqual(calls.order, ['plan', 'inspect', 'plan-prepare', 'plan-commit'])
  assert.deepEqual(outcome.steps.map((step) => step.step), ['plan', 'inspect', 'prepare', 'commit'])
  assert.equal(outcome.steps.every((step) => step.ok), true)
  assert.equal(outcome.commit?.applied_block, 11)
})

test('apply stops at the first failing step and never pretends success', async () => {
  const calls: Calls = { order: [] }
  const state = createInitialAudioProfileState()
  const controller = createAudioProfileController(state, {
    api: createFakeApi(calls, {
      inspectPlan: async () => {
        calls.order.push('inspect')
        throw new ApiError({ status: 409, code: 'admission_rejected', message: 'rejected' })
      }
    }),
    now: () => 9
  })
  const outcome = await controller.apply()
  assert.equal(outcome.ok, false)
  assert.deepEqual(calls.order, ['plan', 'inspect'])
  assert.deepEqual(outcome.steps.map((step) => [step.step, step.ok]), [['plan', true], ['inspect', false]])
  assert.equal(outcome.error?.code, 'admission_rejected')
  // 失败时没有遗留 plan 草稿；abortApply 无草稿返回 false
  assert.equal(await controller.abortApply(), false)
})

test('speaker test requires explicit confirm/audible and stores a local countdown state', async () => {
  const calls: Calls = { order: [] }
  const state = createInitialAudioProfileState()
  const controller = createAudioProfileController(state, { api: createFakeApi(calls), now: () => 1000 })
  const refused = await controller.startTest({ dac: 0, ms: 1000, kind: 'noise', db: -30, audible: false as never, confirm: true })
  assert.equal(refused, null)
  assert.equal(state.error?.category, 'validation')
  assert.deepEqual(calls.order, [])

  const ack = await controller.startTest({ dac: 2, ms: 1000, kind: 'noise', db: -30, audible: true, confirm: true })
  assert.equal(ack?.dac, 2)
  assert.equal(state.test?.active, true)
  assert.equal(state.test?.startedAt, 1000)
  assert.equal(speakerTestRemainingMs(state.test, 1250), 750)
  assert.equal(speakerTestRemainingMs(state.test, 4000), 0)
  assert.equal(await controller.stopTest(), true)
  assert.equal(state.test, null)
  assert.deepEqual(calls.order, ['test', 'stop'])
})

test('busy mutex serializes profile writes', async () => {
  const calls: Calls = { order: [] }
  const state = createInitialAudioProfileState()
  let release: (() => void) | null = null
  const controller = createAudioProfileController(state, {
    api: createFakeApi(calls, {
      prepareProfile: async (profile) => {
        await new Promise<void>((resolve) => {
          release = resolve
        })
        return {
          stage: 'prepare',
          token: 't',
          profile,
          validation: { ok: true, errors: [], warnings: [] },
          diff: { outputs_changed: false, inputs_changed: false, downmix_changed: false, fanout_changed: false },
          expires_in_s: null
        }
      }
    })
  })
  const first = controller.prepare(createDefaultProfile())
  const second = await controller.prepare(createDefaultProfile())
  assert.equal(second, null)
  assert.equal(state.busy, 'prepare')
  release?.()
  await first
  assert.equal(state.busy, null)
})

test('editingProfile falls back to a local 2.0 draft and mergedValidation unions issues', () => {
  const local = editingProfile(null)
  assert.equal(local.outputs.length, 22)
  assert.equal(editingProfile(SNAPSHOT).outputs[0].role, 'L')

  const draft = {
    stage: 'prepare' as const,
    token: 't',
    profile: applyLayoutPreset(createDefaultProfile(), '2.0'),
    validation: {
      ok: false,
      errors: [{ code: 'server_error', path: 'outputs', message: 'server said no' }],
      warnings: []
    },
    diff: { outputs_changed: true, inputs_changed: false, downmix_changed: false, fanout_changed: false },
    expires_in_s: 60
  }
  const merged = mergedValidation(draft)
  assert.equal(merged.ok, false)
  assert.equal(merged.errors.some((issue) => issue.code === 'server_error'), true)
  assert.equal(merged.warnings.some((issue) => issue.code === 'missing_center'), true)
})
