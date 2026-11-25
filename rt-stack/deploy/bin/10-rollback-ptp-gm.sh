#!/usr/bin/env bash
# KHAEN PTPv2 grandmaster add-on rollback (board-side, default dry-run).
#
# Default: stop + disable khaen-ptp.target/units and, when a previous release is recorded,
# repoint /opt/khaen-rt-stack/ptp/current to it. Files/config stay in place (recoverable).
# --remove: additionally delete the units, launcher wrappers, /etc/khaen-ptp and the PTP release
# tree; existing configs are backed up under /var/lib/khaen-rt-stack/ptp-rollback-<ts>/ first.
#
# Usage:
#   ./10-rollback-ptp-gm.sh [--live --authorize WINDOW.json] [--remove] [--root STAGING]
# Exit: 0 OK; 2 usage; 3 state/precheck FAIL; 4 authorization; 7 rollback FAIL.
# Board execution: NOT_RUN (see rt-stack/ptp/INSTALL-ROLLBACK.md).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/common.sh"

REMOVE=0
PTP_PREFIX="${KHAEN_PTP_PREFIX:-$KHAEN_STACK_PREFIX/ptp}"
PTP_ETC="${KHAEN_PTP_ETC:-/etc/khaen-ptp}"
PTP_LIBDIR="${KHAEN_PTP_LIBDIR:-/usr/local/lib/khaen}"
RECORD_FILE="$KHAEN_STATE_DIR/ptp-install.json"
UNITS=(khaen-ptp-master.service khaen-ptp-phc2sys.service khaen-ptp.target)
SERVICE_UNITS=(khaen-ptp-master.service khaen-ptp-phc2sys.service)

usage() { sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --remove) REMOVE=1; shift ;;
    --live) DRY_RUN=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --authorize) AUTHORIZE_FILE="$2"; shift 2 ;;
    --root) ROOT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: $1" 2 ;;
  esac
done
require_authorization "rollback-ptp-master"
require_root_live
log "PTP GM rollback: remove=$REMOVE dry_run=$DRY_RUN root=$ROOT"

RID=""; PREV=""
if [ -f "$(p "$RECORD_FILE")" ]; then
  RID="$(py -c 'import json,sys;print(json.load(open(sys.argv[1])).get("release_id") or "")' "$(p "$RECORD_FILE")")" || true
  PREV="$(py -c 'import json,sys;print(json.load(open(sys.argv[1])).get("previous_release") or "")' "$(p "$RECORD_FILE")")" || true
  log "安装记录: release_id=${RID:-?} previous=${PREV:-none}"
else
  if [ "$REMOVE" = "1" ] || [ "$DRY_RUN" = "1" ]; then
    warn "无安装记录 $(p "$RECORD_FILE")（remove=$REMOVE dry_run=$DRY_RUN）"
  else
    die "无安装记录 $(p "$RECORD_FILE")：拒绝猜测回滚目标（可人工检查或 --remove）" 3
  fi
fi

BACKUP="$(p "$KHAEN_STATE_DIR")/ptp-rollback-$(date +%Y%m%dT%H%M%S)"
CURRENT="$(p "$PTP_PREFIX/current")"

# ---- 1) stop + disable (never touches legacy/slave-only PTP owners) ----
run "$KHAEN_SYSTEMCTL" disable --now khaen-ptp.target
for u in "${SERVICE_UNITS[@]}"; do run "$KHAEN_SYSTEMCTL" stop "$u"; done

if [ "$REMOVE" = "1" ]; then
  # ---- 2a) full removal with evidence backup ----
  log "full removal：备份现有 unit/配置到 $BACKUP"
  run install -d -m 0755 "$BACKUP"
  for u in "${UNITS[@]}"; do
    [ -f "$(p "$KHAEN_SYSTEMD_DIR/$u")" ] && run cp -a "$(p "$KHAEN_SYSTEMD_DIR/$u")" "$BACKUP/$u"
  done
  [ -d "$(p "$PTP_ETC")" ] && run cp -a "$(p "$PTP_ETC")" "$BACKUP/etc-khaen-ptp"
  for u in "${UNITS[@]}"; do run rm -f "$(p "$KHAEN_SYSTEMD_DIR/$u")"; done
  run "$KHAEN_SYSTEMCTL" daemon-reload
  run rm -f "$(p "$PTP_LIBDIR/ptp-lib.sh")" "$(p "$PTP_LIBDIR/ptp-master-start")" "$(p "$PTP_LIBDIR/ptp-phc2sys-start")"
  run rm -rf "$(p "$PTP_PREFIX")"
  run rm -rf "$(p "$PTP_ETC")"
  run rm -rf "$(p /run/khaen-ptp)"
  log "已删除 units/wrappers/$(p "$PTP_PREFIX")/$(p "$PTP_ETC")（备份在 $BACKUP）"
else
  # ---- 2b) stop/disable, optionally repoint current ----
  if [ -n "$PREV" ]; then
    run ln -sfn "releases/$PREV" "$CURRENT"
    log "current 已回指上一版本: releases/$PREV（units/配置保留；需要彻底移除加 --remove）"
  else
    log "无上一版本可回退：已 stop+disable，文件与配置保留（加 --remove 彻底移除）"
  fi
fi

# ---- 3) record state ----
if [ "$DRY_RUN" = "1" ]; then
  log "[dry-run] 将更新安装记录 state=$( [ "$REMOVE" = "1" ] && echo removed || echo rolled_back )"
else
  if [ -f "$(p "$RECORD_FILE")" ]; then
    py - "$(p "$RECORD_FILE")" "$REMOVE" <<'PY' || warn "更新安装记录失败（服务已停，忽略）"
import json, sys, datetime
path, remove = sys.argv[1], sys.argv[2] == '1'
rec = json.load(open(path, encoding='utf-8'))
rec['state'] = 'removed' if remove else 'rolled_back'
rec['rolled_back_at'] = datetime.datetime.now(datetime.timezone.utc).astimezone().isoformat(timespec='seconds')
open(path, 'w', encoding='utf-8').write(json.dumps(rec, ensure_ascii=False, indent=1) + '\n')
print(json.dumps(rec, ensure_ascii=False))
PY
  fi
fi
log "回滚完成（服务已停用；legacy PTP owner 未触碰，需要时人工启动）。"
exit 0
