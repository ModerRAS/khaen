/**
 * i18n 分片：layout（zh-CN）—— Layout 页专用文案（命名空间 layout.* 由本分片独占）。
 *
 * 规则（scripts/i18n-check.mjs 强制）：
 * - 只放本区域新增的键；不得重复定义基础文件/其它分片已有的键；
 * - zh-CN / en-US 两个分片必须键集完全对等（成对提交）；
 * - 源码只通过 t() 引用文案，不在 .vue 里硬编码。
 */

const messages = {
  layout: {
    pageTitle: '内容布局',
    subtitle:
      '内容折叠表（folds）两阶段编辑：prepare 生成补丁草稿，commit 才在块边界生效；当前布局没有读回数据源，表单是本地草稿。',
    capability: {
      title: '界面能力',
      desc: '来自 GET /api/meta 的 surfaces / not_wired：未接通的能力显示原因，不渲染可点击的假操作（API.md §9.1）。',
      wired: '已接通',
      notWired: '未接通（not_wired）',
      unknown: '能力未知（meta 未上报或不可用）',
      wiredHint: '写入口可用；能力变化后可用「重新读取能力」刷新。',
      notWiredHint: '该界面尚未接通：prepare 入口已禁用，不渲染假操作。',
      unknownHint:
        '能力未知：prepare 仍可尝试，但后端可能返回 501 unsupported / 503 not_wired；错误会如实显示。',
      field: {
        surface: '界面名（surface）',
        state: '能力状态',
        reason: '未接通原因'
      },
      reasonMissing: 'meta 未给出原因',
      metaError: 'meta 读取失败（{code}）',
      loading: '正在读取能力…',
      refresh: '重新读取能力'
    },
    current: {
      title: '当前内容布局',
      desc: '当前内容布局来自 /api/status.readback（控制面 base_plan）；readback 不可用时显示原因，不拿默认值冒充。',
      unavailable: '不可读（readback 不可用）',
      reason:
        '/api/status.readback 没有可用的 base_plan（rt-host 未加载/未 commit 方案，或链路降级）；页面不猜当前值，也不拿默认值冒充。',
      note: '上方数字来自 readback；下方表单是本地草稿，默认值只是编辑起点，不是当前布局。',
      field: {
        inChannels: '输入通道数',
        outLanes: '输出 lane 数',
        folds: '折叠项数',
        source: '数据来源'
      },
      sourceValue: 'base_plan 回读（{source}）'
    },
    form: {
      title: '折叠表（本地草稿）',
      desc:
        'in_channels / out_lanes / folds:[[src,dst,gain],...] 是完整替换表：先本地校验形状，再由 prepare 交后端准入。',
      inChannels: '输入通道数（in_channels）',
      outLanes: '输出 lane 数（out_lanes）',
      foldsTitle: '折叠项（folds）',
      foldRow: '折叠项 {index}',
      src: '源通道（src）',
      dst: '目标 lane（dst）',
      gain: '增益（gain）',
      addFold: '添加折叠项',
      removeFold: '删除',
      count: '共 {count} 条折叠项',
      hint: 'gain 为线性增益（同一 dst 的多项相加）；src/dst 越界与空表由后端准入拒绝，前端只校验形状。',
      localDraft: '本地草稿：不是当前布局（当前值无数据源）。',
      prepared: '布局补丁草稿已准备：请在草稿卡确认后提交。'
    },
    validation: {
      channels: 'in_channels / out_lanes 必须是正整数。',
      empty: 'folds 不能为空：至少保留一条折叠项（后端同样拒绝空表）。',
      entry: '折叠项必须是 [src, dst, gain]：src/dst 为非负整数，gain 为有限数。'
    },
    block: {
      capability: '该界面未接通：写入口已禁用。',
      locked: '安全态/故障态：写入已禁用（只读）。',
      busy: '控制面操作进行中，请稍候。',
      invalid: '本地校验未通过：修正后才能 prepare。'
    },
    notice: {
      safeReadOnly: '安全态（safe）：写入已禁用。',
      safeReadOnlyReason: '安全态（safe）：写入已禁用（原因：{reason}）。',
      faultedReadOnly: '故障态（faulted）：写入已禁用。',
      faultedReadOnlyReason: '故障态（faulted）：写入已禁用（原因：{reason}）。',
      abortStillAvailable: '已准备的草稿仍可丢弃（abort 只丢弃草稿，不改变运行态）。',
      statusUnknown: '运行状态不可读：安全/故障写锁状态未知（只在读到 safe/faulted 时禁用写入）。',
      foreignDraft: '控制面已有其它界面的草稿（surface={surface}）：本次 prepare 会替换它（后端单 pending 槽）。',
      noopCommit: '本次提交为空操作（changed=false，applied_block 未上报）。',
      lockedDraft: '安全态/故障态：已准备的草稿禁止提交（只读）；仍可丢弃（abort）。'
    },
    draftFields: {
      title: 'prepare 回执细节（可选字段）',
      desc: 'changed / diff / current / target 是可选回执字段：后端未上报时如实显示「服务端未上报」，不臆造。',
      changed: '是否有变化（changed）',
      diff: '差异（diff）',
      current: '当前值（current）',
      target: '目标值（target）'
    },
    receipt: {
      title: '布局提交回执',
      desc: 'commit 回执来自控制面（块边界生效），不是设备回读；缺失字段显示「服务端未上报」。',
      empty: '本次会话尚无布局提交回执。'
    },
    noBasePlan: {
      title: '还没有已生效方案（no_base_plan）',
      guide:
        '布局补丁必须基于最近一次已生效方案：请先到「方案与声道」页 prepare + commit 一份方案，再回来准备布局补丁。',
      goto: '去方案与声道页',
      detected: '/api/status 未检测到已生效方案版本（plan.version 为空）：布局 prepare 预计会返回 no_base_plan。'
    }
  }
}

export default messages
