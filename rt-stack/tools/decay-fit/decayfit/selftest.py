# -*- coding: utf-8 -*-
"""Offline self-test of the decay-fit models, gate and objective weight rules.

Runs on synthetic EDC curves with known ground truth.  No measurement data needed.
    py -3 -m decayfit.selftest      (or  py -3 selftest.py)
Exit code 0 = all checks passed.
"""
import sys

import numpy as np

from . import gates as G
from . import models as MF
from . import objective as O

FS = 4000.0
RNG = np.random.default_rng(20261121)


def true_piecewise(t, breaks, slopes, y0=0.0):
    y = np.zeros_like(t)
    prev_t, prev_y = 0.0, y0
    for i, s in enumerate(slopes):
        tb = breaks[i] if i < len(breaks) else t[-1] + 1.0
        m = (t >= prev_t) & (t < tb)
        y[m] = prev_y + s * (t[m] - prev_t)
        prev_y = prev_y + s * (tb - prev_t)
        prev_t = tb
    return y


def edc_single(t20_s, noise_db=0.05):
    y = true_piecewise(t, [], [-60.0 / t20_s])
    return y + RNG.normal(0.0, noise_db, y.size)


def edc_two(t20_early, t20_late, knee_level_db, noise_db=0.05):
    sl1 = -60.0 / t20_early
    tk = knee_level_db / sl1
    y = true_piecewise(t, [tk], [sl1, -60.0 / t20_late])
    return y + RNG.normal(0.0, noise_db, y.size)


def edc_three(slopes, knee_levels, noise_db=0.05):
    breaks = []
    y = 0.0
    prev_t = 0.0
    for i, lv in enumerate(knee_levels):
        dt = (lv - y) / slopes[i]
        breaks.append(prev_t + dt)
        y = lv
        prev_t = breaks[-1]
    return true_piecewise(t, breaks, slopes) + RNG.normal(0.0, noise_db, t.size)


t = np.arange(0.0, 1.5, 1.0 / FS)
CHECKS = []


def check(name, cond, detail=""):
    CHECKS.append((name, bool(cond), detail))
    print("%-58s %s %s" % (name, "PASS" if cond else "FAIL", detail))


# 1) single slope -----------------------------------------------------------
y = edc_single(0.50)
r = MF.fit_piecewise(y, FS)
t20 = r["control_single_slope"]["t20_s"]
check("single slope recovery", r["selected"]["n_seg"] == 1 and abs(t20 - 0.50) < 0.01,
      "n_seg=%d T20=%.4f" % (r["selected"]["n_seg"], t20))

# 2) two segments, knee at -15 dB ------------------------------------------
y = edc_two(0.18, 0.45, -15.0)
r = MF.fit_piecewise(y, FS)
sel = r["selected"]
m2 = r["models"].get("2") or {}
t20e, t20l = (m2.get("t20_seg_s") or [None, None])[:2]
knee_t = m2.get("knee", {}).get("t_s")
knee_lv = m2.get("knee", {}).get("level_db")
check("two-segment selected (OK)", sel["n_seg"] == 2 and sel["status"] == "OK",
      "status=%s reasons=%s" % (sel["status"], sel["reasons"]))
check("two-segment T20_early/late recovered",
      t20e is not None and abs(t20e - 0.18) < 0.01 and abs(t20l - 0.45) < 0.02,
      "T20=%.4f/%.4f" % (t20e or 0, t20l or 0))
check("knee time/level recovered", knee_lv is not None and abs(knee_lv + 15.0) < 1.0,
      "knee=%.4fs %.2f dB" % (knee_t or 0, knee_lv or 0))
check("segments R2 >= 0.95", all(s["r2"] >= 0.95 for s in m2.get("segments") or []),
      str([round(s["r2"], 4) for s in m2.get("segments") or []]))

# 3) three segments ---------------------------------------------------------
y = edc_three([-120.0, -40.0, -110.0], [-12.0, -18.0])
r = MF.fit_piecewise(y, FS)
sel = r["selected"]
m3 = r["models"].get("3") or {}
check("three-segment selected", sel["n_seg"] == 3, "n_seg=%d status=%s" % (sel["n_seg"], sel["status"]))
check("three-segment slopes recovered",
      m3.get("slopes_db_per_s") is not None
      and abs(m3["slopes_db_per_s"][0] + 120) < 12 and abs(m3["slopes_db_per_s"][1] + 40) < 8
      and abs(m3["slopes_db_per_s"][2] + 110) < 15,
      str([round(s, 1) for s in (m3.get("slopes_db_per_s") or [])]))

# 4) ambiguous: knee too weak to identify ----------------------------------
y = edc_two(0.45, 0.50, -15.0)      # 11% slope difference, below SLOPE_RATIO_MIN
r = MF.fit_piecewise(y, FS)
sel = r["selected"]
check("weak knee -> not a clean two-segment PASS",
      not (sel["n_seg"] == 2 and sel["status"] == "OK"),
      "n_seg=%s status=%s" % (sel["n_seg"], sel["status"]))

# 5) double exponential recovery ------------------------------------------
tt = np.arange(0.0, 3.0, 1.0 / FS)
e = 0.7 * np.exp(-tt / 0.015) + 0.3 * np.exp(-tt / 0.250)
y = 10.0 * np.log10(e) + RNG.normal(0.0, 0.03, tt.size)
# mimic the real fit window: only -5..-25 dB is handed to fit_biexp through fit_piecewise
r = MF.fit_piecewise(y, FS)
bx = r["biexp"]
check("biexp identifiable", bx.get("status") == "OK", "status=%s reasons=%s"
      % (bx.get("status"), bx.get("reasons")))
