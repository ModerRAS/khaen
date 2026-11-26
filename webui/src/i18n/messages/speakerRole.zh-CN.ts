/**
 * i18n 分片：speakerRole（zh-CN）—— canonical 角色显示名（task-73）。
 *
 * 规则（scripts/i18n-check.mjs 强制）：只放本区域新增键；不得重复基础文件已有键；
 * zh-CN / en-US 键集对等；顶层命名空间 speakerRole.* 由本分片独占。
 */

const messages = {
  speakerRole: {
  L: "左",
  R: "右",
  C: "中置",
  Lw: "左前宽",
  Rw: "右前宽",
  Ls: "左环绕",
  Rs: "右环绕",
  Lb: "左后环绕",
  Rb: "右后环绕",
  LFE: "低音炮（LFE）",
  Sub: "辅助超低音",
  Tfl: "左前天空",
  Tfr: "右前天空",
  Tml: "左中天空",
  Tmr: "右中天空",
  Trl: "左后天空",
  Trr: "右后天空",
  Custom: "自定义",
  None: "未使用",
  group: {
    ground: "地面",
    low: "低频",
    height: "天空",
    other: "其它",
  },
  groupCount: "{count} 只",
  }
}

export default messages
