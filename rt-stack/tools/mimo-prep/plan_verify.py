#!/usr/bin/env python3
"""Independent T66 plan.bin / manifest verifier (offline, stdlib only).

Validates the T66/plan-arena byte contract from the OUTSIDE, without importing the
project's Rust code: header field offsets/semantics, CRC-32C over the 128-byte header
(with the crc field zeroed), SHA-256 cross-binding to the T66 compile-report.json
manifest, table offsets/sizes, path/diag/role tables, and channel-mapping semantics.

Evidence producer for task-232 (MIMO pilot dry-run).  This is NOT the T66 compiler
and does not replace the real host parse: it is a second, independent implementation
used to catch silent contract drift in plan.bin/compile-report.json.

Usage:
  python plan_verify.py --plan plan.bin --manifest compile-report.json \
      [--host-report host-report.json] [--json report.json] [--quiet]

Exit codes: 0 = all checks PASS; 1 = at least one check FAIL; 2 = usage/IO error.
"""
import argparse
import hashlib
import json
import math
import struct
import sys

PLAN_MAGIC = 0x5453_5031  # "TSP1"
ABI_VERSION = 1
HEADER_SIZE = 128
HEADER_CRC_OFFSET = 124
SAMPLE_RATES = (48000, 96000)
BLOCK_SIZES = (64, 128)
MAX_INPUT_CHANNELS = 16
MAX_OUTPUT_CHANNELS = 16
MAX_AFFINITY = 4
MAX_ROLES = 16

# CRC-32C (Castagnoli, reflected poly 0x82F63B78, init/xorout 0xFFFFFFFF).
def _crc32c_table():
    table = []
    for i in range(256):
        crc = i
        for _ in range(8):
            crc = (crc >> 1) ^ (0x82F63B78 if crc & 1 else 0)
        table.append(crc)
    return table

_CRC_TABLE = _crc32c_table()

def crc32c(data: bytes) -> int:
    crc = 0xFFFFFFFF
    for byte in data:
        crc = (crc >> 8) ^ _CRC_TABLE[(crc ^ byte) & 0xFF]
    return crc ^ 0xFFFFFFFF

def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()

class Checker:
    def __init__(self):
        self.checks = []

    def add(self, ident: str, status: str, detail: str):
        self.checks.append({"id": ident, "status": status, "detail": detail})
        return status == "PASS"

    @property
    def fails(self):
        return [c for c in self.checks if c["status"] == "FAIL"]

    @property
    def soft(self):
        return [c for c in self.checks if c["status"] in ("WARN", "NOT_VERIFIED")]

def parse_header(raw: bytes) -> dict:
    if len(raw) < HEADER_SIZE:
        raise ValueError("plan file %d B < header 128 B" % len(raw))
    # Explicit offsets (core-v1 ABI v1.1.2 errata E1; size 128 / align 64).
    buf = raw[:HEADER_SIZE]
    def u16(off):
        return struct.unpack_from("<H", buf, off)[0]
    def u32(off):
        return struct.unpack_from("<I", buf, off)[0]
    header = {
        "magic": u32(0),
        "abi_version": u16(4),
        "header_size": u16(6),
        "plan_version": u32(8),
        "sample_rate": u32(12),
        "block_size": u16(16),
        "n_in": u16(18),
        "n_out": u16(20),
        "low_band_count": u16(22),
        "path_count": u16(24),
        "diag_fir_count": u16(26),
        "max_taps": u32(28),
        "workers": buf[32],
        "flags": buf[33],
        "precision": buf[34],
        "reserved0": buf[35],
        "graph_id": buf[36:68].hex(),
        "total_coeff_bytes": u32(68),
        "path_table_offset": u32(72),
        "diag_table_offset": u32(76),
        "in_stride": u32(80),
        "out_stride": u32(84),
        "role_table_offset": u32(88),
        "role_table_count": u16(92),
        "reserved1": u16(94),
        "affinity_table_offset": u32(96),
        "affinity_count": u16(100),
        "reserved2": u16(102),
        "ctl_meta_offset": u32(104),
        "ctl_meta_len": u32(108),
        "reserved3": [u32(112), u32(116), u32(120)],
        "header_crc32c": u32(124),
    }
    return header

def parse_path_entry(raw: bytes, offset: int) -> dict:
    in_index, out_index, taps, delay = struct.unpack_from("<HHII", raw, offset)
    polarity, flags, reserved0 = struct.unpack_from("<bBH", raw, offset + 12)
    coeff_offset, coeff_len, reserved1 = struct.unpack_from("<QII", raw, offset + 16)
    return {
        "in_index": in_index, "out_index": out_index, "taps": taps,
        "delay_samples": delay, "polarity": polarity, "flags": flags,
        "reserved0": reserved0, "coeff_offset_bytes": coeff_offset,
        "coeff_len_bytes": coeff_len, "reserved1": reserved1,
    }

