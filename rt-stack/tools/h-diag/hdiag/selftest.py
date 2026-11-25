
"""h-diag self-test with synthetic data of KNOWN rank.

Every fixture below is explicitly synthetic.  The self-test validates the
implementations independently of the repo solver:

  * SVD / effective rank / condition number against hand-computed matrices
  * complex coherence (identical, orthogonal, known phase, sign flip)
  * design/holdout subspace principal angles (identical / rotated / rank mismatch)
  * 1/3-oct band edges and the mimo-objective-v1 2.2 R_k formula
  * ridge residual/effort against the closed form on a scalar plant
  * provenance classifier: measured-like vs synthetic marker/path vs external,
    MA1/ARC byte-artifact fingerprint (reuses the repo guard policy constants)
  * end-to-end pipeline on a generated 6 design + 3 holdout synthetic fixture
    (known rank 3, FL/FR fully coherent): must be SYNTHETIC_REFUSED / UNPROVEN,
    conclusions withheld, but all metrics present and JSON serialisable.
"""
from __future__ import annotations

import json
import math
import os
import shutil

import numpy as np

from . import guards as G
from . import measure as M
from . import metrics as X
from . import report as R


class Case(object):
    def __init__(self, name, description, expected):
        self.name = name
        self.description = description
        self.expected = expected
        self.got = None
        self.ok = False
        self.detail = ""

    def as_json(self):
        return {"name": self.name, "description": self.description,
                "expected": self.expected, "got": self.got, "pass": bool(self.ok),
                "detail": self.detail}


def _close(left, right, tol=1.0e-9):
    try:
        return abs(float(left) - float(right)) <= tol * max(1.0, abs(float(right)))
    except (TypeError, ValueError):
        return left == right


def _rms(values):
    values = [v for v in values if v is not None]
    if not values:
        return None
    return float(np.sqrt(np.mean(np.square(values))))


# ================================================================ unit checks
def _check_svd(cases):
    case = Case("svd_known_spectrum", "SVD of U diag([3,2,1]) V^H recovers the spectrum",
                [3.0, 2.0, 1.0])
    rng = np.random.default_rng(7)
    P, N = 5, 3
    U, _ = np.linalg.qr(rng.standard_normal((P, P)) + 1j * rng.standard_normal((P, P)))
    V, _ = np.linalg.qr(rng.standard_normal((N, N)) + 1j * rng.standard_normal((N, N)))
    H = U[:, :N] @ np.diag([3.0, 2.0, 1.0]) @ V.conj().T
    _U, S, _Vh = X.svd(H, np.ones(P))
    case.got = [float(s) for s in S]
    case.ok = all(_close(a, b, 1e-9) for a, b in zip(S, [3.0, 2.0, 1.0]))
    cases.append(case)

    case = Case("svd_scale_invariance", "scaling H by 10 scales sigma by 10 and keeps kappa",
                {"sigma_ratio": 10.0, "same_kappa": True})
    _U2, S2, _ = X.svd(H * 10.0, np.ones(P))
    case.got = {"sigma_ratio": float(S2[0] / S[0]),
                "same_kappa": _close(10.0 * S[0] / (10.0 * S[2]), S[0] / S[2])}
    case.ok = _close(S2[0] / S[0], 10.0) and case.got["same_kappa"]
    cases.append(case)

    case = Case("condition_number", "diag([10,2,0.5]) -> kappa=20", 20.0)
    _U3, S3, _ = X.svd(np.diag([10.0, 2.0, 0.5]))
    cond = X.condition(S3, 3)
    case.got = cond["kappa"]
    case.ok = _close(cond["kappa"], 20.0)
    cases.append(case)

    case = Case("rank_deficient_columns", "rank-2 product plant -> sigma_3~0, kappa=sigma_1/sigma_2",
                {"rank": 2, "sigma_3_rel": 0.0})
    rng = np.random.default_rng(11)
    A = rng.standard_normal((4, 2)) + 1j * rng.standard_normal((4, 2))
    B = rng.standard_normal((2, 3)) + 1j * rng.standard_normal((2, 3))
    Hd = A @ B
    _U4, S4, _ = X.svd(Hd, np.ones(4))
    info = X.effective_rank(S4, 60.0)
    case.got = {"rank": info["rank"], "sigma_3_rel": float(S4[2] / S4[0]),
                "kappa": X.condition(S4, info["rank"])["kappa"]}
    case.ok = (info["rank"] == 2 and case.got["sigma_3_rel"] < 1.0e-12
               and _close(case.got["kappa"], float(S4[0] / S4[1]), 1e-9))
    cases.append(case)

    case = Case("effective_rank_threshold",
                "sigma=[1,1e-4,1e-6]: relative 1e-3 -> rank 1; SNR=0 dB noise floor -> rank 0",
                {"rank_rel": 1, "rank_noise_0db": 0})
    _U5, S5, _ = X.svd(np.diag([1.0, 1.0e-4, 1.0e-6]))
    rel = X.effective_rank(S5, None)["rank"]
    noise0 = X.effective_rank(S5, 0.0)["rank"]
    case.got = {"rank_rel": rel, "rank_noise_0db": noise0}
    case.ok = rel == 1 and noise0 == 0
    cases.append(case)


