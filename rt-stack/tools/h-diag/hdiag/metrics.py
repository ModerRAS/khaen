
"""Complex transfer-matrix diagnostics for h-diag.

All definitions are documented in README.md / REPORT.md and in the docstrings below.
Nothing in this module modifies the solver; it only analyses measured H(f).

Notation
--------
P = number of positions in the role subset (design or holdout)
N = number of actuators (5: FL FR SL SR SUB1)
H[p, i] in C^P x C^N at one frequency; position weights w_p (design weights only).
The weighted plant is Hw = diag(sqrt(w)) H, so the weighted SVD is
Hw = U diag(sigma) V^H and sigma_1 >= ... >= sigma_min >= 0.

Effective rank
--------------
rank_eff = #{ i : sigma_i > max(1e-3 * sigma_1, sigma_1 * 10^(-SNR/20)) }
  * 1e-3 relative threshold mirrors modal-design/src/solve.rs: RANK_ZERO_REL = 1e-6
    on the Gram matrix eigen-values (sigma^2): sqrt(1e-6) = 1e-3 (plan note
    "sigma < 1e-3 sigma_max").
  * the measurement-noise threshold uses the measured amplitude SNR: a singular
    direction whose amplitude is below the measurement error is not identifiable.
  An effective rank below N means rank deficiency (uncontrollable directions).

Condition number
----------------
kappa = sigma_1 / sigma_rank_eff (only when rank_eff >= 2); kappa_full = sigma_1/sigma_min
is reported as a diagnostic even when rank-deficient but never used as the
controllability condition number.

Complex coherence
-----------------
Spatial coherence between actuators i,j at frequency f (positions form the ensemble):

    c_ij(f) = | sum_p w_p conj(H[p,i,f]) H[p,j,f] |
              / sqrt( (sum_p w_p |H[p,i,f]|^2) (sum_p w_p |H[p,j,f]|^2) )

    angle_ij(f) = arg( sum_p w_p conj(H[p,i,f]) H[p,j,f] )

c in [0,1]: 1 = the two actuators produce the same spatial signature (fully
redundant for this position set), 0 = orthogonal (independent spatial control).
The band coherence averages the complex cross-spectrum over the band and positions
with w_f = 1.0 (mimo-objective-v1 v1.1 default):

    C_ij(B) = sum_{f in B} sum_p w_p conj(H[p,i,f]) H[p,j,f]
              / sqrt( (sum_{f in B} sum_p w_p |H[p,i,f]|^2)
                      (sum_{f in B} sum_p w_p |H[p,j,f]|^2) )

Controllability / achievable cancellation depth
-----------------------------------------------
Two independent bounds are reported for every actuator combination and band; the
bottleneck is their minimum:

  * nullspace attenuation (random spatial target): with rank r over P positions,
    an arbitrary unit correction vector has r/P of its energy in the controllable
    subspace; the average random-target rejection is
        D_null = 10 log10( P / (P - r) )   dB   (P > r; else "exact")
  * measurement-limited inversion depth: with relative amplitude measurement error
    eps = 10^(-SNR/20) and condition number kappa, inverting the plant amplifies the
    relative error by kappa, so the deepest reliable cancellation is
        D_meas = 20 log10( 1 / (eps * kappa) )  dB, floored at 0 dB.
  Neither bound is a promise; they are the two failure mechanisms the diagnostic
  must expose.  sigma_min_rel_db = 20 log10(sigma_min/sigma_1).

Subspace consistency (design vs holdout)
----------------------------------------
The left singular vectors live in position space and the design and holdout point
sets differ, so they are NOT comparable.  The right singular vectors V (actuator
space, C^N) are comparable: they say which actuator combinations are controllable.
With V_d, V_h the first k right singular vectors (k = min(rank_d, rank_h)):
    C = V_d^H V_h;  singular values of C are cos(theta_j) -> principal angles.
    max_angle_deg, mean_angle_deg, sigma_min_overlap = min singular value of C,
    subspace_distance = sqrt(1 - sigma_min_overlap^2).
Identical actuator controllability subspaces -> 0 deg; orthogonal -> 90 deg.
Overfitting risk flag: max_angle_deg > angle_warn_deg (default 30 deg), or
rank_design != rank_holdout.
"""
from __future__ import annotations

import math

import numpy as np

