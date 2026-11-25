#!/usr/bin/env python3
"""RoomForge -> KHAEN low-frequency MIMO pipeline driver (offline, synthetic only).

Steps
  1. (optional) generate deterministic fixtures with the RoomForge C# generator;
  2. verify every fixture file against its SHA256 manifest;
  3. run the Rust optimizer CLI (khaen-mimo-opt) over every scene;
  4. run the independent Python reference metrics and cross-check Rust vs Python;
  5. aggregate evidence: summary.json / SUMMARY.csv / MANIFEST.sha256.

Everything this script touches is SIMULATED data: no audio device, no real room, no
network.  Results must never be reported as measured (KHAEN spec/mimo-objective-v1 §0.4).

Usage (from projects/khaen-audio-dsp):
  python rt-stack/tools/analysis/run_mimo_roomforge_pipeline.py \
      --roomforge C:/WorkSpace/CSharp/RoomForge \
      --fixtures rt-stack/evidence/calib-chain-20261121/mimo-opt-20261122/fixtures \
      --runs rt-stack/evidence/calib-chain-20261121/mimo-opt-20261122/runs \
      --validation rt-stack/evidence/calib-chain-20261121/mimo-opt-20261122/validation
"""
from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
RT_STACK = os.path.join(ROOT, "rt-stack")
ANALYSIS = os.path.join(RT_STACK, "tools", "analysis")

# Pre-declared assumptions this study compares against.  These are the values currently
# written by hand in docs/MIMO-COEFF-PLAN.md §0: room 4.10 x 5.20 x 2.70 m, T60 0.35-0.45 s.
HANDWRITTEN_ASSUMPTION = {
    "room_length_m": 4.10,
    "room_width_m": 5.20,
    "room_height_m": 2.70,
    "t60_s_min": 0.35,
    "t60_s_max": 0.45,
    "source": "docs/MIMO-COEFF-PLAN.md (hand-written assumption, not a measurement)",
}


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def rel(path: str, base: str) -> str:
    try:
        return os.path.relpath(path, base).replace("\\", "/")
    except ValueError:
        return path


def run(cmd: list[str], cwd: str | None = None, log_path: str | None = None) -> int:
    print("RUN:", " ".join(cmd), flush=True)
    proc = subprocess.run(cmd, cwd=cwd, text=True, capture_output=True)
    if log_path:
        with open(log_path, "w", encoding="utf-8") as fh:
            fh.write("$ " + " ".join(cmd) + "\n")
            fh.write("\n--- stdout ---\n" + proc.stdout)
            fh.write("\n--- stderr ---\n" + proc.stderr)
            fh.write(f"\n--- exit {proc.returncode} ---\n")
    if proc.stdout.strip():
        print(proc.stdout.strip()[:4000])
    if proc.returncode != 0 and proc.stderr.strip():
        print(proc.stderr.strip()[:4000], file=sys.stderr)
    return proc.returncode


# ---------------------------------------------------------------- fixtures
def generate_fixtures(roomforge: str, fixtures: str, log_dir: str) -> int:
    proj = os.path.join("tools", "AudioOptimizer.MimoFixtures")
    cmd = ["dotnet", "run", "--project", proj, "-c", "Release", "--", "--out", fixtures, "--force"]
    return run(cmd, cwd=roomforge, log_path=os.path.join(log_dir, "generate-fixtures.log"))


