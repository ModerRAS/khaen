import { test } from 'node:test'
import assert from 'node:assert/strict'
import type {
  ChannelsApi,
  ChannelsInfo,
  ChannelsPrepareAck,
  PlanHistory,
  PlanInspect,
  PlanUploadAck
} from '../src/api/channels.ts'
import type { ControlCommitAck, ControlDraft } from '../src/api/control.ts'
import { ApiError } from '../src/api/client.ts'
import { buildChannelsSeed } from '../scripts/channels-fixtures.mjs'
import { createChannelsController, createInitialChannelsState } from '../src/stores/channels.ts'

function planDraft(overrides: Partial<ControlDraft> = {}): ControlDraft {
  return {
    ok: true,
    surface: 'plan',
    engine_token: 'plan-token',
    planned_version: 8,
    changed: null,
    graph_id: 'b'.repeat(64),
    rolled_back_to: null,
    mode: null,
    format: null,
    device_restart_required: null,
    applies_to: null,
    note: null,
    ...overrides
  }
}

function commitAck(overrides: Partial<ControlCommitAck> = {}): ControlCommitAck {
  return {
    ok: true,
    version: 8,
    applied_block: 1025,
    changed: null,
    all_ack: true,
    graph_id: 'b'.repeat(64),
    mode: null,
    format: null,
    device_restart_required: null,
    graph_dropped: null,
    history_cleared: null,
    mute_preserved: null,
    note: null,
    ...overrides
  }
}

function uploadAck(sha: string): PlanUploadAck {
  // daemon 真实上传回执形态：没有 upload_id/version，segments 是区段名数组 + segment_count，
  // 并有 stored_path（POST /api/plan/inspect 与 /api/prepare 的 plan_path 来源）。
  return {
    upload_id: null,
    sha256: sha,
    graph_id: sha,
    version: null,
    segments: ['content', 'matrix', 'protection'],
    segment_count: 3,
    latency_samples: 1023,
    stored_path: '/var/lib/khaen/plans/' + sha + '.plan'
  }
}

const CHANNELS_INFO: ChannelsInfo = JSON.parse(JSON.stringify(buildChannelsSeed('wired').channels))
const INSPECT: PlanInspect = JSON.parse(JSON.stringify(buildChannelsSeed('wired').inspect))
const HISTORY: PlanHistory = {
  current_version: 7,
  current_graph_id: null,
  source: 'daemon-process',
  versions: [
    {
      version: 5,
      graph_id: 'g5',
      sha256: 's5',
      applied_at: 't5',
      recorded_at: 't5',
      applied_block: null,
      latency_samples: null,
      observed: true,
      source: 'upload'
    },
    {
      version: 7,
      graph_id: 'g7',
      sha256: 's7',
      applied_at: 't7',
      recorded_at: 't7',
      applied_block: 897,
      latency_samples: 1023,
      observed: true,
      source: 'upload'
    }
  ],
  reasons: {}
}

interface Calls {
  channels: number
  prepareChannels: number
  commit: number
  abort: number
  history: number
  inspect: number
  upload: number
  preparePlan: number
  rollback: number
}

function fakeApi(partial: Partial<ChannelsApi> = {}): { api: ChannelsApi; calls: Calls } {
  const calls: Calls = {
    channels: 0,
    prepareChannels: 0,
    commit: 0,
    abort: 0,
    history: 0,
    inspect: 0,
    upload: 0,
    preparePlan: 0,
    rollback: 0
  }
  const api: ChannelsApi = {
    async fetchChannels(): Promise<ChannelsInfo> {
      calls.channels += 1
      return CHANNELS_INFO
    },
    async prepareChannels(): Promise<ChannelsPrepareAck> {
      calls.prepareChannels += 1
      return {
        ok: true,
        engine_token: 'channels-token',
        planned_version: 9,
        changed: true,
        preview: {
          old_channels: 22,
          new_channels: 20,
          old_input_channels: 2,
          new_input_channels: 2,
          audio_gap_ms_estimate: 1200
        },
        warnings: ['audio output will be interrupted'],
        blockers: []
      }
    },
    async commit(): Promise<ControlCommitAck> {
      calls.commit += 1
      return commitAck()
    },
    async abort(): Promise<{ ok: true; aborted: boolean; aborted_surface: string | null }> {
      calls.abort += 1
      return { ok: true, aborted: true, aborted_surface: null }
    },
    async fetchPlanHistory(): Promise<PlanHistory> {
      calls.history += 1
      return HISTORY
    },
    async fetchPlanInspect(_planPath: string, _planSha256?: string | null): Promise<PlanInspect> {
      calls.inspect += 1
      return INSPECT
    },
    async uploadPlan(bytes: Uint8Array): Promise<PlanUploadAck> {
      calls.upload += 1
      return uploadAck('a'.repeat(64))
    },
    async preparePlan(): Promise<ControlDraft> {
      calls.preparePlan += 1
      return planDraft()
    },
    async rollbackPlan(): Promise<ControlDraft> {
      calls.rollback += 1
      return planDraft({ surface: 'rollback', rolled_back_to: 5, graph_id: 'g5' })
    },
    ...partial
  }
  return { api, calls }
}

