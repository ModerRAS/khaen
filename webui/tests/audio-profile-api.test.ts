import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiClient } from '../src/api/client.ts'
import {
  AES67_PRESETS,
  AUDIO_PROFILE_PATHS,
  AUDIO_PROFILE_SCHEMA,
  DEFAULT_DEVICE_CHANNELS,
  LAYOUT_PRESETS,
  SPEAKER_ROLES,
  applyInputPreset,
  applyLayoutPreset,
  buildDownmixHints,
  buildRoleFanout,
  classifyAudioProfileError,
  createAudioProfileApi,
  createDefaultProfile,
  deriveLayoutLabel,
  enabledOutputs,
  parseAudioProfileSnapshot,
  parseDeviceOutputs,
  parseGeneratedPlan,
  parsePrepareResponse,
  parseSpeakerTestAck,
  summarizeRoleGroups,
  unusedDacs,
  validateProfileLocally,
  validateSpeakerTestRequest
} from '../src/api/audioProfile.ts'
import { ApiError } from '../src/api/client.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

function createClient(handler: (call: any, index: number) => Response, csrf = 'csrf-test') {
  const recording = createRecordingFetch(handler)
  const client = new ApiClient({ baseUrl: 'http://mock.local', fetchImpl: recording.fetch, csrfToken: () => csrf })
  return { client, calls: recording.calls, api: createAudioProfileApi(client) }
}

test('default profile is 2.0 with AES67 input and 22 disabled outputs', () => {
  const profile = createDefaultProfile()
  assert.equal(profile.schema, AUDIO_PROFILE_SCHEMA)
  assert.equal(profile.device.channels, DEFAULT_DEVICE_CHANNELS)
  assert.equal(profile.outputs.length, 22)
  assert.deepEqual(enabledOutputs(profile).map((output) => [output.dac, output.role]), [[0, 'L'], [1, 'R']])
  assert.deepEqual(unusedDacs(profile), Array.from({ length: 20 }, (_, index) => index + 2))
  assert.equal(profile.inputs.sources[0].kind, 'aes67')
  assert.equal(profile.inputs.sources[0].enabled, true)
  assert.deepEqual(profile.inputs.sources[0].channels.map((channel) => channel.role), ['L', 'R'])
  assert.equal(profile.inputs.sources[1].enabled, false)
  assert.equal(validateProfileLocally(profile).ok, true)
})

test('local validation rejects only structural contradictions (duplicates are legal)', () => {
  const duplicates = createDefaultProfile()
  duplicates.outputs[2] = { dac: 2, enabled: true, role: 'L', gain_db: 0, custom_label: null }
  const duplicateValidation = validateProfileLocally(duplicates)
  assert.equal(duplicateValidation.ok, true)
  assert.equal(duplicateValidation.errors.length, 0)
  assert.equal(duplicates.outputs.filter((output) => output.enabled && output.role === 'L').length, 2)

  const noneEnabled = createDefaultProfile()
  noneEnabled.outputs = noneEnabled.outputs.map((output) => ({ ...output, enabled: false, role: 'None' as const }))
  assert.equal(validateProfileLocally(noneEnabled).ok, false)
  assert.equal(validateProfileLocally(noneEnabled).errors[0].code, 'no_enabled_outputs')

  const enabledNone = createDefaultProfile()
  enabledNone.outputs[0] = { dac: 0, enabled: true, role: 'None', gain_db: 0, custom_label: null }
  assert.equal(validateProfileLocally(enabledNone).errors[0].code, 'enabled_without_role')

  const custom = createDefaultProfile()
  custom.outputs[2] = { dac: 2, enabled: true, role: 'Custom', gain_db: 0, custom_label: null }
  assert.equal(validateProfileLocally(custom).errors.some((issue) => issue.code === 'custom_without_label'), true)

  const outOfRange = createDefaultProfile()
  outOfRange.outputs[0] = { ...outOfRange.outputs[0], dac: 99 }
  assert.equal(validateProfileLocally(outOfRange).errors[0].code, 'dac_out_of_range')
})

