#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""合并响应/相位抵消检查器（纯 stdlib；读频响 CSV）。

配套 CONTRACT.md / ACCEPTANCE.md §6。输入每通道 FR CSV：三列（表头可选）
  freq_hz, db, phase_deg
同一测量会话内得到（相位参考一致）才有效。

pair 口径（2026-11-17，C-15）：
  - pair = **同一源信号 + 互补分频（目标 LR4）的两个通道**；fc 必须等于 profile 中某 HP/LP 的频率；
  - effect channel（高度/环绕/中置等与 SUB 内容不同、或低频未重定向到 SUB 的通道）**不构成 pair**；
  - group 模式只用于 5.3.2 的多 SUB（30–80 Hz）；当前 4.1.2 只有 SUB1，不适用；
  - 输入默认是 direct 测量 + 模型的 **predicted** 频响；应用后复测（T3/T6）才是 measured。

用法：
  # 交叉对：H1/H2 在 fc 交叉
  python3 tools/check_pair_response.py --h1 FL-fr.csv --h2 SUB1-fr.csv --fc 80
  # 多低音组（2+ 文件）
  python3 tools/check_pair_response.py --files SUB1-fr.csv SUB2-fr.csv SUB3-fr.csv \
      --fc 60 --mode group --band-low 30 --band-high 80 --ripple-max 4
  # 机器可读
  ... --json
