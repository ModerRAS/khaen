"""TSPI 校准批量闭环：测量记录（capture meta）解析与 IR 提取。

capture meta schema：tspi-cal-batch-capture/v1
板端 khaen-calibrate measure-multi 产出 capture.raw（S24_3LE 单通道/多通道）+ 每通道
*.analysis.json；本模块只依赖冻结的 sweep 布局（plan 生成）与裸流本身，不依赖 Rust 工具内部结论。

布局公式（与 calibrate/src/main.rs measure-multi 一致）：
  interval = sweep_seconds + repeat_gap_seconds
  slot     = repeats * interval + slot_gap_seconds
  record_seconds = pre_seconds + n_channels * slot + post_seconds + 2.0
测量记录起点到播放起点的偏移（lead_in_seconds）由 plan 冻结为 0.35 s（单设备）或 1.0 s（双设备）；
分析用互相关在每个 slot 的搜索窗内重新定位实际起点，不信任该偏移（只作搜索窗中心）。
"""

from __future__ import annotations

import hashlib
import json
import os
from typing import List

CAPTURE_SCHEMA = "tspi-cal-batch-capture/v1"


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sweep_timeline(sweep: dict, channels: int, repeats: int) -> dict:
    fs = int(sweep["rate"])
    seconds = float(sweep["seconds"])
    gap = float(sweep.get("repeat_gap_seconds", 0.5))
    slot_gap = float(sweep.get("slot_gap_seconds", 0.1))
    pre = float(sweep.get("pre_seconds", 1.5))
    post = float(sweep.get("post_seconds", 1.5))
    interval = seconds + gap
    slot = repeats * interval + slot_gap
    record_seconds = pre + channels * slot + post + 2.0
    return {
        "rate": fs,
        "seconds": seconds,
        "pre_seconds": pre,
        "post_seconds": post,
        "repeat_gap_seconds": gap,
        "slot_gap_seconds": slot_gap,
        "interval_seconds": interval,
        "slot_seconds": slot,
        "sweep_samples": int(round(seconds * fs)),
        "interval_samples": int(round(interval * fs)),
        "slot_samples": int(round(slot * fs)),
        "pre_samples": int(round(pre * fs)),
        "record_seconds": record_seconds,
        "channels": int(channels),
        "repeats": int(repeats),
    }


def make_meta(plan_step: dict, plan: dict, recordings: List[dict], source: str) -> dict:
    """构造 capture meta。recordings: [{device, path, channels, format, channel_index}]。"""
    tl = sweep_timeline(plan["sweep"], len(plan["channels"]), plan_step["repeats"])
    meta = {
        "schema": CAPTURE_SCHEMA,
        "step_id": plan_step["id"],
        "batch_label": plan["batch_label"],
        "round": plan["round"],
        "lane": plan_step.get("lane", "production"),
        "timepoint": plan_step["timepoint"],
        "point": plan_step["point"],
        "point_role": plan_step.get("point_role", ""),
        "pilot_only": bool(plan_step.get("pilot_only", False)),
        "source": source,
        "rate": tl["rate"],
        "sweep": {
            "start_hz": float(plan["sweep"]["start_hz"]),
            "end_hz": float(plan["sweep"]["end_hz"]),
            "seconds": tl["seconds"],
            "fade_ms": float(plan["sweep"].get("fade_ms", 10.0)),
            "level_dbfs": float(plan.get("drive_level_dbfs", -50.0)),
        },
        "timeline": tl,
        "lead_in_seconds": float(plan.get("capture_lead_in_seconds", 0.35)),
        "channels": [
            {"slot": c["slot"], "playback_channel": c["playback_channel"], "name": c["name"],
             "fit_band_hz": list(c.get("fit_band_hz", [60.0, 20000.0]))}
            for c in plan["channels"]
        ],
        "recordings": recordings,
        "created_at": plan.get("created_at", ""),
        "not_run": [],
        "notes": [],
    }
    return meta


