
"""h-diag orchestration + reporting: hdiag.json (machine readable), REPORT.md,
CSV curves (always) and optional PNG plots (only when matplotlib is importable).

The diagnostic output is deliberately split into independent status bits so later
spec judgements (mimo-objective-v1 v1.1) can consume them without re-parsing prose:

  provenance.status            MEASURED | UNPROVEN | SYNTHETIC_REFUSED | INVALID_MEASUREMENT
  consistency.status           CONSISTENT | INCONSISTENT   (+ drift table)
  completeness.status          OK | INCOMPLETE
  conclusions_allowed          bool (false => controllability conclusions withheld)
  statuses.energy_migration    ENERGY_MIGRATION | NONE | NOT_APPLICABLE
  statuses.holdout_severe_regression
                               FAIL | PASS | NOT_APPLICABLE  (> +3.0 dB band list)
  holdout_consistency.status   AGREEMENT | INCONSISTENT | INCOMPLETE
"""
from __future__ import annotations

import csv
import json
import math
import os
import sys

import numpy as np

from . import guards as G
from . import measure as M
from . import metrics as X


# ================================================================ orchestration
def run_diagnostics(manifest, analyses, bundle, options):
    freq_range = tuple(options.get("freq_range_hz", (20.0, 300.0)))
    prov = G.evaluate_provenance(
        manifest, analyses, bundle,
        extra_evidence_roots=options.get("extra_evidence_roots") or [],
        require_session=not options.get("skip_session_require", False))
    consistency = M.check_consistency(manifest, analyses, options)
    completeness = M.check_completeness(manifest, consistency, prov, options)

    freq_field = M.pick_freq_field(
        [rec for rec in analyses if rec.get("freq") is not None],
        options.get("freq_field", "auto"), freq_range)
    try:
        plant = M.build_h(manifest, analyses, freq_field, freq_range,
                          common_d0_samples=float(manifest.get("common_d0_samples") or 0.0))
    except M.InputError as exc:
        raise M.InputError("cannot assemble H: %s" % (exc,))
    if plant["freq_hz"].size == 0:
        raise M.InputError("no analysis frequency inside %r Hz" % (freq_range,))

    design = plant["design"]
    holdout = plant["holdout"]
    if not design["labels"]:
        raise M.InputError("no design position with usable measurements")
    snr_median = _design_snr_median(analyses)
    result = {
        "schema": G.SCHEMA_H_DIAG,
        "tool": {
            "name": G.TOOL_NAME,
            "version": G.TOOL_VERSION,
            "generated_utc": G.utc_now(),
            "python": sys.version.split()[0],
            "numpy": np.__version__,
            "command": options.get("command") or [],
            "git_commit": options.get("git_commit"),
            "repository": G.repo_root(),
            "does_not_modify_solver": True,
            "solver_sources_untouched": [
                "crates/modal-design/src/solve.rs",
                "crates/modal-design/src/regularize.rs",
                "crates/modal-design/src/validate.rs",
                "crates/modal-design/src/artifact.rs",
                "crates/modal-design/src/timebase.rs",
                "crates/modal-design/src/target.rs",
            ],
        },
        "input": {
            "manifest_path": manifest["path"],
            "manifest_sha256": manifest["sha256"],
            "label": manifest.get("label"),
            "layout": manifest.get("layout"),
            "actuators": manifest["actuators"],
            "excluded_roles": manifest["excluded_roles"],
            "freq_field": freq_field,
            "freq_range_hz": list(freq_range),
            "grid_points_in_range": int(plant["freq_hz"].size),
            "grid_full_points": int(plant["grid_full_points"]),
            "sample_rate_hz": _sample_rate(analyses),
            "common_d0_samples": plant["common_d0_samples"],
            "phase_model": plant["phase_model"],
            "bands_hz": X.band_definition(),
            "weights": {
                "w_f": X.BAND_W_F,
                "alpha_min": X.ALPHA_MIN,
                "alpha_max": X.ALPHA_MAX,
                "position_weights_design": dict(zip(design["labels"],
                                                    [float(w) for w in design["weights"]])),
                "position_weights_holdout": dict(zip(holdout["labels"],
                                                     [float(w) for w in holdout["weights"]])),
                "holdout_used_for_tuning": False,
            },
            "s_ref": {"kind": "ones_real", "values": [1.0] * len(G.ACTUATORS)},
            "g_kind": "identity_placeholder",
            "g_note": ("G = delta_D * I (no design G yet). P_after = H * G * s_ref. The "
                       "holdout prediction block predicts the holdout field from the "
                       "design complex-mean plant, so the comparison is non-trivial even "
                       "with identity G; replace the predictor when a design G exists."),
            "after_manifest": options.get("after_manifest"),
            "declared_parameters": {
                "frozen_alpha_min": X.ALPHA_MIN, "frozen_alpha_max": X.ALPHA_MAX,
                "frozen_epsilon_energy_migration": X.EPS_ENERGY_MIGRATION,
                "frozen_delta_mimo_vs_peq_db": X.DELTA_MIMO_VS_PEQ_DB,
                "frozen_holdout_c_min": X.HOLDOUT_C_MIN,
                "frozen_holdout_severe_regression_db": X.HOLDOUT_SEVERE_REGRESSION_DB,
                "rank_rel_threshold": X.RANK_REL_THRESHOLD,
                "high_condition_warn": X.HIGH_CONDITION_WARN,
                "subspace_angle_warn_deg": options.get("subspace_angle_warn_deg",
                                                       X.SUBSPACE_ANGLE_WARN_DEG),
                "holdout_agreement_tol_db": options.get("holdout_tol_db", 3.0),
                "lambda_grid": X.LAMBDA_GRID,
                "effort_caps": options.get("effort_caps", X.DEFAULT_EFFORT_CAPS),
            },
        },
        "provenance": prov,
        "consistency": consistency,
        "completeness": completeness,
        "measurements": {
            "positions": _position_table(manifest, analyses),
            "design_count": len(design["labels"]),
            "holdout_count": len(holdout["labels"]),
            "snr_median_design_db": snr_median,
            "missing": {"design": design.get("missing", []),
                        "holdout": holdout.get("missing", [])},
        },
    }

    per_freq = _per_frequency(plant, options, snr_median)
    result["per_frequency"] = per_freq
    bands = _bands(plant, per_freq, snr_median, options)
    result["bands"] = bands

    after = options.get("_after_plant")
    rk = _holdout_block(manifest, plant, after, per_freq, options)
    result["holdout_consistency"] = rk["holdout_consistency"]
    result["statuses"] = {
        "energy_migration": rk["energy_migration"],
        "holdout_severe_regression": rk["holdout_severe_regression"],
    }
    result["status_bits"] = {
        "ENERGY_MIGRATION": rk["energy_migration"]["status"],
        "HOLDOUT_SEVERE_REGRESSION": rk["holdout_severe_regression"]["status"],
    }

    verdict, reason = _verdict(prov, consistency, completeness)
    result["verdict"] = verdict
    result["conclusions_allowed"] = verdict == "DIAGNOSTIC_ONLY"
    result["conclusions"] = _conclusions(result, bands, verdict, reason)
    result["not_run"] = _not_run()
    result["definitions"] = _definitions()
    return result


def _design_snr_median(analyses):
    values = []
    for rec in analyses:
        if rec.get("role") != "design" or rec.get("json") is None:
            continue
        try:
            raw = rec["json"].get("snr_band_db")
            if raw is None:
                raw = rec["json"].get("snr_db")
            snr = float(raw)
        except (TypeError, ValueError):
            continue
        if math.isfinite(snr):
            values.append(snr)
    return float(np.median(values)) if values else None


def _sample_rate(analyses):
    for rec in analyses:
        meta = rec.get("meta") or {}
        if meta.get("sample_rate") is not None:
            return meta.get("sample_rate")
    return None


