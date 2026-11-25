#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""用 CamillaDSP 文件后端跑上混配置，产出各通道输出 WAV + 计时（stdlib + PyYAML）。

可在开发机跑（Windows/Linux），也可直接 scp 到板端跑（板端只需 python3+PyYAML+camilladsp）。
分析交给 analyze.py（需要 numpy）。

用法示例：
  python3 run_file_backend.py \
    --workdir ../scratch/upmix-run --signals tests/signals \
    --camilladsp /usr/local/bin/camilladsp \
    --khaen-upmix ./khaen-upmix --layout-presets /etc/khaen-audio/layout-presets.yml \
    --channelmap /usr/local/bin/khaen-channelmap \
    --variants standalone,renderer --presets upmix-4.1.2-music,upmix-5.3.2-music

产物：
  <workdir>/plans/<preset>.json          解析后的 DSP 计划（供 analyze 仿真）
  <workdir>/configs/<variant>/<preset>/<signal>.yml
  <workdir>/out/<variant>/<preset>/<signal>.wav
  <workdir>/results.json                 计时/返回码/RTF
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import time

try:
    import yaml
except ImportError:  # pragma: no cover
    sys.stderr.write("需要 PyYAML（Debian 包 python3-yaml）\n")
    sys.exit(2)

HERE = os.path.dirname(os.path.abspath(__file__))


def die(msg):
    sys.stderr.write("run_file_backend: %s\n" % msg)
    sys.exit(2)


def run(cmd, timeout=600, cwd=None):
    t0 = time.perf_counter()
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, cwd=cwd,
                          encoding="utf-8", errors="replace")
    return p, time.perf_counter() - t0