退出码：0 PASS；1 FAIL；2 用法/数据错误。
"""
import argparse
import csv
import json
import math
import sys

GRID_POINTS_PER_OCT = 24


def read_csv(path):
    rows = []
    with open(path, "r", encoding="utf-8", newline="") as f:
        rdr = csv.reader(f)
        for row in rdr:
            if not row or row[0].strip().startswith("#"):
                continue
            try:
                vals = [float(x) for x in row[:3]]
            except ValueError:
                continue  # 表头
            if len(vals) >= 2:
                phase = vals[2] if len(vals) >= 3 else 0.0
                rows.append((vals[0], vals[1], phase))
    if len(rows) < 8:
        raise ValueError("%s 有效数据点太少（%d）" % (path, len(rows)))
    rows.sort(key=lambda x: x[0])
    # 去重/保证严格递增
    out = []
    for r in rows:
        if out and abs(r[0] - out[-1][0]) < 1e-12:
            continue
        out.append(r)
    if len(out) < 8:
        raise ValueError("%s 频点不足" % path)
    return out


def interp(rows, f):
    if f <= rows[0][0]:
        return rows[0][1], rows[0][2]
    if f >= rows[-1][0]:
        return rows[-1][1], rows[-1][2]
    lo, hi = 0, len(rows) - 1
    while hi - lo > 1:
        mid = (lo + hi) // 2
        if rows[mid][0] <= f:
            lo = mid
        else:
            hi = mid
    f0, d0, p0 = rows[lo]
    f1, d1, p1 = rows[hi]
    t = (math.log(f) - math.log(f0)) / (math.log(f1) - math.log(f0)) if f1 > f0 else 0.0
    return d0 + (d1 - d0) * t, p0 + (p1 - p0) * t


def unwrap_deg(phases):
    out = list(phases)
    for i in range(1, len(out)):
        d = out[i] - out[i - 1]
        while d > 180.0:
            out[i] -= 360.0
            d = out[i] - out[i - 1]
        while d < -180.0:
            out[i] += 360.0
            d = out[i] - out[i - 1]
    return out


def wrap_deg(p):
    p = math.fmod(p, 360.0)
    if p > 180.0:
        p -= 360.0
    if p <= -180.0:
        p += 360.0
    return p


def smooth_db(values, points_per_oct, octaves=1.0 / 3.0):
    half = max(1, int(round(points_per_oct * octaves / 2.0)))
    n = len(values)
    out = []
    for i in range(n):
        a, b = max(0, i - half), min(n, i + half + 1)
        out.append(sum(values[a:b]) / (b - a))
    return out


def complex_at(db, deg):
    mag = 10.0 ** (db / 20.0)
    rad = math.radians(deg)
    return mag * math.cos(rad), mag * math.sin(rad)


def db_of(z):
    mag = math.hypot(z[0], z[1])
    return 20.0 * math.log10(max(mag, 1e-12))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--h1", default=None)
    ap.add_argument("--h2", default=None)
    ap.add_argument("--files", nargs="+", default=None, help="group 模式：多个 FR CSV")
    ap.add_argument("--mode", choices=["pair", "group"], default=None)
    ap.add_argument("--fc", type=float, required=True,
                    help="交叉/检查中心频率 Hz；pair 模式必须等于 profile 中该 pair 的 HP/LP 频率")
    ap.add_argument("--band-low", type=float, default=None, help="默认 fc/2")
    ap.add_argument("--band-high", type=float, default=None, help="默认 2*fc")
    ap.add_argument("--target-phase-deg", type=float, default=0.0)
    ap.add_argument("--phase-max", type=float, default=30.0)
    ap.add_argument("--phase-mean-max", type=float, default=45.0)
    ap.add_argument("--cd-min-fc", type=float, default=-3.0)
    ap.add_argument("--cd-min-band", type=float, default=-6.0)
    ap.add_argument("--notch-min", type=float, default=-6.0)
    ap.add_argument("--ripple-max", type=float, default=3.0)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()
    mode = args.mode or ("group" if args.files else "pair")
    if mode == "pair":
        files = [args.h1, args.h2]
        if not all(files):
            sys.stderr.write("pair 模式需要 --h1 与 --h2\n")
            return 2
    else:
        files = args.files or [f for f in (args.h1, args.h2) if f]
        if len(files) < 2:
            sys.stderr.write("group 模式需要 >=2 个文件（--files 或 --h1/--h2...）\n")
            return 2
    low = args.band_low if args.band_low else args.fc / 2.0
    high = args.band_high if args.band_high else args.fc * 2.0
    if not (0 < low < high):
        sys.stderr.write("频带非法：%s..%s\n" % (low, high))
        return 2
    try:
        rows = [read_csv(f) for f in files]
    except Exception as e:  # noqa: BLE001
        sys.stderr.write("读取 FR 失败：%s\n" % e)
        return 2

    # 1/24 oct 对数网格（裁剪到数据范围）
    f_min = max(low, max(r[0][0] for r in rows))
    f_max = min(high, min(r[-1][0] for r in rows))
    if f_max <= f_min:
        sys.stderr.write("数据覆盖不到频带 %s..%s\n" % (low, high))
        return 2
    n = max(32, int(round(GRID_POINTS_PER_OCT * math.log2(f_max / f_min)))) + 1
    freqs = [f_min * (f_max / f_min) ** (i / (n - 1)) for i in range(n)]
    hs = []
    for r in rows:
        # 先对原始 CSV 相位整体 unwrap（避免在相位 ±180° 跳变处做线性插值）
        raw_ph = unwrap_deg([x[2] for x in r])
        rr = [(x[0], x[1], ph) for x, ph in zip(r, raw_ph)]
        dbv = [interp(rr, f)[0] for f in freqs]
        ph = [interp(rr, f)[1] for f in freqs]
        hs.append([(d, p) for d, p in zip(dbv, ph)])

    # 公共相位参考：去掉两路/多路在 fc 的平均相位（只影响绝对相位，不影响 Δφ/CD）
    ref = sum(h[len(h) // 2][1] for h in hs) / len(hs)
    hs = [[(d, p - ref) for d, p in h] for h in hs]

    comps = [[complex_at(d, p) for d, p in h] for h in hs]
    if mode == "pair":
        h1, h2 = hs[0], hs[1]
        c1, c2 = comps[0], comps[1]
        s = [(a[0] + b[0], a[1] + b[1]) for a, b in zip(c1, c2)]
        # fc 处相位
        i_fc = min(range(n), key=lambda i: abs(math.log(freqs[i] / args.fc)))
        dphi = wrap_deg(h1[i_fc][1] - h2[i_fc][1] - args.target_phase_deg)
        # 加权平均相位差（两路都在各自带内峰值 -10 dB 以上）
        m1 = max(d for d, _ in h1)
        m2 = max(d for d, _ in h2)
        deltas = []
        for i in range(n):
            if h1[i][0] >= m1 - 10.0 and h2[i][0] >= m2 - 10.0:
                deltas.append(abs(wrap_deg(h1[i][1] - h2[i][1] - args.target_phase_deg)))
        mean_dphi = sum(deltas) / len(deltas) if deltas else 999.0
        # CD(f) = 20log10|S| - 20log10(|H1|+|H2|)
        cd = [db_of(s[i]) - 20.0 * math.log10(max(10 ** (h1[i][0] / 20.0) + 10 ** (h2[i][0] / 20.0), 1e-12))
              for i in range(n)]
        s_db = [db_of(z) for z in s]
        notch = min(s_db) - max(m1, m2)
        ripple = max(smooth_db(s_db, GRID_POINTS_PER_OCT)) - min(smooth_db(s_db, GRID_POINTS_PER_OCT))
        metrics = {
            "delta_phase_fc_deg": round(dphi, 2),
            "delta_phase_mean_deg": round(mean_dphi, 2),
            "cd_fc_db": round(cd[i_fc], 2),
            "cd_min_band_db": round(min(cd), 2),
            "notch_margin_db": round(notch, 2),
            "sum_ripple_db": round(ripple, 2),
        }
        checks = {
            "phase_fc": abs(dphi) <= args.phase_max,
            "phase_mean": mean_dphi <= args.phase_mean_max,
            "cd_fc": cd[i_fc] >= args.cd_min_fc,
            "cd_band": min(cd) >= args.cd_min_band,
            "notch": notch >= args.notch_min,
            "ripple": ripple <= args.ripple_max,
        }
    else:
        s = [(0.0, 0.0)] * n
        for c in comps:
            s = [(s[i][0] + c[i][0], s[i][1] + c[i][1]) for i in range(n)]
        # 两两相位差（带内加权平均的最大值）
        worst_pair, worst_mean = None, 0.0
        for a in range(len(hs)):
            for b in range(a + 1, len(hs)):
                m_a = max(d for d, _ in hs[a])
                m_b = max(d for d, _ in hs[b])
                ds = []
                for i in range(n):
                    if hs[a][i][0] >= m_a - 10.0 and hs[b][i][0] >= m_b - 10.0:
                        ds.append(abs(wrap_deg(hs[a][i][1] - hs[b][i][1])))
                if ds:
                    mean = sum(ds) / len(ds)
                    if mean > worst_mean:
                        worst_mean = mean
                        worst_pair = (a, b)
        s_db = [db_of(z) for z in s]
        ideal = 20.0 * math.log10(max(sum(10 ** (h[i][0] / 20.0) for h in hs) for i in range(n)))
        cd = [s_db[i] - 20.0 * math.log10(max(sum(10 ** (hs[k][i][0] / 20.0) for k in range(len(hs))), 1e-12))
              for i in range(n)]
        i_fc = min(range(n), key=lambda i: abs(math.log(freqs[i] / args.fc)))
        notch = min(s_db) - max(max(d for d, _ in h) for h in hs)
        sm = smooth_db(s_db, GRID_POINTS_PER_OCT)
        ripple = max(sm) - min(sm)
        metrics = {
            "phase_mean_worst_pair_deg": round(worst_mean, 2),
            "cd_fc_db": round(cd[i_fc], 2),
            "cd_min_band_db": round(min(cd), 2),
            "notch_margin_db": round(notch, 2),
            "sum_ripple_db": round(ripple, 2),
            "n_inputs": len(hs),
        }
        checks = {
            "phase_mean": worst_mean <= args.phase_mean_max,
            "cd_fc": cd[i_fc] >= args.cd_min_fc,
            "cd_band": min(cd) >= args.cd_min_band,
            "notch": notch >= args.notch_min,
            "ripple": ripple <= args.ripple_max,
        }
    ok = all(checks.values())
    out = {"schema": "khaen-calibration-pair-check/v1", "mode": mode, "files": files,
           "fc_hz": args.fc, "band_hz": [round(f_min, 1), round(f_max, 1)],
           "pass": ok, "checks": checks, "metrics": metrics}
    if args.json:
        print(json.dumps(out, ensure_ascii=False, indent=2))
    else:
        print(("PASS" if ok else "FAIL") + "  mode=%s fc=%.1f Hz band=%.0f-%.0f Hz"
              % (mode, args.fc, f_min, f_max))
        for k, v in metrics.items():
            print("  %-28s %s" % (k, v))
        for k, v in checks.items():
            print("  %-28s %s" % (k, "PASS" if v else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
