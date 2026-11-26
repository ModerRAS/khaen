/**
 * i18n 分片：mode（zh-CN）—— Mode 页专用文案（命名空间 mode.* 由本分片独占）。
 *
 * 规则（scripts/i18n-check.mjs 强制）：
 * - 只放本区域新增的键；不得重复定义基础文件/其它分片已有的键；
 * - zh-CN / en-US 两个分片必须键集完全对等（成对提交）；
 * - 源码只通过 t() 引用文案，不在 .vue 里硬编码。
 */

const messages = {
  mode: {
    pageTitle: '工作模式',
    subtitle:
      '输入使能两阶段（off / aes67）：prepare 只生成草稿，commit 才在块边界生效；当前模式没有读回数据源，页面不猜当前值。',
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
      title: '当前模式',
      desc: '当前模式没有数据源：/api/status 不含 mode，控制协议也没有读取 op。',
      unavailable: '不可读（无数据源）',
      reason:
        '/api/status 不包含当前工作模式，控制协议也没有读取模式的 op；页面不猜当前值，也不拿默认值冒充。',
      receiptLabel: '最近一次提交回执（不是设备回读）',
      noReceipt: '本次会话尚无模式提交回执。',
      mutedLabel: '静音（/api/status.state.muted）',
      mutedNote: '静音是独立状态：手动静音或 mode=off 都可能让它为 true，不能反推当前 mode。',
      mutedUnknown: '/api/status 未上报静音状态。',
      mutedOn: '已静音',
      mutedOff: '未静音'
    },
    target: {
      title: '目标模式',
      desc: '两阶段：prepare 生成草稿（不改变运行态），commit 才在块边界生效，abort 丢弃草稿。',
      off: 'off（整链静音）',
      aes67: 'aes67 / on（解除静音；当前唯一输入源）',
      alias: '后端接受 on 作为 aes67 的别名；回执里可能显示 on，页面按 aes67 展示。',
      analogReason:
        'analog / toslink / dante 输入源切换尚未实现（后端返回 bad_request）：页面不提供这些选项，不假装可选。',
      selectHint: '先选择目标模式。',
      prepared: '模式草稿已准备：请在草稿卡确认后提交。',
      noopHint: '服务端回执 changed=false：当前已处于目标模式，commit 将是空操作（不应用新块）。'
    },
    block: {
      capability: '该界面未接通：写入口已禁用。',
      locked: '安全态/故障态：写入已禁用（只读）。',
      busy: '控制面操作进行中，请稍候。',
      noTarget: '请先选择目标模式。'
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
    receipt: {
      title: '模式提交回执',
      desc: 'commit 回执来自控制面（块边界生效），不是设备回读；缺失字段显示「服务端未上报」。',
      empty: '本次会话尚无模式提交回执。'
    }
  }
}

export default messages
