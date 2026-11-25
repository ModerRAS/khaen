#!/usr/bin/env python3
"""T28 release bundle 清单校验（04-install-release.sh / make-release-bundle.sh 共用）。

bundle 结构（schema khaen-rt-stack/release/v1）：
  <bundle>/manifest.json
  <bundle>/bin/<name>            # 可执行（khaen-rt-host, khaen-rt-daemon）
  <bundle>/units/*.service|*.target
  <bundle>/etc/*.example         # env/config 模板（可选）
  <bundle>/README.deploy.md

manifest.json 字段：schema, release_id, built_at, target (aarch64-unknown-linux-musl),
  files: [{path(相对 bundle), size, sha256, kind}], arch(可选)

用法: release_manifest.py verify <bundle> [--require-arch aarch64] [--require-elf]
输出 JSON；exit 0=PASS，3=FAIL（缺失/哈希不符/架构不符）。
"""
import json
import os
import subprocess
import sys

SCHEMA = "khaen-rt-stack/release/v1"
HERE = os.path.dirname(os.path.abspath(__file__))
CHECK_ELF = os.path.join(HERE, "check_elf.py")


def fail(msg, extra=None):
    out = {"ok": False, "error": msg}
    if extra:
        out.update(extra)
    print(json.dumps(out, ensure_ascii=False))
    sys.exit(3)


def sha256(path):
    import hashlib

    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def safe_join(bundle, rel):
    if os.path.isabs(rel) or ".." in rel.split("/") or "\\" in rel:
        fail(f"manifest 路径非法（必须是 bundle 内相对路径）: {rel}")
    return os.path.join(bundle, rel)


def main(argv):
    if len(argv) < 3 or argv[1] != "verify":
        print(json.dumps({"ok": False, "error": "用法: release_manifest.py verify <bundle> [--require-arch aarch64] [--require-elf]"}))
        return 2
    bundle = argv[2]
    require_arch = None
    if "--require-arch" in argv:
        require_arch = argv[argv.index("--require-arch") + 1]
    require_elf = "--require-elf" in argv
    mpath = os.path.join(bundle, "manifest.json")
    if not os.path.isfile(mpath):
        fail(f"bundle 缺 manifest.json: {mpath}")
    try:
        with open(mpath, encoding="utf-8") as f:
            m = json.load(f)
    except json.JSONDecodeError as e:
        fail(f"manifest.json 非法 JSON: {e}")
    if m.get("schema") != SCHEMA:
        fail(f"schema 必须是 {SCHEMA}，实际 {m.get('schema')!r}")
    if not m.get("release_id"):
        fail("manifest 缺 release_id")
    files = m.get("files")
    if not isinstance(files, list) or not files:
        fail("manifest.files 必须是非空数组")
    problems, checked, binaries = [], [], []
    listed = set()
    for ent in files:
        rel = ent.get("path")
        if not rel:
            problems.append("files[] 条目缺 path")
            continue
        listed.add(rel)
        fp = safe_join(bundle, rel)
        if not os.path.isfile(fp):
            problems.append(f"缺失文件: {rel}")
            continue
        size = os.path.getsize(fp)
        if ent.get("size") is not None and size != ent["size"]:
            problems.append(f"size 不符: {rel} manifest={ent['size']} actual={size}")
        digest = sha256(fp)
        if ent.get("sha256") and digest != ent["sha256"]:
            problems.append(f"sha256 不符: {rel} manifest={ent['sha256']} actual={digest}")
        checked.append({"path": rel, "size": size, "sha256": digest, "kind": ent.get("kind", "")})
        if ent.get("kind") == "binary":
            binaries.append(rel)
    # 额外文件提示（不阻断，但记录）
    extras = []
    for dirpath, _dirs, names in os.walk(bundle):
        for name in names:
            rel = os.path.relpath(os.path.join(dirpath, name), bundle).replace(os.sep, "/")
            if rel != "manifest.json" and rel not in listed:
                extras.append(rel)
    if require_arch or require_elf:
        for rel in binaries or [e["path"] for e in checked if "/bin/" in e["path"] or e["path"].startswith("bin/")]:
            fp = os.path.join(bundle, rel)
            if not os.path.isfile(fp):
                continue
            cmd = [sys.executable, CHECK_ELF, fp]
            if require_arch:
                cmd += ["--require-arch", require_arch]
            try:
                r = subprocess.run(cmd, capture_output=True, text=True)
            except OSError as e:
                problems.append(f"架构检查无法执行: {e}")
                continue
            info = json.loads(r.stdout) if r.stdout.strip().startswith("{") else {"ok": False, "error": r.stdout[-400:]}
            if not info.get("ok"):
                problems.append(f"架构不符: {rel} :: {info.get('error') or info}")
    if problems:
        fail("release bundle 校验失败", {"problems": problems, "checked": len(checked), "extras": extras[:20]})
    print(json.dumps({
        "ok": True,
        "bundle": os.path.abspath(bundle),
        "release_id": m["release_id"],
        "built_at": m.get("built_at"),
        "target": m.get("target"),
        "files_checked": len(checked),
        "binaries": binaries,
        "extras": extras[:20],
    }, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
