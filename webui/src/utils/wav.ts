/**
 * WAV（RIFF/WAVE）头部解析与校准上传格式校验（PLAN §6.7 向导「选择输入」；任务 T37）。
 *
 * 为什么在前端做：第 5 步要求"上传 baseline/applied 两段 WAV → sha256/大小/格式校验"。
 * 格式不合法应当在上传前当场标红（不把明显错误的文件传到 daemon），sha256/字节数仍以服务端
 * POST /api/calibration/uploads 的返回为准（前端不冒充"服务端已校验"）。
 *
 * 纪律：
 * - 纯函数、不依赖 DOM / Vue（Node 单测直接喂字节）；File/Blob 由组件读成 Uint8Array 后传入；
 * - 只做**结构**校验（RIFF/WAVE、fmt、data、PCM、48 kHz、16/24 bit、1-2 ch），不做声学对齐/校准计算
 *   （那是后端 job 的职责）；校验失败给出稳定 code，由 UI 映射成本地化文案；
 * - 不解析/不展示任何假数值：缺 data chunk 就是缺，不补 0。
 */

/** 校准上传 WAV 大小上限（与 daemon 契约一致：64 MiB，超出 413 upload_too_large）。 */
export const MAX_WAV_BYTES = 64 * 1024 * 1024

/** 校准要求：采样率 48 kHz（PLAN §6.7「不满足格式（48k/16-24bit/单声道/双声道）当场标红」）。 */
export const CALIBRATION_SAMPLE_RATE = 48000

/** 允许的位深。 */
export const CALIBRATION_BITS = [16, 24] as const

/** 允许的声道数（单声道 / 双声道）。 */
export const CALIBRATION_CHANNELS = [1, 2] as const

/** WAV 解析/校验的稳定错误码（前端映射本地化文案，后端 upload 还会再校验一次）。 */
export type WavErrorCode =
  | 'file_too_large'
  | 'file_too_small'
  | 'not_riff'
  | 'not_wave'
  | 'missing_fmt'
  | 'missing_data'
  | 'unsupported_codec'
  | 'bad_sample_rate'
  | 'bad_bits'
  | 'bad_channels'
  | 'bad_block_align'
  | 'bad_data_length'

export interface WavFormat {
  /** WAVE format tag：1 = PCM，0xFFFE = WAVE_FORMAT_EXTENSIBLE。 */
  formatTag: number
  channels: number
  sampleRate: number
  bitsPerSample: number
  byteRate: number
  blockAlign: number
}

export interface WavInfo {
  format: WavFormat
  /** data chunk 的字节数。 */
  dataBytes: number
  /** 采样帧数 = dataBytes / blockAlign（向下取整；bad_data_length 时拒绝解析）。 */
  frames: number
  /** 时长（秒）= frames / sampleRate。 */
  durationS: number
  /** 头部到 data payload 的偏移（调试/展示用）。 */
  dataOffset: number
}

export type WavParseResult =
  | { ok: true; info: WavInfo }
  | { ok: false; code: WavErrorCode; detail?: string }

function fail(code: WavErrorCode, detail?: string): { ok: false; code: WavErrorCode; detail?: string } {
  return detail === undefined ? { ok: false, code } : { ok: false, code, detail }
}

function ascii(bytes: Uint8Array, offset: number, length: number): string {
  let text = ''
  for (let index = 0; index < length; index += 1) {
    text += String.fromCharCode(bytes[offset + index] ?? 0)
  }
  return text
}

function readU32(bytes: Uint8Array, offset: number): number {
  return (
    (bytes[offset] ?? 0) +
    ((bytes[offset + 1] ?? 0) << 8) +
    ((bytes[offset + 2] ?? 0) << 16) +
    ((bytes[offset + 3] ?? 0) << 24)
  ) >>> 0
}

function readU16(bytes: Uint8Array, offset: number): number {
  return (bytes[offset] ?? 0) + ((bytes[offset + 1] ?? 0) << 8)
}

/**
 * 解析 WAV 头（只看结构，不做校准格式判定）。
 * 非法/截断返回 {ok:false, code}；合法返回 fmt/data 事实（不编造）。
 */
