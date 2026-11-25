// WS 实时通道（现有 T24 兼容子集）。预览代理未反代 /ws 时必须显式降级，不假装已连接。
import { reactive } from 'vue'
import { api } from '@/api/client'
import { errorText } from '@/api/client'

export type WsPhase = 'idle' | 'connecting' | 'open' | 'closed' | 'unsupported' | 'error'

export const wsState = reactive({
  phase: 'idle' as WsPhase,
  detail: '',
  log: [] as string[],
  lastMessageAt: 0,
  values: {} as Record<string, unknown>,
  sent: 0,
  received: 0
})

let socket: WebSocket | null = null
let pollTimer: number | null = null

function pushLog(line: string): void {
  const stamp = new Date().toLocaleTimeString('zh-CN', { hour12: false })
  wsState.log.push(`[${stamp}] ${line}`)
  if (wsState.log.length > 60) { wsState.log.splice(0, wsState.log.length - 60) }
}

export function wsUrl(): string {
  const scheme = window.location.protocol === 'https:' ? 'wss' : 'ws'
  return `${scheme}://${window.location.host}/ws`
}

export function connectWs(): void {
  disconnectWs()
  if (!window.WebSocket) {
    wsState.phase = 'unsupported'
    wsState.detail = '浏览器不支持 WebSocket'
    return
  }
  wsState.phase = 'connecting'
  wsState.detail = wsUrl()
  pushLog(`连接 ${wsUrl()}`)
  let failed = false
  try {
    socket = new WebSocket(wsUrl())
  } catch (error) {
    wsState.phase = 'error'
    wsState.detail = errorText(error)
    pushLog(`创建失败: ${wsState.detail}`)
    return
  }
  socket.onopen = () => {
    wsState.phase = 'open'
    wsState.detail = '已连接（T24 兼容子集）'
    pushLog('open')
    queue(['GetVersion', 'GetConfigVersion', 'GetCalibrationJobs'])
    pollTimer = window.setInterval(() => {
      queue(['GetState', 'GetVolume', 'GetMute', 'GetCaptureRate', 'GetClippedSamples', 'GetConfigVersion', 'GetCalibrationJobs'])
    }, 2000)
  }
  socket.onmessage = (event: MessageEvent) => {
    wsState.received += 1
    wsState.lastMessageAt = Date.now()
    try {
      const parsed = JSON.parse(String(event.data)) as Record<string, { result?: string; value?: unknown }>
      const key = Object.keys(parsed)[0]
      if (!key) { return }
      const body = parsed[key] ?? {}
      if (body.result === 'Ok') {
        wsState.values[key] = body.value
        pushLog(`${key} -> Ok`)
      } else {
        pushLog(`${key} -> ${String(body.result)}`)
      }
    } catch {
      pushLog('收到非 JSON 消息')
    }
  }
  socket.onerror = () => {
    failed = true
    wsState.phase = 'error'
    wsState.detail = 'WS 传输错误（预览代理可能返回 501，已降级为 REST）'
    pushLog('transport error（代理未反代 /ws 时为预期降级）')
  }
  socket.onclose = (event: CloseEvent) => {
    if (pollTimer !== null) { window.clearInterval(pollTimer); pollTimer = null }
    socket = null
    if (!failed) {
      if (event.code === 1006) {
        wsState.phase = 'unsupported'
        wsState.detail = `WS 关闭 code=1006（代理未反代 /ws，已降级为 REST 轮询）`
      } else {
        wsState.phase = 'closed'
        wsState.detail = `WS 已断开 code=${event.code}`
      }
      pushLog(`close code=${event.code}`)
    }
  }
}

function queue(commands: string[]): void {
  if (!socket || socket.readyState !== WebSocket.OPEN) { return }
  for (const command of commands) {
    socket.send(JSON.stringify(command))
    wsState.sent += 1
  }
}

export function disconnectWs(): void {
  if (pollTimer !== null) { window.clearInterval(pollTimer); pollTimer = null }
  if (socket) {
    try { socket.close(1000, 'user_disconnect') } catch { /* ignore */ }
    socket = null
  }
  if (wsState.phase !== 'error' && wsState.phase !== 'unsupported') {
    wsState.phase = 'idle'
    wsState.detail = '未连接'
  }
}

export function clearWsLog(): void {
  wsState.log.splice(0, wsState.log.length)
}

/** 通过 REST 探测 /ws 是否可用（代理 501 时明确「不支持」）。 */
export async function probeWsSupport(): Promise<boolean> {
  const controller = new AbortController()
  const timer = window.setTimeout(() => controller.abort(), 3000)
  try {
    const response = await fetch('/ws', { method: 'GET', credentials: 'same-origin', signal: controller.signal })
    if (response.status === 501) {
      wsState.phase = 'unsupported'
      wsState.detail = '预览代理未反代 /ws（501）；WS 实时状态不可用，页面已降级为 REST 轮询'
      return false
    }
    return true
  } catch {
    wsState.phase = 'unsupported'
    wsState.detail = '无法探测 /ws（3s 超时或网络错误）'
    return false
  } finally {
    window.clearTimeout(timer)
  }
}