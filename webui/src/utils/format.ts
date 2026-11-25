// 展示层格式化（只读；不改语义、不推断 actual）。
export function shortHash(value?: string | null, len = 16): string {
  if (!value) { return '—' }
  return value.length > len ? `${value.slice(0, len)}…` : value
}

export function formatDb(value?: number | null): string {
  if (value === null || value === undefined || !Number.isFinite(value)) { return '—' }
  return `${value.toFixed(1)} dB`
}

export function formatUnix(value?: number | null): string {
  if (!value || !Number.isFinite(value)) { return '—' }
  const date = new Date(value * 1000)
  if (Number.isNaN(date.getTime())) { return '—' }
  return date.toLocaleString('zh-CN', { hour12: false })
}

export function formatClock(value?: number | null): string {
  if (!value) { return '—' }
  return new Date(value).toLocaleTimeString('zh-CN', { hour12: false })
}

export function formatBytes(value?: number | null): string {
  if (value === null || value === undefined || !Number.isFinite(value)) { return '—' }
  if (value < 1024) { return `${value} B` }
  if (value < 1024 * 1024) { return `${(value / 1024).toFixed(1)} KiB` }
  return `${(value / (1024 * 1024)).toFixed(2)} MiB`
}

export function jsonPretty(value: unknown): string {
  try { return JSON.stringify(value, null, 2) } catch { return String(value) }
}

export function runtimeStateClass(state?: string | null): string {
  switch (state) {
    case 'Running': return 'chip--ok'
    case 'SafetyMute': return 'chip--warn'
    case 'Faulted': return 'chip--danger'
    case 'Draining': return 'chip--warn'
    case 'Stopped': return 'chip--danger'
    default: return ''
  }
}

export function jobStateClass(state?: string | null): string {
  switch (state) {
    case 'complete': return 'chip--ok'
    case 'failed': return 'chip--danger'
    case 'cancelled':
    case 'rolled_back': return 'chip--warn'
    case 'prepared': return 'chip--info'
    default: return ''
  }
}

export const JOB_TERMINAL_STATES = ['complete', 'failed', 'cancelled', 'rolled_back']
export const JOB_STEPS = [
  'queued', 'preflight', 'sweep_capture', 'quality_check', 'fit_p',
  'solve_g', 'compile', 'prepared', 'apply', 'remeasure',
  'independent_holdout', 'complete'
] as const

export const JOB_STEP_LABELS: Record<string, string> = {
  queued: '排队',
  preflight: '预检',
  sweep_capture: '扫频采集',
  quality_check: '质量检查',
  fit_p: 'PEQ 拟合',
  solve_g: '求解 G',
  compile: '编译',
  prepared: '已准备（待 apply）',
  apply: '应用',
  remeasure: '复测',
  independent_holdout: '独立留出验证',
  complete: '完成',
  failed: '失败',
  cancelled: '已取消',
  rolled_back: '已回滚'
}

export function isTerminalJob(state?: string | null): boolean {
  return !!state && JOB_TERMINAL_STATES.includes(state)
}
