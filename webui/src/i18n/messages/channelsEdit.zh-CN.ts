/**
 * i18n 分片：channelsEdit（zh-CN）—— 方案与声道页的新两阶段文案，避免多人同时改巨型 i18n 文件。
 *
 * 规则（scripts/i18n-check.mjs 强制）：
 * - 本分片只放本区域新增的键；不得重复定义 src/i18n/zh-CN.ts 基础文件已有的键；
 * - zh-CN / en-US 两个分片必须键集完全对等（成对提交）；
 * - 顶层命名空间由本区域独占：channelsEdit.*（其它分片/基础文件不得再定义该顶层键）。
 *
 * 说明：基础文件 channels.* 里的过期里程碑文案（M3b/T52/T55）不在本页引用；本文件提供
 * 「PUT /api/channels 两阶段热重配」的真实文案，错误类别也重映射到 channelsEdit.error.*。
 */
const messages = {
  channelsEdit: {
    pageTitle: '方案与声道',
    pageSubtitle:
      '当前方案（/api/status + /api/plan/history）、输出通道与 MOTU 物理映射、AES67 输入（/api/channels）；声道热重配走 PUT /api/channels 两阶段（prepare → commit/abort）。mapping/dac_map 属方案 routing（由方案 prepare+commit 生效），数据面回执如实展示，不假成功。',
    unavailable: {
      title: '/api/channels 不可用',
      desc: 'daemon 读取声道状态失败：{message}。页面不展示假映射/假通道数。',
      reason: 'daemon 未上报（/api/channels 不可用）'
    },
    plan: {
      historyUnavailableHint: '方案历史不可用：当前版本/回滚点无法读取；页面不编造历史。'
    },
    input: {
      note:
        'AES67 接收参数来自 rt-host 数据面（channels_status.current）；来源（sdp/manual）与 SDP sha256 控制通道不上报时如实显示「未上报」，不猜。'
    },
    history: {
      unavailable: '方案历史不可用：{message}。页面不编造历史。'
    },
    hotswap: {
      title: '声道热重配（PUT /api/channels 两阶段）',
      desc:
        'prepare 生成 rt-host 草稿（engine_token/diff/current/target），commit 在块边界生效；失败由 rt-host 回滚并返回 rolled_back。mapping/dac_map 是方案 routing，channels_commit 不能改，需方案 prepare+commit 才生效。',
      warningTitle: '会中断出声',
      warningDesc:
        'commit 会静音并在块边界关闭/重开设备（实测空档以回执 gap_ms 为准）；失败时 rt-host 自动回滚旧配置。',
      capabilityLabel: '热重配能力（hot_reconfigure.wired）',
      capabilityWired: '已接线（wired=true）',
      capabilityNotWired: '未接线（wired=false）',
      capabilityUnknown: '无法核实（wired=null）',
      targetLabel: 'host_link 目标',
      generationLabel: '当前代际（generation）',
      busyLabel: '热重配进行中（busy）',
      pendingLabel: '未提交草稿（pending）',
      pendingYes: '有',
      pendingNo: '无',
      lastGapLabel: '最近一次实测空档（last_gap_ms）',
      lastGapValue: '{ms} ms',
      lastResultLabel: '最近一次结果（last_result）',
      lastResultNone: '服务端未上报',
      deviceLabel: '输出设备',
      probeLabel: '设备探测（probe）',
      deviceValue: '{name}（{state}）',
      probeUnavailable: '设备探测不可用',
      probeValue: '{rate} Hz / {channels} ch',
      sourceLabel: '当前值来源',
      sourceValue:
        '数据面 channels_status.current（rt-host 权威）；mapping 是 daemon 记录的方案 routing 镜像，不是实时回读。',
      mappingUnknown:
        '当前方案 routing 未知（mapping=null）：不预填 DAC 路由、不假设直通；提交格式变化后需准备匹配新格式的方案。',
      formTitle: '目标配置（本地草稿）',
      field: {
        channels: '输出通道数',
        sampleRate: '采样率（Hz）',
        aes67Group: 'AES67 组播地址',
        aes67Port: 'AES67 端口',
        payloadType: 'RTP payload type',
        aes67Channels: 'AES67 接收通道数',
        aes67Map: 'AES67 映射（逗号分隔）',
        jitterFrames: 'jitter 目标帧数',
        dacMap: '方案 routing（DAC → lane，逗号分隔；- = 无 lane）'
      },
      routingTitle: '方案 routing（mapping/dac_map）',
      routingDesc:
        '这是已生效方案的 routing 段（lane → DAC），不是数据面通道字段。只改它时 PUT /api/channels 返回 commit_supported=false + blockers，本页禁用 commit；请走方案上传 → prepare → commit。',
      routingPlaceholder: '例如 0,1,2,-（第 i 项 = 喂给 DAC i 的 lane，- 表示该 DAC 静音）',
      resetDrafts: '恢复为当前值',
      prepare: '准备（prepare）',
      prepared: '声道草稿已准备（未提交）',
      commit: '提交（热重配）',
      abort: '丢弃草稿（abort）',
      discard: '本地取消（无服务端草稿）',
      discarded: '已本地丢弃草稿（未调用服务端）',
      aborted: '服务端草稿已丢弃（运行态未改变）',
      committed: '热重配提交成功（回执见下）',
      commitConfirmTitle: '提交声道热重配？',
      commitConfirmMessage: '将静音并在块边界关闭/重开设备；失败时 rt-host 自动回滚。确认提交？',
      commitConfirmOk: '确认提交',
      draftTitle: 'prepare 草稿（未提交）',
      receiptTitle: 'commit 回执',
      changedLabel: 'changed',
      changedYes: '有变化',
      changedNo: '无变化（no-op）',
      notReported: '服务端未上报',
      formatLabel: '目标/生效格式',
      formatValue: '{rate} Hz / {channels} ch / {frames} frames',
      graphActionLabel: '图（graph）动作',
      graphDropped: 'dropped：commit 会丢弃运行中的旧图，需重新准备并提交匹配新格式的方案',
      graphKept: 'kept：引擎格式不变，仅重建 AES67 输入',
      graphValue: '{action}：{reason}',
      admissionLabel: '准入（admission）',
      admissionAccepted: '已通过',
      admissionRejected: '未通过',
      admissionUnknown: '服务端未上报',
      warningsTitle: 'warnings',
      blockersTitle: 'blockers（commit 不可用）',
      guidance:
        'commit_supported=false：本次改动不会被热重配应用。请上传匹配新格式的方案并 prepare → commit（方案两阶段卡），再回本页 prepare/commit 数据面配置。',
      commitSupportedLabel: 'commit_supported',
      commitSupportedYes: '是',
      commitSupportedNo: '否',
      engineTokenLabel: 'engine_token',
      plannedGenerationLabel: '计划生效代际（planned_generation）',
      diffTitle: 'diff（prepare 上报）',
      currentTitle: 'current（服务端上报）',
      targetTitle: 'target（服务端上报）',
      detailsTitle: '原始回执（JSON）',
      appliedLabel: 'applied',
      appliedYes: '已生效',
      appliedNo: '未生效',
      idempotentLabel: '幂等回放（idempotent）',
      commitGenerationLabel: '生效代际（generation）',
      gapLabel: '实测静音→恢复空档（gap_ms）',
      gapValue: '{ms} ms',
      graphDroppedLabel: 'graph_dropped',
      rolledBackLabel: 'rolled_back',
      routingAppliedLabel: 'routing 由本次 commit 应用',
      routingAppliedYes: '是',
      routingAppliedNo: '否（channels_commit 不改方案 routing）',
      planRecommitLabel: '需重新提交方案（plan_recommit_required）',
      noteLabel: 'rt-host note',
      commitAt: '回执时间：{time}',
      commitFormatHint:
        '回执里的 format/applied/generation 是数据面真实回执；「设备已换格式」只在 rt-host 回执如此上报时才成立，本页不额外推断。',
      failureTitle: '热重配失败（502 reconfig_failed / ERR_RECONFIGURE_*）',
      failureCode: '错误码',
      failureStage: '失败阶段',
      failureRolledBack: 'rolled_back（是否已回滚旧配置）',
      failureDeviceStarted: 'device_started',
      failureReceiverRunning: 'receiver_running',
      failureApplied: 'applied（失败时必须是 false）',
      failureGap: '失败前空档（gap_ms）',
      failureGeneration: 'generation（失败不前进）',
      failureYes: '是',
      failureNo: '否',
      failureUnknown: '服务端未上报',
      validation: {
        channels_range: '输出通道数必须是 1–22 的整数',
        sample_rate_value: '采样率只支持 48000 / 96000 Hz',
        aes67_group_value: 'AES67 组播地址必须是 224.0.0.0/4 范围内的 IPv4 地址',
        aes67_port_value: 'AES67 端口必须是 1–65535 的整数',
        payload_type_value: 'RTP payload type 必须是 96–127 的整数',
        aes67_channels_range: 'AES67 接收通道数必须是 1–8 的整数',
        aes67_map_length: 'AES67 映射长度必须等于接收通道数',
        aes67_map_value: 'AES67 映射必须是互不重复且小于输出通道数的序号',
        jitter_frames_value: 'jitter 目标帧数必须是非负整数',
        dac_map_length: 'DAC 映射长度必须等于输出通道数（或省略 channels 由它推导）',
        dac_map_value: 'DAC 映射必须是互不重复的非负 lane 序号（- 表示静音）',
        mapping_value: 'lane/DAC 必须是非负整数且各自不重复',
        mapping_dac_map_conflict: 'mapping 与 dac_map 描述了不同的路由，只能给一种形式',
        empty: '请至少修改一项（当前值与草稿一致）'
      }
    },
    error: {
      network: '无法连接 daemon（后端未启动或网络中断），请检查后重试。',
      timeout: '声道/方案请求超时，请重试。',
      unauthorized: '会话已过期，请重新登录。',
      hostLinkUnavailable:
        'daemon 与 rt-host 的控制通道不可用（503 host_link_unavailable）：写操作未发送，页面不会显示假成功。',
      hostLinkUnsupported:
        'rt-host/daemon 明确不支持该操作（501 host_link_unsupported）：不签发 token、不假成功；process 目标下热重配不可用。',
      notWired: '该能力尚未接线（503 not_wired）：页面显示「未接入」，不编造数据。',
      notFound: '目标资源不存在（404）。',
      invalidRequest: '请求参数不合法（400）：请检查表单后重试。',
      invalidPlan: '方案文件不合法（ABI v3 解码/结构校验失败）：原方案保持不变。',
      uploadTooLarge: '方案文件超过 4 MiB 上传上限。',
      uploadNotFound: '上传的 plan 不存在（可能已过期或上传失败）：请重新上传。',
      shaMismatch: '客户端与服务端计算的 sha256 不一致：已拒绝继续（不信任任何一侧）。',
      admissionRejected: '方案/配置未通过准入（admission_rejected）：原配置保持不变。',
      noHistory: '没有可回滚的历史版本（no_history）。',
      planPathUnavailable:
        '上传回执没有 plan_path（stored_path）：无法调用 inspect/prepare；页面不猜路径、不伪造草稿。',
      conflict: '当前状态不允许该操作（409 busy/token/admission 冲突）：草稿可能仍可重试或丢弃，请刷新后重试。',
      reconfigFailed:
        '数据面热重配失败（502 reconfig_failed）：rt-host 已按回执回滚，页面原样展示 rolled_back 等明细，不显示假成功。',
      commitUnsupported:
        'commit_supported=false：mapping/dac_map 属方案 routing，本次热重配不应用它；请走方案 prepare+commit。',
      abortNotApplied: '服务端没有丢弃草稿（aborted=false）：运行态未改变，请刷新后重试。',
      server: 'daemon 返回服务器错误，请稍后重试。',
      invalidResponse: 'daemon 返回的结构不符合 docs/API.md 约定，已拒绝显示（不把畸形响应当成功）。',
      unknown: '声道/方案操作失败，请重试。'
    }
  }
}

export default messages
