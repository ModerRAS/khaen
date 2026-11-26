/**
 * zh-CN —— **源语言**（唯一事实来源）。
 *
 * 规则（PLAN §12.2）：
 * - 键命名 <scope>.<page|component>.<key>，全部界面文案必须走这里，不在 .vue 里硬编码；
 * - en-US 必须与本文件键集完全对等（scripts/i18n-check.mjs 强制，vue-tsc 也用 Messages 类型把关）；
 * - 后端稳定错误码（如 E_DEVICE_RATE_MISMATCH）在前端映射成本地化文案，原始 message 只作详情。
 */
const messages = {
  app: {
    name: 'khaen',
    tagline: '实时音频 DSP 引擎控制台',
    version: '版本 {version}'
  },
  common: {
    dash: '—',
    notWired: '未接入',
    refresh: '刷新',
    cancel: '取消',
    confirm: '确认',
    close: '关闭',
    loading: '加载中',
    unknown: '未知'
  },
  nav: {
    overview: '概览',
    volume: '音量与静音',
    channels: '方案与声道',
    routing: '路由矩阵',
    format: '音频格式',
    calibration: '校准',
    system: '系统信息',
    logs: '日志'
  },
  navGroup: {
    main: '主功能',
    system: '系统'
  },
  navFlag: {
    planned: '待接入'
  },
  shell: {
    brandSub: 'DSP 控制台',
    collapse: '收起侧栏',
    expand: '展开侧栏',
    menu: '打开导航',
    navDisabled: '该页面尚未接入（计划 {milestone}）',
    stubBanner: 'M1：认证与会话已接入；状态与控制类接口自 M2 起逐个接入，未接入前一律显示「未接入」，不展示假数据。',
    hostLink: '控制通道',
    hostLinkNotWired: '控制通道未接入'
  },
  topbar: {
    runState: '运行状态',
    plan: '方案版本',
    deviceRate: '设备速率',
    hostLink: '控制通道',
    refreshTodo: '手动刷新待接入（计划 {milestone}）',
    changePassword: '修改密码',
    logout: '退出登录',
    comingSoon: '{milestone} 接入',
    appearance: '外观',
    language: '语言',
    userMenu: '用户菜单',
    themeToggle: '切换深浅色'
  },
  theme: {
    label: '主题',
    system: '跟随系统',
    dark: '深色',
    light: '浅色',
    toggleToDark: '切换到深色',
    toggleToLight: '切换到浅色'
  },
  locale: {
    label: '语言',
    zhCN: '简体中文',
    enUS: 'English'
  },
  status: {
    running: '运行中',
    faulted: '故障',
    muted: '已静音',
    safe: '安全态',
    unknown: '未知'
  },
  truth: {
    actual: '实测',
    demo: '示意',
    notWired: '未接入'
  },
  login: {
    pageTitle: '登录',
    title: {
      setup: '初始化管理员密码',
      login: '登录 khaen'
    },
    desc: {
      setup: '首次使用：设置管理员密码（至少 {min} 个字符），保存后自动进入控制台。',
      login: '输入管理员密码进入控制台。'
    },
    field: {
      password: '密码',
      confirm: '再次输入密码'
    },
    placeholder: {
      password: '请输入密码',
      confirm: '请再次输入密码'
    },
    action: {
      submitSetup: '设置密码并进入',
      submitLogin: '登录',
      retry: '重试'
    },
    state: {
      checking: '正在检查认证状态…',
      unavailable: '无法连接 daemon'
    },
    validation: {
      required: '请输入密码',
      tooShort: '密码至少 {min} 个字符',
      mismatch: '两次输入的密码不一致'
    },
    notice: {
      sessionExpired: '会话已过期，请重新登录。',
      alreadyConfigured: '管理员密码已设置，请直接登录。'
    },
    footer: {
      version: '版本 {version}',
      build: '构建 {id}'
    }
  },
  error: {
    auth: {
      invalidCredentials: '密码错误，请重试',
      invalidCurrentPassword: '当前密码不正确',
      rateLimited: '尝试次数过多（每分钟最多 5 次），请稍后再试',
      alreadyConfigured: '管理员密码已设置，请直接登录',
      notConfigured: '服务端尚未初始化管理员密码，请刷新页面重试',
      csrfInvalid: '请求校验失败，请刷新页面后重试',
      validation: '提交内容不合法',
      unauthenticated: '请先登录',
      sessionExpired: '会话已过期，请重新登录',
      network: '无法连接 daemon（后端未启动或网络中断），请检查后重试',
      server: '服务器错误，请稍后重试',
      unknown: '未知错误，请重试'
    }
  },
  overview: {
    title: '概览',
    subtitle: '运行状态、输入/输出统计、当前方案与控制通道。数据来自 daemon 的 /api/status 聚合（M2 接入）。',
    band: {
      runState: '运行状态',
      lastError: '最近错误',
      hostLink: '控制通道',
      lastUpdate: '最后更新 {time}',
      neverUpdated: '尚未刷新'
    },
    card: {
      runtime: {
        title: '运行状态',
        desc: 'rt-host 块引擎状态与 uptime',
        field: {
          state: '状态',
          muted: '静音',
          safe: '安全态',
          blocks: '已处理块数',
          uptime: '运行时长'
        }
      },
      input: {
        title: '输入统计',
        desc: 'AES67 接收端计数与欠载',
        field: {
          packets: '收包数',
          underruns: '欠载次数',
          jitterFill: '抖动缓冲填充',
          ssrc: 'SSRC',
          queueFullDrops: '队列满丢弃',
          recvErrors: '接收错误'
        }
      },
      output: {
        title: '输出统计',
        desc: 'ALSA 播放计数与 xrun',
        field: {
          blocks: '播放块数',
          xruns: 'xrun',
          errors: '输出错误',
          bytes: '输出字节'
        }
      },
      plan: {
        title: '当前方案',
        desc: '已应用的方案文件与版本',
        field: {
          graphId: 'graph_id',
          sha256: 'sha256',
          version: '版本',
          latencySamples: '延迟（samples）',
          appliedAt: '应用时间',
          source: '来源'
        }
      },
      format: {
        title: '格式与设备',
        desc: '方案速率与实际设备速率',
        field: {
          planRate: '方案速率',
          deviceRate: '设备速率',
          blockFrames: '块长',
          channels: '声道数',
          encoding: '编码'
        }
      },
      aoip: {
        title: 'AES67',
        desc: 'AoIP 接收参数与通道映射',
        field: {
          group: '组播地址',
          port: '端口',
          pt: 'payload type',
          channels: '接收声道',
          map: '通道映射'
        }
      },
      calibration: {
        title: '最近校准',
        desc: '最近一次校准 job 与结论',
        field: {
          lastJob: '最近 job',
          verdict: '结论'
        }
      },
      control: {
        title: '控制通道',
        desc: 'daemon ↔ rt-host 控制链路（T70f）',
        field: {
          connected: '已连接',
          statusAgeMs: '状态新鲜度'
        }
      }
    },
    notWired: {
      reason: '未接入：等待 {milestone} 提供该数据源'
    }
  },
  unit: {
    db: 'dB',
    hz: 'Hz',
    ms: 'ms',
    s: 's',
    percent: '%',
    blocks: '块',
    bytes: 'B',
    samples: 'samples'
  }
}

export type Messages = typeof messages
export default messages
