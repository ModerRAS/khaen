#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""板端计时/实时因子测试（RK3576，文件后端；stdlib + PyYAML，不需要 numpy）。

在板端运行：
  python3 board_time.py --workdir /tmp/upmix-test/run \
      --signals /tmp/upmix-test/signals \
      --tspi-upmix /tmp/upmix-test/tspi-upmix \
      --layout-presets /etc/tspi-audio/layout-presets.yml \
      --camilladsp /usr/local/bin/camilladsp \
      --presets upmix-4.1.2-music,upmix-5.3.2-music \
      --chunksizes 128,256,512,1024,2048

产物：<workdir>/board-results.json（analyze.py 会自动读取并写入报告）
"""
import argparse
import json
import os
import platform
import resource
import subprocess
import sys
import time


def audio_seconds(path):
    with open(path, "rb") as f:
        data = f.read(128)
    if data[:4] != b"RIFF":
        raise ValueError("not wav: %s" % path)
    pos, fmt = 12, None
    while pos + 8 <= len(data):
        cid = data[pos:pos + 4]
        size = int.from_bytes(data[pos + 4:pos + 8], "little")
        if cid == b"fmt ":
            fmt = data[pos + 8:pos + 8 + size]
            break
        pos += 8 + size + (size & 1)
    if not fmt:
        raise ValueError("no fmt: %s" % path)
    channels = int.from_bytes(fmt[2:4], "little")
    rate = int.from_bytes(fmt[4:8], "little")
    bits = int.from_bytes(fmt[14:16], "little")
    total = os.path.getsize(path) - 44
    return (total // max(1, channels * max(8, bits) // 8)) / float(rate)


def run(cmd, timeout=1800):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    t0 = time.perf_counter()
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    wall = time.perf_counter() - t0
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    return p, wall, (r1.ru_utime - r0.ru_utime), (r1.ru_stime - r0.ru_stime), r1.ru_maxrss


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--signals", required=True)
    ap.add_argument("--tspi-upmix", required=True)
    ap.add_argument("--layout-presets", default="/etc/tspi-audio/layout-presets.yml")
    ap.add_argument("--camilladsp", default="/usr/local/bin/camilladsp")
    ap.add_argument("--presets", default="upmix-4.1.2-music,upmix-5.3.2-music")
    ap.add_argument("--chunksizes", default="128,256,512,1024,2048")
    ap.add_argument("--signal", default="noise_pink_stereo.wav")
    ap.add_argument("--fs", type=int, default=48000)
    args = ap.parse_args()

    workdir = os.path.abspath(args.workdir)
    outdir = os.path.join(workdir, "board-out")
    cfgdir = os.path.join(workdir, "board-cfg")
    os.makedirs(outdir, exist_ok=True)
    os.makedirs(cfgdir, exist_ok=True)
    sig = os.path.join(args.signals, args.signal)
    if not os.path.isfile(sig):
        print("信号不存在：%s" % sig, file=sys.stderr)
        return 2
    dur = audio_seconds(sig)
    presets = [p.strip() for p in args.presets.split(",") if p.strip()]
    chunksizes = [int(x) for x in args.chunksizes.split(",") if x.strip()]
    upmix = [sys.executable, os.path.abspath(args.tspi_upmix),
             "--layout-presets", args.layout_presets]

    env = []
    for path in ("/proc/device-tree/model", "/proc/version"):
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as f:
                env.append(f.read().strip("\x00").strip())
        except OSError:
            pass
    try:
        p, _, _, _, _ = run([args.camilladsp, "--version"])
        env.append((p.stdout or p.stderr).strip().splitlines()[0])
    except Exception:  # noqa: BLE001
        pass

    entries = []
    for preset in presets:
        # ALSA 配置 --check（不打开设备，仅结构与取值校验）
        alsa_cfg = os.path.join(cfgdir, preset + ".alsa.yml")
        p, wall, u, s, _ = run(upmix + ["camilla", preset, "-o", alsa_cfg,
                                        "--fs", str(args.fs)])
        chk = {"preset": preset, "alsa_check": None, "alsa_check_msg": ""}
        if p.returncode == 0:
            q, _, _, _, _ = run([args.camilladsp, "--check", alsa_cfg])
            chk["alsa_check"] = (q.returncode == 0)
            chk["alsa_check_msg"] = ((q.stderr or q.stdout).strip().splitlines() or [""])[-1][:300]
        else:
            chk["alsa_check_msg"] = "生成 ALSA 配置失败：%s" % (p.stderr or p.stdout)[:300]
        entries.append(chk)

        for cs in chunksizes:
            outp = os.path.join(outdir, "%s_cs%d.wav" % (preset, cs))
            cfg = os.path.join(cfgdir, "%s_cs%d.yml" % (preset, cs))
            p, _, _, _, _ = run(upmix + ["file-config", preset, "-o", cfg,
                                         "--input", sig, "--out-wav", outp,
                                         "--fs", str(args.fs), "--chunksize", str(cs)])
            if p.returncode != 0:
                entries.append({"preset": preset, "chunksize": cs,
                                "error": "生成配置失败：%s" % (p.stderr or p.stdout)[:300]})
                continue
            q, wall, u, s, rss = run([args.camilladsp, "-l", "warn", cfg])
            ok = q.returncode == 0
            entries.append({
                "preset": preset, "chunksize": cs, "ok": ok,
                "audio_seconds": round(dur, 3),
                "wall_seconds": round(wall, 4),
                "cpu_user": round(u, 4), "cpu_sys": round(s, 4),
                "cpu_total": round(u + s, 4),
                "rtf": round(dur / wall, 2) if wall > 0 else None,
                "maxrss_kb": rss,
                "output_bytes": os.path.getsize(outp) if os.path.isfile(outp) else 0,
                "error": "" if ok else (q.stderr or q.stdout)[-500:],
            })
            print("%-22s cs=%-5d wall=%.3fs cpu=%.3fs rtf=%s" % (
                preset, cs, wall, u + s, entries[-1]["rtf"]))

    result = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "env": " | ".join(env),
        "machine": platform.machine(),
        "python": platform.python_version(),
        "entries": entries,
    }
    path = os.path.join(workdir, "board-results.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump(result, f, ensure_ascii=False, indent=2)
    print("板端结果：%s" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
