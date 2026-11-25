#!/usr/bin/env python3
"""ir-diag: compute the G0 RAW time-domain baseline from a single-point MIMO capture.

Pure offline analysis: no playback, no device I/O.

    py -3 run_ir_diag.py --capture ... --meta ... --out ... [--composite-dir ...]

Inputs
------
--capture     capture.raw  (2ch interleaved S24_3LE @48k, UMIK)
--meta        directory with the *.diag.json / session.diag.json written by measure-multi
--composite-dir  optional: dd slices of composite.raw used to prove the excitation
                 reconstruction is byte-identical (see README.md).  Files expected:
                    fl_ch0.bin     composite frame 96000*,   channel 0  (FL sweep)
                    sub_ch7.bin    composite frame 4819200*, channel 7  (SUB1 sweep)
                    fl_ch0_s6.bin  composite frame 4819200*, channel 0  (must be silent)
--out         evidence directory (JSON/CSV/PNG/REPORT.md/ANALYSIS_MANIFEST.json)
"""
import argparse
import datetime
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import numpy as np  # noqa: E402

from irdiag import analysis as A  # noqa: E402
from irdiag import excitation as exc  # noqa: E402
from irdiag import pipeline  # noqa: E402
from irdiag import report  # noqa: E402

NCH_COMPOSITE = 22


def _load_composite_channel(path, nch, ch, n=48000):
    raw = np.fromfile(path, dtype=np.uint8)
    nf = raw.size // (3 * nch)
    frames = raw[:nf * 3 * nch].reshape(nf, nch, 3)[:n, ch, :].astype(np.int32)
    s = frames[:, 0] | (frames[:, 1] << 8) | (frames[:, 2] << 16)
    s = np.where(s & 0x800000, s - (1 << 24), s)
    return s / 8388608.0


def verify_composite(comp_dir):
    """Prove the rebuilt excitation == the bytes actually played (S24 quantised)."""
    out = []
    spec = [
        ("FL", "fl_ch0.bin", 0, 20.0, 20000.0, "signal"),
        ("SUB1", "sub_ch7.bin", 7, 20.0, 130.0, "signal"),
        ("FL@SUB1-slot", "fl_ch0_s6.bin", 0, 20.0, 20000.0, "silence"),
        ("SUB1@FL-slot", "fl_ch0.bin", 7, 20.0, 130.0, "silence"),
    ]
    for name, fn, ch, f0, f1, kind in spec:
        p = os.path.join(comp_dir, fn)
        if not os.path.exists(p):
            continue
        got = _load_composite_channel(p, NCH_COMPOSITE, ch)
        if kind == "silence":
            out.append({"name": name, "band": "must be silent", "max_err_lsb": 0.0,
                        "corr": 0.0, "max_abs": float(np.abs(got).max()),
                        "kind": kind, "pass": bool(np.abs(got).max() == 0.0)})
            continue
        ref = exc.gen_sweep(f0=f0, f1=f1)[:got.size]
        err = float(np.abs(got - ref).max() * 8388608.0)
        corr = float(np.corrcoef(got, ref)[0, 1])
        out.append({"name": name, "band": "%.0f-%.0f Hz" % (f0, f1),
                    "max_err_lsb": err, "corr": corr, "kind": kind,
                    "pass": bool(err < 1.0 and corr > 0.9999)})
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--capture", required=True)
    ap.add_argument("--meta", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--composite-dir", default=None)
    ap.add_argument("--decay-fit", action="store_true",
                    help="OPT-IN: also run tools/decay-fit and attach a non-standard "
                         "segmented (1/2/3 segment) decay description per band. "
                         "Default off; the frozen T20 verdicts are unchanged.")
    ap.add_argument("--decay-fit-bands", default=None,
                    help="optional comma list of band centres for --decay-fit "
                         "(default: all analysed bands)")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv)

    os.makedirs(args.out, exist_ok=True)
    decay_cfg = None
    if args.decay_fit:
        decay_cfg = {}
        if args.decay_fit_bands:
            decay_cfg["bands"] = [float(x) for x in args.decay_fit_bands.split(",") if x.strip()]
    result = pipeline.run(args.capture, args.meta, args.out, verbose=not args.quiet,
                          decay_fit=decay_cfg)
    result["generated_at"] = datetime.datetime.now().astimezone().isoformat(timespec="seconds")
    result["tool"] = {"name": "ir-diag", "version": "1.0.0"}

    checks = verify_composite(args.composite_dir) if args.composite_dir else []
    result["excitation"]["composite_verification"] = checks

    manifest, mpath = report.write_manifest(
        args.out, HERE, result["inputs"]["capture_sha256"],
        {"excitation_composite_check": checks,
         "capture_frames": result["inputs"]["capture_frames"],
         "clock_eps_ppm": result["clock"]["eps_ppm"]})
    result["analysis_manifest"] = {"path": os.path.basename(mpath),
                                   "parameter_hash": manifest["parameter_hash"]}
    jpath = report.write_json(result, args.out)
    report.write_csvs(result, args.out)
    figs = report.make_figures(result, args.out)
    rpath = report.write_report(result, manifest, args.out,
                               {"excitation_check": checks or [
                                   {"name": n, "band": "-", "max_err_lsb": float("nan"), "corr": float("nan")}
                                   for n in ("FL", "SUB1")]})
    if not args.quiet:
        print("\nwrote:")
        for p in [jpath, mpath, rpath] + figs:
            print("  " + p)
    return 0


if __name__ == "__main__":
    sys.exit(main())
