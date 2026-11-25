#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Independent verifier for the KHAEN f64 reference package (WBS T03, P0).

It checks, without needing T06 / rt-check / any Rust crate:

  1. fixtures/index.json hashes and file inventory (input hash binding);
  2. fixture schema / graph consistency / declared support length formulas
     (L_F = L_A + L_G + L_P - 2, D counted inside L_G, L_out = L_in + L_F - 1);
  3. expected outputs against an exact-rational per-sample direct convolution
     implementation written independently of tools/gen_fixtures.py (which uses
     polynomial multiplication);
  4. the binary64 implementation tools/ref_f64.py against expected outputs
     (bit-exact for comparison mode "exact", declared tolerance otherwise),
     offline and streaming for every declared block size;
  5. semantic checks for fixture tags (identity / offdiag / tail / D / strict /
     scatter / column_fir / five_by_five / dense / tolerance / streaming ...);
  6. hand-calculated vectors embedded as fenced json handcalc / handcheck /
     handtrap blocks in HAND-CALC.md against the fixture data;
  7. DoD coverage: offdiag + tail + D are present and hand-covered.
  8. With --negative-validation: 8 invalid fixtures must be rejected, the three
     trap wrong-values must equal their declared wrong formulas, and handcheck
     samples must match ref_f64 independently (12 extra checks; default stays 108).

Exit code 0 iff every check passes.  Writes a JSON report when --report is given.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import sys
from fractions import Fraction
from pathlib import Path

REF_ROOT = Path(__file__).resolve().parents[1]
TOOLS_DIR = REF_ROOT / "tools"
FIXTURES_DIR = REF_ROOT / "fixtures"
CASES_DIR = FIXTURES_DIR / "cases"
INDEX_PATH = FIXTURES_DIR / "index.json"
HANDCALC_PATH = REF_ROOT / "HAND-CALC.md"
SCHEMA = "khaen-reference-fixture/v1"
INDEX_SCHEMA = "khaen-reference-fixtures-index/v1"
FENCE = chr(96) * 3  # three backticks, kept out of source text for tooling reasons

sys.path.insert(0, str(TOOLS_DIR))
import ref_f64  # noqa: E402  (second, binary64 implementation)

KNOWN_TAGS = {
    "identity", "baseline", "offdiag", "tail", "D", "strict", "scatter",
    "polarity", "zerofill", "column_fir", "input_delay", "five_by_five",
    "embed", "multichannel", "row_column", "dense", "random_exact",
    "tolerance", "streaming",
}
REQUIRED_TAGS = {
    "identity", "offdiag", "tail", "D", "strict", "scatter", "column_fir",
    "tolerance", "dense", "streaming", "five_by_five",
}
# semantic tags are asserted per fixture; baseline/random_exact are descriptive indices (SPEC 6.5)
DESCRIPTIVE_TAGS = {"baseline", "random_exact"}
SEMANTIC_TAGS = KNOWN_TAGS - DESCRIPTIVE_TAGS
REQUIRED_HAND_TAGS = {"offdiag", "tail", "D"}
TOP_KEYS = {"schema", "id", "title", "tags", "fs_hz", "generated_by", "graph",
            "supports", "input", "expected", "expected_len", "comparison",
            "streaming", "notes", "traps"}
GRAPH_KEYS = {"n_src", "n_out", "container_channels", "d_samples", "a_map",
              "a_fir", "participants", "g", "p", "scatter"}


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------
def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def frac(v) -> Fraction:
    if isinstance(v, Fraction):
        return v
    if isinstance(v, bool):
        raise ValueError("boolean is not a numeric literal")
    if isinstance(v, int):
        return Fraction(v)
    if isinstance(v, float):
        if not math.isfinite(v):
            raise ValueError(f"non-finite value {v!r}")
        return Fraction(v)
    raise ValueError(f"unsupported numeric literal {v!r}")


def all_finite(values) -> bool:
    for v in values:
        if isinstance(v, bool) or not isinstance(v, (int, float)):
            return False
        if isinstance(v, float) and not math.isfinite(v):
            return False
    return True


class Report:
    def __init__(self) -> None:
        self.checks = []
        self.failures = 0

    def add(self, check_id: str, description: str, ok: bool, detail: str = "") -> None:
        self.checks.append(dict(id=check_id, description=description,
                                result="PASS" if ok else "FAIL", detail=detail))
        if not ok:
            self.failures += 1
            print(f"FAIL [{check_id}] {description} {detail}", file=sys.stderr)


# ---------------------------------------------------------------------------
# fixture validation
# ---------------------------------------------------------------------------
def supports_from_graph(fx: dict) -> dict:
    g = fx["graph"]
    d = int(g["d_samples"])
    a_fir, p = g["a_fir"], g["p"]
    l_a = max(len(ch) for ch in a_fir)
    max_g_idx = -1
    for row in g["g"]:
        for cell in row:
            for t, v in enumerate(cell):
                if frac(v) != 0:
                    max_g_idx = max(max_g_idx, t)
    l_g = max(d + 1, max_g_idx + 1)
    l_p = max(len(ch) for ch in p)
    l_f = l_a + l_g + l_p - 2
    lens = {len(ch) for ch in fx["input"]}
    if len(lens) != 1:
        raise ValueError("all input channels must share one length")
    l_x = lens.pop()
    return dict(l_a=l_a, l_g=l_g, l_p=l_p, l_f=l_f, l_out=l_x + l_f - 1, l_x=l_x)


