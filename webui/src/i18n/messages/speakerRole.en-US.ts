/**
 * i18n 分片：speakerRole（en-US）—— canonical role display names（task-73）。
 *
 * 规则（scripts/i18n-check.mjs 强制）：只放本区域新增键；不得重复基础文件已有键；
 * zh-CN / en-US 键集对等；顶层命名空间 speakerRole.* 由本分片独占。
 */

const messages = {
  speakerRole: {
  L: "Left",
  R: "Right",
  C: "Center",
  Lw: "Front wide left",
  Rw: "Front wide right",
  Ls: "Left surround",
  Rs: "Right surround",
  Lb: "Left back",
  Rb: "Right back",
  LFE: "LFE",
  Sub: "Subwoofer",
  Tfl: "Top front left",
  Tfr: "Top front right",
  Tml: "Top middle left",
  Tmr: "Top middle right",
  Trl: "Top rear left",
  Trr: "Top rear right",
  Custom: "Custom",
  None: "Unused",
  group: {
    ground: "Ground",
    low: "Low frequency",
    height: "Height",
    other: "Other",
  },
  groupCount: "{count} speaker(s)",
  }
}

export default messages
