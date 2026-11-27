/**
 * i18n 分片：mode（en-US）—— Mode page strings (namespace mode.* is exclusive to this shard).
 *
 * 规则（scripts/i18n-check.mjs 强制）：
 * - 只放本区域新增的键；不得重复定义基础文件/其它分片已有的键；
 * - zh-CN / en-US 两个分片必须键集完全对等（成对提交）；
 * - 源码只通过 t() 引用文案，不在 .vue 里硬编码。
 */

const messages = {
  mode: {
    pageTitle: 'Operating mode',
    subtitle:
      'Two-phase input enable (off / aes67): prepare only stages a draft, commit applies at a block boundary. The current mode comes from GET /api/status mode; when it is empty the page shows the reason and never substitutes the last commit receipt.',
    capability: {
      title: 'Surface capability',
      desc: 'From GET /api/meta surfaces / not_wired: not-wired capabilities show the reason and never render clickable fake actions (API.md §9.1).',
      wired: 'Wired',
      notWired: 'Not wired (not_wired)',
      unknown: 'Capability unknown (meta not reported or unavailable)',
      wiredHint: 'The write entry is available; use "Reload capability" after the capability changes.',
      notWiredHint: 'This surface is not wired yet: prepare is disabled and no fake action is rendered.',
      unknownHint:
        'Capability unknown: prepare can still be attempted, but the backend may answer 501 unsupported / 503 not_wired; errors are shown truthfully.',
      field: {
        surface: 'Surface',
        state: 'Capability state',
        reason: 'Not-wired reason'
      },
      reasonMissing: 'meta gave no reason',
      metaError: 'Failed to load meta ({code})',
      loading: 'Loading capability…',
      refresh: 'Reload capability'
    },
    current: {
      title: 'Current mode',
      desc: 'The current mode comes from the top-level mode field of GET /api/status. The latest commit receipt is listed separately and is not a device readback.',
      unavailable: 'Unreadable (no data source)',
      source: 'Source: GET /api/status mode (rt-host control status)',
      statusMissing: 'Runtime status has not been read yet: the current mode is unknown and the last commit receipt is not used in its place.',
      reason:
        'GET /api/status mode is empty and reasons.mode is missing. The page does not substitute the last commit receipt, a default, or host_link.mode.',
      receiptLabel: 'Last commit receipt (not a device readback)',
      receiptNote: 'This is the commit receipt for this session, not a device readback.',
      noReceipt: 'No mode commit receipt in this session yet.',
      mutedLabel: 'Muted (/api/status.state.muted)',
      mutedNote: 'Mute is an independent state: manual mute or mode=off can both make it true, so it cannot be used to infer the current mode.',
      mutedUnknown: '/api/status did not report the mute state.',
      mutedOn: 'Muted',
      mutedOff: 'Not muted'
    },
    target: {
      title: 'Target mode',
      desc: 'Two-phase: prepare stages a draft (running state unchanged), commit applies at a block boundary, abort discards the draft.',
      off: 'off (mute the whole chain)',
      aes67: 'aes67 / on (unmute; the only wired input source)',
      alias: 'The backend accepts on as an alias of aes67; receipts may say on and the page displays it as aes67.',
      analogReason:
        'analog / toslink / dante input-source switching is not implemented (the backend answers bad_request): the page does not offer these options.',
      selectHint: 'Select a target mode first.',
      prepared: 'Mode draft staged: review the draft card, then commit.',
      noopHint: 'prepare reported changed=false: the mode already matches, so commit is a no-op (no block applied).'
    },
    block: {
      capability: 'This surface is not wired: the write entry is disabled.',
      locked: 'Safe/faulted state: writes are disabled (read-only).',
      busy: 'A control operation is in progress; please wait.',
      noTarget: 'Select a target mode first.'
    },
    notice: {
      safeReadOnly: 'Safe mode (safe): writes are disabled.',
      safeReadOnlyReason: 'Safe mode (safe): writes are disabled (reason: {reason}).',
      faultedReadOnly: 'Faulted state (faulted): writes are disabled.',
      faultedReadOnlyReason: 'Faulted state (faulted): writes are disabled (reason: {reason}).',
      abortStillAvailable: 'A staged draft can still be discarded (abort only drops the draft and never changes the running state).',
      statusUnknown: 'Runtime status is unreadable: the safe/faulted write lock is unknown (writes are disabled only when safe/faulted is actually reported).',
      foreignDraft: 'The control plane already holds a draft for another surface (surface={surface}): this prepare will replace it (single pending slot).',
      noopCommit: 'This commit was a no-op (changed=false, applied_block not reported).',
      lockedDraft: 'Safe/faulted state: the staged draft cannot be committed (read-only); it can still be discarded (abort).'
    },
    receipt: {
      title: 'Mode commit receipt',
      desc: 'The commit receipt comes from the control plane (applied at a block boundary), not from a device readback; missing fields display "Not reported by the server".',
      empty: 'No mode commit receipt in this session yet.'
    }
  }
}

export default messages
