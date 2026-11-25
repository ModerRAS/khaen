# -*- coding: utf-8 -*-
"""Piecewise-linear (1/2/3 segment) and double-exponential fits of a Schroeder EDC.

All fits describe the same part of the decay that the frozen T20 uses (-5 .. -25 dB,
noise margin >= 10 dB).  They are NOT a T20 redefinition: T20_early / T20_late are
*segment* descriptors, derived from each segment's slope by the same -60 dB/s rule.

Models (u = t - t_ref, continuous at the knee(s)):
    SINGLE  y = b + s1*u                                        (2 params)
    TWO     y = b + s1*u + d2*max(u-tb1,0),  s2 = s1+d2          (4 params)
    THREE   y = b + s1*u + d2*max(u-tb1,0) + d3*max(u-tb2,0)     (6 params)
Breaks are grid-searched (coarse -> local refine) with a linear least-squares solve
per candidate, so the fit is globally optimal on the candidate grid.

Selection (pre-registered, no "pick the nicest" logic):
  * every segment must span >= MIN_SEG_SPAN_DB, last >= MIN_SEG_MS and >= MIN_SEG_SAMPLES;
  * BIC_eff (BIC with an effective sample size from the residual autocorrelation) and
    an alternative dR2 >= DR2_MIN rule (EDC residuals are strongly correlated, so BIC
    alone is conservative);
  * identifiability: knee time CI (profile RSS), slope ratio, slope-change t-statistic,
    and the knee must not sit at the window edge.
  * weak evidence / failed identifiability -> AMBIGUOUS, never a forced segment count.
"""
import math

import numpy as np

# ---- pre-registered fit/selection constants (recorded in every result) -------
FIT_LO_DB = -5.0
FIT_HI_DB = -25.0
FIT_NOISE_MARGIN_DB = 10.0          # same as frozen NOISE_MARGIN_MIN_DB
MIN_SEG_SPAN_DB = 4.0
MIN_SEG_MS = 8.0
MIN_SEG_SAMPLES = 12
MIN_SEG_SAMPLES_FULL = 24           # for the 2-break model
BIC_MARGIN = 10.0                   # d_BIC needed to accept the richer model (Kass-Raftery "strong")
BIC_AMBIGUOUS = 2.0                 # d_BIC in [2,6) => weak evidence
DR2_MIN = 0.05                      # alternative evidence for a knee
SLOPE_RATIO_MIN = 2.0               # |T20_late/T20_early| (or inverse)
SLOPE_DIFF_T_MIN = 3.0              # (s2-s1)/SE(s2-s1)
KNEE_CI_MIN_MS = 25.0
KNEE_CI_FRAC = 0.20                 # of the fit-window duration
BIEXP_TAU_RATIO_MIN = 1.6
BIEXP_WEIGHT_MIN = 0.05
BIEXP_RSE_MAX = 0.60
EXTENT_HI_DB = -30.0                 # knee-detection window floor (noise margin still applies)
T20_PER_TAU = 60.0 / (10.0 / math.log(10.0))   # dB/s/T20 = 4.3429/stat -> T20=13.8155*tau


def t20_from_slope(slope_db_per_s):
    return (-60.0 / slope_db_per_s) if slope_db_per_s is not None and slope_db_per_s < 0 else None


