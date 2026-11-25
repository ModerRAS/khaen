#!/bin/bash
# KHAEN PTP slave-side offset/stability sampler (pmc based; no root needed).
#
# Samples a running linuxptp slave through its pmc UDS: CURRENT_DATA_SET.offsetFromMaster
# (raw PTP measurement, independent of the slave servo) plus PORT_DATA_SET.portState, once per
# second for DURATION seconds, then writes summary.json/offsets.csv with statistics including a
# linear drift estimate (ppb between the slave local clock and the GM time base).
#
# Usage: measure-slave-offset.sh --uds PATH --pmc PATH --duration SEC --out DIR
#                                [--interval SEC] [--tag NAME]
# Exit: 0 always when enough samples were collected (stability is reported, not gated); 2 usage.
set -uo pipefail

UDS=""; PMC=""; DURATION=600; OUT=""; INTERVAL=1; TAG="slave"
usage() { sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//' >&2; }
die() { echo "[offset-sampler][error] $*" >&2; exit 2; }
log() { echo "[offset-sampler $(date +%H:%M:%S)] $*" >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --uds) UDS="$2"; shift 2 ;;
    --pmc) PMC="$2"; shift 2 ;;
    --duration) DURATION="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --interval) INTERVAL="$2"; shift 2 ;;
    --tag) TAG="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done
[ -n "$UDS" ] || die "--uds required"
[ -n "$PMC" ] || die "--pmc required"
[ -x "$PMC" ] || die "pmc not executable: $PMC"
[ -n "$OUT" ] || die "--out required"
mkdir -p "$OUT" || die "cannot create $OUT"
CSV="$OUT/offsets.csv"
echo "elapsed_s,offset_ns,port_state" > "$CSV"
date -u +%Y-%m-%dT%H:%M:%SZ > "$OUT/started_at.txt"
start=$SECONDS
while [ $((SECONDS - start)) -lt "$DURATION" ]; do
  cur=$("$PMC" -s "$UDS" -u -b 0 'GET CURRENT_DATA_SET' 2>/dev/null)
  port=$("$PMC" -s "$UDS" -u -b 0 'GET PORT_DATA_SET' 2>/dev/null)
  off=$(printf '%s\n' "$cur" | awk '/offsetFromMaster/ {print $2; exit}')
  st=$(printf '%s\n' "$port" | awk '/portState/ {print $2; exit}')
  [ -n "$off" ] && echo "$((SECONDS - start)),$off,${st:-?}" >> "$CSV"
  sleep "$INTERVAL"
done
"$PMC" -s "$UDS" -u -b 0 'GET PARENT_DATA_SET' > "$OUT/slave-parent-dataset.txt" 2>&1 || true
"$PMC" -s "$UDS" -u -b 0 'GET CURRENT_DATA_SET' > "$OUT/slave-current-dataset.txt" 2>&1 || true
python3 - "$OUT" "$TAG" "$INTERVAL" <<'PY'
import json, os, statistics, sys
out, tag, interval = sys.argv[1], sys.argv[2], float(sys.argv[3])
offs, states, xs = [], [], []
with open(os.path.join(out, 'offsets.csv'), encoding='utf-8') as f:
    for line in f.readlines()[1:]:
        p = line.strip().split(',')
        if len(p) == 3:
            states.append(p[2])
            try:
                offs.append(float(p[1]))
                xs.append(float(p[0]))
            except ValueError:
                pass
stats = {}
if offs:
    absv = [abs(x) for x in offs]
    n = len(offs)
    mx = sum(xs) / n
    my = sum(offs) / n
    sxx = sum((x - mx) ** 2 for x in xs)
    sxy = sum((x - mx) * (y - my) for x, y in zip(xs, offs))
    slope_ns_per_s = sxy / sxx if sxx else 0.0
    # Detrended residuals isolate the PTP measurement jitter from the constant clock-rate skew
    # between the two (virtual) machines; raw std over a 10-30 min run is dominated by that skew.
    resid = [y - (my + slope_ns_per_s * (x - mx)) for x, y in zip(xs, offs)]
    absr = [abs(v) for v in resid]
    stats = {
        'count': n,
        'mean_ns': round(statistics.fmean(offs), 1),
        'mean_abs_ns': round(statistics.fmean(absv), 1),
        'std_ns': round(statistics.pstdev(offs), 1) if n > 1 else 0.0,
        'min_ns': round(min(offs), 1),
        'max_ns': round(max(offs), 1),
        'p95_abs_ns': round(sorted(absv)[max(0, int(0.95 * n) - 1)], 1),
        'first_ns': round(offs[0], 1),
        'last_ns': round(offs[-1], 1),
        'drift_slope_ns_per_s': round(slope_ns_per_s, 1),
        'drift_slope_ppb': round(slope_ns_per_s, 1),
        'drift_slope_ppm': round(slope_ns_per_s / 1000.0, 4),
        'detrended_std_ns': round(statistics.pstdev(resid), 1) if n > 1 else 0.0,
        'detrended_p95_abs_ns': round(sorted(absr)[max(0, int(0.95 * n) - 1)], 1),
        'detrended_max_abs_ns': round(max(absr), 1),
    }
summary = {
    'schema': 'khaen-ptp/slave-offset-sampler/v1',
    'tag': tag,
    'interval_s': interval,
    'port_state_counts': {s: states.count(s) for s in sorted(set(states))},
    'offset_ns': stats,
}
with open(os.path.join(out, 'summary.json'), 'w', encoding='utf-8') as f:
    json.dump(summary, f, ensure_ascii=False, indent=1)
print(json.dumps(summary, ensure_ascii=False))
sys.exit(0 if offs else 3)
PY
log "summary: $OUT/summary.json"
