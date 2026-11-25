#!/usr/bin/env python3
"""TSPI 校准批量闭环 CLI（task-201）。

一次批量跑完：前/后 多点扫频 -> 应用 -> 复测 -> 瀑布/RT/FR/MIMO/holdout 对比（观察量，不是验收门）。

子命令：
  plan     窗口前冻结：校验配置、生成每步 argv/预计时长/RUN-PLAN.md（不碰板）
  run      窗口内执行：只跑 plan 冻结命令（cmd）或离线合成（fake）；失败 fail-closed
  analyze  窗口后分析：FR/瀑布(CSD)/EDC/T20/RT60/MIMO 前后/holdout/问题定位/报告
  selftest 离线端到端自测（fake 合成数据）
  version

退出码：0 成功；2 用法/配置错误；3 NOT_RUN/阻塞；4 执行/分析失败；5 自测失败。
"""

from __future__ import annotations

import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from calbatch import config as cfgmod, plan as planmod, runner  # noqa: E402


def _print_json(obj):
    print(json.dumps(obj, ensure_ascii=False, indent=2))


def cmd_plan(args):
    cfg = cfgmod.load_config(args.config)
    repo_root = args.repo_root or cfgmod.repo_root_from(HERE)
    rounds = [args.round] if args.round else (["A", "B"] if int(cfg.get("rounds", 1)) >= 2 else ["A"])
    results = []
    failed = False
    for rid in rounds:
        plan = planmod.build_plan(cfg, rid, repo_root, scenario=args.scenario)
        val = plan["validation"]
        est = plan["estimate"]
        if val["errors"]:
            failed = True
            print("PLAN %s FAILED: %d error(s)" % (rid, len(val["errors"])), file=sys.stderr)
            for e in val["errors"]:
                print("  [%s] %s" % (e["code"], e["message"]), file=sys.stderr)
            continue
        if not est["fits_window"]:
            failed = True
            print("PLAN %s FAILED: 时间预算 %.1f min 超出窗口 %.0f min"
                  % (rid, est["total_minutes"], est["window"]["duration_seconds"] / 60.0), file=sys.stderr)
            continue
        out_dir = args.out if len(rounds) == 1 else os.path.join(args.out, "round-%s" % rid)
        info = planmod.write_plan(plan, out_dir)
        results.append({"round": rid, "plan": info["plan"], "dir": info["dir"],
                        "files": info["files"], "warnings": val["warnings"],
                        "estimate_minutes": est["total_minutes"],
                        "steps": len(plan["steps"]), "not_run": plan["not_run"]})
        print("PLAN %s OK  steps=%d  est=%.1f min  window=%s-%s  out=%s"
              % (rid, len(plan["steps"]), est["total_minutes"], est["window"]["start"], est["window"]["end"], out_dir))
        for w in val["warnings"]:
            print("  WARN [%s] %s" % (w["code"], w["message"]))
    if args.json:
        _print_json({"ok": not failed, "rounds": results})
    if failed:
        return 2
    return 0


def cmd_run(args):
    if not os.path.isfile(args.plan):
        print("plan 不存在: %s" % args.plan, file=sys.stderr)
        return 2
    try:
        summary = runner.run_plan(args.plan, args.work, transport=args.transport,
                                  scenario=args.scenario or "", dry_run=args.dry_run)
    except ValueError as exc:
        print("run 失败: %s" % exc, file=sys.stderr)
        return 2
    if args.json:
        _print_json(summary)
    if summary.get("status") == "dry-run":
        print("DRY-RUN OK（未执行任何命令）")
        return 0
    if summary.get("status") != "ok":
        print("RUN FAILED: %s" % json.dumps(summary.get("failure"), ensure_ascii=False), file=sys.stderr)
        return 4
    print("RUN OK  transport=%s  work=%s" % (args.transport, args.work))
    return 0