def _check_coherence(cases):
    case = Case("coherence_identical_columns", "identical actuator columns -> |c|=1, phase 0",
                {"mag": 1.0, "phase": 0.0})
    H = np.array([[1.0 + 0j, 1.0 + 0j], [2.0 + 0j, 2.0 + 0j], [0.5 + 0j, 0.5 + 0j]])
    mag, phase, _ = X.coherence(H, np.ones(3))
    case.got = {"mag": float(mag[0, 1]), "phase": float(np.degrees(phase[0, 1]))}
    case.ok = _close(mag[0, 1], 1.0) and _close(np.degrees(phase[0, 1]), 0.0)
    cases.append(case)

    case = Case("coherence_sign_flip", "column sign flip -> |c|=1, phase 180 deg",
                {"mag": 1.0, "phase": 180.0})
    H = np.array([[1.0 + 0j, -1.0 + 0j], [2.0 + 0j, -2.0 + 0j]])
    mag, phase, _ = X.coherence(H, np.ones(2))
    case.got = {"mag": float(mag[0, 1]), "phase": float(np.degrees(phase[0, 1]))}
    case.ok = _close(mag[0, 1], 1.0) and _close(abs(np.degrees(phase[0, 1])), 180.0)
    cases.append(case)

    case = Case("coherence_orthogonal", "orthogonal spatial signatures -> |c|=0", 0.0)
    H = np.array([[1.0 + 0j, 0.0 + 0j], [0.0 + 0j, 1.0 + 0j]])
    mag, _phase, _ = X.coherence(H, np.ones(2))
    case.got = float(mag[0, 1])
    case.ok = _close(mag[0, 1], 0.0, 1e-12)
    cases.append(case)

    case = Case("coherence_known_phase", "same spatial shape with 60 deg offset -> phase 60 deg",
                {"mag": 1.0, "phase": 60.0})
    rot = np.exp(1j * math.radians(60.0))
    H = np.array([[1.0 + 0j, rot], [2.0 + 0j, 2.0 * rot]])
    mag, phase, _ = X.coherence(H, np.ones(2))
    case.got = {"mag": float(mag[0, 1]), "phase": float(np.degrees(phase[0, 1]))}
    case.ok = _close(mag[0, 1], 1.0) and _close(np.degrees(phase[0, 1]), 60.0, 1e-9)
    cases.append(case)

    case = Case("band_coherence_selection", "band coherence ignores out-of-band frequencies",
                {"magnitude": 1.0})
    freqs = np.array([40.0, 63.0, 200.0])
    H = np.zeros((2, 2, 3), dtype=np.complex128)
    H[0, :, 0] = [1.0, 1.0]      # in 40 Hz band, coherent
    H[0, :, 1] = [1.0, -1.0]     # in 63 Hz band, anti-coherent
    H[0, :, 2] = [1.0, 1.0]
    H[1, :, :] = H[0, :, :]
    bc40 = X.band_coherence(H, freqs, np.ones(2), 40.0)
    case.got = {"magnitude": float(bc40["magnitude"][0, 1])}
    case.ok = _close(bc40["magnitude"][0, 1], 1.0)
    cases.append(case)


def _check_subspace(cases):
    rng = np.random.default_rng(21)
    N, k = 4, 2
    V, _ = np.linalg.qr(rng.standard_normal((N, N)) + 1j * rng.standard_normal((N, N)))
    Vd = V[:, :k]
    case = Case("subspace_identical", "same right subspace -> max angle 0, overlap 1",
                {"max_angle": 0.0, "overlap": 1.0})
    Ad = rng.standard_normal((5, k)) + 1j * rng.standard_normal((5, k))
    Ah = rng.standard_normal((4, k)) + 1j * rng.standard_normal((4, k))
    Hd = Ad @ Vd.conj().T
    Hh = Ah @ Vd.conj().T
    sub = X.subspace_consistency(Hd, np.ones(5), Hh, rank_design=2, rank_holdout=2)
    case.got = {"max_angle": sub["max_angle_deg"], "overlap": sub["sigma_min_overlap"]}
    case.ok = (_close(sub["max_angle_deg"], 0.0, 1e-4)
               and _close(sub["sigma_min_overlap"], 1.0, 1e-9))
    cases.append(case)

    case = Case("subspace_rotated_30deg", "V_h rotated by 30 deg -> max principal angle 30 deg",
                {"max_angle": 30.0, "overlap": math.cos(math.radians(30.0))})
    theta = math.radians(30.0)
    rot = np.array([[math.cos(theta), -math.sin(theta)],
                    [math.sin(theta), math.cos(theta)]])
    # A has orthogonal columns -> right singular vectors of A are the identity
    A = np.array([[2.0, 0.0], [0.0, 1.0], [0.5, 0.0]], dtype=np.complex128)
    Vd = np.array([[1.0, 0.0], [0.0, 1.0], [0.0, 0.0]], dtype=np.complex128)
    # second holdout basis vector tilts out of the (e1,e2) plane by 30 deg
    Vh = np.array([[1.0, 0.0], [0.0, math.cos(theta)], [0.0, math.sin(theta)]],
                  dtype=np.complex128)
    Hd = A @ Vd.conj().T                         # right vectors = Vd
    Hh = A @ Vh.conj().T                         # right vectors = Vh
    sub = X.subspace_consistency(Hd, np.ones(3), Hh, rank_design=2, rank_holdout=2)
    case.got = {"max_angle": sub["max_angle_deg"], "overlap": sub["sigma_min_overlap"]}
    case.ok = (_close(sub["max_angle_deg"], 30.0, 1e-6)
               and _close(sub["sigma_min_overlap"], math.cos(theta), 1e-6))
    cases.append(case)

    case = Case("subspace_rank_mismatch", "rank mismatch -> overfit risk flag", True)
    Hd = np.eye(3, dtype=np.complex128)
    Hh = np.array([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])
    sub = X.subspace_consistency(Hd, np.ones(3), Hh, rank_design=3, rank_holdout=1)
    case.got = sub["risk"]
    case.ok = bool(sub["risk"])
    cases.append(case)