const SHA_A = 'a'.repeat(64)

function shaStub(expected: string) {
  return async () => expected
}

test('channels controller loads the layout, keeps data on refresh failure and dedupes', async () => {
  const state = createInitialChannelsState()
  const { api, calls } = fakeApi()
  const controller = createChannelsController(state, { api, now: () => 1111 })
  await controller.load()
  assert.equal(calls.channels, 1)
  assert.equal(state.phase, 'ready')
  assert.equal(state.info?.output.channels, 22)
  assert.equal(state.loadedAt, 1111)

  const failing = createChannelsController(state, {
    api: fakeApi({
      async fetchChannels(): Promise<ChannelsInfo> {
        throw new ApiError({ status: 401, code: 'not_authenticated', message: 'auth' })
      }
    }).api
  })
  await failing.refresh()
  assert.equal(state.phase, 'ready')
  assert.equal(state.error?.category, 'unauthorized')
  assert.equal(state.info?.output.channels, 22)

  const fresh = createInitialChannelsState()
  const failingController = createChannelsController(fresh, {
    api: fakeApi({
      async fetchChannels(): Promise<ChannelsInfo> {
        throw new ApiError({ status: 503, code: 'host_link_unavailable', message: 'down' })
      }
    }).api
  })
  await failingController.load()
  assert.equal(fresh.phase, 'error')
  assert.equal(fresh.info, null)
  assert.equal(fresh.error?.category, 'host_link_unavailable')
})

test('plan history and inspect failures clear the data and keep a stable error', async () => {
  const state = createInitialChannelsState()
  const { api } = fakeApi({
    async fetchPlanHistory(): Promise<PlanHistory> {
      throw new ApiError({ status: 501, code: 'not_wired', message: 'T55' })
    },
    async fetchPlanInspect(): Promise<PlanInspect> {
      throw new ApiError({ status: 501, code: 'not_wired', message: 'T55' })
    }
  })
  const controller = createChannelsController(state, { api })
  await controller.loadHistory()
  assert.equal(state.history, null)
  assert.equal(state.historyError?.category, 'not_wired')
  // 显式传 plan_path 才会发请求；没有 plan_path 是 store 的 NOT_AVAILABLE（另测）。
  await controller.loadInspect('/var/lib/khaen/plans/whatever.plan')
  assert.equal(state.plan.inspect, null)
  assert.equal(state.plan.inspectError?.category, 'not_wired')
})

test('selectPlanFile computes the client sha256 and upload verifies both sides', async () => {
  const state = createInitialChannelsState()
  const { api } = fakeApi()
  const controller = createChannelsController(state, {
    api,
    sha256: shaStub(SHA_A)
  })
  const bytes = new Uint8Array(128).fill(7)
  await controller.selectPlanFile('demo.bin', bytes)
  assert.equal(state.plan.upload.name, 'demo.bin')
  assert.equal(state.plan.upload.byteLength, 128)
  assert.equal(state.plan.upload.sha256, SHA_A)
  assert.equal(state.plan.upload.bytes?.length, 128)

  const ack = await controller.uploadPlan()
  assert.equal(ack?.sha256, SHA_A)
  // 真实 daemon 不回 upload_id：用 sha256/stored_path；upload_id 如实 null（不拿 sha 冒充新字段）。
  assert.equal(state.plan.upload.ack?.upload_id, null)
  assert.equal(state.plan.upload.ack?.stored_path, '/var/lib/khaen/plans/' + SHA_A + '.plan')
  // 上传后自动尝试 inspect（失败不影响上传回执）。
  assert.equal(state.plan.inspect?.sha256, INSPECT.sha256)
  assert.equal(state.plan.busy, null)

  // 双侧 sha 不一致 -> 拒绝进入 prepare。
  const mismatch = createInitialChannelsState()
  const mismatchController = createChannelsController(mismatch, {
    api: fakeApi({
      async uploadPlan(): Promise<PlanUploadAck> {
        return uploadAck('b'.repeat(64))
      }
    }).api,
    sha256: shaStub(SHA_A)
  })
  await mismatchController.selectPlanFile('demo.bin', bytes)
  const rejected = await mismatchController.uploadPlan()
  assert.equal(rejected, null)
  assert.equal(mismatch.plan.upload.ack, null)
  assert.equal(mismatch.plan.upload.error?.category, 'sha_mismatch')
  assert.equal(mismatch.plan.upload.error?.detail, 'client ' + SHA_A + ' != server ' + 'b'.repeat(64))
})

