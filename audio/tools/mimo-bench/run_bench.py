#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""x86 原型 benchmark：CamillaDSP N×M 全矩阵 FIR 展开的实测 CPU/实时余量。
无 ALSA 硬件依赖：capture=WavFile，playback=File(/dev/null)。
用法：python3 run_bench.py --dir DIR --camilladsp /path/camilladsp-x86 --cases preset
输出 results.csv + summary.md
"""
import argparse, csv, io, json, math, os, struct, subprocess, sys, wave

CASES = [
    ("12x10@48k", 12, 10, 48000),
    ("12x10@96k", 12, 10, 96000),
    ("16x10@48k", 16, 10, 48000),
    ("2x10@48k", 2, 10, 48000),
]
TAPS = [512, 2048, 4096]


def wav_multi(path, channels, fs, seconds, dtype=32):
    n = int(fs * seconds)
    with wave.open(path, "wb") as f:
        f.setnchannels(channels)
        f.setsampwidth(dtype // 8)
        f.setframerate(fs)
        buf = bytearray()
        for i in range(n):
            v = int(0.1 * 32767 * math.sin(2 * math.pi * 1000.0 * i / fs))
            for _ in range(channels):
                if dtype == 32:
                    buf += struct.pack("<i", v << 16)
                else:
                    buf += struct.pack("<h", v)
        f.writeframes(bytes(buf))


def wav_coeff(path, taps, fs, lp_hz=None):
    if lp_hz:
        fc = lp_hz / fs
        w = []
        for i in range(taps):
            n = i - (taps - 1) / 2.0
            s = 2 * fc if n == 0 else math.sin(2 * math.pi * fc * n) / (math.pi * n)
            win = 0.5 - 0.5 * math.cos(2 * math.pi * i / max(1, taps - 1))
            w.append(s * win)
        sm = sum(w) or 1.0
        data = [int(max(-1.0, min(1.0, x / sm)) * 32767) for x in w]
    else:
        data = [0] * taps
        data[0] = 32767
    with wave.open(path, "wb") as f:
        f.setnchannels(1); f.setsampwidth(2); f.setframerate(fs)
        f.writeframes(b"".join(struct.pack("<h", x) for x in data))


def build_cfg(out, n, m, taps, fs, chunksize, input_wav, bandlimit):
    import yaml
    coeff = os.path.join(out, "coeff_%d_%s.wav" % (taps, "lp" if bandlimit else "imp"))
    if not os.path.isfile(coeff):
        wav_coeff(coeff, taps, fs, 300.0 if bandlimit else None)
    filters = {"f_%d_%d" % (i, j): {"type": "Conv",
               "parameters": {"type": "Wav", "filename": coeff}}
               for i in range(n) for j in range(m)}
    split = {"channels": {"in": n, "out": n * m},
             "mapping": [{"dest": i * m + j, "sources": [{"channel": i}]}
                         for i in range(n) for j in range(m)]}
    summ = {"channels": {"in": n * m, "out": m},
            "mapping": [{"dest": j, "sources": [{"channel": i * m + j} for i in range(n)]}
                        for j in range(m)]}
    cfg = {"devices": {
        "samplerate": fs, "chunksize": chunksize, "target_level": chunksize * 4,
        "adjust_period": 10, "enable_rate_adjust": False, "queuelimit": 4,
        "capture": {"type": "WavFile", "filename": input_wav},
        "playback": {"type": "File", "channels": m,
                     "filename": os.path.join(out, "out.raw"), "format": "S32_LE",
                     "wav_header": False}},
        "filters": filters, "mixers": {"split": split, "sum": summ},
        "pipeline": [
            {"type": "Mixer", "name": "split"},
            {"type": "Filter", "description": "%dx%d FIR" % (n, m),
             "channels": list(range(n * m)),
             "names": ["f_%d_%d" % (i, j) for i in range(n) for j in range(m)]},
            {"type": "Mixer", "name": "sum"}]}
    p = os.path.join(out, "cfg_%dx%d_%d_%d.yml" % (n, m, taps, fs))
    with open(p, "w", encoding="utf-8") as f:
        yaml.safe_dump(cfg, f, sort_keys=False, allow_unicode=True)
    return p


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--camilladsp", required=True)
    ap.add_argument("--seconds", type=float, default=10.0)
    ap.add_argument("--chunksize", type=int, default=64)
    ap.add_argument("--bandlimit", action="store_true")
    a = ap.parse_args()
    os.makedirs(a.dir, exist_ok=True)
    rows = []
    for name, n, m, fs in CASES:
        inp = os.path.join(a.dir, "in_%dch_%d.wav" % (n, fs))
        if not os.path.isfile(inp):
            wav_multi(inp, n, fs, a.seconds)
        for taps in TAPS:
            cfg = build_cfg(a.dir, n, m, taps, fs, a.chunksize, inp, a.bandlimit)
            log = os.path.join(a.dir, "run_%s_%d.log" % (name, taps))
            cmd = [a.camilladsp, "-l", "warn", cfg]
            t0 = os.times()
            with open(log, "w") as lf:
                p = subprocess.run(cmd, stdout=lf, stderr=subprocess.STDOUT, timeout=600)
            t1 = os.times()
            cpu = (t1.children_user - t0.children_user) + (t1.children_system - t0.children_system)
            audio_s = a.seconds * (n and 1) if False else a.seconds
            pct = 100.0 * cpu / audio_s
            xr = open(log, errors="replace").read().count("underrun") + \
                 open(log, errors="replace").read().count("overrun")
            npaths = n * m
            dg = 2.0 * npaths * taps * fs / 1e9
            rows.append({"case": name, "n": n, "m": m, "paths": npaths, "fs": fs,
                         "taps": taps, "chunksize": a.chunksize,
                         "cpu_s": round(cpu, 2), "cpu_pct_one_core": round(pct, 1),
                         "xruns": xr, "rc": p.returncode, "direct_gflops": round(dg, 1),
                         "group_delay_ms": round((taps / 2.0) / fs * 1000.0, 1)})
            print(rows[-1])
    out_csv = os.path.join(a.dir, "results.csv")
    with open(out_csv, "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0].keys()))
        w.writeheader(); w.writerows(rows)
    print("csv:", out_csv)


if __name__ == "__main__":
    main()
