"""TSPI 校准批量闭环：冻结配置加载与 fail-closed 校验。

校验口径来自（只读）：
- calibration-spec/MICS.md：生产 = 单支 UMIK-1 移动多点；三麦固定仅 pilot。
- rt-stack/crates/calibration/src/positions.rs：design>=6、holdout>=3、design repeats>=3、
  SNR>=25 dB、重复 delay/magnitude 散差门。
- docs/BOARD-WINDOW-RUNBOOK-T67-T74.md §2：48k only、-50 dBFS 起步、白天窗 -30 dB 级、
  可闻扫频授权、CueMix 只读。
- rt-stack/crates/calibration/src/peq_fit.rs：Q/S/gain 限制参考（本工具只作观察指标拟合演示）。
所有错误都是 fail-closed：errors 非空 -> plan 拒绝生成，exit 2。
"""

from __future__ import annotations

import hashlib
import json
import os

CONFIG_SCHEMA = "tspi-cal-batch-config/v1"

DEFAULT_CHANNELS_4_1_2 = [
    {"name": "FL", "playback_channel": 0, "fit_band_hz": [60.0, 20000.0]},
    {"name": "FR", "playback_channel": 1, "fit_band_hz": [60.0, 20000.0]},
    {"name": "SL", "playback_channel": 2, "fit_band_hz": [60.0, 20000.0]},
    {"name": "SR", "playback_channel": 3, "fit_band_hz": [60.0, 20000.0]},
    {"name": "HFL", "playback_channel": 4, "fit_band_hz": [60.0, 20000.0]},
    {"name": "HFR", "playback_channel": 5, "fit_band_hz": [60.0, 20000.0]},
    {"name": "SUB1", "playback_channel": 7, "fit_band_hz": [20.0, 130.0]},
]

MORNING = ("08:30", "12:00")
AFTERNOON = ("14:30", "18:00")

E_BATCH_SCHEMA = "E_BATCH_SCHEMA"
E_BATCH_FS = "E_BATCH_FS"
E_BATCH_POINTS = "E_BATCH_POINTS"
E_BATCH_PARTITION = "E_BATCH_PARTITION"
E_BATCH_PILOT = "E_BATCH_PILOT"
E_BATCH_LEVEL = "E_BATCH_LEVEL"
E_BATCH_WINDOW = "E_BATCH_WINDOW"
E_BATCH_CAL = "E_BATCH_CAL"
E_BATCH_SWEEP = "E_BATCH_SWEEP"
E_BATCH_CHANNELS = "E_BATCH_CHANNELS"
E_BATCH_COMMANDS = "E_BATCH_COMMANDS"
E_BATCH_ESTIMATE = "E_BATCH_ESTIMATE"
I_BATCH_PILOT_ONLY = "I_BATCH_PILOT_ONLY"
I_BATCH_INPUT_UNVERIFIED = "I_BATCH_INPUT_UNVERIFIED"


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def repo_root_from(here: str) -> str:
    """从工具目录（calbatch 或 calib-batch）向上找含 rt-stack/calibration-spec 的项目根。"""
    cur = os.path.abspath(here)
    for _ in range(8):
        if os.path.isdir(os.path.join(cur, "rt-stack")) and os.path.isdir(os.path.join(cur, "calibration-spec")):
            return cur
        parent = os.path.dirname(cur)
        if parent == cur:
            break
        cur = parent
    return os.path.abspath(os.path.join(here, "..", "..", ".."))


def load_config(path: str) -> dict:
    with open(path, "r", encoding="utf-8") as fh:
        cfg = json.load(fh)
    cfg["_config_path"] = os.path.abspath(path)
    return cfg


def _minutes(hhmm: str) -> int:
    hh, mm = hhmm.split(":")
    return int(hh) * 60 + int(mm)


def resolve_time_window(cfg: dict, round_id: str):
    win = cfg.get("window", {})
    rounds = win.get("rounds", {})
    slot = rounds.get(round_id)
    if slot == "morning":
        return MORNING
    if slot == "afternoon":
        return AFTERNOON
    m = win.get("morning", {"start": MORNING[0], "end": MORNING[1]})
    a = win.get("afternoon", {"start": AFTERNOON[0], "end": AFTERNOON[1]})
    if round_id == "A":
        return (m["start"], m["end"])
    return (a["start"], a["end"])


