/**
 * 数值 / 时间 / 单位格式化（PLAN §12.2：Intl 且 locale 感知；单位符号由 i18n 键给出）。
 *
 * 纪律：
 * - 纯函数，locale 显式传入（组件传当前 locale），Node 单测可确定断言；
 * - 不创建全局 Intl 缓存（避免首帧语言切换后不生效）；数字格式化对 null/非有限值一律返回 null，
 *   由调用方渲染「—」+ TruthTag，不显示 0/NaN 之类的假值；
 * - 单位只返回 i18n 键（unit.hz / unit.khz / ...），文案由组件 t() 给出；
 * - 不在单位字符串里硬编码语言。
 */

/** 安全 Intl.NumberFormat（locale 非法时回退 en-US，避免抛错打断渲染）。 */
function numberFormat(locale: string, options: Intl.NumberFormatOptions): Intl.NumberFormat {
  try {
    return new Intl.NumberFormat(locale, options)
  } catch {
    return new Intl.NumberFormat('en-US', options)
  }
}

function isValidNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value)
}

/** 整数（千分位）；null/非有限返回 null。 */
export function formatInteger(value: number | null | undefined, locale: string): string | null {
  if (!isValidNumber(value)) {
    return null
  }
  return numberFormat(locale, { maximumFractionDigits: 0 }).format(value)
}

/** 小数（固定最多 fractionDigits 位）；null/非有限返回 null。 */
export function formatDecimal(
  value: number | null | undefined,
  fractionDigits: number,
  locale: string
): string | null {
  if (!isValidNumber(value)) {
    return null
  }
  const digits = Number.isFinite(fractionDigits) ? Math.min(Math.max(Math.trunc(fractionDigits), 0), 6) : 2
  return numberFormat(locale, {
    minimumFractionDigits: 0,
    maximumFractionDigits: digits
  }).format(value)
}

/** 比例 -> 百分比文本（0.0001234 -> "0.0123%"）；null/非有限返回 null。 */
export function formatPercent(rate: number | null | undefined, locale: string): string | null {
  if (!isValidNumber(rate)) {
    return null
  }
  return numberFormat(locale, {
    style: 'percent',
    minimumFractionDigits: 0,
    maximumFractionDigits: 4
  }).format(rate)
}

/** RFC3339 时间 -> 本地化日期时间；无法解析返回 null（不显示原始串冒充时间）。 */
export function formatTimestamp(iso: string | null | undefined, locale: string): string | null {
  if (typeof iso !== 'string' || iso.length === 0) {
    return null
  }
  const timestamp = Date.parse(iso)
  if (!Number.isFinite(timestamp)) {
    return null
  }
  try {
    return new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'medium' }).format(
      new Date(timestamp)
    )
  } catch {
    return null
  }
}

/** 本地时钟（毫秒时间戳）-> 本地化时间（只带时分秒）；无效返回 null。 */
export function formatClock(epochMs: number | null | undefined, locale: string): string | null {
  if (!isValidNumber(epochMs)) {
    return null
  }
  try {
    return new Intl.DateTimeFormat(locale, { timeStyle: 'medium' }).format(new Date(epochMs))
  } catch {
    return null
  }
}

/**
 * 长 id 中间省略：head + '…' + tail。
 * 短于 head+tail+1 的值原样返回；null/空串返回 null（调用方显示「—」）。
 */
export function shortenId(
  value: string | null | undefined,
  head: number = 8,
  tail: number = 4
): string | null {
  if (typeof value !== 'string' || value.length === 0) {
    return null
  }
  const headLength = Number.isFinite(head) ? Math.max(Math.trunc(head), 1) : 8
  const tailLength = Number.isFinite(tail) ? Math.max(Math.trunc(tail), 0) : 4
  if (value.length <= headLength + tailLength + 1) {
    return value
  }
  return value.slice(0, headLength) + '\u2026' + (tailLength > 0 ? value.slice(value.length - tailLength) : '')
}

/** 字节数换算成合适单位（1024 进制）；返回数值 + i18n 单位键。null/负值/非有限返回 null。 */
export function toByteUnit(
  bytes: number | null | undefined
): { value: number; unitKey: string } | null {
  if (!isValidNumber(bytes) || bytes < 0) {
    return null
  }
  const units: Array<{ unitKey: string; factor: number }> = [
    { unitKey: 'unit.bytes', factor: 1 },
    { unitKey: 'unit.kib', factor: 1024 },
    { unitKey: 'unit.mib', factor: 1024 ** 2 },
    { unitKey: 'unit.gib', factor: 1024 ** 3 },
    { unitKey: 'unit.tib', factor: 1024 ** 4 }
  ]
  let index = 0
  while (index < units.length - 1 && bytes >= units[index + 1].factor) {
    index += 1
  }
  return { value: bytes / units[index].factor, unitKey: units[index].unitKey }
}

/** 采样率换算成 Hz / kHz / MHz；返回数值 + i18n 单位键。非正数/非有限返回 null。 */
export function toRateUnit(
  hz: number | null | undefined
): { value: number; unitKey: string } | null {
  if (!isValidNumber(hz) || hz <= 0) {
    return null
  }
  if (hz >= 1_000_000) {
    return { value: hz / 1_000_000, unitKey: 'unit.mhz' }
  }
  if (hz >= 1_000) {
    return { value: hz / 1_000, unitKey: 'unit.khz' }
  }
  return { value: hz, unitKey: 'unit.hz' }
}
