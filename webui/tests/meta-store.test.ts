import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ApiError } from '../src/api/client.ts'
import {
  createInitialMetaState,
  createMetaController,
  metaCapabilityReason,
  metaOpState,
  metaSurfaceState
} from '../src/stores/meta.ts'

const SNAPSHOT = {
  apiVersion: '1',
  webVersion: '0.3.0',
  ops: ['surface'],
  surfaces: ['volume'],
  surfaceStates: {},
  calibration: { wired: false, reason: 'not wired' },
  hostLink: null,
  notWired: [{ op: 'calibration.jobs', surface: null, reason: 'not wired' }]
}

function createState() {
  return createInitialMetaState()
}

test('meta controller loads once, caches, and shares concurrent requests', async () => {
  const state = createState()
  let calls = 0
  let resolveFirst: ((value: unknown) => void) | null = null
  const controller = createMetaController(state, {
    api: {
      fetch: () => {
        calls += 1
        if (calls > 1) {
          return Promise.resolve(SNAPSHOT) as never
        }
        return new Promise((resolve) => {
          resolveFirst = resolve as (value: unknown) => void
        }) as never
      }
    },
    now: () => 1234
  })
  const first = controller.load()
  const second = controller.load()
  assert.equal(calls, 1, 'concurrent loads share the in-flight request')
  assert.equal(state.phase, 'loading')
  resolveFirst?.(SNAPSHOT)
  const [a, b] = await Promise.all([first, second])
  assert.deepEqual(a, SNAPSHOT)
  assert.equal(b, a)
  assert.equal(state.phase, 'ready')
  assert.equal(state.loadedAt, 1234)
  await controller.load()
  assert.equal(calls, 1, 'cached snapshot is reused')
  await controller.load(true)
  assert.equal(calls, 2, 'force refreshes')
})

test('meta controller keeps the last snapshot and marks retryable errors', async () => {
  const state = createState()
  let fail = false
  const controller = createMetaController(state, {
    api: {
      fetch: async () => {
        if (fail) {
          throw new ApiError({ status: 503, code: 'not_wired', message: 'not wired yet' })
        }
        return SNAPSHOT as never
      }
    }
  })
  await controller.load()
  assert.equal(state.data?.apiVersion, '1')
  fail = true
  const result = await controller.load(true)
  assert.equal(result, null)
  assert.equal(state.phase, 'error')
  assert.equal(state.data?.apiVersion, '1', 'a failed refresh does not erase the last known snapshot')
  assert.equal(state.error?.retryable, true)
  controller.reset()
  assert.equal(state.data, null)
  assert.equal(state.phase, 'idle')
})

test('meta helpers degrade to unknown when no snapshot is loaded', () => {
  assert.equal(metaSurfaceState('volume'), 'unknown')
  assert.equal(metaOpState('surface'), 'unknown')
  assert.equal(metaCapabilityReason({ surface: 'volume' }), null)
})