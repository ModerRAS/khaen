# -*- coding: utf-8 -*-
"""CSV / figure writers for the decay-fit evidence directory."""
import json
import os

import numpy as np


def _default(o):
    if isinstance(o, (np.floating,)):
        return float(o)
    if isinstance(o, (np.integer,)):
        return int(o)
    if isinstance(o, np.ndarray):
        return o.tolist()
    if isinstance(o, (np.bool_,)):
        return bool(o)
    return str(o)


def write_json(obj, path):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(obj, f, indent=1, ensure_ascii=False, default=_default)
    return path


def _write_csv(path, header, rows):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(",".join(header) + "\n")
        for r in rows:
            f.write(",".join("" if v is None else
                             ("%.6g" % v if isinstance(v, float) else str(v)) for v in r) + "\n")
    return path


def write_per_repeat_csv(results, path):
    rows = []
    for tag, bands in results["per_repeat"].items():
        group = tag.split("_")[0]
        for b, d in bands.items():
            fits = d.get("plateau_fit") or {}
            sel = fits.get("selected") or {}
            m2 = (fits.get("models") or {}).get("2") or {}
            m3 = (fits.get("models") or {}).get("3") or {}
            segs = fits.get("segments") or []
            bx = fits.get("biexp") or {}
            fx = fits.get("fixed_split") or {}
            fe = fx.get("early") or {}
            fl = fx.get("late") or {}
            ck = fits.get("curvature_knee") or {}
            knee = fits.get("knee") or {}
            t0w = (fits.get("fit_range") or {}).get("t_lo_s") or 0.0
            kn = knee.get("t_abs_s", (knee.get("t_s") + t0w) if knee.get("t_s") is not None else None)
            if kn is None and knee.get("knees"):
                kn = knee["knees"][0]["t_s"]
            rows.append([
                tag, group, float(b),
                d["edc"]["noise_method"], d["edc"]["dr_db"], d["edc"]["edc100_db"],
                d["edc"]["plateau_spread_db"],
                d["frozen_control"]["t20_s"], d["frozen_control"]["r2"],
                sel.get("n_seg"), sel.get("status"),
                fits.get("t20_seg_s")[0] if fits.get("t20_seg_s") else None,
                fits.get("t20_seg_s")[1] if fits.get("t20_seg_s") and len(fits["t20_seg_s"]) > 1 else None,
                fits.get("t20_seg_s")[2] if fits.get("t20_seg_s") and len(fits["t20_seg_s"]) > 2 else None,
                1000.0 * kn if kn is not None else None,
                (knee.get("level_db") if "level_db" in knee else
                 (knee["knees"][0]["level_db"] if knee.get("knees") else None)),
                segs[0]["r2"] if len(segs) > 0 else None,
                segs[1]["r2"] if len(segs) > 1 else None,
                segs[2]["r2"] if len(segs) > 2 else None,
                fits.get("r2"),
                (m2.get("bic_eff")), (m3.get("bic_eff")),
                ((m2.get("delta") or {}).get("d_bic")),
                ((m2.get("delta") or {}).get("d_r2")),
                bx.get("status"), bx.get("tau1_s"), bx.get("tau2_s"), bx.get("a1"),
                (1000.0 * bx["knee_time_s"]) if bx.get("knee_time_s") is not None else None,
                fe.get("t20_s"), fe.get("r2"), fl.get("t20_s"), fl.get("r2"),
                (1000.0 * ck["t_abs_s"]) if ck.get("t_abs_s") is not None else None,
                ck.get("level_db"), ck.get("slope_ratio"),
                d["frozen_control"]["t20_s"],
            ])
    header = ["tag", "group", "band_hz", "noise_method", "dr_db", "edc100_db",
              "plateau_spread_db", "frozen_t20_s", "frozen_r2", "n_seg", "sel_status",
              "t20_seg1_s", "t20_seg2_s", "t20_seg3_s", "knee_abs_ms", "knee_level_db",
              "seg1_r2", "seg2_r2", "seg3_r2", "fit_r2",
              "bic_2seg", "bic_3seg", "d_bic_2", "d_r2_2",
              "biexp_status", "tau1_s", "tau2_s", "a1", "biexp_knee_ms",
              "fixed_early_t20_s", "fixed_early_r2", "fixed_late_t20_s", "fixed_late_r2",
              "curv_knee_abs_ms", "curv_knee_level_db", "curv_slope_ratio",
              "frozen_t20_s"]
    return _write_csv(path, header, rows)


