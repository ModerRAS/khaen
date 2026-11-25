# -*- coding: utf-8 -*-
"""Adaptive acceptance gate for two-stage / knee decays (NON-STANDARD, opt-in).

The frozen spec gate is about the single T20: repeat range of one number plus fit R2.
For a band whose EDC is two-stage that gate rejects a *reproducible* decay because a
single slope cannot describe it.  This module defines the parallel gate:

  * automatic model path (@@segmented_gate@@): the segment count is selected per
    repeat by decayfit.models; the gate passes only if
      - every repeat selects the same segment count (else AMBIGUOUS),
      - per segment R2_seg >= 0.95 in every repeat,
      - per segment T20_seg is repeat-stable (median < 0.2 s -> absolute range
        <= 0.05 s; else relative <= 10% and absolute <= 0.15 s),
      - knee time range <= max(25 ms, 15% of the median knee time);
  * canonical fixed-split path (@@evaluate_fixed_split@@): the two fixed -5..-15 dB
    and -15..-25 dB segments.  PASS_FIXED_TWO requires both segments to be
    repeat-stable AND the two T20s to differ by >= 1.5x (otherwise the band is
    single-slope).  When only the early segment is certifiable and a repeatable
    max-curvature knee marks where the decay stalls, the status is PASS_EARLY_ONLY
    (early segment + knee position certified; no late T20 claim).

Statuses: PASS_SEGMENTED / PASS_FIXED_TWO / PASS_EARLY_ONLY / FAIL_SEGMENTED /
FAIL_FIXED / AMBIGUOUS / SINGLE_SLOPE / NO_COVERAGE.
AMBIGUOUS is never a pass.  NOT a redefinition of T20: the standard T20 gate is
still computed and reported (standard_t20_gate) as the control.
"""
import numpy as np

SEG_R2_MIN = 0.95
SEG_REL_RANGE_MAX = 0.10
SEG_ABS_RANGE_MAX_S = 0.05
SEG_LONG_ABS_RANGE_MAX_S = 0.15
SEG_SHORT_T20_S = 0.20
KNEE_ABS_RANGE_MAX_MS = 25.0
KNEE_REL_RANGE_MAX = 0.15
KNEE_SLOPE_DROP_MAX = 0.60          # curvature knee counts only if the local slope drops
SLOPE_RATIO_MIN = 2.0

# frozen single-slope gate constants (spec v1.2, mirrored in irdiag/metrics.py)
FIT_R2_MIN = 0.90
T20_ABS_RANGE_MAX_S = 0.05
T20_REL_RANGE_MAX = 0.10
T20_RANGE_MAX_S = 0.15


def _range(vals):
    v = [x for x in vals if x is not None]
    if not v:
        return None, None, None
    med = float(np.median(v))
    rng = float(max(v) - min(v)) if len(v) > 1 else 0.0
    rel = (rng / med) if med > 0 else None
    return med, rng, rel


def _seg_check(segs, name):
    """Check one fixed segment over repeats. Returns (ok, reasons, stats)."""
    reasons = []
    n_undef = sum(1 for s in segs if not s)
    if n_undef:
        return False, ["%s segment undefined in %d/%d repeats" % (name, n_undef, len(segs))], {}
    r2s = [s.get("r2") for s in segs]
    t20s = [s.get("t20_s") for s in segs]
    med, rng, rel = _range(t20s)
    stats = {"t20_median_s": med, "t20_range_s": rng, "t20_rel_range": rel,
             "r2_min": min([v for v in r2s if v is not None]) if any(
                 v is not None for v in r2s) else None,
             "r2_repeats": r2s, "t20_repeats_s": t20s,
             "level_range_db": [min(s.get("level0_db", 0.0) for s in segs),
                                max(s.get("level1_db", 0.0) for s in segs)]}
    if stats["r2_min"] is not None and stats["r2_min"] < SEG_R2_MIN:
        reasons.append("%s segment R2 min %.3f < %.2f" % (name, stats["r2_min"], SEG_R2_MIN))
    if med is None:
        reasons.append("%s segment T20 undefined" % name)
    elif med < SEG_SHORT_T20_S:
        if rng is not None and rng > SEG_ABS_RANGE_MAX_S:
            reasons.append("%s T20 range %.3f s > %.2f s (short-T20 absolute)"
                           % (name, rng, SEG_ABS_RANGE_MAX_S))
    else:
        if rel is not None and rel > SEG_REL_RANGE_MAX:
            reasons.append("%s T20 relative range %.1f%% > %.0f%%"
                           % (name, 100 * rel, 100 * SEG_REL_RANGE_MAX))
        if rng is not None and rng > SEG_LONG_ABS_RANGE_MAX_S:
            reasons.append("%s T20 range %.3f s > %.2f s" % (name, rng, SEG_LONG_ABS_RANGE_MAX_S))
    return (not reasons), reasons, stats