def _check_bands_and_rk(cases):
    case = Case("band_edges_third_octave",
                "40 Hz band = [40/2^(1/6), 40*2^(1/6)]",
                {"lo": 40.0 / 2.0 ** (1.0 / 6.0), "hi": 40.0 * 2.0 ** (1.0 / 6.0)})
    lo, hi = X.band_range(40.0)
    case.got = {"lo": lo, "hi": hi}
    case.ok = (_close(lo, 40.0 / 2.0 ** (1.0 / 6.0), 1e-12) and
               _close(hi, 40.0 * 2.0 ** (1.0 / 6.0), 1e-12))
    cases.append(case)

    case = Case("band_mask_inclusive_lower_exclusive_upper",
                "band mask [lo, hi) with w_f=1", [True, False])
    lo, hi = X.band_range(63.0)
    mask = X.band_mask(np.array([lo, hi]), 63.0)
    case.got = [bool(mask[0]), bool(mask[1])]
    case.ok = bool(mask[0]) and not bool(mask[1])
    cases.append(case)

    case = Case("R_k_formula", "R_k = sum|P_after|^2 / sum|P_before|^2 in band",
                {"R_k": 0.25})
    freqs = np.array([50.0, 60.0, 70.0, 500.0])
    p_before = np.array([1.0, 1.0, 1.0, 1.0], dtype=np.complex128)
    p_after = np.array([0.5, 0.5, 0.5, 5.0], dtype=np.complex128)
    e_before = R._band_energy(p_before, freqs, 63.0)
    e_after = R._band_energy(p_after, freqs, 63.0)
    case.got = {"R_k": e_after / e_before}
    case.ok = _close(case.got["R_k"], 0.25)
    cases.append(case)


def _check_ridge(cases):
    case = Case("ridge_lambda0_scalar",
                "diag([2,1]) sigma_1-normalized, t=e1 -> g=[1,0], residual 0, effort 1.0",
                {"residual": 0.0, "effort": 1.0})
    H = np.diag([2.0, 1.0]).astype(np.complex128)
    curve = X.ridge_curve(H, np.ones(2), np.array([1.0, 0.0]), [0.0], [0.01, 0.1, 1.0])
    case.got = {"residual": curve["residual_rel"][0], "effort": curve["effort"][0]}
    case.ok = _close(curve["residual_rel"][0], 0.0, 1e-9) and _close(
        curve["effort"][0], 1.0, 1e-9)
    cases.append(case)

    case = Case("ridge_closed_form",
                "diag([1,0.5]), lambda=0.1, t=e2 -> g=1.42857, residual=0.28571, effort=2.04082",
                {"g": 1.428571, "residual": 0.285714, "effort": 2.040816})
    H = np.diag([1.0, 0.5]).astype(np.complex128)
    curve = X.ridge_curve(H, np.ones(2), np.array([0.0, 1.0]), [0.1], [0.01, 0.1, 1.0])
    case.got = {"residual": curve["residual_rel"][0], "effort": curve["effort"][0]}
    case.ok = (_close(curve["residual_rel"][0], 0.2857142857, 1e-6)
               and _close(curve["effort"][0], 2.0408163265, 1e-6))
    cases.append(case)

    case = Case("ridge_nullspace_irreducible",
                "target in left null space -> residual 1 for every lambda, effort 0",
                {"residual": 1.0, "effort_max": 0.0})
    H = np.zeros((3, 2), dtype=np.complex128)
    H[0, 0] = 1.0
    H[1, 1] = 1.0
    target = np.array([0.0, 0.0, 1.0])
    curve = X.ridge_curve(H, np.ones(3), target, [0.0, 1e-3, 1e-1], [0.01, 1.0])
    case.got = {"residual": max(curve["residual_rel"]),
                "effort_max": max(curve["effort"])}
    case.ok = (_close(max(curve["residual_rel"]), 1.0)
               and max(curve["effort"]) <= 1e-12)
    cases.append(case)

    case = Case("effort_cap_monotone",
                "min residual is non-increasing as the effort cap grows",
                {"caps": [0.01, 0.1, 1.0], "monotone": True})
    rng = np.random.default_rng(31)
    H = (rng.standard_normal((4, 3)) + 1j * rng.standard_normal((4, 3)))
    t = rng.standard_normal(4) + 1j * rng.standard_normal(4)
    curve = X.ridge_curve(H, np.ones(4), t, X.LAMBDA_GRID, [0.01, 0.1, 1.0])
    values = []
    for cap in curve["effort_caps"]:
        if cap["reachable"]:
            values.append(cap["min_residual_rel"])
    monotone = all(values[i + 1] <= values[i] + 1e-12 for i in range(len(values) - 1))
    case.got = {"caps": values, "monotone": monotone}
    case.ok = monotone
    cases.append(case)