def write_bands_csv(results, path):
    rows = []
    for group, bands in results["bands"].items():
        for band, d in bands.items():
            if not d:
                continue
            std = d.get("gate_standard") or {}
            seg = d.get("gate_segmented") or {}
            fxd = d.get("gate_fixed_split") or {}
            fe = fxd.get("early") or {}
            fl = fxd.get("late") or {}
            ck = fxd.get("knee") or {}
            sg = seg.get("segments") or []
            kn = seg.get("knees") or []
            rows.append([
                float(band), group, d.get("n_repeats"),
                std.get("status"), std.get("t20_median_s"), std.get("t20_range_s"),
                std.get("t20_rel_range"),
                seg.get("status"), seg.get("n_seg"),
                sg[0].get("t20_median_s") if len(sg) > 0 else None,
                sg[0].get("t20_range_s") if len(sg) > 0 else None,
                sg[1].get("t20_median_s") if len(sg) > 1 else None,
                sg[1].get("t20_range_s") if len(sg) > 1 else None,
                sg[2].get("t20_median_s") if len(sg) > 2 else None,
                sg[0].get("r2_min") if len(sg) > 0 else None,
                sg[1].get("r2_min") if len(sg) > 1 else None,
                kn[0].get("t_median_abs_s") if len(kn) > 0 else None,
                kn[0].get("t_range_ms") if len(kn) > 0 else None,
                ";".join(seg.get("reasons") or []),
                fxd.get("status"), fxd.get("t20_ratio_late_over_early"),
                fe.get("t20_median_s"), fe.get("t20_range_s"), fe.get("r2_min"),
                fl.get("t20_median_s"), fl.get("t20_range_s"), fl.get("r2_min"),
                ck.get("knee_t_median_abs_ms"), ck.get("knee_t_range_ms"),
                ck.get("slope_ratio_median"),
            ])
    header = ["band_hz", "group", "n_repeats", "std_gate", "std_t20_med_s", "std_t20_range_s",
              "std_t20_rel", "seg_gate", "n_seg",
              "seg1_t20_med_s", "seg1_t20_range_s", "seg2_t20_med_s", "seg2_t20_range_s",
              "seg3_t20_med_s", "seg1_r2_min", "seg2_r2_min",
              "knee_med_abs_s", "knee_range_ms", "reasons",
              "fixed_gate", "fixed_t20_ratio", "fixed_early_t20_s", "fixed_early_range_s",
              "fixed_early_r2_min", "fixed_late_t20_s", "fixed_late_range_s",
              "fixed_late_r2_min", "curv_knee_med_ms", "curv_knee_range_ms",
              "curv_slope_ratio"]
    return _write_csv(path, header, rows)


def write_model_selection_csv(results, path):
    rows = []
    for tag, bands in results["per_repeat"].items():
        for b, d in bands.items():
            for fitkind in ("plateau_fit",):
                f = d.get(fitkind) or {}
                for n in ("1", "2", "3"):
                    m = (f.get("models") or {}).get(n)
                    if not m:
                        continue
                    rows.append([tag, float(b), fitkind, n, m.get("bic_eff"),
                                 m.get("aic_eff"), m.get("r2"), m.get("p"),
                                 (m.get("delta") or {}).get("d_bic"),
                                 (m.get("delta") or {}).get("d_r2"),
                                 "yes" if (m.get("evidence_ok") if n != "1" else None) else
                                 ("no" if n != "1" else ""),
                                 ";".join(m.get("ident_reasons") or []),
                                 (f.get("selected") or {}).get("n_seg"),
                                 (f.get("selected") or {}).get("status")])
    header = ["tag", "band_hz", "fit", "n_seg", "bic_eff", "aic_eff", "r2", "params",
              "d_bic", "d_r2", "evidence_ok", "ident_reasons", "selected_n_seg",
              "selected_status"]
    return _write_csv(path, header, rows)


def write_objective_csv(sugg, path):
    rows = []
    for b in sugg["bands"]:
        rows.append([b.get("band_hz"), b.get("decay_class"), b.get("gate_status"),
                     b.get("n_seg"), b.get("knee_ms"), b.get("t20_early_s"),
                     b.get("t20_late_s"), b.get("severity_factor"), b.get("actionability"),
                     b.get("alpha_energy"), b.get("alpha_severity"),
                     b.get("alpha_recommended"), b.get("target_reduction_db"),
                     b.get("target_residual_ratio"), b.get("prefer_early_energy_cut"),
                     b.get("give_up_late")])
    header = ["band_hz", "decay_class", "gate_status", "n_seg", "knee_ms", "t20_early_s",
              "t20_late_s", "severity_factor", "actionability", "alpha_energy",
              "alpha_severity", "alpha_recommended", "target_reduction_db",
              "target_residual_ratio", "prefer_early_energy_cut", "give_up_late"]
    return _write_csv(path, header, rows)


