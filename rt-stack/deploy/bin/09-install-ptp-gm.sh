#!/usr/bin/env bash
# TSPI PTPv2 grandmaster add-on installer (board-side, default dry-run).
#
# Verifies the aarch64 PTP add-on bundle, installs it under /opt/tspi-rt-stack/ptp/releases/<id>
# with an atomic "current" switching symlink, installs the launcher wrappers + unit files, seeds
# /etc/tspi-ptp/ templates only when missing, daemon-reloads and ENABLES (never starts) tspi-ptp.target.
# Records the previous release id in /var/lib/tspi-rt-stack/ptp-install.json for rollback.
#
# Usage:
#   ./09-install-ptp-gm.sh --bundle DIR [--live --authorize WINDOW.json] [--root STAGING] [--force]
# Exit: 0 OK; 2 usage/input; 3 bundle/precheck FAIL; 4 authorization; 7 environment/install FAIL.
# Board execution: NOT_RUN (see rt-stack/ptp/INSTALL-ROLLBACK.md).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"

BUNDLE=""; FORCE=0
PTP_PREFIX="${TSPI_PTP_PREFIX:-$TSPI_STACK_PREFIX/ptp}"
PTP_ETC="${TSPI_PTP_ETC:-/etc/tspi-ptp}"
PTP_LIBDIR="${TSPI_PTP_LIBDIR:-/usr/local/lib/tspi}"
RECORD_FILE="$TSPI_STATE_DIR/ptp-install.json"
REQUIRED_BINS=(ptp4l phc2sys pmc phc_ctl)
REQUIRED_LIB=(ptp-lib.sh ptp-master-start ptp-phc2sys-start)
REQUIRED_UNITS=(tspi-ptp-master.service tspi-ptp-phc2sys.service tspi-ptp.target)
LEGACY_UNITS=(tspi-ptp4l-master.service tspi-phc2sys.service tspi-statime-inferno.service tspi-aes67-daemon.service tspi-ravenna-lkm.service)

usage() { sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --bundle) BUNDLE="$2"; shift 2 ;;
    --force) FORCE=1; shift ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --authorize) AUTHORIZE_FILE="$2"; shift 2 ;;
    --root) ROOT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done
[ -n "$BUNDLE" ] || die "缺少 --bundle <PTP add-on bundle 目录>" 2
[ -d "$BUNDLE" ] || die "bundle 不存在: $BUNDLE" 2
[ -f "$BUNDLE/manifest.json" ] || die "bundle 缺 manifest.json: $BUNDLE" 3
require_authorization "install-ptp-master"
require_root_live
log "PTP GM install: bundle=$BUNDLE dry_run=$DRY_RUN root=$ROOT"

# ---- 0) read-only bundle verification (both dry-run and live) ----
for b in "${REQUIRED_BINS[@]}"; do [ -f "$BUNDLE/bin/$b" ] || die "bundle 缺 bin/$b" 3; done
for f in "${REQUIRED_LIB[@]}"; do [ -f "$BUNDLE/lib/$f" ] || die "bundle 缺 lib/$f" 3; done
for u in "${REQUIRED_UNITS[@]}"; do [ -f "$BUNDLE/units/$u" ] || die "bundle 缺 units/$u" 3; done
[ -f "$BUNDLE/INSTALL-ROLLBACK.md" ] || warn "bundle 缺 INSTALL-ROLLBACK.md（继续）"
py "$DEPLOY_ROOT/lib/release_manifest.py" verify "$BUNDLE" --require-arch aarch64 >/dev/null \
  || die "bundle manifest/arch 校验 FAIL（拒绝安装）" 3
RID="$(py -c 'import json,sys;print(json.load(open(sys.argv[1]))["release_id"])' "$BUNDLE/manifest.json")" \
  || die "无法读取 release_id" 3
log "bundle verify PASS: release_id=$RID"

# ---- 1) read-only preflight: report legacy PTP owners (we never stop them silently) ----
if [ "$DRY_RUN" = "1" ]; then
  for u in "${LEGACY_UNITS[@]}"; do log "[dry-run][ro] 将检查 legacy unit: $u"; done
else
  for u in "${LEGACY_UNITS[@]}"; do
    if sunit_active "$u"; then
      warn "$u 当前 active：tspi-ptp.target 启动时因 Conflicts= 会停掉它；请确认时钟 owner 切换是本次窗口的意图"
    fi
  done
fi

RELDIR="$(p "$PTP_PREFIX/releases/$RID")"
CURRENT="$(p "$PTP_PREFIX/current")"
PREV=""
if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] 读取 current 软链: $CURRENT"
else
  if [ -L "$CURRENT" ]; then
    PREV="$(basename "$(readlink "$CURRENT")")"
    [ "$PREV" = "$RID" ] && PREV=""
  elif [ -e "$CURRENT" ]; then
    die "$CURRENT 存在但不是符号链接（拒绝覆盖；人工检查）" 3
  fi
fi
log "安装布局: release=$RELDIR current=$CURRENT previous=${PREV:-none}"
log "配置模板目录: $(p "$PTP_ETC")；wrapper 目录: $(p "$PTP_LIBDIR")；记录: $(p "$RECORD_FILE")"

