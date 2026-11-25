#!/bin/bash
systemctl stop tspi-soak5.service 2>&1 || true
sleep 2
systemctl is-active tspi-soak5.service 2>&1 || true
pkill -f '/tmp/camilla-fifo80' 2>/dev/null || true
pkill -f 'aplay -D hw:5,0,0' 2>/dev/null || true
D=/var/lib/tspi-soak
STAMP=$(date +%Y%m%d-%H%M%S)
mkdir -p "$D/prev-$STAMP"
for f in status.csv soak.log camilla.log aplay.log camilla.yml; do
  [ -e "$D/$f" ] && mv "$D/$f" "$D/prev-$STAMP/"
done
cp "$D/soak12.sh" "$D/soak-min.sh"
python3 - <<'PY'
from pathlib import Path
p = Path('/var/lib/tspi-soak/soak-min.sh')
s = p.read_text()
s = s.replace('HOURS=${1:-12}\nMAX_S=$((HOURS * 3600))', 'MINUTES=${1:-5}\nMAX_S=$((MINUTES * 60))')
s = s.replace('log "duration=${HOURS}h (max=${MAX_S}s)"', 'log "duration=${MINUTES}m (max=${MAX_S}s)"')
s = s.replace('for i in $(seq 1 $((HOURS * 60))); do', 'for i in $(seq 1 $MINUTES); do')
s = s.replace('log "DONE ${HOURS}h xrun_total=$X"', 'log "DONE ${MINUTES}m xrun_total=$X"')
p.write_text(s)
PY
chmod +x "$D/soak-min.sh"
grep -nE 'MINUTES|MAX_S|for i in|DONE' "$D/soak-min.sh" | head
echo '== start 5m =='
systemd-run --unit=tspi-soak5m --collect --property=Restart=no "$D/soak-min.sh" 5
sleep 8
systemctl is-active tspi-soak5m.service
tail -5 "$D/soak.log"
