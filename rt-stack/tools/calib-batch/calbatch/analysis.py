"""TSPI 校准批量闭环：分析（前后瀑布/RT/FR/MIMO/holdout + 问题定位 + 报告）。

诚实性边界：
- 所有输出都是**观察量**；classification（IMPROVED/FLAT/WORSE/INSUFFICIENT）不是 PASS/FAIL，
  不替代 rt-stack/spec/mimo-t6.md（T6-M）与 calibration-spec/ACCEPTANCE.md（T6-cal）。
- fake 数据（capture meta simulated=true）只用于离线自测；REPORT 会显式标注 simulated。
- 缺数据 -> INCOMPLETE 并列出 missing；不补数、不推测。
"""

from __future__ import annotations

import json
import math
import os

import numpy as np

from . import config as cfgmod
from . import dsp
from . import measure

ANALYSIS_SCHEMA = "tspi-cal-batch-analysis/v1"
BT = chr(96)

# 观察阈值（不是验收门；见 docs/METRICS-THRESHOLDS.md，变更需升版并标注）
TH = {
    "rt_improve_frac": 0.10,       # 模态 RT 相对下降 >=10% 记一个改善带
    "rt_worsen_frac": 0.05,        # 模态 RT 相对上升 >=5% 记一个恶化带
    "residual_improve_db": 1.0,    # EDC@100ms 降低 >=1 dB 记一个改善带
    "residual_worsen_db": 1.0,     # EDC@100ms 升高 >=1 dB 记一个恶化带
    "sigma_improve_db": 0.5,       # 座间 sigma 降 >=0.5 dB
    "fr_guard_worsen_db": 1.0,     # 任一通道模态带 FR 恶化 >=1 dB -> issue
    "high_guard_worsen_db": 1.0,   # 300Hz 以上平均 FR 恶化 >=1 dB -> issue
    "improve_min_bands": 3,        # 至少 3 个模态带改善
    "improve_min_residual_bands": 2,
    "worsen_min_bands": 3,
    "holdout_consistency_ratio": 0.5,  # holdout 收益 >= design 收益的一半
    "invalid_capture_ratio": 0.2,  # 无效采集超过 20% -> INSUFFICIENT
}
MODAL_BANDS = [40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0]


def _sha(path):
    return cfgmod.sha256_file(path)


