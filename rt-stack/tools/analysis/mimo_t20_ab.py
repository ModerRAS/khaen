#!/usr/bin/env python3
"""Offline identity-vs-MIMO-on T20 A/B analyzer (single point; no device, no apply).

SCOPE TAG: PREDICTED_OFFLINE_ANALYSIS / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT

This tool is an OFFLINE analysis utility. It never opens a sound card, never talks to
the board, never applies a filter and never emits audio. It reads two RIR WAV files
(identity vs MIMO-on) that must have been captured in the SAME session, with the SAME
stimulus family, at the SAME microphone position, and reports per-band decay / level /
(optionally) improvement numbers. It does NOT certify anything: there is no verdict anywhere in
the outputs, and no accept/reject token is ever emitted.

Metrics (normative definitions follow spec/mimo-t6.md sec 4.5 / sec 4.3):

  * 1/3-octave band IR: brick-wall FFT band pass at fc * 2**(+-1/6).
  * Schroeder EDC: L(t) = 10*log10( int_t^inf h^2 / int_0^inf h^2 ).
  * T20 = -20 / slope of L(t) over the [-5, -25] dB window  [s]  (20 dB decay time;
    this is the meeting convention of acoustic-verify-20261122 final_lf.py / lf_edc.py and
    of the main-chain T20 tables). NOTE: spec/mimo-t6.md sec 4.5 labels T20 = 3x that value
    (60 dB extrapolated); this tool reports RT60 = 3 * T20 as a separate column so both
    conventions are visible and never mixed.
  * EDC100 = EDC level 100 ms after the band-IR peak, relative to the peak (0 dB).
  * Band level = 10*log10( sum h_band^2 )  [dB], plus delta vs identity.
  * Validity gates (per band, per arm): V1 noise margin >= 10 dB and noise floor
    <= -35 dB; V2 capture length >= 1.2*t(-25) and tail reached noise floor +5 dB;
    V3 R^2 >= --r2-min (default 0.90 per single-point-diag; use 0.99 for T6-grade work)
    and residual RMS <= 1 dB; V4 slope < 0 and T20 in [0.02, 2.0] s; V5 band SNR >= 25 dB
    (SNR = dynamic range above tail noise).
  * Improvement I (only if --desired is given):
        I_band = 10*log10( sum_f |b(f)-d(f)|^2 / sum_f |y(f)-d(f)|^2 )
    with b=identity complex FR, y=MIMO-on complex FR, d=desired complex FR, f on an
    equal-weight log grid inside the band (spec sec 4.3; overall 63-250 Hz).
    If no desired response d is supplied the tool does NOT invent one: it falls back to
    the DECLARED fallback criterion "relative T20 drop vs identity + band level guard",
    which is reported explicitly in the JSON field "criterion".

Usage:
  py -3 tools/analysis/mimo_t20_ab.py --selftest --out OUTDIR
  py -3 tools/analysis/mimo_t20_ab.py --identity ID.wav --on ON.wav --out OUTDIR
  py -3 tools/analysis/mimo_t20_ab.py --identity ID.wav --on ON.wav --desired D.wav --out OUTDIR
  py -3 tools/analysis/mimo_t20_ab.py --baseline a.wav,b.wav,c.wav --out OUTDIR   (single-arm table)

Multi-channel WAVs: channel k of --identity is paired with channel k of --on (and of
--desired when supplied); each channel is one (input, mic) pair. Mono is the common case.

Dependencies: Python 3.9+, numpy (required; checked at import time).
"""
from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import math
import os
import struct
import sys

try:
    import numpy as np
except Exception as _exc:  # pragma: no cover - environment guard
    sys.stderr.write("mimo_t20_ab: numpy is required (stdlib-only FFT is not implemented): %s\n" % _exc)
    raise SystemExit(4)

try:  # optional: scipy provides the butter(3) zero-phase 1/3-oct filter used by calib-acoustic-verify
    from scipy.signal import butter as _butter, sosfiltfilt as _sosfiltfilt
    HAVE_SCIPY = True
except Exception:  # pragma: no cover - numpy-only fallback path
    HAVE_SCIPY = False

TOOL_VERSION = "1.0.0"
SCHEMA = "khaen-mimo-t20-ab/v1"
SCOPE_CLASS = "SINGLE_POINT_NOT_SPATIAL"
SCOPE_TAGS = ("PREDICTED_OFFLINE_ANALYSIS", "SINGLE_POINT_NOT_SPATIAL", "NO_MEASUREMENT")
SCOPE_NOTE = ("offline analysis of pre-existing RIR WAVs; no sound card, no board, no apply, "
              "no new acoustic measurement, no certification")
DEFAULT_BANDS = (63.0, 100.0, 125.0, 250.0)
DECAY_WINDOW_DB = (-5.0, -25.0)
T20_FACTOR = 3.0
EDC100_S = 0.100
DEFAULT_R2_MIN = 0.90
DEFAULT_T20_RANGE = (0.02, 2.0)
DEFAULT_SNR_MIN_DB = 25.0
DEFAULT_NOISE_MARGIN_MIN_DB = 10.0
DEFAULT_NOISE_FLOOR_MAX_DB = -35.0
DEFAULT_LEVEL_GUARD_DB = 1.5
DEFAULT_GRID_PER_OCT = 24


# --------------------------------------------------------------------------- WAV I/O
def _parse_fmt(body):
    if len(body) < 16:
        raise ValueError("fmt chunk too short")
    afmt, ch, fs, _brate, _align, bits = struct.unpack("<HHIIHH", body[:16])
    sub = afmt
    if afmt == 0xFFFE and len(body) >= 40:  # WAVE_FORMAT_EXTENSIBLE
        sub = struct.unpack("<H", body[24:26])[0]
    return sub, int(ch), int(fs), int(bits)


def wav_read(path):
    """Read a RIFF/WAVE file into (float64 ndarray [frames, channels], meta dict)."""
    with open(path, "rb") as fh:
        raw = fh.read()
    if len(raw) < 44 or raw[0:4] != b"RIFF" or raw[8:12] != b"WAVE":
        raise ValueError("%s: not a RIFF/WAVE file" % path)
    pos = 12
    fmt = None
    data = None
    while pos + 8 <= len(raw):
        cid = raw[pos:pos + 4]
        size = struct.unpack("<I", raw[pos + 4:pos + 8])[0]
        body = raw[pos + 8:pos + 8 + size]
        pos += 8 + size + (size & 1)
        if cid == b"fmt " and fmt is None:
            fmt = _parse_fmt(body)
        elif cid == b"data" and data is None:
            data = body
    if fmt is None or data is None:
        raise ValueError("%s: missing fmt/data chunk" % path)
    afmt, ch, fs, bits = fmt
    if ch < 1:
        raise ValueError("%s: bad channel count %d" % (path, ch))
    if afmt == 1 and bits == 16:
        x = np.frombuffer(data, dtype="<i2").astype(np.float64) / 32768.0
    elif afmt == 1 and bits == 24:
        b = np.frombuffer(data, dtype=np.uint8)
        if b.size % 3:
            b = b[:b.size - (b.size % 3)]
        t = b.reshape(-1, 3).astype(np.int32)
        v = t[:, 0] | (t[:, 1] << 8) | (t[:, 2] << 16)
        v = np.where(v & 0x800000, v - 0x1000000, v)
        x = v.astype(np.float64) / 8388608.0
    elif afmt == 1 and bits == 32:
        x = np.frombuffer(data, dtype="<i4").astype(np.float64) / 2147483648.0
    elif afmt == 3 and bits == 32:
        x = np.frombuffer(data, dtype="<f4").astype(np.float64)
    elif afmt == 3 and bits == 64:
        x = np.frombuffer(data, dtype="<f8").astype(np.float64)
    else:
        raise ValueError("%s: unsupported WAV format (audio_format=%d bits=%d)" % (path, afmt, bits))
    frames = x.size // ch
    if frames == 0:
        raise ValueError("%s: empty data chunk" % path)
    x = x[:frames * ch].reshape(frames, ch)
    meta = {"path": os.path.abspath(path), "sha256": _sha256(path), "fs": int(fs),
            "channels": int(ch), "bits": int(bits), "audio_format": int(afmt),
            "frames": int(frames), "duration_s": frames / float(fs),
            "dtype": "PCM%d" % bits if afmt == 1 else "FLOAT%d" % bits,
            "peak_dbfs": float(20.0 * math.log10(float(np.max(np.abs(x))) + 1e-30))}
    return x, meta


