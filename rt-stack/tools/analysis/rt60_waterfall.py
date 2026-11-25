#!/usr/bin/env python3
"""RT60 (EDT/T20/T30) + CSD waterfall analysis for measured impulse responses.

Offline analysis helper for the TSPI calibration chain (goal 6: record
waterfall / RT60 for every measurement). It never touches audio devices.

Method (v2, 2026-11-22)
-----------------------
The v1 prototype (lead, 2026-11-22) truncated the IR to a fixed 1.5 s and
estimated the "noise floor" from the last 10 % of the *Schroeder EDC*.  That is
wrong by construction: the EDC of a noise-filled tail falls as
10*log10(remaining duration), so the last-10 % value is ~13 dB below the
true noise level.  The resulting gate was too permissive and T20 was fitted
into the noise plateau -> high bands showed 2-25 s RT60.

v2 instead:
 1. band-pass each octave band (Butterworth order 3, zero-phase);
 2. estimate the in-band noise floor from *flat block-energy plateaus* of the
    IR tail (25 ms blocks, spread <= 4 dB, duration >= 0.4 s, at least 15 dB
    below the band peak; the highest such plateau wins so that the numerical
    zero-padding floor of an FFT-deconvolution IR is not mistaken for noise);
 3. locate the decay end ("gate") where the smoothed block energy stays
    below noise + 3 dB for 5 consecutive blocks;
 4. integrate the Schroeder EDC over [onset, decay_end + margin], clipped to
    --window-ms (default 900 ms: our sweep deconvolution keeps tail_s = 0.9 s
    of valid IR), subtract the expected noise energy (Chu/Lundeby noise
    correction) and fit EDT / T15 / T20 / T30 by least squares;
 5. require a quality gate: EDC noise plateau <= -27 dB for T20, fit span and
    R^2 thresholds; a smoothed block-envelope -5..-25 dB fit is computed as a
    cross-check and used as fallback when the Schroeder range is too short;
 6. report per-band flags instead of silently emitting an implausible number.

Usage:
  python rt60_waterfall.py --ir IR1.wav [IR2.wav ...] --out DIR \
         [--label NAME ...] [--bands 31.5,63,...] [--window-ms 900]
  python rt60_waterfall.py --selftest --out DIR

Outputs: rt60.csv, summary.json, csd-<label>.png, decay-<label>.png.
Requires: numpy, scipy, matplotlib.
"""
from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import os
import sys

import numpy as np
import scipy.signal as sig
import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

TOOL_VERSION = "v2-20261122"
DEFAULT_BANDS = [31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0,
                 4000.0, 8000.0, 16000.0]
BLOCK_MS = 25.0
SMOOTH_BLOCKS = 3
NOISE_MIN_DUR_S = 0.4
NOISE_MAX_SPREAD_DB = 4.0
NOISE_MIN_BELOW_PEAK_DB = 15.0
GATE_MARGIN_S = 0.04


# ---------------------------------------------------------------------------
# I/O
# ---------------------------------------------------------------------------
def read_wav(path):
    """Read a PCM (16/24/32-bit int) or IEEE-float mono file; return (x, fs)."""
    import wave
    with wave.open(path, "rb") as w:
        ch, width, rate, frames = (w.getnchannels(), w.getsampwidth(),
                                   w.getframerate(), w.getnframes())
        raw = w.readframes(frames)
        comp = w.getcomptype()
    if comp != "NONE":
        raise SystemExit("%s: unsupported compression %s" % (path, comp))
    if width == 2:
        x = np.frombuffer(raw, dtype="<i2").astype(np.float64) / 32768.0
    elif width == 3:
        b = np.frombuffer(raw, dtype=np.uint8).reshape(-1, 3)
        v = (b[:, 0].astype(np.int32)
             | (b[:, 1].astype(np.int32) << 8)
             | (b[:, 2].astype(np.int32) << 16))
        v = np.where(v & 0x800000, v - 0x1000000, v)
        x = v.astype(np.float64) / 8388608.0
    elif width == 4:
        x = np.frombuffer(raw, dtype="<i4").astype(np.float64) / 2147483648.0
    else:
        raise SystemExit("%s: unsupported sample width %d" % (path, width))
    if ch > 1:
        x = x.reshape(-1, ch)[:, 0]
    return x, rate