test('upload failure records the stable code and keeps the local file selection', async () => {
  const state = createInitialChannelsState()
  const controller = createChannelsController(state, {
    api: fakeApi({
      async uploadPlan(): Promise<PlanUploadAck> {
        throw new ApiError({ status: 400, code: 'invalid_plan', message: 'bad abi' })
      }
    }).api,
    sha256: shaStub(SHA_A)
  })
  await controller.selectPlanFile('bad.bin', new Uint8Array(128))
  assert.equal(await controller.uploadPlan(), null)
  assert.equal(state.plan.upload.error?.category, 'invalid_plan')
  assert.equal(state.plan.upload.name, 'bad.bin')
})

test('plan inspect and prepare use the upload stored_path (daemon POST contract)', async () => {
  const state = createInitialChannelsState()
  const inspectArgs: Array<{ planPath: string; sha256: string | null | undefined }> = []
  const prepareArgs: Array<{ plan_path: string; sha256: string }> = []
  const { api, calls } = fakeApi({
    async fetchPlanInspect(planPath: string, planSha256?: string | null): Promise<PlanInspect> {
      inspectArgs.push({ planPath, sha256: planSha256 })
      calls.inspect += 1
      return INSPECT
    },
    async preparePlan(input: { plan_path: string; sha256: string }): Promise<ControlDraft> {
      prepareArgs.push(input)
      calls.preparePlan += 1
      return planDraft()
    }
  })
  const controller = createChannelsController(state, { api, sha256: shaStub(SHA_A) })
  await controller.selectPlanFile('demo.bin', new Uint8Array(128))
  await controller.uploadPlan()
  // 上传回执的 stored_path + 服务端 sha256 是 inspect 的两个入参（真实 daemon：POST {plan_path, plan_sha256}）。
  assert.deepEqual(inspectArgs, [
    { planPath: '/var/lib/khaen/plans/' + SHA_A + '.plan', sha256: SHA_A }
  ])

  const draft = await controller.preparePlan()
  assert.equal(draft?.engine_token, 'plan-token')
  assert.deepEqual(prepareArgs, [
    { plan_path: '/var/lib/khaen/plans/' + SHA_A + '.plan', sha256: SHA_A }
  ])
})

test('upload ack without stored_path degrades inspect/prepare to NOT_AVAILABLE (no fake path)', async () => {
  const state = createInitialChannelsState()
  const { api, calls } = fakeApi({
    async uploadPlan(): Promise<PlanUploadAck> {
      calls.upload += 1
      return {
        upload_id: null,
        sha256: SHA_A,
        graph_id: null,
        version: null,
        segments: null,
        segment_count: null,
        latency_samples: null,
        stored_path: null
      }
    }
  })
  const controller = createChannelsController(state, { api, sha256: shaStub(SHA_A) })
  await controller.selectPlanFile('demo.bin', new Uint8Array(128))
  const ack = await controller.uploadPlan()
  assert.equal(ack?.sha256, SHA_A)
  // 没有 stored_path：不猜路径、不发 inspect 请求，如实 NOT_AVAILABLE。
  assert.equal(calls.inspect, 0)
  assert.equal(state.plan.inspect, null)
  assert.equal(state.plan.inspectError?.category, 'plan_path_unavailable')
  assert.equal(state.plan.inspectError?.retryable, false)
  assert.equal(await controller.preparePlan(), null)
  assert.equal(calls.preparePlan, 0)
  assert.equal(state.plan.error?.category, 'plan_path_unavailable')

  // 没有上传回执时直接 loadInspect 同样如实 NOT_AVAILABLE（不伪造预览）。
  const fresh = createInitialChannelsState()
  const freshController = createChannelsController(fresh, { api: fakeApi().api })
  await freshController.loadInspect()
  assert.equal(fresh.plan.inspect, null)
  assert.equal(fresh.plan.inspectError?.category, 'plan_path_unavailable')
})

test('plan prepare/commit/abort drive the two-phase state machine', async () => {
  const state = createInitialChannelsState()
  const { api, calls } = fakeApi()
  const controller = createChannelsController(state, { api, sha256: shaStub(SHA_A), now: () => 2222 })
  await controller.selectPlanFile('demo.bin', new Uint8Array(128))
  await controller.uploadPlan()

  const draft = await controller.preparePlan()
  assert.equal(draft?.engine_token, 'plan-token')
  assert.equal(state.plan.draft?.surface, 'plan')
  assert.equal(calls.preparePlan, 1)

  const ack = await controller.commitPlan()
  assert.equal(ack?.version, 8)
  assert.equal(state.plan.lastCommit?.version, 8)
  assert.equal(state.plan.lastCommitAt, 2222)
  assert.equal(state.plan.draft, null)
  // 提交后自动刷新历史与声道概览。
  assert.equal(calls.history, 1)
  assert.equal(calls.channels, 1)

  await controller.preparePlan()
  assert.equal(await controller.abortPlan(), true)
  assert.equal(state.plan.draft, null)
})

