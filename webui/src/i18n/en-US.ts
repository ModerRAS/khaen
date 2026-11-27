/**
 * en-US —— 与 zh-CN 键集**完全对等**（类型 Messages 强制；scripts/i18n-check.mjs 复核）。
 * 英文通常比中文长：新增文案时注意窄屏（390px）不截断/不溢出（PLAN §11.6）。
 */
import type { Messages } from './zh-CN'

const messages: Messages = {
  app: {
    name: 'khaen',
    tagline: 'Real-time audio DSP engine console',
    version: 'Version {version}'
  },
  common: {
    dash: '—',
    notWired: 'Not wired',
    refresh: 'Refresh',
    cancel: 'Cancel',
    confirm: 'Confirm',
    close: 'Close',
    loading: 'Loading',
    unknown: 'Unknown',
    yes: 'Yes',
    no: 'No',
    none: 'None',
    retry: 'Retry',
    gapTask: '(task {task})'
  },
  nav: {
    overview: 'Overview',
    volume: 'Volume & mute',
    mode: 'Operating mode',
    channels: 'Plan & channels',
    routing: 'Routing matrix',
    layout: 'Content layout',
    speakers: "Speaker layout",
    format: 'Audio format',
    calibration: 'Calibration',
    system: 'System',
    logs: 'Logs'
  },
  navGroup: {
    main: 'Main',
    system: 'System'
  },
  shell: {
    collapse: 'Collapse sidebar',
    expand: 'Expand sidebar',
    menu: 'Open navigation',
    hostLinkNotWired: 'Control link not wired',
    hostLinkConnected: 'Control link connected',
    hostLinkStale: 'Control link status is stale',
  },
  topbar: {
    runState: 'Run state',
    plan: 'Plan version',
    deviceRate: 'Device rate',
    changePassword: 'Change password',
    logout: 'Sign out',
    userMenu: 'User menu',
    themeToggle: 'Toggle dark / light',
    version: 'Version',
    versionHint: 'Release version (from daemon /healthz); no value is shown while it is unknown'
  },
  theme: {
    label: 'Theme',
    system: 'Follow system',
    dark: 'Dark',
    light: 'Light',
    toggleToDark: 'Switch to dark',
    toggleToLight: 'Switch to light'
  },
  locale: {
    label: 'Language',
    zhCN: '简体中文',
    enUS: 'English'
  },
  status: {
    running: 'Running',
    faulted: 'Faulted',
    muted: 'Muted',
    safe: 'Safe',
    unknown: 'Unknown'
  },
  truth: {
    actual: 'Measured',
    demo: 'Illustrative',
    notWired: 'Not wired'
  },
  login: {
    pageTitle: 'Sign in',
    title: {
      setup: 'Set the admin password',
      login: 'Sign in to khaen'
    },
    desc: {
      setup: 'First run: choose an admin password (at least {min} characters). The console opens right after it is saved.',
      login: 'Enter the admin password to open the console.'
    },
    field: {
      password: 'Password',
      confirm: 'Repeat password'
    },
    placeholder: {
      password: 'Enter password',
      confirm: 'Repeat password'
    },
    action: {
      submitSetup: 'Set password and continue',
      submitLogin: 'Sign in',
      retry: 'Retry'
    },
    state: {
      checking: 'Checking authentication status…',
      unavailable: 'Cannot reach the daemon'
    },
    validation: {
      required: 'Password is required',
      tooShort: 'Password must be at least {min} characters',
      mismatch: 'The two passwords do not match'
    },
    notice: {
      sessionExpired: 'Your session has expired. Please sign in again.',
      alreadyConfigured: 'The admin password is already set. Please sign in.',
      passwordChanged: 'Password changed. Every session has been invalidated; sign in again with the new password.'
    },
    footer: {
      version: 'Version {version}',
      build: 'Build {id}'
    }
  },
  error: {
    auth: {
      invalidCredentials: 'Wrong password. Try again.',
      invalidCurrentPassword: 'The current password is incorrect.',
      rateLimited: 'Too many attempts (max 5 per minute). Please wait and try again.',
      alreadyConfigured: 'The admin password is already set. Please sign in.',
      notConfigured: 'The daemon has no admin password yet. Refresh the page and retry.',
      csrfInvalid: 'Request verification failed. Refresh the page and retry.',
      validation: 'The submitted values are not valid.',
      unauthenticated: 'Sign in to continue.',
      sessionExpired: 'Your session has expired. Please sign in again.',
      network: 'Cannot reach the daemon (not running or network down). Check it and retry.',
      server: 'Server error. Please retry later.',
      unknown: 'Unexpected error. Please retry.'
    },
    status: {
      network: 'Cannot fetch the status: daemon unreachable (not running or network down). Check it and retry.',
      timeout: 'Fetching the status timed out. Please retry.',
      unauthorized: 'Your session has expired. Please sign in again.',
      server: 'The daemon returned a server error. Please retry later.',
      invalidResponse: 'The daemon returned an invalid status document (not khaen-status/v1); it was rejected instead of displayed.',
      unknown: 'Fetching the status failed. Please retry.'
    },
    calibration: {
      network: 'Cannot reach the daemon (not running or network down). Check it and retry.',
      timeout: 'The calibration request timed out. Please retry.',
      unauthorized: 'Your session has expired. Please sign in again.',
      notWired: 'The calibration backend is not wired yet: this action is unavailable (no fake data or fake success).',
      busy: 'Another calibration job is running. Wait for it to finish or cancel it first.',
      conflict: 'The current state does not allow this action (already applied/rolled back or prerequisites missing). Refresh and retry.',
      notFound: 'The target does not exist (job/upload/artifact may have been removed).',
      invalidRequest: 'The request parameters are invalid; check the form and retry.',
      uploadTooLarge: 'The uploaded file exceeds the size limit (64 MiB).',
      unsupportedFormat: 'Unsupported file format: calibration input requires 48 kHz / 16–24 bit / mono or stereo WAV.',
      server: 'The daemon returned a server error. Please retry later.',
      invalidResponse: 'The daemon returned an invalid calibration document; it was rejected instead of displayed.',
      unknown: 'The calibration action failed. Please retry.'
    },
    system: {
      network: 'Cannot load the system information: the daemon is unreachable (not running or network down). Check it and retry.',
      timeout: 'Loading the system information timed out. Please retry.',
      unauthorized: 'Your session has expired. Please sign in again.',
      invalidResponse: 'The daemon returned an invalid system document; it was rejected instead of displayed.',
      disabled: 'The backend does not allow this action (restart requires the daemon to run with KHAEN_ALLOW_RESTART=1).',
      unavailable: 'This capability is not wired on the backend (journalctl / systemctl unavailable or not implemented); no fake data is shown.',
      validation: 'The submitted values are not valid; check them and retry.',
      server: 'Server error. Please retry later.',
      unknown: 'The system action failed. Please retry.'
    },
    control: {
      network: 'Cannot reach the daemon (not running or network down). Check it and retry.',
      timeout: 'The control request timed out. Please retry.',
      unauthorized: 'Your session has expired. Please sign in again.',
      hostLinkUnavailable:
        'The daemon to rt-host control channel is unavailable (the control socket is unreachable): the write was not sent and the page will not show a fake success.',
      unsupported: 'The backend explicitly does not support this action (unsupported); it will not pretend to succeed.',
      notWired: 'The daemon does not have this capability yet (not_wired); no fake data is shown.',
      noBasePlan: 'There is no committed plan to patch volume gains (no_base_plan): apply a plan on the plan page first.',
      tokenInvalid: 'The engine_token is invalid or the draft was replaced (engine_token_invalid): prepare again.',
      admissionRejected: 'The plan patch was rejected by admission (admission_rejected): adjust the gains and retry.',
      noHistory: 'There is no committed history to roll back to (no_history).',
      notFound: 'The target resource was not found (the backend may not implement this endpoint yet).',
      invalidRequest: 'The request parameters are invalid (bad_request); check the input and retry.',
      conflict: 'The current state does not allow this action (conflict); refresh and retry.',
      server: 'The daemon returned a server error. Please retry later.',
      invalidResponse: 'The daemon returned an invalid control document; it was rejected instead of treated as success.',
      unknown: 'The control action failed. Please retry.'
    },
    channels: {
      shaMismatch: 'The client and server sha256 do not match: the operation was rejected (neither side is trusted).',
      shaUnavailable: 'This browser does not support WebCrypto SHA-256, so the upload cannot be verified (there is no "verify nothing" fallback).',
      planPathUnavailable:
        'The upload receipt has no plan_path (stored_path): the daemon did not report where the plan was stored, so POST /api/plan/inspect and /api/prepare cannot be called; the page does not guess a path or invent a preview/draft.',
    }
  },
  overview: {
    title: 'Overview',
    subtitle:
      'Run state, input/output counters, current plan and the control link. Data comes from the daemon /api/status aggregate, polled every 5 seconds and paused while the tab is hidden.',
    error: {
      title: 'Cannot load the run state'
    },
    band: {
      runState: 'Run state',
      lastError: 'Last error',
      hostLink: 'Control link',
      lastUpdate: 'Updated {time}',
      neverUpdated: 'Never refreshed',
      noError: 'None'
    },
    hostLink: {
      connected: 'Connected',
      stale: 'Stale',
      disconnected: 'Disconnected',
      disconnectedStale: 'Disconnected (stale)'
    },
    notice: {
      stale:
        'The control link is stale (connected={connected}, freshness={age}); the values shown are from the last successful fetch.',
      hostLinkNotWired:
        'Control link unavailable: every host_link field is null; changing control state requires the daemon ↔ rt-host control link to recover.',
      hostLinkNotWiredReason: 'Control link unavailable: {reason}',
      refreshFailed: 'Refresh failed: {message} (last successful update {time}). The last successful values are still shown.'
    },
    uptime: {
      withDays: '{d}d {h}h {m}m {s}s',
      withoutDays: '{h}h {m}m {s}s'
    },
    calibration: {
      none: 'No records yet'
    },
    device: {
      present: 'Present',
      absent: 'Absent'
    },
    plan: {
      source: {
        upload: 'Upload',
        calibration: 'Calibration'
      }
    },
    copy: {
      action: 'Copy',
      copied: 'Copied',
      failed: 'Copy failed (the browser denied clipboard access)'
    },
    notWiredList: {
      title: 'Not wired (reported by the daemon)',
      desc: 'The capabilities below are reported as not wired by /api/status not_wired; this page never shows fake data for them.'
    },
    card: {
      runtime: {
        title: 'Run state',
        desc: 'rt-host block engine state and uptime',
        field: {
          state: 'State',
          muted: 'Muted',
          safe: 'Safe',
          blocks: 'Blocks processed',
          uptime: 'Uptime'
        }
      },
      input: {
        title: 'Input counters',
        desc: 'AES67 receiver counters and underruns',
        field: {
          packets: 'Packets',
          underruns: 'Underruns',
          underrunRate: 'Underrun rate',
          jitterFill: 'Jitter buffer fill',
          ssrc: 'SSRC',
          queueFullDrops: 'Queue-full drops',
          recvErrors: 'Receive errors'
        }
      },
      output: {
        title: 'Output counters',
        desc: 'ALSA playback counters and xruns',
        field: {
          blocks: 'Blocks played',
          xruns: 'xruns',
          errors: 'Output errors',
          bytes: 'Bytes written'
        }
      },
      plan: {
        title: 'Current plan',
        desc: 'Applied plan file and version',
        field: {
          graphId: 'graph_id',
          sha256: 'sha256',
          version: 'Version',
          latencySamples: 'Latency (samples)',
          appliedAt: 'Applied at',
          source: 'Source'
        }
      },
      format: {
        title: 'Format & device',
        desc: 'Plan rate, device readback and the actual output format',
        field: {
          planRate: 'Plan rate',
          deviceRate: 'Device rate',
          rateAsserted: 'Rate asserted',
          blockFrames: 'Block frames',
          channels: 'Channels',
          encoding: 'Encoding',
          deviceId: 'Device id',
          deviceState: 'Device state',
          deviceCard: 'ALSA card',
          rateReadback: 'Rate readback'
        }
      },
      aoip: {
        title: 'AES67',
        desc: 'AoIP receive parameters and channel map',
        field: {
          group: 'Multicast group',
          port: 'Port',
          pt: 'Payload type',
          channels: 'Received channels',
          map: 'Channel map'
        }
      },
      calibration: {
        title: 'Last calibration',
        desc: 'Most recent calibration job and verdict',
        field: {
          lastJob: 'Last job',
          verdict: 'Verdict',
          activeJob: 'Active job'
        },
        stub: 'This card only reads the last calibration job reported by the daemon (/api/calibration/jobs); when it is unavailable the reason is shown instead of fake data.'
      },
      control: {
        title: 'Control link',
        desc: 'daemon ↔ rt-host control path',
        field: {
          connected: 'Connected',
          statusAgeMs: 'Status age',
          since: 'Connected since',
          socket: 'Socket path'
        },
        notWiredHint: 'Control link unavailable: host_link is null; changing control state requires the control link to recover.'
      }
    }
  },
  calibration: {
    review: {
      applyState: 'Apply state',
      appliedVersion: 'Applied version',
      before: 'Before (current running plan)',
      after: 'After (job artifact)',
      beforeHint: 'Pre-apply snapshot: rollback returns to this version.',
      snapshot: 'Pre-apply snapshot',
      version: 'Version',
      graphId: 'graph_id',
      sha256: 'sha256',
      apply: 'Apply calibration plan',
      rollback: 'Roll back to the pre-apply version',
      noJob: 'No job available to apply.',
      exportConfig: 'Export config JSON',
      applyReceipt: 'Latest apply receipt',
      rollbackReceipt: 'Latest rollback receipt',
      appliedAt: 'Applied at',
      restoredVersion: 'Restored version',
      ack: 'Ack kind',
      ackActual: 'actual (really applied)',
      ackSynthetic: 'synthetic (simulated commit)',
      confirmApply: {
        title: 'Apply the calibration plan?',
        message: 'The job artifact will be prepare → commit-ed to the running plan; a snapshot of the current version {version} is recorded and can be rolled back.',
        ok: 'Apply'
      },
      confirmRollback: {
        title: 'Roll back to the pre-apply version?',
        message: 'The running plan will be restored to version {version} (graph_id {graphId}).',
        ok: 'Roll back'
      },
      appliedToast: 'Calibration plan applied',
      rolledBackToast: 'Rolled back to the pre-apply version'
    },
    wizard: {
      pageTitle: 'Calibration wizard',
      subtitle:
        'Dirac-style 8-step calibration wizard. Every gate is decided by the daemon (/api/calibration/wizard/state); step 5 can start a real rt-host measurement (C3b1, audible), while solve/design/apply belong to C3b2; missing capabilities (multi-point/ART, ...) are shown grey with NOT_AVAILABLE. No fake progress or results.',
      stepNav: 'Wizard steps',
      stepShort: 'Step {step}',
      stepOf: 'Step {current} / {total}',
      backToList: 'Back to calibration',
      previous: 'Previous',
      next: 'Next',
      nextSkip: 'Next (step not wired)',
      finish: 'Finish',
      cancelWizard: 'Cancel wizard',
      confirmCancel: {
        title: 'Leave the wizard?',
        message: 'Wizard settings, measurement uploads and finished steps stay on the backend; you can come back later.',
        ok: 'Leave wizard'
      },
      unavailable: {
        title: 'Calibration backend not wired',
      },
      error: {
        title: 'Cannot read the wizard state'
      },
      actionError: 'Action failed: {message}',
      toast: {
        saved: 'Calibration settings saved',
        uploaded: 'Upload complete',
        jobCreated: 'Analysis job created',
        measurementStarted: 'Real measurement job created',
        jobCancelled: 'Job cancelled'
      },
      device: {
        desc: 'Data comes from /api/status (device / format / control link); the gate is decided by the daemon.',
        device: 'Device',
        deviceState: 'Device state',
        deviceCard: 'ALSA card',
        deviceRate: 'Device rate',
        rateAsserted: 'Rate asserted',
        hostLink: 'Control link',
        hostLinkAge: 'Status age',
        runState: 'Run state',
        channels: 'Output channels',
        encoding: 'Encoding',
        noStatus: 'No /api/status data yet (backend not wired or request failed); no values are shown for this step.',
        statusError: 'Cannot fetch /api/status: {message}'
      },
      channels: {
        desc: 'Read-only format/channel facts from /api/status and /api/channels; missing values show the reason instead of fake data.',
        notAvailable: '/api/channels is unavailable: this step is read-only; use the Plan & channels page for the two-phase hot reconfiguration.',
        noMapping: 'No channel mapping is shown or invented: only the daemon-reported mapping is displayed, with a reason when unknown.',
        readOnly: 'This step is read-only and produces no configuration.',
        planRate: 'Plan rate',
        deviceRate: 'Device rate',
        channels: 'Output channels',
        encoding: 'Encoding',
        aes67Channels: 'AES67 receive channels',
        aes67Map: 'AES67 channel map',
        aes67Group: 'AES67 multicast group',
        aes67Port: 'AES67 port',
        task: 'Missing capability'
      },
      mic: {
        desc: 'Upload the microphone calibration file, choose orientation and target curve, set single-point parameters; the daemon decides the gate after saving.',
        file: 'Microphone calibration file',
        fileHint: 'UMIK-1 calibration txt (≤ 1 MiB); the SHA-256 is computed by the server.',
        fileSelect: 'Choose calibration file',
        fileTooLarge: 'Calibration file exceeds the 1 MiB limit',
        savedSha: 'Saved SHA-256',
        filename: 'File name',
        device: 'Microphone capture device',
        deviceHint: 'ALSA device name (for example hw:CARD=U18dB,DEV=0); the UI never fabricates enumeration.',
        serial: 'Microphone serial',
        serialHint: 'Must match the SERNO line at the top of the calibration file; prefilled after upload, still editable.',
        parsed: 'Server parse',
        parsedValue: 'SERNO {serno} · {points} points',
        rtHost: 'rt-host UMIK status',
        rtHostAvailable: 'rt-host declared and can use the UMIK (real measurement ready)',
        orientation: 'Microphone orientation',
        orientation0: '0° (pointing at the source)',
        orientation90: '90° (pointing at the ceiling)',
        targetMode: 'Target curve',
        targetFlat: 'flat (flat response)',
        targetTilt10: 'tilt10 (−10 dB at 20 kHz)',
        centers: 'Center frequencies (Hz, comma separated)',
        centersHint: '1/3-oct center frequencies, up to 64; default 25 Hz–20 kHz (30 values).',
        tolerance: 'Tolerance (dB)',
        minBands: 'Min bands',
        save: 'Save settings',
        saving: 'Saving…',
        savedAt: 'Saved ({time})',
        notSaved: 'Not saved yet',
        validation: {
          deviceRequired: 'Enter the microphone capture device (ALSA name)',
          serialRequired: 'Enter the microphone serial number',
          micRequired: 'Upload the microphone calibration file first',
          orientationRequired: 'Choose the microphone orientation',
          targetRequired: 'Choose the target curve',
          centersRange: 'Center frequencies must be numbers in 1–24000 Hz (comma/space separated)',
          centersTooMany: 'At most 64 center frequencies',
          toleranceRange: 'Tolerance must be between 0.1 and 24 dB',
          minBandsRange: 'Min bands must be an integer between 1 and 64'
        }
      },
      positions: {
        desc: 'khaen P1 supports a single position (listening seat) only; multi-point and ART are out of scope.',
        single: 'Single point (listening seat)',
        singleDesc: 'Fixed primary listening position; the only supported scheme in this version.',
        selected: 'Confirmed',
        notSelected: 'Not confirmed yet (save step 3 settings first)',
        multi: 'Multi-point',
        multiReason: 'khaen does not target multi-point: a multi-point backend is required.',
        art: 'ART (active room treatment)',
        artReason: 'ART needs multi-point measurements and support groups; khaen has no such backend.'
      },
      measure: {
        desc: 'Step 5 has two mutually exclusive paths: real rt-host measurement (primary, C3b1, audible, requires an explicit audible authorization) and offline WAV upload (offline/CI).',
        baseline: 'baseline (before calibration)',
        applied: 'applied (after calibration)',
        hint: '48 kHz / 16–24 bit / mono or stereo; ≤ 64 MiB.',
        select: 'Choose WAV',
        format: 'Format',
        duration: 'Duration',
        formatValue: '{rate} / {bits} bit / {channels} ch',
        savedBaseline: 'Saved baseline',
        savedApplied: 'Saved applied',
        availability: {
          micProfile: 'MicProfile (saved in step 3)',
          profileValue: '{device} · SERNO {serial}',
          profileMissing: 'No MicProfile saved yet (the daemon rejects real measurement with 409 mic_profile_missing)',
          profileReason: 'Why the daemon has no profile',
          rtHost: 'rt-host UMIK status',
          available: 'rt-host available (real measurement ready)',
          unavailable: 'rt-host unavailable',
          notLoaded: 'rt-host status not fetched yet (press refresh to retry)',
          unavailableTitle: 'Real rt-host measurement unavailable',
          statusErrorTitle: 'Cannot fetch the rt-host status',
          refresh: 'Refresh status'
        },
        realtime: {
          title: 'Real rt-host measurement (single_point_rt_host)',
          desc: 'The rt-host plays a sweep on the real device and the UMIK captures it; progress and artifacts come from the real daemon job (no fake progress).',
          audibleHint: 'Audible: the audible authorization (P08) must be confirmed before starting.',
          audibleConfirmTitle: 'Start the real measurement?',
          audibleConfirmMessage: 'A real measurement makes the rt-host play an audible sweep through the device and captures it with the UMIK. Continue only inside the allowed sound window and after confirming the room is safe.',
          audibleConfirmOk: 'Confirm audible and start',
          start: 'Start real measurement',
          starting: 'Creating job…',
          creating: 'Create request sent; fetching the real job state…',
          advanced: 'Advanced settings (omitted by default; the rt-host configuration decides)',
          channels: 'Measurement channels (comma separated)',
          channelsPlaceholder: 'empty = rt-host KHAEN_CAL_CHANNELS',
          channelsHint: 'Non-empty, ≤22 entries, non-negative integers, no duplicates; empty lets the rt-host configuration decide.',
          channelsInvalid: 'Invalid measurement channels: non-empty, ≤22 entries, non-negative integers, no duplicates (or leave empty).',
          levelDbfs: 'Sweep level (dBFS)',
          levelHint: 'Empty = the rt-host level; an explicit value must be within -60 … -1 dBFS.',
          levelInvalid: 'Invalid sweep level: must be within -60 … -1 dBFS (or leave empty).',
          omittedHint: 'Empty fields are omitted so the rt-host configuration decides; the page never invents daemon defaults.',
          jobId: 'job id',
          kind: 'job kind',
          progress: 'Real progress',
          currentStep: 'Current step',
          engine: 'Engine',
          cancel: 'Cancel measurement',
          cancelConfirmTitle: 'Cancel the real measurement?',
          cancelConfirmMessage: 'This cancels the running measurement job {id}; the daemon stops cooperatively and calls cal_leave, finished steps are kept.',
          jobFailedTitle: 'Real measurement failed',
          jobCancelledTitle: 'Real measurement cancelled',
          jobCancelledDesc: 'The calibration mode was restored; you can start the measurement again.',
          succeededTitle: 'Real measurement finished',
          failedTitle: 'Real measurement request failed',
          errorCode: 'Stable error code',
          artifacts: 'Artifacts (downloadable)',
          noArtifacts: 'This job has no artifacts (failed/cancelled or the backend produced none).',
          download: 'Download',
          c3b2Note: 'The real measurement job now includes solve_g and compile: design/plan are marked PREDICTED_ONLY (not a measured PASS; no remeasure/holdout was run); applying requires an explicit two-phase prepare→commit and never happens automatically.',
          noPassNote: 'A null job verdict is truthful (quality.json records available:false with the reason), not a failure.',
          solveUnavailable:
            'The rt-host measurement job now runs validate → sweep_capture → quality_check → solve_g → compile → bundle; design/plan artifacts are marked PREDICTED_ONLY. analysis/design/apply gates follow the backend job/artifacts, and apply stays an explicit two-phase user action.'
        },
        offline: {
          title: 'Offline WAV (offline / CI closed loop)',
          badge: 'offline / CI',
          desc: 'This is the single_point_offline path: upload the baseline/applied WAVs and create an offline closed-loop job in step 6; it never drives the real rt-host measurement, and neither path replaces the other.'
        },
        errors: {
          micProfileMissing: 'Save the MicProfile in step 3 first (device / serial / calibration file).',
          busy: 'A calibration job is already running: wait for it to finish or cancel it.',
          notWired: 'The daemon explicitly reports the calibration backend as not wired: no fake progress or results.'
        },
        parseError: 'The file is not a usable WAV: {message}',
        formatError: 'Format does not meet the calibration requirements: {message}',
        pairError: 'The two recordings are structurally inconsistent: {message}',
        wavErrors: {
          file_too_large: 'File exceeds the 64 MiB limit',
          file_too_small: 'File is too small to be a valid WAV',
          not_riff: 'Not a RIFF container (missing RIFF header)',
          not_wave: 'Not WAVE format',
          missing_fmt: 'Missing fmt chunk',
          missing_data: 'Missing data chunk',
          unsupported_codec: 'Unsupported codec (PCM only)',
          bad_sample_rate: 'Sample rate must be 48 kHz',
          bad_bits: 'Bit depth must be 16 or 24 bit',
          bad_channels: 'Channel count must be 1 or 2',
          bad_block_align: 'blockAlign does not match channels/bit depth',
          bad_data_length: 'data chunk length is invalid or truncated',
          format_mismatch: 'The two recordings differ in sample rate/channels/bit depth',
          length_mismatch: 'The two recordings differ in length (within ±1 frame only)'
        }
      },
      analysis: {
        desc: 'Create a single-point offline job; progress and results come from the real daemon job state (no fake progress bars).',
        planSha: 'Current plan sha256 (from /api/status)',
        planMissing: 'No plan is applied (status.plan.sha256 is null)',
        uploadsMissing: 'Upload the baseline / applied WAVs in step 5 first',
        settingsMissing: 'Save the calibration settings in step 3 first',
        start: 'Start analysis',
        starting: 'Creating job…',
        retry: 'Retry with the same inputs',
        cancel: 'Cancel job',
        jobId: 'job id',
        noJob: 'No analysis job created yet.',
        failedTitle: 'Analysis failed',
        cancelledTitle: 'Analysis cancelled',
        cancelledDesc: 'You can retry or go back one step.',
        errorCode: 'Stable error code',
        cancelConfirm: {
          title: 'Cancel the analysis job?',
          message: 'This cancels the running job {id}; finished steps are kept.',
        }
      },
      design: {
        desc: 'Shows the produced plan summary, design residual (FULL / PARTIAL / NONE) and admission result.',
        noResult: 'The job has not succeeded yet, so there is no design artifact.',
        nextBlocked: 'No applicable artifact: the next step is blocked by the gate.'
      },
      review: {
        desc: 'Before applying, the before/after summary and snapshot are shown; apply and rollback both require confirmation and receipts come from the real daemon commit result.'
      }
    },
    pageTitle: 'Calibration',
    subtitle:
      'Calibration jobs and the 8-step wizard (P1 offline closed loop). Data comes from the daemon calibration REST (/api/calibration/*); when the backend is not wired the page says so instead of showing fake data.',
    verdict: {
      none: '—'
    },
    engine: {
      label: 'engine: {engine}'
    },
    jobStatus: {
      queued: 'Queued',
      running: 'Running',
      succeeded: 'Succeeded',
      failed: 'Failed',
      cancelled: 'Cancelled'
    },
    jobKind: {
      single_point_offline: 'Offline closed loop (single_point_offline)',
      single_point_rt_host: 'Real rt-host measurement (single_point_rt_host)'
    },
    jobStepStatus: {
      pending: 'Pending',
      running: 'Running',
      succeeded: 'Done',
      failed: 'Failed',
      cancelled: 'Cancelled',
      skipped: 'Skipped'
    },
    applyState: {
      not_applied: 'Not applied',
      applied: 'Applied',
      rolled_back: 'Rolled back'
    },
    list: {
      title: 'Calibration jobs',
      newWizard: 'New calibration wizard',
      engine: 'Engine authenticity',
      engineUnknown: 'not reported',
      summary: {
        lastJob: 'Latest job',
        wizard: 'Wizard state',
        uploads: 'Measurement uploads',
        none: 'No calibration job yet',
        backendNotWired: 'Calibration backend not wired',
        wizardStep: 'Current step {step} / 8',
        gatesPassed: 'Gates passed: {passed} / {total}',
        openWizard: 'Open wizard',
        baseline: 'baseline',
        applied: 'applied'
      },
      emptyTitle: 'No calibration job yet',
      emptyDesc: 'No calibration analysis has run yet. Start from the 8-step wizard (P1 supports offline WAV closed loop only).',
      emptyUnavailableTitle: 'Calibration backend not wired, no job data',
      table: {
        id: 'job id',
        kind: 'Kind',
        status: 'Status',
        verdict: 'Verdict',
        created: 'Created',
        duration: 'Duration',
        actions: 'Actions'
      },
      action: {
        detail: 'Details',
        cancel: 'Cancel'
      },
      cancelConfirm: {
        title: 'Cancel the calibration job?',
        message: 'This cancels the running job {id}; finished steps are kept and the analysis can be restarted.',
        ok: 'Cancel job',
        done: 'Job cancelled'
      },
      pageInfo: 'Page {page} / {pages} · {total} total',
      pager: {
        prev: 'Previous',
        next: 'Next'
      }
    },
    job: {
      pageTitle: 'Calibration job detail',
      back: 'Back to calibration',
      created: 'Created',
      started: 'Started',
      finished: 'Finished',
      duration: 'Duration',
      status: 'Status',
      engine: 'Engine',
      applyState: 'Apply state',
      steps: 'State timeline',
      artifacts: 'Artifacts',
      noArtifacts: 'This job has no artifacts (failed/cancelled or not produced yet).',
      inputs: 'Inputs',
      inputsPlanSha: 'plan sha256',
      inputsBaseline: 'baseline upload',
      inputsApplied: 'applied upload',
      inputsCenters: 'Center frequencies (Hz)',
      inputsTolerance: 'Tolerance (dB)',
      inputsMinBands: 'Min bands',
      error: 'Failure reason',
      resultTitle: 'Analysis result',
      applyTitle: 'Apply / rollback',
      loadError: 'Cannot load the job detail',
      notFound: 'Job not found or deleted; go back to the list and retry.'
    },
    step: {
      names: {
        device_check: 'Device check',
        layout_channels: 'Layout & channels',
        mic_settings: 'Microphone & settings',
        positions: 'Measurement positions',
        measurement: 'Measurement',
        analysis: 'Analysis',
        design: 'Design',
        review_apply: 'Review / apply / rollback'
      },
      jobNames: {
        compile: 'Compile plan',
        levels: 'Levels / prediction',
        closed_loop: 'Closed-loop analysis',
        validate: 'Preflight validation',
        sweep_capture: 'Sweep capture',
        quality_check: 'Quality check',
        solve_g: 'Solve design (PREDICTED_ONLY)',
        bundle: 'Bundle artifacts'
      }
    },
    gate: {
      title: 'Gate',
      passed: 'Gate passed',
      blocked: 'Gate not passed',
      not_available: 'Capability not available',
      missing: 'Gate data missing',
      lockedStep: 'Locked step',
      task: 'Follow-up task: {task}',
      canContinue: 'You can continue to the next step.',
      mustFix: 'Complete this step to continue; see the reason above.',
      canSkip: 'This capability is not wired: the step produces no data and can be skipped (grey step).',
      reason: 'Reason: {reason}',
      failClosed: 'The daemon did not report a gate for this step; treated as blocked (fail closed).'
    },
    result: {
      syntheticHint: 'This job engine is {engine}: the values below are marked as demo data, not khaen measurements.',
      jobVerdictMismatch: 'job summary verdict={job}, result.verdict={result} (result wins)',
      maxDelta: 'Max delta',
      comparedBands: 'Compared bands',
      missingBands: 'Missing bands',
      violations: 'Violations',
      bands: 'Band details',
      noBands: 'No band data',
      bandHz: 'Frequency (Hz)',
      predicted: 'Predicted (dB)',
      observed: 'Observed (dB)',
      delta: 'Delta (dB)',
      missing: 'Missing',
      empty: 'The job has no result yet (result: null).'
    },
    design: {
      title: 'Design summary',
      residual: 'Design residual',
      admission: 'Admission',
      admitted: 'Admission accepted',
      rejected: 'Admission rejected',
      planSha: 'Artifact plan sha256',
      graphId: 'Artifact graph_id'
    },
    upload: {
      replace: 'Choose another file',
      uploading: 'Uploading…',
      sha256: 'SHA-256',
      bytes: 'Bytes'
    }
  },
  system: {
    pageTitle: 'System',
    logsPageTitle: 'Logs',
    subtitle:
      'Versions and build, host, services, devices, control link, logs, diagnostics and maintenance. Data comes from the daemon /api/system/* endpoints; missing fields are shown as "not wired", never faked.',
    logsSubtitle:
      'The last N journalctl lines of daemon / rt-host (read-only). Data comes from the daemon /api/system/logs endpoint; when journalctl is unavailable the page says so instead of faking logs.',
    actions: {
      refresh: 'Refresh',
      downloadDiagnostics: 'Download diagnostics JSON',
      downloading: 'Generating diagnostics…',
      openLogs: 'Open full logs',
      backToSystem: 'Back to system',
      retry: 'Retry'
    },
    notices: {
      refreshFailed: 'Refresh failed: {message} (the last successful values are still shown, updated {time})',
      lastUpdate: 'Updated {time}',
      neverUpdated: 'Never refreshed',
      degraded: 'The backend reported {count} fields as unavailable (null + reason): they are shown as "not wired", never faked.',
      partialUnavailable: '{count} capabilities are not wired (see the reasons on each field)'
    },
    error: {
      title: 'Cannot load the system information'
    },
    version: {
      title: 'Versions & build',
      desc: 'Separates the crate version compiled into the binary, the deployed release version and the commit. Any mismatch is flagged prominently at the top of the page.',
      consistent: 'Crate and release match',
      unknown: 'Version consistency cannot be determined (crate or release version missing)',
      mismatchBadge: 'Version mismatch',
      mismatchTitle: 'Version mismatch: crate {crate} ≠ release {release}',
      mismatchDesc:
        'The crate version is the Cargo version compiled into the binary; the release version is what was actually published and deployed. /healthz and the system info must report both honestly. Check that the release workflow injects KHAEN_BUILD_VERSION / KHAEN_BUILD_COMMIT.',
      localBuild: 'Local build (release version unknown)',
      field: {
        crateVersion: 'Crate version',
        releaseVersion: 'Release version',
        commit: 'Commit',
        buildProfile: 'Build profile',
        rustc: 'rustc',
        target: 'Target',
        webuiBuild: 'Web UI build'
      }
    },
    host: {
      title: 'Host',
      desc: 'hostname / kernel / OS / uptime / bind address (read-only collection).',
      field: {
        hostname: 'Hostname',
        kernel: 'Kernel',
        os: 'Operating system',
        uptime: 'Uptime',
        bind: 'Bind address'
      }
    },
    services: {
      title: 'Services',
      desc: 'systemd service state (daemon / rt-host): active / enabled / PID; failed commands are shown as not wired.',
      daemon: 'daemon',
      rtHost: 'rt-host',
      field: {
        active: 'active',
        enabled: 'enabled',
        pid: 'PID'
      }
    },
    devices: {
      title: 'Devices',
      desc: 'Sound cards from /proc/asound; MOTU devices are flagged.',
      motu: 'MOTU',
      empty: 'The daemon reported no sound cards (/proc/asound unreadable or the collector not wired).',
      field: {
        cardId: 'Card id',
        driver: 'Driver',
        model: 'Model',
        channels: 'Channels',
        rates: 'Supported rates'
      },
      noRates: 'Not reported'
    },
    hostLink: {
      title: 'Control link',
      desc: 'daemon → rt-host control channel (same source as the overview host_link).',
      field: {
        connected: 'Connected',
        statusAgeMs: 'Status freshness',
        since: 'Connected since',
        socket: 'Socket path'
      }
    },
    restart: {
      title: 'Maintenance: restart rt-host',
      desc: 'Shown only when the daemon runs with KHAEN_ALLOW_RESTART=1 and allows it; the UI owns the typed second confirmation.',
      disabledTitle: 'Restart not allowed by the backend',
      disabledReason: 'The daemon has restart disabled (KHAEN_ALLOW_RESTART=1 is required): {reason}',
      button: 'Restart rt-host',
      running: 'Requesting restart…',
      confirmTitle: 'Restart rt-host?',
      confirmDesc:
        'The daemon will run systemctl restart khaen-rt-host. Playback is interrupted (about 1-3 seconds) and the status file / session are re-established. Type the confirmation word below to submit.',
      confirmWordLabel: 'Confirmation word',
      confirmWord: 'restart',
      confirmWordPlaceholder: 'Type {word} to confirm',
      confirm: 'Confirm restart',
      cancel: 'Cancel',
      confirmMismatch: 'The confirmation word is incorrect; nothing was submitted',
      success: 'Restart of rt-host requested',
      lastRestartedAt: 'Last restart request',
      daemonTitle: 'Daemon self-restart',
      daemonReason: 'Daemon self-restart is not provided (the backend does not implement it and returns 501); use systemd/systemctl on the host instead.'
    },
    password: {
      title: 'Change password',
      desc: 'The current password is required. On success every session is invalidated and you must sign in with the new password.',
      field: {
        old: 'Current password',
        new: 'New password',
        confirm: 'Repeat new password'
      },
      submit: 'Change password',
      busy: 'Changing…',
      success: 'Password changed: every session has been invalidated; sign in with the new password.',
      validation: {
        required: 'Enter the current password',
        tooShort: 'The new password must be at least {min} characters',
        mismatch: 'The two new passwords do not match'
      }
    },
    logs: {
      title: 'Logs',
      desc: 'journalctl -u <unit> -n <N> --no-pager (the last N lines, read-only; N is at most 500).',
      unit: 'Unit',
      lines: 'Lines',
      autoRefresh: 'Auto refresh',
      copy: 'Copy logs',
      copied: 'Logs copied',
      copyFailed: 'Copy failed (the browser did not allow clipboard access)',
      copyEmpty: 'There are no log lines to copy',
      empty: 'The logs are empty (the daemon returned no lines).',
      unavailableTitle: 'Logs unavailable',
      unavailableReason: 'The daemon cannot read the logs: {reason}',
      source: 'Source: {source}',
      requested: 'Requested {lines} lines',
      truncated: 'Truncated',
      lastUpdate: 'Updated {time}',
      loading: 'Reading logs…',
      errorTitle: 'Cannot load the logs'
    },
    diagnostics: {
      success: 'Diagnostics package downloaded: {filename}',
      failed: 'Diagnostics download failed',
      notWired: 'Diagnostics unavailable: {reason}'
    }
  },
  control: {
    error: {
      title: 'Control action failed'
    },
    actions: {
      prepare: 'Prepare',
      commit: 'Commit',
      committing: 'Committing…',
      abort: 'Discard draft (abort)',
      aborting: 'Discarding…',
      retry: 'Retry'
    },
    toast: {
      committed: 'Committed: applied on the control plane (device readback in /api/status)',
      aborted: 'Draft discarded'
    },
    surface: {
      volume: 'Volume (per-lane gains)',
      mode: 'Run mode',
      format: 'Audio format',
      plan: 'Plan',
      rollback: 'Rollback'
    },
    receipt: {
      title: 'Two-phase receipt',
      desc: 'prepare only creates a draft; commit applies it at the block boundary and abort discards it. Receipts come from the daemon control plane (T70f-b proxies rt-host), not from page-side inference.',
      draftTitle: 'Draft (prepared)',
      empty: 'No draft has been prepared yet.',
      commitTitle: 'Commit receipt',
      commitEmpty: 'No commit receipt yet.',
      surface: 'Surface',
      engineToken: 'engine_token',
      plannedVersion: 'Planned version',
      changed: 'Changed',
      graphId: 'graph_id',
      mode: 'Target mode',
      format: 'Target format',
      deviceRestart: 'Device restart required',
      rolledBackTo: 'Rollback target version',
      note: 'Backend note',
      version: 'Applied version',
      appliedBlock: 'Applied block',
      allAck: 'All acknowledged (all_ack)',
      graphDropped: 'Previous graph dropped',
      historyCleared: 'History cleared',
      at: 'Receipt time',
      commitHint: 'Commit applies atomically at the next block boundary; on failure the draft is kept so you can retry or discard it (abort).'
    },
    format: {
      summary: '{rate} Hz / {block} frames / {channels} ch'
    },
    deviceRestart: {
      title: 'Restart rt-host to make this effective on the real device',
      desc: 'The control-plane engine was rebuilt for the new format; a real ALSA device must reopen its PCM stream (restart rt-host) before device_rate changes. The page never treats "control plane switched" as "device applied".',
      goto: 'Restart rt-host on the system page'
    },
    history: {
      title: 'Recent operations',
      empty: 'No operations recorded yet.',
      ok: 'OK',
      failed: 'Failed',
      action: {
        mute: 'Mute',
        prepare: 'Prepare',
        commit: 'Commit',
        abort: 'Discard',
        retry: 'Device retry'
      }
    }
  },
  volume: {
    pageTitle: 'Volume & mute',
    subtitle:
      'Mute (POST /api/mute, applied immediately and read back) and per-lane protection gains (POST /api/surface volume → POST /api/commit, two-phase). The control protocol has no master volume and no readback of current per-lane gains, so the page shows "Not wired" instead of inventing numbers.',
    actions: {
      refresh: 'Refresh'
    },
    error: {
      title: 'Cannot load the run state'
    },
    band: {
      mute: 'Mute',
      mutedOn: 'Muted',
      mutedOff: 'Not muted',
      runState: 'Run state',
      planVersion: 'Plan version',
      lastUpdate: 'Updated {time}',
      neverUpdated: 'Not refreshed yet'
    },
    notice: {
      safeReadOnly: 'Safe mode: volume/mode writes are disabled (the mute switch stays available as the recovery path).',
      safeReadOnlyReason:
        'Safe mode: volume/mode writes are disabled (the mute switch stays available as the recovery path). Reason: {reason}',
      faultedReadOnly: 'Faulted: volume/mode writes are disabled (the mute switch stays available as the recovery path).',
      faultedReadOnlyReason:
        'Faulted: volume/mode writes are disabled (the mute switch stays available as the recovery path). Reason: {reason}',
      stale: 'The control link status is stale: the page shows the last successful data.',
      refreshFailed: 'Refresh failed: {message} (last successful update {time}). The last successful data is still shown.',
      muteUnknown: 'Cannot read the current mute state (/api/status state.muted is null): no guessed value is shown.'
    },
    mute: {
      title: 'Mute',
      desc: 'POST /api/mute applies immediately (safety action); after success the page refreshes /api/status for readback instead of guessing locally.',
      on: 'Muted',
      off: 'Not muted',
      switch: 'Mute switch',
      field: {
        readback: 'Readback (/api/status state.muted)',
        runState: 'Run state',
        appliedBlock: 'Receipt applied block'
      },
      noReceipt: 'No mute receipt yet',
      successMuted: 'Muted (readback in /api/status)',
      successUnmuted: 'Unmuted (readback in /api/status)',
      motu: {
        field: 'MOTU device side effect',
        failedField: 'MOTU device write failed',
        applied: 'Written to the device and read back',
        skipped: 'Device unavailable: no device write (skipped)',
        notApplied: 'No device-write receipt',
        failedToast: 'The engine is muted, but the MOTU device write failed: {code} {message}',
        skippedToast: 'The engine is muted; the MOTU side effect was skipped: {reason}'
      },
      recoveryNote:
        'Recovering from a latched device safety state means explicitly unmuting: that is why the mute switch stays available in safe/faulted states while other writes are disabled.'
    },
    master: {
      title: 'Master volume (NOT_AVAILABLE)',
      desc: 'The control protocol (T70f-a/b) has no master volume op',
      reason:
        'The control plane has no master volume: /api/surface volume only accepts per-lane protection gains （lane, gain_db）; there is no single-master field, storage or readback. Pretending "write every lane" were a master would overwrite per-lane trims, so the page offers per-lane gain drafts instead.',
      field: {
        source: 'Data source',
        task: 'Task',
        readback: 'Current master volume readback'
      },
      sourceValue: 'daemon control plane (T70f-b proxies rt-host)',
      taskValue: 'R',
      replacedBy: 'Alternative: use the per-lane gain editor on the right; the current gains cannot be read back (no read endpoint).'
    },
    gains: {
      title: 'Per-lane gain draft',
      desc: 'POST /api/surface （surface: "volume", data: gains_db） → POST /api/commit, two-phase. Current protection gains come from /api/status.readback (base_plan); the draft still starts from a neutral 0 dB and you select the lanes to submit.',
      readbackAvailable: 'Current per-lane protection gains: read from base_plan (source={source})',
      readbackMissing:
        'Current per-lane protection gains: not wired (the control protocol has no per-lane gain read op; no invented 0 dB)',
      current: 'Current value',
      noLanes: 'Cannot build per-lane sliders: /api/status did not report format.channels (reason: {reason}).',
      noLanesReasonUnknown: 'the daemon gave no reason',
      toolbar: {
        uniform: 'Uniform value',
        applyUniform: 'Apply to draft',
        includeAll: 'Select all',
        clear: 'Reset to 0 dB'
      },
      count: '{count} lanes (readback.protection.lanes when available, otherwise format.channels)',
      include: 'Include in this commit',
      lane: 'lane {lane} gain',
      patchSummary: '{count} lane gains will be submitted (unselected lanes keep their current values)',
      validation: {
        empty: 'Adjust and select at least one lane first',
        lane: 'Lane index is out of range or duplicated',
        gain: 'Gain must be a finite number between -150 and +50 dB'
      },
      lockedHint: 'Volume writes are disabled in safe/faulted states',
      emptyPatchHint: 'Move a slider or set a uniform value, then select the lanes to submit',
      draftReady: 'Draft prepared (planned version {version}): commit or discard it in the two-phase receipt below',
      prepared: 'Volume draft prepared'
    }
  },
  formatPage: {
    pageTitle: 'Audio format',
    subtitle:
      'Current actual format (/api/status) and two-phase format changes (prepare → commit). Format is an engine configuration: commit rebuilds the engine and drops the running graph; a real device must restart rt-host to reopen PCM before it runs at the new rate.',
    actions: {
      refresh: 'Refresh'
    },
    error: {
      title: 'Cannot load the run state'
    },
    band: {
      runState: 'Run state',
      deviceRate: 'Device rate',
      rateAsserted: 'Rate asserted',
      planVersion: 'Plan version',
      lastUpdate: 'Updated {time}',
      neverUpdated: 'Not refreshed yet'
    },
    notice: {
      safeReadOnly: 'Safe mode: format writes are disabled (no further configuration changes while safe).',
      safeReadOnlyReason: 'Safe mode: format writes are disabled. Reason: {reason}',
      faultedReadOnly: 'Faulted: format writes are disabled; recover the device first.',
      faultedReadOnlyReason: 'Faulted: format writes are disabled; recover the device first. Reason: {reason}',
      rateNotAsserted: 'The device rate is not asserted yet (rate_asserted=false): the device may not run at the plan rate.',
      rateMismatch: 'The plan rate and the device readback rate differ: the device is not running at the plan rate (readback after restarting rt-host).',
      stale: 'The control link status is stale: the page shows the last successful data.',
      refreshFailed: 'Refresh failed: {message} (last successful update {time}). The last successful data is still shown.'
    },
    current: {
      title: 'Current format (/api/status)',
      desc: 'Plan rate, device rate readback, block length and channel count come from /api/status. The AES67 group, port, payload type and map come from /api/channels current. Encoding has no data source.',
      sourceHint: 'Rate and block length come from /api/status. AES67 comes from /api/channels current (not the always-empty format.aes67 on status).',
      aes67Loading: 'Loading /api/channels; the AES67 current value is not here yet.',
      encodingReason: 'The daemon does not report encoding (reserved field, no data source; the readback phase will add one)',
      rateNotAsserted: 'The device rate is not asserted yet (rate_asserted=false)',
      retryReadback: 'Retry readback',
      retryOk: 'Asked the device supervisor to retry and read back',
      retryStillUnasserted: 'The device rate is still not asserted (rate_asserted remains false)',
      field: {
        planRate: 'Plan rate',
        deviceRate: 'Device rate',
        rateAsserted: 'Rate asserted',
        blockFrames: 'Block length',
        channels: 'Channels',
        encoding: 'Encoding',
        aes67Channels: 'AES67 input channels',
        aes67Map: 'AES67 channel map',
        aes67Group: 'AES67 multicast group',
        aes67Pt: 'AES67 payload type',
        aes67Port: 'AES67 port'
      }
    },
    apply: {
      title: 'Apply format (two-phase)',
      desc: 'prepare → commit; the backend changed / device_restart_required fields are shown as-is.',
      field: {
        sampleRate: 'Sample rate',
        block: 'Block length',
        controlFormat: 'Control-plane receipt format'
      },
      frames: 'frames',
      note: 'Supported sample rates 48k/96k and block lengths 64/128 (backend capability sets); whether the device really supports them is validated by the backend, and a rejection is shown as a stable error code.',
      prepared: 'Format draft prepared',
      selectHint: 'Choose a sample rate or block length (at least one)',
      lockedHint: 'Writes are disabled in safe/faulted states',
      restartTitle: 'Restart rt-host to make this effective on the real device',
      restartDesc:
        'The control-plane engine was rebuilt for the new format and the running graph was dropped; a real ALSA device must reopen its PCM stream (restart rt-host) before device_rate changes. Until then the page does not treat the control-plane receipt as device-effective.',
      noControlReceipt: 'No format change committed yet (no control-plane receipt)'
    }
  },
  channels: {
    pageTitle: 'Plan & channels',
    actions: {
      refresh: 'Refresh'
    },
    updated: {
      last: 'Updated {time}',
      never: 'Not refreshed yet'
    },
    band: {
      runState: 'Run state',
      planVersion: 'Plan version',
      outputChannels: 'Output ch',
      inputChannels: 'Input ch'
    },
    notice: {
      refreshFailed: 'Refresh failed: {message} (the last successful channel data is shown)',
      statusRefreshFailed: '/api/status refresh failed: {message} (last successful update {time})',
      historyUnavailable: 'Plan history unavailable: {message}',
      inspectUnavailable: 'Plan preview (/api/plan/inspect) unavailable: only the upload summary is shown; sections/latency are not invented'
    },
    plan: {
      title: 'Current plan',
      desc: 'Plan metadata from /api/status plus the current version from /api/plan/history; missing fields show "not wired" plus the reason.',
      field: {
        graphId: 'graph_id',
        sha256: 'sha256',
        version: 'Version',
        latency: 'Latency (samples)',
        appliedAt: 'Applied at',
        source: 'Source'
      },
      noPlan: 'No plan has been applied yet',
      shaUnavailable: '/api/status has no sha256 (no data source)',
      prepared: 'Plan draft prepared',
      committed: 'Plan committed (receipt in the two-phase card below)'
    },
    output: {
      title: 'Output channels & MOTU physical mapping',
      desc: 'From /api/channels (actual rt-host configuration + the running plan routing section); only known positions are labelled L/R/SL/SR/TF/TFR/LFE, unknown positions show the output number.',
      field: {
        channels: 'Output channels',
        dacMap: 'DAC map (0-based)',
        knownLabels: 'Known position labels'
      },
      table: {
        lane: 'lane',
        physical: 'MOTU output',
        label: 'Label'
      },
      unknownLabel: 'OUT {channel}',
      empty: 'The daemon reports no output channels/mapping (either /api/channels is not wired or the rt-host status is unavailable).',
      mappingUnknown:
        'The routing section of the running plan is unknown (mapping=null): the daemon cannot associate the applied plan routing, so the page draws no lane → DAC mapping and assumes no passthrough.'
    },
    input: {
      title: 'AES67 input',
      desc: 'From /api/channels.input (SDP descriptor or manual configuration); sdp_sha256 identifies the input descriptor version.',
      field: {
        channels: 'Input channels',
        map: 'Channel map',
        source: 'Input source',
        sdpSha: 'SDP sha256'
      },
      source: {
        sdp: 'SDP descriptor (sdp)',
        manual: 'Manual configuration (manual)'
      },
    },
    inspect: {
      title: 'Plan preview (/api/plan/inspect)',
      unavailable: 'Plan preview unavailable: only the upload summary (sha/graph_id/segments/latency) is shown; sections are not invented.',
      accepted: 'Accepted',
      rejected: 'Rejected by admission',
      field: {
        sampleRate: 'Sample rate (Hz)',
        blockSize: 'Block size (frames)',
        inChannels: 'Input channels',
        lanes: 'Lanes',
        dacChannels: 'DAC channels',
        latency: 'Compiled latency (samples)',
        compiled: 'Graph compiled',
        admission: 'Admission'
      }
    },
    upload: {
      title: 'Plan upload (plan ABI v3)',
      desc: 'Raw plan bytes (≤ 4 MiB) → browser computes sha256 → the server validates and returns graph_id/section names/segment count/latency/stored_path → inspect → prepare → commit. Upload never applies anything automatically.',
      select: 'Choose plan file',
      noFile: 'No file selected yet',
      tooLarge: 'The file exceeds the upload limit {limit}',
      clientSha: 'Client sha256',
      upload: 'Upload & verify',
      uploaded: 'Plan uploaded and sha256 verified',
      prepare: 'Prepare to apply',
      ackTitle: 'Upload receipt (server)',
      versionUnavailable: 'The real daemon upload receipt has no target version (versions are produced by the commit receipt / history).',
      segmentsUnavailable: 'This backend reports only the segment count (a numeric segments field), not the section names: no fake section list is derived from the number.',
      field: {
        sha256: 'sha256 (server)',
        graphId: 'graph_id',
        version: 'Target version',
        segments: 'Sections',
        segmentCount: 'Segment count',
        latency: 'Latency (samples)'
      }
    },
    history: {
      title: 'History & rollback',
      desc: 'Applied versions from /api/plan/history; a rollback creates a new version (history is not rewritten) and must be committed to take effect.',
      current: 'current',
      rollback: 'Roll back to this version',
      empty: 'No applied version in history yet.',
      noSource: 'The history endpoint has no data source (the daemon reports neither the current nor the available versions): no history is invented.',
      rollbackNote: 'Rollback is a forward commit: history is kept, and it still needs a commit in the two-phase card to take effect.',
      rollbackPrepared: 'Prepared a rollback draft to v{version}: commit it to apply',
      table: {
        version: 'Version',
        graphId: 'graph_id',
        appliedAt: 'Applied at',
        source: 'Source',
        actions: 'Actions'
      },
      confirmRollback: {
        title: 'Roll back to an older version?',
        message: 'A draft rolling back to v{version} will be prepared (the version number increases, history is not rewritten); it still requires a commit to take effect.',
        ok: 'Prepare rollback'
      }
    },
    draft: {
      desc: 'prepare only creates a draft (engine_token/preview); commit applies it and abort discards it; receipts come from the daemon control plane.',
      empty: 'No plan draft: upload a plan and press "Prepare to apply", or roll back to an older version in history.',
      planHint: 'Uploaded-plan draft: committing changes graph_id / version.',
      rollbackHint: 'Rollback draft: from v{version} (the version number increases, history is not rewritten); commit it to apply.',
      confirmCommitTitle: 'Commit the plan?',
      confirmCommitMessage: 'The draft will be committed as v{version} (graph switched atomically at a block boundary; graph_id changes after apply).',
      confirmRollbackTitle: 'Commit the rollback?',
      confirmRollbackMessage: 'The rollback draft to v{version} will be committed (a new version is created, history is kept).'
    },
    label: {
      l: 'L',
      r: 'R',
      sl: 'SL',
      sr: 'SR',
      tf: 'TF',
      tfr: 'TFR',
      lfe: 'LFE'
    }
  },
  routing: {
    pageTitle: 'Routing matrix',
    lanes: {
      unknownLabel: 'OUT {channel}'
    },
    actions: {
      refresh: 'Refresh'
    },
    updated: {
      last: 'Last updated {time}',
      never: 'Not refreshed yet'
    },
    band: {
      hostLink: 'Control link',
      hostLinkOk: 'Connected',
      hostLinkStale: 'Status stale',
      hostLinkUnknown: 'Not wired',
      runState: 'Run state',
      planVersion: 'Plan version',
      matrixLanes: 'Matrix lanes',
      outputChannels: 'DAC outputs',
      routedLanes: 'Routed lanes'
    },
    notice: {
      statusRefreshFailed: '/api/status refresh failed: {message} (showing the last successful snapshot)',
      channelsRefreshFailed: '/api/channels refresh failed: {message} (showing the last successful snapshot)',
      historyUnavailable: 'Plan history is unavailable: {message} (current version / latency fallback missing)',
      hostLinkDown:
        'The control link is unavailable or stale: rt-host data-plane facts cannot be read back, so this page only shows the last successful snapshot; routing write access does not become available either.',
      noPlan: 'No applied plan has been read: there is no source of truth for the matrix/output routing (this page does not guess an identity mapping).'
    },
    statusUnavailable: '/api/status is not wired: the routing page cannot read input/format/control-link facts; check the session and the daemon.',
    chain: {
      title: 'Current routing chain (input → matrix → DAC output)',
      desc: 'Read-only: AES67 input (/api/status.format.aes67) → matrix lanes (format.channels) → DAC physical outputs (/api/channels.output + mapping). Fields without a data source show "not wired" plus the reason; no lane i → DAC i identity mapping is assumed.',
      input: {
        title: 'Input · AES67',
        channels: 'Receive channels',
        map: 'Channel map',
        source: 'Input source',
        sourceSdp: 'SDP descriptor (sdp)',
        sourceManual: 'Manual configuration (manual)'
      },
      matrix: {
        title: 'Matrix domain',
        lanes: 'Lanes',
        routedLanes: 'Routed lanes',
        routes: 'Routes'
      },
      output: {
        title: 'Output · DAC',
        channels: 'DAC channels',
        dacMap: 'DAC map (0-based)',
        knownLabels: 'Known physical positions',
        knownLabelsValue: '{count} known label positions'
      },
      note: 'The matrix lane count comes from the data-plane format (/api/status.format.channels); output channels / DAC map come from /api/channels (actual rt-host configuration + the running plan routing section). When both are missing this page does not draw an identity path.'
    },
    plan: {
      title: 'Plan routing section summary',
      desc: 'Composed from /api/status.plan and /api/plan/history; routing details come from /api/channels (mapping / dac_channels).',
      field: {
        version: 'Plan version',
        latency: 'Latency (samples)',
        segments: 'Segments',
        graphId: 'graph_id',
        appliedAt: 'Applied at',
        source: 'Plan source',
        historyCurrent: 'History current version'
      },
      versionReason: 'The running plan version is unreadable (neither /api/status nor /api/plan/history has a value)',
      latencyReason: 'The plan latency is unreadable (neither status nor history has a value)',
      historyReason: 'Plan history has not been read or is unavailable',
      segmentsReason:
        'No read-only endpoint reports the applied plan section list/count: the daemon reports segments/segment_count only in the POST /api/plan/upload ack (T55); /api/status and /api/plan/history do not contain it. This page never passes the route count off as the segment count; the upload/view entry is on /channels.',
    },
    notWired: {
      title: 'Capabilities reported as not wired by the backend',
      desc: 'Verbatim entries from /api/channels.not_wired and /api/status.not_wired (capability gaps declared by the daemon itself).',
      empty: 'The backend reported no not-wired entries.',
      source: {
        channels: '/api/channels',
        status: '/api/status'
      }
    }
  },
  unit: {
    db: 'dB',
    hz: 'Hz',
    khz: 'kHz',
    mhz: 'MHz',
    ms: 'ms',
    s: 's',
    percent: '%',
    blocks: 'blocks',
    packets: 'pkts',
    channels: 'ch',
    bytes: 'B',
    kib: 'KiB',
    mib: 'MiB',
    gib: 'GiB',
    tib: 'TiB',
    samples: 'samples'
  }
}

export default messages