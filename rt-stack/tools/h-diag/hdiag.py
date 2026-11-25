
#!/usr/bin/env python3
"""h-diag command line: complex H-matrix diagnostics for the 4.1.2 low-band MIMO set.

Read-only: no solver changes, no board access, no sound.  See README.md.

Usage
-----
  python hdiag.py run --manifest <dir>/positions.json --out-dir <dir>
  python hdiag.py run --manifest ... --after-manifest ...   # measured after set
  python hdiag.py selftest --out-dir <dir>
  python hdiag.py check-manifest --manifest <dir>/positions.json
  python hdiag.py version

Exit codes (same style as the repo provenance guards):
  0  diagnostics produced and conclusions allowed (MEASURED + CONSISTENT + complete)
  3  conclusions withheld (UNPROVEN / SYNTHETIC_REFUSED / INVALID_MEASUREMENT /
     INCONSISTENT / INCOMPLETE); hdiag.json + REPORT.md are still written
  2  usage/input error
  1  unexpected failure
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

import numpy as np

from hdiag import guards as G
from hdiag import measure as M
from hdiag import metrics as X
from hdiag import report as R
from hdiag import selftest as T


def _git_commit():
    try:
        out = subprocess.run(["git", "-C", G.repo_root(), "rev-parse", "--short", "HEAD"],
                             capture_output=True, text=True, timeout=20)
        if out.returncode == 0:
            return out.stdout.strip()
    except Exception:
        pass
    return None


def _parse_float_list(text, default):
    if not text:
        return list(default)
    values = []
    for chunk in str(text).replace(";", ",").split(","):
        chunk = chunk.strip()
        if not chunk:
            continue
        values.append(float(chunk))
    return values or list(default)


def _resolve_freq_field(manifest, analyses, requested, freq_range):
    want = requested
    if want in (None, "auto") and manifest.get("freq_field"):
        want = manifest["freq_field"]
    if want not in ("fr", "fr_modal"):
        want = "auto"
    field = M.pick_freq_field([rec for rec in analyses if rec.get("json") is not None],
                              want, freq_range)
    if any(rec.get("fr_field") != field for rec in analyses):
        analyses = M.parse_all(manifest, field)
    return field, analyses


def _load_after(args, manifest, field, freq_range):
    if not args.after_manifest:
        return None
    after_manifest = M.load_manifest(args.after_manifest)
    after_analyses = M.parse_all(after_manifest, field)
    after_field, after_analyses = _resolve_freq_field(
        after_manifest, after_analyses, field, freq_range)
    if after_field != field:
        raise M.InputError("after manifest resolved frequency field %r != %r"
                           % (after_field, field))
    plant = M.build_h(after_manifest, after_analyses, field, freq_range,
                      common_d0_samples=float(after_manifest.get("common_d0_samples") or 0.0))
    labels_before = [p["label"] for p in manifest["positions"]]
    labels_after = [p["label"] for p in after_manifest["positions"]]
    if labels_before != labels_after:
        raise M.InputError("after manifest positions %r != baseline %r"
                           % (labels_after, labels_before))
    if plant["design"]["H"].shape[0] != len(
            [p for p in manifest["positions"] if p["role"] == "design"]):
        raise M.InputError("after manifest design position count differs")
    return {"plant": plant, "manifest": after_manifest,
            "analyses": after_analyses, "path": after_manifest["path"]}


def cmd_run(args):
    bundle = G.GuardBundle()
    manifest = M.load_manifest(args.manifest)
    freq_range = (float(args.band_lo), float(args.band_hi))
    analyses = M.parse_all(manifest, args.freq_field)
    field, analyses = _resolve_freq_field(manifest, analyses, args.freq_field,
                                          freq_range)
    after = _load_after(args, manifest, field, freq_range)
    options = {
        "freq_range_hz": freq_range,
        "freq_field": field,
        "extra_evidence_roots": [os.path.abspath(p) for p in (args.evidence_root or [])],
        "holdout_tol_db": float(args.holdout_tol_db),
        "subspace_angle_warn_deg": float(args.subspace_angle_warn_deg),
        "effort_caps": _parse_float_list(args.effort_caps, X.DEFAULT_EFFORT_CAPS),
        "min_design_points": int(args.min_design_points),
        "min_holdout_points": int(args.min_holdout_points),
        "min_report_snr_db": float(args.min_report_snr_db),
        "skip_session_require": bool(args.skip_session_require),
        "command": sys.argv,
        "git_commit": _git_commit(),
        "after_manifest": after["path"] if after else None,
        "_after_plant": after["plant"] if after else None,
    }
    result = R.run_diagnostics(manifest, analyses, bundle, options)
    result["input"]["after_manifest"] = options["after_manifest"]
    outputs = R.write_outputs(result, args.out_dir)
    result["_out_dir"] = os.path.abspath(args.out_dir)
    print("h-diag: verdict=%s conclusions_allowed=%s" %
          (result["verdict"], result["conclusions_allowed"]))
    print("  provenance=%s consistency=%s completeness=%s" %
          (result["provenance"]["status"], result["consistency"]["status"],
           result["completeness"]["status"]))
    print("  ENERGY_MIGRATION=%s HOLDOUT_SEVERE_REGRESSION=%s" %
          (result["statuses"]["energy_migration"]["status"],
           result["statuses"]["holdout_severe_regression"]["status"]))
    if result["provenance"]["codes"]:
        print("  provenance codes: %s" % ", ".join(result["provenance"]["codes"]))
    if result["consistency"]["codes"]:
        print("  consistency codes: %s" % ", ".join(result["consistency"]["codes"]))
    print("  wrote %s" % outputs["json"])
    print("  wrote %s" % outputs["report"])
    for path in outputs["csv"]:
        print("  wrote %s" % path)
    for path in outputs["png"]:
        print("  wrote %s" % path)
    if not result["conclusions_allowed"]:
        print("  NOTE: controllability conclusions withheld (%s)" %
              result["conclusions"]["withheld_reason"])
    return 0 if result["conclusions_allowed"] else 3


def cmd_selftest(args):
    report = T.run(args.out_dir)
    print("h-diag selftest: %d/%d passed, %d failed" %
          (report["summary"]["passed"], report["summary"]["total"],
           report["summary"]["failed"]))
    for case in report["cases"]:
        status = "PASS" if case["pass"] else "FAIL"
        print("[%s] %-42s %s" % (status, case["name"], case["description"]))
        if not case["pass"]:
            print("        expected: %r" % (case["expected"],))
            print("        got     : %r" % (case["got"],))
            if case["detail"]:
                print("        detail  : %s" % case["detail"])
    return 0 if report["summary"]["failed"] == 0 else 1


def cmd_check_manifest(args):
    bundle = G.GuardBundle()
    manifest = M.load_manifest(args.manifest)
    freq_range = (float(args.band_lo), float(args.band_hi))
    analyses = M.parse_all(manifest, args.freq_field)
    field, analyses = _resolve_freq_field(manifest, analyses, args.freq_field,
                                          freq_range)
    prov = G.evaluate_provenance(manifest, analyses, bundle)
    consistency = M.check_consistency(manifest, analyses, {})
    completeness = M.check_completeness(manifest, consistency, prov, {})
    summary = {
        "manifest": manifest["path"],
        "positions": len(manifest["positions"]),
        "design_positions": completeness["design_positions"],
        "holdout_positions": completeness["holdout_positions"],
        "freq_field": field,
        "analysis_errors": [{"path": rec["path"], "error": rec["error"]}
                            for rec in analyses if rec.get("error")],
        "provenance": {"status": prov["status"], "codes": prov["codes"]},
        "consistency": {"status": consistency["status"],
                        "codes": consistency["codes"]},
        "completeness": {"status": completeness["status"],
                         "codes": completeness["codes"]},
        "guards": prov["policy"]["ok"],
    }
    print(json.dumps(summary, indent=2, ensure_ascii=False))
    return 0 if (prov["status"] == "MEASURED"
                 and consistency["status"] == "CONSISTENT") else 3


def cmd_version(args):
    print("h-diag %s (rt-stack tools/h-diag, repo %s)" %
          (G.TOOL_VERSION, G.repo_root()))
    return 0


def build_parser():
    parser = argparse.ArgumentParser(
        prog="hdiag.py",
        description="Complex H-matrix diagnostics for the 4.1.2 low-band MIMO matrix")
    sub = parser.add_subparsers(dest="command", required=True)

    run = sub.add_parser("run", help="run diagnostics on a pre-registered multipoint set")
    run.add_argument("--manifest", required=True,
                     help="positions.json (schema tspi-hdiag-positions/v1)")
    run.add_argument("--out-dir", required=True)
    run.add_argument("--after-manifest", default=None,
                     help="optional measured-after positions.json (same labels/roles)")
    run.add_argument("--freq-field", default="auto", choices=["auto", "fr", "fr_modal"])
    run.add_argument("--band-lo", type=float, default=20.0)
    run.add_argument("--band-hi", type=float, default=300.0)
    run.add_argument("--evidence-root", action="append", default=[],
                     help="additional machine-checkable evidence root")
    run.add_argument("--holdout-tol-db", type=float, default=3.0)
    run.add_argument("--subspace-angle-warn-deg", type=float, default=30.0)
    run.add_argument("--effort-caps", default="0.01,0.1,1.0")
    run.add_argument("--min-design-points", type=int, default=6)
    run.add_argument("--min-holdout-points", type=int, default=3)
    run.add_argument("--min-report-snr-db", type=float, default=20.0)
    run.add_argument("--skip-session-require", action="store_true",
                     help="do not require session/mic identity (stays UNPROVEN)")
    run.set_defaults(func=cmd_run)

    st = sub.add_parser("selftest", help="synthetic known-rank self-test")
    st.add_argument("--out-dir", default=None)
    st.set_defaults(func=cmd_selftest)

    cm = sub.add_parser("check-manifest", help="validate manifest + analyses + gates")
    cm.add_argument("--manifest", required=True)
    cm.add_argument("--freq-field", default="auto", choices=["auto", "fr", "fr_modal"])
    cm.add_argument("--band-lo", type=float, default=20.0)
    cm.add_argument("--band-hi", type=float, default=300.0)
    cm.set_defaults(func=cmd_check_manifest)

    ver = sub.add_parser("version")
    ver.set_defaults(func=cmd_version)
    return parser


def main(argv=None):
    args = build_parser().parse_args(argv)
    try:
        return args.func(args)
    except M.InputError as exc:
        print("h-diag: input error: %s" % (exc,), file=sys.stderr)
        return 2
    except (OSError, ValueError) as exc:
        print("h-diag: error: %s" % (exc,), file=sys.stderr)
        return 2
    except Exception as exc:  # keep exit codes honest for CI consumption
        import traceback
        traceback.print_exc()
        print("h-diag: unexpected failure: %s" % (exc,), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
