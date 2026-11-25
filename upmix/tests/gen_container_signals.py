# -*- coding: utf-8 -*-
"""生成 H16 容器（7.1.4）测试信号：只有 2.0 内容 / 真 7.1.4 / 只有中置 / 静音。

用法：python3 tests/gen_container_signals.py --out DIR [--fs 48000] [--seconds 8]
依赖 numpy + scipy（仅开发机需要；板端测试直接用生成好的 WAV）。
"""
import argparse
import os

import numpy as np
from scipy.io import wavfile

ROLES = ["FL", "FR", "C", "LFE", "SL", "SR", "BL", "BR", "TFL", "TFR", "TRL", "TRR"]
CH = {r: i for i, r in enumerate(ROLES)}


def pink(seed, n, fs):
    rng = np.random.default_rng(seed)
    x = rng.standard_normal(n)
    b = np.cumsum(x)
    k = max(1, int(fs * 0.01))
    b = b - np.convolve(b, np.ones(k) / k, mode="same")
    return b / max(np.max(np.abs(b)), 1e-9) * 0.05


def write(path, data, fs):
    wavfile.write(path, fs, np.asarray(data, dtype=np.float32))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--fs", type=int, default=48000)
    ap.add_argument("--seconds", type=float, default=8.0)
    args = ap.parse_args()
    fs = args.fs
    n = int(fs * args.seconds)
    t = np.arange(n) / fs
    os.makedirs(args.out, exist_ok=True)

    # 1) 7.1.4 容器但只有 2.0 内容（FL/FR 粉噪，其余静音）
    d = np.zeros((n, 12))
    d[:, CH["FL"]] = pink(7, n, fs)
    d[:, CH["FR"]] = np.roll(pink(8, n, fs), 100)
    write(os.path.join(args.out, "container20.wav"), d, fs)

    # 2) 真 7.1.4：每声道不同频率单音（便于验证映射）
    freqs = {"FL": 100, "FR": 110, "C": 200, "LFE": 60, "SL": 500, "SR": 510,
             "BL": 600, "BR": 610, "TFL": 300, "TFR": 310, "TRL": 400, "TRR": 410}
    d = np.zeros((n, 12))
    for r, f in freqs.items():
        d[:, CH[r]] = 0.05 * np.sin(2 * np.pi * f * t)
    write(os.path.join(args.out, "container714.wav"), d, fs)

    # 3) 只有中置能量（用于验证 stereo 模式下直通路径被静音）
    d = np.zeros((n, 12))
    d[:, CH["C"]] = 0.05 * np.sin(2 * np.pi * 200 * t)
    write(os.path.join(args.out, "container_c_only.wav"), d, fs)

    # 4) 静音容器
    write(os.path.join(args.out, "container_silence.wav"), np.zeros((n, 12)), fs)

    print("已生成：", ", ".join(sorted(os.listdir(args.out))))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