def _autocorr_tau(resid):
    """Integrated autocorrelation time of a residual (>=1)."""
    r = np.asarray(resid, dtype=np.float64)
    r = r - r.mean()
    n = r.size
    if n < 8:
        return 1.0
    var = float(np.dot(r, r))
    if var <= 0.0:
        return 1.0
    # FFT autocorrelation
    nfft = 1
    while nfft < 2 * n:
        nfft <<= 1
    f = np.fft.rfft(r, nfft)
    ac = np.fft.irfft(f * np.conj(f), nfft)[:n]
    ac = ac / var
    tau = 1.0
    max_lag = min(n // 2, 500)
    for k in range(1, max_lag):
        rho = float(ac[k])
        if rho < 0.05:
            break
        tau += 2.0 * rho
    return float(max(1.0, tau))


def _n_eff(resid):
    return float(max(8.0, len(resid) / _autocorr_tau(resid)))


def _design(t, u, breaks):
    cols = [np.ones_like(u), u]
    for tb in breaks:
        cols.append(np.maximum(u - tb, 0.0))
    return np.vstack(cols).T


def _solve(t, y, breaks):
    """Least squares for given break times; returns params, rss, slopes, covariance.

    Breaks are absolute times on the same axis as t; the design uses u = t - t[0],
    so the break argument is shifted into the same relative axis.
    """
    u = t - t[0]
    X = _design(t, u, [b - t[0] for b in breaks])
    beta, *_ = np.linalg.lstsq(X, y, rcond=None)
    yhat = X @ beta
    resid = y - yhat
    rss = float(np.sum(resid * resid))
    n, p = X.shape
    slopes = [float(beta[1])]
    for k in range(2, p):
        slopes.append(slopes[-1] + float(beta[k]))
    sstot = float(np.sum((y - y.mean()) ** 2))
    r2 = 1.0 - rss / sstot if sstot > 0 else None
    return {"beta": beta, "slopes": slopes, "rss": rss, "r2": r2, "p": p,
            "n": n, "yhat": yhat, "resid": resid, "breaks": list(breaks)}


def _seg_r2(t, y, breaks, yhat):
    """Per-segment R2 (0-based segment index)."""
    edges = [t[0]] + list(breaks) + [t[-1] + 1e-12]
    out = []
    for k in range(len(edges) - 1):
        m = (t >= edges[k] - 1e-12) & (t < edges[k + 1] - 1e-12) if k < len(edges) - 2 \
            else (t >= edges[k] - 1e-12)
        yy = y[m]
        sst = float(np.sum((yy - yy.mean()) ** 2))
        sse = float(np.sum((yy - yhat[m]) ** 2))
        out.append({"r2": (1.0 - sse / sst) if sst > 0 else None,
                    "n": int(m.sum()),
                    "t0": float(edges[k]), "t1": float(min(edges[k + 1], t[-1])),
                    "level0": float(np.interp(edges[k], t, y)),
                    "level1": float(np.interp(min(edges[k + 1], t[-1]), t, y)),
                    "span_db": float(abs(np.interp(edges[k], t, y)
                                         - np.interp(min(edges[k + 1], t[-1]), t, y)))})
    return out


def _knee_ci(t, y, breaks, ib_idx, best_rss, dof):
    """Profile-likelihood CI of one break time (1 param, 95%%)."""
    F = 3.84
    thr = best_rss * (1.0 + F / max(1, dof))
    n = len(t)
    lo_i, hi_i = ib_idx, ib_idx
    # walk outwards while the profile RSS stays under the 95% threshold
    for direction in (-1, 1):
        i = ib_idx
        ok_last = ib_idx
        while 0 <= i + direction < n:
            i += direction
            others = [b for j, b in enumerate(breaks) if j != 0]
            cand = sorted([t[i]] + others)
            if len(cand) != len(breaks):
                break
            sol = _solve(t, y, cand)
            if sol["rss"] <= thr:
                ok_last = i
            else:
                break
        if direction < 0:
            lo_i = ok_last
        else:
            hi_i = ok_last
    return {"ci_lo_s": float(t[lo_i] - t[0]), "ci_hi_s": float(t[hi_i] - t[0]),
            "ci_width_ms": float(1000.0 * (t[hi_i] - t[lo_i]))}


def _valid_segments(segs, window_s):
    reasons = []
    for k, s in enumerate(segs):
        if s["span_db"] < MIN_SEG_SPAN_DB:
            reasons.append("segment %d span %.1f dB < %.1f dB" % (k, s["span_db"], MIN_SEG_SPAN_DB))
        dur_ms = 1000.0 * (s["t1"] - s["t0"])
        if dur_ms < MIN_SEG_MS:
            reasons.append("segment %d duration %.1f ms < %.0f ms" % (k, dur_ms, MIN_SEG_MS))
        if s["n"] < (MIN_SEG_SAMPLES if len(segs) == 2 else MIN_SEG_SAMPLES_FULL):
            reasons.append("segment %d has only %d samples" % (k, s["n"]))
    return reasons


def _bic(rss, n_eff, p):
    return n_eff * math.log(max(rss, 1e-300) / n_eff) + p * math.log(max(n_eff, 2.0))


def _aic(rss, n_eff, p):
    return n_eff * math.log(max(rss, 1e-300) / n_eff) + 2.0 * p


def _model_stats(sol, t, y):
    n_eff = _n_eff(sol["resid"])
    segs = _seg_r2(t, y, sol["breaks"], sol["yhat"])
    return {
        "p": sol["p"], "rss": sol["rss"], "r2": sol["r2"],
        "bic_eff": _bic(sol["rss"], n_eff, sol["p"]),
        "aic_eff": _aic(sol["rss"], n_eff, sol["p"]),
        "n_eff": n_eff,
        "slopes_db_per_s": sol["slopes"],
        "t20_seg_s": [t20_from_slope(s) for s in sol["slopes"]],
        "segments": segs,
        "breaks_s": [float(b - t[0]) for b in sol["breaks"]],
    }


def _best_two(t, y):
    n = len(t)
    step = max(1, n // 200)
    i0 = max(MIN_SEG_SAMPLES, int(MIN_SEG_MS * 1e-3 * 1.0))  # refined by span later
    best = None
    for i in range(max(2, MIN_SEG_SAMPLES), n - max(2, MIN_SEG_SAMPLES), step):
        tb = t[i]
        sol = _solve(t, y, [tb])
        if sol["rss"] <= 0:
            sol["rss"] = 1e-300
        segs = _seg_r2(t, y, sol["breaks"], sol["yhat"])
        if _valid_segments(segs, t[-1] - t[0]):
            continue
        if best is None or sol["rss"] < best["sol"]["rss"]:
            best = {"sol": sol, "i": i}
    if best is None:
        return None
    # local refine over neighbours
    for i in range(max(2, MIN_SEG_SAMPLES), n - max(2, MIN_SEG_SAMPLES)):
        if abs(i - best["i"]) > step + 2:
            continue
        sol = _solve(t, y, [t[i]])
        sol["rss"] = max(sol["rss"], 1e-300)
        segs = _seg_r2(t, y, sol["breaks"], sol["yhat"])
        if _valid_segments(segs, t[-1] - t[0]):
            continue
        if sol["rss"] < best["sol"]["rss"]:
            best = {"sol": sol, "i": i}
    return best


def _best_three(t, y):
    n = len(t)
    step = max(1, n // 70)
    i0 = max(MIN_SEG_SAMPLES_FULL, 2)
    best = None
    cand = list(range(i0, n - i0, step))
    for a in range(len(cand)):
        for b in range(a + 1, len(cand)):
            i, j = cand[a], cand[b]
            if j - i < MIN_SEG_SAMPLES_FULL:
                continue
            sol = _solve(t, y, [t[i], t[j]])
            sol["rss"] = max(sol["rss"], 1e-300)
            segs = _seg_r2(t, y, sol["breaks"], sol["yhat"])
            if _valid_segments(segs, t[-1] - t[0]):
                continue
            if best is None or sol["rss"] < best["sol"]["rss"]:
                best = {"sol": sol, "i": i, "j": j}
    if best is None:
        return None
    for i in range(i0, n - i0):
        for j in range(i + MIN_SEG_SAMPLES_FULL, n - i0):
            if abs(i - best["i"]) > step + 2 or abs(j - best["j"]) > step + 2:
                continue
            sol = _solve(t, y, [t[i], t[j]])
            sol["rss"] = max(sol["rss"], 1e-300)
            segs = _seg_r2(t, y, sol["breaks"], sol["yhat"])
            if _valid_segments(segs, t[-1] - t[0]):
                continue
            if sol["rss"] < best["sol"]["rss"]:
                best = {"sol": sol, "i": i, "j": j}
    return best


def _slope_change_stats(sol, t, y):
    """Slope ratio and t-statistic of the adjacent slope change(s)."""
    u = t - t[0]
    X = _design(t, u, [b - t[0] for b in sol["breaks"]])
    n, p = X.shape
    dof = max(1, n - p)
    sigma2 = sol["rss"] / dof
    try:
        cov = sigma2 * np.linalg.inv(X.T @ X)
    except np.linalg.LinAlgError:
        cov = sigma2 * np.linalg.pinv(X.T @ X)
    out = []
    for k in range(2, p):
        coef = float(sol["beta"][k])
        se = float(math.sqrt(max(cov[k, k], 0.0)))
        ratio = abs(coef) / se if se > 0 else float("inf")
        out.append({"d_slope_db_per_s": coef, "se_db_per_s": se, "t_stat": ratio})
    return out


# ---------------------------------------------------------------------------
# piecewise models
# ---------------------------------------------------------------------------
def _first_below(edc, level, start=0):
    idx = np.nonzero(edc[start:] <= level)[0]
    return None if idx.size == 0 else start + int(idx[0])


def _first_valid(edc, level, noise_db, margin, start=0):
    """First index >= start where EDC <= level AND >= margin above the noise floor."""
    idx = np.nonzero(edc[start:] <= level)[0]
    for i in idx:
        j = start + int(i)
        if noise_db is None or (edc[j] - noise_db[j]) >= margin:
            return j
    return None


def _valid_fixed(t, y):
    if len(t) < 10:
        return None
    X = np.vstack([np.ones_like(t), t - t[0]]).T
    sol, *_ = np.linalg.lstsq(X, y, rcond=None)
    res = y - X @ sol
    sst = float(np.sum((y - y.mean()) ** 2))
    slope = float(sol[1])
    return {"slope_db_per_s": slope,
            "t20_s": t20_from_slope(slope),
            "r2": (1.0 - float(np.sum(res * res)) / sst) if sst > 0 else None,
            "level0_db": float(y[0]), "level1_db": float(y[-1]),
            "t0_s": float(t[0]), "t1_s": float(t[-1]), "n": int(len(t))}


def _fixed_split_fits(edc, fs, i5, i25):
    """Canonical fixed-split descriptors: -5..-15 dB and -15..-25 dB."""
    out = {"early": None, "late": None, "ranges_db": [[FIT_LO_DB, -15.0], [-15.0, FIT_HI_DB]]}
    i15 = _first_below(edc, -15.0, i5)
    if i15 is not None and i15 > i5 + 8:
        out["early"] = _valid_fixed(np.arange(i5, i15 + 1) / fs, edc[i5:i15 + 1])
    if i15 is not None and i25 > i15 + 8:
        out["late"] = _valid_fixed(np.arange(i15, i25 + 1) / fs, edc[i15:i25 + 1])
    return out


def curvature_knee(t, y, initial_lo_ms=8.0, initial_hi_ms=30.0, drop_frac=0.5,
                   hold_ms=25.0, min_level_db=-8.0):
    """Descriptive knee: first sustained slowdown of the EDC decay slope.

    Independent of the piecewise model.  Estimates the initial decay slope over
    [8, 30] ms after the window start, then finds the first time where the decay
    slope drops below 50% of it, holds for >= 25 ms and the level is below -8 dB.
    Also reports the max-|curvature| peak for diagnostics.

    Used when a piecewise segment pair is not identifiable inside the available
    range (e.g. the 200 Hz knee sits at/just beyond the -25 dB T20 end point).
    Returns t_s=None when there is no sustained slowdown (single-slope band).
    """
    from scipy.signal import savgol_filter
    n = len(y)
    if n < 41:
        return None
    dt = float(t[1] - t[0])
    win = min(31, (n // 2) * 2 - 1)
    if win < 11:
        return None
    d1 = savgol_filter(y, win, 3, deriv=1, delta=dt)
    d2 = savgol_filter(y, win, 3, deriv=2, delta=dt)
    i0 = max(1, int(round(initial_lo_ms * 1e-3 / dt)))
    i1 = min(max(i0 + 3, int(round(initial_hi_ms * 1e-3 / dt))), n - 5)
    s0 = float(np.median(np.abs(d1[i0:i1])))
    if not np.isfinite(s0) or s0 <= 0:
        return None
    thr = drop_frac * s0
    hold = max(2, int(round(hold_ms * 1e-3 / dt)))
    knee_i = None
    for i in range(i1, n - hold):
        if np.all(np.abs(d1[i:i + hold]) < thr) and y[i] <= min_level_db:
            knee_i = i
            break
    edge = max(1, int(0.05 * n))
    cur_i = edge + int(np.argmax(np.abs(d2[edge:n - edge])))
    out = {"curvature_peak": {"t_s": float(t[cur_i] - t[0]),
                              "level_db": float(y[cur_i]),
                              "curvature_db_per_s2": float(d2[cur_i])},
           "initial_slope_db_per_s": -s0,
           "detected": knee_i is not None,
           "t_s": None, "level_db": None, "slope_before_db_per_s": None,
           "slope_after_db_per_s": None, "slope_ratio": None}
    if knee_i is None:
        return out
    w = max(3, int(round(0.03 / dt)))
    seg = np.abs(d1[knee_i:min(n, knee_i + w)])
    s_after = float(np.median(seg)) if seg.size else None
    out.update({"t_s": float(t[knee_i] - t[0]), "t_abs_s": float(t[knee_i]),
                "level_db": float(y[knee_i]),
                "slope_before_db_per_s": -s0,
                "slope_after_db_per_s": (-s_after if s_after is not None else None),
                "slope_ratio": (abs(s_after / s0) if s_after is not None else None)})
    return out


def _curvature_knee_out(t, y):
    ck = curvature_knee(t, y)
    if ck is not None and ck.get("t_s") is not None:
        ck["t_abs_s"] = float(t[0] + ck["t_s"])
    return ck


def fit_piecewise(edc_db, fs, fit_lo=FIT_LO_DB, fit_hi=FIT_HI_DB,
                  noise_db=None, margin_db=FIT_NOISE_MARGIN_DB,
                  extent_hi_db=EXTENT_HI_DB, max_samples=4000):
    """Fit 1/2/3 continuous piecewise lines to the EDC.

    Two windows are used:
      * the frozen T20 window (-5..-25 dB) -> control_single_slope and the canonical
        fixed-split descriptors (-5..-15, -15..-25); this control reproduces the
        frozen analysis exactly;
      * the extended knee-detection window (-5 dB .. extent_hi_db, but never below
        the -10 dB noise-margin rule) -> the 1/2/3-segment model selection.  The
        extended window is needed because a knee can sit at (or just beyond) the
        -25 dB end point (200 Hz case) and would be invisible in the T20 window.

    Returns None / {"status": "NO_COVERAGE"} exactly like before when the frozen
    -5/-25 coverage rule fails.
    """
    edc_db = np.asarray(edc_db, dtype=np.float64)
    i5 = _first_below(edc_db, fit_lo)
    if i5 is None:
        return None
    i25 = _first_below(edc_db, fit_hi, i5)
    if i25 is None:
        return {"status": "NO_COVERAGE", "reason": "EDC never reaches %.0f dB" % fit_hi,
                "i_lo": i5, "i_hi": None}
    if i25 <= i5 + 8:
        return {"status": "NO_COVERAGE", "reason": "too few samples between -5 and -25 dB",
                "i_lo": i5, "i_hi": i25}
    noise_margin = None
    if noise_db is not None:
        noise_margin = float(edc_db[i25] - noise_db[i25])
        if noise_margin < margin_db:
            return {"status": "NO_COVERAGE",
                    "reason": "-25 dB point sits only %.1f dB above the noise floor "
                              "(needs >= %.0f dB)" % (noise_margin, margin_db),
                    "i_lo": i5, "i_hi": i25, "noise_margin_db": noise_margin}

    # ---- frozen-window control (-5..-25 dB, exactly the T20 fit) --------------
    t_froz = np.arange(i5, i25 + 1, dtype=np.float64) / float(fs)
    y_froz = edc_db[i5:i25 + 1]
    single = _valid_fixed(t_froz, y_froz)
    fixed = _fixed_split_fits(edc_db, float(fs), i5, i25)

    # ---- extended knee-detection window --------------------------------------
    i_ext = _first_valid(edc_db, extent_hi_db, noise_db, margin_db, i5)
    if i_ext is None:
        i_ext = i25
    i_ext = max(i_ext, i25)
    t = np.arange(i5, i_ext + 1, dtype=np.float64) / float(fs)
    y = edc_db[i5:i_ext + 1]
    # the frozen control and the fixed splits above are computed at full resolution
    # (so they reproduce the frozen analysis exactly); the expensive automatic model
    # search runs on a bounded number of samples (the EDC is smooth).
    step = max(1, int(np.ceil(y.size / float(max_samples))))
    if step > 1:
        t = t[::step]
        y = y[::step]
    window_s = float(t[-1] - t[0])
    u = t - t[0]
    X1 = np.vstack([np.ones_like(u), u]).T
    b1, *_ = np.linalg.lstsq(X1, y, rcond=None)
    res1 = y - X1 @ b1
    rss1 = max(float(np.sum(res1 * res1)), 1e-300)
    sst = float(np.sum((y - y.mean()) ** 2))
    r2_1 = 1.0 - rss1 / sst if sst > 0 else None

    models = {"1": {"model": "SINGLE",
                    "params": {"slope_db_per_s": float(b1[1]),
                               "level_at_window_start_db": float(b1[0])},
                    "breaks_s": [],
                    "segments": [{"r2": r2_1, "n": int(y.size),
                                  "t0": float(t[0]), "t1": float(t[-1]),
                                  "level0": float(y[0]), "level1": float(y[-1]),
                                  "span_db": float(y[0] - y[-1])}],
                    "slopes_db_per_s": [float(b1[1])],
                    "t20_seg_s": [t20_from_slope(float(b1[1]))],
                    "rss": rss1, "r2": r2_1,
                    "bic_eff": _bic(rss1, _n_eff(res1), 2),
                    "aic_eff": _aic(rss1, _n_eff(res1), 2),
                    "n_eff": _n_eff(res1)}}

    # ---- 2 segments
    b2 = _best_two(t, y)
    if b2 is not None:
        st = _model_stats(b2["sol"], t, y)
        st["model"] = "TWO"
        st["params"] = {"slope1_db_per_s": st["slopes_db_per_s"][0],
                        "slope2_db_per_s": st["slopes_db_per_s"][1],
                        "level_at_window_start_db": float(b2["sol"]["beta"][0]),
                        "knee_time_s": st["breaks_s"][0],
                        "knee_level_db": float(np.interp(b2["sol"]["breaks"][0], t, y))}
        st["knee"] = {"t_s": st["breaks_s"][0],
                      "t_abs_s": float(b2["sol"]["breaks"][0]),
                      "level_db": st["params"]["knee_level_db"],
                      "slope_ratio": abs(st["slopes_db_per_s"][1] / st["slopes_db_per_s"][0])
                      if st["slopes_db_per_s"][0] else None,
                      "slope_change": _slope_change_stats(b2["sol"], t, y)}
        st["knee"].update(_knee_ci(t, y, b2["sol"]["breaks"], b2["i"],
                                   b2["sol"]["rss"], max(1, len(t) - 4)))
        st["segment_valid_reasons"] = _valid_segments(st["segments"], window_s)
        models["2"] = st
    # ---- 3 segments
    b3 = _best_three(t, y)
    if b3 is not None:
        st = _model_stats(b3["sol"], t, y)
        st["model"] = "THREE"
        st["params"] = {"slope1_db_per_s": st["slopes_db_per_s"][0],
                        "slope2_db_per_s": st["slopes_db_per_s"][1],
                        "slope3_db_per_s": st["slopes_db_per_s"][2],
                        "level_at_window_start_db": float(b3["sol"]["beta"][0]),
                        "knee1_time_s": st["breaks_s"][0],
                        "knee2_time_s": st["breaks_s"][1],
                        "knee1_level_db": float(np.interp(b3["sol"]["breaks"][0], t, y)),
                        "knee2_level_db": float(np.interp(b3["sol"]["breaks"][1], t, y))}
        st["knee"] = {"knees": [{"t_s": st["breaks_s"][0],
                                 "t_abs_s": float(b3["sol"]["breaks"][0]),
                                 "level_db": st["params"]["knee1_level_db"]},
                                {"t_s": st["breaks_s"][1],
                                 "t_abs_s": float(b3["sol"]["breaks"][1]),
                                 "level_db": st["params"]["knee2_level_db"]}],
                      "slope_ratio_12": abs(st["slopes_db_per_s"][1] / st["slopes_db_per_s"][0]),
                      "slope_ratio_23": abs(st["slopes_db_per_s"][2] / st["slopes_db_per_s"][1])
                      if st["slopes_db_per_s"][1] else None,
                      "slope_change": _slope_change_stats(b3["sol"], t, y)}
        st["segment_valid_reasons"] = _valid_segments(st["segments"], window_s)
        models["3"] = st

    # ---------------- selection -------------------------------------------
    def _ident_ok(st, nseg):
        r = []
        if st.get("segment_valid_reasons"):
            r += st["segment_valid_reasons"]
        if nseg == 2:
            k = st["knee"]
            ratio = k["slope_ratio"]
            if ratio is None or max(ratio, 1.0 / ratio) < SLOPE_RATIO_MIN:
                r.append("slope ratio %.2f < %.1f" % (ratio or 0.0, SLOPE_RATIO_MIN))
            if not k["slope_change"] or k["slope_change"][0]["t_stat"] < SLOPE_DIFF_T_MIN:
                r.append("slope-change t=%.1f < %.1f" % (
                    k["slope_change"][0]["t_stat"] if k["slope_change"] else 0.0, SLOPE_DIFF_T_MIN))
            ci_max = max(KNEE_CI_MIN_MS, 1000.0 * KNEE_CI_FRAC * window_s)
            if k["ci_width_ms"] > ci_max:
                r.append("knee CI %.0f ms > %.0f ms" % (k["ci_width_ms"], ci_max))
            if k["t_s"] < 0.5 * MIN_SEG_MS * 1e-3 or k["t_s"] > window_s - 0.5 * MIN_SEG_MS * 1e-3:
                r.append("knee at fit-window edge")
        elif nseg == 3:
            k = st["knee"]
            for j, ratio in enumerate([k["slope_ratio_12"], k["slope_ratio_23"]]):
                if ratio is None or max(ratio, 1.0 / ratio) < SLOPE_RATIO_MIN:
                    r.append("3-seg slope ratio %d (%.2f) < %.1f" % (j + 1, ratio or 0.0, SLOPE_RATIO_MIN))
            if len(k["slope_change"]) < 2 or any(c["t_stat"] < SLOPE_DIFF_T_MIN
                                                 for c in k["slope_change"]):
                r.append("3-seg slope-change t < %.1f" % SLOPE_DIFF_T_MIN)
        return r

    best = None
    for nseg in (1, 2, 3):
        st = models[str(nseg)]
        if nseg > 1:
            rx = models[str(nseg - 1)]
            d_bic = rx["bic_eff"] - st["bic_eff"]
            d_r2 = (st["r2"] or 0.0) - (rx["r2"] or 0.0)
            st["delta"] = {"d_bic": d_bic, "d_r2": d_r2}
            evidence = (d_bic >= BIC_MARGIN) or (d_r2 >= DR2_MIN)
            st["evidence_ok"] = bool(evidence)
            st["ident_reasons"] = _ident_ok(st, nseg)
            if evidence and not st["ident_reasons"]:
                best = nseg
        else:
            st["ident_reasons"] = []
    if best is None or best == 1:
        selected = {"n_seg": 1, "status": "SINGLE_SLOPE", "reasons": [],
                    "note": "no identifiable knee inside the knee-detection window"}
    else:
        selected = {"n_seg": best, "status": "OK", "reasons": [],
                    "note": "knee(s) identified by BIC/dR2 + identifiability checks"}
        st = models[str(best)]
        for k, s in enumerate(st["segments"]):
            if s.get("r2") is not None and s["r2"] < 0.95:
                selected["reasons"].append("segment %d R2 %.3f < 0.95" % (k, s["r2"]))
        if selected["reasons"]:
            selected["status"] = "AMBIGUOUS"
            selected["note"] = "selected segments have poor per-segment R2"
    if best is not None and best > 1:
        st = models[str(best)]
        if st.get("segment_valid_reasons") or st.get("ident_reasons"):
            selected["status"] = "AMBIGUOUS"
            selected["reasons"] += (st.get("segment_valid_reasons") or []) + (st.get("ident_reasons") or [])
            selected["note"] = "a knee is suggested but not fully identifiable"

    biexp_init = None
    if "2" in models:
        st2 = models["2"]
        try:
            t20e, t20l = st2["t20_seg_s"]
            if t20e and t20l:
                biexp_init = {"tau1": max(0.002, t20e / T20_PER_TAU),
                              "tau2": max(0.01, t20l / T20_PER_TAU),
                              "a1": min(0.95, max(0.05, 1.0 - 10.0 ** (
                                  (st2["knee"]["level_db"] - y[0]) / 10.0)))}
        except Exception:
            biexp_init = None
    biexp = fit_biexp(t, y, init=biexp_init)

    out = {
        "status": "OK",
        "fit_range": {"i_lo": i5, "i_hi": i_ext, "t_lo_s": float(t[0]), "t_hi_s": float(t[-1]),
                      "level_lo_db": float(y[0]), "level_hi_db": float(y[-1]),
                      "window_s": window_s, "n": int(y.size),
                      "frozen_window_level_hi_db": float(y_froz[-1]),
                      "extent_hi_db": float(extent_hi_db),
                      "note": "model selection uses the extended window; the frozen "
                              "-5..-25 dB window is kept for the single-slope control"},
        "noise_margin_db": noise_margin,
        "control_single_slope": single,
        "fixed_split": fixed,
        "curvature_knee": _curvature_knee_out(t, y),
        "models": models,
        "selected": selected,
        "biexp": biexp,
        "constants": {"FIT_LO_DB": fit_lo, "FIT_HI_DB": fit_hi,
                      "EXTENT_HI_DB": EXTENT_HI_DB,
                      "MIN_SEG_SPAN_DB": MIN_SEG_SPAN_DB, "MIN_SEG_MS": MIN_SEG_MS,
                      "BIC_MARGIN": BIC_MARGIN, "DR2_MIN": DR2_MIN,
                      "SLOPE_RATIO_MIN": SLOPE_RATIO_MIN, "SLOPE_DIFF_T_MIN": SLOPE_DIFF_T_MIN,
                      "KNEE_CI_MIN_MS": KNEE_CI_MIN_MS, "KNEE_CI_FRAC": KNEE_CI_FRAC},
    }
    if selected["n_seg"] > 1:
        st = models[str(selected["n_seg"])]
        out["segments"] = st["segments"]
        out["t20_seg_s"] = st["t20_seg_s"]
        out["knee"] = st["knee"]
        out["r2"] = st["r2"]
    else:
        out["segments"] = models["1"]["segments"]
        out["t20_seg_s"] = models["1"]["t20_seg_s"]
        out["r2"] = models["1"]["r2"]
    return out


# ---------------------------------------------------------------------------
# double exponential
# ---------------------------------------------------------------------------
def fit_biexp(t, y_db, init=None):
    """E(t) = C + 10*log10( A1*exp(-t/tau1) + A2*exp(-t/tau2) ), fitted in dB.

    C is a nuisance offset (the fit window starts at -5 dB, not at the direct-sound
    peak), tau1 < tau2 are decay time constants, a1/a2 their energy weights at the
    window start.  Multi-start bounded least squares; init optionally supplies a
    guess derived from a piecewise fit.

    Identifiability (ALL required, otherwise UNIDENTIFIABLE -- still reported, but
    never used for a gate):
      * tau2/tau1 >= BIEXP_TAU_RATIO_MIN
      * both weights >= BIEXP_WEIGHT_MIN
      * relative standard error of both taus <= BIEXP_RSE_MAX
    """
    from scipy.optimize import least_squares
    t = np.asarray(t, dtype=np.float64) - t[0]
    y = np.asarray(y_db, dtype=np.float64)

    def model(p):
        tau1, tau2, a1, b0 = p
        tau1, tau2 = abs(tau1), abs(tau2)
        a1 = min(max(a1, 1e-3), 1.0 - 1e-3)
        e = a1 * np.exp(-t / tau1) + (1.0 - a1) * np.exp(-t / tau2)
        return b0 + 10.0 * np.log10(np.maximum(e, 1e-12))

    def resid(p):
        return model(p) - y

    starts = []
    if init:
        starts.append([float(init["tau1"]), float(init["tau2"]), float(init["a1"]), float(y[0])])
    for tau1 in (0.006, 0.012, 0.02, 0.035):
        for tau2 in (0.08, 0.15, 0.3, 0.6):
            for a1 in (0.3, 0.5, 0.7, 0.85):
                starts.append([tau1, tau2, a1, float(y[0])])
    best = None
    for s in starts:
        try:
            r = least_squares(resid, s,
                              bounds=([0.001, 0.004, 0.01, -40.0], [1.0, 8.0, 0.99, 5.0]),
                              max_nfev=600)
        except Exception:
            continue
        if best is None or r.cost < best.cost:
            best = r
    if best is None:
        return {"status": "FIT_FAILED"}
    p = [float(v) for v in best.x]
    tau1, tau2, a1, b0 = abs(p[0]), abs(p[1]), p[2], p[3]
    if tau1 > tau2:
        tau1, tau2, a1 = tau2, tau1, 1.0 - a1
    a2 = 1.0 - a1
    rss = float(np.sum(best.fun ** 2))
    sst = float(np.sum((y - y.mean()) ** 2))
    r2 = 1.0 - rss / sst if sst > 0 else None
    J = best.jac
    dof = max(1, y.size - 4)
    try:
        cov = (rss / dof) * np.linalg.pinv(J.T @ J)
        se = np.sqrt(np.maximum(np.diag(cov), 0.0))
    except Exception:
        se = np.full(4, np.nan)
    knee_t = None
    if abs(1.0 / tau1 - 1.0 / tau2) > 1e-9 and a2 > 0 and a1 > 0:
        knee_t = math.log(a1 / a2) / (1.0 / tau1 - 1.0 / tau2)
        if knee_t < 0:
            knee_t = None
    reasons = []
    ratio = tau2 / tau1 if tau1 > 0 else float("inf")
    if ratio < BIEXP_TAU_RATIO_MIN:
        reasons.append("tau2/tau1 %.2f < %.1f" % (ratio, BIEXP_TAU_RATIO_MIN))
    if min(a1, a2) < BIEXP_WEIGHT_MIN:
        reasons.append("one exponential weight < %.2f" % BIEXP_WEIGHT_MIN)
    if tau1 > 0 and se[0] / tau1 > BIEXP_RSE_MAX:
        reasons.append("tau1 RSE %.0f%% > %.0f%%" % (100 * se[0] / tau1, 100 * BIEXP_RSE_MAX))
    if tau2 > 0 and se[1] / tau2 > BIEXP_RSE_MAX:
        reasons.append("tau2 RSE %.0f%% > %.0f%%" % (100 * se[1] / tau2, 100 * BIEXP_RSE_MAX))
    return {
        "status": "UNIDENTIFIABLE" if reasons else "OK",
        "reasons": reasons,
        "tau1_s": float(tau1), "tau2_s": float(tau2),
        "a1": float(a1), "a2": float(a2),
        "offset_db": float(b0),
        "tau_ratio": float(ratio),
        "knee_time_s": knee_t,
        "rss": rss, "r2": r2,
        "t20_equiv_s": {"early": float(T20_PER_TAU * tau1), "late": float(T20_PER_TAU * tau2)},
        "se": {"tau1_s": float(se[0]), "tau2_s": float(se[1]),
               "a1": float(se[2]), "offset_db": float(se[3])},
        "rse": {"tau1": float(se[0] / tau1) if tau1 > 0 else None,
                "tau2": float(se[1] / tau2) if tau2 > 0 else None},
        "note": "tau-equivalent T20 = 13.8155*tau is derived, only valid for a single "
                "exponential; it is a diagnostic, not a T20.",
    }