def validate_fixture(fx: dict, expected_id_from_file: str) -> dict:
    """Raises ValueError on any schema/consistency problem; returns computed info."""
    if not isinstance(fx, dict):
        raise ValueError("fixture root must be an object")
    extra = set(fx) - TOP_KEYS
    if extra:
        raise ValueError(f"unknown fixture keys: {sorted(extra)}")
    missing = TOP_KEYS - {"notes", "traps"} - set(fx)
    if missing:
        raise ValueError(f"missing fixture keys: {sorted(missing)}")
    if fx["schema"] != SCHEMA:
        raise ValueError(f"schema must be {SCHEMA!r}, got {fx['schema']!r}")
    if fx["id"] != expected_id_from_file:
        raise ValueError(f"id {fx['id']!r} does not match file name {expected_id_from_file!r}")
    if not isinstance(fx["title"], str) or not fx["title"]:
        raise ValueError("title must be a non-empty string")
    if not isinstance(fx["tags"], list) or not fx["tags"]:
        raise ValueError("tags must be a non-empty list")
    unknown = set(fx["tags"]) - KNOWN_TAGS
    if unknown:
        raise ValueError(f"unknown tags: {sorted(unknown)}")
    if int(fx["fs_hz"]) <= 0:
        raise ValueError("fs_hz must be positive")
    g = fx["graph"]
    if not isinstance(g, dict):
        raise ValueError("graph must be an object")
    extra = set(g) - GRAPH_KEYS
    if extra:
        raise ValueError(f"unknown graph keys: {sorted(extra)}")
    missing = GRAPH_KEYS - set(g)
    if missing:
        raise ValueError(f"missing graph keys: {sorted(missing)}")
    n_src, n_out, container = int(g["n_src"]), int(g["n_out"]), int(g["container_channels"])
    if not (1 <= n_src <= 32):
        raise ValueError("n_src out of range 1..32")
    if not (1 <= n_out <= 22):
        raise ValueError("n_out out of range 1..22")
    if container < n_out:
        raise ValueError("container_channels must be >= n_out")
    d = int(g["d_samples"])
    if d < 0:
        raise ValueError("d_samples must be >= 0")
    a_map = g["a_map"]
    if len(a_map) != n_out or any(len(row) != n_src for row in a_map):
        raise ValueError("a_map must be n_out x n_src")
    for j, row in enumerate(a_map):
        for v in row:
            if v not in (0, 1):
                raise ValueError(f"a_map[{j}] entries must be 0 or 1")
        if sum(1 for v in row if v == 1) > 1:
            raise ValueError(f"a_map row {j} selects more than one source")
    col_count = [0] * n_src
    for row in a_map:
        for i, v in enumerate(row):
            if v == 1:
                col_count[i] += 1
    if any(c > 1 for c in col_count):
        raise ValueError("a_map must be injective (a source may feed at most one role)")
    a_fir = g["a_fir"]
    if len(a_fir) != n_src or any(not isinstance(ch, list) or not ch for ch in a_fir):
        raise ValueError("a_fir must be a non-empty FIR per source")
    for ch in a_fir:
        if not all_finite(ch):
            raise ValueError("a_fir contains non-finite/non-numeric values")
    p = g["p"]
    if len(p) != n_out or any(not isinstance(ch, list) or not ch for ch in p):
        raise ValueError("p must be a non-empty FIR per output role")
    for ch in p:
        if not all_finite(ch):
            raise ValueError("p contains non-finite/non-numeric values")
    participants = g["participants"]
    m = len(participants)
    if len(set(participants)) != m:
        raise ValueError("participants must be unique")
    for v in participants:
        if not isinstance(v, int) or isinstance(v, bool) or not (0 <= v < n_out):
            raise ValueError(f"participant role index out of range: {v!r}")
    grid = g["g"]
    if len(grid) != m or any(len(row) != m for row in grid):
        raise ValueError("g must be M x M where M = len(participants)")
    for row in grid:
        for cell in row:
            if not isinstance(cell, list):
                raise ValueError("g cells must be FIR arrays ([] = zero filter)")
            if not all_finite(cell):
                raise ValueError("g contains non-finite/non-numeric values")
    scatter = g["scatter"]
    if len(scatter) != n_out:
        raise ValueError("scatter must have n_out entries")
    if len(set(scatter)) != n_out:
        raise ValueError("scatter must be injective")
    for v in scatter:
        if not isinstance(v, int) or isinstance(v, bool) or not (0 <= v < container):
            raise ValueError(f"scatter index out of range: {v!r}")
    x = fx["input"]
    if len(x) != n_src:
        raise ValueError("input must have n_src channels")
    if len({len(ch) for ch in x}) != 1:
        raise ValueError("all input channels must share one length")
    for ch in x:
        if not all_finite(ch):
            raise ValueError("input contains non-finite/non-numeric values")
    comp = supports_from_graph(fx)
    supp = fx["supports"]
    for key in ("l_a", "l_g", "l_p", "l_f", "l_out"):
        if key not in supp:
            raise ValueError(f"supports missing {key}")
        if int(supp[key]) != comp[key]:
            raise ValueError(f"supports.{key}={supp[key]} but graph implies {comp[key]}")
    if int(fx["expected_len"]) != comp["l_out"]:
        raise ValueError("expected_len does not equal l_out")
    exp = fx["expected"]
    if len(exp) != container:
        raise ValueError("expected must have container_channels arrays")
    for ch in exp:
        if len(ch) != comp["l_out"]:
            raise ValueError("every expected channel must have expected_len samples")
        if not all_finite(ch):
            raise ValueError("expected contains non-finite/non-numeric values")
    cmp_ = fx["comparison"]
    if cmp_.get("mode") not in ("exact", "tolerance"):
        raise ValueError("comparison.mode must be exact|tolerance")
    if cmp_["mode"] == "tolerance":
        if not (float(cmp_.get("rtol", 0)) > 0) or float(cmp_.get("atol", 0)) < 0:
            raise ValueError("tolerance comparison needs rtol > 0 and atol >= 0")
        if cmp_.get("norm", "max_abs") != "max_abs":
            raise ValueError("only norm=max_abs is defined")
    st = fx["streaming"]
    if st.get("flush_zeros") != comp["l_f"] - 1:
        raise ValueError("streaming.flush_zeros must equal l_f - 1")
    sizes = st.get("block_sizes")
    if not isinstance(sizes, list) or not sizes or any((not isinstance(b, int)) or isinstance(b, bool) or b < 1 for b in sizes):
        raise ValueError("streaming.block_sizes must be a non-empty list of positive ints")
    if not isinstance(st.get("contract"), str) or not st["contract"]:
        raise ValueError("streaming.contract must be a non-empty string")
    for trap in fx.get("traps", []):
        if not isinstance(trap, dict) or not trap.get("id") or not trap.get("explanation"):
            raise ValueError("each trap needs id + explanation")
        if "wrong_expected" in trap:
            if len(trap["wrong_expected"]) != container:
                raise ValueError(f"trap {trap['id']}: wrong_expected channel count mismatch")
        if "wrong_outputs" in trap:
            if not isinstance(trap["wrong_outputs"], dict):
                raise ValueError(f"trap {trap['id']}: wrong_outputs must be an object")
    return dict(n_src=n_src, n_out=n_out, container=container, d=d, m=m,
                supports=comp, participants=participants, scatter=scatter,
                a_map=a_map, a_fir=a_fir, p=p, grid=grid)


