// 运行时状态共享轮询（REST 是唯一 actual 来源；WS 只是可选叠加）。
import { reactive } from 'vue'
import { onMounted, onUnmounted } from 'vue'
import { api } from '@/api/client'
import { errorText, ApiError } from '@/api/client'
import type { RuntimeStatus } from '@/api/types'

export const runtimeState = reactive({
  status: null as RuntimeStatus | null,
  loading: false,
  error: null as string | null,
  errorCode: '' as string,
  lastUpdated: 0,
  lastAttempt: 0
})

export async function refreshRuntime(): Promise<RuntimeStatus | null> {
  runtimeState.loading = true
  runtimeState.lastAttempt = Date.now()
  try {
    const status = await api.runtimeStatus()
    runtimeState.status = status
    runtimeState.error = null
    runtimeState.errorCode = ''
    runtimeState.lastUpdated = Date.now()
    return status
  } catch (error) {
    runtimeState.error = errorText(error)
    runtimeState.errorCode = error instanceof ApiError ? error.code : ''
    return null
  } finally {
    runtimeState.loading = false
  }
}

let timer: number | null = null
let refCount = 0
let interval = 5000
let visibilityBound = false

function tick(): void {
  if (document.hidden) { return }
  void refreshRuntime()
}

function start(): void {
  if (timer !== null) { return }
  timer = window.setInterval(tick, interval)
  if (!visibilityBound) {
    document.addEventListener('visibilitychange', onVisible)
    visibilityBound = true
  }
}

function stop(): void {
  if (timer !== null) { window.clearInterval(timer); timer = null }
}

function onVisible(): void {
  if (!document.hidden) { void refreshRuntime() }
}

/** 组件内调用：引用计数式启停轮询（避免每个页面各自一份定时器）。 */
export function useRuntimePolling(everyMs = 5000): void {
  interval = everyMs
  onMounted(() => {
    refCount += 1
    start()
    void refreshRuntime()
  })
  onUnmounted(() => {
    refCount -= 1
    if (refCount <= 0) { refCount = 0; stop() }
  })
}