def verify_fixtures(fixtures: str, validation: str) -> dict:
    manifest_path = os.path.join(fixtures, "MANIFEST.json")
    if not os.path.isfile(manifest_path):
        raise SystemExit(f"missing fixture manifest: {manifest_path}")
    with open(manifest_path, "r", encoding="utf-8") as fh:
        manifest = json.load(fh)

    report = {
        "format": "khaen-roomforge-fixture-verify/v1",
        "manifest_sha256": sha256_file(manifest_path),
        "scenes": [],
        "ok": True,
        "file_count": 0,
        "total_bytes": 0,
    }
    for scene_entry in manifest.get("scenes", []):
        scene_dir_name = scene_entry.get("dir") or scene_entry.get("path")
        if not scene_dir_name:
            raise SystemExit(f"MANIFEST.json scene entry has neither 'dir' nor 'path': {scene_entry}")
        scene_dir = os.path.join(fixtures, scene_dir_name)
        sums_path = os.path.join(scene_dir, "SHA256SUMS")
        if not os.path.isfile(sums_path):
            raise SystemExit(f"missing SHA256SUMS: {sums_path}")
        checked, bad = 0, []
        with open(sums_path, "r", encoding="utf-8") as fh:
            for line in fh:
                line = line.rstrip("\n")
                if not line:
                    continue
                digest, name = line.split("  ", 1)
                target = os.path.join(scene_dir, name.replace("/", os.sep))
                if not os.path.isfile(target):
                    bad.append({"path": name, "error": "missing"})
                    continue
                actual = sha256_file(target)
                checked += 1
                report["file_count"] += 1
                report["total_bytes"] += os.path.getsize(target)
                if actual != digest:
                    bad.append({"path": name, "expected": digest, "actual": actual})
        gt_path = os.path.join(scene_dir, "ground-truth.json")
        gt = json.load(open(gt_path, "r", encoding="utf-8"))
        scene = {
            "scene_id": gt.get("scene_id", scene_dir_name),
            "dir": scene_dir_name,
            "ground_truth_sha256": sha256_file(gt_path),
            "sha256sums_sha256": sha256_file(sums_path),
            "files_checked": checked,
            "bad": bad,
            "seed": gt.get("seed"),
            "n_sources": len(gt.get("sources", [])),
            "n_mics": len(gt.get("mics", [])),
            "room": gt.get("room"),
            "rir_scale": gt.get("rir", {}).get("scale", 1.0),
        }
        report["scenes"].append(scene)
        if bad:
            report["ok"] = False
    report["scenes"].sort(key=lambda s: s["scene_id"])
    os.makedirs(validation, exist_ok=True)
    with open(os.path.join(validation, "fixture-verify.json"), "w", encoding="utf-8") as fh:
        json.dump(report, fh, ensure_ascii=False, indent=2, sort_keys=True)
    if not report["ok"]:
        raise SystemExit("fixture SHA256 verification FAILED (see fixture-verify.json)")
    print(f"fixtures OK: {report['file_count']} files, {report['total_bytes'] / 1e6:.2f} MB, "
          f"{len(report['scenes'])} scenes")
    return report


# ---------------------------------------------------------------- rust / python steps
def run_rust(fixtures: str, runs: str, validation: str) -> int:
    os.makedirs(runs, exist_ok=True)
    cmd = ["cargo", "run", "--release", "-p", "khaen-mimo-opt", "--",
           "--fixtures", fixtures, "--out", runs, "--verify-fixtures"]
    rc = run(cmd, cwd=RT_STACK, log_path=os.path.join(validation, "rust-mimo-opt.log"))
    if rc == 0:
        # second pass without --verify-fixtures is unnecessary: reports are written either way
        pass
    return rc


def run_python_metrics(fixtures: str, runs: str, validation: str) -> int:
    script = os.path.join(ANALYSIS, "mimo_roomforge_metrics.py")
    if not os.path.isfile(script):
        print(f"WARNING: reference metrics script missing: {script}", file=sys.stderr)
        return 0
    cmd = [sys.executable, script, "--fixtures", fixtures, "--runs", runs, "--out", validation]
    return run(cmd, cwd=ROOT, log_path=os.path.join(validation, "python-metrics.log"))


# ---------------------------------------------------------------- aggregation
def load_json(path: str):
    try:
        with open(path, "r", encoding="utf-8") as fh:
            return json.load(fh)
    except (OSError, json.JSONDecodeError):
        return None


def variant_row(scene_id: str, scene: dict, variant: dict) -> dict:
    design = variant.get("design") or {}
    holdout = variant.get("holdout") or {}
    cons = variant.get("constraints") or {}
    return {
        "scene_id": scene_id,
        "n_sources": scene.get("n_sources"),
        "n_mics": scene.get("n_mics"),
        "n_design": scene.get("n_design"),
        "n_holdout": scene.get("n_holdout"),
        "variant": variant.get("id"),
        "design_E_modal_db": design.get("e_modal_db"),
        "design_R_max": design.get("r_max"),
        "design_energy_migration": design.get("energy_migration"),
        "holdout_E_modal_db": holdout.get("e_modal_db"),
        "holdout_R_max": holdout.get("r_max"),
        "holdout_consistency_C": holdout.get("consistency_c"),
        "holdout_severe_regression": holdout.get("severe_regression"),
        "effort_vs_baseline": cons.get("effort_vs_baseline"),
        "global_scale": cons.get("global_scale"),
    }