def validate_config(cfg: dict, repo_root: str, verify_inputs: bool = True) -> dict:
    errors = []
    warnings = []
    selftest = bool(cfg.get("selftest", False))

    def err(code, msg):
        errors.append({"code": code, "message": msg})

    def warn(code, msg):
        warnings.append({"code": code, "message": msg})

    if cfg.get("schema") != CONFIG_SCHEMA:
        err(E_BATCH_SCHEMA, "schema 必须是 %s，实际 %r" % (CONFIG_SCHEMA, cfg.get("schema")))

    fs = cfg.get("fs")
    if not isinstance(fs, int) or fs <= 0:
        err(E_BATCH_FS, "fs 必须是正整数")
    else:
        if not selftest:
            if fs != 48000:
                err(E_BATCH_FS, "板端生产校准只允许 48 kHz（UMIK-1/T62 实现即拒其它率），实际 %d" % fs)
        elif fs not in (8000, 16000, 24000, 32000, 44100, 48000):
            err(E_BATCH_FS, "selftest fs 不受支持: %d" % fs)

    # ---- sweep
    sweep = cfg.get("sweep") or {}
    f1, f2 = float(sweep.get("start_hz", 0) or 0), float(sweep.get("end_hz", 0) or 0)
    secs = float(sweep.get("seconds", 0) or 0)
    if not (0 < f1 < f2 <= (fs or 48000) / 2.0):
        err(E_BATCH_SWEEP, "sweep 频率非法: start=%r end=%r fs=%r" % (f1, f2, fs))
    if not (0.05 <= secs <= 120.0):
        err(E_BATCH_SWEEP, "sweep.seconds 需在 [0.05, 120]，实际 %r" % secs)
    if float(sweep.get("pre_seconds", 0) or 0) < 0.1:
        err(E_BATCH_SWEEP, "sweep.pre_seconds 需 >= 0.1")
    if float(sweep.get("post_seconds", 0) or 0) < 0.1:
        err(E_BATCH_SWEEP, "sweep.post_seconds 需 >= 0.1")

    # ---- level / window（板端生产口径）
    level = float(cfg.get("drive_level_dbfs", 0) or 0)
    if level > 0.0 or level < -60.0:
        err(E_BATCH_LEVEL, "drive_level_dbfs 需在 [-60, 0]，实际 %r" % level)
    win = cfg.get("window", {})
    if not selftest:
        if level > -30.0:
            err(E_BATCH_LEVEL, "drive_level_dbfs=%r 属于可闻级；批量流程只允许 <= -30 dBFS 且必须在白天窗" % level)
        elif level > -50.0:
            if not win.get("quiet_hours_ack"):
                err(E_BATCH_WINDOW, "drive_level_dbfs=%r 高于 -50；需要 window.quiet_hours_ack=true 且白天窗" % level)
        if not win.get("audible_sweep_authorized"):
            err(E_BATCH_WINDOW, "window.audible_sweep_authorized 必须为 true（可闻扫频授权，runbook C11）")
        if not win.get("quiet_hours_ack"):
            err(E_BATCH_WINDOW, "window.quiet_hours_ack 必须为 true（安静时段授权）")
        for rid in ("A", "B"):
            ws, we = resolve_time_window(cfg, rid)
            if not (_minutes(ws) >= _minutes(MORNING[0]) and _minutes(we) <= _minutes(MORNING[1])) and \
               not (_minutes(ws) >= _minutes(AFTERNOON[0]) and _minutes(we) <= _minutes(AFTERNOON[1])):
                err(E_BATCH_WINDOW, "round %s 窗口 %s-%s 不在 08:30-12:00/14:30-18:00 白天窗" % (rid, ws, we))

    # ---- mic
    mic = cfg.get("mic") or {}
    if selftest:
        if not mic:
            warn(I_BATCH_INPUT_UNVERIFIED, "selftest 未声明 mic；生产配置必须写 mic{model,serial,cal_file,cal_sha256,orientation}")
    else:
        for key in ("model", "serial", "cal_file", "cal_sha256", "orientation"):
            if not mic.get(key):
                err(E_BATCH_CAL, "mic 缺字段 %s（runbook §4.1：报告必须记录 cal_file+sha256+orientation）" % key)
        if mic.get("orientation") != "90deg":
            err(E_BATCH_CAL, "生产口径 UMIK-1 必须 90deg（垂直朝上），实际 %r" % mic.get("orientation"))
        if mic.get("pilot_only"):
            err(E_BATCH_CAL, "生产 lane 的 mic.pilot_only 必须 false")
        if verify_inputs and mic.get("cal_file"):
            path = mic["cal_file"]
            if not os.path.isabs(path):
                path = os.path.join(repo_root, path)
            if not os.path.isfile(path):
                err(E_BATCH_CAL, "mic cal 文件不存在: %s" % path)
            else:
                got = sha256_file(path)
                want = str(mic.get("cal_sha256", "")).lower()
                if want and got.lower() != want:
                    err(E_BATCH_CAL, "mic cal sha256 不符: 期望 %s 实际 %s" % (want, got))
    if verfy_skip_placeholder := False:
        pass

    # ---- channels
    channels = cfg.get("channels") or DEFAULT_CHANNELS_4_1_2
    names = set()
    slots = set()
    if len(channels) > 32:
        err(E_BATCH_CHANNELS, "通道数 > 32")
    for ch in channels:
        nm = str(ch.get("name", "")).strip()
        if not nm:
            err(E_BATCH_CHANNELS, "channel 缺 name")
        if nm in names:
            err(E_BATCH_CHANNELS, "channel name 重复: %s" % nm)
        names.add(nm)
        slot = int(ch.get("playback_channel", ch.get("slot", -1)))
        if slot < 0 or slot > 255:
            err(E_BATCH_CHANNELS, "%s playback_channel 非法: %r" % (nm, slot))
        if slot in slots:
            err(E_BATCH_CHANNELS, "playback_channel 重复: %d" % slot)
        slots.add(slot)
        band = ch.get("fit_band_hz") or [60.0, 20000.0]
        if len(band) != 2 or float(band[0]) <= 0 or float(band[1]) <= float(band[0]):
            err(E_BATCH_CHANNELS, "%s fit_band_hz 非法: %r" % (nm, band))

    # ---- 测点：design>=6 + holdout>=3（mimo-t6 §3.2），复次>=3（positions.rs MIN_DESIGN_REPEATS）
    design = cfg.get("design_points") or []
    holdout = cfg.get("holdout_points") or []
    ids = []
    for group, label, min_count in ((design, "design", 6), (holdout, "holdout", 3)):
        if len(group) < min_count:
            err(E_BATCH_POINTS, "%s 点 %d 个 < %d（mimo-t6 §3.2；5+3 只能算 pilot → I-03）"
                % (label, len(group), min_count))
        for pt in group:
            pid = str(pt.get("id", "")).strip()
            rol = str(pt.get("role", "")).strip()
            if not pid:
                err(E_BATCH_PARTITION, "%s 点缺 id" % label)
            if pid in ids:
                err(E_BATCH_PARTITION, "测点 id 重复: %s" % pid)
            ids.append(pid)
            expect_role = "design" if label == "design" else "validation"
            if rol and rol != expect_role:
                err(E_BATCH_PARTITION, "%s(%s) role=%r，应为 %r" % (pid, label, rol, expect_role))
            rep = int(pt.get("repeats", cfg.get("sweep", {}).get("repeats", 3)))
            if rep < 3:
                err(E_BATCH_POINTS, "%s repeats=%d < 3（positions.rs 设计点最少 3 遍）" % (pid, rep))
            if not pt.get("label"):
                warn(E_BATCH_POINTS, "%s 缺 label（禁止无标签测点）" % pid)

    # ---- pilot（三麦固定，严格分开）
    pilot = cfg.get("pilot") or {}
    pilot_points = list(pilot.get("points") or []) if pilot.get("enabled") else []
    if pilot.get("enabled"):
        if not pilot.get("pilot_only"):
            err(E_BATCH_PILOT, "pilot.enabled=true 时 pilot.pilot_only 必须 true（MICS.md：三麦只作 pilot）")
        mics = pilot.get("mics") or []
        mic_names = [str(m.get("name", "")).upper() for m in mics]
        for must in ("UMIK", "MA1", "ARC"):
            if not any(nm.startswith(must) for nm in mic_names):
                err(E_BATCH_PILOT, "pilot mics 缺 %s（MICS.md 三麦登记）" % must)
        if not pilot.get("claim"):
            err(E_BATCH_PILOT, "pilot 缺 claim（必须注明 not eligible for T67/T68/T69/T6-M）")
        if not pilot_points:
            warn(I_BATCH_PILOT_ONLY, "pilot.enabled=true 但 points 为空")
        for pt in pilot_points:
            pid = str(pt.get("id", ""))
            if pid in ids:
                err(E_BATCH_PILOT, "pilot 点 %r 与 design/holdout 重名（必须分开标注）" % pid)
            if int(pt.get("repeats", 3)) < 2:
                err(E_BATCH_PILOT, "pilot %s repeats < 2" % pid)
        for pt in pilot_points:
            pid = str(pt.get("id", ""))
            for group in (design, holdout):
                if any(str(x.get("id")) == pid for x in group):
                    err(E_BATCH_PILOT, "pilot 点 %s 混入 design/holdout" % pid)
    cfg["_resolved_channels"] = channels
    cfg["_pilot_points"] = [dict(pt, role="pilot", pilot_only=True) for pt in pilot_points]

    # ---- commands / 估算
    cmds = cfg.get("commands") or {}
    if not selftest:
        for key in ("preflight", "measure", "apply"):
            if key not in cmds:
                err(E_BATCH_COMMANDS, "commands.%s 必须预先冻结（全部命令窗口前生成）" % key)
        for key in ("measure", "apply", "preflight"):
            spec = cmds.get(key) or {}
            if not spec.get("argv"):
                err(E_BATCH_COMMANDS, "commands.%s.argv 为空" % key)
    est = cfg.get("estimates") or {}
    if float(est.get("margin_fraction", 0.15)) <= 0 or float(est.get("margin_fraction", 0.15)) > 1.0:
        err(E_BATCH_ESTIMATE, "estimates.margin_fraction 非法")
    rounds = int(cfg.get("rounds", 1))
    if rounds < 1 or rounds > 2:
        err(E_BATCH_ESTIMATE, "rounds 只支持 1 或 2（A/B 两轮独立重复）")

    return {"errors": errors, "warnings": warnings, "selftest": selftest}


