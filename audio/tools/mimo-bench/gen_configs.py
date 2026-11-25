#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""生成 CamillaDSP N×M 全矩阵 FIR 配置 + 系数 WAV（x86 原型 benchmark 用）。
结构：Mixer(split N -> N*M 路径) -> Filter(每路径 Conv) -> Mixer(sum N*M -> M)
用法：python3 gen_configs.py --out DIR --n 12 --m 10 --taps 2048 --fs 48000 --chunksize 64 [--bandlimit]
"""
import argparse, os, wave, struct, math


def write_wav(path, taps, fs, lp_hz=None):
    if lp_hz:
        # 简单窗函数低通（sinc*hann），用于 band-limited 路径
        fc = lp_hz / fs
        w = []
        for i in range(taps):
            n = i - (taps - 1) / 2.0
            s = 2 * fc if n == 0 else math.sin(2 * math.pi * fc * n) / (math.pi * n)
            win = 0.5 - 0.5 * math.cos(2 * math.pi * i / (taps - 1)) if taps > 1 else 1.0
            w.append(s * win)
        norm = sum(w) or 1.0
        data = [int(max(-1.0, min(1.0, x / norm)) * 32767) for x in w]
    else:
        data = [0] * taps
        if taps:
            data[0] = 32767  # 脉冲（延迟基准）
    with wave.open(path, "wb") as f:
        f.setnchannels(1)
        f.setsampwidth(2)
        f.setframerate(fs)
        f.writeframes(b"".join(struct.pack("<h", x) for x in data))


def gen(out, n, m, taps, fs, chunksize, bandlimit=False):
    os.makedirs(out, exist_ok=True)
    coeff = os.path.join(out, "coeff_%d.wav" % taps)
    write_wav(coeff, taps, fs, 300.0 if bandlimit else None)
    npaths = n * m
    filters = {}
    for i in range(n):
        for j in range(m):
            filters["f_%d_%d" % (i, j)] = {
                "type": "Conv", "parameters": {"type": "Wav", "filename": coeff}}
    split = {"channels": {"in": n, "out": npaths}, "mapping": []}
    for i in range(n):
        for j in range(m):
            split["mapping"].append({"dest": i * m + j, "sources": [{"channel": i}]})
    summ = {"channels": {"in": npaths, "out": m}, "mapping": []}
    for j in range(m):
        summ["mapping"].append({"dest": j, "sources": [{"channel": i * m + j} for i in range(n)]})
    cfg = {
        "devices": {
            "samplerate": fs, "chunksize": chunksize, "target_level": chunksize * 4,
            "adjust_period": 10, "enable_rate_adjust": False, "queuelimit": 4,
            "capture": {"type": "Alsa", "channels": n, "device": "hw:LoopbackA,1,0", "format": "S32_LE"},
            "playback": {"type": "Alsa", "channels": m, "device": "hw:LoopbackB,0,0", "format": "S32_LE"},
        },
        "filters": filters,
        "mixers": {"split": split, "sum": summ},
        "pipeline": [
            {"type": "Mixer", "name": "split"},
            {"type": "Filter", "description": "N x M FIR paths",
             "channels": list(range(npaths)), "names": [f"f_{i}_{j}" for i in range(n) for j in range(m)]},
            {"type": "Mixer", "name": "sum"},
        ],
    }
    import yaml
    cpath = os.path.join(out, "camilla_%dx%d_%d_%d.yml" % (n, m, taps, fs))
    with open(cpath, "w", encoding="utf-8") as f:
        yaml.safe_dump(cfg, f, sort_keys=False, allow_unicode=True)
    return cpath


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--n", type=int, required=True)
    ap.add_argument("--m", type=int, required=True)
    ap.add_argument("--taps", type=int, required=True)
    ap.add_argument("--fs", type=int, default=48000)
    ap.add_argument("--chunksize", type=int, default=64)
    ap.add_argument("--bandlimit", action="store_true")
    a = ap.parse_args()
    print(gen(a.out, a.n, a.m, a.taps, a.fs, a.chunksize, a.bandlimit))
