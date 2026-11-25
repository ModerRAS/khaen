#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""task-19 输入内容模式自测（纯 stdlib，可板端运行）。

覆盖：
  1) detect --wav：H16 容器（7.1.4）里的 2.0 / 多声道 / 静音 / 仅中置判定
  2) input-mode get/set：别名归一化、override 优先级、持久化文件
  3) autodetect --simulate：默认直通、迟滞切换（stereo/multichannel）
  4) （给了 --camilladsp 时）双路径 CamillaDSP 文件后端：
     stereo 模式 == 2ch 上混；pass 模式通道映射；C-only 在 stereo 模式静音；
     2.0 容器在 pass 模式只有 FL/FR 有能量

用法：
  python3 tests/test_input_mode.py --khaen-upmix ./khaen-upmix \
      --layout-presets /etc/khaen-audio/layout-presets.yml \
      --signals /tmp/container-signals [--camilladsp /usr/local/bin/camilladsp] \
      [--workdir /tmp/upmix-input-mode] [--preset upmix-4.1.2-music]
"""
import argparse
import json
import math
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import time

ROLES = ["FL", "FR", "C", "LFE", "SL", "SR", "BL", "BR", "TFL", "TFR", "TRL", "TRR"]
FREQS = {"FL": 100.0, "FR": 110.0, "C": 200.0, "LFE": 60.0, "SL": 500.0, "SR": 510.0,
         "BL": 600.0, "BR": 610.0, "TFL": 300.0, "TFR": 310.0, "TRL": 400.0, "TRR": 410.0}

PASS = 0
FAIL = 0


def check(cond, name, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print("  PASS  " + name)
    else:
        FAIL += 1
        print("  FAIL  " + name + (("  [" + str(detail) + "]") if detail else ""))


def run(cmd, timeout=180):
    r = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                       timeout=timeout, text=True, encoding="utf-8", errors="replace")
    return r.returncode, r.stdout, r.stderr


def read_wav(path):
    """返回 (fs, channels, [每通道样本])；支持 float32 / int16。"""
    with open(path, "rb") as f:
        head = f.read(12)
        if head[:4] not in (b"RIFF", b"WAVE"):
            raise ValueError("not wav: " + path)
        fmt = None
        data = None
        while True:
            hdr = f.read(8)
            if len(hdr) < 8:
                break
            cid, size = hdr[:4], struct.unpack("<I", hdr[4:])[0]
            if cid == b"fmt ":
                b = f.read(size)
                fmt = struct.unpack("<HHIIHH", b[:16])
                if size & 1:
                    f.seek(1, 1)
                continue
            if cid == b"data":
                # CamillaDSP 文件后端流式写 WAV，data size 可能是 0xFFFFFFFF 占位：
                # 按文件实际剩余字节截断，避免按 4GB 分配内存。
                off = f.tell()
                remain = max(0, os.path.getsize(path) - off)
                n = size if (size < 0xFFFFFF00 and size <= remain) else remain
                data = f.read(n)
                break
            f.seek(size + (size & 1), 1)
    code, channels, rate, _, _, bits = fmt
    if code == 3 and bits == 32:
        vals = list(struct.unpack("<%df" % (len(data) // 4), data))
    elif code == 1 and bits == 16:
        vals = [v / 32768.0 for v in struct.unpack("<%dh" % (len(data) // 2), data)]
    else:
        raise ValueError("unsupported wav fmt=%s bits=%s" % (code, bits))
    ch = list(vals[c::channels] for c in range(channels))
    return rate, channels, ch


def rms(x):
    return math.sqrt(sum(v * v for v in x) / max(1, len(x)))


def db(x):
    return 20.0 * math.log10(max(x, 1e-12))


def goertzel(x, fs, freq, start=0.5, seconds=2.0):
    n0 = int(start * fs)
    n = min(len(x) - n0, int(seconds * fs))
    if n <= 0:
        return 0.0
    w = 2.0 * math.pi * freq / fs
    c = 2.0 * math.cos(w)
    s1 = s2 = 0.0
    for i in range(n0, n0 + n):
        s0 = x[i] + c * s1 - s2
        s2, s1 = s1, s0
    return math.sqrt(max(s1 * s1 + s2 * s2 - c * s1 * s2, 0.0)) / n


def parse_json_output(out):
    i = out.find("{")
    return json.loads(out[i:]) if i >= 0 else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--khaen-upmix", required=True)
    ap.add_argument("--layout-presets", required=True)
    ap.add_argument("--signals", required=True)
    ap.add_argument("--camilladsp", default=None)
    ap.add_argument("--workdir", default=None)
    ap.add_argument("--preset", default="upmix-4.1.2-music")
    ap.add_argument("--input-layout", default="h16-7.1.4")
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()
    py = sys.executable
    up = args.khaen_upmix
    signals = args.signals
    work = args.workdir or tempfile.mkdtemp(prefix="upmix-inputmode-")
    os.makedirs(work, exist_ok=True)
    base = [py, up, "--layout-presets", args.layout_presets]

    print("== 1) detect --wav ==")
    cases = [("container20.wav", "stereo", False, "fl-fr-only"),
             ("container714.wav", "multichannel", True, "secondary-active"),
             ("container_c_only.wav", "multichannel", True, "secondary-active"),
             ("container_silence.wav", "unknown", None, "no-signal")]
    for fname, want, want_h16, want_reason in cases:
        path = os.path.join(signals, fname)
        if not os.path.isfile(path):
            check(False, "signal exists: " + fname, path)
            continue
        rc, out, err = run(base + ["detect", "--wav", path, "--input-layout", args.input_layout])
        doc = parse_json_output(out) if rc == 0 else None
        check(rc == 0 and doc is not None, "detect rc: " + fname, err[-200:])
        if not doc:
            continue
        check(doc.get("detected") == want, "detected=%s (%s)" % (want, fname), doc.get("detected"))
        check(doc.get("h16_internal_upmix") == want_h16, "h16_internal_upmix (%s)" % fname,
              doc.get("h16_internal_upmix"))
        check(doc.get("reason") == want_reason, "reason=%s (%s)" % (want_reason, fname),
              doc.get("reason"))
        check(doc.get("schema") == "khaen-input-content/v1", "state schema")
        check("rms_db" in doc and "threshold_db" in doc and "content_mode" in doc,
              "state 字段齐全 (%s)" % fname)
        check("container_layout" in doc and doc.get("container_layout") == args.input_layout,
              "container_layout (%s)" % fname, doc.get("container_layout"))

    print("== 2) input-mode get/set（别名与 override）==")
    modef = os.path.join(work, "input-mode.json")
    statef = os.path.join(work, "input-content.json")
    for p in (modef, statef):
        if os.path.isfile(p):
            os.remove(p)
    rc, out, err = run(base + ["input-mode", "get", "--mode-file", modef, "--state", statef])
    doc = parse_json_output(out) if rc == 0 else None
    check(rc == 0 and doc and doc.get("override") is None and doc.get("content_mode") == "auto",
          "get 无覆盖文件 → override=null/content_mode=auto", out[-200:])
    rc, out, err = run(base + ["input-mode", "set", "force-stereo", "--mode-file", modef,
                               "--state", statef])
    doc = parse_json_output(out) if rc == 0 else None
    check(rc == 0 and doc and doc.get("override") == "stereo" and doc.get("content_mode") == "stereo",
          "set force-stereo → 归一化 stereo", out[-200:])
    try:
        saved = json.load(open(modef, encoding="utf-8"))
    except Exception:
        saved = {}
    check(saved.get("mode") == "stereo" and saved.get("schema") == "khaen-input-mode/v1",
          "覆盖文件内容", saved)
    rc, out, err = run(base + ["input-mode", "set", "force-multichannel", "--mode-file", modef,
                               "--state", statef])
    doc = parse_json_output(out) if rc == 0 else None
    check(rc == 0 and doc and doc.get("override") == "multichannel" and doc.get("detector") == "manual",
          "set force-multichannel → 归一化 multichannel", out[-200:])
    rc, out, err = run(base + ["input-mode", "set", "auto", "--mode-file", modef, "--state", statef])
    doc = parse_json_output(out) if rc == 0 else None
    check(rc == 0 and doc and doc.get("override") == "auto", "set auto", out[-200:])

    print("== 3) autodetect --simulate（默认直通 + 迟滞）==")
    frames = []
    for _ in range(4):
        frames.append({"rms_db": dict({r: -120.0 for r in ROLES}, FL=-20.0, FR=-20.0)})
    for _ in range(4):
        frames.append({"rms_db": dict({r: -120.0 for r in ROLES}, FL=-20.0, FR=-20.0, C=-18.0)})
    for _ in range(4):
        frames.append({"rms_db": dict({r: -120.0 for r in ROLES}, FL=-20.0, FR=-20.0)})
    simf = os.path.join(work, "simframes.json")
    with open(simf, "w", encoding="utf-8") as f:
        json.dump({"frames": frames}, f)
    if os.path.isfile(modef):
        os.remove(modef)
    rc, out, err = run(base + ["autodetect", "--simulate", simf, "--input-layout", args.input_layout,
                               "--hold-ms", "200", "--interval-ms", "100", "--startup-freeze-ms", "0",
                               "--dry-run", "--no-write",
                               "--state", statef, "--mode-file", modef])
    lines = [json.loads(ln) for ln in out.splitlines() if ln.strip().startswith("{")]
    modes = [ln.get("content_mode") for ln in lines]
    check(rc == 0 and len(lines) == len(frames), "simulate 输出行数", len(lines))
    check(modes and modes[0] == "multichannel", "auto 默认直通（首帧 multichannel）", modes[:2])
    check("stereo" in modes, "检测到 stereo 时切上混", modes)
    check("multichannel" in modes[modes.index("stereo"):] if "stereo" in modes else False,
          "多声道内容切回直通", modes)
    check(any(ln.get("reason") == "fl-fr-only" for ln in lines), "reason fl-fr-only")
    check(any(ln.get("reason") == "secondary-active" for ln in lines), "reason secondary-active")
    # LFE 不作判据：2.0 + LFE（H16 旧固件）仍应判 stereo
    frames_lfe = [{"rms_db": dict({r: -120.0 for r in ROLES}, FL=-20.0, FR=-20.0, LFE=-18.0)}
                  for _ in range(8)]
    simf2 = os.path.join(work, "simframes-lfe.json")
    with open(simf2, "w", encoding="utf-8") as f:
        json.dump({"frames": frames_lfe}, f)
    rc4, out4, err4 = run(base + ["autodetect", "--simulate", simf2, "--input-layout",
                                  args.input_layout, "--hold-ms", "200", "--interval-ms", "100",
                                  "--startup-freeze-ms", "0", "--dry-run", "--no-write",
                                  "--state", statef, "--mode-file", modef])
    lines4 = [json.loads(ln) for ln in out4.splitlines() if ln.strip().startswith("{")]
    check(all(ln.get("detected") == "stereo" for ln in lines4),
          "LFE 不作判据（2.0+LFE 判 stereo）", [l.get("detected") for l in lines4][:3])
    check(any(ln.get("content_mode") == "stereo" for ln in lines4), "2.0+LFE 最终切上混")
    # 启动冻结：默认 2500ms 内保持直通
    if os.path.isfile(modef):
        os.remove(modef)
    rc5, out5, err5 = run(base + ["autodetect", "--simulate", simf, "--input-layout",
                                  args.input_layout, "--hold-ms", "200", "--interval-ms", "100",
                                  "--dry-run", "--no-write", "--state", statef, "--mode-file", modef])
    lines5 = [json.loads(ln) for ln in out5.splitlines() if ln.strip().startswith("{")]
    check(all(ln.get("content_mode") == "multichannel" for ln in lines5[:5]),
          "启动冻结期间保持直通", [l.get("content_mode") for l in lines5][:5])
    check(any(ln.get("reason") == "startup-freeze" for ln in lines5), "reason startup-freeze")
    # override 强制直通：所有帧 manual
    rc2, out2, err2 = run(base + ["input-mode", "set", "multichannel", "--mode-file", modef,
                                  "--state", statef])
    rc3, out3, err3 = run(base + ["autodetect", "--simulate", simf, "--input-layout",
                                  args.input_layout, "--hold-ms", "200", "--interval-ms", "100",
                                  "--startup-freeze-ms", "0",
                                  "--dry-run", "--no-write", "--state", statef, "--mode-file", modef])
    lines3 = [json.loads(ln) for ln in out3.splitlines() if ln.strip().startswith("{")]
    check(all(ln.get("content_mode") == "multichannel" and ln.get("detector") == "manual"
              for ln in lines3), "override=multichannel 优先于检测", [l.get("content_mode") for l in lines3][:3])

    if not args.camilladsp:
        print("\n（跳过双路径文件后端：未给 --camilladsp）")
    else:
        print("== 4) 双路径 CamillaDSP 文件后端 ==")
        cdsp = args.camilladsp
        # 4.1.2 输出 8ch
        pb = 8
        sig20 = os.path.join(signals, "container20.wav")
        sig714 = os.path.join(signals, "container714.wav")
        sigc = os.path.join(signals, "container_c_only.wav")
        # 参考：把 container20 前 2ch 转 2ch WAV 跑普通 file-config
        fs, chans, ch = read_wav(sig20)
        w2 = os.path.join(work, "container20-2ch.wav")
        with open(w2, "wb") as f:
            data = b"".join(struct.pack("<ff", ch[0][i], ch[1][i]) for i in range(len(ch[0])))
            f.write(b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVEfmt ")
            f.write(struct.pack("<IHHIIHH", 16, 3, 2, fs, fs * 8, 8, 32))
            f.write(b"data" + struct.pack("<I", len(data)) + data)

        def gen_cfg(mode, inwav, outwav):
            cfg = os.path.join(work, "cfg-%s-%s.yml" % (mode, os.path.basename(outwav)))
            rc, out, err = run(base + ["autodetect-config", args.preset, "-o", cfg,
                                       "--input-layout", args.input_layout, "--input", inwav,
                                       "--out-wav", outwav, "--playback-channels", str(pb),
                                       "--content-mode", mode])
            return cfg if rc == 0 else None

        def run_cdsp(cfg, gain1, gain2):
            return run([cdsp, "-l", "warn", "--gain1=%s" % gain1, "--gain2=%s" % gain2, cfg])

        ref_cfg = os.path.join(work, "ref-2ch.yml")
        ref_wav = os.path.join(work, "ref-2ch.wav")
        rc, out, err = run(base + ["file-config", args.preset, "-o", ref_cfg, "--input", w2,
                                   "--out-wav", ref_wav])
        ok_ref = rc == 0 and run([cdsp, "-l", "warn", ref_cfg])[0] == 0
        check(ok_ref, "参考 2ch 上混配置运行", err[-200:])

        # A) stereo 模式 == 2ch 上混
        outa = os.path.join(work, "out-stereo.wav")
        cfga = gen_cfg("auto", sig20, outa)
        oka = cfga and run_cdsp(cfga, 0, -120)[0] == 0
        check(bool(oka), "stereo 模式运行")
        if oka and ok_ref:
            fa, ca, ya = read_wav(outa)
            fb, cb, yb = read_wav(ref_wav)
            n = min(min(len(x) for x in ya), min(len(x) for x in yb))
            diffs = [max(abs(ya[c][i] - yb[c][i]) for i in range(n)) for c in range(min(ca, cb))]
            check(max(diffs) < 1e-5, "stereo 模式输出 == 2ch 上混（逐样本）", max(diffs))
        # B) pass 模式：多声道容器按角色映射
        outb = os.path.join(work, "out-pass.wav")
        cfgb = gen_cfg("auto", sig714, outb)
        okb = cfgb and run_cdsp(cfgb, -120, 0)[0] == 0
        check(bool(okb), "pass 模式运行")
        if okb:
            fb2, cb2, yb2 = read_wav(outb)
            out_roles = ["FL", "FR", "SL", "SR", "HFL", "HFR", "unused", "SUB1"]
            src_of = {"FL": "FL", "FR": "FR", "SL": "SL", "SR": "SR",
                      "HFL": "TFL", "HFR": "TFR", "SUB1": "LFE"}
            for ch_i, role in enumerate(out_roles):
                if role == "unused":
                    check(rms(yb2[ch_i]) < 1e-4, "pass 模式 unused 通道静音")
                    continue
                src = src_of[role]
                f = FREQS[src]
                mag = goertzel(yb2[ch_i], fb2, f)
                others = [goertzel(yb2[j], fb2, f) for j in range(cb2) if j != ch_i]
                check(mag > 10 * max(others or [1e-9]) and mag > 1e-4,
                      "pass 映射 %s <- %s (%g Hz)" % (role, src, f),
                      "mag=%.3g others=%.3g" % (mag, max(others or [1e-9])))
        # C) C-only 容器 + stereo 模式 → 静音（直通路径被 Aux2 静音）
        outc = os.path.join(work, "out-c-stereo.wav")
        cfgc = gen_cfg("auto", sigc, outc)
        okc = cfgc and run_cdsp(cfgc, 0, -120)[0] == 0
        if okc:
            fc, cc, yc = read_wav(outc)
            check(max(rms(x) for x in yc) < 1e-4, "C-only + stereo 模式输出静音",
                  max(rms(x) for x in yc))
        # D) 2.0 容器 + pass 模式 → 只有 FL/FR
        outd = os.path.join(work, "out-20-pass.wav")
        cfgd = gen_cfg("auto", sig20, outd)
        okd = cfgd and run_cdsp(cfgd, -120, 0)[0] == 0
        if okd:
            fd, cd, yd = read_wav(outd)
            quiet = [i for i in (2, 3, 4, 5) if rms(yd[i]) < 1e-3]
            check(len(quiet) == 4, "2.0 容器 + pass 模式：SL/SR/HFL/HFR 静音",
                  [db(rms(yd[i])) for i in (2, 3, 4, 5)])

    print("\n结果：PASS=%d FAIL=%d" % (PASS, FAIL))
    if FAIL == 0 and not args.keep:
        shutil.rmtree(work, ignore_errors=True)
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