def normalize_config(cfg: dict) -> dict:
    """填充默认值并展开 channels/pilot points，返回新 dict（不改原）。"""
    out = json.loads(json.dumps({k: v for k, v in cfg.items() if not k.startswith("_")}))
    out.setdefault("schema", CONFIG_SCHEMA)
    out.setdefault("batch_label", "t6-batch")
    out.setdefault("rounds", 1)
    out.setdefault("fs", 48000)
    out.setdefault("drive_level_dbfs", -50.0)
    out.setdefault("transport_default", "cmd")
    sweep = out.setdefault("sweep", {})
    sweep.setdefault("start_hz", 20.0)
    sweep.setdefault("end_hz", 20000.0)
    sweep.setdefault("seconds", 5.0)
    sweep.setdefault("pre_seconds", 1.5)
    sweep.setdefault("post_seconds", 1.5)
    sweep.setdefault("repeat_gap_seconds", 0.5)
    sweep.setdefault("slot_gap_seconds", 0.1)
    sweep.setdefault("fade_ms", 10.0)
    sweep.setdefault("repeats", 3)
    sweep["rate"] = int(out["fs"])
    channels = out.get("channels") or DEFAULT_CHANNELS_4_1_2
    norm_ch = []
    for i, ch in enumerate(channels):
        nm = ch.get("name") or "CH%d" % i
        play = ch.get("playback_channel", ch.get("slot", i))
        norm_ch.append({
            "slot": i,
            "index": i,
            "name": nm,
            "playback_channel": int(play),
            "fit_band_hz": list(ch.get("fit_band_hz") or [60.0, 20000.0]),
        })
    out["channels"] = norm_ch
    for key, role in (("design_points", "design"), ("holdout_points", "validation")):
        pts = []
        for pt in out.get(key) or []:
            pts.append({
                "id": str(pt["id"]),
                "label": str(pt.get("label", pt["id"])),
                "role": str(pt.get("role", role)),
                "weight": float(pt.get("weight", 1.0)),
                "repeats": int(pt.get("repeats", sweep["repeats"])),
                "pilot_only": False,
            })
        out[key] = pts
    pilot = out.setdefault("pilot", {"enabled": False})
    pilot_points = []
    if pilot.get("enabled"):
        for pt in pilot.get("points") or []:
            pilot_points.append({
                "id": str(pt["id"]),
                "label": str(pt.get("label", pt["id"])),
                "role": "pilot",
                "weight": float(pt.get("weight", 1.0)),
                "repeats": int(pt.get("repeats", sweep["repeats"])),
                "pilot_only": True,
            })
    out["_pilot_points"] = pilot_points
    out.setdefault("estimates", {})
    est = out["estimates"]
    est.setdefault("move_seconds_per_point", 60.0)
    est.setdefault("apply_seconds", 600.0)
    est.setdefault("preflight_seconds", 180.0)
    est.setdefault("analyze_seconds_per_capture", 20.0)
    est.setdefault("report_seconds", 60.0)
    est.setdefault("margin_fraction", 0.15)
    out.setdefault("analysis", {})
    an = out["analysis"]
    an.setdefault("ir_seconds", 1.0)
    an.setdefault("peq_fit_band_hz", [60.0, 8000.0])
    an.setdefault("peq_max_stages", 6)
    an.setdefault("peq_boost_cap_db", 3.0)
    an.setdefault("peq_cut_cap_db", 12.0)
    an.setdefault("waterfall_times_ms", [0, 20, 40, 60, 80, 100, 150, 200, 300, 400])
    an.setdefault("modal_bands_hz", [40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0])
    an.setdefault("rt_bands_hz", [40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0,
                                  315.0, 400.0, 500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0,
                                  2000.0, 2500.0, 3150.0, 4000.0])
    an.setdefault("seed", 20261117)
    out.setdefault("capture_lead_in_seconds", 0.35)
    return out
