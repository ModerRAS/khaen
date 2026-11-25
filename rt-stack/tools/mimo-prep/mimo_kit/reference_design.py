#!/usr/bin/env python3
"""Maintained copy of the pilot_3mic reference G-design objective (task-252).

Provenance / why this file exists
---------------------------------
The 2026-11-19 pilot design chain (evidence/mimo-pilot-3mic/prep/ops-0830/steps/
design_from_identity.py) imported design_g() from the *dry-run* harness
prep/dryrun/mimo_chain.py (a module whose own docstring says the room is a
synthetic 4.10x5.20x2.70 m assumption and whose design report is tagged
synthetic=true).  For task-252 the numeric objective is kept BIT-COMPATIBLE
(verified: tests/test_provenance.py::test_reference_design_parity_with_dryrun
reproduces evidence/mimo-pilot-3mic/prep/dryrun/out/design/g_design.npz exactly
from the dry-run h_estimate.npz), but the module lives in the maintained kit,
carries real-H provenance, and never tags a design as synthetic unless its H is.

Objective (old pilot target, unchanged): per input role, spatial-average
equalisation with weighted, band-limited, regularised complex LS over the design
mics; per-DAC coherent-sum guard <= 12 dB (uniform scaling fallback); linear-phase
FIR realization (identity outside 20-520 Hz).  P=3<M=5 pilot: no holdout.

Origin: evidence/mimo-pilot-3mic/prep/dryrun/mimo_chain.py
        sha256 c6096059fb9b78587c18aaa1dbfe5db75cf70f71c85ca3d2c9ae7473d8a161d0
        (functions design_g/_smooth_complex/_realize_fir/_interp_complex/_band_beta)
"""
import json
import math
import os

import numpy as np

FS = 48000
DESIGN_BAND = (20.0, 300.0)
ROLE_TO_SLOT = {"FL": 0, "FR": 1, "SL": 2, "SR": 3, "HFL": 4, "HFR": 5, "SUB1": 7}
PARTICIPANTS = ["FL", "FR", "SL", "SR", "SUB1"]
DESIGN_MICS = ["UMIK", "MA1", "ARC"]
OUTPUTS = [("FL", 0), ("FR", 1), ("SL", 2), ("SR", 3), ("HFL", 4), ("HFR", 5), ("SUB1", 7)]
BAND_LIMIT = {"FL": (80.0, 300.0), "FR": (80.0, 300.0), "SL": (100.0, 300.0),
              "SR": (100.0, 300.0), "SUB1": (20.0, 130.0)}
PILOT_MICS = ["UMIK", "MA1", "ARC"]


def jdump(path, obj):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(obj, fh, ensure_ascii=False, indent=2)
        fh.write("\n")


def db(x):
    return 20.0 * np.log10(np.abs(x) + 1e-30)


def participant_index(name):
    return [n for n, _ in OUTPUTS].index(name)


def _interp_complex(grid_f, grid_h, target_f):
    g = np.asarray(grid_f)
    pts = np.asarray(target_f)
    return np.interp(pts, g, np.real(grid_h)) + 1j * np.interp(pts, g, np.imag(grid_h))


def _band_beta(freqs, name):
    lo, hi = BAND_LIMIT.get(name, (20.0, 300.0))
    beta = np.ones_like(freqs)
    beta[(freqs < lo) | (freqs > hi)] = 1e4
    return beta


def _smooth_complex(freqs, G, per_oct=12.0):
    """Complex power-average smoothing on the log grid (reduces band-edge ringing)."""
    f = np.asarray(freqs, dtype=np.float64)
    out = np.zeros_like(G)
    for i, fc in enumerate(f):
        lo, hi = fc / 2.0 ** (0.5 / per_oct), fc * 2.0 ** (0.5 / per_oct)
        m = (f >= lo) & (f <= hi)
        if not m.any():
            m = np.array([i])
        mag = np.sqrt(np.mean(np.abs(G[:, :, m]) ** 2, axis=2))
        ref = G[:, :, m].sum(axis=2) / np.maximum(m.sum(), 1)
        out[:, :, i] = mag * np.exp(1j * np.angle(ref + 1e-30))
    return out


def _realize_fir(freqs, G, taps, delay):
    """Linear-phase FIR realization of a 7x7 (or NxN) frequency-domain G matrix."""
    nfft = max(taps, 4096)
    f_fir = np.fft.rfftfreq(nfft, 1.0 / FS)
    G_fir = np.zeros((G.shape[0], G.shape[1], taps))
    for j in range(G.shape[0]):
        for i in range(G.shape[1]):
            Gf = _interp_complex(freqs, G[j, i, :], f_fir)
            Gf[(f_fir < 20.0) | (f_fir > 520.0)] = 1.0 if i == j else 0.0
            spec = np.zeros(nfft, dtype=np.complex128)
            spec[: f_fir.size] = Gf * np.exp(-2j * math.pi * f_fir * delay / FS)
            spec[f_fir.size:] = np.conj(spec[1:f_fir.size - 1][::-1])
            G_fir[j, i, :] = np.fft.ifft(spec).real[:taps]
    return G_fir