def build_exact_graph(fx: dict):
    info = validate_fixture(fx, fx["id"])
    n_out, l_g = info["n_out"], info["supports"]["l_g"]
    d = info["d"]
    participants = info["participants"]
    m = info["m"]
    G = [[[Fraction(0)] * l_g for _ in range(n_out)] for _ in range(n_out)]
    for j in range(n_out):
        G[j][j][d] += Fraction(1)
    for a in range(m):
        j = participants[a]
        for b in range(m):
            k = participants[b]
            for t, v in enumerate(info["grid"][a][b]):
                G[j][k][t] += frac(v)
    return info, G


def exact_offline(fx: dict):
    """exact-rational per-sample direct convolution (independent of the generator)"""
    info, G = build_exact_graph(fx)
    n_src, n_out, container = info["n_src"], info["n_out"], info["container"]
    comp = info["supports"]
    l_a, l_g, l_out = comp["l_a"], comp["l_g"], comp["l_out"]
    x = [[frac(v) for v in ch] for ch in fx["input"]]
    lx = comp["l_x"]
    u_src = []
    for i in range(n_src):
        a = [frac(v) for v in info["a_fir"][i]]
        ch = []
        for n in range(lx + l_a - 1):
            acc = Fraction(0)
            for k in range(len(a)):
                if 0 <= n - k < lx:
                    acc += a[k] * x[i][n - k]
            ch.append(acc)
        u_src.append(ch)
    u_role = []
    for j in range(n_out):
        row = info["a_map"][j]
        src = [i for i, v in enumerate(row) if v == 1]
        u_role.append(list(u_src[src[0]]) if src else [Fraction(0)] * (lx + l_a - 1))
    v = []
    for j in range(n_out):
        ch = []
        for n in range(lx + l_a + l_g - 2):
            acc = Fraction(0)
            for k in range(n_out):
                gk = G[j][k]
                for m2 in range(l_g):
                    if 0 <= n - m2 < len(u_role[k]):
                        acc += gk[m2] * u_role[k][n - m2]
            ch.append(acc)
        v.append(ch)
    y = []
    for j in range(n_out):
        pj = [frac(val) for val in info["p"][j]]
        ch = []
        for n in range(l_out):
            acc = Fraction(0)
            for k in range(len(pj)):
                if 0 <= n - k < len(v[j]):
                    acc += pj[k] * v[j][n - k]
            ch.append(acc)
        y.append(ch)
    z = [[Fraction(0)] * l_out for _ in range(container)]
    for j in range(n_out):
        z[info["scatter"][j]] = list(y[j])
    return info, z


# ---------------------------------------------------------------------------
# comparison helpers
# ---------------------------------------------------------------------------
def compare_arrays(actual, expected, comparison) -> tuple[bool, float, float]:
    """actual/expected: container x l_out floats. Returns (ok, max_abs_err, tol)."""
    mode = comparison["mode"]
    expected_scale = max((abs(float(v)) for ch in expected for v in ch), default=0.0)
    if mode == "tolerance":
        atol = float(comparison.get("atol", 0.0))
        rtol = float(comparison["rtol"])
        tol = atol + rtol * max(1.0, expected_scale)
    else:
        tol = 0.0
    max_err = 0.0
    ok = True
    for c in range(len(expected)):
        if len(actual[c]) != len(expected[c]):
            return False, float("inf"), tol
        for n, e in enumerate(expected[c]):
            a = actual[c][n]
            if not math.isfinite(a):
                return False, float("inf"), tol
            err = abs(a - float(e))
            if err > max_err:
                max_err = err
            if mode == "exact":
                if float(a) != float(e):
                    ok = False
            else:
                if err > tol:
                    ok = False
    return ok, max_err, tol


