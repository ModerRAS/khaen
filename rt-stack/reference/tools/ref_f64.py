#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Independent binary64 reference evaluator for TSPI reference fixtures (WBS T03).

The model implemented here is the one normatively defined in ../SPEC.md:

    u_i   = a_i * x_i                      (input-side FIR per source, * = convolution)
    u_j   = u_{src(j)} for roles selected by A_map, else 0   (selection/zero-fill)
    G[j][k] = (j==k ? z^-D : 0) + g[participant(j)][participant(k)]   (causal FIRs)
    v_j   = sum_k G[j][k] * u_k
    y_j   = p_j * v_j                      (output diagonal FIR)
    z[c]  = y_j where scatter[j] == c, else 0   (container scatter)

All arithmetic is IEEE-754 binary64 in this file; it is the *second*, floating
point implementation used by tools/verify_reference.py (the first one is exact
rational arithmetic).  It is not a normative tolerance authority by itself: the
fixture's "comparison" block defines how implementations are compared.

CLI examples (run from reference/):
  python tools/ref_f64.py --case fixtures/cases/C04_tail_conv_1x1.json --mode offline
  python tools/ref_f64.py --case fixtures/cases/C10_streaming_128.json --mode streaming --block 64
  python tools/ref_f64.py --case fixtures/cases/C04_tail_conv_1x1.json --mode streaming --block 2 --out R:/tmp/c04-b2.json