def sha256_file(path, limit=16 * 1024 * 1024):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ---------------------------------------------------------------------------
# Signal helpers
# ---------------------------------------------------------------------------
def onset_index(x, rate):
    """Direct-sound onset: |peak| in the first 100 ms (IR is already trimmed)."""
    lim = min(len(x), int(0.1 * rate))
    return int(np.argmax(np.abs(x[:lim])))


def band_signal(x, fc, rate):
    """Octave band centred on fc, Butterworth order 3, zero phase.

    The IR starts abruptly at the direct sound.  scipy's default padlen for
    sosfiltfilt (~20 samples) is far too short for narrow low-frequency bands:
    the forward-backward pass then starts with a large artificial ramp (for a
    250 Hz octave a unit impulse produced an EDC error of ~30 dB and 1000x too
    much energy).  Zero-pad by ~10 periods / >=0.3 s on both sides, filter,
    then crop; the measured impulse-response energy then matches theory.
    """
    nyq = rate / 2.0 * 0.98
    lo = fc / math.sqrt(2.0)
    hi = min(fc * math.sqrt(2.0), nyq)
    if lo >= nyq * 0.99:
        return None
    pad = int(min(1.0, max(0.3, 10.0 / fc)) * rate)
    sos = sig.butter(3, [lo, hi], btype="bandpass", fs=rate, output="sos")
    xp = np.concatenate([np.zeros(pad), x, np.zeros(pad)])
    y = sig.sosfiltfilt(sos, xp)[pad:pad + len(x)]
    return y


def block_power(y, rate, block_ms=BLOCK_MS):
    blk = max(1, int(rate * block_ms / 1000.0))
    n = len(y) // blk
    if n < 4:
        return np.zeros(0), blk / float(rate)
    return np.mean(y[:n * blk].reshape(n, blk) ** 2, axis=1), blk / float(rate)


def medfilt_small(x, k=SMOOTH_BLOCKS):
    if len(x) < k:
        return x
    pad = k // 2
    xp = np.pad(x, (pad, pad), mode="edge")
    return np.median(np.stack([xp[i:i + len(x)] for i in range(k)]), axis=0)


def find_noise_plateau(pblock, dt, peak_db):
    """Return (index_lo, index_hi, level_db, duration_s) of the in-band noise plateau.

    Scans 25 ms block powers for runs with <= NOISE_MAX_SPREAD_DB variation and
    duration >= NOISE_MIN_DUR_S that are >= NOISE_MIN_BELOW_PEAK_DB below the band
    peak.  Among the runs the *highest* level wins: an FFT-deconvolution IR often
    contains a much lower numerical-floor plateau (zero padding) after the real
    measurement noise, which must not be used as the noise gate.
    """
    if len(pblock) < 8:
        return None
    db = 10.0 * np.log10(np.maximum(pblock, 1e-300))
    sm = medfilt_small(db, 3)
    win = max(4, int(NOISE_MIN_DUR_S / dt))
    runs = []
    i = 0
    n = len(sm)
    while i + win <= n:
        seg = sm[i:i + win]
        if seg.max() - seg.min() <= NOISE_MAX_SPREAD_DB and np.median(seg) <= peak_db - NOISE_MIN_BELOW_PEAK_DB:
            j = i + win
            while j < n:
                s2 = sm[i:j + 1]
                if s2.max() - s2.min() > NOISE_MAX_SPREAD_DB:
                    break
                j += 1
            runs.append((i, j - 1, float(np.median(sm[i:j])), (j - i) * dt))
            i = j
        else:
            i += 1
    if not runs:
        return None
    long_runs = [r for r in runs if r[3] >= NOISE_MIN_DUR_S] or runs
    return max(long_runs, key=lambda r: r[2])


def decay_gate_index(pblock, dt, nf_db, peak_idx):
    """First block after the peak where smoothed level stays <= nf+3 dB (5 blocks)."""
    if len(pblock) < 6:
        return None
    sm = 10.0 * np.log10(np.maximum(medfilt_small(pblock, 3), 1e-300))
    gate = nf_db + 3.0
    for i in range(max(peak_idx + 1, 1), len(sm) - 4):
        if all(sm[i + k] <= gate for k in range(5)):
            return i
    return None


def last_above_gate(pblock, nf_db):
    db = 10.0 * np.log10(np.maximum(pblock, 1e-300))
    above = np.flatnonzero(db > nf_db + 3.0)
    return int(above[-1]) if len(above) else 0