def _position_table(manifest, analyses):
    by_key = {}
    for rec in analyses:
        by_key[(rec.get("position"), rec.get("actuator"))] = rec
    rows = []
    for pos in manifest["positions"]:
        files = {}
        for actuator in G.ACTUATORS:
            rec = by_key.get((pos["label"], actuator))
            files[actuator] = None if rec is None else {
                "path": rec.get("path"),
                "sha256": rec.get("sha256"),
                "channel": rec.get("channel"),
                "polarity": rec.get("polarity"),
                "snr_db": (rec.get("json") or {}).get("snr_db"),
                "peak_db": (rec.get("json") or {}).get("peak_db"),
                "delay_samples": (rec.get("json") or {}).get("delay_samples"),
                "fr_field": rec.get("fr_field"),
                "declared_sha256": rec.get("declared_sha256"),
                "sha_mismatch": bool(rec.get("sha_mismatch")),
                "error": rec.get("error"),
            }
        rows.append({
            "label": pos["label"],
            "role": pos["role"],
            "role_declared": pos["role_declared"],
            "weight": pos["weight"],
            "files": files,
        })
    return rows


# ================================================================ per frequency
def _combo_defs(actuators):
    return [
        ("SUB1", ["SUB1"]),
        ("SUB1+FL+FR", ["SUB1", "FL", "FR"]),
        ("ALL5", list(actuators)),
    ]


def _combo_matrix(H, actuators, columns):
    idx = [actuators.index(a) for a in columns]
    return H[:, idx, :]


def _per_frequency(plant, options, snr_median):
    freqs = plant["freq_hz"]
    actuators = plant["actuators"]
    Hd = plant["design"]["H"]
    wd = plant["design"]["weights"]
    Hh = plant["holdout"]["H"]
    wh = plant["holdout"]["weights"]
    combos = _combo_defs(actuators)
    lambdas = options.get("lambdas", X.LAMBDA_GRID)
    caps = options.get("effort_caps", X.DEFAULT_EFFORT_CAPS)

    out = {
        "freq_hz": [float(f) for f in freqs],
        "design": {"labels": plant["design"]["labels"],
                   "weights": [float(w) for w in wd],
                   "combos": {}, "coherence": _empty_coherence(actuators)},
        "holdout": {"labels": plant["holdout"]["labels"],
                    "weights": [float(w) for w in wh],
                    "sigma": [], "rank": []},
        "subspace_consistency": {"sigma_min_overlap": [], "max_angle_deg": [],
                                 "mean_angle_deg": [], "rank_design": [],
                                 "rank_holdout": [], "risk": []},
        "controllability": {},
        "regularization": {},
    }
    for name, columns in combos:
        out["design"]["combos"][name] = {
            "actuators": columns,
            "sigma": [], "sigma_rel_db": [], "rank": [], "kappa": [],
            "sigma_min": [], "sigma_min_rel_db": [],
        }
        out["controllability"][name] = {"nullspace_attenuation_db": [],
                                        "measurement_limited_depth_db": [],
                                        "weakest_channel_depth_db": [],
                                        "bottleneck": []}
        out["regularization"][name] = {}
        for probe in ("random", "weakest_mode"):
            out["regularization"][name][probe] = {
                "lambda": [float(x) for x in lambdas],
                "residual_rel": [], "effort": [],
                "lcurve_lambda": [], "effort_caps": [],
            }

    coh_mag = {("%s|%s" % (a, b)): [] for i, a in enumerate(actuators)
               for b in actuators[i + 1:]}
    coh_phase = {key: [] for key in coh_mag}

    for fi, freq in enumerate(freqs):
        Hdf = Hd[:, :, fi]
        Hhf = Hh[:, :, fi] if Hh.shape[0] else None
        if np.all(np.isfinite(Hdf)):
            mag, phase, _gram = X.coherence(Hdf, wd)
            for i, a in enumerate(actuators):
                for j in range(i + 1, len(actuators)):
                    key = "%s|%s" % (a, actuators[j])
                    coh_mag[key].append(float(mag[i, j]))
                    coh_phase[key].append(float(np.degrees(phase[i, j])))
        else:
            for key in coh_mag:
                coh_mag[key].append(None)
                coh_phase[key].append(None)
        for name, columns in combos:
            Hc = _combo_matrix(Hd, actuators, columns)[:, :, fi]
            if not np.all(np.isfinite(Hc)):
                entry = out["design"]["combos"][name]
                entry["sigma"].append(None)
                entry["sigma_rel_db"].append(None)
                entry["rank"].append(0)
                entry["kappa"].append(None)
                entry["sigma_min"].append(None)
                entry["sigma_min_rel_db"].append(None)
                out["controllability"][name]["nullspace_attenuation_db"].append(None)
                out["controllability"][name]["measurement_limited_depth_db"].append(None)
                out["controllability"][name]["weakest_channel_depth_db"].append(None)
                out["controllability"][name]["bottleneck"].append("missing")
                for probe in ("random", "weakest_mode"):
                    out["regularization"][name][probe]["residual_rel"].append(None)
                    out["regularization"][name][probe]["effort"].append(None)
                    out["regularization"][name][probe]["lcurve_lambda"].append(None)
                    out["regularization"][name][probe]["effort_caps"].append(None)
                continue
            U, S, Vh = X.svd(Hc, wd)
            ranks = X.effective_rank(S, snr_median)
            rank = ranks["rank"]
            cond = X.condition(S, rank)
            entry = out["design"]["combos"][name]
            entry["sigma"].append([float(s) for s in S])
            entry["sigma_rel_db"].append(cond["sigma_rel_db"])
            entry["rank"].append(int(rank))
            entry["kappa"].append(cond["kappa"])
            entry["sigma_min"].append(cond["sigma_min"])
            entry["sigma_min_rel_db"].append(cond["sigma_min_rel_db"])
            ctrl = X.controllability(S, rank, Hc.shape[0], snr_median)
            out["controllability"][name]["nullspace_attenuation_db"].append(
                ctrl["nullspace_attenuation_db"])
            out["controllability"][name]["measurement_limited_depth_db"].append(
                ctrl["measurement_limited_depth_db"])
            out["controllability"][name]["weakest_channel_depth_db"].append(
                ctrl["weakest_channel_depth_db"])
            out["controllability"][name]["bottleneck"].append(ctrl["bottleneck"])
            probes = X.probe_targets(U, S, rank, fi)
            for probe_name in ("random", "weakest_mode"):
                store = out["regularization"][name][probe_name]
                probe = probes.get(probe_name)
                if probe is None:
                    store["residual_rel"].append(None)
                    store["effort"].append(None)
                    store["lcurve_lambda"].append(None)
                    store["effort_caps"].append(None)
                    continue
                curve = X.ridge_curve(Hc, wd, probe["vector"], lambdas, caps)
                store["residual_rel"].append(curve["residual_rel"])
                store["effort"].append(curve["effort"])
                store["lcurve_lambda"].append(curve["lcurve_lambda"])
                store["effort_caps"].append(curve["effort_caps"])
        Hh_ok = Hhf is not None and np.all(np.isfinite(Hhf))
        if Hh_ok and Hhf.shape[0] >= 1:
            _U, Sh, _Vh = X.svd(Hhf, None)
            rank_h = X.effective_rank(Sh, snr_median)["rank"]
            out["holdout"]["sigma"].append([float(s) for s in Sh])
            out["holdout"]["rank"].append(int(rank_h))
        else:
            out["holdout"]["sigma"].append(None)
            out["holdout"]["rank"].append(0)
            rank_h = 0
        Hd_ok = np.all(np.isfinite(Hd[:, :, fi]))
        if Hd_ok and Hh_ok:
            _U, Sd, _Vh = X.svd(Hd[:, :, fi], wd)
            rank_d = X.effective_rank(Sd, snr_median)["rank"]
            sub = X.subspace_consistency(Hd[:, :, fi], wd, Hhf,
                                         rank_design=rank_d, rank_holdout=rank_h,
                                         angle_warn_deg=options.get(
                                             "subspace_angle_warn_deg",
                                             X.SUBSPACE_ANGLE_WARN_DEG))
            out["subspace_consistency"]["rank_design"].append(int(rank_d))
            out["subspace_consistency"]["rank_holdout"].append(int(rank_h))
            out["subspace_consistency"]["sigma_min_overlap"].append(
                sub["sigma_min_overlap"])
            out["subspace_consistency"]["max_angle_deg"].append(sub["max_angle_deg"])
            out["subspace_consistency"]["mean_angle_deg"].append(sub["mean_angle_deg"])
            out["subspace_consistency"]["risk"].append(bool(sub["risk"]))
        else:
            for key in ("rank_design", "rank_holdout", "sigma_min_overlap",
                        "max_angle_deg", "mean_angle_deg"):
                out["subspace_consistency"][key].append(None)
            out["subspace_consistency"]["risk"].append(None)

    out["design"]["coherence"] = {
        "magnitude": coh_mag, "phase_deg": coh_phase,
        "definition": "spatial coherence |sum_p w_p conj(H[p,i]) H[p,j]| / "
                      "(||h_i|| ||h_j||) at each frequency; 1=redundant, 0=orthogonal",
    }
    return out