def aggregate(fixtures: str, runs: str, validation: str, fixture_report: dict) -> dict:
    summary = {
        "format": "khaen-mimo-roomforge-pipeline-summary/v1",
        "source": "SIMULATED",
        "honesty_status": "UNPROVEN",
        "claim": "all data are RoomForge shoebox image-source simulations; no real room, no measurement",
        "generated_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "handwritten_khaen_assumption": HANDWRITTEN_ASSUMPTION,
        "fixtures": {
            "manifest_sha256": fixture_report.get("manifest_sha256"),
            "file_count": fixture_report.get("file_count"),
            "scenes": {s["scene_id"]: {
                "ground_truth_sha256": s["ground_truth_sha256"],
                "seed": s["seed"],
                "n_sources": s["n_sources"],
                "n_mics": s["n_mics"],
                "room": s["room"],
                "rir_scale": s["rir_scale"],
            } for s in fixture_report.get("scenes", [])},
        },
        "scenes": {},
        "crosscheck": None,
        "t60_analysis": load_json(os.path.join(validation, "t60-analysis.json")),
    }
    rows: list[dict] = []
    for name in sorted(os.listdir(runs)):
        scene_dir = os.path.join(runs, name)
        report_path = os.path.join(scene_dir, "report.json")
        if not os.path.isdir(scene_dir) or not os.path.isfile(report_path):
            continue
        report = load_json(report_path)
        if not report:
            continue
        scene_meta = report.get("scene") or {}
        scene_id = report.get("scene_id") or scene_meta.get("id") or name
        raw_variants = report.get("variants") or {}
        if isinstance(raw_variants, dict):
            variants = [dict(v, id=vid) if isinstance(v, dict) else {"id": vid}
                        for vid, v in raw_variants.items()]
        else:
            variants = raw_variants
        scene_summary = {
            "run_dir": name,
            "report_sha256": sha256_file(report_path),
            "provenance": report.get("provenance"),
            "config": report.get("config"),
            "variants": {},
        }
        for variant in variants:
            vid = variant.get("id")
            scene_summary["variants"][vid] = variant
            rows.append(variant_row(scene_id, scene_meta, variant))
        summary["scenes"][scene_id] = scene_summary

    with open(os.path.join(validation, "SUMMARY.csv"), "w", encoding="utf-8", newline="") as fh:
        fields = ["scene_id", "n_sources", "n_mics", "n_design", "n_holdout", "variant",
                  "design_E_modal_db", "design_R_max", "design_energy_migration",
                  "holdout_E_modal_db", "holdout_R_max", "holdout_consistency_C",
                  "holdout_severe_regression", "effort_vs_baseline", "global_scale"]
        writer = csv.DictWriter(fh, fieldnames=fields, lineterminator="\n")
        writer.writeheader()
        for row in rows:
            writer.writerow(row)
    summary["summary_csv_sha256"] = sha256_file(os.path.join(validation, "SUMMARY.csv"))

    # Rust vs Python cross-check, if the reference script produced one.
    py = load_json(os.path.join(validation, "python-metrics.json"))
    if py:
        summary["crosscheck"] = {
            "python_metrics_sha256": sha256_file(os.path.join(validation, "python-metrics.json")),
            "max_abs_diff": py.get("max_abs_diff"),
            "per_scene": {k: v.get("crosscheck") for k, v in (py.get("scenes") or {}).items()},
        }

    # Round-2 objective comparison / independent cross-check (optional).
    obj_cmp_path = os.path.join(validation, "objective-comparison.json")
    if os.path.isfile(obj_cmp_path):
        obj = load_json(obj_cmp_path) or {}
        rows = obj.get("rows") or []
        status_counts = {}
        for row in rows:
            vid = row.get("variant")
            if vid not in ("B2", "B3"):
                continue
            key = f"{row.get('tag')}:{vid}:{row.get('predicted_status')}"
            status_counts[key] = status_counts.get(key, 0) + 1
        summary["objective_comparison"] = {
            "objective_comparison_sha256": sha256_file(obj_cmp_path),
            "tags": obj.get("tags"),
            "status_counts": status_counts,
        }
    obj_py_path = os.path.join(validation, "python-metrics-objectives", "objective-summary.json")
    if os.path.isfile(obj_py_path):
        op = load_json(obj_py_path) or {}
        summary["objective_crosscheck"] = {
            "objective_summary_sha256": sha256_file(obj_py_path),
            "overall_status": op.get("overall_status"),
            "rule": op.get("status_rule") or op.get("rule"),
            "max_abs_diff": op.get("max_abs_diff"),
        }
    with open(os.path.join(validation, "pipeline-summary.json"), "w", encoding="utf-8") as fh:
        json.dump(summary, fh, ensure_ascii=False, indent=2, sort_keys=True)
    print(f"aggregated {len(summary['scenes'])} scenes, {len(rows)} variant rows")
    return summary