def schroeder_edc(seg, rate, noise_power):
    """Noise-corrected Schroeder EDC in dB re total energy (Chu correction).

    E_c(t) = sum_{i>=t} x_i^2 - N * (#samples >= t).  Guarded against negative
    values.  Returns (t, edc_db_corrected, edc_db_raw, e0).
    """
    e = np.cumsum((seg ** 2)[::-1])[::-1]
    e0 = float(e[0]) if len(e) else 0.0
    t = np.arange(len(seg)) / float(rate)
    if e0 <= 0:
        return t, np.zeros(len(seg)), np.zeros(len(seg)), 0.0
    raw = 10.0 * np.log10(np.maximum(e / e0, 1e-300))
    corr = np.maximum(e - noise_power * np.arange(len(seg), 0, -1), 1e-300)
    corr_db = 10.0 * np.log10(corr / e0)
    return t, corr_db, raw, e0


def fit_decay(t, y, start_db, end_db, min_points=4):
    """Least-squares line through the EDC between start_db and end_db."""
    ia = int(np.argmax(y <= start_db))
    if ia >= len(y) or y[ia] > start_db:
        return None
    ib = int(np.argmax(y <= end_db))
    if ib >= len(y) or y[ib] > end_db or ib <= ia:
        return None
    tt, yy = t[ia:ib + 1], y[ia:ib + 1]
    if len(yy) < min_points or tt[-1] <= tt[0]:
        return None
    a = np.vstack([tt, np.ones_like(tt)]).T
    sol = np.linalg.lstsq(a, yy, rcond=None)[0]
    pred = a @ sol
    ss_res = float(np.sum((yy - pred) ** 2))
    ss_tot = float(np.sum((yy - yy.mean()) ** 2))
    r2 = 1.0 - ss_res / ss_tot if ss_tot > 0 else 0.0
    slope = float(sol[0])
    return {
        "slope_db_s": slope,
        "intercept_db": float(sol[1]),
        "t0_s": float(tt[0]),
        "t1_s": float(tt[-1]),
        "span_s": float(tt[-1] - tt[0]),
        "r2": float(r2),
        "rt60_s": (-60.0 / slope) if slope < -1e-9 else None,
        "i0": int(ia),
        "i1": int(ib),
    }


def fit_envelope(pblock, dt, peak_db, nf_db):
    """Block-envelope decay fit (-5..-25 dB and -5..-20 dB), a cross-check."""
    db = 10.0 * np.log10(np.maximum(pblock, 1e-300))
    last = last_above_gate(pblock, nf_db)
    pk = int(np.argmax(db))
    if last <= pk + 4:
        return None

    def fit(a_db, b_db):
        i0 = next((i for i in range(pk, last + 1) if db[i] <= peak_db - a_db), None)
        if i0 is None:
            return None
        i1 = next((i for i in range(i0, last + 1) if db[i] <= peak_db - b_db), None)
        if i1 is None or i1 - i0 < 4:
            return None
        tt = np.arange(i0, i1 + 1) * dt
        yy = db[i0:i1 + 1]
        aa = np.vstack([tt, np.ones_like(tt)]).T
        sol = np.linalg.lstsq(aa, yy, rcond=None)[0]
        pred = aa @ sol
        ss_res = float(np.sum((yy - pred) ** 2))
        ss_tot = float(np.sum((yy - yy.mean()) ** 2))
        r2 = 1.0 - ss_res / ss_tot if ss_tot > 0 else 0.0
        slope = float(sol[0])
        return {
            "slope_db_s": slope, "t0_s": float(tt[0]), "t1_s": float(tt[-1]),
            "span_s": float(tt[-1] - tt[0]), "r2": r2,
            "rt60_s": (-60.0 / slope) if slope < -1e-9 else None,
        }

    return {"env20": fit(5.0, 25.0), "env15": fit(5.0, 20.0), "last_above_s": last * dt}


