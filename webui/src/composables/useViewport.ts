/**
 * 视口断点（PLAN §11.3：XL >=1440 / L 1280–1439 / M 768–1279 / S <768）。
 * 只读 window.innerWidth + resize 监听；SSR 不需要（daemon 只托管静态 SPA）。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'

export type Breakpoint = 'S' | 'M' | 'L' | 'XL'

function readWidth(): number {
  return typeof window === 'undefined' ? 1440 : window.innerWidth
}

export function useViewport() {
  const width = ref<number>(readWidth())

  function onResize(): void {
    width.value = readWidth()
  }

  onMounted(() => {
    window.addEventListener('resize', onResize, { passive: true })
    onResize()
  })

  onBeforeUnmount(() => {
    window.removeEventListener('resize', onResize)
  })

  const breakpoint = computed<Breakpoint>(() => {
    if (width.value >= 1440) {
      return 'XL'
    }
    if (width.value >= 1280) {
      return 'L'
    }
    if (width.value >= 768) {
      return 'M'
    }
    return 'S'
  })

  /** <768：侧栏变抽屉、卡片单列。 */
  const isMobile = computed(() => breakpoint.value === 'S')
  /** <1280：侧栏强制折叠为 64px（不做抽屉）。 */
  const isNarrow = computed(() => width.value < 1280)

  return { width, breakpoint, isMobile, isNarrow }
}