def _empty_coherence(actuators):
    mag = {("%s|%s" % (a, b)): [] for i, a in enumerate(actuators)
           for b in actuators[i + 1:]}
    return {"magnitude": dict(mag), "phase_deg": dict(mag)}


# ================================================================ bands
def _bands(plant, per_freq, snr_median, options):
    freqs = plant["freq_hz"]
    actuators = plant["actuators"]
    Hd = plant["design"]["H"]
    wd = plant["design"]["weights"]
    Hh = plant["holdout"]["H"]
    combos = _combo_defs(actuators)
    bands = []
    for center in X.BAND_CENTERS_HZ:
        mask = X.band_mask(freqs, center)
        idx = np.nonzero(mask)[0]
        entry = {"center_hz": float(center), "range_hz": list(X.band_range(center)),
                 "n_freqs": int(idx.size), "combos": {}}
        if idx.size == 0:
            entry["status"] = "NO_DATA"
            bands.append(entry)
            continue
        entry["status"] = "OK"
        for name, columns in combos:
            summary = X.band_summary_combo(freqs, name, actuators, Hd, wd,
                                           snr_median, band_centers=[center])
            entry["combos"][name] = summary[0] if summary else {"status": "NO_DATA"}
        if Hh.shape[0] and idx.size and np.all(np.isfinite(Hh[:, :, idx])):
            S_list = []
            ranks = []
            for fi in idx:
                _U, S, _Vh = X.svd(Hh[:, :, fi], None)
                S_list.append(S)
                ranks.append(X.effective_rank(S, snr_median)["rank"])
            S = np.asarray(S_list)
            entry["holdout"] = {
                "sigma_band": [float(s) for s in np.sqrt(np.mean(S ** 2, axis=0))],
                "rank_min": int(np.min(ranks)), "rank_max": int(np.max(ranks)),
                "n_positions": int(Hh.shape[0]),
            }
        else:
            entry["holdout"] = {"sigma_band": None, "rank_min": None,
                                "rank_max": None, "n_positions": int(Hh.shape[0])}
        angles = [per_freq["subspace_consistency"]["max_angle_deg"][i] for i in idx]
        angles = [a for a in angles if a is not None]
        overlaps = [per_freq["subspace_consistency"]["sigma_min_overlap"][i]
                    for i in idx]
        overlaps = [o for o in overlaps if o is not None]
        ranks_d = [per_freq["subspace_consistency"]["rank_design"][i] for i in idx]
        ranks_d = [r for r in ranks_d if r is not None]
        ranks_h = [per_freq["subspace_consistency"]["rank_holdout"][i] for i in idx]
        ranks_h = [r for r in ranks_h if r is not None]
        risks = [per_freq["subspace_consistency"]["risk"][i] for i in idx]
        risks = [r for r in risks if r is not None]
        entry["subspace"] = {
            "max_angle_deg": float(np.max(angles)) if angles else None,
            "median_angle_deg": float(np.median(angles)) if angles else None,
            "mean_angle_deg": float(np.mean(angles)) if angles else None,
            "sigma_min_overlap_median": float(np.median(overlaps)) if overlaps else None,
            "subspace_distance_median": (
                float(math.sqrt(max(0.0, 1.0 - float(np.median(overlaps)) ** 2)))
                if overlaps else None),
            "rank_design_min": int(np.min(ranks_d)) if ranks_d else None,
            "rank_holdout_min": int(np.min(ranks_h)) if ranks_h else None,
            "overfit_risk": bool(any(risks)) if risks else None,
            "overfit_risk_available": bool(risks),
            "overfit_risk_reason": (
                "max principal angle > %.1f deg or design/holdout rank mismatch at "
                "some in-band frequency" %
                options.get("subspace_angle_warn_deg", X.SUBSPACE_ANGLE_WARN_DEG)),
        }
        bc = (X.band_coherence(Hd, freqs, wd, center)
              if np.all(np.isfinite(Hd)) else None)
        if bc is not None:
            pairs = []
            for i, a in enumerate(actuators):
                for j in range(i + 1, len(actuators)):
                    pairs.append({"pair": "%s|%s" % (a, actuators[j]),
                                  "magnitude": float(bc["magnitude"][i, j]),
                                  "phase_deg": float(bc["phase_deg"][i, j])})
            pairs.sort(key=lambda p: p["magnitude"], reverse=True)
            entry["coherence"] = {"pairs": pairs,
                                  "redundant_pairs_gt_0_95":
                                      [p["pair"] for p in pairs
                                       if p["magnitude"] > 0.95]}
        else:
            entry["coherence"] = {"pairs": []}
        entry["rank_deficient"] = [
            name for name, item in entry["combos"].items()
            if item.get("status") == "OK" and item.get("rank_min") is not None
            and item["rank_min"] < (len(actuators) if name == "ALL5"
                                    else len(name.split("+")))
        ]
        bands.append(entry)
    return bands


# ================================================================ R_k / statuses
def _sref(actuators):
    return np.ones(len(actuators), dtype=np.complex128)


def _band_energy(field, freqs, center):
    mask = X.band_mask(freqs, center)
    if not np.any(mask):
        return None
    return float(np.sum(np.abs(field[..., mask]) ** 2))


def _predict_design_mean(Hd, wd, freqs):
    """Design complex-mean plant predictor (placeholder for a design G prediction).

    H_pred[n, f] = sum_d w_d H_d[p,n,f] / sum_d w_d.  Declared predictor, never a
    solver output; replaced by H_design * G once a design G exists.
    """
    if Hd.shape[0] == 0:
        return None
    w = np.asarray(wd, dtype=np.float64)
    return np.einsum("p,pnf->nf", w / np.sum(w), Hd)


