import { test } from 'node:test'
import assert from 'node:assert/strict'
import type {
  ChannelsAbortAck,
  ChannelsApi,
  ChannelsCommitAck,
  ChannelsInfo,
  ChannelsPrepareAck,
  PlanHistory,
  PlanInspect,
  PlanUploadAck
} from '../src/api/channels.ts'
import type { ControlCommitAck, ControlDraft } from '../src/api/control.ts'
import { ApiError } from '../src/api/client.ts'
import {
  MAPPING_PLAN_OWNED_REASON,
  buildChannelsAbortAck,
  buildChannelsCommitAck,
  buildChannelsCommitFailureDetails,
  buildChannelsMappingOnlyAck,
  buildChannelsPrepareAck,
  buildChannelsSeed
} from '../scripts/channels-fixtures.mjs'
import { createChannelsController, createInitialChannelsState } from '../src/stores/channels.ts'

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value))
}

function prepareAck(overrides: Partial<ChannelsPrepareAck> = {}): ChannelsPrepareAck {
  return { ...clone(buildChannelsPrepareAck()), ...overrides }
}

function mappingOnlyAck(overrides: Partial<ChannelsPrepareAck> = {}): ChannelsPrepareAck {
  return { ...clone(buildChannelsMappingOnlyAck()), ...overrides }
}

function commitAck(overrides: Partial<ChannelsCommitAck> = {}): ChannelsCommitAck {
  return { ...clone(buildChannelsCommitAck()), ...overrides }
}

function abortAck(overrides: Partial<ChannelsAbortAck> = {}): ChannelsAbortAck {
  return { ...clone(buildChannelsAbortAck()), ...overrides }
}

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

