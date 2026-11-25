#!/bin/bash
# Start the BREN7P lab PTPv2 GM on a Linux VM (builder VM in the recorded run).
# Assumes the release payload is staged at /opt/khaen-rt-stack/ptp/current + /usr/local/lib/khaen + /etc/khaen-ptp
# (see rt-stack/ptp/INSTALL-ROLLBACK.md; for the lab the x86_64 linuxptp 4.2 build was staged the same way).
# Default dry-run: prints the plan; --live starts the GM and waits for MASTER.
# Usage: start-lab-gm.sh --iface IFACE [--out DIR] [--conf CONF] [--live]
set -uo pipefail
IFACE=""; OUT="$HOME/lab/bren7p-gm"; CONF=/etc/khaen-ptp/ptp4l-master.conf; LIVE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --iface) IFACE="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --conf) CONF="$2"; shift 2 ;;
    --live) LIVE=1; shift ;;
    --dry-run) LIVE=0; shift ;;
    -h|--help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$IFACE" ] || { echo "--iface required (e.g. enp0s8 on the BREN7P host-only network)" >&2; exit 2; }
[ -e "/sys/class/net/$IFACE" ] || { echo "interface not found: $IFACE" >&2; exit 3; }
[ -x /usr/local/lib/khaen/ptp-master-start ] || { echo "wrapper missing: /usr/local/lib/khaen/ptp-master-start" >&2; exit 3; }
CMD=(env KHAEN_PTP_IFACE="$IFACE" KHAEN_PTP_CONF="$CONF" KHAEN_PTP_HW_TS=auto /usr/local/lib/khaen/ptp-master-start)
if [ "$LIVE" = "0" ]; then
  echo "[dry-run] mkdir -p $OUT"
  echo "[dry-run] sudo nohup ${CMD[@]} > $OUT/gm.log 2>&1 &"
  echo "[dry-run] sudo pmc -s /run/khaen-ptp/ptp4l.uds -u -b 0 'GET PORT_DATA_SET'  (expect portState MASTER)"
  exit 0
fi
mkdir -p "$OUT" || { echo "cannot create $OUT" >&2; exit 3; }
: > "$OUT/gm-states.csv"
sudo nohup "${CMD[@]}" > "$OUT/gm.log" 2>&1 &
echo "gm pid=$!"
for i in $(seq 1 20); do
  st=$(sudo /opt/khaen-rt-stack/ptp/current/bin/pmc -s /run/khaen-ptp/ptp4l.uds -u -b 0 'GET PORT_DATA_SET' 2>/dev/null | awk '/portState/ {print $2; exit}')
  echo "wait[$i] portState=${st:-}"
  [ "$st" = "MASTER" ] && break
  sleep 1
done
[ "$st" = "MASTER" ] || { echo "GM did not become MASTER"; tail -n 20 "$OUT/gm.log" >&2; exit 3; }
sudo /opt/khaen-rt-stack/ptp/current/bin/pmc -s /run/khaen-ptp/ptp4l.uds -u -b 0 'GET PARENT_DATA_SET' > "$OUT/gm-parent-dataset.txt" 2>&1
( while :; do
    printf '%s,%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      "$(sudo /opt/khaen-rt-stack/ptp/current/bin/pmc -s /run/khaen-ptp/ptp4l.uds -u -b 0 'GET PORT_DATA_SET' 2>/dev/null | awk '/portState/ {print $2; exit}')" >> "$OUT/gm-states.csv"
    sleep 60
  done ) > "$OUT/gm-monitor.log" 2>&1 &
echo "gm monitor pid=$!"