BAND_CENTERS_HZ = [40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0]
BAND_W_F = 1.0                     # mimo-objective-v1 v1.1: w_f default 1.0
ALPHA_MIN = 0.5
ALPHA_MAX = 2.0
EPS_ENERGY_MIGRATION = 0.05
DELTA_MIMO_VS_PEQ_DB = 1.0
HOLDOUT_C_MIN = 0.50
HOLDOUT_SEVERE_REGRESSION_DB = 3.0
RANK_REL_THRESHOLD = 1.0e-3        # solve.rs RANK_ZERO_REL=1e-6 on sigma^2
HIGH_CONDITION_WARN = 100.0        # solve.rs HIGH_CONDITION_WARN
SUBSPACE_ANGLE_WARN_DEG = 30.0
MODE_RANDOM_SEED = 20261121
LAMBDA_GRID = [0.0, 1.0e-6, 1.0e-5, 1.0e-4, 1.0e-3, 1.0e-2, 1.0e-1, 1.0]
DEFAULT_EFFORT_CAPS = [0.01, 0.1, 1.0]
MAX_DEPTH_DB = 60.0


def band_range(center_hz):
    half = 2.0 ** (1.0 / 6.0)
    return (float(center_hz) / half, float(center_hz) * half)


def band_mask(freq_hz, center_hz):
    lo, hi = band_range(center_hz)
    freq_hz = np.asarray(freq_hz, dtype=np.float64)
    return (freq_hz >= lo) & (freq_hz < hi)


def band_definition():
    return [{"center_hz": c, "range_hz": list(band_range(c))} for c in BAND_CENTERS_HZ]


def _db(value, floor=1.0e-300):
    return 20.0 * math.log10(max(float(value), floor))


def weighted_plant(H, weights):
    H = np.asarray(H, dtype=np.complex128)
    w = np.asarray(weights, dtype=np.float64)
    return np.sqrt(w)[:, None] * H


def svd(H, weights=None):
    if weights is None:
        weights = np.ones(H.shape[0], dtype=np.float64)
    Hw = weighted_plant(H, weights)
    U, S, Vh = np.linalg.svd(Hw, full_matrices=False)
    return U, S, Vh


def snr_amplitude_error(snr_db):
    if snr_db is None or not math.isfinite(float(snr_db)):
        return None
    return 10.0 ** (-float(snr_db) / 20.0)


def effective_rank(S, snr_db=None):
    S = np.asarray(S, dtype=np.float64)
    if S.size == 0 or not (S[0] > 0.0):
        return {"rank": 0, "threshold_abs": 0.0, "threshold_rel": RANK_REL_THRESHOLD,
                "threshold_noise_abs": None, "threshold_used_abs": 0.0,
                "relative_singular_values": [], "basis": "empty"}
    rel = RANK_REL_THRESHOLD * S[0]
    amp = snr_amplitude_error(snr_db)
    noise = None if amp is None else amp * S[0]
    threshold = rel if noise is None else max(rel, noise)
    rank = int(np.count_nonzero(S > threshold))
    return {
        "rank": rank,
        "threshold_abs": float(threshold),
        "threshold_rel": RANK_REL_THRESHOLD,
        "threshold_noise_abs": None if noise is None else float(noise),
        "threshold_used_abs": float(threshold),
        "relative_singular_values": [float(s / S[0]) for s in S],
        "basis": ("sigma > max(1e-3*sigma_1, sigma_1*10^(-SNR/20)); 1e-3 mirrors "
                  "solve.rs RANK_ZERO_REL=1e-6 on the Gram matrix"),
    }


def condition(S, rank):
    S = np.asarray(S, dtype=np.float64)
    if S.size == 0 or not (S[0] > 0.0):
        return {"kappa": None, "kappa_full": None, "sigma_min": 0.0,
                "sigma_min_rel_db": None, "rank_full": 0}
    sigma_min = float(S[-1])
    kappa_full = float(S[0] / S[-1]) if sigma_min > 0.0 else None
    if rank is None or rank < 2:
        kappa = None
    else:
        kappa = float(S[0] / S[rank - 1])
    return {
        "kappa": kappa,
        "kappa_full": kappa_full,
        "sigma_min": sigma_min,
        "sigma_min_rel_db": _db(sigma_min / S[0]) if sigma_min > 0.0 else -300.0,
        "rank_full": int(S.size),
        "sigma": [float(s) for s in S],
        "sigma_rel_db": [_db(s / S[0]) if s > 0 else -300.0 for s in S],
    }