def wav_write(path, fs, data):
    """Write float64 [frames, channels] as IEEE float32 mono/multi-channel WAV (helper)."""
    x = np.asarray(data, dtype=np.float64)
    if x.ndim == 1:
        x = x[:, None]
    n, ch = x.shape
    pcm = x.astype("<f4").tobytes()
    hdr = b"RIFF" + struct.pack("<I", 36 + len(pcm)) + b"WAVE"
    hdr += b"fmt " + struct.pack("<IHHIIHH", 16, 3, ch, int(fs), int(fs) * ch * 4, ch * 4, 32)
    hdr += b"data" + struct.pack("<I", len(pcm))
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(hdr)
        fh.write(pcm)
    return path


def _sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for blk in iter(lambda: fh.read(1 << 20), b""):
            h.update(blk)
    return h.hexdigest()


# --------------------------------------------------------------------------- DSP
def next_pow2(n):
    return 1 << int(math.ceil(math.log2(max(2, int(n)))))


_BUTTER_CACHE = {}


def resolve_filter(method="auto"):
    if method == "auto":
        return "butter3" if HAVE_SCIPY else "brickwall"
    if method == "butter3" and not HAVE_SCIPY:
        raise SystemExit("filter 'butter3' requested but scipy is not available")
    if method not in ("butter3", "brickwall"):
        raise SystemExit("unknown filter method %r" % method)
    return method


def band_ir(ir, fc, fs, pad_s=1.0, method="auto"):
    """1/3-octave band IR at fc.

    method="butter3": 3rd-order Butterworth band pass applied zero-phase (sosfiltfilt);
    same convention as evidence/calib-chain-20261121/acoustic-verify-20261122 final_lf.py.
    method="brickwall": FFT brick-wall mask (numpy-only fallback). Note: an ideal brick-wall
    filter rings on the IR onset and can fake a late tail, which is why butter3 is preferred.
    """
    x = np.asarray(ir, dtype=np.float64)
    lo = fc * 2.0 ** (-1.0 / 6.0)
    hi = fc * 2.0 ** (1.0 / 6.0)
    method = resolve_filter(method)
    if method == "butter3":
        key = (round(float(fc), 6), int(fs))
        sos = _BUTTER_CACHE.get(key)
        if sos is None:
            sos = _butter(3, [lo / (fs / 2.0), hi / (fs / 2.0)], btype="band", output="sos")
            _BUTTER_CACHE[key] = sos
        return _sosfiltfilt(sos, x - float(np.mean(x)))
    x = x - float(np.mean(x))
    n = next_pow2(len(x) + int(pad_s * fs))
    X = np.fft.rfft(x, n)
    f = np.fft.rfftfreq(n, 1.0 / fs)
    X[(f < lo) | (f > hi)] = 0.0
    return np.fft.irfft(X, n)[: len(x)]


def edc_curve(ir):
    """Schroeder reverse energy integral, normalized to L(0)=0 dB. Returns (L_db, p)."""
    p = np.asarray(ir, dtype=np.float64) ** 2
    e = np.cumsum(p[::-1])[::-1]
    e0 = float(e[0]) if e.size else 0.0
    if e0 <= 0.0:
        L = np.full(p.shape, -300.0)
    else:
        L = 10.0 * np.log10(np.maximum(e, 1e-300) / e0)
    return L, p


def tail_noise(ir, fs, tail_frac=0.05, min_tail_s=0.050):
    """Tail noise stats: per-sample noise power q and L_noise relative to E0."""
    p = np.asarray(ir, dtype=np.float64) ** 2
    n = p.size
    if n == 0:
        return {"q": 0.0, "L_noise_db": -300.0, "tail_s": 0.0}
    tail_n = max(int(round(tail_frac * n)), int(round(min_tail_s * fs)), 8)
    tail_n = min(tail_n, n)
    q = float(np.median(p[-tail_n:]))
    e0 = float(np.sum(p))
    if e0 <= 0.0 or q <= 0.0:
        return {"q": 0.0, "L_noise_db": -300.0, "tail_s": tail_n / float(fs)}
    L_noise = 10.0 * math.log10(max(q * n / e0, 1e-300))
    return {"q": q, "L_noise_db": float(L_noise), "tail_s": tail_n / float(fs)}


def fit_band_t20(ir, fc, fs, r2_min=DEFAULT_R2_MIN, t20_range=DEFAULT_T20_RANGE,
                 snr_min_db=DEFAULT_SNR_MIN_DB,
                 noise_margin_min_db=DEFAULT_NOISE_MARGIN_MIN_DB,
                 noise_floor_max_db=DEFAULT_NOISE_FLOOR_MAX_DB, filter_method="auto"):
    """One (arm, band) T20 estimation with spec/mimo-t6.md sec 4.5 validity gates V1..V5."""
    n = len(ir)
    out = {"band_hz": float(fc), "valid": False, "reasons": [], "t20_s": None,
           "rt60_equiv_s": None, "dt_5_25_s": None, "t20_est_s": None, "t5_s": None,
           "t25_s": None, "r2": None,
           "slope_db_per_s": None, "residual_rms_db": None, "edc100_db": None,
           "band_level_db": None, "noise_floor_db": None, "noise_margin_db": None,
           "snr_db": None, "at_peak_s": None}
    if ir is None or n < 64:
        out["reasons"].append("IR_TOO_SHORT")
        return out
    b = band_ir(ir, fc, fs, method=filter_method)
    L, p = edc_curve(b)
    band_level = 10.0 * math.log10(float(np.sum(p)) + 1e-300)
    out["band_level_db"] = float(band_level)
    nz = tail_noise(b, fs)
    out["noise_floor_db"] = float(nz["L_noise_db"])
    peak = int(np.argmax(p))
    out["at_peak_s"] = peak / float(fs)
    i100 = peak + int(round(EDC100_S * fs))
    if i100 < L.size:
        out["edc100_db"] = float(L[i100] - L[peak])
    i5 = int(np.argmax(L <= DECAY_WINDOW_DB[0]))
    i25 = int(np.argmax(L <= DECAY_WINDOW_DB[1]))
    if L[i5] > DECAY_WINDOW_DB[0]:
        out["reasons"].append("NO_MINUS5_CROSSING")
        return out
    if L[i25] > DECAY_WINDOW_DB[1]:
        out["reasons"].append("NO_MINUS25_CROSSING")
        return out
    t = np.arange(L.size) / float(fs)
    i5 = max(i5, 1)
    if i25 <= i5 + 4:
        out["reasons"].append("WINDOW_TOO_SHORT")
        return out
    seg = L[i5:i25 + 1]
    tseg = t[i5:i25 + 1]
    coef = np.polyfit(tseg, seg, 1)
    slope = float(coef[0])
    fit = np.polyval(coef, tseg)
    ss_res = float(np.sum((seg - fit) ** 2))
    ss_tot = float(np.sum((seg - np.mean(seg)) ** 2))
    r2 = 1.0 - ss_res / ss_tot if ss_tot > 0 else 0.0
    resid = math.sqrt(ss_res / seg.size)
    dt20 = float(t[i25] - t[i5])          # -5..-25 dB decay time (20 dB)
    t20_est = dt20                         # meeting convention: T20 = 20 dB decay time (s)
    out.update({"t5_s": float(t[i5]), "t25_s": float(t[i25]), "r2": float(r2),
                "slope_db_per_s": slope, "residual_rms_db": float(resid),
                "dt_5_25_s": dt20, "t20_est_s": float(t20_est),
                "rt60_equiv_s": float(T20_FACTOR * dt20)})
    margin = float(L[i25] - nz["L_noise_db"])
    out["noise_margin_db"] = margin
    snr = -float(nz["L_noise_db"])
    out["snr_db"] = snr
    reasons = []
    if nz["L_noise_db"] > noise_floor_max_db:
        reasons.append("V1_NOISE_FLOOR_HIGH")
    if margin < noise_margin_min_db:
        reasons.append("V1_NOISE_MARGIN_LOW")
    tail_reached = float(L[-1]) <= float(nz["L_noise_db"]) + 5.0
    if (n / float(fs)) < 1.2 * float(t[i25]):
        reasons.append("V2_CAPTURE_TOO_SHORT")
    if not tail_reached:
        reasons.append("V2_TAIL_NOT_AT_NOISE")
    if r2 < r2_min:
        reasons.append("V3_R2_LOW")
    if resid > 1.0:
        reasons.append("V3_RESIDUAL_HIGH")
    if not (slope < 0.0):
        reasons.append("V4_SLOPE_NOT_NEGATIVE")
    if not (t20_range[0] <= t20_est <= t20_range[1]):
        reasons.append("V4_T20_OUT_OF_RANGE")
    if snr < snr_min_db:
        reasons.append("V5_SNR_LOW")
    out["reasons"] = reasons
    out["valid"] = len(reasons) == 0
    if out["valid"]:
        out["t20_s"] = float(t20_est)
        out["rt60_equiv_s"] = float(T20_FACTOR * t20_est)
    return out


