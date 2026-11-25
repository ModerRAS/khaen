#!/bin/bash
# KHAEN PTP offline loopback validation (no board, no PHC, no production LAN traffic).
#
# Creates two network namespaces connected by a veth pair:
#   GM  namespace: ptp4l grandmaster (raw binary OR the shipped ptp-master-start wrapper)
#   SLV namespace: linuxptp ptp4l slave, clock_servo nullf (never steps the host clock)
# PTP traffic is confined to the veth pair; the host NIC / production network is untouched.
#
# The slave CURRENT_DATA_SET.offsetFromMaster is sampled through pmc once per second and
# summarised (mean/std/min/max/p95 of |offset|). Software timestamping is used (veth has no
# PHC), so these numbers validate protocol/config/lock behaviour, NOT board PHC accuracy.
#
# Usage:
#   sudo tests/lab-gm-slave.sh --ptp4l BIN --pmc BIN --gm-conf CONF \
#        --transport udp|l2 --duration SEC --out DIR [--max-abs-ns N]
#   sudo tests/lab-gm-slave.sh --gm-wrapper /usr/local/lib/khaen/ptp-master-start \
#        --gm-conf /etc/khaen-ptp/ptp4l-master.conf --pmc BIN ... [--gm-env K=V ...]
#
# Exit: 0 PASS; 2 usage/environment; 3 validation FAIL.
set -uo pipefail

PTP4L=""; PMC=""; GM_WRAPPER=""; GM_CONF=""; TRANSPORT="udp"; DURATION=60
OUT=""; MAX_ABS_NS=5000000; SLAVE_SYNC="-3"; GM_ENV=()
NS_GM=""; NS_SLV=""; GM_PID=""; SLAVE_PID=""

usage() { sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//' >&2; }
die() { echo "[lab][error] $*" >&2; exit "${2:-2}"; }
log() { echo "[lab $(date +%H:%M:%S)] $*" >&2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --ptp4l) PTP4L="${2:-}"; shift 2 ;;
    --pmc) PMC="${2:-}"; shift 2 ;;
    --gm-wrapper) GM_WRAPPER="${2:-}"; shift 2 ;;
    --gm-conf) GM_CONF="${2:-}"; shift 2 ;;
    --transport) TRANSPORT="${2:-}"; shift 2 ;;
    --duration) DURATION="${2:-}"; shift 2 ;;
    --out) OUT="${2:-}"; shift 2 ;;
    --max-abs-ns) MAX_ABS_NS="${2:-}"; shift 2 ;;
    --slave-sync) SLAVE_SYNC="${2:-}"; shift 2 ;;
    --gm-env) GM_ENV+=("${2:-}"); shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

[ "$(id -u)" -eq 0 ] || die "must run as root (ip netns/veth)"
command -v ip >/dev/null 2>&1 || die "iproute2 missing"
command -v python3 >/dev/null 2>&1 || die "python3 missing (summary)"
[ -n "$OUT" ] || die "--out required"
{ [ -n "$PMC" ] && [ -x "$PMC" ]; } || die "--pmc must point to an executable pmc"
{ [ -n "$GM_CONF" ] && [ -r "$GM_CONF" ]; } || die "--gm-conf must be readable"
if [ -z "$GM_WRAPPER" ]; then
  { [ -n "$PTP4L" ] && [ -x "$PTP4L" ]; } || die "--ptp4l or --gm-wrapper required"
fi
case "$TRANSPORT" in udp|l2) ;; *) die "--transport must be udp|l2" ;; esac
case "$DURATION" in ''|*[!0-9]*) die "--duration must be an integer (seconds)" ;; esac

mkdir -p "$OUT" || die "cannot create out dir: $OUT"
NS_GM="khaen-gm-$$"; NS_SLV="khaen-slv-$$"