def _knee_repeat_check(fits):
    """Max-curvature knee repeatability + local slope drop."""
    ts = [((f.get("curvature_knee") or {}).get("t_s")) for f in fits]
    ratios = [((f.get("curvature_knee") or {}).get("slope_ratio")) for f in fits]
    levels = [((f.get("curvature_knee") or {}).get("level_db")) for f in fits]
    if any(t is None for t in ts):
        return False, ["curvature knee undefined in some repeats"], {}
    med, rng, rel = _range(ts)
    allow = max(KNEE_ABS_RANGE_MAX_MS, 1000.0 * KNEE_REL_RANGE_MAX * med)
    r2 = [x for x in ratios if x is not None]
    ratio_med = float(np.median(r2)) if r2 else None
    lv = [x for x in levels if x is not None]
    t_lo = (fits[0].get("fit_range") or {}).get("t_lo_s", 0.0) or 0.0
    stats = {"knee_t_median_ms": 1000.0 * med, "knee_t_range_ms": 1000.0 * rng,
             "knee_t_median_abs_s": float(t_lo + med),
             "knee_t_median_abs_ms": 1000.0 * (t_lo + med),
             "knee_t_rel_range": rel, "allow_ms": allow,
             "slope_ratio_median": ratio_med,
             "knee_level_db_median": float(np.median(lv)) if lv else None,
             "knee_t_repeats_ms": [1000.0 * x for x in ts]}
    reasons = []
    if rng * 1000.0 > allow:
        reasons.append("knee time range %.0f ms > %.0f ms" % (rng * 1000.0, allow))
    if ratio_med is not None and ratio_med > KNEE_SLOPE_DROP_MAX:
        reasons.append("curvature knee slope ratio %.2f > %.2f (no real slowdown)"
                       % (ratio_med, KNEE_SLOPE_DROP_MAX))
    return (not reasons), reasons, stats


def evaluate_fixed_split(fits):
    """Canonical -5..-15 / -15..-25 dB split evaluation over the repeats."""
    fixed = [f.get("fixed_split") or {} for f in fits]
    early_ok, early_reasons, early = _seg_check([x.get("early") for x in fixed], "early")
    late_ok, late_reasons, late = _seg_check([x.get("late") for x in fixed], "late")
    knee_ok, knee_reasons, knee = _knee_repeat_check(fits)
    ratio = None
    if early.get("t20_median_s") and late.get("t20_median_s"):
        ratio = late["t20_median_s"] / early["t20_median_s"]
    two_slope = bool(ratio is not None and max(ratio, 1.0 / ratio) >= SLOPE_RATIO_MIN)
    if early_ok and late_ok:
        if two_slope:
            status, reasons = "PASS_FIXED_TWO", []
        else:
            status, reasons = "SINGLE_SLOPE", [
                "fixed segments consistent with one slope (T20 ratio %.2f < %.1f)"
                % (ratio if ratio else 0.0, SLOPE_RATIO_MIN)]
    elif early_ok and knee_ok:
        status, reasons = "PASS_EARLY_ONLY", []
    else:
        status = "FAIL_FIXED"
        reasons = early_reasons + ([] if early_ok else []) + \
                  (late_reasons if not late_ok else []) + (knee_reasons if not knee_ok else [])
    return {"not_standard": True,
            "status": status, "reasons": reasons,
            "split_db": [[-5.0, -15.0], [-15.0, -25.0]],
            "early": early, "late": late, "knee": knee,
            "t20_ratio_late_over_early": ratio, "two_slope_evidence": two_slope,
            "thresholds": {"SEG_R2_MIN": SEG_R2_MIN, "SEG_REL_RANGE_MAX": SEG_REL_RANGE_MAX,
                           "SEG_ABS_RANGE_MAX_S": SEG_ABS_RANGE_MAX_S,
                           "SEG_LONG_ABS_RANGE_MAX_S": SEG_LONG_ABS_RANGE_MAX_S,
                           "KNEE_ABS_RANGE_MAX_MS": KNEE_ABS_RANGE_MAX_MS,
                           "KNEE_REL_RANGE_MAX": KNEE_REL_RANGE_MAX,
                           "KNEE_SLOPE_DROP_MAX": KNEE_SLOPE_DROP_MAX,
                           "SLOPE_RATIO_MIN": SLOPE_RATIO_MIN}}