# ---------------------------------------------------------------------------
# Per-band analysis
# ---------------------------------------------------------------------------
def analyze_band(x, rate, onset, fc, window_ms, verbose=False):
    y = band_signal(x[onset:], fc, rate)
    if y is None or len(y) < int(0.2 * rate):
        return None
    pblock, dt = block_power(y, rate)
    if len(pblock) < 8:
        return None
    pk_db = 10.0 * np.log10(max(float(np.max(pblock)), 1e-300))
    noise = find_noise_plateau(pblock, dt, pk_db)
    if noise is None:
        # fall back to the last 20 % of the record as noise estimate
        nb = max(4, int(len(pblock) * 0.2))
        nf = float(np.median(pblock[-nb:]))
        noise = (len(pblock) - nb, len(pblock) - 1, 10.0 * math.log10(max(nf, 1e-300)),
                 nb * dt)
        noise_conf = "low"
    else:
        noise_conf = "ok"
    nf_db = noise[2]
    nf_power = 10.0 ** (nf_db / 10.0)
    nf_spread = None
    run = pblock[noise[0]:noise[1] + 1]
    if len(run) >= 2:
        nf_spread = float(10.0 * math.log10(
            (np.percentile(run, 90) + 1e-300) / (np.percentile(run, 10) + 1e-300)))

    peak_idx = int(np.argmax(pblock))
    gate_idx = decay_gate_index(pblock, dt, nf_db, peak_idx)
    if gate_idx is None:
        decay_end_s = len(y) / float(rate)
        window_limited = True
    else:
        decay_end_s = gate_idx * dt
        window_limited = False

    win_s = min(len(y) / float(rate), window_ms / 1000.0)
    if not window_limited:
        win_s = min(win_s, decay_end_s + GATE_MARGIN_S)
    win_s = max(win_s, 0.12)
    nseg = min(len(y), int(win_s * rate))
    t, edc, edc_raw, e0 = schroeder_edc(y[:nseg], rate, nf_power)
    plateau_db = 10.0 * math.log10(max(nf_power * nseg / max(e0, 1e-300), 1e-300))

    out = {
        "center_hz": fc,
        "noise_floor_db": nf_db,
        "noise_floor_dbfs": nf_db,  # band levels are dB re full-scale peak
        "noise_plateau_s": (noise[0] * dt, noise[1] * dt),
        "noise_spread_db": nf_spread,
        "noise_confidence": noise_conf,
        "decay_end_s": decay_end_s,
        "window_s": nseg / float(rate),
        "edc_plateau_db": plateau_db,
        "edc": edc.tolist(),
        "edc_t": t.tolist(),
    }
    out["edt"] = fit_decay(t, edc, 0.0, -10.0)
    out["t15"] = fit_decay(t, edc, -5.0, -20.0)
    out["t20"] = fit_decay(t, edc, -5.0, -25.0)
    out["t30"] = fit_decay(t, edc, -5.0, -35.0)
    out["env"] = fit_envelope(pblock, dt, pk_db, nf_db)

    env20 = out["env"]["env20"] if out["env"] else None
    env15 = out["env"]["env15"] if out["env"] else None
    flags = []
    method = None
    rt60 = None
    r2 = None

    def noise_ratio(fit):
        """noise energy remaining at fit end / corrected signal energy there."""
        if not fit or "i1" not in fit:
            return None
        rem = max(nseg - fit["i1"], 1)
        noise_rem = nf_power * rem
        pred_db = fit["intercept_db"] + fit["slope_db_s"] * fit["t1_s"]
        sig = max(e0 * 10.0 ** (pred_db / 10.0), 1e-300)
        return float(noise_rem / sig)

    def ok(fit, min_span, min_r2, max_ratio):
        if not (fit and fit.get("rt60_s") and fit["span_s"] >= min_span and fit["r2"] >= min_r2):
            return False
        nr = noise_ratio(fit)
        return nr is not None and nr <= max_ratio

    out["noise_ratio_t20"] = noise_ratio(out["t20"])
    out["noise_ratio_t15"] = noise_ratio(out["t15"])

    if ok(out["t20"], 0.06, 0.99, 2.0):
        method, rt60, r2 = "T20", out["t20"]["rt60_s"], out["t20"]["r2"]
    elif ok(out["t15"], 0.05, 0.985, 3.0):
        method, rt60, r2 = "T15x4", out["t15"]["rt60_s"] * 4.0 / 3.0, out["t15"]["r2"]
        flags.append("T20_noise_limited")
    elif env20 and env20.get("rt60_s") and env20["span_s"] >= 0.05 and env20["r2"] >= 0.97:
        method, rt60, r2 = "ENV20", env20["rt60_s"], env20["r2"]
        flags.append("schroeder_limited")
    elif env15 and env15.get("rt60_s") and env15["span_s"] >= 0.04 and env15["r2"] >= 0.96:
        method, rt60, r2 = "ENV15x1.33", env15["rt60_s"] * 20.0 / 15.0, env15["r2"]
        flags.extend(["schroeder_limited", "envelope_limited"])
    elif ok(out["edt"], 0.03, 0.98, 4.0):
        method, rt60, r2 = "EDT_x6", out["edt"]["rt60_s"] * 6.0, out["edt"]["r2"]
        flags.extend(["schroeder_limited", "edt_only_low_confidence"])
    else:
        flags.append("unmeasurable")

    if window_limited:
        flags.append("decay_exceeds_window")
    if noise_conf != "ok":
        flags.append("noise_confidence_low")
    if nf_spread is not None and nf_spread > 8.0:
        flags.append("noise_nonstationary")
    if plateau_db > -27.0:
        flags.append("edc_noise_plateau_high")
    # cross check between the chosen fit family and the other estimator
    cross = None
    if rt60:
        others = []
        if method.startswith("T"):
            if env20 and env20.get("rt60_s"):
                others.append(env20["rt60_s"])
        else:
            if out["t20"] and out["t20"].get("rt60_s"):
                others.append(out["t20"]["rt60_s"])
        others = [v for v in others if v and v > 1e-3]
        if others:
            ref = others[0]
            cross = abs(rt60 - ref) / max(ref, 1e-6)
            if cross > 0.15:
                flags.append("cross_check_mismatch")
    if rt60 is not None:
        if rt60 < 0.1:
            flags.append("rt_short_suspect")
        elif rt60 > 1.2:
            flags.append("rt_long_suspect")
        elif not (0.2 <= rt60 <= 0.8):
            flags.append("outside_typical_residential")
        if decay_end_s >= window_ms / 1000.0 * 0.95:
            flags.append("gate_at_window_edge")

    # A reported RT60 is only "valid" when it comes from a supported estimator
    # and none of the sanity flags that would disqualify it are set.  EDT_x6
    # and ENV15 fallbacks are kept for traceability but never claim validity.
    disqualified = {"unmeasurable", "rt_long_suspect", "rt_short_suspect",
                    "cross_check_mismatch", "decay_exceeds_window",
                    "noise_confidence_low"}
    valid = bool(rt60 is not None and method in {"T20", "T15x4", "ENV20"}
                 and not any(f in flags for f in disqualified))
    if method == "T20" and valid and (cross is None or cross <= 0.10):
        confidence = "high"
    elif valid:
        confidence = "medium"
    elif rt60 is not None:
        confidence = "low"
    else:
        confidence = "none"
    out.update({
        "method": method,
        "rt60_s": round(rt60, 4) if rt60 else None,
        "r2": round(r2, 4) if r2 else None,
        "env_rt60_s": round(env20["rt60_s"], 4) if env20 and env20.get("rt60_s") else None,
        "env_r2": round(env20["r2"], 4) if env20 else None,
        "cross_check_rel": round(cross, 4) if cross is not None else None,
        "flags": ";".join(flags),
        "valid": valid,
        "confidence": confidence,
    })
    if verbose:
        print("  %7.1f Hz %-12s rt60=%s r2=%s nf=%.1f plateau=%.1f gate=%.2fs win=%.2fs flags=%s"
              % (fc, str(method), ("%.3f" % rt60) if rt60 else "None",
                 ("%.3f" % r2) if r2 else "-", nf_db, plateau_db, decay_end_s,
                 nseg / float(rate), out["flags"]))
    return out