def complex_fr(ir, freqs, fs, pre_ms=1.5, win_ms=600.0):
    """Windowed-IR complex frequency response at requested frequencies (kit-compatible)."""
    x = np.asarray(ir, dtype=np.float64)
    peak = int(np.argmax(np.abs(x)))
    pre = max(1, int(round(pre_ms / 1000.0 * fs)))
    n = max(32, int(round(win_ms / 1000.0 * fs)))
    p0 = max(0, peak - pre)
    w = x[p0:p0 + n].astype(np.float64)
    if w.size < n:
        w = np.pad(w, (0, n - w.size))
    win = np.ones(n)
    rise = min(pre, n)
    win[:rise] = 0.5 - 0.5 * np.cos(math.pi * np.arange(rise) / rise)
    fade = max(1, int(0.15 * n))
    j = np.arange(fade - 1, -1, -1)
    win[n - fade:] = np.minimum(win[n - fade:], 0.5 - 0.5 * np.cos(math.pi * j / fade))
    nfft = max(1 << 15, next_pow2(n + 1))
    FR = np.fft.rfft(w * win, nfft)
    f = np.fft.rfftfreq(nfft, 1.0 / fs)
    out = np.empty(len(freqs), dtype=np.complex128)
    for i, fc in enumerate(freqs):
        lo, hi = fc * 2.0 ** (-0.5 / 24.0), fc * 2.0 ** (0.5 / 24.0)
        m = (f >= lo) & (f <= hi)
        out[i] = FR[m].mean() if m.any() else FR[int(np.argmin(np.abs(f - fc)))]
    return out


def log_grid(lo_hz, hi_hz, per_oct=DEFAULT_GRID_PER_OCT):
    n = int(round(per_oct * math.log2(hi_hz / lo_hz)))
    return lo_hz * 2.0 ** (np.arange(n + 1) / float(per_oct))


def intervention_index(identity_ir, on_ir, desired_ir, freqs, fs):
    """Spec sec 4.3 I = 10*log10(sum|b-d|^2 / sum|y-d|^2) over the requested grid."""
    b = complex_fr(identity_ir, freqs, fs)
    y = complex_fr(on_ir, freqs, fs)
    d = complex_fr(desired_ir, freqs, fs)
    e_off = float(np.sum(np.abs(b - d) ** 2))
    e_on = float(np.sum(np.abs(y - d) ** 2))
    numeric_floor = False
    if e_off <= 0.0:
        e_off = 1e-300
    if e_on <= 0.0:
        e_on = 1e-300
        numeric_floor = True
    I = 10.0 * math.log10(e_off / e_on)
    I = max(min(I, 60.0), -60.0)
    return {"I_db": float(I), "E_off": e_off, "E_on": e_on,
            "numeric_floor": bool(numeric_floor), "grid_points": int(len(freqs))}


def band_edges(fc):
    return fc * 2.0 ** (-1.0 / 6.0), fc * 2.0 ** (1.0 / 6.0)


# --------------------------------------------------------------------------- analysis core
def _default_cfg():
    return {"bands_hz": list(DEFAULT_BANDS), "r2_min": DEFAULT_R2_MIN,
            "t20_range_s": list(DEFAULT_T20_RANGE), "snr_min_db": DEFAULT_SNR_MIN_DB,
            "noise_margin_min_db": DEFAULT_NOISE_MARGIN_MIN_DB,
            "noise_floor_max_db": DEFAULT_NOISE_FLOOR_MAX_DB,
            "level_guard_db": DEFAULT_LEVEL_GUARD_DB, "grid_per_oct": DEFAULT_GRID_PER_OCT,
            "filter_method": "auto", "t20_definition": "T20 = -20/slope over EDC [-5,-25] dB "
                                                        "(20 dB decay time); RT60 = 3*T20",
            "observation_target_drop_pct": 15.0}


def _as_channel_matrix(x, what):
    a = np.asarray(x, dtype=np.float64)
    if a.ndim == 1:
        a = a[:, None]
    if a.ndim != 2:
        raise ValueError("%s: expected 1-D or 2-D array" % what)
    return [a[:, k] for k in range(a.shape[1])]


