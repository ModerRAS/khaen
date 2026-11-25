// 内容布局 / 中置下混（GET/PUT /api/content/layout，task-316 契约 khaen-host-content/v1）。
// 错误态如实分类：501 not_wired → 「待接线（host 需 plan-arena 图）」并禁用控件，绝不标 actual；
// 503 layout_presets_unavailable / 503 host_unavailable / 409 content_busy / 504 content_timeout 各自显示原因。
import { reactive } from 'vue'
import { api, ApiError, errorText } from '@/api/client'
import type { ContentLayoutAck, ContentLayoutBody, ContentLayoutCurrent, ContentLayoutDraft } from '@/api/types'

export type ContentPhase =
  | 'unknown' | 'loading' | 'ok' | 'not_wired' | 'presets_unavailable'
  | 'unavailable' | 'busy' | 'timeout' | 'error'

export const contentState = reactive({
  phase: 'unknown' as ContentPhase,
  body: null as ContentLayoutBody | null,
  current: null as ContentLayoutCurrent | null,
  message: '',
  errorCode: '',
  saving: false,
  lastAck: null as ContentLayoutAck | null
})

export function classifyContentError(error: unknown): { phase: ContentPhase; message: string; code: string } {
  if (!(error instanceof ApiError)) {
    return { phase: 'error', message: String(error), code: 'error' }
  }
  const code = error.code || `http_${error.status}`
  const message = errorText(error)
  if (code === 'not_wired' || error.status === 501 || code === 'not_found' || error.status === 404) {
    return { phase: 'not_wired', message, code }
  }
  if (code === 'layout_presets_unavailable') { return { phase: 'presets_unavailable', message, code } }
  if (code === 'content_busy' || error.status === 409) { return { phase: 'busy', message, code } }
  if (code === 'host_unavailable') { return { phase: 'unavailable', message, code } }
  if (code === 'content_timeout' || error.status === 504) { return { phase: 'timeout', message, code } }
  return { phase: 'error', message, code }
}

export const CONTENT_NOT_WIRED_HINT = '待接线（host 需 plan-arena 图）'

export function contentPhaseText(phase: ContentPhase): string {
  switch (phase) {
    case 'not_wired': return CONTENT_NOT_WIRED_HINT
    case 'presets_unavailable': return '布局预设不可用'
    case 'busy': return '内容路径忙（content_busy）'
    case 'unavailable': return 'host 不可用（host_unavailable）'
    case 'timeout': return '内容适配超时（content_timeout）'
    case 'ok': return '已接线'
    case 'loading': return '读取中'
    case 'error': return '读取失败'
    default: return '未读取'
  }
}

export function presetInputList(body: ContentLayoutBody | null): string[] {
  const presets = body?.presets
  if (!presets || presets.available !== true) { return [] }
  return Array.isArray(presets.input) ? presets.input : []
}

export function presetOutputList(body: ContentLayoutBody | null): string[] {
  const presets = body?.presets
  if (!presets || presets.available !== true) { return [] }
  return Array.isArray(presets.output) ? presets.output : []
}

export function presetOutputError(body: ContentLayoutBody | null): string {
  const presets = body?.presets
  if (!presets) { return '' }
  const output = presets.output
  if (output && !Array.isArray(output) && output.available === false) { return output.error }
  if (presets.available === false) { return presets.error ?? '' }
  return ''
}

/** 由 current 构造可编辑草稿。 */
export function draftFromCurrent(current: ContentLayoutCurrent | null): ContentLayoutDraft {
  if (!current) {
    return { input_layout: '', output_layout: '', center: 'auto', program_peak_dbfs: -18 }
  }
  return {
    input_layout: current.input_layout,
    output_layout: current.output_layout,
    center: current.center,
    program_peak_dbfs: current.program_peak_dbfs
  }
}

export async function loadContentLayout(): Promise<void> {
  contentState.phase = 'loading'
  contentState.message = ''
  contentState.errorCode = ''
  try {
    const body = await api.contentLayoutGet()
    contentState.body = body
    contentState.current = body.current ?? null
    const presets = body.presets
    const outputError = presets && presets.output && !Array.isArray(presets.output) && presets.output.available === false
      ? presets.output.error
      : ''
    if (presets && presets.available !== true) {
      // GET 成功但 presets 不可用（后端也可能直接 503 layout_presets_unavailable）
      contentState.phase = 'presets_unavailable'
      contentState.message = presets.error || outputError || 'layout presets unavailable'
      contentState.errorCode = 'layout_presets_unavailable'
    } else {
      contentState.phase = 'ok'
    }
  } catch (error) {
    const classified = classifyContentError(error)
    contentState.phase = classified.phase
    contentState.message = classified.message
    contentState.errorCode = classified.code
  }
}

/** 保存/停用；返回 unsupported 表示后端未接线（不静默成功）。 */
export async function saveContentLayout(draft: ContentLayoutDraft): Promise<'actual' | 'synthetic' | 'unsupported' | 'error'> {
  contentState.saving = true
  try {
    const ack = await api.contentLayoutPut(draft)
    contentState.lastAck = ack
    contentState.body = { ...(contentState.body ?? { current: ack.current }), current: ack.current }
    contentState.current = ack.current
    contentState.phase = 'ok'
    contentState.message = ''
    contentState.errorCode = ''
    return ack.actual === true ? 'actual' : 'synthetic'
  } catch (error) {
    const classified = classifyContentError(error)
    contentState.phase = classified.phase
    contentState.message = classified.message
    contentState.errorCode = classified.code
    return classified.phase === 'not_wired' ? 'unsupported' : 'error'
  } finally {
    contentState.saving = false
  }
}