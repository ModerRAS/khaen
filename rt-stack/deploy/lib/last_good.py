#!/usr/bin/env python3
"""T28 自研 last-good 记录校验（07-mark-last-good.sh 写 / 08-rollback.sh 读）。

schema khaen-rt-stack/last-good/v1：
  release_id, release_dir, manifest_sha256, binary_sha256{name->hash}, units_sha256{unit->hash},
  verified_at, verify_report, c_policy, previous{release_id,release_dir} | null
用法:
  last_good.py validate <last-good.json> [--require-release]
  last_good.py show <last-good.json> <field>
exit 0 OK；3 无效（字段缺失/schema 不符/release 目录不可用）。
"""
import json
import os
import sys

SCHEMA = "khaen-rt-stack/last-good/v1"
C_POLICY_MUST = "never-camilladsp"


def out(obj, code=0):
    print(json.dumps(obj, ensure_ascii=False))
    sys.exit(code)


def load(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except OSError as e:
        out({"ok": False, "error": f"last-good 不可读: {e}"}, 3)
    except json.JSONDecodeError as e:
        out({"ok": False, "error": f"last-good 不是合法 JSON: {e}"}, 3)


def main(argv):
    if len(argv) < 3 or argv[1] not in ("validate", "show"):
        out({"ok": False, "error": "用法: last_good.py validate|show <file> [field|--require-release]"}, 2)
    d = load(argv[2])
    if argv[1] == "show":
        field = argv[3] if len(argv) > 3 else ""
        out({"ok": True, "field": field, "value": d.get(field)})
    problems = []
    if d.get("schema") != SCHEMA:
        problems.append(f"schema 必须是 {SCHEMA}")
    for k in ("release_id", "release_dir", "verified_at", "binary_sha256", "units_sha256"):
        if not d.get(k):
            problems.append(f"缺字段 {k}")
    if not isinstance(d.get("binary_sha256"), dict) or not d.get("binary_sha256"):
        problems.append("binary_sha256 必须为非空对象")
    if d.get("c_policy") != "never-camilladsp":
        problems.append("c_policy 必须为 never-camilladsp（P1 禁止 C 回退）")
    if "--require-release" in argv:
        rd = d.get("release_dir") or ""
        if not os.path.isdir(rd):
            problems.append(f"release_dir 不存在: {rd}")
        elif not os.path.isfile(os.path.join(rd, "manifest.json")):
            problems.append(f"release_dir 缺 manifest.json: {rd}")
        else:
            for name, want in (d.get("binary_sha256") or {}).items():
                fp = os.path.join(rd, "bin", name)
                if not os.path.isfile(fp):
                    problems.append(f"release 缺 binary: {fp}")
                    continue
                import hashlib

                h = hashlib.sha256()
                with open(fp, "rb") as f:
                    for chunk in iter(lambda: f.read(1 << 20), b""):
                        h.update(chunk)
                if h.hexdigest() != want:
                    problems.append(f"binary hash 不符: {name}")
    if problems:
        out({"ok": False, "problems": problems, "release_id": d.get("release_id")}, 3)
    out({
        "ok": True,
        "release_id": d.get("release_id"),
        "release_dir": d.get("release_dir"),
        "verified_at": d.get("verified_at"),
        "previous": d.get("previous"),
        "c_policy": d.get("c_policy"),
    })
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