def analyze_pairs(identity_channels, on_channels, fs,
                  desired_channels=None, labels=None, cfg=None, provenance=None):
    """Core API: arrays in / result dict out. No file I/O."""
    cfg = dict(_default_cfg(), **(cfg or {}))
    id_ch = _as_channel_matrix(identity_channels, "identity")
    on_ch = _as_channel_matrix(on_channels, "on")
    if len(id_ch) != len(on_ch):
        raise ValueError("identity has %d channels but on has %d" % (len(id_ch), len(on_ch)))
    des_ch = None
    if desired_channels is not None:
        des_ch = _as_channel_matrix(desired_channels, "desired")
        if len(des_ch) != len(id_ch):
            raise ValueError("desired has %d channels but identity has %d" % (len(des_ch), len(id_ch)))
    if labels is not None and len(labels) != len(id_ch):
        raise ValueError("labels count %d != channel count %d" % (len(labels), len(id_ch)))
    names = list(labels) if labels else ["IN%d" % (k + 1) for k in range(len(id_ch))]
    bands = [float(b) for b in cfg["bands_hz"]]
    overall_lo, overall_hi = min(bands), max(bands)
    grid_all = log_grid(overall_lo, overall_hi, cfg["grid_per_oct"])
    rows = []
    per_input = []
    for k in range(len(id_ch)):
        ir_id = id_ch[k]
        ir_on = on_ch[k]
        flags = {"criterion": "REL_T20_DROP_PLUS_LEVEL_GUARD (no --desired)",
                 "I_available": False}
        overall_I = None
        if des_ch is not None:
            overall_I = intervention_index(ir_id, ir_on, des_ch[k], grid_all, fs)
            flags = {"criterion": "INTERVENTION_INDEX_I (desired response supplied)",
                     "I_available": True}
        for fc in bands:
            lo, hi = band_edges(fc)
            fm = cfg.get("filter_method", "auto")
            a = fit_band_t20(ir_id, fc, fs, cfg["r2_min"], tuple(cfg["t20_range_s"]),
                             cfg["snr_min_db"], cfg["noise_margin_min_db"],
                             cfg["noise_floor_max_db"], fm)
            b = fit_band_t20(ir_on, fc, fs, cfg["r2_min"], tuple(cfg["t20_range_s"]),
                             cfg["snr_min_db"], cfg["noise_margin_min_db"],
                             cfg["noise_floor_max_db"], fm)
            row = {"input": names[k], "channel": k, "fs": int(fs), "band_hz": float(fc),
                   "band_lo_hz": lo, "band_hi_hz": hi}
            row["t20_identity_s"] = a["t20_s"]
            row["t20_on_s"] = b["t20_s"]
            row["rt60_identity_s"] = a["rt60_equiv_s"]
            row["rt60_on_s"] = b["rt60_equiv_s"]
            row["t20_identity_est_s"] = a["t20_est_s"]
            row["t20_on_est_s"] = b["t20_est_s"]
            row["edc100_identity_db"] = a["edc100_db"]
            row["edc100_on_db"] = b["edc100_db"]
            row["band_level_identity_db"] = a["band_level_db"]
            row["band_level_on_db"] = b["band_level_db"]
            if a["band_level_db"] is not None and b["band_level_db"] is not None:
                row["band_level_delta_db"] = float(b["band_level_db"] - a["band_level_db"])
            else:
                row["band_level_delta_db"] = None
            row["r2_identity"] = a["r2"]
            row["r2_on"] = b["r2"]
            row["slope_identity_db_per_s"] = a["slope_db_per_s"]
            row["slope_on_db_per_s"] = b["slope_db_per_s"]
            row["t5_identity_s"], row["t25_identity_s"] = a["t5_s"], a["t25_s"]
            row["t5_on_s"], row["t25_on_s"] = b["t5_s"], b["t25_s"]
            row["noise_floor_identity_db"] = a["noise_floor_db"]
            row["noise_floor_on_db"] = b["noise_floor_db"]
            row["noise_margin_identity_db"] = a["noise_margin_db"]
            row["noise_margin_on_db"] = b["noise_margin_db"]
            row["snr_identity_db"] = a["snr_db"]
            row["snr_on_db"] = b["snr_db"]
            row["edc100_delta_db"] = (None if (a["edc100_db"] is None or b["edc100_db"] is None)
                                      else float(b["edc100_db"] - a["edc100_db"]))
            row["validity_identity"] = "VALID" if a["valid"] else "NA"
            row["validity_on"] = "VALID" if b["valid"] else "NA"
            row["invalid_reasons_identity"] = ";".join(a["reasons"])
            row["invalid_reasons_on"] = ";".join(b["reasons"])
            both_valid = bool(a["valid"] and b["valid"])
            drop = None
            if both_valid and a["t20_s"] and b["t20_s"]:
                drop = 100.0 * (a["t20_s"] - b["t20_s"]) / a["t20_s"]
            row["t20_drop_pct"] = drop
            row["rt60_drop_pct"] = drop
            row["observation_target_met"] = (None if drop is None
                                             else bool(drop >= cfg["observation_target_drop_pct"]))
            guard = None
            if row["band_level_delta_db"] is not None:
                guard = bool(abs(row["band_level_delta_db"]) <= cfg["level_guard_db"])
            row["level_guard_ok"] = guard
            row["criterion"] = flags["criterion"]
            if des_ch is not None:
                gmask = (grid_all >= lo) & (grid_all <= hi)
                grid_band = grid_all[gmask]
                if grid_band.size < 2:
                    row["I_band_db"] = None
                    row["I_note"] = "INSUFFICIENT_GRID_POINTS_IN_BAND"
                else:
                    ii = intervention_index(ir_id, ir_on, des_ch[k], grid_band, fs)
                    row["I_band_db"] = ii["I_db"]
                    row["I_E_off"] = ii["E_off"]
                    row["I_E_on"] = ii["E_on"]
                    row["I_numeric_floor"] = ii["numeric_floor"]
            else:
                row["I_band_db"] = None
                row["I_note"] = "NO_DESIRED_RESPONSE"
            if row["I_band_db"] is not None and row["t20_drop_pct"] is not None:
                row["observation_criteria_met"] = bool(
                    row["I_band_db"] >= 3.0
                    and row["t20_drop_pct"] >= cfg["observation_target_drop_pct"]
                    and row["level_guard_ok"] is True)
            else:
                row["observation_criteria_met"] = None
            if drop is None:
                row["notes"] = "T20_NOT_MEASURABLE_IN_BAND"
            elif row["observation_target_met"]:
                row["notes"] = "T20_DROP_OBSERVED_GE_TARGET"
            else:
                row["notes"] = "T20_DROP_BELOW_TARGET"
            rows.append(row)
        valid_drops = [r["t20_drop_pct"] for r in rows
                       if r["input"] == names[k] and r["t20_drop_pct"] is not None]
        valid_bands = [r["band_hz"] for r in rows
                       if r["input"] == names[k] and r["t20_drop_pct"] is not None]
        sumry = {"input": names[k], "channel": k,
                 "bands_total": len(bands), "bands_valid": len(valid_drops),
                 "bands_not_measurable": [r["band_hz"] for r in rows
                                          if r["input"] == names[k] and r["t20_drop_pct"] is None],
                 "t20_drop_pct_by_valid_band": dict(zip([str(x) for x in valid_bands], valid_drops)),
                 "median_t20_drop_pct_valid": (float(np.median(valid_drops)) if valid_drops else None),
                 "any_band_drop_ge_target": (None if not valid_drops
                                             else bool(max(valid_drops) >= cfg["observation_target_drop_pct"])),
                 "I_overall_63_250_db": (overall_I["I_db"] if overall_I else None)}
        per_input.append(sumry)
    return {"schema": SCHEMA, "tool_version": TOOL_VERSION,
            "scope": {"class": SCOPE_CLASS, "tags": list(SCOPE_TAGS), "note": SCOPE_NOTE,
                      "certification": "NONE", "verdict": "NONE"},
            "config": cfg, "fs": int(fs), "filter_method": resolve_filter(cfg.get("filter_method", "auto")),
            "rows": rows, "per_input": per_input,
            "provenance": provenance or {},
            "criterion_note": (
                "T20: Schroeder EDC, linear fit over -5..-25 dB, T20 = (t(-25)-t(-5)) = 20 dB "
                "decay time; RT60 = 3*T20 (spec/mimo-t6.md sec 4.5 labels the 3x value T20; this "
                "tool follows the measurement-chain convention and reports RT60 separately). "
                "Validity gates V1..V5 per spec/mimo-t6.md sec 4.5. "
                "I: spec sec 4.3 only when a desired response d is supplied; otherwise the "
                "declared fallback is relative T20 drop vs identity plus band level guard "
                "(|delta band level| <= %.1f dB), which is NOT the spec I."
                % cfg["level_guard_db"]),
            "observation_target": {"description": "T20 drop >= 15% vs identity is a pre-declared "
                                                  "engineering observation, not a verdict",
                                   "affects_verdict": False, "threshold_drop_pct": 15.0},
            "no_measurement_note": ("Offline analysis only. No sound card, board, apply or audio "
                                    "output was touched; these numbers are not a new acoustic "
                                    "measurement and not a certification.")}


