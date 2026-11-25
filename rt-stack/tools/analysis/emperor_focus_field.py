#!/usr/bin/env python3
"""皇帝位现场测量。不加 --play 时只跑求解自检，不出声。

几分钟内自行完成：逐只喇叭 20–500 Hz 扫频、求解、恒等与修正各复测一次、打印结论。
不调用 agent。生产 soak 占用的声卡不要传给 --play。

示例（环境由调用者准备好之后）：
  python emperor_focus_field.py --self-check
  python emperor_focus_field.py --play hw:Device,0 --rec hw:UMIK,0 \\
      --yes-this-bench --channels 8 --map FL=0,FR=1,SL=4,SR=5,SUB1=7 \\
      --check FL --supports FR,SL
"""

from __future__ import annotations

import argparse
import json
import shutil
import struct
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXAMPLE = ["cargo", "run", "-q", "-p", "tspi-mimo-design", "--example", "emperor_focus", "--"]


def run_example(args: list[str], cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        EXAMPLE + args,
        cwd=cwd,
        text=True,
        capture_output=True,
        check=False,
    )


def self_check(cwd: Path) -> int:
    proc = run_example(["--self-check"], cwd)
    sys.stdout.write(proc.stdout)
    sys.stderr.write(proc.stderr)
    return proc.returncode


def write_s24(path: Path, channels: list[list[float]], amp: float) -> None:
    n = len(channels[0])
    raw = bytearray()
    scale = (2**23 - 1) * amp
    for i in range(n):
        for ch in channels:
            v = 0.0 if i >= len(ch) else ch[i]
            s = int(max(-1.0, min(1.0, v)) * scale)
            raw += struct.pack("<i", s)[:3]
    path.write_bytes(raw)


def read_s24_ch0(path: Path, channels: int) -> list[float]:
    raw = path.read_bytes()
    frame = 3 * channels
    out = []
    scale = float(2**23)
    for i in range(0, len(raw) - frame + 1, frame):
        b = raw[i : i + 3]
        v = int.from_bytes(b, "little", signed=False)
        if v & 0x800000:
            v -= 0x1000000
        out.append(v / scale)
    return out


def play_rec(play: str, rec: str, channels: int, wav_s24: Path, seconds: float, out: Path) -> None:
    rec_proc = subprocess.Popen(
        [
            "arecord", "-D", rec, "-f", "S24_3LE", "-c", "2", "-r", "48000",
            "-t", "raw", "-d", str(int(seconds) + 2), str(out),
        ]
    )
    time.sleep(0.4)
    subprocess.run(
        ["aplay", "-D", play, "-f", "S24_3LE", "-c", str(channels), "-r", "48000", "-t", "raw", str(wav_s24)],
        check=True,
    )
    rec_proc.wait(timeout=seconds + 10)
    if rec_proc.returncode not in (0, None):
        raise RuntimeError(f"arecord 退出 {rec_proc.returncode}")


