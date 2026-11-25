#!/usr/bin/env python3
"""Byte-exact S24_3LE (24-bit little-endian in 3 bytes) helpers -- stdlib only.

ALSA S24_3LE interleaving (MOTU UltraLite-mk5 20ch capture, UMIK 2ch):
  frame = ch0[LSB, MID, MSB], ch1[LSB, MID, MSB], ..., chN-1[LSB, MID, MSB]
i.e. channel c occupies byte offsets 3*c, 3*c+1, 3*c+2 inside every frame of
3*channels bytes. This is NOT the "3 groups of N bytes" layout
(data[c::3N] / data[c+N::3N] / data[c+2N::3N]) which was the 2026-11-19 kit bug
(planrt_extract.py sha256 5d024af7...): reading ch0/ch1 with
data[ch::60] / data[ch+20::60] / data[ch+40::60] mixes in other channels' bytes
and produces the -42.14 dBFS "LSB full-scale" artifact.

Fixes/verification: tests/test_mimo_kit.py::test_extract_byte_mapping_exact,
test_extract_buggy_mapping_reproduces_artifact.
"""
from __future__ import annotations

S24_MAX = 8388607
S24_MIN = -8388608
S24_SCALE = 8388608.0


def frame_bytes(interleaved_channels: int) -> int:
    return interleaved_channels * 3


def extract_channel_bytes(interleaved, channels: int, ch: int):
    """Return a bytearray with channel *ch* of an S24_3LE interleaved stream.

    Byte-for-byte exact inverse of interleave_channels; sign extension is
    implicit because only the 3 sample bytes are copied (no arithmetic).
    """
    if ch < 0 or ch >= channels:
        raise ValueError("channel %r out of range 0..%d" % (ch, channels - 1))
    fb = frame_bytes(channels)
    n = len(interleaved) // fb
    out = bytearray(3 * n)
    o = ch * 3
    out[0::3] = interleaved[o::fb]
    out[1::3] = interleaved[o + 1::fb]
    out[2::3] = interleaved[o + 2::fb]
    return out


def extract_channel_file(src_path, dst_path, channels: int, ch: int, chunk_frames: int = 4800) -> int:
    """Stream-extract one S24_3LE channel from src_path into dst_path.

    Returns the number of frames written. Chunked so a 423 MB 20ch capture is
    never fully materialised in RAM.
    """
    import os
    fb = frame_bytes(channels)
    total = os.path.getsize(src_path) // fb
    done = 0
    o = ch * 3
    with open(src_path, "rb") as fi, open(dst_path, "wb") as fo:
        while done < total:
            n = min(chunk_frames, total - done)
            ba = fi.read(fb * n)
            if len(ba) != fb * n:
                raise IOError("short read in %s at frame %d" % (src_path, done))
            buf = bytearray(3 * n)
            buf[0::3] = ba[o::fb]
            buf[1::3] = ba[o + 1::fb]
            buf[2::3] = ba[o + 2::fb]
            fo.write(buf)
            done += n
    return total


def interleave_channels(channel_bytes):
    """Interleave per-channel S24_3LE bytearrays (test fixture helper)."""
    chans = list(channel_bytes)
    if not chans:
        return b""
    n = min(len(c) // 3 for c in chans)
    out = bytearray(3 * len(chans) * n)
    fb = len(chans) * 3
    for ci, c in enumerate(chans):
        o = ci * 3
        out[o::fb] = c[0:3 * n][0::3]
        out[o + 1::fb] = c[0:3 * n][1::3]
        out[o + 2::fb] = c[0:3 * n][2::3]
    return out


def int24_to_bytes(value: int) -> bytes:
    """Signed 24-bit int -> 3 LE bytes (two's complement)."""
    v = int(value)
    if v < S24_MIN or v > S24_MAX:
        raise ValueError("sample %d outside S24 range" % v)
    u = v & 0xFFFFFF
    return bytes((u & 0xFF, (u >> 8) & 0xFF, (u >> 16) & 0xFF))


def bytes_to_int24(b0: int, b1: int, b2: int) -> int:
    """3 LE bytes -> signed 24-bit int (sign extension included)."""
    u = (b0 & 0xFF) | ((b1 & 0xFF) << 8) | ((b2 & 0xFF) << 16)
    return u - 0x1000000 if u & 0x800000 else u


def channel_rms_peak_dbfs(mono_bytes, limit_frames=None):
    """RMS/peak in dBFS of a mono S24_3LE bytearray (pure python; no numpy)."""
    import math
    n = len(mono_bytes) // 3
    if limit_frames is not None:
        n = min(n, int(limit_frames))
    acc = 0.0
    peak = 0
    for i in range(n):
        j = i * 3
        v = bytes_to_int24(mono_bytes[j], mono_bytes[j + 1], mono_bytes[j + 2])
        av = -v if v < 0 else v
        if av > peak:
            peak = av
        acc += float(v) * float(v)
    rms = math.sqrt(acc / max(1, n)) / S24_SCALE
    peak_f = peak / S24_SCALE
    rms_db = 20.0 * math.log10(rms) if rms > 0 else -300.0
    peak_db = 20.0 * math.log10(peak_f) if peak_f > 0 else -300.0
    return {"frames": n, "rms_dbfs": rms_db, "peak_dbfs": peak_db}


def sha256_file(path: str) -> str:
    import hashlib
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()