# ---- 2) install release payload ----
run install -d -m 0755 "$RELDIR/bin" "$RELDIR/lib" "$RELDIR/units" "$RELDIR/etc" \
  "$(p "$PTP_LIBDIR")" "$(p "$PTP_ETC")" "$(p "$TSPI_SYSTEMD_DIR")" "$(p "$TSPI_STATE_DIR")"
for b in "${REQUIRED_BINS[@]}"; do run install -m 0755 "$BUNDLE/bin/$b" "$RELDIR/bin/$b"; done
for f in "${REQUIRED_LIB[@]}"; do
  case "$f" in
    ptp-lib.sh) run install -m 0644 "$BUNDLE/lib/$f" "$RELDIR/lib/$f" ;;
    *)          run install -m 0755 "$BUNDLE/lib/$f" "$RELDIR/lib/$f" ;;
  esac
done
for u in "${REQUIRED_UNITS[@]}"; do run install -m 0644 "$BUNDLE/units/$u" "$RELDIR/units/$u"; done
for f in "$BUNDLE"/etc/*.example; do [ -f "$f" ] && run install -m 0644 "$f" "$RELDIR/etc/$(basename "$f")"; done
run install -m 0644 "$BUNDLE/manifest.json" "$RELDIR/manifest.json"
[ -f "$BUNDLE/README.deploy.md" ] && run install -m 0644 "$BUNDLE/README.deploy.md" "$RELDIR/README.deploy.md"
[ -f "$BUNDLE/INSTALL-ROLLBACK.md" ] && run install -m 0644 "$BUNDLE/INSTALL-ROLLBACK.md" "$RELDIR/INSTALL-ROLLBACK.md"

# ---- 3) launcher wrappers into /usr/local/lib/tspi ----
run install -m 0644 "$BUNDLE/lib/ptp-lib.sh" "$(p "$PTP_LIBDIR/ptp-lib.sh")"
run install -m 0755 "$BUNDLE/lib/ptp-master-start" "$(p "$PTP_LIBDIR/ptp-master-start")"
run install -m 0755 "$BUNDLE/lib/ptp-phc2sys-start" "$(p "$PTP_LIBDIR/ptp-phc2sys-start")"

# ---- 4) site config: seed only when absent (never overwrite an existing PTP site config) ----
seed_cfg() {
  local src="$1" dst="$2" mode="$3"
  if [ -e "$(p "$dst")" ]; then
    log "保留现有配置（不覆盖）: $dst"
  else
    run install -m "$mode" "$src" "$(p "$dst")"
  fi
}
seed_cfg "$BUNDLE/etc/ptp-master.env.example"      "$PTP_ETC/ptp-master.env"      0600
seed_cfg "$BUNDLE/etc/ptp4l-master-udp.conf.example" "$PTP_ETC/ptp4l-master.conf" 0644
seed_cfg "$BUNDLE/etc/ptp4l-master-l2.conf.example"  "$PTP_ETC/ptp4l-master-l2.conf" 0644

# ---- 5) units + enable (do not start) ----
for u in "${REQUIRED_UNITS[@]}"; do run install -m 0644 "$BUNDLE/units/$u" "$(p "$TSPI_SYSTEMD_DIR/$u")"; done
run "$TSPI_SYSTEMCTL" daemon-reload
run "$TSPI_SYSTEMCTL" enable tspi-ptp.target

# ---- 6) atomic current switch ----
run ln -sfn "releases/$RID" "$CURRENT"

# ---- 7) record for rollback + post checks ----
if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] 将写安装记录: $(p "$RECORD_FILE")（release_id=$RID previous=${PREV:-none}）"
else
  install -d -m 0755 "$(p "$TSPI_STATE_DIR")" || die "无法创建 $(p "$TSPI_STATE_DIR")" 7
  py - "$(p "$RECORD_FILE")" "$RID" "${PREV:-}" "$BUNDLE" <<'PY' || die "写安装记录失败" 7
import json, sys, datetime
path, rid, prev, bundle = sys.argv[1:5]
rec = {
    'schema': 'tspi-rt-stack/ptp-install/v1',
    'release_id': rid,
    'previous_release': prev or None,
    'bundle': bundle,
    'state': 'installed',
    'installed_at': datetime.datetime.now(datetime.timezone.utc).astimezone().isoformat(timespec='seconds'),
    'notes': 'tspi-ptp.target enabled but NOT started by the installer; start inside the authorized window',
}
open(path, 'w', encoding='utf-8').write(json.dumps(rec, ensure_ascii=False, indent=1) + '\n')
print(json.dumps(rec, ensure_ascii=False))
PY
  [ -x "$CURRENT/bin/ptp4l" ] || die "current/bin/ptp4l 不可执行（安装后自检失败）" 7
  for f in ptp-master-start ptp-phc2sys-start; do
    [ -x "$(p "$PTP_LIBDIR/$f")" ] || die "wrapper 不可执行: $f" 7
  done
  log "安装后自检 PASS: current/bin/ptp4l + wrappers 可执行"
fi
log "完成：release $RID 已安装；tspi-ptp.target 已 enable（未 start）。启动/验证/回滚见 INSTALL-ROLLBACK.md。"
exit 0