def coherence(H, weights=None):
    H = np.asarray(H, dtype=np.complex128)
    if weights is None:
        weights = np.ones(H.shape[0], dtype=np.float64)
    Hw = np.sqrt(np.asarray(weights, dtype=np.float64))[:, None] * H
    gram = Hw.conj().T @ Hw
    diag = np.sqrt(np.maximum(np.real(np.diag(gram)), 0.0))
    denom = np.outer(diag, diag)
    mag = np.divide(np.abs(gram), denom, out=np.zeros_like(np.abs(gram)),
                    where=denom > 0.0)
    phase = np.angle(gram)
    return mag, phase, gram


def band_coherence(H, freqs, weights, center_hz, w_f=BAND_W_F):
    """Frequency+position averaged complex coherence for one band."""
    H = np.asarray(H, dtype=np.complex128)
    mask = band_mask(freqs, center_hz)
    if not np.any(mask):
        return None
    Hw = np.sqrt(np.asarray(weights, dtype=np.float64))[:, None, None] * H[:, :, mask]
    Hw = Hw * float(w_f)
    gram = np.einsum("pif,pjf->ij", Hw.conj(), Hw)
    diag = np.sqrt(np.maximum(np.real(np.diag(gram)), 0.0))
    denom = np.outer(diag, diag)
    mag = np.divide(np.abs(gram), denom, out=np.zeros_like(np.abs(gram)),
                    where=denom > 0.0)
    return {"magnitude": mag, "phase_deg": np.degrees(np.angle(gram))}


def subspace_consistency(H_design, w_design, H_holdout, rank_design=None,
                         rank_holdout=None, angle_warn_deg=SUBSPACE_ANGLE_WARN_DEG):
    """Principal angles between design and holdout *actuator-space* subspaces."""
    U_d, S_d, Vh_d = svd(H_design, w_design)
    U_h, S_h, Vh_h = svd(H_holdout, None)  # holdout never weighted for tuning
    if rank_design is None:
        rank_design = int(np.count_nonzero(S_d > RANK_REL_THRESHOLD * S_d[0])) if S_d.size and S_d[0] > 0 else 0
    if rank_holdout is None:
        rank_holdout = int(np.count_nonzero(S_h > RANK_REL_THRESHOLD * S_h[0])) if S_h.size and S_h[0] > 0 else 0
    k = min(rank_design, rank_holdout)
    out = {
        "rank_design": int(rank_design),
        "rank_holdout": int(rank_holdout),
        "k": int(k),
        "max_angle_deg": None,
        "mean_angle_deg": None,
        "sigma_min_overlap": None,
        "subspace_distance": None,
        "angles_deg": [],
        "risk": False,
        "risk_reasons": [],
    }
    if int(rank_design) != int(rank_holdout):
        out["risk_reasons"].append(
            "rank mismatch design=%d holdout=%d" % (rank_design, rank_holdout))
    if k == 0:
        out["risk"] = bool(out["risk_reasons"])
        return out
    Vd = Vh_d.conj().T[:, :k]   # N x k
    Vh = Vh_h.conj().T[:, :k]
    C = Vd.conj().T @ Vh
    sv = np.linalg.svd(C, compute_uv=False)
    cos = np.clip(np.real(sv), 0.0, 1.0)
    angles = np.degrees(np.arccos(cos))
    out["angles_deg"] = [float(a) for a in angles]
    out["max_angle_deg"] = float(np.max(angles))
    out["mean_angle_deg"] = float(np.mean(angles))
    out["sigma_min_overlap"] = float(np.min(cos))
    out["subspace_distance"] = float(math.sqrt(max(0.0, 1.0 - float(np.min(cos)) ** 2)))
    if out["max_angle_deg"] > angle_warn_deg:
        out["risk_reasons"].append(
            "max principal angle %.2f deg > %.1f deg" %
            (out["max_angle_deg"], angle_warn_deg))
    out["risk"] = bool(out["risk_reasons"])
    return out


