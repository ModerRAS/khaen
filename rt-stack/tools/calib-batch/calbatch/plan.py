"""TSPI 校准批量闭环：计划生成（窗口前冻结）。

设计目标（用户需求 39：所有输入/命令/文件提前生成，现场只跑；需求 28/44：前后瀑布+RT+MIMO+holdout；
需求 27：至少两轮独立重复）：
- 一次生成两轮（A/B）完整 plan：preflight -> freeze -> baseline（design+holdout）-> apply ->
  after（design+holdout）-> pilot（三麦，前后，独立 lane）-> analyze -> report -> restore(可选)。
- 每个测量步给出可直接执行的 argv（生产模板来自 commands.measure/measure_pilot），
  命令里所有值（点位/通道/重复/电平/sweep 参数）都来自冻结配置；窗口内不再改参数。
- 时间预算逐项计算并对照白天窗口（08:30-12:00 / 14:30-18:00），不足即 fail-closed。
- 产出 plan.json / commands.txt / RUN-PLAN.md / time-budget.json；analyze 阶段默认在工作站执行，
  板端只负责采集（用户二阶段策略：现场一波批量测完再分析）。
"""

from __future__ import annotations

import datetime
import hashlib
import json
import os

from . import config as cfgmod
from . import measure

PLAN_SCHEMA = "tspi-cal-batch-plan/v1"
BT = chr(96)


def _now_iso() -> str:
    return datetime.datetime.now().astimezone().isoformat(timespec="seconds")


def _hash_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _hash_file(path: str):
    if not path or not os.path.isfile(path):
        return None
    return {"path": path, "sha256": cfgmod.sha256_file(path), "bytes": os.path.getsize(path)}


def _fill_template(template: str, values: dict) -> str:
    out = template
    for key, value in values.items():
        out = out.replace("{%s}" % key, str(value))
    return out


def _subst_argv(argv, values) -> list:
    return [_fill_template(str(tok), values) for tok in argv]


def _fmt_cmd(argv) -> str:
    def quote(tok):
        tok = str(tok)
        if not tok:
            return "''"
        if any(c in tok for c in " \t\"'$" + BT + "\\"):
            return "'" + tok.replace("'", "'\\''") + "'"
        return tok
    return " ".join(quote(t) for t in argv)


def _channel_lists(plan_channels):
    slots = ",".join(str(c["playback_channel"]) for c in plan_channels)
    names = ",".join(str(c["name"]) for c in plan_channels)
    return slots, names


def _measure_duration_seconds(cfg: dict, repeats: int, capture2: bool) -> float:
    sweep = cfg["sweep"]
    tl = measure.sweep_timeline(sweep, len(cfg["channels"]), repeats)
    extra = 3.0
    if capture2:
        extra += 2.0
    return float(tl["record_seconds"]) + extra


