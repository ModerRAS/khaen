#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""T72 no-C 本地干跑 harness（真实执行 board_check_no_c.sh + 故障注入，不连板）。

用 make_fixture.sh 生成假板根（/proc、/dev/snd、/etc/systemd、/run、/opt 等），
用 rt-stack/deploy/tests/mocks 的 systemctl/ss 拦截真实系统服务，
真实执行 tools/no-c-proof/board_check_no_c.sh 的 1 个 PASS + 1 个回滚快照 + 12 个故障注入用例，
断言 exit / overall / 关键 FAIL 项，输出 per-case 证据 + results.json + RESULTS.md。

用法: python3 run_sim.py [--out DIR] [--keep]
退出码: 0 全部符合预期; 3 有断言不符; 7 环境不支持
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOL_DIR = HERE.parent
RS = HERE.parents[2]
PROJECT = RS.parent
MOCKS = RS / "deploy" / "tests" / "mocks"
TOOL = TOOL_DIR / "board_check_no_c.sh"
FIXGEN = HERE / "make_fixture.sh"

CASES = [
    # name, variant, extra args, expect_exit, expect_overall, expect_fail_id, expect_nv_id
    ("pass", "pass", [], 0, "PASS", None, None),
    ("rollback-mute", "pass-rollback-mute", ["--allow-host-inactive"], 0, "INCOMPLETE", None, "N8.host"),
    ("fail-proc", "fail-proc", ["--require-daemon"], 5, "FAIL", "N1.process", None),
    ("fail-unit", "fail-unit", ["--require-daemon"], 5, "FAIL", "N2.units", None),
    ("fail-cgroup", "fail-cgroup", ["--require-daemon"], 5, "FAIL", "N3.cgroup", None),
    ("fail-port1234", "fail-port1234", ["--require-daemon"], 5, "FAIL", "N4.ports", None),
    ("fail-port18080", "fail-port18080", ["--require-daemon"], 5, "FAIL", "N4.ports", None),
    ("fail-entry", "fail-entry", ["--require-daemon"], 5, "FAIL", "N5.entry-files", None),
    ("fail-socketfd", "fail-socketfd", ["--require-daemon"], 5, "FAIL", "N6.socket-pipe", None),
    ("fail-pcm-owner", "fail-pcm-owner", ["--require-daemon"], 5, "FAIL", "N7.pcm-owner", None),
    ("fail-daemon-pcm", "fail-daemon-pcm", ["--require-daemon"], 5, "FAIL", "N7.pcm-owner", None),
    ("fail-keeper-before", "fail-keeper-before", ["--require-daemon"], 5, "FAIL", "N9.deps", None),
    ("fail-wants", "fail-wants", ["--require-daemon"], 5, "FAIL", "N9.deps", None),
    ("fail-duplicate-host", "fail-duplicate-host", ["--require-daemon"], 5, "FAIL", "N8.host", None),
]

TQ = chr(96)


def find_bash():
    cands = []
    env_bash = os.environ.get("TSPI_BASH")
    if env_bash:
        cands.append(env_bash)
    w = shutil.which("bash")
    if w:
        cands.append(w)
    cands += [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
        "bash",
    ]
    for c in cands:
        try:
            r = subprocess.run([c, "-c", "echo ok"], capture_output=True, text=True, timeout=20)
            if r.returncode == 0 and "ok" in r.stdout:
                return c
        except Exception:
            continue
    return None


def msys(p):
    return str(p).replace("\\\\", "/")


def cygpath_u(bash, p):
    try:
        r = subprocess.run([bash, "-c", 'cygpath -u "$1"', "cygpath", str(p).replace("\\", "/")],
                           capture_output=True, text=True, timeout=30)
        s = (r.stdout or "").strip()
        return s or str(p).replace("\\", "/")
    except Exception:
        return str(p).replace("\\", "/")


def run(cmd, env=None, timeout=180):
    if env is None:
        env = os.environ.copy()
    return subprocess.run(cmd, env=env, capture_output=True, text=True,
                          encoding="utf-8", errors="replace", timeout=timeout)