def manifest(validation: str, extra_roots: list[str]) -> str:
    """SHA256 manifest over the text evidence (WAVs are git-ignored per repo hygiene)."""
    lines: list[tuple[str, str]] = []
    for root_path in extra_roots:
        if not os.path.isdir(root_path):
            continue
        for dirpath, _dirnames, filenames in os.walk(root_path):
            for filename in filenames:
                if filename.lower().endswith(".wav") or filename == "MANIFEST.sha256":
                    continue
                full = os.path.join(dirpath, filename)
                lines.append((rel(full, os.path.dirname(root_path)), sha256_file(full)))
    out = os.path.join(validation, "MANIFEST.sha256")
    lines.sort()
    with open(out, "w", encoding="utf-8", newline="\n") as fh:
        for name, digest in lines:
            fh.write(f"{digest}  {name}\n")
    print(f"manifest: {len(lines)} files -> {out}")
    return out


def print_table(summary: dict) -> None:
    print()
    print(f"{'scene':<26}{'variant':<8}{'design Emodal dB':>17}{'design Rmax':>12}"
          f"{'holdout C':>10}{'severe':>8}")
    for scene_id, scene in sorted(summary["scenes"].items()):
        for vid in ("B0", "B1", "B2", "B3"):
            v = scene["variants"].get(vid) or {}
            d = v.get("design") or {}
            h = v.get("holdout") or {}
            print(f"{scene_id:<26}{vid:<8}{str(d.get('e_modal_db')):>17}{str(d.get('r_max')):>12}"
                  f"{str(h.get('consistency_c')):>10}{str(h.get('severe_regression')):>8}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--roomforge", default=os.environ.get("ROOMFORGE_DIR", "C:/WorkSpace/CSharp/RoomForge"))
    ap.add_argument("--fixtures", required=True)
    ap.add_argument("--runs", required=True)
    ap.add_argument("--validation", required=True)
    ap.add_argument("--skip-generate", action="store_true")
    ap.add_argument("--skip-rust", action="store_true")
    ap.add_argument("--skip-python", action="store_true")
    ok = ap.parse_args()
    fixtures, runs, validation = ok.fixtures, ok.runs, ok.validation
    os.makedirs(validation, exist_ok=True)
    os.makedirs(runs, exist_ok=True)

    if ok.skip_generate:
        if not os.path.isdir(fixtures):
            print(f"fixtures missing: {fixtures}", file=sys.stderr)
            return 2
    else:
        rc = generate_fixtures(ok.roomforge, fixtures, validation)
        if rc != 0:
            print("fixture generation failed", file=sys.stderr)
            return rc

    fixture_report = verify_fixtures(fixtures, validation)

    if not ok.skip_rust:
        rc = run_rust(fixtures, runs, validation)
        if rc != 0:
            print("rust optimizer failed", file=sys.stderr)
            return rc
    if not ok.skip_python:
        rc = run_python_metrics(fixtures, runs, validation)
        if rc != 0:
            print("python reference metrics failed", file=sys.stderr)
            return rc

    summary = aggregate(fixtures, runs, validation, fixture_report)
    print_table(summary)
    # One manifest over the whole evidence directory: fixtures + runs + runs-obj + validation
    # (WAVs are git-ignored; MANIFEST.sha256 excludes itself).
    manifest(validation, [os.path.dirname(os.path.abspath(validation))])
    return 0


if __name__ == "__main__":
    sys.exit(main())
