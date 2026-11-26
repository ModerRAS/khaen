/**
 * MOTU 页面展示辅助（纯函数；Node 单测覆盖）。
 *
 * 纪律：
 * - 只格式化后端真实回执/回读里的值，缺失一律 null（页面渲染「—」+「未上报/原因」），不补 0；
 * - linear <-> dB 使用与设备相同的换算（后端 crates/motu-cuemix/src/fader.rs：20*log10(linear)，
 *   linear 0 = 静音 / -inf dB）；linear 才是设备真值，dB 是等效展示；
 * - 本模块不依赖 Vue / DOM，也避开 @ 别名（相对导入），便于 node --test 直接 import。
 */
import { formatDecimal, formatTimestamp } from '../../utils/format.ts'
import type {
  MotuStatus,
  MotuVolumeCommitAck,
  MotuVolumeDraft,
  MotuWriteReceipt
} from '../../api/motu.ts'

/** 8.24 量化网格的最小声学门限（后端 FADER_DB_FLOOR）。 */
export const MOTU_DB_FLOOR = -144.49

/** linear -> dB（20*log10）；非正/非有限返回 null（静音 = -inf，不是 0 dB）。 */
export function linearToDb(linear: number | null | undefined): number | null {
  if (typeof linear !== 'number' || !Number.isFinite(linear) || linear <= 0) {
    return null
  }
  return 20 * Math.log10(linear)
}

/** dB 文本（不含单位，最多 2 位小数）；null 返回 null。 */
export function formatMotuDb(db: number | null | undefined, locale: string): string | null {
  return formatDecimal(db, 2, locale)
}

/** linear 文本（最多 4 位小数）；null 返回 null。 */
export function formatMotuLinear(linear: number | null | undefined, locale: string): string | null {
  return formatDecimal(linear, 4, locale)
}

/** 设备回读值的一行文本：优先 dB，linear=0 显示「−∞」语义（由组件拼 i18n 文案）。 */
export interface MotuVolumeValueText {
  /** dB 文本（无单位）；null = 没上报或静音 */
  db: string | null
  /** linear 文本（无单位）；null = 没上报 */
  linear: string | null
  /** true = 设备回读线性值为 0（静音 / -inf dB），不是“没数据” */
  silent: boolean
}

/** GET vol[] 一项 -> 可展示文本（linear 是设备真值，db 可能为 null=静音）。 */
export function motuVolumeValueText(
  entry: { linear: number | null; db: number | null } | null,
  locale: string
): MotuVolumeValueText {
  if (entry === null || entry.linear === null) {
    return { db: null, linear: null, silent: false }
  }
  const silent = entry.linear === 0
  return {
    db: entry.db === null && silent ? null : formatMotuDb(entry.db, locale),
    linear: formatMotuLinear(entry.linear, locale),
    silent
  }
}

/** commit 回执里某通道的真值回读行（before/wrote/readback 都是 linear）。 */
export interface MotuReceiptView {
  channel: number | null
  name: string | null
  /** 写入前设备上的线性值（未收到过 = null） */
  beforeLinear: string | null
  /** 目标线性值（8.24 量化后） */
  wroteLinear: string | null
  /** 设备回读线性值（verified=true 才代表写成功） */
  readbackLinear: string | null
  /** 目标值的等效 dB 文本（前端换算，仅展示） */
  wroteDb: string | null
  /** 回读值的等效 dB 文本 */
  readbackDb: string | null
  changed: boolean | null
  frameSent: boolean | null
  verified: boolean | null
}

/** commit 回执 channels[] -> 展示行（真值回读；不做“全部成功”推断）。 */
export function motuReceiptViews(
  receipts: readonly MotuWriteReceipt[] | null,
  locale: string
): MotuReceiptView[] {
  if (receipts === null) {
    return []
  }
  return receipts.map((receipt) => ({
    channel: receipt.channel,
    name: receipt.name,
    beforeLinear: formatMotuLinear(receipt.before, locale),
    wroteLinear: formatMotuLinear(receipt.wrote, locale),
    readbackLinear: formatMotuLinear(receipt.readback, locale),
    wroteDb: formatMotuDb(linearToDb(receipt.wrote), locale),
    readbackDb: formatMotuDb(receipt.readback === null ? null : linearToDb(receipt.readback), locale),
    changed: receipt.changed,
    frameSent: receipt.frame_sent,
    verified: receipt.verified
  }))
}

/** commit 回执整体是否可标“已写入设备”：applied=true 且 writes_device=true（缺一不可）。 */
export function motuCommitWritesDevice(ack: MotuVolumeCommitAck | null): boolean {
  return ack !== null && ack.applied === true && ack.writes_device === true
}

/** commit 回执里通过回读校验的通道数 / 回执通道总数（无可读回执时 null）。 */
export function motuVerifiedCount(ack: MotuVolumeCommitAck | null): { verified: number; total: number } | null {
  if (ack === null || ack.channels === null || ack.channels.length === 0) {
    return null
  }
  let verified = 0
  for (const receipt of ack.channels) {
    if (receipt.verified === true) {
      verified += 1
    }
  }
  return { verified, total: ack.channels.length }
}

/** prepare 回执的目标通道号（target.channels[]，缺回执时 []）。 */
export function motuDraftChannels(draft: MotuVolumeDraft | null): number[] {
  if (draft === null || draft.target === null) {
    return []
  }
  return draft.target.channels.map((channel) => channel.channel)
}

/** 草稿 TTL 剩余秒数（created_at + expires_in_s - now）；无法计算返回 null。 */
export function motuDraftRemainingSeconds(draft: MotuVolumeDraft | null, nowMs: number): number | null {
  if (draft === null || draft.expires_in_s === null || draft.created_at === null) {
    return null
  }
  const createdMs = Date.parse(draft.created_at)
  if (!Number.isFinite(createdMs) || !Number.isFinite(nowMs)) {
    return null
  }
  const remaining = draft.expires_in_s - (nowMs - createdMs) / 1000
  return remaining > 0 ? Math.ceil(remaining) : 0
}

/** RFC3339 -> 本地化时间文本（无效返回 null）。 */
export function formatMotuTimestamp(iso: string | null | undefined, locale: string): string | null {
  return formatTimestamp(iso, locale)
}

/** GET reasons 映射的某路径原因（组件直接展示后端原文，不翻译）。 */
export function motuStatusReason(status: MotuStatus | null, path: string): string | null {
  if (status === null) {
    return null
  }
  const reason = status.reasons[path]
  return typeof reason === 'string' && reason.length > 0 ? reason : null
}
