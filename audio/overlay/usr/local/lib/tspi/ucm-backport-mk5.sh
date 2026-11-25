#!/usr/bin/env bash
# ucm-backport-mk5.sh — 把 MOTU UltraLite-mk5 的 UCM 回移进 Debian 12 的 alsa-ucm-conf 1.2.8
#
# 背景：
#   - bookworm alsa-ucm-conf = 1.2.8-1，没有 UltraLite-mk5；
#   - 上游 mk5 UCM 自 2024-05-27（commit 2bd2aca），首个 release v1.2.13；
#   - 上游 v1.2.13 整棵树是 UCM Syntax 7，而 bookworm alsa-lib 1.2.8 只支持 <=Syntax 6，
#     所以不能整树替换（会让所有 UCM 失效）。本脚本做“最小回移”：
#       1) 安装 mk5 的两个 UCM 文件（精简版，Syntax 6 兼容，随 overlay 提供）；
#       2) 在 /usr/share/alsa/ucm2/USB-Audio/USB-Audio.conf 里插入 Syntax 6 的 mk5 分派分支。
#
# 用法：ucm-backport-mk5.sh [--rootfs /] [--dry-run] [--revert]
# 退出码：0 成功或已打过；3 环境不满足；4 锚点找不到（文件版本不符）。
set -euo pipefail

ROOTFS="/"
DRY=0
REVERT=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --rootfs) ROOTFS="$2"; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    --revert) REVERT=1; shift ;;
    -h|--help) sed -n "2,20p" "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

DISPATCH="$ROOTFS/usr/share/alsa/ucm2/USB-Audio/USB-Audio.conf"
MARK_BEGIN="# >>> tspi-audio MOTU UltraLite-mk5 backport (BEGIN)"
MARK_END="# <<< tspi-audio MOTU UltraLite-mk5 backport (END)"

[[ -f "$DISPATCH" ]] || { echo "ERROR: 找不到 $DISPATCH（alsa-ucm-conf 未安装？）" >&2; exit 3; }

if [[ "$REVERT" == 1 ]]; then
  if grep -q "tspi-audio MOTU UltraLite-mk5 backport (BEGIN)" "$DISPATCH"; then
    [[ "$DRY" == 1 ]] && { echo "would revert $DISPATCH"; exit 0; }
    sed -i "/$MARK_BEGIN/,/$MARK_END/d" "$DISPATCH"
    echo "reverted: $DISPATCH"
  else
    echo "nothing to revert"
  fi
  exit 0
fi

if grep -q "tspi-audio MOTU UltraLite-mk5 backport (BEGIN)" "$DISPATCH"; then
  echo "already patched: $DISPATCH"
  exit 0
fi

if ! grep -q "^If.mixremap {" "$DISPATCH"; then
  echo "ERROR: $DISPATCH 里找不到锚点行 If.mixremap { —— 版本不符，拒绝修改" >&2
  exit 4
fi

BLOCK="$(mktemp)"
cat > "$BLOCK" <<EOF
$MARK_BEGIN
# MOTU UltraLite-mk5 (USB 07fd:000c) —— tspi-audio 回移（Syntax 6，兼容 alsa-lib 1.2.8）
If.motu-ultralite-mk5 {
	Condition {
		Type RegexMatch
		String "\${CardComponents}"
		Regex "USB07fd:000c"
	}
	True.Define.ProfileName "MOTU/UltraLite-mk5"
}
$MARK_END
EOF

if [[ "$DRY" == 1 ]]; then
  echo "would patch $DISPATCH with:"; cat "$BLOCK"; rm -f "$BLOCK"; exit 0
fi

# 在 If.mixremap { 之前插入块
awk -v blockfile="$BLOCK" '
  /^If\.mixremap \{/ && !done { while ((getline line < blockfile) > 0) print line; done=1 }
  { print }
' "$DISPATCH" > "$DISPATCH.tspi.tmp"
mv "$DISPATCH.tspi.tmp" "$DISPATCH"
rm -f "$BLOCK"
echo "patched: $DISPATCH"
echo "verify: alsaucm -c <card> list _verbs   # 应能看到 HiFi/Direct"
