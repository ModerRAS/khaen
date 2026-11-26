#!/usr/bin/env node
/**
 * /api/system/* contract fixtures for the DEV/TEST mock only (scripts/ is never bundled into dist).
 *
 * Honest shapes（T46 后端契约；T47 前端按此 mock 开发）：
 *  - mismatch:   **缺陷实例**——deployed release v0.1.2，但 /healthz 报 crate version 0.1.0，
 *                前端必须醒目区分 crate_version / release_version / commit（并给出不一致警示）；
 *                daemon 运行中、rt-host 未启动（pid=null + reason），MOTU + 板载卡各一张；
 *  - consistent: crate_version == release_version（0.1.2），daemon/rt-host 都在跑（含 PID），
 *                host_link 新鲜；用于演练「一致」形态（无警示条）；
 *  - degraded:   所有现场采集失败（systemctl / /proc 不可用）——每个字段 null + reasons，
 *                设备列表空、日志 503 logs_unavailable、restart 不允许（KHAEN_ALLOW_RESTART 未设）。
 *
 * 时间戳固定（不用 Date.now），保证测试断言与截图可复现。
 * 用法：createMockAuthServer({ systemFixture: 'degraded' })
 *       node scripts/mock-auth-server.mjs --system-fixture degraded
 */
import { buildStatusFixture } from './status-fixtures.mjs'

export const SYSTEM_FIXTURES = ['mismatch', 'consistent', 'degraded']

export const DEFAULT_SYSTEM_FIXTURE = 'mismatch'

/** 是否是合法的 system fixture 名。 */
export function isSystemFixture(value) {
  if (typeof value !== 'string') {
    return false
  }
  for (const fixture of SYSTEM_FIXTURES) {
    if (fixture === value) {
      return true
    }
  }
  return false
}

const SINCE = '2026-11-26T04:00:00.000Z'
const APPLIED_AT = '2026-11-26T03:45:12.000Z'
const COMMIT = '855f4c4'
const RUSTC = 'rustc 1.89.0 (29483883e 2025-08-04)'
const KERNEL = 'Linux 6.8.0-45-generic'
const OS = 'Debian GNU/Linux 12 (bookworm)'
const HOSTNAME = 'AoipDSP'
const STATE_DIR = '/var/lib/khaen'
const RT_STATUS_FILE = '/var/lib/khaen/rt-host-status.json'
const SOCKET = '/run/khaen/rt-host.sock'

const DEVICES = [
  {
    card_id: 'UltraLitemk5',
    driver: 'snd_usb_audio',
    model: 'MOTU UltraLite mk5',
    channels: 22,
    rates: [44100, 48000, 88200, 96000]
  },
  {
    card_id: 'PCH',
    driver: 'snd_hda_intel',
    model: 'HDA Intel PCH',
    channels: 8,
    rates: [44100, 48000]
  }
]

const MISMATCH = {
  schema: 'khaen-system-info/v1',
  crate_version: '0.1.0',
  release_version: 'v0.1.2',
  commit: COMMIT,
  build_profile: 'release',
  rustc: RUSTC,
  target: 'x86_64-unknown-linux-gnu',
  bind: '0.0.0.0:80',
  state_dir: STATE_DIR,
  rt_status_file: RT_STATUS_FILE,
  uptime_s: 18342,
  hostname: HOSTNAME,
  kernel: KERNEL,
  os: OS,
  services: {
    daemon: { active: true, enabled: true, pid: 4211, reason: null },
    rt_host: { active: false, enabled: true, pid: null, reason: 'rt-host is not running' }
  },
  devices: DEVICES,
  host_link: {
    connected: false,
    since: null,
    socket: SOCKET,
    status_age_ms: null
  },
  restart: { allowed: true, target: 'rt-host', reason: null },
  reasons: {
    'services.rt_host.pid': 'rt-host is not running',
    'services.rt_host.active': 'rt-host is not running',
    'host_link.connected': 'rt-host status socket is not connected',
    'host_link.status_age_ms': 'rt-host did not report its status file'
  }
}

