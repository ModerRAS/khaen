#!/usr/bin/env python3
"""task-227 sweep replay verification -- offline analysis (numpy; stdlib otherwise).

task-229 fix (2026-11-18): IR 提取段内偏移 delay_ms_from_segment_start 曾被当作绝对直达延迟
并换算距离（MA1 "0.08 ms 近场/贴箱" 结论错误）。本版改为显式时基口径：
- ir.direct_path.relative_to_segment：段内相对峰值位置，只能段内对齐/相对比较；
- ir.direct_path.absolute_from_t0 / distance：只有 --timebase-source != none 且给出
  --t0-sample（同一次采集内的公共时基）时才可能输出；否则 INVALID_NO_COMMON_TIMEBASE；
- 房间/未知麦算出的距离 < 0.5 m 标 SUSPECT_TOO_CLOSE_FOR_ROOM_MIC（见 ir_distance.py）。

Method reused from the T67 pilot analysis chain
(rt-stack/evidence/board-window-20261118-calib/analysis/pilot_analysis.py):
Farina-style spectral-division deconvolution of a known exponential sweep -> IR;
IR -> 1/3-octave FR with mic calibration applied additively; octave-band Schroeder
EDC -> T20/T15x4 RT60 + EDR@100 ms; 1/3-octave band-passed envelope -> CSD 0..400 ms.
On top of that: cross-correlation verification (peak lag / normalised coefficient /
band-limited SNR) and expected(flat)-vs-measured FR comparison.

Inputs: raw S24_3LE 48 kHz interleaved captures plus the exact played sweep
(playback-channel raw extracted from the board composite, or the source WAV).
"""
import json
import math
import os
import struct
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ir_distance  # noqa: E402

FS = 48000
OCT_BANDS = (63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0)
NOMINAL_3OCT = (20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0,
                400.0, 500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0,
                4000.0, 5000.0, 6300.0, 8000.0, 10000.0, 12500.0, 16000.0, 20000.0)
CSD_BANDS = tuple(63.0 * 2 ** (k / 3.0) for k in range(0, 25))
CSD_T = np.arange(0, 401, 10, dtype=np.float64)


def jload(path):
    with open(path, "r", encoding="utf-8") as fh:
        return json.load(fh)


def sha256_file(path):
    import hashlib
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def read_s24(path, nch, max_frames=None):
    n = os.path.getsize(path) // (nch * 3)
    if max_frames:
        n = min(n, max_frames)
    b = np.fromfile(path, dtype=np.uint8, count=n * nch * 3).reshape(n, nch * 3).astype(np.int32)
    v = b[:, 0::3] | (b[:, 1::3] << 8) | (b[:, 2::3] << 16)
    v = np.where(v & 0x800000, v - 0x1000000, v)
    return v.astype(np.float64) / 8388608.0


def read_wav_s24_ch0(path):
    data = open(path, "rb").read()
    pos, fmt, payload = 12, None, None
    while pos + 8 <= len(data):
        cid = data[pos:pos + 4]
        sz = struct.unpack("<I", data[pos + 4:pos + 8])[0]
        body = data[pos + 8:pos + 8 + sz]
        if cid == b"fmt ":
            fmt = struct.unpack("<HHIIHH", body[:16])
        elif cid == b"data":
            payload = body
        pos += 8 + sz + (sz & 1)
    af, ch, sr, br, ba, bits = fmt
    assert af == 1 and bits == 24 and ba == ch * 3, "expect PCM24"
    n = len(payload) // ba
    b = np.frombuffer(payload[:n * ba], dtype=np.uint8).reshape(n, ch * 3).astype(np.int32)
    v = b[:, 0::3] | (b[:, 1::3] << 8) | (b[:, 2::3] << 16)
    v = np.where(v & 0x800000, v - 0x1000000, v)
    return v.astype(np.float64)[:, 0] / 8388608.0, ch, sr


def write_wav24_mono(path, x, fs=FS):
    ints = np.clip(np.round(np.asarray(x) * 8388607.0), -8388608, 8388607).astype(np.int64)
    raw = bytearray()
    for v in ints.tolist():
        if v < 0:
            v += 1 << 24
        raw += v.to_bytes(3, "little")
    block = bytes(raw)
    hdr = (b"RIFF" + struct.pack("<I", 36 + len(block)) + b"WAVE" +
           b"fmt " + struct.pack("<IHHIIHH", 16, 1, 1, fs, fs * 3, 3, 24) +
           b"data" + struct.pack("<I", len(block)))
    with open(path, "wb") as fh:
        fh.write(hdr)
        fh.write(block)