def parse_diag_entry(raw: bytes, offset: int) -> dict:
    out_index, flags, taps, delay, reserved0 = struct.unpack_from("<HHIII", raw, offset)
    coeff_offset, coeff_len, reserved1 = struct.unpack_from("<QII", raw, offset + 16)
    return {
        "out_index": out_index, "flags": flags, "taps": taps,
        "delay_samples": delay, "reserved0": reserved0,
        "coeff_offset_bytes": coeff_offset, "coeff_len_bytes": coeff_len,
        "reserved1": reserved1,
    }

def parse_role_entry(raw: bytes, offset: int) -> dict:
    role_id, out_index, flags, reserved0 = struct.unpack_from("<HHHH", raw, offset)
    delay, gain_q15, polarity, reserved1 = struct.unpack_from("<Ihbb", raw, offset + 8)
    peq_count, reserved2, name_hash, reserved3, reserved4 = struct.unpack_from("<HHIII", raw, offset + 16)
    return {
        "role_id": role_id, "out_index": out_index, "flags": flags,
        "reserved0": reserved0, "delay_samples": delay, "gain_q15": gain_q15,
        "polarity": polarity, "reserved1": reserved1, "peq_count": peq_count,
        "reserved2": reserved2, "name_hash": name_hash, "reserved3": reserved3,
        "reserved4": reserved4,
    }

