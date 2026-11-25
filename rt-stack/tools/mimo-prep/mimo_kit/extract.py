#!/usr/bin/env python3
"""Fixed MIMO planrt arm extractor -- drop-in replacement for the 2026-11-19 kit
planrt_extract.py (board side; stdlib only).

Usage (board, same positional convention as the old script):
  python3 planrt_extract.py <work> <ev> <arm> [render.raw]
      --backend "aplay-direct-alsa" --level-db -12.58 --spl-target-db 75
      --session-meta /dev/shm/planrt/session.json
      [--session-input-sha SHA] [--file-sha SHA]

Outputs <ev>/<arm>/:
  capture.raw      UMIK 2ch S24_3LE (copy of <work>/capture-umik.raw)
  MA1-mono.raw     MOTU capture ch0 mono S24_3LE (byte-exact, see s24.py)
  ARC-mono.raw     MOTU capture ch1 mono S24_3LE
  playback.json    arm/provenance marker (full v2 contract)
  meta/playback.json  same marker (fixes R3: analyze reads meta/playback.json)
  meta/session.json   normalised role-major schedule (if --session-meta given)
  extract.json     extraction summary (frames, sha256, levels)

Bug fixed (task-249 / route-a R1): the old byte mapping read
  mono[0::3] = data[ch::60]; mono[1::3] = data[ch+20::60]; mono[2::3] = data[ch+40::60]
which mixes channels' bytes and yields the -42.14 dBFS LSB-full-scale artifact.
Correct S24_3LE mapping is byte offsets 3*ch, 3*ch+1, 3*ch+2 (s24.extract_channel_file).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

try:  # package import (PC tests / tooling)
    from . import s24  # type: ignore
except Exception:  # script import (board: planrt_extract.py + s24.py in same dir)
    import s24  # type: ignore

KIT_VERSION = "mimo_kit/1.0.0"


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _role_order(meta):
    if isinstance(meta.get("roles"), list) and meta["roles"]:
        return list(meta["roles"])
    if isinstance(meta.get("role_slots"), dict) and meta["role_slots"]:
        return list(meta["role_slots"].keys())
    return ["FL", "FR", "SL", "SR", "SUB1"]


def _starts_matrix(meta, roles):
    """Return (starts list-of-rows role-major, repeats) from either meta flavour."""
    repeats = int((meta.get("schedule") or {}).get("repeats") or meta.get("repeats") or 3)
    raw = meta.get("starts")
    if isinstance(raw, list) and raw and isinstance(raw[0], dict):
        rows = []
        for role in roles:
            row = [int(s["start_frame"]) for s in raw
                   if s.get("role") == role]
            row.sort()
            if len(row) != repeats:
                raise ValueError("starts for role %s has %d entries, expected %d"
                                 % (role, len(row), repeats))
            rows.append(row)
        return rows
    if isinstance(raw, list) and raw and isinstance(raw[0], list):
        return [[int(v) for v in row] for row in raw]
    sched = meta.get("schedule") or {}
    if isinstance(sched.get("starts"), list):
        return [[int(v) for v in row] for row in sched["starts"]]
    raise ValueError("session meta has no usable 'starts'")


def normalise_session_meta(meta, source_path=None):
    """Normalise v1 (planrt_session.py / 08:30) session meta to a v2 role-major doc."""
    roles = _role_order(meta)
    role_slots = meta.get("role_slots") or {"FL": 0, "FR": 1, "SL": 2, "SR": 3, "SUB1": 7}
    role_slots = {str(k): int(v) for k, v in role_slots.items()}
    starts = _starts_matrix(meta, roles)
    sched_in = dict(meta.get("schedule") or {})
    out = {
        "schema": "khaen-planrt-session/v2",
        "kit": KIT_VERSION,
        "fs": int(meta.get("fs", 48000)),
        "channels": int(meta.get("channels", 8)),
        "container": meta.get("container", "4.1.2"),
        "roles": roles,
        "role_slots": role_slots,
        "starts": starts,
        "schedule": {
            "sweep_seconds": float(sched_in.get("sweep_seconds", meta.get("sweep_seconds", 10.0))),
            "repeats": int(sched_in.get("repeats", len(starts[0]) if starts else 3)),
            "pre": float(sched_in.get("pre", meta.get("pre", 1.5))),
            "repeat_gap": float(sched_in.get("repeat_gap", meta.get("repeat_gap", 1.0))),
            "slot_gap": float(sched_in.get("slot_gap", meta.get("slot_gap", 1.0))),
        },
        "total_frames": int(meta.get("total_frames", starts[-1][-1] if starts else 0)),
    }
    if "sweep" in meta:
        out["sweep"] = meta["sweep"]
    out["source"] = {"path": source_path, "sha256": sha256_file(source_path) if source_path else None}
    return out


def extract_arm(work, ev, arm, render=None, *, backend="aplay-direct-alsa",
                level_db=-30.0, spl_target_db=75.0, session_meta=None,
                file_sha=None, session_input_sha=None, motu_channels=20,
                ma1_ch=0, arc_ch=1, copy_umik=True, stats=True):
    out = os.path.join(ev, arm)
    os.makedirs(out, exist_ok=True)
    os.makedirs(os.path.join(out, "meta"), exist_ok=True)
    umik = os.path.join(work, "capture-umik.raw")
    motu = os.path.join(work, "capture-motu.raw")
    if not os.path.isfile(motu):
        raise IOError("missing MOTU capture %s" % motu)
    if not os.path.isfile(umik):
        raise IOError("missing UMIK capture %s" % umik)

    result = {"schema": "khaen-planrt-extract/v2", "kit": KIT_VERSION, "arm": arm, "out": out,
              "motu_channels": motu_channels, "ma1_ch": ma1_ch, "arc_ch": arc_ch}
    dst_umik = os.path.join(out, "capture.raw")
    if os.path.islink(dst_umik) or os.path.exists(dst_umik):
        os.remove(dst_umik)
    if copy_umik:
        import shutil
        shutil.copyfile(umik, dst_umik)
    result["umik"] = {"path": umik, "bytes": os.path.getsize(umik),
                      "sha256": sha256_file(umik)}
    motu_bytes = os.path.getsize(motu)
    frames = motu_bytes // s24.frame_bytes(motu_channels)
    result["motu"] = {"path": motu, "bytes": motu_bytes, "frames": frames,
                      "sha256": sha256_file(motu)}
    result["ma1"] = {"ch": ma1_ch, "path": os.path.join(out, "MA1-mono.raw"),
                     "frames": s24.extract_channel_file(motu, os.path.join(out, "MA1-mono.raw"),
                                                        motu_channels, ma1_ch)}
    result["arc"] = {"ch": arc_ch, "path": os.path.join(out, "ARC-mono.raw"),
                     "frames": s24.extract_channel_file(motu, os.path.join(out, "ARC-mono.raw"),
                                                        motu_channels, arc_ch)}
    for key in ("ma1", "arc"):
        p = result[key]["path"]
        result[key]["bytes"] = os.path.getsize(p)
        result[key]["sha256"] = sha256_file(p)
        if stats:
            with open(p, "rb") as fh:
                result[key]["level"] = s24.channel_rms_peak_dbfs(fh.read(), limit_frames=48000)

    if render and os.path.isfile(render):
        render_path, render_sha = os.path.abspath(render), sha256_file(render)
    else:
        render_path, render_sha = render, None
    if file_sha is None:
        file_sha = session_input_sha or render_sha
    session_meta_out = None
    if session_meta and os.path.isfile(session_meta):
        norm = normalise_session_meta(json.load(open(session_meta, "r", encoding="utf-8")),
                                      source_path=session_meta)
        session_meta_out = os.path.join(out, "meta", "session.json")
        with open(session_meta_out, "w", encoding="utf-8") as fh:
            json.dump(norm, fh, indent=1)
            fh.write("\n")
        result["session_meta"] = {"path": session_meta_out, "total_frames": norm["total_frames"],
                                  "roles": norm["roles"], "role_slots": norm["role_slots"]}

    marker = {
        "schema": "khaen-planrt-playback/v2",
        "kit": KIT_VERSION,
        "arm": arm,
        "path": render_path or "aplay -D hw:UltraLitemk5,0 (8ch render expanded to 22ch S24_3LE 48k) -> MOTU line outs",
        "backend": backend,
        "playback": "aplay -D hw:UltraLitemk5,0 -c 22 -t raw -f S24_3LE -r 48000",
        "level_db": float(level_db),
        "spl_target_db": float(spl_target_db),
        "file_sha256": file_sha,
        "session_input_sha256": session_input_sha,
        "render_raw": render_path,
        "render_sha256": render_sha,
        "umik_capture": {"path": umik, "bytes": os.path.getsize(umik), "sha256": result["umik"]["sha256"]},
        "motu_capture": {"path": motu, "bytes": motu_bytes, "frames": frames,
                         "sha256": result["motu"]["sha256"], "channels": motu_channels},
        "mic_map": {"MA1": "MOTU capture ch%d" % ma1_ch, "ARC": "MOTU capture ch%d" % arc_ch,
                    "UMIK": "hw:3,0 ch0"},
        "channel_map": {"container": "S24_3LE %dch interleaved" % motu_channels,
                        "byte_rule": "sample c at byte offsets 3c, 3c+1, 3c+2 (LSB,MID,MSB)",
                        "ma1_ch": ma1_ch, "arc_ch": arc_ch},
        "extract": {"file": os.path.abspath(__file__), "sha256": sha256_file(os.path.abspath(__file__)),
                    "version": KIT_VERSION},
        "note": "captures in /dev/shm; mono extracts + JSON written to evidence dir",
    }
    for p in (os.path.join(out, "playback.json"), os.path.join(out, "meta", "playback.json")):
        with open(p, "w", encoding="utf-8") as fh:
            json.dump(marker, fh, indent=1)
            fh.write("\n")
    result["playback_marker"] = {"path": os.path.join(out, "playback.json"),
                                 "meta_path": os.path.join(out, "meta", "playback.json")}
    with open(os.path.join(out, "extract.json"), "w", encoding="utf-8") as fh:
        json.dump(result, fh, indent=1)
        fh.write("\n")
    return result


def selfcheck():
    """On-board stdlib-only byte-exactness check (no numpy needed)."""
    import random
    rng = random.Random(20261119)
    chans = 20
    n = 256
    src = [[rng.randint(s24.S24_MIN, s24.S24_MAX) for _ in range(n)] for _ in range(chans)]
    raw = s24.interleave_channels([b"".join(s24.int24_to_bytes(v) for v in ch) for ch in src])
    ok = True
    for c in (0, 1, 7, 19):
        got = s24.extract_channel_bytes(raw, chans, c)
        exp = b"".join(s24.int24_to_bytes(v) for v in src[c])
        if bytes(got) != exp:
            ok = False
    # buggy mapping must differ (regression fingerprint)
    buggy = bytearray(3 * n)
    for k, off in enumerate((0, 20, 40)):
        buggy[k::3] = raw[off::60]
    if bytes(buggy) == bytes(s24.extract_channel_bytes(raw, chans, 0)):
        ok = False
    print(json.dumps({"selfcheck": "PASS" if ok else "FAIL", "channels": chans, "frames": n}))
    return 0 if ok else 1


def main(argv=None):
    ap = argparse.ArgumentParser(description="fixed planrt arm extractor (mimo_kit)")
    ap.add_argument("work", nargs="?", help="work dir with capture-umik.raw/capture-motu.raw")
    ap.add_argument("ev", nargs="?")
    ap.add_argument("arm", nargs="?")
    ap.add_argument("render", nargs="?", default=None)
    ap.add_argument("--backend", default="aplay-direct-alsa")
    ap.add_argument("--level-db", type=float, default=-30.0)
    ap.add_argument("--spl-target-db", type=float, default=75.0)
    ap.add_argument("--session-meta", default=None)
    ap.add_argument("--session-input-sha", default=None)
    ap.add_argument("--file-sha", default=None)
    ap.add_argument("--motu-channels", type=int, default=20)
    ap.add_argument("--ma1-ch", type=int, default=0)
    ap.add_argument("--arc-ch", type=int, default=1)
    ap.add_argument("--no-copy-umik", action="store_true")
    ap.add_argument("--selfcheck", action="store_true")
    args = ap.parse_args(argv)
    if args.selfcheck:
        return selfcheck()
    if not (args.work and args.ev and args.arm):
        ap.error("need <work> <ev> <arm>")
    res = extract_arm(args.work, args.ev, args.arm, args.render,
                      backend=args.backend, level_db=args.level_db,
                      spl_target_db=args.spl_target_db, session_meta=args.session_meta,
                      file_sha=args.file_sha, session_input_sha=args.session_input_sha,
                      motu_channels=args.motu_channels, ma1_ch=args.ma1_ch,
                      arc_ch=args.arc_ch, copy_umik=not args.no_copy_umik)
    print(json.dumps({"arm": res["arm"], "out": res["out"], "frames": res["motu"]["frames"],
                      "ma1_peak_dbfs": res["ma1"].get("level", {}).get("peak_dbfs"),
                      "ma1_rms_dbfs": res["ma1"].get("level", {}).get("rms_dbfs"),
                      "arc_peak_dbfs": res["arc"].get("level", {}).get("peak_dbfs"),
                      "arc_rms_dbfs": res["arc"].get("level", {}).get("rms_dbfs"),
                      "ma1_sha256": res["ma1"]["sha256"], "arc_sha256": res["arc"]["sha256"]}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