def _holdout_block(manifest, plant, after, per_freq, options):
    freqs = plant["freq_hz"]
    actuators = plant["actuators"]
    sref = _sref(actuators)
    Hd = plant["design"]["H"]
    wd = plant["design"]["weights"]
    Hh = plant["holdout"]["H"]
    tol = float(options.get("holdout_tol_db", 3.0))
    predicted = _predict_design_mean(Hd, wd, freqs)
    points = []
    deviations = []
    predicted_ok = predicted is not None and np.all(np.isfinite(predicted))
    if predicted_ok and Hh.shape[0]:
        for hp, label in enumerate(plant["holdout"]["labels"]):
            if not np.all(np.isfinite(Hh[hp])):
                points.append({"label": label, "status": "NO_DATA", "bands": []})
                continue
            p_before = Hh[hp].T @ sref
            p_pred = predicted.T @ sref
            bands = []
            for center in X.BAND_CENTERS_HZ:
                e_before = _band_energy(p_before, freqs, center)
                e_pred = _band_energy(p_pred, freqs, center)
                if e_before is None or e_before <= 0.0:
                    continue
                r_pred = e_pred / e_before
                dev_db = 10.0 * math.log10(r_pred) if r_pred > 0 else None
                r_meas = 1.0
                basis = "identity_placeholder"
                if after is not None:
                    Ha = after["holdout"]["H"]
                    if hp < Ha.shape[0] and np.all(np.isfinite(Ha[hp])):
                        p_after = Ha[hp].T @ sref
                        e_after = _band_energy(p_after, freqs, center)
                        if e_after is not None:
                            r_meas = e_after / e_before
                            basis = "measured_after"
                dev_meas = (10.0 * math.log10(r_meas)
                            if (r_meas is not None and r_meas > 0) else None)
                delta = (None if (dev_db is None or dev_meas is None)
                         else dev_db - dev_meas)
                status = ("AGREEMENT" if (delta is not None and abs(delta) <= tol)
                          else "INCONSISTENT")
                if delta is not None:
                    deviations.append(delta)
                bands.append({
                    "center_hz": float(center), "range_hz": list(X.band_range(center)),
                    "R_k_design_predicted": r_pred,
                    "delta_db_design_predicted": dev_db,
                    "R_k_holdout_measured": r_meas,
                    "delta_db_holdout_measured": dev_meas,
                    "deviation_db": delta,
                    "status": status,
                    "basis": basis,
                })
            points.append({"label": label, "status": "OK", "bands": bands})
    n_ok = sum(1 for p in points if p["status"] == "OK")
    any_inconsistent = any(b["status"] == "INCONSISTENT" for p in points
                           for b in p.get("bands") or [])
    required = int(manifest.get("expected", {}).get("holdout_points", 3))
    if n_ok == 0:
        status = "INCOMPLETE"
        reason = "no holdout position with a complete 5-actuator measurement"
    elif n_ok < required:
        status = "INCOMPLETE"
        reason = ("valid holdout positions %d < required %d -> INCOMPLETE (spec "
                  "mimo-objective-v1 6.2)" % (n_ok, required))
    elif any_inconsistent:
        status = "INCONSISTENT"
        reason = ("at least one holdout point/band deviates more than %.1f dB from the "
                  "design-predicted band field" % tol)
    else:
        status = "AGREEMENT"
        reason = ("all holdout point/bands within %.1f dB of the design-mean predicted "
                  "band field" % tol)
    c_reason = ("C requires a real design G + measured after data; current basis is "
                "identity_placeholder / design-mean prediction")
    holdout = {
        "status": status,
        "reason": reason,
        "valid_holdout_points": int(n_ok),
        "required_holdout_points": required,
        "tolerance_db": tol,
        "predictor": "design_complex_mean (placeholder; replace with design-G prediction)",
        "g_kind": "identity_placeholder",
        "C_spec": None,
        "C_spec_reason": c_reason,
        "notes": [
            "R_k is the mimo-objective-v1 2.2 normalized residual energy ratio "
            "(sum w_f |P|^2 after / before) on the frozen 1/3-oct bands.",
            "the predicted side uses a design complex-mean plant because no design G "
            "exists yet; identity G is applied on both sides, so this block measures "
            "whether the design points represent the holdout field (data consistency), "
            "not a control performance prediction.",
        ],
        "points": points,
        "deviation_abs_median_db": (float(np.median(np.abs(deviations)))
                                    if deviations else None),
        "deviation_abs_max_db": (float(np.max(np.abs(deviations)))
                                 if deviations else None),
    }

    migration = {
        "applicable": after is not None,
        "status": "NOT_APPLICABLE",
        "basis": "identity_placeholder_no_measured_after",
        "epsilon": X.EPS_ENERGY_MIGRATION,
        "threshold_db": 10.0 * math.log10(1.0 + X.EPS_ENERGY_MIGRATION),
        "bands": [],
        "migrated_bands": [],
        "bands_available": False,
        "definition": "R_k > 1+eps in a band (local worsening) while another band "
                      "decreases; computed on measured after/before when available",
    }
    if after is not None:
        rows = []
        for center in X.BAND_CENTERS_HZ:
            e_before = 0.0
            e_after = 0.0
            for role in ("design", "holdout"):
                base = plant[role]["H"]
                other = after[role]["H"]
                for pos in range(min(base.shape[0], other.shape[0])):
                    if np.all(np.isfinite(base[pos])) and np.all(np.isfinite(other[pos])):
                        eb = _band_energy(base[pos].T @ sref, freqs, center)
                        ea = _band_energy(other[pos].T @ sref, freqs, center)
                        if eb is not None and ea is not None:
                            e_before += eb
                            e_after += ea
            if e_before <= 0.0:
                rows.append({"center_hz": float(center), "R_k": None,
                             "delta_db": None, "E_before": e_before,
                             "E_after": e_after, "status": "N/A"})
                continue
            r = e_after / e_before
            delta = 10.0 * math.log10(r) if r > 0 else None
            rows.append({"center_hz": float(center), "R_k": r, "delta_db": delta,
                         "E_before": e_before, "E_after": e_after,
                         "status": ("WORSE" if r > 1.0 + X.EPS_ENERGY_MIGRATION
                                    else "OK")})
        worse = {row["center_hz"] for row in rows
                 if row["R_k"] is not None and row["R_k"] > 1.0 + X.EPS_ENERGY_MIGRATION}
        better = {row["center_hz"] for row in rows
                  if row["R_k"] is not None and row["R_k"] < 1.0}
        migration.update({"status": ("ENERGY_MIGRATION" if (worse and better)
                                     else "NONE"),
                          "basis": "measured_after_vs_before", "bands": rows,
                          "migrated_bands": (sorted(worse) if (worse and better)
                                             else []),
                          "bands_available": True})

    reg = {
        "applicable": after is not None,
        "status": "NOT_APPLICABLE",
        "basis": "identity_placeholder_no_measured_after",
        "threshold_db": X.HOLDOUT_SEVERE_REGRESSION_DB,
        "regressions": [],
        "definition": "any valid holdout point with any modal band measured "
                      "10log10(after/before) > +3.0 dB is an independent FAIL",
    }
    if after is not None:
        regressions = []
        for hp, label in enumerate(plant["holdout"]["labels"]):
            if hp >= after["holdout"]["H"].shape[0]:
                continue
            if not np.all(np.isfinite(Hh[hp])) or \
                    not np.all(np.isfinite(after["holdout"]["H"][hp])):
                continue
            p_before = Hh[hp].T @ sref
            p_after = after["holdout"]["H"][hp].T @ sref
            for center in X.BAND_CENTERS_HZ:
                eb = _band_energy(p_before, freqs, center)
                ea = _band_energy(p_after, freqs, center)
                if eb is None or eb <= 0.0:
                    continue
                delta = 10.0 * math.log10(ea / eb) if ea > 0 else None
                if delta is not None and delta > X.HOLDOUT_SEVERE_REGRESSION_DB:
                    regressions.append({"label": label, "center_hz": float(center),
                                        "delta_db": delta})
        reg.update({"status": "FAIL" if regressions else "PASS",
                    "basis": "measured_after_vs_before", "regressions": regressions})
    return {"holdout_consistency": holdout, "energy_migration": migration,
            "holdout_severe_regression": reg}


# ================================================================ verdict
def _verdict(prov, consistency, completeness):
    if prov["status"] == "SYNTHETIC_REFUSED":
        return "UNPROVEN", ("synthetic/dry-run input: provenance refuses to let it be "
                            "reported as a real measured conclusion")
    if prov["status"] == "INVALID_MEASUREMENT":
        return "INVALID_MEASUREMENT", "known invalid-measurement fingerprint (guard refusal)"
    if prov["status"] != "MEASURED":
        return "UNPROVEN", "provenance cannot be confirmed (fail-closed)"
    if consistency["status"] != "CONSISTENT":
        return "INCONSISTENT", "measurement sweep/role/grid metadata drift"
    if completeness["status"] != "OK":
        return "INCOMPLETE", ("design/holdout point counts or pre-registration metadata "
                              "incomplete")
    return "DIAGNOSTIC_ONLY", ("measured design/holdout multipoint data, provenance and "
                               "consistency gates passed; diagnostic conclusions allowed "
                               "(not a T6-M acceptance)")


