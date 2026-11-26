/**
 * i18n 分片：routingEdit（zh-CN）—— 路由页「只读诊断 + 可编辑两阶段」新增文案。
 *
 * 规则（scripts/i18n-check.mjs 强制）：
 * - 本分片只放本区域新增的键；不得重复定义 src/i18n/zh-CN.ts 基础文件已有的键；
 * - zh-CN / en-US 两个分片必须键集完全对等（成对提交）；
 * - 顶层命名空间由本区域独占：routingEdit.*（其它分片/基础文件不得再定义该顶层键）。
 *
 * 基础文件里旧的 routing.* 键保留（不删不改）；本页不再引用其中描述「只读 / 编辑未接入（T52/T59）」的过期文案。
 */

const messages = {
  routingEdit: {
    subtitle:
      '输入 → 22 lane 矩阵域 → DAC 物理输出的只读诊断 + lane → DAC 可编辑两阶段：当前值来自 /api/channels 的 mapping（daemon 在 prepare/upload 时记录的方案 routing 镜像，不是数据面实时回读）；编辑经 POST /api/surface {surface:"routing"} → POST /api/commit / POST /api/abort。mapping=null 时显示「当前值未知」+ 原因，不用旧值/默认值冒充；changed/diff 等可选回执缺失时如实显示「服务端未上报」。',
    meta: {
      label: '能力（/api/meta surface=routing）',
      wired: '已接入：可以 prepare → commit',
      notWired: '未接入：后端上报 routing surface 未接通，本页禁用 prepare（不假成功）',
      unknown: '能力未知：/api/meta 未上报 routing surface；仍可尝试 prepare，结果以服务端回执为准',
      reason: '后端上报原因：{reason}'
    },
    current: {
      title: '当前值来源（只读诊断）',
      desc: '/api/channels.mapping 是 daemon 在 prepare/upload 时记录的方案 routing 镜像（lane → 0-based DAC），不是数据面实时回读；mapping=null 时当前值未知，本页用空表 + 原因如实标注，不用旧值/默认值冒充。',
      source: '数据来源',
      sourceValue: '/api/channels.mapping（方案 routing 镜像）',
      sourceUnknown: '无来源（mapping 未上报）',
      semantics: '语义',
      semanticsValue: 'daemon 在 prepare/upload 时记录的方案 routing 镜像，不是数据面实时回读',
      planVersion: '来源方案版本',
      graphId: '来源 graph_id',
      dacChannels: 'DAC 声道数（output.channels）',
      routes: 'mapping 路由条目',
      routingKnown: 'daemon 关联到方案 routing 段',
      readAt: '读取时间',
      unknownTitle: '当前值未知（不能用旧值/默认值冒充）',
      unknownReason: '后端未给出原因（/api/channels 未上报 reasons.mapping）',
      knownZero: 'daemon 上报了已知的 0 条路由（mapping: []）：这不是「未知」。',
      knownHint: '以上行已用于预填编辑表；提交时按「全量替换表」发送 routes。',
      staleHint: '刷新失败：显示的是最后一次成功快照（{message}）。',
      unavailableTitle: '/api/channels 不可用：当前值无法读取',
      unavailableDesc:
        '/api/channels 读取失败（{message}）：拿不到 lane → DAC 映射来源，本页不展示假映射、不用旧值冒充当前值。'
    },
    edit: {
      title: 'lane → DAC 可编辑表（两阶段）',
      desc: '编辑 lane → DAC 全量替换表：本地校验 → POST /api/surface {surface:"routing", data:{routes, dac_channels?}} 生成草稿 → POST /api/commit 生效 / POST /api/abort 丢弃。prepare 只准备草稿，不改变运行态。',
      toolbar: {
        prefill: '从 mapping 预填',
        add: '新增 lane',
        clear: '清空草稿',
        count: '{count} 行'
      },
      table: {
        lane: 'lane',
        dac: 'DAC（0-based）',
        output: 'MOTU 输出',
        role: '角色（canonical，来自 /api/channels.speaker_roles）',
        actions: '操作',
        remove: '删除该行'
      },
      noRows: '草稿为空：点「新增 lane」添加行；新增行默认 DAC 0，请逐行确认。',
      unknownCurrent: '当前值未知：无法用 /api/channels.mapping 预填，需手动添加 lane → DAC 行。原因：{reason}',
      prefilled: '已用 /api/channels.mapping 预填（{time}）。',
      editedHint: '草稿已被手动编辑（刷新不会覆盖）；需要重新预填请点「从 mapping 预填」。',
      laneHint: '矩阵域设计 {lanes} lane；lane 越界由后端准入拒绝（前端不猜 lane 总数）。',
      dacChannels: 'DAC 声道数（dac_channels）',
      explicitDac: '显式指定 dac_channels',
      inheritDac: '未显式指定：沿用当前方案值（请求体不发送 dac_channels）',
      inheritDacValue: '当前方案值（output.channels）：{value}',
      dacChannelsUnknown: '当前 DAC 声道数未知：勾选「显式指定」后填写正整数，或保持沿用方案值。',
      dacChannelsHint: 'dac_channels 只作用于本次补丁；不勾选时不发送该字段（API.md §4 routing data）。',
      prepare: '准备草稿（prepare）',
      preparing: '正在准备…',
      prepareBlockedMeta: '后端上报 routing surface 未接入（not_wired）：不发送 prepare。',
      prepareBlockedMetaReason: '原因：{reason}',
      prepareBlockedDraft: '已有 routing 草稿：请先 commit 或 abort，再重新 prepare。',
      foreignDraft: '控制面已有 {surface} 草稿（来自其它页面）：本页 prepare 会替换它（先 commit/abort 更安全）。',
      validation: {
        empty: '至少需要一行 lane → DAC',
        entry: 'lane / DAC 必须是非负整数',
        duplicate: '同一 lane 不能出现两次',
        dac_channels: 'dac_channels 必须是正整数',
        dac_range: 'DAC 序号必须小于 DAC 声道数（{channels}）'
      }
    },
    draft: {
      title: '草稿审查（prepare 回执）',
      desc: 'changed / diff / current / target / graph / admission 是可选回执字段：服务端未上报时如实显示「服务端未上报」，不臆造；commit 只发送 engine_token。',
      empty: '当前没有 routing 草稿：编辑表校验通过后点「准备草稿」。',
      abortHint: 'abort 只丢弃服务端草稿（不改变运行态）；失败会保留草稿并显示稳定错误码。'
    },
    receipt: {
      title: '提交回执（commit）',
      desc: '以服务端真实回执为准（version / applied_block / graph_id / changed）；不本地伪造。',
      empty: '本次页面还没有成功 commit 回执。',
      version: '版本（version）',
      appliedBlock: '生效块（applied_block）',
      graphId: 'graph_id',
      changed: 'changed',
      changedUnreported: '服务端未上报',
      at: '提交时间'
    },
    error: {
      noBasePlanGuide:
        '没有已生效方案（no_base_plan）：routing 补丁需要以已生效方案为基线。请先在「方案与声道」页 prepare + commit 一份方案，再回来编辑路由。',
      goChannels: '去「方案与声道」页',
      tokenInvalidGuide:
        'engine_token 无效或草稿已被替换（engine_token_invalid）：本地草稿已失效，请重新 prepare。'
    },
    plan: {
      segmentsReason:
        '当前生效方案的区段清单/区段数没有任何只读接口上报：daemon 只在方案上传回执里报告 segments/segment_count；本页不用路由条目数冒充区段数。',
      segmentsNote:
        '区段数（content/crossover/matrix/protection/routing 的条数）目前只能从方案上传回执看到；本页如实显示「未接入」。'
    },
    toast: {
      prefilled: '草稿已从 /api/channels.mapping 预填（当前值未知时不可用）',
      prepared: 'routing 草稿已准备（尚未生效）',
      committed: 'routing 补丁已提交生效',
      aborted: 'routing 草稿已丢弃'
    }
  }
}

export default messages
