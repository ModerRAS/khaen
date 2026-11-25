"""Capture loading, deconvolution, IR extraction and the per-channel analysis run."""
import json
import os

import numpy as np

from . import excitation as exc
from . import metrics as M

FS = exc.FS
IR_SECONDS_CONFIG = 5.0      # frozen: min(IR_SECONDS_CONFIG, repeat interval - 0.05 s)
IR_GUARD_S = 0.05
IR_PRE_S = 0.10
PULSE_SEARCH_SAMPLES = 12000  # +-0.25 s around the expected arrival
DELAY_GUESS_SAMPLES = 900     # playback latency + first arrival, rough anchor


# ---------------------------------------------------------------------------
# I/O
# ---------------------------------------------------------------------------
def load_capture(path, keep_channels=False):
    """UMIK capture: interleaved 2ch S24_3LE @48k.  ch0 is used; ch1 is a copy."""
    raw = np.fromfile(path, dtype=np.uint8)
    n_frames = raw.size // 6
    frames = raw[:n_frames * 6].reshape(n_frames, 2, 3)
    v = np.zeros((n_frames, 2), dtype=np.float64)
    for c in range(2):
        a = frames[:, c, :].astype(np.int32)
        s = a[:, 0] | (a[:, 1] << 8) | (a[:, 2] << 16)
        s = np.where(s & 0x800000, s - (1 << 24), s)
        v[:, c] = s / 8388608.0
    if keep_channels:
        return v[:, 0], v[:, 1]
    return v[:, 0]


def load_meta(meta_dir):
    """Read the measure-multi diagnostics written next to capture.raw."""
    session = None
    chans = {}
    for name in os.listdir(meta_dir):
        if name.endswith(".repeats.csv"):
            chname = name.split("-")[0]
            try:
                with open(os.path.join(meta_dir, name), "r", encoding="utf-8") as f:
                    rows = list(__import__("csv").DictReader(f))
                vals = [float(r["snr_db"]) for r in rows if r.get("snr_db")]
                if vals:
                    chans.setdefault(chname, {})["snr_db_from_repeats_csv"] = float(
                        __import__("numpy").median(vals))
            except Exception:
                pass
        if not name.endswith(".diag.json"):
            continue
        p = os.path.join(meta_dir, name)
        with open(p, "r", encoding="utf-8") as f:
            d = json.load(f)
        if name.startswith("session"):
            session = d
        else:
            chans[d["name"]] = d
    return session, chans


# ---------------------------------------------------------------------------
# deconvolution
# ---------------------------------------------------------------------------
def next_pow2(n):
    p = 1
    while p < n:
        p <<= 1
    return p


def deconvolve_spectrum(capture, inv, n_fft=None):
    n = n_fft or next_pow2(len(capture) + len(inv))
    y = np.fft.rfft(capture, n)
    out = np.fft.irfft(y * np.fft.rfft(inv, n), n)
    return out


def expected_pulse(layout, slot_index, repeat):
    nominal = (layout["pre_samples"] + slot_index * layout["slot_samples"]
               + repeat * layout["interval_samples"] + layout["dev_samples"])
    return nominal


def find_pulses(out, layout, sweep_len):
    pulses = {}
    for ch, name, slot, _f1 in exc.CHANNELS:
        for k in range(layout["repeats"]):
            center = expected_pulse(layout, slot, k) + sweep_len + DELAY_GUESS_SAMPLES
            lo = max(0, center - PULSE_SEARCH_SAMPLES)
            hi = min(len(out), center + PULSE_SEARCH_SAMPLES)
            seg = np.abs(out[lo:hi])
            j = int(np.argmax(seg))
            pulses[(name, k)] = {"abs_sample": lo + j, "amp": float(seg[j])}
    return pulses


def estimate_stretch(out, layout, sweep_len):
    """Per-channel drift fit of the detected repeat spacing -> clock ratio eps."""
    per_channel = {}
    for ch, name, slot, f1 in exc.CHANNELS:
        if f1 < 1000.0:
            # a band-limited sweep (SUB1) does not correlate with the full-band
            # inverse filter, so its pulse position is not usable for the clock fit
            continue
        p = [pulses_abs(out, layout, sweep_len, slot, k) for k in range(layout["repeats"])]
        if layout["repeats"] >= 2:
            eps = (p[-1] - p[0]) / ((layout["repeats"] - 1) * layout["interval_samples"]) - 1.0
            per_channel[name] = eps
    vals = np.array([v for v in per_channel.values()], dtype=np.float64)
    if vals.size == 0:
        return 0.0, per_channel
    med = float(np.median(vals))
    keep = vals[np.abs(vals - med) < 200e-6]
    return (float(np.median(keep)) if keep.size else med), per_channel