def load_cal(path):
    f, d = [], []
    for line in open(path, encoding="utf-8", errors="replace"):
        line = line.strip()
        if not line or line.startswith((chr(34), "*", "#")):
            continue
        p = line.split()
        if len(p) >= 2:
            try:
                f.append(float(p[0]))
                d.append(float(p[1]))
            except ValueError:
                pass
    return np.array(f), np.array(d)


def db(x):
    return float(20 * np.log10(max(float(x), 1e-15)))


def fft_bandpass(x, lo, hi):
    X = np.fft.rfft(x)
    f = np.fft.rfftfreq(len(x), 1.0 / FS)
    X[(f < lo) | (f > hi)] = 0.0
    return np.fft.irfft(X, len(x))


def xcorr_find_start(rec, sweep):
    n = 1 << int(math.ceil(math.log2(len(rec) + len(sweep))))
    a = np.fft.rfft(rec, n)
    b = np.fft.rfft(sweep, n)
    c = np.fft.irfft(a * np.conj(b), n)
    k = int(np.argmax(np.abs(c)))
    if k > n // 2:
        k -= n
    lo = max(0, k)
    seg = rec[lo:lo + len(sweep)]
    if len(seg) < len(sweep):
        seg = np.pad(seg, (0, len(sweep) - len(seg)))
    denom = float(np.linalg.norm(seg) * np.linalg.norm(sweep))
    coef = float(np.max(np.abs(c))) / denom if denom > 0 else 0.0
    return k, coef, c


def deconvolve(seg, sweep, tail_s=0.9, reg=1e-5):
    nfft = 1 << int(math.ceil(math.log2(len(seg) + len(sweep))))
    Y = np.fft.rfft(seg, nfft)
    X = np.fft.rfft(sweep, nfft)
    P = np.abs(X) ** 2
    H = Y * np.conj(X) / (P + reg * P.max())
    ir = np.fft.irfft(H, nfft)
    return ir[: int(len(sweep) + tail_s * FS)]


def third_oct_grid(fmin=20.0, fmax=20000.0):
    return 20.0 * 2 ** (np.arange(0, int(3 * math.log2(fmax / fmin)) + 1) / 3.0)


def fr_on_grid(ir, peak, grid, cal=None, window_ms=200.0):
    """1/N-octave energy FR on an explicit (nominal) centre-frequency grid."""
    p0 = max(0, peak - int(0.002 * FS))
    n = int(window_ms / 1000.0 * FS)
    w = ir[p0:p0 + n]
    if len(w) < n:
        w = np.pad(w, (0, n - len(w)))
    win = np.hanning(n)
    NF = 1 << 17
    FR = np.fft.rfft(w * win, NF)
    f = np.fft.rfftfreq(NF, 1.0 / FS)
    mag = 20 * np.log10(np.abs(FR) + 1e-30)
    out = np.full(len(grid), np.nan)
    for i, fc in enumerate(grid):
        lo, hi = fc / 2 ** (1.0 / 6.0), fc * 2 ** (1.0 / 6.0)
        m = (f >= lo) & (f <= hi)
        if m.sum():
            out[i] = 10 * np.log10(np.mean(10 ** (mag[m] / 10.0)) + 1e-30)
    if cal is not None:
        cf, cd = cal
        out = out + np.interp(grid, cf, cd)
    g = np.array(grid, dtype=np.float64)
    refm = (g > 200) & (g < 2000)
    return out - np.nanmedian(out[refm])


def fr_oct(ir, peak, cal=None, frac=24, window_ms=200.0):
    p0 = max(0, peak - int(0.002 * FS))
    n = int(window_ms / 1000.0 * FS)
    w = ir[p0:p0 + n]
    if len(w) < n:
        w = np.pad(w, (0, n - len(w)))
    win = np.hanning(n)
    NF = 1 << 17
    FR = np.fft.rfft(w * win, NF)
    f = np.fft.rfftfreq(NF, 1.0 / FS)
    mag = 20 * np.log10(np.abs(FR) + 1e-30)
    grid = 20.0 * 2 ** (np.arange(0, int(frac * math.log2(20000.0 / 20.0)) + 1) / frac)
    out = np.full_like(grid, np.nan)
    for i, fc in enumerate(grid):
        lo, hi = fc / 2 ** (0.5 / frac), fc * 2 ** (0.5 / frac)
        m = (f >= lo) & (f <= hi)
        if m.sum():
            out[i] = 10 * np.log10(np.mean(10 ** (mag[m] / 10.0)) + 1e-30)
    if cal is not None:
        cf, cd = cal
        out = out + np.interp(grid, cf, cd)
    refm = (grid > 200) & (grid < 2000)
    return grid, out - np.median(out[refm])