def expected_matches_exact(fx: dict, exact_z) -> tuple[bool, str]:
    """exact mode: expected == exact rational result; tolerance mode: expected
    is the single correctly-rounded binary64 value of the exact result."""
    mode = fx["comparison"]["mode"]
    for c, ch in enumerate(fx["expected"]):
        for n, e in enumerate(ch):
            ev = exact_z[c][n]
            if mode == "exact":
                if frac(e) != ev:
                    return False, f"expected[{c}][{n}]={e!r} != exact {ev}"
            else:
                if float(ev) != float(e):
                    return False, f"expected[{c}][{n}]={e!r} != rounded exact {float(ev)!r}"
    return True, ""


# ---------------------------------------------------------------------------
# tag semantics
# ---------------------------------------------------------------------------
def check_tag_semantics(fx: dict, info: dict, exact_z) -> list[str]:
    problems = []
    tags = set(fx["tags"])
    comp = info["supports"]
    lx = comp["l_x"]
    g = fx["graph"]
    grid = g["g"]
    n_out = info["n_out"]
    participants = info["participants"]
    m = info["m"]
    nonparticipant = [j for j in range(n_out) if j not in set(participants)]
    x = fx["input"]
    if "identity" in tags:
        if any(frac(v) != 0 for row in grid for cell in row for v in cell):
            problems.append("identity tag but g is non-zero")
        if any([frac(v) for v in ch] != [Fraction(1)] for ch in g["a_fir"]):
            problems.append("identity tag but a_fir != [1]")
        if any([frac(v) for v in ch] != [Fraction(1)] for ch in g["p"]):
            problems.append("identity tag but p != [1]")
        d = info["d"]
        for j in range(n_out):
            src = [i for i, v in enumerate(info["a_map"][j]) if v == 1]
            c = info["scatter"][j]
            for n in range(comp["l_out"]):
                want = Fraction(0)
                if src and 0 <= n - d < lx:
                    want = frac(x[src[0]][n - d])
                if exact_z[c][n] != want:
                    problems.append(f"identity tag but role {j} sample {n} != delayed input")
                    break
    if "offdiag" in tags:
        found = any(frac(grid[a][b][t]) != 0
                    for a in range(m) for b in range(m) if a != b
                    for t in range(len(grid[a][b])))
        if not found:
            problems.append("offdiag tag but no non-zero off-diagonal g entry")
    if "tail" in tags:
        if not any(frac(v) != 0 for ch in exact_z for v in ch[lx:]):
            problems.append("tail tag but no non-zero output at n >= input length")
    if "D" in tags and info["d"] <= 0:
        problems.append("D tag but d_samples == 0")
    if "strict" in tags:
        zero_rows = [j for j in range(n_out) if not any(v == 1 for v in info["a_map"][j])]
        if not zero_rows:
            problems.append("strict tag but every role is mapped")
        for j in zero_rows:
            if any(frac(v) != 0 for v in exact_z[info["scatter"][j]]):
                problems.append(f"strict tag but unmapped role {j} emits non-zero output")
                break
        mapped_nonzero = any(any(frac(v) != 0 for v in exact_z[info["scatter"][j]])
                             for j in range(n_out) if j not in zero_rows)
        if zero_rows and not mapped_nonzero:
            problems.append("strict tag but no mapped role emits output")
    if "scatter" in tags:
        if info["scatter"] == list(range(n_out)) and info["container"] == n_out:
            problems.append("scatter tag but scatter is identity with no unused container channels")
        used = set(info["scatter"])
        for c in range(info["container"]):
            if c not in used and any(frac(v) != 0 for v in exact_z[c]):
                problems.append(f"scatter tag but unused container channel {c} is non-zero")
                break
    if "column_fir" in tags:
        if all([frac(v) for v in ch] == [Fraction(1)] for ch in g["a_fir"]):
            problems.append("column_fir tag but every a_fir is the identity")
    if "five_by_five" in tags:
        if m != 5:
            problems.append("five_by_five tag but len(participants) != 5")
        if n_out <= m:
            problems.append("five_by_five tag but n_out <= M (no embedding into wider output set)")
        if not nonparticipant:
            problems.append("five_by_five tag but every output role is a participant")
    if "dense" in tags:
        cells = sum(1 for row in grid for cell in row
                    if any(frac(v) != 0 for v in cell))
        if cells != m * m:
            problems.append(f"dense tag but only {cells}/{m * m} g cells are non-zero")
    if "tolerance" in tags and fx["comparison"]["mode"] != "tolerance":
        problems.append("tolerance tag but comparison.mode != tolerance")
    if "streaming" in tags and not fx["streaming"]["block_sizes"]:
        problems.append("streaming tag but no block sizes")
    if "input_delay" in tags:
        if not any(len(ch) > 1 and frac(ch[0]) == 0 for ch in g["a_fir"]):
            problems.append("input_delay tag but no a_fir starts with a zero tap")
    if "polarity" in tags:
        neg = any(frac(v) < 0 for ch in g["a_fir"] for v in ch) or \
              any(frac(v) < 0 for ch in g["p"] for v in ch)
        if not neg:
            problems.append("polarity tag but no negative coefficient exists")
    if "zerofill" in tags:
        zero_rows = [j for j in range(n_out) if not any(v == 1 for v in info["a_map"][j])]
        if not zero_rows:
            problems.append("zerofill tag but no all-zero a_map row")
        else:
            for j in zero_rows:
                if any(frac(v) != 0 for v in exact_z[info["scatter"][j]]):
                    problems.append(f"zerofill tag but zero-fill role {j} is non-zero")
                    break
    if "multichannel" in tags:
        if info["container"] <= 2:
            problems.append("multichannel tag but container_channels <= 2")
        nonzero_roles = sum(1 for j in range(n_out)
                            if any(frac(v) != 0 for v in exact_z[info["scatter"][j]]))
        if nonzero_roles < 2:
            problems.append(f"multichannel tag but only {nonzero_roles} non-zero output role(s)")
    if "embed" in tags:
        if m >= n_out:
            problems.append("embed tag but participants count >= n_out")
    if "row_column" in tags:
        shapes = {tuple(frac(v) for v in ch) for ch in g["p"]}
        offdiag = any(frac(grid[a][b][t]) != 0
                      for a in range(m) for b in range(m) if a != b
                      for t in range(len(grid[a][b])))
        if not offdiag:
            problems.append("row_column tag but no off-diagonal g")
        if len(shapes) == 1:
            problems.append("row_column tag but all p_j are identical (P/G commute)")
    return problems