test('missing C / LFE produce informational downmix hints, not errors', () => {
  const profile = applyLayoutPreset(createDefaultProfile(), '2.0')
  const validation = validateProfileLocally(profile)
  assert.equal(validation.ok, true)
  assert.equal(validation.warnings.some((issue) => issue.code === 'missing_center'), true)
  assert.equal(validation.warnings.some((issue) => issue.code === 'missing_lfe'), true)
  const hints = buildDownmixHints(profile)
  assert.equal(hints.missingCenter, true)
  assert.equal(hints.missingLfe, true)
  assert.equal(hints.centerGainDb, -3)
  assert.equal(hints.lfeGainDb, -3)
  assert.equal(hints.includeHeights, false)
  assert.equal(hints.lfeTargetCount, 2)
  assert.deepEqual(hints.lfeTargetDacs, [0, 1])
})

test('layout presets fill outputs and derive a stable label', () => {
  for (const preset of LAYOUT_PRESETS) {
    const profile = applyLayoutPreset(createDefaultProfile(), preset.id)
    assert.deepEqual(enabledOutputs(profile).map((output) => output.role), preset.roles, preset.id)
    assert.equal(deriveLayoutLabel(profile), preset.id)
    assert.equal(validateProfileLocally(profile).ok, true, preset.id)
  }
  const custom = createDefaultProfile()
  custom.outputs[2] = { dac: 2, enabled: true, role: 'L', gain_db: 0, custom_label: null }
  assert.match(deriveLayoutLabel(custom), /^custom:/)
})

test('role groups keep duplicates and the fanout preview maps roles to DACs', () => {
  const profile = applyLayoutPreset(createDefaultProfile(), '5.1')
  profile.outputs[2] = { dac: 2, enabled: true, role: 'L', gain_db: 0, custom_label: null } // L ×2
  const groups = summarizeRoleGroups(profile)
  const left = groups.find((group) => group.role === 'L')
  assert.deepEqual(left, { role: 'L', count: 2, dacs: [0, 2] })
  const fanout = buildRoleFanout(profile)
  const leftRow = fanout.find((row) => row.role === 'L')
  assert.deepEqual(leftRow?.inputs, [{ source: 'aes67', channel: 0 }])
  assert.deepEqual(leftRow?.dacs, [0, 2])
})

test('AES67 presets fill per-channel input roles', () => {
  const source = createDefaultProfile().inputs.sources[0]
  const surround = applyInputPreset(source, '5.1')
  assert.deepEqual(surround.channels.map((channel) => channel.role), ['L', 'R'])
  assert.equal(surround.preset, '5.1')
  const stereo = AES67_PRESETS.find((preset) => preset.id === 'stereo')
  assert.deepEqual(stereo?.roles, ['L', 'R'])
})

test('parseAudioProfileSnapshot degrades honestly when the backend is not wired', () => {
  const snapshot = parseAudioProfileSnapshot({
    ok: false,
    schema: null,
    profile: null,
    validation: null,
    capability: null,
    plan: null,
    reasons: { profile: 'audio profile store is not wired yet' }
  })
  assert.equal(snapshot.profile, null)
  assert.equal(snapshot.capability.device_inputs.available, null)
  assert.equal(snapshot.reasons.profile, 'audio profile store is not wired yet')
  assert.equal(snapshot.validation.ok, true)
})

