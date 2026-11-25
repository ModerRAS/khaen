#!/bin/bash
# MOTU + snd-aloop 连续 xrun 冒烟（全零静音；不出声）
# 用法: sudo modprobe snd-aloop && bash loopback-xrun.sh [seconds=60] [chunksize=32] [fs=48000]
set -u
SEC=$1; SEC=${SEC:-60}
CHUNK=$2; CHUNK=${CHUNK:-32}
FS=$3; FS=${FS:-48000}
MOTU_DEV=${MOTU_DEV:-hw:3,0}
CAMILLA=${CAMILLA:-/usr/local/bin/camilladsp}
if [ ! -x "$CAMILLA" ]; then echo "camilladsp not found: $CAMILLA"; exit 1; fi
if ! lsmod | grep -q snd_aloop; then echo "snd-aloop 未加载：sudo modprobe snd-aloop"; exit 1; fi
LP_CARD=$(grep -i Loopback /proc/asound/cards | head -1 | awk -F: '{gsub(/ /,"",$1); print $1}')
if [ -z "$LP_CARD" ]; then echo "未找到 Loopback 卡"; exit 1; fi
echo "Loopback card=$LP_CARD MOTU=$MOTU_DEV chunk=$CHUNK fs=$FS sec=$SEC"
D=/tmp/motu-loopback
mkdir -p "$D"
[ -f "$D/zeros.raw" ] || dd if=/dev/zero of="$D/zeros.raw" bs=4096 count=4096 2>/dev/null
CFG="$D/camilla.yml"
{
  echo "devices:"
  echo "  samplerate: $FS"
  echo "  chunksize: $CHUNK"
  echo "  target_level: 128"
  echo "  adjust_period: 10"
  echo "  enable_rate_adjust: false"
  echo "  queuelimit: 4"
  echo "  capture:"
  echo "    type: Alsa"
  echo "    channels: 22"
  echo '    device: "hw:'"$LP_CARD"',1,0"'
  echo "    format: S32_LE"
  echo "  playback:"
  echo "    type: Alsa"
  echo "    channels: 22"
  echo '    device: "'"$MOTU_DEV"'"'
  echo "    format: S24_3LE"
  echo "filters: {}"
  echo "mixers: {}"
  echo "pipeline: []"
} > "$CFG"
LOG="$D/run-$(date +%s).log"
"$CAMILLA" -l warn "$CFG" > "$LOG" 2>&1 &
CAMPID=$!
aplay -D "hw:$LP_CARD,0,0" -t raw -c 22 -r "$FS" -f S32_LE /dev/zero > "$D/aplay.log" 2>&1 &
APID=$!
START=$(date +%s)
LAST=0
i=0
while [ "$i" -lt "$SEC" ]; do
  if [ -r /proc/$CAMPID/stat ]; then LAST=$(awk '{print $14+$15}' /proc/$CAMPID/stat 2>/dev/null); fi
  sleep 1
  i=$((i+1))
done
kill "$APID" "$CAMPID" 2>/dev/null
wait "$APID" "$CAMPID" 2>/dev/null
END=$(date +%s)
XR=$(grep -ciE "underrun|overrun|XRUN|xrun" "$LOG" 2>/dev/null)
XR=${XR:-0}
awk -v s=$START -v e=$END -v t=$LAST -v x=$XR -v req=$SEC 'BEGIN{wall=e-s; if(wall<=0)wall=1; cpu=t/100; printf "wall=%ds cpu=%.2fs cpu_pct=%.1f xruns=%s requested=%ss\n", wall, cpu, 100*cpu/wall, x, req}'
echo "log: $LOG"
echo "--- hw_params (playback) ---"
cat /proc/asound/card3/pcm0p/sub0/hw_params 2>/dev/null
echo "--- xrun lines ---"
grep -iE "underrun|overrun|XRUN" "$LOG" | head -10