def make_figures(results, outdir, bands=(125.0, 200.0)):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    os.makedirs(outdir, exist_ok=True)
    figs = []
    # ---- EDC + selected fit, 2x2
    fig, axes = plt.subplots(2, 2, figsize=(12.0, 8.0), sharex=False)
    for i, group in enumerate(("RAW", "S2")):
        for j, b in enumerate(bands):
            ax = axes[i][j]
            gd = results["bands"].get(group, {}).get(str(int(b)), {})
            curves = gd.get("edc_curves") or []
            for tag, t, y in curves:
                ax.plot(t, y, lw=0.8, alpha=0.6, label=tag if j == 0 else None)
            fitline = gd.get("fit_overlay") or {}
            for tag, t, y in fitline.get("model", []):
                ax.plot(t, y, "k--", lw=1.4, label="selected piecewise" if tag == "model" else None)
            for tag, t, y in fitline.get("biexp", []):
                ax.plot(t, y, "r:", lw=1.2, label="double exp" if tag == "biexp" else None)
            seg = gd.get("gate_segmented") or {}
            for k in seg.get("knees") or []:
                if k.get("t_median_abs_s") is not None:
                    ax.axvline(k["t_median_abs_s"], color="tab:green", lw=0.9, ls="-.",
                               label="piecewise knee")
            cka = gd.get("curv_knee_median_abs_s")
            if cka:
                ax.axvline(cka, color="magenta", lw=0.9, ls=":",
                           label="slope-drop knee")
            segs = seg.get("segments") or []
            xmax = 0.6
            if segs:
                v = segs[-1].get("t20_median_s")
                if v:
                    xmax = max(0.6, min(2.5, 3.0 * v))
            ax.set_xlim(0, xmax)
            ax.set_ylim(-45, 2)
            ax.set_title("%s  %d Hz  %s (n_seg=%s)" % (group, b, seg.get("status"), seg.get("n_seg")))
            ax.set_xlabel("time after broadband peak (s)")
            ax.set_ylabel("EDC (dB)")
            ax.grid(alpha=0.3)
            if i == 0 and j == 0:
                h, l = ax.get_legend_handles_labels()
                if h:
                    ax.legend(fontsize=7, loc="lower left")
    fig.suptitle("decay-fit: Schroeder EDC + auto-selected piecewise model (dashed) / double exp (dotted)")
    fig.tight_layout(rect=(0, 0, 1, 0.96))
    p = os.path.join(outdir, "edc-fit-125-200.png")
    fig.savefig(p, dpi=130)
    plt.close(fig)
    figs.append(p)
    # ---- segment stability
    fig, ax = plt.subplots(1, 1, figsize=(11.0, 5.0))
    width = 0.09
    for j, b in enumerate(bands):
        for gi, (group, color) in enumerate((("RAW", "tab:blue"), ("S2", "tab:orange"))):
            d = results["bands"].get(group, {}).get(str(int(b)), {}) or {}
            seg = d.get("gate_segmented") or {}
            for k, s in enumerate(seg.get("segments") or []):
                ys = [v for v in (s.get("t20_repeats_s") or []) if v is not None]
                if not ys:
                    continue
                x0 = j + (gi - 0.5) * 0.34 + (k - 1) * width
                ax.scatter([x0] * len(ys), ys, s=24, color=color, alpha=0.85,
                           label=(group + " seg%d" % (k + 1)) if j == 0 else None)
                ax.hlines(float(np.median(ys)), x0 - width * 0.45, x0 + width * 0.45,
                          color=color, lw=2)
    ax.set_xticks(range(len(bands)))
    ax.set_xticklabels(["%d Hz" % float(b) for b in bands])
    ax.set_xlim(-0.5, len(bands) - 0.5)
    ax.set_ylabel("segment T20 (s)  [non-standard, log]")
    ax.set_yscale("log")
    ax.set_ylim(0.08, 8.0)
    ax.set_title("per-segment T20 across repeats (dots) + median (bar); "
                 "standard T20 is reported separately")
    h, l = ax.get_legend_handles_labels()
    if h:
        ax.legend(fontsize=7, ncol=3, loc="upper left")
    ax.grid(alpha=0.3)
    fig.tight_layout()
    p = os.path.join(outdir, "segment-stability.png")
    fig.savefig(p, dpi=130)
    plt.close(fig)
    figs.append(p)
    return figs