const CONSISTENT = {
  ...MISMATCH,
  crate_version: '0.1.2',
  services: {
    daemon: { active: true, enabled: true, pid: 4211, reason: null },
    rt_host: { active: true, enabled: true, pid: 4247, reason: null }
  },
  host_link: {
    connected: true,
    since: SINCE,
    socket: SOCKET,
    status_age_ms: 95
  },
  reasons: {}
}

const DEGRADED = {
  schema: 'khaen-system-info/v1',
  crate_version: '0.1.0',
  release_version: null,
  commit: null,
  build_profile: null,
  rustc: RUSTC,
  target: null,
  bind: null,
  state_dir: null,
  rt_status_file: null,
  uptime_s: null,
  hostname: null,
  kernel: null,
  os: null,
  services: {
    daemon: { active: null, enabled: null, pid: null, reason: 'systemctl is not available on this host' },
    rt_host: { active: null, enabled: null, pid: null, reason: 'systemctl is not available on this host' }
  },
  devices: [],
  host_link: { connected: null, since: null, socket: null, status_age_ms: null },
  restart: { allowed: false, target: 'rt-host', reason: 'KHAEN_ALLOW_RESTART is not set' },
  reasons: {
    release_version: 'KHAEN_BUILD_VERSION is not set (unversioned build)',
    commit: 'git metadata is not available in this build',
    build_profile: 'build profile was not recorded',
    'bind': 'bind address was not recorded',
    state_dir: 'state directory was not recorded',
    rt_status_file: 'rt-host status file path was not recorded',
    uptime_s: 'daemon uptime was not recorded',
    hostname: 'hostname is not available on this host',
    kernel: '/proc/version is not readable',
    os: '/etc/os-release is not readable',
    'services.daemon.active': 'systemctl is not available on this host',
    'services.daemon.enabled': 'systemctl is not available on this host',
    'services.daemon.pid': 'systemctl is not available on this host',
    'services.rt_host.active': 'systemctl is not available on this host',
    'services.rt_host.enabled': 'systemctl is not available on this host',
    'services.rt_host.pid': 'systemctl is not available on this host',
    devices: '/proc/asound/cards is not readable',
    'host_link.connected': 'rt-host status file is not readable',
    'host_link.status_age_ms': 'rt-host status file is not readable',
    restart: 'KHAEN_ALLOW_RESTART is not set'
  }
}

const INFO_FIXTURES = { mismatch: MISMATCH, consistent: CONSISTENT, degraded: DEGRADED }

/**
 * 构造指定 fixture 的 /api/system/info 响应体（深拷贝，调用方可安全修改）。
 * @param {string} name
 * @returns {object}
 */
export function buildSystemInfoFixture(name = DEFAULT_SYSTEM_FIXTURE) {
  const fixture = INFO_FIXTURES[isSystemFixture(name) ? name : DEFAULT_SYSTEM_FIXTURE]
  return JSON.parse(JSON.stringify(fixture))
}

/* ------------------------------------------------------------------ 日志 */

const LOG_UNITS = ['daemon', 'rt-host']
const LOG_UNIT_NAMES = { daemon: 'khaen-daemon', 'rt-host': 'khaen-rt-host' }
const LOG_AVAILABLE = { mismatch: true, consistent: true, degraded: false }
const LOGS_UNAVAILABLE_REASON = 'journalctl is not available on this host (systemd not detected)'

/** 是否是合法的日志 unit。 */
export function isLogUnit(value) {
  return value === 'daemon' || value === 'rt-host'
}