test('parseAudioProfileSnapshot normalizes a full document', () => {
  const snapshot = parseAudioProfileSnapshot({
    ok: true,
    schema: AUDIO_PROFILE_SCHEMA,
    profile: createDefaultProfile(),
    validation: { ok: true, errors: [], warnings: ['missing center'] },
    capability: {
      aes67: { channels: 2, map: [0, 1], channel_order: 'M', channel_order_source: 'sdp' },
      device_inputs: {
        available: true,
        optical_mode: 'adat',
        sample_rate: 48000,
        channels: [{ index: 0, label: 'Analog 1', kind: 'analog' }, { index: 1, label: null, kind: 'optical' }],
        source: 'motu-alsa',
        alsa_capture: [
          { name: 'hw:CARD=UltraLitemk5,DEV=0', description: 'UltraLite-mk5', card: 0, device: 0, channels_min: 2, channels_max: 18, rate_min: 44100, rate_max: 96000, open_error: null },
          { name: 'default', description: null, card: null, device: null, channels_min: null, channels_max: null, rate_min: null, rate_max: null, open_error: 'Device or resource busy' }
        ]
      }
    },
    plan: { lanes: 2, enabled_outputs: 2, dac_channels: 22, graph_id: 'graph-1' }
  })
  assert.equal(snapshot.profile?.inputs.channels, 22)
  assert.equal(snapshot.profile?.inputs.sources[0].kind, 'aes67')
  assert.equal(snapshot.capability.aes67.channel_order_source, 'sdp')
  assert.equal(snapshot.capability.device_inputs.optical_mode, 'adat')
  assert.deepEqual(snapshot.capability.device_inputs.channels?.map((channel) => channel.kind), ['analog', 'optical'])
  assert.deepEqual(snapshot.capability.device_inputs.alsa_capture?.map((device) => device.name), [
    'hw:CARD=UltraLitemk5,DEV=0',
    'default'
  ])
  assert.equal(snapshot.capability.device_inputs.alsa_capture?.[0].channels_max, 18)
  assert.equal(snapshot.capability.device_inputs.alsa_capture?.[0].rate_min, 44100)
  assert.equal(snapshot.capability.device_inputs.alsa_capture?.[0].rate_max, 96000)
  assert.equal(snapshot.capability.device_inputs.alsa_capture?.[1].rate_min, null)
  assert.equal(snapshot.capability.device_inputs.alsa_capture?.[1].open_error, 'Device or resource busy')
  assert.equal(snapshot.plan.lanes, 2)
  assert.deepEqual(snapshot.validation.warnings.map((issue) => issue.message), ['missing center'])
})

test('parsePrepareResponse/parseGeneratedPlan/parseSpeakerTestAck validate the receipts', () => {
  const draft = parsePrepareResponse({
    ok: true,
    stage: 'prepare',
    profile: createDefaultProfile(),
    validation: { ok: true, errors: [], warnings: [] },
    diff: { outputs_changed: true, inputs_changed: false, downmix_changed: false, fanout_changed: false },
    token: 'profile-token',
    expires_in_s: 60
  })
  assert.equal(draft.token, 'profile-token')
  assert.equal(draft.diff.outputs_changed, true)
  assert.equal(draft.expires_in_s, 60)

  const plan = parseGeneratedPlan({
    ok: true,
    plan_path: '/var/lib/khaen/plans/p.bin',
    sha256: 'a'.repeat(64),
    lane_count: 6,
    inspect: { ok: true },
    folds_preview: [{ source: 'aes67', channel: 0, role: 'L', lane: 0 }],
    downmix_applied: [{ kind: 'center', gain_db: -3, lanes: [0, 1] }]
  })
  assert.equal(plan.plan_path, '/var/lib/khaen/plans/p.bin')
  assert.equal(plan.lane_count, 6)
  assert.deepEqual(plan.folds_preview[0], { source: 'aes67', channel: 0, role: 'L', lane: 0 })
  assert.deepEqual(plan.downmix_applied[0], { kind: 'center', gain_db: -3, lanes: [0, 1] })

  const ack = parseSpeakerTestAck({ ok: true, dac: 3, ms: 1000, kind: 'noise', level_db: -30, remaining_ms: 1000 })
  assert.equal(ack.dac, 3)
  assert.equal(ack.kind, 'noise')
  assert.equal(ack.level_db, -30)
})

test('speaker test request validation enforces confirm/audible, DAC, duration and level', () => {
  const valid = { dac: 0, ms: 1000, kind: 'noise' as const, db: -30, audible: true as const, confirm: true as const }
  assert.equal(validateSpeakerTestRequest(valid), null)
  assert.equal(validateSpeakerTestRequest({ ...valid, audible: false as never }), 'confirm')
  assert.equal(validateSpeakerTestRequest({ ...valid, confirm: false as never }), 'confirm')
  assert.equal(validateSpeakerTestRequest({ ...valid, dac: -1 }), 'dac')
  assert.equal(validateSpeakerTestRequest({ ...valid, dac: 1.5 }), 'dac')
  assert.equal(validateSpeakerTestRequest({ ...valid, ms: 100 }), 'ms')
  assert.equal(validateSpeakerTestRequest({ ...valid, ms: 20000 }), 'ms')
  assert.equal(validateSpeakerTestRequest({ ...valid, db: -5 }), 'db')
  assert.equal(validateSpeakerTestRequest({ ...valid, db: -61 }), 'db')
  assert.equal(SPEAKER_ROLES.includes('None'), true)
})

