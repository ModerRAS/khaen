"""TSPI 校准批量闭环：DSP 基础函数（numpy 依赖，仅分析/合成使用）。

设计口径（2026-11-17，task-201）：
- 分析输入是"测量记录"（WAV 或 S24_3LE raw）+ 冻结的 capture meta（slot 布局）。
- 用 Farina 指数扫频反卷积得到每通道脉冲响应 IR；IR 只在分析内使用，不作为交付产物。
- 频响/瀑布(CSD)/混响时间(EDC/T20/RT60) 全部从 IR 复算；指标只作**观察量**，不是验收门
  （验收权威仍是 rt-stack/spec/mimo-t6.md 与 calibration-spec/ACCEPTANCE.md）。
- 本模块不 import 项目 Rust 代码，也不写板端。

命名约定：
  freq_hz        频率（Hz）
  db             幅度（dB，相对量；FR 在 1 kHz 归一）
  edc_db         反向累积能量（Schroeder），0 dB = 直接声，向负值衰减
  rt20_s         T20 外推的 RT60（秒）
  residual_db_100ms  EDC 在 100 ms 的读数（越负 = 余振越小）
"""

from __future__ import annotations

import math
import os
import struct
import wave

import numpy as np

# ---------------------------------------------------------------- FFT helpers


def next_pow2(n: int) -> int:
    n = int(max(1, n))
    return 1 << (n - 1).bit_length()


def rfft(x, n=None):
    return np.fft.rfft(x, n=n)


def irfft(spec, n=None):
    return np.fft.irfft(spec, n=n)


def fft_conv(x, h) -> np.ndarray:
    """线性卷积（FFT），返回长度 len(x)+len(h)-1。"""
    x = np.asarray(x, dtype=np.float64)
    h = np.asarray(h, dtype=np.float64)
    n = next_pow2(len(x) + len(h) - 1)
    y = irfft(rfft(x, n) * rfft(h, n), n)
    return np.asarray(y[: len(x) + len(h) - 1], dtype=np.float64)


# ------------------------------------------------------------- sweep / 反卷积


