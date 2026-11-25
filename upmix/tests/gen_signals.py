#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""生成 2.0 上混测试信号（48k/2ch/F32 WAV）。

依赖：numpy + scipy（开发机；板端不跑本脚本，信号由开发机生成后 scp）。
用法：
    python3 gen_signals.py --out tests/signals [--fs 48000]

信号清单（文件名）：
  noise_pink_stereo     左右独立粉噪（-20 dBFS RMS）      20 s
  noise_pink_mono       左右相同粉噪（中心）              10 s
  noise_pink_antiphase  右=-左（纯差信号）                10 s
  sweep_stereo          20-20k 对数扫频（同相）           10 s
  sweep_antiphase       20-20k 对数扫频（反相）           10 s
  tone_center_1k        1kHz 同相（-12 dBFS peak）        3 s
  tone_left_1k          1kHz 仅左
  tone_right_1k         1kHz 仅右
  tone_antiphase_1k     1kHz 反相
  sine_40hz_center      40Hz 同相（低音管理）
  sine_100hz_center     100Hz 同相
  impulse               双声道脉冲（0.5s 处）             2 s
  music_proxy           合成"音乐"（中心低音/旋律 + 左右打击乐）15 s
"""
import argparse
import os

import numpy as np
from scipy.io import wavfile


def _pink(n, rng):
    white = rng.standard_normal(n)
    spec = np.fft.rfft(white)
    freqs = np.fft.rfftfreq(n)
    scale = np.ones_like(freqs)
    scale[1:] = 1.0 / np.sqrt(freqs[1:])
    scale[0] = 0.0
    pink = np.fft.irfft(spec * scale, n)
    pink /= max(np.sqrt(np.mean(pink ** 2)), 1e-12)
    return pink


def _sweep(n, fs, f0=20.0, f1=20000.0):
    t = np.arange(n) / fs
    dur = n / fs
    k = np.log(f1 / f0) / dur
    phase = 2 * np.pi * f0 * (np.exp(k * t) - 1.0) / k
    return np.sin(phase)


def _write(path, fs, left, right):
    data = np.stack([left, right], axis=1).astype(np.float32)
    wavfile.write(path, fs, data)


def _scaled(x, peak_dbfs):
    peak = np.max(np.abs(x)) or 1.0
    return x * (10 ** (peak_dbfs / 20.0) / peak)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="tests/signals")
    ap.add_argument("--fs", type=int, default=48000)
    args = ap.parse_args()
    fs = args.fs
    os.makedirs(args.out, exist_ok=True)
    rng = np.random.default_rng(20261115)

    def nsec(sec):
        return int(round(sec * fs))

    # ---- 粉噪 ----
    n = nsec(20)
    l = _pink(n, rng)
    r = _pink(n, rng)
    g = 10 ** (-20.0 / 20.0) / np.sqrt(np.mean(np.stack([l, r]) ** 2))
    _write(os.path.join(args.out, "noise_pink_stereo.wav"), fs, l * g, r * g)

    n = nsec(10)
    m = _pink(n, rng)
    m = m / np.sqrt(np.mean(m ** 2)) * 10 ** (-20.0 / 20.0)
    _write(os.path.join(args.out, "noise_pink_mono.wav"), fs, m, m)
    _write(os.path.join(args.out, "noise_pink_antiphase.wav"), fs, m, -m)

    # ---- 扫频 ----
    n = nsec(10)
    sw = _scaled(_sweep(n, fs), -6.0)
    _write(os.path.join(args.out, "sweep_stereo.wav"), fs, sw, sw)
    _write(os.path.join(args.out, "sweep_antiphase.wav"), fs, sw, -sw)

    # ---- 单音 ----
    def tone(sec, freq):
        t = np.arange(nsec(sec)) / fs
        return _scaled(np.sin(2 * np.pi * freq * t), -12.0)

    for name, (lchan, rchan) in {
        "tone_center_1k": (tone(3, 1000), tone(3, 1000)),
        "tone_left_1k": (tone(3, 1000), np.zeros(nsec(3))),
        "tone_right_1k": (np.zeros(nsec(3)), tone(3, 1000)),
        "tone_antiphase_1k": (tone(3, 1000), -tone(3, 1000)),
        "sine_40hz_center": (tone(3, 40), tone(3, 40)),
        "sine_100hz_center": (tone(3, 100), tone(3, 100)),
    }.items():
        _write(os.path.join(args.out, name + ".wav"), fs, lchan, rchan)

    # ---- 脉冲 ----
    n = nsec(2)
    imp = np.zeros(n)
    imp[int(0.5 * fs)] = 0.5
    _write(os.path.join(args.out, "impulse.wav"), fs, imp, imp.copy())

    # ---- 合成音乐 ----
    n = nsec(15)
    t = np.arange(n) / fs
    center = np.zeros(n)
    # 低音：小节根音 55/65/73/82 Hz
    roots = [55.0, 65.4, 73.4, 82.4]
    for i in range(15):
        f = roots[i % 4]
        seg = (t >= i) & (t < i + 1)
        env = np.minimum(1.0, (t - i) / 0.02) * np.exp(-(t - i) / 0.9)
        center += seg * env * np.sin(2 * np.pi * f * t) * 0.5
    # 中心旋律：五声音阶
    for i in range(15):
        f = 440.0 * 2 ** ((i % 5) / 12.0)
        seg = (t >= i) & (t < i + 0.8)
        env = np.minimum(1.0, (t - i) / 0.01) * np.exp(-(t - i) / 0.25)
        center += seg * env * (np.sin(2 * np.pi * f * t) + 0.3 * np.sin(4 * np.pi * f * t)) * 0.22
    # 侧向打击乐：左右不同带限噪声脉冲
    left_side = np.zeros(n)
    right_side = np.zeros(n)
    hat = _pink(n, rng) * 0.3
    for k in range(int(15 / 0.25)):
        start = int(k * 0.25 * fs)
        ln = int(0.03 * fs)
        env = np.exp(-np.arange(ln) / (0.008 * fs))
        noise = rng.standard_normal(ln) * env
        if k % 2 == 0:
            left_side[start:start + ln] += noise * 0.25
        else:
            right_side[start:start + ln] += noise * 0.25
    left = center * 0.6 + left_side + hat * 0.03
    right = center * 0.6 + right_side + np.roll(hat, 173) * 0.03
    peak = max(np.max(np.abs(left)), np.max(np.abs(right))) or 1.0
    scale = 10 ** (-9.0 / 20.0) / peak
    _write(os.path.join(args.out, "music_proxy.wav"), fs, left * scale, right * scale)

    names = sorted(f for f in os.listdir(args.out) if f.endswith(".wav"))
    print("已生成 %d 个信号 -> %s" % (len(names), os.path.abspath(args.out)))
    for name in names:
        print("  " + name)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
