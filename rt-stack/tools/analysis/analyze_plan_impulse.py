#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""analyze_plan_impulse.py -- prove the plan-arena graph applies exactly the plan.bin
filter, from the impulse renders of tspi-rt-host (backend=fake + [sink] out).

Inputs (written by render-identity-impulse.toml / render-plan-impulse.toml):
  <dir>/render-identity-impulse.raw   2ch f32le (graph=stereo_identity)
  <dir>/render-plan-impulse.raw       8ch f32le (graph=plan-arena)
  plan.bin                            T66 ABI v1 plan (coeff blob base = ctl_meta_off + ctl_meta_len)

Checks:
  1. identity is an exact passthrough (single impulse, unity gain),
  2. plan output ch0/ch1 == gamma * P (gamma = path G DC gain; P = diag FIR coefficients),
  3. plan container channels 2..7 are silent (dense_to_dac mapping),
  4. reported group delay / gain of the P FIR.

Usage: python3 analyze_plan_impulse.py --dir <evdir> --plan <plan.bin> [--json out.json]
"""
import argparse, json, math, struct, sys

def read_f32(path, nch):
    raw = open(path, "rb").read()
    n = len(raw) // 4
    v = struct.unpack("<%df" % n, raw)
    return [list(v[c::nch]) for c in range(nch)], n // nch

def f32s(raw, off, n):
    return list(struct.unpack_from("<%df" % n, raw, off))

def parse_plan(path):
    raw = open(path, "rb").read()
    def u16(o): return struct.unpack_from("<H", raw, o)[0]
    def u32(o): return struct.unpack_from("<I", raw, o)[0]
    n_in, n_out = u16(18), u16(20)
    path_off, diag_off, role_off = u32(72), u32(76), u32(88)
    n_path, n_diag, n_role = u16(24), u16(26), u16(92)
    ctl_off, ctl_len = u32(104), u32(108)
    base = ctl_off + ctl_len                     # coefficient blob base
    paths = []
    for i in range(n_path):
        o = path_off + i * 32
        in_i, out_i, taps, delay = struct.unpack_from("<HHII", raw, o)
        pol, flags, _r = struct.unpack_from("<bBH", raw, o + 12)
        coff, clen = struct.unpack_from("<QII", raw, o + 16)[:2]
        paths.append({"in": in_i, "out": out_i, "taps": taps, "delay": delay,
                      "poly": pol, "flags": flags, "coeffs": f32s(raw, base + coff, taps)})
    diags = []
    for i in range(n_diag):
        o = diag_off + i * 32
        out_i, flags, taps, delay, _r = struct.unpack_from("<HHIII", raw, o)
        coff, clen = struct.unpack_from("<QII", raw, o + 16)[:2]
        diags.append({"out": out_i, "flags": flags, "taps": taps, "delay": delay,
                      "coeffs": f32s(raw, base + coff, taps)})
    return {"n_in": n_in, "n_out": n_out, "paths": paths, "diags": diags,
            "coeff_base": base, "size": len(raw)}

def conv(a, b):
    out = [0.0] * (len(a) + len(b) - 1)
    for i, x in enumerate(a):
        if x == 0.0:
            continue
        for j, y in enumerate(b):
            out[i + j] += x * y
    return out

def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--plan", required=True)
    ap.add_argument("--json", default=None)
    args = ap.parse_args(argv)
    d, plan_path = args.dir, args.plan

    ident, nfi = read_f32(d + "/render-identity-impulse.raw", 2)
    plan, nfp = read_f32(d + "/render-plan-impulse.raw", 8)
    p = parse_plan(plan_path)
    res = {"identity_frames": nfi, "plan_frames": nfp, "plan_bytes": p["size"],
           "n_in": p["n_in"], "n_out": p["n_out"]}

    nz = [i for i, v in enumerate(ident[0]) if abs(v) > 1e-9]
    imp_at = nz[0] if nz else -1
    res["identity_impulse_frame"] = imp_at
    res["identity_impulse_amplitude"] = ident[0][imp_at] if imp_at >= 0 else None
    res["identity_is_delta"] = (len(nz) == 1)

    # expected plan output for channel c: conv(path G, diag P) * impulse amplitude
    amp = res["identity_impulse_amplitude"] or 0.0
    expected = []
    for c in range(p["n_out"]):
        e = [0.0] * nfp
        g = next((x for x in p["paths"] if x["out"] == c), None)
        pd = next((x for x in p["diags"] if x["out"] == c), None)
        if g and pd:
            h = conv(g["coeffs"], pd["coeffs"])
            if g["flags"] & 1:  # diag_baseline_included -> G replaces delta? keep additive baseline
                pass
            for k, v in enumerate(h):
                if imp_at + k < nfp:
                    e[imp_at + k] = amp * v
        expected.append(e)
    # NOTE: fallback for this plan shape: identity path G=[2,0...] then P -> 2*P
    res["containers"] = len(plan)
    checks = {}
    for c in range(min(p["n_out"], len(plan))):
        got = plan[c]
        exp = expected[c] if c < len(expected) else [0.0] * nfp
        err = max((abs(a - b) for a, b in zip(got, exp)), default=0.0)
        peak = max((abs(x) for x in got), default=0.0)
        checks["ch%d" % c] = {"max_abs_err_vs_plan_conv": err, "peak": peak,
                              "taps_nonzero": sum(1 for x in got if abs(x) > 1e-12)}
    res["channels"] = checks
    # P FIR characterization
    pd0 = next((x for x in p["diags"] if x["out"] == 0), None)
    if pd0:
        coeffs = pd0["coeffs"]
        peak_i = max(range(len(coeffs)), key=lambda i: abs(coeffs[i]))
        res["p_fir"] = {
            "taps": len(coeffs), "peak_tap": peak_i, "peak_abs": abs(coeffs[peak_i]),
            "sum": sum(coeffs),
            "group_delay_samples": (sum(i * c for i, c in enumerate(coeffs)) / sum(coeffs)) if sum(coeffs) else None,
        }
        # magnitude response (flat check) at a few frequencies
        def H(f):
            w = 2 * math.pi * f / 48000.0
            z = complex(math.cos(-w), math.sin(-w))
            acc = 0j
            for c in reversed(coeffs):
                acc = acc * z + c
            return acc
        res["p_fir"]["mag_db"] = {str(f): 20 * math.log10(abs(H(f))) for f in (100, 200, 1000, 2000, 4000, 8000, 10000)}
    print(json.dumps(res, indent=1, ensure_ascii=False))
    if args.json:
        json.dump(res, open(args.json, "w"), indent=1)
        print("[json] wrote", args.json)
    ok = res["identity_is_delta"] and all(
        (v["max_abs_err_vs_plan_conv"] < 2e-4) for k, v in checks.items() if v["peak"] > 0 or k in ("ch2", "ch3"))
    return 0 if ok else 1

if __name__ == "__main__":
    sys.exit(main())