def audio_seconds(path):
    """解析 WAV 头（float32/int PCM 都支持；不使用 wave 模块，因其不支持 format=3）。"""
    with open(path, "rb") as f:
        data = f.read(128)
    if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise ValueError("不是 WAV：%s" % path)
    pos = 12
    fmt = None
    while pos + 8 <= len(data):
        cid = data[pos:pos + 4]
        size = int.from_bytes(data[pos + 4:pos + 8], "little")
        if cid == b"fmt ":
            fmt = data[pos + 8:pos + 8 + size]
            break
        pos += 8 + size + (size & 1)
    if fmt is None or len(fmt) < 16:
        raise ValueError("WAV 缺少 fmt chunk：%s" % path)
    channels = int.from_bytes(fmt[2:4], "little")
    rate = int.from_bytes(fmt[4:8], "little")
    bits = int.from_bytes(fmt[14:16], "little")
    # data 长度可能在后面的块里；用文件剩余大小估算（CamillaDSP 流式头是 dummy 长度，输入文件是完整的）
    total = os.path.getsize(path) - 44
    frame_bytes = max(1, channels * max(8, bits) // 8)
    frames = max(0, total // frame_bytes)
    return frames / float(rate)


def patch_devices_to_file(cfg_path, input_wav, output_wav, fs, chunksize):
    with open(cfg_path, "r", encoding="utf-8") as f:
        cfg = yaml.safe_load(f)
    ch = int(cfg["devices"]["playback"]["channels"])
    cfg["devices"] = {
        "samplerate": int(fs), "chunksize": int(chunksize),
        "capture": {"type": "WavFile", "filename": input_wav.replace("\\", "/")},
        "playback": {"type": "File", "channels": ch,
                     "filename": output_wav.replace("\\", "/"),
                     "format": "F32_LE", "wav_header": True},
    }
    tmp = cfg_path + ".file.yml"
    with open(tmp, "w", encoding="utf-8", newline="\n") as f:
        yaml.safe_dump(cfg, f, sort_keys=False, allow_unicode=True)
    return tmp


def list_signals(sigdir, patterns=None):
    names = sorted(f for f in os.listdir(sigdir) if f.endswith(".wav"))
    if patterns:
        import fnmatch
        names = [n for n in names if any(fnmatch.fnmatch(n, p) for p in patterns)]
    return names


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--signals", required=True)
    ap.add_argument("--camilladsp", default="camilladsp")
    ap.add_argument("--khaen-upmix", default=os.path.join(HERE, "..", "khaen-upmix"))
    ap.add_argument("--layout-presets", default="/etc/khaen-audio/layout-presets.yml")
    ap.add_argument("--presets-file", default=None, help="upmix 预设 YAML（默认用 khaen-upmix 内置查找）")
    ap.add_argument("--channelmap", default="khaen-channelmap", help="khaen-channelmap（renderer 变体用）")
    ap.add_argument("--variants", default="standalone")
    ap.add_argument("--presets", default="upmix-4.1.2-music,upmix-4.1.2-movie,upmix-5.3.2-music,upmix-5.3.2-movie")
    ap.add_argument("--signals-filter", default=None, help="逗号分隔通配，如 '*tone*,*noise*'")
    ap.add_argument("--fs", type=int, default=48000)
    ap.add_argument("--chunksize", type=int, default=1024)
    ap.add_argument("--channelmap-python", default=sys.executable)
    args = ap.parse_args()

    workdir = os.path.abspath(args.workdir)
    sigdir = os.path.abspath(args.signals)
    for sub in ("plans", "configs", "out", "logs"):
        os.makedirs(os.path.join(workdir, sub), exist_ok=True)
    if not os.path.isdir(sigdir):
        die("信号目录不存在：%s（先跑 gen_signals.py）" % sigdir)
    camilladsp = shutil.which(args.camilladsp) or args.camilladsp
    if not (os.path.isfile(camilladsp) or shutil.which(camilladsp)):
        die("camilladsp 不存在：%s" % camilladsp)
    upmix = os.path.abspath(args.khaen_upmix)
    if not os.path.isfile(upmix):
        die("khaen-upmix 不存在：%s" % upmix)

    variants = [v.strip() for v in args.variants.split(",") if v.strip()]
    presets = [p.strip() for p in args.presets.split(",") if p.strip()]
    signals = list_signals(sigdir, [p.strip() for p in args.signals_filter.split(",")]
                           if args.signals_filter else None)
    if not signals:
        die("信号目录为空")

    base = [sys.executable, upmix]
    if args.presets_file:
        base += ["--presets", os.path.abspath(args.presets_file)]
    base += ["--layout-presets", args.layout_presets]

    results = {"workdir": workdir, "fs": args.fs, "chunksize": args.chunksize,
               "camilladsp": camilladsp, "signals": signals, "variants": variants,
               "presets": presets, "entries": []}

    channelmap_cmd = args.channelmap
    if not os.path.isfile(channelmap_cmd) and not shutil.which(channelmap_cmd):
        die("renderer 变体需要 khaen-channelmap：%s" % channelmap_cmd)

    for preset in presets:
        # 1) 解析计划（生成器 show --json）
        p, dt = run(base + ["show", preset, "--json", "--fs", str(args.fs)])
        if p.returncode != 0:
            die("生成计划失败 %s：%s" % (preset, (p.stderr or p.stdout)[:500]))
        plan = json.loads(p.stdout[p.stdout.find("{"):])
        plan_path = os.path.join(workdir, "plans", preset + ".json")
        with open(plan_path, "w", encoding="utf-8") as f:
            json.dump(plan, f, ensure_ascii=False, indent=2)
        nout = int(plan["output_channels"])
        print("[plan] %-22s %s -> %dch" % (preset, plan["layout"], nout))

        profile_path = os.path.join(workdir, "profiles", preset + ".yml")
        if "renderer" in variants:
            os.makedirs(os.path.dirname(profile_path), exist_ok=True)
            p, _ = run(base + ["profile", preset, "-o", profile_path, "--fs", str(args.fs)])
            if p.returncode != 0:
                die("生成 profile 失败：%s" % (p.stderr or p.stdout)[:500])

        for sig in signals:
            inp = os.path.join(sigdir, sig)
            dur = audio_seconds(inp)
            for variant in variants:
                outdir = os.path.join(workdir, "out", variant, preset)
                cfgdir = os.path.join(workdir, "configs", variant, preset)
                os.makedirs(outdir, exist_ok=True)
                os.makedirs(cfgdir, exist_ok=True)
                base_name = os.path.splitext(sig)[0]
                outp = os.path.join(outdir, base_name + ".wav")
                cfgp = os.path.join(cfgdir, base_name + ".yml")
                entry = {"variant": variant, "preset": preset, "signal": sig,
                         "input": inp, "output": outp, "config": cfgp,
                         "layout": plan["layout"], "channels": nout, "fs": args.fs,
                         "audio_seconds": dur}
                try:
                    if variant == "standalone":
                        p, dt = run(base + ["file-config", preset, "-o", cfgp,
                                            "--input", inp, "--out-wav", outp,
                                            "--fs", str(args.fs),
                                            "--chunksize", str(args.chunksize)])
                        if p.returncode != 0:
                            entry.update(error="生成配置失败：%s" % (p.stderr or p.stdout)[:500])
                            results["entries"].append(entry)
                            continue
                        run_cfg = cfgp
                    elif variant == "renderer":
                        cmd = [args.channelmap_python, channelmap_cmd, "render", profile_path,
                               "--presets", args.layout_presets,
                               "--samplerate", str(args.fs),
                               "--capture-channels", "2",
                               "--playback-channels", str(nout),
                               "--capture-device", "dummy-capture",
                               "--playback-device", "dummy-playback",
                               "-o", cfgp, "--no-check"]
                        p, dt = run(cmd)
                        if p.returncode != 0:
                            entry.update(error="渲染 profile 失败：%s" % (p.stderr or p.stdout)[:500])
                            results["entries"].append(entry)
                            continue
                        run_cfg = patch_devices_to_file(cfgp, inp, outp, args.fs,
                                                        args.chunksize)
                    else:
                        die("未知 variant：%s（支持 standalone,renderer）" % variant)
                    p, dt = run([camilladsp, "-l", "warn", run_cfg])
                    ok = p.returncode == 0 and os.path.isfile(outp)
                    entry.update(rc=p.returncode, wall_seconds=round(dt, 4),
                                 rtf=round(dur / dt, 2) if dt > 0 else None,
                                 output_bytes=os.path.getsize(outp) if os.path.isfile(outp) else 0,
                                 ok=ok)
                    if not ok:
                        entry["error"] = (p.stderr or p.stdout)[-800:]
                    logp = os.path.join(workdir, "logs",
                                        "%s_%s_%s.log" % (variant, preset, base_name))
                    with open(logp, "w", encoding="utf-8") as f:
                        f.write("cmd: %r\n\n%s\n%s" % ([camilladsp, "-l", "warn", run_cfg],
                                                       p.stdout or "", p.stderr or ""))
                    print("  [%s] %-10s %-22s rtf=%s wall=%.2fs" %
                          (variant, base_name, preset,
                           entry.get("rtf"), entry.get("wall_seconds", 0)))
                except Exception as e:  # noqa: BLE001
                    entry.update(error="异常：%s" % e)
                results["entries"].append(entry)

    res_path = os.path.join(workdir, "results.json")
    with open(res_path, "w", encoding="utf-8") as f:
        json.dump(results, f, ensure_ascii=False, indent=2)
    ok = sum(1 for e in results["entries"] if e.get("ok"))
    print("\n完成：%d/%d 项成功 -> %s" % (ok, len(results["entries"]), res_path))
    return 0 if ok == len(results["entries"]) else 1


if __name__ == "__main__":
    sys.exit(main())
