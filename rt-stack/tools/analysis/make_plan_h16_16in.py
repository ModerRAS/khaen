#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""make_plan_h16_16in.py -- generate a T66 ABI v1 identity plan for a 16-in /
12-role / 22-out H16 plan-arena graph (NSP 16ch = 2x8 flow AES67 capture).

Why this file exists
--------------------
The 7ch 4.1.2 userspace path uses make_plan_412_test.py (8-in / 7-role / 8-out).
The 16ch AES67 path needs plan.n_in=16 so khaen-rt-host derives
graph_input_channels=16 (aes67_rx adapters are limited by plan.n_in).  This is
an independent fixture generator (no project code import), mirroring the T66
ABI v1 layout documented by make_plan_412_test.py; it never patches a binary.

Layout (H16 container order, see artifacts/audio/aes67-16ch-20261122/01-DESIGN.md §4.1):
  graph ch / container slot -> role
  0 FL, 1 FR, 2 C, 3 LFE, 4 SL, 5 SR, 6 BL, 7 BR,
  10 TFL, 11 TFR, 14 TRL, 15 TRR
  8 Lw, 9 Rw, 12 Aux12, 13 Aux13 are non-logical in phase 1 (no role).
  role --diag path--> out container = role index (0..11) for the identity fixture.
  source(graph ch) of each role = its H16 container slot.

Tables: diagonal G baseline path (taps=2, coeffs=[gain, 0]) + P diag FIR per role
(taps=1, coeffs=[1.0]) + role table (gain 32767).  For gain=1.0 the G cell
(gain - delta_D) is all zero, so output = P(A_map(source)) = source (identity).

Usage:
  python3 make_plan_h16_16in.py --out plan-h16-16in-identity.bin [--json meta.json]
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
N_IN = 16
N_ROLE = 16
DEFAULT_N_OUT = 22

# 9.1.6 -> H16 container order -> MOTU USB playback channel mapping (user-confirmed,
# MOTU optical port switched to ADAT at 48 kHz so USB ch10-15 carry ADAT Out 1-6).
#   analog (USB ch0-9 = Main1-2 + Line3-10): L=0, R=1, Ls=2, Rs=3, Ltf=4, Rtf=5,
#                                            C=6, LFE=7 (SUB1/Line Out 8 convention),
#                                            Ltr=8, Rtr=9
#   ADAT  (USB ch10-17 = ADAT Out 1-8; 6 used, ch16/17 idle):
#                                            Lrs=10, Rrs=11, Lw=12, Rw=13, Ltm=14, Rtm=15
# 4.1.2 regression mapping is unchanged and independent of this plan:
#   FL=0, FR=1, SL=2, SR=3, HFL=4, HFR=5, SUB1=7 (ch6 and 8-21 silent).
# (role name, graph input channel = H16 container slot, MOTU USB ch, output path)
ROLES = [
    ("FL", 0, 0, "analog"),
    ("FR", 1, 1, "analog"),
    ("C", 2, 6, "analog"),
    ("LFE", 3, 7, "analog"),
    ("SL", 4, 2, "analog"),
    ("SR", 5, 3, "analog"),
    ("BL", 6, 10, "adat"),
    ("BR", 7, 11, "adat"),
    ("TFL", 10, 4, "analog"),
    ("TFR", 11, 5, "analog"),
    ("TRL", 14, 8, "analog"),
    ("TRR", 15, 9, "analog"),
    # aux roles: non-logical H16 container channels (Lw/Rw/Ltm/Rtm) still carry real
    # 9.1.6 program audio; routed to ADAT channels instead of "unmapped".
    ("AUX_Lw", 8, 12, "adat"),
    ("AUX_Rw", 9, 13, "adat"),
    ("AUX_Ltm", 12, 14, "adat"),
    ("AUX_Rtm", 13, 15, "adat"),
]

# ---- CRC-32C (Castagnoli, reflected 0x82F63B78), same as the host header CRC ----
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


