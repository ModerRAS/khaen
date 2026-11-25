"""End-to-end RAW baseline pipeline: capture -> IR -> EDC/T20 -> gates."""
import hashlib
import json
import os

import numpy as np

from . import analysis as A
from . import excitation as exc
from . import metrics as M

# ---- optional decay-fit hook (default OFF; nothing is imported unless enabled) ----
_DECAYFIT_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..",
                                             "decay-fit"))


def _decay_fit_module():
    import sys
    if _DECAYFIT_DIR not in sys.path:
        sys.path.insert(0, _DECAYFIT_DIR)
    from decayfit import integrate as DFI
    return DFI


def _decay_fit_band(edc_curves, rep_list):
    """Segmented-decay block for one band (non-standard; opt-in only)."""
    DFI = _decay_fit_module()
    fs = rep_list[0].get("fs_dec", 4000.0) if rep_list else 4000.0
    margins = [r.get("noise_margin_db") for r in rep_list]
    return DFI.band_decay_fit(edc_curves, fs, margins)

# Frozen measurement-session layout (from the measure-multi command line that
# produced capture.raw; cross-checked against session.diag.json at run time).
PRE_S = 2.0
DEV_S = 0.35
REPEATS = 3

EXPECTED_NOMINAL = {  # diag "nominal_start" = pre + slot*slot_samples + dev
    "FL": 112800, "FR": 900000, "SL": 1687200, "SR": 2474400,
    "HFL": 3261600, "HFR": 4048800, "SUB1": 4836000,
}


def sha256_file(path, chunk=1 << 20):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            b = f.read(chunk)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


def build_layout(session, capture_len):
    interval = int(session["interval_samples"]) if session else 249600
    slot = int(session["slot_samples"]) if session else 787200
    layout = {
        "pre_samples": int(round(PRE_S * A.FS)),
        "dev_samples": int(round(DEV_S * A.FS)),
        "interval_samples": interval,
        "slot_samples": slot,
        "repeats": int(session.get("repeats", REPEATS)) if session else REPEATS,
        "capture_frames": int(capture_len),
        "post_s": 2.0,
    }
    layout["expected_total_frames"] = (layout["pre_samples"] + 7 * slot
                                      + int(round(2.0 * A.FS)) + 2 * A.FS)
    return layout


def _gate_band(rep_list, snr_db, xcorr, band_fc, excitation_f1):
    """Spec v1.2 §1.1/§1.2 gates for one channel/band.  Returns (status, reasons)."""
    reasons = []
    t20s = [r.get("t20_s") for r in rep_list]
    e100 = [r.get("edc100_db") for r in rep_list]
    r2s = [r.get("r2") for r in rep_list]
    drs = [r.get("dr_db") for r in rep_list]
    recs = [r.get("record_after_peak_s") for r in rep_list]
    nmarg = [r.get("noise_margin_db") for r in rep_list]

    if band_fc > excitation_f1 * 1.02:
        return "NOT_DRIVEN", ["band above the excitation band edge %.0f Hz" % excitation_f1]
    if any(v is None for v in t20s):
        first_bad = next((r for r in rep_list if r.get("t20_s") is None), None)
        why = (first_bad or {}).get("fit_reason") or "coverage: T20 undefined"
        reasons.append("coverage: T20 undefined in %d/%d repeats (%s)"
                       % (sum(1 for v in t20s if v is None), len(t20s), why))
        return "UNRELIABLE", reasons
    if any(v is None for v in e100):
        reasons.append("EDC@100ms undefined")
        return "UNRELIABLE", reasons
    if any(v is None or v < M.FIT_R2_MIN for v in r2s):
        reasons.append("fit R2 below %.2f (%s)" % (M.FIT_R2_MIN,
                       "/".join("%.3f" % v if v is not None else "na" for v in r2s)))
    med = float(np.median(t20s))
    rng = float(max(t20s) - min(t20s))
    rel = rng / med if med > 0 else float("inf")
    if med < 0.2:
        if rng > M.T20_ABS_RANGE_MAX_S:
            reasons.append("T20 repeat range %.3f s > %.2f s (short-T20 absolute gate)"
                           % (rng, M.T20_ABS_RANGE_MAX_S))
    else:
        if rel > M.T20_REL_RANGE_MAX:
            reasons.append("T20 relative repeat range %.1f%% > %.0f%%" % (100 * rel, 100 * M.T20_REL_RANGE_MAX))
        if rng > M.T20_RANGE_MAX_S:
            reasons.append("T20 repeat range %.3f s > %.2f s" % (rng, M.T20_RANGE_MAX_S))
    e_rng = float(max(e100) - min(e100))
    if e_rng > M.EDC100_RANGE_MAX_DB:
        reasons.append("EDC@100ms repeat range %.2f dB > %.1f dB" % (e_rng, M.EDC100_RANGE_MAX_DB))
    if snr_db is not None and snr_db < M.SNR_HARD_MIN_DB:
        reasons.append("band SNR %.1f dB < %.0f dB" % (snr_db, M.SNR_HARD_MIN_DB))
    low_snr_or_lowband = (band_fc <= 63.0) or (snr_db is not None and snr_db < M.SNR_STRICT_MIN_DB)
    if low_snr_or_lowband:
        if min(drs) < M.DR_MIN_DB:
            reasons.append("EDC dynamic range %.1f dB < %.0f dB" % (min(drs), M.DR_MIN_DB))
        if any(v is None or v < M.NOISE_MARGIN_MIN_DB for v in nmarg):
            reasons.append("noise floor < %.0f dB below the -25 dB point" % M.NOISE_MARGIN_MIN_DB)
        need = max(M.RECORD_T20_FACTOR * med, M.RECORD_MIN_S)
        if min(recs) < need:
            reasons.append("decay record %.2f s < max(1.5*T20, 1.0 s)=%.2f s" % (min(recs), need))
    if xcorr is not None and xcorr["aligned"] < M.IR_XCORR_MIN:
        reasons.append("IR repeat xcorr %.3f < %.2f" % (xcorr["aligned"], M.IR_XCORR_MIN))
    status = "UNRELIABLE" if reasons else "OK"
    return status, reasons


