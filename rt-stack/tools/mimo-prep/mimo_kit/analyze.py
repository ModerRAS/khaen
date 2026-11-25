#!/usr/bin/env python3
"""Fixed MIMO planrt before/after analyzer (numpy, PC side).

Fixes vs the 2026-11-19 kit planrt_analyze.py (sha256 7e7f8bcd...):
  1) schedule() is role-major (n_role rows) but estimate_H/compare indexed starts/H/irs by
     container slot (SUB1=7) -> IndexError. Fixed: rows by role index, container slots only
     for H/ir rows (SUB1 -> slot 7, slots 4/5/6 never written).
  2) roles/schedule/container slots now come from <arm>/meta/session.json (v2 role-major),
     not from hard-coded container assumptions.
  3) detect_offset was an unbounded circular cross-correlation (argmax over wrap-around lags)
     plus an arbitrary +-1 s clamp: route-a identity UMIK got raw_lag=-2,933,280 -> clamped
     -48000, g UMIK +45600 -> false "two-arm offset mismatch". Fixed estimator cross-
     correlates the capture envelope with the known sweep-activity envelope over a bounded
     +-max-lag linear (zero-padded) FFT; route-a both arms now give +48960 samples (1.020 s).
  4) marker contract: reads <arm>/meta/playback.json (and falls back to <arm>/playback.json),
     requires path/backend/level_db/spl_target_db/file_sha256 and gates A/B equality.
  5) RT60/CSD are noise-floor aware: noise power is estimated from the IR pre-arrival /
     tail; T20 is only reported when the decay is at least ~25 dB above the noise floor and
     0.05 <= RT60 <= 10 s, else null (old kit reported 52-77 s noise-tail values).
  6) mics with no signal (e.g. ARC digital silence -116 dBFS) are marked unavailable and
     excluded from spatial std; a 3-mic pilot verdict requires all three available.

Scope: pilot_3mic observation only; NOT T6/T67/T68/T69 acceptance.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys

import numpy as np

FS = 48000
S24 = 8388608.0
DEFAULT_ROLES = ["FL", "FR", "SL", "SR", "SUB1"]
DEFAULT_ROLE_SLOTS = {"FL": 0, "FR": 1, "SL": 2, "SR": 3, "SUB1": 7}
MIC_FILES = [("UMIK", "capture.raw", 2, 0), ("MA1", "MA1-mono.raw", 1, 0),
             ("ARC", "ARC-mono.raw", 1, 0)]
REQUIRED_MARKER_KEYS = ("path", "backend", "level_db", "spl_target_db", "file_sha256")
MAX_RT60_S = 5.0
MIC_MIN_SNR_DB = 6.0
MIC_MIN_PEAK_DBFS = -90.0


# ---------------------------------------------------------------- low level
def write_s24(path, data):
    x = np.asarray(data, dtype=np.float64)
    if x.ndim == 1:
        x = x[:, None]
    q = np.clip(np.round(x * S24), -S24, S24 - 1).astype(np.int64)
    q &= 0xFFFFFF
    out = np.empty((x.shape[0], x.shape[1], 3), dtype=np.uint8)
    out[..., 0] = q & 0xFF
    out[..., 1] = (q >> 8) & 0xFF
    out[..., 2] = (q >> 16) & 0xFF
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(out.tobytes())
    return x.shape


def read_s24(path, channels):
    chunk = channels * 3
    n = os.path.getsize(path) // chunk
    b = np.fromfile(path, dtype=np.uint8, count=n * chunk).reshape(n, chunk).astype(np.int32)
    v = b[:, 0::3] | (b[:, 1::3] << 8) | (b[:, 2::3] << 16)
    v = np.where(v & 0x800000, v - 0x1000000, v)
    return v.astype(np.float64) / S24


def sweep(seconds, level_db=-30.0, start=20.0, end=500.0, fs=FS):
    n = int(round(seconds * fs))
    k = math.log(end / start)
    amp = 10.0 ** (level_db / 20.0)
    t = np.arange(n) / fs
    phase = 2.0 * math.pi * start * (n / fs) / k * ((end / start) ** (t / (n / fs)) - 1.0)
    w = np.ones(n)
    fade = max(1, int(0.01 * fs))
    w[:fade] = 0.5 - 0.5 * np.cos(math.pi * np.arange(fade) / fade)
    j = np.arange(fade - 1, -1, -1)
    w[n - fade:] = np.minimum(w[n - fade:], 0.5 - 0.5 * np.cos(math.pi * j / fade))
    return amp * np.sin(phase) * w


def oct_grid(fmin=20.0, fmax=500.0, per_oct=24):
    n = int(round(per_oct * math.log2(fmax / fmin)))
    return fmin * 2.0 ** (np.arange(n + 1) / per_oct)


def bandpass(x, lo, hi, pad_s=1.0, fs=FS):
    x = np.asarray(x, dtype=np.float64)
    x = x - float(np.mean(x))
    n = 1 << int(math.ceil(math.log2(len(x) + int(pad_s * fs))))
    X = np.fft.rfft(x, n)
    f = np.fft.rfftfreq(n, 1.0 / fs)
    X[(f < lo) | (f > hi)] = 0.0
    return np.fft.irfft(X, n)[: len(x)]


def deconv(seg, ref, tail_s=0.6, reg=1e-3, fs=FS):
    n = 1 << int(math.ceil(math.log2(len(seg) + len(ref))))
    Y = np.fft.rfft(seg, n)
    X = np.fft.rfft(ref, n)
    P = np.abs(X) ** 2
    ir = np.fft.irfft(Y * np.conj(X) / (P + reg * P.max()), n)
    return ir[: int(len(ref) + tail_s * fs)]


def fr_from_ir(ir, freqs, pre_ms=1.5, win_ms=600.0, fs=FS):
    peak = int(np.argmax(np.abs(ir)))
    pre = max(1, int(pre_ms / 1000.0 * fs))
    n = int(win_ms / 1000.0 * fs)
    p0 = max(0, peak - pre)
    w = np.array(ir[p0:p0 + n], dtype=np.float64)
    if len(w) < n:
        w = np.pad(w, (0, n - len(w)))
    win = np.ones(n)
    rise = min(pre, n)
    win[:rise] = 0.5 - 0.5 * np.cos(math.pi * np.arange(rise) / rise)
    fade = max(1, int(0.15 * n))
    j = np.arange(fade - 1, -1, -1)
    win[n - fade:] = np.minimum(win[n - fade:], 0.5 - 0.5 * np.cos(math.pi * j / fade))
    FR = np.fft.rfft(w * win, 1 << 15)
    f = np.fft.rfftfreq(1 << 15, 1.0 / fs)
    out = np.empty(len(freqs), dtype=np.complex128)
    for i, fc in enumerate(freqs):
        lo, hi = fc * 2.0 ** (-0.5 / 48.0), fc * 2.0 ** (0.5 / 48.0)
        m = (f >= lo) & (f <= hi)
        out[i] = FR[m].mean() if m.any() else FR[int(np.argmin(np.abs(f - fc)))]
    return out


def envelope(x, decim=480):
    n = len(x) // decim * decim
    return np.sqrt(np.mean(x[:n].reshape(-1, decim) ** 2, axis=1))


# ---------------------------------------------------------------- schedule
def schedule(n_roles, sweep_seconds, repeats, pre, repeat_gap, slot_gap, fs=FS):
    sweep_n = int(round(sweep_seconds * fs))
    interval = sweep_n + int(round(repeat_gap * fs))
    slot = repeats * interval + int(round(slot_gap * fs))
    starts = np.zeros((n_roles, repeats), dtype=np.int64)
    for oi in range(n_roles):
        for r in range(repeats):
            starts[oi, r] = int(round(pre * fs)) + oi * slot + r * interval
    total = int(round(pre * fs)) + n_roles * slot + int(round(1.5 * fs)) + fs * 2
    return starts, total


def load_session_meta(arm_dir):
    p = os.path.join(arm_dir, "meta", "session.json")
    if not os.path.isfile(p):
        return None
    try:
        meta = json.load(open(p, "r", encoding="utf-8"))
    except ValueError:
        return None
    if isinstance(meta.get("starts"), list) and meta["starts"] and isinstance(meta["starts"][0], dict):
        roles = list(meta.get("roles") or (meta.get("role_slots") or DEFAULT_ROLE_SLOTS).keys())
        repeats = int((meta.get("schedule") or {}).get("repeats", 3))
        rows = []
        for role in roles:
            row = sorted(int(s["start_frame"]) for s in meta["starts"] if s.get("role") == role)
            if len(row) != repeats:
                return None
            rows.append(row)
        meta = dict(meta)
        meta["starts"] = rows
        meta.setdefault("roles", roles)
    if not (isinstance(meta.get("starts"), list) and meta["starts"] and
            isinstance(meta["starts"][0], list)):
        sched = meta.get("schedule") or {}
        if isinstance(sched.get("starts"), list):
            meta = dict(meta)
            meta["starts"] = sched["starts"]
            meta.setdefault("roles", list((meta.get("role_slots") or DEFAULT_ROLE_SLOTS).keys()))
        else:
            return None
    roles = list(meta.get("roles") or list((meta.get("role_slots") or DEFAULT_ROLE_SLOTS).keys()))
    role_slots = {str(k): int(v) for k, v in (meta.get("role_slots") or DEFAULT_ROLE_SLOTS).items()}
    sched = meta.get("schedule") or {}
    out = {
        "source": p,
        "roles": roles,
        "role_slots": role_slots,
        "starts": np.array(meta["starts"], dtype=np.int64),
        "sweep_seconds": float(sched.get("sweep_seconds", meta.get("sweep_seconds", 10.0))),
        "repeats": int(sched.get("repeats", len(meta["starts"][0]))),
        "pre": float(sched.get("pre", 1.5)),
        "repeat_gap": float(sched.get("repeat_gap", 1.0)),
        "slot_gap": float(sched.get("slot_gap", 1.0)),
        "total_frames": int(meta.get("total_frames", meta["starts"][-1][-1] if meta["starts"] else 0)),
    }
    return out


def resolve_schedule(arm_dir, params):
    meta = load_session_meta(arm_dir)
    if meta is not None:
        return meta
    roles = list(DEFAULT_ROLES)
    starts, total = schedule(len(roles), params["sweep_seconds"], params["repeats"],
                             params["pre"], params["repeat_gap"], params["slot_gap"])
    return {"source": None, "roles": roles, "role_slots": dict(DEFAULT_ROLE_SLOTS),
            "starts": starts, "sweep_seconds": float(params["sweep_seconds"]),
            "repeats": int(params["repeats"]), "pre": float(params["pre"]),
            "repeat_gap": float(params["repeat_gap"]), "slot_gap": float(params["slot_gap"]),
            "total_frames": int(total)}


# ---------------------------------------------------------------- offset (fixed)
def activity_envelope(starts, sweep_n, n_frames, decim=480):
    n_blocks = int(np.ceil(n_frames / decim)) + 1
    act = np.zeros(n_blocks)
    for row in np.asarray(starts):
        for s0 in row:
            a = int(s0) // decim
            b = int(min(n_frames, s0 + sweep_n)) // decim
            if b > a:
                act[a:b] = 1.0
    if len(act) > 41:
        win = np.hanning(41)
        win /= win.sum()
        act = np.convolve(act, win, mode="same")
    return act


def detect_offset(capture, starts, sweep_n, max_lag_s=10.0, fs=FS, decim=480):
    """Bounded linear (no circular wrap) envelope cross-correlation.

    Returns (offset_samples, quality) where capture[n] ~ schedule[n - offset].
    """
    e = envelope(capture, decim)
    act = activity_envelope(starts, sweep_n, len(capture), decim)
    n = min(len(e), len(act))
    e = e[:n].astype(np.float64)
    act = act[:n].astype(np.float64)
    e -= e.mean()
    act -= act.mean()
    max_lag = int(round(max_lag_s * fs / decim))
    m = 1 << int(np.ceil(np.log2(len(e) + len(act) + 2 * max_lag + 2)))
    c = np.fft.irfft(np.fft.rfft(e, m) * np.conj(np.fft.rfft(act, m)), m)
    lags = np.arange(-max_lag, max_lag + 1)
    vals = c[lags % m]
    best = int(np.argmax(vals))
    k = int(lags[best])
    denom = math.sqrt(float(np.sum(e * e)) * float(np.sum(act * act)))
    quality = float(vals[best] / denom) if denom > 0 else 0.0
    return k * decim, quality


# ---------------------------------------------------------------- arm analysis
def load_mics(arm_dir):
    out = {}
    for name, rel, ch, sel in MIC_FILES:
        p = os.path.join(arm_dir, rel)
        if os.path.isfile(p):
            out[name] = read_s24(p, ch)[:, sel]
            continue
        c2 = os.path.join(arm_dir, "capture2.raw")
        if os.path.isfile(c2):
            data = read_s24(c2, 20)
            out[name] = data[:, 0] if name == "MA1" else data[:, 1]
    return out


def mic_stats(y, fs=FS):
    y = y.astype(np.float64)
    peak = float(20 * np.log10(np.max(np.abs(y)) + 1e-30))
    rms = float(20 * np.log10(np.sqrt(np.mean(y * y)) + 1e-30))
    qn = min(len(y), int(1.2 * fs))
    quiet = y[int(0.2 * fs):qn] if qn > int(0.3 * fs) else y[:qn]
    quiet_rms = float(20 * np.log10(np.sqrt(np.mean(quiet * quiet)) + 1e-30))
    snr = rms - quiet_rms
    available = bool(peak > MIC_MIN_PEAK_DBFS and snr >= MIC_MIN_SNR_DB)
    return {"frames": int(len(y)), "peak_dbfs": peak, "rms_dbfs": rms,
            "quiet_rms_dbfs": quiet_rms, "snr_db": snr, "available": available}


def estimate_noise_ir_power(y, offset_samples, sw, fs=FS):
    """IR-domain noise power from a quiet capture segment (before playback starts).

    The deconvolution inverse filter amplifies capture noise; propagating a real quiet
    segment through the same deconvolution gives a principled IR noise floor that does
    not depend on where the IR peak sits. Used as a lower bound by rt60_nw/csd_t20_nw.
    """
    a = max(0, int(offset_samples) - int(1.0 * fs))
    b = max(a + fs // 4, int(offset_samples) - int(0.05 * fs))
    seg = y[a:b]
    if len(seg) < fs // 8:
        seg = y[: max(fs // 8, int(0.2 * fs))]
    try:
        ir = deconv(np.asarray(seg, dtype=np.float64), sw, tail_s=0.0)
    except Exception:
        return None
    return float(np.mean(ir * ir))


def estimate_H(arm_dir, params, freqs, offset_override=None):
    mics = load_mics(arm_dir)
    if not mics:
        raise IOError("no mic data in %s" % arm_dir)
    sched = resolve_schedule(arm_dir, params)
    sweep_n = int(round(sched["sweep_seconds"] * FS))
    sw = sweep(sched["sweep_seconds"], params.get("level_db", -30.0))
    n_slots = max(sched["role_slots"].values()) + 1
    result = {"freqs": freqs, "H": {}, "irs": {}, "offset_samples": {},
              "offset_quality": {}, "noise_ir_power": {}, "mic_stats": {}, "schedule": {
                  "source": sched["source"], "roles": sched["roles"],
                  "role_slots": sched["role_slots"], "sweep_seconds": sched["sweep_seconds"],
                  "repeats": sched["repeats"], "total_frames": sched["total_frames"]}}
    for name, y in mics.items():
        stats = mic_stats(y)
        result["mic_stats"][name] = stats
        if offset_override is not None and name in offset_override:
            offset, quality = int(offset_override[name]), float("nan")
        else:
            offset, quality = detect_offset(y, sched["starts"], sweep_n)
        result["offset_samples"][name] = int(offset)
        result["offset_quality"][name] = float(quality)
        result["noise_ir_power"][name] = estimate_noise_ir_power(y, int(offset), sw)
        H = np.zeros((n_slots, freqs.size), dtype=np.complex128)
        ir_store = {}
        for role_i, role in enumerate(sched["roles"]):
            oi = int(sched["role_slots"][role])
            frs = []
            for r in range(sched["repeats"]):
                s0 = int(sched["starts"][role_i, r])
                onset = int(offset + s0)
                raw = y[max(0, onset - 32): onset + len(sw) + int(0.6 * FS)]
                ir = deconv(raw, sw)
                frs.append(fr_from_ir(ir, freqs))
                if r == 0:
                    ir_store[oi] = ir
            H[oi] = np.stack(frs, axis=0).mean(axis=0)
        result["H"][name] = H
        result["irs"][name] = ir_store
    return result


def _band_noise_power(q_ir, fc, fs=FS):
    """Scale a broadband IR-domain noise power to a 1/6-oct band (triangular bandwidth)."""
    bw = fc * (2 ** (1.0 / 6) - 2 ** (-1.0 / 6))
    return max(float(q_ir) * (2.0 * bw / fs), 1e-30)


def rt60_nw(ir, fc, fs=FS, max_rt_s=MAX_RT60_S, min_dynamic_db=25.0, q_ir=None):
    """Noise-floor-aware Schroeder T20 -> RT60. Returns None when not measurable."""
    if ir is None or len(ir) < 64:
        return None
    b = bandpass(ir, fc / 2 ** (1.0 / 6), fc * 2 ** (1.0 / 6))
    p = b * b
    n = len(p)
    peak = int(np.argmax(p))
    if peak >= n - 16:
        return None
    tail_n = max(16, int(0.2 * fs))
    q_tail = float(np.median(p[-tail_n:]))
    pre0 = peak - int(0.5 * fs)
    pre1 = peak - int(0.1 * fs)
    if pre0 >= 0 and pre1 - pre0 >= 16:   # only use a genuine pre-arrival window
        q_pre = float(np.median(p[pre0:pre1]))
    else:
        q_pre = 0.0
    q = max(q_tail, q_pre, 1e-30)
    if q_ir is not None:
        q = max(q, _band_noise_power(q_ir, fc, fs))
    e = np.cumsum(p[::-1])[::-1]
    e_noise = q * (n - np.arange(n))
    if e[peak] <= 0:
        return None
    dyn = 10.0 * math.log10(e[peak] / max(e_noise[peak], 1e-300))
    if dyn < min_dynamic_db:
        return None
    ec = np.maximum(e - e_noise, 1e-30)
    edc = 10.0 * np.log10(ec / ec[peak])
    edc = edc[peak:]
    t = np.arange(len(edc)) / fs
    i5 = int(np.argmax(edc <= -5.0))
    i25 = int(np.argmax(edc <= -25.0))
    if edc[i5] > -5.0 or edc[i25] > -25.0 or i25 <= i5 + 4:
        return None
    seg = edc[i5:i25 + 1]
    tseg = t[i5:i25 + 1]
    slope = float(np.polyfit(tseg, seg, 1)[0])
    if slope >= 0:
        return None
    rt = -60.0 / slope
    if rt < 0.05 or rt > max_rt_s:
        return None
    return float(rt)


def csd_t20_nw(ir, fc, fs=FS, q_ir=None):
    """Noise-floor-aware CSD T20 (time for band envelope to fall -20 dB); None if not reached."""
    if ir is None or len(ir) < 64:
        return None
    b = bandpass(ir, fc / 2 ** (1.0 / 6), fc * 2 ** (1.0 / 6))
    env = np.abs(b)
    n = len(env)
    peak = int(np.argmax(env))
    e = env[peak:]
    if e.size == 0 or e[0] <= 0:
        return None
    tail_n = max(16, int(0.2 * fs))
    q = float(np.median(env[-tail_n:]))
    if q_ir is not None:
        q = max(q, math.sqrt(_band_noise_power(q_ir, fc, fs)))
    e_corr = np.maximum(e * e - q * q, 1e-30)
    e_db = 10.0 * np.log10(e_corr / e_corr[0])
    hit = int(np.argmax(e_db <= -20.0))
    if e_db[hit] > -20.0:
        return None
    return float(hit / fs)


# ---------------------------------------------------------------- markers/gates
def load_marker(arm_dir):
    for rel in (os.path.join("meta", "playback.json"), "playback.json"):
        p = os.path.join(arm_dir, rel)
        if os.path.isfile(p):
            try:
                return json.load(open(p, "r", encoding="utf-8"))
            except ValueError:
                return None
    return None


def marker_gate(mb, ma):
    reasons = []
    if not mb or not ma:
        return {"ok": False, "reasons": ["marker_missing"], "before": mb, "after": ma}
    for side, m in (("before", mb), ("after", ma)):
        missing = [k for k in REQUIRED_MARKER_KEYS if k not in m]
        if missing:
            reasons.append("marker_%s_missing_keys:%s" % (side, ",".join(missing)))
    if not reasons:
        for k in REQUIRED_MARKER_KEYS:
            if mb[k] != ma[k]:
                reasons.append("marker_mismatch:%s" % k)
    return {"ok": not reasons, "reasons": reasons,
            "required_keys": list(REQUIRED_MARKER_KEYS),
            "before": {k: mb.get(k) for k in REQUIRED_MARKER_KEYS},
            "after": {k: ma.get(k) for k in REQUIRED_MARKER_KEYS}}


def consistency_gate(before, after, tol_samples=2400):
    """Same-path gate: equal detected offsets per mic across arms (+-tol samples)."""
    reasons = []
    common = sorted(set(before["offsets"]) & set(after["offsets"]))
    if not common:
        reasons.append("no_common_mics")
    deltas = {}
    for mic in common:
        d = int(before["offsets"][mic] - after["offsets"][mic])
        deltas[mic] = d
        if abs(d) > tol_samples:
            reasons.append("offset_mismatch:%s:%d_samples" % (mic, d))
    return {"ok": not reasons, "reasons": reasons, "tolerance_samples": tol_samples,
            "offset_delta_samples": deltas}


# ---------------------------------------------------------------- analyze / compare
def analyze_arm(arm_dir, params, label):
    freqs = oct_grid()
    est = estimate_H(arm_dir, params, freqs)
    bands = [40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0]
    available = [m for m in est["H"] if est["mic_stats"][m]["available"]]
    out = {"label": label, "arm_dir": os.path.abspath(arm_dir), "freqs": freqs.tolist(),
           "mics": {m: est["mic_stats"][m] for m in est["H"]},
           "mics_available": available,
           "offsets": est["offset_samples"], "offset_quality": est["offset_quality"],
           "schedule": est["schedule"], "marker": load_marker(arm_dir),
           "per_input": {}}
    for role in est["schedule"]["roles"]:
        oi = int(est["schedule"]["role_slots"][role])
        row = {"container_slot": oi, "fr_db": {}, "fr_db_norm": {}, "rt60_s": {},
               "csd_t20_ms": {}, "spatial_std_db": {}, "rt60_measured": False}
        for mname in est["H"]:
            fr = 20 * np.log10(np.abs(est["H"][mname][oi]) + 1e-30)
            row["fr_db"][mname] = [round(float(v), 3) for v in fr]
            ref = float(np.median(fr[(freqs >= 80) & (freqs <= 300)]))
            row["fr_db_norm"][mname] = [round(float(v - ref), 3) for v in fr]
            ir = est["irs"][mname].get(oi)
            q_ir = est.get("noise_ir_power", {}).get(mname)
            rt = {str(fc): rt60_nw(ir, fc, q_ir=q_ir) for fc in (63.0, 125.0, 250.0)}
            csd = {str(fc): csd_t20_nw(ir, fc, q_ir=q_ir) for fc in (63.0, 125.0, 160.0, 200.0)}
            row["rt60_s"][mname] = {k: (round(v, 3) if v is not None else None) for k, v in rt.items()}
            row["csd_t20_ms"][mname] = {k: (round(v * 1000.0, 1) if v is not None else None)
                                        for k, v in csd.items()}
            if any(v is not None for v in rt.values()):
                row["rt60_measured"] = True
            row["spatial_std_db"][mname] = round(
                float(np.std(fr[(freqs >= 40) & (freqs <= 200)] - ref)), 3)
        # spatial std across mics (only available mics)
        band = (freqs >= 40) & (freqs <= 200)
        av = [m for m in available if m in row["fr_db_norm"]]
        if len(av) >= 2:
            mat = np.array([row["fr_db_norm"][m] for m in av])
            row["spatial_std_3mic_db"] = round(float(np.mean(np.std(mat, axis=0))), 3)
            row["spatial_std_n_mics"] = len(av)
            row["spatial_std_scope"] = "3mic" if len(av) == 3 else ("%dmic" % len(av))
        else:
            row["spatial_std_3mic_db"] = None
            row["spatial_std_n_mics"] = len(av)
            row["spatial_std_scope"] = None
        row["spatial_std_mean_db_40_200"] = row["spatial_std_3mic_db"]
        # per-band mean over available mics
        row["band_mean_db"] = {}
        for fc in bands:
            bm = (freqs >= fc / 2 ** (1.0 / 6)) & (freqs <= fc * 2 ** (1.0 / 6))
            vals = [float(np.mean(np.array(row["fr_db_norm"][m])[bm])) for m in av]
            row["band_mean_db"][str(fc)] = vals
        rts = [v for m in row["rt60_s"].values() for v in m.values() if v is not None]
        row["rt60_median_s"] = round(float(np.median(rts)), 3) if rts else None
        out["per_input"][role] = row
    return out


def compare(before, after, outdir, limits=None, require_mics=3):
    os.makedirs(outdir, exist_ok=True)
    limits = limits or {"offset_tol_samples": 2400, "rt60_max_s": MAX_RT60_S}
    gates = {}
    gates["marker"] = marker_gate(before["marker"], after["marker"])
    gates["path_consistency"] = consistency_gate(before, after,
                                                 tol_samples=limits["offset_tol_samples"])
    reasons = list(gates["marker"]["reasons"]) + list(gates["path_consistency"]["reasons"])
    # mic availability gate (pilot_3mic requires all 3 in both arms)
    for label, arm in (("before", before), ("after", after)):
        miss = [m for m in ("UMIK", "MA1", "ARC") if m not in arm["mics_available"]]
        if miss:
            reasons.append("mic_unavailable_%s:%s" % (label, ",".join(miss)))
    mics_available = sorted(set(before["mics_available"]) & set(after["mics_available"]))
    if require_mics and len(mics_available) < require_mics:
        reasons.append("insufficient_mics:%d<%d" % (len(mics_available), require_mics))
    rows = []
    for role in before["per_input"]:
        b = before["per_input"][role]
        a = after["per_input"].get(role)
        if a is None:
            continue
        rows.append({
            "input": role,
            "container_slot": b["container_slot"],
            "spatial_std_before_db": b.get("spatial_std_3mic_db"),
            "spatial_std_after_db": a.get("spatial_std_3mic_db"),
            "spatial_std_improvement_db": (round(b["spatial_std_3mic_db"] - a["spatial_std_3mic_db"], 3)
                                           if (b.get("spatial_std_3mic_db") is not None and
                                               a.get("spatial_std_3mic_db") is not None) else None),
            "rt60_before_s": b.get("rt60_median_s"),
            "rt60_after_s": a.get("rt60_median_s"),
            "rt60_change_pct": (round(100.0 * (a["rt60_median_s"] - b["rt60_median_s"]) /
                                      b["rt60_median_s"], 2)
                                if (b.get("rt60_median_s") and a.get("rt60_median_s")) else None)})
    if not rows:
        reasons.append("no_participants")
    # RT60 is a secondary metric: report its coverage honestly instead of failing the whole
    # comparison when one participant/band is below the noise floor (63 Hz is usually null).
    rt60_cov_before = sum(1 for r in rows if r["rt60_before_s"] is not None)
    rt60_cov_after = sum(1 for r in rows if r["rt60_after_s"] is not None)
    rt60_coverage = {"before": "%d/%d" % (rt60_cov_before, len(rows)),
                     "after": "%d/%d" % (rt60_cov_after, len(rows))}
    if not reasons and rt60_cov_before == 0 and rt60_cov_after == 0:
        reasons.append("rt60_not_measured_at_all")
    summary = {
        "schema": "tspi-mimo-pilot-before-after/v2",
        "kit": "mimo_kit/1.0.0",
        "valid": not reasons,
        "invalid_reasons": reasons,
        "gates": gates,
        "mic_availability": {"before": before["mics_available"], "after": after["mics_available"],
                             "common": mics_available},
        "offsets": {"before": before["offsets"], "after": after["offsets"],
                    "offset_quality": {"before": before["offset_quality"],
                                       "after": after["offset_quality"]}},
        "schedule": {"before": before["schedule"], "after": after["schedule"]},
        "rows": rows,
        "rt60_coverage": rt60_coverage,
        "rt60_note": "RT60 reported only where the decay is >=25 dB above the noise floor "
                     "(63 Hz typically null at this level); null != pass/fail",
        "same_path_ab": bool(gates["marker"]["ok"] and gates["path_consistency"]["ok"]),
        "mimo_participants": list(before["per_input"].keys()),
        "height_channels_excluded": True,
        "mean_spatial_std_improvement_db": (round(float(np.mean([r["spatial_std_improvement_db"] for r in rows
                                                                 if r["spatial_std_improvement_db"] is not None])), 3)
                                            if any(r["spatial_std_improvement_db"] is not None for r in rows) else None),
        "mean_rt60_change_pct": (round(float(np.mean([r["rt60_change_pct"] for r in rows
                                                      if r["rt60_change_pct"] is not None])), 2)
                                 if any(r["rt60_change_pct"] is not None for r in rows) else None),
        "claim": "pilot_3mic observation only; NOT T6/T67/T68/T69 acceptance",
        "scope_note": "3 fixed mics (UMIK listening position, MA1 front, ARC TV cabinet); no holdout" if not reasons else
                      "INVALID: no before/after FR/RT60/CSD claim (see invalid_reasons)",
    }
    with open(os.path.join(outdir, "before-after.json"), "w", encoding="utf-8") as fh:
        json.dump(summary, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    with open(os.path.join(outdir, "before-after-full.json"), "w", encoding="utf-8") as fh:
        json.dump({"summary": summary, "before": before, "after": after}, fh,
                  ensure_ascii=False, indent=2)
        fh.write("\n")
    with open(os.path.join(outdir, "before-after.csv"), "w", encoding="utf-8") as fh:
        fh.write("input,container_slot,spatial_std_before_db,spatial_std_after_db,"
                 "spatial_std_improvement_db,rt60_before_s,rt60_after_s,rt60_change_pct\n")
        for r in rows:
            fh.write("%s,%d,%s,%s,%s,%s,%s,%s\n" % (
                r["input"], r["container_slot"],
                "" if r["spatial_std_before_db"] is None else "%.3f" % r["spatial_std_before_db"],
                "" if r["spatial_std_after_db"] is None else "%.3f" % r["spatial_std_after_db"],
                "" if r["spatial_std_improvement_db"] is None else "%.3f" % r["spatial_std_improvement_db"],
                "" if r["rt60_before_s"] is None else "%.3f" % r["rt60_before_s"],
                "" if r["rt60_after_s"] is None else "%.3f" % r["rt60_after_s"],
                "" if r["rt60_change_pct"] is None else "%.2f" % r["rt60_change_pct"]))
    md = ["# MIMO pilot before/after (pilot_3mic, fixed kit)", "",
          "**valid=%s**%s" % (str(not reasons).lower(),
                              "" if not reasons else " -- INVALID: " + "; ".join(reasons)), "",
          "same-path A/B: identity plan (before) vs G-4096 plan (after); MIMO participants "
          "FL/FR/SL/SR/SUB1 only; height channels excluded.", "",
          "| input | slot | spatial std 40-200 Hz before/after (dB) | delta dB | RT60 before/after (s) |",
          "|---|---|---|---|---|"]
    for r in rows:
        md.append("| %s | %d | %s -> %s | %s | %s -> %s |" % (
            r["input"], r["container_slot"],
            "n/a" if r["spatial_std_before_db"] is None else "%.2f" % r["spatial_std_before_db"],
            "n/a" if r["spatial_std_after_db"] is None else "%.2f" % r["spatial_std_after_db"],
            "n/a" if r["spatial_std_improvement_db"] is None else "%+.2f" % r["spatial_std_improvement_db"],
            "n/a" if r["rt60_before_s"] is None else "%.2f" % r["rt60_before_s"],
            "n/a" if r["rt60_after_s"] is None else "%.2f" % r["rt60_after_s"]))
    md += ["", "offsets: before %s / after %s (samples)" % (before["offsets"], after["offsets"]),
           "mics available: before %s / after %s" % (before["mics_available"], after["mics_available"]),
           "", "Observational pilot only: 3 fixed mics, no holdout; not T6 acceptance."]
    if gates["marker"]["reasons"] or gates["path_consistency"]["reasons"]:
        md += ["", "gate failures: " + "; ".join(gates["marker"]["reasons"] +
                                                 gates["path_consistency"]["reasons"])]
    with open(os.path.join(outdir, "BEFORE-AFTER.md"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(md) + "\n")
    with open(os.path.join(outdir, "verdict.json"), "w", encoding="utf-8") as fh:
        json.dump({"schema": "tspi-mimo-pilot-verdict/v2", "valid": summary["valid"],
                   "invalid_reasons": reasons,
                   "gates": {"marker_ok": gates["marker"]["ok"],
                             "path_consistency_ok": gates["path_consistency"]["ok"],
                             "mics_required": require_mics,
                             "mics_common": mics_available},
                   "offsets": summary["offsets"],
                   "scope": summary["claim"]}, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    return summary


def main(argv=None):
    ap = argparse.ArgumentParser(description="fixed MIMO pilot before/after analyzer (mimo_kit)")
    ap.add_argument("--before", required=True)
    ap.add_argument("--after", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--sweep-seconds", type=float, default=None)
    ap.add_argument("--repeats", type=int, default=None)
    ap.add_argument("--pre", type=float, default=None)
    ap.add_argument("--repeat-gap", type=float, default=None)
    ap.add_argument("--slot-gap", type=float, default=None)
    ap.add_argument("--level-db", type=float, default=-30.0)
    ap.add_argument("--require-mics", type=int, default=3)
    ap.add_argument("--offset-tol-samples", type=int, default=2400)
    args = ap.parse_args(argv)
    # session meta (if present per arm) wins; CLI is only a fallback
    meta = load_session_meta(args.before) or load_session_meta(args.after) or {}
    sched = meta.get("schedule") if isinstance(meta.get("schedule"), dict) else {}
    defaults = {"sweep_seconds": 10.0, "repeats": 3, "pre": 1.5, "repeat_gap": 1.0, "slot_gap": 1.0}
    params = {
        "sweep_seconds": float(args.sweep_seconds if args.sweep_seconds is not None
                               else sched.get("sweep_seconds", defaults["sweep_seconds"])),
        "repeats": int(args.repeats if args.repeats is not None else sched.get("repeats", defaults["repeats"])),
        "pre": float(args.pre if args.pre is not None else sched.get("pre", defaults["pre"])),
        "repeat_gap": float(args.repeat_gap if args.repeat_gap is not None
                            else sched.get("repeat_gap", defaults["repeat_gap"])),
        "slot_gap": float(args.slot_gap if args.slot_gap is not None
                          else sched.get("slot_gap", defaults["slot_gap"])),
        "level_db": float(args.level_db),
    }
    before = analyze_arm(args.before, params, "before")
    after = analyze_arm(args.after, params, "after")
    summary = compare(before, after, args.out,
                      limits={"offset_tol_samples": args.offset_tol_samples},
                      require_mics=args.require_mics)
    print("analyze: valid=%s offsets before=%s after=%s mics before=%s after=%s" % (
        summary["valid"], before["offsets"], after["offsets"],
        before["mics_available"], after["mics_available"]))
    if summary["invalid_reasons"]:
        print("INVALID: " + "; ".join(summary["invalid_reasons"]))
    print("mean spatial std improvement: %s dB; mean RT60 change: %s %%" % (
        summary["mean_spatial_std_improvement_db"], summary["mean_rt60_change_pct"]))
    return 0 if summary["valid"] else 3


if __name__ == "__main__":
    sys.exit(main())