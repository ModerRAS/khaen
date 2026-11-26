/**
 * M3 控制面契约 fixture（DEV / TEST ONLY；scripts/ 不进 dist）。
 *
 * 与真实 daemon/rt-host 控制面的字段一致（T70f-b 转发；语义见 crates/host/src/control/mod.rs）：
 * - mute / mode / format / version / base plan；
 * - fixture 'wired'：控制通道可用（mute/surface/commit 都真实生效，模拟 rt-host 在线）；
 * - fixture 'unavailable'：T70f-b 链路不可用（写操作 503 host_link_unavailable），
 *   页面应如实显示「控制通道不可用」而不是假成功。
 *
 * 历史版本/时间戳固定，保证 mock 可复现（不依赖当前时间）。
 */

export const CONTROL_FIXTURES = ['wired', 'unavailable']
export const DEFAULT_CONTROL_FIXTURE = 'wired'

export function isControlFixture(value) {
  return typeof value === 'string' && CONTROL_FIXTURES.includes(value)
}

/** 固定的 64 位十六进制 graph_id（mock 用；真实 daemon 由方案 ABI 计算）。 */
const GRAPH_IDS = {
  5: '5a5a5a5a0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c',
  6: '6b6b6b6b0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c',
  7: '7c7c7c7c0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c'
}

/** 固定 sha256（plan 历史回执；mock 用）。 */
const PLAN_SHA = {
  5: 'a5a5a5a5b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacb',
  6: 'a6a6a6a6b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacb',
  7: 'a7a7a7a7b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacb'
}

/** 固定时间戳（UTC；与 status/system fixture 的 2026-11-26 基线一致）。 */
const APPLIED_AT = '2026-11-26T06:20:00.000Z'

/**
 * 构造一份可变的控制面初始状态（调用方直接改字段；不要共享引用）。
 *
 * @param {'wired'|'unavailable'} fixture
 */
export function buildControlSeed(fixture) {
  const wired = fixture !== 'unavailable'
  const seed = {
    fixture,
    wired,
    reason: wired
      ? null
      : 'rt-host control channel is unavailable (mock fixture: host_link.mode=none); T70f-b cannot proxy control ops',
    muted: false,
    mode: 'aes67',
    format: { sample_rate: 48000, block_size: 128, channels: 22 },
    version: 7,
    basePlan: {
      graph_id: GRAPH_IDS[7],
      sha256: PLAN_SHA[7],
      latency_samples: 1023,
      applied_at: APPLIED_AT,
      source: 'upload'
    },
    /** 最近 3 个已生效版本（version 升序；与 rt-host 有限历史语义一致）。 */
    history: [
      { version: 5, graph_id: GRAPH_IDS[5], sha256: PLAN_SHA[5], latency_samples: 1023, applied_at: '2026-11-26T05:00:00.000Z', source: 'upload' },
      { version: 6, graph_id: GRAPH_IDS[6], sha256: PLAN_SHA[6], latency_samples: 1023, applied_at: '2026-11-26T05:40:00.000Z', source: 'calibration' },
      { version: 7, graph_id: GRAPH_IDS[7], sha256: PLAN_SHA[7], latency_samples: 1023, applied_at: APPLIED_AT, source: 'upload' }
    ],
    /** 每个 lane 的保护增益（mock 基线；真实后端由方案 protection 段提供）。 */
    gains_db: Array.from({ length: 22 }, () => 0),
    /** 设备速率回读（/api/status 用固定的 status fixture；这里给 /api/device/retry 回执）。 */
    device_rate: 48000,
    rate_asserted: true
  }
  return seed
}

/** 按版本号生成稳定的 graph_id（mock 用；32 字节十六进制）。 */
export function graphIdForVersion(version) {
  const head = version.toString(16).padStart(2, '0').repeat(4)
  const tail = '0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c'
  return (head + tail).slice(0, 64)
}
