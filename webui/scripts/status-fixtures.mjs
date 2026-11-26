#!/usr/bin/env node
/**
 * /api/status contract fixtures for the DEV/TEST mock only (scripts/ is never bundled into dist).
 *
 * Three honest shapes (PLAN §4.4 khaen-status/v1；任务 T35b)：
 *  - present: 全字段真实值，host_link 新鲜（connected=true, status_age_ms=120）；
 *  - stale:   host_link 断开 + status_age_ms=15000 + state=faulted（前端黄色 stale 标识）；
 *  - missing: 每个叶子字段都有理由的 null + reasons + not_wired（前端逐字段「未接入」TruthTag）。
 *
 * 时间戳固定（不用 Date.now），保证测试断言与截图可复现。
 * 用法：createMockAuthServer({ statusFixture: 'stale' })
 *       node scripts/mock-auth-server.mjs --status-fixture missing
 */

export const STATUS_FIXTURES = ['present', 'stale', 'missing']

export const DEFAULT_STATUS_FIXTURE = 'present'

/** 是否是合法的 fixture 名。 */
export function isStatusFixture(value) {
  return typeof value === 'string' && STATUS_FIXTURES.includes(value)
}

const PRESENT_SINCE = '2026-11-26T04:00:00.000Z'
const PRESENT_APPLIED_AT = '2026-11-26T03:45:12.000Z'
const GRAPH_ID = '8bcc0f0b1d2e3f405162738495a6b7c8'
const PLAN_SHA256 = '9f2c8e4a7b1d3f6058a9c2e4d6b8a0c1e3f5a7b9c1d2e3f4a6b8c0d2e4f6a8b0'

const PRESENT = {
  schema: 'khaen-status/v1',
  daemon: { version: '0.3.0-mock', uptime_s: 43210, bind: '127.0.0.1:8790' },
  host_link: {
    connected: true,
    since: PRESENT_SINCE,
    socket: '/run/khaen/rt-host.sock',
    status_age_ms: 120
  },
  state: { value: 'running', muted: false, safe: false, last_error: null },
  plan: {
    graph_id: GRAPH_ID,
    sha256: PLAN_SHA256,
    version: 3,
    latency_samples: 1023,
    applied_at: PRESENT_APPLIED_AT,
    source: 'upload'
  },
  format: {
    plan_rate: 48000,
    device_rate: 48000,
    rate_asserted: true,
    block_frames: 128,
    channels: 22,
    encoding: 'L24',
    aes67: { channels: 2, map: [0, 1], pt: 97, group: '239.69.67.1', port: 5004 }
  },
  input: {
    packets: 895912,
    underruns: 21,
    jitter_fill: 880,
    ssrc: 534361700,
    queue_full_drops: 0,
    recv_errors: 0
  },
  output: { blocks: 352000, xruns: 0, out_errors: 0, bytes_written: 2973696000 },
  device: {
    id: 'MOTU UltraLite mk5',
    card: 'hw:CARD=UltraLitemk5,DEV=0',
    state: 'present',
    rate_readback: 48000
  },
  calibration: { active_job: null, last_job_id: 'job-20261126-0001', last_verdict: 'PASS' },
  not_wired: [
    { path: '/api/surfaces', reason: 'M3: surfaces aggregate not implemented in the mock' },
    { path: '/api/calibration/jobs', reason: 'M5: calibration jobs not implemented in the mock' }
  ],
  reasons: {}
}

const STALE = {
  ...PRESENT,
  host_link: {
    connected: false,
    since: PRESENT_SINCE,
    socket: null,
    status_age_ms: 15000
  },
  state: {
    value: 'faulted',
    muted: false,
    safe: true,
    last_error: 'E_HOST_LINK_LOST: rt-host status socket closed'
  },
  reasons: {
    'host_link.socket': 'rt-host did not report its socket path',
    socket: 'rt-host did not report its socket path'
  }
}

const MISSING_REASONS = {
  'daemon.version': 'daemon did not report its version',
  'daemon.uptime_s': 'daemon did not report uptime',
  'host_link.connected': 'control link (T70f) not wired',
  'host_link.status_age_ms': 'control link (T70f) not wired',
  'state.value': 'rt-host did not report its run state',
  'plan.graph_id': 'no plan applied',
  'plan.sha256': 'no plan applied',
  'format.device_rate': 'rt-host did not report the device rate',
  'format.rate_asserted': 'rt-host did not report the device rate',
  'format.aes67.group': 'AES67 receiver not configured',
  'input.packets': 'AES67 receiver not started',
  'input.underruns': 'AES67 receiver not started',
  'output.blocks': 'ALSA playback not opened',
  'output.bytes_written': 'ALSA playback not opened',
  'device.id': 'MOTU card not enumerated',
  'device.state': 'MOTU card not enumerated',
  'calibration.last_verdict': 'no calibration job yet',
  'calibration.last_job_id': 'no calibration job yet'
}

const MISSING = {
  schema: 'khaen-status/v1',
  daemon: { version: null, uptime_s: null, bind: null },
  host_link: { connected: null, since: null, socket: null, status_age_ms: null },
  state: { value: null, muted: null, safe: null, last_error: null },
  plan: {
    graph_id: null,
    sha256: null,
    version: null,
    latency_samples: null,
    applied_at: null,
    source: null
  },
  format: {
    plan_rate: null,
    device_rate: null,
    rate_asserted: null,
    block_frames: null,
    channels: null,
    encoding: null,
    aes67: { channels: null, map: null, pt: null, group: null, port: null }
  },
  input: { packets: null, underruns: null, jitter_fill: null, ssrc: null, queue_full_drops: null, recv_errors: null },
  output: { blocks: null, xruns: null, out_errors: null, bytes_written: null },
  device: { id: null, card: null, state: null, rate_readback: null },
  calibration: { active_job: null, last_job_id: null, last_verdict: null },
  not_wired: [
    { path: '/api/status', reason: 'this mock fixture reports every value as missing' },
    { path: '/api/surfaces', reason: 'M3: surfaces aggregate not implemented in the mock' }
  ],
  reasons: MISSING_REASONS
}

const FIXTURES = { present: PRESENT, stale: STALE, missing: MISSING }

/**
 * 构造指定 fixture 的响应体（深拷贝，调用方可安全修改）。
 * @param {string} name
 * @returns {object}
 */
export function buildStatusFixture(name = DEFAULT_STATUS_FIXTURE) {
  const fixture = FIXTURES[isStatusFixture(name) ? name : DEFAULT_STATUS_FIXTURE]
  return JSON.parse(JSON.stringify(fixture))
}
