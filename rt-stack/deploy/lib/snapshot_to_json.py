#!/usr/bin/env python3
"""T28 快照组装器：把 00-snapshot-old-c.sh 采集的 TSV/raw 组装成 snapshot.json。

输入目录结构（由 00-snapshot-old-c.sh 生成）：
  meta.tsv        两列：key<TAB>value
  files.tsv       五列：kind<TAB>path<TAB>size<TAB>sha256<TAB>status
  units.tsv       四列：unit<TAB>active<TAB>enabled<TAB>note
  procs.tsv       三列：pid<TAB>comm<TAB>cmdline
  runtime.tsv     四列：path<TAB>exists<TAB>size<TAB>sha256
  raw/            各只读命令原始输出（*.txt）与 *.exit
输出：<dir>/snapshot.json（schema tspi-rt-stack/old-c-snapshot/v1）
只读输入；exit 0=成功，2=结构缺失（缺 meta/files），3=快照自检失败（integrity_equal=false）。
"""
import hashlib
import json
import os
import sys

SCHEMA = "tspi-rt-stack/old-c-snapshot/v1"


def read_tsv(path, min_cols):
    rows = []
    if not os.path.isfile(path):
        return None
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line:
                continue
            parts = line.split("\t")
            while len(parts) < min_cols:
                parts.append("")
            rows.append(parts)
    return rows


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main(argv):
    d = argv[1] if len(argv) > 1 else "."
    meta_rows = read_tsv(os.path.join(d, "meta.tsv"), 2)
    files = read_tsv(os.path.join(d, "files.tsv"), 5)
    units = read_tsv(os.path.join(d, "units.tsv"), 4)
    procs = read_tsv(os.path.join(d, "procs.tsv"), 3)
    runtime = read_tsv(os.path.join(d, "runtime.tsv"), 4)
    if meta_rows is None or files is None:
        print(json.dumps({"ok": False, "error": "缺 meta.tsv 或 files.tsv；快照未按契约采集"}))
        return 2
    meta = {r[0]: r[1] for r in meta_rows}
    raw_dir = os.path.join(d, "raw")
    raw_files = []
    if os.path.isdir(raw_dir):
        for name in sorted(os.listdir(raw_dir)):
            fp = os.path.join(raw_dir, name)
            if os.path.isfile(fp):
                raw_files.append({
                    "name": name,
                    "size": os.path.getsize(fp),
                    "sha256": sha256_file(fp),
                })
    out = {
        "schema": SCHEMA,
        "generated_at": meta.get("generated_at"),
        "label": meta.get("label"),
        "meta": meta,
        "files": [
            {"kind": r[0], "path": r[1], "size": r[2], "sha256": r[3], "status": r[4]}
            for r in (files or [])
        ],
        "units": [
            {"unit": r[0], "active": r[1], "enabled": r[2], "note": r[3]}
            for r in (units or [])
        ],
        "processes": [
            {"pid": r[0], "comm": r[1], "cmdline": r[2]} for r in (procs or [])
        ],
        "runtime_files": [
            {"path": r[0], "exists": r[1], "size": r[2], "sha256": r[3]}
            for r in (runtime or [])
        ],
        "raw_files": raw_files,
        "missing": [f["path"] for f in [
            {"path": r[1], "status": r[4]} for r in (files or [])
        ] if f["status"] != "OK"],
        "integrity_equal": meta.get("integrity_equal", "UNKNOWN"),
    }
    with open(os.path.join(d, "snapshot.json"), "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=1)
        f.write("\n")
    if out["integrity_equal"] == "false":
        print(json.dumps({"ok": False, "error": "快照期间状态变化（integrity_equal=false），结果不可作为基线", "snapshot": "snapshot.json"}))
        return 3
    print(json.dumps({
        "ok": True,
        "snapshot": "snapshot.json",
        "files": len(out["files"]),
        "units": len(out["units"]),
        "processes": len(out["processes"]),
        "runtime_files": len(out["runtime_files"]),
        "raw_files": len(raw_files),
        "missing": out["missing"],
        "integrity_equal": out["integrity_equal"],
    }, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