def run(capture_path, meta_dir, out_dir, verbose=True, decay_fit=None):
    """Run the frozen G0 analysis.

    decay_fit=None (default) keeps the frozen behaviour bit-for-bit.  Passing a
    truthy value enables the optional segmented-decay block from tools/decay-fit
    (non-standard description; the standard T20 verdicts are not changed).
    """
    log = (lambda *a: print(*a, flush=True)) if verbose else (lambda *a: None)
    session, chmeta = A.load_meta(meta_dir)
    mic, mic_ch1 = A.load_capture(capture_path, keep_channels=True)
    capture_sha = sha256_file(capture_path)
    layout = build_layout(session, len(mic))
    log("capture: %d frames (%.2f s)  sha256=%s" % (len(mic), len(mic) / A.FS, capture_sha[:16]))
    log("ch0 == ch1: %s" % bool(np.array_equal(mic, mic_ch1)))

    # ---- pass 1: nominal-length inverse filter, drift estimate -----------------
    src_nom = exc.gen_sweep(f0=exc.F0_HZ, f1=exc.F1_FULL_HZ)
    inv_nom = exc.farina_inverse_filter(src_nom, A.FS, exc.F0_HZ, exc.F1_FULL_HZ)
    out_nom = A.deconvolve_spectrum(mic, inv_nom)
    eps, eps_per_ch = A.estimate_stretch(out_nom, layout, len(src_nom))
    del out_nom
    stretch_len = int(round(len(src_nom) * (1.0 + eps)))
    log("clock drift eps = %.2f ppm -> inverse-filter length %d samples" % (eps * 1e6, stretch_len))

    # ---- pass 2: drift-corrected deconvolution --------------------------------
    src_full = exc.stretched_source(exc.F0_HZ, exc.F1_FULL_HZ, stretch_len)
    inv_full = exc.farina_inverse_filter(src_full, A.FS, exc.F0_HZ, exc.F1_FULL_HZ)
    n_fft = A.next_pow2(len(mic) + len(inv_full))
    Y = np.fft.rfft(mic, n_fft)
    out_full = np.fft.irfft(Y * np.fft.rfft(inv_full, n_fft), n_fft)
    src_sub = exc.stretched_source(exc.F0_HZ, exc.F1_SUB_HZ, stretch_len)
    inv_sub = exc.farina_inverse_filter(src_sub, A.FS, exc.F0_HZ, exc.F1_SUB_HZ)
    out_sub = np.fft.irfft(Y * np.fft.rfft(inv_sub, n_fft), n_fft)
    log("deconvolved %d samples (full + sub inverse filters)" % n_fft)

    pulses = A.find_pulses(out_full, layout, stretch_len)
    pulses_sub = A.find_pulses(out_sub, layout, stretch_len)
    for key, val in pulses_sub.items():
        if key[0] == "SUB1":   # only the band-limited sweep uses the sub inverse filter
            pulses[key] = val
    ordered = sorted((v["abs_sample"], k) for k, v in pulses.items())

    # noise slice: pre-roll before the first sweep
    noise_slice = (0, int(round(1.8 * A.FS)))

    result = {
        "schema": "tspi-ir-diag/v1",
        "group": "G0",
        "treatment": "RAW / P_post bypass (identity)",
        "apply": "NOT_RUN",
        "inputs": {
            "capture": os.path.basename(capture_path),
            "capture_sha256": capture_sha,
            "capture_frames": int(len(mic)),
            "capture_seconds": len(mic) / A.FS,
            "meta_dir": os.path.abspath(meta_dir),
            "session_diag": session,
            "capture_ch0_equals_ch1": bool(np.array_equal(mic, mic_ch1)),
        },
        "layout": layout,
        "excitation": {
            "source": "calibrate/src/sweep.rs:28-55 (SweepSpec::generate)",
            "inverse_filter": "calibrate/src/dsp.rs:66-92 (farina_inverse_filter)",
            "sweep_seconds": exc.SWEEP_SECONDS,
            "level_dbfs": exc.LEVEL_DBFS,
            "fade_ms": exc.FADE_MS,
            "f0_hz": exc.F0_HZ,
            "f1_full_hz": exc.F1_FULL_HZ,
            "f1_sub_hz": exc.F1_SUB_HZ,
            "sub1_excitation_band_hz": [exc.F0_HZ, exc.F1_SUB_HZ],
            "channels": [{"name": n, "dac_channel": c, "slot": s, "f1_hz": f1}
                         for c, n, s, f1 in exc.CHANNELS],
        },
        "clock": {
            "eps_ppm": eps * 1e6,
            "per_channel_ppm": {k: v * 1e6 for k, v in eps_per_ch.items()},
            "inverse_filter_length_samples": stretch_len,
            "method": "per-channel repeat-spacing fit of the 18 detected IR pulses; "
                      "capture clock runs (eps) relative to the playback clock",
            "diag_clock_ppm": {k: chmeta[k].get("clock_ppm") for k in chmeta},
        },
        "capture_check": {
            "ch0_eq_ch1": bool(np.array_equal(mic, mic_ch1)),
            "frames": int(len(mic)),
            "expected_total_frames": layout["expected_total_frames"],
            "nominal_start_check": {k: chmeta[k]["nominal_start"] for k in chmeta},
        },
        "channels": {},
    }

    xcorr_by_channel = {}
    xcorr_full_by_channel = {}
    band_snr_all = {}
    for ch, name, slot, f1 in exc.CHANNELS:
        arr = out_sub if name == "SUB1" else out_full
        idx = [i for i, (pos, key) in enumerate(ordered) if key[0] == name]
        reps = []
        ir_segs = []
        ir_dec_all = []
        rep_peaks = []
        rep_fs = 4000.0
        for k in range(layout["repeats"]):
            p = pulses[(name, k)]["abs_sample"]
            later = [pos for pos, _ in ordered if pos > p]
            next_p = later[0] if later else len(arr)
            rep = A.analyze_repeat(arr, p, next_p, exc.ANALYSIS_BANDS_HZ)
            reps.append(rep)
            ir_dec_all.append(rep.pop("ir_dec"))
            rep_peaks.append(rep.pop("ir_dec_peak"))
            rep.pop("ir_dec_len")
            rep_fs = rep["fs_dec"]
            pre_n = int(round(A.IR_PRE_S * A.FS))
            pk = rep["ir_peak_abs_sample"]
            ir_segs.append(arr[pk - int(0.006 * A.FS): pk + int(0.006 * A.FS)])
        # IR repeatability, measured on the 4 kHz-decimated IR (the analysed band is
        # <= 200 Hz; the full-band 48 kHz correlation is dominated by the residual
        # fractional-sample time-base difference between repeats -> reported as diagnostic)
        half_dec = int(round(0.002 * rep_fs))
        xc = []
        for i in range(len(ir_dec_all) - 1):
            a = ir_dec_all[i]
            b = ir_dec_all[i + 1]
            pa = int(round(rep_peaks[i] * 1.0))
            pb = int(round(rep_peaks[i + 1] * 1.0))
            sa = a[pa - half_dec: pa + half_dec]
            sb = b[pb - half_dec: pb + half_dec]
            xc.append(M.xcorr_ir(sa, sb, rep_fs))
        xcorr_by_channel[name] = xc
        xc_full = []
        for i in range(len(ir_segs) - 1):
            ha = int(round(0.002 * A.FS))
            ca = ir_segs[i][len(ir_segs[i]) // 2 - ha: len(ir_segs[i]) // 2 + ha]
            cb = ir_segs[i + 1][len(ir_segs[i + 1]) // 2 - ha: len(ir_segs[i + 1]) // 2 + ha]
            xc_full.append(M.xcorr_ir(ca, cb, A.FS))
        xcorr_full_by_channel[name] = xc_full
        # per-band SNR from the raw capture (repeat 0 sweep vs ambient pre-roll)
        p0 = pulses[(name, 0)]["abs_sample"]
        sweep_start = max(0, p0 - stretch_len - 2000)
        bsnr = {}
        for fc in exc.ANALYSIS_BANDS_HZ:
            bsnr[str(int(fc))] = A.band_snr_capture(mic, A.FS, fc, sweep_start, stretch_len, noise_slice)
        band_snr_all[name] = bsnr

        chres = {
            "dac_channel": ch,
            "slot": slot,
            "excitation_band_hz": [exc.F0_HZ, f1],
            "diag": {
                "snr_db": chmeta[name].get("snr_db"),
                "snr_band_db": chmeta[name].get("snr_band_db"),
                "snr_band_hz": chmeta[name].get("snr_band_hz"),
                "onset_invalid": chmeta[name].get("onset_invalid"),
                "insufficient_valid_passes": chmeta[name].get("insufficient_valid_passes"),
                "clock_ppm": chmeta[name].get("clock_ppm"),
                "clock_ppm_ok": chmeta[name].get("clock_ppm_ok"),
                "repeats_csv_snr_db": chmeta[name].get("snr_db_from_repeats_csv"),
                "band_limit_hz": chmeta[name].get("band_limit_hz"),
                "level_db": chmeta[name].get("level_db"),
            },
            "pulses_abs_sample": [pulses[(name, k)]["abs_sample"] for k in range(layout["repeats"])],
            "pulse_residual_samples": [float(v) for v in _pulse_residuals(
                [pulses[(name, k)]["abs_sample"] for k in range(layout["repeats"])],
                layout["interval_samples"])],
            "pulse_residual_spread_samples": float(np.ptp(_pulse_residuals(
                [pulses[(name, k)]["abs_sample"] for k in range(layout["repeats"])],
                layout["interval_samples"]))),
            "diag_repeat_start_delta_samples": [
                float(round(pulses[(name, k)]["abs_sample"]
                            - (chmeta[name]["repeat_starts"][k]), 1))
                for k in range(layout["repeats"])],
            "ir_xcorr_pairwise": xc,
            "ir_xcorr_pairwise_fullband_48k": xc_full,
            "ir_xcorr_domain": {"fs_hz": rep_fs, "note": "4 kHz decimated IR (analysis band); "
                                "full-band 48 kHz value is diagnostic only"},
            "band_snr_db": bsnr,
            "repeats": [],
            "bands": {},
        }
        for k, rep in enumerate(reps):
            chres["repeats"].append({
                "repeat": k,
                "ir_window_s": rep["ir_window_s"],
                "ir_peak_abs_sample": rep["ir_peak_abs_sample"],
                "bands": {fc: {kk: vv for kk, vv in b.items() if kk not in ("edc_db",)}
                          for fc, b in rep["bands"].items()},
            })
        for fc in exc.ANALYSIS_BANDS_HZ:
            rep_list = [rep["bands"][str(int(fc))] for rep in reps]
            t20s_all = [r.get("t20_s") for r in rep_list]
            t20s = [v for v in t20s_all if v is not None]
            e100 = [r["edc100_db"] for r in rep_list if r.get("edc100_db") is not None]
            xc_summary = (None if not xc else {
                "aligned": min(v["aligned"] for v in xc),
                "unaligned": min(v["unaligned"] for v in xc),
                "best_shift_samples": max(abs(v["best_shift_samples"]) for v in xc),
            })
            status, reasons = _gate_band(rep_list, bsnr.get(str(int(fc))), xc_summary, fc, f1)
            edc_curves = [rep["bands"][str(int(fc))].get("edc_db") for rep in reps]
            nmin = min([len(c) for c in edc_curves if c is not None]) if any(
                c is not None for c in edc_curves) else 0
            med_curve = (np.nanmedian(np.vstack([c[:nmin] for c in edc_curves]), axis=0)
                         if nmin > 0 else None)
            decay_block = None
            if decay_fit is not None:
                _cfg = decay_fit if isinstance(decay_fit, dict) else {}
                _sel = _cfg.get("bands")
                if _sel is None or float(fc) in [float(x) for x in _sel]:
                    try:
                        decay_block = _decay_fit_band(edc_curves, rep_list)
                    except Exception as _e:   # optional path must never break the frozen one
                        decay_block = {"status": "DECAY_FIT_ERROR", "error": repr(_e)}
            wf = {}
            for ms in M.WATERFALL_MS:
                key = str(ms)
                vals = [r["waterfall_db"].get(key) for r in rep_list]
                vals = [v for v in vals if v is not None]
                wf[key] = float(np.median(vals)) if vals else None
            chres["bands"][str(int(fc))] = {
                "status": status,
                "reasons": reasons,
                "t20_repeats_s": t20s_all,
                "t20_median_s": float(np.median(t20s)) if t20s else None,
                "t20_range_s": float(max(t20s) - min(t20s)) if len(t20s) > 1 else None,
                "t20_rel_range": (float((max(t20s) - min(t20s)) / np.median(t20s))
                                  if len(t20s) > 1 and np.median(t20s) > 0 else None),
                "r2_repeats": [r.get("r2") for r in rep_list],
                "dr_db_repeats": [r.get("dr_db") for r in rep_list],
                "edc100_repeats_db": e100,
                "edc100_median_db": float(np.median(e100)) if e100 else None,
                "edc100_range_db": float(max(e100) - min(e100)) if len(e100) > 1 else None,
                "record_after_peak_s": [r.get("record_after_peak_s") for r in rep_list],
                "noise_margin_repeats_db": [r.get("noise_margin_db") for r in rep_list],
                "band_snr_db": bsnr.get(str(int(fc))),
                "t20_partial_s": ([v for v in [r.get("t20_partial_s") for r in rep_list]
                                   if v is not None] and
                                  float(np.median([r["t20_partial_s"] for r in rep_list
                                                   if r.get("t20_partial_s") is not None]))),
                "t20_partial_r2": ([v for v in [r.get("t20_partial_r2") for r in rep_list]
                                    if v is not None] and
                                   float(np.median([r["t20_partial_r2"] for r in rep_list
                                                    if r.get("t20_partial_r2") is not None]))),
                "t20_partial_range_db": [r.get("t20_partial_range_db") for r in rep_list],
                "t20_partial_valid": bool(_partial_ok(rep_list)),
                "initial_drop_100ms_db": (None if not e100 else float(-np.median(e100))),
                "decay_slope_db_per_s": (None if not t20s else float(-60.0 / np.median(t20s))),
                "waterfall_db": wf,
                "edc_curve_median": (None if med_curve is None else
                                     [None if not np.isfinite(v) else float(np.round(v, 3))
                                      for v in med_curve[::5]]),
                "edc_curve_step_samples": 5,
                "edc_curve_fs_hz": rep_list[0].get("fs_dec", 4000),
            }
            if decay_block is not None:
                chres["bands"][str(int(fc))]["decay_fit"] = decay_block
        result["channels"][name] = chres
        log("  %-5s pulses=%s  band-SNR40=%.1f dB  T20(63Hz)=%s"
            % (name, [pulses[(name, k)]["abs_sample"] for k in range(layout["repeats"])],
               bsnr.get("40") or float("nan"),
               ("%.2f" % chres["bands"]["63"]["t20_median_s"])
               if chres["bands"]["63"]["t20_median_s"] else "NA"))
    if decay_fit is not None:
        result["decay_fit"] = {
            "enabled": True,
            "tool": "rt-stack/tools/decay-fit",
            "version": "1.0.0",
            "default_off": True,
            "not_standard": True,
            "note": "per-band segmented (1/2/3) decay description attached as "
                    "channels[*].bands[*].decay_fit; the frozen T20 verdicts and all "
                    "other fields are unchanged",
        }
    return result


def _pulse_residuals(positions, interval):
    """IR peak positions with the mean repeat spacing removed (samples)."""
    import numpy as _np
    p = _np.asarray(positions, dtype=float)
    k = _np.arange(p.size)
    if p.size < 2:
        return p - p[0]
    slope = (p[-1] - p[0]) / (p.size - 1)
    resid = p - (p[0] + slope * k)
    return resid


def _partial_ok(rep_list):
    """The supplementary -5..-20 dB slope is only quoted when it is really above the
    noise floor over >=12 dB and the fit is clean."""
    for r in rep_list:
        rng = r.get("t20_partial_range_db")
        if not rng or len(rng) != 2:
            return False
        if (rng[0] - rng[1]) < 11.5:
            return False
        if (r.get("t20_partial_r2") or 0.0) < 0.90:
            return False
        if (r.get("dr_db") or 0.0) < 20.0:
            return False
    return True