test('audio profile API uses the documented paths, methods and bodies', async () => {
  const bodies: unknown[] = []
  const methods: string[] = []
  const paths: string[] = []
  const { api } = createClient((call) => {
    methods.push(call.method)
    paths.push(String(call.url).replace('http://mock.local', ''))
    bodies.push(call.body === undefined ? undefined : JSON.parse(String(call.body)))
    const path = String(call.url).replace('http://mock.local', '')
    if (path === AUDIO_PROFILE_PATHS.profile && call.method === 'GET') {
      return jsonResponse(200, { ok: true, schema: AUDIO_PROFILE_SCHEMA, profile: createDefaultProfile(), validation: { ok: true }, capability: {}, plan: {} })
    }
    if (path === AUDIO_PROFILE_PATHS.profile) {
      return jsonResponse(200, { ok: true, stage: 'prepare', profile: createDefaultProfile(), validation: { ok: true }, diff: {}, token: 'token-1' })
    }
    if (path === AUDIO_PROFILE_PATHS.deviceInputs) {
      return jsonResponse(200, { ok: true, available: false, reason: 'no device' })
    }
    if (path === AUDIO_PROFILE_PATHS.deviceOutputs) {
      return jsonResponse(200, { ok: true, alsa_playback: [], reason: null })
    }
    if (path === AUDIO_PROFILE_PATHS.plan) {
      return jsonResponse(200, { ok: true, plan_path: '/p.bin', sha256: 'b'.repeat(64), lane_count: 2 })
    }
    if (path === AUDIO_PROFILE_PATHS.planInspect) {
      return jsonResponse(200, { ok: true })
    }
    if (path === AUDIO_PROFILE_PATHS.planPrepare) {
      return jsonResponse(200, { ok: true, engine_token: 'engine-token', planned_version: 3, graph_id: 'g' })
    }
    if (path === AUDIO_PROFILE_PATHS.planCommit) {
      return jsonResponse(200, { ok: true, version: 3, graph_id: 'g', applied_block: 10 })
    }
    if (path === AUDIO_PROFILE_PATHS.planAbort) {
      return jsonResponse(200, { ok: true, aborted: true })
    }
    if (path === AUDIO_PROFILE_PATHS.speakerTest) {
      return jsonResponse(200, { ok: true, dac: 2, ms: 1000, kind: 'noise', level_db: -30, remaining_ms: 1000 })
    }
    if (path === AUDIO_PROFILE_PATHS.speakerTestStop) {
      return jsonResponse(200, { ok: true, stopped: true })
    }
    throw new Error('unexpected request ' + call.method + ' ' + path)
  })
  const profile = createDefaultProfile()
  await api.fetchProfile()
  await api.fetchDeviceInputs()
  await api.prepareProfile(profile)
  await api.commitProfile('token-1')
  await api.abortProfile('token-1')
  await api.generatePlan()
  await api.inspectPlan('/p.bin', 'b'.repeat(64))
  await api.preparePlan('/p.bin', 'b'.repeat(64))
  await api.commitPlan('engine-token')
  await api.abortPlan('engine-token')
  await api.startSpeakerTest({ dac: 2, ms: 1000, kind: 'noise', db: -30, audible: true, confirm: true })
  await api.stopSpeakerTest()
  await api.fetchDeviceOutputs()

  assert.deepEqual(paths, [
    AUDIO_PROFILE_PATHS.profile,
    AUDIO_PROFILE_PATHS.deviceInputs,
    AUDIO_PROFILE_PATHS.profile,
    AUDIO_PROFILE_PATHS.profile,
    AUDIO_PROFILE_PATHS.profile,
    AUDIO_PROFILE_PATHS.plan,
    AUDIO_PROFILE_PATHS.planInspect,
    AUDIO_PROFILE_PATHS.planPrepare,
    AUDIO_PROFILE_PATHS.planCommit,
    AUDIO_PROFILE_PATHS.planAbort,
    AUDIO_PROFILE_PATHS.speakerTest,
    AUDIO_PROFILE_PATHS.speakerTestStop,
    AUDIO_PROFILE_PATHS.deviceOutputs
  ])
  assert.deepEqual(methods.filter((method) => method === 'PUT').length, 3)
  // PUT prepare/commit/abort body 形状
  assert.equal((bodies[2] as any).stage, 'prepare')
  assert.equal((bodies[2] as any).profile.schema, AUDIO_PROFILE_SCHEMA)
  assert.deepEqual(bodies[3], { stage: 'commit', token: 'token-1' })
  assert.deepEqual(bodies[4], { stage: 'abort', token: 'token-1' })
  // 方案链路
  assert.deepEqual(bodies[5], {})
  assert.deepEqual(bodies[6], { plan_path: '/p.bin', plan_sha256: 'b'.repeat(64) })
  assert.deepEqual(bodies[7], { plan_path: '/p.bin', plan_sha256: 'b'.repeat(64) })
  assert.deepEqual(bodies[8], { engine_token: 'engine-token' })
  assert.deepEqual(bodies[9], { engine_token: 'engine-token' })
  // 测试音必须显式 audible/confirm
  const testBody = bodies[10] as Record<string, unknown>
  assert.equal(testBody.audible, true)
  assert.equal(testBody.confirm, true)
  assert.equal(testBody.dac, 2)
  assert.equal(testBody.db, -30)
  assert.deepEqual(bodies[11], {})
})

