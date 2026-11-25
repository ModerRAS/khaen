#!/bin/bash
# rollback-to-rx.sh —— 一键回滚：停常驻 tspi-rt-host-nsp7 -> 启 standalone aes67-rx supervisor
# 用法: bash rollback-to-rx.sh            # 仅本会话回滚（unit 仍 enabled，重启后会再起 host）
#       bash rollback-to-rx.sh --disable  # 永久回滚（disable unit，重启后走 rx）
set -u
DISABLE=0
[ "${1:-}" = "--disable" ] && DISABLE=1
say() { echo "[rollback] $(date -Is) $*"; }

say "stop tspi-rt-host-nsp7"
systemctl stop tspi-rt-host-nsp7 2>/dev/null || true
sleep 1
if pgrep -x tspi-rt-host >/dev/null 2>&1; then
  say "host still alive; SIGKILL"
  pkill -9 -x tspi-rt-host 2>/dev/null || true
  sleep 0.5
fi

say "start standalone rx supervisor"
bash /root/aes67-rx-work/tools/start-rx.sh

if [ "$DISABLE" = "1" ]; then
  say "disable tspi-rt-host-nsp7 (permanent rollback)"
  systemctl disable tspi-rt-host-nsp7 2>/dev/null || true
fi
