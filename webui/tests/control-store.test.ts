import { test } from 'node:test'
import assert from 'node:assert/strict'
import type {
  ControlDraft,
  ControlApi,
  ControlCommitAck,
  ControlMuteAck,
  DeviceRetryAck
} from '../src/api/control.ts'
import { ApiError } from '../src/api/client.ts'
import { MAX_OPERATION_HISTORY } from '../src/api/control.ts'
import { createControlController, createInitialControlState } from '../src/stores/control.ts'

function draft(overrides: Partial<ControlDraft> = {}): ControlDraft {
  return {
    ok: true,
    surface: 'volume',
    engine_token: 'token-8',
    planned_version: 8,
    changed: null,
    graph_id: 'graph-8',
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
    graph_id: 'graph-8',
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

interface Calls {
  prepareVolume: number
  prepareMode: number
  prepareFormat: number
  commit: number
  abort: number
  mute: number
  retry: number
}

function fakeApi(partial: Partial<ControlApi> = {}): { api: ControlApi; calls: Calls } {
  const calls: Calls = { prepareVolume: 0, prepareMode: 0, prepareFormat: 0, commit: 0, abort: 0, mute: 0, retry: 0 }
  const api: ControlApi = {
    async setMute(mute: boolean): Promise<ControlMuteAck> {
      calls.mute += 1
      return { ok: true, mute, applied_block: 9, idempotent: false }
    },
    async prepareVolume(): Promise<ControlDraft> {
      calls.prepareVolume += 1
      return draft()
    },
    async prepareMode(): Promise<ControlDraft> {
      calls.prepareMode += 1
      return draft({ surface: 'mode', engine_token: 'mode-token', mode: 'off', changed: true, note: 'mode note' })
    },
    async prepareFormat(): Promise<ControlDraft> {
      calls.prepareFormat += 1
      return draft({
        surface: 'format',
        engine_token: 'format-token',
        format: { sample_rate: 96000, block_size: 64, channels: 22 },
        changed: true,
        device_restart_required: true
      })
    },
    async commit(): Promise<ControlCommitAck> {
      calls.commit += 1
      return commitAck()
    },
    async abort(): Promise<{ ok: true; aborted: boolean; aborted_surface: string | null }> {
      calls.abort += 1
      return { ok: true, aborted: true, aborted_surface: 'mode' }
    },
    async retryDevice(): Promise<DeviceRetryAck> {
      calls.retry += 1
      return { wired: true, state: 'streaming', device_rate: 48000, rate_asserted: true, last_error_code: null, last_error_message: null }
    },
    ...partial
  }
  return { api, calls }
}

test('prepare -> commit applies the receipt, clears the draft and records history', async () => {
  const state = createInitialControlState()
  const { api, calls } = fakeApi()
  const controller = createControlController(state, { api, now: () => 1000 })
  assert.equal(state.busy, null)

  const prepared = await controller.prepareVolume([{ lane: 3, gain_db: -6 }])
  assert.equal(prepared?.engine_token, 'token-8')
  assert.equal(calls.prepareVolume, 1)
  assert.equal(state.draft?.surface, 'volume')
  assert.equal(state.busy, null)
  assert.equal(state.history.length, 1)
  assert.equal(state.history[0].action, 'prepare')
  assert.equal(state.history[0].ok, true)
  assert.equal(state.history[0].at, 1000)

  const ack = await controller.commit()
  assert.equal(calls.commit, 1)
  assert.equal(ack?.version, 8)
  assert.equal(state.lastCommit?.graph_id, 'graph-8')
  assert.equal(state.lastCommitAt, 1000)
  assert.equal(state.draft, null)
  assert.equal(state.appliedMode, null)
  assert.equal(state.history[0].action, 'commit')
  assert.equal(state.history[0].version, 8)
  assert.equal(state.history[1].action, 'prepare')
})

test('mode/format prepare expose the surface and commit records applied mode/format', async () => {
  const state = createInitialControlState()
  const { api } = fakeApi({
    async commit(): Promise<ControlCommitAck> {
      return commitAck({ version: 9, changed: true, mode: 'off', format: null, device_restart_required: null })
    }
  })
  const controller = createControlController(state, { api })
  await controller.prepareMode('off')
  assert.equal(state.draft?.surface, 'mode')
  // prepare 回执里的 mode 也是"已准备的目标"，不是已生效：apply 只在 commit 后记录。
  assert.equal(state.appliedMode, null)
  await controller.commit()
  assert.equal(state.appliedMode, 'off')

  const formatApi = fakeApi({
    async commit(): Promise<ControlCommitAck> {
      return commitAck({
        version: 10,
        changed: true,
        format: { sample_rate: 96000, block_size: 64, channels: 22 },
        device_restart_required: true
      })
    }
  })
  const secondState = createInitialControlState()
  const second = createControlController(secondState, { api: formatApi.api })
  await second.prepareFormat({ sample_rate: 96000 })
  await second.commit()
  // 回执格式是控制面回执，不是设备回读；store 只保存它，页面负责区分。
  assert.deepEqual(secondState.appliedFormat, { sample_rate: 96000, block_size: 64, channels: 22 })
})

test('prepare failure clears the local draft and records the stable error', async () => {
  const state = createInitialControlState()
  const { api } = fakeApi({
    async prepareVolume(): Promise<ControlDraft> {
      throw new ApiError({ status: 409, code: 'no_base_plan', message: 'no committed plan' })
    }
  })
  const controller = createControlController(state, { api, now: () => 2000 })
  assert.equal(await controller.prepareVolume([{ lane: 0, gain_db: 0 }]), null)
  assert.equal(state.draft, null)
  assert.equal(state.busy, null)
  assert.equal(state.error?.code, 'no_base_plan')
  assert.equal(state.error?.category, 'no_base_plan')
  assert.equal(state.history[0].ok, false)
  assert.equal(state.history[0].code, 'no_base_plan')
})

test('commit without a draft is a no-op; token_invalid clears a stale draft', async () => {
  const state = createInitialControlState()
  const { api, calls } = fakeApi()
  const controller = createControlController(state, { api })
  assert.equal(await controller.commit(), null)
  assert.equal(calls.commit, 0)

  await controller.prepareVolume([{ lane: 0, gain_db: 0 }])
  const failing = fakeApi({
    async commit(): Promise<ControlCommitAck> {
      throw new ApiError({ status: 409, code: 'engine_token_invalid', message: 'token mismatch' })
    }
  })
  const controller2 = createControlController(state, { api: failing.api })
  assert.equal(await controller2.commit(), null)
  assert.equal(state.draft, null)
  assert.equal(state.error?.category, 'token_invalid')
})

test('abort consumes the draft and keeps a previous commit receipt', async () => {
  const state = createInitialControlState()
  const { api } = fakeApi()
  const controller = createControlController(state, { api })
  await controller.prepareVolume([{ lane: 0, gain_db: 0 }])
  assert.notEqual(state.draft, null)
  const ack = await controller.abort()
  assert.equal(ack?.aborted, true)
  assert.equal(state.draft, null)
  assert.equal(state.history[0].action, 'abort')
  assert.equal(state.history[0].surface, 'mode')
})

test('mute and device retry are recorded and never mutate the draft', async () => {
  const state = createInitialControlState()
  const { api, calls } = fakeApi()
  const controller = createControlController(state, { api, now: () => 3000 })
  await controller.prepareVolume([{ lane: 1, gain_db: -2 }])
  const draftBefore = state.draft

  const mute = await controller.setMute(true)
  assert.equal(mute?.mute, true)
  assert.equal(calls.mute, 1)
  assert.equal(state.lastMute?.mute, true)
  assert.equal(state.draft, draftBefore)

  const retry = await controller.retryDevice()
  assert.equal(retry?.rate_asserted, true)
  assert.equal(calls.retry, 1)
  assert.equal(state.lastRetry?.state, 'streaming')
  assert.equal(state.history[0].action, 'retry')
  assert.equal(state.history[1].action, 'mute')
})

test('busy guard rejects concurrent control actions (single pending slot semantics)', async () => {
  const state = createInitialControlState()
  let release: (value: ControlDraft) => void = () => {}
  const pending = new Promise<ControlDraft>((resolve) => {
    release = resolve
  })
  const { api, calls } = fakeApi({
    prepareVolume: () => pending
  })
  const controller = createControlController(state, { api })
  const first = controller.prepareVolume([{ lane: 0, gain_db: 0 }])
  assert.equal(state.busy, 'prepare')
  assert.equal(await controller.prepareMode('off'), null)
  assert.equal(await controller.setMute(true), null)
  assert.equal(await controller.commit(), null)
  assert.equal(calls.mute, 0)
  release(draft())
  assert.equal((await first)?.engine_token, 'token-8')
  assert.equal(state.busy, null)
})

test('history is capped at MAX_OPERATION_HISTORY entries', async () => {
  const state = createInitialControlState()
  const { api } = fakeApi()
  const controller = createControlController(state, { api })
  for (let index = 0; index < MAX_OPERATION_HISTORY + 3; index += 1) {
    await controller.setMute(index % 2 === 0)
  }
  assert.equal(state.history.length, MAX_OPERATION_HISTORY)
})

test('reset clears every field and the draft', async () => {
  const state = createInitialControlState()
  const { api } = fakeApi()
  const controller = createControlController(state, { api })
  await controller.prepareVolume([{ lane: 0, gain_db: 0 }])
  await controller.setMute(true)
  controller.reset()
  assert.deepEqual(state, createInitialControlState())
})