#!/bin/bash
# nsp7-restart-after-mimo.sh —— MIMO A/B 结束后：重启同一常驻 host 并验证 streaming
# 前提：mimo-prod 已释放 card1；本脚本不 disable/不改配置、不改 CueMix（仅回读报告）。
set -u
EV="${1:-/root/aes67-rx-work/chain/nsp7-daemon-20261122/game-window}"
mkdir -p "$EV"
TAG=obs30-after-mimo
date -Is | tee "$EV/restart-after-mimo.txt"
echo '--- pre: card1 state (expect closed) ---' | tee -a "$EV/restart-after-mimo.txt"
head -3 /proc/asound/card1/pcm0p/sub0/status 2>&1 | tee -a "$EV/restart-after-mimo.txt"
echo '--- start unit ---' | tee -a "$EV/restart-after-mimo.txt"
systemctl start khaen-rt-host-nsp7; echo START_RC=$? | tee -a "$EV/restart-after-mimo.txt"
for i in $(seq 1 30); do
  A=$(systemctl is-active khaen-rt-host-nsp7)
  P=$(pgrep -x khaen-rt-host | head -1)
  S=$(awk '/^state:/{print $2}' /proc/asound/card1/pcm0p/sub0/status 2>/dev/null)
  if [ "$A" = active ] && [ -n "$P" ] && [ "$S" = RUNNING ]; then
    echo "started_after=${i}s unit=$A pid=$P alsa=$S" | tee -a "$EV/restart-after-mimo.txt"; break
  fi
  sleep 1
done
echo '--- live verify ---' | tee -a "$EV/restart-after-mimo.txt"
pgrep -x khaen-rt-host | tee -a "$EV/restart-after-mimo.txt"
ss -uanp | grep ':5004' | tee -a "$EV/restart-after-mimo.txt"
head -3 /proc/asound/card1/pcm0p/sub0/status | tee -a "$EV/restart-after-mimo.txt"
echo '--- cue mix (read-only) ---' | tee -a "$EV/restart-after-mimo.txt"
python3 /root/cuemix_client.py --host 169.254.111.126 --read 4 --save "$EV/cuemix-after-mimo.json" >/dev/null 2>&1
python3 - <<'EOF' | tee -a "$EV/restart-after-mimo.txt"
import json
d=json.load(open('/root/aes67-rx-work/chain/nsp7-daemon-20261122/game-window/cuemix-after-mimo.json'))['values']
print('koTrim', d.get('koTrim',[])[:12], 'kMainTrim', d.get('kMainTrim'), 'koBusMute', d.get('koBusMute'))
EOF
echo '--- 30s observation ---' | tee -a "$EV/restart-after-mimo.txt"
bash /root/aes67-rx-work/tools/nsp7-observe.sh $TAG 30 2 "$EV" >/dev/null 2>&1
cat "$EV/$TAG.samples.csv" | tee -a "$EV/restart-after-mimo.txt"
echo '--- journal tail ---' | tee -a "$EV/restart-after-mimo.txt"
journalctl -u khaen-rt-host-nsp7 -n 10 --no-pager | tail -10 | tee -a "$EV/restart-after-mimo.txt"
echo RESTART_AFTER_MIMO_DONE
