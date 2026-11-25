"""TSPI 校准批量闭环：离线合成房间/应用模型（fake transport 专用）。

诚实性边界（写进证据，别混用）：
- 本模型只用于**离线自测**：证明批量编排、采集布局、反卷积、瀑布/RT/前后对比/问题定位
  这条流水线能跑通，并验证"指标变差时能定位到具体频段/通道"。
- 它不是真实校准算法，不产生任何可用于 T67/T6-M 签收的结论；真实房间结果一律 NOT_RUN。
- fake apply 只允许用 design 点数据拟合（holdout 不参与），用于验证 holdout 隔离逻辑。

模型结构（可复现，seed 固定）：
  IR(ch, p) = delay/polarity/early-reflections  +  低频模态（频率/衰减/位置耦合）
              + 音箱宽带基调（biquad）  经  PEQ（apply 后）+ 模态阻尼（apply 后）
  capture = sweep 与 IR 的线性卷积 + 噪声 + 已知 lead_in 偏移
"""

from __future__ import annotations

import json
import math
import os

import numpy as np

from . import dsp, measure

SCENARIOS = ("improved", "worse", "flat")


def _rng(seed: int):
    return np.random.default_rng(int(seed))


def _mode_bank(fs: int, seed: int) -> dict:
    """固定一组低频模态：频率/衰减时间常数/基准幅度。"""
    rng = _rng(seed + 1000)
    freqs = np.array([35.0, 42.0, 50.0, 59.0, 71.0, 84.0, 100.0, 118.0, 140.0, 168.0, 200.0, 240.0])
    freq_jit = 1.0 + 0.03 * rng.standard_normal(len(freqs))
    taus = np.array([0.080, 0.075, 0.070, 0.065, 0.060, 0.055, 0.050, 0.045, 0.040, 0.035, 0.030, 0.025])
    tau_jit = 1.0 + 0.08 * rng.standard_normal(len(freqs))
    gains_db = np.array([9.0, 8.0, 7.5, 7.0, 6.0, 6.0, 5.5, 5.0, 4.5, 4.0, 3.5, 3.0])
    return {"freqs": freqs * freq_jit, "taus": taus * tau_jit, "gains_db": gains_db}