def estimate_round(cfg: dict, round_id: str) -> dict:
    est = cfg["estimates"]
    items = []

    def add(phase, seconds, note=""):
        items.append({"phase": phase, "seconds": round(float(seconds), 1), "note": note})

    add("preflight", float(est["preflight_seconds"]), "board-window.sh preflight（只读门 G0-G10）")
    add("freeze", 10.0, "冻结清单 hash/点位/授权检查")
    design = cfg["design_points"]
    holdout = cfg["holdout_points"]
    for group, label in ((design, "design"), (holdout, "holdout")):
        for pt in group:
            dur = _measure_duration_seconds(cfg, pt["repeats"], capture2=False)
            move = float(est["move_seconds_per_point"])
            # 同一点前/后各采一次；每点之间 move 一次（保守按每次采集都算 move）
            add("measure", 2.0 * (dur + move), "%s %s baseline+after（各含 move %.0fs）" % (label, pt["id"], move))
    add("apply", float(est["apply_seconds"]), "应用校准（REST job apply 或 prep/apply.sh；B4 前 NOT_RUN）")
    pilot_mics = 0
    if cfg.get("pilot", {}).get("enabled"):
        pilot_mics = len([m for m in cfg["pilot"].get("mics", [])
                          if not str(m.get("name", "")).upper().startswith("UMIK")])
        for pt in cfg.get("_pilot_points") or []:
            add("pilot", 2.0 * pilot_mics * _measure_duration_seconds(cfg, pt["repeats"], capture2=True),
                "三麦 pilot %s 前/后 x %d 参考麦（capture2=MA1/ARC，pilot_only）" % (pt["id"], pilot_mics))
    n_captures = 2 * (len(design) + len(holdout)) + 2 * pilot_mics * len(cfg.get("_pilot_points") or [])
    add("analyze", n_captures * float(est["analyze_seconds_per_capture"]),
        "工作站分析（前后 FR/瀑布/RT/MIMO/holdout；板端只采集）")
    add("report", float(est["report_seconds"]), "报告/证据打包")
    raw = sum(i["seconds"] for i in items)
    margin = raw * float(est["margin_fraction"])
    total = raw + margin
    ws, we = cfgmod.resolve_time_window(cfg, round_id)
    window_seconds = (cfgmod._minutes(we) - cfgmod._minutes(ws)) * 60
    fits = total <= window_seconds
    return {
        "round": round_id,
        "window": {"start": ws, "end": we, "duration_seconds": window_seconds},
        "items": items,
        "raw_seconds": round(raw, 1),
        "margin_seconds": round(margin, 1),
        "total_seconds": round(total, 1),
        "total_minutes": round(total / 60.0, 1),
        "fits_window": bool(fits),
        "not_run": [] if fits else ["E_BATCH_ESTIMATE: 估算 %.1f min > 窗口 %.1f min" % (total / 60.0, window_seconds / 60.0)],
    }


def _step(step_id, phase, lane, point, timepoint, argv, expects, timeout_s, duration_s,
          point_role="", pilot_only=False, repeats=3, kind="cmd", not_run=()):
    return {
        "id": step_id,
        "phase": phase,
        "lane": lane,
        "point": point,
        "point_role": point_role,
        "timepoint": timepoint,
        "kind": kind,
        "argv": list(argv or []),
        "expects": list(expects or []),
        "timeout_s": int(timeout_s),
        "duration_est_s": round(float(duration_s), 1),
        "pilot_only": bool(pilot_only),
        "repeats": int(repeats),
        "not_run": list(not_run),
    }