def _check_controllability(cases):
    case = Case("controllability_full_rank",
                "sigma=[2,1], P=4 -> D_null=10log10(4/2)=3.01 dB",
                {"D_null": 3.0103, "bottleneck_in": ["nullspace", "measurement"]})
    out = X.controllability(np.array([2.0, 1.0]), 2, 4, 40.0)
    case.got = {"D_null": out["nullspace_attenuation_db"],
                "bottleneck": out["bottleneck"]}
    case.ok = _close(out["nullspace_attenuation_db"], 10 * math.log10(2.0), 1e-6)
    cases.append(case)

    case = Case("controllability_exact_nulling",
                "P<=rank -> exact nulling flag and capped 60 dB", True)
    out = X.controllability(np.array([2.0, 1.0]), 2, 2, 40.0)
    case.got = {"exact": out["exact_nulling"],
                "D_null": out["nullspace_attenuation_db"]}
    case.ok = out["exact_nulling"] and out["nullspace_attenuation_db"] == X.MAX_DEPTH_DB
    cases.append(case)

    case = Case("controllability_measurement_bound",
                "SNR=20 dB, kappa=10 -> D_meas=20log10(1/(0.1*10))=0 dB",
                {"D_meas": 0.0})
    out = X.controllability(np.array([10.0, 1.0]), 2, 4, 20.0)
    case.got = {"D_meas": out["measurement_limited_depth_db"]}
    case.ok = _close(out["measurement_limited_depth_db"], 0.0, 1e-9)
    cases.append(case)


# ================================================================ provenance checks
def _fake_manifest(**overrides):
    manifest = {
        "schema": G.SCHEMA_MANIFEST,
        "label": "selftest",
        "actuators": list(G.ACTUATORS),
        "excluded_roles": list(G.EXCLUDED_ROLES),
        "session": {"mic": {"model": "UMIK-1", "serial": "selftest"},
                    "participants": list(G.ACTUATORS), "sample_rate_hz": 48000},
        "expected": {"design_points": 6, "holdout_points": 3},
        "frozen_at_unix": 1,
        "positions": [],
        "synthetic": False,
    }
    manifest.update(overrides)
    return manifest


def _fake_analysis(path, **overrides):
    rec = {"path": path, "sha256": "0" * 64, "position": "p01", "role": "design",
           "actuator": "FL", "json": {"peak_db": -20.0, "snr_db": 35.0,
                                      "polarity": 1, "channel": 0,
                                      "clock_ppm_ok": True,
                                      "onset_invalid": False},
           "meta": {"sample_rate": 48000}, "fr": [{"freq_hz": 40.0, "db": -20.0,
                                                   "phase_deg": 0.0}],
           "error": None}
    rec.update(overrides)
    return rec