test('parseDeviceOutputs keeps the real playback list and reports unavailable as null + reason', () => {
  const list = parseDeviceOutputs({
    ok: true,
    alsa_playback: [
      { name: 'hw:CARD=UltraLitemk5,DEV=0', description: 'UltraLite-mk5', card: 0, device: 0, channels_min: 2, channels_max: 22, rate_min: 44100, rate_max: 96000, open_error: null }
    ],
    reason: null
  })
  assert.equal(list.devices?.length, 1)
  assert.equal(list.devices?.[0].channels_max, 22)
  assert.equal(list.devices?.[0].rate_max, 96000)
  assert.equal(list.reason, null)
  const unavailable = parseDeviceOutputs({ ok: true, alsa_playback: null, reason: 'ALSA playback enumeration is Linux-only' })
  assert.equal(unavailable.devices, null)
  assert.equal(unavailable.reason, 'ALSA playback enumeration is Linux-only')
  assert.throws(() => parseDeviceOutputs(null), /invalid_response|unexpected/)
})

test('audio profile errors are classified into stable UI categories', () => {
  const cases: Array<[ApiError, string]> = [
    [new ApiError({ status: 503, code: 'not_wired', message: 'not wired' }), 'not_wired'],
    [new ApiError({ status: 501, code: 'unsupported', message: 'nope' }), 'unsupported'],
    [new ApiError({ status: 409, code: 'busy', message: 'busy' }), 'busy'],
    [new ApiError({ status: 403, code: 'sound_window_closed', message: 'window' }), 'sound_window_closed'],
    [new ApiError({ status: 400, code: 'bad_request', message: 'bad' }), 'validation'],
    [new ApiError({ status: 401, code: 'not_authenticated', message: 'auth' }), 'unauthorized'],
    [new ApiError({ status: 0, code: 'network_error', message: 'down' }), 'network'],
    [new ApiError({ status: 200, code: 'invalid_response', message: 'bad json' }), 'invalid_response'],
    [new ApiError({ status: 500, code: 'internal_error', message: 'boom' }), 'server']
  ]
  for (const [error, category] of cases) {
    const info = classifyAudioProfileError(error)
    assert.equal(info.category, category, error.code)
    assert.match(info.i18nKey, /^speakerLayout\.error\./)
  }
})