def build_plan(cfg: dict, round_id: str, repo_root: str, scenario: str = "") -> dict:
    cfg = cfgmod.normalize_config(cfg)
    validation = cfgmod.validate_config(cfg, repo_root)
    steps = []
    idx = 0
    commands = cfg.get("commands") or {}
    prep_dir = str(cfg.get("prep_dir", "/var/tmp/tspi-cal-batch/prep"))
    step_root = "steps"
    slots_csv, names_csv = _channel_lists(cfg["channels"])
    base_vals = {
        "rate": cfg["fs"],
        "seconds": cfg["sweep"]["seconds"],
        "pre": cfg["sweep"]["pre_seconds"],
        "post": cfg["sweep"]["post_seconds"],
        "gap": cfg["sweep"]["repeat_gap_seconds"],
        "slot_gap": cfg["sweep"]["slot_gap_seconds"],
        "level": cfg["drive_level_dbfs"],
        "slots": slots_csv,
        "names": names_csv,
        "prep_dir": prep_dir,
        "repo_root": repo_root,
        "round": round_id,
        "batch": cfg["batch_label"],
    }

    def push(step):
        nonlocal idx
        step["dir_name"] = "%02d-%s" % (idx, step["id"])
        step["index"] = idx
        idx += 1
        steps.append(step)
        return step

    preflight = commands.get("preflight") or {"argv": ["bash", "{prep_dir}/preflight.sh", "{step_dir}"], "timeout_s": 300}
    pf_vals = dict(base_vals, step_dir=step_root + "/00-preflight")
    push(_step("preflight", "preflight", "production", "", "before",
               _subst_argv(preflight.get("argv", []), pf_vals), preflight.get("expects", ["preflight.json"]),
               preflight.get("timeout_s", 300), cfg["estimates"]["preflight_seconds"],
               kind="cmd", not_run=preflight.get("not_run", [])))
    push(_step("freeze", "freeze", "production", "", "before", [], ["frozen.json"],
               30, 10.0, kind="local"))
    push(_step("freeze-pilot", "freeze", "pilot", "", "before", [], ["frozen-pilot.json"],
               30, 5.0, kind="local", pilot_only=True))

    measure_spec = commands.get("measure") or {"argv": [], "timeout_s": 1200, "expects": ["capture.raw"]}
    for timepoint, prefix in (("before", "baseline"), ("after", "after")):
        for pt in cfg["design_points"] + cfg["holdout_points"]:
            s = _step("measure-%s-%s" % (prefix, pt["id"]), "measure", "production", pt["id"], timepoint,
                      [], measure_spec.get("expects", ["capture.raw"]),
                      measure_spec.get("timeout_s", 1200),
                      _measure_duration_seconds(cfg, pt["repeats"], capture2=False),
                      point_role=pt["role"], repeats=pt["repeats"],
                      not_run=measure_spec.get("not_run", []))
            push(s)
            step_dir = step_root + "/" + s["dir_name"]
            s["argv"] = _subst_argv(measure_spec.get("argv", []),
                                    dict(base_vals, point=pt["id"], repeats=pt["repeats"],
                                         step_dir=step_dir, prefix="%s-%s" % (prefix, pt["id"])))
            s["out_dir"] = step_dir
            s["prefix"] = "%s-%s" % (prefix, pt["id"])
        if timepoint == "before":
            apply_spec = commands.get("apply") or {"argv": [], "timeout_s": 1200, "expects": ["applied.json"]}
            s = _step("apply", "apply", "production", "", "after", [], apply_spec.get("expects", ["applied.json"]),
                      apply_spec.get("timeout_s", 1200), cfg["estimates"]["apply_seconds"],
                      kind="cmd", not_run=apply_spec.get("not_run", []))
            push(s)
            step_dir = step_root + "/" + s["dir_name"]
            s["argv"] = _subst_argv(apply_spec.get("argv", []), dict(base_vals, step_dir=step_dir))
            s["out_dir"] = step_dir

    if cfg.get("pilot", {}).get("enabled"):
        pilot_cmd = commands.get("measure_pilot") or {"argv": [], "timeout_s": 1800}
        mics = [m for m in cfg["pilot"].get("mics", [])
                if not str(m.get("name", "")).upper().startswith("UMIK")]
        for timepoint in ("before", "after"):
            for pt in cfg.get("_pilot_points") or []:
                for mic in mics:
                    sid = "pilot-%s-%s-%s" % (timepoint, pt["id"], str(mic["name"]).lower())
                    s = _step(sid, "pilot", "pilot", pt["id"], timepoint, [], ["capture.raw"],
                              pilot_cmd.get("timeout_s", 1800),
                              _measure_duration_seconds(cfg, pt["repeats"], capture2=True),
                              point_role="pilot", pilot_only=True, repeats=pt["repeats"],
                              not_run=pilot_cmd.get("not_run", []))
                    push(s)
                    prefix = "pilot-%s-%s-%s" % (timepoint, pt["id"], str(mic["name"]).lower())
                    vals = dict(base_vals, point=pt["id"], repeats=pt["repeats"],
                                step_dir=step_root + "/" + s["dir_name"],
                                capture2_channel=int(mic.get("capture2_channel", 0)),
                                mic_name=str(mic["name"]), prefix=prefix)
                    s["argv"] = _subst_argv(pilot_cmd.get("argv", []), vals)
                    s["out_dir"] = vals["step_dir"]
                    s["prefix"] = prefix
                    s["mic"] = str(mic["name"])

    push(_step("analyze", "analyze", "local", "", "after", [], ["analysis/batch-report.json"],
               3600, 0.0, kind="local"))
    if cfg.get("restore_after_batch"):
        restore_spec = commands.get("restore") or {"argv": [], "timeout_s": 600}
        s = _step("restore", "restore", "production", "", "after", [], restore_spec.get("expects", ["restored.json"]),
                  restore_spec.get("timeout_s", 600), 120.0, kind="cmd",
                  not_run=restore_spec.get("not_run", []))
        push(s)
        s["argv"] = _subst_argv(restore_spec.get("argv", []),
                                dict(base_vals, step_dir=step_root + "/" + s["dir_name"]))
    push(_step("report", "report", "local", "", "after", [], ["REPORT.md", "MANIFEST.json"],
               600, cfg["estimates"]["report_seconds"], kind="local"))

    estimate = estimate_round(cfg, round_id)
    inputs = []
    cfg_path = cfg.get("_config_path")
    if cfg_path:
        hp = _hash_file(cfg_path)
        if hp:
            inputs.append(dict(hp, role="config", path=os.path.basename(cfg_path)))
    mic = cfg.get("mic") or {}
    if mic.get("cal_file") and not cfg.get("selftest"):
        p = mic["cal_file"]
        hp = _hash_file(p if os.path.isabs(p) else os.path.join(repo_root, p))
        if hp:
            hp["role"] = "umik_cal"
            inputs.append(hp)
    hb = _hash_file(os.path.join(repo_root, "docs", "BOARD-WINDOW-RUNBOOK-T67-T74.md"))
    if hb:
        hb["role"] = "runbook"
        inputs.append(hb)

    board_not_run = [
        {"id": "NR-BOARD-MEASURE", "what": "板端真实 measure-multi 采集",
         "why": "本 plan 只冻结命令；任务约束不碰板端，未执行；B2/B3（真实 ALSA 后端/stage backend 未接线）未解锁"},
        {"id": "NR-BOARD-APPLY", "what": "板端真实 apply（新栈 job apply / REST）",
         "why": "B1/B4：T71 未签发、daemon 未挂 CalibrationPort（REST 503）；apply.sh 属窗口 prep，未在本机验证"},
        {"id": "NR-BOARD-PILOT", "what": "板端三麦 pilot 采集",
         "why": "pilot lane 命令已冻结，但需板端窗口执行；pilot 结果不得进入 T67/T6-M"},
        {"id": "NR-REAL-ACOUSTIC", "what": "真实房间前后瀑布/RT/MIMO 结论",
         "why": "无真实采集数据；本工具只做离线 fake 自测，真实结论 NOT_RUN"},
        {"id": "NR-CROSS-ROUND", "what": "A/B 两轮跨轮预注册/一致性",
         "why": "mimo-t6 §5.6 预注册流程属 T69，本 plan 只提供两轮命令占位"},
    ]
    not_run = board_not_run + estimate.get("not_run", [])
    if cfg.get("selftest"):
        not_run = [{"id": "NR-SELFTEST-ONLY", "what": "真实板端",
                    "why": "selftest 配置只用 fake transport 验证流水线；真实结果 NOT_RUN"}]

    plan = {
        "schema": PLAN_SCHEMA,
        "created_at": _now_iso(),
        "batch_label": cfg["batch_label"],
        "round": round_id,
        "scenario": scenario,
        "selftest": bool(cfg.get("selftest", False)),
        "fs": cfg["fs"],
        "layout": cfg.get("layout", "4.1.2"),
        "sweep": dict(cfg["sweep"], rate=cfg["fs"]),
        "drive_level_dbfs": cfg["drive_level_dbfs"],
        "capture_lead_in_seconds": cfg.get("capture_lead_in_seconds", 0.35),
        "capture": {
            "format": "raw_s24",
            "file": "capture.raw",
            "channels": 1,
            "channel_index": 0,
            "device": "umik",
            "capture2_file": "capture2.raw",
            "capture2_channels": int(cfg.get("pilot", {}).get("capture2_channels", 20)),
            "capture2_channel_index_default": 0,
        },
        "channels": cfg["channels"],
        "design_points": cfg["design_points"],
        "holdout_points": cfg["holdout_points"],
        "pilot": cfg.get("pilot", {"enabled": False}),
        "pilot_points": cfg.get("_pilot_points", []),
        "analysis": cfg.get("analysis", {}),
        "estimate": estimate,
        "window": estimate["window"],
        "inputs": inputs,
        "validation": validation,
        "steps": steps,
        "not_run": not_run,
        "policy": {
            "single_production_lane": "生产口径 = 单支 UMIK-1 移动多点（design>=6 + holdout>=3）",
            "pilot_only": "三麦固定 = pilot only；不得用于 T67/T68/T69/T6-M 签收",
            "apply_adapter": (commands.get("apply") or {}).get("adapter", "unset"),
            "window_only": "白天 08:30-12:00 / 14:30-18:00；-50 dBFS 起步；可闻授权必须 true",
            "no_cueMix_writes": "CueMix/Toslink 只读；本流程不写 CueMix",
        },
    }
    plan["plan_sha256"] = _plan_hash(plan)
    return plan