check("biexp tau2 recovered (<=25%)", bx.get("tau2_s") is not None
      and abs(bx["tau2_s"] - 0.25) / 0.25 <= 0.25, "tau2=%s" % bx.get("tau2_s"))
check("biexp tau ratio > 1.6", (bx.get("tau_ratio") or 0) > 1.6, "ratio=%s" % bx.get("tau_ratio"))

# 6) single exponential -> biexp unidentifiable ---------------------------
y = edc_single(0.30, noise_db=0.03)
r = MF.fit_piecewise(y, FS)
check("single exp -> biexp UNIDENTIFIABLE", r["biexp"].get("status") == "UNIDENTIFIABLE",
      str(r["biexp"].get("reasons")))

# 7) segmented gate: stable 3 repeats pass, unstable 3 repeats fail --------
fits = [MF.fit_piecewise(edc_two(0.18, 0.45, -15.0), FS) for _ in range(3)]
g = G.segmented_gate(fits)
check("segmented gate stable -> PASS_SEGMENTED", g.get("status") == "PASS_SEGMENTED",
      "%s %s" % (g.get("status"), g.get("reasons")))
fits = [MF.fit_piecewise(edc_two(0.18, 0.45 * f, -15.0), FS) for f in (1.0, 1.15, 0.85)]
g = G.segmented_gate(fits)
check("segmented gate 15% late variation -> FAIL/AMBIGUOUS",
      g.get("status") in ("FAIL_SEGMENTED", "AMBIGUOUS"), g.get("status"))

# 8) standard gate stays the control, unchanged ----------------------------
g = G.standard_t20_gate([0.70, 0.80, 0.83], [0.95, 0.96, 0.95])
check("standard T20 gate flags 15% spread", g["status"] == "UNRELIABLE",
      str(g["reasons"]))

# 9) objective weights: sum=K, clamp respected, severity monotone ----------
energy = {b: -40.0 for b in O.BAND_CENTERS_HZ}
energy[125.0] = -30.0
alpha = O.energy_weights(energy)
check("alpha_energy sums to K", abs(sum(alpha.values()) - len(alpha)) < 1e-9,
      "sum=%.6f K=%d" % (sum(alpha.values()), len(alpha)))
check("alpha_energy positive, loud band weighted highest",
      all(v > 0 for v in alpha.values())
      and alpha[125.0] == max(alpha.values())
      and abs(alpha[125.0] / alpha[40.0] - 4.0) < 1e-6,
      str([round(v, 3) for v in alpha.values()]))
late = {b: 0.4 for b in O.BAND_CENTERS_HZ}
late[40.0] = 0.8
late[125.0] = 0.2
a_sev, sev = O.severity_weights(energy, late, {})
check("severity ordered by T20_late", sev[40.0] > sev[125.0],
      "sev40=%.2f sev125=%.2f" % (sev[40.0], sev[125.0]))
check("alpha_severity sums to K", abs(sum(a_sev.values()) - len(a_sev)) < 1e-9,
      "sum=%.6f" % sum(a_sev.values()))

# 10) canonical fixed split (-5..-15 / -15..-25) evaluation -------------------
fits = [MF.fit_piecewise(edc_two(0.18, 0.45, -15.0), FS) for _ in range(3)]
fx = G.evaluate_fixed_split(fits)
fe = (fx.get("early") or {}).get("t20_median_s")
fl = (fx.get("late") or {}).get("t20_median_s")
check("fixed split -> PASS_FIXED_TWO", fx.get("status") == "PASS_FIXED_TWO",
      "%s %s" % (fx.get("status"), fx.get("reasons")))
check("fixed split T20s recovered", fe is not None and abs(fe - 0.18) < 0.01
      and fl is not None and abs(fl - 0.45) < 0.02, "early=%.4f late=%.4f" % (fe or 0, fl or 0))

# 11) single slope -> fixed split reports SINGLE_SLOPE -----------------------
fits = [MF.fit_piecewise(edc_single(0.50), FS) for _ in range(3)]
fx = G.evaluate_fixed_split(fits)
check("fixed split on single slope -> SINGLE_SLOPE", fx.get("status") == "SINGLE_SLOPE",
      "%s ratio=%s" % (fx.get("status"), fx.get("t20_ratio_late_over_early")))

# 12) max-curvature knee locates a sharp knee -------------------------------
r = MF.fit_piecewise(edc_two(0.18, 0.45, -15.0), FS)
ck = r.get("curvature_knee") or {}
check("slope-drop knee near piecewise knee (+/-20 ms)",
      ck.get("t_s") is not None and abs(ck["t_s"] - 0.030) < 0.02,
      "knee=%.4fs piecewise=%.4fs ratio=%s" % (ck.get("t_s") or 0, 0.030,
                                               None if ck.get("slope_ratio") is None
                                               else round(ck["slope_ratio"], 2)))

# 13) gate statuses are never a T20 pass -----------------------------------
check("segmented gate marked non-standard",
      G.segmented_gate(fits).get("not_standard_note", "").lower().find("not the spec t20") >= 0)

failed = [c for c in CHECKS if not c[1]]
print("\n%d/%d checks passed" % (len(CHECKS) - len(failed), len(CHECKS)))
RETURN_CODE = 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(RETURN_CODE)
