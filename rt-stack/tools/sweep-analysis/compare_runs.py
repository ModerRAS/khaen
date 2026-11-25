#!/usr/bin/env python3
"""task-227 expected-vs-measured comparison across take paths (numpy).

task-229 fix (2026-11-18): IR 直达声列不再叫 ir_direct_delay_ms / "IR direct ms"；
改为 ir_relative_to_segment_ms（段内相对偏移，只可段内对齐/相对比较）+ 明确的
distance 结论（status/distance_m，无公共 t0 时为 INVALID_NO_COMMON_TIMEBASE）。
旧字段 delay_ms_from_segment_start 不得再当绝对直达延迟/距离使用。

Reads analysis-summary.json (produced by analyze_sweep.py) and compares:
  - board-ALSA aplay path UMIK vs rt-host file-input path UMIK (same room/speaker)
  - UMIK vs MA1 (different mic positions, same take)
Writes comparison.json + combined FR SVG + markdown table.
"""
import json
import math
import os
import sys

import numpy as np

FS = 48000
BANDS_CMP = np.array([100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0, 1000.0,
                      1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0])

COLORS = ["#1f77b4", "#ff7f0e", "#2ca02c", "#d62728"]


def fr_at(run, freqs):
    g = np.array(run["fr_third_oct"]["freq_hz"], dtype=np.float64)
    d = np.array([np.nan if v is None else v for v in run["fr_third_oct"]["db"]], dtype=np.float64)
    return np.interp(freqs, g, d)


def fr(ir):
    pass


