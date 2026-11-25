#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""估算 MIMO FIR 全矩阵展开的算力/延迟（含 CamillaDSP N×M 展开与自定义引擎对照）。
用法：python3 estimate.py [--md] > table.md
口径：直接卷积 2*N*fs FLOP/s；CamillaDSP 实际用 FFT 卷积（长 FIR 显著更低），
这里同时给 direct MAC 上界与 FFT 估算（overlap-save，3×FFT/块）。
群延迟：线性相位 ≈ (taps/2)/fs；最小相位/短 FIR 另见文档。
"""
import argparse

CASES = [  # (名称, N输入, M输出)
    ("12x10(H16 7.1.4 实有源)", 12, 10),
    ("16x10(全容器 worst)", 16, 10),
    ("2x10(PC 立体声)", 2, 10),
]
TAPS = [512, 1024, 2048, 4096, 8192]
FS = [48000, 96000]
# 参考：用户给出的 120 路径 @48k 数值（GFLOP/s）
REF = {512: 5.9, 1024: 11.8, 2048: 23.6, 4096: 47.0, 8192: 94.0}


def direct_gflops(npaths, taps, fs):
    return 2.0 * npaths * taps * fs / 1e9


def fft_gflops(npaths, taps, fs):
    import math
    # overlap-save: FFT size M=2*taps；3 次变换 + 频域乘加；/hop taps
    m = 2 * taps
    ops_per_sample_per_path = (3 * 5.0 * m * math.log2(m) + 6.0 * m) / taps
    return npaths * ops_per_sample_per_path * fs / 1e9


def latency_ms(taps, fs, chunksize):
    fir_group = (taps / 2.0) / fs * 1000.0
    block = chunksize / float(fs) * 1000.0
    return fir_group, block


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--md", action="store_true")
    a = ap.parse_args()
    rows = []
    for name, n, m in CASES:
        npaths = n * m
        for fs in FS:
            for taps in TAPS:
                d = direct_gflops(npaths, taps, fs)
                f = fft_gflops(npaths, taps, fs)
                gd, blk = latency_ms(taps, fs, 64)
                rows.append((name, n, m, npaths, fs, taps, d, f, gd, blk))
    if a.md:
        print("| 矩阵 | 路径 | fs | taps | 直接卷积 GFLOP/s | FFT 卷积估算 GFLOP/s | 线性相位群延迟 ms | 64 帧块延迟 ms |")
        print("| --- | --- | --- | --- | --- | --- | --- | --- |")
        for r in rows:
            print("| %s | %d | %d | %d | %.1f | %.2f | %.1f | %.2f |" % (
                r[0], r[3], r[4], r[5], r[6], r[7], r[8], r[9]))
    else:
        for r in rows:
            print("%s npaths=%d fs=%d taps=%d direct=%.1fG fft=%.2fG groupdel=%.1fms" % (
                r[0], r[3], r[4], r[5], r[6], r[7], r[8]))
    print()
    print("参考对照（Lead 口径 120 路径 @48k，直接 MAC）：" + ", ".join(
        "%d taps≈%.1f GFLOP/s" % (t, REF[t]) for t in TAPS))
    print("说明：CamillaDSP 用 FFT 卷积；上表 FFT 估算为 overlap-save 量级，实测以 x86/板上为准。")
    print("RT 余量/32 帧取舍：32 帧仅压缩 DSP 块延迟；长 FIR 群延迟是独立项（2048@48k≈21ms、4096≈43ms）。")


if __name__ == "__main__":
    main()
