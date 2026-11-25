# -*- coding: utf-8 -*-
"""Opt-in design-weight / target-allocation proposal derived from the decay fit.

mimo-objective-v1 v1.1 fixes the band weights as energy-only.  The decay severity
factor proposed here multiplies the energy weight by

    s_k = clamp(T20_late_k / median_k(T20_late), 0.5, 2.0)
    alpha_severity_k = (a_k * s_k) * K / sum_j(a_j * s_j)

as requested (longer late T20 -> higher weight, clamped, renormalised).  It only
changes how the residual is ALLOCATED between bands.  It does NOT change any room
physics and cannot make an open-loop FIR shorten a room pole.

Because a long late segment is exactly what an open-loop FIR cannot change (spec
v1.2 section 11), a second weight is also produced:

    alpha_recommended_k = (a_k * actionability_k) * K / sum(...)
    actionability_k = clamp(1 - A2_biexp, 0.2, 1.0)

alpha_recommended avoids spending effort on bands whose energy is late/pole
dominated; alpha_severity is the literal requested variant.  Both are opt-in:
mimo-objective-v1 v1.1 stays the default and is unchanged.

The module also emits the executable per-band JSON proposed for a future
merge/fit/experiment-G step: segment count, target residual ratio, whether to
prioritise the initial energy (cut-only PEQ), whether to give up the late segment.
"""
import numpy as np

BAND_CENTERS_HZ = [40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0]
ALPHA_MIN = 0.5
ALPHA_MAX = 2.0
SEVERITY_MIN = 0.5
SEVERITY_MAX = 2.0
ACTION_MIN = 0.2
ACTION_MAX = 1.0
TARGET_DB_PER_ACTION = 3.0
TARGET_DB_MIN = 0.5
TARGET_DB_MAX = 3.0
LATE_DOMINANT_S = 0.30


def _clamp(v, lo, hi):
    return float(max(lo, min(hi, v)))


def energy_weights(energy_db):
    """mimo-objective-v1 v1.1 energy-only alpha_k (control / default)."""
    bands = [b for b in BAND_CENTERS_HZ if energy_db.get(b) is not None]
    if not bands:
        return {}
    e = np.array([10.0 ** (energy_db[b] / 10.0) for b in bands], dtype=np.float64)
    x = e / e.mean()
    a = np.clip(x, ALPHA_MIN, ALPHA_MAX)
    alpha = a * len(bands) / a.sum()
    return {b: float(v) for b, v in zip(bands, alpha)}


def severity_weights(energy_db, t20_late_s, t20_late_source):
    """Energy weights modulated by the decay-severity factor (literal request)."""
    bands = [b for b in BAND_CENTERS_HZ if energy_db.get(b) is not None
             and t20_late_s.get(b) is not None]
    if not bands:
        return {}, {}
    e = np.array([10.0 ** (energy_db[b] / 10.0) for b in bands], dtype=np.float64)
    x = e / e.mean()
    a = np.clip(x, ALPHA_MIN, ALPHA_MAX)
    late = np.array([t20_late_s[b] for b in bands], dtype=np.float64)
    ref = float(np.median(late))
    sev = np.clip(late / ref, SEVERITY_MIN, SEVERITY_MAX)
    w = a * sev
    alpha = w * len(bands) / w.sum()
    return ({b: float(v) for b, v in zip(bands, alpha)},
            {b: float(v) for b, v in zip(bands, sev)})


def actionability_weights(energy_db, actionability):
    bands = [b for b in BAND_CENTERS_HZ if energy_db.get(b) is not None
             and actionability.get(b) is not None]
    if not bands:
        return {}
    e = np.array([10.0 ** (energy_db[b] / 10.0) for b in bands], dtype=np.float64)
    x = e / e.mean()
    a = np.clip(x, ALPHA_MIN, ALPHA_MAX)
    act = np.clip(np.array([actionability[b] for b in bands], dtype=np.float64),
                  ACTION_MIN, ACTION_MAX)
    w = a * act
    alpha = w * len(bands) / w.sum()
    return {b: float(v) for b, v in zip(bands, alpha)}


def band_actionability(band_fit, biexp_key="biexp"):
    """Fraction of the in-window decay energy an open-loop FIR could plausibly touch.

    Lower value = the late (room-pole) part dominates, so weighting it up would
    spend design effort on something an open-loop FIR cannot change.
    """
    if band_fit.get("n_seg", 1) and band_fit.get("n_seg", 1) >= 2:
        bx = band_fit.get(biexp_key) or {}
        if bx.get("status") == "OK" and bx.get("a2") is not None:
            return _clamp(1.0 - float(bx["a2"]), ACTION_MIN, ACTION_MAX), "biexp_a2"
        t20 = band_fit.get("t20_seg_s") or []
        if len(t20) >= 2 and t20[0] and t20[1]:
            return _clamp(t20[0] / t20[1], ACTION_MIN, ACTION_MAX), "early_late_ratio"
    gate = band_fit.get("gate") or {}
    fx = gate.get("fixed_split") or {}
    e = (fx.get("early") or {}).get("t20_median_s")
    l = (fx.get("late") or {}).get("t20_median_s")
    if e and l:
        return _clamp(e / l, ACTION_MIN, ACTION_MAX), "fixed_split_ratio"
    return 0.5, "single_slope_default"