def main():
    root = sys.argv[1]
    out = sys.argv[2]
    os.makedirs(out, exist_ok=True)
    os.makedirs(os.path.join(out, "plots"), exist_ok=True)
    s = json.load(open(os.path.join(root, "analysis-summary.json"), encoding="utf-8"))
    runs = {}
    for r in s["runs"]:
        runs[(r["tag"], r["mic"])] = r
    curves = {}
    for k, r in runs.items():
        curves[k] = fr_at(r, BANDS_CMP)
    comp = {
        "schema": "khaen-sweep-comparison/v2",
        "field_semantics": {
            "ir_relative_to_segment_ms": "IR 提取段内相对峰值位置；只能段内对齐/相对比较，不是绝对直达延迟",
            "ir_distance_status": "VALID / SUSPECT_TOO_CLOSE_FOR_ROOM_MIC / INVALID_NO_COMMON_TIMEBASE / INVALID_TIMEBASE / INVALID_NEGATIVE_DELAY / INVALID_SPEED_OF_SOUND",
            "ir_distance_m": "仅 VALID 可用；其余状态必须视为不可用（无公共 t0 一律 null）",
        },
        "bands_hz": [float(x) for x in BANDS_CMP],
        "pairs": [],
        "runs": {},
        "verification": [],
    }
    for k, r in runs.items():
        comp["runs"]["%s/%s" % k] = {
            "fr_100_8k_db": [round(float(v), 2) for v in curves[k]],
            "rt60_500_1k_2k_s": [r["rt_octave"][str(b)]["rt60_s"] for b in (500, 1000, 2000)],
            "xcorr": r["xcorr"],
            "ir": r["ir"],
            "levels": r["levels"],
            "fr_third_oct": r["fr_third_oct"],
            "rt_octave": r["rt_octave"],
        }
        comp["verification"].append({
            "run": "%s/%s" % k,
            "onset_ms_in_recording": r["xcorr"]["onset_ms_in_recording"],
            "expected_onset_ms": r["xcorr"]["expected_onset_ms"],
            "onset_dev_ms": r["xcorr"]["onset_dev_ms"],
            "norm_xcorr_coef": r["xcorr"]["norm_coef"],
            "bandlimited_snr_db": r["xcorr"]["bandlimited_snr_db_100_8000"],
            "ir_relative_to_segment_ms": r["ir"]["direct_path"]["relative_to_segment"]["delay_ms"],
            "ir_timebase_source": r["ir"]["direct_path"]["timebase"]["source"],
            "ir_distance_status": r["ir"]["direct_path"]["distance"]["status"],
            "ir_distance_m": r["ir"]["direct_path"]["distance"]["distance_m"],
            "ir_peak_to_noise_db": r["ir"]["peak_to_noise_db"],
            "polarity": r["ir"]["polarity"],
            "clipped_samples": r["levels"]["clipped_samples"],
        })
    pairs = [("board-aplay", "UMIK", "host-fileinput", "UMIK"),
             ("board-aplay", "UMIK", "board-aplay", "MA1")]
    for a, b, c, d in pairs:
        ka, kb = (a, b), (c, d)
        if ka not in curves or kb not in curves:
            continue
        x, y = curves[ka], curves[kb]
        m = ~np.isnan(x) & ~np.isnan(y)
        dvec = (x[m] - y[m])
        rms = float(np.sqrt(np.mean(dvec ** 2)))
        corr = float(np.corrcoef(x[m], y[m])[0, 1]) if m.sum() > 2 else None
        comp["pairs"].append({
            "a": "/".join(ka), "b": "/".join(kb),
            "bands": int(m.sum()),
            "rms_diff_db": round(rms, 2),
            "max_abs_diff_db": round(float(np.max(np.abs(dvec))), 2),
            "mean_diff_db": round(float(np.mean(dvec)), 2),
            "pearson_r": None if corr is None else round(corr, 3),
        })
    json.dump(comp, open(os.path.join(out, "comparison.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    # combined FR svg
    w, h = 900, 540
    lx0, lx1, ly0, ly1 = 70, w - 20, 40, h - 50
    ymin, ymax = -30.0, 10.0
    lxmin, lxmax = math.log10(20.0), math.log10(20000.0)

    def xp(f):
        return lx0 + (math.log10(f) - lxmin) / (lxmax - lxmin) * (lx1 - lx0)

    def yp(dd):
        return ly0 + (ymax - max(ymin, min(ymax, dd))) / (ymax - ymin) * (ly1 - ly0)

    out_svg = ['<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" viewBox="0 0 %d %d">' % (w, h, w, h),
               '<rect width="100%" height="100%" fill="#ffffff"/>',
               '<text x="10" y="18" font-family="sans-serif" font-size="14" fill="#111">Expected-vs-measured FR (1/3 oct, mic cal applied; expected = ideal flat 0 dB)</text>',
               '<line x1="%d" y1="%d" x2="%d" y2="%d" stroke="#333"/>' % (lx0, ly1, lx1, ly1),
               '<line x1="%d" y1="%d" x2="%d" y2="%d" stroke="#333"/>' % (lx0, ly0, lx0, ly1)]
    for f in (20, 30, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000):
        out_svg.append('<line x1="%.1f" y1="%d" x2="%.1f" y2="%d" stroke="#eee"/>' % (xp(f), ly0, xp(f), ly1))
        out_svg.append('<text x="%.1f" y="%d" font-family="sans-serif" font-size="10" fill="#666" text-anchor="middle">%d</text>' % (xp(f), ly1 + 14, f))
    for dd in range(int(ymin), int(ymax) + 1, 5):
        out_svg.append('<line x1="%d" y1="%.1f" x2="%d" y2="%.1f" stroke="#eee"/>' % (lx0, yp(dd), lx1, yp(dd)))
        out_svg.append('<text x="%d" y="%.1f" font-family="sans-serif" font-size="10" fill="#666" text-anchor="end">%d</text>' % (lx0 - 6, yp(dd) + 3, dd))
    out_svg.append('<line x1="%d" y1="%.1f" x2="%d" y2="%.1f" stroke="#888" stroke-dasharray="4,3"/>' % (lx0, yp(0.0), lx1, yp(0.0)))
    for i, (k, r) in enumerate(runs.items()):
        g = np.array(r["fr_third_oct"]["freq_hz"], dtype=np.float64)
        d = np.array([np.nan if v is None else v for v in r["fr_third_oct"]["db"]], dtype=np.float64)
        pts = ["%.1f,%.1f" % (xp(f), yp(v)) for f, v in zip(g, d) if v == v]
        out_svg.append('<polyline points="%s" fill="none" stroke="%s" stroke-width="1.5"/>' % (" ".join(pts), COLORS[i % len(COLORS)]))
        out_svg.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" fill="%s">%s / %s</text>' % (lx1 - 240, ly0 + 14 + i * 14, COLORS[i % len(COLORS)], k[0], k[1]))
    out_svg.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" fill="#333" text-anchor="middle">frequency (Hz)</text>' % ((lx0 + lx1) // 2, h - 12))
    out_svg.append('</svg>')
    with open(os.path.join(out, "plots", "fr-compare.svg"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(out_svg))
    # markdown table
    lines = ["# task-227 comparison (expected vs measured)", "",
             "> task-229 口径：IR 列是提取段内相对偏移（ir_relative_to_segment_ms），**不是**绝对直达延迟；",
             "> 距离只在存在公共时基（--timebase-source + --t0-sample）时才可能 VALID，本次数据为 INVALID_NO_COMMON_TIMEBASE。",
             "",
             "| run | onset ms | expected ms | dev ms | xcorr coef | BL SNR dB | IR seg-rel ms | IR distance | IR p/n dB | polarity | clipped |",
             "|---|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|"]
    for v in comp["verification"]:
        distance_text = v["ir_distance_status"]
        if v["ir_distance_m"] is not None and distance_text == "VALID":
            distance_text = "%.3f m VALID" % v["ir_distance_m"]
        lines.append("| %s | %.1f | %s | %s | %.3f | %.1f | %.2f | %s | %.1f | %d | %d |" % (
            v["run"], v["onset_ms_in_recording"],
            "" if v["expected_onset_ms"] is None else "%.1f" % v["expected_onset_ms"],
            "" if v["onset_dev_ms"] is None else "%.1f" % v["onset_dev_ms"],
            v["norm_xcorr_coef"], v["bandlimited_snr_db"], v["ir_relative_to_segment_ms"],
            distance_text, v["ir_peak_to_noise_db"], v["polarity"], v["clipped_samples"]))
    lines += ["", "| pair | bands | RMS diff dB | max abs diff dB | mean diff dB | Pearson r |", "|---|---:|---:|---:|---:|---:|"]
    for p in comp["pairs"]:
        lines.append("| %s vs %s | %d | %.2f | %.2f | %.2f | %s |" % (
            p["a"], p["b"], p["bands"], p["rms_diff_db"], p["max_abs_diff_db"], p["mean_diff_db"],
            "" if p["pearson_r"] is None else "%.3f" % p["pearson_r"]))
    lines += ["", "| run | RT60 500 Hz | RT60 1 kHz | RT60 2 kHz |", "|---|---:|---:|---:|"]
    for k, r in runs.items():
        lines.append("| %s / %s | %s | %s | %s |" % (k[0], k[1],
            r["rt_octave"]["500"]["rt60_s"], r["rt_octave"]["1000"]["rt60_s"], r["rt_octave"]["2000"]["rt60_s"]))
    with open(os.path.join(out, "COMPARISON.md"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    main()