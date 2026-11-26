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
    unknown: 'Unknown'
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
      'M0/M1 skeleton: the backend API is not wired yet. Every value on this page is a "not wired" placeholder; no fake data is shown.',
    hostLink: 'Control link',
    hostLinkNotWired: 'Control link not wired'
  },
  topbar: {
    runState: 'Run state',
    plan: 'Plan version',
    deviceRate: 'Device rate',
    hostLink: 'Control link',
    refreshTodo: 'Manual refresh is not wired yet (planned {milestone})',
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
      confirm: 'Repeat password',
      token: 'Access token (advanced)'
    },
    placeholder: {
      password: 'Enter password',
      confirm: 'Repeat password',
      token: 'Optional: sign in with X-KHAEN-Token'
    },
    action: {
      submitSetup: 'Set password and continue',
      submitLogin: 'Sign in'
    },
    validation: {
      required: 'Password is required',
      tooShort: 'Password must be at least {min} characters',
      mismatch: 'The two passwords do not match'
    },
    stub: {
      submitBlocked: 'Not wired in M1 yet: the auth API does not exist, so this form cannot sign in for real.',
      note: 'The login / setup endpoints (/api/auth/*) land in M1. This page is a static shell with local validation only.',
      previewLabel: 'Preview state (removed once M1 lands)',
      previewSetup: 'Setup',
      previewLogin: 'Sign in'
    },
    footer: {
      version: 'Version {version}',
      build: 'Build {id}'
    }
  },
  overview: {
    title: 'Overview',
    subtitle:
      'Run state, input/output counters, current plan and the control link. Data comes from the daemon /api/status aggregate (wired in M2).',
    band: {
      runState: 'Run state',
      lastError: 'Last error',
      hostLink: 'Control link',
      lastUpdate: 'Updated {time}',
      neverUpdated: 'Never refreshed'
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
        desc: 'Plan rate against the actual device rate',
        field: {
          planRate: 'Plan rate',
          deviceRate: 'Device rate',
          blockFrames: 'Block frames',
          channels: 'Channels',
          encoding: 'Encoding'
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
          verdict: 'Verdict'
        }
      },
      control: {
        title: 'Control link',
        desc: 'daemon ↔ rt-host control path (T70f)',
        field: {
          connected: 'Connected',
          statusAgeMs: 'Status age'
        }
      }
    },
    notWired: {
      reason: 'Not wired: waiting for {milestone} to provide this data source'
    }
  },
  unit: {
    db: 'dB',
    hz: 'Hz',
    ms: 'ms',
    s: 's',
    percent: '%',
    blocks: 'blocks',
    bytes: 'B',
    samples: 'samples'
  }
}

export default messages