def design_reference(h, outdir, max_gain_db=6.0, delay=4096, taps=8192,
                     design_band=DESIGN_BAND, provenance=None):
    """legacy pilot_3mic reference objective (numeric parity with design_g).

    h: {"freq": (nf,), "H_norm": (n_mics, 7, nf) in OUTPUTS order, "info": {...}}
    provenance: optional dict recorded in design-report.json (H sha/arm, guard verdict)
    """
    os.makedirs(outdir, exist_ok=True)
    freqs = np.asarray(h["freq"])
    Hn = np.asarray(h["H_norm"])
    if Hn.shape[1] != len(OUTPUTS):
        raise ValueError("H_norm second axis must be %d OUTPUTS entries, got %s"
                         % (len(OUTPUTS), Hn.shape))
    band = (freqs >= design_band[0]) & (freqs <= design_band[1])
    nf = freqs.size
    n_in = len(PARTICIPANTS)
    p_idx = [participant_index(p) for p in PARTICIPANTS]
    weights = np.array([1.0, 0.7, 0.5])[: Hn.shape[0]]
    t60_est = 0.45
    H = Hn
    g_all = np.zeros((len(DESIGN_MICS), n_in, n_in, nf), dtype=np.complex128)
    h_info = (h.get("info") or {}) if isinstance(h, dict) else {}
    synthetic = bool(h_info.get("synthetic", False)) or os.path.normpath(
        str(h_info.get("arm") or h_info.get("session") or "")).replace("\\", "/").find("/prep/dryrun/") >= 0
    diag = {"schema": "khaen-mimo-reference-design/v2", "synthetic": synthetic,
            "claim": ("pilot_3mic reference design from a REAL remeasure identity arm; "
                      "NOT production design_g.py; no holdout; not T6/T67/T68/T69 acceptance"
                      if not synthetic else
                      "pilot_3mic reference design (dry-run); NOT production design_g.py; no holdout"),
            "participants": PARTICIPANTS,
            "role_to_slot": {p: ROLE_TO_SLOT[p] for p in PARTICIPANTS},
            "mics": list(h_info.get("mic_names") or DESIGN_MICS),
            "design_band_hz": list(design_band), "max_gain_db": max_gain_db,
            "fir_geometry": {"taps_48k": int(taps), "delay_samples": int(delay),
                             "delay_ms": delay / FS * 1000.0},
            "per_input": []}
    if provenance:
        diag["provenance"] = provenance
    lam_scale = float(np.median(np.abs(H[:, p_idx, :][:, :, band]) ** 2)) or 1.0
    lam_candidates = 10.0 ** np.arange(-3.0, 1.01, 0.25)
    for u, uname in enumerate(PARTICIPANTS):
        b = Hn[:, p_idx[u], :]
        b_avg = b.mean(axis=0, keepdims=True)
        d = np.repeat(b_avg, b.shape[0], axis=0)
        beta = np.stack([_band_beta(freqs, p) for p in PARTICIPANTS], axis=0)
        best = None
        for lam_rel in lam_candidates:
            lam = lam_rel * lam_scale
            g = np.zeros((n_in, nf), dtype=np.complex128)
            for k in np.where(band)[0]:
                A = H[:, p_idx, k] * weights[:, None]
                M = A.conj().T @ (A * weights[:, None]) + lam * np.diag(beta[:, k])
                v = A.conj().T @ ((d[:, k] - b[:, k]) * weights)
                try:
                    g[:, k] = np.linalg.solve(M, v)
                except np.linalg.LinAlgError:
                    g[:, k] = np.linalg.pinv(M) @ v
            maxgain = 20.0 * math.log10(np.max(np.abs(g)) + 1e-30)
            if best is None:
                best = {"lam": lam, "lam_rel": float(lam_rel), "g": g, "maxgain": maxgain}
            elif (maxgain <= max_gain_db + 1e-6 and (best["maxgain"] > max_gain_db or maxgain > best["maxgain"])) \
                    or (not (maxgain <= max_gain_db) and best["maxgain"] > max_gain_db and maxgain < best["maxgain"]):
                best = {"lam": lam, "lam_rel": float(lam_rel), "g": g, "maxgain": maxgain}
        g = best["g"]
        g_all[:, u, :, :] = g
        pred = b.copy()
        for k in range(nf):
            pred[:, k] = b[:, k] + H[:, p_idx, k] @ g[:, k]
        ba = (freqs >= 40) & (freqs <= 200)
        spatial_std = lambda x: float(np.mean(np.std(db(x[:, ba]), axis=0)))
        imp = spatial_std(b) - spatial_std(pred)
        hd = 20.0 * np.log10(np.max(np.abs(g), axis=1) + 1e-30)
        diag["per_input"].append({"input": uname, "lambda": best["lam"], "lambda_rel": best["lam_rel"],
                                  "max_gain_db": float(np.max(hd)),
                                  "per_path_gain_db": {p: float(hd[i]) for i, p in enumerate(PARTICIPANTS)},
                                  "predicted_spatial_std_before_db": spatial_std(b),
                                  "predicted_spatial_std_after_db": spatial_std(pred),
                                  "predicted_improvement_db": imp})
    slots = [participant_index(p) for p in PARTICIPANTS]
    G_ideal = np.zeros((len(OUTPUTS), len(OUTPUTS), nf), dtype=np.complex128)
    for j in range(len(OUTPUTS)):
        G_ideal[j, j, :] = 1.0
    for u in range(n_in):
        for i in range(n_in):
            G_ideal[slots[i], slots[u], :] += g_all[:, u, i, :].mean(axis=0)

    def _dac_gains(matrix):
        out = {}
        for j, (oname, slot) in enumerate(OUTPUTS):
            if oname in PARTICIPANTS:
                out[oname] = {"dac_slot": slot,
                              "max_coherent_gain_db": float(np.max(20.0 * np.log10(np.sum(np.abs(matrix[j]), axis=0) + 1e-30)))}
        return out

    dac_gain = _dac_gains(G_ideal)
    total_scale = 1.0
    for _ in range(6):
        worst = max(v["max_coherent_gain_db"] for v in dac_gain.values())
        if worst <= 11.9:
            break
        scale = 10.0 ** ((11.9 - worst) / 20.0)
        total_scale *= scale
        for u in range(n_in):
            g_all[:, u, :, :] *= scale
        G_ideal = np.zeros((len(OUTPUTS), len(OUTPUTS), nf), dtype=np.complex128)
        for j in range(len(OUTPUTS)):
            G_ideal[j, j, :] = 1.0
        for u in range(n_in):
            for i in range(n_in):
                G_ideal[slots[i], slots[u], :] += g_all[:, u, i, :].mean(axis=0)
        dac_gain = _dac_gains(G_ideal)
    if total_scale != 1.0:
        diag["per_dac_scaled_by"] = total_scale
        for u, uname in enumerate(PARTICIPANTS):
            b = Hn[:, p_idx[u], :]
            pred = b.copy()
            for k in range(nf):
                pred[:, k] = b[:, k] + H[:, p_idx, k] @ g_all[:, u, :, k].mean(axis=0)
            ba = (freqs >= 40) & (freqs <= 200)
            std = lambda x: float(np.mean(np.std(db(x[:, ba]), axis=0)))
            rec = diag["per_input"][u]
            rec["predicted_spatial_std_before_db"] = std(b)
            rec["predicted_spatial_std_after_db"] = std(pred)
            rec["predicted_improvement_db"] = std(b) - std(pred)
    G_smooth = _smooth_complex(freqs, G_ideal, per_oct=12.0)
    G_fir = _realize_fir(freqs, G_smooth, taps, delay)
    realized = _response_of_fir(G_fir, freqs, delay)
    err = 20.0 * np.log10(np.maximum(np.abs(realized - G_ideal), 1e-12))
    band_m = (freqs >= 20) & (freqs <= 300)
    diag["fir_realization"] = {
        "taps_48k": int(taps), "delay_samples": int(delay), "delay_ms": delay / FS * 1000.0,
        "max_abs_err_db_20_300": float(np.max(err[:, :, band_m])),
        "median_abs_err_db_20_300": float(np.median(err[:, :, band_m])),
        "note": "linear-phase FIR realization of the frequency-domain design; production design_g.py solves the causal time-domain Toeplitz problem"}
    diag["per_dac_gain_db"] = dac_gain
    diag["per_dac_gain_limit_db"] = 12.0
    diag["per_dac_gain_ok"] = all(v["max_coherent_gain_db"] <= 12.0 for v in dac_gain.values())
    diag["max_path_gain_ok"] = all(x["max_gain_db"] <= max_gain_db + 1e-6 for x in diag["per_input"])
    np.savez(os.path.join(outdir, "g_design.npz"), freq=freqs, G_ideal=G_ideal, G_fir=G_fir)
    jdump(os.path.join(outdir, "design-report.json"), diag)
    return {"freq": freqs, "G_ideal": G_ideal, "G_fir": G_fir, "report": diag}


def _response_of_fir(G_fir, freqs, delay):
    nfft = 1 << 15
    ff = np.fft.rfftfreq(nfft, 1.0 / FS)
    out = np.zeros((G_fir.shape[0], G_fir.shape[1], freqs.size), dtype=np.complex128)
    for j in range(G_fir.shape[0]):
        for i in range(G_fir.shape[1]):
            Hf = np.fft.rfft(G_fir[j, i], nfft)
            out[j, i, :] = _interp_complex(ff, Hf, freqs) * np.exp(2j * math.pi * freqs * delay / FS)
    return out
