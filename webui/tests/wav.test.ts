import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  CALIBRATION_BITS,
  CALIBRATION_CHANNELS,
  CALIBRATION_SAMPLE_RATE,
  MAX_WAV_BYTES,
  checkCalibrationWavFormat,
  checkWavPairAlignment,
  parseWav
} from '../src/utils/wav.ts'

interface WavSpec {
  sampleRate?: number
  channels?: number
  bits?: number
  frames?: number
  formatTag?: number
  /** data 字节数覆盖（制造 blockAlign 不自洽/截断）。 */
  dataBytes?: number
  /** 省略 data chunk（只留 fmt）。 */
  omitData?: boolean
  /** 省略 fmt chunk（只留 data）。 */
  omitFmt?: boolean
  /** RIFF 头标识（默认 RIFF）。 */
  riff?: string
  /** WAVE 标识（默认 WAVE）。 */
  wave?: string
}

/** 构造一个结构合法的 WAV（默认 48k/16-bit/mono/4800 帧）。 */
function buildWav(spec: WavSpec = {}): Uint8Array {
  const sampleRate = spec.sampleRate ?? 48000
  const channels = spec.channels ?? 1
  const bits = spec.bits ?? 16
  const blockAlign = channels * (bits / 8)
  const frames = spec.frames ?? 4800
  const dataBytes = spec.dataBytes ?? frames * blockAlign
  const formatTag = spec.formatTag ?? 1

  const fmtPayload = new Uint8Array(16)
  const view = new DataView(fmtPayload.buffer)
  view.setUint16(0, formatTag, true)
  view.setUint16(2, channels, true)
  view.setUint32(4, sampleRate, true)
  view.setUint32(8, sampleRate * blockAlign, true)
  view.setUint16(12, blockAlign, true)
  view.setUint16(14, bits, true)

  const chunks: Uint8Array[] = []
  if (spec.omitFmt !== true) {
    const header = new Uint8Array(8)
    header.set(new TextEncoder().encode('fmt '), 0)
    new DataView(header.buffer).setUint32(4, fmtPayload.length, true)
    chunks.push(header, fmtPayload)
  }
  if (spec.omitData !== true) {
    const header = new Uint8Array(8)
    header.set(new TextEncoder().encode('data'), 0)
    new DataView(header.buffer).setUint32(4, dataBytes, true)
    chunks.push(header, new Uint8Array(Math.max(0, dataBytes)))
  }

  const payloadSize = chunks.reduce((sum, chunk) => sum + chunk.length, 0)
  const bytes = new Uint8Array(12 + payloadSize)
  bytes.set(new TextEncoder().encode(spec.riff ?? 'RIFF'), 0)
  new DataView(bytes.buffer).setUint32(4, 4 + payloadSize, true)
  bytes.set(new TextEncoder().encode(spec.wave ?? 'WAVE'), 8)
  let offset = 12
  for (const chunk of chunks) {
    bytes.set(chunk, offset)
    offset += chunk.length
  }
  return bytes
}

test('parseWav reads a valid 48 kHz / 16-bit mono header', () => {
  const result = parseWav(buildWav({ frames: 4800 }))
  assert.equal(result.ok, true)
  if (result.ok !== true) {
    return
  }
  assert.equal(result.info.format.sampleRate, 48000)
  assert.equal(result.info.format.channels, 1)
  assert.equal(result.info.format.bitsPerSample, 16)
  assert.equal(result.info.format.blockAlign, 2)
  assert.equal(result.info.format.formatTag, 1)
  assert.equal(result.info.dataBytes, 9600)
  assert.equal(result.info.frames, 4800)
  assert.equal(result.info.durationS, 0.1)
  assert.equal(result.info.dataOffset, 44)
})

test('parseWav reads a valid 48 kHz / 24-bit stereo header', () => {
  const result = parseWav(buildWav({ bits: 24, channels: 2, frames: 9600 }))
  assert.equal(result.ok, true)
  if (result.ok !== true) {
    return
  }
  assert.equal(result.info.format.bitsPerSample, 24)
  assert.equal(result.info.format.channels, 2)
  assert.equal(result.info.format.blockAlign, 6)
  assert.equal(result.info.frames, 9600)
  assert.equal(result.info.durationS, 0.2)
})

test('parseWav rejects truncated and non-RIFF containers without guessing', () => {
  assert.equal(parseWav(new Uint8Array(0)).ok, false)
  const short = parseWav(new Uint8Array(10))
  assert.equal(short.ok, false)
  if (short.ok === false) {
    assert.equal(short.code, 'file_too_small')
  }

  const notRiff = parseWav(buildWav({ riff: 'RIFX' }))
  assert.equal(notRiff.ok, false)
  if (notRiff.ok === false) {
    assert.equal(notRiff.code, 'not_riff')
  }

  const notWave = parseWav(buildWav({ wave: 'AVI ' }))
  assert.equal(notWave.ok, false)
  if (notWave.ok === false) {
    assert.equal(notWave.code, 'not_wave')
  }
})

