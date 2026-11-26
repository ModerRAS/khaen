import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { ApiClient, type ApiError } from '../src/api/client.ts'
import {
  AUDIO_PROFILE_PATHS,
  classifyAudioProfileError,
  createAudioProfileApi,
  createDefaultProfile
} from '../src/api/audioProfile.ts'
import { createRecordingFetch, jsonResponse } from '../scripts/mock-test-support.mjs'

/**
 * Speaker Layout 契约补充测试（task-73 / docs/API.md §12）：
 * - 文档确实声明了本页使用的所有端点（防止单边漂移）；
 * - 401 走会话统一处理（onUnauthorized），不静默吞掉；
 * - 503 not_wired / 403 sound_window_closed / 409 busy 归类为 UI 稳定类别；
 * - 写请求都带 CSRF（与 client 统一策略一致）。
 */

const HERE = dirname(fileURLToPath(import.meta.url))
const DOC = readFileSync(resolve(HERE, '..', '..', 'docs', 'API.md'), 'utf8')

async function captureError(run: () => Promise<unknown>): Promise<ApiError> {
  try {
    await run()
  } catch (error) {
    assert.ok(error instanceof Error)
    return error as ApiError
  }
  return assert.fail('expected the call to reject') as never
}

test('docs/API.md §12 declares every speaker-layout endpoint', () => {
  for (const path of [
    '/api/audio-profile',
    '/api/audio-profile/plan',
    '/api/device/inputs',
    '/api/speaker-test',
    '/api/speaker-test/stop'
  ]) {
    assert.match(DOC, new RegExp(path.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')), path + ' must be documented')
  }
  assert.match(DOC, /kHAEN_SPEAKER_TEST/i)
  assert.match(DOC, /audible/)
  assert.match(DOC, /confirm/)
})

test('401 on the audio profile endpoint reaches the shared unauthorized handler', async () => {
  const seen: number[] = []
  const recording = createRecordingFetch(() =>
    jsonResponse(401, { error: { code: 'not_authenticated', message: 'session expired' } })
  )
  const client = new ApiClient({
    baseUrl: 'http://mock.local',
    fetchImpl: recording.fetch,
    onUnauthorized: (error) => seen.push(error.status)
  })
  const api = createAudioProfileApi(client)
  const error = await captureError(() => api.fetchProfile())
  assert.equal(error.status, 401)
  assert.deepEqual(seen, [401])
})

test('speaker test failures keep their stable codes (403 window / 409 busy / 503 not_wired)', async () => {
  const cases: Array<[number, Record<string, unknown>, string]> = [
    [403, { error: { code: 'sound_window_closed', message: 'outside the sound window' } }, 'sound_window_closed'],
    [409, { error: { code: 'busy', message: 'calibration is running' } }, 'busy'],
    [503, { error: { code: 'not_wired', message: 'test tone is not wired' } }, 'not_wired']
  ]
  for (const [status, body, expected] of cases) {
    const recording = createRecordingFetch(() => jsonResponse(status, body))
    const client = new ApiClient({ baseUrl: 'http://mock.local', fetchImpl: recording.fetch })
    const api = createAudioProfileApi(client)
    const error = await captureError(() =>
      api.startSpeakerTest({ dac: 0, ms: 1000, kind: 'noise', db: -30, audible: true, confirm: true })
    )
    assert.equal(error.code, expected)
    const info = classifyAudioProfileError(error)
    assert.equal(info.code, expected)
    assert.match(info.i18nKey, /^speakerLayout\.error\./)
  }
})

test('profile writes carry CSRF and the exact two-phase bodies', async () => {
  const bodies: unknown[] = []
  const calls: Array<{ method: string; path: string; csrf: string | undefined }> = []
  const recording = createRecordingFetch((call) => {
    const path = String(call.url).replace('http://mock.local', '')
    calls.push({ method: call.method, path, csrf: call.headers['X-KHAEN-CSRF'] })
    bodies.push(JSON.parse(String(call.body)))
    if (path === AUDIO_PROFILE_PATHS.profile) {
      return jsonResponse(200, {
        ok: true,
        stage: 'prepare',
        profile: createDefaultProfile(),
        validation: { ok: true, errors: [], warnings: [] },
        diff: {},
        token: 't1'
      })
    }
    return jsonResponse(200, { ok: true, aborted: true })
  })
  const client = new ApiClient({ baseUrl: 'http://mock.local', fetchImpl: recording.fetch, csrfToken: () => 'csrf-1' })
  const api = createAudioProfileApi(client)
  await api.prepareProfile(createDefaultProfile())
  await api.commitProfile('t1')
  await api.abortProfile('t1')
  assert.equal(calls.length, 3)
  assert.deepEqual(calls.map((call) => [call.method, call.path, call.csrf]), [
    ['PUT', AUDIO_PROFILE_PATHS.profile, 'csrf-1'],
    ['PUT', AUDIO_PROFILE_PATHS.profile, 'csrf-1'],
    ['PUT', AUDIO_PROFILE_PATHS.profile, 'csrf-1']
  ])
  assert.deepEqual((bodies[0] as any).stage, 'prepare')
  assert.deepEqual(bodies[1], { stage: 'commit', token: 't1' })
  assert.deepEqual(bodies[2], { stage: 'abort', token: 't1' })
})
