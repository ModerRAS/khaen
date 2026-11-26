/**
 * MOTU 快照轮询接线（音量页）：页面挂载开始 5s 轮询；visibilitychange 暂停/恢复；卸载停止。
 *
 * 与 composables/useStatusPolling.ts 同模式：定时器/请求逻辑在 stores/motu.ts（纯逻辑、可单测），
 * 这里只做 Vue 生命周期 + 浏览器事件接线，避免 setInterval 泄漏。
 */
import { onBeforeUnmount, onMounted } from 'vue'
import {
  motuPollState,
  pauseMotuPolling,
  refreshMotu,
  resumeMotuPolling,
  startMotuPolling,
  stopMotuPolling,
  type MotuPollState
} from '../../stores/motu.ts'

function isDocumentHidden(): boolean {
  return typeof document !== 'undefined' && document.visibilityState === 'hidden'
}

export interface UseMotuPollingResult {
  state: MotuPollState
  refresh: () => Promise<void>
}

export function useMotuPolling(): UseMotuPollingResult {
  function onVisibilityChange(): void {
    if (isDocumentHidden()) {
      pauseMotuPolling()
      return
    }
    resumeMotuPolling()
  }

  onMounted(() => {
    document.addEventListener('visibilitychange', onVisibilityChange)
    if (isDocumentHidden()) {
      // 首屏就隐藏（后台标签页）：先挂起，等可见事件再拉取，避免无意义请求。
      startMotuPolling()
      pauseMotuPolling()
      return
    }
    startMotuPolling()
  })

  onBeforeUnmount(() => {
    document.removeEventListener('visibilitychange', onVisibilityChange)
    stopMotuPolling()
  })

  return { state: motuPollState, refresh: refreshMotu }
}
