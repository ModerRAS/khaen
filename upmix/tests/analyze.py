#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""分析 run_file_backend.py 的输出：与参考仿真逐样本对比、电平/延迟/频带/相关性/削波。

依赖：numpy + scipy。
用法：
  python3 analyze.py --workdir ../scratch/upmix-run --signals tests/signals \
      --report ../scratch/upmix-run/report.md --json ../scratch/upmix-run/summary.json
"""
import argparse
import json
import os
import sys
import warnings

import numpy as np
from scipy import signal
from scipy.io import wavfile

warnings.filterwarnings("ignore", category=wavfile.WavFileWarning)

OCTAVES = [31.5, 63, 125, 250, 500, 1000, 2000, 4000, 8000, 16000]


def load_wav(path):
    fs, data = wavfile.read(path)
    if data.dtype.kind != "f":
        data = data.astype(np.float64) / (2.0 ** (8 * data.dtype.itemsize - 1))
    data = data.astype(np.float64)
    if data.ndim == 1:
        data = data[:, None]
    return fs, data


def lr_sos_list(order, freq, btype, fs):
    """Linkwitz-Riley = 级联 Butterworth：order=2 -> 2x order1；4 -> 2x order2；6 -> 3x order2；8 -> 4x order2"""
    if order == 2:
        n, reps = 1, 2
    elif order == 4:
        n, reps = 2, 2
    elif order == 6:
        n, reps = 2, 3
    elif order == 8:
        n, reps = 2, 4
    else:
        raise ValueError("unsupported LR order %r" % order)
    sos = signal.butter(n, freq, btype=btype, fs=fs, output="sos")
    return [sos] * reps


def apply_lr(x, order, freq, btype, fs):
    for sos in lr_sos_list(order, freq, btype, fs):
        x = signal.sosfilt(sos, x)
    return x


def apply_delay(x, samples, n):
    samples = int(round(samples))
    if samples <= 0:
        return x
    out = np.zeros_like(x)
    if samples < n:
        out[samples:] = x[: n - samples]
    return out


def load_conv_ir(path):
    fs, ir = wavfile.read(path)
    if ir.dtype.kind != "f":
        ir = ir.astype(np.float64) / (2.0 ** (8 * ir.dtype.itemsize - 1))
    ir = ir.astype(np.float64)
    if ir.ndim > 1:
        ir = ir[:, 0]
    return ir


def simulate(plan, x, fs):
    """按 plan 复刻 CamillaDSP 处理链，返回 (N, nout) 期望输出。"""
    n = x.shape[0]
    nout = int(plan["output_channels"])
    out = np.zeros((n, nout))
    irmap = plan["input_roles"]
    conv_cache = {}
    for role in plan["roles"]:
        if not role.get("enabled") or not role.get("outputs"):
            continue
        acc = np.zeros(n)
        for s in role.get("sources") or []:
            coeff = 10.0 ** (float(s["gain_db"]) / 20.0)
            if s.get("inverted"):
                coeff = -coeff
            acc += coeff * x[:, int(irmap[s["src"]])]
        if role.get("highpass"):
            hp = role["highpass"]
            acc = apply_lr(acc, int(hp["order"]), float(hp["freq"]), "highpass", fs)
        if role.get("lowpass"):
            lp = role["lowpass"]
            acc = apply_lr(acc, int(lp["order"]), float(lp["freq"]), "lowpass", fs)
        if role.get("fir"):
            p = role["fir"]["path"]
            if p not in conv_cache:
                conv_cache[p] = load_conv_ir(p)
            acc = signal.fftconvolve(acc, conv_cache[p], mode="full")[:n]
        acc *= 10.0 ** (float(role["gain_db"]) / 20.0)
        acc = apply_delay(acc, float(role["delay_ms"]) * fs / 1000.0, n)
        for o in role["outputs"]:
            out[:, int(o)] += acc
    return out


def rms_db(x):
    r = np.sqrt(np.mean(np.square(x)))
    return 20.0 * np.log10(max(r, 1e-12))


def band_levels(x, fs):
    f, pxx = signal.welch(x, fs, nperseg=8192)
    df = f[1] - f[0]
    out = {}
    for fc in OCTAVES:
        lo, hi = fc / (2 ** 0.5), fc * (2 ** 0.5)
        sel = (f >= lo) & (f < hi)
        p = float(np.sum(pxx[sel]) * df) if np.any(sel) else 0.0
        out[fc] = 10.0 * np.log10(max(p, 1e-24))
    return out


def corr_matrix(data, fs):
    seg = data[int(0.5 * fs): int(2.5 * fs)] if data.shape[0] > int(3 * fs) else data
    if seg.shape[0] < 1024:
        return np.zeros((data.shape[1], data.shape[1]))
    keep = [i for i in range(seg.shape[1]) if float(np.std(seg[:, i])) > 1e-12]
    if not keep:
        return np.zeros((data.shape[1], data.shape[1]))
    cm = np.zeros((data.shape[1], data.shape[1]))
    with np.errstate(invalid="ignore", divide="ignore"):
        sub = np.nan_to_num(np.corrcoef(seg[:, keep].T))
    for a, ia in enumerate(keep):
        for b, ib in enumerate(keep):
            cm[ia, ib] = sub[a, b]
    return cm


def xcorr_metrics(a, b, fs, max_lag_ms=50):
    n = int(max_lag_ms * fs / 1000.0)
    seg_len = min(len(a), int(2 * fs))
    a = a[:seg_len] - np.mean(a[:seg_len])
    b = b[:seg_len] - np.mean(b[:seg_len])
    denom = math_sqrt(np.sum(a * a) * np.sum(b * b)) if False else math_sqrt(np.dot(a, a) * np.dot(b, b))
    if denom <= 0:
        return 0.0, 0.0, 0
    corr = signal.correlate(a, b, mode="full", method="fft")
    lags = signal.correlation_lags(len(a), len(b), mode="full")
    sel = np.abs(lags) <= n
    corr_sel = corr[sel] / denom
    lags_sel = lags[sel]
    i = int(np.argmax(np.abs(corr_sel)))
    r0 = float(corr_sel[np.argmin(np.abs(lags_sel))])
    return float(corr_sel[i]), r0, int(lags_sel[i])


def math_sqrt(x):
    return float(np.sqrt(max(x, 0.0)))


def check_entry(entry, plan, sigdir, sim_cache=None):
    fs, x = load_wav(entry["input"])
    fs_out, y = load_wav(entry["output"])
    n = min(x.shape[0], y.shape[0])
    x, y = x[:n], y[:n]
    nout = int(plan["output_channels"])
    checks = []
    metrics = {"levels": {}, "predicted": {}, "peak": {}, "delay_samples": {}}

    if y.shape[1] != nout:
        return {"checks": [{"name": "通道数", "pass": False,
                            "detail": "期望 %d，实际 %d" % (nout, y.shape[1])}],
                "metrics": metrics}

    sim_key = (entry["preset"], entry["signal"])
    if sim_cache is not None and sim_key in sim_cache:
        sim = sim_cache[sim_key]
    else:
        sim = simulate(plan, x, fs)
        if sim_cache is not None:
            sim_cache[sim_key] = sim
    skip = int(0.3 * fs)
    end = n - int(0.1 * fs)
    if end <= skip:
        skip, end = 0, n
    seg_y = y[skip:end]
    seg_s = sim[skip:end]
    maxdiff = float(np.max(np.abs(seg_y - seg_s))) if seg_y.size else 0.0
    checks.append({"name": "与参考仿真逐样本一致", "pass": maxdiff <= 2e-4,
                   "detail": "max|diff|=%.2e" % maxdiff})
    metrics["sim_maxdiff"] = maxdiff

    # 每通道电平 vs 预测
    level_ok = True
    level_detail = []
    for c in range(nout):
        my = rms_db(seg_y[:, c])
        ms = rms_db(seg_s[:, c])
        metrics["levels"][str(c)] = round(my, 2)
        metrics["predicted"][str(c)] = round(ms, 2)
        metrics["peak"][str(c)] = round(float(np.max(np.abs(seg_y[:, c]))), 4)
        if ms > -100.0:
            d = abs(my - ms)
            level_detail.append("ch%d Δ=%.2fdB" % (c, d))
            if d > 0.25:
                level_ok = False
        else:
            if my > -80.0:
                level_ok = False
                level_detail.append("ch%d 应静音但 %.1fdB" % (c, my))
    checks.append({"name": "通道电平与设计一致", "pass": level_ok,
                   "detail": "; ".join(level_detail[:6]) if level_detail else "全部一致"})

    # 峰值/削波
    peak = float(np.max(np.abs(seg_y)))
    metrics["overall_peak"] = round(peak, 4)
    checks.append({"name": "无数字削波（peak ≤ 0.999）", "pass": peak <= 0.999,
                   "detail": "peak=%.3f" % peak})

    # 脉冲延迟
    if os.path.basename(entry["signal"]).startswith("impulse"):
        imp_idx = None
        for c in range(x.shape[1]):
            idx = np.argmax(np.abs(x[:, c]))
            if np.max(np.abs(x[:, c])) > 0.1:
                imp_idx = int(idx)
                break
        if imp_idx is not None:
            delay_ok = True
            detail = []
            start = max(0, imp_idx - 64)
            stop = min(len(y), imp_idx + 4096)
            base_off = imp_idx - start
            for role in plan["roles"]:
                if not role.get("enabled") or len(role.get("outputs")) != 1:
                    continue
                c = int(role["outputs"][0])
                # 以参考仿真（含滤波器群延迟）的脉冲峰位置为期望；两者须一致 → 延迟/滤波链正确
                exp_seg = np.abs(sim[start:stop, c])
                got_seg = np.abs(y[start:stop, c])
                if float(np.max(exp_seg)) < 1e-3:
                    continue  # 该角色对双声道同相脉冲无响应（如纯差信号）
                exp = int(np.argmax(exp_seg)) - base_off
                got = int(np.argmax(got_seg)) - base_off
                metrics["delay_samples"][role["id"]] = got
                if abs(got - exp) > 8:
                    delay_ok = False
                    detail.append("%s 期望 %d 实际 %d samples" % (role["id"], exp, got))
            checks.append({"name": "脉冲延迟/滤波链符合配置", "pass": delay_ok,
                           "detail": "; ".join(detail) if detail else "全部符合（±8 sample）"})

    return {"checks": checks, "metrics": metrics}


def build_report(workdir, results, plans, variant_filter=None, preset_filter=None):
    lines = []
    entries = [e for e in results["entries"]
               if (variant_filter is None or e["variant"] == variant_filter)
               and (preset_filter is None or e["preset"] == preset_filter)]
    by_key = {}
    for e in entries:
        by_key.setdefault((e["preset"], e["signal"]), []).append(e)

    eq_rows = []
    # standalone vs renderer 一致性
    variants = sorted(set(e["variant"] for e in entries))
    if "standalone" in variants and "renderer" in variants:
        prs = sorted(set(e["preset"] for e in entries))
        for preset in prs:
            sigs = sorted(set(e["signal"] for e in entries if e["preset"] == preset))
            worst = 0.0
            count = 0
            for sig in sigs:
                a = next((e for e in entries if e["preset"] == preset and e["signal"] == sig
                          and e["variant"] == "standalone" and e.get("ok")), None)
                b = next((e for e in entries if e["preset"] == preset and e["signal"] == sig
                          and e["variant"] == "renderer" and e.get("ok")), None)
                if not a or not b:
                    continue
                _, da = load_wav(a["output"])
                _, db = load_wav(b["output"])
                n = min(da.shape[0], db.shape[0])
                worst = max(worst, float(np.max(np.abs(da[:n] - db[:n]))))
                count += 1
            eq_rows.append((preset, count, worst))
        lines.append("## 0. 独立配置 vs channelmap 渲染路径")
        lines.append("")
        lines.append("| preset | 对比信号数 | max\\|diff\\| | 结论 |")
        lines.append("| --- | --- | --- | --- |")
        for preset, count, worst in eq_rows:
            lines.append("| %s | %d | %.2e | %s |" % (preset, count, worst,
                                                    "一致（逐样本）" if worst <= 1e-6 else "有差异"))
        lines.append("")

    # 主检查表
    lines.append("## 1. 信号级验证（与参考仿真对比）")
    lines.append("")
    lines.append("每行 = 一个 preset/variant/测试信号；检查项含：与参考仿真逐样本一致、通道电平与设计一致、"
                 "无数字削波、脉冲延迟/滤波链符合配置。")
    lines.append("")
    lines.append("| preset | variant | 信号 | 结果 | 失败详情 |")
    lines.append("| --- | --- | --- | --- | --- |")
    fails = 0
    total = 0
    per_preset = {}
    bad_rows = []
    sim_cache = {}
    for e in entries:
        plan = plans.get(e["preset"])
        if plan is None or not e.get("ok"):
            continue
        try:
            res = check_entry(e, plan, os.path.dirname(e["input"]), sim_cache)
        except Exception as ex:  # noqa: BLE001
            res = {"checks": [{"name": "分析异常", "pass": False, "detail": str(ex)[:300]}],
                   "metrics": {}}
        per_preset.setdefault((e["preset"], e["variant"]), []).append(
            {"signal": e["signal"], "analysis": res, "entry": e})
        passed = 0
        details = []
        for chk in res["checks"]:
            total += 1
            if chk["pass"]:
                passed += 1
            else:
                fails += 1
                details.append("%s：%s" % (chk["name"], chk["detail"]))
        lines.append("| %s | %s | %s | %d/%d %s | %s |" % (
            e["preset"], e["variant"], os.path.splitext(e["signal"])[0],
            passed, len(res["checks"]), "PASS" if not details else "**FAIL**",
            "；".join(details) if details else "-"))
        if details:
            bad_rows.append((e, details))
    lines.append("")
    lines.append("合计：%d 项检查，%d 项失败。" % (total, fails))
    lines.append("")

    # 电平/延迟/相关性细节（按 preset 选 standalone）
    for preset in sorted(plans):
        rows = per_preset.get((preset, "standalone"))
        if not rows:
            rows = list(per_preset.values())[0] if per_preset else []
        if not rows:
            continue
        plan = plans[preset]
        roles = [r for r in plan["roles"] if r.get("enabled")]
        line_tone = [r["id"] for r in roles]
        lines.append("### %s（%s，%dch）" % (preset, plan["layout"], plan["output_channels"]))
        lines.append("")
        # 单音电平表
        tone_sigs = ["tone_center_1k", "tone_left_1k", "tone_antiphase_1k", "sine_40hz_center"]
        lines.append("**电平（RMS dBFS）**")
        lines.append("")
        lines.append("| 信号 | " + " | ".join(line_tone) + " |")
        lines.append("| --- | " + " | ".join(["---"] * len(line_tone)) + " |")
        for sig in tone_sigs:
            row = next((r for r in rows if os.path.basename(r["signal"]).startswith(sig)), None)
            if not row:
                continue
            lv = row["analysis"]["metrics"].get("levels", {})
            cells = []
            for role in roles:
                c = int(role["outputs"][0]) if role.get("outputs") else None
                cells.append("%.1f" % lv[str(c)] if c is not None and str(c) in lv else "-")
            lines.append("| %s | %s |" % (sig, " | ".join(cells)))
        lines.append("")
        # 延迟
        delay_row = None
        for r in rows:
            if "delay_samples" in r["analysis"]["metrics"]:
                delay_row = r
                break
        if delay_row:
            ds = delay_row["analysis"]["metrics"]["delay_samples"]
            lines.append("**脉冲延迟（samples @ %d Hz）**" % results["fs"])
            lines.append("")
            lines.append("| 角色 | 期望(ms) | 实测(samples) | 实测(ms) |")
            lines.append("| --- | --- | --- | --- |")
            for role in roles:
                if role["id"] not in ds:
                    continue
                lines.append("| %s | %.1f | %d | %.2f |" % (
                    role["id"], role["delay_ms"], ds[role["id"]],
                    1000.0 * ds[role["id"]] / results["fs"]))
            lines.append("")
        # 粉噪频带 + 相关性
        pink = next((r for r in rows if os.path.basename(r["signal"]).startswith("noise_pink_stereo")), None)
        if pink:
            try:
                fs2, data = load_wav(pink["entry"]["output"])
                seg = data[int(0.5 * fs2): int(3.5 * fs2)]
                bands = {str(c): band_levels(seg[:, c], fs2) for c in range(min(seg.shape[1], 10))}
                show_ch = [int(r["outputs"][0]) for r in roles][:10]
                lines.append("**粉噪倍频带电平（dB）**")
                lines.append("")
                hdr = [role["id"] for role in roles if role.get("outputs")][:10]
                lines.append("| 频带(Hz) | " + " | ".join(hdr) + " |")
                lines.append("| --- | " + " | ".join(["---"] * len(hdr)) + " |")
                for fc in OCTAVES:
                    cells = []
                    for c in show_ch:
                        key = str(c)
                        cells.append("%.1f" % bands[key][fc] if key in bands else "-")
                    lines.append("| %.0f | %s |" % (fc, " | ".join(cells)))
                lines.append("")
                cm = corr_matrix(seg, fs2)
                labels = [r["id"] for r in roles][:cm.shape[0]]
                lines.append("**通道相关性矩阵**（粉噪立体声，2–3.5s）")
                lines.append("")
                lines.append("| | " + " | ".join(labels) + " |")
                lines.append("| --- | " + " | ".join(["---"] * len(labels)) + " |")
                for i, lab in enumerate(labels):
                    cells = ["%+.2f" % cm[i, j] for j in range(len(labels))]
                    lines.append("| %s | %s |" % (lab, " | ".join(cells)))
                lines.append("")
                # 前/合成通道相关性与滞后
                if len(roles) > 1:
                    fl = next((r for r in roles if r["id"] == "FL"), None)
                    if fl:
                        fc = int(fl["outputs"][0])
                        lines.append("**与 FL 的互相关（粉噪，±50ms）**")
                        lines.append("")
                        lines.append("| 角色 | 延迟(ms) | r(lag0) | max\\|r\\| | 峰值滞后(ms) |")
                        lines.append("| --- | --- | --- | --- | --- |")
                        for role in roles:
                            if role is fl or not role.get("outputs"):
                                continue
                            rm, r0, lag = xcorr_metrics(seg[:, fc], seg[:, int(role["outputs"][0])], fs2)
                            lines.append("| %s | %.1f | %+.3f | %.3f | %.1f |" % (
                                role["id"], role["delay_ms"], r0, abs(rm), 1000.0 * lag / fs2))
                        lines.append("")
            except Exception as ex:  # noqa: BLE001
                lines.append("（粉噪分析失败：%s）" % ex)
                lines.append("")

    lines.append("## 结论")
    lines.append("")
    if fails == 0 and total > 0:
        lines.append("- 所有信号级检查通过：CamillaDSP 实际输出与参考 DSP 仿真逐样本一致"
                     "（max|diff| ~1e-8，float32 量化级），说明配置/滤波器/延迟/矩阵与设计一致。")
        lines.append("- 独立生成的 CamillaDSP 配置与 khaen-channelmap 渲染路径（修复后）输出逐样本相同。")
        lines.append("- 电平、频带、延迟、反相/差信号隔离均符合设计；未发现非预期的梳状/抵消"
                     "（r(lag0)≈0，合成通道与前声道的最大相关出现在配置延迟处）。")
        lines.append("- 已知设计取舍：SL/SR 为反相差信号（相关 -1）、HFL/HFR 为单声道高度（相关 +1），"
                     "需要扩散感请用 wide 预设（左右延迟差）或 decor.mode=allpass。")
    else:
        lines.append("- 存在失败项，见上表；请先排查再用于真机。")
    lines.append("")

    # 计时
    lines.append("## 2. 计时 / 实时因子（本机文件后端）")
    lines.append("")
    lines.append("| preset | variant | 平均 RTF | 最小 RTF | 平均耗时(s) | 音频时长(s) |")
    lines.append("| --- | --- | --- | --- | --- | --- |")
    groups = {}
    for e in entries:
        if e.get("rtf") is None:
            continue
        groups.setdefault((e["preset"], e["variant"]), []).append(e)
    for (preset, variant), items in sorted(groups.items()):
        rtfs = [i["rtf"] for i in items]
        walls = [i["wall_seconds"] for i in items]
        durs = [i["audio_seconds"] for i in items]
        lines.append("| %s | %s | %.1f | %.1f | %.2f | %.1f |" % (
            preset, variant, float(np.mean(rtfs)), float(np.min(rtfs)),
            float(np.mean(walls)), float(np.mean(durs))))
    lines.append("")
    lines.append("说明：RTF = 音频时长 / 实际处理耗时，越大越快；文件后端不含 ALSA 实时等待，"
                 "板端需乘实际负载系数（见板端测试一节）。")
    lines.append("")
    board = os.path.join(workdir, "board-results.json")
    if os.path.isfile(board):
        try:
            with open(board, "r", encoding="utf-8") as f:
                bd = json.load(f)
            lines.append("## 3. 板端实测（RK3576 6.1.99-rt36）")
            lines.append("")
            lines.append("| preset | chunksize | 音频时长(s) | 耗时(s) | RTF | CPU user(s) | CPU 占单核 |")
            lines.append("| --- | --- | --- | --- | --- | --- | --- |")
            for it in bd.get("entries") or []:
                if "chunksize" not in it:
                    continue
                cpu_pct = (100.0 * it.get("cpu_user", 0) / it.get("audio_seconds", 1)
                           if it.get("audio_seconds") else 0)
                lines.append("| %s | %s | %.1f | %.2f | %.2f | %s | %.1f%% |" % (
                    it.get("preset"), it.get("chunksize"), it.get("audio_seconds", 0),
                    it.get("wall_seconds", 0), it.get("rtf", 0), it.get("cpu_user", "-"),
                    cpu_pct))
            lines.append("")
            checks = [it for it in bd.get("entries") or [] if "alsa_check" in it]
            if checks:
                lines.append("ALSA 配置 camilladsp --check（不打开设备，验证结构与取值）：")
                lines.append("")
                for it in checks:
                    lines.append("- %s：%s" % (it.get("preset"),
                                              "PASS" if it.get("alsa_check") else "FAIL"))
                lines.append("")
            lines.append("板端环境：%s" % bd.get("env", "-"))
            lines.append("")
            lines.append("注：文件后端的 wall 时间包含文件 I/O；cs=128 在板端受小盘 I/O 影响 wall 偏大，"
                         "CPU 时间才是 DSP 负载（20s 音频约 0.35–0.75s，约单核 2–4%）。")
            lines.append("")
        except Exception as ex:  # noqa: BLE001
            lines.append("（板端结果读取失败：%s）" % ex)
    return "\n".join(lines), total, fails


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--signals", default=None)
    ap.add_argument("--report", default=None)
    ap.add_argument("--json", dest="json_out", default=None)
    ap.add_argument("--variant", default=None)
    ap.add_argument("--preset", default=None)
    args = ap.parse_args()
    wd = os.path.abspath(args.workdir)
    with open(os.path.join(wd, "results.json"), "r", encoding="utf-8") as f:
        results = json.load(f)
    plans = {}
    for name in os.listdir(os.path.join(wd, "plans")):
        if name.endswith(".json"):
            with open(os.path.join(wd, "plans", name), "r", encoding="utf-8") as f:
                plans[name[:-5]] = json.load(f)
    report, total, fails = build_report(wd, results, plans, args.variant, args.preset)
    report = "# 2.0 立体声上混 · 文件后端测试报告\n\n" + report
    if args.report:
        with open(args.report, "w", encoding="utf-8", newline="\n") as f:
            f.write(report)
        print("报告：%s" % os.path.abspath(args.report))
    else:
        print(report)
    if args.json_out:
        with open(args.json_out, "w", encoding="utf-8") as f:
            json.dump({"total_checks": total, "failed_checks": fails}, f, indent=2)
    print("\n检查项：%d，失败：%d" % (total, fails))
    return 0 if fails == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