# ---------------------------------------------------------------------------
# hand-calc blocks
# ---------------------------------------------------------------------------
def parse_hand_blocks(path: Path) -> list[dict]:
    blocks = []
    if not path.exists():
        raise ValueError(f"missing {path}")
    lines = path.read_text(encoding="utf-8").splitlines()
    start_re = re.compile(r"^" + re.escape(FENCE) + r"json\s+(handcalc|handcheck|handtrap)\b(.*)$")
    i = 0
    while i < len(lines):
        m = start_re.match(lines[i].strip())
        if not m:
            i += 1
            continue
        kind, label = m.group(1), m.group(2).strip()
        start = i + 1
        body = []
        i += 1
        while i < len(lines) and not lines[i].strip().startswith(FENCE):
            body.append(lines[i])
            i += 1
        if i >= len(lines):
            raise ValueError(f"{path}: unterminated {kind} block starting at line {start}")
        try:
            payload = json.loads("\n".join(body))
        except json.JSONDecodeError as exc:
            raise ValueError(f"{path}: invalid JSON in {kind} block at line {start}: {exc}") from exc
        blocks.append(dict(kind=kind, label=label, line=start, payload=payload))
        i += 1
    return blocks


def check_hand_blocks(blocks, fixtures, report: Report) -> dict:
    hand_cases = set()
    hand_tags = set()
    counts = {"handcalc": 0, "handcheck": 0, "handtrap": 0}
    if not blocks:
        report.add("T03-HAND-0", "HAND-CALC.md contains machine-readable hand vectors", False, "no blocks")
        return dict(cases=hand_cases, tags=hand_tags, counts=counts)
    for b in blocks:
        kind, line, p = b["kind"], b["line"], b["payload"]
        cid = p.get("case")
        ok = cid in fixtures
        detail = f"line {line}: case {cid!r}"
        if not ok:
            report.add(f"T03-HAND-{line}", f"HAND-CALC block {kind} at line {line}", False,
                       f"unknown case {cid!r}")
            continue
        fx = fixtures[cid]
        hand_cases.add(cid)
        hand_tags |= set(fx["tags"])
        if kind == "handcalc":
            counts["handcalc"] += 1
            ok = ok and "expected" in p
            if ok:
                ok = p["expected"] == fx["expected"]
                if ok and "input" in p:
                    ok = p["input"] == fx["input"]
                if ok and "supports" in p:
                    ok = all(int(p["supports"][k]) == int(fx["supports"][k])
                             for k in p["supports"] if k in fx["supports"])
                detail += " (handcalc expected == fixture expected)"
        elif kind == "handcheck":
            counts["handcheck"] += 1
            samples = p.get("samples")
            ok = ok and isinstance(samples, list) and len(samples) > 0
            if ok:
                for s in samples:
                    c, n, v = s.get("container"), s.get("n"), s.get("value")
                    if not isinstance(c, int) or not isinstance(n, int):
                        ok = False
                        detail = f"line {line}: invalid sample {s!r}"
                        break
                    if not (0 <= c < len(fx["expected"]) and 0 <= n < len(fx["expected"][c])):
                        ok = False
                        detail = f"line {line}: sample out of range {s!r}"
                        break
                    if float(fx["expected"][c][n]) != float(v):
                        ok = False
                        detail = (f"line {line}: sample mismatch container {c} n {n}: "
                                  f"fixture {fx['expected'][c][n]} vs hand {v}")
                        break
                if ok:
                    detail += f" ({len(samples)} samples match)"
        else:
            counts["handtrap"] += 1
            tid = p.get("trap")
            traps = {t["id"]: t for t in fx.get("traps", [])}
            ok = ok and tid in traps
            if ok and "wrong_expected" in p:
                ok = p["wrong_expected"] == traps[tid].get("wrong_expected")
            if ok and "wrong_outputs" in p:
                ok = p["wrong_outputs"] == traps[tid].get("wrong_outputs")
            detail += f" (trap {tid!r})"
        report.add(f"T03-HAND-{line}", f"HAND-CALC block {kind} at line {line}", ok, detail)
    report.add("T03-HAND-COUNT", "at least 10 hand blocks and 8 distinct hand-covered cases",
               (sum(counts.values()) >= 10 and len(hand_cases) >= 8),
               f"blocks={counts}, cases={len(hand_cases)}")
    missing = REQUIRED_HAND_TAGS - hand_tags
    report.add("T03-HAND-TAGS", "hand vectors cover offdiag + tail + D",
               not missing, f"missing tags: {sorted(missing)}" if missing else "offdiag/tail/D covered")
    return dict(cases=hand_cases, tags=hand_tags, counts=counts)