def oct_band_padded(ir, fc, width_oct=1.0, pad_s=2.0):
    xp = np.concatenate([ir, np.zeros(int(pad_s * FS))])
    X = np.fft.rfft(xp)
    f = np.fft.rfftfreq(len(xp), 1.0 / FS)
    lo, hi = fc / 2 ** (width_oct / 2.0), fc * 2 ** (width_oct / 2.0)
    X[(f < lo) | (f > hi)] = 0.0
    return np.fft.irfft(X, len(xp))[: len(ir)]


def edc_curve(x):
    e = np.cumsum((np.asarray(x, dtype=np.float64) ** 2)[::-1])[::-1]
    e = np.maximum(e, 1e-300)
    return 10.0 * np.log10(e / e[0])


def rt_estimate(tail, fc):
    b = oct_band_padded(tail, fc)
    edc = edc_curve(b)
    t = np.arange(len(edc)) / FS
    edr100 = float(np.interp(0.1, t, edc))
    nfl = float(np.median(edc[int(0.9 * len(edc)):]))
    limit_db = min(-25.0, nfl - 3.0)

    def fit(a_db, b_db):
        if edc[0] < a_db or edc.min() > b_db:
            return None
        i0 = int(np.argmax(edc <= a_db))
        i1 = int(np.argmax(edc <= b_db))
        if i1 <= i0:
            return None
        tt = t[i0:i1 + 1]
        ee = edc[i0:i1 + 1]
        A = np.vstack([tt, np.ones_like(tt)]).T
        slope = np.linalg.lstsq(A, ee, rcond=None)[0][0]
        if slope >= -1e-9:
            return None
        return float(-60.0 / slope), float(slope)

    r20 = fit(-5.0, -25.0) if limit_db <= -25.0 else None
    r15 = fit(-5.0, -20.0)
    out = {"edr100_db": round(edr100, 2), "noise_floor_edc_db": round(nfl, 2),
           "method": None, "rt60_s": None, "slope_db_s": None}
    if r20 is not None:
        out.update(method="T20", rt60_s=round(r20[0], 3), slope_db_s=round(r20[1], 2))
    elif r15 is not None:
        out.update(method="T15x4", rt60_s=round(r15[0] * 4.0 / 3.0, 3), slope_db_s=round(r15[1], 2))
    return out


def csd_matrix(tail):
    mat = np.zeros((len(CSD_BANDS), len(CSD_T)))
    for bi, fc in enumerate(CSD_BANDS):
        b = oct_band_padded(tail, fc, width_oct=1.0 / 3.0, pad_s=0.5)
        for ti, tms in enumerate(CSD_T):
            i0 = int(tms / 1000.0 * FS)
            i1 = i0 + int(0.010 * FS)
            if i1 > len(b):
                break
            mat[bi, ti] = float(np.mean(b[i0:i1] ** 2))
    d = 10 * np.log10(mat + 1e-30)
    d -= np.nanmax(d, axis=1, keepdims=True)
    return d


def spectrum_third_oct(x, t0=0.0, t1=None):
    seg = x[int(t0 * FS): int((t1 if t1 is not None else len(x) / FS) * FS)]
    n = len(seg)
    w = np.hanning(n)
    mag = np.abs(np.fft.rfft(seg * w)) / (np.sum(w) / 2.0)
    f = np.fft.rfftfreq(n, 1.0 / FS)
    grid = third_oct_grid()
    out = np.full_like(grid, np.nan)
    for i, fc in enumerate(grid):
        lo, hi = fc / 2 ** (1.0 / 6.0), fc * 2 ** (1.0 / 6.0)
        m = (f >= lo) & (f <= hi)
        if m.sum():
            out[i] = 10 * np.log10(np.mean(mag[m] ** 2) + 1e-30)
    return grid, out


def _svg_head(w, h, title):
    return ['<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" viewBox="0 0 %d %d">' % (w, h, w, h),
            '<rect width="100%" height="100%" fill="#ffffff"/>',
            '<text x="10" y="18" font-family="sans-serif" font-size="14" fill="#111">%s</text>' % title]