# --------------------------------------------------------------------------- emit
def _fmt(v, nd=4):
    if v is None:
        return ""
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, float):
        if not math.isfinite(v):
            return ""
        return ("%%.%df" % nd) % v
    return str(v)


CSV_FIELDS = ["input", "band_hz", "fs", "t20_identity_s", "t20_on_s", "t20_drop_pct",
              "rt60_identity_s", "rt60_on_s",
              "observation_target_met", "edc100_identity_db", "edc100_on_db",
              "band_level_identity_db", "band_level_on_db", "band_level_delta_db",
              "r2_identity", "r2_on", "noise_floor_identity_db", "noise_floor_on_db",
              "noise_margin_identity_db", "noise_margin_on_db", "snr_identity_db", "snr_on_db",
              "validity_identity", "validity_on", "criterion", "I_band_db",
              "level_guard_ok", "notes"]


def emit(result, outdir, tag="t20_ab"):
    os.makedirs(outdir, exist_ok=True)
    created = _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="seconds")
    result = dict(result)
    result["created_utc"] = created
    json_path = os.path.join(outdir, tag + ".json")
    with open(json_path, "w", encoding="utf-8") as fh:
        json.dump(result, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    csv_path = os.path.join(outdir, tag + ".csv")
    with open(csv_path, "w", encoding="utf-8", newline="") as fh:
        fh.write(",".join(CSV_FIELDS) + "\n")
        for row in result["rows"]:
            fh.write(",".join(_fmt(row.get(f)) for f in CSV_FIELDS) + "\n")
    md_path = os.path.join(outdir, "T20-AB-TABLE.md")
    lines = []
    lines.append("# T20 A/B table -- PREDICTED_OFFLINE_ANALYSIS / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT")
    lines.append("")
    lines.append("> Offline analysis only. No sound card, board, apply or audio output was touched.")
    lines.append("> No verdict, no certification. T20 drop >= 15% is a pre-declared engineering observation.")
    lines.append("")
    lines.append("Schema: %s | tool mimo_t20_ab.py v%s | created %s" % (SCHEMA, TOOL_VERSION, created))
    lines.append("")
    lines.append("## Method")
    lines.append("")
    lines.append("- 1/3-oct brick-wall band IR; Schroeder EDC L(t)=10log10(int_t^inf h^2 / int_0^inf h^2);")
    lines.append("- T20 = (t(-25)-t(-5)) [s] (20 dB decay time); RT60 = 3*T20;")
    lines.append("- validity gates V1..V5 (spec/mimo-t6.md sec 4.5);")
    lines.append("- EDC100 = EDC level 100 ms after band-IR peak, relative to peak (0 dB);")
    lines.append("- I = 10log10(sum|b-d|^2 / sum|y-d|^2) only when a desired response d is supplied")
    lines.append("  (--desired); otherwise the declared fallback criterion 'relative T20 drop vs identity")
    lines.append("  + band level guard' is used and reported as such (it is not the spec I).")
    lines.append("")
    if result.get("provenance"):
        lines.append("## Provenance")
        lines.append("")
        lines.append("    " + json.dumps(result["provenance"], ensure_ascii=False, indent=2).replace("\n", "\n    "))
        lines.append("")
    lines.append("## Per (input, band)")
    lines.append("")
    lines.append("| input | band Hz | T20 id [s] | T20 on [s] | drop % | RT60 id [s] | RT60 on [s] | obs>=15% | EDC100 id [dB] | EDC100 on [dB] | level id [dB] | level on [dB] | dLevel [dB] | R2 id | R2 on | validity id/on | I [dB] | level guard |")
    lines.append("| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | ---: | --- |")
    for r in result["rows"]:
        lines.append("| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s/%s | %s | %s |" % (
            r["input"], _fmt(r["band_hz"], 0), _fmt(r["t20_identity_s"]), _fmt(r["t20_on_s"]),
            _fmt(r["t20_drop_pct"], 1), _fmt(r.get("rt60_identity_s")), _fmt(r.get("rt60_on_s")),
            _fmt(r["observation_target_met"]),
            _fmt(r["edc100_identity_db"], 2), _fmt(r["edc100_on_db"], 2),
            _fmt(r["band_level_identity_db"], 1), _fmt(r["band_level_on_db"], 1),
            _fmt(r["band_level_delta_db"], 2), _fmt(r["r2_identity"], 4), _fmt(r["r2_on"], 4),
            r["validity_identity"], r["validity_on"], _fmt(r.get("I_band_db"), 2),
            _fmt(r.get("level_guard_ok"))))
    lines.append("")
    lines.append("## Per input")
    lines.append("")
    lines.append("| input | bands valid / total | N/A bands | median drop % (valid) | max drop >= 15% | I 63-250 [dB] |")
    lines.append("| --- | ---: | --- | ---: | --- | ---: |")
    for s in result["per_input"]:
        lines.append("| %s | %d / %d | %s | %s | %s | %s |" % (
            s["input"], s["bands_valid"], s["bands_total"],
            ", ".join(_fmt(x, 0) for x in s["bands_not_measurable"]) or "-",
            _fmt(s["median_t20_drop_pct_valid"], 1), _fmt(s["any_band_drop_ge_target"]),
            _fmt(s["I_overall_63_250_db"], 2)))
    lines.append("")
    lines.append("## Caveats")
    lines.append("")
    lines.append("- SINGLE_POINT_NOT_SPATIAL: one microphone position only; no spatial/holdout claim.")
    lines.append("- NO_MEASUREMENT made by this tool; inputs must be same-session/same-stimulus/same-point raw RIRs.")
    lines.append("- A band marked NA must not be read as 'no change': it is NOT_MEASURABLE under V1..V5.")
    lines.append("- --desired absent => the I column stays empty; the fallback criterion is clearly labelled.")
    lines.append("")
    with open(md_path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines))
    return {"json": json_path, "csv": csv_path, "md": md_path}


def _read_inputs(identity_path, on_path, desired_path):
    id_x, id_meta = wav_read(identity_path)
    on_x, on_meta = wav_read(on_path)
    if id_meta["fs"] != on_meta["fs"]:
        raise SystemExit("identity fs %d != on fs %d (same session / same fs required)"
                         % (id_meta["fs"], on_meta["fs"]))
    if id_meta["channels"] != on_meta["channels"]:
        raise SystemExit("identity channels %d != on channels %d"
                         % (id_meta["channels"], on_meta["channels"]))
    des = None
    if desired_path:
        des_x, des_meta = wav_read(desired_path)
        if des_meta["fs"] != id_meta["fs"]:
            raise SystemExit("desired fs %d != identity fs %d" % (des_meta["fs"], id_meta["fs"]))
        if des_meta["channels"] != id_meta["channels"]:
            raise SystemExit("desired channels %d != identity channels %d"
                             % (des_meta["channels"], id_meta["channels"]))
        des = (des_x, des_meta)
    return (id_x, id_meta), (on_x, on_meta), des


def _provenance(meta_id, meta_on, meta_des, extra):
    prov = {"identity": meta_id, "on": meta_on}
    if meta_des:
        prov["desired"] = meta_des
    if extra:
        prov.update(extra)
    return prov


def _load_provenance(path):
    if not path:
        return {}
    with open(path, "r", encoding="utf-8") as fh:
        return json.load(fh)


