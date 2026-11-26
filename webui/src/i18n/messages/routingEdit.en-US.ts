/**
 * i18n shard: routingEdit (en-US) — new copy for the routing page
 * (read-only diagnostics + editable two-phase).
 *
 * Rules (enforced by scripts/i18n-check.mjs):
 * - only new keys for this area; never redefine keys already in src/i18n/en-US.ts;
 * - zh-CN / en-US shards must carry exactly the same key set (committed as a pair);
 * - this area owns the top-level namespace: routingEdit.*
 */

const messages = {
  routingEdit: {
    subtitle:
      'Read-only diagnostics of input → 22-lane matrix → DAC outputs, plus editable two-phase lane → DAC routing: the current value comes from /api/channels mapping (the plan routing mirror daemon recorded at prepare/upload, not a live data-plane readback). Edits go through POST /api/surface {surface:"routing"} → POST /api/commit / POST /api/abort. When mapping=null the page shows "current value unknown" with the reason — old/default values are never passed off as current; optional receipts (changed/diff/…) are shown honestly as "not reported by server" when absent.',
    meta: {
      label: 'Capability (/api/meta surface=routing)',
      wired: 'Wired: prepare → commit is available',
      notWired: 'Not wired: backend reports the routing surface as unwired; prepare is disabled here (no fake success)',
      unknown: 'Capability unknown: /api/meta did not report the routing surface; prepare can still be attempted and the server receipt is authoritative',
      reason: 'Backend reason: {reason}'
    },
    current: {
      title: 'Current value source (read-only diagnostics)',
      desc: '/api/channels mapping is the plan routing mirror daemon recorded at prepare/upload (lane → 0-based DAC), not a live data-plane readback. When mapping=null the current value is unknown: this page shows an empty draft plus the reason and never passes off old/default values as current.',
      source: 'Data source',
      sourceValue: '/api/channels mapping (plan routing mirror)',
      sourceUnknown: 'No source (mapping not reported)',
      semantics: 'Semantics',
      semanticsValue: 'plan routing mirror recorded by daemon at prepare/upload, not a live data-plane readback',
      planVersion: 'Source plan version',
      graphId: 'Source graph_id',
      dacChannels: 'DAC channels (output.channels)',
      routes: 'mapping route entries',
      routingKnown: 'Daemon links the plan routing section',
      readAt: 'Read at',
      unknownTitle: 'Current value unknown (never substitute old/default values)',
      unknownReason: 'Backend gave no reason (/api/channels did not report reasons.mapping)',
      knownZero: 'Daemon reported a known 0 routes (mapping: []): this is not "unknown".',
      knownHint: 'The rows above prefill the editor; routes are submitted as a full replacement table.',
      staleHint: 'Refresh failed: showing the last successful snapshot ({message}).',
      unavailableTitle: '/api/channels unavailable: current value cannot be read',
      unavailableDesc:
        'Reading /api/channels failed ({message}): the lane → DAC mapping source is unavailable, so this page shows no fake mapping and never passes off old values as current.'
    },
    edit: {
      title: 'Editable lane → DAC table (two-phase)',
      desc: 'Edit the full lane → DAC replacement table: local validation → POST /api/surface {surface:"routing", data:{routes, dac_channels?}} creates a draft → POST /api/commit applies it / POST /api/abort discards it. prepare only creates a draft and does not change the running state.',
      toolbar: {
        prefill: 'Prefill from mapping',
        add: 'Add lane',
        clear: 'Clear draft',
        count: '{count} rows'
      },
      table: {
        lane: 'lane',
        dac: 'DAC (0-based)',
        output: 'MOTU output',
        role: 'Role (canonical, from /api/channels.speaker_roles)',
        actions: 'Actions',
        remove: 'Remove this row'
      },
      noRows: 'Draft is empty: click "Add lane". New rows default to DAC 0 — confirm each row.',
      unknownCurrent: 'Current value unknown: cannot prefill from /api/channels.mapping, so add lane → DAC rows manually. Reason: {reason}',
      prefilled: 'Prefilled from /api/channels.mapping ({time}).',
      editedHint: 'The draft was edited manually (refresh does not overwrite it); click "Prefill from mapping" to rebuild it.',
      laneHint: 'The matrix domain is designed for {lanes} lanes; out-of-range lanes are rejected by backend admission (frontend does not guess the lane count).',
      dacChannels: 'DAC channels (dac_channels)',
      explicitDac: 'Send dac_channels explicitly',
      inheritDac: 'Not explicit: inherit the current plan value (dac_channels is not sent)',
      inheritDacValue: 'Current plan value (output.channels): {value}',
      dacChannelsUnknown: 'Current DAC channel count unknown: tick "Send dac_channels explicitly" and enter a positive integer, or keep inheriting the plan value.',
      dacChannelsHint: 'dac_channels only applies to this patch; when unticked the field is not sent (API.md §4 routing data).',
      prepare: 'Prepare draft (prepare)',
      preparing: 'Preparing…',
      prepareBlockedMeta: 'Backend reports the routing surface as unwired (not_wired): prepare is not sent.',
      prepareBlockedMetaReason: 'Reason: {reason}',
      prepareBlockedDraft: 'A routing draft already exists: commit or abort it before preparing again.',
      foreignDraft: 'The control plane already holds a {surface} draft (from another page): prepare here will replace it (commit/abort first is safer).',
      validation: {
        empty: 'At least one lane → DAC row is required',
        entry: 'lane / DAC must be non-negative integers',
        duplicate: 'The same lane cannot appear twice',
        dac_channels: 'dac_channels must be a positive integer',
        dac_range: 'DAC indexes must be below the DAC channel count ({channels})'
      }
    },
    draft: {
      title: 'Draft review (prepare receipt)',
      desc: 'changed / diff / current / target / graph / admission are optional receipt fields: when the server does not report them the page says so honestly instead of inventing values; commit only sends engine_token.',
      empty: 'No routing draft yet: validate the editor, then click "Prepare draft".',
      abortHint: 'abort only discards the server-side draft (it does not change the running state); on failure the draft is kept and the stable error code is shown.'
    },
    receipt: {
      title: 'Commit receipt',
      desc: 'Shown from the server receipt (version / applied_block / graph_id / changed); nothing is fabricated locally.',
      empty: 'This page has no successful commit receipt yet.',
      version: 'Version',
      appliedBlock: 'Applied block',
      graphId: 'graph_id',
      changed: 'changed',
      changedUnreported: 'Not reported by server',
      at: 'Committed at'
    },
    error: {
      noBasePlanGuide:
        'No applied plan (no_base_plan): a routing patch needs an applied plan as its base. Prepare + commit a plan on the "Plan & channels" page first, then come back to edit routing.',
      goChannels: 'Open "Plan & channels"',
      tokenInvalidGuide:
        'engine_token invalid or the draft was replaced (engine_token_invalid): the local draft is gone, prepare again.'
    },
    plan: {
      segmentsReason:
        'No read-only endpoint reports the applied plan section list/count: daemon only reports segments/segment_count in the plan upload receipt. This page never passes off the route count as a section count.',
      segmentsNote:
        'Section counts (content/crossover/matrix/protection/routing) are only visible in the plan upload receipt; this page honestly shows "not wired".'
    },
    toast: {
      prefilled: 'Draft prefilled from /api/channels.mapping (unavailable when the current value is unknown)',
      prepared: 'Routing draft prepared (not applied yet)',
      committed: 'Routing patch committed and applied',
      aborted: 'Routing draft discarded'
    }
  }
}

export default messages
