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
    unknown: '未知',
    yes: '是',
    no: '否',
    none: '无',
    retry: '重试'
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
    stubBanner:
      'M2：概览页已接 daemon 的 /api/status 聚合（5s 轮询，页面隐藏暂停）；音量、方案与声道、路由、格式、校准、系统页面尚未接入，未接入前一律显示「未接入」，不展示假数据。',
    hostLink: '控制通道',
    hostLinkNotWired: '控制通道未接入',
    hostLinkConnected: '控制通道已连接',
    hostLinkStale: '控制通道状态过期',
    hostLinkDisconnected: '控制通道已断开'
  },
  topbar: {
    runState: '运行状态',
    plan: '方案版本',
    deviceRate: '设备速率',
    hostLink: '控制通道',
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
    },
    status: {
      network: '无法获取状态：daemon 不可达（后端未启动或网络中断），请检查后重试。',
      timeout: '获取状态超时，请重试。',
      unauthorized: '会话已过期，请重新登录。',
      server: 'daemon 返回服务器错误，请稍后重试。',
      invalidResponse: 'daemon 返回的状态结构不合法（不是 khaen-status/v1），已拒绝显示。',
      unknown: '获取状态失败，请重试。'
    }
  },
  overview: {
    title: '概览',
    subtitle:
      '运行状态、输入/输出统计、当前方案与控制通道。数据来自 daemon 的 /api/status 聚合，5 秒轮询，页面隐藏时暂停。',
    error: {
      title: '无法获取运行状态'
    },
    band: {
      runState: '运行状态',
      lastError: '最近错误',
      hostLink: '控制通道',
      lastUpdate: '最后更新 {time}',
      neverUpdated: '尚未刷新',
      noError: '无'
    },
    hostLink: {
      connected: '已连接',
      stale: '已过期',
      disconnected: '已断开',
      disconnectedStale: '已断开（状态过期）'
    },
    notice: {
      stale: '控制通道状态过期（连接={connected}，新鲜度={age}）；当前显示的是最后一次成功获取的数据。',
      hostLinkNotWired: '控制通道（T70f）尚未接入：host_link 字段全部为 null，变更控制需重启后端。',
      hostLinkNotWiredReason: '控制通道（T70f）尚未接入：{reason}',
      refreshFailed: '刷新失败：{message}（最后成功更新 {time}）。仍在显示最后一次成功获取的数据。'
    },
    uptime: {
      withDays: '{d} 天 {h} 时 {m} 分 {s} 秒',
      withoutDays: '{h} 时 {m} 分 {s} 秒'
    },
    calibration: {
      none: '尚无记录'
    },
    device: {
      present: '在位',
      absent: '不在位'
    },
    plan: {
      source: {
        upload: '上传',
        calibration: '校准'
      }
    },
    copy: {
      action: '复制',
      copied: '已复制',
      failed: '复制失败（浏览器未允许访问剪贴板）'
    },
    notWiredList: {
      title: '未接入项（daemon 上报）',
      desc: '以下能力由 /api/status 的 not_wired 如实上报为未接入，页面不展示假数据。'
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
          underrunRate: '欠载率',
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
        desc: '方案速率、设备回读与实际输出格式',
        field: {
          planRate: '方案速率',
          deviceRate: '设备速率',
          rateAsserted: '速率已确认',
          blockFrames: '块长',
          channels: '声道数',
          encoding: '编码',
          deviceId: '设备 id',
          deviceState: '设备状态',
          deviceCard: 'ALSA 卡',
          rateReadback: '速率回读'
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
          verdict: '结论',
          activeJob: '进行中 job'
        },
        stub: '校准页在 M5 接入，本卡只读展示 daemon 上报的最近一次 job。'
      },
      control: {
        title: '控制通道',
        desc: 'daemon ↔ rt-host 控制链路（T70f）',
        field: {
          connected: '已连接',
          statusAgeMs: '状态新鲜度',
          since: '连接起始时间',
          socket: 'socket 路径'
        },
        notWiredHint: '控制通道（T70f）未接入：变更控制需重启后端。'
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
    blocks: '块',
    packets: '包',
    channels: '声道',
    bytes: 'B',
    kib: 'KiB',
    mib: 'MiB',
    gib: 'GiB',
    tib: 'TiB',
    samples: 'samples'
  }
}

export type Messages = typeof messages
export default messages