def log_sweep(
    fs: int,
    f1: float,
    f2: float,
    seconds: float,
    fade_ms: float = 10.0,
    level_dbfs: float = 0.0,
) -> np.ndarray:
    """Farina 指数正弦扫频（f1 -> f2），首尾加余弦 fade；峰值归一化到 level_dbfs。"""
    if not (0.0 < f1 < f2):
        raise ValueError("sweep 需要 0 < f1 < f2，实际 f1=%r f2=%r" % (f1, f2))
    n = int(round(seconds * fs))
    if n < 16:
        raise ValueError("sweep 太短：%r s" % seconds)
    t = np.arange(n, dtype=np.float64) / float(fs)
    k = math.log(f2 / f1)
    phase = 2.0 * math.pi * f1 * seconds / k * (np.exp(t * k / seconds) - 1.0)
    x = np.sin(phase)
    n_fade = int(round(fade_ms * 1e-3 * fs))
    n_fade = max(0, min(n_fade, n // 4))
    if n_fade > 0:
        up = 0.5 - 0.5 * np.cos(math.pi * np.arange(n_fade) / float(n_fade))
        w = np.ones(n, dtype=np.float64)
        w[:n_fade] = up
        w[-n_fade:] = up[::-1]
        x = x * w
    peak = float(np.max(np.abs(x))) or 1.0
    amp = 10.0 ** (level_dbfs / 20.0) if level_dbfs <= 0.0 else 1.0
    return (x / peak * amp).astype(np.float64)


def inverse_filter(sweep, fs: int, f1: float, f2: float, seconds: float) -> np.ndarray:
    """Farina 逆滤波器 h_inv(t) = x(T-t) * exp(-t*ln(f2/f1)/T)。"""
    x = np.asarray(sweep, dtype=np.float64)
    n = len(x)
    t = np.arange(n, dtype=np.float64) / float(fs)
    k = math.log(f2 / f1)
    env = np.exp(-t * k / seconds)
    inv = x[::-1] * env
    # 归一化使 sweep ⊛ inv 的主峰幅度为 1，便于复用
    conv = fft_conv(x, inv)
    peak = float(np.max(np.abs(conv))) or 1.0
    return (inv / peak).astype(np.float64)


def deconvolve(rec, inv) -> np.ndarray:
    """记录 ⊛ 逆滤波 -> 全序列（IR 延迟 = len(sweep)-1 + 到达延迟）。"""
    return fft_conv(rec, inv)


def xcorr_abs(window, sweep) -> np.ndarray:
    """|window ⊛ sweep|（有效 lag 0..len(window)-1），用于扫频起点检测。"""
    w = np.asarray(window, dtype=np.float64)
    s = np.asarray(sweep, dtype=np.float64)
    n = next_pow2(len(w) + len(s) - 1)
    corr = irfft(rfft(w, n) * np.conj(rfft(s, n)), n)
    return np.abs(np.asarray(corr[: len(w)], dtype=np.float64))


def onset_xcorr(window, sweep) -> int:
    """在 window 内找 sweep 起点：返回相对 window 起点的样本下标（>=0）。"""
    corr = xcorr_abs(window, sweep)
    if corr.size == 0 or not np.any(corr):
        return -1
    return int(np.argmax(corr))


def extract_ir(
    rec,
    sweep,
    inv,
    fs: int,
    onset: int,
    ir_seconds: float,
    search_peak_seconds: float = 0.30,
) -> np.ndarray:
    """从 rec 的 onset 处取一段做反卷积，返回以直达峰为 0 时刻、长度 ir_seconds 的 IR。"""
    sweep_len = len(sweep)
    tail = int(round(ir_seconds * fs))
    end = min(len(rec), onset + sweep_len + tail)
    seg = np.asarray(rec[onset:end], dtype=np.float64)
    if len(seg) < sweep_len // 2:
        raise ValueError("记录段太短，无法反卷积（len=%d < sweep/2）" % len(seg))
    full = deconvolve(seg, inv)
    lo = max(0, sweep_len - 1 - int(round(search_peak_seconds * fs)))
    hi = min(len(full), sweep_len - 1 + int(round(search_peak_seconds * fs)) + 1)
    if hi <= lo:
        raise ValueError("反卷积峰搜索窗为空")
    peak = lo + int(np.argmax(np.abs(full[lo:hi])))
    ir = np.zeros(tail, dtype=np.float64)
    avail = full[peak : peak + tail]
    ir[: len(avail)] = avail
    return ir


# --------------------------------------------------------------- 频响/平滑


def _run_rms(vals: np.ndarray) -> float:
    if len(vals) == 0:
        return 0.0
    return float(np.sqrt(np.mean(vals * vals)))


def fraction_octave_smooth(freqs, db, frac: int = 6, fmin=None, fmax=None) -> np.ndarray:
    """1/frac 倍频程滑动平均（对数频域）。freqs 单调递增。"""
    freqs = np.asarray(freqs, dtype=np.float64)
    db = np.asarray(db, dtype=np.float64)
    if fmin is None:
        fmin = float(freqs[0])
    if fmax is None:
        fmax = float(freqs[-1])
    half = 1.0 / (2.0 * frac)
    out = np.empty_like(db)
    for i, f in enumerate(freqs):
        if f <= 0.0:
            out[i] = db[i]
            continue
        if f < fmin or f > fmax:
            out[i] = db[i]
            continue
        lo = f * 2.0 ** (-half)
        hi = f * 2.0 ** half
        i0 = int(np.searchsorted(freqs, lo, side="left"))
        i1 = int(np.searchsorted(freqs, hi, side="right"))
        i1 = max(i1, i0 + 1)
        out[i] = float(np.mean(db[i0:i1]))
    return out


def log_freq_grid(f_min: float, f_max: float, points_per_octave: int = 48) -> np.ndarray:
    if f_min <= 0 or f_max <= f_min:
        raise ValueError("freq grid 范围非法")
    n = int(math.ceil(math.log2(f_max / f_min) * points_per_octave))
    return f_min * 2.0 ** (np.arange(n + 1, dtype=np.float64) / float(points_per_octave))


def fr_from_ir(
    ir,
    fs: int,
    nfft: int = 0,
    smooth_frac: int = 6,
    fade_fraction: float = 0.1,
    ref_hz: float = 1000.0,
) -> tuple:
    """IR -> 频响 (freqs, db, complex)。峰值归一到最后 1 kHz 附近。"""
    ir = np.asarray(ir, dtype=np.float64)
    n = len(ir)
    if n == 0:
        raise ValueError("IR 为空")
    if nfft <= 0:
        nfft = next_pow2(2 * n)
    x = ir.copy()
    n_fade = max(1, int(n * fade_fraction))
    x[-n_fade:] *= 0.5 + 0.5 * np.cos(np.pi * np.arange(n_fade) / float(n_fade))
    spec = rfft(x, nfft)
    freqs = np.fft.rfftfreq(nfft, d=1.0 / fs)
    mag = np.abs(spec)
    mag = np.maximum(mag, 1e-12)
    db = 20.0 * np.log10(mag)
    db_s = fraction_octave_smooth(freqs, db, smooth_frac)
    i_ref = int(np.argmin(np.abs(freqs - ref_hz)))
    offset = db_s[i_ref]
    return freqs, db_s - offset, spec


def interp_db_at(freqs, db, f: float) -> float:
    freqs = np.asarray(freqs, dtype=np.float64)
    db = np.asarray(db, dtype=np.float64)
    if f <= freqs[0]:
        return float(db[0])
    if f >= freqs[-1]:
        return float(db[-1])
    i = int(np.searchsorted(freqs, f))
    f0, f1 = freqs[i - 1], freqs[i]
    t = (f - f0) / (f1 - f0)
    return float(db[i - 1] + t * (db[i] - db[i - 1]))


# ------------------------------------------------------------------ 带通/EDC


def band_mask(freqs, fc: float, frac: int = 3) -> np.ndarray:
    """1/frac 倍频程带通 mask（余弦过渡）用于频域滤波。"""
    half = 1.0 / (2.0 * frac)
    soft = half / 2.0
    lo_edge = fc * 2.0 ** (-half)
    hi_edge = fc * 2.0 ** (half)
    lo_outer = fc * 2.0 ** (-half - soft)
    hi_outer = fc * 2.0 ** (half + soft)
    m = np.zeros_like(freqs, dtype=np.float64)
    m[(freqs >= lo_edge) & (freqs <= hi_edge)] = 1.0
    up = (freqs > lo_outer) & (freqs < lo_edge)
    if np.any(up):
        t = (freqs[up] - lo_outer) / max(lo_edge - lo_outer, 1e-12)
        m[up] = 0.5 - 0.5 * np.cos(np.pi * t)
    down = (freqs > hi_edge) & (freqs < hi_outer)
    if np.any(down):
        t = (hi_outer - freqs[down]) / max(hi_outer - hi_edge, 1e-12)
        m[down] = 0.5 - 0.5 * np.cos(np.pi * t)
    return m


def bandpass_biquad(fc: float, fs: int, frac: int = 3, passes: int = 2) -> list:
    """1/frac 倍频程带通（RBJ bandpass，0 dB 峰值）；默认级联 2 级。

    用 IIR 而不是 FFT 矩形/余弦带掩码：后者在 0.85 s 短 IR 上会产生 sinc 长尾，
    把 EDC/T20 的衰减判断污染成"几乎不衰减"（离线自测已暴露）。
    """
    q = 1.0 / (2.0 ** (1.0 / (2.0 * frac)) - 2.0 ** (-1.0 / (2.0 * frac)))
    w0 = 2.0 * math.pi * float(fc) / float(fs)
    alpha = math.sin(w0) / (2.0 * q)
    cosw = math.cos(w0)
    b0 = alpha
    b2 = -alpha
    a0 = 1.0 + alpha
    a1 = -2.0 * cosw
    a2 = 1.0 - alpha
    bq = {"type": "bandpass", "freq_hz": float(fc), "gain_db": 0.0, "q": float(q),
          "b": [b0 / a0, 0.0, b2 / a0], "a": [1.0, a1 / a0, a2 / a0]}
    return [bq for _ in range(max(1, passes))]


def band_ir(ir, fs: int, fc: float, frac: int = 3) -> np.ndarray:
    return apply_filters_fft(ir, bandpass_biquad(fc, fs, frac), fs)


def edc_db(ir) -> np.ndarray:
    """Schroeder 反向累积能量（dB），0 dB 在 IR 起点。"""
    ir = np.asarray(ir, dtype=np.float64)
    if len(ir) == 0:
        return np.zeros(0, dtype=np.float64)
    e = np.cumsum((ir[::-1]) ** 2)[::-1]
    e0 = float(e[0]) if len(e) else 0.0
    if e0 <= 0.0:
        return np.full(len(ir), -200.0, dtype=np.float64)
    return 10.0 * np.log10(np.maximum(e / e0, 1e-12))


def rt_from_edc(edc: np.ndarray, fs: int, t20_low_db: float = -5.0, t20_high_db: float = -25.0,
                min_r2: float = 0.90, min_dynamic_range_db: float = 15.0):
    """从 EDC 拟合 T20 -> RT60。返回 dict(rt20_s, r2, valid, reason)。"""
    edc = np.asarray(edc, dtype=np.float64)
    if len(edc) < 16:
        return {"rt20_s": None, "r2": None, "valid": False, "reason": "edc_too_short"}
    i5 = int(np.argmax(edc <= t20_low_db)) if np.any(edc <= t20_low_db) else -1
    i25 = int(np.argmax(edc <= t20_high_db)) if np.any(edc <= t20_high_db) else -1
    if i5 <= 0 or i25 <= i5:
        return {"rt20_s": None, "r2": None, "valid": False, "reason": "edc_range_lt_25db"}
    seg = edc[i5:i25]
    if float(seg[0] - seg[-1]) < min_dynamic_range_db:
        return {"rt20_s": None, "r2": None, "valid": False, "reason": "dynamic_range_low"}
    t = np.arange(i5, i25, dtype=np.float64) / float(fs)
    y = seg
    slope, intercept = np.polyfit(t, y, 1)
    if slope >= -1e-9:
        return {"rt20_s": None, "r2": None, "valid": False, "reason": "edc_not_decaying"}
    pred = slope * t + intercept
    ss_res = float(np.sum((y - pred) ** 2))
    ss_tot = float(np.sum((y - float(np.mean(y))) ** 2)) or 1e-12
    r2 = 1.0 - ss_res / ss_tot
    rt20 = -60.0 / slope
    valid = r2 >= min_r2
    return {
        "rt20_s": float(rt20),
        "r2": float(r2),
        "valid": bool(valid),
        "reason": "" if valid else "fit_r2_low",
    }


def edc_at_ms(edc: np.ndarray, fs: int, ms: float) -> float:
    edc = np.asarray(edc, dtype=np.float64)
    idx = int(round(ms * 1e-3 * fs))
    if len(edc) == 0:
        return -200.0
    if idx >= len(edc):
        return float(edc[-1])
    if idx <= 0:
        return float(edc[0])
    return float(edc[idx])


def estimate_snr_db(ir, fs: int, noise_tail_seconds: float = 0.1) -> float:
    """IR 直达峰 / IR 末端噪声底（简单估计，只作观察与质量门提示）。"""
    ir = np.asarray(ir, dtype=np.float64)
    if len(ir) < 64:
        return 0.0
    peak = float(np.max(np.abs(ir)))
    n_tail = max(16, int(noise_tail_seconds * fs))
    if len(ir) <= n_tail:
        return 0.0
    tail = ir[-n_tail:]
    tail_rms = float(np.sqrt(np.mean(tail * tail)))
    if tail_rms <= 0.0:
        return 120.0
    return float(20.0 * math.log10(max(peak, 1e-12) / max(tail_rms, 1e-12)))


# ------------------------------------------------------------------ Biquad（PEQ）


def peaking_biquad(fc: float, gain_db: float, q: float, fs: int) -> dict:
    """RBJ peaking EQ 系数（a0 归一）。"""
    a = 10.0 ** (gain_db / 40.0)
    w0 = 2.0 * math.pi * fc / fs
    alpha = math.sin(w0) / (2.0 * max(q, 1e-4))
    b0 = 1.0 + alpha * a
    b1 = -2.0 * math.cos(w0)
    b2 = 1.0 - alpha * a
    a0 = 1.0 + alpha / a
    a1 = -2.0 * math.cos(w0)
    a2 = 1.0 - alpha / a
    return {"type": "peaking", "freq_hz": float(fc), "gain_db": float(gain_db), "q": float(q),
            "b": [b0 / a0, b1 / a0, b2 / a0], "a": [1.0, a1 / a0, a2 / a0]}


def lowpass_biquad(fc: float, fs: int, q: float = 0.7071) -> dict:
    w0 = 2.0 * math.pi * fc / fs
    alpha = math.sin(w0) / (2.0 * q)
    cosw = math.cos(w0)
    b0 = (1.0 - cosw) / 2.0
    b1 = 1.0 - cosw
    b2 = (1.0 - cosw) / 2.0
    a0 = 1.0 + alpha
    a1 = -2.0 * cosw
    a2 = 1.0 - alpha
    return {"type": "lowpass", "freq_hz": float(fc), "gain_db": 0.0, "q": float(q),
            "b": [b0 / a0, b1 / a0, b2 / a0], "a": [1.0, a1 / a0, a2 / a0]}


def biquad_response_db(biquad: dict, freqs, fs: int) -> np.ndarray:
    w = 2.0 * math.pi * np.asarray(freqs, dtype=np.float64) / float(fs)
    z1 = np.exp(-1j * w)
    z2 = np.exp(-2j * w)
    b = biquad["b"]
    a = biquad["a"]
    h = (b[0] + b[1] * z1 + b[2] * z2) / (a[0] + a[1] * z1 + a[2] * z2)
    return 20.0 * np.log10(np.maximum(np.abs(h), 1e-12))


def filters_response_db(filters, freqs, fs: int) -> np.ndarray:
    total = np.zeros(len(np.asarray(freqs)), dtype=np.float64)
    for bq in filters:
        total += biquad_response_db(bq, freqs, fs)
    return total


def apply_filters_fft(ir, filters, fs: int) -> np.ndarray:
    ir = np.asarray(ir, dtype=np.float64)
    if not filters or len(ir) == 0:
        return ir
    n = next_pow2(max(len(ir), 256))
    freqs = np.fft.rfftfreq(n, d=1.0 / fs)
    h_db = filters_response_db(filters, freqs, fs)
    spec = rfft(ir, n) * (10.0 ** (h_db / 20.0))
    out = irfft(spec, n)
    return out[: len(ir)]


# ------------------------------------------------------------------ 文件 I/O


def write_wav_raw(path: str, data, fs: int, channels: int = 1) -> int:
    """写 16-bit PCM WAV（分析用；不作为板端生产格式）。返回字节数。"""
    arr = np.asarray(data, dtype=np.float64)
    if arr.ndim == 1 and channels > 1:
        raise ValueError("单声道数组不能按多声道写")
    if arr.ndim == 2 and arr.shape[1] != channels:
        raise ValueError("通道数不符")
    peak = float(np.max(np.abs(arr))) if arr.size else 0.0
    scale = 32767.0 * (0.99 / peak) if peak > 1.0 else 32767.0
    ints = np.clip(np.round(arr * scale), -32768, 32767).astype("<i2")
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with wave.open(path, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(fs)
        w.writeframes(ints.tobytes())
    return os.path.getsize(path)


def read_wav(path: str):
    """读 WAV -> (float64 [n] 或 [n,ch], fs, channels)。"""
    with wave.open(path, "rb") as w:
        channels = w.getnchannels()
        width = w.getsampwidth()
        fs = w.getframerate()
        raw = w.readframes(w.getnframes())
    if width == 2:
        data = np.frombuffer(raw, dtype="<i2").astype(np.float64) / 32768.0
    elif width == 3:
        b = np.frombuffer(raw, dtype=np.uint8).reshape(-1, 3).astype(np.int32)
        ints = b[:, 0] | (b[:, 1] << 8) | (b[:, 2] << 16)
        ints = np.where(ints >= 1 << 23, ints - (1 << 24), ints)
        data = ints.astype(np.float64) / float(1 << 23)
    elif width == 4:
        data = np.frombuffer(raw, dtype="<i4").astype(np.float64) / float(1 << 31)
    else:
        raise ValueError("不支持的 WAV 采样宽度: %d" % width)
    if channels > 1:
        data = data.reshape(-1, channels)
    return data, fs, channels


def read_raw_s24(path: str, channels: int = 1):
    """读板端 S24_3LE 裸流（tspi-calibrate 的 capture.raw 格式）。"""
    with open(path, "rb") as fh:
        raw = fh.read()
    if len(raw) % 3 != 0:
        raise ValueError("raw 字节数不是 3 的倍数: %d" % len(raw))
    b = np.frombuffer(raw, dtype=np.uint8).reshape(-1, 3)
    ints = (b[:, 0].astype(np.int32) | (b[:, 1].astype(np.int32) << 8) | (b[:, 2].astype(np.int32) << 16))
    ints = np.where(ints >= (1 << 23), ints - (1 << 24), ints).astype(np.int32)
    data = ints.astype(np.float64) / float(1 << 23)
    if channels > 1:
        usable = (len(data) // channels) * channels
        data = data[:usable].reshape(-1, channels)
    return data


# ------------------------------------------------------------------ SVG 输出


def _svg_header(width: int, height: int, title: str) -> list:
    return [
        '<?xml version="1.0" encoding="UTF-8"?>',
        '<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" viewBox="0 0 %d %d">' % (width, height, width, height),
        '<rect width="100%%" height="100%%" fill="#ffffff"/>',
        '<text x="%d" y="24" font-family="sans-serif" font-size="16" text-anchor="middle">%s</text>' % (width // 2, _xml_escape(title)),
    ]


def _xml_escape(s: str) -> str:
    return (str(s).replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;"))


def svg_decay_plot(path: str, series: dict, title: str, y_label: str = "level (dB)",
                   y_min: float = -60.0, y_max: float = 5.0, x_max: float = 400.0,
                   x_unit: str = "ms") -> None:
    """series: {label: [(x,y), ...]}，画多曲线 SVG。"""
    width, height = 760, 420
    left, right, top, bottom = 64, 18, 40, 52
    pw, ph = width - left - right, height - top - bottom
    lines = _svg_header(width, height, title)
    lines.append('<line x1="%d" y1="%d" x2="%d" y2="%d" stroke="#333" stroke-width="1"/>' % (left, top + ph, left + pw, top + ph))
    lines.append('<line x1="%d" y1="%d" x2="%d" y2="%d" stroke="#333" stroke-width="1"/>' % (left, top, left, top + ph))
    n_ticks = 6
    for i in range(n_ticks + 1):
        yv = y_min + (y_max - y_min) * i / float(n_ticks)
        y = top + ph - ph * i / float(n_ticks)
        lines.append('<line x1="%d" y1="%.1f" x2="%d" y2="%.1f" stroke="#e5e5e5" stroke-width="1"/>' % (left, y, left + pw, y))
        lines.append('<text x="%d" y="%.1f" font-family="sans-serif" font-size="11" text-anchor="end">%.0f</text>' % (left - 6, y + 4, yv))
    for i in range(6 + 1):
        xv = x_max * i / 6.0
        x = left + pw * i / 6.0
        lines.append('<line x1="%.1f" y1="%d" x2="%.1f" y2="%d" stroke="#e5e5e5" stroke-width="1"/>' % (x, top, x, top + ph))
        lines.append('<text x="%.1f" y="%d" font-family="sans-serif" font-size="11" text-anchor="middle">%.0f</text>' % (x, top + ph + 16, xv))
    palette = ["#1f77b4", "#d62728", "#2ca02c", "#ff7f0e", "#9467bd", "#8c564b", "#17becf", "#7f7f7f"]
    for idx, (label, pts) in enumerate(series.items()):
        color = palette[idx % len(palette)]
        d = []
        for (xv, yv) in pts:
            if xv > x_max or yv < y_min or yv > y_max:
                continue
            x = left + pw * (xv / x_max)
            y = top + ph * (1.0 - (yv - y_min) / (y_max - y_min))
            d.append("%.1f,%.1f" % (x, y))
        if len(d) >= 2:
            lines.append('<polyline fill="none" stroke="%s" stroke-width="1.6" points="%s"/>' % (color, " ".join(d)))
        lx = left + 10 + (idx % 4) * 170
        ly = top + ph + 34 + (idx // 4) * 14
        lines.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" fill="%s">%s</text>' % (lx, ly, color, _xml_escape(label)))
    lines.append('<text x="%d" y="%d" font-family="sans-serif" font-size="12" text-anchor="middle">%s</text>' % (left + pw // 2, height - 6, _xml_escape(x_unit)))
    lines.append('<text transform="translate(16,%d) rotate(-90)" font-family="sans-serif" font-size="12" text-anchor="middle">%s</text>' % (top + ph // 2, _xml_escape(y_label)))
    lines.append("</svg>")
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")


def svg_heatmap(path: str, x_labels, y_labels, matrix, title: str, vmin: float = -60.0, vmax: float = 0.0) -> None:
    """matrix[y][x] -> 颜色热图（用于瀑布/CSD 概览）。"""
    ny, nx = len(y_labels), len(x_labels)
    cell_w = max(8, min(30, int(640 / max(nx, 1))))
    cell_h = max(10, min(26, int(360 / max(ny, 1))))
    width = 90 + nx * cell_w + 10
    height = 50 + ny * cell_h + 60
    lines = _svg_header(width, height, title)

    def color(v):
        t = (float(v) - vmin) / (vmax - vmin) if vmax > vmin else 0.0
        t = max(0.0, min(1.0, t))
        r = int(255 * (1.0 - t))
        g = int(90 + 165 * t)
        b = int(255 * (1.0 - t) + 40 * t)
        return "#%02x%02x%02x" % (r, g, b)

    for yi in range(ny):
        for xi in range(nx):
            v = matrix[yi][xi]
            x = 90 + xi * cell_w
            y = 50 + yi * cell_h
            lines.append('<rect x="%d" y="%d" width="%d" height="%d" fill="%s" stroke="#ffffff" stroke-width="0.5"/>'
                         % (x, y, cell_w, cell_h, color(v)))
    for xi in range(nx):
        if nx <= 24 or xi % max(1, nx // 16) == 0:
            lines.append('<text x="%d" y="%d" font-family="sans-serif" font-size="9" text-anchor="end" transform="rotate(-60 %d %d)">%s</text>'
                         % (90 + xi * cell_w + cell_w // 2, 50 + ny * cell_h + 18, 90 + xi * cell_w + cell_w // 2, 50 + ny * cell_h + 18, _xml_escape(x_labels[xi])))
    for yi in range(ny):
        lines.append('<text x="84" y="%d" font-family="sans-serif" font-size="9" text-anchor="end">%s</text>'
                     % (50 + yi * cell_h + cell_h - 3, _xml_escape(y_labels[yi])))
    lines.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" text-anchor="middle">%s (%.0f..%.0f dB)</text>'
                 % (90 + nx * cell_w // 2, height - 8, _xml_escape("level"), vmin, vmax))
    lines.append("</svg>")
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")