def verify_plan(plan_path: str, manifest_path: str = None, host_report_path: str = None, quiet=False) -> dict:
    ck = Checker()
    raw = open(plan_path, "rb").read()
    ck.add("plan.exists", "PASS", "%s (%d B, sha256 %s)" % (plan_path, len(raw), hashlib.sha256(raw).hexdigest()))
    try:
        header = parse_header(raw)
    except ValueError as exc:
        ck.add("plan.header.parse", "FAIL", str(exc))
        return report(ck, plan_path, None, None, None)

    def chk(ident, cond, detail, soft=False):
        return ck.add(ident, "PASS" if cond else ("WARN" if soft else "FAIL"), detail)

    chk("magic", header["magic"] == PLAN_MAGIC,
        "magic=0x%08x expected=0x%08x" % (header["magic"], PLAN_MAGIC))
    chk("abi_version", header["abi_version"] == ABI_VERSION,
        "abi_version=%d expected=%d" % (header["abi_version"], ABI_VERSION))
    chk("header_size", header["header_size"] == HEADER_SIZE,
        "header_size=%d expected=128" % header["header_size"])
    chk("precision", header["precision"] == 0, "precision=%d (v1 only f32=0)" % header["precision"])
    reserved_zero = header["reserved0"] == 0 and header["reserved1"] == 0 and header["reserved2"] == 0 and header["reserved3"] == [0, 0, 0]
    chk("reserved_zero", reserved_zero, "reserved0/1/2/3 all-zero check")
    chk("sample_rate", header["sample_rate"] in SAMPLE_RATES,
        "sample_rate=%d (allowed 48000/96000)" % header["sample_rate"])
    chk("block_size", header["block_size"] in BLOCK_SIZES,
        "block_size=%d (allowed 64/128)" % header["block_size"])
    chk("workers", header["workers"] in (2, 4), "workers=%d (allowed 2/4)" % header["workers"])
    chk("n_in", 1 <= header["n_in"] <= MAX_INPUT_CHANNELS, "n_in=%d" % header["n_in"])
    chk("n_out", 1 <= header["n_out"] <= MAX_OUTPUT_CHANNELS, "n_out=%d" % header["n_out"])
    chk("low_band_count", 0 <= header["low_band_count"] <= 8, "low_band_count=%d (<=8)" % header["low_band_count"])
    chk("total_coeff_bytes", header["total_coeff_bytes"] <= len(raw),
        "total_coeff_bytes=%d <= file_bytes=%d" % (header["total_coeff_bytes"], len(raw)))
    chk("in_stride", header["in_stride"] > 0 and header["in_stride"] % 16 == 0,
        "in_stride=%d (multiple of 16)" % header["in_stride"])
    chk("out_stride", header["out_stride"] > 0 and header["out_stride"] % 16 == 0,
        "out_stride=%d (multiple of 16)" % header["out_stride"])
    chk("affinity_count", header["affinity_count"] <= MAX_AFFINITY,
        "affinity_count=%d (<=4)" % header["affinity_count"])
    chk("role_table_count", header["role_table_count"] <= MAX_ROLES,
        "role_table_count=%d (<=16)" % header["role_table_count"])

    crc_input = bytearray(raw[:HEADER_SIZE])
    stored_crc = header["header_crc32c"]
    crc_input[HEADER_CRC_OFFSET:HEADER_CRC_OFFSET + 4] = b"\x00\x00\x00\x00"
    computed_crc = crc32c(bytes(crc_input))
    chk("header_crc32c", stored_crc == computed_crc,
        "stored=0x%08x computed=0x%08x (CRC-32C over 128 B header, crc field zeroed)" % (stored_crc, computed_crc))

    # Table layout/semantics.
    path_off = header["path_table_offset"]
    diag_off = header["diag_table_offset"]
    role_off = header["role_table_offset"]
    path_n = header["path_count"]
    diag_n = header["diag_fir_count"]
    role_n = header["role_table_count"]
    tables_ok = True
    for name, off, count, size in (("path", path_off, path_n, 32), ("diag", diag_off, diag_n, 32), ("role", role_off, role_n, 32)):
        if count == 0:
            continue
        if off < HEADER_SIZE or off + count * size > len(raw):
            tables_ok = False
            chk("%s_table_range" % name, False,
                "%s table offset=%d count=%d size=%d file=%d" % (name, off, count, size, len(raw)))
    if tables_ok:
        chk("tables_range", True, "path@%d(%d) diag@%d(%d) role@%d(%d) inside file" % (path_off, path_n, diag_off, diag_n, role_off, role_n))

    paths = [parse_path_entry(raw, path_off + i * 32) for i in range(path_n)] if tables_ok else []
    diags = [parse_diag_entry(raw, diag_off + i * 32) for i in range(diag_n)] if tables_ok else []
    roles = [parse_role_entry(raw, role_off + i * 32) for i in range(role_n)] if tables_ok else []

    path_semantic = True
    max_coeff_end = HEADER_SIZE
    for i, p in enumerate(paths):
        ok = (p["taps"] >= 1 and p["taps"] <= header["max_taps"]
              and p["in_index"] < header["n_in"] and p["out_index"] < header["n_out"]
              and p["polarity"] in (1, -1)
              and p["reserved0"] == 0 and p["reserved1"] == 0
              and p["coeff_len_bytes"] == p["taps"] * 4
              and p["coeff_offset_bytes"] % 64 == 0
              and p["coeff_offset_bytes"] + p["coeff_len_bytes"] <= len(raw))
        if not ok:
            path_semantic = False
            chk("path[%d]" % i, False, json.dumps(p))
        max_coeff_end = max(max_coeff_end, p["coeff_offset_bytes"] + p["coeff_len_bytes"])
    if paths:
        chk("paths_semantic", path_semantic,
            "%d paths: taps<=max_taps=%d, in/out in range, polarity +-1, reserved 0, coeff len=taps*4, 64B aligned" % (path_n, header["max_taps"]))
        pairs = [(p["in_index"], p["out_index"]) for p in paths]
        chk("paths_unique", len(pairs) == len(set(pairs)), "path (in_index,out_index) pairs unique: %d" % len(pairs))
    diag_semantic = True
    for i, d in enumerate(diags):
        ok = (d["taps"] >= 1 and d["taps"] <= header["max_taps"]
              and d["out_index"] < header["n_out"]
              and d["reserved0"] == 0 and d["reserved1"] == 0
              and d["coeff_len_bytes"] == d["taps"] * 4
              and d["coeff_offset_bytes"] % 64 == 0
              and d["coeff_offset_bytes"] + d["coeff_len_bytes"] <= len(raw))
        if not ok:
            diag_semantic = False
            chk("diag[%d]" % i, False, json.dumps(d))
        max_coeff_end = max(max_coeff_end, d["coeff_offset_bytes"] + d["coeff_len_bytes"])
    if diags:
        chk("diags_semantic", diag_semantic,
            "%d diag FIRs: taps<=max_taps, out in range, reserved 0, coeff len=taps*4" % diag_n)
    role_semantic = True
    for i, r in enumerate(roles):
        ok = (r["role_id"] < MAX_ROLES and r["reserved0"] == 0 and r["reserved1"] == 0
              and r["reserved2"] == 0 and r["reserved3"] == 0 and r["reserved4"] == 0
              and r["polarity"] in (1, -1) and 0 <= r["peq_count"] <= 32
              and 0 <= r["delay_samples"] <= 9600)
        out_ok = r["out_index"] == 0xFFFF or r["out_index"] < header["n_out"]
        if not (ok and out_ok):
            role_semantic = False
            chk("role[%d]" % i, False, json.dumps(r))
    if roles:
        chk("roles_semantic", role_semantic,
            "%d roles: role_id<16, out_index<n_out or 0xffff, delay<=9600, peq<=32, reserved 0" % role_n)
    chk("coeff_area", max_coeff_end <= len(raw),
        "max coeff end=%d <= file bytes=%d" % (max_coeff_end, len(raw)))

    manifest = None
    if manifest_path:
        try:
            manifest = json.load(open(manifest_path, "r", encoding="utf-8"))
        except (OSError, ValueError) as exc:
            chk("manifest.read", False, str(exc))
        if manifest is not None:
            plan_sha = sha256_file(plan_path)
            declared = None
            graph_declared = None
            try:
                declared = manifest["plan"]["plan_sha256"]
                graph_declared = manifest["plan"]["graph_id"]
            except (KeyError, TypeError):
                declared = None
            if declared is None:
                # tolerate the artifact->host_graph_sha256 layout as a fallback binding
                graph_declared = (manifest.get("host_graph_sha256")
                                  or (manifest.get("artifact") or {}).get("host_graph_sha256")
                                  or (manifest.get("plan") or {}).get("graph_id"))
            chk("manifest.plan_sha256", (declared or "").lower() == plan_sha.lower(),
                "declared=%s actual=%s" % (declared, plan_sha))
            if graph_declared:
                chk("manifest.graph_binding", graph_declared.lower() == header["graph_id"].lower(),
                    "manifest host_graph_sha256/graph_id=%s plan header graph_id=%s" % (graph_declared, header["graph_id"]))
            chk("manifest.plan_fields", bool(manifest.get("plan")),
                "manifest has plan{} section (compile-report.json)")
    host_report = None
    if host_report_path:
        try:
            host_report = json.load(open(host_report_path, "r", encoding="utf-8"))
        except (OSError, ValueError) as exc:
            chk("host_report.read", False, str(exc))
        if host_report is not None:
            plan = host_report.get("plan") or {}
            swap = host_report.get("swap") or {}
            chk("host.plan_sha256", (plan.get("sha256") or "").lower() == sha256_file(plan_path).lower(),
                "host plan.sha256=%s actual=%s" % (plan.get("sha256"), sha256_file(plan_path)))
            chk("host.graph_id", (plan.get("graph_id") or "").lower() == header["graph_id"].lower(),
                "host plan.graph_id=%s header graph_id=%s" % (plan.get("graph_id"), header["graph_id"]))
            chk("host.header_crc32c", plan.get("header_crc32c") == "ok", "host header_crc32c=%s" % plan.get("header_crc32c"))
            chk("host.parse", plan.get("parse") == "t20-ok", "host parse=%s" % plan.get("parse"))
            chk("host.manifest", plan.get("manifest") == "verified", "host manifest=%s" % plan.get("manifest"))
            chk("host.retire_ready", swap.get("retire_ready") is True and swap.get("synthetic") is False,
                "swap.retire_ready=%s synthetic=%s source=%s" % (swap.get("retire_ready"), swap.get("synthetic"), swap.get("retire_ready_source")))
            chk("host.acks", int(swap.get("acks") or 0) >= int(header["workers"]),
                "swap.acks=%s >= workers=%d" % (swap.get("acks"), header["workers"]))
    if not quiet:
        print("plan_verify: %s" % plan_path)
        for item in ck.checks:
            print("  [%s] %-24s %s" % (item["status"], item["id"], item["detail"]))
    return report(ck, plan_path, header, paths, {"manifest": manifest, "host_report": host_report})

def report(ck, plan_path, header, paths, extra) -> dict:
    out = {
        "schema": "tspi-mimo-plan-verify/v1",
        "plan": plan_path,
        "plan_sha256": sha256_file(plan_path) if plan_path else None,
        "header": header,
        "paths": paths,
        "checks": ck.checks,
        "fails": len(ck.fails),
        "soft": len(ck.soft),
        "result": "PASS" if not ck.fails else "FAIL",
    }
    return out

def main(argv=None):
    ap = argparse.ArgumentParser(description="Independent T66 plan.bin/manifest verifier")
    ap.add_argument("--plan", required=True)
    ap.add_argument("--manifest", default=None)
    ap.add_argument("--host-report", default=None)
    ap.add_argument("--json", default=None)
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv)
    result = verify_plan(args.plan, args.manifest, args.host_report, args.quiet)
    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(result, fh, ensure_ascii=False, indent=2)
            fh.write("\n")
    if not args.quiet:
        print("plan_verify result=%s fails=%d soft=%d" % (result["result"], result["fails"], result["soft"]))
    return 0 if result["result"] == "PASS" else 1

if __name__ == "__main__":
    sys.exit(main())