def _conclusions(result, bands, verdict, reason):
    allowed = verdict == "DIAGNOSTIC_ONLY"
    base = {
        "withheld": not allowed,
        "withheld_reason": None if allowed else reason,
        "verdict": verdict,
        "scope": ("diagnostic only: measured-H controllability/subspace analysis; "
                  "NOT applied, NOT re-measured, NOT a T6-cal/T6-M acceptance"),
    }
    if not allowed:
        base.update({"controllable_bands": None, "rank_deficient_bands": None,
                     "ill_conditioned_bands": None, "overfit_risk_bands": None,
                     "redundant_actuator_pairs": None,
                     "deepest_band_estimate": None})
        return base
    all5 = []
    rank_def = []
    ill = []
    overfit = []
    redundant = set()
    depth = []
    for band in bands:
        if band.get("status") != "OK":
            continue
        combo = band["combos"].get("ALL5")
        if combo and combo.get("status") == "OK":
            all5.append({"center_hz": band["center_hz"],
                         "class": combo.get("controllability_class"),
                         "rank_min": combo.get("rank_min"),
                         "kappa_max": combo.get("kappa_max"),
                         "depth_bottleneck": combo.get("depth", {}).get("bottleneck"),
                         "depth_db": combo.get("depth", {}).get("bottleneck_depth_db")})
            if combo.get("controllability_class") == "RANK_DEFICIENT":
                rank_def.append(band["center_hz"])
            elif combo.get("controllability_class") == "ILL_CONDITIONED":
                ill.append(band["center_hz"])
            if combo.get("depth", {}).get("bottleneck_depth_db") is not None:
                depth.append((band["center_hz"],
                              combo["depth"]["bottleneck_depth_db"]))
        sub = band.get("subspace") or {}
        if sub.get("overfit_risk"):
            overfit.append(band["center_hz"])
        for pair in (band.get("coherence") or {}).get(
                "redundant_pairs_gt_0_95", []):
            redundant.add(pair)
    deepest = min(depth, key=lambda item: item[1]) if depth else None
    base.update({
        "controllable_bands": [row["center_hz"] for row in all5
                               if row["class"] == "CONTROLLABLE"],
        "rank_deficient_bands": rank_def,
        "ill_conditioned_bands": ill,
        "overfit_risk_bands": overfit,
        "redundant_actuator_pairs": sorted(redundant),
        "per_band_all5": all5,
        "deepest_band_estimate": (None if deepest is None else
                                  {"center_hz": deepest[0], "depth_db": deepest[1]}),
        "caveats": [
            "rank/condition/depth are measured-H properties; they are not a promise "
            "that the FIR G can realise them under the effort/headroom/causality caps",
            "a band with full rank can still be useless if the required correction "
            "exceeds the headroom cap; see the regularization trade-off curves",
            "holdout never enters the design; overfit-risk flags use only the measured "
            "design/holdout actuator subspaces",
        ],
    })
    return base


def _not_run():
    return [
        {"item": "real design(>=6)+holdout(>=3) multipoint H",
         "status": "NOT_RUN", "reason": "no real multipoint measurement collected yet"},
        {"item": "design G / solver output consumption",
         "status": "NOT_RUN", "reason": "diagnostic step only; solver not modified or run"},
        {"item": "applied correction, post-apply remeasurement, EDC/RT60",
         "status": "NOT_RUN", "reason": "no apply, no sound emission in this task"},
        {"item": "T6-cal / T6-M acceptance",
         "status": "NOT_RUN", "reason": "requires measured applied + holdout; out of scope"},
        {"item": "per-band R_k from a real design G",
         "status": "NOT_RUN", "reason": "identity G placeholder only"},
    ]


def _definitions():
    return {
        "H_matrix": "H[p,n,f] = 10^(db/20)*exp(j*radians(phase_deg - 360*f*D0/fs))*polarity; "
                    "rows=positions, cols=actuators, f=frequency on the analysis grid",
        "singular_values": "weighted SVD of Hw=diag(sqrt(w_p))H; sigma_1>=...>=sigma_min",
        "effective_rank": "sigma_i > max(1e-3*sigma_1, sigma_1*10^(-SNR/20)); 1e-3 mirrors "
                          "solve.rs RANK_ZERO_REL=1e-6 on the Gram matrix",
        "condition_number": "kappa=sigma_1/sigma_rank_eff; kappa_full=sigma_1/sigma_min is "
                            "diagnostic only",
        "coherence": "spatial: |sum_p w_p conj(H[p,i])H[p,j]|/(||h_i|| ||h_j||) at each f; "
                     "band: same with sum over p and f (w_f=1.0)",
        "controllability": "actuator combinations SUB1 / SUB1+FL+FR / ALL5; rank, kappa, "
                           "nullspace attenuation D_null=10log10(P/(P-r)) for random target, "
                           "measurement-limited depth D_meas=20log10(1/(eps*kappa))",
        "regularization": "sigma_1-normalized ridge g=(Hn^H Hn+lambda I)^-1 Hn^H t on two "
                          "declared probes (seeded random direction, weakest retained mode); "
                          "residual=||Hn g-t||/||t|| (t unit norm), effort=sum|g|^2; "
                          "L-curve corner lambda; min residual within effort caps",
        "subspace_consistency": "principal angles between design/holdout right singular "
                                "subspaces V (actuator space, C^N); max/mean angle, "
                                "sigma_min_overlap, distance=sqrt(1-sigma_min^2)",
        "R_k": "mimo-objective-v1 2.2 normalized residual energy: sum_{f in B} w_f |P|^2 "
               "after / before; w_f=1.0 default, 1/3-oct centers "
               "40/50/63/80/100/125/160/200 Hz",
        "s_ref": "frozen declaration: 5 participant channels each 1.0 (real unit); "
                 "P=H*G*s_ref; baseline G=delta_D*I",
        "statuses": "ENERGY_MIGRATION: R_k>1+eps(0.05) in one band while another decreases; "
                    "HOLDOUT_SEVERE_REGRESSION: measured band delta > +3.0 dB at any valid "
                    "holdout point (independent FAIL); both NOT_APPLICABLE without measured "
                    "after data",
    }


# ================================================================ writers
def write_outputs(result, out_dir, plant=None):
    os.makedirs(out_dir, exist_ok=True)
    result.pop("_out_dir", None)
    json_path = os.path.join(out_dir, "hdiag.json")
    with open(json_path, "w", encoding="utf-8") as fh:
        json.dump(result, fh, indent=2, ensure_ascii=False)
        fh.write(chr(10))
    csv_files = _write_csv(result, out_dir, plant)
    png_files = []
    try:
        png_files = _write_plots(result, out_dir, plant)
    except Exception as exc:  # plots are optional; never fail the run
        png_files = []
        result.setdefault("warnings", []).append("plot generation skipped: %s" % (exc,))
    md_path = os.path.join(out_dir, "REPORT.md")
    with open(md_path, "w", encoding="utf-8") as fh:
        fh.write(render_report(result, plant, out_dir))
    return {"json": json_path, "report": md_path, "csv": csv_files, "png": png_files}


