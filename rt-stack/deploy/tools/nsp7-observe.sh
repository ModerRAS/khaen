#!/bin/bash
# nsp7-observe.sh TAG SECONDS [INTERVAL] —— 出声窗口实时观察（frames/CPU/RSS/NSP pps/ALSA 状态）
set -u
TAG="${1:-obs}"
SECS="${2:-30}"
IV="${3:-2}"
EV="${4:-/root/aes67-rx-work/chain/nsp7-daemon-20261122/game-window}"
mkdir -p "$EV"
CSV="$EV/$TAG.samples.csv"
echo "t_s,unit,pid,state,hw_ptr,appl_ptr,delay,utime,stime,rss_kb,load1,temp0,nsp_pps" > "$CSV"
date -Is > "$EV/$TAG.start.txt"
T0=$(date +%s)
while :; do
  NOW=$(date +%s); T=$(( NOW - T0 ))
  [ "$T" -gt "$SECS" ] && break
  U=$(systemctl is-active khaen-rt-host-nsp7)
  P=$(pgrep -x khaen-rt-host | head -1)
  ST=$(awk '/^state:/{print $2}' /proc/asound/card1/pcm0p/sub0/status 2>/dev/null)
  HW=$(awk '/hw_ptr/{print $3}' /proc/asound/card1/pcm0p/sub0/status 2>/dev/null)
  AP=$(awk '/appl_ptr/{print $3}' /proc/asound/card1/pcm0p/sub0/status 2>/dev/null)
  DL=$(awk '/delay /{print $3}' /proc/asound/card1/pcm0p/sub0/status 2>/dev/null)
  UT=0; ST2=0; RSS=0
  if [ -n "$P" ] && [ -r "/proc/$P/stat" ]; then
    UT=$(awk '{print $14}' /proc/$P/stat); ST2=$(awk '{print $15}' /proc/$P/stat)
    RSS=$(awk '/VmRSS/{print $2}' /proc/$P/status)
  fi
  LOAD=$(awk '{print $1}' /proc/loadavg)
  TEMP=$(cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null || echo 0)
  PPS=$(timeout 2 tcpdump -ni enp2s0 -q 'udp dst port 5004' -c 5000 2>/dev/null | wc -l)
  echo "$T,$U,${P:-none},${ST:-NA},${HW:-0},${AP:-0},${DL:-0},$UT,$ST2,${RSS:-0},$LOAD,$TEMP,$PPS" >> "$CSV"
  sleep $IV
done
date -Is > "$EV/$TAG.end.txt"
echo OBS_DONE_TAG=$TAG > "$EV/$TAG.done"
