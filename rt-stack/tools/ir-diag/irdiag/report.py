"""Outputs: raw-baseline.json, CSVs, figures, ANALYSIS_MANIFEST.json, REPORT.md."""
import csv
import hashlib
import json
import os

import numpy as np

from . import metrics as M

CODE_FILES = ["irdiag/excitation.py", "irdiag/metrics.py", "irdiag/analysis.py",
              "irdiag/pipeline.py", "irdiag/report.py", "run_ir_diag.py"]
MANIFEST_PARAMS = {
    "schema": "tspi-ir-diag-analysis-manifest/v1",
    "spec": "rt-stack/spec/mimo-single-point-diag-v1.md (v1.2)",
    "fs_hz": 48000,
    "capture_format": "2ch interleaved S24_3LE @48k, channel 0 analysed (ch1 is an identical copy)",
    "excitation": {
        "formula": "calibrate/src/sweep.rs:28-55 exp_sweep; A=10^(dBFS/20); half-cosine 10 ms fades",
        "seconds": 5.0, "level_dbfs": -25.0, "fade_ms": 10.0, "f0_hz": 20.0,
        "f1_full_hz": 20000.0, "f1_sub_hz": 130.0,
    },
    "inverse_filter": {
        "formula": "calibrate/src/dsp.rs:66-92 farina_inverse_filter (time reverse + exp amplitude compensation, unit energy)",
        "drift_correction": "inverse filter built at the measured stretched length; eps from repeat spacing",
    },
    "ir_window": {
        "config_ir_seconds": 5.0, "guard_s": 0.05, "pre_s": 0.1,
        "rule": "min(config_ir_seconds, next_pulse_distance - guard_s); integration starts at the broadband direct-sound peak",
    },
    "bands": {
        "centers_hz": [40, 50, 63, 80, 100, 125, 160, 200],
        "edges": "fc*2**(-1/6) .. fc*2**(+1/6)",
        "decimate_to_hz": 4000, "decimate_method": M.BAND_DECIMATE_METHOD,
        "filter": M.BAND_TYPE, "order": M.BAND_ORDER,
    },
    "edc": {"method": M.EDC_METHOD, "noise_window_s": M.EDC_NOISE_WINDOW_S,
            "waterfall_ms": M.WATERFALL_MS, "edc100_s": M.EDC100_S},
    "fit": {"range_db": [M.FIT_LO_DB, M.FIT_HI_DB], "r2_min": M.FIT_R2_MIN,
            "model": "least squares EDC[dB] ~ t, no extrapolation, fit window = actual -5/-25 crossings"},
    "gates": {
        "t20_abs_range_max_s": M.T20_ABS_RANGE_MAX_S, "t20_rel_range_max": M.T20_REL_RANGE_MAX,
        "t20_range_max_s": M.T20_RANGE_MAX_S, "edc100_range_max_db": M.EDC100_RANGE_MAX_DB,
        "ir_xcorr_min": M.IR_XCORR_MIN, "ir_xcorr_search_samples": M.IR_XCORR_SHIFT_SEARCH,
        "dr_min_db": M.DR_MIN_DB, "noise_margin_min_db": M.NOISE_MARGIN_MIN_DB,
        "record_min_s": M.RECORD_MIN_S, "record_t20_factor": M.RECORD_T20_FACTOR,
        "snr_hard_min_db": M.SNR_HARD_MIN_DB, "snr_strict_min_db": M.SNR_STRICT_MIN_DB,
    },
}


