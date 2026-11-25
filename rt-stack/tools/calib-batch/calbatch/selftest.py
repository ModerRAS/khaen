"""TSPI 校准批量闭环：离线端到端自测（fake 合成数据）。

证明目标（对应 task-201 第 4 条）：
1) plan 生成 + fail-closed 校验能拒绝非法配置（design<6、电平越窗、pilot 混入、cal sha 不符）。
2) fake transport 能一次跑完 baseline -> apply -> after -> pilot（不人工干预）。
3) analyze 能输出 FR/瀑布/RT/MIMO 前后对比/holdout，并在"变好/变差/不变"三个合成场景下
   分别给出 IMPROVED / WORSE（含问题定位）/ FLAT。
4) 所有真实板端项标 NOT_RUN。

本自测只验证流水线与指标机器可判，不验证真实校准算法（真实结果一律 NOT_RUN）。
"""

from __future__ import annotations

import json
import os
import shutil
import tempfile
import time

from . import analysis, config as cfgmod, plan as planmod, runner

SELFTEST_SCHEMA = "tspi-cal-batch-selftest/v1"


def _base_config(repo_root: str, with_commands: bool = True) -> dict:
    cfg = {
        "schema": cfgmod.CONFIG_SCHEMA,
        "batch_label": "selftest",
        "selftest": True,
        "layout": "4.1.2",
        "fs": 16000,
        "drive_level_dbfs": -50.0,
        "rounds": 1,
        "window": {"quiet_hours_ack": True, "audible_sweep_authorized": True,
                   "rounds": {"A": "morning"}},
        "sweep": {"start_hz": 20.0, "end_hz": 7000.0, "seconds": 0.6,
                  "pre_seconds": 0.2, "post_seconds": 0.2,
                  "repeat_gap_seconds": 0.5, "slot_gap_seconds": 0.1, "fade_ms": 8.0, "repeats": 3},
        "mic": {"model": "UMIK-1", "serial": "718-7696",
                "cal_file": "calibration-spec/mics/umik1_718-7696_90deg.txt",
                "cal_sha256": "49482D8773AC5809B07D4A00B027058876AE6F6E3BB821532C483B2CD5588130",
                "orientation": "90deg", "pilot_only": False},
        "channels": [dict(c) for c in cfgmod.DEFAULT_CHANNELS_4_1_2],
        "design_points": [{"id": "P%d" % i, "label": "design-%d" % i, "role": "design",
                           "weight": 1.0, "repeats": 3} for i in range(1, 7)],
        "holdout_points": [{"id": "P%d" % i, "label": "holdout-%d" % i, "role": "validation",
                            "weight": 1.0, "repeats": 3} for i in range(7, 10)],
        "pilot": {"enabled": True, "pilot_only": True,
                  "claim": "three-mic fixed pilot; not production multi-point; not eligible for T67/T68/T69/T6-M",
                  "points": [{"id": "PILOT1", "label": "fixed-three-mic", "repeats": 3}],
                  "mics": [{"name": "UMIK", "device": "umik"},
                           {"name": "MA1", "device": "ma1", "capture2_channel": 0},
                           {"name": "ARC", "device": "arc", "capture2_channel": 1}]},
        "estimates": {"move_seconds_per_point": 30.0, "apply_seconds": 60.0, "preflight_seconds": 5.0,
                      "analyze_seconds_per_capture": 1.0, "report_seconds": 5.0, "margin_fraction": 0.15},
        "analysis": {"ir_seconds": 0.9, "peq_fit_band_hz": [60.0, 6000.0], "peq_max_stages": 6},
        "capture_lead_in_seconds": 0.35,
    }
    if with_commands:
        cfg["commands"] = {
            "preflight": {"argv": ["bash", "{prep_dir}/preflight.sh"], "timeout_s": 60},
            "measure": {"argv": ["khaen-calibrate", "measure-multi"], "timeout_s": 60},
            "apply": {"argv": ["bash", "{prep_dir}/apply.sh"], "timeout_s": 60},
        }
    return cfg


