/**
 * 概览页状态轮询接线（PLAN §6.2；任务 T35b）。
 *
 * - 页面挂载：开始 5s 轮询（立即拉一次）；
 * - visibilitychange：页面隐藏 -> pause（停止定时器 + 中止在途请求）；重新可见 -> resume（立即刷新再继续定时）；
 * - 页面卸载：stop（离开概览页后不再发请求）；
 * - 返回共享 statusState 与 refresh，供概览页/顶栏消费。
 *
 * 定时器/请求逻辑都在 stores/status.ts（纯逻辑、可单测），这里只做 Vue 生命周期 + 浏览器事件接线。
 */
import { onBeforeUnmount, onMounted } from 'vue'
import {
  pauseStatusPolling,
  refreshStatus,
  resumeStatusPolling,
  startStatusPolling,
  statusState,
  stopStatusPolling,
  type StatusState
} from '@/stores/status'

function isDocumentHidden(): boolean {
  return typeof document !== 'undefined' && document.visibilityState === 'hidden'
}

export interface UseStatusPollingResult {
  state: StatusState
  refresh: () => Promise<void>
}

export function useStatusPolling(): UseStatusPollingResult {
  function onVisibilityChange(): void {
    if (isDocumentHidden()) {
      pauseStatusPolling()
      return
    }
    resumeStatusPolling()
  }

  onMounted(() => {
    document.addEventListener('visibilitychange', onVisibilityChange)
    if (isDocumentHidden()) {
      // 首屏就隐藏（后台标签页）：先挂起，等可见事件再拉取，避免无意义的请求。
      startStatusPolling()
      pauseStatusPolling()
      return
    }
    startStatusPolling()
  })

  onBeforeUnmount(() => {
    document.removeEventListener('visibilitychange', onVisibilityChange)
    stopStatusPolling()
  })

  return { state: statusState, refresh: refreshStatus }
}