# ---------------------------------------------------------------------------
# negative validation / trap formula / independent handcheck (task-52 P2-2/P3-5/P3-6)
# ---------------------------------------------------------------------------
def trap_semantic_problems(fx: dict) -> list[str]:
    """Return problems where a trap's declared wrong value equals the correct output."""
    problems = []
    expected = [[float(v) for v in ch] for ch in fx["expected"]]
    for trap in fx.get("traps", []):
        tid = str(trap.get("id", "?"))
        if "wrong_expected" in trap:
            wrong = [[float(v) for v in ch] for ch in trap["wrong_expected"]]
            same = (len(wrong) == len(expected)
                    and all(len(wrong[c]) == len(expected[c]) and wrong[c] == expected[c]
                            for c in range(len(wrong))))
            if same:
                problems.append(f"trap {tid}: wrong_expected equals correct output")
        if "wrong_outputs" in trap:
            for c, arr in trap["wrong_outputs"].items():
                c = int(c)
                if c < len(expected) and [float(v) for v in arr] == expected[c]:
                    problems.append(f"trap {tid}: wrong_outputs[{c}] equals correct output")
    return problems


def _conv_frac(a, b):
    out = [Fraction(0)] * (len(a) + len(b) - 1)
    for i, ai in enumerate(a):
        if ai == 0:
            continue
        for j, bj in enumerate(b):
            if bj != 0:
                out[i + j] += ai * bj
    return out


def _overflow_conv_float(a, b):
    out = [0.0] * (len(a) + len(b) - 1)
    for i, ai in enumerate(a):
        for j, bj in enumerate(b):
            out[i + j] += ai * bj
    return out


def run_negative_validation(fixtures: dict, report: Report, hand_blocks: list[dict]) -> None:
    import copy as _copy

    def expect_reject(cid: str, fx: dict, needle: str) -> None:
        ok = False
        detail = ""
        try:
            validate_fixture(fx, fx["id"])
            detail = "validate_fixture unexpectedly accepted invalid fixture"
        except ValueError as exc:
            ok = needle in str(exc)
            detail = str(exc)[:240]
        report.add(cid, "invalid fixture must be rejected", ok, detail)

    base01 = _copy.deepcopy(fixtures["C01_identity_d0_1x1"])
    base04 = _copy.deepcopy(fixtures["C04_tail_conv_1x1"])
    base06 = _copy.deepcopy(fixtures["C06_row_col_asymmetry"])
    base09 = _copy.deepcopy(fixtures["C09_4_1_2_5x5_embed"])
    base14 = _copy.deepcopy(fixtures["C14_input_column_fir"])

    fx = _copy.deepcopy(base01); fx["unknown_top"] = 1
    expect_reject("T03-NEG-01", fx, "unknown fixture keys")
    fx = _copy.deepcopy(base01); fx["graph"]["unknown_graph_field"] = 1
    expect_reject("T03-NEG-02", fx, "unknown graph keys")
    fx = _copy.deepcopy(base09); fx["graph"]["a_map"][0][1] = 1
    expect_reject("T03-NEG-03", fx, "selects more than one source")
    fx = _copy.deepcopy(base09); fx["supports"]["l_g"] = int(fx["graph"]["d_samples"])
    expect_reject("T03-NEG-04", fx, "supports.l_g")
    fx = _copy.deepcopy(base09); fx["graph"]["scatter"][1] = fx["graph"]["scatter"][0]
    expect_reject("T03-NEG-05", fx, "scatter must be injective")
    fx = _copy.deepcopy(base01); fx["tags"] = list(fx["tags"]) + ["no_such_tag"]
    expect_reject("T03-NEG-06", fx, "unknown tags")
    fx = _copy.deepcopy(base01); fx["expected_len"] = int(fx["expected_len"]) + 1
    expect_reject("T03-NEG-07", fx, "expected_len")
    fx = _copy.deepcopy(base04); fx["traps"][0]["wrong_expected"] = _copy.deepcopy(fx["expected"])
    report.add("T03-NEG-08", "trap wrong equal to correct must be flagged",
               bool(trap_semantic_problems(fx)), "trap equality detected")

    # P3-5: rebuild the three trap 'wrong implementations' from formulas
    fx = base04; gr = fx["graph"]
    lg = int(fx["supports"]["l_g"])
    G = [Fraction(0)] * lg
    G[int(gr["d_samples"])] += 1
    for t, v in enumerate(gr["g"][0][0]):
        G[t] += frac(v)
    F = _conv_frac([frac(v) for v in gr["p"][0]], G)
    x = [frac(v) for v in fx["input"][0]]
    N = 4
    xp = [x[i] if i < len(x) else Fraction(0) for i in range(N)]
    yc = [sum((F[k] * xp[(n - k) % N] for k in range(min(len(F), N))), Fraction(0))
          for n in range(N)]
    wrong = [[float(v) for v in yc]]
    stored = fx["traps"][0]["wrong_expected"]
    report.add("T03-TRAPFORM-C04", "C04 trap equals circular-convolution len4 formula",
               wrong == stored, f"computed={wrong} stored={stored}")

    fx = base06; gr = fx["graph"]
    d = int(gr["d_samples"])
    lg = int(fx["supports"]["l_g"])
    M = len(gr["participants"])
    G = [[[Fraction(0)] * lg for _ in range(2)] for _ in range(2)]
    for j in range(2):
        G[j][j][d] += 1
    for a in range(M):
        for b in range(M):
            j = gr["participants"][a]; k = gr["participants"][b]
            for t, v in enumerate(gr["g"][a][b]):
                G[j][k][t] += frac(v)
    pv = [frac(gr["p"][j][0]) for j in range(2)]
    uv = [frac(fx["input"][j][0]) for j in range(2)]
    # wrong implementation: matrix multiply G*P instead of row-scaling P*G
    wrong = [[float(sum((G[j][k][0] * pv[k] * uv[k] for k in range(2)), Fraction(0)))]
             for j in range(2)]
    stored = fx["traps"][0]["wrong_expected"]
    report.add("T03-TRAPFORM-C06", "C06 trap equals G*P column-scaled formula",
               wrong == stored, f"computed={wrong} stored={stored}")

    fx = base14
    offline = ref_f64.offline(fx)
    a0 = [float(v) for v in fx["graph"]["a_fir"][0]]
    a1 = [float(v) for v in fx["graph"]["a_fir"][1]]
    wrong = {"0": _overflow_conv_float(a0, offline[0]),
             "1": _overflow_conv_float(a1, offline[1])}
    stored = fx["traps"][0]["wrong_outputs"]
    report.add("T03-TRAPFORM-C14", "C14 trap equals output-side A misapplication",
               wrong == stored, f"computed={wrong} stored={stored}")

    # P3-6: handcheck points vs ref_f64 output (independent of fixture expected)
    ok = True
    detail = ""
    checked = 0
    for b in hand_blocks:
        if b["kind"] != "handcheck":
            continue
        payload = b["payload"]
        fx = fixtures.get(payload.get("case"))
        if fx is None:
            ok = False
            detail = f"unknown handcheck case {payload.get('case')!r}"
            break
        actual = ref_f64.offline(fx)
        for s in payload["samples"]:
            c, n, v = s["container"], s["n"], s["value"]
            checked += 1
            if float(actual[c][n]) != float(v):
                ok = False
                detail = f"container {c} n {n}: ref_f64 {actual[c][n]} vs hand {v}"
                break
        if not ok:
            break
    report.add("T03-HANDCHK-F64", "handcheck samples match independent ref_f64 output",
               ok, detail or f"{checked} samples checked")


