#!/bin/bash
D=/var/lib/khaen-soak
STAMP=$(date +%Y%m%d-%H%M%S)
mkdir -p "$D/prev-$STAMP"
for f in status.csv soak.log camilla.log aplay.log camilla.yml; do
  [ -e "$D/$f" ] && mv "$D/$f" "$D/prev-$STAMP/"
done
systemd-run --unit=khaen-soak12 --collect --property=Restart=no "$D/soak12.sh" 12
sleep 8
systemctl is-active khaen-soak12.service
tail -6 "$D/soak.log"
tail -2 "$D/status.csv" 2>/dev/null || true
