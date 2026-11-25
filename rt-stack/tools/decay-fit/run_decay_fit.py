#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""decay-fit CLI: offline segmented / double-exponential analysis of EDC curves.

Reads already-captured UMIK .raw files (48 kHz S24_3LE 2ch, 30 s sweep), deconvolves
them with the frozen ir-diag chain, and fits SINGLE / TWO / THREE piecewise-linear
models plus a double exponential on the -5..-25 dB EDC window.  The segment count is
selected automatically (BIC/dR2 + identifiability) and an opt-in segmented gate and
parameter-suggestion JSON are produced.  No sound card, no playback, no apply.

    py -3 run_decay_fit.py --run-dir <dir with RAW_r1.raw ...> \
        --tags RAW_r1,S2_r1,RAW_r2,S2_r2,RAW_r3,S2_r3 --out <evidence dir> \
        [--bands 40,50,63,80,100,125,160,200] [--eps-ppm -60.4] [--validate analysis.json]
"""
import argparse
import datetime
import hashlib
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from decayfit import edc as E          # noqa: E402
from decayfit import gates as G        # noqa: E402
from decayfit import models as MF      # noqa: E402
from decayfit import objective as O    # noqa: E402
from decayfit import report as R       # noqa: E402

from irdiag import metrics as M        # noqa: E402

DEFAULT_TAGS = ["RAW_r1", "S2_r1", "RAW_r2", "S2_r2", "RAW_r3", "S2_r3"]
DEFAULT_BANDS = [40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0]
FIG_BANDS = (125.0, 200.0)


def sha256_file(path, chunk=1 << 20):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(chunk), b""):
            h.update(b)
    return h.hexdigest()


def model_curve(fit, fs):
    """Evaluate the selected piecewise model on its fit window."""
    if not fit or fit.get("status") != "OK":
        return None
    fr = fit["fit_range"]
    sel = fit["selected"]["n_seg"]
    m = fit["models"][str(sel)]
    b = m["params"].get("level_at_window_start_db")
    if b is None:
        return None
    t0 = fr["t_lo_s"]
    t = t0 + np.arange(fr["n"], dtype=np.float64) / fs
    u = t - t0
    slopes = m["slopes_db_per_s"]
    y = b + slopes[0] * u
    for k, tb in enumerate(m["breaks_s"]):
        y = y + (slopes[k + 1] - slopes[k]) * np.maximum(u - tb, 0.0)
    return t, y


def biexp_curve(bx, fit, fs):
    if not bx or bx.get("status") == "FIT_FAILED" or bx.get("offset_db") is None:
        return None
    if not fit or fit.get("status") != "OK":
        return None
    fr = fit["fit_range"]
    t0 = fr["t_lo_s"]
    t = t0 + np.arange(fr["n"], dtype=np.float64) / fs
    u = t - t0
    e = bx["a1"] * np.exp(-u / bx["tau1_s"]) + bx["a2"] * np.exp(-u / bx["tau2_s"])
    return t, bx["offset_db"] + 10.0 * np.log10(np.maximum(e, 1e-12))


def analyze_capture(run_dir, tag, bands, eps_ppm):
    path = os.path.join(run_dir, tag + ".raw")
    if not os.path.exists(path):
        raise SystemExit("missing capture: %s" % path)
    dec = E.deconvolve_capture(path, eps_ppm=eps_ppm)
    meta = {k: v for k, v in dec.items() if k not in ("ir_dec", "ir_dec_seg")}
    fs = dec["fs_dec"]
    fs_seg = dec["fs_dec_seg"]
    res = {}
    for b in bands:
        pl = E.band_edc(dec, b, noise="plateau")
        fr = E.band_edc_frozen(dec, b)
        fit_pl = MF.fit_piecewise(pl["edc_db"], fs, noise_db=pl["edc_noise_db"])
        # frozen-estimator sensitivity fit only for the two target bands (runtime)
        fit_fr = (MF.fit_piecewise(fr["edc_db"], fs_seg, noise_db=fr["edc_noise_db"])
                  if b in FIG_BANDS else None)
        std_fr = M.t20_fit(fr["edc_db"], fs_seg, fr["edc_noise_db"])
        std_pl = M.t20_fit(pl["edc_db"], fs, pl["edc_noise_db"])
        rec = {
            "band_hz": float(b),
            "edc": {k: pl[k] for k in ("noise_method", "p_noise", "plateau_spread_db",
                                       "dr_db", "valid_n", "valid_edc_s", "edc100_db",
                                       "ir_energy_db", "ir_energy_0_100ms_db", "fs", "window_s")},
            "frozen_control": {"t20_s": std_fr["t20_s"], "r2": std_fr["r2"],
                               "slope_db_per_s": std_fr.get("slope_db_per_s"),
                               "t_lo_s": std_fr.get("t_lo_s"), "t_hi_s": std_fr.get("t_hi_s"),
                               "reason": std_fr.get("reason")},
            "plateau_control": {"t20_s": std_pl["t20_s"], "r2": std_pl["r2"],
                                "t_lo_s": std_pl.get("t_lo_s"), "t_hi_s": std_pl.get("t_hi_s"),
                                "reason": std_pl.get("reason")},
            "plateau_fit": fit_pl,
            "frozen_fit": fit_fr,
        }
        if b in FIG_BANDS:
            step = 4
            rec["_plot"] = {"t": (np.arange(0, pl["edc_db"].size, step) / fs).tolist(),
                            "y": pl["edc_db"][::step].tolist(),
                            "model": model_curve(fit_pl, fs),
                            "biexp": biexp_curve(fit_pl.get("biexp"), fit_pl, fs)}
        res[str(int(b))] = rec
    del dec
    return meta, res


def summarize_group(tag_list, per_repeat, band):
    """Aggregate one group (RAW / S2 / POOLED) for one band."""
    fits = [per_repeat[t][band]["plateau_fit"] for t in tag_list]
    std_t20 = [per_repeat[t][band]["frozen_control"]["t20_s"] for t in tag_list]
    std_r2 = [per_repeat[t][band]["frozen_control"]["r2"] for t in tag_list]
    e100 = [per_repeat[t][band]["edc"]["edc100_db"] for t in tag_list]
    gate_seg = G.segmented_gate(fits)
    return {"n_repeats": len(tag_list),
            "gate_standard": G.standard_t20_gate(std_t20, std_r2, e100),
            "gate_segmented": gate_seg,
            "gate_fixed_split": gate_seg.get("fixed_split"),
            "fits": [{"tag": t,
                      "selected": per_repeat[t][band]["plateau_fit"].get("selected"),
                      "t20_seg_s": per_repeat[t][band]["plateau_fit"].get("t20_seg_s"),
                      "segments": per_repeat[t][band]["plateau_fit"].get("segments"),
                      "knee": per_repeat[t][band]["plateau_fit"].get("knee"),
                      "curvature_knee": per_repeat[t][band]["plateau_fit"].get("curvature_knee"),
                      "fixed_split": per_repeat[t][band]["plateau_fit"].get("fixed_split"),
                      "r2": per_repeat[t][band]["plateau_fit"].get("r2"),
                      "biexp": per_repeat[t][band]["plateau_fit"].get("biexp")}
                     for t in tag_list]}


def attach_figure_data(groups, per_repeat, tags, bands):
    for b in FIG_BANDS:
        key = str(int(b))
        for group, gtags in (("RAW", [t for t in tags if t.startswith("RAW")]),
                             ("S2", [t for t in tags if t.startswith("S2")])):
            if not gtags or group not in groups:
                continue
            gd = groups[group][key]
            fs = per_repeat[gtags[0]][key]["edc"]["fs"]
            gd["edc_curves"] = [(t, np.array(per_repeat[t][key]["_plot"]["t"]),
                                 np.array(per_repeat[t][key]["_plot"]["y"])) for t in gtags]
            models = []
            bxs = []
            for t in gtags:
                p = per_repeat[t][key]["_plot"]
                if p.get("model") is not None:
                    models.append((t, p["model"][0], p["model"][1]))
                if p.get("biexp") is not None:
                    bxs.append((t, p["biexp"][0], p["biexp"][1]))
            gd["fit_overlay"] = {"model": models, "biexp": bxs}
            fxd = gd.get("gate_fixed_split") or {}
            gd["curv_knee_median_abs_s"] = (fxd.get("knee") or {}).get("knee_t_median_abs_s")


def strip_private(results):
    for tag, bands in results["per_repeat"].items():
        for key, rec in bands.items():
            rec.pop("_plot", None)
    for group, bands in results["bands"].items():
        for key, gd in bands.items():
            gd.pop("curv_knee_median_abs_s", None)
            gd.pop("edc_curves", None)
            gd.pop("fit_overlay", None)
            gd.pop("_tags", None)
            gd.pop("_fs", None)


def s2_effect(per_repeat, bands, tag_list):
    """Effect of the one-condition S2 PEQ (100 Hz -6 dB Q1.0) on each segment."""
    raw = [t for t in tag_list if t.startswith("RAW")]
    s2 = [t for t in tag_list if t.startswith("S2")]
    out = {}
    for b in bands:
        rec = {}
        for idx, name in ((0, "early"), (1, "late")):
            def med(group):
                vals = []
                for t in group:
                    ts = per_repeat[t][str(int(b))]["plateau_fit"].get("t20_seg_s") or []
                    if len(ts) > idx and ts[idx] is not None:
                        vals.append(ts[idx])
                return (float(np.median(vals)), float(max(vals) - min(vals))) if vals else (None, None)
            m_raw, r_raw = med(raw)
            m_s2, r_s2 = med(s2)
            if m_raw is None or m_s2 is None:
                rec["delta_t20_%s_s" % name] = None
                rec["repeatability_s"] = None
            else:
                rec["delta_t20_%s_s" % name] = float(m_s2 - m_raw)
                rec["repeatability_s"] = float(max(r_raw or 0.0, r_s2 or 0.0))
        for segname in ("early", "late"):
            def medf(group):
                vals = []
                for t in group:
                    fx = per_repeat[t][str(int(b))]["plateau_fit"].get("fixed_split") or {}
                    s = fx.get(segname)
                    if s and s.get("t20_s"):
                        vals.append(s["t20_s"])
                return (float(np.median(vals)), float(max(vals) - min(vals))) if vals else (None, None)
            fm_raw, fr_raw = medf(raw)
            fm_s2, fr_s2 = medf(s2)
            if fm_raw is not None and fm_s2 is not None:
                rec["delta_fixed_%s_s" % segname] = float(fm_s2 - fm_raw)
                rec["repeatability_fixed_%s_s" % segname] = float(
                    max(fr_raw or 0.0, fr_s2 or 0.0))
            else:
                rec["delta_fixed_%s_s" % segname] = None
                rec["repeatability_fixed_%s_s" % segname] = None
        out[float(b)] = rec
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--run-dir", required=True, help="directory with <tag>.raw captures")
    ap.add_argument("--tags", default=",".join(DEFAULT_TAGS))
    ap.add_argument("--bands", default=",".join("%g" % b for b in DEFAULT_BANDS))
    ap.add_argument("--out", required=True)
    ap.add_argument("--eps-ppm", type=float, default=E.DEFAULT_EPS_PPM)
    ap.add_argument("--sweep-s", type=float, default=E.SWEEP_S)
    ap.add_argument("--validate", default=None,
                    help="optional prior analysis.json to validate the frozen T20 control against")
    ap.add_argument("--no-figures", action="store_true")
    args = ap.parse_args(argv)

    tags = [t.strip() for t in args.tags.split(",") if t.strip()]
    bands = [float(b) for b in args.bands.split(",") if b.strip()]
    os.makedirs(args.out, exist_ok=True)
    os.makedirs(os.path.join(args.out, "csv"), exist_ok=True)
    os.makedirs(os.path.join(args.out, "figures"), exist_ok=True)

    inputs = []
    per_repeat = {}
    for tag in tags:
        print("[decay-fit] %s ..." % tag, flush=True)
        meta, bands_res = analyze_capture(args.run_dir, tag, bands, args.eps_ppm)
        per_repeat[tag] = bands_res
        inputs.append(dict(meta, tag=tag))

    groups = {}
    for group, gtags in (("RAW", [t for t in tags if t.startswith("RAW")]),
                         ("S2", [t for t in tags if t.startswith("S2")]),
                         ("POOLED", tags)):
        if not gtags:
            continue
        groups[group] = {str(int(b)): summarize_group(gtags, per_repeat, str(int(b)))
                         for b in bands}
    attach_figure_data(groups, per_repeat, tags, bands)

    # ---- objective weights (energy proxy = band IR energy, single-point relative)
    energy_db = {b: float(np.mean([per_repeat[t][str(int(b))]["edc"]["ir_energy_db"]
                                   for t in tags])) for b in bands}
    eff = s2_effect(per_repeat, bands, tags)
    band_results = {}
    gate_by_band = {}
    for b in bands:
        key = str(int(b))
        pooled = groups.get("POOLED", {}).get(key) or {}
        gate_by_band[b] = pooled.get("gate_segmented")
        reps = [per_repeat[t][key]["plateau_fit"] for t in tags]
        valid = [r for r in reps if r and r.get("status") == "OK"]
        rep = valid[len(valid) // 2] if valid else (valid[0] if valid else None)
        if rep is None:
            continue
        band_results[b] = {"band_hz": b,
                           "n_seg": (rep.get("selected") or {}).get("n_seg", 1),
                           "t20_seg_s": rep.get("t20_seg_s"),
                           "knee": rep.get("knee"),
                           "biexp": rep.get("biexp"),
                           "gate_status": (gate_by_band[b] or {}).get("status")}
    sugg = O.build_suggestions(band_results, gate_by_band, energy_db, eff)

    results = {
        "schema": "tspi-decay-fit/v1",
        "task": "decay-fit-20261121: 125/200 Hz two-stage decay fit (offline, no sound card)",
        "generated_at": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
        "tool": {"name": "decay-fit", "version": "1.0.0", "path": "rt-stack/tools/decay-fit"},
        "inputs": {
            "run_dir": os.path.abspath(args.run_dir),
            "captures": inputs,
            "eps_ppm": args.eps_ppm,
            "sweep_s": args.sweep_s,
            "deconvolution": "frozen ir-diag chain (irdiag.excitation + analysis.deconvolve_spectrum), "
                             "single common clock stretch for the device pair",
            "noise_primary": "plateau (>=4.5 s after peak inside the real capture); the frozen "
                             "200 ms last-window estimate is reported as control",
        },
        "bands_hz": bands,
        "fit_constants": {
            "FIT_LO_DB": MF.FIT_LO_DB, "FIT_HI_DB": MF.FIT_HI_DB,
            "EXTENT_HI_DB": MF.EXTENT_HI_DB,
            "FIT_NOISE_MARGIN_DB": MF.FIT_NOISE_MARGIN_DB,
            "MIN_SEG_SPAN_DB": MF.MIN_SEG_SPAN_DB, "MIN_SEG_MS": MF.MIN_SEG_MS,
            "BIC_MARGIN": MF.BIC_MARGIN, "DR2_MIN": MF.DR2_MIN,
            "SLOPE_RATIO_MIN": MF.SLOPE_RATIO_MIN, "SLOPE_DIFF_T_MIN": MF.SLOPE_DIFF_T_MIN,
            "KNEE_CI_MIN_MS": MF.KNEE_CI_MIN_MS, "KNEE_CI_FRAC": MF.KNEE_CI_FRAC,
            "BIEXP_TAU_RATIO_MIN": MF.BIEXP_TAU_RATIO_MIN,
            "BIEXP_WEIGHT_MIN": MF.BIEXP_WEIGHT_MIN, "BIEXP_RSE_MAX": MF.BIEXP_RSE_MAX,
        },
        "per_repeat": per_repeat,
        "bands": groups,
        "s2_effect": {str(int(b)): v for b, v in eff.items()},
        "objective": sugg,
        "honesty": {
            "offline_only": True,
            "sound_card_used": False,
            "changes_standard_T20_definition": False,
            "segmented_metrics_are_non_standard": True,
            "single_point_single_mic": True,
            "T20_early_late_definition": "slope of one piecewise segment -> T20_seg = -60/slope "
                                         "(same dB/s rule as T20, applied per segment); NOT T20",
        },
    }

    if args.validate:
        prior = json.load(open(args.validate, "r", encoding="utf-8"))
        mism = []
        maxd = {"t20": 0.0, "r2": 0.0}
        for tag in tags:
            g = tag.split("_")[0]
            grp = (prior.get("groups") or {}).get(g)
            if not grp:
                continue
            pr = next((r for r in grp.get("runs", []) if r.get("tag") == tag), None)
            if not pr:
                continue
            for b in bands:
                key = str(int(b))
                if key not in pr.get("bands", {}):
                    continue
                mine = per_repeat[tag][key]["frozen_control"]
                ref = pr["bands"][key]
                if mine["t20_s"] is None or ref.get("t20_s") is None:
                    continue
                d = abs(mine["t20_s"] - ref["t20_s"])
                dr = abs((mine["r2"] or 0.0) - (ref.get("r2") or 0.0))
                maxd["t20"] = max(maxd["t20"], d)
                maxd["r2"] = max(maxd["r2"], dr)
                if d > 2e-3 or dr > 5e-3:
                    mism.append([tag, key, mine["t20_s"], ref["t20_s"],
                                 mine["r2"], ref.get("r2")])
        results["validation"] = {
            "prior": os.path.abspath(args.validate),
            "max_abs_dt20_s": maxd["t20"], "max_abs_dr2": maxd["r2"],
            "tolerance": {"t20_s": 2e-3, "r2": 5e-3},
            "mismatches": mism,
            "note": "the frozen control uses exactly the frozen-chain 5 s segment + decimation "
                    "and must reproduce the prior analysis; the plateau estimator is the only "
                    "variant (whole-stream IR, late quiet-window noise floor)",
        }
        print("[decay-fit] validation: max |dT20| = %.6f s, max |dR2| = %.6f, mismatches=%d"
              % (maxd["t20"], maxd["r2"], len(mism)))

    if not args.no_figures:
        figs = R.make_figures(results, os.path.join(args.out, "figures"), FIG_BANDS)
        print("[decay-fit] figures:", [os.path.basename(p) for p in figs])
    strip_private(results)
    R.write_json(results, os.path.join(args.out, "results.json"))
    R.write_per_repeat_csv(results, os.path.join(args.out, "csv", "per_repeat.csv"))
    R.write_bands_csv(results, os.path.join(args.out, "csv", "bands.csv"))
    R.write_model_selection_csv(results, os.path.join(args.out, "csv", "model_selection.csv"))
    R.write_objective_csv(sugg, os.path.join(args.out, "csv", "objective_weights.csv"))
    R.write_json(sugg, os.path.join(args.out, "param-suggestions.json"))

    code_sha = {}
    for rel in ("run_decay_fit.py", "decayfit/edc.py", "decayfit/models.py",
                "decayfit/gates.py", "decayfit/objective.py", "decayfit/report.py"):
        code_sha[rel] = sha256_file(os.path.join(HERE, rel.replace("/", os.sep)))
    code_sha["ir-diag/irdiag/metrics.py"] = sha256_file(
        os.path.abspath(os.path.join(HERE, "..", "ir-diag", "irdiag", "metrics.py")))
    manifest = {"schema": "tspi-decay-fit-manifest/v1",
                "generated_at": results["generated_at"],
                "code_sha256": code_sha,
                "captures": [{"tag": i["tag"], "sha256": i["capture_sha256"],
                              "frames": i["capture_frames"], "ir_peak_s": i["ir_peak_s"]}
                             for i in inputs],
                "params": {"eps_ppm": args.eps_ppm, "sweep_s": args.sweep_s,
                           "bands_hz": bands, "fit_constants": results["fit_constants"]}}
    R.write_json(manifest, os.path.join(args.out, "run-manifest.json"))
    print("[decay-fit] wrote %s" % os.path.abspath(args.out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
