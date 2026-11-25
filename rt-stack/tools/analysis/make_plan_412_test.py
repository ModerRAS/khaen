#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""make_plan_412_test.py -- generate a T66 ABI v1 *test* plan for a 4.1.2 (8-container,
7-role) plan-arena graph, used to validate the AES67 7ch 4.1.2 capture path.

Layout mirrors the host's own TestPlan::multichannel_412 (rt-host main.rs):
  container slots / role out_index = 0,1,2,3,4,5,7  (ch6 empty, LFE at ch7)
  role table: role_id = 0..6, out_index = same, gain_q15 = 32767, no delay/polarity/IIR
  path table (diagonal G baseline, sorted by (out,in)): taps=2, coeffs=[gain, 0]
  diag table (P FIR per role, sorted by out): taps=1, coeffs=[1.0]
Header: magic TSP1, ABI v1, 48 kHz / 128, workers=4, flags=0, ctl_meta empty,
coefficients appended as one 64-byte-aligned blob at the file tail.

This is an independent implementation (no project code import); it is intentionally a
*test fixture* generator, NOT the T66 compiler.

Modes:
  identity : all 7 role gains = 1.0 (bit-exact identity plan; in_index == out_index)
  gains    : FL=1.0 FR=1.0 SL=0.5 SR=0.5 TFL=2.0 TFR=2.0 LFE=1.0
             (per-role distinct gain -> proves AES67 7ch -> role/container -> DSP -> ALAC mapping)
  from-512 : same 7 output slots, wire inputs 0,1,4,5,6,7,3 (NSP 5.1.2 content order;
             center wire index 2 is not a path). graph_id seed is tspi-plan-512-to-412-v1.