def read_json(path):
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(PROJECT / "rt-stack" / "evidence" / "no-c-proof" / "localsim"))
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)

    if not TOOL.is_file() or not FIXGEN.is_file():
        print("[sim] 缺少脚本: %s / %s" % (TOOL, FIXGEN), file=sys.stderr)
        return 7
    if not MOCKS.is_dir():
        print("[sim] 缺少 mocks 目录: %s" % MOCKS, file=sys.stderr)
        return 7
    bash = find_bash()
    if not bash:
        print("[sim] 找不到可用的 bash（可用 TSPI_BASH 指定）", file=sys.stderr)
        return 7

    work = Path(tempfile.mkdtemp(prefix="tspi-t72-sim-"))
    generated = datetime.datetime.now().astimezone().isoformat(timespec="seconds")
    results = []
    try:
        for name, variant, extra, want_rc, want_overall, want_fail, want_nv in CASES:
            cdir = out / name
            cdir.mkdir(parents=True, exist_ok=True)
            fix = work / ("fix-" + name)
            fix_u = cygpath_u(bash, str(fix))
            cdir_u = cygpath_u(bash, str(cdir))
            fx = run([bash, cygpath_u(bash, str(FIXGEN)), variant, fix_u])
            (cdir / "fixture.log").write_text((fx.stdout or "") + (fx.stderr or ""), encoding="utf-8")
            if fx.returncode != 0:
                results.append({"case": name, "variant": variant, "assertion": "MISMATCH",
                                "detail": "fixture build failed rc=%d" % fx.returncode,
                                "actual_fails": [], "actual_not_verified": [],
                                "expect_exit": want_rc, "expect_overall": want_overall,
                                "expect_fail": want_fail or "", "expect_not_verified": want_nv or "",
                                "actual_exit": -1, "actual_overall": "?", "extra_args": extra,
                                "evidence": "%s/board.json" % name})
                print("[sim] %-20s FIXTURE FAIL" % name)
                continue
            env = os.environ.copy()
            env["PATH"] = cygpath_u(bash, str(MOCKS)) + ";" + env.get("PATH", "")
            env["MOCK_STATE"] = fix_u + "/mock"
            env["MOCK_LOG"] = fix_u + "/mock/log"
            env["MOCK_PROCS"] = fix_u + "/mock/procs"
            env["MOCK_SS"] = fix_u + "/mock/ss.txt"
            cmd = [bash, cygpath_u(bash, str(TOOL)), "--live", "--root", fix_u + "/board", "--phase", name,
                   "--evidence-dir", cdir_u, "--json-out", cdir_u + "/board.json",
                   "--human-out", cdir_u + "/board.txt"] + extra
            pr = run(cmd, env)
            (cdir / "stdout.txt").write_text((pr.stdout or "") + (pr.stderr or ""), encoding="utf-8")
            doc = read_json(cdir / "board.json")
            got_rc = pr.returncode
            got_overall = (doc or {}).get("overall", "?")
            fails = [c.get("id") for c in (doc or {}).get("checks", []) if c.get("status") == "FAIL"]
            nvs = [c.get("id") for c in (doc or {}).get("checks", []) if c.get("status") == "NOT_VERIFIED"]
            problems = []
            if got_rc != want_rc:
                problems.append("exit(want=%s got=%s)" % (want_rc, got_rc))
            if got_overall != want_overall:
                problems.append("overall(want=%s got=%s)" % (want_overall, got_overall))
            if want_fail and want_fail not in fails:
                problems.append("missing_fail(%s)" % want_fail)
            if want_nv and want_nv not in nvs:
                problems.append("missing_not_verified(%s)" % want_nv)
            if want_overall == "PASS" and fails:
                problems.append("unexpected_fails(%s)" % ",".join(fails))
            ok = not problems
            tag = "PASS" if ok else "**MISMATCH**"
            results.append({
                "case": name, "variant": variant, "extra_args": extra,
                "expect_exit": want_rc, "actual_exit": got_rc,
                "expect_overall": want_overall, "actual_overall": got_overall,
                "expect_fail": want_fail or "", "expect_not_verified": want_nv or "",
                "actual_fails": fails, "actual_not_verified": nvs,
                "assertion": "MATCH" if ok else "MISMATCH",
                "problems": problems,
                "evidence": "%s/board.json" % name,
            })
            print("[sim] %-20s variant=%-20s rc=%s overall=%-10s fails=%-32s -> %s" %
                  (name, variant, got_rc, got_overall, ",".join(fails) if fails else "-", tag))
    finally:
        if not args.keep:
            shutil.rmtree(work, ignore_errors=True)

    executed = len(results)
    matched = sum(1 for r in results if r.get("assertion") == "MATCH")
    mismatched = executed - matched
    (out / "results.json").write_text(json.dumps({
        "schema": "tspi-rt-stack/no-c-proof-localsim/v1",
        "generated_at": generated,
        "tool": "rt-stack/tools/no-c-proof/board_check_no_c.sh",
        "fixture_generator": "rt-stack/tools/no-c-proof/localsim/make_fixture.sh",
        "mocks": "rt-stack/deploy/tests/mocks (systemctl/ss)",
        "bash": bash,
        "executed": executed,
        "matched": matched,
        "mismatched": mismatched,
        "cases": results,
    }, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")

    md = []
    md.append("# T72 no-C 板端检查脚本 —— 本地干跑结果（真实执行）")
    md.append("")
    md.append("> 生成时间：%s；工具：%sboard_check_no_c.sh%s；夹具：%smake_fixture.sh%s；"
              "桩：%sdeploy/tests/mocks%s（systemctl/ss）。不连板、不触碰真实 systemd/设备/PCM。" %
              (generated, TQ, TQ, TQ, TQ, TQ, TQ))
    md.append("")
    md.append("| # | 用例 | 夹具变体 | 期望 exit | 实际 exit | 期望 overall | 实际 overall | 期望 FAIL 项 | 实际 FAIL 项 | 结果 |")
    md.append("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for i, r in enumerate(results, 1):
        tag = "PASS" if r.get("assertion") == "MATCH" else "**MISMATCH**"
        md.append("| %d | %s | %s | %s | %s | %s | %s | %s | %s | %s |" % (
            i, r.get("case"), r.get("variant"), r.get("expect_exit"), r.get("actual_exit"),
            r.get("expect_overall"), r.get("actual_overall"),
            r.get("expect_fail") or r.get("expect_not_verified") or "-",
            ",".join(r.get("actual_fails", [])) if r.get("actual_fails") is not None else "-", tag))
    md.append("")
    md.append("## 汇总")
    md.append("")
    md.append("- 执行用例：**%d**（全部经 bash 真实执行）" % executed)
    md.append("- 符合预期：**%d**；不符：**%d**" % (matched, mismatched))
    if mismatched:
        md.append("")
        md.append("结论：**MISMATCH — 检查脚本、夹具或 mock 需修**")
    else:
        md.append("")
        md.append("结论：**全部用例符合预期**。PASS 夹具 overall=PASS/exit 0；"
                  "每个注入故障都被对应硬门捕获为 FAIL/exit 5；回滚/mute 快照在 --allow-host-inactive 下为 INCOMPLETE/exit 0。")
    md.append("")
    md.append("## 复现")
    md.append("")
    md.append(TQ*3 + "bash")
    md.append("python3 rt-stack/tools/no-c-proof/localsim/run_sim.py --out rt-stack/evidence/no-c-proof/localsim")
    md.append(TQ*3)
    md.append("")
    md.append("## 说明")
    md.append("")
    md.append("- %s--root <fixture>/board%s 只改文件系统前缀；%ssystemctl%s/%sss%s 由 PATH 前置 mock 提供；"
              "%s/proc/*%s、%s/dev/snd/*%s 为夹具文件/符号链接。" % (TQ, TQ, TQ, TQ, TQ, TQ, TQ, TQ, TQ, TQ))
    md.append("- 这是**脚本自检**，不能替代板端 T72 实测（板端仍为 NOT_RUN，见 %s../NOT_RUN.md%s）。" % (TQ, TQ))
    (out / "RESULTS.md").write_text("\n".join(md) + "\n", encoding="utf-8")

    print()
    print("[sim] executed=%d matched=%d mismatched=%d" % (executed, matched, mismatched))
    print("[sim] results: %s" % (out / "RESULTS.md"))
    return 0 if mismatched == 0 else 3


if __name__ == "__main__":
    sys.exit(main())