export function parseWav(bytes: Uint8Array): WavParseResult {
  if (bytes.byteLength > MAX_WAV_BYTES) {
    return fail('file_too_large')
  }
  // 至少 RIFF(4) + size(4) + WAVE(4) + 一个 chunk header(8) = 20 字节；
  // 只有 fmt 没有 data（36 字节）是合法容器，交给后面的 missing_data 判定。
  if (bytes.byteLength < 20) {
    return fail('file_too_small')
  }
  if (ascii(bytes, 0, 4) !== 'RIFF') {
    return fail('not_riff')
  }
  if (ascii(bytes, 8, 4) !== 'WAVE') {
    return fail('not_wave')
  }

  let format: WavFormat | null = null
  let dataBytes: number | null = null
  let dataOffset = -1
  let offset = 12
  // 遍历 chunk；越界即截断（不猜测缺失内容）。
  while (offset + 8 <= bytes.byteLength) {
    const id = ascii(bytes, offset, 4)
    const size = readU32(bytes, offset + 4)
    const payload = offset + 8
    if (payload + size > bytes.byteLength) {
      return fail('bad_data_length', 'chunk "' + id + '" is truncated')
    }
    if (id === 'fmt ') {
      if (size < 16) {
        return fail('missing_fmt', 'fmt chunk is shorter than 16 bytes')
      }
      const formatTag = readU16(bytes, payload)
      const channels = readU16(bytes, payload + 2)
      const sampleRate = readU32(bytes, payload + 4)
      const byteRate = readU32(bytes, payload + 8)
      const blockAlign = readU16(bytes, payload + 12)
      const bitsPerSample = readU16(bytes, payload + 14)
      format = { formatTag, channels, sampleRate, bitsPerSample, byteRate, blockAlign }
    } else if (id === 'data') {
      dataBytes = size
      dataOffset = payload
      // data 可能是最后一个 chunk；继续扫后面的 chunk 没有意义（且常见实现不再写）。
      break
    }
    // chunk 按偶数字节对齐（RIFF 规范）。
    offset = payload + size + (size % 2 === 0 ? 0 : 1)
  }

  if (format === null) {
    return fail('missing_fmt')
  }
  if (dataBytes === null) {
    return fail('missing_data')
  }
  if (format.blockAlign <= 0) {
    return fail('bad_block_align')
  }
  if (dataBytes <= 0 || dataBytes % format.blockAlign !== 0) {
    return fail('bad_data_length', 'data chunk is not a whole number of sample frames')
  }
  const frames = dataBytes / format.blockAlign
  return {
    ok: true,
    info: {
      format,
      dataBytes,
      frames,
      durationS: format.sampleRate > 0 ? frames / format.sampleRate : 0,
      dataOffset
    }
  }
}

export interface WavFormatCheck {
  /** 是否满足校准输入格式（48 kHz / 16-24 bit / 1-2 ch / PCM）。 */
  ok: boolean
  /** 失败时的稳定 code（ok=true 时省略）。 */
  code?: WavErrorCode
}

/** 校准输入格式判定（PLAN §6.7：48k / 16-24 bit / 单声道或双声道）。 */
export function checkCalibrationWavFormat(info: WavInfo): WavFormatCheck {
  const format = info.format
  if (format.formatTag !== 1 && format.formatTag !== 0xfffe) {
    return { ok: false, code: 'unsupported_codec' }
  }
  if (format.sampleRate !== CALIBRATION_SAMPLE_RATE) {
    return { ok: false, code: 'bad_sample_rate' }
  }
  if (!(CALIBRATION_BITS as readonly number[]).includes(format.bitsPerSample)) {
    return { ok: false, code: 'bad_bits' }
  }
  if (!(CALIBRATION_CHANNELS as readonly number[]).includes(format.channels)) {
    return { ok: false, code: 'bad_channels' }
  }
  // 位深与 blockAlign 必须自洽（否则 data 解释会错）。
  if (format.blockAlign !== format.channels * (format.bitsPerSample / 8)) {
    return { ok: false, code: 'bad_block_align' }
  }
  return { ok: true }
}

export type WavPairErrorCode = 'format_mismatch' | 'length_mismatch'

export interface WavPairCheck {
  ok: boolean
  code?: WavPairErrorCode
}

/**
 * baseline / applied 两段录音的结构一致性（同采样率/声道/位深/帧数）。
 * 只做结构对齐；真正的时基对齐由后端 closed_loop 阶段负责。
 * 帧数允许 ±1 帧（编码器/导出取整），超过视为长度不一致。
 */
export function checkWavPairAlignment(a: WavInfo, b: WavInfo): WavPairCheck {
  if (
    a.format.sampleRate !== b.format.sampleRate ||
    a.format.channels !== b.format.channels ||
    a.format.bitsPerSample !== b.format.bitsPerSample
  ) {
    return { ok: false, code: 'format_mismatch' }
  }
  if (Math.abs(a.frames - b.frames) > 1) {
    return { ok: false, code: 'length_mismatch' }
  }
  return { ok: true }
}