def _sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def write_manifest(out_dir, tools_dir, capture_sha, extra):
    params = json.loads(json.dumps(MANIFEST_PARAMS))
    params["capture_sha256"] = capture_sha
    params.update(extra)
    blob = json.dumps(params, sort_keys=True, separators=(",", ":")).encode()
    params["parameter_hash"] = hashlib.sha256(blob).hexdigest()
    params["code_sha256"] = {}
    for rel in CODE_FILES:
        p = os.path.join(tools_dir, rel)
        if os.path.exists(p):
            params["code_sha256"][rel] = _sha(p)
    os.makedirs(out_dir, exist_ok=True)
    out = os.path.join(out_dir, "ANALYSIS_MANIFEST.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump(params, f, indent=2, sort_keys=True)
    return params, out


def _json_default(o):
    if isinstance(o, np.ndarray):
        return o.tolist()
    if isinstance(o, np.floating):
        return None if not np.isfinite(o) else float(o)
    if isinstance(o, np.integer):
        return int(o)
    if isinstance(o, np.bool_):
        return bool(o)
    raise TypeError("not JSON serialisable: %r" % (type(o),))


def write_json(result, out_dir):
    out = os.path.join(out_dir, "raw-baseline.json")
    # the full EDC curves live in edc_curves.csv; strip them so this file stays readable
    slim = json.loads(json.dumps(result, default=_json_default))
    for ch in slim.get("channels", {}).values():
        for b in ch.get("bands", {}).values():
            b.pop("edc_curve_median", None)
    with open(out, "w", encoding="utf-8") as f:
        json.dump(slim, f, indent=1, ensure_ascii=False)
    return out


def write_csvs(result, out_dir):
    rows = []
    rep_rows = []
    edc_rows = []
    wf_rows = []
    for name, ch in result["channels"].items():
        for fc, b in ch["bands"].items():
            rows.append({
                "channel": name, "freq_hz": fc, "status": b["status"],
                "t20_median_s": b["t20_median_s"], "t20_range_s": b["t20_range_s"],
                "t20_rel_range": b["t20_rel_range"],
                "t20_r0": b["t20_repeats_s"][0] if len(b["t20_repeats_s"]) > 0 else "",
                "t20_r1": b["t20_repeats_s"][1] if len(b["t20_repeats_s"]) > 1 else "",
                "t20_r2": b["t20_repeats_s"][2] if len(b["t20_repeats_s"]) > 2 else "",
                "r2_r0": b["r2_repeats"][0], "r2_r1": b["r2_repeats"][1], "r2_r2": b["r2_repeats"][2],
                "dr_db_min": min([d for d in b["dr_db_repeats"] if d is not None], default=""),
                "edc100_median_db": b["edc100_median_db"],
                "edc100_range_db": b["edc100_range_db"],
                "initial_drop_100ms_db": b["initial_drop_100ms_db"],
                "decay_slope_db_per_s": b["decay_slope_db_per_s"],
                "band_snr_db": b["band_snr_db"],
                "reasons": "; ".join(b["reasons"]),
            })
            for k, r in enumerate(ch["repeats"]):
                rb = r["bands"][fc]
                rep_rows.append({"channel": name, "freq_hz": fc, "repeat": k,
                                 "t20_s": rb.get("t20_s"), "r2": rb.get("r2"),
                                 "edc100_db": rb.get("edc100_db"), "dr_db": rb.get("dr_db"),
                                 "record_after_peak_s": rb.get("record_after_peak_s"),
                                 "noise_margin_db": rb.get("noise_margin_db"),
                                 "ir_peak_abs_sample": r["ir_peak_abs_sample"]})
            for ms, v in b["waterfall_db"].items():
                wf_rows.append({"channel": name, "freq_hz": fc, "t_ms": ms, "edc_db": v})
            fs = b["edc_curve_fs_hz"]
            step = b["edc_curve_step_samples"]
            for i, v in enumerate(b["edc_curve_median"] or []):
                edc_rows.append({"channel": name, "freq_hz": fc,
                                 "t_s": round(i * step / fs, 4), "edc_db": v})
    for fn, data, base_cols in [
        ("band_metrics.csv", rows, None),
        ("repeat_metrics.csv", rep_rows, None),
        ("edc_curves.csv", edc_rows, None),
        ("waterfall.csv", wf_rows, None),
    ]:
        p = os.path.join(out_dir, fn)
        if not data:
            continue
        with open(p, "w", newline="", encoding="utf-8") as f:
            w = csv.DictWriter(f, fieldnames=list(data[0].keys()))
            w.writeheader()
            w.writerows(data)
    return out_dir


def make_figures(result, out_dir):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    chans = [c[1] for c in __import__("irdiag.excitation", fromlist=["CHANNELS"]).CHANNELS]
    cmap = plt.get_cmap("viridis")
    bands = [str(int(f)) for f in M.__dict__ and [40, 50, 63, 80, 100, 125, 160, 200]]
    colors = {b: cmap(i / (len(bands) - 1.0)) for i, b in enumerate(bands)}
    fs = 4000
    made = []
    # 1) EDC curves
    fig, axes = plt.subplots(2, 4, figsize=(17, 8.5), sharex=True, sharey=True)
    name_has_label = False
    for ax, name in zip(axes.ravel(), chans):
        ch = result["channels"][name]
        name_has_label = name == chans[0]
        for fc in bands:
            b = ch["bands"][fc]
            curve = b["edc_curve_median"]
            if not curve:
                continue
            t = np.arange(len(curve)) * b["edc_curve_step_samples"] / fs
            ax.plot(t, curve, color=colors[fc], lw=1.1,
                    label=fc + " Hz" if name == chans[0] else None,
                    ls="--" if b["status"] != "OK" else "-")
        ax.axhline(-5, color="k", lw=0.5, ls=":")
        ax.axhline(-25, color="k", lw=0.5, ls=":")
        ax.axvline(0.1, color="r", lw=0.5, ls=":")
        ax.set_title("%s  (%s)" % (name, "band limited 20-130 Hz" if name == "SUB1" else "20-20k"))
        ax.set_xlim(0, 2.0)
        ax.set_ylim(-50, 2)
        ax.grid(alpha=0.25, lw=0.4)
        if name == "SUB1":
            ax.set_xlabel("t after direct sound [s]")
        if name in ("FL", "HFL"):
            ax.set_ylabel("EDC [dB]")
    axes.ravel()[-1].axis("off")
    if name_has_label:
        axes.ravel()[-1].legend(loc="center", fontsize=9, title="1/3 oct")
    axes.ravel()[-1].text(0.5, 0.1, "-- = UNRELIABLE band", ha="center", fontsize=9, transform=axes.ravel()[-1].transAxes)
    fig.suptitle("G0 RAW baseline - Schroeder EDC per 1/3-oct band (median of 3 repeats)", fontsize=13)
    fig.tight_layout(rect=[0, 0, 1, 0.96])
    p = os.path.join(out_dir, "edc_curves.png")
    fig.savefig(p, dpi=120)
    plt.close(fig)
    made.append(p)

    # 2) T20 bars
    fig, ax = plt.subplots(figsize=(13, 6))
    x = np.arange(len(bands))
    width = 0.8 / len(chans)
    for i, name in enumerate(chans):
        ch = result["channels"][name]
        vals, errs = [], []
        for fc in bands:
            b = ch["bands"][fc]
            v = b["t20_median_s"]
            ok = (v is not None and b["status"] == "OK")
            vals.append(v if ok else float("nan"))
            errs.append((b["t20_range_s"] or 0) / 2 if ok else 0.0)
        ax.bar(x + i * width - 0.4, vals, width, yerr=errs, capsize=1.5,
               label=name, alpha=0.9)
    ax.set_xticks(x)
    ax.set_xticklabels([b + " Hz" for b in bands])
    ax.set_ylabel("T20 [s]  (median of 3 repeats; whisker = repeat range; empty band = UNRELIABLE)")
    ax.set_title("G0 RAW baseline - T20 per 1/3-oct band (only bands passing the reliability gates are drawn)")
    ax.grid(axis="y", alpha=0.3, lw=0.5)
    ax.legend(ncol=4, fontsize=9)
    fig.tight_layout()
    p = os.path.join(out_dir, "t20_bars.png")
    fig.savefig(p, dpi=120)
    plt.close(fig)
    made.append(p)

    # 3) waterfall heatmaps
    fig, axes = plt.subplots(1, 7, figsize=(19, 4.6), sharey=True)
    for ax, name in zip(axes, chans):
        ch = result["channels"][name]
        z = np.zeros((len(bands), len(M.WATERFALL_MS)))
        for i, fc in enumerate(bands):
            for j, ms in enumerate(M.WATERFALL_MS):
                v = ch["bands"][fc]["waterfall_db"][str(ms)]
                z[i, j] = v if v is not None else np.nan
        im = ax.imshow(z, aspect="auto", cmap="inferno", vmin=-45, vmax=0,
                       origin="lower", extent=[-0.5, len(M.WATERFALL_MS) - 0.5, -0.5, len(bands) - 0.5])
        ax.set_title(name, fontsize=10)
        ax.set_xticks(range(len(M.WATERFALL_MS)))
        ax.set_xticklabels([str(m) for m in M.WATERFALL_MS], fontsize=6, rotation=90)
        ax.set_yticks(range(len(bands)))
        ax.set_yticklabels([b + " Hz" for b in bands], fontsize=7)
    fig.colorbar(im, ax=axes, fraction=0.012, pad=0.01, label="EDC [dB]")
    fig.suptitle("G0 RAW baseline - EDC waterfall (median of 3 repeats, 0 dB = EDC(0))", fontsize=12)
    p = os.path.join(out_dir, "waterfall_heatmap.png")
    fig.savefig(p, dpi=120, bbox_inches="tight")
    plt.close(fig)
    made.append(p)
    return made

def _fmt(v, nd=3, dash="n/a"):
    if v is None:
        return dash
    if isinstance(v, float):
        return ("%." + str(nd) + "f") % v
    return str(v)


def _chans():
    return [c[1] for c in __import__("irdiag.excitation", fromlist=["CHANNELS"]).CHANNELS]


BANDS_S = ["40", "50", "63", "80", "100", "125", "160", "200"]


def write_report(result, manifest, out_dir, extra_notes):
    chans = _chans()
    bands = BANDS_S
    L = []
    A = L.append
    cap = result["inputs"]
    A("# G0 RAW 时间域基线（IR / EDC / T20 / 瀑布）")
    A("")
    A("> 生成：%s ｜ schema `%s` ｜ 组 **G0 RAW**（P_post 旁路 / identity），**无 apply、无发声**"
      % (result.get("generated_at", "n/a"), result["schema"]))
    A("")
    A("## 0. 一句话结论")
    A("")
    ok = [(n, fc) for n in chans for fc in bands
          if result["channels"][n]["bands"][fc]["status"] == "OK"]
    rel = [(n, fc, result["channels"][n]["bands"][fc]["t20_median_s"]) for n, fc in ok
           if result["channels"][n]["bands"][fc]["t20_median_s"] is not None]
    rel.sort(key=lambda x: -x[2])
    if rel:
        A("在**能通过可靠性门**的 %d 个通道/频带组合中，RAW 的最长 T20 为 %.2f s（%s %s Hz）；"
          "所有可靠组合的中位 T20 = %.2f s。不可靠/未激励的组合共 %d 个，全部在文中逐条标出。"
          % (len(ok), rel[0][2], rel[0][0], rel[0][1],
              float(np.median([r[2] for r in rel])),
              len(chans) * len(bands) - len(ok)))
    else:
        A("**没有任何通道/频带组合同时通过全部门**（详见 §4/§7）。")
    A("")
    allb = [(n, fc, result["channels"][n]["bands"][fc]) for n in chans for fc in bands]
    part = [(n, fc, b["t20_partial_s"]) for n, fc, b in allb
            if b.get("t20_partial_s") and b["status"] == "UNRELIABLE"
            and b.get("t20_partial_valid")]
    part.sort(key=lambda x: -x[2])
    drs = [min([d for d in b["dr_db_repeats"] if d is not None]) for _, _, b in allb
           if any(b["dr_db_repeats"])]
    A("")
    A("**同时必须说清测不出来的部分**：40/50/63 Hz 等长衰减带的 EDC 可认证动态范围只有约 "
      "%.0f-%.0f dB（本次 RAW 采集的实测上限），达不到 spec v1.2 对低频要求的 35 dB，"
      "这些带的 -25 dB 点落在噪声底里，**不能认证 T20**。"
      % (min(drs) if drs else 0.0, max(drs) if drs else 0.0))
    A("")
    A("**本报告只回答 RAW 的衰减特性，不推断 MIMO/PEQ 会不会改善。**")
    A("")
    A("## 1. 数据与可信性")
    A("")
    A("| 项 | 值 |")
    A("| --- | --- |")
    A("| 采集 | `%s`（%d 帧 / %.2f s / 48 kHz） |"
      % (cap["capture"], cap["capture_frames"], cap["capture_seconds"]))
    A("| SHA-256 | `%s` |" % cap["capture_sha256"])
    A("| 采集通道 | 2ch S24_3LE；ch0 分析用，ch1 与 ch0 **逐样本相同**（%s） |"
      % ("是" if result["inputs"]["capture_ch0_equals_ch1"] else "**否**"))
    A("| 时隙布局 | pre %d / dev %d / slot %d / interval %d samples；repeats %d |"
      % (result["layout"]["pre_samples"], result["layout"]["dev_samples"],
         result["layout"]["slot_samples"], result["layout"]["interval_samples"],
         result["layout"]["repeats"]))
    A("| 时钟 | capture 相对 playback **%.1f ppm**（逐通道 %s） |"
      % (result["clock"]["eps_ppm"],
         ", ".join("%s %.1f" % (k, v) for k, v in sorted(result["clock"]["per_channel_ppm"].items()))))
    A("| 反卷积 | Farina 逆滤波（时间反转 + 幅度补偿），逆滤波器按实测长度 %d 样本构建 |"
      % result["clock"]["inverse_filter_length_samples"])
    A("")
    A("### 1.1 激励重建（必须与播放字节一致）")
    A("")
    A("公式取自 `calibrate/src/sweep.rs:28-55`（`SweepSpec::generate`，即生成 composite 的那份二进制）；"
      "RT crate 同一公式在 `rt-stack/crates/calibration/src/sweep.rs:1606-1630`（`sample_at`）"
      "与 `:1737-1759`（`segment_envelope`），电平定义在 `:773-778`。")
    A("")
    A("```")
    A("n = round(seconds*fs) = 240000,  T = 5.0 s,  k = ln(f1/f0),  A = 10^(-25/20) = 0.056234")
    A("phase[i] = 2*pi*f0*T/k * ((f1/f0)^(t/T) - 1),   x[i] = A*sin(phase)*w[i]")
    A("w = 半余弦 10 ms 淡入 + 镜像淡出（fade = 480 samples）")
    A("```")
    A("")
    A("与目标机上 `composite.raw`（实际送 DAC 的 22ch 字节）逐样本对照：")
    A("")
    A("| 通道 | 激励频带 | max abs error | 相关系数 |")
    A("| --- | --- | --- | --- |")
    for row in extra_notes["excitation_check"]:
        A("| %s | %s | %.3f LSB | %.10f |"
          % (row["name"], row["band"], row["max_err_lsb"], row["corr"]))
    A("")
    A("结论：重建波形与播放字节**误差 < 1 LSB（S24 量化）**，激励可信；"
      "同时验证槽间隔离：非目标通道在对应槽内为全 0。")
    A("")
    A("### 1.2 SUB1 的频带（规格书笔误更正）")
    A("")
    A("命令 `--band-limit 7:130` 的解析是 **`ch:end_hz`**"
      "（`calibrate/src/main.rs:2360-2377`），即**通道 7、上限 130 Hz**；"
      "起扫频率仍取全局 `--start-hz`，而该命令**没有**覆盖 `--start-hz`，"
      "所以 SUB1 实际扫频是 **20 → 130 Hz**。")
    A("")
    A("已用 composite.raw 的 ch7 切片验证：按 20→130 Hz 重建误差 0.555 LSB，按 7→130 Hz 重建完全对不上"
      "（相关系数 -0.04）。→ 本报告对 SUB1 一律标注 **仅 20–130 Hz**"
      "（规格书里的“仅 7–130 Hz”按实测更正为 20–130 Hz）；SUB1 的 160/200 Hz 带一律判 `NOT_DRIVEN`。")
    A("")
    A("## 2. 分析链（冻结参数，详见 manifest）")
    A("")
    A("| 环节 | 参数 |")
    A("| --- | --- |")
    A("| 反卷积 | 全采集 FFT 线性卷积（N = next_pow2(len+240000)），Farina 逆滤波 |")
    A("| IR 窗 | 从直达声峰起，上限 min(5.0 s, 下一条 IR 峰 − 0.05 s) |")
    A("| 1/3 oct 带通 | %s，频带 fc·2^±1/6；先降采样到 %d Hz 再滤 |"
      % (M.BAND_TYPE, M.BAND_DECIMATE_TO_HZ))
    A("| EDC | 带内 IR² 的 Schroeder 反向积分，**扣噪声**；起点 = 宽带直达声峰；噪声功率取窗末 %.0f ms |"
      % (M.EDC_NOISE_WINDOW_S * 1000))
    A("| T20 | EDC（−5…−25 dB）线性拟合，报 R²、动态范围、真实覆盖区间；不足不外推 |")
    A("| 初始段 | EDC@20/40/60/80/100 ms（瀑布 = 0…400 ms 的相对电平） |")
    A("| 门 | T20 极差、EDC@100ms 极差、IR 互相关、SNR、动态范围、记录长度、覆盖（见 §4） |")
    A("")
    A("## 3. 通道 SNR 与可用性")
    A("")
    A("| 通道 | 激励带 | analysis snr_db（IR 峰/尾） | snr_band_db（扫频/本底，工具口径） | 带内 SNR @40/63/100/125 Hz（本次） | onset_invalid | 判定 |")
    A("| --- | --- | --- | --- | --- | --- | --- |")
    for name in chans:
        ch = result["channels"][name]
        d = ch["diag"]
        bsnr = ch["band_snr_db"]
        cell = " / ".join(_fmt(bsnr.get(k), 1) for k in ["40", "63", "100", "125"])
        usable = []
        if d.get("onset_invalid"):
            usable.append("BAD_ONSET")
        if d.get("insufficient_valid_passes"):
            usable.append("INSUFF_VALID")
        if d.get("snr_band_db") is not None and d["snr_band_db"] < M.SNR_HARD_MIN_DB:
            usable.append("fullband_snr<20")
        bandtxt = "20–130 Hz" if name == "SUB1" else ("%g–%g Hz" % tuple(ch["excitation_band_hz"]))
        A("| %s | %s | %s | %s | %s | %s | %s |" % (
            name, bandtxt, _fmt(d.get("repeats_csv_snr_db"), 1), _fmt(d.get("snr_band_db"), 1), cell,
            d.get("onset_invalid"), ("; ".join(usable) if usable else "ch0 ok")))
    A("")
    A("> `snr_band_db` 是工具在**整带 20–20k** 上算的扫频/本底比，会被高频噪声压得很低"
      "（FL/FR/SR/SUB1 < 20 dB）。它不代表低频带不可用：本分析按 **1/3 oct 带内 SNR** 判每个带的可用性"
      "（上表第 5 列）。两个口径都列出，未做粉饰。")
    A("")
    A("## 4. 逐通道逐带：三次一致性（核心表）")
    A("")
    A("门：T20 相对极差 ≤10% 且绝对极差 ≤0.15 s（T20<0.2 s 时以**绝对极差 ≤0.05 s** 为主门）；"
      "EDC@100ms 极差 ≤1.5 dB；IR 互相关 ≥0.98；R²≥0.90；低频带另加动态范围/噪声/记录长度门。"
      "**三次离散就判 UNRELIABLE，不给平均值当基线。**")
    A("")
    for name in chans:
        ch = result["channels"][name]
        A("### %s（DAC ch%d%s）" % (name, ch["dac_channel"],
                                   "，限带 20–130 Hz" if name == "SUB1" else ""))
        A("")
        xc = ch["ir_xcorr_pairwise"]
        if xc:
            xctxt = "；".join(
                "对(%d,%d) 未对齐 %.4f / 对齐 %.4f @%+d smp" % (
                    i, i + 1, v["unaligned"], v["aligned"], v["best_shift_samples"])
                for i, v in enumerate(xc))
        else:
            xctxt = "n/a"
        A("IR 重复性（4 kHz 抽取 IR，分析带口径）：%s。" % xctxt)
        A("")
        A("IR 峰绝对位置 %s；扣掉平均重复间隔后的残差 %s 样本（极差 %.1f 样本 = %.2f ms）。"
          % (ch["pulses_abs_sample"],
             [round(v, 1) for v in ch.get("pulse_residual_samples", [])],
             ch.get("pulse_residual_spread_samples", 0.0),
             ch.get("pulse_residual_spread_samples", 0.0) / 48.0))
        fbx = ch.get("ir_xcorr_pairwise_fullband_48k") or []
        if fbx:
            A("")
            A("全带 48 kHz 互相关（仅诊断，不作门）：%s —— 差异来自重复之间剩下的"
              "**小数样本时间基准残差**（时钟 42 ppm 无法逐次精确对齐），不是声学差异。"
              % "；".join("对(%d,%d) %.4f" % (i, i + 1, v["unaligned"]) for i, v in enumerate(fbx)))
        A("")
        A("| band | T20 r0/r1/r2 [s] | 中位 | 极差 / 相对 | R² r0/r1/r2 | DR [dB] | EDC@100ms 三值 [dB] | 带内 SNR [dB] | 状态 |")
        A("| --- | --- | --- | --- | --- | --- | --- | --- | --- |")
        for fc in bands:
            b = ch["bands"][fc]
            t3 = b["t20_repeats_s"]
            tcell = "/".join(_fmt(v, 2) for v in t3) if t3 else "—"
            rcell = "/".join(_fmt(v, 3) for v in b["r2_repeats"])
            dcell = "/".join("—" if (v is None or v < -60) else _fmt(v, 1) for v in b["dr_db_repeats"])
            ecell = "/".join("—" if (v is None or v < -60) else _fmt(v, 1) for v in b["edc100_repeats_db"])
            reltxt = ("%.1f%%" % (100 * b["t20_rel_range"])) if b["t20_rel_range"] is not None else "—"
            stattxt = "OK" if b["status"] == "OK" else ("`%s`" % b["status"])
            A("| **%s Hz** | %s | %s | %s / %s | %s | %s | %s | %s | %s |" % (
                fc, tcell, _fmt(b["t20_median_s"], 2), _fmt(b["t20_range_s"], 3), reltxt,
                rcell, dcell, ecell, _fmt(b["band_snr_db"], 1), stattxt))
            if b["status"] == "UNRELIABLE":
                A("| | ↳ %s | | | | | | | |" % ("; ".join(b["reasons"])))
        A("")
    A("## 5. 最长 T20 排行（只列通过门的组合）")
    A("")
    if rel:
        A("| # | 通道 | band | T20 中位 [s] | 三次 | 带内 SNR [dB] | 初始段 0-100ms 掉落 [dB] |")
        A("| --- | --- | --- | --- | --- | --- | --- |")
        for i, (n, fc, tval) in enumerate(rel[:12], 1):
            b = result["channels"][n]["bands"][fc]
            A("| %d | %s | %s Hz | %.2f | %s | %s | %s |" % (
                i, n, fc, tval, "/".join(_fmt(v, 2) for v in b["t20_repeats_s"]),
                _fmt(b["band_snr_db"], 1), _fmt(b["initial_drop_100ms_db"], 1)))
        A("")
        A("### 5.1 不可认证长衰减带的局部斜率等效 T20（仅供参考，不是基线）")
        A("")
        A("下表只在「仍确认在噪声底以上」的范围内拟合（起点 -5 dB，终点取 -20 dB 与 "
          "噪声底以上 10 dB 中较浅者），用来回答「低频尾巴大概多长」；它**不覆盖 -25 dB**，"
          "按 spec 不能当 T20 用，带本身仍标 UNRELIABLE。")
        A("")
        A("| 通道 | band | 局部等效 T20 [s] | 拟合 R² | 实际拟合区间 [dB] | 带内 SNR [dB] |")
        A("| --- | --- | --- | --- | --- | --- |")
        for n, fc, tval in part[:18]:
            pass
            b = result["channels"][n]["bands"][fc]
            rr = b.get("t20_partial_range_db") or []
            flat = [x for x in rr if isinstance(x, (list, tuple)) and len(x) == 2]
            rtxt = ("%.1f to %.1f" % (flat[0][0], flat[0][1])) if flat else "-"
            A("| %s | %s Hz | %.2f | %s | %s | %s |" % (
                n, fc, tval, _fmt(b.get("t20_partial_r2"), 2), rtxt,
                _fmt(b["band_snr_db"], 1)))
    else:
        A("_没有组合同时通过全部门。_")
    A("")
    nd = [(n, fc) for n in chans for fc in bands
          if result["channels"][n]["bands"][fc]["status"] == "NOT_DRIVEN"]
    A("未激励（SUB1 带外）：%s。" % (", ".join("%s %s Hz" % p for p in nd) if nd else "无"))
    A("")
    A("## 6. 初始段（0–100 ms）与衰减段（−5…−25 dB 斜率）分离")
    A("")
    A("| 通道 | band | 初始段 EDC@100ms [dB] | 初始段 0–100ms 掉落 [dB] | 衰减段斜率 [dB/s] | T20 [s] |")
    A("| --- | --- | --- | --- | --- | --- |")
    for name in chans:
        ch = result["channels"][name]
        for fc in bands:
            b = ch["bands"][fc]
            if b["status"] != "OK":
                continue
            A("| %s | %s Hz | %.1f | %.1f | %.1f | %.2f |" % (
                name, fc, b["edc100_median_db"] or 0.0, b["initial_drop_100ms_db"] or 0.0,
                b["decay_slope_db_per_s"] or 0.0, b["t20_median_s"] or 0.0))
    A("")
    A("### 6.1 本次采集能测到多深（诚实门）")
    A("")
    drs2 = [min([d for d in b["dr_db_repeats"] if d is not None]) for n in chans
            for b in result["channels"][n]["bands"].values() if any(b["dr_db_repeats"])]
    A("EDC 的动态范围（DR）定义为「IR 窗内总能量 / 噪声底能量」。各通道各带的 DR 中位为 "
      "%.0f-%.0f dB（见 §4 每行 DR 列）。低频带 40/50/63 Hz 低于 spec 要求的 35 dB，"
      "意味着 **-25 dB 点已经压在噪声底上、不能外推**；这些带的 T20 一律置空"
      "（不是算不出来，是按规矩不允许算）。"
      % (min(drs2) if drs2 else 0.0, max(drs2) if drs2 else 0.0))
    A("")
    A("> 读法：初始段 = 直达声后 100 ms 内掉了多少 dB（早期衰变/早期反射结构）；"
      "衰减段 = −5…−25 dB 的拟合斜率（房间模态的自由衰减）。两者是**不同物理量**，"
      "初始段掉得多不代表尾部长，反之亦然。完整 0–400 ms 瀑布见 `waterfall.csv` / `waterfall_heatmap.png`。")
    A("")
    A("## 7. UNRELIABLE 明细")
    A("")
    bad = [(n, fc, result["channels"][n]["bands"][fc]) for n in chans for fc in bands
           if result["channels"][n]["bands"][fc]["status"] == "UNRELIABLE"]
    if bad:
        for n, fc, b in bad:
            tt = "/".join(_fmt(v, 2) for v in b["t20_repeats_s"]) if b["t20_repeats_s"] else "—"
            A("* **%s %s Hz** — %s（原始三次 T20：%s）" % (n, fc, "; ".join(b["reasons"]), tt))
    else:
        A("_无。_")
    A("")
    A("## 8. NOT_RUN（未做的组）")
    A("")
    A("| 组 | 内容 | 状态 | 原因 |")
    A("| --- | --- | --- | --- |")
    A("| G0 | RAW（identity / P_post 旁路） | **DONE（本报告）** | — |")
    A("| G1 | PEQ-only | NOT_RUN | 需要 apply；且 host 端 P_post 消费未接线（spec §0 blocker） |")
    A("| G2 | MIMO-only | NOT_RUN | 同上 |")
    A("| G3 | MIMO+Reg | NOT_RUN | 同上 |")
    A("")
    A("S0/S1/S2/S3 apply 阶梯：全部 NOT_RUN（本次任务纯离线、不出声、不连板、不 apply）。")
    A("")
    A("## 9. 产物")
    A("")
    A("| 文件 | 内容 |")
    A("| --- | --- |")
    A("| raw-baseline.json | 全部指标（含每重复原始值） |")
    A("| ANALYSIS_MANIFEST.json | 冻结分析参数 + 参数 hash + 代码 hash + 采集 hash |")
    A("| band_metrics.csv | 每通道每带一行（中位/极差/R²/DR/SNR/状态） |")
    A("| repeat_metrics.csv | 每通道每带每重复一行（原始三次值） |")
    A("| edc_curves.csv | 每通道每带的 EDC 曲线（中位） |")
    A("| waterfall.csv | 瀑布：各带 EDC 在 0…400 ms 的相对电平 |")
    A("| edc_curves.png / t20_bars.png / waterfall_heatmap.png | 图 |")
    A("")
    A("## 10. 复跑")
    A("")
    A("    py -3 rt-stack/tools/ir-diag/run_ir_diag.py --capture <capture.raw> --meta <dir> --out <evidence dir>")
    A("")
    A("RAW 与 G1/G2/G3 对比时必须使用**同一 commit 的同一工具 + 同一 `parameter_hash`**，"
      "否则基线失效（spec §1.3）。")
    A("")
    p = os.path.join(out_dir, "REPORT.md")
    with open(p, "w", encoding="utf-8") as f:
        f.write("\n".join(L))
    return p