def validate_meta(meta: dict) -> List[str]:
    problems = []
    if meta.get("schema") != CAPTURE_SCHEMA:
        problems.append("schema 不符: %r" % meta.get("schema"))
    for key in ("step_id", "lane", "timepoint", "point", "rate", "sweep", "timeline", "channels", "recordings"):
        if key not in meta:
            problems.append("缺字段 %s" % key)
    if not meta.get("recordings"):
        problems.append("无 recordings")
    return problems


def load_recording(rec: dict):
    import numpy as np
    from . import dsp
    fmt = rec.get("format", "wav")
    ch = int(rec.get("channels", 1))
    if fmt == "wav":
        data, fs, nch = dsp.read_wav(rec["path"])
    elif fmt == "raw_s24":
        data = dsp.read_raw_s24(rec["path"], ch)
        fs = int(rec.get("rate", 0))
        nch = ch
    else:
        raise ValueError("不支持的记录格式: %r" % fmt)
    idx = int(rec.get("channel_index", 0))
    if nch > 1:
        if idx >= nch:
            raise ValueError("channel_index %d 超出 %d 通道" % (idx, nch))
        data = data[:, idx]
    return np.asarray(data, dtype=np.float64), fs


def extract_capture(meta: dict, ir_seconds: float = 1.0, verbose: bool = False) -> dict:
    """从 capture meta 提取每通道 IR（重复遍取中位数）与质量指标。

    返回：{ok, point, lane, timepoint, rate, irs{name: np.ndarray}, quality{...}, reasons}
    （numpy/dsp 延迟导入：plan/run --transport cmd 只需要标准库。）
    """
    import numpy as np
    from . import dsp
    problems = validate_meta(meta)
    out = {
        "ok": False,
        "step_id": meta.get("step_id", ""),
        "point": meta.get("point", ""),
        "point_role": meta.get("point_role", ""),
        "lane": meta.get("lane", "production"),
        "timepoint": meta.get("timepoint", ""),
        "pilot_only": bool(meta.get("pilot_only", False)),
        "rate": int(meta.get("rate", 0)),
        "irs": {},
        "quality": {"per_channel": {}, "valid": False},
        "reasons": list(problems),
    }
    if problems:
        return out
    tl = meta["timeline"]
    fs = int(meta["rate"])
    if fs <= 0:
        out["reasons"].append("rate<=0")
        return out
    sweep_cfg = meta["sweep"]
    sweep = dsp.log_sweep(fs, sweep_cfg["start_hz"], sweep_cfg["end_hz"], sweep_cfg["seconds"],
                          sweep_cfg.get("fade_ms", 10.0), 0.0)
    inv = dsp.inverse_filter(sweep, fs, sweep_cfg["start_hz"], sweep_cfg["end_hz"], sweep_cfg["seconds"])
    try:
        rec, rec_fs = load_recording(meta["recordings"][0])
    except Exception as exc:  # noqa: BLE001 - 降级为 INCOMPLETE，不中断批量
        out["reasons"].append("读取记录失败: %s" % exc)
        return out
    if rec_fs and int(rec_fs) != fs:
        out["reasons"].append("记录 rate=%s 与 meta rate=%d 不符" % (rec_fs, fs))
        return out
    lead = float(meta.get("lead_in_seconds", 0.35))
    # IR 窗口必须短于重复间隔，避免把下一遍 sweep 的直达峰卷进同一 IR（离线自测暴露）。
    ir_seconds = min(float(ir_seconds), max(0.05, float(tl["interval_seconds"]) - 0.05))
    ir_n = int(round(ir_seconds * fs))
    # 起点搜索：冻结 lead 已知，只容许 ±0.25 s 偏差；窗内只接受中心附近的峰，
    # 避免把相邻 repeat（间隔 0.7 s 起）误当本遍起点（离线自测已暴露该风险）。
    search = int(round(0.25 * fs))
    accept_span = int(round(0.25 * fs))
    all_valid = True
    for c in meta["channels"]:
        name = c["name"]
        slot = int(c["slot"])
        ch_irs = []
        dpk = []
        for k in range(int(tl["repeats"])):
            center = int(round((lead + tl["pre_seconds"] + slot * tl["slot_seconds"]
                                + k * tl["interval_seconds"]) * fs))
            lo = max(0, center - search)
            hi = min(len(rec), center + int(tl["sweep_samples"]) + 2 * search)
            if hi - lo < int(tl["sweep_samples"]) + 256:
                out["reasons"].append("%s rep%d 搜索窗越界" % (name, k))
                all_valid = False
                continue
            win = rec[lo:hi]
            corr = dsp.xcorr_abs(win, sweep)
            # 只接受 onset ∈ center±0.25 s 的峰（严格排除相邻 repeat 的同等峰）
            limit = min(len(corr), (center - lo) + accept_span)
            if limit <= 0:
                xc = -1
            else:
                xc = int(np.argmax(corr[:limit]))
            if xc < 0:
                out["reasons"].append("%s rep%d 未找到扫频起点" % (name, k))
                all_valid = False
                continue
            onset = lo + xc
            if abs(onset - center) > accept_span:
                out["reasons"].append("%s rep%d 起点偏离 %.0f samples" % (name, k, onset - center))
                all_valid = False
                continue
            try:
                ir = dsp.extract_ir(rec, sweep, inv, fs, onset, ir_seconds)
            except Exception as exc:  # noqa: BLE001
                out["reasons"].append("%s rep%d 反卷积失败: %s" % (name, k, exc))
                all_valid = False
                continue
            ch_irs.append(ir)
            dpk.append(int(np.argmax(np.abs(ir))))
        if not ch_irs:
            continue
        stack = np.vstack(ch_irs)
        med = np.median(stack, axis=0)
        snr = dsp.estimate_snr_db(med, fs)
        grid = dsp.log_freq_grid(40.0, min(10000.0, fs * 0.45), 6)
        frs = []
        for ir in ch_irs:
            f, db, _ = dsp.fr_from_ir(ir, fs, smooth_frac=6)
            frs.append(np.array([dsp.interp_db_at(f, db, gf) for gf in grid]))
        frs = np.vstack(frs)
        spread_db = float(np.max(np.ptp(frs, axis=0))) if len(ch_irs) > 1 else 0.0
        delay_spread = float(np.max(dpk) - np.min(dpk)) if len(dpk) > 1 else 0.0
        spread_rms = float(np.sqrt(np.mean(np.ptp(frs, axis=0) ** 2))) if len(ch_irs) > 1 else 0.0
        valid_ch = True
        reasons = []
        if snr < 25.0:
            valid_ch = False
            reasons.append("snr_low(%.1f dB < 25)" % snr)
        if delay_spread > 0.5:
            valid_ch = False
            reasons.append("repeat_delay_spread_large(%.2f samples)" % delay_spread)
        if spread_db > 0.8:
            valid_ch = False
            reasons.append("repeat_fr_spread_large(%.2f dB)" % spread_db)
        if not valid_ch:
            all_valid = False
        out["irs"][name] = med
        out["quality"]["per_channel"][name] = {
            "snr_db": round(snr, 2),
            "repeat_mag_spread_max_db": round(spread_db, 3),
            "repeat_mag_spread_rms_db": round(spread_rms, 3),
            "repeat_delay_spread_samples": round(delay_spread, 3),
            "repeats": len(ch_irs),
            "valid": bool(valid_ch),
            "reasons": reasons,
        }
    out["quality"]["valid"] = bool(all_valid and len(out["irs"]) == len(meta["channels"]))
    if not out["quality"]["valid"] and not out["reasons"]:
        bad = [n for n, q in out["quality"]["per_channel"].items() if not q["valid"]]
        out["reasons"].append("通道质量门失败: %s" % ", ".join(bad))
    out["ok"] = bool(out["irs"])
    if verbose:
        print("[capture] %s/%s/%s valid=%s channels=%d reasons=%s"
              % (out["lane"], out["timepoint"], out["point"], out["quality"]["valid"],
                 len(out["irs"]), out["reasons"]))
    return out


def save_meta(meta: dict, path: str) -> str:
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(meta, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    return path


def load_meta(path: str) -> dict:
    with open(path, "r", encoding="utf-8") as fh:
        return json.load(fh)