def _run_pipeline(cfg: dict, repo_root: str, scenario: str, base_tmp: str) -> dict:
    tag = scenario
    plan_dir = os.path.join(base_tmp, "plan-" + tag)
    work_dir = os.path.join(base_tmp, "work-" + tag)
    out_dir = os.path.join(base_tmp, "analysis-" + tag)
    plan = planmod.build_plan(cfg, "A", repo_root, scenario=scenario)
    if plan["validation"]["errors"]:
        raise RuntimeError("selftest plan 校验失败: %s" % plan["validation"]["errors"])
    planmod.write_plan(plan, plan_dir)
    run = runner.run_plan(os.path.join(plan_dir, "plan.json"), work_dir, transport="fake",
                          scenario=scenario)
    if run["status"] != "ok":
        raise RuntimeError("selftest run 失败: %s" % json.dumps(run.get("failure"), ensure_ascii=False))
    result = analysis.analyze_workdir(work_dir, out_dir)
    return {"plan": plan, "plan_dir": plan_dir, "work_dir": work_dir, "out_dir": out_dir,
            "run": run, "analysis": result}


def _load_json(path):
    with open(path, "r", encoding="utf-8") as fh:
        return json.load(fh)


def _check(checks, name, ok, detail=""):
    checks.append({"name": name, "ok": bool(ok), "detail": str(detail)})


def _negative_plan_checks(cfg: dict, repo_root: str, checks: list) -> None:
    # 负向用例必须在"生产模式"下校验（selftest 模式按设计跳过 fs/level/cal 硬门）
    prod = json.loads(json.dumps(cfg))
    prod["selftest"] = False
    prod["fs"] = 48000
    prod["sweep"]["end_hz"] = 20000.0
    prod["analysis"]["peq_fit_band_hz"] = [60.0, 8000.0]
    cases = []
    c = json.loads(json.dumps(prod)); c["design_points"] = c["design_points"][:5]
    cases.append(("design5_rejected", c, cfgmod.E_BATCH_POINTS))
    c = json.loads(json.dumps(prod)); c["drive_level_dbfs"] = -20.0
    cases.append(("level_window_rejected", c, cfgmod.E_BATCH_LEVEL))
    c = json.loads(json.dumps(prod)); c["pilot"]["points"] = [{"id": "P1", "repeats": 3}]
    cases.append(("pilot_mixed_rejected", c, cfgmod.E_BATCH_PILOT))
    c = json.loads(json.dumps(prod)); c["mic"]["cal_sha256"] = "0" * 64
    cases.append(("cal_sha_rejected", c, cfgmod.E_BATCH_CAL))
    c = json.loads(json.dumps(prod)); c["fs"] = 44100
    cases.append(("fs_44100_rejected", c, cfgmod.E_BATCH_FS))
    for name, case_cfg, want in cases:
        norm = cfgmod.normalize_config(case_cfg)
        val = cfgmod.validate_config(norm, repo_root)
        codes = [e["code"] for e in val["errors"]]
        _check(checks, "negative_plan_" + name, want in codes,
               "want %s got %s" % (want, codes))


