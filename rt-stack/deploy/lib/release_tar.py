#!/usr/bin/env python3
"""F1（2026-11-18 板窗缺陷修复）：可复现 tar 打包 + 归档成员模式校验。

背景（rt-stack/evidence/board-window-20261118/deploy/FINDINGS.md F1）：
  构建机 Windows（Git Bash/NTFS）上 chmod +x 不改变 tar 记录的 POSIX 模式；
  git archive 出的 deploy/bin/*.sh 在包内是 0664，板端 tar xzf 后直接运行 rc=126
  （T71 首次 05 入口 Permission denied，零变更）。
本工具在**写归档时显式设置成员模式**（不依赖宿主 chmod/tar），并在打包后校验归档内
必需脚本带 owner-exec 位；Linux 上由调用方追加解包 test -x（见 make-deploy-package.sh）。

用法:
  release_tar.py pack   --root DIR --out ARCHIVE.tgz [--exec GLOB]... [--json OUT]
  release_tar.py verify --archive ARCHIVE.tgz [--require-exec GLOB]... [--sums FILE] [--json OUT]
  release_tar.py list   --archive ARCHIVE.tgz

模式规则：
  目录 0755；命中 --exec（相对 root 的 glob，fnmatch；* 可跨 /）的普通文件 0755；
  其余普通文件 0644；符号链接保留目标（不跟随）。uid/gid=0、uname/gname 空、
  GNU tar 格式、gzip mtime=0（归档头不引入构建时间；成员 mtime 取源文件）。
退出码：0 PASS；2 用法/输入缺失；3 校验失败。
"""
import fnmatch
import gzip
import hashlib
import json
import os
import sys
import tarfile

SCHEMA = "tspi-rt-stack/deploy-tar/v1"
EXEC_MODE = 0o755
FILE_MODE = 0o644
DIR_MODE = 0o755


def die(msg, code=2):
    print(json.dumps({"ok": False, "error": msg, "schema": SCHEMA}, ensure_ascii=False))
    return code


def parse_args(argv):
    if len(argv) < 2:
        return None, {}
    cmd = argv[1]
    opts, rest = {}, []
    i = 2
    while i < len(argv):
        a = argv[i]
        if a in ("--root", "--out", "--archive", "--json", "--sums", "--exec", "--require-exec"):
            if i + 1 >= len(argv):
                return cmd, None
            opts.setdefault(a, []).append(argv[i + 1])
            i += 2
            continue
        rest.append(a)
        i += 1
    opts["_rest"] = rest
    return cmd, opts


def one(opts, key):
    vals = opts.get(key) or []
    return vals[-1] if vals else None


def matches(rel, patterns):
    return any(fnmatch.fnmatchcase(rel, p) for p in patterns)


def add_tree(tar, root, exec_patterns):
    root = os.path.abspath(root)
    entries = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames.sort()
        filenames.sort()
        rel_dir = os.path.relpath(dirpath, root).replace(os.sep, "/")
        for name in dirnames:
            fp = os.path.join(dirpath, name)
            rel = name if rel_dir == "." else rel_dir + "/" + name
            entries.append(("dir", rel, fp))
        for name in filenames:
            fp = os.path.join(dirpath, name)
            rel = name if rel_dir == "." else rel_dir + "/" + name
            entries.append(("file", rel, fp))
    entries.sort(key=lambda e: e[1])
    for kind, rel, fp in entries:
        st = os.lstat(fp)
        ti = tarfile.TarInfo(rel)
        ti.uid = 0
        ti.gid = 0
        ti.uname = ""
        ti.gname = ""
        ti.mtime = int(st.st_mtime)
        if kind == "dir":
            ti.type = tarfile.DIRTYPE
            ti.mode = DIR_MODE
            tar.addfile(ti)
            continue
        if os.path.islink(fp):
            ti.type = tarfile.SYMTYPE
            ti.mode = 0o777
            ti.linkname = os.readlink(fp)
            tar.addfile(ti)
            continue
        if not os.path.isfile(fp):
            continue
        ti.type = tarfile.REGTYPE
        ti.mode = EXEC_MODE if matches(rel, exec_patterns) else FILE_MODE
        ti.size = st.st_size
        with open(fp, "rb") as fh:
            tar.addfile(ti, fh)


def cmd_pack(opts):
    root = one(opts, "--root")
    out = one(opts, "--out")
    if not root or not out:
        return die("pack 缺少 --root 或 --out")
    if not os.path.isdir(root):
        return die(f"pack root 不是目录: {root}")
    exec_patterns = opts.get("--exec") or []
    out = os.path.abspath(out)
    tmp = out + ".partial"
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    try:
        with open(tmp, "wb") as raw:
            with gzip.GzipFile(fileobj=raw, mode="wb", mtime=0, filename="") as gz:
                with tarfile.open(fileobj=gz, mode="w", format=tarfile.GNU_FORMAT) as tar:
                    add_tree(tar, root, exec_patterns)
        os.replace(tmp, out)
    except OSError as e:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        return die(f"pack 写失败: {e}", 7)
    info = {
        "ok": True,
        "schema": "tspi-rt-stack/deploy-tar-pack/v1",
        "archive": out,
        "size": os.path.getsize(out),
        "sha256": sha256_file(out),
        "members": count_members(out),
        "exec_patterns": exec_patterns,
    }
    emit(info, one(opts, "--json"))
    return 0


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def count_members(archive):
    with tarfile.open(archive, "r:gz") as tar:
        return len(tar.getmembers())


