#!/bin/bash
# switch-to-host-nsp7.sh —— 一键切换：停 standalone aes67-rx supervisor -> 启常驻 khaen-rt-host-nsp7
# 前提：CueMix 已静音（koTrim[0..11]=100 + kMainTrim=100）；NSP 正在向 udp/5004 发流
# （unit 内 wrapper 会等流，NSP-first）。回滚见 rollback-to-rx.sh。
set -u
say() { echo "[switch] $(date -Is) $*"; }

say "stop host unit if running"
systemctl stop khaen-rt-host-nsp7 2>/dev/null || true

say "stop standalone rx supervisor (release 5004 + MOTU ALSA)"
bash /root/aes67-rx-work/tools/stop-rx.sh

for i in $(seq 1 12); do
  if ! pgrep -x aes67-rx >/dev/null 2>&1 && ! ss -lun 2>/dev/null | grep -q ':5004 '; then
    break
  fi
  sleep 0.5
done

say "start khaen-rt-host-nsp7 (NSP-first gate inside)"
systemctl start khaen-rt-host-nsp7
say "started; check: systemctl status khaen-rt-host-nsp7 --no-pager; journalctl -u khaen-rt-host-nsp7 -n 20"
