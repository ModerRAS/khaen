#!/usr/bin/env python3
"""Guarded 12k design / [mimo12k] artifact packaging + plan provenance (task-255).

Why (2026-11-20 board window): release-v11 ships the 12k correction branch, and the
window would render/apply the task-253 conservative 12k G/plan.  That artifact was
designed from a real remeasure identity-arm H, but nothing in the delivery chain wrote a
provenance record, bound the f32 coefficients to the f64 design output, or bound the plan
to the artifact -- so the 48 kHz guard could prove nothing about it.  This module is the
guarded generation/packaging path:

  redesign_artifact()   re-runs the delivered design objective from the measured H
                        (deterministic: reproduces g_pure_sha256 f0a4a689...) and writes
                        a self-contained guarded artifact directory:
                          params.json (byte-identical to the delivered artifact)
                          g12_v2_4096.f32 + g12_v2_4096.f64.npz (f32<->f64 content binding)
                          resamp95.f32, design-summary.json, design-script.py
                          provenance.json (tspi-mimo-12k-artifact-provenance/v1)
                        then it must pass provenance.g12_artifact_guard() or raise.
  make_synthetic_fixture()  builds the negative fixture from the dry-run synthetic H;
                        the guard must REFUSE it (markers, dry-run source, content sha).
  stamp_plan()          copies the delivered direct-branch plan and writes
                        plan-provenance.json binding plan bytes <-> compile report <->
                        guarded artifact; must pass provenance.g12_plan_guard().

No board, no sound, no git.  Offline evidence under rt-stack/evidence/mimo-12k-provenance/.
"""
from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import time

import numpy as np

try:  # package or direct import
    from . import provenance as pv
except ImportError:  # pragma: no cover
    import provenance as pv

KIT = os.path.dirname(os.path.abspath(__file__))
PROJECT = os.path.abspath(os.path.join(KIT, "..", "..", "..", ".."))
EVIDENCE = os.path.join(PROJECT, "rt-stack", "evidence")
EVID_12K = os.path.join(EVIDENCE, "mimo-12k-v2")
EVID_PROV12K = os.path.join(EVIDENCE, "mimo-12k-provenance")

DELIVERED_ARTIFACT = os.path.join(EVID_12K, "artifacts")
DELIVERED_SUMMARY = os.path.join(EVID_12K, "design", "summary.json")
DELIVERED_DESIGN_SCRIPT = os.path.join(EVID_12K, "scripts", "design_12k.py")
DELIVERED_PLAN_DIR = os.path.join(EVID_12K, "runtime", "plan-direct-12k")
REAL_H = os.path.join(PROJECT, "scratch", "lead", "mimo-v2", "probe", "real", "h_identity.npz")
DRYRUN_H = os.path.join(EVIDENCE, "mimo-pilot-3mic", "prep", "dryrun", "out", "h",
                        "h_estimate.npz")

GUARDED_ARTIFACT = os.path.join(EVID_PROV12K, "artifact-12k-guarded")
GUARDED_PLAN = os.path.join(EVID_PROV12K, "plan-12k-guarded")
SYNTHETIC_FIXTURE = os.path.join(EVID_PROV12K, "synthetic-12k")

DESIGN_PARAMS = {"taps": pv.A12_TAPS, "d12": pv.A12_D12, "lam_rel": 3.0, "ratio": 0.90}
KNOWN_12K_G_PURE_SHA = "f0a4a689c75a7e06cfc66d8b62ce0db6173337a32e1ded27e9ca96e183804359"
KNOWN_12K_G_FIR_SHA = "def35047979d2405847236ddadf2806ffda7704ac604fe95c488a2a7e71d6636"


def sha_file(path):
    return pv.sha256_file(path)


def write_json(path, obj, indent=2):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(obj, fh, ensure_ascii=False, indent=indent)
        fh.write("\n")
    return path


def _copy(src, dst):
    os.makedirs(os.path.dirname(os.path.abspath(dst)), exist_ok=True)
    with open(src, "rb") as fh:
        data = fh.read()
    with open(dst, "wb") as fh:
        fh.write(data)
    return dst