def pulses_abs(out, layout, sweep_len, slot, k):
    center = expected_pulse(layout, slot, k) + sweep_len + DELAY_GUESS_SAMPLES
    lo = max(0, center - PULSE_SEARCH_SAMPLES)
    hi = min(len(out), center + PULSE_SEARCH_SAMPLES)
    return lo + int(np.argmax(np.abs(out[lo:hi])))


# ---------------------------------------------------------------------------
# single-repeat IR metrics
# ---------------------------------------------------------------------------
def ir_window(pulse, next_pulse, n_total, fs=FS):
    gap = (next_pulse - pulse) / float(fs) - IR_GUARD_S
    win = min(IR_SECONDS_CONFIG, gap)
    return max(0.25, win), int(round(max(0.25, win) * fs))


def analyze_repeat(out, pulse, next_pulse, bands):
    """Return per-band EDC/T20 metrics for one deconvolved IR."""
    win_s, win_n = ir_window(pulse, next_pulse, len(out))
    pre_n = int(round(IR_PRE_S * FS))
    lo = max(0, pulse - pre_n)
    ir = out[lo:pulse + win_n].astype(np.float64)
    t0 = pulse - lo
    ir_dec, fs_dec = M.decimate_ir(ir, FS)
    t0_dec = int(round(t0 * fs_dec / FS))
    # broadband (4 kHz limited) direct-sound peak = shared t=0 for every band
    seg = ir_dec[max(0, t0_dec - int(0.01 * fs_dec)):]
    pk = int(np.argmax(np.abs(seg)))
    t0_dec = max(0, t0_dec - int(0.01 * fs_dec)) + pk
    res = {"ir_window_s": win_s, "ir_peak_abs_sample": int(pulse),
           "ir_peak_in_window": int(t0_dec), "fs_dec": fs_dec, "bands": {},
           "ir_dec": ir_dec, "ir_dec_peak": int(t0_dec), "ir_dec_len": int(ir_dec.size)}
    for fc in bands:
        sos = M.band_sos(fc, fs_dec)
        xb = M.sosfiltfilt(sos, ir_dec)
        x = xb[t0_dec:]
        ed = M.edc_curve(x, fs_dec)
        if ed is None:
            res["bands"][str(int(fc))] = {"t20_s": None, "reason": "ir too short"}
            continue
        fit = M.t20_fit(ed["edc_db"], fs_dec, ed["edc_noise_db"])
        partial = M.t20_partial(ed["edc_db"], fs_dec, ed["edc_noise_db"])
        edc_db = ed["edc_db"]
        # report the EDC only down to where it is still above the noise floor
        vn = ed["valid_n"]
        edc_plot = np.full_like(edc_db, np.nan)
        edc_plot[:vn] = edc_db[:vn]
        b = {
            "t20_s": fit["t20_s"],
            "r2": fit["r2"],
            "slope_db_per_s": fit.get("slope_db_per_s"),
            "t_lo_s": fit.get("t_lo_s"),
            "t_hi_s": fit.get("t_hi_s"),
            "edc100_db": M.edc_at(edc_db, fs_dec, M.EDC100_S),
            "dr_db": ed["dr_db"],
            "valid_edc_range_db": float(-ed["floor"]) if np.isfinite(ed["floor"]) else None,
            "valid_edc_s": float(vn) / fs_dec,
            "record_after_peak_s": float(x.size) / fs_dec,
            "edc_db": edc_plot,
            "fit_reason": fit.get("reason"),
            "t20_partial_s": partial.get("t20_s"),
            "t20_partial_r2": partial.get("r2"),
            "t20_partial_range_db": partial.get("range_db"),
            "t20_partial_reason": partial.get("reason"),
            "noise_margin_db": None,
        }
        if fit.get("i_hi") is not None:
            i25 = fit["i_hi"]
            noise_rem = ed["p_noise"] * (ed["n"] - i25)
            b["noise_margin_db"] = float(10.0 * np.log10(max(ed["e_nc"][i25], 1e-300)
                                                          / max(noise_rem, 1e-300)))
        b["waterfall_db"] = {str(ms): (M.edc_at(edc_db, fs_dec, ms / 1000.0)
                                       if int(ms / 1000.0 * fs_dec) < vn else None)
                             for ms in M.WATERFALL_MS}
        res["bands"][str(int(fc))] = b
    return res


def band_snr_capture(capture, fs, fc, sweep_start, sweep_len, noise_slice):
    return M.band_snr(capture, fs, fc, sweep_start, sweep_len, noise_slice)