cleanup() {
  [ -n "$SLAVE_PID" ] && kill "$SLAVE_PID" 2>/dev/null || true
  [ -n "$GM_PID" ] && kill "$GM_PID" 2>/dev/null || true
  sleep 0.3 2>/dev/null || true
  ip netns del "$NS_SLV" 2>/dev/null || true
  ip netns del "$NS_GM" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ---------- configs ----------
if [ "$TRANSPORT" = "udp" ]; then TRANSPORT_CFG="UDPv4"; else TRANSPORT_CFG="L2"; fi
GM_CONF_RUN="$OUT/gm.conf"
sed "s|^[[:space:]]*uds_address.*|uds_address                 $OUT/gm.uds|" "$GM_CONF" > "$GM_CONF_RUN" || die "cannot render GM conf"
if ! grep -q "$OUT/gm.uds" "$GM_CONF_RUN"; then
  echo "uds_address                 $OUT/gm.uds" >> "$GM_CONF_RUN"
fi
GM_UDS="$OUT/gm.uds"

SLAVE_CONF="$OUT/slave.conf"
cat > "$SLAVE_CONF" <<EOF
[global]
clientOnly                  1
serverOnly                  0
domainNumber                0
network_transport           $TRANSPORT_CFG
delay_mechanism             E2E
clock_servo                 nullf
step_threshold              1000
first_step_threshold        1000
logAnnounceInterval         0
logSyncInterval             $SLAVE_SYNC
announceReceiptTimeout      3
tx_timestamp_timeout        10
uds_address                 $OUT/slave.uds
EOF

DATE_BEFORE=$(date -u +%s.%N)
UP_BEFORE=$(cut -d' ' -f1 /proc/uptime)

# ---------- netns + veth ----------
log "netns $NS_GM <-> $NS_SLV (veth), transport=$TRANSPORT"
ip netns add "$NS_GM" || die "ip netns add failed (root?)"
ip netns add "$NS_SLV" || die "ip netns add failed"
ip link add veth-gm type veth peer name veth-slv || die "veth add failed"
ip link set veth-gm netns "$NS_GM"
ip link set veth-slv netns "$NS_SLV"
for ns in "$NS_GM" "$NS_SLV"; do ip -n "$ns" link set lo up; done
ip -n "$NS_GM" link set veth-gm up
ip -n "$NS_SLV" link set veth-slv up
ip -n "$NS_GM" addr add 169.254.11.1/30 dev veth-gm 2>/dev/null || true
ip -n "$NS_SLV" addr add 169.254.11.2/30 dev veth-slv 2>/dev/null || true

pmc_gm()  { "$PMC" -s "$GM_UDS" -u -b 0 "$1" 2>&1; }
pmc_slv() { "$PMC" -s "$OUT/slave.uds" -u -b 0 "$1" 2>&1; }

# ---------- GM ----------
if [ -n "$GM_WRAPPER" ]; then
  log "starting GM via wrapper: $GM_WRAPPER"
  ip netns exec "$NS_GM" env "${GM_ENV[@]}" KHAEN_PTP_IFACE=veth-gm KHAEN_PTP_CONF="$GM_CONF_RUN" "$GM_WRAPPER" > "$OUT/gm.log" 2>&1 &
else
  log "starting GM binary: $PTP4L"
  ip netns exec "$NS_GM" "$PTP4L" -f "$GM_CONF_RUN" -i veth-gm -S -m -q > "$OUT/gm.log" 2>&1 &
fi
GM_PID=$!

gm_state=""; gm_deadline=$((SECONDS + 20))
while [ "$SECONDS" -lt "$gm_deadline" ]; do
  kill -0 "$GM_PID" 2>/dev/null || { tail -n 20 "$OUT/gm.log" >&2; die "GM process exited early" 3; }
  gm_state=$(pmc_gm 'GET PORT_DATA_SET' | awk '/portState/ {print $2}')
  [ "$gm_state" = "MASTER" ] && break
  sleep 1
done
if [ "$gm_state" != "MASTER" ]; then
  tail -n 30 "$OUT/gm.log" >&2
  die "GM did not become MASTER (portState=$gm_state)" 3
fi
log "GM portState=MASTER"
pmc_gm 'GET PORT_DATA_SET'    > "$OUT/gm-port-dataset.txt" 2>&1
pmc_gm 'GET PARENT_DATA_SET'  > "$OUT/gm-parent-dataset.txt" 2>&1
pmc_gm 'GET CURRENT_DATA_SET' > "$OUT/gm-current-dataset.txt" 2>&1
GM_ID=$(awk '/grandmasterIdentity/ {print $2}' "$OUT/gm-parent-dataset.txt" | head -n 1)
if [ -z "$GM_ID" ]; then
  GM_ID=$(awk '/portIdentity/ {print $2}' "$OUT/gm-port-dataset.txt" | head -n 1 | sed 's/-1$//')
fi

# ---------- slave ----------
if [ -z "${PTP4L:-}" ] && [ -n "$GM_WRAPPER" ]; then
  PTP4L=/opt/khaen-rt-stack/ptp/current/bin/ptp4l
fi
[ -n "${PTP4L:-}" ] || die "slave binary unknown (pass --ptp4l too when using --gm-wrapper)"
log "starting slave in $NS_SLV: $PTP4L"
ip netns exec "$NS_SLV" "$PTP4L" -f "$SLAVE_CONF" -i veth-slv -S -m -q > "$OUT/slave.log" 2>&1 &
SLAVE_PID=$!

slv_state=""; lock_t0=$SECONDS; slv_deadline=$((SECONDS + 40)); lock_time=""
while [ "$SECONDS" -lt "$slv_deadline" ]; do
  kill -0 "$SLAVE_PID" 2>/dev/null || { tail -n 20 "$OUT/slave.log" >&2; die "slave exited early" 3; }
  slv_state=$(pmc_slv 'GET PORT_DATA_SET' | awk '/portState/ {print $2}')
  if [ "$slv_state" = "SLAVE" ]; then lock_time=$((SECONDS - lock_t0)); break; fi
  sleep 1
done
if [ "$slv_state" != "SLAVE" ]; then
  tail -n 30 "$OUT/slave.log" >&2
  die "slave did not lock (portState=$slv_state)" 3
fi
log "slave portState=SLAVE after ${lock_time}s"
pmc_slv 'GET PORT_DATA_SET'   > "$OUT/slave-port-dataset.txt" 2>&1
pmc_slv 'GET PARENT_DATA_SET' > "$OUT/slave-parent-dataset.txt" 2>&1

# ---------- sample offsets ----------
CSV="$OUT/offsets.csv"
echo "elapsed_s,offset_ns" > "$CSV"
start=$SECONDS
while [ $((SECONDS - start)) -lt "$DURATION" ]; do
  off=$(pmc_slv 'GET CURRENT_DATA_SET' | awk '/offsetFromMaster/ {print $2}' | head -n 1)
  [ -n "$off" ] && echo "$((SECONDS - start)),$off" >> "$CSV"
  sleep 1
done
pmc_slv 'GET CURRENT_DATA_SET' > "$OUT/slave-current-dataset.txt" 2>&1
pmc_slv 'GET PARENT_DATA_SET'   > "$OUT/slave-parent-dataset-end.txt" 2>&1

DATE_AFTER=$(date -u +%s.%N)
UP_AFTER=$(cut -d' ' -f1 /proc/uptime)

# ---------- summary ----------
python3 - "$OUT" "$TRANSPORT" "$DURATION" "$lock_time" "$MAX_ABS_NS" "$GM_ID" "$DATE_BEFORE" "$DATE_AFTER" "$UP_BEFORE" "$UP_AFTER" <<'PY'
import json, os, statistics, sys
out, transport, duration, lock_time, max_abs_ns, gm_id = sys.argv[1:7]
date_before, date_after, up_before, up_after = (float(x) for x in sys.argv[7:11])
max_abs_ns = int(max_abs_ns)
offs = []
with open(os.path.join(out, 'offsets.csv'), encoding='utf-8') as f:
    for line in f.readlines()[1:]:
        parts = line.strip().split(',')
        if len(parts) == 2 and parts[1] not in ('', 'nan'):
            try:
                offs.append(float(parts[1]))
            except ValueError:
                pass

def ds(text, key):
    for line in text.splitlines():
        s = line.strip().split()
        if s and s[0] == key and len(s) > 1:
            return s[1]
    return None

def read(name):
    p = os.path.join(out, name)
    return open(p, encoding='utf-8', errors='replace').read() if os.path.isfile(p) else ''

slv_port = read('slave-port-dataset.txt')
slv_par = read('slave-parent-dataset.txt')
slv_cur = read('slave-current-dataset.txt')
gm_port = read('gm-port-dataset.txt')
gm_par = read('gm-parent-dataset.txt')
gm_cur = read('gm-current-dataset.txt')
stats = {}
if offs:
    absv = [abs(x) for x in offs]
    stats = {
        'count': len(offs),
        'mean_ns': round(statistics.fmean(offs), 1),
        'mean_abs_ns': round(statistics.fmean(absv), 1),
        'std_ns': round(statistics.pstdev(offs), 1) if len(offs) > 1 else 0.0,
        'min_ns': round(min(offs), 1),
        'max_ns': round(max(offs), 1),
        'p95_abs_ns': round(sorted(absv)[max(0, int(0.95 * len(absv)) - 1)], 1),
    }
rt_elapsed = date_after - date_before
mono_elapsed = up_after - up_before
clock_jump_s = abs(rt_elapsed - mono_elapsed)
slave_gm = ds(slv_par, 'grandmasterIdentity')
checks = {
    'gm_port_state_master': ds(gm_port, 'portState') == 'MASTER',
    'slave_port_state_slave': ds(slv_port, 'portState') == 'SLAVE',
    'slave_steps_removed_1': ds(slv_cur, 'stepsRemoved') in ('1', '1.0'),
    'gm_identity_match': (slave_gm is not None and gm_id is not None and slave_gm == gm_id),
    'offset_samples_ge_50pct': len(offs) >= 0.5 * int(duration),
    'clock_not_stepped': clock_jump_s < 0.05,
}
if stats:
    checks['offset_mean_abs_within_threshold'] = stats['mean_abs_ns'] <= max_abs_ns
    checks['offset_p95_within_threshold'] = stats['p95_abs_ns'] <= max_abs_ns
else:
    checks['offset_mean_abs_within_threshold'] = False
    checks['offset_p95_within_threshold'] = False
summary = {
    'schema': 'khaen-ptp/lab-gm-slave/v1',
    'transport': transport,
    'duration_s': int(duration),
    'lock_time_s': int(lock_time or 0),
    'gm': {
        'port_state': ds(gm_port, 'portState'),
        'port_identity': ds(gm_port, 'portIdentity'),
        'grandmaster_identity': ds(gm_par, 'grandmasterIdentity'),
        'clock_class': ds(gm_par, 'gm.ClockClass') or ds(gm_par, 'clockClass'),
        'priority1': ds(gm_par, 'grandmasterPriority1'),
        'steps_removed': ds(gm_cur, 'stepsRemoved'),
    },
    'slave': {
        'port_state': ds(slv_port, 'portState'),
        'parent_port_identity': ds(slv_par, 'parentPortIdentity'),
        'grandmaster_identity': slave_gm,
        'steps_removed': ds(slv_cur, 'stepsRemoved'),
    },
    'offset_ns': stats,
    'thresholds': {'max_abs_ns': max_abs_ns},
    'clock_guard': {'realtime_delta_s': round(rt_elapsed, 3), 'monotonic_delta_s': round(mono_elapsed, 3), 'jump_s': round(clock_jump_s, 3)},
    'checks': checks,
    'ok': all(checks.values()),
}
with open(os.path.join(out, 'summary.json'), 'w', encoding='utf-8') as f:
    json.dump(summary, f, ensure_ascii=False, indent=1)
print(json.dumps(summary, ensure_ascii=False))
sys.exit(0 if summary['ok'] else 3)
PY
RC=$?
log "summary: $OUT/summary.json (rc=$RC)"
exit "$RC"