function planCommitAck(overrides: Partial<ControlCommitAck> = {}): ControlCommitAck {
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

const CHANNELS_INFO: ChannelsInfo = clone(buildChannelsSeed('wired').channels) as ChannelsInfo
const INSPECT: PlanInspect = clone(buildChannelsSeed('wired').inspect) as PlanInspect
const HISTORY: PlanHistory = {
  current_version: 7,
  current_graph_id: null,
  source: 'rt-host',
  versions: [
    {
      version: 7,
      graph_id: 'g7',
      sha256: 's7',
      applied_at: 't7',
      recorded_at: 't7',
      applied_block: 897,
      latency_samples: 1023,
      observed: true,
      source: 'commit'
    }
  ],
  reasons: {}
}

interface Calls {
  channels: number
  prepareChannels: number
  commitChannels: number
  abortChannels: number
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
    commitChannels: 0,
    abortChannels: 0,
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
      return prepareAck()
    },
    async commitChannels(): Promise<ChannelsCommitAck> {
      calls.commitChannels += 1
      return commitAck()
    },
    async abortChannels(): Promise<ChannelsAbortAck> {
      calls.abortChannels += 1
      return abortAck()
    },
    async commit(): Promise<ControlCommitAck> {
      calls.commit += 1
      return planCommitAck()
    },
    async abort(): Promise<{ ok: true; aborted: boolean; aborted_surface: string | null }> {
      calls.abort += 1
      return { ok: true, aborted: true, aborted_surface: null }
    },
    async fetchPlanHistory(): Promise<PlanHistory> {
      calls.history += 1
      return HISTORY
    },
    async fetchPlanInspect(): Promise<PlanInspect> {
      calls.inspect += 1
      return INSPECT
    },
    async uploadPlan(): Promise<PlanUploadAck> {
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

/* ---------------------------------------------------------------- 读取 */

test('channels controller loads the layout, keeps data on refresh failure and dedupes concurrent loads', async () => {
  const state = createInitialChannelsState()
  const { api, calls } = fakeApi()
  const controller = createChannelsController(state, { api, now: () => 1111 })
  await controller.load()
  assert.equal(calls.channels, 1)
  assert.equal(state.phase, 'ready')
  assert.equal(state.info?.output.channels, 22)
  assert.equal(state.info?.hot_reconfigure.wired, true)
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
  let release: (value: ChannelsInfo) => void = () => {}
  const pending = new Promise<ChannelsInfo>((resolve) => {
    release = resolve
  })
  const deduped = createChannelsController(fresh, { api: fakeApi({ fetchChannels: () => pending }).api })
  const first = deduped.load()
  const second = deduped.load()
  assert.equal(fresh.refreshing, true)
  release(CHANNELS_INFO)
  await Promise.all([first, second])
  assert.equal(fresh.info?.output.channels, 22)
})

test('plan history and inspect failures clear the data and keep a stable error', async () => {
  const state = createInitialChannelsState()
  const { api } = fakeApi({
    async fetchPlanHistory(): Promise<PlanHistory> {
      throw new ApiError({ status: 501, code: 'not_wired', message: 'no' })
    },
    async fetchPlanInspect(): Promise<PlanInspect> {
      throw new ApiError({ status: 503, code: 'host_link_unavailable', message: 'down' })
    }
  })
  const controller = createChannelsController(state, { api })
  await controller.loadHistory()
  assert.equal(state.history, null)
  assert.equal(state.historyError?.category, 'not_wired')
  await controller.loadInspect('/var/lib/khaen/plans/whatever.plan')
  assert.equal(state.plan.inspect, null)
  assert.equal(state.plan.inspectError?.category, 'host_link_unavailable')
})

/* ------------------------------------------------- 方案上传/两阶段（既有功能保留） */

test('selectPlanFile computes the client sha256 and upload verifies both sides', async () => {
  const state = createInitialChannelsState()
  const controller = createChannelsController(state, {
    api: fakeApi().api,
    sha256: shaStub(SHA_A)
  })
  await controller.selectPlanFile('demo.bin', new Uint8Array(128).fill(7))
  assert.equal(state.plan.upload.name, 'demo.bin')
  assert.equal(state.plan.upload.sha256, SHA_A)
  const ack = await controller.uploadPlan()
  assert.equal(ack?.sha256, SHA_A)
  assert.equal(state.plan.upload.ack?.stored_path, '/var/lib/khaen/plans/' + SHA_A + '.plan')
  assert.equal(state.plan.inspect?.sha256, INSPECT.sha256)
  assert.equal(state.plan.busy, null)

  const mismatch = createInitialChannelsState()
  const mismatchController = createChannelsController(mismatch, {
    api: fakeApi({ async uploadPlan(): Promise<PlanUploadAck> { return uploadAck('b'.repeat(64)) } }).api,
    sha256: shaStub(SHA_A)
  })
  await mismatchController.selectPlanFile('demo.bin', new Uint8Array(128))
  assert.equal(await mismatchController.uploadPlan(), null)
  assert.equal(mismatch.plan.upload.error?.category, 'sha_mismatch')
})

test('page plan two-phase still uses POST /api/prepare|commit|abort and rollback', async () => {
  const state = createInitialChannelsState()
  const { api, calls } = fakeApi()
  const controller = createChannelsController(state, { api, sha256: shaStub(SHA_A), now: () => 2222 })
  await controller.selectPlanFile('demo.bin', new Uint8Array(128))
  await controller.uploadPlan()
  const draft = await controller.preparePlan()
  assert.equal(draft?.engine_token, 'plan-token')
  const ack = await controller.commitPlan()
  assert.equal(ack?.version, 8)
  assert.equal(state.plan.lastCommitAt, 2222)
  assert.equal(state.plan.draft, null)
  assert.equal(calls.commit, 1)
  assert.equal(calls.channels, 1)
  assert.equal(await controller.abortPlan(), false)

  const rollback = await controller.prepareRollback(5)
  assert.equal(rollback?.surface, 'rollback')
  assert.equal(await controller.abortPlan(), true)
  assert.equal(state.plan.draft, null)
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
  await controller.uploadPlan()
  assert.equal(calls.inspect, 0)
  assert.equal(state.plan.inspectError?.category, 'plan_path_unavailable')
  assert.equal(await controller.preparePlan(), null)
  assert.equal(calls.preparePlan, 0)
  assert.equal(state.plan.error?.category, 'plan_path_unavailable')
})

/* ------------------------------------------------ 声道热重配 prepare 状态机 */

test('prepareChannels stores the draft and exposes commit_supported/warnings/blockers', async () => {
  const state = createInitialChannelsState()
  const { api, calls } = fakeApi()
  const controller = createChannelsController(state, { api })
  const ack = await controller.prepareChannels({ channels: 20 })
  assert.equal(calls.prepareChannels, 1)
  assert.equal(ack?.commit_supported, true)
  assert.equal(state.channelsDraft?.engine_token, 'channels-token-3')
  assert.equal(state.channelsDraft?.changed, true)
  assert.equal(state.channelsDraft?.graph?.action, 'dropped')
  assert.equal(state.channelsDraft?.warnings.length, 1)
  assert.equal(state.channelsBusy, null)
  assert.equal(state.channelsError, null)
})

test('prepareChannels failure clears the draft and records errorStage=prepare', async () => {
  const state = createInitialChannelsState()
  const controller = createChannelsController(state, {
    api: fakeApi({
      async prepareChannels(): Promise<ChannelsPrepareAck> {
        throw new ApiError({ status: 502, code: 'ERR_RECONFIGURE_DEVICE', message: 'probe failed', details: buildChannelsCommitFailureDetails() })
      }
    }).api
  })
  assert.equal(await controller.prepareChannels({ channels: 20 }), null)
  assert.equal(state.channelsDraft, null)
  assert.equal(state.channelsError?.category, 'reconfig_failed')
  assert.equal(state.channelsErrorStage, 'prepare')
  assert.equal(state.channelsFailure?.rolled_back, true)
})

/* ------------------------------------ commit：commit_supported=false 时不发请求 */

test('commitChannels refuses to send commit when commit_supported=false (mapping belongs to plan routing)', async () => {
  const state = createInitialChannelsState()
  const { api, calls } = fakeApi({
    async prepareChannels(): Promise<ChannelsPrepareAck> {
      calls.prepareChannels += 1
      return mappingOnlyAck()
    }
  })
  const controller = createChannelsController(state, { api })
  const ack = await controller.prepareChannels({ dac_map: [0, null, 1, null] })
  assert.equal(ack?.commit_supported, false)
  assert.equal(state.channelsDraft?.engine_token, null)

  const result = await controller.commitChannels()
  assert.equal(result, null)
  // 关键：不发 PUT stage=commit（不假成功、不签发假 token）。
  assert.equal(calls.commitChannels, 0)
  assert.equal(state.channelsError?.category, 'commit_unsupported')
  assert.equal(state.channelsError?.code, 'commit_unsupported')
  assert.match(String(state.channelsError?.detail), /plan routing section/)
  assert.equal(state.channelsErrorStage, 'commit')
  // 草稿保留：用户可 abort（本地丢弃，因为没有服务端 token）。
  assert.notEqual(state.channelsDraft, null)
  assert.equal(await controller.abortChannels(), true)
  assert.equal(calls.abortChannels, 0)
  assert.equal(state.channelsDraft, null)
})

/* ------------------------------------------------------------- commit 成功/失败 */

test('commitChannels sends the token, stores the real receipt and reloads GET /api/channels', async () => {
  const state = createInitialChannelsState()
  const tokens: string[] = []
  const { api, calls } = fakeApi({
    async commitChannels(token: string): Promise<ChannelsCommitAck> {
      tokens.push(token)
      calls.commitChannels += 1
      return commitAck()
    }
  })
  const controller = createChannelsController(state, { api, now: () => 3333 })
  await controller.prepareChannels({ channels: 20 })
  const ack = await controller.commitChannels()
  assert.deepEqual(tokens, ['channels-token-3'])
  assert.equal(ack?.applied, true)
  assert.equal(state.channelsCommit?.generation, 4)
  assert.equal(state.channelsCommit?.gap_ms, 37)
  assert.equal(state.channelsCommitAt, 3333)
  assert.equal(state.channelsDraft, null)
  // commit 后重新读取数据面真实配置（GET /api/channels），不用旧值冒充新状态。
  assert.equal(calls.channels, 1)
  assert.equal(state.channelsBusy, null)
})

test('commitChannels 502 keeps error.failure rolled_back details and drops the consumed draft', async () => {
  const state = createInitialChannelsState()
  const controller = createChannelsController(state, {
    api: fakeApi({
      async commitChannels(): Promise<ChannelsCommitAck> {
        throw new ApiError({
          status: 502,
          code: 'ERR_RECONFIGURE_DEVICE',
          message: 'cannot reopen the output device',
          details: buildChannelsCommitFailureDetails()
        })
      }
    }).api
  })
  await controller.prepareChannels({ channels: 20 })
  assert.equal(await controller.commitChannels(), null)
  assert.equal(state.channelsError?.category, 'reconfig_failed')
  assert.equal(state.channelsFailure?.rolled_back, true)
  assert.equal(state.channelsFailure?.applied, false)
  assert.equal(state.channelsFailure?.gap_ms, 412)
  assert.equal(state.channelsErrorStage, 'commit')
  // 失败发生在提交开始之后：rt-host 已消费草稿，本地丢弃（不给出可重放的假 token）。
  assert.equal(state.channelsDraft, null)
  assert.equal(state.channelsCommit, null)
})

test('commitChannels 409 conflict keeps the draft for retry/abort', async () => {
  const state = createInitialChannelsState()
  const controller = createChannelsController(state, {
    api: fakeApi({
      async commitChannels(): Promise<ChannelsCommitAck> {
        throw new ApiError({ status: 409, code: 'channels_busy', message: 'commit in flight' })
      }
    }).api
  })
  await controller.prepareChannels({ channels: 20 })
  assert.equal(await controller.commitChannels(), null)
  assert.equal(state.channelsError?.category, 'conflict')
  assert.notEqual(state.channelsDraft, null)
})

/* ------------------------------------------------------------------- abort */

test('abortChannels sends PUT stage=abort with the engine token and clears the draft', async () => {
  const state = createInitialChannelsState()
  const tokens: string[] = []
  const { api } = fakeApi({
    async abortChannels(token: string): Promise<ChannelsAbortAck> {
      tokens.push(token)
      return abortAck()
    }
  })
  const controller = createChannelsController(state, { api })
  await controller.prepareChannels({ channels: 20 })
  assert.equal(await controller.abortChannels(), true)
  assert.deepEqual(tokens, ['channels-token-3'])
  assert.equal(state.channelsDraft, null)
})

test('abortChannels reports aborted=false honestly and keeps the draft', async () => {
  const state = createInitialChannelsState()
  const controller = createChannelsController(state, {
    api: fakeApi({ async abortChannels(): Promise<ChannelsAbortAck> { return abortAck({ aborted: false }) } }).api
  })
  await controller.prepareChannels({ channels: 20 })
  assert.equal(await controller.abortChannels(), false)
  assert.notEqual(state.channelsDraft, null)
  assert.equal(state.channelsError?.code, 'abort_not_applied')
  assert.equal(state.channelsErrorStage, 'abort')
})

test('discardChannelsDraft drops the local draft without calling the server', async () => {
  const state = createInitialChannelsState()
  const { api, calls } = fakeApi()
  const controller = createChannelsController(state, { api })
  await controller.prepareChannels({ channels: 20 })
  controller.discardChannelsDraft()
  assert.equal(state.channelsDraft, null)
  assert.equal(calls.abortChannels, 0)
  assert.equal(state.channelsError, null)
})

/* ------------------------------------------------------------------ 互斥/reset */

test('busy guards reject concurrent channel writes and reset clears everything', async () => {
  const state = createInitialChannelsState()
  state.channelsBusy = 'prepare'
  const { api } = fakeApi()
  const controller = createChannelsController(state, { api })
  assert.equal(await controller.prepareChannels({ channels: 20 }), null)
  state.channelsBusy = 'commit'
  state.channelsDraft = prepareAck()
  assert.equal(await controller.commitChannels(), null)
  state.channelsBusy = 'abort'
  assert.equal(await controller.abortChannels(), false)
  state.channelsBusy = null
  controller.reset()
  assert.deepEqual(state, createInitialChannelsState())
})