# --------------------------------------------------------------------------- baseline (single arm)
def analyze_baseline(signals, fs, labels=None, cfg=None, provenance=None):
    """Single-arm T20 / EDC / DR table (no identity-vs-on pairing)."""
    cfg = dict(_default_cfg(), **(cfg or {}))
    ch = _as_channel_matrix(signals, "signals")
    if labels is not None and len(labels) != len(ch):
        raise ValueError("labels count %d != channel count %d" % (len(labels), len(ch)))
    names = list(labels) if labels else ["IN%d" % (k + 1) for k in range(len(ch))]
    bands = [float(b) for b in cfg["bands_hz"]]
    fm = cfg.get("filter_method", "auto")
    rows = []
    for k, ir in enumerate(ch):
        for fc in bands:
            m = fit_band_t20(ir, fc, fs, cfg["r2_min"], tuple(cfg["t20_range_s"]),
                             cfg["snr_min_db"], cfg["noise_margin_min_db"],
                             cfg["noise_floor_max_db"], fm)
            rows.append({"input": names[k], "channel": k, "fs": int(fs), "band_hz": float(fc),
                         "t20_s": m["t20_s"], "rt60_equiv_s": m["rt60_equiv_s"],
                         "edc100_db": m["edc100_db"], "band_level_db": m["band_level_db"],
                         "r2": m["r2"], "slope_db_per_s": m["slope_db_per_s"],
                         "residual_rms_db": m["residual_rms_db"],
                         "t5_s": m["t5_s"], "t25_s": m["t25_s"],
                         "noise_floor_db": m["noise_floor_db"],
                         "noise_margin_db": m["noise_margin_db"], "snr_db": m["snr_db"],
                         "validity": "VALID" if m["valid"] else "NA",
                         "invalid_reasons": ";".join(m["reasons"])})
    return {"schema": "khaen-mimo-t20-baseline/v1", "tool_version": TOOL_VERSION,
            "scope": {"class": SCOPE_CLASS, "tags": list(SCOPE_TAGS), "note": SCOPE_NOTE,
                      "certification": "NONE", "verdict": "NONE"},
            "config": cfg, "filter_method": resolve_filter(fm), "fs": int(fs),
            "rows": rows, "provenance": provenance or {},
            "criterion_note": ("T20 = -20/slope over EDC [-5,-25] dB (20 dB decay time); "
                               "RT60 = 3*T20. Validity gates V1..V5 per spec/mimo-t6.md sec 4.5."),
            "no_measurement_note": ("Offline analysis only. No sound card, board, apply or audio "
                                    "output was touched; not a new acoustic measurement.")}


BASELINE_CSV_FIELDS = ["input", "band_hz", "fs", "t20_s", "rt60_equiv_s", "edc100_db",
                       "band_level_db", "r2", "slope_db_per_s", "t5_s", "t25_s",
                       "noise_floor_db", "noise_margin_db", "snr_db", "validity",
                       "invalid_reasons"]


def emit_baseline(result, outdir, tag="t20_baseline"):
    os.makedirs(outdir, exist_ok=True)
    created = _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="seconds")
    result = dict(result)
    result["created_utc"] = created
    json_path = os.path.join(outdir, tag + ".json")
    with open(json_path, "w", encoding="utf-8") as fh:
        json.dump(result, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    csv_path = os.path.join(outdir, tag + ".csv")
    with open(csv_path, "w", encoding="utf-8", newline="") as fh:
        fh.write(",".join(BASELINE_CSV_FIELDS) + "\n")
        for row in result["rows"]:
            fh.write(",".join(_fmt(row.get(x)) for x in BASELINE_CSV_FIELDS) + "\n")
    md_path = os.path.join(outdir, "T20-BASELINE.md")
    lines = ["# Single-arm T20 baseline -- PREDICTED_OFFLINE_ANALYSIS / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT",
             "",
             "> Offline analysis of pre-existing RIR WAVs. No measurement session performed by this tool.",
             "> T20 = -20/slope over EDC [-5,-25] dB (20 dB decay time); RT60 = 3*T20.",
             "",
             "Schema: %s | tool mimo_t20_ab.py v%s | filter %s | created %s"
             % ("khaen-mimo-t20-baseline/v1", TOOL_VERSION, result["filter_method"], created),
             "",
             "| input | band Hz | T20 [s] | RT60=3*T20 [s] | EDC100 [dB] | level [dB] | R2 | slope [dB/s] | DR [dB] | validity | reasons |",
             "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |"]
    for r in result["rows"]:
        lines.append("| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |" % (
            r["input"], _fmt(r["band_hz"], 0), _fmt(r["t20_s"]), _fmt(r["rt60_equiv_s"]),
            _fmt(r["edc100_db"], 2), _fmt(r["band_level_db"], 1), _fmt(r["r2"], 4),
            _fmt(r["slope_db_per_s"], 1), _fmt(r["snr_db"], 1), r["validity"],
            r["invalid_reasons"]))
    lines.append("")
    if result.get("provenance"):
        lines.append("## Provenance")
        lines.append("")
        lines.append("    " + json.dumps(result["provenance"], ensure_ascii=False,
                                         indent=2).replace("\n", "\n    "))
        lines.append("")
    lines.append("## Caveats")
    lines.append("")
    lines.append("- SINGLE_POINT_NOT_SPATIAL: single position; no spatial/holdout claim.")
    lines.append("- NO_MEASUREMENT made by this tool; NA bands are NOT_MEASURABLE under V1..V5, not 'no change'.")
    lines.append("- Deconvolved IRs with small dynamic range will correctly fail V1/V5; use the raw-capture")
    lines.append("  band-decay path (acoustic-verify lf_edc.py) when DR is insufficient.")
    lines.append("")
    with open(md_path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines))
    return {"json": json_path, "csv": csv_path, "md": md_path}


# --------------------------------------------------------------------------- selftest
def synth_decay_rir(t60_s, fs, dur_s, band_centers, amp=1.0):
    """Synthetic RIR with a KNOWN decay: one damped sinusoid per 1/3-octave test band.

    A single damped tone at the band center has an analytically known Schroeder EDC slope
    in that band (tone energy decays as exp(-2*alpha*t)), so this validates the estimator
    without the EDC noise of a noise*envelope signal (which pushes r2 below 0.9) and
    without the band-edge attenuation of a tone placed at a band edge. Bands are generated
    one at a time in the selftest so 1/3-oct filter stop-band leakage from a neighbouring
    band cannot contaminate the known answer.
    """
    n = int(round(dur_s * fs))
    if n < 64:
        raise ValueError("synthetic RIR too short")
    t = np.arange(n) / float(fs)
    alpha = 6.907755278982137 / float(t60_s)          # ln(1000)/T60
    env = np.exp(-alpha * t)
    x = np.zeros(n)
    for fc in band_centers:
        x += np.sin(2.0 * math.pi * float(fc) * t)
    x = amp * x * env / math.sqrt(max(1, len(band_centers)))
    x = x / (np.max(np.abs(x)) + 1e-30)
    x = x + 1e-9 * np.random.default_rng(0).standard_normal(n)
    return x


def _selftest_fit(ir, fc, fs, cfg, method="auto"):
    return fit_band_t20(ir, fc, fs, cfg["r2_min"], tuple(cfg["t20_range_s"]),
                        cfg["snr_min_db"], cfg["noise_margin_min_db"],
                        cfg["noise_floor_max_db"], method)


