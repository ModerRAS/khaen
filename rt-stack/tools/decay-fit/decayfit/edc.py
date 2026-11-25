# -*- coding: utf-8 -*-
"""Capture -> deconvolved IR -> per-band noise-subtracted Schroeder EDC.

The band filter, decimation and EDC formula are the frozen ir-diag ones
(tools/ir-diag/irdiag/metrics.py, used read-only).  Two noise-floor estimators
are provided and always both reported:

  * "frozen"  -- mean power of the last 200 ms of the 5 s IR window, i.e. exactly
                 irdiag.metrics.edc_curve.  This is the estimator every earlier
                 baseline used.
  * "plateau" -- mean power of the band IR in a quiet late window (4.5..7 s after
                 the direct peak, inside the real capture), where the room response
                 has decayed and only ambient noise is left.  p36-20261121 showed the frozen 200 ms
                 window sits on the filtfilt end transient for very narrow bands
                 (40 Hz), biasing p_noise per repeat; the plateau window is the
                 M1 correction used there.  decay-fit defaults to "plateau" for
                 the 30 s-sweep captures and reports "frozen" as the control.

The 30 s-sweep dr-upgrade captures are deconvolved with the single common clock
stretch measured for that device pair (eps = -60.4 ppm).  No sound card is used.
"""
import hashlib
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
IRDIAG = os.path.abspath(os.path.join(HERE, "..", "..", "ir-diag"))
if IRDIAG not in sys.path:
    sys.path.insert(0, IRDIAG)

from irdiag import analysis as A          # noqa: E402
from irdiag import excitation as exc      # noqa: E402
from irdiag import metrics as M           # noqa: E402

CAPTURE_FS = 48000
SWEEP_S = 30.0
DEFAULT_EPS_PPM = -60.4
IR_WINDOW_S = 5.0
IR_PRE_S = 0.10
FS_DEC = float(M.BAND_DECIMATE_TO_HZ)
ANALYSIS_BANDS_HZ = [40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0]


def sha256_file(path, chunk=1 << 20):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(chunk), b""):
            h.update(b)
    return h.hexdigest()


def _nfft_for(cap_len, sweep_len):
    n = 1
    while n < cap_len + int(sweep_len * 1.0002) + 1024:
        n <<= 1
    return n


def _sweep_at(length, sweep_s):
    s = exc.gen_sweep(f0=exc.F0_HZ, f1=exc.F1_FULL_HZ, seconds=sweep_s, rate=CAPTURE_FS)
    if int(length) == len(s):
        return s
    from scipy.signal import resample
    return resample(s, int(length))


