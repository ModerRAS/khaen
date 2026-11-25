#!/bin/bash
# tspi-upmix 热部署脚本（板端运行；不重烧镜像、不主动启动/重启音频服务）
#
# 安装内容：
#   /usr/local/bin/tspi-upmix                       生成器（Python，PyYAML）
#   /etc/tspi-audio/upmix/presets.yml               预设参数
#   /etc/tspi-audio/profiles/upmix-*.yml            channelmap v2 profile（WebUI 可见；含 *-safe）
#   （无 FIR；allpass 去相关已改用 Biquad）
#   /etc/tspi-audio/upmix/camilladsp-examples/*     独立 CamillaDSP 示例（参考；含 autodetect 双路径）
#   /etc/tspi-audio/profiles/*-from-h16-*.yml       H16 容器输入的上混 profile（--from 容器）
#   /etc/tspi-audio/profiles/passthrough-*.yml      多声道直通 profile（手动强制直通用）
#   /etc/tspi-audio/upmix/tspi-upmix-autodetect.service.example  输入内容检测 daemon（示例，不启用）
#   /etc/tspi-audio/profiles/upmix-off-restore.yml  首次安装时快照当前 active（用于关闭上混）
#
# 用法（在板端 /tmp/upmix-dist 下）：
#   bash install-upmix.sh --dry-run
#   bash install-upmix.sh
#   bash install-upmix.sh --rollback          # 回滚最近一次安装
#   bash install-upmix.sh --uninstall         # 卸载（先备份）
#   bash install-upmix.sh --src /path/to/dist
#
# 约定：只写文件 + validate/render/--check；不 systemctl start/restart。
# 退出码：0 ok；2 用法/环境；3 校验失败；4 安装失败；5 回滚失败。
set -uo pipefail

SELF_DIR="$(cd "$(dirname "$0")" && pwd)"
SRC="$SELF_DIR"
MODE="install"
DRY=0
NO_VALIDATE=0
PROFILE_DIR="/etc/tspi-audio/profiles"
UMIX_DIR="/etc/tspi-audio/upmix"
FIR_DIR="/etc/tspi-audio/fir/upmix"
EXAMPLES_DIR="$UMIX_DIR/camilladsp-examples"
BACKUP_ROOT="/var/lib/tspi/upmix/install-backups"
BIN="/usr/local/bin/tspi-upmix"
LAYOUT_PRESETS="/etc/tspi-audio/layout-presets.yml"
TS="$(date +%Y%m%d-%H%M%S)"
BACKUP_DIR="$BACKUP_ROOT/$TS"

die() { echo "ERROR: $*" >&2; exit 2; }
info() { echo "[upmix] $*"; }

usage() {
  local code=0
  if [[ $# -ge 1 ]]; then code="$1"; fi
  sed -n "2,25p" "$0" | sed "s/^# \{0,1\}//"
  exit "$code"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --src) SRC="$2"; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    --uninstall) MODE="uninstall"; shift ;;
    --rollback) MODE="rollback"; shift ;;
    --no-validate) NO_VALIDATE=1; shift ;;
    -h|--help) usage 0 ;;
    *) die "未知参数：$1" ;;
  esac
done

[[ "$(id -u)" == "0" ]] || die "需要 root（scp 到板子后 sudo bash install-upmix.sh）"
[[ -d "$SRC" ]] || die "源目录不存在：$SRC"
# 唯一真相是板端 layout-presets；缺文件时拒绝安装（内置映射仅兜底，4.1.2 旧内置会错位）
[[ -f "$LAYOUT_PRESETS" ]] || die "缺少 $LAYOUT_PRESETS：install 拒绝使用内置映射（4.1.2 板端为 FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7），请先部署 layout-presets" 3

backup_file() { # backup_file <abs path>
  local f="$1"
  [[ -e "$f" ]] || return 0
  mkdir -p "$BACKUP_DIR/files$(dirname "$f")"
  cp -a "$f" "$BACKUP_DIR/files$f"
  echo "$f" >> "$BACKUP_DIR/backed-up.txt"
}

record_new() { echo "$1" >> "$BACKUP_DIR/new-files.txt"; }

