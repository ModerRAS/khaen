#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""motu_meter_compare.py -- compare MOTU meter recordings from silent DSP-chain runs.

Inputs are the <TAG>.meters.jsonl files written by motu_meters.py (--all) plus the
<TAG>.host.json report of tspi-rt-host.  Produces, per run:

  * per-output-meter steady-window peak (dBFS) and median,
  * host report essentials (blocks / capture counters / ALSA xruns / engine / plan),
  * a markdown table + JSON suitable for evidence/report.

Meter frames are peak values in 0.5 dB steps (dB = -raw/2) sent by the device at ~20 Hz.
The meter tap is pre-trim (CueMix dev.js: "meters are pre trim"), i.e. it shows DSP output
even when the DAC outputs are attenuated/muted -- which is what makes a silent A/B possible.

Usage:
  python3 motu_meter_compare.py --tag identity=<dir> --tag plan=<dir> [--json out.json]
Each <dir> contains <TAG>.meters.jsonl and <TAG>.host.json (or pass explicit files).
"""
import argparse
import json
import math
import os
import sys

OUTPUT_ORDER = ["Main1", "Main2", "Line3", "Line4", "Line5", "Line6",
                "Line7", "Line8", "Line9", "Line10", "Phones1", "Phones2"]


def load_meters(path):
    rows = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            if rec.get("kind") == "meters":
                rows.append(rec)
    return rows


def steady_stats(rows, skip_s=0.7):
    """Return {meter: {'peak_db','median_db','frames_signal'}} over the steady window."""
    if not rows:
        return {}, 0.0
    t0, t1 = rows[0]["t"], rows[-1]["t"]
    lo, hi = t0 + skip_s, t1 - skip_s
    sel = [r for r in rows if lo <= r["t"] <= hi] or rows
    out = {}
    for name in OUTPUT_ORDER:
        vals = [r["db"].get(name) for r in sel]
        good = [v for v in vals if v is not None]
        out[name] = {
            "peak_db": max(good) if good else None,
            "median_db": (sorted(good)[len(good) // 2] if good else None),
            "frames_signal": len(good),
        }
    return out, (sel[-1]["t"] - sel[0]["t"]) if sel else 0.0


def load_host(path):
    try:
        with open(path, encoding="utf-8") as fh:
            d = json.load(fh)
    except Exception as exc:
        return {"error": str(exc)}
    cap = d.get("capture") or {}
    fault = d.get("fault") or {}
    alsa = d.get("alsa") or {}
    plan = d.get("plan") or {}
    return {
        "status": d.get("status"),
        "graph": d.get("graph"),
        "blocks": d.get("blocks"),
        "graceful": d.get("graceful"),
        "capture_transport": cap.get("transport"),
        "capture_data_blocks": cap.get("data_blocks"),
        "capture_underrun_blocks": cap.get("underrun_blocks"),
        "capture_stall_events": cap.get("stall_events"),
        "capture_loss_events": cap.get("loss_events"),
        "capture_pacing": cap.get("pacing"),
        "engine": d.get("engine"),
        "plan_sha256": plan.get("sha256"),
        "plan_graph_id": plan.get("graph_id"),
        "plan_form": plan.get("form"),
        "alsa": alsa,
        "deadline_misses": fault.get("deadline_misses"),
        "swap": d.get("swap"),
    }


def fmt_db(v):
    return "-inf" if v is None else "%.1f" % v


def main(argv=None):
    ap = argparse.ArgumentParser(description="compare MOTU meter JSONL recordings from DSP-chain runs")
    ap.add_argument("--tag", action="append", default=[], metavar="NAME=DIR",
                    help="run tag and its evidence dir (may repeat)")
    ap.add_argument("--skip", type=float, default=0.7,
                    help="seconds to drop at both ends of each recording")
    ap.add_argument("--json", default=None, help="write machine-readable summary JSON")
    args = ap.parse_args(argv)
    if not args.tag:
        print(__doc__)
        return 2

    summary = {}
    for spec in args.tag:
        name, _, path = spec.partition("=")
        if not path:
            path = name
        meters_path = os.path.join(path, "%s.meters.jsonl" % name)
        host_path = os.path.join(path, "%s.host.json" % name)
        rows = load_meters(meters_path)
        stats, span = steady_stats(rows, args.skip)
        summary[name] = {
            "meters_file": meters_path,
            "host_file": host_path,
            "frames": len(rows),
            "steady_span_s": span,
            "meters": stats,
            "host": load_host(host_path),
        }

    # Markdown table
    names = list(summary.keys())
    print("| meter | " + " | ".join("%s peak dBFS" % n for n in names) + " |")
    print("|---|" + "---|" * len(names))
    for m in OUTPUT_ORDER:
        cells = []
        for n in names:
            cells.append(fmt_db(summary[n]["meters"].get(m, {}).get("peak_db")))
        print("| %s | %s |" % (m, " | ".join(cells)))
    print()
    # deltas vs first run for the DSP comparison
    if len(names) >= 2:
        base = names[0]
        for n in names[1:]:
            print("delta %s - %s (dB):" % (n, base))
            for m in OUTPUT_ORDER:
                a = summary[base]["meters"].get(m, {}).get("peak_db")
                b = summary[n]["meters"].get(m, {}).get("peak_db")
                if a is None or b is None:
                    continue
                print("  %-8s %+6.1f dB" % (m, b - a))

    for n in names:
        h = summary[n]["host"]
        print("\n[%s] host: status=%s graph=%s blocks=%s graceful=%s" %
              (n, h.get("status"), h.get("graph"), h.get("blocks"), h.get("graceful")))
        print("  capture transport=%s data_blocks=%s underrun=%s stall=%s loss=%s pace=%s" % (
            h.get("capture_transport"), h.get("capture_data_blocks"), h.get("capture_underrun_blocks"),
            h.get("capture_stall_events"), h.get("capture_loss_events"), h.get("capture_pacing")))
        al = h.get("alsa") or {}
        if isinstance(al, dict) and al.get("backend") == "linux-alsa":
            print("  alsa: blocks=%s xruns=%s underruns=%s recoveries=%s faults=%s | period=%s buffer=%s actual_in=%s actual_out=%s" % (
                al.get("blocks"), al.get("xruns"), al.get("underruns"), al.get("recoveries"),
                al.get("faults"), al.get("period_frames"), al.get("buffer_frames"),
                (al.get("actual") or {}).get("n_in"), (al.get("actual") or {}).get("n_out")))
        print("  deadline_misses=%s engine=%s plan_graph_id=%s" % (
            h.get("deadline_misses"), h.get("engine"), (h.get("plan_graph_id") or "")[:16]))
    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(summary, fh, indent=1, ensure_ascii=False)
        print("\n[json] wrote %s" % args.json)
    return 0


if __name__ == "__main__":
    sys.exit(main())