def run_selftest(outdir, verbose=True):
    """Validate the T20 estimator against synthetic RIRs with a known decay.

    Case A (the requested spec pair): identity T60=0.40 s -> T20=0.1333 s,
    on T60=0.34 s -> T20=0.1133 s, so the T20 drop must be ~15%.
    Case B (same 15% ratio at the measured LF scale): T60 1.20/1.02 s -> T20 0.40/0.34 s,
    which is the resolution regime of the real identity LF sweep (63 Hz T20 ~0.556 s).

    Each band is generated and measured in isolation (one damped tone per band) so a
    neighbouring band's 1/3-oct stop-band leakage cannot contaminate the known answer.
    At 63 Hz a T20 as short as 0.133 s is below the 1/3-oct filter time resolution
    (filter ring ~20 ms vs a 133 ms 20 dB window), so Case A gates only 100/125/250 Hz and
    records 63 Hz as RESOLUTION_LIMITED; Case B gates all four bands.
    """
    fs = 48000
    dur = 2.0
    base = _default_cfg()
    os.makedirs(outdir, exist_ok=True)
    cases = [
        {"name": "A_specified_T60_0.40_vs_0.34",
         "t60_identity": 0.40, "t60_on": 0.34,
         "t20_identity_nominal": 0.40 / 3.0, "t20_on_nominal": 0.34 / 3.0,
         "gated_bands": [100.0, 125.0, 250.0],
         "resolution_limited_bands": [63.0],
         "note": "63 Hz T20=0.133 s is below 1/3-oct filter time resolution; recorded, not gated"},
        {"name": "B_realistic_LF_T60_1.20_vs_1.02",
         "t60_identity": 1.20, "t60_on": 1.02,
         "t20_identity_nominal": 0.40, "t20_on_nominal": 0.34,
         "gated_bands": [63.0, 100.0, 125.0, 250.0],
         "resolution_limited_bands": [],
         "note": "same 15% ratio at the measured identity LF scale (63 Hz T20 0.556 s)"},
    ]
    checks = []
    for case in cases:
        for fc in [float(b) for b in DEFAULT_BANDS]:
            id_ir = synth_decay_rir(case["t60_identity"], fs, dur, [fc])
            on_ir = synth_decay_rir(case["t60_on"], fs, dur, [fc])
            a = _selftest_fit(id_ir, fc, fs, base)
            b = _selftest_fit(on_ir, fc, fs, base)
            drop = (100.0 * (a["t20_s"] - b["t20_s"]) / a["t20_s"]
                    if (a["t20_s"] and b["t20_s"]) else None)
            gated = fc in case["gated_bands"]
            ok_drop = drop is not None and abs(drop - 15.0) <= 3.0
            ok_id = (a["t20_s"] is not None
                     and abs(a["t20_s"] - case["t20_identity_nominal"]) / case["t20_identity_nominal"] <= 0.05)
            ok_on = (b["t20_s"] is not None
                     and abs(b["t20_s"] - case["t20_on_nominal"]) / case["t20_on_nominal"] <= 0.05)
            checks.append({"case": case["name"], "band_hz": fc, "gated": bool(gated),
                           "resolution_limited": bool(fc in case["resolution_limited_bands"]),
                           "t20_identity_s": a["t20_s"], "t20_on_s": b["t20_s"],
                           "t20_identity_nominal_s": case["t20_identity_nominal"],
                           "t20_on_nominal_s": case["t20_on_nominal"],
                           "drop_pct": drop,
                           "drop_within_15pct_3pp": bool(ok_drop),
                           "t20_identity_within_5pct_nominal": bool(ok_id),
                           "t20_on_within_5pct_nominal": bool(ok_on),
                           "validity_identity": "VALID" if a["valid"] else "NA",
                           "validity_on": "VALID" if b["valid"] else "NA",
                           "reasons_identity": ";".join(a["reasons"]),
                           "reasons_on": ";".join(b["reasons"]),
                           "check_ok": bool((not gated) or (ok_drop and ok_id and ok_on))})
    # I path smoke: isolated 100 Hz pair, desired T60 = 0.90 s -> expected positive I
    fc_i = 100.0
    id_i = synth_decay_rir(1.20, fs, dur, [fc_i])
    on_i = synth_decay_rir(1.02, fs, dur, [fc_i])
    des_i = synth_decay_rir(0.90, fs, dur, [fc_i])
    res_i = analyze_pairs(id_i, on_i, fs, desired_channels=des_i,
                          cfg={"bands_hz": [float(b) for b in DEFAULT_BANDS]},
                          provenance={"dataset_class": "SYNTHETIC_SELFTEST_I_PATH"})
    i_row = next(r for r in res_i["rows"] if r["band_hz"] == fc_i)
    i_ok = bool(i_row.get("I_band_db") is not None and i_row["I_band_db"] > 0.0)
    summary = {"schema": SCHEMA, "kind": "selftest", "checks": checks,
               "I_path": {"band_hz": fc_i, "I_band_db": i_row.get("I_band_db"),
                          "positive": i_ok,
                          "note": "desired T60=0.90 s is closer to on (1.02 s) than to identity (1.20 s)"},
               "tolerance": {"drop_pct_expected": 15.0, "drop_pct_tol_pp": 3.0,
                             "t20_definition": "T20 = -20/slope over EDC [-5,-25] dB (20 dB decay time)",
                             "t20_tol_frac": 0.05,
                             "r2_min": base["r2_min"]},
               "t20_definition_note": ("T20 = 20 dB decay time (t(-25)-t(-5)); RT60 = 3*T20. "
                                       "spec/mimo-t6.md sec 4.5 uses the label T20 for the 3x value; "
                                       "this tool follows the measurement-chain convention and reports "
                                       "RT60 separately."),
               "scope": {"class": SCOPE_CLASS, "tags": list(SCOPE_TAGS), "certification": "NONE"}}
    summary["ok"] = bool(all(c["check_ok"] for c in checks) and i_ok)
    with open(os.path.join(outdir, "selftest_summary.json"), "w", encoding="utf-8") as fh:
        json.dump(summary, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    fields = ["case", "band_hz", "t20_identity_s", "t20_on_s", "drop_pct", "gated",
              "resolution_limited", "drop_within_15pct_3pp",
              "t20_identity_within_5pct_nominal", "t20_on_within_5pct_nominal",
              "validity_identity", "validity_on", "reasons_identity", "reasons_on", "check_ok"]
    with open(os.path.join(outdir, "selftest_cases.csv"), "w", encoding="utf-8", newline="") as fh:
        fh.write(",".join(fields) + "\n")
        for c in checks:
            fh.write(",".join(_fmt(c.get(x)) for x in fields) + "\n")
    md = ["# mimo_t20_ab.py selftest -- PREDICTED_OFFLINE_ANALYSIS / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT",
          "",
          "T20 = -20/slope over EDC [-5,-25] dB (20 dB decay time); RT60 = 3*T20.",
          "Tolerance: per-band T20 drop = 15% +-3 pp; absolute T20 within 5% of the nominal value.",
          "Case A = requested spec pair; Case B = same 15% ratio at the measured LF scale.",
          "",
          "| case | band Hz | T20 id [s] | T20 on [s] | drop % | gated | resolution limited | drop ok | T20 id ok | T20 on ok | check |",
          "| --- | ---: | ---: | ---: | ---: | --- | --- | --- | --- | --- | --- |"]
    for c in checks:
        md.append("| %s | %.0f | %s | %s | %s | %s | %s | %s | %s | %s | %s |" % (
            c["case"].split("_")[0], c["band_hz"],
            ("%.4f" % c["t20_identity_s"]) if c["t20_identity_s"] else "NA",
            ("%.4f" % c["t20_on_s"]) if c["t20_on_s"] else "NA",
            ("%.2f" % c["drop_pct"]) if c["drop_pct"] is not None else "NA",
            c["gated"], c["resolution_limited"], c["drop_within_15pct_3pp"],
            c["t20_identity_within_5pct_nominal"], c["t20_on_within_5pct_nominal"],
            c["check_ok"]))
    md += ["", "I-path smoke: band %.0f Hz I=%s dB, positive=%s" % (
        fc_i, _fmt(i_row.get("I_band_db"), 3), i_ok), "",
        "Overall selftest: %s" % ("ALL_CHECKS_WITHIN_TOLERANCE" if summary["ok"]
                                   else "OUT_OF_TOLERANCE"),
        "", "No measurement, no verdict, no certification.", ""]
    with open(os.path.join(outdir, "selftest.md"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(md))
    if verbose:
        print("selftest: ok=%s" % summary["ok"])
        for c in checks:
            print("  [%s] band %6.1f Hz: T20 id=%s on=%s drop=%s%% valid=%s/%s gated=%s check_ok=%s %s%s" % (
                c["case"].split("_")[0], c["band_hz"],
                ("%.4f" % c["t20_identity_s"]) if c["t20_identity_s"] else "NA",
                ("%.4f" % c["t20_on_s"]) if c["t20_on_s"] else "NA",
                ("%.2f" % c["drop_pct"]) if c["drop_pct"] is not None else "NA",
                c["validity_identity"], c["validity_on"], c["gated"], c["check_ok"],
                c["reasons_identity"], c["reasons_on"]))
        print("  I path: band %.0f Hz I=%s dB positive=%s" % (fc_i, _fmt(i_row.get("I_band_db"), 3), i_ok))
        print("  wrote %s" % os.path.join(outdir, "selftest_summary.json"))
    return 0 if summary["ok"] else 2


# --------------------------------------------------------------------------- CLI
def _baseline_inputs(paths, labels):
    sigs = []
    names = []
    metas = []
    fs = None
    for i, p in enumerate(paths):
        x, meta = wav_read(p)
        if fs is None:
            fs = meta["fs"]
        elif meta["fs"] != fs:
            raise SystemExit("baseline fs mismatch: %s has %d, expected %d" % (p, meta["fs"], fs))
        metas.append(meta)
        stem = os.path.splitext(os.path.basename(p))[0]
        base_label = labels[i] if labels and i < len(labels) else stem
        for c in range(meta["channels"]):
            sigs.append(x[:, c])
            names.append(base_label if meta["channels"] == 1 else "%s_ch%d" % (base_label, c + 1))
    return np.stack(sigs, axis=1), fs, names, metas


def main(argv=None):
    ap = argparse.ArgumentParser(
        description="Offline identity-vs-MIMO-on T20 A/B analyzer "
                    "(PREDICTED_OFFLINE_ANALYSIS / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT)")
    ap.add_argument("--identity", help="identity RIR WAV")
    ap.add_argument("--on", help="MIMO-on RIR WAV (same session/stimulus/point)")
    ap.add_argument("--desired", help="optional desired/target RIR WAV (enables spec I)")
    ap.add_argument("--out", default="t20-ab-out", help="output directory")
    ap.add_argument("--tag", default="t20_ab", help="output file stem")
    ap.add_argument("--bands", default="63,100,125,250",
                    help="comma-separated 1/3-oct band centers Hz (default 63,100,125,250)")
    ap.add_argument("--labels", default=None, help="comma-separated per-channel input labels")
    ap.add_argument("--filter", default="auto", choices=["auto", "butter3", "brickwall"],
                    help="1/3-oct band filter: butter3 (zero-phase, default with scipy) or FFT brick-wall")
    ap.add_argument("--baseline", default=None,
                    help="comma-separated RIR WAV list -> single-arm T20 baseline table (no A/B pairing)")
    ap.add_argument("--baseline-labels", default=None)
    ap.add_argument("--r2-min", type=float, default=DEFAULT_R2_MIN)
    ap.add_argument("--snr-min-db", type=float, default=DEFAULT_SNR_MIN_DB)
    ap.add_argument("--level-guard-db", type=float, default=DEFAULT_LEVEL_GUARD_DB)
    ap.add_argument("--grid-per-oct", type=int, default=DEFAULT_GRID_PER_OCT)
    ap.add_argument("--provenance-json", default=None,
                    help="optional JSON file merged into the output provenance block")
    ap.add_argument("--selftest", action="store_true", help="run synthetic self-test and exit")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv)

    if args.selftest:
        return run_selftest(args.out, verbose=not args.quiet)
    bands = [float(s) for s in args.bands.split(",") if s.strip()]
    if not bands:
        ap.error("--bands is empty")
    if args.baseline:
        paths = [s.strip() for s in args.baseline.split(",") if s.strip()]
        labels = ([s.strip() for s in args.baseline_labels.split(",")]
                  if args.baseline_labels else None)
        data, fs, names, metas = _baseline_inputs(paths, labels)
        cfg_b = {"bands_hz": bands, "r2_min": args.r2_min, "snr_min_db": args.snr_min_db,
                 "filter_method": args.filter, "grid_per_oct": args.grid_per_oct}
        res_b = analyze_baseline(data, fs, labels=names, cfg=cfg_b,
                                 provenance={"dataset_class": "BASELINE_RIR_WAV_LIST",
                                             "files": metas})
        outs = emit_baseline(res_b, args.out)
        if not args.quiet:
            print("scope: %s | tags: %s | filter: %s" % (
                SCOPE_CLASS, ",".join(SCOPE_TAGS), res_b["filter_method"]))
            for row in res_b["rows"]:
                print("  %-28s %6.1f Hz: T20=%s s RT60=%s s DR=%s dB validity=%s %s" % (
                    row["input"], row["band_hz"], _fmt(row["t20_s"]), _fmt(row["rt60_equiv_s"]),
                    _fmt(row["snr_db"], 1), row["validity"], row["invalid_reasons"]))
            for p in outs.values():
                print("wrote %s" % p)
            print("NO_MEASUREMENT: offline analysis only; no sound card / board / apply / audio "
                  "output touched; no certification.")
        return 0
    if not args.identity or not args.on:
        ap.error("--identity and --on are required (or use --selftest / --baseline)")
    (id_x, id_meta), (on_x, on_meta), des = _read_inputs(args.identity, args.on, args.desired)
    cfg = {"bands_hz": bands, "r2_min": args.r2_min, "snr_min_db": args.snr_min_db,
           "level_guard_db": args.level_guard_db, "grid_per_oct": args.grid_per_oct,
           "filter_method": args.filter}
    labels = [s.strip() for s in args.labels.split(",")] if args.labels else None
    prov_extra = _load_provenance(args.provenance_json)
    res = analyze_pairs(id_x, on_x, id_meta["fs"],
                        desired_channels=(des[0] if des else None),
                        labels=labels, cfg=cfg,
                        provenance=_provenance(id_meta, on_meta, (des[1] if des else None),
                                               prov_extra))
    paths = emit(res, args.out, tag=args.tag)
    if not args.quiet:
        print("scope: %s | tags: %s" % (SCOPE_CLASS, ",".join(SCOPE_TAGS)))
        print("fs=%d channels=%d bands=%s" % (id_meta["fs"], id_meta["channels"],
                                              ",".join("%g" % b for b in bands)))
        for row in res["rows"]:
            print("  %-6s %6.1f Hz: T20 id=%s on=%s drop=%s%% validity=%s/%s I=%s" % (
                row["input"], row["band_hz"],
                _fmt(row["t20_identity_s"]), _fmt(row["t20_on_s"]),
                _fmt(row["t20_drop_pct"], 1), row["validity_identity"], row["validity_on"],
                _fmt(row.get("I_band_db"), 2)))
        for p in paths.values():
            print("wrote %s" % p)
        print("NO_MEASUREMENT: offline analysis only; no sound card / board / apply / audio "
              "output touched; no certification.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