def controllability(S, rank, n_positions, snr_median_db):
    S = np.asarray(S, dtype=np.float64)
    out = {"effective_rank": int(rank), "n_positions": int(n_positions),
           "nullspace_attenuation_db": None, "measurement_limited_depth_db": None,
           "weakest_channel_depth_db": None, "bottleneck": None,
           "exact_nulling": False, "basis": {}}
    if rank is None or rank <= 0 or S.size == 0 or not (S[0] > 0.0):
        out["bottleneck"] = "rank_zero"
        out["nullspace_attenuation_db"] = -300.0
        return out
    if n_positions <= rank:
        out["exact_nulling"] = True
        null_db = MAX_DEPTH_DB
    else:
        null_db = 10.0 * math.log10(float(n_positions) / float(n_positions - rank))
    rank_used = int(min(max(rank, 1), S.size))
    kappa = float(S[0] / S[rank_used - 1]) if S[rank_used - 1] > 0.0 else None
    amp = snr_amplitude_error(snr_median_db)
    weak_db = _db(1.0 / kappa) if kappa > 0 else None
    if amp is None or kappa is None:
        meas_db = None
    else:
        meas_db = 20.0 * math.log10(1.0 / max(amp * kappa, 1.0e-300))
        meas_db = float(min(MAX_DEPTH_DB, max(0.0, meas_db)))
    out["nullspace_attenuation_db"] = float(min(MAX_DEPTH_DB, null_db))
    out["measurement_limited_depth_db"] = meas_db
    out["weakest_channel_depth_db"] = weak_db
    candidates = [("nullspace", out["nullspace_attenuation_db"])]
    if meas_db is not None:
        candidates.append(("measurement", meas_db))
    winner = min(candidates, key=lambda item: item[1])
    out["bottleneck"] = winner[0]
    out["bottleneck_depth_db"] = float(winner[1])
    out["basis"] = {
        "nullspace": "10log10(P/(P-r)) for a random unit spatial target; P<=r -> exact",
        "measurement": "20log10(1/(eps*kappa)), eps=10^(-SNR/20) amplitude error; "
                       "kappa=sigma_1/sigma_r; capped at 60 dB, floored at 0 dB",
    }
    return out


# ---------------------------------------------------------------- probes
def probe_targets(U, S, rank, freq_index):
    """Two declared correction targets on the position space.

    random  : deterministic seeded complex Gaussian direction, unit norm
              (representative arbitrary modal correction; seed + freq_index make it
              reproducible and frequency-dependent like a real correction field)
    weakest : left singular vector of the weakest retained singular value
              (the hardest direction inside the controllable subspace)
    """
    P = U.shape[0]
    out = {}
    rng = np.random.default_rng(MODE_RANDOM_SEED + int(freq_index))
    v = rng.standard_normal(P) + 1j * rng.standard_normal(P)
    n = np.linalg.norm(v)
    out["random"] = {"kind": "random_seeded", "seed": MODE_RANDOM_SEED + int(freq_index),
                     "vector": v / n if n > 0 else np.zeros(P, dtype=np.complex128)}
    if rank is not None and rank >= 1:
        out["weakest_mode"] = {"kind": "weakest_retained_mode",
                               "vector": U[:, int(rank) - 1].copy()}
    return out


def ridge_curve(H, weights, target, lambdas=None, effort_caps=None):
    """Ridge inversion trade-off: residual_rel(lambda) vs effort(lambda)."""
    if lambdas is None:
        lambdas = LAMBDA_GRID
    if effort_caps is None:
        effort_caps = DEFAULT_EFFORT_CAPS
    U, S, Vh = svd(H, weights)
    if S.size == 0 or not (S[0] > 0.0):
        return {"lambda": list(lambdas), "residual_rel": [None for _ in lambdas],
                "effort": [None for _ in lambdas], "lcurve_lambda": None,
                "at_lambda0": None, "at_lcurve": None, "effort_caps": [],
                "normalization": "none"}
    Hn = weighted_plant(H, weights) / S[0]           # sigma_1 normalized to 1
    t = np.asarray(target, dtype=np.complex128)
    tnorm = np.linalg.norm(t)
    if tnorm == 0:
        return {"lambda": list(lambdas), "residual_rel": [1.0 for _ in lambdas],
                "effort": [0.0 for _ in lambdas], "lcurve_lambda": None,
                "at_lambda0": None, "at_lcurve": None, "effort_caps": [],
                "normalization": "sigma1=1"}
    t = t / tnorm
    n = Hn.shape[1]
    U2, S2, Vh2 = np.linalg.svd(Hn, full_matrices=False)
    Ut = U2.conj().T @ t
    residual, effort = [], []
    for lam in lambdas:
        lam = float(lam)
        if lam == 0.0:
            g = np.linalg.pinv(Hn) @ t
        else:
            g = Vh2.conj().T @ np.diag(S2 / (S2 ** 2 + lam)) @ Ut
        r = Hn @ g - t
        residual.append(float(np.linalg.norm(r)))
        effort.append(float(np.vdot(g, g).real))
    curve = _lcurve(residual, effort, lambdas)
    caps = []
    for cap in effort_caps:
        cap = float(cap)
        best = None
        for i, lam in enumerate(lambdas):
            if effort[i] is not None and effort[i] <= cap:
                if best is None or residual[i] < residual[best[0]]:
                    best = (i, lam)
        caps.append({
            "effort_cap": cap,
            "reachable": best is not None,
            "min_residual_rel": None if best is None else residual[best[0]],
            "min_residual_db": None if best is None or residual[best[0]] <= 0
            else 20.0 * math.log10(residual[best[0]]),
            "lambda": None if best is None else float(best[1]),
            "effort_at_min": None if best is None else effort[best[0]],
        })
    return {
        "lambda": [float(x) for x in lambdas],
        "residual_rel": residual,
        "residual_db": [None if r <= 0 else 20.0 * math.log10(r) for r in residual],
        "effort": effort,
        "effort_db": [None if e <= 0 else 10.0 * math.log10(e) for e in effort],
        "lcurve_lambda": curve["lambda"],
        "lcurve_index": curve["index"],
        "at_lambda0": {"residual_rel": residual[0], "effort": effort[0]},
        "at_lcurve": None if curve["index"] is None else
        {"lambda": float(lambdas[curve["index"]]),
         "residual_rel": residual[curve["index"]], "effort": effort[curve["index"]]},
        "effort_caps": caps,
        "normalization": "sigma_1 of the weighted plant normalized to 1; effort=sum|g|^2; "
                         "target unit norm; lambda therefore relative to sigma_1^2=1",
    }


