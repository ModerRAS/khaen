/**
 * 格式页 AES67 当前值：只认 GET /api/channels 的 current / input。
 *
 * /api/status 的 format.aes67 在 daemon 里恒为 null，不能拿来显示，也不能在 channels
 * 不可用时回退过去。
 */
import { reasonForChannelsPath, type ChannelsInfo } from './channels.ts'

export interface Aes67Fields {
  channels: number | null
  map: number[] | null
  pt: number | null
  group: string | null
  port: number | null
}

export interface Aes67View {
  fields: Aes67Fields
  reasons: Record<keyof Aes67Fields, string | null>
  /** true = /api/channels 没有可用响应；reasons 是调用方给出的不可用原因。 */
  unavailable: boolean
}

function emptyFields(): Aes67Fields {
  return { channels: null, map: null, pt: null, group: null, port: null }
}

/**
 * 从 channels 响应取出 AES67 当前值。
 * info 为 null 时五个字段都用 unavailableReason，不编造组地址或端口。
 */
export function aes67FromChannels(info: ChannelsInfo | null, unavailableReason: string | null): Aes67View {
  if (info === null) {
    return {
      fields: emptyFields(),
      reasons: {
        channels: unavailableReason,
        map: unavailableReason,
        pt: unavailableReason,
        group: unavailableReason,
        port: unavailableReason
      },
      unavailable: true
    }
  }
  const current = info.current
  const channels = current?.aes67_channels ?? info.input.channels
  const map = current?.aes67_map ?? info.input.map
  const pt = current?.aes67_payload_type ?? null
  const group = current?.aes67_group ?? null
  const port = current?.aes67_port ?? null
  const currentMissing =
    current === null ? reasonForChannelsPath(info, 'current') ?? unavailableReason : null
  function missing(value: unknown, path: string): string | null {
    if (value !== null && value !== undefined) {
      return null
    }
    return reasonForChannelsPath(info, path) ?? currentMissing
  }
  return {
    fields: { channels, map, pt, group, port },
    reasons: {
      channels: missing(channels, 'input.channels'),
      map: missing(map, 'input.map'),
      pt: missing(pt, 'current.aes67_payload_type'),
      group: missing(group, 'current.aes67_group'),
      port: missing(port, 'current.aes67_port')
    },
    unavailable: false
  }
}