/** 日志行模板（journalctl --no-pager 的短格式；固定时间戳，确定性）。 */
function logLine(unit, index) {
  const base = Date.UTC(2026, 10, 26, 6, 0, 0) / 1000 + index
  const date = new Date(base * 1000)
  const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']
  const stamp =
    months[date.getUTCMonth()] +
    ' ' +
    String(date.getUTCDate()).padStart(2, ' ') +
    ' ' +
    String(date.getUTCHours()).padStart(2, '0') +
    ':' +
    String(date.getUTCMinutes()).padStart(2, '0') +
    ':' +
    String(date.getUTCSeconds()).padStart(2, '0')
  const pid = unit === 'daemon' ? 4211 : 4247
  const tag = LOG_UNIT_NAMES[unit]
  const messages =
    unit === 'daemon'
      ? [
          'starting khaen-daemon (build release, crate 0.1.0)',
          'webui embedded: 7 files, 988 KiB',
          'auth: admin password configured',
          'rt-host status file: ' + RT_STATUS_FILE,
          'rt-host status stale (age 15000 ms > 2000 ms)',
          'GET /healthz 200 (anonymous)',
          'GET /api/status 200 (session)',
          'GET /api/status 401 (anonymous)'
        ]
      : [
          'starting khaen-rt-host',
          'control: unix socket ' + SOCKET,
          'device: MOTU UltraLite mk5 card UltraLitemk5 22 ch',
          'format: 48000 Hz / 128 frames / S24_3LE',
          'plan: v3 graph 8bcc0f0b applied',
          'xrun: none',
          'control link heartbeat 95 ms',
          'stopping khaen-rt-host (signal SIGTERM)'
        ]
  return stamp + ' ' + HOSTNAME + ' ' + tag + '[' + String(pid) + ']: ' + messages[index % messages.length]
}

/**
 * 构造 /api/system/logs 响应体。
 * @param {string} fixture
 * @param {string} unit
 * @param {number} lines
 * @returns {{available: boolean, reason: string|null, body: object|null}}
 */
export function buildSystemLogsFixture(fixture = DEFAULT_SYSTEM_FIXTURE, unit = 'daemon', lines = 200) {
  const name = isSystemFixture(fixture) ? fixture : DEFAULT_SYSTEM_FIXTURE
  const safeUnit = isLogUnit(unit) ? unit : 'daemon'
  const count = Number.isFinite(lines) ? Math.min(Math.max(Math.trunc(lines), 1), 500) : 200
  if (LOG_AVAILABLE[name] !== true) {
    return { available: false, reason: LOGS_UNAVAILABLE_REASON, body: null }
  }
  const all = []
  for (let index = 0; index < count; index += 1) {
    all.push(logLine(safeUnit, index))
  }
  return {
    available: true,
    reason: null,
    body: {
      schema: 'khaen-system-logs/v1',
      unit: safeUnit,
      unit_name: LOG_UNIT_NAMES[safeUnit],
      requested_lines: count,
      lines: all,
      truncated: false,
      source: 'journalctl',
      reason: null
    }
  }
}

/* ------------------------------------------------------------- 诊断 JSON */

/**
 * 诊断包文件名：带 release 版本（没有则 crate 版本）与 UTC 时间，便于归档定位。
 * @param {object} info /api/system/info 响应体
 * @param {string} generatedAt RFC3339
 */
export function buildDiagnosticsFilename(info, generatedAt) {
  const version = info.release_version ?? info.crate_version
  const safeVersion = typeof version === 'string' && version.length > 0 ? version.replace(/^v/, '') : 'unknown'
  const stamp =
    typeof generatedAt === 'string'
      ? generatedAt.replace(/[-:]/g, '').replace(/\.\d+Z$/, 'Z').replace('T', 'T')
      : 'unknown'
  return 'khaen-diagnostics-v' + safeVersion + '-' + stamp + '.json'
}

export { LOG_UNITS, LOG_UNIT_NAMES, LOGS_UNAVAILABLE_REASON, COMMIT, STATE_DIR, RT_STATUS_FILE, OS, KERNEL, HOSTNAME }

/** 诊断包里的 status 摘要使用 mock 的 present fixture（固定时间戳，可复现）。 */
export function buildDiagnosticsStatusFixture() {
  return buildStatusFixture('present')
}
