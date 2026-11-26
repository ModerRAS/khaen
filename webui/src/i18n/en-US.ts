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
    retry: 'Retry'
  },
  nav: {
    overview: 'Overview',
    volume: 'Volume & mute',
    channels: 'Plan & channels',
    routing: 'Routing matrix',
    format: 'Audio format',
    calibration: 'Calibration',
    system: 'System',
    logs: 'Logs'
  },
  navGroup: {
    main: 'Main',
    system: 'System'
  },
  navFlag: {
    planned: 'Planned'
  },
  shell: {
    brandSub: 'DSP console',
    collapse: 'Collapse sidebar',
    expand: 'Expand sidebar',
    menu: 'Open navigation',
    navDisabled: 'This page is not wired yet (planned {milestone})',
    stubBanner:
      'M2: the overview page is wired to the daemon /api/status aggregate (5s polling, paused while the tab is hidden). Volume, plan & channels, routing, format, calibration and system pages are not wired yet; until then every value is a "not wired" placeholder, never fake data.',
    hostLink: 'Control link',
    hostLinkNotWired: 'Control link not wired',
    hostLinkConnected: 'Control link connected',
    hostLinkStale: 'Control link status is stale',
    hostLinkDisconnected: 'Control link disconnected'
  },
  topbar: {
    runState: 'Run state',
    plan: 'Plan version',
    deviceRate: 'Device rate',
    hostLink: 'Control link',
    changePassword: 'Change password',
    logout: 'Sign out',
    comingSoon: 'Wired in {milestone}',
    appearance: 'Appearance',
    language: 'Language',
    userMenu: 'User menu',
    themeToggle: 'Toggle dark / light'
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
      alreadyConfigured: 'The admin password is already set. Please sign in.'
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
        'The control link (T70f) is not wired yet: every host_link field is null, so changing control state requires a backend restart.',
      hostLinkNotWiredReason: 'The control link (T70f) is not wired yet: {reason}',
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
        stub: 'The calibration page lands in M5; this card only reads the last job reported by the daemon.'
      },
      control: {
        title: 'Control link',
        desc: 'daemon ↔ rt-host control path (T70f)',
        field: {
          connected: 'Connected',
          statusAgeMs: 'Status age',
          since: 'Connected since',
          socket: 'Socket path'
        },
        notWiredHint: 'The control link (T70f) is not wired yet: changing control state requires a backend restart.'
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