def decay_class(gate):
    if gate is None:
        return "UNKNOWN"
    st = gate.get("status")
    n = gate.get("n_seg")
    if st == "PASS_SEGMENTED":
        return "TWO_STAGE_STABLE" if n == 2 else "THREE_STAGE_STABLE"
    if st == "PASS_FIXED_TWO":
        return "TWO_STAGE_STABLE"
    if st == "PASS_EARLY_ONLY":
        return "EARLY_SEGMENT_KNEE"
    fx = ((gate.get("fixed_split") or {}) or {}).get("status")
    if st in ("FAIL_SEGMENTED", "AMBIGUOUS", "SINGLE_SLOPE") and fx == "PASS_FIXED_TWO":
        return "TWO_STAGE_STABLE_FIXED"
    if st in ("FAIL_SEGMENTED", "AMBIGUOUS", "SINGLE_SLOPE") and fx == "PASS_EARLY_ONLY":
        return "EARLY_SEGMENT_KNEE"
    if st == "FAIL_SEGMENTED":
        return "TWO_STAGE_UNSTABLE"
    if st == "AMBIGUOUS":
        return "AMBIGUOUS"
    if st == "SINGLE_SLOPE":
        return "SINGLE_SLOPE"
    return st or "UNKNOWN"


def band_suggestion(band_fit, gate, alpha_energy=None, alpha_sev=None,
                    alpha_rec=None, s2_effect=None):
    """One per-band suggestion record (advisory; no room-physics claim)."""
    b = float(band_fit["band_hz"])
    cls = decay_class(gate)
    t20 = band_fit.get("t20_seg_s") or []
    n = band_fit.get("n_seg", 1)
    act, act_src = band_actionability(band_fit)
    late = t20[-1] if len(t20) >= 1 else None
    early = t20[0] if len(t20) >= 1 else None
    s2 = (s2_effect or {}).get(b) or {}
    dt_late = s2.get("delta_t20_late_s")
    rep_late = s2.get("repeatability_s")
    if dt_late is None:
        dt_late = s2.get("delta_fixed_late_s")
        rep_late = s2.get("repeatability_fixed_late_s")
    within = (dt_late is not None and rep_late is not None and abs(dt_late) <= rep_late)
    gate_d = gate or {}
    fx = gate_d.get("fixed_split") or {}
    knee_ms = None
    if n >= 2 and band_fit.get("knee"):
        kn = band_fit["knee"]
        if kn.get("knees"):
            knee_ms = [1000.0 * float(k.get("t_abs_s", k["t_s"])) for k in kn["knees"]]
        elif kn.get("t_abs_s") is not None:
            knee_ms = 1000.0 * float(kn["t_abs_s"])
        elif kn.get("t_s") is not None:
            knee_ms = 1000.0 * float(kn["t_s"])
    if n < 2 or cls == "EARLY_SEGMENT_KNEE":
        fe = (fx.get("early") or {}).get("t20_median_s")
        fl = (fx.get("late") or {}).get("t20_median_s")
        if fe:
            early = fe
        if fl:
            late = fl
        if knee_ms is None or cls == "EARLY_SEGMENT_KNEE":
            knee_ms = (fx.get("knee") or {}).get("knee_t_median_abs_ms")
    target_db = _clamp(TARGET_DB_PER_ACTION * act, TARGET_DB_MIN, TARGET_DB_MAX)
    tail_t20 = None
    if cls == "EARLY_SEGMENT_KNEE":
        # declare the early segment + knee; the auto-model late tail is a plateau
        # descriptor, not a certifiable T20 -> keep it as a reference field only
        tail_t20 = late
        late = None
    two_stage = bool(n >= 2 or cls in ("EARLY_SEGMENT_KNEE", "TWO_STAGE_STABLE",
                                       "TWO_STAGE_STABLE_FIXED"))
    late_dominant = bool(two_stage and late is not None and late >= LATE_DOMINANT_S
                         and early is not None and late >= 1.5 * early)
    give_up_late = bool(two_stage and (late_dominant or cls == "EARLY_SEGMENT_KNEE"
                                       or act < 0.5) and (within or not s2))
    prefer_early_cut = bool(two_stage and early is not None
                            and (late is None or early <= 0.5 * late))
    rationale = []
    if two_stage:
        if late is None:
            rationale.append("early segment + knee certifiable: T20_early=%.3f s, knee=%s; "
                             "late segment not certifiable (tail T20 %s s is a plateau "
                             "descriptor, not a decay)"
                             % (early or 0.0,
                                ("%.0f ms" % knee_ms) if isinstance(knee_ms, float) else str(knee_ms),
                                None if tail_t20 is None else round(tail_t20, 2)))
        else:
            rationale.append("two-stage EDC: T20_early=%.3f s, T20_late=%.3f s, knee=%s"
                             % (early or 0.0, late or 0.0,
                                ("%.0f ms" % knee_ms) if isinstance(knee_ms, float) else str(knee_ms)))
    else:
        rationale.append("single-slope EDC: standard T20 applies")
    if two_stage:
        rationale.append("open-loop FIR changes initial energy, not the late room pole "
                         "(spec v1.2 section 11)")
    if within:
        rationale.append("one-condition PEQ experiment (S2, 100 Hz -6 dB Q1.0) moved the late "
                         "segment by %.3f s (<= repeatability %.3f s) -> late segment not a target"
                         % (dt_late, rep_late))
    return {
        "band_hz": b,
        "decay_class": cls,
        "n_seg": n,
        "gate_status": (gate or {}).get("status"),
        "knee_ms": knee_ms,
        "t20_early_s": early,
        "t20_late_s": late,
        "tail_t20_s_reference_only": tail_t20,
        "t20_late_source": ("not_certifiable" if cls == "EARLY_SEGMENT_KNEE"
                            else ("t20_late" if two_stage else "single_t20")),
        "actionability": act,
        "actionability_source": act_src,
        "alpha_energy": alpha_energy,
        "alpha_severity": alpha_sev,
        "alpha_recommended": alpha_rec,
        "target_reduction_db": target_db,
        "target_residual_ratio": float(10.0 ** (-target_db / 10.0)),
        "prefer_early_energy_cut": prefer_early_cut,
        "give_up_late": give_up_late,
        "s2_effect": s2 or None,
        "rationale": rationale,
        "not_physical_claim": True,
    }


