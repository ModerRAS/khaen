#!/bin/bash
# status-host-nsp7.sh —— 常驻链路只读状态快照（不写、不重启任何东西）
set -u
echo "=== date ==="; date -Is
echo "=== unit ==="; systemctl status tspi-rt-host-nsp7 --no-pager -l 2>&1 | head -20
echo "=== procs ==="; pgrep -af 'tspi-rt-host|nsp7-wait-stream|aes67-rx' | head -10
echo "=== 5004 ==="; ss -uanp 2>/dev/null | grep -E ':5004' || echo 'NO_5004_SOCKET'
echo "=== ALSA card1 pcm0p ==="; cat /proc/asound/card1/pcm0p/sub0/status 2>&1 | head -8
echo "=== ALSA owner fuser ==="; fuser -v /dev/snd/pcmC1D0p 2>&1 | head -5
echo "=== hw_ptr snapshot 1 ==="; awk '/hw_ptr|appl_ptr/ {print}' /proc/asound/card1/pcm0p/sub0/status 2>/dev/null
echo "=== host proc cpu/rss ==="; for p in $(pgrep -x tspi-rt-host); do awk -v p=$p '{print "pid="p" utime="$14" stime="$15" start="$22}' /proc/$p/stat; grep -E 'VmRSS' /proc/$p/status; done
echo "=== nsp rtp pps (2s) ==="; timeout 2 tcpdump -ni enp2s0 -q 'udp dst port 5004' -c 4000 2>/dev/null | wc -l