install_file() { # install_file <src> <dst> [mode]
  local s="$1" d="$2" m=644
  if [[ $# -ge 3 ]]; then m="$3"; fi
  [[ -f "$s" ]] || die "缺少文件：$s" 3
  if [[ "$DRY" == "1" ]]; then
    info "DRY 安装 $s -> $d (mode $m)"
    return 0
  fi
  mkdir -p "$(dirname "$d")"
  if [[ -e "$d" ]]; then backup_file "$d"; else record_new "$d"; fi
  install -m "$m" "$s" "$d"
  info "已安装 $d"
}

do_rollback() {
  local dir="$1"
  [[ -d "$dir" ]] || die "备份目录不存在：$dir" 5
  info "回滚：$dir"
  local f
  if [[ -f "$dir/backed-up.txt" ]]; then
    while IFS= read -r f; do
      [[ -z "$f" ]] && continue
      if [[ -f "$dir/files$f" ]]; then
        mkdir -p "$(dirname "$f")"
        cp -a "$dir/files$f" "$f"
        info "恢复 $f"
      fi
    done < "$dir/backed-up.txt"
  fi
  if [[ -f "$dir/new-files.txt" ]]; then
    while IFS= read -r f; do
      [[ -z "$f" ]] && continue
      rm -f "$f"
      info "删除新增 $f"
    done < "$dir/new-files.txt"
  fi
  info "回滚完成（未重启任何服务）"
  return 0
}

if [[ "$MODE" == "rollback" ]]; then
  last="$(ls -1d "$BACKUP_ROOT"/* 2>/dev/null | sort | tail -1)"
  [[ -n "$last" ]] || die "没有安装备份可回滚" 5
  do_rollback "$last"
  exit $?
fi

if [[ "$MODE" == "uninstall" ]]; then
  BACKUP_DIR="$BACKUP_ROOT/$TS-uninstall"
  mkdir -p "$BACKUP_DIR"
  for f in "$BIN" "$UMIX_DIR/presets.yml" "$PROFILE_DIR"/upmix-*.yml "$PROFILE_DIR"/*-from-h16-*.yml \
           "$PROFILE_DIR"/passthrough-*.yml "$FIR_DIR"/*.wav "$EXAMPLES_DIR"/*.yml \
           "$UMIX_DIR"/tspi-upmix-autodetect.service.example; do
    [[ -e "$f" ]] || continue
    if [[ "$DRY" == "1" ]]; then
      info "DRY 删除 $f"
    else
      backup_file "$f"
      rm -f "$f"
      info "已删除 $f"
    fi
  done
  info "卸载完成；备份在 $BACKUP_DIR"
  exit 0
fi

# ---------------- 安装 ----------------
info "源目录：$SRC"
info "目标：$PROFILE_DIR/upmix-*.yml + $BIN"
mkdir -p "$BACKUP_DIR"

[[ -f "$SRC/tspi-upmix" ]] || die "缺少 $SRC/tspi-upmix（先运行 tspi-upmix all -o dist）" 3
[[ -d "$SRC/profiles" ]] || die "缺少 $SRC/profiles/" 3

install_file "$SRC/tspi-upmix" "$BIN" 755
if [[ -f "$SRC/presets/upmix-presets.yml" ]]; then
  install_file "$SRC/presets/upmix-presets.yml" "$UMIX_DIR/presets.yml" 644
fi

shopt -s nullglob
set -- "$SRC"/profiles/upmix-*.yml
if [[ -e "$1" ]]; then :; else die "dist/profiles 下没有 upmix-*.yml" 3; fi
for p in "$@"; do
  install_file "$p" "$PROFILE_DIR/$(basename "$p")" 644
done

for f in "$SRC"/camilladsp/*.yml; do
  [[ -e "$f" ]] || continue
  install_file "$f" "$EXAMPLES_DIR/$(basename "$f")" 644
done

# H16 容器输入（task-19）：profile + autodetect 双路径配置 + daemon 服务示例
CONTAINER_PROFILES=()
if [[ -d "$SRC/container/profiles" ]]; then
  for p in "$SRC"/container/profiles/*.yml; do
    [[ -e "$p" ]] || continue
    install_file "$p" "$PROFILE_DIR/$(basename "$p")" 644
    CONTAINER_PROFILES+=("$PROFILE_DIR/$(basename "$p")")
  done
fi
if [[ -d "$SRC/container/camilladsp" ]]; then
  for f in "$SRC"/container/camilladsp/*.yml; do
    [[ -e "$f" ]] || continue
    install_file "$f" "$EXAMPLES_DIR/$(basename "$f")" 644
  done
fi
if [[ -f "$SRC/autodetect/tspi-upmix-autodetect.service" ]]; then
  install_file "$SRC/autodetect/tspi-upmix-autodetect.service" \
    "$UMIX_DIR/tspi-upmix-autodetect.service.example" 644
fi

if [[ "$DRY" == "0" && -f "/etc/tspi-audio/channelmap.yml" && ! -f "$PROFILE_DIR/upmix-off-restore.yml" ]]; then
  cp -a "/etc/tspi-audio/channelmap.yml" "$PROFILE_DIR/upmix-off-restore.yml"
  record_new "$PROFILE_DIR/upmix-off-restore.yml"
  info "已快照当前 active -> $PROFILE_DIR/upmix-off-restore.yml（WebUI 可加载它关闭上混）"
fi

if [[ "$NO_VALIDATE" == "0" ]]; then
  fail=0
  for p in "$@"; do
    dst="$PROFILE_DIR/$(basename "$p")"
    [[ "$DRY" == "1" ]] && dst="$p"
    info "校验 profile：$(basename "$p")"
    rc=0
    if [[ "$DRY" == "0" ]]; then
      "$BIN" --layout-presets "$LAYOUT_PRESETS" check --profile "$dst" >/tmp/upmix-validate.log 2>&1
      rc=$?
    fi
    if [[ $rc -ne 0 ]]; then
      fail=1
      echo "  FAIL：$dst" >&2
      tail -20 /tmp/upmix-validate.log >&2
    else
      echo "  OK"
    fi
  done
  for p in "${CONTAINER_PROFILES[@]:-}"; do
    [[ -n "$p" ]] || continue
    info "校验容器 profile：$(basename "$p")"
    rc=0
    if [[ "$DRY" == "0" ]]; then
      "$BIN" --layout-presets "$LAYOUT_PRESETS" check --profile "$p" >/tmp/upmix-validate.log 2>&1
      rc=$?
    fi
    if [[ $rc -ne 0 ]]; then
      fail=1
      echo "  FAIL：$p" >&2
      tail -20 /tmp/upmix-validate.log >&2
    else
      echo "  OK"
    fi
  done
  if command -v camilladsp >/dev/null 2>&1; then
    for f in "$SRC"/camilladsp/*.yml "$SRC"/container/camilladsp/*.yml; do
      [[ -e "$f" ]] || continue
      if camilladsp --check "$f" >/tmp/upmix-cdsp-check.log 2>&1; then
        info "camilladsp --check OK：$(basename "$f")"
      else
        fail=1
        echo "  camilladsp --check FAIL：$(basename "$f")" >&2
        tail -5 /tmp/upmix-cdsp-check.log >&2
      fi
    done
  fi
  if [[ $fail -ne 0 ]]; then
    if [[ "$DRY" == "0" ]]; then
      info "校验失败，自动回滚本次安装…"
      do_rollback "$BACKUP_DIR"
    fi
    die "校验失败（已回滚）"
  fi
fi

if [[ -f /usr/local/bin/tspi-channelmap ]] && ! grep -q "by_name.setdefault" /usr/local/bin/tspi-channelmap 2>/dev/null; then
  echo "[upmix] 警告：/usr/local/bin/tspi-channelmap 看起来是没有合入 upmix 修复的旧渲染器。" >&2
  echo "        WebUI/channelmap profile 路线会输出错误音频（Filter step/inverted/bass 三处 bug）；" >&2
  echo "        请先部署 audio-stack 修复后的 tspi-channelmap（OEM v5 已包含），或暂用独立路径 tspi-upmix apply。" >&2
fi

cat <<EOF2

[upmix] 安装完成（未启动/重启任何服务）。备份：$BACKUP_DIR

启用上混（两种方式）：
  A) WebUI（推荐）：声道分配/校准页 -> profile 列表 -> 选择 upmix-4.1.2-music（或 -movie/-wide；
     渲染器未修复时用 *-safe）-> 加载 -> 校验 -> 应用。
     关闭：加载 upmix-off-restore（首次安装时的 active 快照）。
  B) SSH/命令行（独立配置，不依赖 channelmap 渲染器）：
     tspi-upmix apply upmix-4.1.2-music            # 模式 off 时只渲染/校验，不启动服务
     tspi-upmix apply upmix-5.3.2-movie --dry-run  # 先看配置
     tspi-upmix rollback                           # 回滚最近一次 apply

输入内容检测（H16 容器里只有 2.0 时自动上混；task-19）：
  - 手动覆盖：tspi-upmix input-mode set auto|stereo|multichannel
              （force-stereo/force-multichannel 别名也接受；文件 /etc/tspi-audio/input-mode.json）
  - 运行态：  /run/tspi/input-content.json（WebUI 只读；schema tspi-input-content/v1）
  - daemon：  tspi-upmix autodetect --input-layout h16-7.1.4（读 CamillaDSP websocket RMS，
              用 Aux1/Aux2 fader 在“上混/直通”间切换，不重启服务）
              服务示例：$UMIX_DIR/tspi-upmix-autodetect.service.example（默认不启用）
  - 建议 CamillaDSP 启动参数带 --gain1=-120 --gain2=0（默认直通，等 daemon 接管），
    或在 enabled 后由 daemon 首次连接立即设置；详见 docs/INPUT-MODE.md。

回滚安装：bash $SELF_DIR/install-upmix.sh --rollback
EOF2
exit 0