def svg_fr(path, title, grid, curve_db):
    w, h = 900, 520
    lx0, lx1, ly0, ly1 = 70, w - 20, 30, h - 50
    ymin, ymax = -30.0, 10.0
    lxmin, lxmax = math.log10(20.0), math.log10(20000.0)

    def xp(f):
        return lx0 + (math.log10(f) - lxmin) / (lxmax - lxmin) * (lx1 - lx0)

    def yp(d):
        return ly0 + (ymax - max(ymin, min(ymax, d))) / (ymax - ymin) * (ly1 - ly0)

    out = _svg_head(w, h, title)
    out.append('<line x1="%d" y1="%d" x2="%d" y2="%d" stroke="#333"/>' % (lx0, ly1, lx1, ly1))
    out.append('<line x1="%d" y1="%d" x2="%d" y2="%d" stroke="#333"/>' % (lx0, ly0, lx0, ly1))
    for f in (20, 30, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000):
        out.append('<line x1="%.1f" y1="%d" x2="%.1f" y2="%d" stroke="#eee"/>' % (xp(f), ly0, xp(f), ly1))
        out.append('<text x="%.1f" y="%d" font-family="sans-serif" font-size="10" fill="#666" text-anchor="middle">%d</text>' % (xp(f), ly1 + 14, f))
    for d in range(int(ymin), int(ymax) + 1, 5):
        out.append('<line x1="%d" y1="%.1f" x2="%d" y2="%.1f" stroke="#eee"/>' % (lx0, yp(d), lx1, yp(d)))
        out.append('<text x="%d" y="%.1f" font-family="sans-serif" font-size="10" fill="#666" text-anchor="end">%d</text>' % (lx0 - 6, yp(d) + 3, d))
    out.append('<line x1="%d" y1="%.1f" x2="%d" y2="%.1f" stroke="#888" stroke-dasharray="4,3"/>' % (lx0, yp(0.0), lx1, yp(0.0)))
    pts = ["%.1f,%.1f" % (xp(f), yp(d)) for f, d in zip(grid, curve_db) if d == d]
    out.append('<polyline points="%s" fill="none" stroke="#1f77b4" stroke-width="1.6"/>' % " ".join(pts))
    out.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" fill="#1f77b4">measured FR (1/24 oct, cal applied)</text>' % (lx1 - 300, ly0 + 14))
    out.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" fill="#888">expected: flat 0 dB (ideal)</text>' % (lx1 - 300, ly0 + 30))
    out.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" fill="#333" text-anchor="middle">frequency (Hz)</text>' % ((lx0 + lx1) // 2, h - 12))
    out.append('<text x="14" y="%d" font-family="sans-serif" font-size="11" fill="#333" transform="rotate(-90 14 %d)">dB re 200-2k median</text>' % ((ly0 + ly1) // 2, (ly0 + ly1) // 2))
    out.append('</svg>')
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(out))


def svg_rt(path, title, bands, series):
    w, h = 860, 440
    lx0, lx1, ly0, ly1 = 70, w - 20, 30, h - 50
    ymax = 3.0

    def xp(i):
        return lx0 + (i + 0.5) / len(bands) * (lx1 - lx0)

    def yp(v):
        return ly1 - (0.0 if v is None else min(v, ymax)) / ymax * (ly1 - ly0)

    out = _svg_head(w, h, title)
    out.append('<line x1="%d" y1="%d" x2="%d" y2="%d" stroke="#333"/>' % (lx0, ly1, lx1, ly1))
    out.append('<line x1="%d" y1="%d" x2="%d" y2="%d" stroke="#333"/>' % (lx0, ly0, lx0, ly1))
    for i, fc in enumerate(bands):
        out.append('<text x="%.1f" y="%d" font-family="sans-serif" font-size="10" fill="#666" text-anchor="middle">%d</text>' % (xp(i), ly1 + 14, fc))
    for v in (0.5, 1.0, 1.5, 2.0, 2.5, 3.0):
        out.append('<line x1="%d" y1="%.1f" x2="%d" y2="%.1f" stroke="#eee"/>' % (lx0, yp(v), lx1, yp(v)))
        out.append('<text x="%d" y="%.1f" font-family="sans-serif" font-size="10" fill="#666" text-anchor="end">%.1f</text>' % (lx0 - 6, yp(v) + 3, v))
    for si, (label, vals) in enumerate(series):
        pts = []
        for i, v in enumerate(vals):
            if v is not None:
                pts.append("%.1f,%.1f" % (xp(i), yp(v)))
        out.append('<polyline points="%s" fill="none" stroke="%s" stroke-width="1.6"/>' % (" ".join(pts), ["#1f77b4", "#ff7f0e"][si % 2]))
        out.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" fill="%s">%s</text>' % (lx1 - 180, ly0 + 14 + si * 14, ["#1f77b4", "#ff7f0e"][si % 2], label))
    out.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" fill="#333" text-anchor="middle">octave band (Hz)</text>' % ((lx0 + lx1) // 2, h - 12))
    out.append('<text x="14" y="%d" font-family="sans-serif" font-size="11" fill="#333" transform="rotate(-90 14 %d)">RT60 (s)</text>' % ((ly0 + ly1) // 2, (ly0 + ly1) // 2))
    out.append('</svg>')
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(out))


def svg_csd(path, title, bands, times, mat, lo=-30.0, hi=0.0):
    w, h = 780, 480
    lx0, lx1, ly0, ly1 = 110, w - 20, 40, h - 55
    nband, nt = len(bands), len(times)
    cw = (lx1 - lx0) / max(1, nt)
    chh = (ly1 - ly0) / max(1, nband)

    def cmap(v):
        t = (max(lo, min(hi, v)) - lo) / (hi - lo)
        r = int(255 * (1 - t))
        g = int(80 * (1 - t))
        b = int(255 * t)
        return "rgb(%d,%d,%d)" % (r, g, b)

    out = _svg_head(w, h, title)
    for i, fc in enumerate(bands):
        y = ly0 + (nband - 1 - i) * chh
        out.append('<text x="%d" y="%.1f" font-family="sans-serif" font-size="9" fill="#333" text-anchor="end">%.0f</text>' % (lx0 - 6, y + chh / 2 + 3, fc))
        for j in range(nt):
            v = mat[i][j]
            c = cmap(v) if v == v else "#999999"
            out.append('<rect x="%.1f" y="%.1f" width="%.1f" height="%.1f" fill="%s"/>' % (lx0 + j * cw, y, cw + 0.5, chh + 0.5, c))
    out.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" fill="#333" text-anchor="middle">time after direct sound (ms)</text>' % ((lx0 + lx1) // 2, h - 12))
    out.append('<text x="14" y="%d" font-family="sans-serif" font-size="11" fill="#333" transform="rotate(-90 14 %d)">1/3-oct band (Hz), dB re band max</text>' % ((ly0 + ly1) // 2, (ly0 + ly1) // 2))
    out.append('</svg>')
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(out))


def svg_xcorr(path, title, lag_ms, corr, peak_lag_ms, peak_val):
    w, h = 860, 380
    lx0, lx1, ly0, ly1 = 70, w - 20, 30, h - 50
    vmax = max(peak_val, 1e-12)

    def xp(t):
        return lx0 + (t + 200.0) / 400.0 * (lx1 - lx0)

    def yp(v):
        return ly0 + (1.0 - v / vmax) * (ly1 - ly0)

    out = _svg_head(w, h, title)
    out.append('<line x1="%d" y1="%d" x2="%d" y2="%d" stroke="#333"/>' % (lx0, ly1, lx1, ly1))
    out.append('<line x1="%d" y1="%d" x2="%d" y2="%d" stroke="#333"/>' % (lx0, ly0, lx0, ly1))
    for t in (-200, -150, -100, -50, 0, 50, 100, 150, 200):
        out.append('<line x1="%.1f" y1="%d" x2="%.1f" y2="%d" stroke="#eee"/>' % (xp(t), ly0, xp(t), ly1))
        out.append('<text x="%.1f" y="%d" font-family="sans-serif" font-size="10" fill="#666" text-anchor="middle">%d</text>' % (xp(t), ly1 + 14, t))
    pts = []
    for t, v in zip(lag_ms, corr):
        if -200 <= t <= 200:
            pts.append("%.1f,%.1f" % (xp(t), yp(abs(v))))
    out.append('<polyline points="%s" fill="none" stroke="#1f77b4" stroke-width="1.2"/>' % " ".join(pts))
    out.append('<line x1="%.1f" y1="%d" x2="%.1f" y2="%d" stroke="#d62728" stroke-dasharray="4,3"/>' % (xp(peak_lag_ms), ly0, xp(peak_lag_ms), ly1))
    out.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" fill="#d62728">peak lag %.2f ms</text>' % (lx1 - 150, ly0 + 14, peak_lag_ms))
    out.append('<text x="%d" y="%d" font-family="sans-serif" font-size="11" fill="#333" text-anchor="middle">lag re correlation peak (ms)</text>' % ((lx0 + lx1) // 2, h - 12))
    out.append('</svg>')
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(out))


def analyze_one(rec, rec_nch, sweep, tag, mic, cal, outdir, pre_s, expected_onset_s=None, note="",
                timebase=None, mic_role="room_listening", speed_mps=ir_distance.SOUND_SPEED_MPS):
    os.makedirs(outdir, exist_ok=True)
    k, coef, corr = xcorr_find_start(rec, sweep)
    start = max(0, k)
    # band-limited SNR (100-8000 Hz): sweep segment vs pre-onset noise
    seg = rec[start:start + len(sweep)]
    if len(seg) < len(sweep):
        seg = np.pad(seg, (0, len(sweep) - len(seg)))
    s_b = fft_bandpass(seg, 100.0, 8000.0)
    n0 = max(0, start - int(0.5 * FS))
    n1 = max(1, start - int(0.05 * FS))
    nz = rec[n0:n1]
    n_b = fft_bandpass(nz, 100.0, 8000.0) if len(nz) > 100 else np.zeros(100)
    snr = db(np.sqrt(np.mean(s_b ** 2) + 1e-30)) - db(np.sqrt(np.mean(n_b ** 2) + 1e-30))
    seg_dec = rec[start:start + len(sweep) + int(0.9 * FS)]
    if len(seg_dec) < len(sweep) + int(0.3 * FS):
        pad = len(sweep) + int(0.9 * FS) - len(seg_dec)
        seg_dec = np.pad(seg_dec, (0, pad))
    ir = deconvolve(seg_dec, sweep)
    peak_lim = min(len(ir), int(0.10 * FS))
    peak = int(np.argmax(np.abs(ir[:peak_lim])))
    polarity = 1 if ir[peak] >= 0 else -1
    # FR 1/24 (plot) + 1/3 oct (delivery)
    grid24, fr24 = fr_oct(ir, peak, cal=cal, frac=24)
    fr3 = fr_on_grid(ir, peak, NOMINAL_3OCT, cal=cal)
    grid3 = np.array(NOMINAL_3OCT, dtype=np.float64)
    tail = ir[peak: peak + int(0.8 * FS)]
    rt = {str(int(fc)): rt_estimate(tail, fc) for fc in OCT_BANDS}
    csd = csd_matrix(tail)
    # captured-source spectrum (measured) vs source sweep spectrum (expected flat)
    grid_sp, meas_sp = spectrum_third_oct(rec, t0=start / FS, t1=(start + len(sweep)) / FS)
    grid_ss, src_sp = spectrum_third_oct(sweep)
    meas_sp = meas_sp - np.nanmedian(meas_sp[(grid_sp > 200) & (grid_sp < 2000)])
    src_sp = src_sp - np.nanmedian(src_sp[(grid_ss > 200) & (grid_ss < 2000)])
    clip = int(np.sum(np.abs(rec) >= 0.999))
    # xcorr curve (decimated for svg)
    res = {
        "tag": tag, "mic": mic, "note": note,
        "recording_frames": int(len(rec)),
        "xcorr": {
            "start_sample": int(start),
            "onset_ms_in_recording": round(start / FS * 1000.0, 2),
            "expected_onset_ms": None if expected_onset_s is None else round(expected_onset_s * 1000.0, 2),
            "onset_dev_ms": None if expected_onset_s is None else round(start / FS * 1000.0 - expected_onset_s * 1000.0, 2),
            "peak_lag_samples": int(k),
            "peak_lag_ms": round(k / FS * 1000.0, 3),
            "norm_coef": round(coef, 4),
            "bandlimited_snr_db_100_8000": round(snr, 2),
        },
        "ir": {
            "peak_index": int(peak),
            # task-229：段内偏移与时基/距离严格分开；无公共 t0 时 distance 为 INVALID。
            "direct_path": ir_distance.estimate_direct_path(
                {
                    "peak_index": int(peak),
                    "fractional_offset": None,
                    "segment_start_sample": float(start),
                    "sample_rate_hz": float(FS),
                },
                timebase if timebase is not None else ir_distance.recording_only_timebase(),
                mic_role=mic_role,
                speed_mps=speed_mps,
            ),
            "polarity": int(polarity),
            "peak_db_rel": round(db(np.max(np.abs(ir[:peak_lim]))), 2),
            "peak_to_noise_db": round(db(np.max(np.abs(ir[:peak_lim]))) - db(np.median(np.abs(ir[int(0.6 * FS):int(0.8 * FS)]))), 2),
        },
        "levels": {
            "rec_rms_dbfs": round(db(np.sqrt(np.mean(rec ** 2))), 2),
            "rec_peak_dbfs": round(db(np.max(np.abs(rec))), 2),
            "pre_onset_noise_rms_dbfs": round(db(np.sqrt(np.mean(nz ** 2)) + 1e-30), 2) if len(nz) > 0 else None,
            "clipped_samples": clip,
        },
        "fr_third_oct": {"freq_hz": [round(float(f), 1) for f in grid3], "db": [None if d != d else round(float(d), 2) for d in fr3]},
        "rt_octave": rt,
        "csd": {"bands_hz": [round(float(b), 1) for b in CSD_BANDS], "times_ms": [int(t) for t in CSD_T],
                "db_re_band_max": [[round(float(v), 2) for v in row] for row in csd]},
        "source_spectrum_third_oct": {"freq_hz": [round(float(f), 1) for f in grid_ss], "db_rel": [None if d != d else round(float(d), 2) for d in src_sp]},
        "measured_capture_spectrum_third_oct": {"freq_hz": [round(float(f), 1) for f in grid_sp], "db_rel": [None if d != d else round(float(d), 2) for d in meas_sp]},
    }
    # writes: FR csv (1/3 oct + 1/24), RT csv, CSD csv, IR wav, plots
    d = os.path.join(outdir, tag + "-" + mic)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "fr-1_3oct.csv"), "w", encoding="utf-8", newline="") as fh:
        fh.write("freq_hz,db_rel_200_2k\n")
        for f, v in zip(grid3, fr3):
            fh.write("%.3f,%.3f\n" % (f, v))
    with open(os.path.join(d, "fr-1_24oct.csv"), "w", encoding="utf-8", newline="") as fh:
        fh.write("freq_hz,db_rel_200_2k\n")
        for f, v in zip(grid24, fr24):
            fh.write("%.3f,%.3f\n" % (f, v))
    with open(os.path.join(d, "rt-octave.csv"), "w", encoding="utf-8", newline="") as fh:
        fh.write("band_hz,rt60_s,method,edr100_db,noise_floor_edc_db\n")
        for fc in OCT_BANDS:
            r = rt[str(int(fc))]
            fh.write("%d,%s,%s,%s,%s\n" % (int(fc), "" if r["rt60_s"] is None else "%.3f" % r["rt60_s"], r["method"] or "",
                                           r["edr100_db"], r["noise_floor_edc_db"]))
    with open(os.path.join(d, "csd.csv"), "w", encoding="utf-8", newline="") as fh:
        fh.write("band_hz," + ",".join(str(int(t)) for t in CSD_T) + "\n")
        for bi, fc in enumerate(CSD_BANDS):
            fh.write("%.1f," % fc + ",".join("%.2f" % v for v in csd[bi]) + "\n")
    write_wav24_mono(os.path.join(d, "ir-" + mic + ".wav"), ir)
    svg_fr(os.path.join(d, "fr.svg"), "FR (1/24 oct, mic-cal applied) -- " + tag + " / " + mic, grid24, fr24)
    series = [(mic, [rt[str(int(fc))]["rt60_s"] for fc in OCT_BANDS])]
    svg_rt(os.path.join(d, "rt.svg"), "RT60 (Schroeder T20/T15x4) -- " + tag + " / " + mic, list(OCT_BANDS), series)
    svg_csd(os.path.join(d, "csd.svg"), "CSD (1/3 oct envelope) -- " + tag + " / " + mic, [round(float(b), 1) for b in CSD_BANDS], [int(t) for t in CSD_T], csd)
    with open(os.path.join(d, "analysis.json"), "w", encoding="utf-8") as fh:
        json.dump(res, fh, ensure_ascii=False, indent=1)
    return res


def main():
    ap = __import__("argparse").ArgumentParser()
    ap.add_argument("--root", required=True, help="evidence dir")
    ap.add_argument("--out", required=True)
    ap.add_argument("--timebase-source", default="none",
                    choices=("none",) + ir_distance.TIMEBASE_SOURCES,
                    help="公共时基来源；none = 单路录音，禁止输出距离（默认）")
    ap.add_argument("--t0-sample", type=float, default=None,
                    help="公共时基 t0（采集域样本，与 IR onset 同一次采集）；source!=none 时必填")
    ap.add_argument("--mic-role", default="room_listening", choices=ir_distance.MIC_ROLES,
                    help="麦克风角色：房间麦 <0.5 m 距离会被标 SUSPECT（默认 room_listening）")
    ap.add_argument("--speed-of-sound", type=float, default=ir_distance.SOUND_SPEED_MPS,
                    help="声速 m/s（默认 343；可用 331.3+0.606*温度 修正）")
    a = ap.parse_args()
    if a.timebase_source == "none":
        timebase = ir_distance.recording_only_timebase()
    else:
        if a.t0_sample is None:
            ap.error("--timebase-source %s requires --t0-sample (无 t0 拒绝输出距离)" % a.timebase_source)
        timebase = ir_distance.known_timebase(a.timebase_source, a.t0_sample, FS)
    if not ir_distance.speed_of_sound_valid(a.speed_of_sound):
        ap.error("--speed-of-sound %r outside %.0f..%.0f m/s (unit error?)" % (
            a.speed_of_sound, ir_distance.MIN_SPEED_OF_SOUND_MPS, ir_distance.MAX_SPEED_OF_SOUND_MPS))
    root, out = a.root, a.out
    os.makedirs(out, exist_ok=True)
    data = os.path.join(root, "data")
    cal_umik = load_cal(r"C:\WorkSpace\Claw\projects\tspi-audio-dsp\calibration-spec\mics\umik1_718-7696_90deg.txt")
    cal_ma1 = load_cal(r"C:\WorkSpace\Claw\projects\tspi-audio-dsp\calibration-spec\mics\ma1_frequency_response_6086517654_6D7C8E.txt")
    # source for board take: extracted played channel (exact composite ch0)
    comp_path = os.path.join(data, "board", "take-comp-ch00-sweep-10s.raw")
    comp = read_s24(comp_path, 1)[:, 0]
    pre = 1.5
    sweep = comp[int(pre * FS): int(pre * FS) + int(10.0 * FS)]
    summary = {
        "schema": "tspi-sweep-analysis/v2",
        "timebase_policy": {
            "distance_requires_common_timebase": True,
            "default_timebase_source": "none",
            "requested_timebase_source": a.timebase_source,
            "t0_sample": a.t0_sample,
            "mic_role": a.mic_role,
            "speed_of_sound_mps": a.speed_of_sound,
            "note": ("IR 段内偏移 relative_to_segment 不得当绝对直达延迟；无公共 t0 时 distance=INVALID_NO_COMMON_TIMEBASE"),
        },
        "inputs": {},
        "runs": [],
    }
    for p in (comp_path,
              os.path.join(data, "board", "take-umik-2ch-20s.raw"),
              os.path.join(data, "board", "take-MA1-mono-20s.raw"),
              os.path.join(data, "hostfi", "hostfi-umik-2ch-20s.raw")):
        summary["inputs"][os.path.basename(p)] = sha256_file(p)
    summary["inputs"]["sweep_wav"] = sha256_file(os.path.join(root, "sweep-source-48k-2ch-24bit.wav")) if os.path.isfile(os.path.join(root, "sweep-source-48k-2ch-24bit.wav")) else None
    # ---- board take (aplay composite -> MOTU -> room; UMIK + MA1) ----
    umik = read_s24(os.path.join(data, "board", "take-umik-2ch-20s.raw"), 2)[:, 0]
    ma1 = read_s24(os.path.join(data, "board", "take-MA1-mono-20s.raw"), 1)[:, 0]
    expected_onset = 1.8 + pre
    r1 = analyze_one(umik, 2, sweep, "board-aplay", "UMIK", cal_umik, out, pre, expected_onset,
                     timebase=timebase, mic_role=a.mic_role, speed_mps=a.speed_of_sound,
                     note="board aplay 22ch composite ch0 -> MOTU playback -> speakers; UMIK-1 card3 ch0; expected onset = 1.8s arecord lead + 1.5s file pre-silence")
    r2 = analyze_one(ma1, 1, sweep, "board-aplay", "MA1", cal_ma1, out, pre, expected_onset,
                     timebase=timebase, mic_role=a.mic_role, speed_mps=a.speed_of_sound,
                     note="MA1 = MOTU capture ch0 acoustic reference; same take")
    summary["runs"] += [r1, r2]
    # ---- host file-input take (rt-host capture.source=file -> graph -> MOTU; UMIK only) ----
    wav_path = os.path.join(data, "hostfi", "hostfi-source-sweep-48k-2ch.wav")
    if os.path.isfile(wav_path):
        src, _ch, _sr = read_wav_s24_ch0(wav_path)
    else:
        src = None
    if src is not None:
        hsw = src[int(pre * FS): int(pre * FS) + int(10.0 * FS)]
        hostfi = read_s24(os.path.join(data, "hostfi", "hostfi-umik-2ch-20s.raw"), 2)[:, 0]
        r3 = analyze_one(hostfi, 2, hsw, "host-fileinput", "UMIK", cal_umik, out, pre, 1.0 + pre,
                         timebase=timebase, mic_role=a.mic_role, speed_mps=a.speed_of_sound,
                         note="rt-host capture.source=file -> stereo_identity graph -> MOTU playback; UMIK only (host holds MOTU capture). expected onset = >=1.0s arecord lead + host start + 1.5s pre")
        summary["runs"].append(r3)
    with open(os.path.join(out, "analysis-summary.json"), "w", encoding="utf-8") as fh:
        json.dump(summary, fh, ensure_ascii=False, indent=1)
    print("ANALYSIS_DONE")
    for r in summary["runs"]:
        print("%-14s %-5s onset=%8.2f ms lag_coef=%.3f snr=%5.1f dB ir_seg_rel=%6.2f ms pol=%d FR(1k)=%.1f dB RT500=%s distance=%s" % (
            r["tag"], r["mic"], r["xcorr"]["onset_ms_in_recording"], r["xcorr"]["norm_coef"],
            r["xcorr"]["bandlimited_snr_db_100_8000"],
            r["ir"]["direct_path"]["relative_to_segment"]["delay_ms"], r["ir"]["polarity"],
            r["fr_third_oct"]["db"][len(r["fr_third_oct"]["db"]) // 2],
            r["rt_octave"]["500"]["rt60_s"],
            r["ir"]["direct_path"]["distance"]["status"]))


if __name__ == "__main__":
    main()