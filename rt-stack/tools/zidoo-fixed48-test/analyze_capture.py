#!/usr/bin/env python3
"""Analyse a MOTU TOSLink capture for the Zidoo fixed-48k APK test.

Reads a WAV recorded from the MOTU UltraLite-mk5 (e.g. 20 channels, S24_3LE, 48 kHz),
extracts the optical input channel pair, and reports:

  * channel RMS in dBFS over the loudest 1 s window,
  * the dominant spectral peak in 20 Hz .. 20 kHz,
  * energy in the expected 1 kHz band versus total,
  * a machine-readable JSON verdict.

Typical use (zero-based channel indexes by default, matching the rt-host
"toslink_channels=[12,13]" convention):

  python analyze_capture.py capture.wav --channels 12,13
  python analyze_capture.py capture.wav --channels 13,14 --one-based

Requires numpy.
"""
import argparse
import json
import sys

import numpy as np


def read_wav(path):
    with open(path, "rb") as f:
        data = f.read()
    if data[0:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise SystemExit("not a RIFF/WAVE file: " + path)
    pos = 12
    fmt = None
    pcm = None
    while pos + 8 <= len(data):
        cid = data[pos:pos + 4]
        size = int.from_bytes(data[pos + 4:pos + 8], "little")
        body = data[pos + 8:pos + 8 + size]
        if cid == b"fmt ":
            tag = int.from_bytes(body[0:2], "little")
            channels = int.from_bytes(body[2:4], "little")
            rate = int.from_bytes(body[4:8], "little")
            block_align = int.from_bytes(body[12:14], "little")
            bits = int.from_bytes(body[14:16], "little")
            if tag == 0xFFFE and len(body) >= 40:
                tag = int.from_bytes(body[24:26], "little")
            fmt = (tag, channels, rate, block_align, bits)
        elif cid == b"data":
            pcm = body
            break
        pos += 8 + size + (size & 1)
    if fmt is None or pcm is None:
        raise SystemExit("missing fmt/data chunk: " + path)
    tag, channels, rate, block_align, bits = fmt
    bps = bits // 8
    if tag not in (1, 3):
        raise SystemExit("unsupported WAV format tag %d" % tag)
    frames = len(pcm) // block_align
    usable = frames * block_align
    raw = np.frombuffer(pcm[:usable], dtype=np.uint8).reshape(frames, block_align)
    if tag == 3 and bits == 32:
        x = raw[:, :channels * 4].copy().view("<f4").astype(np.float64)
    elif bps == 1:
        x = (raw[:, :channels].astype(np.float64) - 128.0) / 128.0
    elif bps == 2:
        x = raw[:, :channels * 2].copy().view("<i2").astype(np.float64) / 32768.0
    elif bps == 3:
        b = raw[:, :channels * 3].reshape(frames, channels, 3).astype(np.int32)
        v = b[:, :, 0] | (b[:, :, 1] << 8) | (b[:, :, 2] << 16)
        v = np.where(v & 0x800000, v - 0x1000000, v)
        x = v.astype(np.float64) / 8388608.0
    elif bps == 4:
        x = raw[:, :channels * 4].copy().view("<i4").astype(np.float64) / 2147483648.0
    else:
        raise SystemExit("unsupported bits per sample: %d" % bits)
    return x, rate, channels


def dbfs(v):
    if v <= 0:
        return -200.0
    return 20.0 * np.log10(v)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("wav")
    ap.add_argument("--channels", default="12,13",
                    help="comma separated channel pair (zero-based by default)")
    ap.add_argument("--one-based", action="store_true",
                    help="interpret --channels as 1-based indexes")
    ap.add_argument("--expect-hz", type=float, default=1000.0)
    ap.add_argument("--tone-min-dbfs", type=float, default=-80.0)
    ap.add_argument("--tone-tol-hz", type=float, default=5.0)
    args = ap.parse_args()

    x, rate, channels = read_wav(args.wav)
    idx = [int(v) for v in args.channels.split(",")]
    if args.one_based:
        idx = [v - 1 for v in idx]
    if max(idx) >= channels:
        raise SystemExit("file has %d channels, requested %s" % (channels, idx))
    mono = x[:, idx].mean(axis=1)

    win = max(1, int(rate))
    if len(mono) > win:
        n = len(mono) // win
        blocks = mono[:n * win].reshape(n, win)
        rms_blocks = np.sqrt(np.mean(blocks ** 2, axis=1))
        k = int(np.argmax(rms_blocks))
        seg = mono[k * win:(k + 1) * win]
        window_at = k
    else:
        seg = mono
        window_at = 0
    rms = float(np.sqrt(np.mean(seg ** 2)))

    take = min(len(mono), 2 * rate)
    start = min(window_at * win, max(0, len(mono) - take))
    spec = mono[start:start + take]
    if len(spec) < 512:
        spec = np.pad(spec, (0, 512 - len(spec)))
    w = np.hanning(len(spec))
    nfft = 1 << (len(spec) - 1).bit_length()
    mag = np.abs(np.fft.rfft(spec * w, nfft))
    freqs = np.fft.rfftfreq(nfft, 1.0 / rate)
    sel = (freqs >= 20.0) & (freqs <= 20000.0)
    peak_hz = float(freqs[sel][int(np.argmax(mag[sel]))])
    # one-sided amplitude of a real tone: 2 * |X[k]| / sum(window)
    peak_dbfs = dbfs(2.0 * float(np.max(mag[sel])) / float(np.sum(w)))
    band = (freqs >= args.expect_hz - 20.0) & (freqs <= args.expect_hz + 20.0)
    band_ratio_db = dbfs(float(np.sum(mag[band] ** 2) / max(np.sum(mag[sel] ** 2), 1e-30)))

    tone_present = (dbfs(rms) > args.tone_min_dbfs) and (abs(peak_hz - args.expect_hz) <= args.tone_tol_hz)
    result = {
        "file": args.wav,
        "sample_rate": rate,
        "file_channels": channels,
        "channels_used_zero_based": idx,
        "loudest_window_1s_index": window_at,
        "rms_dbfs": round(dbfs(rms), 2),
        "peak_hz": round(peak_hz, 2),
        "peak_dbfs_approx": round(peak_dbfs, 2),
        "expected_hz": args.expect_hz,
        "expected_band_ratio_db": round(band_ratio_db, 2),
        "tone_present": bool(tone_present),
        "verdict": "clean_%g_Hz_tone_on_optical" % args.expect_hz if tone_present
                   else "no_clean_%g_Hz_tone_on_optical" % args.expect_hz,
    }
    print(json.dumps(result, ensure_ascii=False, indent=2))
    print("")
    print("RMS %.1f dBFS | peak %.1f Hz | 1 kHz band %.1f dB | %s" % (
        result["rms_dbfs"], result["peak_hz"], result["expected_band_ratio_db"],
        "TONE PRESENT" if tone_present else "NO CLEAN TONE"))
    return 0 if tone_present else 1


if __name__ == "__main__":
    sys.exit(main())