def _lcurve(residual, effort, lambdas):
    """Discrete L-curve corner: maximum-curvature point in (log effort, log residual)."""
    points = [(max(e, 1.0e-300), max(r, 1.0e-300)) for r, e in zip(residual, effort)]
    if len(points) < 3:
        return {"index": None, "lambda": None}
    xs = np.log10([p[0] for p in points])
    ys = np.log10([p[1] for p in points])
    curvature = []
    for i in range(1, len(points) - 1):
        dx1, dy1 = xs[i] - xs[i - 1], ys[i] - ys[i - 1]
        dx2, dy2 = xs[i + 1] - xs[i], ys[i + 1] - ys[i]
        n1 = math.hypot(dx1, dy1)
        n2 = math.hypot(dx2, dy2)
        if n1 < 1.0e-12 or n2 < 1.0e-12:
            curvature.append(-1.0)
            continue
        cross = (dx1 * dy2 - dy1 * dx2) / (n1 * n2)
        curvature.append(abs(cross))
    best = int(np.argmax(curvature)) + 1
    return {"index": best, "lambda": float(lambdas[best]),
            "curvature": curvature}


# ---------------------------------------------------------------- bands
def band_summary_design(freq_hz, S_all, rank_all, kappa_all, snr_median_db,
                        band_centers=BAND_CENTERS_HZ, kappa_warn=HIGH_CONDITION_WARN,
                        n_actuators=None):
    """Aggregate per-frequency design SVD results to the frozen 1/3-oct bands."""
    out = []
    for center in band_centers:
        mask = band_mask(freq_hz, center)
        idx = np.nonzero(mask)[0]
        entry = {"center_hz": float(center), "range_hz": list(band_range(center)),
                 "n_freqs": int(idx.size)}
        if idx.size == 0:
            entry.update({"status": "NO_DATA", "sigma_band": None, "rank_min": None,
                          "rank_max": None, "kappa_max": None, "kappa_median": None,
                          "controllability_class": "NO_DATA"})
            out.append(entry)
            continue
        S = np.asarray([S_all[i] for i in idx], dtype=np.float64)
        ranks = [int(rank_all[i]) for i in idx]
        kappas = [kappa_all[i] for i in idx if kappa_all[i] is not None]
        # energy-average singular values over the band: sqrt(mean(sigma^2))
        sigma_band = np.sqrt(np.mean(S ** 2, axis=0))
        entry["sigma_band"] = [float(s) for s in sigma_band]
        entry["sigma_band_rel_db"] = [_db(s / sigma_band[0]) if s > 0 else -300.0
                                      for s in sigma_band]
        entry["rank_min"] = int(np.min(ranks))
        entry["rank_max"] = int(np.max(ranks))
        entry["rank_median"] = float(np.median(ranks))
        entry["kappa_max"] = float(np.max(kappas)) if kappas else None
        entry["kappa_median"] = float(np.median(kappas)) if kappas else None
        n = n_actuators if n_actuators is not None else (S.shape[1] if S.ndim == 2 else None)
        if entry["rank_min"] < (n or 0):
            klass = "RANK_DEFICIENT"
        elif entry["kappa_max"] is not None and entry["kappa_max"] > kappa_warn:
            klass = "ILL_CONDITIONED"
        else:
            klass = "CONTROLLABLE"
        entry["controllability_class"] = klass
        entry["controllability_basis"] = {
            "RANK_DEFICIENT": "effective rank < number of actuators in at least one "
                              "in-band frequency (uncontrollable directions)",
            "ILL_CONDITIONED": "full effective rank but kappa_max > %.1f (solve.rs "
                               "HIGH_CONDITION_WARN); noise-sensitive" % kappa_warn,
            "CONTROLLABLE": "full effective rank and kappa_max <= %.1f" % kappa_warn,
            "NO_DATA": "no analysis frequency inside the band",
        }[klass]
        entry["depth"] = controllability(sigma_band, entry["rank_min"],
                                         S.shape[0], snr_median_db)
        out.append(entry)
    return out


