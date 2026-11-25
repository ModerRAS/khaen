#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""analyze_plan_render.py -- independent sample-level verification of the plan-arena chain.

Reads the raw f32le files written by khaen-rt-host backend=fake + [sink] out:
  <dir>/render-identity.raw   2ch x frames  (graph=stereo_identity, containers=2)
  <dir>/render-plan.raw       8ch x frames  (graph=plan-arena, containers=8)
and measures, per tone of the multitone WAV fixture:
  * identity channel gain (should be 0 dB: bypass),
  * plan channel gain relative to identity (plan G=2.0 -> +6.02 dB),
  * plan extra group delay from phase slope (P FIR fractional delay),
  * that plan container channels 2..7 carry no signal.

Usage: python3 analyze_plan_render.py --dir <dir> [--json out.json]
"""
import argparse, cmath, json, math, struct, sys

FS = 48000
FREQS = [100, 200, 400, 800, 1000, 2000, 4000, 8000, 10000]
AMP = 0.05
SKIP_S = 0.25  # drop fade edges


def read_f32(path, nch):
    raw = open(path, "rb").read()
    n = len(raw) // 4
    vals = struct.unpack("<%df" % n, raw)
    frames = n // nch
    return [list(vals[c::nch]) for c in range(nch)], frames


def tone_measure(sig, freq):
    """complex amplitude at freq via Goertzel-like correlation over steady window."""
    n = len(sig)
    i0 = int(SKIP_S * FS)
    i1 = n - i0
    seg = sig[i0:i1]
    m = len(seg)
    w = 2 * math.pi * freq / FS
    # Hann window to suppress leakage from other tones
    re = im = 0.0
    wsum = 0.0
    for i in range(0, m):
        win = 0.5 - 0.5 * math.cos(2 * math.pi * i / (m - 1))
        v = seg[i] * win
        re += v * math.cos(w * i)
        im += v * math.sin(w * i)
        wsum += win
    # single-sided amplitude for a sine of peak A: |sum| ~ A*wsum/2
    amp = 2.0 * math.hypot(re, im) / wsum
    phase = math.atan2(im, re)
    return amp, phase


def db(x):
    return 20 * math.log10(x) if x > 0 else -999.0


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--json", default=None)
    args = ap.parse_args(argv)
    d = args.dir

    ident, nf = read_f32(d + "/render-identity.raw", 2)
    plan, nf2 = read_f32(d + "/render-plan.raw", 8)
    print("identity: %d frames x 2ch | plan: %d frames x 8ch" % (nf, nf2))
    if nf != nf2:
        print("WARN frame count differs")

    out = {"frames": nf, "tones": [], "unused_plan_channels_peak": {}}
    print("\n%7s %12s %12s %10s %10s %10s" % ("f(Hz)", "ident_L_dBFS", "plan_L_dBFS", "gain_dB", "phaseDiff", "extraDelay_us"))
    for f in FREQS:
        ai, pi_ = tone_measure(ident[0], f)
        ap_, pp = tone_measure(plan[0], f)
        # wrap phase difference to +/-pi
        dphi = math.atan2(math.sin(pp - pi_), math.cos(pp - pi_))
        # phase delay of the difference at this frequency
        delay_us = (-dphi / (2 * math.pi * f)) * 1e6
        gain = db(ap_) - db(ai)
        print("%7d %12.2f %12.2f %10.2f %10.3f %10.1f" % (f, db(ai), db(ap_), gain, dphi, delay_us))
        out["tones"].append({
            "freq_hz": f,
            "identity_amp": ai, "identity_dbfs": db(ai),
            "plan_amp": ap_, "plan_dbfs": db(ap_),
            "gain_db": gain, "phase_diff_rad": dphi, "phase_delay_us": delay_us,
        })

    # unused plan container channels must be silent (dense_to_dac = [0,1])
    for c in range(2, 8):
        peak = max(abs(x) for x in plan[c]) if plan[c] else 0.0
        out["unused_plan_channels_peak"]["ch%d" % c] = peak
        print("plan container ch%d peak=%.3g %s" % (c, peak, "OK silent" if peak < 1e-9 else "!! NOT SILENT"))
    # input reference: identity gain vs fixture amplitude
    gains = [t["identity_dbfs"] for t in out["tones"]]
    out["identity_gain_vs_fixture_db"] = [g - db(AMP) for g in gains]
    print("\nidentity vs fixture (-26.02 dBFS per tone): %s" %
          ", ".join("%+.2f" % (g - db(AMP)) for g in out["identity_gain_vs_fixture_db"]))
    pg = [t["gain_db"] for t in out["tones"]]
    print("plan-vs-identity gain: min %.2f max %.2f dB (expected +6.02 flat)" % (min(pg), max(pg)))
    delays = [t["phase_delay_us"] for t in out["tones"] if t["freq_hz"] <= 2000]
    print("plan extra phase delay (<=2 kHz): %s us" % ", ".join("%.1f" % x for x in delays))
    if args.json:
        json.dump(out, open(args.json, "w"), indent=1)
        print("[json] wrote", args.json)
    return 0


if __name__ == "__main__":
    sys.exit(main())
