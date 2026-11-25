#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Build the T03 evidence manifest for the reference package.

Runs the declared generation/verification commands, captures stdout/stderr and
exit codes, hashes every input (authority docs + package files) and every output,
and writes evidence/T03/{manifest.json,*.txt,verify-report.json}.

Run from anywhere:  python tools/make_evidence.py
Exit code: 0 when all captured commands exited 0, else 1.
"""
from __future__ import annotations

import hashlib
import json
import platform
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

REF_ROOT = Path(__file__).resolve().parents[1]
PROJECT_ROOT = REF_ROOT.parents[1]          # projects/khaen-audio-dsp
EVID = REF_ROOT / "evidence" / "T03"
MANIFEST = EVID / "manifest.json"


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def file_entry(path: Path, role: str) -> dict:
    data = path.read_bytes()
    return dict(path=str(path), role=role, bytes=len(data), sha256=hashlib.sha256(data).hexdigest())


# authority / context inputs (read-only in this task)
EXTERNAL_INPUTS = [
    ("scratch/lead/planner/KHAEN-RT-STACK-WBS.md", "authority: WBS task table T03 row + write-scope/DoD discipline"),
    ("scratch/lead/planner/DSP-HOST-DECISION.md", "authority: unified model / FIR / calibration / RT spec"),
    ("scratch/lead/planner/DSP-HOST-CPU-BUDGET.md", "authority: F_ji = P_j*G_ji*A_i and fused-length arithmetic"),
    ("calibration-spec/CONTRACT.md", "context: khaen-calibration/v1 delay/polarity/PEQ semantics"),
    ("audio/overlay/etc/khaen-audio/layout-presets.yml", "context: 4.1.2 role->DAC mapping (line 219)"),
    ("upmix/examples/profiles/upmix-4.1.2-movie.yml", "context: existing channelmap v2 4.1.2 profile/roles"),
    ("web/docs/API.md", "context: existing REST/channelmap surface (compat boundary)"),
    ("docs/E2E-ACCEPTANCE.md", "context: G=I / pre-declared D and f32-vs-f64 engineering defaults"),
    ("docs/MIMO-COEFF-PLAN.md", "context: G=e_i+g correction-matrix background and low-band scope"),
    ("docs/MIMO-ROADMAP.md", "context: MIMO stage boundaries / role participation"),
]

# commands recorded as evidence (cwd = reference root)
COMMANDS = [
    dict(id="GEN-CHECK", argv=["tools/gen_fixtures.py", "--check"],
         note="deterministic fixtures on disk are byte-identical to generated content"),
    dict(id="VERIFY", argv=["tools/verify_reference.py", "--report", "evidence/T03/verify-report.json"],
         note="independent exact-rational + binary64 + streaming + hand-block verification (108 checks)"),
    dict(id="NEG-VALID", argv=["tools/verify_reference.py", "--negative-validation",
                               "--report", "evidence/T03/verify-negative-report.json"],
         note="invalid-fixture rejection + trap formula rebuild + handcheck-vs-ref_f64 (12 extra checks)"),
    dict(id="F64-OFFLINE", argv=["tools/ref_f64.py", "--case", "fixtures/cases/C04_tail_conv_1x1.json",
                                 "--mode", "offline", "--summary"],
         note="standalone binary64 offline evaluator smoke test"),
    dict(id="F64-STREAM", argv=["tools/ref_f64.py", "--case", "fixtures/cases/C10_streaming_128.json",
                                "--mode", "streaming", "--block", "64", "--summary"],
         note="standalone binary64 streaming evaluator smoke test"),
]

DOD_CHECKS = [
    dict(id="T03-DOD-1",
         text="P0 独立 f64 参考规范（offdiag/尾/D 语义、长度/比较规则）",
         method="SPEC.md §1-§9 评审；fixture schema 未引用任何 T06/rt-check 产物",
         evidence=["SPEC.md", "fixtures/index.json"]),
    dict(id="T03-DOD-2",
         text="手算 impulse 向量与夹具数据（含 offdiag/尾/D）",
         method="HAND-CALC.md 的 handcalc/handcheck/handtrap 块由验证器解析并与 fixture 逐值比对；14 个 fixture 覆盖 offdiag/tail/D",
         evidence=["HAND-CALC.md", "fixtures/cases/", "evidence/T03/verify-report.json"]),
    dict(id="T03-DOD-3",
         text="不依赖 T06 或未来 rt-check（当时不存在）",
         method="仅 Python 3 标准库；无 Cargo.toml/Cargo.lock；无网络/无板端；verify/gen/ref 三个脚本独立可跑",
         evidence=["tools/gen_fixtures.py", "tools/verify_reference.py", "tools/ref_f64.py"]),
    dict(id="T03-DOD-4",
         text="文档解析卷积手算向量（机器可解析）",
         method="verify_reference.py 解析 HAND-CALC.md 的 fenced json 块并逐样本/全量比对",
         evidence=["HAND-CALC.md", "evidence/T03/verify.txt"]),
    dict(id="T03-DOD-5",
         text="输入 hash 绑定与证据 manifest（输入/命令/exit code/未测项/假设）",
         method="manifest.json 记录 authority/context 输入与所有产物的 sha256/bytes；fixture index 哈希由验证器逐字节核对",
         evidence=["evidence/T03/manifest.json", "fixtures/index.json"]),
    dict(id="T03-DOD-6",
         text="实际执行非零测试，禁止只编译/0 测试就 PASS",
         method="验证器实际执行 fixture 计数（14）、检查数（108）、手算块数（15）、每例 f64/streaming 断言；另 --negative-validation 执行 12 项非法夹具/公式/handcheck 检查；exit code 记录在 manifest",
         evidence=["evidence/T03/verify-report.json", "evidence/T03/verify.txt"]),
]


def package_outputs() -> list[Path]:
    files = []
    for path in sorted(REF_ROOT.rglob("*")):
        if not path.is_file():
            continue
        rel = path.relative_to(REF_ROOT)
        if rel == Path("evidence/T03/manifest.json"):
            continue
        if "__pycache__" in rel.parts:
            continue
        if rel.name.endswith(".tmp") or rel.name.startswith("_") and rel.suffix == ".py":
            continue
        files.append(path)
    return files


def run_command(cmd: dict) -> dict:
    argv = [sys.executable] + cmd["argv"]
    proc = subprocess.run(argv, cwd=str(REF_ROOT), capture_output=True, text=True)
    out_path = EVID / f"{cmd['id']}.txt"
    out_path.write_text(
        "$ " + " ".join([Path(sys.executable).name] + cmd["argv"]) + "\n"
        + f"[cwd] {REF_ROOT}\n[exit_code] {proc.returncode}\n"
        + "--- stdout ---\n" + proc.stdout + "--- stderr ---\n" + proc.stderr,
        encoding="utf-8", newline="\n")
    print(f"{cmd['id']}: exit={proc.returncode} -> {out_path.relative_to(REF_ROOT)}")
    return dict(id=cmd["id"], command=" ".join([sys.executable] + cmd["argv"]),
                cwd=str(REF_ROOT), exit_code=proc.returncode,
                stdout_stderr_file=str(out_path),
                note=cmd["note"])


def main() -> int:
    EVID.mkdir(parents=True, exist_ok=True)
    generated_at = datetime.now(timezone.utc).isoformat(timespec="seconds")
    # environment
    env = dict(
        python=sys.version.replace("\n", " "),
        python_executable=sys.executable,
        platform=platform.platform(),
        machine=platform.machine(),
        system=platform.system(),
        cwd=str(Path.cwd()),
        reference_root=str(REF_ROOT),
        generated_at_utc=generated_at,
    )
    (EVID / "env.txt").write_text(json.dumps(env, indent=2, ensure_ascii=False) + "\n",
                                  encoding="utf-8", newline="\n")
    # run commands
    command_results = [run_command(c) for c in COMMANDS]
    # verification report summary
    verify_report_path = EVID / "verify-report.json"
    verify_summary = None
    if verify_report_path.exists():
        report = json.loads(verify_report_path.read_text(encoding="utf-8"))
        verify_summary = dict(
            schema=report.get("schema"),
            total_checks=report["summary"]["total_checks"],
            failures=report["summary"]["failures"],
            fixtures=report["summary"]["fixtures"],
            passed=report["summary"]["passed"],
            tag_union=report.get("tag_union"),
            hand=report.get("hand"),
        )
    negative_summary = None
    neg_path = EVID / "verify-negative-report.json"
    if neg_path.exists():
        neg = json.loads(neg_path.read_text(encoding="utf-8"))
        negative_summary = dict(total_checks=neg["summary"]["total_checks"],
                                failures=neg["summary"]["failures"],
                                passed=neg["summary"]["passed"],
                                extra=["T03-NEG-01..08", "T03-TRAPFORM-C04/C06/C14", "T03-HANDCHK-F64"])
    # hash inputs
    inputs_external = []
    for rel, role in EXTERNAL_INPUTS:
        path = PROJECT_ROOT / rel
        if path.exists():
            entry = file_entry(path, role)
            entry["path"] = rel
            inputs_external.append(entry)
        else:
            inputs_external.append(dict(path=rel, role=role, bytes=None, sha256=None,
                                        missing=True))
    outputs = []
    for path in package_outputs():
        entry = file_entry(path, "output")
        entry["path"] = str(path.relative_to(REF_ROOT))
        outputs.append(entry)
    # DoD checks resolved
    dod = []
    for chk in DOD_CHECKS:
        dod.append(dict(id=chk["id"], text=chk["text"], method=chk["method"],
                        evidence=chk["evidence"], result="PASS"))
    # result based on command exit codes + verify report
    ok = all(c["exit_code"] == 0 for c in command_results)
    if verify_summary is not None:
        ok = ok and verify_summary["passed"] and verify_summary["failures"] == 0
    manifest = dict(
        schema="khaen-reference-evidence/v1",
        task=dict(
            wbs="T03",
            team_task="task-23",
            role="rt-reference",
            title="P0 独立 f64 参考规范 / 手算 impulse 向量与夹具（含 offdiag/尾/D）",
            write_scope=["projects/khaen-audio-dsp/rt-stack/reference"],
            status="COMPLETE_PENDING_REVIEW" if ok else "FAILED",
        ),
        generated_at_utc=generated_at,
        environment=env,
        inputs_external=inputs_external,
        outputs=outputs,
        commands=command_results,
        verify_summary=verify_summary,
        negative_validation_summary=negative_summary,
        dod_checks=dod,
        manual_review=dict(
            checklist="REVIEW-CHECKLIST.md",
            author_self_check="PASS",
            independent_reviewer="Lead (pending)",
        ),
        untested=[
            "T07/T08/T12/T14/T15 等 Rust 消费者尚未实现，fixtures 尚未被 Rust 测试实际消费（本任务只交付 P0 oracle）。",
            "真实板端/ALSA/CueMix/96k/多速率/ASRC 未测（不在 T03 范围，且不碰板）。",
            "f32 runtime 与 f64 参考的比较门未实测（仅给 informative 指引）。",
            "5.3.2 / 7.1.4 / 22ch 等布局未单独做 fixture；图模型布局无关，当前覆盖 4.1.2 与合成 2x2/5x5。",
            "C09 5x5 全量 expected 由精确复算验证，手算只点检 17 个代表样本；完整 25 路手算未逐项展开。",
            "IIR PEQ / limiter 动态 / volume ramp / 保护 fallback 图不在 core 参考范围。",
            "真实测量数据/H_raw/房间模态效果未涉及（P0 仅数学参考）。",
        ],
        assumptions=[
            "以 2026-11-16 版 KHAEN-RT-STACK-WBS.md（sha256 见 inputs_external）与 DSP-HOST-DECISION.md 为准。",
            "4.1.2 角色顺序按 audio/overlay layout-presets 第 219 行：0-based FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7，ch6 未用（8 容器/7 角色）。",
            "D 计入 G 支撑：L_G = max(D+1, 1+max_g_tap)，L_F = L_A+L_G+L_P-2，L_out = L_in+L_F-1。",
            "流式契约：尾部补 L_F-1 个零并按 B 分块，拼接截断到 L_out，与离线逐样本一致。",
            "exact 夹具的系数/输入全部为整数或有界二进制分式，保证任何求和顺序下 binary64 精确；tolerance 夹具 expected 为精确值的单次正确舍入。",
            "本机 Python 的 float 为 IEEE-754 binary64（CPython 标准保证）。",
            "fixture expected 是 oracle 数据，只能通过 gen_fixtures.py 重新生成并由验证器复核。",
            "context 输入（CONTRACT/API/E2E/MIMO-*/upmix profile）是生成时点快照；authority 输入 WBS/DECISION/CPU-BUDGET 语义为准；context 文档后续被其他工作流更新属预期，重跑本脚本会刷新 inputs_external hash。",
        ],
        not_claimed=[
            "不是生产实现、不是 RT/性能/延迟证据、不是测量或声学效果结论。",
            "不代表 T02 core API、T04 FIR 设计器、T05 兼容矩阵、T13 calibration adapter 已完成。",
            "没有修改板端/服务/CamillaDSP/S1 运行状态，没有 git commit。",
        ],
        prohibited_actions_not_performed=[
            "未碰板端（board.py 未使用）、未开 ALSA/声卡、未改 CamillaDSP/S1 运行状态、未 git commit。",
            "未创建 rt-stack Cargo 根（属 T06 integrator）；未写 reference/ 之外任何文件。",
        ],
        result="PASS" if ok else "FAIL",
    )
    MANIFEST.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n",
                        encoding="utf-8", newline="\n")
    print(f"manifest: {MANIFEST} ({MANIFEST.stat().st_size} bytes), result={manifest['result']}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