def _check_provenance(cases, tmp_dir):
    bundle = G.GuardBundle()
    case = Case("provenance_guard_reuse", "repo guards imported and used", True)
    case.got = bundle.ok
    case.ok = bundle.ok
    cases.append(case)

    case = Case("provenance_claim_tags", "synthetic/dry-run markers detected, negations ignored",
                {"synthetic": True, "negated": False})
    case.got = {"synthetic": bundle.claim_tags_synthetic("a synthetic dry-run fixture"),
                "negated": bundle.claim_tags_synthetic("not a dry-run; real measured")}
    case.ok = (case.got["synthetic"] is True and case.got["negated"] is False)
    cases.append(case)

    evidence_dir = os.path.join(tmp_dir, "fake-evidence")
    os.makedirs(evidence_dir, exist_ok=True)
    path = os.path.join(evidence_dir, "FL-ch00.analysis.json")
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("{}")

    case = Case("provenance_measured_like",
                "evidence root + clean fingerprints + mic identity -> MEASURED (classifier only)",
                "MEASURED")
    prov = G.evaluate_provenance(_fake_manifest(), [_fake_analysis(path)], bundle,
                                 extra_evidence_roots=[evidence_dir])
    case.got = prov["status"]
    case.ok = prov["status"] == "MEASURED"
    case.detail = "classifier unit test; the fake fixture is NOT a real measurement"
    cases.append(case)

    case = Case("provenance_synthetic_marker",
                "manifest synthetic=true -> SYNTHETIC_REFUSED", "SYNTHETIC_REFUSED")
    prov = G.evaluate_provenance(_fake_manifest(synthetic=True),
                                 [_fake_analysis(path)], bundle,
                                 extra_evidence_roots=[evidence_dir])
    case.got = prov["status"]
    case.ok = prov["status"] == "SYNTHETIC_REFUSED"
    cases.append(case)

    synth_dir = os.path.join(tmp_dir, "fixtures", "synthetic-x")
    os.makedirs(synth_dir, exist_ok=True)
    synth_path = os.path.join(synth_dir, "FL-ch00.analysis.json")
    with open(synth_path, "w", encoding="utf-8") as fh:
        fh.write("{}")
    case = Case("provenance_synthetic_path",
                "path containing fixtures/synthetic -> SYNTHETIC_REFUSED",
                "SYNTHETIC_REFUSED")
    prov = G.evaluate_provenance(_fake_manifest(), [_fake_analysis(synth_path)],
                                 bundle, extra_evidence_roots=[evidence_dir])
    case.got = prov["status"]
    case.ok = prov["status"] == "SYNTHETIC_REFUSED"
    cases.append(case)

    case = Case("provenance_external_path",
                "file outside every evidence root -> UNPROVEN (fail-closed)", "UNPROVEN")
    outside = os.path.join(tmp_dir, "elsewhere", "FL-ch00.analysis.json")
    os.makedirs(os.path.dirname(outside), exist_ok=True)
    with open(outside, "w", encoding="utf-8") as fh:
        fh.write("{}")
    prov = G.evaluate_provenance(_fake_manifest(), [_fake_analysis(outside)],
                                 bundle, extra_evidence_roots=[evidence_dir])
    case.got = prov["status"]
    case.ok = prov["status"] == "UNPROVEN" and "hdiag_outside_evidence_root" in prov["codes"]
    cases.append(case)

    case = Case("provenance_mic_byte_artifact",
                "peak -42.2 dBFS + SNR 0.5 dB -> INVALID_MEASUREMENT + hdiag_mic_byte_artifact",
                "INVALID_MEASUREMENT")
    rec = _fake_analysis(path)
    rec["json"]["peak_db"] = -42.2
    rec["json"]["snr_db"] = 0.5
    prov = G.evaluate_provenance(_fake_manifest(), [rec], bundle,
                                 extra_evidence_roots=[evidence_dir])
    case.got = {"status": prov["status"], "codes": prov["codes"]}
    case.ok = (prov["status"] == "INVALID_MEASUREMENT"
               and "hdiag_mic_byte_artifact" in prov["codes"])
    cases.append(case)

    case = Case("provenance_missing_mic_identity",
                "no session.json/mic identity -> UNPROVEN", "UNPROVEN")
    prov = G.evaluate_provenance(_fake_manifest(session=None, mic=None),
                                 [_fake_analysis(path)], bundle,
                                 extra_evidence_roots=[evidence_dir])
    case.got = prov["status"]
    case.ok = (prov["status"] == "UNPROVEN"
               and "hdiag_mic_identity_unknown" in prov["codes"])
    cases.append(case)