def cmd_list(opts):
    archive = one(opts, "--archive")
    if not archive or not os.path.isfile(archive):
        return die("list 缺少可读 --archive")
    with tarfile.open(archive, "r:gz") as tar:
        for m in tar.getmembers():
            kind = "d" if m.isdir() else ("l" if m.issym() else "-")
            print(f"{kind} {oct(m.mode & 0o777)} {m.size:>10} {m.name}")
    return 0


def sum_path(rel):
    """SHA256SUMS 行里的路径：去 ./ 与 Windows/MSYS 二进制指示符 '*'。"""
    rel = rel.strip()
    while rel.startswith("./"):
        rel = rel[2:]
    if rel.startswith("*"):
        rel = rel[1:]
    while rel.startswith("./"):
        rel = rel[2:]
    return rel


def _member_sha(tar, member):
    h = hashlib.sha256()
    f = tar.extractfile(member)
    if f is None:
        return None
    for chunk in iter(lambda: f.read(1 << 20), b""):
        h.update(chunk)
    return h.hexdigest()


def cmd_verify(opts):
    archive = one(opts, "--archive")
    if not archive or not os.path.isfile(archive):
        return die("verify 缺少可读 --archive")
    require_exec = opts.get("--require-exec") or []
    sums_path = one(opts, "--sums")
    failures = []
    required = []
    sums_checked = 0
    try:
        with tarfile.open(archive, "r:gz") as tar:
            members = [m for m in tar.getmembers() if m.isfile()]
            names = [m.name for m in members]
            for pat in require_exec:
                hits = [m for m in members if fnmatch.fnmatchcase(m.name, pat)]
                if not hits:
                    failures.append(f"必需可执行文件缺失（pattern 无匹配）: {pat}")
                    required.append({"pattern": pat, "matches": []})
                    continue
                bad = [m.name for m in hits if (m.mode & 0o111) == 0]
                required.append({"pattern": pat, "matches": [{"path": m.name, "mode": oct(m.mode & 0o777)} for m in hits]})
                if bad:
                    failures.append(f"缺少执行位（owner x）: {pat} -> {bad}")
            if sums_path:
                if not os.path.isfile(sums_path):
                    failures.append(f"--sums 文件不存在: {sums_path}")
                else:
                    by_name = {m.name: m for m in members}
                    for line in open(sums_path, encoding="utf-8"):
                        line = line.rstrip("\n")
                        if not line.strip():
                            continue
                        parts = line.split(None, 1)
                        if len(parts) != 2:
                            failures.append(f"SHA256SUMS 行非法: {line[:120]}")
                            continue
                        want, rel = parts[0], sum_path(parts[1])
                        sums_checked += 1
                        m = by_name.get(rel)
                        if m is None:
                            failures.append(f"归档缺 SHA256SUMS 列出的文件: {rel}")
                            continue
                        got = _member_sha(tar, m)
                        if got != want:
                            failures.append(f"归档成员 sha256 不符: {rel} want={want} got={got}")
            extras = sorted(set(names) - set(required_files_from_sums(sums_path)))
    except (tarfile.TarError, OSError) as e:
        return die(f"verify 读取归档失败: {e}", 3)
    info = {
        "ok": not failures,
        "schema": "tspi-rt-stack/deploy-tar-verify/v1",
        "archive": os.path.abspath(archive),
        "size": os.path.getsize(archive),
        "sha256": sha256_file(archive),
        "members": len(names),
        "required_exec": required,
        "sums_checked": sums_checked,
        "extras": extras[:50],
        "failures": failures,
    }
    emit(info, one(opts, "--json"))
    return 0 if not failures else 3


def required_files_from_sums(sums_path):
    out = set()
    if not sums_path or not os.path.isfile(sums_path):
        return out
    for line in open(sums_path, encoding="utf-8"):
        parts = line.split(None, 1)
        if len(parts) == 2:
            out.add(sum_path(parts[1]))
    return out


def emit(info, json_out):
    text = json.dumps(info, ensure_ascii=False, indent=1)
    print(text)
    if json_out:
        os.makedirs(os.path.dirname(os.path.abspath(json_out)) or ".", exist_ok=True)
        with open(json_out, "w", encoding="utf-8") as f:
            f.write(text + "\n")


def main(argv):
    cmd, opts = parse_args(argv)
    if opts is None:
        return die("参数缺值（--root/--out/--archive/--json/--sums/--exec/--require-exec 需要值）")
    if cmd == "pack":
        return cmd_pack(opts)
    if cmd == "verify":
        return cmd_verify(opts)
    if cmd == "list":
        return cmd_list(opts)
    return die("用法: release_tar.py {pack|verify|list} ... [--help 见文件头]")


if __name__ == "__main__":
    sys.exit(main(sys.argv))