def cmd_analyze(args):
    from calbatch import analysis
    try:
        result = analysis.analyze_workdir(args.work, args.out)
    except SystemExit as exc:
        print(str(exc), file=sys.stderr)
        return 2
    except Exception as exc:  # noqa: BLE001
        print("analyze 失败: %s" % exc, file=sys.stderr)
        return 4
    v = result["comparison"]["verdict"]
    print("ANALYZE OK  verdict=%s（观察量，不是验收门）  out=%s"
          % (v["classification"], args.out))
    print("  改善带=%d 恶化带=%d 余振改善=%d 余振恶化=%d issues=%d"
          % (v["improve_bands"], v["worsen_bands"], v["residual_improve_bands"],
             v["residual_worsen_bands"], len(result["comparison"]["issues"])))
    if args.json:
        _print_json({"ok": True, "verdict": v, "outdir": args.out,
                     "manifest": result["manifest"], "issues": result["comparison"]["issues"]})
    return 0


def cmd_selftest(args):
    from calbatch import selftest
    scenarios = tuple(s.strip() for s in args.scenarios.split(",") if s.strip())
    result = selftest.run_selftest(args.out, keep=args.keep, scenarios=scenarios)
    print("SELFTEST %s  checks=%d/%d  elapsed=%.1fs  out=%s"
          % ("PASS" if result["ok"] else "FAIL", result["checks_pass"], result["checks_total"],
             result["elapsed_s"], args.out))
    for name, info in result["scenarios"].items():
        print("  %-10s verdict=%-12s %.1fs" % (name, info.get("verdict"), info.get("elapsed_s") or 0.0))
    for c in result["checks_fail"]:
        print("  FAIL %s: %s" % (c["name"], c["detail"]), file=sys.stderr)
    if args.json:
        _print_json(result)
    return 0 if result["ok"] else 5


def cmd_version(_args):
    from calbatch import __version__, TOOL_NAME
    print("%s %s (python %s)" % (TOOL_NAME, __version__, sys.version.split()[0]))
    return 0


def build_parser():
    p = argparse.ArgumentParser(prog="tspi_cal_batch", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command")

    pp = sub.add_parser("plan", help="窗口前冻结计划（不碰板）")
    pp.add_argument("--config", required=True)
    pp.add_argument("--out", required=True)
    pp.add_argument("--round", default="", help="只生成单轮 A/B；缺省按 config.rounds 生成")
    pp.add_argument("--scenario", default="", help="fake 自测场景 improved/worse/flat（仅 selftest 配置用）")
    pp.add_argument("--repo-root", default="")
    pp.add_argument("--json", action="store_true")
    pp.set_defaults(func=cmd_plan)

    pr = sub.add_parser("run", help="窗口内执行冻结计划")
    pr.add_argument("--plan", required=True)
    pr.add_argument("--work", required=True)
    pr.add_argument("--transport", choices=["cmd", "fake"], default="cmd")
    pr.add_argument("--scenario", default="")
    pr.add_argument("--dry-run", action="store_true")
    pr.add_argument("--json", action="store_true")
    pr.set_defaults(func=cmd_run)

    pa = sub.add_parser("analyze", help="窗口后分析 + 报告")
    pa.add_argument("--work", required=True)
    pa.add_argument("--out", required=True)
    pa.add_argument("--json", action="store_true")
    pa.set_defaults(func=cmd_analyze)

    ps = sub.add_parser("selftest", help="离线端到端自测")
    ps.add_argument("--out", required=True)
    ps.add_argument("--scenarios", default="improved,worse,flat")
    ps.add_argument("--keep", action="store_true")
    ps.add_argument("--json", action="store_true")
    ps.set_defaults(func=cmd_selftest)

    pv = sub.add_parser("version", help="版本")
    pv.set_defaults(func=cmd_version)
    return p


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    if not getattr(args, "func", None):
        parser.print_help()
        return 2
    try:
        return args.func(args)
    except FileNotFoundError as exc:
        print("文件/命令不存在: %s" % exc, file=sys.stderr)
        return 2
    except json.JSONDecodeError as exc:
        print("JSON 解析失败: %s" % exc, file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
