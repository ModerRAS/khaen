"""Frozen analysis chain: 1/3-oct band filter -> Schroeder EDC -> T20.

Everything in this module is part of the frozen analysis manifest.  Any change to
a constant here invalidates every previous RAW / processed baseline (spec v1.2
§1.3) and MUST be recorded in ANALYSIS_MANIFEST.json together with the file hash.
"""
import numpy as np
from scipy.signal import butter, sosfiltfilt, resample_poly

# ---------------------------------------------------------------------------
# frozen parameters
# ---------------------------------------------------------------------------
BAND_DECIMATE_TO_HZ = 4000          # IR is decimated before the 1/3-oct filters
BAND_DECIMATE_METHOD = "scipy.signal.resample_poly(ir, 1, 12), default Kaiser FIR"
BAND_ORDER = 4                      # Butterworth order
BAND_TYPE = "butterworth bandpass, 4th order, sos, zero-phase filtfilt"
BAND_EDGES = "fc * 2**(-1/6) .. fc * 2**(+1/6)  (1/3 octave)"
EDC_METHOD = ("Schroeder reverse integration of the band IR^2, noise-subtracted "
              "(E_nc(t) = sum_{tau>=t} x^2 - P_noise*(N-t)), normalised to E_nc(0), "
              "integration starts at the broadband direct-sound peak (t=0)")
EDC_NOISE_WINDOW_S = 0.2            # P_noise = mean(x^2) over the last 200 ms of the IR window
FIT_LO_DB = -5.0
FIT_HI_DB = -25.0
FIT_R2_MIN = 0.90
T20_ABS_RANGE_MAX_S = 0.05          # primary gate when T20 < 0.2 s (spec v1.2)
T20_REL_RANGE_MAX = 0.10
T20_RANGE_MAX_S = 0.15
EDC100_RANGE_MAX_DB = 1.5
IR_XCORR_MIN = 0.98
IR_XCORR_SHIFT_SEARCH = 32          # +-32 samples (+-0.67 ms), diagnostic only
DR_MIN_DB = 35.0                    # low-freq strict gate: EDC dynamic range
NOISE_MARGIN_MIN_DB = 10.0          # noise floor must sit >=10 dB below the -25 dB point
RECORD_MIN_S = 1.0
RECORD_T20_FACTOR = 1.5
SNR_HARD_MIN_DB = 20.0              # channel/band usable at all
SNR_STRICT_MIN_DB = 25.0            # strict low-frequency gate

WATERFALL_MS = [0, 20, 40, 60, 80, 100, 150, 200, 300, 400]
EDC100_S = 0.1


def band_sos(fc, fs):
    lo, hi = fc * 2.0 ** (-1.0 / 6.0), fc * 2.0 ** (1.0 / 6.0)
    nyq = fs / 2.0
    return butter(BAND_ORDER, [lo / nyq, hi / nyq], btype="band", output="sos")


def decimate_ir(ir, fs, target=BAND_DECIMATE_TO_HZ):
    factor = int(round(fs / float(target)))
    if factor <= 1:
        return ir.copy(), fs
    return resample_poly(ir, 1, factor), fs / factor