def f32_to_py(path: Path) -> list[float]:
    raw = path.read_bytes()
    return list(struct.unpack("<" + "f" * (len(raw) // 4), raw))


def field(args: argparse.Namespace) -> int:
    if not args.yes_this_bench:
        print("拒绝出声：加上 --yes-this-bench，并且不要把正在 soak 的生产声卡传进来。", file=sys.stderr)
        return 2
    if shutil.which("aplay") is None or shutil.which("arecord") is None:
        print("这台机器没有 aplay/arecord。", file=sys.stderr)
        return 2
    mapping = dict(part.split("=", 1) for part in args.map.split(",") if part)
    if args.check not in mapping:
        print(f"--check {args.check} 不在 --map 里", file=sys.stderr)
        return 2
    work = Path(args.work)
    work.mkdir(parents=True, exist_ok=True)
    cwd = ROOT
    sweep_path = work / "sweep.f32"
    proc = run_example(["sweep", "--seconds", str(args.seconds), "--out", str(sweep_path)], cwd)
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        return proc.returncode
    sweep = f32_to_py(sweep_path)
    names = list(mapping)
    irs = {}
    for name in names:
        buf = [([0.0] * len(sweep)) for _ in range(args.channels)]
        buf[int(mapping[name])] = sweep
        stim = work / f"stim_{name}.s24"
        rec = work / f"rec_{name}.s24"
        write_s24(stim, buf, args.level)
        play_rec(args.play, args.rec, args.channels, stim, args.seconds + args.tail, rec)
        rec_f = work / f"rec_{name}.f32"
        samples = read_s24_ch0(rec, 2)
        rec_f.write_bytes(struct.pack("<" + "f" * len(samples), *samples))
        ir = work / f"ir_{name}.f32"
        proc = run_example(["deconv", "--rec", str(rec_f), "--sweep", str(sweep_path), "--out", str(ir)], cwd)
        if proc.returncode != 0:
            sys.stderr.write(proc.stderr)
            return proc.returncode
        irs[name] = ir
    supports = [s for s in args.supports.split(",") if s]
    for s in supports:
        if s not in irs or s == args.check:
            print(f"从喇叭 {s} 不可用", file=sys.stderr)
            return 2
    out_dir = work / "design"
    cmd = ["solve", "--fs", "48000", "--f-hi", str(args.f_hi), "--primary", str(irs[args.check]), "--out-dir", str(out_dir)]
    for s in supports:
        cmd += ["--support", str(irs[s])]
    proc = run_example(cmd, cwd)
    sys.stdout.write(proc.stdout)
    sys.stderr.write(proc.stderr)
    report = json.loads((out_dir / "report.json").read_text(encoding="utf-8"))
    if proc.returncode != 0 or report.get("claim") == "REFUSED":
        print("FIELD_REFUSED")
        return proc.returncode or 2
    # 恒等：只有主喇叭播扫频。修正：主喇叭播渲染的第 0 路，从喇叭播后面的路。
    g_primary = out_dir / "g_primary.f32"
    render_cmd = ["render", "--sweep", str(sweep_path), "--g-primary", str(g_primary), "--out", str(work / "play.f32")]
    for i, _s in enumerate(supports):
        render_cmd += ["--g-support", str(out_dir / f"g_support_{i}.f32")]
    proc = run_example(render_cmd, cwd)
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        return proc.returncode
    rendered = f32_to_py(work / "play.f32")
    n_ch_render = 1 + len(supports)
    frames = len(rendered) // n_ch_render
    split = [rendered[i * frames : (i + 1) * frames] for i in range(n_ch_render)]

    def record_named(tag: str, by_name: dict[str, list[float]]) -> Path:
        buf = [[0.0] * frames for _ in range(args.channels)]
        for name, data in by_name.items():
            buf[int(mapping[name])] = data
        stim = work / f"stim_{tag}.s24"
        rec = work / f"rec_{tag}.s24"
        write_s24(stim, buf, args.level)
        play_rec(args.play, args.rec, args.channels, stim, frames / 48000 + 1.0, rec)
        return rec

    ident = {args.check: sweep + [0.0] * (frames - len(sweep))}
    corr = {args.check: split[0]}
    for i, s in enumerate(supports):
        corr[s] = split[i + 1]
    rec_ident = record_named("ident", ident)
    rec_corr = record_named("corr", corr)

    def ir_of(tag: str, rec: Path) -> Path:
        rec_f = work / f"rec_{tag}.f32"
        samples = read_s24_ch0(rec, 2)
        rec_f.write_bytes(struct.pack("<" + "f" * len(samples), *[float(s) for s in samples]))
        ir = work / f"ir_{tag}_remeasure.f32"
        proc = run_example(["deconv", "--rec", str(rec_f), "--sweep", str(sweep_path), "--out", str(ir)], cwd)
        if proc.returncode != 0:
            sys.stderr.write(proc.stderr)
            raise RuntimeError(proc.stderr)
        return ir

    ir_ident = ir_of("ident", rec_ident)
    ir_corr = ir_of("corr", rec_corr)
    score_i = run_example(["score", "--fs", "48000", "--ir", str(ir_ident)], cwd)
    score_c = run_example(["score", "--fs", "48000", "--ir", str(ir_corr)], cwd)
    sys.stdout.write(score_i.stdout)
    sys.stdout.write(score_c.stdout)
    def seconds_of(proc: subprocess.CompletedProcess[str]) -> tuple[str, float] | None:
        for line in proc.stdout.splitlines():
            if line.startswith("SCORE_OK"):
                parts = dict(p.split("=", 1) for p in line.split()[1:])
                return parts["metric"], float(parts["seconds"])
        return None
    got_i = seconds_of(score_i)
    got_c = seconds_of(score_c)
    if got_i is None or got_c is None or got_i[0] != got_c[0]:
        print("FIELD_UNFALSIFIABLE")
        print(score_i.stderr)
        print(score_c.stderr)
        return 3
    metric, a, b = got_i[0], got_i[1], got_c[1]
    verdict = "FIELD_SHORTER" if b < a else "FIELD_NOT_SHORTER"
    print(f"{verdict} metric={metric} identity_s={a:.4f} corrected_s={b:.4f}")
    summary = {
        "verdict": verdict,
        "metric": metric,
        "identity_s": a,
        "corrected_s": b,
        "design": report,
        "note": "predicted_tail_ratio 不是声学通过。engine_playable 只说明现有侧路抽头上限够不够。",
    }
    (work / "field_summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"summary={work / 'field_summary.json'}")
    return 0 if verdict == "FIELD_SHORTER" else 4


def main() -> int:
    parser = argparse.ArgumentParser(description="皇帝位现场测量脚本")
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--play", default="")
    parser.add_argument("--rec", default="")
    parser.add_argument("--yes-this-bench", action="store_true")
    parser.add_argument("--channels", type=int, default=8)
    parser.add_argument("--map", default="FL=0,FR=1,SL=4,SR=5,SUB1=7")
    parser.add_argument("--check", default="FL")
    parser.add_argument("--supports", default="")
    parser.add_argument("--f-hi", type=float, default=150.0)
    parser.add_argument("--seconds", type=float, default=4.0)
    parser.add_argument("--tail", type=float, default=2.5)
    parser.add_argument("--level", type=float, default=0.05)
    parser.add_argument("--work", default="")
    args = parser.parse_args()
    if args.play:
        if not args.rec or not args.yes_this_bench:
            print("拒绝出声：--play 必须同时带 --rec 和 --yes-this-bench。", file=sys.stderr)
            return 2
        if not args.work:
            args.work = str(ROOT / "evidence" / "emperor-focus-field")
        return field(args)
    return self_check(ROOT)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except subprocess.CalledProcessError as exc:
        print(exc, file=sys.stderr)
        raise SystemExit(exc.returncode)
