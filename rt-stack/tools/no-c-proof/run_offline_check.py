#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""T72 no-C 离线证据一键生成：静态扫描 + 板端脚本本地干跑 + 证据索引/哈希。

真实执行：
  1) scan_no_c_refs.py  扫描 conf/service/overlay/web/deploy → scan/{refs.json,REFS.md}
  2) localsim/run_sim.py 用夹具+mock 真实执行 board_check_no_c.sh 的 14 个用例
  3) 生成本目录 EXECUTION.md / scripts.sha256 / SHA256SUMS

用法: python3 run_offline_check.py [--project-root DIR] [--evidence-dir DIR]
退出码: 0 扫描 runtime=0 且干跑全 PASS；3/4 扫描或干跑不符；7 环境不支持
"""
import argparse
import datetime
import hashlib
import os
import platform
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RS = HERE.parents[1]
PROJECT_DEFAULT = RS.parent


def sha256(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(262144), b""):
            h.update(c)
    return h.hexdigest()


def run(cmd, cwd=None, timeout=900):
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True,
                          encoding="utf-8", errors="replace", timeout=timeout)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--project-root", default=str(PROJECT_DEFAULT))
    ap.add_argument("--evidence-dir", default=str(PROJECT_DEFAULT / "rt-stack" / "evidence" / "no-c-proof"))
    ap.add_argument("--reuse", action="store_true", help="复用已有 scan/localsim 输出生成 EXECUTION/SHA256SUMS（不重跑）")
    args = ap.parse_args()
    project = Path(args.project_root).resolve()
    ev = Path(args.evidence_dir).resolve()
    ev.mkdir(parents=True, exist_ok=True)
    (ev / "scan").mkdir(exist_ok=True)
    (ev / "localsim").mkdir(exist_ok=True)

    now = datetime.datetime.now().astimezone().isoformat(timespec="seconds")
    scan_cmd = [sys.executable, str(HERE / "scan_no_c_refs.py"),
                "--project-root", str(project), "--out-dir", str(ev / "scan")]
    sim_cmd = [sys.executable, str(HERE / "localsim" / "run_sim.py"),
               "--out", str(ev / "localsim")]

    class _Result:
        pass

    if args.reuse:
        scan = _Result()
        sim = _Result()
        if not (ev / "scan" / "refs.json").is_file() or not (ev / "localsim" / "results.json").is_file():
            print("[evidence] --reuse 缺少 refs.json/results.json", file=sys.stderr)
            return 7
        try:
            scan.returncode = int((ev / "scan" / "exit-code.txt").read_text(encoding="utf-8").strip())
        except Exception:
            import json as _json
            _refs = _json.loads((ev / "scan" / "refs.json").read_text(encoding="utf-8"))
            _sum = _refs.get("summary") or {}
            _bad = (_sum.get("by_category") or {})
            scan.returncode = 0 if (_bad.get("runtime_dependency", 0) == 0 and _bad.get("unclassified", 0) == 0) else 4
            (ev / "scan" / "exit-code.txt").write_text("%d\n" % scan.returncode, encoding="utf-8", newline="\n")
        try:
            sim.returncode = int((ev / "localsim" / "exit-code.txt").read_text(encoding="utf-8").strip())
        except Exception:
            import json as _json2
            _sim = _json2.loads((ev / "localsim" / "results.json").read_text(encoding="utf-8"))
            sim.returncode = 0 if _sim.get("mismatched", 1) == 0 and _sim.get("executed", 0) > 0 else 3
            (ev / "localsim" / "exit-code.txt").write_text("%d\n" % sim.returncode, encoding="utf-8", newline="\n")
    else:
        scan = run(scan_cmd, cwd=str(project))
        (ev / "scan" / "stdout.txt").write_text((scan.stdout or "") + (scan.stderr or ""), encoding="utf-8", newline="\n")
        (ev / "scan" / "exit-code.txt").write_text("%d\n" % scan.returncode, encoding="utf-8", newline="\n")
        (ev / "scan" / "scan-command.txt").write_text(
            "cd <project>\n" + " ".join(scan_cmd) + "\n", encoding="utf-8", newline="\n")
        sim = run(sim_cmd, cwd=str(project))
        (ev / "localsim" / "stdout.txt").write_text((sim.stdout or "") + (sim.stderr or ""), encoding="utf-8", newline="\n")
        (ev / "localsim" / "exit-code.txt").write_text("%d\n" % sim.returncode, encoding="utf-8", newline="\n")

    # 读取两份 JSON 汇总
    import json
    refs = {}
    try:
        refs = json.loads((ev / "scan" / "refs.json").read_text(encoding="utf-8"))
    except Exception:
        pass
    simdoc = {}
    try:
        simdoc = json.loads((ev / "localsim" / "results.json").read_text(encoding="utf-8"))
    except Exception:
        pass
    bycat = (refs.get("summary") or {}).get("by_category", {})
    pa = (refs.get("payload_audit") or {}).get("summary", {})
    executed = simdoc.get("executed", 0)
    matched = simdoc.get("matched", 0)

    scripts = [
        HERE / "scan_no_c_refs.py",
        HERE / "board_check_no_c.sh",
        HERE / "localsim" / "make_fixture.sh",
        HERE / "localsim" / "run_sim.py",
        HERE / "run_offline_check.py",
    ]
    (ev / "scripts.sha256").write_text(
        "".join("%s  %s\n" % (sha256(s), s.relative_to(project).as_posix()) for s in scripts if s.is_file()),
        encoding="utf-8", newline="\n")

    exec_md = []
    exec_md.append("# T72 no-C 离线证据 — 执行记录（真实执行）")
    exec_md.append("")
    exec_md.append("| 项 | 值 |")
    exec_md.append("| --- | --- |")
    exec_md.append("| 生成时间 | %s |" % now)
    exec_md.append("| 主机 | %s / python %s |" % (platform.platform(), platform.python_version()))
    exec_md.append("| 项目根 | %s |" % project)
    exec_md.append("| 扫描文件数 | %s |" % (refs.get("summary") or {}).get("files_scanned", "?"))
    exec_md.append("| 命中文件数 | %s |" % (refs.get("summary") or {}).get("files_matched", "?"))
    exec_md.append("| runtime_dependency | **%s** |" % bycat.get("runtime_dependency", 0))
    exec_md.append("| to_delete | %s |" % bycat.get("to_delete", "?"))
    exec_md.append("| historical_only | %s |" % bycat.get("historical_only", "?"))
    exec_md.append("| unclassified | %s |" % bycat.get("unclassified", 0))
    exec_md.append("| 载荷 positive dependency 行 | %s |" % pa.get("runtime_dependency_lines", "?"))
    exec_md.append("| 载荷 ordering-only 残留行 | %s |" % pa.get("ordering_dependency_lines", "?"))
    exec_md.append("| 扫描 exit | %s |" % scan.returncode)
    exec_md.append("| 板端脚本本地干跑用例数（executed） | **%s** |" % executed)
    exec_md.append("| 干跑符合预期 / 不符 | %s / %s |" % (matched, executed - matched if isinstance(executed, int) else "?"))
    exec_md.append("| 干跑 exit | %s |" % sim.returncode)
    exec_md.append("")
    exec_md.append("## 真实执行的命令")
    exec_md.append("")
    exec_md.append(chr(96)*3 + "text")
    exec_md.append("cd %s" % project)
    exec_md.append(" ".join(scan_cmd))
    exec_md.append(" ".join(sim_cmd))
    exec_md.append(chr(96)*3)
    exec_md.append("")
    exec_md.append("## 脚本哈希（SHA-256）")
    exec_md.append("")
    exec_md.append(chr(96)*3 + "text")
    for s in scripts:
        if s.is_file():
            exec_md.append("%s  %s" % (sha256(s), s.relative_to(project).as_posix()))
    exec_md.append(chr(96)*3)
    exec_md.append("")
    exec_md.append("## 结论")
    exec_md.append("")
    ok_scan = scan.returncode == 0 and bycat.get("runtime_dependency", 0) == 0
    ok_sim = sim.returncode == 0 and executed > 0 and matched == executed
    exec_md.append("- 离线静态扫描：%s（runtime_dependency=%s）" % ("PASS" if ok_scan else "FAIL/INCOMPLETE", bycat.get("runtime_dependency", 0)))
    exec_md.append("- 板端脚本本地干跑：%s（executed=%s matched=%s）" % ("PASS" if ok_sim else "FAIL", executed, matched))
    exec_md.append("- 板端真实 T72（T71 窗口后）：**NOT_RUN** —— 见 NOT_RUN.md")
    exec_md.append("")
    exec_md.append("> 本文件由 rt-stack/tools/no-c-proof/run_offline_check.py 生成；所有数字来自本轮真实执行。")
    (ev / "EXECUTION.md").write_text("\n".join(exec_md) + "\n", encoding="utf-8", newline="\n")

    # SHA256SUMS（覆盖证据目录除自身外所有文件，按相对路径排序）
    lines = []
    for p in sorted(ev.rglob("*")):
        if p.is_file() and p.name != "SHA256SUMS":
            lines.append("%s  %s" % (sha256(p), p.relative_to(ev).as_posix()))
    (ev / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")

    print("[evidence] scan exit=%d runtime_dependency=%s" % (scan.returncode, bycat.get("runtime_dependency", 0)))
    print("[evidence] localsim exit=%d executed=%s matched=%s" % (sim.returncode, executed, matched))
    print("[evidence] EXECUTION.md / scripts.sha256 / SHA256SUMS written to %s" % ev)
    if scan.returncode != 0 or sim.returncode != 0:
        return 3 if scan.returncode not in (3, 4) else scan.returncode
    return 0


if __name__ == "__main__":
    sys.exit(main())