def _write_csv(result, out_dir, plant):
    files = []
    path = os.path.join(out_dir, "per_frequency.csv")
    with open(path, "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["freq_hz", "combo", "rank_eff", "kappa", "sigma_min",
                    "sigma_min_rel_db", "sigma_1"] + ["sigma_%d" % (i + 1) for i in range(5)])
        pf = result["per_frequency"]
        for fi, freq in enumerate(pf["freq_hz"]):
            for combo, entry in pf["design"]["combos"].items():
                sigma = entry["sigma"][fi] or []
                w.writerow([freq, combo, entry["rank"][fi], entry["kappa"][fi],
                            entry["sigma_min"][fi], entry["sigma_min_rel_db"][fi],
                            (sigma[0] if sigma else None)] +
                           [sigma[i] if i < len(sigma) else None for i in range(5)])
    files.append(path)
    path = os.path.join(out_dir, "bands.csv")
    with open(path, "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["center_hz", "n_freqs", "combo", "status", "class", "rank_min",
                    "rank_max", "kappa_max", "kappa_median", "sigma_min_rel_db",
                    "depth_nullspace_db", "depth_measurement_db", "bottleneck"])
        for band in result["bands"]:
            for combo, item in band.get("combos", {}).items():
                depth = item.get("depth") or {}
                w.writerow([band["center_hz"], band.get("n_freqs"), combo,
                            item.get("status"), item.get("controllability_class"),
                            item.get("rank_min"), item.get("rank_max"),
                            item.get("kappa_max"), item.get("kappa_median"),
                            (item.get("sigma_band_rel_db") or [None])[-1],
                            depth.get("nullspace_attenuation_db"),
                            depth.get("measurement_limited_depth_db"),
                            depth.get("bottleneck")])
    files.append(path)
    path = os.path.join(out_dir, "coherence_band.csv")
    with open(path, "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["center_hz", "pair", "magnitude", "phase_deg"])
        for band in result["bands"]:
            for pair in (band.get("coherence") or {}).get("pairs", []):
                w.writerow([band["center_hz"], pair["pair"], pair["magnitude"],
                            pair["phase_deg"]])
    files.append(path)
    path = os.path.join(out_dir, "lambda_tradeoff.csv")
    with open(path, "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["center_hz", "probe", "lambda", "residual_rel_band_median",
                    "residual_db_band_median", "effort_band_median",
                    "effort_db_band_median"])
        pf = result["per_frequency"]
        for band in result["bands"]:
            if band.get("status") != "OK":
                continue
            mask = X.band_mask(np.asarray(pf["freq_hz"]), band["center_hz"])
            idx = np.nonzero(mask)[0]
            for probe in ("random", "weakest_mode"):
                entry = pf["regularization"]["ALL5"][probe]
                for li, lam in enumerate(entry["lambda"]):
                    res = [entry["residual_rel"][fi][li] for fi in idx
                           if entry["residual_rel"][fi] is not None and
                           entry["residual_rel"][fi][li] is not None]
                    eff = [entry["effort"][fi][li] for fi in idx
                           if entry["effort"][fi] is not None and
                           entry["effort"][fi][li] is not None]
                    if not res:
                        continue
                    w.writerow([band["center_hz"], probe, lam,
                                float(np.median(res)),
                                (20.0 * math.log10(float(np.median(res)))
                                 if np.median(res) > 0 else None),
                                float(np.median(eff)),
                                (10.0 * math.log10(float(np.median(eff)))
                                 if np.median(eff) > 0 else None)])
    files.append(path)
    path = os.path.join(out_dir, "holdout_prediction.csv")
    with open(path, "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["label", "center_hz", "R_k_design_predicted", "dev_db_pred",
                    "R_k_holdout_measured", "dev_db_meas", "deviation_db", "status"])
        for point in result["holdout_consistency"]["points"]:
            for band in point.get("bands") or []:
                w.writerow([point["label"], band["center_hz"],
                            band["R_k_design_predicted"],
                            band["delta_db_design_predicted"],
                            band["R_k_holdout_measured"],
                            band["delta_db_holdout_measured"],
                            band["deviation_db"], band["status"]])
    files.append(path)
    return files


def _write_plots(result, out_dir, plant):
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except Exception:
        return []
    files = []
    pf = result["per_frequency"]
    freqs = np.asarray(pf["freq_hz"])
    outdir = os.path.join(out_dir, "plots")
    os.makedirs(outdir, exist_ok=True)
    fig, ax = plt.subplots(figsize=(8, 4))
    sigma_db = pf["design"]["combos"]["ALL5"]["sigma_rel_db"]
    n_act = len(result["input"]["actuators"])
    for si in range(n_act):
        ys = [row[si] if row and si < len(row) else np.nan for row in sigma_db]
        ax.plot(freqs, ys, label="sigma_%d" % (si + 1))
    ax.set_xscale("log")
    ax.set_xlabel("frequency [Hz]")
    ax.set_ylabel("relative singular value [dB re sigma_1]")
    ax.set_title("H singular-value spectrum, ALL5 (design positions)")
    ax.grid(True, which="both", alpha=0.3)
    ax.legend()
    path = os.path.join(outdir, "singular_value_spectrum.png")
    fig.tight_layout()
    fig.savefig(path, dpi=130)
    plt.close(fig)
    files.append(path)
    fig, ax = plt.subplots(figsize=(8, 4))
    for combo in ("ALL5", "SUB1+FL+FR", "SUB1"):
        vals = pf["design"]["combos"][combo]["kappa"]
        ys = [v if v is not None else np.nan for v in vals]
        ax.plot(freqs, ys, label=combo)
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_xlabel("frequency [Hz]")
    ax.set_ylabel("kappa = sigma_1 / sigma_rank")
    ax.set_title("condition number vs frequency")
    ax.grid(True, which="both", alpha=0.3)
    ax.legend()
    path = os.path.join(outdir, "condition_vs_frequency.png")
    fig.tight_layout()
    fig.savefig(path, dpi=130)
    plt.close(fig)
    files.append(path)
    fig, axes = plt.subplots(2, 4, figsize=(14, 7))
    actuators = result["input"]["actuators"]
    n = len(actuators)
    im = None
    for ax, band in zip(axes.flat, result["bands"]):
        if band.get("status") != "OK":
            ax.set_visible(False)
            continue
        mat = np.eye(n)
        for pair in (band.get("coherence") or {}).get("pairs", []):
            i = actuators.index(pair["pair"].split("|")[0])
            j = actuators.index(pair["pair"].split("|")[1])
            mat[i, j] = mat[j, i] = pair["magnitude"]
        im = ax.imshow(mat, vmin=0, vmax=1, cmap="viridis")
        ax.set_title("%.0f Hz" % band["center_hz"])
        ax.set_xticks(range(n))
        ax.set_yticks(range(n))
        ax.set_xticklabels(actuators, fontsize=7)
        ax.set_yticklabels(actuators, fontsize=7)
    fig.suptitle("band-averaged actuator complex coherence magnitude")
    if im is not None:
        fig.colorbar(im, ax=axes.ravel().tolist(), shrink=0.8)
    path = os.path.join(outdir, "coherence_heatmap.png")
    fig.savefig(path, dpi=130, bbox_inches="tight")
    plt.close(fig)
    files.append(path)
    fig, ax = plt.subplots(figsize=(8, 5))
    for band in result["bands"]:
        if band.get("status") != "OK":
            continue
        mask = X.band_mask(freqs, band["center_hz"])
        idx = np.nonzero(mask)[0]
        entry = pf["regularization"]["ALL5"]["random"]
        res = [entry["residual_rel"][fi][-1] for fi in idx
               if entry["residual_rel"][fi] is not None]
        eff = [entry["effort"][fi][-1] for fi in idx
               if entry["effort"][fi] is not None]
        if not res:
            continue
        ax.scatter(np.median(eff), np.median(res),
                   label="%.0f Hz" % band["center_hz"])
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_xlabel("effort sum|g|^2 (lambda=1 point)")
    ax.set_ylabel("relative residual")
    ax.set_title("regularization trade-off (ALL5, random probe)")
    ax.grid(True, which="both", alpha=0.3)
    ax.legend(fontsize=7, ncol=2)
    path = os.path.join(outdir, "lambda_tradeoff.png")
    fig.tight_layout()
    fig.savefig(path, dpi=130)
    plt.close(fig)
    files.append(path)
    return files