Usage: python3 make_plan_412_test.py --mode identity --out plan-412-identity.bin [--json out.json]
"""
import argparse
import hashlib
import json
import struct
import sys

MAGIC = 0x5453_5031           # "TSP1"
ABI_V1 = 1
HEADER_SIZE = 128
RATE = 48000
BLOCK = 128
WORKERS = 4
MAX_TAPS = 2
CONTAINERS = 8                # 4.1.2 8-channel container
ROLE_SLOTS = [0, 1, 2, 3, 4, 5, 7]
GAIN_MAP = {"FL": 1.0, "FR": 1.0, "SL": 0.5, "SR": 0.5, "TFL": 2.0, "TFR": 2.0, "LFE": 1.0}
ROLE_NAMES = ["FL", "FR", "SL", "SR", "TFL", "TFR", "LFE"]

# ---- CRC-32C (Castagnoli, reflected 0x82F63B78), same check as the host header CRC ----
def _crc32c_table():
    table = []
    for i in range(256):
        crc = i
        for _ in range(8):
            crc = (crc >> 1) ^ (0x82F63B78 if crc & 1 else 0)
        table.append(crc)
    return table

_CRC_TABLE = _crc32c_table()

def crc32c(data):
    crc = 0xFFFFFFFF
    for byte in data:
        crc = (crc >> 8) ^ _CRC_TABLE[(crc ^ byte) & 0xFF]
    return crc ^ 0xFFFFFFFF

def _pad64(buf):
    pad = (-len(buf)) % 64
    return buf + b"\x00" * pad

def build_plan(mode):
    if mode == "from-512":
        # out slots stay ROLE_SLOTS; in is NSP 5.1.2 wire order (C=2 is absent).
        in_indices = [0, 1, 4, 5, 6, 7, 3]
        gains = [1.0] * 7
        graph_seed = "tspi-plan-512-to-412-v1"
    elif mode == "identity":
        in_indices = list(ROLE_SLOTS)
        gains = [1.0] * 7
        graph_seed = "tspi-plan-412-7ch-identity-v1"
    else:
        in_indices = list(ROLE_SLOTS)
        gains = [GAIN_MAP[n] for n in ROLE_NAMES]
        graph_seed = "tspi-plan-412-7ch-gains-v1"
    # coefficient blobs in table order: paths first, then diags; each 64-byte aligned in region
    region = bytearray()
    offsets = []
    for gain in gains:
        off = (-len(region)) % 64
        region += b"\x00" * off
        offsets.append(len(region))
        region += struct.pack("<2f", gain, 0.0)          # G baseline: taps=2, [gain, 0]
    for _ in gains:
        off = (-len(region)) % 64
        region += b"\x00" * off
        offsets.append(len(region))
        region += struct.pack("<f", 1.0)                  # P FIR: taps=1, [1.0]
    n_path = n_diag = n_role = 7
    path_off = HEADER_SIZE
    diag_off = path_off + n_path * 32
    role_off = diag_off + n_diag * 32
    ctl_off = role_off + n_role * 32

    # path entries: sorted by (out_index, in_index), diagonal baseline
    paths = bytearray()
    for i, slot in enumerate(ROLE_SLOTS):
        paths += struct.pack("<HHII", in_indices[i], slot, 2, 0)  # in,out,taps,delay
        paths += struct.pack("<bBH", 1, 0x01, 0)                  # polarity, flags=diag_baseline, res0
        paths += struct.pack("<QII", offsets[i], 2 * 4, 0)        # coeff_off,len,res1
    # diag entries: sorted by out_index
    diags = bytearray()
    for i, slot in enumerate(ROLE_SLOTS):
        diags += struct.pack("<HHIII", slot, 0, 1, 0, 0)          # out,flags,taps,delay,res0
        diags += struct.pack("<QII", offsets[n_path + i], 1 * 4, 0)
    # role entries: role_id = table order
    roles = bytearray()
    for i, slot in enumerate(ROLE_SLOTS):
        roles += struct.pack("<HHHH", i, slot, 0x0001, 0)         # role_id, out, flags=ENABLED, res0
        roles += struct.pack("<Ihbb", 0, 32767, 1, 0)             # delay, gain_q15, polarity, res1
        roles += struct.pack("<HHIII", 0, 0, 0, 0, 0)             # peq, res2, name_hash, res3, res4

    graph_id = hashlib.sha256(graph_seed.encode()).digest()
    header = bytearray(HEADER_SIZE)
    struct.pack_into("<I", header, 0, MAGIC)
    struct.pack_into("<H", header, 4, ABI_V1)
    struct.pack_into("<H", header, 6, HEADER_SIZE)
    struct.pack_into("<I", header, 8, 0)                     # plan_version
    struct.pack_into("<I", header, 12, RATE)
    struct.pack_into("<H", header, 16, BLOCK)
    struct.pack_into("<H", header, 18, 8)                    # n_in (4.1.2 strict 8ch graph input)
    struct.pack_into("<H", header, 20, CONTAINERS)            # n_out
    struct.pack_into("<H", header, 22, 2)                     # low_band_count (same as T66 plans)
    struct.pack_into("<H", header, 24, n_path)
    struct.pack_into("<H", header, 26, n_diag)
    struct.pack_into("<I", header, 28, MAX_TAPS)
    header[32] = WORKERS
    header[33] = 0                                            # flags
    header[34] = 0                                            # precision f32
    header[35] = 0
    header[36:68] = graph_id
    struct.pack_into("<I", header, 68, len(region))           # total_coeff_bytes (tail blob)
    struct.pack_into("<I", header, 72, path_off)
    struct.pack_into("<I", header, 76, diag_off)
    struct.pack_into("<I", header, 80, BLOCK)                 # in_stride
    struct.pack_into("<I", header, 84, BLOCK)                 # out_stride
    struct.pack_into("<I", header, 88, role_off)
    struct.pack_into("<H", header, 92, n_role)
    struct.pack_into("<H", header, 94, 0)                     # reserved1
    struct.pack_into("<I", header, 96, 0)                     # affinity offset
    struct.pack_into("<H", header, 100, 0)                    # affinity count
    struct.pack_into("<H", header, 102, 0)                    # reserved2
    struct.pack_into("<I", header, 104, ctl_off)              # ctl_meta_offset
    struct.pack_into("<I", header, 108, 0)                    # ctl_meta_len
    struct.pack_into("<III", header, 112, 0, 0, 0)            # reserved3
    struct.pack_into("<I", header, 124, 0)                    # crc placeholder
    crc = crc32c(bytes(header))
    struct.pack_into("<I", header, 124, crc)

    plan = bytes(header) + bytes(paths) + bytes(diags) + bytes(roles) + bytes(region)
    meta = {
        "mode": mode, "bytes": len(plan), "sha256": hashlib.sha256(plan).hexdigest(),
        "graph_id": graph_id.hex(), "abi_version": ABI_V1, "n_in": 8, "n_out": CONTAINERS,
        "role_slots": ROLE_SLOTS, "in_indices": in_indices, "gains": gains,
        "tables": {"path": path_off, "diag": diag_off, "role": role_off, "ctl_meta": ctl_off},
        "coeff_offsets": offsets, "total_coeff_bytes": len(region), "header_crc32c": crc,
    }
    return plan, meta

def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", choices=["identity", "gains", "from-512"], default="identity")
    ap.add_argument("--out", required=True)
    ap.add_argument("--json", default=None)
    args = ap.parse_args(argv)
    plan, meta = build_plan(args.mode)
    with open(args.out, "wb") as fh:
        fh.write(plan)
    print(json.dumps(meta, indent=1))
    if args.json:
        json.dump(meta, open(args.json, "w"), indent=1)
    return 0

if __name__ == "__main__":
    sys.exit(main())