# ---------------------------------------------------------------------------
# main verification
# ---------------------------------------------------------------------------
def verify(report_path: Path | None, negative_validation_flag: bool = False) -> int:
    report = Report()
    if not INDEX_PATH.exists():
        report.add("T03-INDEX", "fixtures/index.json exists", False, str(INDEX_PATH))
        print("FATAL: index missing", file=sys.stderr)
        return 1
    index = json.loads(INDEX_PATH.read_text(encoding="utf-8"))
    report.add("T03-INDEX-SCHEMA", "fixtures/index.json schema",
               index.get("schema") == INDEX_SCHEMA, f"got {index.get('schema')!r}")
    entries = index.get("cases")
    report.add("T03-INDEX-LIST", "index cases is a non-empty list",
               isinstance(entries, list) and len(entries) > 0,
               f"{len(entries) if isinstance(entries, list) else entries}")
    fixtures = {}
    index_ids = set()
    if isinstance(entries, list):
        for entry in entries:
            cid = entry.get("id")
            rel = entry.get("file", "")
            ok = bool(cid) and bool(rel)
            path = FIXTURES_DIR / rel
            if ok and path.exists():
                data = path.read_bytes()
                ok = (sha256_bytes(data) == entry.get("sha256")
                      and len(data) == entry.get("bytes"))
                report.add(f"T03-HASH-{cid}", f"fixture {cid} sha256/bytes match index", ok,
                           "" if ok else f"{path}")
                try:
                    fixtures[cid] = json.loads(data.decode("utf-8"))
                    index_ids.add(cid)
                except (json.JSONDecodeError, UnicodeDecodeError) as exc:
                    report.add(f"T03-JSON-{cid}", f"fixture {cid} parses as UTF-8 JSON",
                               False, str(exc))
            else:
                report.add(f"T03-FILE-{cid}", f"fixture file {rel} exists", False, str(path))
    on_disk = {p.stem for p in CASES_DIR.glob("*.json")}
    report.add("T03-INVENTORY", "fixtures/cases inventory matches index",
               on_disk == index_ids, f"disk={len(on_disk)} index={len(index_ids)}")
    if not fixtures:
        print("FATAL: no fixtures loadable", file=sys.stderr)
        return 1
    tag_union = set()
    case_results = []
    for cid in sorted(fixtures):
        fx = fixtures[cid]
        cr = dict(id=cid, tags=fx.get("tags", []), ok=True)
        try:
            info = validate_fixture(fx, cid)
        except ValueError as exc:
            report.add(f"T03-SCHEMA-{cid}", f"fixture {cid} schema/consistency", False, str(exc))
            cr["ok"] = False
            case_results.append(cr)
            continue
        report.add(f"T03-SCHEMA-{cid}", f"fixture {cid} schema/consistency", True,
                   f"l_out={info['supports']['l_out']}")
        tag_union |= set(fx["tags"])
        info2, exact_z = exact_offline(fx)
        ok, detail = expected_matches_exact(fx, exact_z)
        report.add(f"T03-EXACT-{cid}",
                   f"fixture {cid} expected == {'exact' if fx['comparison']['mode'] == 'exact' else 'rounded-exact'} rational convolution",
                   ok, detail)
        cr["exact"] = ok
        problems = check_tag_semantics(fx, info2, exact_z)
        report.add(f"T03-TAGS-{cid}", f"fixture {cid} tag semantics", not problems,
                   "; ".join(problems)[:400])
        cr["tags_ok"] = not problems
        expected_floats = [[float(v) for v in ch] for ch in fx["expected"]]
        try:
            actual = ref_f64.offline(fx)
            ok, max_err, tol = compare_arrays(actual, expected_floats, fx["comparison"])
            report.add(f"T03-F64-{cid}", f"fixture {cid} binary64 offline within declaration", ok,
                       f"max_abs_err={max_err:.3e} tol={tol:.3e}")
            cr["f64"] = ok
        except Exception as exc:  # noqa: BLE001
            report.add(f"T03-F64-{cid}", f"fixture {cid} binary64 offline within declaration",
                       False, repr(exc))
            cr["f64"] = False
        stream_ok = True
        stream_detail = []
        try:
            offline_actual = ref_f64.offline(fx)
            for b in fx["streaming"]["block_sizes"]:
                streamed = ref_f64.streaming(fx, b)
                ok, max_err, tol = compare_arrays(streamed, offline_actual, fx["comparison"])
                stream_ok = stream_ok and ok
                stream_detail.append(f"B={b}:{'ok' if ok else f'FAIL({max_err:.2e})'}")
        except Exception as exc:  # noqa: BLE001
            stream_ok = False
            stream_detail.append(repr(exc))
        report.add(f"T03-STREAM-{cid}", f"fixture {cid} streaming == offline for block sizes",
                   stream_ok, " ".join(stream_detail)[:300])
        cr["streaming"] = stream_ok
        trap_probs = trap_semantic_problems(fx)
        trap_ok = not trap_probs
        if fx.get("traps"):
            report.add(f"T03-TRAP-{cid}",
                       f"fixture {cid} trap wrong-output differs from linear result",
                       trap_ok, "; ".join(trap_probs)[:300] or f"{len(fx['traps'])} trap(s)")
        cr["traps"] = trap_ok
        cr["ok"] = all(cr.get(k, True) for k in ("exact", "tags_ok", "f64", "streaming", "traps"))
        case_results.append(cr)
    missing = REQUIRED_TAGS - tag_union
    report.add("T03-COVERAGE", "required fixture tags are covered", not missing,
               f"missing: {sorted(missing)}" if missing else f"{len(tag_union)} tags")
    hand_info = dict(cases=set(), tags=set(), counts={"handcalc": 0, "handcheck": 0, "handtrap": 0})
    hand_blocks = []
    try:
        blocks = parse_hand_blocks(HANDCALC_PATH)
        hand_blocks = blocks
        hand_info = check_hand_blocks(blocks, fixtures, report)
    except ValueError as exc:
        report.add("T03-HAND-PARSE", "HAND-CALC.md machine blocks are valid", False, str(exc))
    if negative_validation_flag:
        run_negative_validation(fixtures, report, hand_blocks)
    passed = report.failures == 0
    doc = dict(
        schema="khaen-reference-verify-report/v1",
        generator="tools/verify_reference.py",
        reference_root=str(REF_ROOT),
        fixtures=case_results,
        tag_union=sorted(tag_union),
        hand=dict(cases=sorted(hand_info["cases"]), tags=sorted(hand_info["tags"]),
                  counts=hand_info["counts"]),
        checks=report.checks,
        summary=dict(total_checks=len(report.checks), failures=report.failures,
                     fixtures=len(fixtures), passed=passed),
        passed=passed,
    )
    if report_path is not None:
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(doc, indent=2, ensure_ascii=False) + "\n",
                               encoding="utf-8", newline="\n")
        print(f"report: {report_path}")
    print(f"fixtures: {len(fixtures)}, checks: {len(report.checks)}, failures: {report.failures}")
    for cr in case_results:
        flags = " ".join(f"{k}={cr.get(k)}" for k in ("exact", "tags_ok", "f64", "streaming"))
        print(f"  {'PASS' if cr['ok'] else 'FAIL'} {cr['id']} {flags}")
    print("RESULT:", "PASS" if passed else "FAIL")
    return 0 if passed else 1


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="verify KHAEN reference fixtures and hand-calc vectors")
    ap.add_argument("--report", default=None, help="write JSON verification report to this path")
    ap.add_argument("--negative-validation", action="store_true",
                    help="also run 12 negative/trap-formula/handcheck checks (task-52 P2-2/P3-5/P3-6)")
    args = ap.parse_args(argv)
    return verify(Path(args.report) if args.report else None, args.negative_validation)


if __name__ == "__main__":
    sys.exit(main())