# ---------------------------------------------------------------------------
# Plots
# ---------------------------------------------------------------------------
def plot_csd(x, rate, onset, path, label, window_ms=600.0):
    seg = x[onset:onset + int(window_ms / 1000.0 * rate)]
    nperseg = 1024
    f, tt, z = sig.stft(seg, fs=rate, nperseg=nperseg,
                        noverlap=nperseg * 3 // 4, boundary=None, padded=False)
    mag = 20.0 * np.log10(np.maximum(np.abs(z), 1e-12))
    mag -= mag.max()
    sel = (f >= 30.0) & (f <= 16000.0)
    fig, ax = plt.subplots(figsize=(10, 6), dpi=130)
    im = ax.pcolormesh(tt * 1000.0, f[sel], mag[sel], shading="auto",
                       cmap="magma", vmin=-72, vmax=0)
    ax.set_yscale("log")
    ax.set_ylim(30, 16000)
    ax.set_xlim(0, window_ms)
    ax.set_xlabel("time after direct sound (ms)")
    ax.set_ylabel("frequency (Hz)")
    ax.set_title("CSD waterfall (dB re peak, 21 ms Hann) - %s" % label)
    fig.colorbar(im, ax=ax, label="dB")
    fig.tight_layout()
    fig.savefig(path)
    plt.close(fig)


def plot_decay(bands, rate, path, label, window_ms):
    fig, axes = plt.subplots(1, 2, figsize=(13, 5.6), dpi=130,
                             gridspec_kw={"width_ratios": [3, 2]})
    ax = axes[0]
    cmap = plt.get_cmap("viridis")
    usable = [b for b in bands if b.get("edc")]
    for i, b in enumerate(usable):
        tt = np.array(b["edc_t"]) * 1000.0
        yy = np.array(b["edc"])
        m = tt <= window_ms
        color = cmap(i / max(1, len(usable) - 1))
        ax.plot(tt[m], yy[m], color=color, lw=1.1,
                label="%d Hz %s" % (int(b["center_hz"]),
                                    ("%.2fs" % b["rt60_s"]) if b.get("rt60_s") else "-"))
        fit = b.get("t20") or b.get("t15")
        if fit and fit.get("t0_s") is not None:
            x0, x1 = fit["t0_s"] * 1000.0, fit["t1_s"] * 1000.0
            if x1 > x0:
                y0 = fit["intercept_db"] + fit["slope_db_s"] * fit["t0_s"]
                y1 = fit["intercept_db"] + fit["slope_db_s"] * fit["t1_s"]
                ax.plot([x0, x1], [y0, y1], color=color, lw=2.2, alpha=0.55)
    ax.set_xlim(0, window_ms)
    ax.set_ylim(-70, 5)
    ax.set_xlabel("time after direct sound (ms)")
    ax.set_ylabel("Schroeder EDC (dB, noise-corrected)")
    ax.grid(alpha=0.25)
    ax.legend(fontsize=6.5, ncol=2, loc="lower left")
    ax.set_title("EDC + least-squares fit - %s" % label)

    ax2 = axes[1]
    for i, b in enumerate(usable):
        tt = np.array(b["edc_t"]) * 1000.0
        yy = np.array(b["edc"])
        m = tt <= min(window_ms, 400.0)
        if not np.any(m):
            continue
        ax2.semilogx(np.maximum(tt[m], 1e-3), yy[m], lw=1.0,
                     color=cmap(i / max(1, len(usable) - 1)))
    ax2.set_xlim(5, 400)
    ax2.set_ylim(-70, 5)
    ax2.set_xlabel("time (ms, log)")
    ax2.set_ylabel("EDC (dB)")
    ax2.grid(alpha=0.25, which="both")
    ax2.set_title("early decay, log time")
    fig.tight_layout()
    fig.savefig(path)
    plt.close(fig)


def plot_rt_summary(rows, path):
    labels = []
    for r in rows:
        if r["label"] not in labels:
            labels.append(r["label"])
    bands = sorted({float(r["center_hz"]) for r in rows})
    fig, ax = plt.subplots(figsize=(9, 5), dpi=130)
    width = 0.8 / max(1, len(labels))
    for li, lab in enumerate(labels):
        vals = []
        for fc in bands:
            hit = [r for r in rows if r["label"] == lab and float(r["center_hz"]) == fc]
            vals.append(hit[0]["rt60_s"] if hit and hit[0].get("rt60_s") else np.nan)
        xs = np.arange(len(bands)) + li * width - 0.4 + width / 2
        ax.bar(xs, vals, width=width, label=lab)
    ax.set_xticks(np.arange(len(bands)))
    ax.set_xticklabels([("%g" % b) for b in bands], rotation=0)
    ax.set_xlabel("octave band (Hz)")
    ax.set_ylabel("RT60 (s)")
    ax.set_ylim(0, max(1.0, np.nanmax([r["rt60_s"] or 0 for r in rows]) * 1.25))
    ax.axhspan(0.2, 0.8, color="green", alpha=0.08, label="typical residential 0.2-0.8 s")
    ax.grid(alpha=0.25, axis="y")
    ax.legend(fontsize=8)
    ax.set_title("RT60 by octave band")
    fig.tight_layout()
    fig.savefig(path)
    plt.close(fig)


# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------
def analyze_ir(path, label, bands, window_ms, outdir, verbose):
    x, rate = read_wav(path)
    peak = float(np.max(np.abs(x))) if len(x) else 0.0
    if peak <= 0:
        raise SystemExit("%s: all-zero IR" % path)
    x = x / peak
    onset = onset_index(x, rate)
    rows = []
    for fc in bands:
        b = analyze_band(x, rate, onset, fc, window_ms, verbose=verbose)
        if b is None:
            rows.append({"label": label, "ir": path, "center_hz": fc, "method": None,
                         "rt60_s": None, "valid": False, "flags": "band_out_of_range"})
            continue
        b.update({"label": label, "ir": path})
        rows.append(b)
    plot_csd(x, rate, onset, os.path.join(outdir, "csd-%s.png" % label), label)
    plot_decay(rows, rate, os.path.join(outdir, "decay-%s.png" % label), label, window_ms)
    return {
        "label": label, "path": path, "sha256": sha256_file(path),
        "sample_rate": rate, "samples": len(x), "onset_sample": onset,
        "onset_ms": onset / float(rate) * 1000.0,
        "peak_dbfs": 20.0 * math.log10(peak),
        "bands": rows,
    }


CSV_FIELDS = ["label", "ir", "center_hz", "method", "rt60_s", "r2", "valid", "confidence",
              "env_rt60_s", "env_r2", "cross_check_rel", "noise_floor_db",
              "noise_spread_db", "noise_confidence", "noise_plateau_start_s",
              "noise_plateau_end_s", "decay_end_s", "window_s", "edc_plateau_db",
              "edt_rt60_s", "t20_s", "t20_r2", "t30_s", "flags"]


def rows_to_csv(rows, path):
    with open(path, "w", newline="", encoding="utf-8") as fh:
        w = csv.DictWriter(fh, fieldnames=CSV_FIELDS, extrasaction="ignore")
        w.writeheader()
        for r in rows:
            rec = dict(r)
            rec["noise_plateau_start_s"] = (r.get("noise_plateau_s") or (None, None))[0]
            rec["noise_plateau_end_s"] = (r.get("noise_plateau_s") or (None, None))[1]
            rec["edt_rt60_s"] = (r.get("edt") or {}).get("rt60_s")
            rec["t20_s"] = (r.get("t20") or {}).get("rt60_s")
            rec["t20_r2"] = (r.get("t20") or {}).get("r2")
            rec["t30_s"] = (r.get("t30") or {}).get("rt60_s")
            w.writerow(rec)


def selftest(outdir, rate=48000):
    """Synthetic-IR self test: recover known exponential RT60 within tolerance."""
    os.makedirs(outdir, exist_ok=True)
    rng = np.random.default_rng(20261122)
    results = []
    for rt_true in (0.30, 0.50, 0.80):
        n = int(1.6 * rate)
        t = np.arange(n) / float(rate)
        # Statistical room IR: short direct pulse + exponentially decaying
        # diffuse tail (amplitude envelope 10^(-3 t / RT60) -> exact RT60 tail)
        # + additive broadband noise at -45 dBFS re peak (realistic band SNR).
        # Average N_AVG independent realisations, like a repeated-measurement
        # IR average; a single realisation of a narrow-band tail has Rayleigh
        # energy fluctuations too large to validate an estimator.
        n_avg = 8
        acc = np.zeros(n)
        for _ in range(n_avg):
            tail = 0.20 * 10.0 ** (-3.0 * t / rt_true) * rng.standard_normal(n)
            ir = tail
            ir[0] += 1.0
            ir = ir + 10.0 ** (-45.0 / 20.0) * rng.standard_normal(n)
            acc += ir / np.max(np.abs(ir))
        ir = acc / n_avg
        band_rows = []
        # 250 Hz octave is deliberately excluded from the self test: its
        # 5.6 ms coherence time means even an 8-realisation average carries
        # ~1 dB EDC ripple, which dominates a 20 dB fit.  Low-band RT needs
        # longer averaging and is reported with flags on real data.
        for fc in (1000.0, 2000.0, 4000.0):
            b = analyze_band(ir, rate, 0, fc, 900.0, verbose=verbose_selftest)
            band_rows.append(b)
        results.append({"rt_true_s": rt_true, "bands": band_rows})
    lines = []
    ok = True
    for res in results:
        for b in res["bands"]:
            if not b:
                continue
            got = b.get("rt60_s")
            tol = max(0.05, 0.12 * res["rt_true_s"])
            passed = bool(got is not None and abs(got - res["rt_true_s"]) <= tol)
            ok &= passed
            lines.append({"rt_true_s": res["rt_true_s"], "center_hz": b["center_hz"],
                          "rt60_s": got, "error_s": None if got is None else got - res["rt_true_s"],
                          "tol_s": tol, "pass": passed, "method": b.get("method"),
                          "flags": b.get("flags")})
    with open(os.path.join(outdir, "selftest.json"), "w", encoding="utf-8") as fh:
        json.dump({"schema": "tspi-rt60-selftest/v1", "tool": TOOL_VERSION,
                   "passed": bool(ok), "cases": lines}, fh, ensure_ascii=False, indent=2)
    for ln in lines:
        print("selftest rt_true=%.2f fc=%6.1f -> rt=%s method=%s %s"
              % (ln["rt_true_s"], ln["center_hz"],
                 ("%.3f" % ln["rt60_s"]) if ln["rt60_s"] else "None",
                 ln["method"], "PASS" if ln["pass"] else "FAIL"))
    return 0 if ok else 2


verbose_selftest = False


def main():
    ap = argparse.ArgumentParser(description="RT60 + CSD waterfall analysis (offline)")
    ap.add_argument("--ir", nargs="+", help="input IR wav file(s)")
    ap.add_argument("--label", nargs="+", default=None, help="labels, one per --ir")
    ap.add_argument("--out", required=True, help="output directory")
    ap.add_argument("--bands", default=",".join("%g" % b for b in DEFAULT_BANDS))
    ap.add_argument("--window-ms", type=float, default=900.0,
                    help="max analysis window after direct sound (default 900 ms: "
                         "sweep deconvolution tail_s=0.9 s)")
    ap.add_argument("--selftest", action="store_true", help="run synthetic-RT self test")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()
    os.makedirs(args.out, exist_ok=True)

    if args.selftest:
        return selftest(args.out)

    if not args.ir:
        ap.error("--ir is required unless --selftest")
    labels = args.label[:] if args.label else []
    if labels and len(labels) != len(args.ir):
        ap.error("--label count must match --ir count")
    if not labels:
        labels = []
        for p in args.ir:
            parent = os.path.basename(os.path.dirname(os.path.abspath(p)))
            base = os.path.splitext(os.path.basename(p))[0]
            labels.append((parent + "-" + base) if parent else base)
    # uniqueness
    seen = {}
    for i, lab in enumerate(labels):
        if lab in seen:
            labels[i] = lab + "-%d" % i
        seen[labels[i]] = i

    bands = [float(v) for v in args.bands.split(",") if v.strip()]
    irs = []
    all_rows = []
    for path, label in zip(args.ir, labels):
        if not os.path.isfile(path):
            raise SystemExit("missing IR: %s" % path)
        if args.verbose:
            print("== %s (%s)" % (label, path))
        info = analyze_ir(path, label, bands, args.window_ms, args.out, args.verbose)
        irs.append(info)
        all_rows.extend(info["bands"])

    rows_to_csv(all_rows, os.path.join(args.out, "rt60.csv"))
    plot_rt_summary(all_rows, os.path.join(args.out, "rt60-summary.png"))
    summary = {
        "schema": "tspi-rt60-waterfall/v2",
        "tool": {"name": "rt60_waterfall.py", "version": TOOL_VERSION},
        "created_unix": int(__import__("time").time()),
        "config": {"bands_hz": bands, "window_ms": args.window_ms,
                   "block_ms": BLOCK_MS, "noise_min_duration_s": NOISE_MIN_DUR_S},
        "irs": [{k: v for k, v in info.items() if k != "bands"} for info in irs],
        "rows": [{k: r.get(k) for k in
                  ["label", "center_hz", "method", "rt60_s", "r2", "valid", "confidence", "flags"]}
                 for r in all_rows],
    }
    with open(os.path.join(args.out, "summary.json"), "w", encoding="utf-8") as fh:
        json.dump(summary, fh, ensure_ascii=False, indent=2)
    valid = [r for r in all_rows if r.get("valid")]
    print(json.dumps({
        "out": args.out,
        "irs": labels,
        "bands_total": len(all_rows),
        "bands_valid": len(valid),
        "bands_0p2_0p8": sum(1 for r in valid if 0.2 <= (r.get("rt60_s") or 0) <= 0.8),
        "csv": os.path.join(args.out, "rt60.csv"),
    }, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