def band_summary_combo(freq_hz, combo_name, actuators, H_design, weights, snr_median_db,
                       band_centers=BAND_CENTERS_HZ):
    """Per-combo band summaries: SVD/rank/kappa/coherence/depth inside one combo."""
    idx = [actuators.index(a) for a in combo_name.split("+")] if combo_name != "ALL5" \
        else list(range(len(actuators)))
    labels = [actuators[i] for i in idx]
    H = H_design[:, idx, :]
    out = []
    for center in band_centers:
        mask = band_mask(freq_hz, center)
        freqs_idx = np.nonzero(mask)[0]
        entry = {"center_hz": float(center), "range_hz": list(band_range(center)),
                 "actuators": labels, "n_freqs": int(freqs_idx.size)}
        if freqs_idx.size == 0:
            entry.update({"status": "NO_DATA"})
            out.append(entry)
            continue
        if not np.all(np.isfinite(H)):
            entry.update({"status": "MISSING",
                          "reason": "some measured actuator is invalid in this band "
                                    "(polarity unknown / parse error / NaN)"})
            out.append(entry)
            continue
        S_list, ranks, kappas = [], [], []
        for fi in freqs_idx:
            U, S, Vh = svd(H[:, :, fi], weights)
            info = effective_rank(S, snr_median_db)
            cond = condition(S, info["rank"])
            S_list.append(S)
            ranks.append(info["rank"])
            kappas.append(cond["kappa"])
        S = np.asarray(S_list, dtype=np.float64)
        sigma_band = np.sqrt(np.mean(S ** 2, axis=0))
        rank_min = int(np.min(ranks))
        kappa_valid = [k for k in kappas if k is not None]
        n_pos = np.count_nonzero(np.isfinite(weights))
        depth = controllability(sigma_band, rank_min, H.shape[0], snr_median_db)
        n_act = len(labels)
        kappa_peak = float(np.max(kappa_valid)) if kappa_valid else None
        if rank_min < n_act:
            klass = "RANK_DEFICIENT"
        elif kappa_peak is not None and kappa_peak > 100.0:
            klass = "ILL_CONDITIONED"
        else:
            klass = "CONTROLLABLE"
        entry.update({
            "status": "OK",
            "controllability_class": klass,
            "controllability_basis": {
                "RANK_DEFICIENT": "effective rank < combo actuator count at some in-band "
                                  "frequency",
                "ILL_CONDITIONED": "full effective rank but kappa_max > 100 "
                                   "(solve.rs HIGH_CONDITION_WARN)",
                "CONTROLLABLE": "full effective rank and kappa_max <= 100",
            }[klass],
            "sigma_band": [float(s) for s in sigma_band],
            "sigma_band_rel_db": [_db(s / sigma_band[0]) if s > 0 else -300.0
                                  for s in sigma_band],
            "rank_min": rank_min,
            "rank_max": int(np.max(ranks)),
            "kappa_max": float(np.max(kappa_valid)) if kappa_valid else None,
            "kappa_median": float(np.median(kappa_valid)) if kappa_valid else None,
            "depth": depth,
        })
        bc = band_coherence(H, freq_hz, weights, center)
        if bc is not None:
            entry["coherence_magnitude"] = bc["magnitude"].tolist()
            entry["coherence_phase_deg"] = bc["phase_deg"].tolist()
        out.append(entry)
    return out