def deconvolve_capture(path, eps_ppm=DEFAULT_EPS_PPM, sweep_s=SWEEP_S):
    """Load a 2ch S24_3LE UMIK capture and deconvolve the exponential sweep.

    Returns the full 48 kHz deconvolved stream (n_fft samples), the absolute IR
    peak sample, and the 4 kHz-decimated IR with its t=0 sample index.
    """
    cap = A.load_capture(path)
    sweep_frames = int(round(sweep_s * CAPTURE_FS))
    length = int(round(sweep_frames * (1.0 + eps_ppm * 1e-6)))
    s = _sweep_at(length, sweep_s)
    inv = exc.farina_inverse_filter(s, CAPTURE_FS, exc.F0_HZ, exc.F1_FULL_HZ)
    n_fft = _nfft_for(len(cap), len(s))
    Y = np.fft.rfft(cap, n_fft)
    out = np.fft.irfft(Y * np.fft.rfft(inv, n_fft), n_fft)
    lo = int(0.5 * CAPTURE_FS)
    hi = min(len(out), int((sweep_s + 12.0) * CAPTURE_FS))
    peak = lo + int(np.argmax(np.abs(out[lo:hi])))
    from scipy.signal import resample_poly
    ir_dec = resample_poly(out, 1, 12).astype(np.float64)
    fs_dec = CAPTURE_FS / 12.0
    t0 = int(round(peak * fs_dec / CAPTURE_FS))
    pre = 40
    seg = ir_dec[max(0, t0 - pre):]
    pk = int(np.argmax(np.abs(seg)))
    t0 = max(0, t0 - pre) + pk
    # frozen-chain IR: exactly analysis.analyze_repeat's 5 s segment, decimated the
    # same way (frozen control must reproduce the frozen analysis, including its
    # edge-estimated noise floor); the plateau estimator uses the whole-stream IR.
    win_s, win_n = A.ir_window(peak, len(out), len(out))
    lo_fr = max(0, peak - int(round(IR_PRE_S * CAPTURE_FS)))
    ir_seg = out[lo_fr:peak + win_n].astype(np.float64)
    ir_seg_dec, fs_dec2 = M.decimate_ir(ir_seg, CAPTURE_FS)
    t0_seg = int(round((peak - lo_fr) * fs_dec2 / CAPTURE_FS))
    pre40 = int(round(0.01 * fs_dec2))
    seg40 = ir_seg_dec[max(0, t0_seg - pre40):]
    pk40 = int(np.argmax(np.abs(seg40)))
    t0_seg = max(0, t0_seg - pre40) + pk40
    rms = float(np.sqrt(np.mean(cap[int(1.5 * CAPTURE_FS):int(min(len(cap), 40.5 * CAPTURE_FS))] ** 2)) + 1e-20)
    return {
        "path": os.path.abspath(path),
        "capture_frames": int(len(cap)),
        "capture_sha256": sha256_file(path),
        "eps_ppm": float(eps_ppm),
        "sweep_s": float(sweep_s),
        "n_fft": int(n_fft),
        "ir_peak_abs": int(peak),
        "ir_peak_s": float(peak) / CAPTURE_FS,
        "mic_rms_dbfs": float(20.0 * np.log10(rms)),
        "fs_dec": fs_dec,
        "ir_dec": ir_dec,
        "t0_dec": int(t0),
        "ir_window_s": float(win_s),
        "fs_dec_seg": float(fs_dec2),
        "ir_dec_seg": ir_seg_dec,
        "t0_dec_seg": int(t0_seg),
    }


def edc_with_noise(x, fs, p_noise):
    """Frozen Schroeder EDC formula with an explicitly supplied noise power."""
    p = np.asarray(x, dtype=np.float64) ** 2
    n = p.size
    e_raw = np.cumsum(p[::-1])[::-1]
    e_nc = e_raw - p_noise * (n - np.arange(n, dtype=np.float64))
    e_nc = np.maximum(e_nc, max(e_nc[0], 1e-300) * 1e-12)
    edc_db = 10.0 * np.log10(e_nc / e_nc[0])
    dr_db = 10.0 * np.log10(max(e_raw[0], 1e-300) / max(p_noise * n, 1e-300))
    noise_energy = p_noise * (n - np.arange(n, dtype=np.float64))
    edc_noise_db = 10.0 * np.log10(np.maximum(noise_energy, 1e-300) / e_nc[0])
    valid = int(np.argmax(edc_db - edc_noise_db < 0.0)) if np.any(edc_db - edc_noise_db < 0.0) else n
    return {"edc_db": edc_db, "edc_noise_db": edc_noise_db, "e_nc": e_nc,
            "p_noise": float(p_noise), "n": int(n), "dr_db": float(dr_db),
            "valid_n": int(max(8, valid))}