# ================================================================ markdown
def render_report(result, plant=None, out_dir=None):
    lines = []
    add = lines.append
    verdict = result["verdict"]
    add("# H 矩阵诊断报告（h-diag v%s）" % result["tool"]["version"])
    add("")
    add("- 生成时间（UTC）：`%s`" % result["tool"]["generated_utc"])
    add("- 输入清单：`%s`（sha256 `%s`）" %
        (result["input"]["manifest_path"], result["input"]["manifest_sha256"]))
    add("- 频带/权重口径：`rt-stack/spec/mimo-objective-v1.md` v1.1（1/3 oct 中心 "
        "40/50/63/80/100/125/160/200 Hz，`w_f`=1.0，`alpha_min`=0.5，"
        "`alpha_max`=2.0，`eps`=0.05，`delta`=1.0 dB，`C_min`=0.50，"
        "holdout 严重恶化 > +3.0 dB）")
    add("- 工具不改求解器：`crates/modal-design/src/{solve,regularize,validate,artifact,"
        "timebase,target}.rs` 均未修改。")
    add("")
    add("## 0. 结论状态（先看这里）")
    add("")
    add("| 项 | 状态 | 说明 |")
    add("| --- | --- | --- |")
    add("| verdict | **%s** | %s |" % (verdict, result["conclusions"].get(
        "withheld_reason") or "诊断结论可用（非 T6-M 验收）"))
    add("| provenance | **%s** | %s |" % (result["provenance"]["status"],
                                          _short_codes(result["provenance"])))
    add("| consistency | **%s** | %s |" % (result["consistency"]["status"],
                                           _short_codes(result["consistency"])))
    add("| completeness | **%s** | design=%s holdout=%s |" % (
        result["completeness"]["status"],
        result["completeness"]["design_positions"],
        result["completeness"]["holdout_positions"]))
    add("| conclusions_allowed | **%s** | 不满足门 -> 不给“可控频带”结论 |" %
        result["conclusions_allowed"])
    em = result["statuses"]["energy_migration"]
    sr = result["statuses"]["holdout_severe_regression"]
    add("| ENERGY_MIGRATION | **%s** | basis=%s |" % (em["status"], em["basis"]))
    add("| HOLDOUT_SEVERE_REGRESSION | **%s** | %d 条 > +%.1f dB；basis=%s |" % (
        sr["status"], len(sr["regressions"]), sr["threshold_db"], sr["basis"]))
    add("")

    add("## 1. Provenance 门（fail-closed）")
    add("")
    add("- 状态：**%s**；证据根：`%s`" % (
        result["provenance"]["status"], ", ".join(result["provenance"]["evidence_roots"])))
    add("- 复用的门：`tools/mimo-prep/mimo12k_guard_lite.py` 与 "
        "`mimo_kit/provenance.py`（sha256 见 hdiag.json "
        "`provenance.policy.sources`）；常量引用 provenance.py:56-63。")
    if result["provenance"]["codes"]:
        add("")
        add("| code | reason |")
        add("| --- | --- |")
        for code in result["provenance"]["codes"]:
            add("| `%s` | %s |" % (code,
                                    result["provenance"]["reasons"].get(code, "")))
    add("")
    add("- 规则：证据根外的文件、synthetic/dry-run/fixture 路径/标记/注册 sha、"
        "MA1/ARC S24 字节映射伪影、死麦/死执行器行、clock/onset 非法声明 -> 拒绝或 "
        "UNPROVEN；synthetic 数据永不产生“真实结论”。")
    add("")

    add("## 2. 测量一致性门")
    add("")
    add("- 状态：**%s**" % result["consistency"]["status"])
    if result["consistency"]["drifts"]:
        add("")
        add("| 漂移字段 | 观测值 | 涉及文件数 |")
        add("| --- | --- | --- |")
        for drift in result["consistency"]["drifts"]:
            values = drift.get("values")
            shown = values if not isinstance(values, list) else values[:6]
            add("| `%s` | %s | %d |" % (drift["field"], shown,
                                          len(drift.get("files") or [])))
    if result["consistency"]["codes"]:
        add("")
        add("| code | reason |")
        add("| --- | --- |")
        for code in result["consistency"]["codes"]:
            add("| `%s` | %s |" % (code,
                                    result["consistency"]["reasons"].get(code, "")))
    for warning in result["consistency"].get("warnings") or []:
        add("- 警告：%s" % warning)
    add("")

    add("## 3. 奇异值谱 / 有效秩 / 条件数（design 点，ALL5）")
    add("")
    add("| 1/3 oct | n_f | rank_min | rank_max | kappa_max | kappa_med | "
        "sigma_min rel dB | 分类 |")
    add("| --- | --- | --- | --- | --- | --- | --- | --- |")
    for band in result["bands"]:
        item = (band.get("combos") or {}).get("ALL5") or {}
        if item.get("status") != "OK":
            add("| %s | %s | - | - | - | - | - | %s |" %
                (_band_label(band["center_hz"]), band.get("n_freqs"),
                 item.get("status", "NO_DATA")))
            continue
        sigma_rel = item.get("sigma_band_rel_db") or []
        rank_min = item.get("rank_min") or 0
        sigma_eff = (sigma_rel[rank_min - 1] if 1 <= rank_min <= len(sigma_rel)
                     else (sigma_rel[-1] if sigma_rel else None))
        add("| %s | %s | %s | %s | %s | %s | %s | %s |" % (
            _band_label(band["center_hz"]), band.get("n_freqs"),
            item.get("rank_min"), item.get("rank_max"),
            _fmt(item.get("kappa_max"), 2), _fmt(item.get("kappa_median"), 2),
            _fmt(sigma_eff, 1),
            item.get("controllability_class")))
    add("")
    add("有效秩阈值：`sigma_i > max(1e-3*sigma_1, sigma_1*10^(-SNR/20))`（1e-3 对应 "
        "solve.rs `RANK_ZERO_REL=1e-6` 在 Gram 上）；kappa 使用有效秩内最小奇异值。"
        " 每频点完整谱见 `hdiag.json` / `per_frequency.csv`。")
    add("")

    add("## 4. 执行器组合可控性（design 点）")
    add("")
    add("| 1/3 oct | 组合 | rank_min | kappa_max | D_null dB | D_meas dB | bottleneck |")
    add("| --- | --- | --- | --- | --- | --- | --- |")
    for band in result["bands"]:
        for combo in ("SUB1", "SUB1+FL+FR", "ALL5"):
            item = (band.get("combos") or {}).get(combo) or {}
            if item.get("status") != "OK":
                continue
            depth = item.get("depth") or {}
            add("| %s | %s | %s | %s | %s | %s | %s |" % (
                _band_label(band["center_hz"]), combo, item.get("rank_min"),
                _fmt(item.get("kappa_max"), 2),
                _fmt(depth.get("nullspace_attenuation_db"), 1),
                _fmt(depth.get("measurement_limited_depth_db"), 1),
                depth.get("bottleneck")))
    add("")
    add("D_null = 10log10(P/(P-r))（随机空间目标的平均抵消上限）；"
        "D_meas = 20log10(1/(eps*kappa))，eps=10^(-SNR/20)。两者取最小为 bottleneck；"
        "都不是承诺，只是两个失效机制的量化暴露。")
    add("")

    add("## 5. 执行器两两复相干性（带平均，幅值/相位）")
    add("")
    add("| 1/3 oct | 最相干三对（abs(c), angle deg） | >0.95 冗余对 |")
    add("| --- | --- | --- |")
    for band in result["bands"]:
        pairs = (band.get("coherence") or {}).get("pairs", [])[:3]
        text = "; ".join("%s (%.3f, %.1f)" % (p["pair"].replace("|", "/"),
                                               p["magnitude"], p["phase_deg"])
                         for p in pairs)
        redundant = ", ".join(
            pair.replace("|", "/") for pair in
            (band.get("coherence") or {}).get("redundant_pairs_gt_0_95") or []) or "-"
        add("| %s | %s | %s |" % (_band_label(band["center_hz"]), text or "-",
                                   redundant))
    add("")
    add("定义：`c_ij(f)=|sum_p w_p conj(H[p,i,f])H[p,j,f]|/(||h_i|| ||h_j||)`，"
        "带平均在 p 与 f 上求和（w_f=1.0）；1=两执行器空间签名相同（冗余），"
        "0=正交（独立）。")
    add("")

    add("## 6. 正则化 trade-off（ALL5，lambda 扫描）")
    add("")
    add("| 1/3 oct | lambda=0 residual/effort | L-curve lambda | residual@L | "
        "effort@L | min residual @ cap 0.01/0.1/1.0 |")
    add("| --- | --- | --- | --- | --- | --- |")
    pf = result["per_frequency"]
    for band in result["bands"]:
        if band.get("status") != "OK":
            continue
        mask = X.band_mask(np.asarray(pf["freq_hz"]), band["center_hz"])
        idx = np.nonzero(mask)[0]
        entry = pf["regularization"]["ALL5"]["random"]
        r0 = [entry["residual_rel"][fi][0] for fi in idx
              if entry["residual_rel"][fi] is not None]
        e0 = [entry["effort"][fi][0] for fi in idx
              if entry["effort"][fi] is not None]
        lc = [entry["lcurve_lambda"][fi] for fi in idx
              if entry["lcurve_lambda"][fi] is not None]
        rl = []
        el = []
        lam_list = entry.get("lambda") or []
        for fi in idx:
            lam = entry["lcurve_lambda"][fi]
            if lam is None or not lam_list:
                continue
            li = min(range(len(lam_list)), key=lambda k: abs(lam_list[k] - lam))
            if entry["residual_rel"][fi] is not None:
                rl.append(entry["residual_rel"][fi][li])
                el.append(entry["effort"][fi][li])
        capvals = []
        for cap in ("0.01", "0.1", "1.0"):
            vals = []
            for fi in idx:
                for c in entry["effort_caps"][fi] or []:
                    if abs(c["effort_cap"] - float(cap)) < 1e-12 and c["reachable"]:
                        vals.append(c["min_residual_rel"])
            capvals.append(_fmt(np.median(vals), 3) if vals else "unreachable")
        add("| %s | %s / %s | %s | %s | %s | %s |" % (
            _band_label(band["center_hz"]),
            _fmt(np.median(r0), 3) if r0 else "-",
            _fmt(np.median(e0), 3) if e0 else "-",
            _fmt(np.median(lc), 6) if lc else "-",
            _fmt(np.median([v for v in rl if v is not None]), 3) if rl else "-",
            _fmt(np.median([v for v in el if v is not None]), 3) if el else "-",
            " / ".join(capvals)))
    add("")
    add("探针 target 为单位范数（每频点固定种子复高斯方向）；H 按 sigma_1 归一化，故 "
        "lambda 相对 sigma_1^2=1；residual=||Hn g - t||，effort=sum|g|^2。曲线 CSV："
        "`lambda_tradeoff.csv`；每频点全 lambda 见 hdiag.json。")
    add("")

    add("## 7. design vs holdout 空间一致性")
    add("")
    add("预测器：`design_complex_mean`（design 点复数均值，identity G 占位；将来用 "
        "design-G 预测替换）。判据阈值 ±%.1f dB（数据一致性用，不是 spec 判据 A/B）。" %
        result["input"]["declared_parameters"]["holdout_agreement_tol_db"])
    add("")
    add("| 1/3 oct | 设计/保持秩 (min) | 主子空间最大夹角 deg | sigma_min overlap | "
        "过拟合风险 |")
    add("| --- | --- | --- | --- | --- |")
    for band in result["bands"]:
        sub = band.get("subspace") or {}
        add("| %s | %s / %s | %s | %s | %s |" % (
            _band_label(band["center_hz"]),
            _fmt(sub.get("rank_design_min"), 0),
            _fmt(sub.get("rank_holdout_min"), 0),
            _fmt(sub.get("max_angle_deg"), 1),
            _fmt(sub.get("sigma_min_overlap_median"), 3),
            ("YES" if sub.get("overfit_risk")
             else ("-" if sub.get("overfit_risk") is None else "no"))))
    add("")
    hc = result["holdout_consistency"]
    add("holdout prediction：状态 **%s**（有效 holdout 点 %d / 需要 %d）；%s" % (
        hc["status"], hc["valid_holdout_points"], hc["required_holdout_points"],
        hc["reason"]))
    add("")
    add("| holdout 点 | 1/3 oct | R_k(design 预测) | dev dB | R_k(holdout 实测) | "
        "偏差 dB | 状态 |")
    add("| --- | --- | --- | --- | --- | --- | --- |")
    for point in hc["points"]:
        for band in point.get("bands") or []:
            add("| %s | %s | %s | %s | %s | %s | %s |" % (
                point["label"], _band_label(band["center_hz"]),
                _fmt(band["R_k_design_predicted"], 4),
                _fmt(band["delta_db_design_predicted"], 1),
                _fmt(band["R_k_holdout_measured"], 4),
                _fmt(band["deviation_db"], 2), band["status"]))
    add("")
    add("R_k 为 spec §2.2 的归一化残余能量比；identity G 下实测侧 R_k=1（占位），"
        "预测侧为 design 均值场预测。C 未判（无 design G）：%s" % hc["C_spec_reason"])
    add("")
    add("子空间对比使用**右奇异向量 V（执行器空间 C^N）**；左奇异向量属于位置空间，"
        "design/holdout 点数与点位不同，不可直接比较。")
    add("")

    add("## 8. 状态位（供后续 spec 判据消费）")
    add("")
    add("### ENERGY_MIGRATION")
    add("")
    add("- status：**%s**；applicable=%s；basis=%s；eps=%.3f（约 +%.2f dB）" % (
        em["status"], em["applicable"], em["basis"], em["epsilon"],
        em["threshold_db"]))
    if em["bands"]:
        add("")
        add("| 1/3 oct | R_k | delta dB | E_before | E_after | status |")
        add("| --- | --- | --- | --- | --- | --- |")
        for row in em["bands"]:
            add("| %s | %s | %s | %.3e | %.3e | %s |" % (
                _band_label(row["center_hz"]), _fmt(row["R_k"], 4),
                _fmt(row["delta_db"], 2), row["E_before"], row["E_after"],
                row["status"]))
    else:
        add("- 无 measured after 数据：每带绝对 dB 变化/归一化残余不可计算（不伪造）。")
    add("")
    add("### HOLDOUT_SEVERE_REGRESSION")
    add("")
    add("- status：**%s**；threshold > +%.1f dB；applicable=%s；basis=%s" % (
        sr["status"], sr["threshold_db"], sr["applicable"], sr["basis"]))
    if sr["regressions"]:
        add("")
        add("| holdout 点 | 1/3 oct | delta dB |")
        add("| --- | --- | --- |")
        for row in sr["regressions"]:
            add("| %s | %s | %.2f |" % (row["label"],
                                        _band_label(row["center_hz"]),
                                        row["delta_db"]))
    add("")

    add("## 9. 图 / CSV（matplotlib 缺失时降级）")
    add("")
    plots_dir = os.path.join(out_dir or "", "plots")
    if out_dir and os.path.isdir(plots_dir):
        add("- PNG：`plots/singular_value_spectrum.png`, "
            "`plots/condition_vs_frequency.png`, `plots/coherence_heatmap.png`, "
            "`plots/lambda_tradeoff.png`")
    else:
        add("- 本机无 matplotlib：按要求降级为 CSV（`per_frequency.csv`, "
            "`bands.csv`, `coherence_band.csv`, `lambda_tradeoff.csv`, "
            "`holdout_prediction.csv`），曲线可自行用 Excel/pandas 绘制。")
    add("")

    add("## 10. NOT_RUN（必须随报告一起读）")
    add("")
    add("| 项 | 状态 | 原因 |")
    add("| --- | --- | --- |")
    for item in result["not_run"]:
        add("| %s | %s | %s |" % (item["item"], item["status"], item["reason"]))
    add("")

    add("## 11. 每项指标定义（实现口径）")
    add("")
    for key, value in result["definitions"].items():
        add("- `%s`：%s" % (key, value))
    add("")
    add("---")
    add("工具：`rt-stack/tools/h-diag/`；证据："
        "`rt-stack/evidence/h-diag-20261121/`；机器可读全量字段见 "
        "`hdiag.json`（schema `%s`）。" % result["schema"])
    return chr(10).join(lines) + chr(10)


def _band_label(center):
    return "%.0f Hz" % center


def _fmt(value, digits=2):
    if value is None:
        return "-"
    try:
        return ("%." + str(digits) + "f") % float(value)
    except (TypeError, ValueError):
        return str(value)


def _short_codes(block):
    codes = block.get("codes") or []
    if not codes:
        return "ok"
    text = ", ".join("`%s`" % c for c in codes[:8])
    return text + (" (+%d)" % (len(codes) - 8) if len(codes) > 8 else "")
