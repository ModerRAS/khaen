// 响应式断点（与任务要求一致）：>=1200 桌面 / 768–1199 平板 / <768 手机。
import { computed, onMounted, onUnmounted, reactive } from 'vue'

export const viewport = reactive({
  width: typeof window === 'undefined' ? 1440 : window.innerWidth,
  height: typeof window === 'undefined' ? 900 : window.innerHeight
})

let bound = false

function update(): void {
  viewport.width = window.innerWidth
  viewport.height = window.innerHeight
}

export function bindViewport(): void {
  if (bound) { return }
  bound = true
  update()
  window.addEventListener('resize', update, { passive: true })
  window.addEventListener('orientationchange', update, { passive: true })
}

export function useViewport() {
  onMounted(bindViewport)
  const isMobile = computed(() => viewport.width < 768)
  const isTablet = computed(() => viewport.width >= 768 && viewport.width < 1200)
  const isDesktop = computed(() => viewport.width >= 1200)
  return { viewport, isMobile, isTablet, isDesktop }
}

/** 简单防抖，用于输入类交互。 */
export function debounce<T extends (...args: never[]) => void>(fn: T, delay = 300): T {
  let handle: number | null = null
  return ((...args: never[]) => {
    if (handle !== null) { window.clearTimeout(handle) }
    handle = window.setTimeout(() => fn(...args), delay)
  }) as T
}

export function onUnmountCleanup(cleanup: () => void): void {
  onUnmounted(cleanup)
}