test('plan prepare failure and commit failure keep stable errors (M3/M4 backend gaps)', async () => {
  const state = createInitialChannelsState()
  const { api } = fakeApi({
    async preparePlan(): Promise<ControlDraft> {
      throw new ApiError({ status: 501, code: 'not_wired', message: 'T55' })
    }
  })
  const controller = createChannelsController(state, { api })
  state.plan.upload.ack = uploadAck(SHA_A)
  const draft = await controller.preparePlan()
  assert.equal(draft, null)
  assert.equal(state.plan.draft, null)
  assert.equal(state.plan.error?.category, 'not_wired')

  const commitState = createInitialChannelsState()
  const commitController = createChannelsController(commitState, {
    api: fakeApi({
      async commit(): Promise<ControlCommitAck> {
        throw new ApiError({ status: 501, code: 'host_link_unsupported', message: 'M3b' })
      }
    }).api
  })
  commitState.plan.draft = planDraft()
  assert.equal(await commitController.commitPlan(), null)
  // commit 失败保留草稿（可重试/abort）。
  assert.notEqual(commitState.plan.draft, null)
  assert.equal(commitState.plan.error?.category, 'host_link_unsupported')
})

test('rollback prepares a version-increasing draft that still needs commit', async () => {
  const state = createInitialChannelsState()
  const { api, calls } = fakeApi()
  const controller = createChannelsController(state, { api })
  const draft = await controller.prepareRollback(5)
  assert.equal(draft?.surface, 'rollback')
  assert.equal(draft?.rolled_back_to, 5)
  assert.equal(calls.rollback, 1)
  assert.equal(state.plan.draft?.surface, 'rollback')
  const ack = await controller.commitPlan()
  assert.equal(ack?.version, 8)
  assert.equal(state.plan.draft, null)
})

test('channels prepare/commit/abort expose the M3b NOT_AVAILABLE commit error honestly', async () => {
  const state = createInitialChannelsState()
  const { api } = fakeApi({
    async commit(): Promise<ControlCommitAck> {
      throw new ApiError({ status: 501, code: 'host_link_unsupported', message: 'M3b/T52' })
    }
  })
  const controller = createChannelsController(state, { api })
  const ack = await controller.prepareChannels({ output: { channels: 20 } })
  assert.equal(ack?.changed, true)
  assert.equal(state.channelsDraft?.engine_token, 'channels-token')
  assert.deepEqual(state.channelsDraft?.warnings, ['audio output will be interrupted'])

  assert.equal(await controller.commitChannels(), null)
  assert.equal(state.channelsError?.category, 'host_link_unsupported')
  // commit 失败保留草稿，便于 abort。
  assert.notEqual(state.channelsDraft, null)

  assert.equal(await controller.abortChannels(), true)
  assert.equal(state.channelsDraft, null)
})

test('channels prepare failure clears the draft and records the code', async () => {
  const state = createInitialChannelsState()
  const controller = createChannelsController(state, {
    api: fakeApi({
      async prepareChannels(): Promise<ChannelsPrepareAck> {
        throw new ApiError({ status: 409, code: 'channel_layout_unavailable', message: 'no rt-host' })
      }
    }).api
  })
  assert.equal(await controller.prepareChannels({ output: { channels: 20 } }), null)
  assert.equal(state.channelsDraft, null)
  assert.equal(state.channelsError?.category, 'conflict')
})

test('busy guards reject concurrent writes and reset clears everything', async () => {
  const state = createInitialChannelsState()
  let release: (value: ChannelsInfo) => void = () => {}
  const pending = new Promise<ChannelsInfo>((resolve) => {
    release = resolve
  })
  const { api } = fakeApi({ fetchChannels: () => pending })
  const controller = createChannelsController(state, { api })
  const first = controller.load()
  assert.equal(state.refreshing, true)
  await controller.loadHistory()
  release(CHANNELS_INFO)
  await first
  assert.equal(state.info?.output.channels, 22)

  state.plan.draft = planDraft()
  state.plan.busy = 'commit'
  assert.equal(await controller.commitPlan(), null)
  state.plan.busy = null
  state.channelsDraft = {
    ok: true,
    engine_token: 'x',
    planned_version: 9,
    changed: true,
    preview: null,
    warnings: [],
    blockers: []
  }
  state.channelsBusy = 'prepare'
  assert.equal(await controller.prepareChannels({ output: { channels: 20 } }), null)
  state.channelsBusy = null
  controller.reset()
  assert.deepEqual(state, createInitialChannelsState())
})