def run_selftest(outdir: str, keep: bool = False, scenarios=("improved", "worse", "flat")) -> dict:
    started = time.time()
    repo_root = cfgmod.repo_root_from(os.path.dirname(os.path.abspath(__file__)))
    base_tmp = tempfile.mkdtemp(prefix="tspi-cal-batch-selftest-")
    checks = []
    results = {}
    errors = []
    try:
        cfg = _base_config(repo_root)
        _negative_plan_checks(cfg, repo_root, checks)
        # 场景流水线
        for scenario in scenarios:
            t0 = time.time()
            try:
                res = _run_pipeline(cfg, repo_root, scenario, base_tmp)
                results[scenario] = res
            except Exception as exc:  # noqa: BLE001
                errors.append("%s: %s" % (scenario, exc))
                _check(checks, "pipeline_" + scenario, False, str(exc))
                continue
            comp = _load_json(os.path.join(res["out_dir"], "comparison.json"))
            v = comp["verdict"]
            dt = time.time() - t0
            results[scenario]["elapsed_s"] = round(dt, 1)
            results[scenario]["verdict"] = v["classification"]
            if scenario == "improved":
                _check(checks, "improved_classification", v["classification"] == "IMPROVED", v["classification"])
                _check(checks, "improved_rt_delta_negative",
                       (comp["mimo"]["modal_rt_delta_pct"] is not None
                        and comp["mimo"]["modal_rt_delta_pct"] <= -0.10),
                       comp["mimo"]["modal_rt_delta_pct"])
                _check(checks, "improved_holdout_present",
                       comp["holdout"]["rt"] and comp["mimo"]["holdout_modal_rt_delta_pct"] is not None,
                       comp["mimo"]["holdout_modal_rt_delta_pct"])
                _check(checks, "improved_issues_le_3", len(comp["issues"]) <= 3, len(comp["issues"]))
            elif scenario == "worse":
                _check(checks, "worse_classification", v["classification"] == "WORSE", v["classification"])
                _check(checks, "worse_has_issues", len(comp["issues"]) > 0, len(comp["issues"]))
                kinds = set(it["kind"] for it in comp["issues"])
                _check(checks, "worse_issue_kinds_modal",
                       any(k.startswith("modal") for k in kinds), sorted(kinds))
            elif scenario == "flat":
                _check(checks, "flat_classification", v["classification"] == "FLAT", v["classification"])
            # 产物完整性
            out = res["out_dir"]
            required = ["REPORT.md", "comparison.json", "MANIFEST.json", "MANIFEST.md", "quality.csv"]
            for name in required:
                _check(checks, "%s_has_%s" % (scenario, name), os.path.isfile(os.path.join(out, name)))
            plots = os.path.join(out, "plots")
            _check(checks, "%s_has_plots" % scenario,
                   os.path.isdir(plots) and len(os.listdir(plots)) >= 1,
                   os.listdir(plots) if os.path.isdir(plots) else "no plots")
            wf_group = os.path.join(out, "waterfall", "design")
            _check(checks, "%s_has_waterfall_csv" % scenario,
                   os.path.isdir(wf_group) and len(os.listdir(wf_group)) > 0,
                   len(os.listdir(wf_group)) if os.path.isdir(wf_group) else 0)
            rt_group = os.path.join(out, "rt", "design")
            _check(checks, "%s_has_rt_csv" % scenario,
                   os.path.isdir(rt_group) and len(os.listdir(rt_group)) > 0,
                   len(os.listdir(rt_group)) if os.path.isdir(rt_group) else 0)
            fr_group = os.path.join(out, "fr", "design")
            _check(checks, "%s_has_fr_csv" % scenario,
                   os.path.isdir(fr_group) and len(os.listdir(fr_group)) > 0,
                   len(os.listdir(fr_group)) if os.path.isdir(fr_group) else 0)
            # pilot 分离/模拟标注/观察量口径
            man = _load_json(os.path.join(out, "MANIFEST.json"))
            _check(checks, "%s_manifest_simulated" % scenario, man.get("simulated") is True)
            _check(checks, "%s_observation_only" % scenario, comp.get("observation_only") is True)
            _check(checks, "%s_pilot_only" % scenario,
                   comp["pilot"].get("pilot_only") is True and "not eligible" in (comp["pilot"].get("claim") or ""))
            # plan/时间预算/NOT_RUN
            pj = _load_json(os.path.join(res["plan_dir"], "plan.json"))
            _check(checks, "%s_plan_steps" % scenario, len(pj["steps"]) >= 20, len(pj["steps"]))
            _check(checks, "%s_plan_commands_txt" % scenario,
                   os.path.isfile(os.path.join(res["plan_dir"], "commands.txt")))
            _check(checks, "%s_plan_estimate" % scenario,
                   pj["estimate"]["total_minutes"] > 0, pj["estimate"]["total_minutes"])
            _check(checks, "%s_not_run_marked" % scenario, len(pj.get("not_run", [])) >= 1)
            # apply 只吃 design 点（holdout 隔离）
            applied = _load_json(os.path.join(res["work_dir"], "applied-state.json"))
            used = set(applied.get("fitted_from_design_points") or [])
            holdout_ids = set(p["id"] for p in pj["holdout_points"])
            _check(checks, "%s_holdout_not_in_fit" % scenario, not (used & holdout_ids),
                   sorted(used & holdout_ids))
        # 样例证据必须在 temp 清理前复制（improved 场景的 plan/analysis 目录此时仍存在）
        _copy_sample(results, outdir, repo_root)
    finally:
        if not keep:
            shutil.rmtree(base_tmp, ignore_errors=True)

    ok = len(errors) == 0 and all(c["ok"] for c in checks)
    result = {
        "schema": SELFTEST_SCHEMA,
        "tool": "tspi-cal-batch",
        "created_at": analysis.plan_now(),
        "ok": ok,
        "checks_total": len(checks),
        "checks_pass": sum(1 for c in checks if c["ok"]),
        "checks_fail": [c for c in checks if not c["ok"]],
        "checks": checks,
        "scenarios": {k: {"verdict": v.get("verdict"), "elapsed_s": v.get("elapsed_s"),
                          "plan_dir": v.get("plan_dir"), "work_dir": v.get("work_dir"),
                          "out_dir": v.get("out_dir")} for k, v in results.items()},
        "errors": errors,
        "elapsed_s": round(time.time() - started, 1),
        "temp_dir": base_tmp if keep else "",
        "not_run": ["真实板端测量/apply/真实房间结论：NOT_RUN"],
        "claim": "只证明批量流水线与观察指标机器可判；不证明校准算法/真实房间效果。",
    }
    os.makedirs(outdir, exist_ok=True)
    with open(os.path.join(outdir, "selftest-result.json"), "w", encoding="utf-8") as fh:
        json.dump(result, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    # improved 场景样例已由 _copy_sample 复制
    with open(os.path.join(outdir, "SELFTEST-REPORT.md"), "w", encoding="utf-8") as fh:
        fh.write(render_selftest_md(result))
    return result


def _copy_sample(results: dict, outdir: str, repo_root: str) -> None:
    """把 improved 场景的 plan/分析报告复制为可复现样例（小文件，仅文本/JSON）。"""
    if "improved" not in results:
        return
    sample_dir = os.path.join(outdir, "sample-improved")
    os.makedirs(sample_dir, exist_ok=True)
    for name in ("RUN-PLAN.md", "commands.txt", "time-budget.json", "plan.json"):
        src = os.path.join(results["improved"]["plan_dir"], name)
        if os.path.isfile(src):
            shutil.copyfile(src, os.path.join(sample_dir, name))
    for name in ("REPORT.md", "comparison.json", "MANIFEST.md", "MANIFEST.json", "quality.csv"):
        src = os.path.join(results["improved"]["out_dir"], name)
        if os.path.isfile(src):
            shutil.copyfile(src, os.path.join(sample_dir, name))
    with open(os.path.join(sample_dir, "selftest-config.json"), "w", encoding="utf-8") as fh:
        json.dump(_base_config(repo_root), fh, ensure_ascii=False, indent=2)
        fh.write("\n")


def render_selftest_md(result: dict) -> str:
    lines = ["# 批量闭环离线自测报告（fake 合成数据）", "",
             "- 结果: **%s**" % ("PASS" if result["ok"] else "FAIL"),
             "- 检查: %d/%d PASS，耗时 %.1f s"
             % (result["checks_pass"], result["checks_total"], result["elapsed_s"]),
             "", "## 场景", "", "| scenario | verdict | 耗时 s |", "|---|---|---|"]
    for name, info in result["scenarios"].items():
        lines.append("| %s | %s | %s |" % (name, info.get("verdict"), info.get("elapsed_s")))
    lines += ["", "## 失败项", ""]
    if result["checks_fail"]:
        for c in result["checks_fail"]:
            lines.append("- FAIL %s: %s" % (c["name"], c["detail"]))
    else:
        lines.append("- 无")
    lines += ["", "## 完整检查", "", "| # | check | ok | detail |", "|---|---|---|---|"]
    for i, c in enumerate(result["checks"], 1):
        lines.append("| %d | %s | %s | %s |" % (i, c["name"], "PASS" if c["ok"] else "FAIL", c["detail"]))
    lines += ["", "## 边界", "", "- " + result["claim"], "- " + "; ".join(result["not_run"])]
    return "\n".join(lines) + "\n"
