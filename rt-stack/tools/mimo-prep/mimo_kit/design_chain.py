#!/usr/bin/env python3
"""Fixed LIVE MIMO G design chain (task-252): real remeasure H -> G -> T66 plan.

Replaces the 2026-11-19 pilot design chain (evidence/mimo-pilot-3mic/prep/ops-0830/
steps/design_from_identity.py), which (a) imported the objective from the dry-run
harness, (b) accepted any H (the 08:30 MA1/ARC byte-mapping artifact would have been
normalised into the design), and (c) had no provenance gate before T66/pack/apply.
The packer-side companion bug is in run_0830.step_t66_compile (silent
prep/dryrun/out/design fallback); the minimal patch is tools/mimo-prep/patches/.

Chain (every boundary fail-closed):
  arm raw (UMIK capture.raw + MA1/ARC mono, meta/session.json+playback.json)
    -> analyze.estimate_H -> H7 (7-output order) -> h_guard -> reference_design
    -> g_design.npz + design-report.json(provenance) -> g_guard (never synthetic)
    -> export T66 text (artifact sha chain) -> mimo_t66_pack (REAL T66 compiler)
    -> plan.bin + compile-report.json -> g_guard_plan (sha differs from registry)
    -> plan-provenance.json + manifest.

Usage:
  python -m mimo_kit.design_chain --arm <identity arm dir> --out <dir> \
      [--taps 4096] [--delay 2048] [--sweep-seconds 8] [--level-db -15.58]
  python -m mimo_kit.design_chain --h <h_estimate.npz> --out <dir> ...
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys

import numpy as np

try:
    from . import analyze
    from . import provenance as pv
    from . import reference_design as rd
except ImportError:  # pragma: no cover - direct execution
    import analyze
    import provenance as pv
    import reference_design as rd

KIT = os.path.dirname(os.path.abspath(__file__))
PROJECT = os.path.abspath(os.path.join(KIT, "..", "..", "..", ".."))
OUTPUT_SLOTS = [slot for _name, slot in rd.OUTPUTS]          # [0,1,2,3,4,5,7]
PARTICIPANT_ROWS = {p: rd.participant_index(p) for p in rd.PARTICIPANTS}  # FL..SUB1 -> 0..3,6
G_BAND_LEVEL_HZ = (80.0, 300.0)
DEFAULT_PARAMS = {"sweep_seconds": 8.0, "repeats": 3, "pre": 1.5,
                  "repeat_gap": 1.0, "slot_gap": 1.0, "level_db": -15.58}


def sha256_file(path):
    return pv.sha256_file(path)


def _load_arm_meta(arm_dir):
    meta = pv.load_json(os.path.join(arm_dir, "meta", "session.json"), {}) or {}
    marker = pv.load_json(os.path.join(arm_dir, "meta", "playback.json"), {}) or {}
    return meta, marker


def build_h7_from_arm(arm_dir, params=None, freqs=None):
    """Estimate H from a measured arm and re-order container slots -> 7 output rows."""
    params = dict(DEFAULT_PARAMS, **(params or {}))
    freqs = analyze.oct_grid() if freqs is None else np.asarray(freqs, dtype=np.float64)
    est = analyze.estimate_H(arm_dir, params, freqs)
    mic_names = [m for m in rd.DESIGN_MICS if m in est["H"]]
    missing = [m for m in rd.DESIGN_MICS if m not in est["H"]]
    if missing:
        raise pv.ProvenanceRefusal("h_missing_mic", {"missing": missing, "found": mic_names})
    H7 = np.stack([np.asarray(est["H"][m])[OUTPUT_SLOTS, :] for m in mic_names], axis=0)
    nsamp = 0
    for m in mic_names:
        for _slot, ir in (est["irs"][m] or {}).items():
            nsamp = max(nsamp, len(np.asarray(ir)))
    irs = np.zeros((len(mic_names), len(rd.OUTPUTS), nsamp), dtype=np.float64)
    for mi, m in enumerate(mic_names):
        for oi, slot in enumerate(OUTPUT_SLOTS):
            ir = (est["irs"][m] or {}).get(slot)
            if ir is not None:
                irs[mi, oi, :len(ir)] = np.asarray(ir, dtype=np.float64)
    # per-mic level normalisation over the actually driven participant rows only
    rows = [PARTICIPANT_ROWS[p] for p in rd.PARTICIPANTS]
    band = (freqs >= G_BAND_LEVEL_HZ[0]) & (freqs <= G_BAND_LEVEL_HZ[1])
    levels = np.array([float(np.median(np.abs(H7[i][rows][:, band]))) for i in range(len(mic_names))])
    levels[levels <= 0] = 1e-30
    H_norm = H7 / levels[:, None, None]
    meta, marker = _load_arm_meta(arm_dir)
    info = {
        "schema": "tspi-mimo-h/v2", "task": "task-252", "synthetic": False,
        "arm": os.path.abspath(arm_dir),
        "arm_meta": {"session_json": os.path.join(arm_dir, "meta", "session.json"),
                     "session_sha256": sha256_file(os.path.join(arm_dir, "meta", "session.json"))
                     if os.path.isfile(os.path.join(arm_dir, "meta", "session.json")) else None,
                     "playback_json": os.path.join(arm_dir, "meta", "playback.json"),
                     "playback_sha256": sha256_file(os.path.join(arm_dir, "meta", "playback.json"))
                     if os.path.isfile(os.path.join(arm_dir, "meta", "playback.json")) else None},
        "fs_hz": analyze.FS, "mic_names": mic_names, "out_names": [n for n, _s in rd.OUTPUTS],
        "mic_stats": est["mic_stats"], "offset_samples": est["offset_samples"],
        "offset_quality": est["offset_quality"], "noise_ir_power": est["noise_ir_power"],
        "level_norm": {m: float(levels[i]) for i, m in enumerate(mic_names)},
        "sweep": {"params": params, "level_db_from_meta": (meta.get("sweep") or {}).get("level_db")},
        "marker": {k: marker.get(k) for k in ("path", "backend", "level_db", "spl_target_db",
                                              "file_sha256", "arm")},
        "claim": "H estimated from a REAL re-measured arm (mimo-kit-fix 16:12 window); "
                 "pilot_3mic; not T6/T67/T68/T69 acceptance",
    }
    return {"freq": freqs, "H_raw": H7, "H_norm": H_norm, "irs": irs,
            "mic_names": mic_names, "out_names": [n for n, _s in rd.OUTPUTS], "info": info}


def save_h(h, outdir):
    os.makedirs(outdir, exist_ok=True)
    npz = os.path.join(outdir, "h_estimate.npz")
    np.savez(npz, freq=h["freq"], H_raw=h["H_raw"], H_norm=h["H_norm"], irs=h["irs"],
             mic_names=np.array(h["mic_names"]), out_names=np.array(h["out_names"]))
    info = dict(h["info"])
    info["h_sha256"] = sha256_file(npz)
    info["h_npz"] = os.path.abspath(npz)
    with open(os.path.join(outdir, "h_estimate.json"), "w", encoding="utf-8") as fh:
        json.dump(info, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    return npz


def load_h_npz(h_npz):
    arrs = pv.load_npz(h_npz)
    info = pv.load_json(os.path.join(os.path.dirname(os.path.abspath(h_npz)), "h_estimate.json"), {}) or {}
    return {"freq": arrs["freq"], "H_raw": arrs["H_raw"], "H_norm": arrs["H_norm"],
            "irs": arrs["irs"], "mic_names": [str(x) for x in arrs.get("mic_names", info.get("mic_names") or [])],
            "out_names": [str(x) for x in arrs.get("out_names", [])], "info": info}


def design_real(h, outdir, taps=4096, delay=2048, registry=None):
    """Guarded real-room design; REFUSES invalid H or synthetic output."""
    hv = pv.h_guard(h, registry=registry, live=True)
    if not hv["ok"]:
        raise pv.ProvenanceRefusal(hv["codes"], {"h_guard": hv, "arm": (h.get("info") or {}).get("arm")})
    design_dir = os.path.join(outdir, "design")
    prov = {"schema": "khaen-mimo-design-provenance/v1", "task": "task-252", "live": True,
            "h_arm": h["info"].get("arm"), "h_sha256": h["info"].get("h_sha256"),
            "h_guard": {"ok": hv["ok"], "codes": hv["codes"], "policy": hv["policy"],
                        "mic_stats": hv["metrics"].get("mic_stats"),
                        "H_max_abs_db_per_participant": hv["metrics"].get("H_max_abs_db_per_participant")},
            "guard_module": "rt-stack/tools/mimo-prep/mimo_kit/provenance.py"}
    res = rd.design_reference(h, design_dir, taps=taps, delay=delay, provenance=prov)
    report = res["report"]
    gv = pv.g_guard(design_dir, registry=registry, live=True)
    if not gv["ok"]:
        raise pv.ProvenanceRefusal(gv["codes"], {"g_guard": gv})
    npz = os.path.join(design_dir, "g_design.npz")
    prov.update({
        "design_g_sha256": sha256_file(npz),
        "G_ideal_content_sha256": pv.canonical_arrays_sha({"G_ideal": res["G_ideal"]}),
        "G_fir_sha256": pv.canonical_arrays_sha({"G_fir": res["G_fir"]}),
        "G_fir_taps": int(taps), "G_fir_delay": int(delay),
        "g_guard": {"ok": gv["ok"], "codes": gv["codes"],
                    "G_fir_max_abs_vs_dryrun_realized": gv["metrics"].get("G_fir_max_abs_vs_dryrun_realized"),
                    "offdiag_peak": gv["metrics"].get("offdiag_peak")},
        "design_report": {"max_path_gain_ok": report.get("max_path_gain_ok"),
                          "per_dac_gain_ok": report.get("per_dac_gain_ok"),
                          "per_dac_scaled_by": report.get("per_dac_scaled_by")},
        "claim": "REAL-room G designed from the corrected remeasure identity-arm H "
                 "(old pilot spatial-average objective); not synthetic; no holdout; not T6 acceptance",
    })
    with open(os.path.join(design_dir, "provenance.json"), "w", encoding="utf-8") as fh:
        json.dump(prov, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    return {"design_dir": design_dir, "g_npz": npz, "report": report, "provenance": prov}


# ---------------------------------------------------------------- T66 export
OUT_INDEX = {name: i for i, (name, _slot) in enumerate(rd.OUTPUTS)}
DAC = {name: slot for name, slot in rd.OUTPUTS}
PARTICIPANT_SOURCES = [0, 1, 2, 3, 7]
UNUSED = [4, 5, 6]
N_IN = 8
CONTAINER = 8


def export_t66(design_dir, outdir, l_g=4096, d=2048, block=128, registry=None):
    """Export a (guarded) real design to the REAL T66 compiler text input."""
    design_dir = os.path.abspath(design_dir)
    gv = pv.g_guard(design_dir, registry=registry, live=True)
    if not gv["ok"]:
        raise pv.ProvenanceRefusal(gv["codes"], {"g_guard": gv, "design_dir": design_dir})
    os.makedirs(outdir, exist_ok=True)
    npz = pv.load_npz(os.path.join(design_dir, "g_design.npz"))
    freqs = npz["freq"]
    G_ideal = npz["G_ideal"]
    G_fir = rd._realize_fir(freqs, G_ideal, int(l_g), int(d))
    cells = np.zeros((N_IN, N_IN, int(l_g)), dtype=np.float64)
    for od, oname in enumerate(rd.PARTICIPANTS):
        for id_, iname in enumerate(rd.PARTICIPANTS):
            coeffs = G_fir[OUT_INDEX[oname], OUT_INDEX[iname]].copy()
            if od == id_:
                coeffs[d] -= 1.0  # compiler injects the delta_D baseline for diagonal cells
            cells[od, id_, :] = coeffs
    files = {
        "g_design": os.path.join(design_dir, "g_design.npz"),
        "design_report": os.path.join(design_dir, "design-report.json"),
        "h_estimate": os.path.join(design_dir, "h_estimate.npz"),
        "h_estimate_json": os.path.join(design_dir, "h_estimate.json"),
        "design_provenance": os.path.join(design_dir, "provenance.json"),
    }
    shas = []
    for key in ("g_design", "design_report", "h_estimate", "h_estimate_json",
                "design_provenance"):
        p = files[key]
        shas.append(sha256_file(p) if os.path.isfile(p) else hashlib.sha256(key.encode()).hexdigest())
    # slots 6-7: the measured arm markers (real capture provenance; T66 wants exactly 7)
    arm = (pv.load_json(files["h_estimate_json"], {}) or {}).get("arm")
    for marker_name, salt in (("playback.json", b"no-playback-marker"),
                              ("session.json", b"no-session-marker")):
        marker = os.path.join(str(arm), "meta", marker_name) if arm else ""
        shas.append(sha256_file(marker) if marker and os.path.isfile(marker)
                    else hashlib.sha256(salt).hexdigest())
    lines = ["# task-252 REAL-room MIMO G design -> real T66 compiler input "
             "(pilot_3mic, remeasure identity H; provenance-guarded)",
             "fs 48000", "block %d" % block, "n_in %d" % N_IN, "container %d" % CONTAINER,
             "l_g %d" % int(l_g), "l_p 1", "d %d" % int(d),
             "unused %s" % ",".join(str(v) for v in UNUSED),
             "artifact_sha %s" % " ".join(shas)]
    for idx, name in enumerate(rd.PARTICIPANTS):
        lines.append("role %d %s %d %d" % (idx, name, DAC[name], PARTICIPANT_SOURCES[idx]))
    for dense, name in enumerate(rd.PARTICIPANTS):
        lines.append("participant %d %d" % (dense, rd.PARTICIPANTS.index(name)))
    for od in range(len(rd.PARTICIPANTS)):
        for id_ in range(len(rd.PARTICIPANTS)):
            vals = " ".join("%.10g" % v for v in cells[od, id_, :])
            lines.append("cell %d %d %d %s" % (od, id_, 1 if od == id_ else 0, vals))
    text_path = os.path.join(outdir, "design.t66.txt")
    with open(text_path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")
    np.savez(os.path.join(outdir, "t66_cells.npz"), cells=cells, l_g=int(l_g), d=int(d),
             participants=np.array(rd.PARTICIPANTS),
             out_indices=np.array([OUT_INDEX[p] for p in rd.PARTICIPANTS]),
             sources=np.array(PARTICIPANT_SOURCES),
             dacs=np.array([DAC[p] for p in rd.PARTICIPANTS]))
    offdiag = [float(np.max(np.abs(cells[od, id_])))
               for od in range(len(rd.PARTICIPANTS)) for id_ in range(len(rd.PARTICIPANTS)) if od != id_]
    exp = {"schema": "tspi-mimo-t66-export/v2", "task": "task-252", "synthetic": False,
           "pilot_3mic": True, "design_source": design_dir, "guard_ok": True,
           "l_g": int(l_g), "block_size": block, "baseline_d": int(d),
           "participants": rd.PARTICIPANTS,
           "dense_to_dac": {p: DAC[p] for p in rd.PARTICIPANTS},
           "dense_to_source": {p: PARTICIPANT_SOURCES[i] for i, p in enumerate(rd.PARTICIPANTS)},
           "n_in": N_IN, "file_container_8ch_4_1_2": True,
           "cells_offdiag_max_abs": offdiag,
           "cells_offdiag_nonzero_gt_1e-3": int(sum(1 for v in offdiag if v > 1e-3)),
           "artifact_sha": shas, "design_g_sha256": shas[0],
           "files": {"design_t66_txt": os.path.abspath(text_path),
                     "t66_cells_npz": os.path.abspath(os.path.join(outdir, "t66_cells.npz"))},
           "claim": "input for REAL T66 compiler from provenance-guarded real-room G; "
                    "not a T3/T67 acceptance artifact"}
    with open(os.path.join(outdir, "t66_export.json"), "w", encoding="utf-8") as fh:
        json.dump(exp, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    return {"text_path": text_path, "cells_path": os.path.join(outdir, "t66_cells.npz"),
            "report": exp, "shas": shas}


def find_packer(project=None):
    project = project or PROJECT
    for rel in (os.path.join("rt-stack", "target", "debug", "mimo_t66_pack.exe"),
                os.path.join("rt-stack", "target", "debug", "mimo_t66_pack"),
                os.path.join("rt-stack", "target", "release", "mimo_t66_pack.exe"),
                os.path.join("rt-stack", "target", "release", "mimo_t66_pack")):
        p = os.path.join(project, rel)
        if os.path.isfile(p):
            return p
    return None


def pack_plan(export_dir, packer=None):
    packer = packer or find_packer()
    if not packer:
        raise RuntimeError("mimo_t66_pack binary not found (build tools/mimo-prep/t66_pack)")
    text = os.path.join(export_dir, "design.t66.txt")
    out = os.path.join(os.path.dirname(os.path.abspath(export_dir)), "plan")
    os.makedirs(out, exist_ok=True)
    p = subprocess.run([packer, "--input", text, "--out", out],
                       capture_output=True, text=True, encoding="utf-8", errors="replace")
    with open(os.path.join(out, "pack.log"), "w", encoding="utf-8") as fh:
        fh.write("packer=%s\nexit=%d\nstdout:\n%s\nstderr:\n%s\n"
                 % (packer, p.returncode, p.stdout or "", p.stderr or ""))
    if p.returncode != 0:
        raise RuntimeError("mimo_t66_pack failed rc=%d: %s" % (p.returncode, (p.stderr or "")[-800:]))
    return {"plan": os.path.join(out, "plan.bin"),
            "compile_report": os.path.join(out, "compile-report.json"),
            "pack_summary": os.path.join(out, "pack-summary.json"),
            "packer": os.path.abspath(packer), "stdout": p.stdout}


def guard_plan(plan_path, design_dir, registry=None):
    gv = pv.g_guard_plan(plan_path, design=design_dir, registry=registry, live=True)
    if not gv["ok"]:
        raise pv.ProvenanceRefusal(gv["codes"], {"plan_guard": gv})
    return gv


def design_only(arm_dir=None, h_npz=None, out=None, taps=4096, delay=2048,
                params=None, registry=None):
    """Guarded design step only (H -> H guard -> real-room G); used by the patched
    ops-0830 orchestrator before the unchanged REAL T66 export/pack step."""
    if not out:
        raise ValueError("--out required")
    os.makedirs(out, exist_ok=True)
    if h_npz:
        h = load_h_npz(h_npz)
    else:
        if not arm_dir:
            raise ValueError("either --arm or --h required")
        h = build_h7_from_arm(arm_dir, params=params)
    npz = save_h(h, os.path.join(out, "design"))
    d = design_real(load_h_npz(npz), out, taps=taps, delay=delay, registry=registry)
    return {"ok": True, "design_dir": d["design_dir"], "g_npz": d["g_npz"],
            "design_g_sha256": d["provenance"]["design_g_sha256"],
            "h_sha256": d["provenance"]["h_sha256"],
            "guard": {"ok": True, "codes": []}}


def run_live_design(arm_dir=None, h_npz=None, out=None, taps=4096, delay=2048,
                    params=None, registry=None):
    """End-to-end guarded design -> export -> real T66 plan."""
    if not out:
        raise ValueError("--out required")
    os.makedirs(out, exist_ok=True)
    if h_npz:
        h = load_h_npz(h_npz)
    else:
        if not arm_dir:
            raise ValueError("either --arm or --h required")
        h = build_h7_from_arm(arm_dir, params=params)
    h_npz_path = save_h(h, os.path.join(out, "design"))
    h = load_h_npz(h_npz_path)  # reload so info.h_sha256 is recorded
    d = design_real(h, out, taps=taps, delay=delay, registry=registry)
    e = export_t66(d["design_dir"], os.path.join(out, "export"), l_g=taps, d=delay)
    p = pack_plan(os.path.join(out, "export"))
    gv = guard_plan(p["plan"], d["design_dir"], registry=registry)
    manifest = {
        "schema": "tspi-mimo-real-design-manifest/v1", "task": "task-252",
        "claim": "REAL-room G from corrected remeasure identity H, provenance-guarded, "
                 "compiled by the REAL T66 compiler; offline only (no board, no sound, no git)",
        "arm": h["info"].get("arm"), "h_sha256": h["info"].get("h_sha256"),
        "design_dir": os.path.abspath(d["design_dir"]),
        "design_g_sha256": d["provenance"]["design_g_sha256"],
        "G_ideal_content_sha256": d["provenance"]["G_ideal_content_sha256"],
        "g_guard": d["provenance"]["g_guard"],
        "taps": int(taps), "delay": int(delay),
        "export": {"text": e["text_path"], "text_sha256": sha256_file(e["text_path"]),
                   "cells_sha256": sha256_file(e["cells_path"]),
                   "artifact_sha": e["shas"],
                   "offdiag_nonzero_gt_1e-3": e["report"]["cells_offdiag_nonzero_gt_1e-3"]},
        "plan": {"path": os.path.abspath(p["plan"]), "sha256": sha256_file(p["plan"]),
                 "compile_report": os.path.abspath(p["compile_report"]),
                 "compile_report_sha256": sha256_file(p["compile_report"])},
        "plan_guard": {"ok": gv["ok"], "codes": gv["codes"], "plan_sha256": gv["metrics"].get("plan_sha256")},
        "not_run": ["board apply", "acoustic remeasure", "T6/T67/T68/T69 acceptance", "holdout"],
    }
    reg = registry or pv.synthetic_registry()
    syn_plans = set((reg.get("synthetic_plan_sha256") or {}).values())
    manifest["synthetic_differs"] = manifest["plan"]["sha256"] not in syn_plans and \
        manifest["design_g_sha256"] not in set((reg.get("synthetic_design_sha256") or {}).values())
    with open(os.path.join(out, "provenance-manifest.json"), "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    return manifest


def main(argv=None):
    ap = argparse.ArgumentParser(description="guarded REAL-room MIMO G design -> T66 plan (task-252)")
    ap.add_argument("--arm", default=None, help="measured arm dir (meta/session.json + mics)")
    ap.add_argument("--h", default=None, help="existing h_estimate.npz from this module")
    ap.add_argument("--out", required=True)
    ap.add_argument("--taps", type=int, default=4096, help="L_G (T66 packer max_taps=4096)")
    ap.add_argument("--delay", type=int, default=2048)
    ap.add_argument("--sweep-seconds", type=float, default=8.0)
    ap.add_argument("--level-db", type=float, default=-15.58)
    ap.add_argument("--design-only", action="store_true",
                    help="run the guarded H->G design step only (no T66 export/pack)")
    ap.add_argument("--json-out", default=None)
    args = ap.parse_args(argv)
    params = dict(DEFAULT_PARAMS, sweep_seconds=args.sweep_seconds, level_db=args.level_db)
    try:
        if args.design_only:
            m = design_only(arm_dir=args.arm, h_npz=args.h, out=args.out,
                            taps=args.taps, delay=args.delay, params=params)
        else:
            m = run_live_design(arm_dir=args.arm, h_npz=args.h, out=args.out,
                                taps=args.taps, delay=args.delay, params=params)
    except pv.ProvenanceRefusal as exc:
        out = {"ok": False, "refused": exc.codes, "detail": exc.detail}
        print(json.dumps(out, ensure_ascii=False, indent=2))
        if args.json_out:
            with open(args.json_out, "w", encoding="utf-8") as fh:
                json.dump(out, fh, ensure_ascii=False, indent=2)
        return 3
    if args.design_only:
        print(json.dumps(m, ensure_ascii=False, indent=2))
    else:
        print(json.dumps({"ok": True, "plan_sha256": m["plan"]["sha256"],
                          "design_g_sha256": m["design_g_sha256"],
                          "synthetic_differs": m["synthetic_differs"],
                          "manifest": os.path.abspath(os.path.join(args.out, "provenance-manifest.json"))},
                         ensure_ascii=False, indent=2))
    if args.json_out:
        with open(args.json_out, "w", encoding="utf-8") as fh:
            json.dump(m, fh, ensure_ascii=False, indent=2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