def _plan_hash(plan: dict) -> str:
    core = {k: v for k, v in plan.items() if k not in ("plan_sha256", "created_at")}
    return _hash_text(json.dumps(core, ensure_ascii=False, sort_keys=True))


def render_commands_txt(plan: dict) -> str:
    lines = []
    lines.append("# %s round %s 命令清单（窗口内只跑；本文件 hash 入 MANIFEST）" % (plan["batch_label"], plan["round"]))
    lines.append("# plan_sha256=%s created_at=%s" % (plan["plan_sha256"], plan["created_at"]))
    for s in plan["steps"]:
        if s["kind"] == "local":
            lines.append("[%02d] %-28s LOCAL  (workstation)  %s" % (s["index"], s["id"], ",".join(s["expects"])))
            continue
        lines.append("[%02d] %-28s %-10s %s" % (s["index"], s["id"], s["phase"], _fmt_cmd(s["argv"])))
        if s["not_run"]:
            lines.append("     NOT_RUN: %s" % "; ".join(str(x) for x in s["not_run"]))
    return "\n".join(lines) + "\n"


def render_run_plan_md(plan: dict) -> str:
    est = plan["estimate"]
    bt = BT
    lines = []
    lines.append("# 批量校准窗口执行计划（%s / round %s）" % (plan["batch_label"], plan["round"]))
    lines.append("")
    lines.append("> 本文件由 tspi_cal_batch plan 生成；窗口内**只执行**，不再改参数/删点/改命令。")
    lines.append("> plan_sha256: %s" % plan["plan_sha256"])
    lines.append("> 生成时间: %s" % plan["created_at"])
    lines.append("")
    lines.append("## 1. 一页结论")
    lines.append("")
    lines.append("- 测量口径：生产 = 单支 UMIK-1 移动多点（design %d + holdout %d），三麦固定 = pilot only（%s）。"
                 % (len(plan["design_points"]), len(plan["holdout_points"]),
                    "启用" if plan["pilot"].get("enabled") else "未启用"))
    lines.append("- 阶段：preflight -> freeze -> baseline(全部点) -> apply -> after(全部点) -> pilot(前后) -> analyze -> report")
    lines.append("- 估算总时长：**%.1f min**（窗口 %s-%s，共 %.0f min，%s）"
                 % (est["total_minutes"], est["window"]["start"], est["window"]["end"],
                    est["window"]["duration_seconds"] / 60.0, "fits" if est["fits_window"] else "EXCEEDS"))
    lines.append("- 板端真实测量/应用：**NOT_RUN**（B1-B13 未解锁；本 plan 只是窗口前冻结产物，未占板）。")
    lines.append("")
    lines.append("## 2. 阶段与步骤")
    lines.append("")
    lines.append("| # | 步骤 | phase | lane | 点 | 前/后 | 预计 s | 关键 expect |")
    lines.append("|---|---|---|---|---|---|---|---|")
    for s in plan["steps"]:
        lines.append("| %02d | %s | %s | %s | %s | %s | %.0f | %s |"
                     % (s["index"], s["id"], s["phase"], s["lane"], s["point"] or "-", s["timepoint"],
                        s["duration_est_s"], ", ".join(s["expects"])))
    lines.append("")
    lines.append("## 3. 时间预算")
    lines.append("")
    lines.append("| 项 | 秒 | 说明 |")
    lines.append("|---|---|---|")
    for item in est["items"]:
        lines.append("| %s | %.1f | %s |" % (item["phase"], item["seconds"], item["note"]))
    lines.append("| **raw** | %.1f | |" % est["raw_seconds"])
    lines.append("| margin | %.1f | 估算项自带 |" % est["margin_seconds"])
    lines.append("| **总计** | %.1f | %.1f min |" % (est["total_seconds"], est["total_minutes"]))
    lines.append("")
    lines.append("## 4. 命令与证据")
    lines.append("")
    lines.append("- 全部命令见同目录 commands.txt（每步 argv 已冻结，执行器不拼命令）。")
    lines.append("- 每步证据写 %s<work>/steps/<NN-id>/%s：stdout.log/stderr.log/exit.json/capture meta + 原始记录。" % (bt, bt))
    lines.append("- 板端窗口证据目录模板见 runbook §10；本工具分析输出写 %s<work>/analysis/%s。" % (bt, bt))
    lines.append("- 板端只跑 measure/apply 命令；analyze/report 在工作站执行（窗口内不现场分析）。")
    lines.append("")
    lines.append("## 5. 中止条件（fail-closed）")
    lines.append("")
    for cond in ("preflight 任一硬门失败（exit 3）",
                 "采集命令 exit != 0 / 超时",
                 "捕获质量门失败：SNR < 25 dB、重复 delay 散差 > 0.5 sample、重复 FR 散差 > 0.8 dB",
                 "时钟 ppm 超限/onset invalid（由 measure-multi 自身分析暴露）",
                 "apply 失败（501/503/缺 applied_version）——按 runbook §9 回退 last-good/mute，不得启动 C",
                 "任何可闻异常、爆音、持续噪声、削波、失锁 -> 立即停并静音"):
        lines.append("- " + cond)
    lines.append("")
    lines.append("## 6. NOT_RUN")
    lines.append("")
    for nr in plan["not_run"]:
        lines.append("- **%s**：%s —— %s" % (nr["id"], nr["what"], nr["why"]))
    lines.append("")
    lines.append("## 7. 观察指标（不是验收门）")
    lines.append("")
    lines.append("- 前后对比只产出**观察量**：瀑布/CSD 衰减、EDC/T20/RT60、模态余振、座位间 σ、")
    lines.append("  MIMO 前后对比、holdout 泛化；分类 IMPROVED/FLAT/WORSE/INSUFFICIENT 不构成 PASS/FAIL。")
    lines.append("- 验收权威仍是 rt-stack/spec/mimo-t6.md（T6-M）与 calibration-spec/ACCEPTANCE.md（T6-cal）；")
    lines.append("  用户要求第 29 条：3 dB/15% 也只是工程观察指标。")
    lines.append("")
    lines.append("## 8. 复现")
    lines.append("")
    lines.append("%s%s%sbash" % (bt, bt, bt))
    lines.append("# 窗口前（本机/工作站）：生成并复核")
    lines.append("python3 tspi_cal_batch.py plan --config configs/batch-config.production-4.1.2.json --round A --out plan/")
    lines.append("# 窗口内（板端）：只跑采集/应用命令（本地 argv）")
    lines.append("python3 tspi_cal_batch.py run --plan plan/round-A/plan.json --transport cmd --work /var/lib/tspi-rt-stack/evidence/calib-batch-A")
    lines.append("# 窗口后（工作站）：分析 + 报告")
    lines.append("python3 tspi_cal_batch.py analyze --work <拉回的 work 目录> --out analysis/A")
    lines.append("%s%s%s" % (bt, bt, bt))
    lines.append("")
    return "\n".join(lines) + "\n"


def write_plan(plan: dict, out_dir: str) -> dict:
    os.makedirs(out_dir, exist_ok=True)
    plan_path = os.path.join(out_dir, "plan.json")
    with open(plan_path, "w", encoding="utf-8") as fh:
        json.dump(plan, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    with open(os.path.join(out_dir, "commands.txt"), "w", encoding="utf-8") as fh:
        fh.write(render_commands_txt(plan))
    with open(os.path.join(out_dir, "RUN-PLAN.md"), "w", encoding="utf-8") as fh:
        fh.write(render_run_plan_md(plan))
    budget = {"schema": "tspi-cal-batch-time-budget/v1", "round": plan["round"],
              "estimate": plan["estimate"], "plan_sha256": plan["plan_sha256"]}
    with open(os.path.join(out_dir, "time-budget.json"), "w", encoding="utf-8") as fh:
        json.dump(budget, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    files = {}
    for name in ("plan.json", "commands.txt", "RUN-PLAN.md", "time-budget.json"):
        p = os.path.join(out_dir, name)
        files[name] = {"sha256": cfgmod.sha256_file(p), "bytes": os.path.getsize(p)}
    return {"dir": out_dir, "plan": plan_path, "files": files}