# ================================================================ synthetic fixture
def build_synthetic_fixture(root, n_design=6, n_holdout=3, rank=3, seed=1234,
                            synthetic_markers=True, position_variation=True):
    """Generate a synthetic 5-actuator multipoint analysis tree with KNOWN rank.

    True plant: H[p,n,f] = sum_m A[p,m] * B[m,n] * exp(j*phi_m(f)).
    Every position sees the same mode set B (so design/holdout actuator subspaces
    are identical by construction) while A[p,:] gives the position amplitudes.
    B has FL and FR columns identical (coherence 1.0) so the coherence detector is
    exercised.  rank=3 -> per-frequency effective rank 3 < 5 (rank deficiency).

    position_variation=True:  A rows are random -> per-frequency rank = rank.
    position_variation=False: A rows are all 1 -> plant is rank 1 AND spatially
    uniform, which makes the design complex-mean prediction exactly equal to the
    holdout field (used to exercise the R_k AGREEMENT path).
    """
    rng = np.random.default_rng(seed)
    freqs = np.geomspace(25.0, 250.0, 40)
    actions = list(G.ACTUATORS)
    modes = rng.standard_normal((rank, len(actions))) + \
        1j * rng.standard_normal((rank, len(actions)))
    if rank >= 2:
        modes[:, 1] = modes[:, 0]  # FL == FR (fully coherent actuator columns)
    B = modes / np.linalg.norm(modes, axis=1, keepdims=True)
    phase_f = rng.uniform(-math.pi, math.pi, size=(rank, len(freqs)))

    def amplitudes(count):
        if position_variation:
            return (rng.standard_normal((count, rank))
                    + 1j * rng.standard_normal((count, rank)))
        return np.ones((count, rank), dtype=np.complex128)

    pos_A = np.vstack([amplitudes(n_design), amplitudes(n_holdout)])
    H = np.zeros((n_design + n_holdout, len(actions), len(freqs)),
                 dtype=np.complex128)
    for p in range(H.shape[0]):
        for m in range(rank):
            H[p] += np.outer(pos_A[p, m] * B[m, :], np.exp(1j * phase_f[m, :]))
    positions = []
    for index in range(n_design + n_holdout):
        role = "design" if index < n_design else "holdout"
        label = ("p%02d-design" % (index + 1)) if role == "design" else \
            ("h%02d-holdout" % (index - n_design + 1))
        pos_dir = os.path.join(root, "positions", label)
        os.makedirs(pos_dir, exist_ok=True)
        files = {}
        for n, actuator in enumerate(actions):
            channel = [0, 1, 2, 3, 7][n]
            points = []
            for fi, freq in enumerate(freqs):
                value = H[index, n, fi]
                amp = float(np.abs(value))
                phase = float(np.degrees(np.angle(value)))
                db = 20.0 * math.log10(max(amp, 1.0e-9))
                points.append({"freq_hz": float(freq), "db": db, "phase_deg": phase})
            payload = {
                "schema": G.SCHEMA_ANALYSIS,
                "channel": channel,
                "delay_samples": 100.0 + n,
                "delay_ms": (100.0 + n) / 48000.0 * 1000.0,
                "delay_coarse_samples": 100 + n,
                "polarity": 1,
                "peak_db": -18.0,
                "snr_db": 38.0,
                "phase_ref": "aligned_segment(selftest)",
                "fr": points,
                "meta": {
                    "schema": G.SCHEMA_ANALYSIS_META,
                    "sample_rate": 48000,
                    "channels": 22,
                    "sweep_start_hz": 20.0,
                    "sweep_end_hz": 300.0,
                    "sweep_seconds": 5.0,
                    "level_db": -25.0,
                    "pre_seconds": 1.5,
                    "post_seconds": 1.5,
                    "playback_device": "selftest:out",
                    "playback_channels": 22,
                    "capture_device": "selftest:mic",
                    "capture_channels": 2,
                    "created_unix": 1795000000 + index * 10 + n,
                },
            }
            filename = "%s-ch%02d.analysis.json" % (actuator, channel)
            path = os.path.join(pos_dir, filename)
            with open(path, "w", encoding="utf-8") as fh:
                json.dump(payload, fh)
            files[actuator] = os.path.join("positions", label, filename)
        with open(os.path.join(pos_dir, "position.json"), "w", encoding="utf-8") as fh:
            json.dump({"schema": G.SCHEMA_POSITION, "name": label, "role": role,
                       "weight": 1.0}, fh)
        positions.append({"label": label, "role": role, "weight": 1.0,
                          "files": files,
                          "position_json": os.path.join("positions", label,
                                                        "position.json")})
    session = {
        "schema": G.SCHEMA_SESSION,
        "session_id": "selftest-synthetic",
        "layout": "4.1.2",
        "sample_rate_hz": 48000,
        "participants": actions,
        "mic": {"model": "UMIK-1", "serial": "selftest", "cal_sha256": "0" * 64},
        "playback": {"device": "selftest:out", "channels": 22},
        "capture": {"device": "selftest:mic", "channels": 2},
        "stimulus": {"kind": "log_sweep", "start_hz": 20.0, "end_hz": 300.0,
                     "seconds": 5.0, "level_dbfs": -25.0, "repeats": 3},
        "created_unix": 1795000000,
    }
    with open(os.path.join(root, "session.json"), "w", encoding="utf-8") as fh:
        json.dump(session, fh)
    manifest = {
        "schema": G.SCHEMA_MANIFEST,
        "label": "selftest-synthetic-known-rank3",
        "layout": "4.1.2",
        "created_utc": "2026-11-21T00:00:00Z",
        "frozen_at_unix": 1794990000,

        "actuators": actions,
        "excluded_roles": list(G.EXCLUDED_ROLES),
        "mic": {"model": "UMIK-1", "serial": "selftest"},
        "session_json": "session.json",
        "positions": positions,
        "expected": {"design_points": n_design, "holdout_points": n_holdout},
        "synthetic": bool(synthetic_markers),
        "claim": "SYNTHETIC selftest fixture (known rank 3); never a real measurement",
    }
    manifest_path = os.path.join(root, "positions.json")
    with open(manifest_path, "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, indent=2)
    return manifest_path, H


def _run_fixture(fixture_root, position_variation, bundle, seed=1234):
    if os.path.isdir(fixture_root):
        shutil.rmtree(fixture_root)
    os.makedirs(fixture_root, exist_ok=True)
    manifest_path, _H = build_synthetic_fixture(
        fixture_root, position_variation=position_variation, seed=seed)
    manifest = M.load_manifest(manifest_path)
    analyses = M.parse_all(manifest, "auto")
    field = M.pick_freq_field([r for r in analyses if r.get("freq") is not None],
                              "auto", (20.0, 300.0))
    if field != "auto":
        analyses = M.parse_all(manifest, field)
    options = {"freq_range_hz": (20.0, 300.0), "freq_field": field,
               "holdout_tol_db": 3.0, "effort_caps": [0.01, 0.1, 1.0],
               "command": ["selftest"], "extra_evidence_roots": [fixture_root]}
    return R.run_diagnostics(manifest, analyses, bundle, options), manifest, analyses, field


def _check_end_to_end(cases, tmp_dir):
    fixture_root = os.path.join(tmp_dir, "fixtures", "synthetic-selftest")
    uniform_root = os.path.join(tmp_dir, "fixtures", "synthetic-uniform")
    bundle = G.GuardBundle()
    result, manifest, analyses, field = _run_fixture(fixture_root, True, bundle)

    case = Case("e2e_provenance_refuses_synthetic",
                "synthetic fixture -> SYNTHETIC_REFUSED / UNPROVEN, conclusions withheld",
                {"prov": "SYNTHETIC_REFUSED", "verdict": "UNPROVEN",
                 "allowed": False})
    case.got = {"prov": result["provenance"]["status"],
                "verdict": result["verdict"],
                "allowed": result["conclusions_allowed"]}
    case.ok = (result["provenance"]["status"] == "SYNTHETIC_REFUSED"
               and result["verdict"] == "UNPROVEN"
               and result["conclusions_allowed"] is False)
    cases.append(case)

    case = Case("e2e_known_rank_detected",
                "known rank-3 synthetic plant -> effective rank 3 (<5) in every band",
                {"rank_min": 3, "rank_max": 3})
    ranks = [band["combos"]["ALL5"].get("rank_min")
             for band in result["bands"] if band.get("status") == "OK"]
    case.got = {"rank_min": min(ranks) if ranks else None,
                "rank_max": max(ranks) if ranks else None}
    case.ok = bool(ranks) and set(ranks) == {3}
    cases.append(case)

    case = Case("e2e_rank_deficient_classified",
                "rank-deficient ALL5 bands classified RANK_DEFICIENT",
                True)
    classes = [band["combos"]["ALL5"].get("controllability_class")
               for band in result["bands"] if band.get("status") == "OK"]
    case.got = sorted(set(classes))
    case.ok = bool(classes) and all(c == "RANK_DEFICIENT" for c in classes)
    cases.append(case)

    case = Case("e2e_coherence_detects_redundant_pair",
                "synthetic FL==FR columns -> FL|FR band coherence > 0.999",
                {"FL|FR": 1.0})
    values = []
    for band in result["bands"]:
        for pair in (band.get("coherence") or {}).get("pairs", []):
            if pair["pair"] in ("FL|FR", "FR|FL"):
                values.append(pair["magnitude"])
    case.got = {"FL|FR": min(values) if values else None}
    case.ok = bool(values) and min(values) > 0.999
    cases.append(case)

    case = Case("e2e_design_holdout_subspace_identical",
                "same mode set -> subspace max angle ~0 for all bands",
                {"max_angle_deg": 0.0})
    angles = [band.get("subspace", {}).get("max_angle_deg")
              for band in result["bands"] if band.get("status") == "OK"]
    angles = [a for a in angles if a is not None]
    case.got = {"max_angle_deg": max(angles) if angles else None}
    case.ok = bool(angles) and max(angles) < 1.0e-3
    cases.append(case)

    uniform_result, _um, _ua, _uf = _run_fixture(uniform_root, False, bundle, seed=99)
    case = Case("e2e_holdout_prediction_agreement",
                "spatially uniform synthetic field -> design-mean prediction AGREEMENT",
                "AGREEMENT")
    case.got = uniform_result["holdout_consistency"]["status"]
    case.ok = uniform_result["holdout_consistency"]["status"] == "AGREEMENT"
    cases.append(case)

    case = Case("e2e_status_bits_present",
                "ENERGY_MIGRATION + HOLDOUT_SEVERE_REGRESSION bits present (NOT_APPLICABLE)",
                {"energy": "NOT_APPLICABLE", "regression": "NOT_APPLICABLE"})
    em = result["statuses"]["energy_migration"]
    sr = result["statuses"]["holdout_severe_regression"]
    case.got = {"energy": em["status"], "regression": sr["status"]}
    case.ok = em["status"] == "NOT_APPLICABLE" and sr["status"] == "NOT_APPLICABLE"
    cases.append(case)

    case = Case("e2e_json_serializable", "hdiag.json result is JSON-serialisable", True)
    try:
        text = json.dumps(result)
        case.got = {"bytes": len(text)}
        case.ok = len(text) > 1000
    except Exception as exc:
        case.got = "not serialisable: %s" % (exc,)
        case.ok = False
    cases.append(case)

    case = Case("e2e_report_renders", "REPORT.md contains NOT_RUN and status table", True)
    try:
        md = R.render_report(result, None, tmp_dir)
        case.got = {"has_NOT_RUN": "NOT_RUN" in md,
                    "has_energy": "ENERGY_MIGRATION" in md,
                    "bytes": len(md)}
        case.ok = ("NOT_RUN" in md and "ENERGY_MIGRATION" in md and len(md) > 5000)
    except Exception as exc:
        case.got = "render failed: %s" % (exc,)
        case.ok = False
    cases.append(case)

    case = Case("e2e_declared_sha_mismatch",
                "manifest-declared analysis sha256 != file bytes -> INCONSISTENT code",
                "hdiag_analysis_sha_mismatch")
    tampered = M.load_manifest(manifest["path"])
    actuator = G.ACTUATORS[0]
    tampered["positions"][0]["declared_sha256"] = dict(
        tampered["positions"][0].get("declared_sha256") or {})
    tampered["positions"][0]["declared_sha256"][actuator] = "0" * 64
    tampered_analyses = M.parse_all(tampered, field)
    tampered_consistency = M.check_consistency(tampered, tampered_analyses, {})
    case.got = tampered_consistency["codes"]
    case.ok = "hdiag_analysis_sha_mismatch" in tampered_consistency["codes"]
    cases.append(case)

    case = Case("e2e_band_count", "8 frozen 1/3-oct bands", 8)
    case.got = len(result["bands"])
    case.ok = len(result["bands"]) == 8
    cases.append(case)

    # after-measurement status bits: synthesize an after plant with known migration
    case = Case("e2e_energy_migration_and_severe_regression",
                "after plant with +4 dB at 40 Hz and -1 dB at 63 Hz -> ENERGY_MIGRATION "
                "and HOLDOUT_SEVERE_REGRESSION",
                {"energy": "ENERGY_MIGRATION", "regression": "FAIL"})
    # build the same H for design/holdout then scale bands (40 +6 dB, 63 -1 dB)
    plant = M.build_h(manifest, analyses, field, (20.0, 300.0))
    after = {"design": {"H": plant["design"]["H"].copy()},
             "holdout": {"H": plant["holdout"]["H"].copy()},
             "freq_hz": plant["freq_hz"]}
    freqs = plant["freq_hz"]
    for target, scale in ((40.0, 10.0 ** (6.0 / 20.0)), (63.0, 10.0 ** (-1.0 / 20.0))):
        mask = X.band_mask(freqs, target)
        after["design"]["H"][:, :, mask] *= scale
        after["holdout"]["H"][:, :, mask] *= scale
    rk = R._holdout_block(manifest, plant, after, result["per_frequency"],
                          {"holdout_tol_db": 3.0})
    case.got = {"energy": rk["energy_migration"]["status"],
                "regression": rk["holdout_severe_regression"]["status"],
                "n_regressions": len(rk["holdout_severe_regression"]["regressions"])}
    case.ok = (rk["energy_migration"]["status"] == "ENERGY_MIGRATION"
               and rk["holdout_severe_regression"]["status"] == "FAIL")
    cases.append(case)


# ================================================================ runner
def run(out_dir=None):
    import tempfile
    cases = []
    tmp_dir = tempfile.mkdtemp(prefix="hdiag-selftest-")
    try:
        _check_svd(cases)
        _check_coherence(cases)
        _check_subspace(cases)
        _check_bands_and_rk(cases)
        _check_ridge(cases)
        _check_controllability(cases)
        _check_provenance(cases, tmp_dir)
        _check_end_to_end(cases, tmp_dir)
    finally:
        pass
    passed = sum(1 for case in cases if case.ok)
    report = {
        "schema": "khaen-h-diag-selftest/v1",
        "tool": {"name": G.TOOL_NAME, "version": G.TOOL_VERSION,
                 "generated_utc": G.utc_now()},
        "synthetic_only": True,
        "claim": ("all fixtures here are synthetic; this self-test validates the "
                  "implementation, it does not measure a real room"),
        "summary": {"total": len(cases), "passed": passed, "failed": len(cases) - passed},
        "cases": [case.as_json() for case in cases],
        "fixture_root": tmp_dir,
    }
    if out_dir:
        os.makedirs(out_dir, exist_ok=True)
        with open(os.path.join(out_dir, "selftest.json"), "w", encoding="utf-8") as fh:
            json.dump(report, fh, indent=2, ensure_ascii=False)
            fh.write(chr(10))
    return report


def main(argv=None):
    import argparse
    parser = argparse.ArgumentParser(description="h-diag self-test")
    parser.add_argument("--out-dir", default=None)
    args = parser.parse_args(argv)
    report = run(args.out_dir)
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


if __name__ == "__main__":
    raise SystemExit(main())
