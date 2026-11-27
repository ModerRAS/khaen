/**
 * 本轮仍无数据源的展示项。有真值的模式和 AES67 不在这里。
 * 任务号与总方案一致：R 读回，C 真实校准，R/IO 读回并改设备抽象。
 * 标签文案在 common.gapTask，这里只做替换。
 */
import zhCN from '../i18n/zh-CN.ts'
import enUS from '../i18n/en-US.ts'

export type GapTask = 'R' | 'C' | 'R/IO'

export type GapLocale = 'zh-CN' | 'en-US'

export type GapId =
  | 'volumeMaster'
  | 'volumeLaneGain'
  | 'layoutFolds'
  | 'routingMirror'
  | 'deviceInputs'
  | 'encoding'
  | 'overviewPlan'
  | 'overviewDevice'
  | 'calibrationMeasure'
  | 'systemRestart'
  | 'systemDaemonRestart'
  | 'speakerTest'

export interface CapabilityGap {
  id: GapId
  task: GapTask
}

export const CAPABILITY_GAPS: Record<GapId, CapabilityGap> = {
  volumeMaster: { id: 'volumeMaster', task: 'R' },
  volumeLaneGain: { id: 'volumeLaneGain', task: 'R' },
  layoutFolds: { id: 'layoutFolds', task: 'R' },
  routingMirror: { id: 'routingMirror', task: 'R/IO' },
  deviceInputs: { id: 'deviceInputs', task: 'R/IO' },
  encoding: { id: 'encoding', task: 'R' },
  overviewPlan: { id: 'overviewPlan', task: 'R' },
  overviewDevice: { id: 'overviewDevice', task: 'R' },
  calibrationMeasure: { id: 'calibrationMeasure', task: 'C' },
  systemRestart: { id: 'systemRestart', task: 'R' },
  systemDaemonRestart: { id: 'systemDaemonRestart', task: 'R' },
  speakerTest: { id: 'speakerTest', task: 'R' }
}

export function gapLocaleOf(locale: string): GapLocale {
  return locale === 'en-US' ? 'en-US' : 'zh-CN'
}

export function gapTag(task: GapTask, locale: GapLocale): string {
  const template = locale === 'en-US' ? enUS.common.gapTask : zhCN.common.gapTask
  return template.replaceAll('{task}', task)
}

/** 值为空时给原因加上任务号；原因本身已含同一标签时不重复追加。 */
export function annotateGap(reason: string | null, task: GapTask, locale: GapLocale): string {
  const tag = gapTag(task, locale)
  if (reason === null || reason.length === 0) {
    return tag
  }
  if (reason.includes(tag)) {
    return reason
  }
  return reason + ' ' + tag
}

export function annotateGapById(reason: string | null, id: GapId, locale: GapLocale): string {
  return annotateGap(reason, CAPABILITY_GAPS[id].task, locale)
}