def standard_t20_gate(t20_s, r2, edc100_db=None):
    """Frozen single-slope gate (control), restricted to the T20/R2/EDC100 part."""
    reasons = []
    t20s = [v for v in t20_s if v is not None]
    if len(t20s) != len(t20_s):
        reasons.append("T20 undefined in %d/%d repeats" % (len(t20_s) - len(t20s), len(t20_s)))
    if any(v is None or v < FIT_R2_MIN for v in r2):
        reasons.append("fit R2 below %.2f (%s)" % (FIT_R2_MIN,
                       "/".join("%.3f" % v if v is not None else "na" for v in r2)))
    if t20s:
        med, rng, rel = _range(t20s)
        if med is None:
            med = float(np.median(t20s))
        if med < SEG_SHORT_T20_S:
            if rng is not None and rng > T20_ABS_RANGE_MAX_S:
                reasons.append("T20 repeat range %.3f s > %.2f s" % (rng, T20_ABS_RANGE_MAX_S))
        else:
            if rel is not None and rel > T20_REL_RANGE_MAX:
                reasons.append("T20 relative repeat range %.1f%% > %.0f%%"
                               % (100 * rel, 100 * T20_REL_RANGE_MAX))
            if rng is not None and rng > T20_RANGE_MAX_S:
                reasons.append("T20 repeat range %.3f s > %.2f s" % (rng, T20_RANGE_MAX_S))
    if edc100_db:
        v = [x for x in edc100_db if x is not None]
        if len(v) > 1 and (max(v) - min(v)) > 1.5:
            reasons.append("EDC@100ms repeat range %.2f dB > 1.5 dB" % (max(v) - min(v)))
    return {"status": "OK" if not reasons else "UNRELIABLE", "reasons": reasons,
            "t20_median_s": (float(np.median(t20s)) if t20s else None),
            "t20_range_s": (float(max(t20s) - min(t20s)) if len(t20s) > 1 else None),
            "t20_rel_range": ((max(t20s) - min(t20s)) / np.median(t20s)
                              if len(t20s) > 1 else None)}