def _position_xyz(points: list) -> dict:
    """按配置顺序给测点确定性的三维位置（米）；坐标只用于模态耦合和 holdout 空间分离。"""
    coords = {}
    for i, pt in enumerate(points):
        x = 0.5 * (i % 3) + 0.62
        y = 0.5 * (i // 3) + 0.95
        z = 1.15
        coords[pt["id"]] = (x, y, z)
    return coords


def _mode_coupling(mode_idx: int, xyz, room=(4.2, 5.4, 2.7)) -> float:
    """简单轴向模态形状：sin(pi*n*x/Lx)*sin(pi*m*y/Ly)，位置不同耦合不同。"""
    x, y, _z = xyz
    lx, ly, _lz = room
    n = (mode_idx % 3) + 1
    m = (mode_idx // 3) + 1
    return float(math.sin(math.pi * n * x / lx) * math.sin(math.pi * m * y / ly))


def _channel_params(plan: dict, seed: int) -> dict:
    """每通道：延迟/极性/音色 biquad（宽带）。"""
    rng = _rng(seed + 2000)
    out = {}
    for i, ch in enumerate(plan["channels"]):
        name = ch["name"]
        out[name] = {
            "slot": int(ch["slot"]),
            "playback_channel": int(ch["playback_channel"]),
            "delay_samples": int(round(20 + 40 * i + 10 * abs(rng.standard_normal()))),
            "polarity": 1.0 if (i % 5 != 3) else -1.0,
            "gain_db": float(rng.uniform(-2.0, 2.0)),
            "tone_filters": [
                dsp.peaking_biquad(float(rng.uniform(90.0, 160.0)), float(rng.uniform(1.5, 3.0)), 0.8, int(plan["fs"])),
                dsp.peaking_biquad(float(rng.uniform(700.0, 1200.0)), float(rng.uniform(-2.0, 1.5)), 1.3, int(plan["fs"])),
                dsp.peaking_biquad(float(rng.uniform(2500.0, 4500.0)), float(rng.uniform(-1.5, 1.2)), 2.0, int(plan["fs"])),
            ],
        }
        band = ch.get("fit_band_hz", [60.0, 20000.0])
        if band and float(band[1]) <= 200.0:
            out[name]["tone_filters"].append(dsp.lowpass_biquad(float(band[1]), int(plan["fs"])))
    return out


class PlantModel:
    """确定性合成 plant：render_capture() 生成一整个 measure-multi 风格的记录。"""

    def __init__(self, plan: dict, seed: int = 20261117):
        self.plan = plan
        self.fs = int(plan["fs"])
        self.seed = int(seed)
        self.modes = _mode_bank(self.fs, self.seed)
        self.channels = _channel_params(plan, self.seed)
        self.coords = _position_xyz(plan["design_points"] + plan["holdout_points"] + plan.get("pilot_points", []))
        self.rng = _rng(self.seed + 3000)
        self.modal_bands = [40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0]

    # ---------------------------------------------------------------- final IR
    def render_ir(self, channel: str, point_id: str, applied: dict, ir_seconds: float) -> np.ndarray:
        ch = self.channels[channel]
        fs = self.fs
        n = int(round(ir_seconds * fs))
        t = np.arange(n, dtype=np.float64) / float(fs)
        ir = np.zeros(n, dtype=np.float64)
        delay = int(min(max(ch["delay_samples"], 0), max(n - 32, 1)))
        amp = 10.0 ** (ch["gain_db"] / 20.0)
        ir[delay] += ch["polarity"] * amp
        rng = _rng(self.seed + 4000 + int(ch["slot"]))
        for _ in range(4):
            tap = delay + int(rng.integers(int(0.004 * fs), int(0.05 * fs)))
            if tap < n:
                ir[tap] += ch["polarity"] * amp * 10.0 ** (float(rng.uniform(-18.0, -10.0)) / 20.0) * float(rng.choice([-1.0, 1.0]))
        xyz = self.coords.get(point_id, (1.0, 1.4, 1.15))
        damp = applied.get("modal_damp", {}) if applied else {}
        for m in range(len(self.modes["freqs"])):
            f = float(self.modes["freqs"][m])
            tau = float(self.modes["taus"][m])
            factor = _band_factor(damp, f)
            tau_eff = max(tau * factor, 0.01)
            coupling = _mode_coupling(m, xyz)
            g = 10.0 ** (float(self.modes["gains_db"][m]) / 20.0)
            modal_amp = g * abs(coupling) * (1.0 if coupling >= 0 else -1.0)
            start = delay
            if start >= n:
                continue
            tt = t[start:] - t[start]
            shape = np.exp(-tt / tau_eff) * np.sin(2.0 * math.pi * f * tt + 0.3 * m)
            seg = modal_amp * amp * shape
            ir[start:] += seg[: n - start]
        filters = list(ch["tone_filters"])
        if applied and applied.get("peq", {}).get(channel):
            filters = filters + list(applied["peq"][channel])
        ir = dsp.apply_filters_fft(ir, filters, fs)
        peak = float(np.max(np.abs(ir))) or 1.0
        return ir / peak

    # ------------------------------------------------------------- render 记录
    def render_capture(self, point_id: str, applied: dict, lane: str, repeats: int,
                       lead_in_seconds: float, device_specs: list, capture2: bool = False) -> dict:
        plan = self.plan
        sweep_cfg = plan["sweep"]
        fs = self.fs
        channels = plan["channels"]
        tl = measure.sweep_timeline(dict(sweep_cfg, rate=fs), len(channels), repeats)
        sweep = dsp.log_sweep(fs, sweep_cfg["start_hz"], sweep_cfg["end_hz"], sweep_cfg["seconds"],
                              sweep_cfg.get("fade_ms", 10.0), 0.0)
        total = int(round((lead_in_seconds + tl["record_seconds"]) * fs))
        rng = _rng(self.seed + 5000 + abs(hash(point_id)) % 10000 + (7 if applied else 0))
        rec = rng.standard_normal(total) * 1e-5  # 噪声底 -100 dBFS 量级
        ir_seconds = float(plan.get("analysis", {}).get("ir_seconds", 1.0))
        for i, ch in enumerate(channels):
            ir = self.render_ir(ch["name"], point_id, applied, ir_seconds)
            conv = dsp.fft_conv(sweep, ir)
            for k in range(repeats):
                start = int(round((lead_in_seconds + tl["pre_seconds"] + i * tl["slot_seconds"]
                                   + k * tl["interval_seconds"]) * fs))
                end = min(total, start + len(conv))
                if end <= start:
                    continue
                rec[start:end] += conv[: end - start] * 0.5
        peak = float(np.max(np.abs(rec)))
        if peak > 0.99:
            rec = rec * (0.99 / peak)
        out = {"recordings": [], "meta_extra": {}}
        wav_rel = "capture-%s.wav" % lane
        out["recordings"].append({"path": wav_rel, "device": device_specs[0] if device_specs else "umik",
                                  "channels": 1, "format": "wav", "channel_index": 0})
        if capture2:
            ref = rng.standard_normal(total) * 1e-5
            ch_name = channels[0]["name"] if channels else "FL"
            ir_ref = self.render_ir(ch_name, point_id, applied, ir_seconds)
            ir_ref = dsp.apply_filters_fft(ir_ref, [dsp.peaking_biquad(300.0, 2.0, 0.7, fs)], fs)
            conv2 = dsp.fft_conv(sweep, ir_ref)
            for i in range(len(channels)):
                for k in range(repeats):
                    start = int(round((lead_in_seconds + tl["pre_seconds"] + i * tl["slot_seconds"]
                                       + k * tl["interval_seconds"]) * fs))
                    end = min(total, start + len(conv2))
                    if end > start:
                        ref[start:end] += conv2[: end - start] * 0.4
            out["recordings"].append({"path": "capture2-%s.wav" % lane, "device": "ma1",
                                      "channels": 1, "format": "wav", "channel_index": 0,
                                      "simulated_ref": True})
            out["meta_extra"]["capture2"] = ref
        return {"data": rec, "capture2_data": out["meta_extra"].get("capture2"), "recordings": out["recordings"]}

   
def _band_factor(damp: dict, freq: float) -> float:
    if not damp:
        return 1.0
    bands = sorted(float(b) for b in damp.keys())
    vals = [float(damp[str(b)]) if str(b) in damp else float(damp[b]) for b in bands]
    if freq <= bands[0]:
        return vals[0]
    if freq >= bands[-1]:
        return vals[-1]
    return float(np.interp(freq, bands, vals))


# --------------------------------------------------------------- fake apply


def _peq_fit_reference(plan: dict, fs: int, irs_by_point: dict) -> dict:
    """用 design 点平均 FR 拟合每通道 PEQ（demo 算法；真实算法以 tspi-peq-fit T64 为准）。"""
    peq = {}
    band_lo = float(plan.get("analysis", {}).get("peq_fit_band_hz", [60.0, 8000.0])[0])
    band_hi = float(plan.get("analysis", {}).get("peq_fit_band_hz", [60.0, 8000.0])[1])
    max_stages = int(plan.get("analysis", {}).get("peq_max_stages", 6))
    boost_cap = float(plan.get("analysis", {}).get("peq_boost_cap_db", 3.0))
    cut_cap = float(plan.get("analysis", {}).get("peq_cut_cap_db", 12.0))
    for ch in plan["channels"]:
        name = ch["name"]
        fit_band = ch.get("fit_band_hz", [band_lo, band_hi])
        lo = max(float(fit_band[0]), band_lo)
        hi = min(float(fit_band[1]), band_hi)
        if hi <= lo * 1.05:
            continue
        grid = dsp.log_freq_grid(max(20.0, lo * 0.8), min(16000.0, hi * 1.15), 24)
        accum = np.zeros(len(grid), dtype=np.float64)
        count = 0
        for point_irs in irs_by_point.values():
            ir = point_irs.get(name)
            if ir is None:
                continue
            f, db, _ = dsp.fr_from_ir(ir, fs, smooth_frac=6)
            accum += np.array([dsp.interp_db_at(f, db, g) for g in grid])
            count += 1
        if count == 0:
            continue
        avg = accum / count
        inband = (grid >= lo) & (grid <= hi)
        ref_idx = int(np.argmin(np.abs(grid - 1000.0)))
        ref = float(avg[ref_idx]) if inband[ref_idx] else float(np.mean(avg[inband]))
        dev = avg - ref
        filters = []
        residual = dev.copy()
        for _ in range(max_stages):
            residual_masked = np.where(inband, residual, 0.0)
            idx = int(np.argmax(np.abs(residual_masked)))
            if abs(residual_masked[idx]) < 0.7:
                break
            fc = float(grid[idx])
            target_gain = float(-residual_masked[idx])
            target_gain = max(-cut_cap, min(boost_cap, target_gain))
            if abs(target_gain) < 0.5:
                break
            best = None
            for q in (0.5, 0.8, 1.2, 2.0, 3.5, 6.0):
                bq = dsp.peaking_biquad(fc, target_gain, q, fs)
                resp = dsp.biquad_response_db(bq, grid, fs)
                cand = residual + resp
                score = float(np.sqrt(np.mean(cand[inband] ** 2)))
                if best is None or score < best[0]:
                    best = (score, bq, cand)
            if best is None:
                break
            filters.append(best[1])
            residual = best[2]
        if filters:
            peq[name] = filters
    return peq


def derive_compensation(plan: dict, scenario: str, baseline_irs: dict, fs: int) -> dict:
    """从 design 点基线测量推导 fake apply 状态；holdout 不进拟合输入。"""
    if scenario not in SCENARIOS:
        raise ValueError("未知 scenario %r（%s）" % (scenario, "/".join(SCENARIOS)))
    design_ids = [p["id"] for p in plan["design_points"]]
    design_irs = {pid: baseline_irs[pid] for pid in design_ids if pid in baseline_irs}
    peq = {} if scenario == "flat" else _peq_fit_reference(plan, fs, design_irs)
    damp = {}
    if scenario == "improved":
        factor = 0.72
    elif scenario == "worse":
        factor = 1.28
    else:
        factor = 1.0
    for b in (40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0):
        damp[str(b)] = factor
    if scenario == "worse":
        # 故意把两条最差通道的 PEQ 反号（模拟"拟合方向错"），用于验证问题定位能指出通道
        for name in list(peq.keys())[:2]:
            peq[name] = [dict(bq, gain_db=-bq["gain_db"],
                              b=[b0 for b0 in bq["b"]], a=list(bq["a"])) for bq in peq[name]]
            # 反号需要重建系数
            peq[name] = [dsp.peaking_biquad(bq["freq_hz"], -bq["gain_db"], bq["q"], fs) for bq in peq[name]]
    state = {
        "schema": "tspi-cal-batch-fake-applied/v1",
        "scenario": scenario,
        "fitted_from_design_points": design_ids,
        "holdout_points_used": [],
        "peq": {name: [dict(bq) for bq in filters] for name, filters in peq.items()},
        "modal_damp": damp,
        "note": "fake apply：仅用于离线自测流水线，不是真实校准算法；不得用于任何签收。",
    }
    return state


# ------------------------------------------------------- 写出 capture 记录


def write_capture_files(workdir: str, step: dict, plan: dict, rendered: dict) -> list:
    """把 render_capture 结果写成 WAV，返回 recordings 列表（相对 step_dir 的文件名）。"""
    step_dir = os.path.join(workdir, "steps", step["dir_name"])
    os.makedirs(step_dir, exist_ok=True)
    recs = []
    for i, rec in enumerate(rendered["recordings"]):
        data = rendered["data"] if i == 0 else rendered["capture2_data"]
        path = os.path.join(step_dir, rec["path"])
        dsp.write_wav_raw(path, np.asarray(data, dtype=np.float64), int(plan["fs"]), 1)
        rec = dict(rec)
        rec["path"] = os.path.basename(path)
        rec["rate"] = int(plan["fs"])
        rec["bytes"] = os.path.getsize(path)
        recs.append(rec)
    return recs