def edc_curve(x, fs, noise_window_s=EDC_NOISE_WINDOW_S):
    """Noise-subtracted Schroeder EDC of a real signal x (t=0 = first sample)."""
    p = np.asarray(x, dtype=np.float64) ** 2
    n = p.size
    if n < 8:
        return None
    e_raw = np.cumsum(p[::-1])[::-1]
    nw = max(8, min(int(noise_window_s * fs), n // 4))
    p_noise = float(np.mean(p[n - nw:]))
    e_nc = e_raw - p_noise * (n - np.arange(n, dtype=np.float64))
    floor = max(e_nc[0], 1e-300) * 1e-12
    e_nc = np.maximum(e_nc, floor)
    edc_db = 10.0 * np.log10(e_nc / e_nc[0])
    dr_db = 10.0 * np.log10(max(e_raw[0], 1e-300) / max(p_noise * n, 1e-300))
    # EDC level the *noise alone* would produce: this is the floor the decay can be
    # followed down to.  Everything below it is not decay any more.
    noise_energy = p_noise * (n - np.arange(n, dtype=np.float64))
    edc_noise_db = 10.0 * np.log10(np.maximum(noise_energy, 1e-300) / e_nc[0])
    valid = int(np.argmax(edc_db - edc_noise_db < 0.0)) if np.any(edc_db - edc_noise_db < 0.0) else n
    return {
        "edc_db": edc_db,
        "edc_noise_db": edc_noise_db,
        "e_raw": e_raw,
        "e_nc": e_nc,
        "p_noise": p_noise,
        "n": n,
        "dr_db": float(dr_db),
        "valid_n": int(max(8, valid)),
        "floor": float(edc_db[min(valid, n - 1)]),
    }


def _first_below(edc_db, level, noise_db=None, margin_db=0.0, start=0, stop=None):
    """First index >= start where EDC <= level AND sits >= margin_db above the noise floor."""
    end = len(edc_db) if stop is None else min(stop, len(edc_db))
    idx = np.nonzero(edc_db[start:end] <= level)[0]
    if idx.size == 0:
        return None
    for i in idx:
        j = start + int(i)
        if noise_db is None or (edc_db[j] - noise_db[j]) >= margin_db:
            return j
    return None


def t20_fit(edc_db, fs, noise_db=None, fit_lo=FIT_LO_DB, fit_hi=FIT_HI_DB,
            margin_db=NOISE_MARGIN_MIN_DB):
    """Linear fit of EDC over the -5..-25 dB window.  No extrapolation.

    The -25 dB point is only accepted if it still sits >= margin_db above the noise
    floor; otherwise the "decay" there is noise and T20 is left undefined.
    """
    i5 = _first_below(edc_db, fit_lo, None, 0.0)
    i25 = _first_below(edc_db, fit_hi, None, 0.0, start=(i5 or 0))
    if i5 is None or i25 is None or i25 <= i5 + 8:
        return {"t20_s": None, "r2": None, "i_lo": i5, "i_hi": i25,
                "reason": "coverage: EDC never reaches -5/-25 dB inside the IR window"}
    if noise_db is not None and (edc_db[i25] - noise_db[i25]) < margin_db:
        return {"t20_s": None, "r2": None, "i_lo": i5, "i_hi": i25,
                "reason": ("coverage: the -25 dB point sits only %.1f dB above the noise "
                           "floor (needs >=%.0f dB) -> no valid T20"
                           % (edc_db[i25] - noise_db[i25], margin_db))}
    t = np.arange(i5, i25 + 1, dtype=np.float64) / fs
    y = edc_db[i5:i25 + 1]
    a = np.vstack([t, np.ones_like(t)]).T
    sol, *_ = np.linalg.lstsq(a, y, rcond=None)
    resid = y - a @ sol
    sstot = float(np.sum((y - y.mean()) ** 2))
    r2 = 1.0 - float(np.sum(resid ** 2)) / sstot if sstot > 0 else None
    slope = float(sol[0])           # dB per second (negative)
    return {
        "t20_s": (-60.0 / slope) if slope < 0 else None,
        "slope_db_per_s": slope,
        "r2": r2,
        "t_lo_s": float(t[0]),
        "t_hi_s": float(t[-1]),
        "i_lo": i5, "i_hi": i25,
        "reason": None,
    }


def edc_at(edc_db, fs, t_s):
    """EDC level (dB) at time t: linear interpolation, None outside the window."""
    pos = t_s * fs
    if pos < 0 or pos > len(edc_db) - 2:
        return None
    i = int(np.floor(pos))
    frac = pos - i
    return float(edc_db[i] * (1.0 - frac) + edc_db[i + 1] * frac)


def xcorr_ir(a, b, fs, main_lobe_ms=5.0, search=IR_XCORR_SHIFT_SEARCH):
    """Normalised IR cross-correlation around the main lobe.

    'unaligned'  : both windows are cut around their own detected peak, no extra shift
    'aligned'    : max over +-search samples, 'shift' reports the best shift
    The aligned value is diagnostic only -- absolute delays are reported separately.
    """
    n = min(len(a), len(b))
    ra = np.asarray(a[:n], dtype=np.float64)
    rb = np.asarray(b[:n], dtype=np.float64)
    if ra.size != rb.size or ra.size < 16:
        return None
    ea = float(np.sqrt(np.sum(ra * ra)))
    eb = float(np.sqrt(np.sum(rb * rb)))
    if ea <= 0 or eb <= 0:
        return None
    unaligned = float(np.sum(ra * rb) / (ea * eb))
    # fractional-sample alignment: the residual capture/playback clock offset leaves a
    # sub-sample time-base error between repeats, which decorrelates even a perfect IR.
    # Diagnose it by upsampling 8x and searching integer shifts on the fine grid.
    up = 8
    from scipy.signal import resample_poly
    nup = ra.size * up
    aup = resample_poly(ra, up, 1)[:nup]
    bup = resample_poly(rb, up, 1)[:nup]
    best, best_shift_up = unaligned, 0
    for s in range(-search * up, search * up + 1):
        if s >= 0:
            x1, x2 = aup[s:], bup[:bup.size - s]
        else:
            x1, x2 = aup[:aup.size + s], bup[-s:]
        if x1.size < 16 * up:
            continue
        v = float(np.sum(x1 * x2) / (np.sqrt(np.sum(x1 * x1) * np.sum(x2 * x2)) + 1e-300))
        if v > best:
            best, best_shift_up = v, s
    return {"unaligned": unaligned, "aligned": best,
            "best_shift_samples": best_shift_up / float(up),
            "window_ms": main_lobe_ms * 2.0}


def band_snr(capture, fs, fc, sweep_start, sweep_len, noise_slice):
    """Per-band SNR: sweep power vs ambient pre-roll power, same 1/3-oct filter."""
    sos = band_sos(fc, fs)
    sweep = capture[sweep_start:sweep_start + sweep_len]
    noise = capture[noise_slice[0]:noise_slice[1]]
    if sweep.size < 1024 or noise.size < 1024:
        return None
    sw = sosfiltfilt(sos, sweep)
    nz = sosfiltfilt(sos, noise)
    ps = float(np.mean(sw * sw))
    pn = float(np.mean(nz * nz))
    if pn <= 0 or ps <= 0:
        return None
    return float(10.0 * np.log10(ps / pn))


def t20_partial(edc_db, fs, noise_db, fit_lo=FIT_LO_DB, target_hi=-20.0, margin_db=10.0):
    """Supplementary (non-spec) slope estimate over -5 dB .. target_hi.

    Used only to describe bands whose -25 dB point is inside the noise floor: the fit
    stops at the deepest point that still sits >= margin_db above the floor.  This is
    NOT a T20 and must never be reported as a baseline value.
    """
    i5 = _first_below(edc_db, fit_lo, None, 0.0)
    if i5 is None:
        return {"t20_s": None, "r2": None, "range_db": None,
                "reason": "EDC never reaches -5 dB"}
    stop = None
    for i in range(i5, len(edc_db)):
        if edc_db[i] - noise_db[i] < margin_db:
            stop = i
            break
    if stop is None:
        stop = len(edc_db) - 1
    if stop <= i5 + 8:
        return {"t20_s": None, "r2": None, "range_db": None,
                "reason": "less than 10 dB of valid decay above the noise floor"}
    # deepest level still >= margin above the floor, capped at target_hi
    valid_level = float(min(target_hi, edc_db[stop] + margin_db))
    i_hi = _first_below(edc_db, valid_level, None, 0.0, start=i5)
    if i_hi is None or i_hi <= i5 + 8:
        return {"t20_s": None, "r2": None, "range_db": None,
                "reason": "no usable fit range"}
    t = np.arange(i5, i_hi + 1, dtype=np.float64) / fs
    y = edc_db[i5:i_hi + 1]
    a = np.vstack([t, np.ones_like(t)]).T
    sol, *_ = np.linalg.lstsq(a, y, rcond=None)
    resid = y - a @ sol
    sstot = float(np.sum((y - y.mean()) ** 2))
    r2 = 1.0 - float(np.sum(resid ** 2)) / sstot if sstot > 0 else None
    slope = float(sol[0])
    return {"t20_s": (-60.0 / slope) if slope < 0 else None,
            "r2": r2,
            "range_db": [float(edc_db[i5]), float(edc_db[i_hi])],
            "t_range_s": [float(t[0]), float(t[-1])],
            "reason": None}