def _jsonify(obj):
    if isinstance(obj, dict):
        return {str(k): _jsonify(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_jsonify(v) for v in obj]
    if isinstance(obj, (np.floating, np.integer)):
        return obj.item()
    if isinstance(obj, np.ndarray):
        return [_jsonify(v) for v in obj.tolist()]
    if isinstance(obj, float):
        if math.isnan(obj) or math.isinf(obj):
            return None
        return obj
    return obj


def _write_json(path, obj):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(_jsonify(obj), fh, ensure_ascii=False, indent=2)
        fh.write("\n")


def _mean(vals):
    vals = [v for v in vals if v is not None]
    if not vals:
        return None
    return float(np.mean(vals))


def _std(vals):
    vals = [v for v in vals if v is not None]
    if len(vals) < 2:
        return None
    return float(np.std(vals, ddof=1))


def _pct_change(before, after):
    if before is None or after is None or before == 0:
        return None
    return (after - before) / abs(before)


class Analysis:
    def __init__(self, workdir: str, outdir: str):
        self.workdir = os.path.abspath(workdir)
        self.outdir = os.path.abspath(outdir)
        self.plan = self._load_plan()
        self.metas = {}
        self.extracted = {}
        self.quality_rows = []
        self.issues = []
        self.missing = []
        self.simulated = False

    def _load_plan(self):
        path = os.path.join(self.workdir, "plan.json")
        if not os.path.isfile(path):
            raise SystemExit("缺少 %s（应先 run 或从窗口目录拉回）" % path)
        with open(path, "r", encoding="utf-8") as fh:
            return json.load(fh)

    # ------------------------------------------------------------- load/extract
    def load_captures(self):
        cap_dir = os.path.join(self.workdir, "captures")
        if not os.path.isdir(cap_dir):
            raise SystemExit("缺少 captures 目录: %s" % cap_dir)
        for name in sorted(os.listdir(cap_dir)):
            if not name.endswith(".meta.json"):
                continue
            meta = measure.load_meta(os.path.join(cap_dir, name))
            self.metas[meta["step_id"]] = meta
            self.simulated = self.simulated or bool(meta.get("source") == "fake" or meta.get("simulated"))
        ir_seconds = float(self.plan["analysis"].get("ir_seconds", 1.0))
        for sid, meta in sorted(self.metas.items()):
            ext = measure.extract_capture(meta, ir_seconds=ir_seconds)
            self.extracted[sid] = ext
            for ch, q in ext["quality"]["per_channel"].items():
                self.quality_rows.append({
                    "step": sid, "lane": ext["lane"], "timepoint": ext["timepoint"], "point": ext["point"],
                    "point_role": ext.get("point_role", ""), "channel": ch,
                    "snr_db": q.get("snr_db"), "repeat_mag_spread_max_db": q.get("repeat_mag_spread_max_db"),
                    "repeat_delay_spread_samples": q.get("repeat_delay_spread_samples"),
                    "valid": q.get("valid"), "reasons": "; ".join(q.get("reasons", [])),
                    "capture_simulated": bool(meta.get("source") == "fake"),
                })
            if not ext["ok"]:
                self.missing.append({"step": sid, "point": ext["point"], "lane": ext["lane"],
                                     "timepoint": ext["timepoint"],
                                     "reasons": ext["reasons"]})
        # 记录 capture 文件 hash（证据链）
        captures = []
        for sid, meta in sorted(self.metas.items()):
            for rec in meta.get("recordings", []):
                if os.path.isabs(rec.get("path", "")):
                    p = rec["path"]
                else:
                    p = os.path.join(self.workdir, "steps", self._step_dir_name(sid), rec["path"])
                item = {"step": sid, "path": rec["path"], "exists": os.path.isfile(p)}
                if os.path.isfile(p):
                    item["sha256"] = _sha(p)
                    item["bytes"] = os.path.getsize(p)
                captures.append(item)
        return captures

    def _step_dir_name(self, step_id):
        for s in self.plan["steps"]:
            if s["id"] == step_id:
                return s["dir_name"]
        return ""

    # --------------------------------------------------------------- metrics
    def compute_capture_metrics(self, ext, ir_seconds):
        fs = int(ext["rate"])
        modal = list(self.plan["analysis"].get("modal_bands_hz", MODAL_BANDS))
        rt_bands = list(self.plan["analysis"].get("rt_bands_hz", modal))
        wf_times = list(self.plan["analysis"].get("waterfall_times_ms", [0, 20, 40, 60, 80, 100, 150, 200, 300, 400]))
        out = {"channels": {}, "point": ext["point"], "timepoint": ext["timepoint"], "lane": ext["lane"],
               "valid": ext["quality"]["valid"], "quality": ext["quality"]}
        for ch, ir in ext["irs"].items():
            ch_data = {"rt20_s": {}, "edc100_db": {}, "r2": {}, "rt_valid": {}}
            edc_curves = {}
            for band in rt_bands:
                bir = dsp.band_ir(ir, fs, float(band), 3)
                edc = dsp.edc_db(bir)
                fit = dsp.rt_from_edc(edc, fs)
                ch_data["rt20_s"][str(band)] = fit["rt20_s"] if fit["valid"] else None
                ch_data["r2"][str(band)] = fit["r2"]
                ch_data["rt_valid"][str(band)] = bool(fit["valid"])
                ch_data["edc100_db"][str(band)] = dsp.edc_at_ms(edc, fs, 100.0)
                edc_curves[str(band)] = [dsp.edc_at_ms(edc, fs, float(t)) for t in wf_times]
            freqs, db, _spec = dsp.fr_from_ir(ir, fs, smooth_frac=6)
            grid = dsp.log_freq_grid(20.0, min(16000.0, fs * 0.45), 24)
            fr_vals = [dsp.interp_db_at(freqs, db, float(g)) for g in grid]
            ch_data["fr_db"] = fr_vals
            ch_data["fr_grid_hz"] = grid.tolist()
            ch_data["waterfall"] = {"times_ms": wf_times, "bands_hz": [str(b) for b in modal],
                                    "level_db": edc_curves}
            ch_data["modal_fr_dev_db"] = self._modal_fr_dev(grid, fr_vals, fs)
            out["channels"][ch] = ch_data
        return out

    @staticmethod
    def _modal_fr_dev(grid, vals, fs):
        grid = np.asarray(grid, dtype=np.float64)
        vals = np.asarray(vals, dtype=np.float64)
        ref_band = (grid >= 200.0) & (grid <= 500.0)
        if not np.any(ref_band):
            return None
        ref = float(np.mean(vals[ref_band]))
        modal = (grid >= 40.0) & (grid <= 200.0)
        if not np.any(modal):
            return None
        return float(np.sqrt(np.mean((vals[modal] - ref) ** 2)))

    def build_dataset(self):
        caps = self.load_captures()
        ir_seconds = float(self.plan["analysis"].get("ir_seconds", 1.0))
        per_point = {}
        for sid, ext in self.extracted.items():
            if not ext["ok"]:
                continue
            key = (ext["lane"], ext["timepoint"], ext["point"])
            per_point[key] = self.compute_capture_metrics(ext, ir_seconds)
            per_point[key]["point_role"] = ext.get("point_role", "")
            per_point[key]["pilot_only"] = ext.get("pilot_only", False)
        return caps, per_point

    # ----------------------------------------------------------- aggregation
    @staticmethod
    def _group_points(plan, group):
        if group == "design":
            return [p["id"] for p in plan["design_points"]]
        if group == "holdout":
            return [p["id"] for p in plan["holdout_points"]]
        if group == "pilot":
            return [p["id"] for p in plan.get("pilot_points", [])]
        return []

    def aggregate_group(self, dataset, group):
        """返回 {band: {rt_before, rt_after, residual_before, residual_after, sigma_before, sigma_after}}。"""
        plan = self.plan
        points = self._group_points(plan, group)
        bands = [str(b) for b in plan["analysis"].get("modal_bands_hz", MODAL_BANDS)]
        channels = [c["name"] for c in plan["channels"]]
        out = {}
        for band in bands:
            rt_b = [] ; rt_a = [] ; res_b = [] ; res_a = []
            rt_per_pos_b = {}; rt_per_pos_a = {}
            for pt in points:
                rb = dataset.get(("production" if group != "pilot" else "pilot", "before", pt))
                ra = dataset.get(("production" if group != "pilot" else "pilot", "after", pt))
                if rb is None or ra is None:
                    continue
                vals_b = [rb["channels"][ch]["rt20_s"].get(band) for ch in channels if ch in rb["channels"]]
                vals_a = [ra["channels"][ch]["rt20_s"].get(band) for ch in channels if ch in ra["channels"]]
                e_b = [rb["channels"][ch]["edc100_db"].get(band) for ch in channels if ch in rb["channels"]]
                e_a = [ra["channels"][ch]["edc100_db"].get(band) for ch in channels if ch in ra["channels"]]
                rt_b.extend(v for v in vals_b if v is not None)
                rt_a.extend(v for v in vals_a if v is not None)
                res_b.extend(v for v in e_b if v is not None)
                res_a.extend(v for v in e_a if v is not None)
                rt_per_pos_b[pt] = _mean(vals_b)
                rt_per_pos_a[pt] = _mean(vals_a)
            out[band] = {
                "rt_before": _mean(rt_b), "rt_after": _mean(rt_a),
                "rt_delta_pct": _pct_change(_mean(rt_b), _mean(rt_a)),
                "residual_before": _mean(res_b), "residual_after": _mean(res_a),
                "residual_delta_db": (None if (_mean(res_a) is None or _mean(res_b) is None)
                                      else _mean(res_a) - _mean(res_b)),
                "sigma_before": _std([v for v in rt_per_pos_b.values()]),
                "sigma_after": _std([v for v in rt_per_pos_a.values()]),
                "sigma_delta": (None if (_std(list(rt_per_pos_b.values())) is None or _std(list(rt_per_pos_a.values())) is None)
                                else _std(list(rt_per_pos_a.values())) - _std(list(rt_per_pos_b.values()))),
                "positions_before": rt_per_pos_b, "positions_after": rt_per_pos_a,
                "n_rt_before": len(rt_b), "n_rt_after": len(rt_a),
            }
        return out

    def aggregate_fr(self, dataset, group):
        plan = self.plan
        points = self._group_points(plan, group)
        channels = [c["name"] for c in plan["channels"]]
        per_ch = {}
        for ch in channels:
            before_vals = [] ; after_vals = []
            for pt in points:
                rb = dataset.get(("production" if group != "pilot" else "pilot", "before", pt))
                ra = dataset.get(("production" if group != "pilot" else "pilot", "after", pt))
                if rb and ch in rb["channels"]:
                    before_vals.append(np.asarray(rb["channels"][ch]["fr_db"], dtype=np.float64))
                if ra and ch in ra["channels"]:
                    after_vals.append(np.asarray(ra["channels"][ch]["fr_db"], dtype=np.float64))
            if not before_vals or not after_vals:
                continue
            grid = np.asarray(dataset[next(iter(dataset))]["channels"][ch]["fr_grid_hz"], dtype=np.float64) \
                if dataset else np.array([])
            m_b = np.mean(np.vstack(before_vals), axis=0)
            m_a = np.mean(np.vstack(after_vals), axis=0)
            sigma_b = np.std(np.vstack(before_vals), axis=0, ddof=1) if len(before_vals) > 1 else np.zeros_like(m_b)
            sigma_a = np.std(np.vstack(after_vals), axis=0, ddof=1) if len(after_vals) > 1 else np.zeros_like(m_a)
            per_ch[ch] = {"grid_hz": grid.tolist(), "before_db": m_b.tolist(), "after_db": m_a.tolist(),
                          "sigma_before_db": sigma_b.tolist(), "sigma_after_db": sigma_a.tolist()}
        # 汇总指标
        summary = {}
        for ch, data in per_ch.items():
            g = np.asarray(data["grid_hz"]); b = np.asarray(data["before_db"]); a = np.asarray(data["after_db"])
            modal = (g >= 40) & (g <= 200); high = (g >= 300) & (g <= 10000)
            ref = (g >= 200) & (g <= 500)
            if not np.any(modal) or not np.any(ref):
                continue
            ref_b = float(np.mean(b[ref])); ref_a = float(np.mean(a[ref]))
            dev_b = float(np.sqrt(np.mean((b[modal] - ref_b) ** 2)))
            dev_a = float(np.sqrt(np.mean((a[modal] - ref_a) ** 2)))
            # 高带"平坦度"观察量：相对自身 300-10k 均值；校正应降低起伏，而不是看整带平移
            if np.any(high):
                flat_b = float(np.std(b[high]))
                flat_a = float(np.std(a[high]))
                high_mean_b = float(np.mean(b[high]))
                high_mean_a = float(np.mean(a[high]))
            else:
                flat_b = flat_a = high_mean_b = high_mean_a = None
            summary[ch] = {
                "modal_dev_before_db": dev_b, "modal_dev_after_db": dev_a,
                "modal_dev_delta_db": dev_a - dev_b,
                "high_mean_before_db": high_mean_b,
                "high_mean_after_db": high_mean_a,
                "high_delta_db": (None if (high_mean_a is None or high_mean_b is None)
                                  else high_mean_a - high_mean_b),
                "high_flat_before_db": flat_b,
                "high_flat_after_db": flat_a,
                "high_flat_delta_db": (None if (flat_a is None or flat_b is None) else flat_a - flat_b),
                "sigma_before_mean_db": float(np.mean(data["sigma_before_db"])) if data["sigma_before_db"] else None,
                "sigma_after_mean_db": float(np.mean(data["sigma_after_db"])) if data["sigma_after_db"] else None,
            }
        return {"per_channel": per_ch, "summary": summary}

    # --------------------------------------------------------- compare/report
    def classify(self, design_rt, design_fr, holdout_rt):
        improve_bands = 0
        worsen_bands = 0
        residual_improve = 0
        residual_worsen = 0
        for band, m in design_rt.items():
            if m.get("rt_delta_pct") is not None and m["rt_delta_pct"] <= -TH["rt_improve_frac"]:
                improve_bands += 1
            if m.get("rt_delta_pct") is not None and m["rt_delta_pct"] >= TH["rt_worsen_frac"]:
                worsen_bands += 1
            if m.get("residual_delta_db") is not None and m["residual_delta_db"] <= -TH["residual_improve_db"]:
                residual_improve += 1
            if m.get("residual_delta_db") is not None and m["residual_delta_db"] >= TH["residual_worsen_db"]:
                residual_worsen += 1
        sigma_improve = sum(1 for m in design_rt.values()
                            if m.get("sigma_delta") is not None and m["sigma_delta"] <= -TH["sigma_improve_db"])
        holdout_consistent = None
        design_pct = _mean([m.get("rt_delta_pct") for m in design_rt.values()])
        holdout_pct = _mean([m.get("rt_delta_pct") for m in holdout_rt.values()]) if holdout_rt else None
        if design_pct is not None and holdout_pct is not None:
            if design_pct < 0:
                holdout_consistent = holdout_pct <= design_pct * TH["holdout_consistency_ratio"]
            else:
                holdout_consistent = holdout_pct <= 0.0
        classification = "FLAT"
        reasons = []
        if improve_bands >= TH["improve_min_bands"] and residual_improve >= TH["improve_min_residual_bands"]:
            classification = "IMPROVED"
            reasons.append("RT 改善带 %d >= %d 且余振改善带 %d >= %d"
                           % (improve_bands, TH["improve_min_bands"], residual_improve, TH["improve_min_residual_bands"]))
        if worsen_bands >= TH["worsen_min_bands"] or residual_worsen >= TH["improve_min_residual_bands"]:
            classification = "WORSE"
            reasons.append("RT 恶化带 %d / 余振恶化带 %d 超阈值" % (worsen_bands, residual_worsen))
        if improve_bands > 0 and classification == "FLAT":
            reasons.append("局部改善但未达 IMPROVED 门槛（改善带 %d，余振 %d）" % (improve_bands, residual_improve))
        return {
            "classification": classification,
            "improve_bands": improve_bands,
            "worsen_bands": worsen_bands,
            "residual_improve_bands": residual_improve,
            "residual_worsen_bands": residual_worsen,
            "sigma_improve_bands": sigma_improve,
            "design_rt_delta_pct_mean": design_pct,
            "holdout_rt_delta_pct_mean": holdout_pct,
            "holdout_consistent": holdout_consistent,
            "reasons": reasons,
            "thresholds": dict(TH),
            "claim": "observation only; NOT an acceptance gate (mimo-t6 T6-M / ACCEPTANCE T6-cal remain authority)",
        }

    def detect_issues(self, design_rt, design_fr, dataset):
        issues = []
        channels = [c["name"] for c in self.plan["channels"]]
        for band, m in design_rt.items():
            if m.get("rt_delta_pct") is not None and m["rt_delta_pct"] >= TH["rt_worsen_frac"]:
                issues.append({"kind": "modal_rt_worsened", "band_hz": float(band), "channel": "*",
                               "metric": "rt_delta_pct", "before": m["rt_before"], "after": m["rt_after"],
                               "delta": m["rt_delta_pct"]})
            if m.get("residual_delta_db") is not None and m["residual_delta_db"] >= TH["residual_worsen_db"]:
                issues.append({"kind": "modal_residual_worsened", "band_hz": float(band), "channel": "*",
                               "metric": "edc100_delta_db", "before": m["residual_before"],
                               "after": m["residual_after"], "delta": m["residual_delta_db"]})
        for ch, s in design_fr.get("summary", {}).items():
            if s.get("modal_dev_delta_db") is not None and s["modal_dev_delta_db"] >= TH["fr_guard_worsen_db"]:
                issues.append({"kind": "modal_fr_dev_worsened", "band_hz": "40-200", "channel": ch,
                               "metric": "modal_fr_dev_delta_db", "before": s["modal_dev_before_db"],
                               "after": s["modal_dev_after_db"], "delta": s["modal_dev_delta_db"]})
            if s.get("high_flat_delta_db") is not None and s["high_flat_delta_db"] >= TH["high_guard_worsen_db"]:
                issues.append({"kind": "high_band_flatness_worsened", "band_hz": "300-10000", "channel": ch,
                               "metric": "high_flat_delta_db", "before": s["high_flat_before_db"],
                               "after": s["high_flat_after_db"], "delta": s["high_flat_delta_db"]})
        # 通道级 RT 恶化（用 design 平均）
        for ch in channels:
            for band in [str(b) for b in self.plan["analysis"].get("modal_bands_hz", MODAL_BANDS)]:
                rb = []; ra = []
                for pt in self._group_points(self.plan, "design"):
                    d_b = dataset.get(("production", "before", pt))
                    d_a = dataset.get(("production", "after", pt))
                    if d_b and ch in d_b["channels"]:
                        v = d_b["channels"][ch]["rt20_s"].get(band)
                        if v is not None:
                            rb.append(v)
                    if d_a and ch in d_a["channels"]:
                        v = d_a["channels"][ch]["rt20_s"].get(band)
                        if v is not None:
                            ra.append(v)
                if len(rb) >= 2 and len(ra) >= 2:
                    pct = _pct_change(_mean(rb), _mean(ra))
                    if pct is not None and pct >= TH["rt_worsen_frac"] * 2:
                        issues.append({"kind": "channel_rt_worsened", "band_hz": float(band), "channel": ch,
                                       "metric": "rt_delta_pct", "before": _mean(rb), "after": _mean(ra),
                                       "delta": pct})
        issues.sort(key=lambda x: abs(x.get("delta") or 0), reverse=True)
        return issues[:20]

    # ------------------------------------------------------------------ write
    def write_outputs(self, caps, dataset):
        os.makedirs(self.outdir, exist_ok=True)
        design_rt = self.aggregate_group(dataset, "design")
        holdout_rt = self.aggregate_group(dataset, "holdout")
        pilot_rt = self.aggregate_group(dataset, "pilot") if self._group_points(self.plan, "pilot") else {}
        design_fr = self.aggregate_fr(dataset, "design")
        holdout_fr = self.aggregate_fr(dataset, "holdout")
        verdict = self.classify(design_rt, design_fr, holdout_rt)
        issues = self.detect_issues(design_rt, design_fr, dataset)
        n_total = len(self.metas)
        n_invalid = sum(1 for ext in self.extracted.values() if not ext["quality"]["valid"])
        if n_total == 0 or (n_total and n_invalid / float(n_total) > TH["invalid_capture_ratio"]):
            verdict["classification"] = "INSUFFICIENT"
            verdict["reasons"].append("有效采集不足：invalid %d/%d（阈值 %.0f%%）"
                                      % (n_invalid, n_total, TH["invalid_capture_ratio"] * 100))
        # 文件：quality.csv
        qpath = os.path.join(self.outdir, "quality.csv")
        with open(qpath, "w", encoding="utf-8") as fh:
            fh.write("step,lane,timepoint,point,point_role,channel,snr_db,repeat_mag_spread_max_db,"
                     "repeat_delay_spread_samples,valid,reasons,capture_simulated\n")
            for row in self.quality_rows:
                fh.write(",".join(str(row.get(k, "")) for k in
                                  ("step", "lane", "timepoint", "point", "point_role", "channel", "snr_db",
                                   "repeat_mag_spread_max_db", "repeat_delay_spread_samples", "valid", "reasons",
                                   "capture_simulated")) + "\n")
        # 文件：fr / rt / waterfall
        fr_dir = os.path.join(self.outdir, "fr")
        rt_dir = os.path.join(self.outdir, "rt")
        wf_dir = os.path.join(self.outdir, "waterfall")
        for (lane, timepoint, point), data in sorted(dataset.items()):
            group = "pilot" if lane == "pilot" else ("design" if point in [p["id"] for p in self.plan["design_points"]] else "holdout")
            os.makedirs(os.path.join(fr_dir, group), exist_ok=True)
            os.makedirs(os.path.join(rt_dir, group), exist_ok=True)
            os.makedirs(os.path.join(wf_dir, group), exist_ok=True)
            names = list(data["channels"].keys())
            if names:
                grid = data["channels"][names[0]]["fr_grid_hz"]
                with open(os.path.join(fr_dir, group, "%s-%s.csv" % (point, timepoint)), "w", encoding="utf-8") as fh:
                    fh.write("freq_hz," + ",".join(names) + "\n")
                    for i, f in enumerate(grid):
                        fh.write("%.4f," % float(f) + ",".join("%.3f" % data["channels"][ch]["fr_db"][i] for ch in names) + "\n")
                bands = [str(b) for b in self.plan["analysis"].get("modal_bands_hz", MODAL_BANDS)]
                with open(os.path.join(rt_dir, group, "%s-%s.csv" % (point, timepoint)), "w", encoding="utf-8") as fh:
                    fh.write("band_hz," + ",".join("%s_rt20_s" % ch for ch in names) + ","
                             + ",".join("%s_edc100_db" % ch for ch in names) + "\n")
                    for band in bands:
                        fh.write("%s," % band + ",".join(str(data["channels"][ch]["rt20_s"].get(band, "")) for ch in names)
                                 + "," + ",".join("%.3f" % data["channels"][ch]["edc100_db"].get(band, float("nan")) for ch in names) + "\n")
                # waterfall 明细（每通道）
                for ch in names:
                    wf = data["channels"][ch]["waterfall"]
                    with open(os.path.join(wf_dir, group, "%s-%s-%s.csv" % (point, timepoint, ch)), "w", encoding="utf-8") as fh:
                        fh.write("time_ms," + ",".join(wf["bands_hz"]) + "\n")
                        for ti, tm in enumerate(wf["times_ms"]):
                            fh.write("%s," % tm + ",".join("%.3f" % wf["level_db"][b][ti] for b in wf["bands_hz"]) + "\n")
        # 聚合 waterfall（design 平均：每通道取各点位平均 EDC 曲线）
        agg_wf = {}
        channels = [c["name"] for c in self.plan["channels"]]
        bands = [str(b) for b in self.plan["analysis"].get("modal_bands_hz", MODAL_BANDS)]
        times = list(self.plan["analysis"].get("waterfall_times_ms", [0, 20, 40, 60, 80, 100, 150, 200, 300, 400]))
        for ch in channels:
            curves = {b: [] for b in bands}
            for pt in self._group_points(self.plan, "design"):
                for tp in ("before", "after"):
                    d = dataset.get(("production", tp, pt))
                    if d and ch in d["channels"]:
                        for b in bands:
                            curves[b].append(d["channels"][ch]["waterfall"]["level_db"][b])
            agg_wf[ch] = {b: (np.mean(np.vstack(v), axis=0).tolist() if v else None) for b, v in curves.items()}
        # 图：RT 前后（design 模态带均值）与 waterfall 热图
        plots = os.path.join(self.outdir, "plots")
        rt_series = {}
        for label, agg in (("before", "rt_before"), ("after", "rt_after")):
            pts = []
            for band in bands:
                v = design_rt.get(band, {}).get(agg)
                if v is not None:
                    pts.append((float(band), float(v)))
            rt_series[label] = pts
        if rt_series.get("before") and rt_series.get("after"):
            dsp.svg_decay_plot(os.path.join(plots, "rt20-design.svg"), rt_series,
                               "modal RT20 before/after (design avg)", y_label="RT20 (s)",
                               y_min=0.0, y_max=max(1.0, max(y for _, y in rt_series["before"] + rt_series["after"]) * 1.2),
                               x_max=max(bands and [float(b) for b in bands]) or 250.0, x_unit="Hz")
        # waterfall 热图：取 -100 ms 相对电平（余振）矩阵，band × position 平均
        wf_ch = "SUB1" if "SUB1" in agg_wf else channels[0]
        matrix = []
        for b in bands:
            row = []
            for tp in ("before", "after"):
                vals = []
                for pt in self._group_points(self.plan, "design"):
                    d = dataset.get(("production", tp, pt))
                    if d and wf_ch in d["channels"]:
                        lb = d["channels"][wf_ch]["waterfall"]["level_db"][b]
                        idx = times.index(100) if 100 in times else min(4, len(lb) - 1)
                        vals.append(lb[idx])
                row.append(_mean(vals))
            matrix.append(row)
        if any(v is not None for row in matrix for v in row):
            dsp.svg_heatmap(os.path.join(plots, "waterfall-%s-before-after.svg" % wf_ch),
                            ["before", "after"], bands, [[v if v is not None else -200.0 for v in row] for row in matrix],
                            "CSD residual @100ms: %s (dB re direct)" % wf_ch, vmin=-40.0, vmax=0.0)
        comparison = {
            "schema": ANALYSIS_SCHEMA,
            "batch_label": self.plan["batch_label"],
            "round": self.plan["round"],
            "simulated": bool(self.simulated),
            "fs": self.plan["fs"],
            "layout": self.plan.get("layout", ""),
            "verdict": verdict,
            "issues": issues,
            "modal_bands_hz": [float(b) for b in bands],
            "waterfall_times_ms": times,
            "design": {"rt": design_rt, "fr": design_fr},
            "holdout": {"rt": holdout_rt, "fr": holdout_fr},
            "pilot": {"rt": pilot_rt, "pilot_only": True,
                      "claim": self.plan["pilot"].get("claim", "pilot only")},
            "mimo": self._mimo_summary(design_rt, holdout_rt),
            "quality": {"captures": n_total, "invalid": n_invalid,
                        "invalid_ratio": (n_invalid / float(n_total)) if n_total else None,
                        "missing": self.missing},
            "not_run": self.plan.get("not_run", []),
            "observation_only": True,
            "claim": "前后瀑布/RT/站间sigma/MIMO 对比是观察量；不是验收门。真实板端数据 NOT_RUN。",
        }
        _write_json(os.path.join(self.outdir, "comparison.json"), comparison)
        report = self.render_report(comparison, dataset, agg_wf, caps)
        with open(os.path.join(self.outdir, "REPORT.md"), "w", encoding="utf-8") as fh:
            fh.write(report)
        with open(os.path.join(self.workdir, "batch-report.json"), "w", encoding="utf-8") as fh:
            json.dump(_jsonify(comparison), fh, ensure_ascii=False, indent=2)
            fh.write("\n")
        manifest = self.write_manifest(caps, comparison)
        return {"comparison": comparison, "manifest": manifest, "report": report}

    def _mimo_summary(self, design_rt, holdout_rt):
        bands = list(design_rt.keys())
        rt_before = _mean([design_rt[b]["rt_before"] for b in bands])
        rt_after = _mean([design_rt[b]["rt_after"] for b in bands])
        sig_b = _mean([design_rt[b]["sigma_before"] for b in bands])
        sig_a = _mean([design_rt[b]["sigma_after"] for b in bands])
        return {
            "modal_rt_mean_before_s": rt_before,
            "modal_rt_mean_after_s": rt_after,
            "modal_rt_delta_pct": _pct_change(rt_before, rt_after),
            "modal_sigma_rt_mean_before": sig_b,
            "modal_sigma_rt_mean_after": sig_a,
            "modal_sigma_delta": (None if (sig_b is None or sig_a is None) else sig_a - sig_b),
            "holdout_modal_rt_delta_pct": _mean([holdout_rt[b]["rt_delta_pct"] for b in holdout_rt]) if holdout_rt else None,
            "scope": "MIMO/模态均衡效果只在 20-300 Hz 声称；pilot 数据不进入本表。",
        }

    def write_manifest(self, caps, comparison):
        inputs = []
        for item in caps:
            inputs.append(dict(item, role="capture"))
        for item in self.plan.get("inputs", []):
            inputs.append(dict(item, role=item.get("role", "plan_input")))
        outputs = []
        for root, _dirs, files in os.walk(self.outdir):
            for name in sorted(files):
                p = os.path.join(root, name)
                outputs.append({"path": os.path.relpath(p, self.outdir).replace("\\", "/"),
                                "sha256": _sha(p), "bytes": os.path.getsize(p)})
        manifest = {
            "schema": "tspi-cal-batch-manifest/v1",
            "created_at": plan_now(),
            "workdir": self.workdir,
            "outdir": self.outdir,
            "plan_sha256": self.plan.get("plan_sha256"),
            "simulated": bool(self.simulated),
            "inputs": inputs,
            "outputs": outputs,
            "commands": {
                "plan": "tspi_cal_batch.py plan --config <cfg> --round %s --out <plan_dir>" % self.plan["round"],
                "run": "tspi_cal_batch.py run --plan <plan_dir>/plan.json --transport <cmd|fake> --work %s" % self.workdir,
                "analyze": "tspi_cal_batch.py analyze --work %s --out %s" % (self.workdir, self.outdir),
            },
            "not_run": self.plan.get("not_run", []),
            "verdict_summary": comparison.get("verdict", {}).get("classification"),
            "observation_only": True,
        }
        _write_json(os.path.join(self.outdir, "MANIFEST.json"), manifest)
        lines = ["# MANIFEST（%s round %s）" % (self.plan["batch_label"], self.plan["round"]), "",
                 "- plan_sha256: %s" % self.plan.get("plan_sha256"),
                 "- simulated: %s" % ("true（fake 离线自测，不是真实测量）" if self.simulated else "false"),
                 "- verdict: %s（observation only，不是验收门）" % comparison.get("verdict", {}).get("classification"),
                 "", "## 输入", "", "| path | role | sha256 | bytes |", "|---|---|---|---|"]
        for it in inputs:
            lines.append("| %s | %s | %s | %s |" % (it.get("path", ""), it.get("role", ""),
                                                    (it.get("sha256") or "")[:16], it.get("bytes", "")))
        lines += ["", "## 输出", "", "| path | sha256 | bytes |", "|---|---|---|"]
        for o in outputs:
            lines.append("| %s | %s | %d |" % (o["path"], o["sha256"][:16], o["bytes"]))
        lines += ["", "## NOT_RUN", ""]
        for nr in self.plan.get("not_run", []):
            lines.append("- **%s**：%s —— %s" % (nr.get("id"), nr.get("what"), nr.get("why")))
        mpath = os.path.join(self.outdir, "MANIFEST.md")
        with open(mpath, "w", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")
        return {"path": mpath, "outputs": len(outputs), "inputs": len(inputs)}

    # -------------------------------------------------------------- report
    def render_report(self, comparison, dataset, agg_wf, caps):
        v = comparison["verdict"]
        design = comparison["design"]["rt"]
        holdout = comparison["holdout"]["rt"]
        mimo = comparison["mimo"]
        lines = []
        lines.append("# 校准批量闭环报告（前后瀑布/RT/MIMO/holdout）")
        lines.append("")
        lines.append("- batch: %s · round: %s" % (comparison["batch_label"], comparison["round"]))
        lines.append("- 数据来源: **%s**" % ("fake 离线合成（仅验证流水线；真实板端 NOT_RUN）"
                                          if comparison["simulated"] else "板端采集（需窗口回传）"))
        lines.append("- 输出目录: %s" % self.outdir)
        lines.append("")
        lines.append("## 1. 一页结论（观察量，不是验收门）")
        lines.append("")
        lines.append("- 分类: **%s**（IMPROVED/FLAT/WORSE/INSUFFICIENT 只描述本次观察）" % v["classification"])
        lines.append("- 原因: %s" % "; ".join(v.get("reasons") or ["无"]))
        lines.append("- 模态 RT 均值: %.3f s -> %.3f s（变化 %s）"
                     % (mimo.get("modal_rt_mean_before_s") or 0.0, mimo.get("modal_rt_mean_after_s") or 0.0,
                        _fmt_pct(mimo.get("modal_rt_delta_pct"))))
        lines.append("- holdout 模态 RT 变化: %s（一致性: %s）"
                     % (_fmt_pct(mimo.get("holdout_modal_rt_delta_pct")),
                        ("是" if v.get("holdout_consistent") else ("否" if v.get("holdout_consistent") is False else "数据不足"))))
        lines.append("- 问题定位: %d 条%s" % (len(comparison["issues"]),
                                        "（见 §6）" if comparison["issues"] else "（无超阈值恶化）"))
        lines.append("- 边界: 这是 %s；**不得**据此声称 T6-M/T6-cal PASS。" %
                     ("合成数据自测" if comparison["simulated"] else "板端实测的观察量"))
        lines.append("")
        lines.append("## 2. 方法")
        lines.append("")
        lines.append("- 激励: 指数扫频（Farina），每通道/点位 %d 遍中位 IR；布局与 slot 时间线由 plan 冻结。"
                     % int(self.plan["sweep"].get("repeats", 3)))
        lines.append("- IR -> FR（1/6 oct 平滑）；1/3 oct 带通 -> Schroeder EDC -> T20 拟合外推 RT60；")
        lines.append("  EDC@100ms 作为余振观察量；瀑布/CSD 用 1/3 oct 带 EDC 在 %s ms 的采样矩阵。"
                     % comparison["waterfall_times_ms"])
        lines.append("- 三麦固定数据单独 lane（pilot_only），不进入 design/holdout/MIMO 结论。")
        lines.append("")
        lines.append("## 3. 前后对比（design 点平均）")
        lines.append("")
        lines.append("| band Hz | RT20 before (s) | RT20 after (s) | Δ% | EDC@100 before (dB) | after (dB) | Δ (dB) | 座间σ before | after |")
        lines.append("|---|---|---|---|---|---|---|---|---|")
        for b in comparison["modal_bands_hz"]:
            m = design.get(str(b)) or design.get(b)
            if not m:
                continue
            lines.append("| %.0f | %s | %s | %s | %s | %s | %s | %s | %s |" % (
                float(b), _fmt(m["rt_before"], 3), _fmt(m["rt_after"], 3), _fmt_pct(m["rt_delta_pct"]),
                _fmt(m["residual_before"], 1), _fmt(m["residual_after"], 1), _fmt(m["residual_delta_db"], 2),
                _fmt(m["sigma_before"], 3), _fmt(m["sigma_after"], 3)))
        lines.append("")
        lines.append("## 4. MIMO / 模态均衡效果（20-300 Hz，观察）")
        lines.append("")
        lines.append("- 模态 RT 均值: %s -> %s（%s）" % (_fmt(mimo["modal_rt_mean_before_s"], 3),
                                                  _fmt(mimo["modal_rt_mean_after_s"], 3),
                                                  _fmt_pct(mimo["modal_rt_delta_pct"])))
        lines.append("- 座间 σ(RT): %s -> %s（Δ %s）" % (_fmt(mimo["modal_sigma_rt_mean_before"], 4),
                                                   _fmt(mimo["modal_sigma_rt_mean_after"], 4),
                                                   _fmt(mimo["modal_sigma_delta"], 4)))
        lines.append("- %s" % mimo["scope"])
        lines.append("")
        lines.append("## 5. holdout（独立验证点，不参与 apply 拟合）")
        lines.append("")
        lines.append("| band Hz | RT before | RT after | Δ% | 余振 before | after |")
        lines.append("|---|---|---|---|---|---|")
        for b in comparison["modal_bands_hz"]:
            m = holdout.get(str(b)) or holdout.get(b)
            if not m:
                continue
            lines.append("| %.0f | %s | %s | %s | %s | %s |" % (
                float(b), _fmt(m["rt_before"], 3), _fmt(m["rt_after"], 3), _fmt_pct(m["rt_delta_pct"]),
                _fmt(m["residual_before"], 1), _fmt(m["residual_after"], 1)))
        lines.append("")
        lines.append("- holdout 只作泛化观察；若 holdout 无改善或反向，说明前次改善可能过拟合/位置依赖（不自动判 FAIL）。")
        lines.append("")
        lines.append("## 6. 问题定位（供算法迭代）")
        lines.append("")
        if comparison["issues"]:
            lines.append("| # | kind | band | channel | metric | before | after | delta |")
            lines.append("|---|---|---|---|---|---|---|---|")
            for i, it in enumerate(comparison["issues"], 1):
                lines.append("| %d | %s | %s | %s | %s | %s | %s | %s |" % (
                    i, it["kind"], it["band_hz"], it["channel"], it["metric"],
                    _fmt(it["before"], 3), _fmt(it["after"], 3), _fmt(it["delta"], 3)))
            lines.append("")
            lines.append("- 定位规则：模态带 RT 恶化 >=5% 或 EDC@100ms 恶化 >=1 dB（按频带/通道/点位分组）；")
            lines.append("  通道级额外看 FR 模态带偏差与 >300 Hz 平均电平漂移；按 delta 排序取前 20。")
        else:
            lines.append("- 无超阈值恶化项（不代表真实房间一定没问题；真实数据 NOT_RUN）。")
        lines.append("")
        lines.append("## 7. 数据质量")
        lines.append("")
        q = comparison["quality"]
        lines.append("- 采集数: %s；无效: %s（%.0f%%）；缺失: %d。" % (
            q["captures"], q["invalid"], (q["invalid_ratio"] or 0) * 100, len(q["missing"])))
        if q["missing"]:
            for m in q["missing"][:10]:
                lines.append("  - %s/%s/%s: %s" % (m["lane"], m["timepoint"], m["point"], "; ".join(m["reasons"])))
        lines.append("- 质量门：SNR>=25 dB、重复 delay 散差<=0.5 sample、重复 FR 散差<=0.8 dB（positions.rs 口径；")
        lines.append("  本工具观察门，不等同 T6/ACCEPTANCE 硬门）。")
        lines.append("")
        lines.append("## 8. 产物索引")
        lines.append("")
        lines.append("- 每点位每通道 FR: %sfr/<group>/%s" % (BT, BT))
        lines.append("- 每点位每通道 RT/EDC: %srt/<group>/%s" % (BT, BT))
        lines.append("- 瀑布/CSD 明细: %swaterfall/<group>/%s；聚合图: %splots/waterfall-*.svg%s" % (BT, BT, BT, BT))
        lines.append("- 前后对比 JSON: %scomparison.json%s；质量: %squality.csv%s；证据链: %sMANIFEST.json%s" % (BT, BT, BT, BT, BT, BT))
        lines.append("")
        lines.append("## 9. NOT_RUN / 诚实性")
        lines.append("")
        for nr in comparison["not_run"]:
            lines.append("- **%s**：%s —— %s" % (nr.get("id"), nr.get("what"), nr.get("why")))
        lines.append("- 本报告不写 PASS/FAIL；观察分类不产生 T6-M/T6-cal 判定。")
        lines.append("- 合成数据结果不得用于声称算法有效；真实房间结论需窗口执行 + 独立复算。")
        lines.append("")
        return "\n".join(lines) + "\n"


def _fmt(value, decimals):
    if value is None:
        return "n/a"
    try:
        return ("%." + str(decimals) + "f") % float(value)
    except (TypeError, ValueError):
        return "n/a"


def _fmt_pct(value):
    if value is None:
        return "n/a"
    return "%.1f%%" % (float(value) * 100.0)


def plan_now():
    import datetime
    return datetime.datetime.now().astimezone().isoformat(timespec="seconds")


def analyze_workdir(workdir: str, outdir: str) -> dict:
    a = Analysis(workdir, outdir)
    caps, dataset = a.build_dataset()
    result = a.write_outputs(caps, dataset)
    return {"comparison": result["comparison"], "manifest": result["manifest"],
            "outdir": outdir, "verdict": result["comparison"]["verdict"]["classification"]}
