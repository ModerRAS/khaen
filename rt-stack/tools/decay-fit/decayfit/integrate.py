# -*- coding: utf-8 -*-
"""Opt-in adapter for the frozen ir-diag acceptance pipeline (default OFF).

The frozen ir-diag pipeline (tools/ir-diag) keeps the per-repeat band EDC curves in
memory while aggregating a band; it does not keep them in the result JSON.  This
adapter consumes those curves and returns the segmented-decay block that the opt-in
--decay-fit flag attaches to each band.  Nothing is imported or executed unless the
caller explicitly enables it, so the default pipeline result is unchanged.

Public pure function: band_decay_fit(edc_curves, fs, noise_margins_db) -> summary dict.
"""
import numpy as np

from . import gates as G
from . import models as MF


def _median(vals):
    v = [x for x in vals if x is not None]
    return float(np.median(v)) if v else None


def band_decay_fit(edc_curves, fs, noise_margins_db=None):
    """Fit 1/2/3-segment models to the per-repeat EDC curves of one band.

    edc_curves : list of per-repeat EDC_dB arrays (same length or None)
    noise_margins_db : per-repeat dB margin of the -25 dB point above the noise
        floor, as already computed by the frozen pipeline; a repeat with
        margin < FIT_NOISE_MARGIN_DB is marked NO_COVERAGE (same rule as T20).
    Returns a compact, JSON-safe summary including the (non-standard) segmented gate.
    """
    fits = []
    for i, c in enumerate(edc_curves):
        if c is None:
            fits.append(None)
            continue
        f = MF.fit_piecewise(np.asarray(c, dtype=np.float64), float(fs))
        nm = None if noise_margins_db is None or i >= len(noise_margins_db) else noise_margins_db[i]
        if f is not None and f.get("status") == "OK" and nm is not None \
                and nm < MF.FIT_NOISE_MARGIN_DB:
            f = {"status": "NO_COVERAGE",
                 "reason": "-25 dB point only %.1f dB above the noise floor "
                           "(needs >= %.0f dB)" % (nm, MF.FIT_NOISE_MARGIN_DB),
                 "noise_margin_db": nm}
        fits.append(f)
    gate = G.segmented_gate(fits)
    ok = [f for f in fits if f is not None and f.get("status") == "OK"]
    out = {
        "not_standard": True,
        "note": "segmented decay description; NOT the spec T20. The standard T20 "
                "verdict of this band is unchanged.",
        "n_repeats": len(fits),
        "model_votes": [((f or {}).get("selected") or {}).get("n_seg")
                        if (f or {}).get("status") == "OK" else None for f in fits],
        "gate": gate,
    }
    if ok:
        sel = ok[0]["selected"]["n_seg"]
        out["n_seg"] = sel
        out["t20_seg_median_s"] = [_median([(f.get("t20_seg_s") or [None] * sel)[k]
                                            if len(f.get("t20_seg_s") or []) > k else None
                                            for f in ok]) for k in range(sel)]
        knees = []
        for k in range(max(0, sel - 1)):
            vals = []
            for f in ok:
                kn = f.get("knee") or {}
                if kn.get("knees"):
                    vals.append(kn["knees"][k]["t_s"])
                elif kn.get("t_s") is not None:
                    vals.append(kn["t_s"])
            knees.append(_median(vals))
        out["knee_ms_median"] = [None if v is None else 1000.0 * v for v in knees]
        r2 = []
        for k in range(sel):
            vals = [(f.get("segments") or [{}] * sel)[k].get("r2") if len(f.get("segments") or []) > k else None
                    for f in ok]
            r2.append(_median(vals))
        out["r2_seg_median"] = r2
        bx = []
        for f in ok:
            b = f.get("biexp") or {}
            bx.append(b)
        out["biexp"] = {"n_status_ok": sum(1 for b in bx if b.get("status") == "OK"),
                        "n_status_unidentifiable": sum(1 for b in bx
                                                       if b.get("status") == "UNIDENTIFIABLE"),
                        "tau1_s_median": _median([b.get("tau1_s") for b in bx
                                                  if b.get("status") == "OK"]),
                        "tau2_s_median": _median([b.get("tau2_s") for b in bx
                                                  if b.get("status") == "OK"]),
                        "a1_median": _median([b.get("a1") for b in bx
                                              if b.get("status") == "OK"]),
                        "knee_time_ms_median": (lambda v: None if v is None else 1000.0 * v)(
                            _median([b.get("knee_time_s") for b in bx
                                     if b.get("status") == "OK"]))}
    return out
