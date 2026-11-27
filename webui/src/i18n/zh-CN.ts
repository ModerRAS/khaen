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
    retry: '重试',
    gapTask: '（任务 {task}）'
  },
  nav: {
    overview: '概览',
    volume: '音量与静音',
    mode: '工作模式',
    channels: '方案与声道',
    routing: '路由矩阵',
    layout: '内容布局',
    speakers: "音频映射",
    format: '音频格式',
    calibration: '校准',
    system: '系统信息',
    logs: '日志'
  },
  navGroup: {
    main: '主功能',
    system: '系统'
  },
  shell: {
    collapse: '收起侧栏',
    expand: '展开侧栏',
    menu: '打开导航',
    hostLinkNotWired: '控制通道未接入',
    hostLinkConnected: '控制通道已连接',
    hostLinkStale: '控制通道状态过期',
  },
  topbar: {
    runState: '运行状态',
    plan: '方案版本',
    deviceRate: '设备速率',
    changePassword: '修改密码',
    logout: '退出登录',
    userMenu: '用户菜单',
    themeToggle: '切换深浅色',
    version: '版本',
    versionHint: 'release 版本（来自 daemon /healthz）；未知时不显示数值'
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
      alreadyConfigured: '管理员密码已设置，请直接登录。',
      passwordChanged: '密码已修改，所有会话已失效；请用新密码重新登录。'
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
    },
    calibration: {
      network: '无法连接 daemon（后端未启动或网络中断），请检查后重试。',
      timeout: '校准请求超时，请重试。',
      unauthorized: '会话已过期，请重新登录。',
      notWired: '校准后端尚未接入：该操作不可用（不展示假数据/假成功）。',
      busy: '已有校准 job 正在进行，请等待完成或取消后再试。',
      conflict: '当前状态不允许该操作（可能已应用/已回滚/未满足前置条件），请刷新后重试。',
      notFound: '目标不存在（job/上传/产物可能已被删除）。',
      invalidRequest: '请求参数不合法，请检查表单后重试。',
      uploadTooLarge: '上传文件超过大小上限（64 MiB）。',
      unsupportedFormat: '不支持的文件格式：校准输入要求 48 kHz / 16–24 bit / 单声道或双声道 WAV。',
      server: 'daemon 返回服务器错误，请稍后重试。',
      invalidResponse: 'daemon 返回的校准数据结构不合法，已拒绝显示。',
      unknown: '校准操作失败，请重试。'
    },
    system: {
      network: '无法获取系统信息：daemon 不可达（后端未启动或网络中断），请检查后重试。',
      timeout: '获取系统信息超时，请重试。',
      unauthorized: '会话已过期，请重新登录。',
      invalidResponse: 'daemon 返回的系统信息结构不合法，已拒绝显示。',
      disabled: '后端未允许该操作（如需重启，daemon 需以 KHAEN_ALLOW_RESTART=1 启动）。',
      unavailable: '后端未接入该能力（journalctl / systemctl 不可用或尚未实现）：不展示假数据。',
      validation: '提交内容不合法，请检查后重试。',
      server: '服务器错误，请稍后重试。',
      unknown: '系统信息操作失败，请重试。'
    },
    control: {
      network: '无法连接 daemon（后端未启动或网络中断），请检查后重试。',
      timeout: '控制请求超时，请重试。',
      unauthorized: '会话已过期，请重新登录。',
      hostLinkUnavailable:
        'daemon 与 rt-host 的控制通道不可用（控制 socket 不可达）：写操作未发送，页面不会显示假成功。',
      unsupported: '后端明确不支持该操作（unsupported）：不会假装成功。',
      notWired: '该能力尚未接入 daemon（not_wired）：不展示假数据。',
      noBasePlan: '还没有已生效的方案可作为音量补丁的基线（no_base_plan）：请先在方案页应用一份方案。',
      tokenInvalid: 'engine_token 无效或草稿已被替换（engine_token_invalid）：请重新准备。',
      admissionRejected: '方案补丁被准入拒绝（admission_rejected）：请调整增益后重试。',
      noHistory: '没有可回滚的历史版本（no_history）。',
      notFound: '找不到目标资源（后端可能尚未实现该端点）。',
      invalidRequest: '请求参数不合法（bad_request）：请检查输入后重试。',
      conflict: '当前状态不允许该操作（冲突），请刷新后重试。',
      server: 'daemon 返回服务器错误，请稍后重试。',
      invalidResponse: 'daemon 返回的控制面结构不合法，已拒绝显示（不把畸形响应当成功）。',
      unknown: '控制面操作失败，请重试。'
    },
    channels: {
      shaMismatch: '客户端与服务端计算的 sha256 不一致：已拒绝继续（不信任任何一侧）。',
      shaUnavailable: '浏览器不支持 WebCrypto SHA-256：无法完成上传校验（不支持降级为不校验）。',
      planPathUnavailable:
        '上传回执没有 plan_path（stored_path）：daemon 未返回方案的落盘路径，无法调用 POST /api/plan/inspect 或 /api/prepare；页面不猜路径、不伪造预览/草稿。',
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
      hostLinkNotWired: '控制通道不可用：host_link 字段全部为 null；变更控制需要 daemon 与 rt-host 的控制通道恢复。',
      hostLinkNotWiredReason: '控制通道不可用：{reason}',
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
        stub: '本卡只读展示 daemon /api/calibration/jobs 上报的最近一次 job；拿不到数据时显示未接入原因，不展示假数据。'
      },
      control: {
        title: '控制通道',
        desc: 'daemon ↔ rt-host 控制链路',
        field: {
          connected: '已连接',
          statusAgeMs: '状态新鲜度',
          since: '连接起始时间',
          socket: 'socket 路径'
        },
        notWiredHint: '控制通道不可用：host_link 字段为 null，变更控制需要控制通道恢复。'
      }
    }
  },
  calibration: {
    review: {
      applyState: '应用状态',
      appliedVersion: '已应用版本',
      before: '应用前（当前运行方案）',
      after: '应用后（job 产物）',
      beforeHint: '应用前快照：失败时回滚到该版本。',
      snapshot: '应用前快照',
      version: '版本',
      graphId: 'graph_id',
      sha256: 'sha256',
      apply: '应用校准方案',
      rollback: '回滚到应用前版本',
      noJob: '没有可应用的 job。',
      exportConfig: '导出配置 JSON',
      applyReceipt: '最近应用回执',
      rollbackReceipt: '最近回滚回执',
      appliedAt: '应用时间',
      restoredVersion: '恢复版本',
      ack: '回执类型',
      ackActual: 'actual（真实生效）',
      ackSynthetic: 'synthetic（模拟提交）',
      confirmApply: {
        title: '应用校准方案？',
        message: '将把 job 产出的方案 prepare → commit 到运行方案；应用前会记录快照（当前版本 {version}），失败可回滚。',
        ok: '确认应用'
      },
      confirmRollback: {
        title: '回滚到应用前版本？',
        message: '将把运行方案恢复到应用前版本 {version}（graph_id {graphId}）。',
        ok: '确认回滚'
      },
      appliedToast: '已应用校准方案',
      rolledBackToast: '已回滚到应用前版本'
    },
    wizard: {
      pageTitle: '校准向导',
      subtitle:
        'Dirac 式 8 步离线校准闭环（P1）。每步门禁由 daemon 的 /api/calibration/wizard/state 判定；缺失能力（多点/ART、实时测量等）灰态明示，不伪造进度与结果。',
      stepNav: '向导步骤',
      stepShort: '第 {step} 步',
      stepOf: '第 {current} / {total} 步',
      backToList: '返回校准列表',
      previous: '上一步',
      next: '下一步',
      nextSkip: '下一步（此步未接入）',
      finish: '完成',
      cancelWizard: '取消向导',
      confirmCancel: {
        title: '退出向导？',
        message: '向导配置、测量上传与已完成的步骤会保留在后端；可以随时回来继续。',
        ok: '退出向导'
      },
      unavailable: {
        title: '校准后端未接入',
      },
      error: {
        title: '无法读取向导状态'
      },
      actionError: '操作失败：{message}',
      toast: {
        saved: '校准设置已保存',
        uploaded: '上传完成',
        jobCreated: '分析 job 已创建',
        jobCancelled: 'job 已取消'
      },
      device: {
        desc: '数据来自 /api/status（设备 / 格式 / 控制通道），本步门禁由 daemon 判定。',
        device: '设备',
        deviceState: '设备状态',
        deviceCard: 'ALSA 卡',
        deviceRate: '设备速率',
        rateAsserted: '速率已确认',
        hostLink: '控制通道',
        hostLinkAge: '状态新鲜度',
        runState: '运行状态',
        channels: '输出声道数',
        encoding: '编码',
        noStatus: '尚未获取 /api/status 数据（后端未接入或请求失败），本步不显示任何数值。',
        statusError: '无法获取 /api/status：{message}'
      },
      channels: {
        desc: '只读展示 /api/status 与 /api/channels 的格式/声道事实；当前值拿不到时显示未接入原因，不展示假数据。',
        notAvailable: '/api/channels 不可用：本步只读展示，写入请使用「方案与声道」页的热重配两阶段。',
        noMapping: '不展示也不伪造声道映射：只显示 daemon 上报的 mapping；未知时显示原因。',
        readOnly: '本步只读，不产生任何配置。',
        planRate: '方案速率',
        deviceRate: '设备速率',
        channels: '输出声道数',
        encoding: '编码',
        aes67Channels: 'AES67 接收声道',
        aes67Map: 'AES67 声道映射',
        aes67Group: 'AES67 组播地址',
        aes67Port: 'AES67 端口',
        task: '缺失能力'
      },
      mic: {
        desc: '上传麦克风校准文件、选择朝向与目标曲线、设置单点参数；保存后由 daemon 判定本步门禁。',
        file: '麦克风校准文件',
        fileHint: 'UMIK-1 校准 txt（≤ 1 MiB）；上传后显示服务端计算的 SHA-256。',
        fileSelect: '选择校准文件',
        fileTooLarge: '校准文件超过 1 MiB 上限',
        savedSha: '已保存 SHA-256',
        filename: '文件名',
        orientation: '麦克风朝向',
        orientation0: '0°（指向声源）',
        orientation90: '90°（指向天花板）',
        targetMode: '目标曲线',
        targetFlat: 'flat（平坦）',
        targetTilt10: 'tilt10（−10 dB 于 20 kHz）',
        centers: '中心频率（Hz，逗号分隔）',
        centersHint: '1/3-oct 中心频率，最多 64 个；默认 25 Hz–20 kHz（30 个）。',
        tolerance: '容差（dB）',
        minBands: '最少频段数',
        save: '保存设置',
        saving: '正在保存…',
        savedAt: '已保存（{time}）',
        notSaved: '尚未保存',
        validation: {
          micRequired: '请先上传麦克风校准文件',
          orientationRequired: '请选择麦克风朝向',
          targetRequired: '请选择目标曲线',
          centersRange: '中心频率必须是 1–24000 Hz 的数字（逗号/空格分隔）',
          centersTooMany: '最多 64 个中心频率',
          toleranceRange: '容差必须在 0.1–24 dB 之间',
          minBandsRange: '最少频段数必须是 1–64 的整数'
        }
      },
      positions: {
        desc: 'khaen P1 只支持单点（皇帝位）；多点与 ART 不在本版本目标内。',
        single: '单点（皇帝位）',
        singleDesc: '固定主听音位；本版本唯一支持的测点方案。',
        selected: '已确认',
        notSelected: '待确认（需先保存第 3 步设置）',
        multi: '多点测量',
        multiReason: 'khaen 目标不含多点：需要多点测量后端。',
        art: 'ART（主动房间处理）',
        artReason: 'ART 需要多点测量与支持组，khaen 当前没有对应后端。'
      },
      measure: {
        desc: 'P1 仅离线：上传 baseline 与 applied 两段 WAV；实时测量需要采集服务（后续版本）。',
        baseline: 'baseline（校准前）',
        applied: 'applied（校准后）',
        hint: '48 kHz / 16–24 bit / 单声道或双声道；≤ 64 MiB。',
        select: '选择 WAV',
        format: '格式',
        duration: '时长',
        formatValue: '{rate} / {bits} bit / {channels} ch',
        savedBaseline: '已保存 baseline',
        savedApplied: '已保存 applied',
        realtime: '实时测量（麦克风采集 / 自动扫频）',
        realtimeReason: 'daemon 没有采集服务，实时测量在本版本不可用（需要 T42 延伸的实时测量后端，P2）。',
        realtimeTask: 'C',
        parseError: '文件不是可用的 WAV：{message}',
        formatError: '格式不符合校准要求：{message}',
        pairError: '两段录音结构不一致：{message}',
        wavErrors: {
          file_too_large: '文件超过 64 MiB 上限',
          file_too_small: '文件太小，不是有效 WAV',
          not_riff: '不是 RIFF 容器（缺 RIFF 头）',
          not_wave: '不是 WAVE 格式',
          missing_fmt: '缺少 fmt chunk',
          missing_data: '缺少 data chunk',
          unsupported_codec: '不支持的编码（只接受 PCM）',
          bad_sample_rate: '采样率必须是 48 kHz',
          bad_bits: '位深必须是 16 或 24 bit',
          bad_channels: '声道数必须是 1 或 2',
          bad_block_align: 'blockAlign 与声道/位深不一致',
          bad_data_length: 'data chunk 长度非法或被截断',
          format_mismatch: '两段录音的采样率/声道/位深不一致',
          length_mismatch: '两段录音长度不一致（±1 帧以内才接受）'
        }
      },
      analysis: {
        desc: '创建单点离线 job；进度与结果全部来自 daemon 的真实 job 状态（无假进度条）。',
        planSha: '当前方案 sha256（来自 /api/status）',
        planMissing: '当前没有已应用的方案（status.plan.sha256 为 null）',
        uploadsMissing: '需要先在第 5 步上传 baseline / applied 两段 WAV',
        settingsMissing: '需要先在第 3 步保存校准设置',
        start: '开始分析',
        starting: '正在创建 job…',
        retry: '用相同输入重试',
        cancel: '取消 job',
        jobId: 'job id',
        noJob: '尚未创建分析 job。',
        failedTitle: '分析失败',
        cancelledTitle: '分析已取消',
        cancelledDesc: '可重试或返回上一步。',
        errorCode: '稳定错误码',
        cancelConfirm: {
          title: '取消分析 job？',
          message: '将取消进行中的 job {id}；已完成的步骤保留。',
        }
      },
      design: {
        desc: '展示 job 产出的方案摘要、设计残留（FULL / PARTIAL / NONE）与准入结果。',
        noResult: 'job 尚未成功，没有设计产物。',
        nextBlocked: '没有可应用的产物：下一步被门禁阻止。'
      },
      review: {
        desc: '应用前展示 before / after 与快照；应用与回滚都需要二次确认，回执来自 daemon 的真实 commit 结果。'
      }
    },
    pageTitle: '校准',
    subtitle:
      '校准 job 列表与 8 步向导（P1 离线闭环）。数据来自 daemon 的校准 REST（/api/calibration/*）；后端未接入时如实显示「未接入」，不展示假数据。',
    verdict: {
      none: '—'
    },
    engine: {
      label: '引擎：{engine}'
    },
    jobStatus: {
      queued: '排队中',
      running: '运行中',
      succeeded: '成功',
      failed: '失败',
      cancelled: '已取消'
    },
    jobStepStatus: {
      pending: '等待',
      running: '进行中',
      succeeded: '完成',
      failed: '失败',
      cancelled: '已取消',
      skipped: '已跳过'
    },
    applyState: {
      not_applied: '未应用',
      applied: '已应用',
      rolled_back: '已回滚'
    },
    list: {
      title: '校准 job',
      newWizard: '新建校准向导',
      engine: '引擎真实性',
      engineUnknown: '未上报',
      summary: {
        lastJob: '最近 job',
        wizard: '向导状态',
        uploads: '测量上传',
        none: '尚无校准 job',
        backendNotWired: '校准后端未接入',
        wizardStep: '当前第 {step} / 8 步',
        gatesPassed: '门禁通过 {passed} / {total} 步',
        openWizard: '打开向导',
        baseline: 'baseline',
        applied: 'applied'
      },
      emptyTitle: '暂无校准 job',
      emptyDesc: '还没有运行过校准分析；从 8 步向导开始（P1 只支持离线 WAV 闭环）。',
      emptyUnavailableTitle: '校准后端未接入，暂无 job 数据',
      table: {
        id: 'job id',
        kind: '类型',
        status: '状态',
        verdict: '结论',
        created: '创建时间',
        duration: '耗时',
        actions: '操作'
      },
      action: {
        detail: '详情',
        cancel: '取消'
      },
      cancelConfirm: {
        title: '取消校准 job？',
        message: '将取消进行中的 job {id}；已完成的步骤保留，可重新发起分析。',
        ok: '取消 job',
        done: 'job 已取消'
      },
      pageInfo: '第 {page} / {pages} 页 · 共 {total} 条',
      pager: {
        prev: '上一页',
        next: '下一页'
      }
    },
    job: {
      pageTitle: '校准 job 详情',
      back: '返回校准列表',
      created: '创建时间',
      started: '开始时间',
      finished: '结束时间',
      duration: '耗时',
      status: '状态',
      engine: '引擎',
      applyState: '应用状态',
      steps: '状态时间线',
      artifacts: '产物',
      noArtifacts: '该 job 没有产物（失败/取消或后端未产出）。',
      inputs: '输入参数',
      inputsPlanSha: 'plan sha256',
      inputsBaseline: 'baseline 上传',
      inputsApplied: 'applied 上传',
      inputsCenters: '中心频率（Hz）',
      inputsTolerance: '容差（dB）',
      inputsMinBands: '最少频段数',
      error: '失败原因',
      resultTitle: '分析结果',
      applyTitle: '应用 / 回滚',
      loadError: '无法获取 job 详情',
      notFound: 'job 不存在或已被删除；请返回列表重试。'
    },
    step: {
      names: {
        device_check: '设备检查',
        layout_channels: '布局与声道',
        mic_settings: '麦克风与校准设置',
        positions: '测点',
        measurement: '测量',
        analysis: '分析',
        design: '设计',
        review_apply: '回顾 / 应用 / 回滚'
      },
      jobNames: {
        compile: '编译方案',
        levels: '电平/预测',
        closed_loop: '闭环分析',
        bundle: '产物打包'
      }
    },
    gate: {
      title: '门禁',
      passed: '门禁通过',
      blocked: '门禁未通过',
      not_available: '能力未接入',
      missing: '缺少门禁数据',
      lockedStep: '未解锁的步骤',
      task: '后续任务：{task}',
      canContinue: '可以进入下一步。',
      mustFix: '完成本步后进入下一步；原因见上方。',
      canSkip: '该步能力未接入：不产生数据，可继续（灰态步骤）。',
      reason: '原因：{reason}',
      failClosed: 'daemon 未上报本步门禁，按不可继续处理（fail closed）。'
    },
    result: {
      syntheticHint: '当前 job 的引擎为 {engine}：以下数值标为「示意」，不是 khaen 实测。',
      jobVerdictMismatch: 'job 摘要 verdict={job}，result.verdict={result}（以 result 为准）',
      maxDelta: '最大偏差',
      comparedBands: '比较频段',
      missingBands: '缺失频段',
      violations: '违规项',
      bands: '频段明细',
      noBands: '没有频段数据',
      bandHz: '频率（Hz）',
      predicted: '预测（dB）',
      observed: '实测（dB）',
      delta: '偏差（dB）',
      missing: '缺失',
      empty: 'job 尚无结果（result: null）。'
    },
    design: {
      title: '设计摘要',
      residual: '设计残留',
      admission: '准入结果',
      admitted: '已通过准入',
      rejected: '准入未通过',
      planSha: '产物 plan sha256',
      graphId: '产物 graph_id'
    },
    upload: {
      replace: '重新选择',
      uploading: '正在上传…',
      sha256: 'SHA-256',
      bytes: '字节数'
    }
  },
  system: {
    pageTitle: '系统信息',
    logsPageTitle: '日志',
    subtitle:
      '版本与构建、主机、服务、设备、控制通道、日志、诊断与维护。数据来自 daemon 的 /api/system/*；缺失字段如实显示「未接入」，不展示假数据。',
    logsSubtitle:
      'daemon / rt-host 的 journalctl 最近 N 行（只读）。数据来自 daemon 的 /api/system/logs；journalctl 不可用时如实显示「未接入」。',
    actions: {
      refresh: '刷新',
      downloadDiagnostics: '下载诊断 JSON',
      downloading: '正在生成诊断包…',
      openLogs: '打开完整日志',
      backToSystem: '返回系统信息',
      retry: '重试'
    },
    notices: {
      refreshFailed: '刷新失败：{message}（显示的是最后一次成功获取的数据，最后成功更新 {time}）',
      lastUpdate: '最后更新 {time}',
      neverUpdated: '尚未刷新',
      degraded: '后端有 {count} 个字段未上报（null + 原因）：页面如实显示「未接入」，不展示假数据。',
      partialUnavailable: '{count} 项能力未接入（原因见各字段）'
    },
    error: {
      title: '无法获取系统信息'
    },
    version: {
      title: '版本与构建',
      desc: '区分 crate 版本（编译进二进制）、release 版本（部署发布）与 commit；不一致时页面顶部醒目提示。',
      consistent: 'crate 与 release 一致',
      unknown: '无法判定版本一致性（crate 或 release 版本缺失）',
      mismatchBadge: '版本不一致',
      mismatchTitle: '版本不一致：crate {crate} ≠ release {release}',
      mismatchDesc:
        'crate 版本是编译进二进制的 Cargo 版本，release 版本才是实际部署发布的版本。/healthz 与系统信息必须如实区分；请检查发布流程是否注入了 KHAEN_BUILD_VERSION / KHAEN_BUILD_COMMIT。',
      localBuild: '本地构建（release 版本未知）',
      field: {
        crateVersion: 'crate 版本',
        releaseVersion: 'release 版本',
        commit: 'commit',
        buildProfile: '构建 profile',
        rustc: 'rustc',
        target: 'target',
        webuiBuild: 'webui 构建'
      }
    },
    host: {
      title: '主机',
      desc: 'hostname / 内核 / 系统 / uptime / 监听地址（只读采集）。',
      field: {
        hostname: 'hostname',
        kernel: '内核',
        os: '操作系统',
        uptime: '运行时长',
        bind: '监听地址'
      }
    },
    services: {
      title: '服务',
      desc: 'systemd 服务状态（daemon / rt-host）：active / enabled / PID；命令失败如实显示未接入。',
      daemon: 'daemon',
      rtHost: 'rt-host',
      field: {
        active: 'active',
        enabled: 'enabled',
        pid: 'PID'
      }
    },
    devices: {
      title: '设备',
      desc: '/proc/asound 声卡列表；MOTU 设备单独标出。',
      motu: 'MOTU',
      empty: 'daemon 未上报任何声卡（/proc/asound 不可读或未接入）。',
      field: {
        cardId: 'card id',
        driver: '驱动',
        model: '型号',
        channels: '声道',
        rates: '支持速率'
      },
      noRates: '未上报'
    },
    hostLink: {
      title: '控制通道',
      desc: 'daemon → rt-host 控制链路（与概览页 host_link 同源）。',
      field: {
        connected: '已连接',
        statusAgeMs: '状态新鲜度',
        since: '连接起始时间',
        socket: 'socket 路径'
      }
    },
    restart: {
      title: '维护：重启 rt-host',
      desc: '仅当 daemon 以 KHAEN_ALLOW_RESTART=1 启动且后端允许时显示操作；前端负责二次确认（输入确认词）。',
      disabledTitle: '后端未允许重启',
      disabledReason: 'daemon 未启用重启（需要 KHAEN_ALLOW_RESTART=1）：{reason}',
      button: '重启 rt-host',
      running: '正在请求重启…',
      confirmTitle: '重启 rt-host？',
      confirmDesc:
        '将请求 daemon 执行 systemctl restart khaen-rt-host；重启会中断当前播放（约 1–3 秒），状态文件与会话会重新建立。输入下方确认词后提交。',
      confirmWordLabel: '确认词',
      confirmWord: 'restart',
      confirmWordPlaceholder: '输入 {word} 以确认',
      confirm: '确认重启',
      cancel: '取消',
      confirmMismatch: '确认词不正确，操作未提交',
      success: '已请求重启 rt-host',
      lastRestartedAt: '最近重启请求时间',
      daemonTitle: 'daemon 自身重启',
      daemonReason: '不提供 daemon 自身重启（后端未实现，会返回 501）；请通过 systemd/systemctl 在主机上手动操作。'
    },
    password: {
      title: '修改密码',
      desc: '需要当前密码；修改成功后所有会话失效，必须用新密码重新登录。',
      field: {
        old: '当前密码',
        new: '新密码',
        confirm: '再次输入新密码'
      },
      submit: '修改密码',
      busy: '正在修改…',
      success: '密码已修改：所有会话已失效，请用新密码重新登录。',
      validation: {
        required: '请输入当前密码',
        tooShort: '新密码至少 {min} 个字符',
        mismatch: '两次输入的新密码不一致'
      }
    },
    logs: {
      title: '日志',
      desc: 'journalctl -u <unit> -n <N> --no-pager（只读最近 N 行；N 最多 500）。',
      unit: '单元',
      lines: '行数',
      autoRefresh: '自动刷新',
      copy: '复制日志',
      copied: '已复制日志',
      copyFailed: '复制失败（浏览器未允许访问剪贴板）',
      copyEmpty: '没有可复制的日志',
      empty: '日志为空（daemon 没有返回任何行）。',
      unavailableTitle: '日志不可用',
      unavailableReason: 'daemon 无法读取日志：{reason}',
      source: '来源：{source}',
      requested: '请求 {lines} 行',
      truncated: '已截断',
      lastUpdate: '最后更新 {time}',
      loading: '正在读取日志…',
      errorTitle: '无法获取日志'
    },
    diagnostics: {
      success: '诊断包已下载：{filename}',
      failed: '诊断下载失败',
      notWired: '诊断不可用：{reason}'
    }
  },
  control: {
    error: {
      title: '控制面操作失败'
    },
    actions: {
      prepare: '准备（prepare）',
      commit: '提交（commit）',
      committing: '正在提交…',
      abort: '丢弃草稿（abort）',
      aborting: '正在丢弃…',
      retry: '重试'
    },
    toast: {
      committed: '已提交：控制面已生效（设备回读见 /api/status）',
      aborted: '草稿已丢弃'
    },
    surface: {
      volume: '音量（逐 lane 增益）',
      mode: '工作模式',
      format: '音频格式',
      plan: '方案',
      rollback: '回滚'
    },
    receipt: {
      title: '两阶段回执',
      desc: 'prepare 只准备草稿，commit 才在块边界生效，abort 丢弃。回执来自 daemon 控制面（T70f-b 转发 rt-host），不是页面推断。',
      draftTitle: '草稿（已准备）',
      empty: '尚未准备任何草稿。',
      commitTitle: '提交回执',
      commitEmpty: '尚无提交回执。',
      surface: '界面（surface）',
      engineToken: 'engine_token',
      plannedVersion: '计划版本',
      changed: '是否有变化',
      graphId: 'graph_id',
      mode: '目标模式',
      format: '目标格式',
      deviceRestart: '需要重启设备',
      rolledBackTo: '回滚目标版本',
      note: '后端说明',
      version: '生效版本',
      appliedBlock: '生效块号',
      allAck: '全部确认（all_ack）',
      graphDropped: '旧图已丢弃',
      historyCleared: '历史已清空',
      at: '回执时间',
      commitHint: '提交会在下一个块边界原子应用；失败时保留草稿，可重试或丢弃（abort）。'
    },
    format: {
      summary: '{rate} Hz / {block} frames / {channels} ch'
    },
    deviceRestart: {
      title: '需要重启 rt-host 才能在真实设备生效',
      desc: '控制面引擎已按新格式重建；真实 ALSA 设备必须重开 PCM 流（重启 rt-host）后 device_rate 才会变化。页面不会把「控制面已切换」当成「设备已生效」。',
      goto: '去系统页重启 rt-host'
    },
    history: {
      title: '最近操作',
      empty: '暂无操作记录。',
      ok: '成功',
      failed: '失败',
      action: {
        mute: '静音',
        prepare: '准备',
        commit: '提交',
        abort: '丢弃',
        retry: '设备重试'
      }
    }
  },
  volume: {
    pageTitle: '音量与静音',
    subtitle:
      '静音（POST /api/mute，立即生效并回读）与逐 lane 保护增益（POST /api/surface volume → POST /api/commit，两阶段）。控制协议没有主音量与当前逐 lane 增益的读接口，页面如实显示「未接入」，不编数值。',
    actions: {
      refresh: '刷新'
    },
    error: {
      title: '无法获取运行状态'
    },
    band: {
      mute: '静音',
      mutedOn: '已静音',
      mutedOff: '未静音',
      runState: '运行状态',
      planVersion: '方案版本',
      lastUpdate: '最后更新 {time}',
      neverUpdated: '尚未刷新'
    },
    notice: {
      safeReadOnly: '安全态：音量/模式写入已禁用（只保留静音开关作为恢复路径）。',
      safeReadOnlyReason: '安全态：音量/模式写入已禁用（只保留静音开关作为恢复路径）。原因：{reason}',
      faultedReadOnly: '故障态：音量/模式写入已禁用（只保留静音开关作为恢复路径）。',
      faultedReadOnlyReason: '故障态：音量/模式写入已禁用（只保留静音开关作为恢复路径）。原因：{reason}',
      stale: '控制通道状态过期：当前显示的是最后一次成功获取的数据。',
      refreshFailed: '刷新失败：{message}（最后成功更新 {time}）。仍在显示最后一次成功获取的数据。',
      muteUnknown: '无法读取当前静音状态（/api/status state.muted 为 null）：不显示猜测值。'
    },
    mute: {
      title: '静音',
      desc: 'POST /api/mute 立即生效（安全操作）；成功后刷新 /api/status 回读，不显示本地猜测。',
      on: '已静音',
      off: '未静音',
      switch: '静音开关',
      field: {
        readback: '回读（/api/status state.muted）',
        runState: '运行状态',
        appliedBlock: '回执生效块号'
      },
      noReceipt: '尚无静音回执',
      successMuted: '已静音（回读见 /api/status）',
      successUnmuted: '已取消静音（回读见 /api/status）',
      motu: {
        field: 'MOTU 设备副作用',
        failedField: 'MOTU 设备写入失败',
        applied: '已写入设备并回读',
        skipped: '设备不可用，未写设备（已跳过）',
        notApplied: '设备未回执写入',
        failedToast: '引擎已静音，但 MOTU 设备写入失败：{code} {message}',
        skippedToast: '引擎已静音；MOTU 设备副作用已跳过：{reason}'
      },
      recoveryNote:
        '设备安全锁存后的恢复路径就是显式取消静音：因此故障/安全态下静音开关仍然可用，其它写入被禁用。'
    },
    master: {
      title: '主音量（NOT_AVAILABLE）',
      desc: '控制协议（T70f-a/b）没有 master volume op',
      reason:
        '控制面没有主音量：/api/surface volume 只接受逐 lane 保护增益 （lane, gain_db），没有单一主音量的字段、存储或回读接口。把「写所有 lane」伪装成主音量会覆盖逐 lane 修剪，因此页面不提供主音量控件，改为逐 lane 增益草稿。',
      field: {
        source: '数据来源',
        task: '对应任务',
        readback: '当前主音量回读'
      },
      sourceValue: 'daemon 控制面（T70f-b 转发 rt-host）',
      taskValue: 'R',
      replacedBy: '替代方案：用右侧「逐 lane 增益」逐个设置并提交；当前实际增益不可读（无读接口）。'
    },
    gains: {
      title: '逐 lane 增益草稿',
      desc: 'POST /api/surface （surface: "volume", data: gains_db） → POST /api/commit，两阶段。当前实际保护增益来自 /api/status.readback（base_plan）；草稿仍从 0 dB 中性值开始，勾选要提交的 lane。',
      readbackAvailable: '当前实际逐 lane 保护增益：来自 base_plan 回读（source={source}）',
      readbackMissing: '当前实际逐 lane 保护增益：未接入（控制协议没有读取 per-lane gains 的 op；不编造 0 dB）',
      current: '当前实际值',
      noLanes: '无法生成逐 lane 滑杆：/api/status 未上报 format.channels（原因：{reason}）。',
      noLanesReasonUnknown: 'daemon 未给出原因',
      toolbar: {
        uniform: '统一值',
        applyUniform: '应用到草稿',
        includeAll: '全选',
        clear: '全部归零'
      },
      count: '共 {count} 个 lane（优先 readback.protection.lanes，拿不到时退回 format.channels）',
      include: '参与本次提交',
      lane: 'lane {lane} 增益',
      patchSummary: '将提交 {count} 个 lane 的增益（未勾选的 lane 保持原值）',
      validation: {
        empty: '请先调整并勾选至少一个 lane',
        lane: 'lane 下标越界或重复',
        gain: '增益必须是 -150…+50 dB 之间的有限数'
      },
      lockedHint: '故障/安全态下禁止写入音量',
      emptyPatchHint: '先拖动滑杆或设置统一值，然后勾选要提交的 lane',
      draftReady: '草稿已准备（计划版本 {version}）：在下方「两阶段回执」提交或丢弃',
      prepared: '音量草稿已准备'
    }
  },
  formatPage: {
    pageTitle: '音频格式',
    subtitle:
      '当前实际格式（/api/status）与两阶段格式变更（prepare → commit）。格式属于控制面引擎配置：commit 会重建引擎并丢弃正在跑的图，真实设备需重启 rt-host 重开 PCM 后才按新速率运行。',
    actions: {
      refresh: '刷新'
    },
    error: {
      title: '无法获取运行状态'
    },
    band: {
      runState: '运行状态',
      deviceRate: '设备速率',
      rateAsserted: '速率已确认',
      planVersion: '方案版本',
      lastUpdate: '最后更新 {time}',
      neverUpdated: '尚未刷新'
    },
    notice: {
      safeReadOnly: '安全态：格式写入已禁用（避免在安全态继续改配置）。',
      safeReadOnlyReason: '安全态：格式写入已禁用。原因：{reason}',
      faultedReadOnly: '故障态：格式写入已禁用，请先恢复设备。',
      faultedReadOnlyReason: '故障态：格式写入已禁用，请先恢复设备。原因：{reason}',
      rateNotAsserted: '设备速率尚未确认（rate_asserted=false）：设备可能未按方案速率运行。',
      rateMismatch: '方案速率与设备回读速率不一致：设备未按方案速率运行（重启 rt-host 后回读）。',
      stale: '控制通道状态过期：当前显示的是最后一次成功获取的数据。',
      refreshFailed: '刷新失败：{message}（最后成功更新 {time}）。仍在显示最后一次成功获取的数据。'
    },
    current: {
      title: '当前格式（/api/status）',
      desc: '方案速率、设备速率回读、块长与声道数来自 /api/status。AES67 组地址、端口、载荷类型和映射来自 /api/channels.current。编码没有数据源。',
      sourceHint: '速率与块长来自 /api/status；AES67 来自 /api/channels.current（不读 status 里恒为空的 format.aes67）。',
      aes67Loading: '正在读取 /api/channels，AES67 当前值尚未到达。',
      encodingReason: 'daemon 未上报编码（预留字段，无数据源，留到读回阶段补源）',
      rateNotAsserted: '设备速率尚未确认（rate_asserted=false）',
      retryReadback: '重试回读',
      retryOk: '已请求设备监督器重试并回读',
      retryStillUnasserted: '设备速率仍未确认（rate_asserted 仍为 false）',
      field: {
        planRate: '方案速率',
        deviceRate: '设备速率',
        rateAsserted: '速率已确认',
        blockFrames: '块长',
        channels: '声道数',
        encoding: '编码',
        aes67Channels: 'AES67 接收声道',
        aes67Map: 'AES67 声道映射',
        aes67Group: 'AES67 组播地址',
        aes67Pt: 'AES67 payload type',
        aes67Port: 'AES67 端口'
      }
    },
    apply: {
      title: '应用格式（两阶段）',
      desc: 'prepare → commit；后端返回 changed / device_restart_required 如实显示。',
      field: {
        sampleRate: '采样率',
        block: '块长',
        controlFormat: '控制面回执格式'
      },
      frames: 'frames',
      note: '支持的采样率 48k/96k、块长 64/128（后端能力集合）；设备是否真的支持由后端校验，被拒绝时显示稳定错误码。',
      prepared: '格式草稿已准备',
      selectHint: '请选择采样率或块长（至少一项）',
      lockedHint: '故障/安全态下禁止写入',
      restartTitle: '需要重启 rt-host 才能在真实设备生效',
      restartDesc:
        '控制面引擎已按新格式重建，正在跑的图已丢弃；真实 ALSA 设备必须重开 PCM 流（重启 rt-host）后 device_rate 才会变化，重启前页面不把控制面回执当成设备已生效。',
      noControlReceipt: '尚未提交过格式变更（无控制面回执）'
    }
  },
  channels: {
    pageTitle: '方案与声道',
    actions: {
      refresh: '刷新'
    },
    updated: {
      last: '最后更新 {time}',
      never: '尚未刷新'
    },
    band: {
      runState: '运行状态',
      planVersion: '方案版本',
      outputChannels: '输出声道',
      inputChannels: '输入声道'
    },
    notice: {
      refreshFailed: '刷新失败：{message}（显示的是最后一次成功获取的声道数据）',
      statusRefreshFailed: '/api/status 刷新失败：{message}（最后成功更新 {time}）',
      historyUnavailable: '方案历史不可用：{message}',
      inspectUnavailable: '方案预览（/api/plan/inspect）不可用：只显示上传摘要，不伪造段列表/延迟'
    },
    plan: {
      title: '当前方案',
      desc: '来自 /api/status 的 plan 元数据与 /api/plan/history 的当前版本；缺失字段显示「未接入」+ 原因。',
      field: {
        graphId: 'graph_id',
        sha256: 'sha256',
        version: '版本',
        latency: '延迟（samples）',
        appliedAt: '应用时间',
        source: '来源'
      },
      noPlan: '尚无已应用的方案',
      shaUnavailable: '/api/status 不包含 sha256（无数据源）',
      prepared: '方案草稿已准备',
      committed: '方案已提交（回执见下方两阶段卡）'
    },
    output: {
      title: '输出通道与 MOTU 物理映射',
      desc: '来自 /api/channels（rt-host 实际配置 + 当前方案 routing 段）；已知位置才标注 L/R/SL/SR/TF/TFR/LFE，未知位置显示输出序号。',
      field: {
        channels: '输出通道数',
        dacMap: 'DAC 映射（0-based）',
        knownLabels: '已知位置标签'
      },
      table: {
        lane: 'lane',
        physical: 'MOTU 输出',
        label: '标签'
      },
      unknownLabel: 'OUT {channel}',
      empty: 'daemon 未上报输出通道/映射（/api/channels 未接入或 rt-host 状态不可用）。',
      mappingUnknown:
        '当前方案的 routing 段未知（mapping=null）：daemon 关联不到已生效方案的路由，页面不画 lane → DAC 映射、不假设直通。'
    },
    input: {
      title: 'AES67 输入',
      desc: '来自 /api/channels.input（SDP 描述符或手工配置）；sdp_sha256 用于核对输入描述符版本。',
      field: {
        channels: '接收声道数',
        map: '通道映射',
        source: '输入来源',
        sdpSha: 'SDP sha256'
      },
      source: {
        sdp: 'SDP 描述符（sdp）',
        manual: '手工配置（manual）'
      },
    },
    inspect: {
      title: '方案预览（/api/plan/inspect）',
      unavailable: '方案预览不可用：只显示上传摘要（sha/graph_id/段数/延迟），不伪造段列表。',
      accepted: '已通过准入',
      rejected: '准入未通过',
      field: {
        sampleRate: '采样率（Hz）',
        blockSize: '块长（frames）',
        inChannels: '输入声道',
        lanes: 'lane 数',
        dacChannels: 'DAC 声道',
        latency: '编译延迟（samples）',
        compiled: '图编译',
        admission: '准入'
      }
    },
    upload: {
      title: '方案上传（plan ABI v3）',
      desc: '原始 plan 字节（≤ 4 MiB）→ 浏览器算 sha256 → 服务端校验后返回 graph_id/区段名/段数/延迟/落盘路径（stored_path）→ inspect → prepare → commit。上传不会自动应用。',
      select: '选择 plan 文件',
      noFile: '尚未选择文件',
      tooLarge: '文件超过上传上限 {limit}',
      clientSha: '客户端 sha256',
      upload: '上传并校验',
      uploaded: 'plan 已上传并通过 sha256 校验',
      prepare: '准备应用（prepare）',
      ackTitle: '上传回执（服务端）',
      versionUnavailable: '真实 daemon 的上传回执不含目标版本（版本号在 commit 回执 / 历史里产生）。',
      segmentsUnavailable: '该后端只回报区段数（segments 为数字），不回报区段名：不拿数字伪造区段列表。',
      field: {
        sha256: 'sha256（服务端）',
        graphId: 'graph_id',
        version: '目标版本',
        segments: '区段（sections）',
        segmentCount: '区段数',
        latency: '延迟（samples）'
      }
    },
    history: {
      title: '历史与回滚',
      desc: '/api/plan/history 的已生效版本；回滚会创建一个新版本（历史不改写），且必须先 commit 才生效。',
      current: '当前',
      rollback: '回滚到此版本',
      empty: '尚无已生效的历史版本。',
      noSource: '历史接口没有可用数据源（daemon 未上报当前版本/可用版本）：不编造历史。',
      rollbackNote: '回滚 = 版本号递增的新提交：历史保留，回滚后仍需在「两阶段」卡提交（commit）才生效。',
      rollbackPrepared: '已准备回滚到 v{version} 的草稿：提交后生效',
      table: {
        version: '版本',
        graphId: 'graph_id',
        appliedAt: '应用时间',
        source: '来源',
        actions: '操作'
      },
      confirmRollback: {
        title: '回滚到旧版本？',
        message: '将准备回滚到 v{version} 的草稿（版本号递增，历史不改写）；仍需提交（commit）才生效。',
        ok: '准备回滚'
      }
    },
    draft: {
      desc: 'prepare 只准备草稿（含 engine_token/预览），commit 才生效，abort 丢弃；回执来自 daemon 控制面。',
      empty: '尚无方案草稿：上传 plan 并点「准备应用」，或在历史里回滚到旧版本。',
      planHint: '上传方案草稿：提交后 graph_id / 版本号会变化。',
      rollbackHint: '回滚草稿：来自 v{version}（版本号递增，历史不改写）；提交后生效。',
      confirmCommitTitle: '提交方案？',
      confirmCommitMessage: '将把草稿提交为 v{version}（块边界原子换图，apply 后 graph_id 变化）。',
      confirmRollbackTitle: '提交回滚？',
      confirmRollbackMessage: '将把回滚到 v{version} 的草稿提交生效（创建新版本，历史保留）。'
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
    pageTitle: '路由矩阵',
    lanes: {
      unknownLabel: 'OUT {channel}'
    },
    actions: {
      refresh: '刷新'
    },
    updated: {
      last: '最后更新 {time}',
      never: '尚未刷新'
    },
    band: {
      hostLink: '控制通道',
      hostLinkOk: '已连接',
      hostLinkStale: '状态过期',
      hostLinkUnknown: '未接入',
      runState: '运行状态',
      planVersion: '方案版本',
      matrixLanes: '矩阵 lane',
      outputChannels: 'DAC 输出',
      routedLanes: '已路由 lane'
    },
    notice: {
      statusRefreshFailed: '/api/status 刷新失败：{message}（显示的是最后一次成功数据）',
      channelsRefreshFailed: '/api/channels 刷新失败：{message}（显示的是最后一次成功数据）',
      historyUnavailable: '方案历史不可用：{message}（当前版本/latency 回退来源缺失）',
      hostLinkDown:
        '控制通道不可达或状态过期：rt-host 数据面事实无法回读，本页只显示最后成功快照；路由写入口也不会因此变得可用。',
      noPlan: '尚未读到已应用的方案：矩阵域/输出路由没有事实来源（本页不猜直通映射）。'
    },
    statusUnavailable: '/api/status 未接入：路由页拿不到输入/格式/控制通道事实；请检查会话与 daemon。',
    chain: {
      title: '当前路由链（输入 → 矩阵域 → DAC 输出）',
      desc: '只读诊断：AES67 输入（/api/status.format.aes67）→ 22 lane 矩阵域（format.channels）→ DAC 物理输出（/api/channels.output + mapping）。没有数据源的字段显示「未接入」+ 原因，不假设 lane i → DAC i 的直通映射。',
      input: {
        title: '输入 · AES67',
        channels: '接收声道数',
        map: '通道映射',
        source: '输入来源',
        sourceSdp: 'SDP 描述符（sdp）',
        sourceManual: '手工配置（manual）'
      },
      matrix: {
        title: '矩阵域',
        lanes: 'lane 数',
        expectedSuffix: '（设计 {lanes} lane）',
        routedLanes: '已路由 lane',
        routes: '路由条目'
      },
      output: {
        title: '输出 · DAC',
        channels: 'DAC 声道数',
        dacMap: 'DAC 映射（0-based）',
        knownLabels: '已知物理位置',
        knownLabelsValue: '{count} 个已知标签位置'
      },
      note: '矩阵域 lane 数来自数据面格式（/api/status.format.channels）；输出声道/DAC 映射来自 /api/channels（rt-host 实际配置 + 当前方案 routing 段）。两者都缺失时本页不画「直通」。'
    },
    plan: {
      title: '计划内 routing 段摘要',
      desc: '组合 /api/status.plan 与 /api/plan/history；routing 段明细来自 /api/channels（mapping / dac_channels）。',
      field: {
        version: '方案版本',
        latency: '延迟（samples）',
        segments: '区段数（segments）',
        graphId: 'graph_id',
        appliedAt: '应用时间',
        source: '方案来源',
        historyCurrent: '历史当前版本'
      },
      versionReason: '运行中方案版本不可读（/api/status 与 /api/plan/history 都没有值）',
      latencyReason: '方案延迟不可读（status 与历史都没有值）',
      historyReason: '方案历史尚未读取或不可用',
      segmentsReason:
        '当前生效方案的区段清单/区段数没有任何只读接口上报：daemon 只在 POST /api/plan/upload 的回执里报告 segments/segment_count（T55），/api/status 与 /api/plan/history 都不含它。本页不把路由条目数冒充区段数；上传/查看入口在 /channels。',
    },
    notWired: {
      title: '后端上报的未接入能力',
      desc: '来自 /api/channels.not_wired 与 /api/status.not_wired 的原始条目（daemon 自己声明的能力缺口）。',
      empty: '后端没有上报未接入条目。',
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