def segmented_gate(fits):
    """Gate on a list of per-repeat piecewise-fit results (models.fit_piecewise)."""
    out = {"standard": "non_standard", "not_standard_note":
           "segmented decay description; NOT the spec T20. Single-slope T20 is reported separately."}
    ok_fits = []
    coverage_bad = 0
    for f in fits:
        if f is None or f.get("status") != "OK":
            coverage_bad += 1
        else:
            ok_fits.append(f)
    fixed_eval = evaluate_fixed_split(ok_fits) if ok_fits else None
    out["fixed_split"] = fixed_eval
    if coverage_bad:
        out.update({"status": "NO_COVERAGE",
                    "reasons": ["%d/%d repeats have no valid -5/-25 dB coverage above the noise floor"
                                % (coverage_bad, len(fits))]})
        return out
    nseg = [f["selected"]["n_seg"] for f in ok_fits]
    sel_status = [f["selected"]["status"] for f in ok_fits]
    out["n_seg_repeats"] = nseg
    if len(set(nseg)) != 1:
        out.update({"status": "AMBIGUOUS",
                    "reasons": ["segment count varies across repeats: %s" % nseg]})
        return out
    n = nseg[0]
    out["n_seg"] = n
    if n == 1:
        fx = fixed_eval or {}
        st = fx.get("status", "SINGLE_SLOPE")
        out.update({"status": st,
                    "reasons": fx.get("reasons") or
                    ["all repeats select a single slope; fixed early/late split is the control"]})
        return out
    if any(s != "OK" for s in sel_status):
        out.update({"status": "AMBIGUOUS",
                    "reasons": ["model status not OK in all repeats: %s" % sel_status]})
        return out

    reasons = []
    seg_out = []
    for k in range(n):
        t20s = [f["t20_seg_s"][k] for f in ok_fits]
        r2s = [f["segments"][k]["r2"] for f in ok_fits]
        med, rng, rel = _range(t20s)
        seg = {"segment": k, "t20_repeats_s": t20s, "r2_repeats": r2s,
               "t20_median_s": med, "t20_range_s": rng, "t20_rel_range": rel,
               "r2_min": min([r for r in r2s if r is not None]) if any(
                   r is not None for r in r2s) else None,
               "t_range_s": [ok_fits[0]["segments"][k]["t0"],
                             ok_fits[0]["segments"][k]["t1"]],
               "level_range_db": [ok_fits[0]["segments"][k]["level0"],
                                  ok_fits[0]["segments"][k]["level1"]]}
        if seg["r2_min"] is not None and seg["r2_min"] < SEG_R2_MIN:
            reasons.append("segment %d R2 min %.3f < %.2f" % (k, seg["r2_min"], SEG_R2_MIN))
        if med is not None:
            if med < SEG_SHORT_T20_S:
                if rng is not None and rng > SEG_ABS_RANGE_MAX_S:
                    reasons.append("segment %d T20 range %.3f s > %.2f s (short-T20 absolute)"
                                   % (k, rng, SEG_ABS_RANGE_MAX_S))
            else:
                if rel is not None and rel > SEG_REL_RANGE_MAX:
                    reasons.append("segment %d T20 relative range %.1f%% > %.0f%%"
                                   % (k, 100 * rel, 100 * SEG_REL_RANGE_MAX))
                if rng is not None and rng > SEG_LONG_ABS_RANGE_MAX_S:
                    reasons.append("segment %d T20 range %.3f s > %.2f s"
                                   % (k, rng, SEG_LONG_ABS_RANGE_MAX_S))
        seg_out.append(seg)

    knees_out = []
    if n == 2:
        series = [[f["knee"]["t_s"]] for f in ok_fits]
    else:
        series = [[f["knee"]["knees"][j]["t_s"] for j in range(n - 1)] for f in ok_fits]
    for j in range(n - 1):
        ts = [s[j] for s in series]
        med, rng, rel = _range(ts)
        t_lo = (ok_fits[0].get("fit_range") or {}).get("t_lo_s", 0.0) or 0.0
        k = {"knee": j + 1, "t_repeats_s": ts, "t_median_s": med,
             "t_median_abs_s": (float(t_lo + med) if med is not None else None),
             "t_range_s": rng, "t_rel_range": rel,
             "t_range_ms": (rng * 1000.0 if rng is not None else None)}
        if med is not None and med > 0:
            allow = max(KNEE_ABS_RANGE_MAX_MS, 1000.0 * KNEE_REL_RANGE_MAX * med)
            if k["t_range_ms"] > allow:
                reasons.append("knee %d repeat range %.0f ms > %.0f ms"
                               % (j + 1, k["t_range_ms"], allow))
        knees_out.append(k)

    out.update({"status": "PASS_SEGMENTED" if not reasons else "FAIL_SEGMENTED",
                "reasons": reasons, "segments": seg_out, "knees": knees_out,
                "thresholds": {"SEG_R2_MIN": SEG_R2_MIN,
                               "SEG_REL_RANGE_MAX": SEG_REL_RANGE_MAX,
                               "SEG_ABS_RANGE_MAX_S": SEG_ABS_RANGE_MAX_S,
                               "SEG_LONG_ABS_RANGE_MAX_S": SEG_LONG_ABS_RANGE_MAX_S,
                               "KNEE_ABS_RANGE_MAX_MS": KNEE_ABS_RANGE_MAX_MS,
                               "KNEE_REL_RANGE_MAX": KNEE_REL_RANGE_MAX}})
    return out