"""
from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path


def load_fixture(path: Path) -> dict:
    with path.open("r", encoding="utf-8") as fh:
        fx = json.load(fh)
    if fx.get("schema") != "tspi-reference-fixture/v1":
        raise ValueError(f"{path}: unsupported schema {fx.get('schema')!r}")
    return fx


def _conv(a, b):
    """linear convolution of two causal float sequences"""
    if not a or not b:
        return []
    out = [0.0] * (len(a) + len(b) - 1)
    for i, ai in enumerate(a):
        if ai == 0.0:
            continue
        for j, bj in enumerate(b):
            if bj != 0.0:
                out[i + j] += ai * bj
    return out


class Graph:
    """Flattened graph: G_full with the z^-D identity baseline already embedded."""

    def __init__(self, fx: dict) -> None:
        g = fx["graph"]
        self.fx = fx
        self.n_src = int(g["n_src"])
        self.n_out = int(g["n_out"])
        self.container = int(g["container_channels"])
        self.d = int(g["d_samples"])
        self.a_fir = [[float(v) for v in ch] for ch in g["a_fir"]]
        self.p = [[float(v) for v in ch] for ch in g["p"]]
        self.scatter = [int(v) for v in g["scatter"]]
        self.participants = [int(v) for v in g["participants"]]
        supp = fx["supports"]
        self.l_a = int(supp["l_a"])
        self.l_g = int(supp["l_g"])
        self.l_p = int(supp["l_p"])
        self.l_f = int(supp["l_f"])
        self.l_out = int(supp["l_out"])
        # A_map: one source index per role or None
        self.src_of_role = []
        for row in g["a_map"]:
            hits = [i for i, v in enumerate(row) if int(v) == 1]
            if len(hits) > 1:
                raise ValueError("a_map row selects more than one source")
            self.src_of_role.append(hits[0] if hits else None)
        # G_full: O x O FIRs of length l_g (causal, zero padded)
        m = len(self.participants)
        pos = {role: idx for idx, role in enumerate(self.participants)}
        self.G = [[[0.0] * self.l_g for _ in range(self.n_out)] for _ in range(self.n_out)]
        for j in range(self.n_out):
            if self.d >= self.l_g:
                raise ValueError("D must be inside the declared G support (l_g >= D+1)")
            self.G[j][j][self.d] += 1.0
        grid = g["g"]
        if len(grid) != m or any(len(row) != m for row in grid):
            raise ValueError("g must be M x M")
        for a in range(m):
            j = self.participants[a]
            for b in range(m):
                k = self.participants[b]
                for t, v in enumerate(grid[a][b]):
                    self.G[j][k][t] += float(v)
        self.u_len = self._input_len() + self.l_a - 1
        self.v_len = self.u_len + self.l_g - 1
        # sanity: y length equals declared l_out
        assert max(self.v_len - 1 + self.l_p, 0) <= self.l_out, (self.v_len, self.l_p, self.l_out)

    def _input_len(self) -> int:
        lens = {len(ch) for ch in self.fx["input"]}
        if len(lens) != 1:
            raise ValueError("all input channels must share one length")
        return lens.pop()


def offline(fx: dict) -> list[list[float]]:
    """offline full linear convolution -> container channels x l_out"""
    gr = Graph(fx)
    x = [[float(v) for v in ch] for ch in fx["input"]]
    n_src, n_out, container = gr.n_src, gr.n_out, gr.container
    l_out = gr.l_out
    if [len(ch) for ch in x] != [gr._input_len()] * n_src:
        raise ValueError("input length mismatch")
    # input-side FIRs
    u_src = [_conv(gr.a_fir[i], x[i]) for i in range(n_src)]
    for i, ch in enumerate(u_src):
        if len(ch) < gr.u_len:
            ch.extend([0.0] * (gr.u_len - len(ch)))
    # role-domain selection / zero fill
    u_role = []
    for j in range(n_out):
        src = gr.src_of_role[j]
        u_role.append(list(u_src[src]) if src is not None else [0.0] * gr.u_len)
    # G stage
    v = []
    for j in range(n_out):
        acc = [0.0] * gr.v_len
        for k in range(n_out):
            c = _conv(gr.G[j][k], u_role[k])
            for t, val in enumerate(c):
                acc[t] += val
        v.append(acc)
    # P stage
    y = []
    for j in range(n_out):
        yj = _conv(gr.p[j], v[j])
        if len(yj) > l_out:
            raise ValueError("route longer than declared l_out")
        yj.extend([0.0] * (l_out - len(yj)))
        y.append(yj)
    z = [[0.0] * l_out for _ in range(container)]
    for j in range(n_out):
        z[gr.scatter[j]] = list(y[j])
    return z


def streaming(fx: dict, block_size: int) -> list[list[float]]:
    """block streaming evaluation.

    Contract (SPEC.md 3.3): pad the input with flush_zeros = l_f-1 zeros, pad
    again to a multiple of block_size, process sample by sample through the
    same causal state machine as a real-time host, concatenate blocks, then
    truncate to expected_len.
    """
    if block_size < 1:
        raise ValueError("block_size must be >= 1")
    gr = Graph(fx)
    x = [[float(v) for v in ch] for ch in fx["input"]]
    n_src, n_out, container = gr.n_src, gr.n_out, gr.container
    lx = gr._input_len()
    flush = gr.l_f - 1
    padded_len = lx + flush
    total = ((padded_len + block_size - 1) // block_size) * block_size
    # per-source sliding window of raw input (current + l_a-1 past samples)
    x_hist = [[0.0] * gr.l_a for _ in range(n_src)]
    # per-role sliding window of u (current + l_g-1 past samples)
    u_hist = [[0.0] * gr.l_g for _ in range(n_out)]
    # per-output sliding window of v (current + l_p-1 past samples)
    v_hist = [[0.0] * gr.l_p for _ in range(n_out)]
    out = [[0.0] * total for _ in range(container)]
    for n in range(total):
        for i in range(n_src):
            xn = x[i][n] if n < lx else 0.0
            hist = x_hist[i]
            hist.pop()
            hist.insert(0, xn)
        # input-side FIRs: u_i[n] = sum_k a_i[k] * x_i[n-k]
        u_src_now = []
        for i in range(n_src):
            a = gr.a_fir[i]
            acc = 0.0
            for k in range(len(a)):
                acc += a[k] * x_hist[i][k]
            u_src_now.append(acc)
        # role-domain selection / zero fill, then per-role u history
        for j in range(n_out):
            src = gr.src_of_role[j]
            uj = u_src_now[src] if src is not None else 0.0
            uh = u_hist[j]
            uh.pop()
            uh.insert(0, uj)
        # G stage: v_j = sum_k G[j][k] * u_k (u_hist[k] holds u_k[n-m] at m)
        v_now = []
        for j in range(n_out):
            acc = 0.0
            for k in range(n_out):
                gk = gr.G[j][k]
                uh = u_hist[k]
                for m in range(gr.l_g):
                    acc += gk[m] * uh[m]
            v_now.append(acc)
        # P stage
        for j in range(n_out):
            vh = v_hist[j]
            vh.pop()
            vh.insert(0, v_now[j])
            yj = 0.0
            pj = gr.p[j]
            for k in range(len(pj)):
                yj += pj[k] * vh[k]
            out[gr.scatter[j]][n] = yj
    return [ch[: gr.l_out] for ch in out]


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="binary64 reference evaluator for TSPI reference fixtures")
    ap.add_argument("--case", required=True, help="path to a tspi-reference-fixture/v1 JSON")
    ap.add_argument("--mode", choices=["offline", "streaming"], default="offline")
    ap.add_argument("--block", type=int, default=None, help="block size for --mode streaming")
    ap.add_argument("--out", default=None, help="write resulting container channels as JSON here")
    ap.add_argument("--summary", action="store_true", help="print per-channel peaks and length instead of full arrays")
    args = ap.parse_args(argv)
    fx = load_fixture(Path(args.case))
    expected_len = int(fx["expected_len"])
    if args.mode == "offline":
        result = offline(fx)
    else:
        if args.block is None:
            ap.error("--mode streaming requires --block")
        result = streaming(fx, args.block)
    for ch in result:
        if len(ch) != expected_len:
            raise SystemExit(f"internal error: channel length {len(ch)} != expected_len {expected_len}")
        for v in ch:
            if not math.isfinite(v):
                raise SystemExit("internal error: non-finite output")
    if args.summary:
        print(f"case={fx['id']} mode={args.mode} expected_len={expected_len} channels={len(result)}")
        for c, ch in enumerate(result):
            peak = max((abs(v) for v in ch), default=0.0)
            print(f"  ch{c:02d} peak={peak:.17g} nonzero={sum(1 for v in ch if v != 0.0)}")
    if args.out:
        Path(args.out).write_text(json.dumps(result) + "\n", encoding="utf-8", newline="\n")
        print(f"wrote {args.out} ({len(result)} x {expected_len} binary64 values)")
    elif not args.summary:
        print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
