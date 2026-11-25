#!/usr/bin/env python3
"""gen-fir-wav.py — 生成 CamillaDSP Conv(Raw/TEXT) 用的线性相位 FIR 系数（仅标准库）。

用法:
  python3 gen-fir-wav.py --taps 4096 --rate 48000 --cutoff 2000 --mode lowpass  --out fir_low_48k.txt
  python3 gen-fir-wav.py --taps 4096 --rate 48000 --cutoff 2000 --mode highpass --out fir_high_48k.txt

说明:
- 窗函数法（Blackman-Harris）+ 理想低通/高通；对称（线性相位），群延迟 (taps-1)/2 样本。
- 输出文本每行一个系数，CamillaDSP `Conv parameters.type: Raw`（默认 TEXT 格式）。
- 归一化：低通 DC 增益=1；高通按 Nyquist 增益=1（对同一信号，低+高近似全通）。
"""
import argparse
import math


def ideal_lowpass(n_taps: int, cutoff_ratio: float):
    """cutoff_ratio = fc / fs（0..0.5）。返回 n_taps 个线性相位低通系数（未加窗）。"""
    m = n_taps - 1
    h = []
    for n in range(n_taps):
        k = n - m / 2.0
        if abs(k) < 1e-12:
            h.append(2.0 * cutoff_ratio)
        else:
            h.append(math.sin(2.0 * math.pi * cutoff_ratio * k) / (math.pi * k))
    return h


def blackman_harris(n_taps: int):
    a0, a1, a2, a3 = 0.35875, 0.48829, 0.14128, 0.01168
    m = n_taps - 1
    w = []
    for n in range(n_taps):
        x = 2.0 * math.pi * n / m
        w.append(a0 - a1 * math.cos(x) + a2 * math.cos(2 * x) - a3 * math.cos(3 * x))
    return w


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--taps", type=int, default=4096)
    ap.add_argument("--rate", type=int, default=48000)
    ap.add_argument("--cutoff", type=float, default=2000.0)
    ap.add_argument("--mode", choices=["lowpass", "highpass"], default="lowpass")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    if args.taps < 8 or args.taps % 2 != 0:
        raise SystemExit("--taps 需为 >=8 的偶数（保持整数群延迟）")
    if not (0 < args.cutoff < args.rate / 2.0):
        raise SystemExit("--cutoff 必须在 (0, rate/2) 内")

    ratio = args.cutoff / args.rate
    h = ideal_lowpass(args.taps, ratio)
    w = blackman_harris(args.taps)
    hp = [(-x if i % 2 else x) for i, x in enumerate(h)]  # 频移到 Nyquist => 高通

    win = w
    coeffs = h if args.mode == "lowpass" else hp
    coeffs = [c * wi for c, wi in zip(coeffs, win)]

    # 归一化
    if args.mode == "lowpass":
        dc = sum(coeffs)
        coeffs = [c / dc for c in coeffs]
    else:
        nyq = sum(coeffs[i] * ((-1) ** i) for i in range(len(coeffs)))
        coeffs = [c / nyq for c in coeffs]

    with open(args.out, "w", encoding="utf-8") as f:
        for c in coeffs:
            f.write(f"{c:.17g}\n")
    total = sum(abs(c) for c in coeffs)
    print(f"wrote {args.out}: taps={len(coeffs)} rate={args.rate} cutoff={args.cutoff} "
          f"mode={args.mode} sum|c|={total:.3f}")


if __name__ == "__main__":
    main()
