/**
 * i18n 分片：layout（en-US）—— Layout page strings (namespace layout.* is exclusive to this shard).
 *
 * 规则（scripts/i18n-check.mjs 强制）：
 * - 只放本区域新增的键；不得重复定义基础文件/其它分片已有的键；
 * - zh-CN / en-US 两个分片必须键集完全对等（成对提交）；
 * - 源码只通过 t() 引用文案，不在 .vue 里硬编码。
 */

const messages = {
  layout: {
    pageTitle: 'Content layout',
    subtitle:
      'Two-phase content fold table (folds) editing: prepare stages a patch draft and commit applies at a block boundary. There is no readback source for the current layout, so the form is a local draft.',
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
      title: 'Current content layout',
      desc: 'The current content layout comes from /api/status.readback (control-plane base_plan); when readback is unavailable the page shows why and never passes defaults off as current.',
      unavailable: 'Unreadable (readback unavailable)',
      reason:
        '/api/status.readback has no usable base_plan (rt-host loaded/committed no plan, or the link is degraded); the page never guesses it or passes a default value off as current.',
      note: 'The numbers above come from readback; the form below is a local draft whose defaults are only an editing starting point, not the current layout.',
      field: {
        inChannels: 'Input channels',
        outLanes: 'Output lanes',
        folds: 'Fold count',
        source: 'Data source'
      },
      sourceValue: 'base_plan readback ({source})'
    },
    form: {
      title: 'Fold table (local draft)',
      desc:
        'in_channels / out_lanes / folds:[[src,dst,gain],...] is a full replacement table: shape is validated locally, then prepare hands it to backend admission.',
      inChannels: 'Input channels (in_channels)',
      outLanes: 'Output lanes (out_lanes)',
      foldsTitle: 'Folds',
      foldRow: 'Fold {index}',
      src: 'Source channel (src)',
      dst: 'Target lane (dst)',
      gain: 'Gain (gain)',
      addFold: 'Add fold',
      removeFold: 'Remove',
      count: 'Fold entries: {count}',
      hint: 'gain is a linear gain (entries for the same dst are summed); out-of-range src/dst and an empty table are rejected by backend admission, the frontend only validates shape.',
      localDraft: 'Local draft: this is not the current layout (no data source for the current values).',
      prepared: 'Layout patch draft staged: review the draft card, then commit.'
    },
    validation: {
      channels: 'in_channels / out_lanes must be positive integers.',
      empty: 'folds must not be empty: keep at least one fold entry (the backend rejects an empty table too).',
      entry: 'Each fold entry must be [src, dst, gain]: src/dst are non-negative integers and gain is finite.'
    },
    block: {
      capability: 'This surface is not wired: the write entry is disabled.',
      locked: 'Safe/faulted state: writes are disabled (read-only).',
      busy: 'A control operation is in progress; please wait.',
      invalid: 'Local validation failed: fix the form before prepare.'
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
    draftFields: {
      title: 'prepare receipt details (optional fields)',
      desc: 'changed / diff / current / target are optional receipt fields: when the server does not report them the page says "Not reported by the server" instead of inventing values.',
      changed: 'Changed',
      diff: 'Diff',
      current: 'Current value',
      target: 'Target value'
    },
    receipt: {
      title: 'Layout commit receipt',
      desc: 'The commit receipt comes from the control plane (applied at a block boundary), not from a device readback; missing fields display "Not reported by the server".',
      empty: 'No layout commit receipt in this session yet.'
    },
    noBasePlan: {
      title: 'No committed plan yet (no_base_plan)',
      guide:
        'A layout patch must be based on the most recently committed plan: go to the Channels page and prepare + commit a plan first, then come back to stage the layout patch.',
      goto: 'Go to Channels',
      detected: '/api/status shows no committed plan version (plan.version is empty): layout prepare is expected to answer no_base_plan.'
    }
  }
}

export default messages