def build_suggestions(band_results, gate_by_band, energy_db, s2_effect=None):
    """Assemble the executable parameter-suggestion JSON.

    Only analysed bands participate (mimo-objective-v1 2.2/2.3: an N/A band does not
    enter the weighted sum), so sum(alpha) == number of analysed bands.
    """
    analysed = [b for b in BAND_CENTERS_HZ if b in band_results]
    energy_db = {b: energy_db[b] for b in analysed if energy_db.get(b) is not None}
    late = {}
    late_source = {}
    for b, br in band_results.items():
        t20 = br.get("t20_seg_s") or []
        if len(t20) >= 2 and t20[-1] is not None:
            late[b] = t20[-1]
            late_source[b] = "t20_late"
        elif t20 and t20[0] is not None:
            late[b] = t20[0]
            late_source[b] = "single_t20"
    a_energy = energy_weights(energy_db)
    a_sev, sev_factor = severity_weights(energy_db, late, late_source)
    act = {}
    for b, br in band_results.items():
        act[b] = band_actionability(br)[0]
    a_rec = actionability_weights(energy_db, act)
    bands = []
    for b in BAND_CENTERS_HZ:
        br = band_results.get(b)
        if br is None:
            bands.append({"band_hz": b, "decay_class": "NOT_ANALYSED",
                          "gate_status": "NOT_ANALYSED"})
            continue
        sug = band_suggestion(br, gate_by_band.get(b), a_energy.get(b), a_sev.get(b),
                              a_rec.get(b), s2_effect)
        sug["severity_factor"] = sev_factor.get(b)
        bands.append(sug)
    return {
        "schema": "tspi-decay-param-suggestions/v1",
        "purpose": "advisory parameters for merge/fit/experiment-G; opt-in, default "
                   "mimo-objective-v1 v1.1 energy-only weights stay in force",
        "weights": {
            "mode_control": "alpha_energy (mimo-objective-v1 v1.1, energy only)",
            "mode_requested": "alpha_severity (energy x decay severity, clamped, renormalised)",
            "mode_recommended": "alpha_recommended (energy x actionability)",
            "alpha_min": ALPHA_MIN, "alpha_max": ALPHA_MAX,
            "severity_min": SEVERITY_MIN, "severity_max": SEVERITY_MAX,
            "note": "alpha values are normalised to sum = K (number of analysed bands), "
                    "same convention as mimo-objective-v1 section 2.3",
        },
        "bands": bands,
        "honesty": {
            "changes_room_physics": False,
            "changes_standard_T20_definition": False,
            "segmented_metrics_are_non_standard": True,
            "late_segment_not_fixable_by_open_loop_FIR": True,
            "single_point_single_mic": True,
        },
    }
