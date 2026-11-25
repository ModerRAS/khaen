#!/usr/bin/env python3
"""T28/T147 二进制架构检查：ELF / PE 头解析（纯 stdlib，不解包/不执行）。

用法: check_elf.py <file> [--require-arch aarch64|aarch64-musl|x86_64]
输出 JSON；exit 0=匹配/仅报告，3=要求架构不匹配或非可部署格式，2=文件不可读。
"""
import hashlib
import json
import os
import struct
import sys

EM_AARCH64 = 183
EM_X86_64 = 62
PE_MACHINES = {0xAA64: "aarch64", 0x8664: "x86_64", 0x014C: "x86"}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def inspect(path):
    with open(path, "rb") as f:
        head = f.read(64)
        f.seek(0)
        data = f.read(4096)
    out = {
        "path": os.path.basename(path),
        "size": os.path.getsize(path),
        "sha256": sha256(path),
    }
    if len(head) >= 20 and head[:4] == b"\x7fELF":
        ei_class, ei_data = head[4], head[5]
        e_machine = struct.unpack_from("<H" if ei_data == 1 else ">H", head, 18)[0]
        out["format"] = "elf"
        out["bits"] = 64 if ei_class == 2 else 32
        out["endian"] = "little" if ei_data == 1 else "big"
        out["machine"] = e_machine
        out["arch"] = {EM_AARCH64: "aarch64", EM_X86_64: "x86_64"}.get(e_machine, f"elf-machine-{e_machine}")
        # 动态/静态判断：有 PT_INTERP 视为动态（ELF64 头 64B + phdr）
        try:
            if ei_class == 2:
                e_phoff = struct.unpack_from("<Q" if ei_data == 1 else ">Q", head, 32)[0]
                e_phentsize, e_phnum = struct.unpack_from("<HH" if ei_data == 1 else ">HH", head, 54)
                out["phnum"] = e_phnum
                out["static_hint"] = "unknown"
                if e_phoff and e_phnum and e_phoff < 65536:
                    out["static_hint"] = "check-readelf-needed"
            else:
                out["static_hint"] = "unknown"
        except struct.error:
            pass
    elif len(data) >= 0x40 and data[:2] == b"MZ":
        pe_off = struct.unpack_from("<I", data, 0x3C)[0]
        if 0 < pe_off < len(data) - 6 and data[pe_off:pe_off + 4] == b"PE\x00\x00":
            machine = struct.unpack_from("<H", data, pe_off + 4)[0]
            out["format"] = "pe"
            out["machine"] = machine
            out["arch"] = PE_MACHINES.get(machine, f"pe-machine-{machine:#x}")
        else:
            out["format"] = "pe-invalid"
            out["arch"] = "unknown"
    else:
        out["format"] = "unknown"
        out["arch"] = "unknown"
    return out


def main(argv):
    if len(argv) < 2:
        print(json.dumps({"ok": False, "error": "用法: check_elf.py <file> [--require-arch ARCH]"}))
        return 2
    path = argv[1]
    want = None
    if "--require-arch" in argv:
        want = argv[argv.index("--require-arch") + 1]
    if not os.path.isfile(path):
        print(json.dumps({"ok": False, "error": f"文件不存在: {path}"}))
        return 2
    info = inspect(path)
    info["ok"] = True
    if want:
        # aarch64-musl 要求 ELF aarch64；不允许用 Windows PE 冒充板端产物。
        ok = info.get("arch") == "aarch64" and info.get("format") == "elf"
        info["required_arch"] = want
        info["arch_ok"] = ok
        info["ok"] = ok
        if not ok:
            info["error"] = f"要求 {want}（ELF aarch64），实际 {info.get('format')}/{info.get('arch')}"
    print(json.dumps(info, ensure_ascii=False))
    return 0 if info["ok"] else 3


if __name__ == "__main__":
    sys.exit(main(sys.argv))