def _now_utc():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def load_design_module(design_script=None):
    """Import the delivered task-253 design_12k.py (numeric objective is not forked)."""
    path = os.path.abspath(design_script or DELIVERED_DESIGN_SCRIPT)
    if not os.path.isfile(path):
        raise FileNotFoundError("delivered 12k design script not found: %s" % path)
    spec = importlib.util.spec_from_file_location("tspi_design12k_delivered", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _decimated_h12(mod, h):
    """48 kHz H (8 container slots / mic) -> 12 kHz IR + gated FR bundle."""
    hres = mod.design_resampler()
    h12 = {"freq": h["freq"], "mics": h["mics"], "H": {}, "irs": {},
           "source": h["source"], "sha256": h["sha256"]}
    for mic in h["mics"]:
        dec = [mod.decimate(np.asarray(h["irs"][mic][kk], dtype=np.float64), hres)
               for kk in range(h["irs"][mic].shape[0])]
        n12 = min(len(x) for x in dec)
        h12["irs"][mic] = np.zeros((len(dec), n12))
        h12["H"][mic] = np.zeros((len(dec), len(h["freq"])), dtype=np.complex128)
        for kk, ir in enumerate(dec):
            h12["irs"][mic][kk] = ir[:n12]
            h12["H"][mic][kk] = mod.fr_from_ir12(h12["irs"][mic][kk], h["freq"])
    return hres, h12


def design_g12(h_npz=None, design_script=None, taps=pv.A12_TAPS, d12=pv.A12_D12,
               lam_rel=DESIGN_PARAMS["lam_rel"], ratio=DESIGN_PARAMS["ratio"]):
    """Run the delivered 12k objective from a measured H; return f64 G / g_pure + hashes."""
    mod = load_design_module(design_script)
    h_path = os.path.abspath(h_npz or REAL_H)
    h = mod.m.load_h_npz(h_path)
    hres, h12 = _decimated_h12(mod, h)
    design = mod.design_v2_12k(h12, int(taps), int(d12), int(mod.FS12),
                               lam=float(lam_rel), ratio=float(ratio))
    G12 = np.asarray(design["G_fir"], dtype=np.float64)
    g_pure = np.array(G12, dtype=np.float64)
    for j in range(len(pv.A12_PARTICIPANTS)):
        g_pure[j, j, int(d12)] -= 1.0
    return {"mod": mod, "h": h, "h12": h12, "resampler": hres, "design": design,
            "G_fir": G12, "g_pure": g_pure,
            "G_fir_sha256": mod.m.sha256_arr(G12),
            "g_pure_sha256": mod.m.sha256_arr(g_pure),
            "taps": int(taps), "d12": int(d12),
            "lam_rel": float(lam_rel), "ratio": float(ratio)}


def _f32_bytes(arr):
    return np.ascontiguousarray(np.asarray(arr, dtype="<f4")).tobytes()


def _write_artifact_files(out_dir, d, params_src, summary_src, design_script, resamp_src=None):
    """Write the guarded artifact payload; returns the artifact provenance 'artifact' block."""
    os.makedirs(out_dir, exist_ok=True)
    g32 = _f32_bytes(d["g_pure"])
    g_file = "g12_v2_4096.f32"
    with open(os.path.join(out_dir, g_file), "wb") as fh:
        fh.write(g32)
    f64_file = "g12_v2_4096.f64.npz"
    np.savez_compressed(os.path.join(out_dir, f64_file),
                        g_pure=np.asarray(d["g_pure"], dtype=np.float64),
                        taps=np.array(d["taps"]), d12=np.array(d["d12"]),
                        participants=np.array(pv.A12_PARTICIPANTS))
    res_file = "resamp95.f32"
    if resamp_src and os.path.isfile(resamp_src):
        _copy(resamp_src, os.path.join(out_dir, res_file))
    else:
        with open(os.path.join(out_dir, res_file), "wb") as fh:
            fh.write(_f32_bytes(d["resampler"]))
    _copy(params_src, os.path.join(out_dir, "params.json"))
    _copy(summary_src, os.path.join(out_dir, "design-summary.json"))
    _copy(design_script, os.path.join(out_dir, "design-script.py"))
    g_up = pv._load_f32(os.path.join(out_dir, g_file),
                        (len(pv.A12_PARTICIPANTS), len(pv.A12_PARTICIPANTS), d["taps"]))
    return {
        "g_file": g_file,
        "g_file_sha256": sha_file(os.path.join(out_dir, g_file)),
        "g_f32_content_sha256": pv._sha256_f64_array(g_up),
        "g_f64_npz": f64_file,
        "g_f64_npz_sha256": sha_file(os.path.join(out_dir, f64_file)),
        "g_pure_sha256": d["g_pure_sha256"],
        "g_fir_sha256": d["G_fir_sha256"],
        "resamp_file": res_file,
        "resamp_sha256": sha_file(os.path.join(out_dir, res_file)),
        "taps": d["taps"],
        "d12": d["d12"],
        "group": pv.A12_GROUP,
        "dsys_samples": pv.A12_DSYS,
        "participants": list(pv.A12_PARTICIPANTS),
        "schema": pv.A12_ARTIFACT_SCHEMA,
    }


def _delivered_summary_binding(delivered_summary):
    summary = pv.load_json(delivered_summary, None)
    if not isinstance(summary, dict):
        return None
    v2 = summary.get("v2_12k") or {}
    return {"summary_json": delivered_summary,
            "summary_sha256": sha_file(delivered_summary),
            "g_pure_sha256": v2.get("g_pure_sha256"),
            "G_fir_sha256": v2.get("G_fir_sha256"),
            "h_sha256": (summary.get("inputs") or {}).get("h_sha256"),
            "params": dict(v2.get("params") or {}),
            "taps": v2.get("taps"),
            "d12": (summary.get("delay") or {}).get("d12"),
            "claim": summary.get("claim")}


def redesign_artifact(out_dir=None, h_npz=None, design_script=None, taps=pv.A12_TAPS,
                      d12=pv.A12_D12, lam_rel=DESIGN_PARAMS["lam_rel"],
                      ratio=DESIGN_PARAMS["ratio"], delivered_artifact=None,
                      delivered_summary=None, expect_g_pure_sha=None,
                      expect_g_fir_sha=None, strict_delivered=True):
    """Re-design the 12k G from the measured H and package it with provenance, fail closed."""
    out_dir = os.path.abspath(out_dir or GUARDED_ARTIFACT)
    delivered_artifact = os.path.abspath(delivered_artifact or DELIVERED_ARTIFACT)
    delivered_summary = os.path.abspath(delivered_summary or DELIVERED_SUMMARY)
    design_script = os.path.abspath(design_script or DELIVERED_DESIGN_SCRIPT)
    d = design_g12(h_npz, design_script, taps, d12, lam_rel, ratio)
    if expect_g_pure_sha and d["g_pure_sha256"] != expect_g_pure_sha:
        raise pv.ProvenanceRefusal("a12_reproduction_mismatch", {
            "expected_g_pure_sha256": expect_g_pure_sha,
            "actual_g_pure_sha256": d["g_pure_sha256"],
            "detail": "the delivered 12k design objective did not reproduce; do not issue"})
    if expect_g_fir_sha and d["G_fir_sha256"] != expect_g_fir_sha:
        raise pv.ProvenanceRefusal("a12_reproduction_mismatch", {
            "expected_G_fir_sha256": expect_g_fir_sha,
            "actual_G_fir_sha256": d["G_fir_sha256"]})
    h12v = pv.h12_source_check(d["h"]["source"])
    if not h12v["ok"]:
        raise pv.ProvenanceRefusal(h12v["codes"], {"h12_guard": h12v})
    params_src = os.path.join(delivered_artifact, "params.json")
    params = pv.load_json(params_src, None)
    if not isinstance(params, dict):
        raise pv.ProvenanceRefusal("a12_delivered_missing", {
            "detail": "delivered artifact params.json not found: %s" % params_src})
    if pv._as_int(params.get("taps")) != d["taps"] or pv._as_int(params.get("d12")) != d["d12"]:
        raise pv.ProvenanceRefusal("a12_delivered_mismatch", {
            "detail": "delivered params taps/d12 differ from the requested design"})
    # numeric parity against the delivered design + artifact bytes
    delivery = _delivered_summary_binding(delivered_summary)
    parity = {"delivered_artifact_dir": delivered_artifact,
              "delivered_summary_json": delivered_summary,
              "delivered_g_pure_sha256": (delivery or {}).get("g_pure_sha256"),
              "delivered_G_fir_sha256": (delivery or {}).get("G_fir_sha256"),
              "reproduced_g_pure_sha256": d["g_pure_sha256"],
              "reproduced_G_fir_sha256": d["G_fir_sha256"]}
    if delivery:
        if delivery.get("g_pure_sha256") and delivery["g_pure_sha256"] != d["g_pure_sha256"]:
            raise pv.ProvenanceRefusal("a12_delivered_mismatch", {
                "detail": "delivered summary g_pure_sha256 != reproduced", "parity": parity})
        if delivery.get("G_fir_sha256") and delivery["G_fir_sha256"] != d["G_fir_sha256"]:
            raise pv.ProvenanceRefusal("a12_delivered_mismatch", {
                "detail": "delivered summary G_fir_sha256 != reproduced", "parity": parity})
        if delivery.get("h_sha256") and delivery["h_sha256"] != d["h"]["sha256"]:
            raise pv.ProvenanceRefusal("a12_delivered_mismatch", {
                "detail": "delivered summary H sha256 != the measured H being redesigned",
                "parity": parity})
    parity["matched"] = True
    delivered_g = os.path.join(delivered_artifact, str(params.get("g_file") or "g12_v2_4096.f32"))
    delivered_r = os.path.join(delivered_artifact,
                               str(params.get("resamp_file") or "resamp95.f32"))
    if os.path.isfile(delivered_g):
        got_sha = hashlib.sha256(_f32_bytes(d["g_pure"])).hexdigest()
        parity["delivered_g_file_sha256"] = sha_file(delivered_g)
        parity["reproduced_g_f32_sha256"] = got_sha
        if strict_delivered and got_sha != parity["delivered_g_file_sha256"]:
            raise pv.ProvenanceRefusal("a12_delivered_mismatch", {
                "detail": "reproduced g f32 bytes differ from the delivered artifact", "parity": parity})
    art = _write_artifact_files(out_dir, d, params_src, delivered_summary, design_script,
                                resamp_src=delivered_r if os.path.isfile(delivered_r) else None)
    params_out_sha = sha_file(os.path.join(out_dir, "params.json"))
    prov = {
        "schema": pv.A12_ART_PROV_SCHEMA,
        "task": "task-255",
        "live": True,
        "synthetic": False,
        "created_utc": _now_utc(),
        "claim": "REAL measured-H 12k conservative variant (taps=%d, D12=%d, group=%d) "
                 "reproduced and packaged through the task-255 guarded path; measured arm "
                 "data only; offline only (no board run, no acoustic acceptance)"
                 % (d["taps"], d["d12"], pv.A12_GROUP),
        "artifact": dict(art, dir=out_dir, params_json="params.json",
                         params_sha256=params_out_sha),
        "design": {
            "script": "design-script.py",
            "script_source": design_script,
            "script_sha256": sha_file(os.path.join(out_dir, "design-script.py")),
            "design_params": {"taps": d["taps"], "d12": d["d12"],
                              "lam_rel": d["lam_rel"], "ratio": d["ratio"]},
            "summary_json": "design-summary.json",
            "summary_sha256": sha_file(os.path.join(out_dir, "design-summary.json")),
            "aggregate_model": {k: d["design"]["aggregate"].get(k) for k in
                                ("rt60_n", "rt60_median_change_pct", "rt60_improved_pct",
                                 "rt60_worsened_pct", "edc100_median_change_db",
                                 "level_min_db", "level_max_db", "regression_count")},
            "reproduction": parity,
            "delivered_summary": delivery,
        },
        "h": {"npz": d["h"]["source"], "source": d["h"]["source"],
              "sha256": d["h"]["sha256"], "format": h12v["metrics"].get("format"),
              "guard_schema": pv.A12_H_GUARD_SCHEMA, "guard": h12v},
        "guard": {"ok": True, "codes": [], "module": "rt-stack/tools/mimo-prep/mimo_kit/provenance.py",
                  "policy": {"g12_match_atol": pv.A12_G_MATCH_ATOL,
                             "ir_max_peak_ms": pv.IR_MAX_PEAK_MS,
                             "ir_max_peak_spread_ms": pv.IR_MAX_PEAK_SPREAD_MS}},
        "not_run": ["board apply", "real-time host run", "acoustic remeasure",
                    "T6/T67/T68/T69 acceptance", "holdout"],
    }
    write_json(os.path.join(out_dir, "provenance.json"), prov)
    verdict = pv.g12_artifact_guard(out_dir)
    if not verdict["ok"]:
        raise pv.ProvenanceRefusal(verdict["codes"],
                                   {"artifact_guard": verdict, "artifact_dir": out_dir})
    prov["guard"] = {"ok": True, "codes": [],
                     "module": "rt-stack/tools/mimo-prep/mimo_kit/provenance.py",
                     "artifact_guard_metrics": {k: verdict["metrics"].get(k) for k in
                                                ("params_sha256", "g_file_sha256",
                                                 "g_f32_content_sha256", "g_pure_sha256",
                                                 "g_f64_npz_sha256", "resamp_sha256",
                                                 "h_sha256", "offdiag_peak")}}
    write_json(os.path.join(out_dir, "provenance.json"), prov)
    final = pv.g12_artifact_guard(out_dir)
    if not final["ok"]:  # pragma: no cover - the passport rewrite must not invalidate itself
        raise pv.ProvenanceRefusal(final["codes"], {"artifact_guard": final})
    return {"ok": True, "artifact_dir": out_dir, "guard": final,
            "provenance": prov, "g_pure_sha256": d["g_pure_sha256"],
            "G_fir_sha256": d["G_fir_sha256"], "parity": parity}


def make_synthetic_fixture(out_dir=None, dryrun_h=None, design_script=None,
                           taps=pv.A12_TAPS, d12=pv.A12_D12):
    """Generate the negative fixture from the dry-run synthetic H (the guard must refuse it)."""
    out_dir = os.path.abspath(out_dir or SYNTHETIC_FIXTURE)
    h_path = os.path.abspath(dryrun_h or DRYRUN_H)
    if not os.path.isfile(h_path):
        raise FileNotFoundError("dry-run synthetic H not found: %s" % h_path)
    mod = load_design_module(design_script)
    z = pv.load_npz(h_path)
    mics = [str(x) for x in z["mic_names"]]
    Hn = np.asarray(z["H_raw"], dtype=np.complex128)  # (nm, 7, nf)
    irs = np.asarray(z["irs"], dtype=np.float64)      # (nm, 7, ns)
    freq = np.asarray(z["freq"], dtype=np.float64)
    h = {"freq": freq, "mics": mics, "H": {}, "irs": {},
         "source": h_path, "sha256": sha_file(h_path)}
    for mi, mic in enumerate(mics):
        H8 = np.zeros((8, Hn.shape[2]), dtype=np.complex128)
        ir8 = np.zeros((8, irs.shape[2]), dtype=np.float64)
        for row, (_nm, slot) in enumerate(pv.rd.OUTPUTS):
            H8[slot] = Hn[mi, row, :]
            ir8[slot] = irs[mi, row, :]
        h["H"][mic] = H8
        h["irs"][mic] = ir8
    hres, h12 = _decimated_h12(mod, h)
    design = mod.design_v2_12k(h12, int(taps), int(d12), int(mod.FS12),
                               lam=float(DESIGN_PARAMS["lam_rel"]),
                               ratio=float(DESIGN_PARAMS["ratio"]))
    G12 = np.asarray(design["G_fir"], dtype=np.float64)
    g_pure = np.array(G12, dtype=np.float64)
    for j in range(len(pv.A12_PARTICIPANTS)):
        g_pure[j, j, int(d12)] -= 1.0
    d = {"mod": mod, "h": h, "h12": h12, "resampler": hres, "design": design,
         "G_fir": G12, "g_pure": g_pure, "G_fir_sha256": mod.m.sha256_arr(G12),
         "g_pure_sha256": mod.m.sha256_arr(g_pure), "taps": int(taps), "d12": int(d12),
         "lam_rel": DESIGN_PARAMS["lam_rel"], "ratio": DESIGN_PARAMS["ratio"]}
    os.makedirs(out_dir, exist_ok=True)
    params = {
        "schema": pv.A12_ARTIFACT_SCHEMA, "fs_hz": pv.A12_FS_48,
        "lowrate_hz": pv.A12_FS_12, "group": pv.A12_GROUP,
        "participants": list(pv.A12_PARTICIPANTS), "taps": d["taps"], "d12": d["d12"],
        "resamp_taps": pv.A12_RESAMP_TAPS, "resamp_delay": pv.A12_RESAMP_DELAY,
        "dsys_samples": pv.A12_DSYS, "g_file": "g12_v2_4096.f32",
        "resamp_file": "resamp95.f32",
        "g_layout": "g[j][i][tap] f32le, j,i in participants order",
        "g_sha256": d["g_pure_sha256"], "synthetic": True,
    }
    write_json(os.path.join(out_dir, "params.json"), params, indent=1)
    g_up = _f32_bytes(d["g_pure"])
    with open(os.path.join(out_dir, "g12_v2_4096.f32"), "wb") as fh:
        fh.write(g_up)
    np.savez_compressed(os.path.join(out_dir, "g12_v2_4096.f64.npz"),
                        g_pure=np.asarray(d["g_pure"], dtype=np.float64),
                        taps=np.array(d["taps"]), d12=np.array(d["d12"]),
                        participants=np.array(pv.A12_PARTICIPANTS))
    with open(os.path.join(out_dir, "resamp95.f32"), "wb") as fh:
        fh.write(_f32_bytes(hres))
    h12v = pv.h12_source_check(h_path)
    g32 = pv._load_f32(os.path.join(out_dir, "g12_v2_4096.f32"), (5, 5, d["taps"]))
    prov = {
        "schema": pv.A12_ART_PROV_SCHEMA, "task": "task-255", "live": False,
        "synthetic": True, "created_utc": _now_utc(),
        "claim": "SYNTHETIC negative fixture: 12k G designed from the dry-run synthetic H "
                 "through the same objective; must be REFUSED by the 12k guard",
        "artifact": {"dir": out_dir, "params_json": "params.json",
                     "params_sha256": sha_file(os.path.join(out_dir, "params.json")),
                     "g_file": "g12_v2_4096.f32",
                     "g_file_sha256": sha_file(os.path.join(out_dir, "g12_v2_4096.f32")),
                     "g_f32_content_sha256": pv._sha256_f64_array(g32),
                     "g_f64_npz": "g12_v2_4096.f64.npz",
                     "g_f64_npz_sha256": sha_file(os.path.join(out_dir, "g12_v2_4096.f64.npz")),
                     "g_pure_sha256": d["g_pure_sha256"], "g_fir_sha256": d["G_fir_sha256"],
                     "resamp_file": "resamp95.f32",
                     "resamp_sha256": sha_file(os.path.join(out_dir, "resamp95.f32")),
                     "taps": d["taps"], "d12": d["d12"], "group": pv.A12_GROUP,
                     "dsys_samples": pv.A12_DSYS,
                     "participants": list(pv.A12_PARTICIPANTS),
                     "schema": pv.A12_ARTIFACT_SCHEMA},
        "design": {"script": "design-script.py",
                   "script_source": os.path.abspath(design_script or DELIVERED_DESIGN_SCRIPT),
                   "design_params": {"taps": d["taps"], "d12": d["d12"],
                                     "lam_rel": d["lam_rel"], "ratio": d["ratio"]},
                   "summary_json": None, "dryrun_source": h_path},
        "h": {"npz": h_path, "source": h_path, "sha256": d["h"]["sha256"],
              "format": h12v["metrics"].get("format"), "guard_schema": pv.A12_H_GUARD_SCHEMA,
              "guard": h12v},
        "guard": {"ok": False, "codes": [], "note": "negative fixture; must not pass"},
        "not_run": ["everything: this fixture exists only to prove the guard refuses it"],
    }
    write_json(os.path.join(out_dir, "provenance.json"), prov)
    verdict = pv.g12_artifact_guard(out_dir)
    if verdict["ok"]:  # pragma: no cover - a synthetic fixture passing the guard is a bug
        raise AssertionError("synthetic 12k fixture unexpectedly passed the provenance guard")
    return {"ok": True, "fixture_dir": out_dir, "guard": verdict,
            "g_pure_sha256": d["g_pure_sha256"]}


def stamp_plan(plan_dir=None, artifact_dir=None, out_dir=None):
    """Copy the delivered 12k plan and bind it to the guarded artifact with provenance."""
    plan_dir = os.path.abspath(plan_dir or DELIVERED_PLAN_DIR)
    artifact_dir = os.path.abspath(artifact_dir or GUARDED_ARTIFACT)
    out_dir = os.path.abspath(out_dir or GUARDED_PLAN)
    src_plan = os.path.join(plan_dir, "plan.bin")
    src_rep = os.path.join(plan_dir, "compile-report.json")
    if not os.path.isfile(src_plan) or not os.path.isfile(src_rep):
        raise pv.ProvenanceRefusal("a12_delivered_missing",
                                   {"detail": "plan dir must hold plan.bin + compile-report.json",
                                    "plan_dir": plan_dir})
    av = pv.g12_artifact_guard(artifact_dir)
    if not av["ok"]:
        raise pv.ProvenanceRefusal(av["codes"],
                                   {"detail": "guarded artifact must pass before plan stamping",
                                    "artifact_guard": av})
    os.makedirs(out_dir, exist_ok=True)
    _copy(src_plan, os.path.join(out_dir, "plan.bin"))
    _copy(src_rep, os.path.join(out_dir, "compile-report.json"))
    design_rep = os.path.join(plan_dir, "design-report.json")
    if os.path.isfile(design_rep):
        _copy(design_rep, os.path.join(out_dir, "design-report.json"))
    plan_sha = sha_file(os.path.join(out_dir, "plan.bin"))
    rep_sha = sha_file(os.path.join(out_dir, "compile-report.json"))
    rep = pv.load_json(os.path.join(out_dir, "compile-report.json"), {}) or {}
    claimed = ((rep.get("plan") or {}).get("plan_sha256") or rep.get("plan_sha256"))
    if claimed and str(claimed).lower() != plan_sha.lower():
        raise pv.ProvenanceRefusal("a12_plan_sha_mismatch", {
            "compile_report_plan_sha256": claimed, "plan_sha256": plan_sha})
    am = av["metrics"]
    prov = {
        "schema": pv.A12_PLAN_PROV_SCHEMA, "task": "task-255", "live": True,
        "synthetic": False, "created_utc": _now_utc(),
        "claim": "Guarded 12k plan: direct-branch plan (G=I, P=I, D=deltasys) whose "
                 "[mimo12k] artifact is the provenance-guarded measured-H 12k G; offline "
                 "only (no board run, no acoustic acceptance)",
        "plan_sha256": plan_sha, "plan_bytes": os.path.getsize(os.path.join(out_dir, "plan.bin")),
        "compile_report_sha256": rep_sha,
        "design_source": {"design_report_schema": rep.get("design_schema"),
                          "design_claim": rep.get("design_claim"),
                          "compile_report_plan_sha256": claimed},
        "mimo12k_artifact": {
            "artifact_dir": artifact_dir, "params_sha256": am.get("params_sha256"),
            "g_file_sha256": am.get("g_file_sha256"),
            "g_f32_content_sha256": am.get("g_f32_content_sha256"),
            "g_pure_sha256": am.get("g_pure_sha256"),
            "resamp_sha256": am.get("resamp_sha256"), "h_sha256": am.get("h_sha256"),
            "provenance_json": os.path.join(artifact_dir, "provenance.json")},
        "artifact_guard": {"ok": True, "codes": []},
        "not_run": ["board render/apply", "real-time host run", "acoustic remeasure"],
    }
    write_json(os.path.join(out_dir, "plan-provenance.json"), prov)
    verdict = pv.g12_plan_guard(out_dir, artifact_dir=artifact_dir)
    if not verdict["ok"]:
        raise pv.ProvenanceRefusal(verdict["codes"],
                                   {"plan_guard": verdict, "plan_dir": out_dir})
    prov["artifact_guard"] = {"ok": True, "codes": [],
                              "artifact_guard_metrics": {k: am.get(k) for k in
                                                         ("params_sha256", "g_file_sha256",
                                                          "g_f32_content_sha256",
                                                          "g_pure_sha256", "resamp_sha256",
                                                          "h_sha256")},
                              "plan_guard_metrics": {"plan_sha256": verdict["metrics"].get("plan_sha256"),
                                                     "compile_report_sha256": verdict["metrics"].get("compile_report_sha256")}}
    write_json(os.path.join(out_dir, "plan-provenance.json"), prov)
    final = pv.g12_plan_guard(out_dir, artifact_dir=artifact_dir)
    if not final["ok"]:  # pragma: no cover
        raise pv.ProvenanceRefusal(final["codes"], {"plan_guard": final})
    return {"ok": True, "plan_dir": out_dir, "guard": final, "provenance": prov}
