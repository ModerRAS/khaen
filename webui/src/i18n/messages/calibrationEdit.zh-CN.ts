/**
 * i18n 分片：calibrationEdit（zh-CN）—— 并行开发专用，避免多人同时改巨型 i18n 文件。
 *
 * 规则（scripts/i18n-check.mjs 强制）：
 * - 本分片只放本区域新增的键；不得重复定义 src/i18n/zh-CN.ts 基础文件已有的键；
 * - zh-CN / en-US 两个分片必须键集完全对等（成对提交）；
 * - 顶层命名空间由本区域独占：calibrationEdit.*（其它分片/基础文件不得再定义该顶层键）。
 *
 * 本分片只放 task-69 新增的「校准 REST 按 docs/API.md §7 接线」文案：
 * NOT_AVAILABLE 原因渲染、apply/rollback 两阶段（prepare→commit/abort）回执、带认证产物下载。
 */

const messages = {
  calibrationEdit: {
    list: {
      notAvailableTitle: '校准后端未接入（NOT_AVAILABLE）',
      notAvailableReason: 'daemon 上报当前不可用，原因：{reason}',
      notAvailableNoReason: 'daemon 未上报原因（reasons 为空）：不展示假 job、不伪造进度。',
      engineUnknown: '未上报',
      engineUnknownReason: 'daemon 未上报 engine 字段：引擎真实性未知，不标成实测。',
      reasonLabel: '原因'
    },
    staged: {
      title: '两阶段草稿（prepare）',
      desc: 'prepare 只生成草稿并返回一次性 engine_token，运行态尚未改变；必须再 commit 才生效，或 abort 丢弃草稿。',
      prepareAction: '两阶段准备（prepare）',
      prepareHint: '只生成草稿，不改变运行态；拿到回执后再决定 commit / abort。',
      draftOnly: '草稿未生效',
      token: 'engine_token',
      planSha: '产物 plan sha256',
      previousVersion: 'apply 前版本',
      targetVersion: '回滚目标版本',
      commit: '提交（commit）',
      committing: '正在提交…',
      abort: '丢弃草稿（abort）',
      aborting: '正在丢弃…',
      commitDone: '两阶段提交已生效（以服务端回执为准）。',
      abortDone: '草稿已丢弃，运行态未改变。',
      pendingTitle: '存在未提交的两阶段草稿',
      pendingDesc: 'token 只能消费一次；commit 生效或 abort 丢弃前，运行方案保持不变。',
      noPending: '没有待决的 prepare 草稿：先点「两阶段准备」。',
      notAppliedHint: '这是 prepare 草稿：未生效，不得显示成已应用/已回滚。',
      tokenMissingJob: 'prepare 回执没有 job_id，无法按 job 提交/丢弃（请改用单发或重新 prepare）。'
    },
    wizardApply: {
      jobTitle: '向导应用回执',
      jobDesc: '服务端返回了 job_id / location（配置草稿或已创建的 job）；是否生效以 job 详情与控制面回执为准。',
      jobId: 'job id',
      location: 'location'
    },
    receipt: {
      applied: '应用回执（已生效）',
      rolledBack: '回滚回执（已生效）',
      prepared: 'prepare 草稿回执（未生效）',
      aborted: 'abort 回执（草稿已丢弃）',
      appliedVersion: '应用版本',
      previousVersion: '应用前版本',
      restoredVersion: '恢复版本',
      graphId: 'graph_id',
      sha256: 'sha256',
      snapshot: '应用前快照',
      ack: '回执类型',
      ackActual: 'actual（真实生效）',
      ackSynthetic: 'synthetic（模拟提交）',
      appliedAt: '应用时间',
      rolledBackAt: '回滚时间',
      jobId: 'job id',
      planSha: '产物 plan sha256',
      targetVersion: '回滚目标版本',
      control: '控制面回执',
      abortedDesc: '运行态未改变（以服务端回执为准）。',
      prepareDesc: '只生成草稿；运行态未改变。'
    },
    artifact: {
      download: '带认证下载',
      downloading: '正在下载…',
      failed: '产物下载失败',
      done: '产物已下载：{filename}',
      filenameFallback: '服务端未给 Content-Disposition 文件名，使用产物名：{name}',
      shaHeader: 'daemon 上报的产物 sha256（未在本地重算）'
    },
    error: {
      noPendingDraft: '本地没有待提交的 prepare 草稿（未发送 commit/abort 请求）。'
    }
  }
}

export default messages
