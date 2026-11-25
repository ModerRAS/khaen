"""KHAEN 校准批量闭环：计划执行器（板端只跑采集/应用命令；本地 freeze/analyze/report）。

执行纪律：
- 只执行 plan.json 里冻结的 argv，不拼命令、不改参数、不删步骤。
- cmd transport：subprocess 本地执行（板端 root shell 或本地 prep）；每步独立目录 + stdout/stderr/exit.json。
- fake transport：用离线合成 plant 生成同布局的"测量记录"（证明流程可跑通；不产出真实结论）。
- 失败 fail-closed：preflight/measure/apply 失败 -> 停批，执行 restore（如配置）并 exit 4；
  分析阶段缺失数据的点标 INCOMPLETE 而不会伪造。
- 任何真实板端结果在本任务一律 NOT_RUN（见 plan.not_run）。
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import time

from . import config as cfgmod
from . import measure
from . import plan as planmod

RUN_SCHEMA = "khaen-cal-batch-run/v1"
ABORT_PHASES = ("preflight", "measure", "apply")


def _now() -> str:
    import datetime
    return datetime.datetime.now().astimezone().isoformat(timespec="seconds")


def _rel(workdir: str, path: str) -> str:
    return os.path.relpath(path, workdir)


def _load_plan(path: str) -> dict:
    with open(path, "r", encoding="utf-8") as fh:
        plan = json.load(fh)
    if plan.get("schema") != planmod.PLAN_SCHEMA:
        raise ValueError("plan schema 不符: %r" % plan.get("schema"))
    expect = planmod._plan_hash(plan)
    if plan.get("plan_sha256") != expect:
        raise ValueError("plan_sha256 校验失败：文件被改动过（expect %s got %s）" % (expect, plan.get("plan_sha256")))
    return plan


class Runner:
    def __init__(self, plan: dict, workdir: str, transport: str = "cmd", scenario: str = "improved",
                 dry_run: bool = False, abort_on_error: bool = True):
        self.plan = plan
        self.workdir = os.path.abspath(workdir)
        self.transport = transport
        self.scenario = scenario
        self.dry_run = dry_run
        self.abort_on_error = abort_on_error
        self.steps_state = []
        self.plant = None
        self.applied = None
        self.records = []

    # ------------------------------------------------------------------ utils
    def _step_dir(self, step: dict) -> str:
        return os.path.join(self.workdir, "steps", step["dir_name"])

    def _log(self, text: str) -> None:
        line = "[%s] %s" % (_now(), text)
        print(line)
        with open(os.path.join(self.workdir, "run.log"), "a", encoding="utf-8") as fh:
            fh.write(line + "\n")

    def _write_json(self, path: str, obj) -> str:
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            json.dump(obj, fh, ensure_ascii=False, indent=2)
            fh.write("\n")
        return path

    # ------------------------------------------------------------------ freeze
    def _do_freeze(self, step: dict) -> dict:
        step_dir = self._step_dir(step)
        os.makedirs(step_dir, exist_ok=True)
        plan = self.plan
        frozen = {
            "schema": "khaen-cal-batch-frozen/v1",
            "batch_label": plan["batch_label"],
            "round": plan["round"],
            "step": step["id"],
            "created_at": _now(),
            "plan_sha256": plan["plan_sha256"],
            "fs": plan["fs"],
            "layout": plan["layout"],
            "sweep": plan["sweep"],
            "drive_level_dbfs": plan["drive_level_dbfs"],
            "channels": [{"name": c["name"], "playback_channel": c["playback_channel"],
                          "fit_band_hz": c.get("fit_band_hz")} for c in plan["channels"]],
            "design_points": [{"id": p["id"], "label": p["label"], "role": p["role"], "weight": p["weight"],
                               "repeats": p["repeats"]} for p in plan["design_points"]],
            "holdout_points": [{"id": p["id"], "label": p["label"], "role": p["role"], "weight": p["weight"],
                                "repeats": p["repeats"]} for p in plan["holdout_points"]],
            "pilot": {"enabled": bool(plan["pilot"].get("enabled")),
                      "pilot_only": bool(plan["pilot"].get("pilot_only")),
                      "claim": plan["pilot"].get("claim", ""),
                      "points": plan.get("pilot_points", []),
                      "mics": plan["pilot"].get("mics", [])},
            "inputs": plan.get("inputs", []),
            "sealed": True,
            "note": "冻结于批量执行前；窗口内不得改参数/删点/改阈值（mimo-t6 F-09/F-10）。",
        }
        if step.get("pilot_only"):
            frozen["lane"] = "pilot"
            frozen["claim"] = plan["pilot"].get("claim", "")
            frozen["note"] = "三麦固定 pilot lane；不得进入 T67/T68/T69/T6-M。"
        path = os.path.join(step_dir, "frozen.json")
        self._write_json(path, frozen)
        return {"status": "ok", "artifacts": [path]}

    # --------------------------------------------------------------- preflight
    def _fake_preflight(self, step: dict) -> dict:
        step_dir = self._step_dir(step)
        os.makedirs(step_dir, exist_ok=True)
        preflight = {
            "schema": "khaen-cal-batch-preflight/v1",
            "simulated": True,
            "step": step["id"],
            "created_at": _now(),
            "PF": {"SELFTEST_FAKE": 1, "C_RETIRED": 1, "KEEPER_48K": 1, "NO_192K": 1,
                   "PCM_FREE": 1, "UMIK_PRESENT": 1, "MOTU_PRESENT": 1},
            "not_run": ["板端 G0-G10 真实门：NOT_RUN（fake transport 不碰板端）"],
        }
        path = os.path.join(step_dir, "preflight.json")
        self._write_json(path, preflight)
        return {"status": "ok", "artifacts": [path], "simulated": True}

    # ------------------------------------------------------------------ apply
    def _fake_apply(self, step: dict) -> dict:
        from . import model
        step_dir = self._step_dir(step)
        os.makedirs(step_dir, exist_ok=True)
        design_ids = [p["id"] for p in self.plan["design_points"]]
        baseline_irs = {}
        for s in self.plan["steps"]:
            if s["phase"] != "measure" or s["timepoint"] != "before" or s["lane"] != "production":
                continue
            meta_path = os.path.join(self.workdir, "captures", "%s.meta.json" % s["id"])
            if not os.path.isfile(meta_path):
                raise RuntimeError("apply 前缺 baseline capture: %s" % meta_path)
            meta = measure.load_meta(meta_path)
            ext = measure.extract_capture(meta, ir_seconds=float(self.plan["analysis"].get("ir_seconds", 1.0)))
            baseline_irs[ext["point"]] = ext["irs"]
        state = model.derive_compensation(self.plan, self.scenario, baseline_irs, int(self.plan["fs"]))
        state["applied_at"] = _now()
        state["step"] = step["id"]
        path = os.path.join(step_dir, "applied-state.json")
        self._write_json(path, state)
        self.applied = state
        self._write_json(os.path.join(self.workdir, "applied-state.json"), state)
        applied_json = {
            "schema": "khaen-cal-batch-apply-summary/v1",
            "simulated": True,
            "scenario": self.scenario,
            "fitted_from_design_points": design_ids,
            "holdout_used_in_fit": [],
            "channels_with_peq": sorted(state.get("peq", {}).keys()),
            "modal_bands": sorted(state.get("modal_damp", {}).keys()),
            "not_run": ["真实 apply（REST job apply / last-good 切换）：NOT_RUN；本步只是 fake 补偿模型"],
        }
        apath = os.path.join(step_dir, "applied.json")
        self._write_json(apath, applied_json)
        return {"status": "ok", "artifacts": [path, apath], "simulated": True}

    def _fake_restore(self, step: dict) -> dict:
        step_dir = self._step_dir(step)
        os.makedirs(step_dir, exist_ok=True)
        self.applied = None
        if os.path.isfile(os.path.join(self.workdir, "applied-state.json")):
            os.remove(os.path.join(self.workdir, "applied-state.json"))
        path = os.path.join(step_dir, "restored.json")
        self._write_json(path, {"schema": "khaen-cal-batch-restore-summary/v1", "simulated": True,
                                "restored": True, "note": "fake：清除合成补偿状态；真实 rollback/mute 由 prep/restore.sh 覆盖，NOT_RUN"})
        return {"status": "ok", "artifacts": [path], "simulated": True}

    # -------------------------------------------------------------- fake measure
    def _fake_measure(self, step: dict) -> dict:
        from . import model
        if self.plant is None:
            self.plant = model.PlantModel(self.plan, seed=int(self.plan["analysis"].get("seed", 20261117)))
        if self.applied is None and step["timepoint"] == "after":
            raise RuntimeError("after 采集前没有 apply 状态（fake transport 纪律）")
        applied = self.applied if step["timepoint"] == "after" else None
        lane = step.get("lane", "production")
        capture2 = bool(step.get("mic"))  # pilot 参考麦步携带 capture2
        rendered = self.plant.render_capture(
            point_id=step["point"], applied=applied, lane=lane, repeats=int(step["repeats"]),
            lead_in_seconds=float(self.plan.get("capture_lead_in_seconds", 0.35)),
            device_specs=[step.get("mic", "umik")], capture2=capture2)
        step_dir = os.path.join(self.workdir, "steps", step["dir_name"])
        os.makedirs(step_dir, exist_ok=True)
        recs = []
        for i, rec in enumerate(rendered["recordings"]):
            data = rendered["data"] if i == 0 else rendered["capture2_data"]
            if step.get("mic") and str(step["mic"]).upper().startswith("ARC") and i > 0 and data is not None:
                # ARC 无官方 cal：合成一个小的高频衰减以区分参考麦（仅用于离线自测）
                import numpy as np
                arr = np.asarray(data, dtype=np.float64)
                data = arr * 0.9
            path = os.path.join(step_dir, rec["path"])
            from . import dsp
            dsp.write_wav_raw(path, data, int(self.plan["fs"]), 1)
            rec = dict(rec)
            rec["path"] = os.path.abspath(path)
            rec["rate"] = int(self.plan["fs"])
            rec["bytes"] = os.path.getsize(path)
            rec["channel_index"] = 0
            rec["channels"] = 1
            recs.append(rec)
        meta = measure.make_meta(step, self.plan, recs, source="fake")
        meta["simulated"] = True
        meta["not_run"] = ["fake transport：合成记录，不是真实测量；真实板端采集 NOT_RUN"]
        meta_path = os.path.join(self.workdir, "captures", "%s.meta.json" % step["id"])
        measure.save_meta(meta, meta_path)
        self.records.append({"step": step["id"], "meta": meta_path, "simulated": True})
        return {"status": "ok", "artifacts": [p for p in [meta_path] + [r["path"] for r in recs]],
                "simulated": True}

    # ------------------------------------------------------------------ cmd run
    def _cmd_run(self, step: dict) -> dict:
        step_dir = self._step_dir(step)
        os.makedirs(step_dir, exist_ok=True)
        argv = step.get("argv") or []
        if not argv:
            return {"status": "failed", "error": "cmd step 无 argv: %s" % step["id"], "exit_code": 2}
        start = time.time()
        try:
            proc = subprocess.run(argv, cwd=self.workdir, capture_output=True, text=True,
                                  timeout=int(step.get("timeout_s", 600)), errors="replace")
        except FileNotFoundError as exc:
            self._write_json(os.path.join(step_dir, "exit.json"),
                             {"step": step["id"], "argv": argv, "error": "command_not_found: %s" % exc,
                              "exit_code": 127, "not_run": ["命令不存在：板端包/工具未安装，NOT_RUN"]})
            return {"status": "failed", "error": "command_not_found: %s" % exc, "exit_code": 127}
        except subprocess.TimeoutExpired:
            self._write_json(os.path.join(step_dir, "exit.json"),
                             {"step": step["id"], "argv": argv, "error": "timeout", "exit_code": 124})
            return {"status": "failed", "error": "timeout", "exit_code": 124}
        elapsed = time.time() - start
        with open(os.path.join(step_dir, "stdout.log"), "w", encoding="utf-8") as fh:
            fh.write(proc.stdout or "")
        with open(os.path.join(step_dir, "stderr.log"), "w", encoding="utf-8") as fh:
            fh.write(proc.stderr or "")
        record = {"step": step["id"], "phase": step["phase"], "argv": argv, "cwd": self.workdir,
                  "started_at": start, "elapsed_s": round(elapsed, 3), "exit_code": proc.returncode,
                  "status": "ok" if proc.returncode == 0 else "failed"}
        self._write_json(os.path.join(step_dir, "exit.json"), record)
        if proc.returncode != 0:
            return {"status": "failed", "error": "exit %d" % proc.returncode, "exit_code": proc.returncode}
        # 采集步：按冻结布局写 capture meta（记录仍然只是工具产物，不含结论）
        if step["phase"] in ("measure", "pilot"):
            cap = self.plan["capture"]
            recs = [{
                "path": os.path.join(step_dir, cap["file"]), "format": cap["format"], "channels": int(cap["channels"]),
                "channel_index": int(cap["channel_index"]), "device": cap.get("device", "umik"),
                "rate": int(self.plan["fs"]),
            }]
            if step.get("mic") and step.get("phase") == "pilot":
                recs.append({
                    "path": os.path.join(step_dir, cap["capture2_file"]), "format": cap["format"],
                    "channels": int(cap["capture2_channels"]), "channel_index": 0,
                    "device": step.get("mic", "ma1"), "rate": int(self.plan["fs"]),
                })
            meta = measure.make_meta(step, self.plan, recs, source="cmd")
            meta["not_run"] = ["板端真实采集产物：需窗口回传；本 run 只验证命令与取证布局"]
            meta_path = os.path.join(self.workdir, "captures", "%s.meta.json" % step["id"])
            measure.save_meta(meta, meta_path)
            self.records.append({"step": step["id"], "meta": meta_path, "simulated": False,
                                 "capture_files": [os.path.join(step_dir, r["path"]) for r in recs]})
        if step["phase"] == "apply":
            self.applied = {"source": "cmd"}
        return {"status": "ok", "artifacts": [step_dir], "elapsed_s": round(elapsed, 3)}

    # ------------------------------------------------------------------- local
    def _do_local(self, step: dict) -> dict:
        if step["phase"] == "freeze":
            return self._do_freeze(step)
        if step["phase"] in ("analyze", "report"):
            return {"status": "deferred", "artifacts": [],
                    "note": "由 khaen_cal_batch analyze 在工作站执行（窗口内不现场分析）"}
        return {"status": "skipped", "artifacts": [], "note": "local step %s" % step["id"]}

    # -------------------------------------------------------------------- main
    def execute(self) -> dict:
        os.makedirs(self.workdir, exist_ok=True)
        self._write_json(os.path.join(self.workdir, "plan.json"), self.plan)
        if not self.dry_run:
            self._write_json(os.path.join(self.workdir, "run.json"), {
                "schema": RUN_SCHEMA, "started_at": _now(), "plan_sha256": self.plan["plan_sha256"],
                "batch_label": self.plan["batch_label"], "round": self.plan["round"],
                "transport": self.transport, "scenario": self.scenario,
                "workdir": self.workdir, "dry_run": False, "status": "running",
            })
        commands_log = []
        failed = None
        for step in self.plan["steps"]:
            if step["kind"] == "local":
                state = {"status": "dry-run"} if self.dry_run else self._do_local(step)
            elif self.dry_run:
                state = {"status": "dry-run", "argv": step["argv"]}
                if step["argv"]:
                    commands_log.append("[%02d] %s" % (step["index"], " ".join(step["argv"])))
            else:
                step_dir = self._step_dir(step)
                os.makedirs(step_dir, exist_ok=True)
                try:
                    if self.transport == "fake":
                        if step["phase"] == "preflight":
                            state = self._fake_preflight(step)
                        elif step["phase"] == "measure" or step["phase"] == "pilot":
                            state = self._fake_measure(step)
                        elif step["phase"] == "apply":
                            state = self._fake_apply(step)
                        elif step["phase"] == "restore":
                            state = self._fake_restore(step)
                        else:
                            state = self._cmd_run(step)
                    elif self.transport == "cmd":
                        state = self._cmd_run(step)
                        self._log("cmd %s rc=%s" % (step["id"], state.get("exit_code", 0)))
                    else:
                        raise ValueError("未知 transport: %r" % self.transport)
                except Exception as exc:  # noqa: BLE001 - 转成 fail-closed 状态
                    state = {"status": "failed", "error": str(exc)}
                if step["argv"]:
                    commands_log.append("[%02d] %s  -> %s" % (step["index"], " ".join(step["argv"]),
                                                              state.get("status", "?")))
            state["step"] = step["id"]
            state["phase"] = step["phase"]
            self.steps_state.append(state)
            self._log("step %-28s %-10s %s" % (step["id"], step["phase"], state.get("status", "?")))
            if state.get("status") == "failed" and step["phase"] in ABORT_PHASES and self.abort_on_error:
                failed = {"step": step["id"], "error": state.get("error", "failed"),
                          "exit_code": state.get("exit_code", 4)}
                break
        if self.dry_run:
            print("\n".join(commands_log))
            return {"status": "dry-run", "steps": self.steps_state}
        # 收尾：失败时尝试 restore
        restore_info = None
        if failed is not None:
            restore_step = next((s for s in self.plan["steps"] if s["phase"] == "restore"), None)
            if restore_step is not None and self.transport == "cmd":
                restore_info = self._cmd_run(restore_step)
            elif restore_step is not None:
                restore_info = self._fake_restore(restore_step)
        with open(os.path.join(self.workdir, "commands-executed.txt"), "w", encoding="utf-8") as fh:
            fh.write("\n".join(commands_log) + ("\n" if commands_log else ""))
        status = "ok" if failed is None else "failed"
        summary = {
            "schema": RUN_SCHEMA, "finished_at": _now(), "plan_sha256": self.plan["plan_sha256"],
            "batch_label": self.plan["batch_label"], "round": self.plan["round"],
            "transport": self.transport, "scenario": self.scenario, "workdir": self.workdir,
            "status": status, "failure": failed, "restore": restore_info,
            "steps": self.steps_state,
            "not_run": self.plan.get("not_run", []),
            "board_run": "NOT_RUN",
            "real_measurement": "NOT_RUN",
            "note": "本 run.json 只证明批量编排/取证；真实板端采集与 apply 未执行（NOT_RUN）。",
        }
        self._write_json(os.path.join(self.workdir, "run.json"), summary)
        return summary


def run_plan(plan_path: str, workdir: str, transport: str = "cmd", scenario: str = "",
             dry_run: bool = False) -> dict:
    plan = _load_plan(plan_path)
    if scenario:
        plan["scenario"] = scenario
    runner = Runner(plan, workdir, transport=transport, scenario=plan.get("scenario") or "improved",
                    dry_run=dry_run)
    return runner.execute()