test('parseWav rejects missing fmt / missing data / truncated data chunks', () => {
  const noFmt = parseWav(buildWav({ omitFmt: true }))
  assert.equal(noFmt.ok, false)
  if (noFmt.ok === false) {
    assert.equal(noFmt.code, 'missing_fmt')
  }

  const noData = parseWav(buildWav({ omitData: true }))
  assert.equal(noData.ok, false)
  if (noData.ok === false) {
    assert.equal(noData.code, 'missing_data')
  }

  const zeroData = parseWav(buildWav({ frames: 0 }))
  assert.equal(zeroData.ok, false)
  if (zeroData.ok === false) {
    assert.equal(zeroData.code, 'bad_data_length')
  }

  const oddData = parseWav(buildWav({ frames: 10, dataBytes: 21 }))
  assert.equal(oddData.ok, false)
  if (oddData.ok === false) {
    assert.equal(oddData.code, 'bad_data_length')
  }

  // 声明 1000 帧但实际文件被截断：拒绝，不按 0 补齐。
  const truncated = buildWav({ frames: 10 })
  const cut = truncated.slice(0, truncated.length - 4)
  const truncatedResult = parseWav(cut)
  assert.equal(truncatedResult.ok, false)
})

test('parseWav rejects oversized files before parsing', () => {
  const oversized = new Uint8Array(MAX_WAV_BYTES + 1)
  const result = parseWav(oversized)
  assert.equal(result.ok, false)
  if (result.ok === false) {
    assert.equal(result.code, 'file_too_large')
  }
})

test('checkCalibrationWavFormat enforces 48 kHz / 16-24 bit / 1-2 channels', () => {
  assert.equal(CALIBRATION_SAMPLE_RATE, 48000)
  assert.deepEqual([...CALIBRATION_BITS], [16, 24])
  assert.deepEqual([...CALIBRATION_CHANNELS], [1, 2])

  const ok = parseWav(buildWav())
  assert.equal(ok.ok, true)
  if (ok.ok === true) {
    assert.equal(checkCalibrationWavFormat(ok.info).ok, true)
  }

  const wrongRate = parseWav(buildWav({ sampleRate: 44100 }))
  assert.equal(wrongRate.ok, true)
  if (wrongRate.ok === true) {
    assert.deepEqual(checkCalibrationWavFormat(wrongRate.info), { ok: false, code: 'bad_sample_rate' })
  }

  const wrongBits = parseWav(buildWav({ bits: 32 }))
  assert.equal(wrongBits.ok, true)
  if (wrongBits.ok === true) {
    assert.deepEqual(checkCalibrationWavFormat(wrongBits.info), { ok: false, code: 'bad_bits' })
  }

  const wrongChannels = parseWav(buildWav({ channels: 6 }))
  assert.equal(wrongChannels.ok, true)
  if (wrongChannels.ok === true) {
    assert.deepEqual(checkCalibrationWavFormat(wrongChannels.info), { ok: false, code: 'bad_channels' })
  }

  const floatCodec = parseWav(buildWav({ formatTag: 3 }))
  assert.equal(floatCodec.ok, true)
  if (floatCodec.ok === true) {
    assert.deepEqual(checkCalibrationWavFormat(floatCodec.info), { ok: false, code: 'unsupported_codec' })
  }

  // 位深与 blockAlign 不自洽（16-bit 却声明 blockAlign=3）：拒绝。
  const badAlign = buildWav()
  new DataView(badAlign.buffer).setUint16(32, 3, true)
  const badAlignResult = parseWav(badAlign)
  assert.equal(badAlignResult.ok, true)
  if (badAlignResult.ok === true) {
    assert.deepEqual(checkCalibrationWavFormat(badAlignResult.info), { ok: false, code: 'bad_block_align' })
  }
})

test('checkWavPairAlignment compares structure, tolerating one frame', () => {
  const baseline = parseWav(buildWav({ frames: 4800 }))
  const applied = parseWav(buildWav({ frames: 4800 }))
  assert.equal(baseline.ok, true)
  assert.equal(applied.ok, true)
  if (baseline.ok !== true || applied.ok !== true) {
    return
  }
  assert.deepEqual(checkWavPairAlignment(baseline.info, applied.info), { ok: true })

  const offByOne = parseWav(buildWav({ frames: 4801 }))
  assert.equal(offByOne.ok, true)
  if (offByOne.ok === true) {
    assert.deepEqual(checkWavPairAlignment(baseline.info, offByOne.info), { ok: true })
  }

  const shorter = parseWav(buildWav({ frames: 2400 }))
  assert.equal(shorter.ok, true)
  if (shorter.ok === true) {
    assert.deepEqual(checkWavPairAlignment(baseline.info, shorter.info), { ok: false, code: 'length_mismatch' })
  }

  const otherRate = parseWav(buildWav({ sampleRate: 44100 }))
  assert.equal(otherRate.ok, true)
  if (otherRate.ok === true) {
    assert.deepEqual(checkWavPairAlignment(baseline.info, otherRate.info), { ok: false, code: 'format_mismatch' })
  }

  const stereo = parseWav(buildWav({ channels: 2 }))
  assert.equal(stereo.ok, true)
  if (stereo.ok === true) {
    assert.deepEqual(checkWavPairAlignment(baseline.info, stereo.info), { ok: false, code: 'format_mismatch' })
  }
})
