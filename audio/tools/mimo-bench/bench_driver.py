#!/usr/bin/env python3
# -*- coding: utf-8 -*-
import csv, os, resource, subprocess, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from run_bench import CASES, TAPS, wav_multi, build_cfg

def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "."
    cam = sys.argv[2] if len(sys.argv) > 2 else "./camilladsp"
    seconds = float(sys.argv[3]) if len(sys.argv) > 3 else 3.0
    os.makedirs(out, exist_ok=True)
    rows = []
    for name, n, m, fs in CASES:
        inp = os.path.join(out, "in_%dch_%d.wav" % (n, fs))
        if not os.path.isfile(inp):
            wav_multi(inp, n, fs, seconds)
        for taps in TAPS:
            cfg = build_cfg(out, n, m, taps, fs, 64, inp, False)
            log = os.path.join(out, "run_%s_%d.log" % (name, taps))
            r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
            with open(log, "w") as lf:
                p = subprocess.run([cam, "-l", "warn", cfg], stdout=lf, stderr=subprocess.STDOUT, timeout=300)
            r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
            cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
            txt = open(log, errors="replace").read()
            xr = txt.count("underrun") + txt.count("overrun")
            rows.append({"case": name, "n": n, "m": m, "paths": n * m, "fs": fs, "taps": taps,
                         "seconds": seconds, "cpu_s": round(cpu, 3),
                         "cpu_pct_one_core": round(100.0 * cpu / seconds, 1),
                         "xruns": xr, "rc": p.returncode,
                         "direct_gflops": round(2.0 * n * m * taps * fs / 1e9, 1),
                         "group_delay_ms": round((taps / 2.0) / fs * 1000.0, 1)})
            print(rows[-1], flush=True)
    with open(os.path.join(out, "results.csv"), "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0].keys()))
        w.writeheader(); w.writerows(rows)
    print("OK", out)

if __name__ == "__main__":
    main()
