// 真 daemon 形状契约测试：fixture 由本机真实 khaen-daemon（v0.1.11，process 目标、无音频设备）
// 的实际响应体捕获并脱敏（无 cookie/csrf/token、无用户/临时路径）。目的：把前端解析器锁在
// **真实后端形状**上，而不只是手写 mock 形状；后端字段形状变化时这里先失败。
import { test } from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { parseMeta } from '../src/api/meta.ts'
import { parseStatusResponse } from '../src/api/status.ts'
import { parseChannelsInfo, parsePlanHistory } from '../src/api/channels.ts'
import { parseRoutingChannels, parseRoutingPlanHistory } from '../src/api/routing.ts'
import { parseAudioProfileSnapshot, parseDeviceInputs } from '../src/api/audioProfile.ts'
import { parseJobsResponse, parseWizardResponse, parseWizardStateResponse } from '../src/api/calibration.ts'
import { parseMotuStatus } from '../src/api/motu.ts'
import { parseSystemInfo, parseHealthz } from '../src/api/system.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const fixture = JSON.parse(fs.readFileSync(path.join(here, 'fixtures/real-daemon-v0111.json'), 'utf8')) as {
  schema: string
  responses: Record<string, { path: string; status: number; body: unknown }>
}

function captured(name: string): unknown {
  const entry = fixture.responses[name]
  assert.ok(entry, 'fixture has response ' + name)
  assert.equal(entry.status, 200, 'fixture captured 200 for ' + name)
  return entry.body
}

test('fixture is a real-daemon capture without secrets or local paths', () => {
  assert.equal(fixture.schema, 'khaen-real-daemon-fixture/v1')
  // 只认“键名”是凭据字段；reason 文本里出现 token 一词（"no token is issued"）属于说明，不是凭据。
  const credentialKey = /^(csrf|csrf_token|xsrf|session|session_token|cookie|set-cookie|authorization|password|secret|token)$/i
  const offenders: string[] = []
  const walk = (node: unknown, at: string): void => {
    if (Array.isArray(node)) { node.forEach((item, index) => walk(item, at + '[' + index + ']')); return }
    if (node && typeof node === 'object') {
      for (const [key, value] of Object.entries(node as Record<string, unknown>)) {
        if (credentialKey.test(key)) offenders.push(at + '.' + key)
        walk(value, at + '.' + key)
      }
    }
  }
  walk(fixture.responses, 'responses')
  assert.deepEqual(offenders, [], 'fixture must not contain auth material')
  const text = JSON.stringify(fixture)
  assert.ok(!text.includes('ModerRAS'), 'fixture must not contain the user name')
  assert.ok(!/khaen-r12-state/.test(text), 'fixture must not contain temp state dirs')
})

test('frontend parsers accept the captured real daemon bodies', () => {
  const meta = parseMeta(captured('meta'))
  assert.equal(meta.apiVersion, '1') // real backend sends api_version as a JSON number
  assert.ok(meta.surfaces.length > 0)
  assert.equal(typeof meta.calibration.wired, 'boolean')

  const status = parseStatusResponse(captured('status'))
  assert.equal(status.schema, 'khaen-status/v1')
  assert.ok(status.reasons && typeof status.reasons === 'object')

  const channels = parseChannelsInfo(captured('channels'))
  assert.equal(channels.schema, 'khaen-channels/v1')
  assert.ok(channels.hot_reconfigure && channels.output && channels.format)

  const routingChannels = parseRoutingChannels(captured('channels'))
  assert.ok(routingChannels)
  const routingHistory = parseRoutingPlanHistory(captured('planHistory'))
  assert.ok(routingHistory)

  const audioProfile = parseAudioProfileSnapshot(captured('audioProfile'))
  assert.ok(audioProfile.profile, 'real daemon returns a default profile')
  assert.equal(audioProfile.profile.outputs.length, 22)
  assert.equal(audioProfile.validation.ok, true)

  const inputs = parseDeviceInputs(captured('deviceInputs'))
  assert.equal(inputs.available, false, 'Windows/no ALSA capture: honest available=false')

  const jobs = parseJobsResponse(captured('calibrationJobs'))
  assert.equal(jobs.available, true)
  assert.ok(Array.isArray(jobs.jobs))
  assert.equal(jobs.total, 0)

  const wizard = parseWizardResponse(captured('calibrationWizard'))
  assert.ok(wizard.config)
  const wizardState = parseWizardStateResponse(captured('calibrationWizardState'))
  assert.ok(Array.isArray(wizardState.steps) && wizardState.steps.length > 0)

  const motu = parseMotuStatus(captured('motu'))
  assert.equal(motu.detected, false, 'Windows/no MOTU: honest detected=false')
  assert.ok(Array.isArray(motu.channels))

  const history = parsePlanHistory(captured('planHistory'))
  assert.ok(Array.isArray(history.versions))

  const systemInfo = parseSystemInfo(captured('systemInfo'))
  assert.ok(systemInfo.schema)
  assert.equal(typeof systemInfo.crate_version, 'string')
  assert.equal(systemInfo.state_dir, '<state_dir>')

  const healthz = parseHealthz(captured('healthz'))
  assert.equal(healthz.ok, true)
  assert.equal(typeof healthz.version, 'string')
})