def band_edc(dec, fc, window_s=IR_WINDOW_S, noise="plateau",
             plateau_lo_s=4.5, plateau_hi_s=7.0):
    """1/3-oct band filter + Schroeder EDC for one capture and band.

    dec is the dict returned by deconvolve_capture.  The whole (long) decimated
    IR is filtered before cutting the window, so filtfilt end transients cannot
    contaminate either the decay or the noise window.
    """
    ir_dec = dec["ir_dec"]
    t0 = dec["t0_dec"]
    fs = dec["fs_dec"]
    sos = M.band_sos(fc, fs)
    y = M.sosfiltfilt(sos, ir_dec)
    x = y[t0:t0 + int(round(window_s * fs))]
    win_n = x.size
    plateau = None
    spread = None
    method_used = noise
    if noise == "plateau":
        lo = t0 + int(round(plateau_lo_s * fs))
        hi = t0 + int(round(plateau_hi_s * fs))
        hi = min(hi, len(y) - int(0.5 * fs))
        if hi - lo >= int(0.5 * fs):
            pw = []
            seg_len = int(0.5 * fs)
            for k in range((hi - lo) // seg_len):
                pw.append(float(np.mean(y[lo + k * seg_len:lo + (k + 1) * seg_len] ** 2)))
            plateau = float(np.mean(pw))
            spread = float(np.std(10.0 * np.log10(np.array(pw) + 1e-300)))
            if spread > 3.0:
                method_used = "frozen"
        else:
            method_used = "frozen"
    if method_used == "frozen":
        ed = M.edc_curve(x, fs)
        p_noise = ed["p_noise"]
        edc_db = ed["edc_db"]
        edc_noise_db = ed["edc_noise_db"]
        dr_db = ed["dr_db"]
        valid_n = ed["valid_n"]
        plateau = p_noise
        method_used = "frozen"
    else:
        ed = edc_with_noise(x, fs, plateau)
        edc_db, edc_noise_db, dr_db, valid_n = ed["edc_db"], ed["edc_noise_db"], ed["dr_db"], ed["valid_n"]
    return {
        "fc": float(fc),
        "noise_method": method_used,
        "p_noise": float(plateau),
        "plateau_spread_db": spread,
        "fs": fs,
        "t0_s": 0.0,
        "edc_db": edc_db,
        "edc_noise_db": edc_noise_db,
        "dr_db": float(dr_db),
        "valid_n": int(valid_n),
        "valid_edc_s": float(valid_n) / fs,
        "edc100_db": float(edc_db[int(round(0.1 * fs))]) if int(round(0.1 * fs)) < win_n else None,
        "ir_energy_db": float(10.0 * np.log10(np.sum(x * x) + 1e-300)),
        "ir_energy_0_100ms_db": float(10.0 * np.log10(np.sum(x[:int(0.1 * fs)] ** 2) + 1e-300)),
        "window_s": float(win_n) / fs,
    }


def band_edc_frozen(dec, fc):
    """Frozen-chain band EDC: 5.0 s IR segment cut from the deconvolved stream,
    decimated like analysis.analyze_repeat, filtered, irdiag.metrics.edc_curve.

    This is the control that must reproduce dr-upgrade-20261121/analysis.json.
    """
    fs = dec["fs_dec_seg"]
    sos = M.band_sos(fc, fs)
    xb = M.sosfiltfilt(sos, dec["ir_dec_seg"])
    x = xb[dec["t0_dec_seg"]:]
    ed = M.edc_curve(x, fs)
    return {
        "fc": float(fc),
        "noise_method": "frozen",
        "p_noise": float(ed["p_noise"]),
        "plateau_spread_db": None,
        "fs": float(fs),
        "t0_s": 0.0,
        "edc_db": ed["edc_db"],
        "edc_noise_db": ed["edc_noise_db"],
        "dr_db": float(ed["dr_db"]),
        "valid_n": int(ed["valid_n"]),
        "valid_edc_s": float(ed["valid_n"]) / fs,
        "edc100_db": float(ed["edc_db"][int(round(0.1 * fs))]),
        "ir_energy_db": float(10.0 * np.log10(np.sum(x * x) + 1e-300)),
        "ir_energy_0_100ms_db": float(10.0 * np.log10(np.sum(x[:int(0.1 * fs)] ** 2) + 1e-300)),
        "window_s": float(x.size) / fs,
    }
