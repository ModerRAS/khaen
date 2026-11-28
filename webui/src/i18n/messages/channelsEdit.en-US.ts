/**
 * i18n 分片：channelsEdit（en-US）—— 方案与声道页的新两阶段文案，避免多人同时改巨型 i18n 文件。
 *
 * 规则（scripts/i18n-check.mjs 强制）：
 * - 本分片只放本区域新增的键；不得重复定义 src/i18n/en-US.ts 基础文件已有的键；
 * - zh-CN / en-US 两个分片必须键集完全对等（成对提交）；
 * - 顶层命名空间由本区域独占：channelsEdit.*（其它分片/基础文件不得再定义该顶层键）。
 *
 * 说明：基础文件 channels.* 里的过期里程碑文案（M3b/T52/T55）不在本页引用；本文件提供
 * 「PUT /api/channels 两阶段热重配」的真实文案，错误类别也重映射到 channelsEdit.error.*。
 */
const messages = {
  channelsEdit: {
    pageTitle: 'Plan & channels',
    pageSubtitle:
      'Current plan (/api/status + /api/plan/history), output channels and MOTU physical mapping, AES67 input (/api/channels); channel hot reconfiguration uses the PUT /api/channels two-phase flow (prepare → commit/abort). mapping/dac_map belong to the plan routing section (applied by plan prepare+commit); data-plane receipts are shown as reported, never faked.',
    unavailable: {
      title: '/api/channels unavailable',
      desc: 'The daemon failed to read the channel state: {message}. This page does not show fake maps or channel counts.',
      reason: 'not reported by the daemon (/api/channels unavailable)'
    },
    plan: {
      historyUnavailableHint: 'Plan history unavailable: the current version/rollback points cannot be read; history is not fabricated.'
    },
    input: {
      note:
        'AES67 receive parameters come from the rt-host data plane (channels_status.current); when the control channel does not report source (sdp/manual) or the SDP sha256, the page shows "not reported" instead of guessing.'
    },
    history: {
      unavailable: 'Plan history unavailable: {message}. History is not fabricated.'
    },
    hotswap: {
      title: 'Channel hot reconfiguration (PUT /api/channels two-phase)',
      desc:
        'prepare creates an rt-host draft (engine_token/diff/current/target); commit applies it at a block boundary. On failure rt-host rolls back and reports rolled_back. mapping/dac_map are the plan routing section: channels_commit cannot change them — a plan prepare+commit is required.',
      warningTitle: 'audio output is interrupted',
      warningDesc:
        'commit mutes and closes/reopens the device at a block boundary (the measured gap is the receipt gap_ms); on failure rt-host rolls back to the previous configuration.',
      capabilityLabel: 'hot reconfiguration capability (hot_reconfigure.wired)',
      capabilityWired: 'wired (wired=true)',
      capabilityNotWired: 'not wired (wired=false)',
      capabilityUnknown: 'cannot be verified (wired=null)',
      targetLabel: 'host_link target',
      generationLabel: 'current generation',
      busyLabel: 'reconfiguration in flight (busy)',
      pendingLabel: 'uncommitted draft (pending)',
      pendingYes: 'yes',
      pendingNo: 'no',
      lastGapLabel: 'last measured gap (last_gap_ms)',
      lastGapValue: '{ms} ms',
      lastResultLabel: 'last result (last_result)',
      lastResultNone: 'not reported by the server',
      deviceLabel: 'output device',
      probeLabel: 'device probe',
      deviceValue: '{name} ({state})',
      probeUnavailable: 'device probe unavailable',
      probeValue: '{rate} Hz / {channels} ch',
      sourceLabel: 'current value source',
      sourceValue:
        'data plane channels_status.current (rt-host is authoritative); mapping prefers the rt-host readback.routing and only falls back to the daemon-recorded plan routing.',
      mappingUnknown:
        'the current plan routing is unknown (mapping=null): DAC routes are not prefilled and passthrough is not assumed; after a format change, prepare and commit a plan that matches the new format.',
      formTitle: 'Target configuration (local draft)',
      sdpPlaceholder: 'Paste SDP text; rt-host parses it at prepare and replaces the AES67 group/port/PT/channels/map as a whole',
      sdpHint: 'A non-empty SDP replaces the AES67 receiver parameters as a whole, so the per-field AES67 inputs above do not apply; SDP and explicit fields are mutually exclusive.',
      field: {
        channels: 'output channels',
        sampleRate: 'sample rate (Hz)',
        sdp: 'Raw SDP (optional runtime receiver reload)',
        aes67Group: 'AES67 destination (multicast or unicast)',
        aes67Port: 'AES67 port',
        payloadType: 'RTP payload type',
        aes67Channels: 'AES67 receive channels',
        aes67Map: 'AES67 map (comma separated)',
        jitterFrames: 'jitter target frames',
        dacMap: 'plan routing (DAC → lane, comma separated; - = no lane)'
      },
      routingTitle: 'Plan routing (mapping/dac_map)',
      routingDesc:
        'This is the applied plan routing section (lane → DAC), not a data-plane channel field. When only this changes, PUT /api/channels returns commit_supported=false + blockers and this page disables commit; use plan upload → prepare → commit instead.',
      routingPlaceholder: 'e.g. 0,1,2,- (entry i = lane feeding DAC i; - means that DAC is silent)',
      resetDrafts: 'reset to current values',
      prepare: 'prepare',
      prepared: 'channel draft prepared (not committed)',
      commit: 'commit (hot reconfiguration)',
      abort: 'discard draft (abort)',
      discard: 'cancel locally (no server draft)',
      discarded: 'draft discarded locally (the server was not called)',
      aborted: 'server draft discarded (running configuration unchanged)',
      committed: 'hot reconfiguration committed (receipt below)',
      commitConfirmTitle: 'Commit channel hot reconfiguration?',
      commitConfirmMessage:
        'The device will be muted and closed/reopened at a block boundary; rt-host rolls back automatically on failure. Commit?',
      commitConfirmOk: 'commit',
      draftTitle: 'prepare draft (not committed)',
      receiptTitle: 'commit receipt',
      changedLabel: 'changed',
      changedYes: 'changed',
      changedNo: 'no change (no-op)',
      notReported: 'not reported by the server',
      formatLabel: 'target/applied format',
      formatValue: '{rate} Hz / {channels} ch / {frames} frames',
      graphActionLabel: 'graph action',
      graphDropped: 'dropped: commit drops the running graph; a plan matching the new format must be prepared and committed',
      graphKept: 'kept: engine format unchanged, only the AES67 input is rebuilt',
      graphValue: '{action}: {reason}',
      admissionLabel: 'admission',
      admissionAccepted: 'accepted',
      admissionRejected: 'rejected',
      admissionUnknown: 'not reported by the server',
      warningsTitle: 'warnings',
      blockersTitle: 'blockers (commit unavailable)',
      guidance:
        'commit_supported=false: this change will not be applied by hot reconfiguration. Upload a plan matching the new format and prepare → commit it (plan two-phase card), then prepare/commit the data-plane configuration here.',
      commitSupportedLabel: 'commit_supported',
      commitSupportedYes: 'yes',
      commitSupportedNo: 'no',
      engineTokenLabel: 'engine_token',
      plannedGenerationLabel: 'planned generation (planned_generation)',
      diffTitle: 'diff (reported by prepare)',
      currentTitle: 'current (reported by the server)',
      targetTitle: 'target (reported by the server)',
      detailsTitle: 'raw receipt (JSON)',
      appliedLabel: 'applied',
      appliedYes: 'applied',
      appliedNo: 'not applied',
      idempotentLabel: 'idempotent replay (idempotent)',
      commitGenerationLabel: 'applied generation',
      gapLabel: 'measured mute→resume gap (gap_ms)',
      gapValue: '{ms} ms',
      graphDroppedLabel: 'graph_dropped',
      rolledBackLabel: 'rolled_back',
      routingAppliedLabel: 'routing applied by this commit',
      routingAppliedYes: 'yes',
      routingAppliedNo: 'no (channels_commit does not change the plan routing)',
      planRecommitLabel: 'plan recommit required (plan_recommit_required)',
      noteLabel: 'rt-host note',
      commitAt: 'receipt time: {time}',
      commitFormatHint:
        'The receipt format/applied/generation are the data-plane truth; "the device changed format" holds only when rt-host reports it — this page does not infer beyond the receipt.',
      failureTitle: 'hot reconfiguration failed (502 reconfig_failed / ERR_RECONFIGURE_*)',
      failureCode: 'error code',
      failureStage: 'failed stage',
      failureRolledBack: 'rolled_back (previous configuration restored)',
      failureDeviceStarted: 'device_started',
      failureReceiverRunning: 'receiver_running',
      failureApplied: 'applied (must be false on failure)',
      failureGap: 'gap before failure (gap_ms)',
      failureGeneration: 'generation (does not advance on failure)',
      failureYes: 'yes',
      failureNo: 'no',
      failureUnknown: 'not reported by the server',
      validation: {
        channels_range: 'output channels must be an integer in 1–22',
        sample_rate_value: 'only 48000 / 96000 Hz are supported',
        sdp_value: 'SDP must not be empty',
        sdp_conflict: 'SDP and explicit AES67 fields are mutually exclusive: give either the SDP or the per-field parameters',
        aes67_group_value: 'the AES67 destination must be a concrete IPv4 address (multicast group or unicast local address; not 0.0.0.0/broadcast)',
        aes67_port_value: 'the AES67 port must be an integer in 1–65535',
        payload_type_value: 'the RTP payload type must be an integer in 96–127',
        aes67_channels_range: 'AES67 receive channels must be an integer in 1–8',
        aes67_map_length: 'the AES67 map length must equal the receive channel count',
        aes67_map_value: 'the AES67 map must contain unique slot indices below the output channel count',
        jitter_frames_value: 'the jitter target must be a non-negative integer',
        dac_map_length: 'the DAC map length must equal the output channel count (or omit channels and let it derive)',
        dac_map_value: 'the DAC map must contain unique non-negative lane indices (- means silent)',
        mapping_value: 'lane/DAC must be non-negative integers and unique',
        mapping_dac_map_conflict: 'mapping and dac_map describe different routes; give only one form',
        empty: 'change at least one field (the draft matches the current values)'
      }
    },
    error: {
      network: 'Cannot reach the daemon (backend down or network interrupted); check it and retry.',
      timeout: 'The channel/plan request timed out; retry.',
      unauthorized: 'Session expired; sign in again.',
      hostLinkUnavailable:
        'The daemon→rt-host control link is unavailable (503 host_link_unavailable): the write was not sent and the page will not show a fake success.',
      hostLinkUnsupported:
        'rt-host/daemon does not support this operation (501 host_link_unsupported): no token is issued and nothing is faked; hot reconfiguration is unavailable for the process target.',
      notWired: 'This capability is not wired (503 not_wired): the page shows "not wired" instead of inventing data.',
      notFound: 'Target resource not found (404).',
      invalidRequest: 'Invalid request parameters (400); check the form and retry.',
      invalidPlan: 'Invalid plan file (ABI v3 decode/structure check failed): the current plan is unchanged.',
      uploadTooLarge: 'The plan file exceeds the 4 MiB upload limit.',
      uploadNotFound: 'The uploaded plan does not exist (expired or upload failed); upload it again.',
      shaMismatch: 'The client and server sha256 differ: refusing to continue (neither side is trusted).',
      admissionRejected: 'The plan/configuration was rejected by admission (admission_rejected): the current configuration is unchanged.',
      noHistory: 'No historic version to roll back to (no_history).',
      planPathUnavailable:
        'The upload receipt has no plan_path (stored_path): inspect/prepare cannot run; the page does not guess a path or fake a draft.',
      conflict:
        'The current state does not allow this operation (409 busy/token/admission conflict): the draft may still be retryable or discardable; refresh and retry.',
      reconfigFailed:
        'Data-plane hot reconfiguration failed (502 reconfig_failed): rt-host rolled back as reported; the page shows rolled_back and other details verbatim and never a fake success.',
      commitUnsupported:
        'commit_supported=false: mapping/dac_map belong to the plan routing section and are not applied by this hot reconfiguration; use plan prepare+commit.',
      abortNotApplied: 'The server did not discard the draft (aborted=false): the running configuration is unchanged; refresh and retry.',
      server: 'The daemon returned a server error; retry later.',
      invalidResponse: 'The daemon response does not match docs/API.md; it is rejected instead of being shown (malformed responses are not treated as success).',
      unknown: 'Channel/plan operation failed; retry.'
    }
  }
}

export default messages