def build_plan(mode, n_out):
    if n_out < N_ROLE or n_out > 22:
        raise SystemExit("n_out must be in [12, 22] (host MAX_OUTPUT_CHANNELS=22)")
    gains = [1.0] * N_ROLE if mode == "identity" else [
        1.0, 0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.25, 0.2, 0.15, 0.1, 0.08, 0.06, 0.04, 0.02,
    ]
    n_path = n_diag = n_role = N_ROLE
    path_off = HEADER_SIZE
    diag_off = path_off + n_path * 32
    role_off = diag_off + n_diag * 32
    ctl_off = role_off + n_role * 32

    # T20 requires the path table sorted by (out_index, in_index) and the diag
    # table sorted by out_index; the role table keeps role_id = 0..N-1 order.
    # Our 9.1.6 -> device-channel scatter is not monotonic, so sort explicitly and
    # lay the coefficient blob out in table order.
    path_entries = sorted(
        ((role_id, name, source, out, physical) for role_id, (name, source, out, physical) in enumerate(ROLES)),
        key=lambda entry: (entry[3], entry[2]),
    )
    diag_entries = sorted(path_entries, key=lambda entry: entry[3])

    # coefficient blob: paths first (path table order), then diags (diag table order)
    region = bytearray()
    path_offsets = []
    for role_id, _name, _source, _out, _physical in path_entries:
        off = (-len(region)) % 64
        region += b"\x00" * off
        path_offsets.append(len(region))
        region += struct.pack("<2f", gains[role_id], 0.0)  # G baseline: taps=2, [gain, 0]
    diag_offsets = []
    for _role_id, _name, _source, _out, _physical in diag_entries:
        off = (-len(region)) % 64
        region += b"\x00" * off
        diag_offsets.append(len(region))
        region += struct.pack("<f", 1.0)                   # P FIR: taps=1, [1.0]

    paths = bytearray()
    for offset, (role_id, _name, source, out, _physical) in zip(path_offsets, path_entries):
        paths += struct.pack("<HHII", source, out, 2, 0)  # in,out,taps,delay
        paths += struct.pack("<bBH", 1, 0x01, 0)          # polarity, flags=diag_baseline, res0
        paths += struct.pack("<QII", offset, 2 * 4, 0)    # coeff_off,len,res1
    diags = bytearray()
    for offset, (_role_id, _name, _source, out, _physical) in zip(diag_offsets, diag_entries):
        diags += struct.pack("<HHIII", out, 0, 1, 0, 0)   # out,flags,taps,delay,res0
        diags += struct.pack("<QII", offset, 1 * 4, 0)
    roles = bytearray()
    for i, (_name, _source, out, _physical) in enumerate(ROLES):
        roles += struct.pack("<HHHH", i, out, 0x0001, 0)  # role_id, out, flags=ENABLED, res0
        roles += struct.pack("<Ihbb", 0, 32767, 1, 0)     # delay, gain_q15, polarity, res1
        roles += struct.pack("<HHIII", 0, 0, 0, 0, 0)     # peq, res2, name_hash, res3, res4

    graph_id = hashlib.sha256(("tspi-plan-h16-16in-%s-v1" % mode).encode()).digest()
    header = bytearray(HEADER_SIZE)
    struct.pack_into("<I", header, 0, MAGIC)
    struct.pack_into("<H", header, 4, ABI_V1)
    struct.pack_into("<H", header, 6, HEADER_SIZE)
    struct.pack_into("<I", header, 8, 0)                     # plan_version
    struct.pack_into("<I", header, 12, RATE)
    struct.pack_into("<H", header, 16, BLOCK)
    struct.pack_into("<H", header, 18, N_IN)                 # n_in = 16 (graph input ch)
    struct.pack_into("<H", header, 20, n_out)                # n_out = 22 (MOTU playback)
    struct.pack_into("<H", header, 22, 2)                    # low_band_count
    struct.pack_into("<H", header, 24, n_path)
    struct.pack_into("<H", header, 26, n_diag)
    struct.pack_into("<I", header, 28, MAX_TAPS)
    header[32] = WORKERS
    header[33] = 0                                           # flags
    header[34] = 0                                           # precision f32
    header[35] = 0
    header[36:68] = graph_id
    struct.pack_into("<I", header, 68, len(region))          # total_coeff_bytes (tail blob)
    struct.pack_into("<I", header, 72, path_off)
    struct.pack_into("<I", header, 76, diag_off)
    struct.pack_into("<I", header, 80, BLOCK)                # in_stride
    struct.pack_into("<I", header, 84, BLOCK)                # out_stride
    struct.pack_into("<I", header, 88, role_off)
    struct.pack_into("<H", header, 92, n_role)
    struct.pack_into("<H", header, 94, 0)                    # reserved1
    struct.pack_into("<I", header, 96, 0)                    # affinity offset
    struct.pack_into("<H", header, 100, 0)                   # affinity count
    struct.pack_into("<H", header, 102, 0)                   # reserved2
    struct.pack_into("<I", header, 104, ctl_off)             # ctl_meta_offset
    struct.pack_into("<I", header, 108, 0)                   # ctl_meta_len
    struct.pack_into("<III", header, 112, 0, 0, 0)           # reserved3
    struct.pack_into("<I", header, 124, 0)                   # crc placeholder
    struct.pack_into("<I", header, 124, crc32c(bytes(header)))

    plan = bytes(header) + bytes(paths) + bytes(diags) + bytes(roles) + bytes(region)
    meta = {
        "mode": mode,
        "bytes": len(plan),
        "sha256": hashlib.sha256(plan).hexdigest(),
        "graph_id": graph_id.hex(),
        "abi_version": ABI_V1,
        "n_in": N_IN,
        "n_out": n_out,
        "n_role": n_role,
        "roles": [{"role_id": i, "role": name, "source_ch": source,
                   "out_container": out, "physical_speaker": physical}
                  for i, (name, source, out, physical) in enumerate(ROLES)],
        "mapping_note": "9.1.6 -> H16 container -> MOTU USB ch with optical port in ADAT mode: analog ch0-9 (L,R,Ls,Rs,Ltf,Rtf,C,LFE,Ltr,Rtr), ADAT ch10-15 (Lrs,Rrs,Lw,Rw,Ltm,Rtm); USB ch16/17 idle; 4.1.2 mapping unchanged",
        "output_paths": {"analog_usb": [0,1,2,3,4,5,6,7,8,9], "adat_usb": [10,11,12,13,14,15]},
        "file_channels_h16_order": [
            "L", "R", "C", "LFE", "Ls", "Rs", "Lrs", "Rrs",
            "Lw", "Rw", "Ltf", "Rtf", "Aux12", "Aux13", "Ltr", "Rtr",
        ],
        "tables": {"path": path_off, "diag": diag_off, "role": role_off, "ctl_meta": ctl_off},
        "path_entries": [{"role_id": e[0], "role": e[1], "source_ch": e[2], "out_container": e[3]} for e in path_entries],
        "diag_entries": [{"role_id": e[0], "role": e[1], "source_ch": e[2], "out_container": e[3]} for e in diag_entries],
        "gains": gains,
        "total_coeff_bytes": len(region),
        "header_crc32c": struct.unpack_from("<I", header, 124)[0],
    }
    return plan, meta


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", choices=["identity", "gains"], default="identity")
    ap.add_argument("--n-out", type=int, default=DEFAULT_N_OUT)
    ap.add_argument("--out", required=True)
    ap.add_argument("--json", default=None)
    args = ap.parse_args(argv)
    plan, meta = build_plan(args.mode, args.n_out)
    with open(args.out, "wb") as fh:
        fh.write(plan)
    print(json.dumps(meta, indent=1, ensure_ascii=False))
    if args.json:
        with open(args.json, "w") as fh:
            json.dump(meta, fh, indent=1, ensure_ascii=False)
    return 0


if __name__ == "__main__":
    sys.exit(main())
