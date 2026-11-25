#!/usr/bin/env python3
"""task-229：重生成 sweep 验证分析产物（统一时基/字段语义）+ 旧错误产物 SUPERSEDED 快照。

背景：2026-11-18 sweep 验证把 ir.delay_ms_from_segment_start（IR 提取段内相对位置）当绝对
直达延迟换算距离，得出 MA1 近场/贴箱的错误结论。本脚本用修好的 analyze_sweep.py /
compare_runs.py 从同一批原始数据重算 JSON/COMPARISON，并把旧产物按原样快照到
SUPERSEDED/ 并保留 sha256；FR/RT/CSD/IR-wav/SVG 必须与旧产物逐字节一致（只改 IR 语义块），
脚本会逐文件核对并写报告。

用法：
  python regenerate_artifacts.py --repo-root <tspi-audio-dsp> \
      [--work-dir <dir>] [--report-out <dir>] [--skip-snapshot] [--sums-only]
退出码 0 = 成功（含核对全通过）；1 = 有核对失败。
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys

TOOLS_DIR = os.path.dirname(os.path.abspath(__file__))
REL_EVIDENCE = os.path.join("rt-stack", "evidence", "board-window-20261118-sweep")
REL_ARTIFACT = os.path.join("artifacts", "tspi-audio", "sweep-verification-20261118")
REL_REPORT = os.path.join("rt-stack", "evidence", "cursor-fix-ir-distance", "regen")

# 重生成时允许变化的文件（IR 语义/报告）；其余分析产物必须逐字节一致。
CHANGED_BY_DESIGN = {
    "README.md",
    "analysis/COMPARISON.md",
    "analysis/analysis-summary.json",
    "analysis/comparison.json",
    "analysis/board-aplay-MA1-analysis.json",
    "analysis/board-aplay-UMIK-analysis.json",
    "analysis/host-fileinput-UMIK-analysis.json",
    "analysis/tool-crosscheck.json",  # 由既有工具交叉核对生成，本任务不重跑
    "SHA256SUMS.txt",
    "SUPERSEDED",
}


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path, payload):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(payload, handle, ensure_ascii=False, indent=1)
        handle.write("\n")


def snapshot_old(artifact, evidence, force):
    sup = os.path.join(artifact, "SUPERSEDED")
    marker = os.path.join(sup, "README.md")
    if os.path.isfile(marker) and not force:
        print("snapshot: SUPERSEDED/ already exists (keep); use --force-snapshot to redo")
        return False
    os.makedirs(os.path.join(sup, "old-artifact"), exist_ok=True)
    os.makedirs(os.path.join(sup, "old-scripts"), exist_ok=True)
    old_files = ["README.md", "SHA256SUMS.txt",
                 os.path.join("analysis", "COMPARISON.md"),
                 os.path.join("analysis", "analysis-summary.json"),
                 os.path.join("analysis", "comparison.json"),
                 os.path.join("analysis", "board-aplay-MA1-analysis.json"),
                 os.path.join("analysis", "board-aplay-UMIK-analysis.json"),
                 os.path.join("analysis", "host-fileinput-UMIK-analysis.json"),
                 os.path.join("analysis", "tool-crosscheck.json")]
    rows = []
    for rel in old_files:
        src = os.path.join(artifact, rel)
        if not os.path.isfile(src):
            continue
        dst = os.path.join(sup, "old-artifact", rel)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copy2(src, dst)
        rows.append((hashlib.sha256(open(src, "rb").read()).hexdigest(), "old-artifact/" + rel.replace(os.sep, "/")))
    for rel in ["analyze_sweep.py", "compare_runs.py"]:
        src = os.path.join(evidence, "scripts", rel)
        if os.path.isfile(src):
            dst = os.path.join(sup, "old-scripts", rel)
            shutil.copy2(src, dst)
            rows.append((sha256_file(src), "old-scripts/" + rel))
    rows.sort(key=lambda row: row[1])
    with open(os.path.join(sup, "SHA256SUMS-before.txt"), "w", encoding="utf-8", newline="\n") as handle:
        for digest, rel in rows:
            handle.write("%s  %s\n" % (digest, rel))
    with open(marker, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(
            "# SUPERSEDED —— task-229（2026-11-18）时基/距离口径修复前的 sweep 分析产物\n\n"
            "原因：旧 README/JSON/COMPARISON 把 IR 提取段内相对偏移 "
            "(delay_ms_from_segment_start / ir_direct_delay_ms) 当成绝对直达延迟并换算距离，"
            "得出 MA1 近场/贴箱的错误结论。按 calibration-spec/MICS.md 第 2 节口径，"
            "单路录音无公共时基，不得输出绝对距离。\n\n"
            "- old-artifact/：修复前交付文件原样副本（README/JSON/COMPARISON/tool-crosscheck）。\n"
            "- old-scripts/：生成它们的旧脚本原样副本（evidence/board-window-20261118-sweep/scripts）。\n"
            "- SHA256SUMS-before.txt：以上副本的 sha256（保留 hash，可复核）。\n"
            "- 新产物见本目录上一级（README.md / analysis/），旧结论作废。\n"
        )
    print("snapshot: wrote %d files + README to SUPERSEDED/" % len(rows))
    return True


def run(cmd, cwd):
    print("run:", " ".join(cmd))
    env = dict(os.environ)
    env["PYTHONIOENCODING"] = "utf-8"
    result = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, encoding="utf-8",
                            errors="replace", env=env)
    sys.stdout.write(result.stdout)
    sys.stderr.write(result.stderr)
    if result.returncode != 0:
        raise SystemExit("command failed rc=%d: %s" % (result.returncode, " ".join(cmd)))
    return result.stdout


def map_outputs(work_out, artifact):
    written = []
    run_dirs = []
    for name in sorted(os.listdir(work_out)):
        candidate = os.path.join(work_out, name)
        if os.path.isdir(candidate) and os.path.isfile(os.path.join(candidate, "analysis.json")):
            run_dirs.append(candidate)
    for run_dir in run_dirs:
        payload = json.load(open(os.path.join(run_dir, "analysis.json"), encoding="utf-8"))
        tag, mic = payload["tag"], payload["mic"]
        mapping = {
            "analysis.json": os.path.join("analysis", "%s-%s-analysis.json" % (tag, mic)),
            "fr-1_3oct.csv": os.path.join("fr", "%s-%s-fr-1_3oct.csv" % (tag, mic)),
            "fr-1_24oct.csv": os.path.join("fr", "%s-%s-fr-1_24oct.csv" % (tag, mic)),
            "rt-octave.csv": os.path.join("rt", "%s-%s-rt-octave.csv" % (tag, mic)),
            "csd.csv": os.path.join("csd", "%s-%s-csd.csv" % (tag, mic)),
            "ir-%s.wav" % mic: os.path.join("ir", "%s-%s-ir.wav" % (tag, mic)),
            "fr.svg": os.path.join("plots", "%s-%s-fr.svg" % (tag, mic)),
            "rt.svg": os.path.join("plots", "%s-%s-rt.svg" % (tag, mic)),
            "csd.svg": os.path.join("plots", "%s-%s-csd.svg" % (tag, mic)),
        }
        for source_name, rel in mapping.items():
            source = os.path.join(run_dir, source_name)
            if not os.path.isfile(source):
                continue
            destination = os.path.join(artifact, rel)
            os.makedirs(os.path.dirname(destination), exist_ok=True)
            shutil.copy2(source, destination)
            written.append(rel.replace(os.sep, "/"))
    for source_name, rel in (
        ("analysis-summary.json", os.path.join("analysis", "analysis-summary.json")),
        ("comparison.json", os.path.join("analysis", "comparison.json")),
        ("COMPARISON.md", os.path.join("analysis", "COMPARISON.md")),
        (os.path.join("plots", "fr-compare.svg"), os.path.join("plots", "fr-compare.svg")),
    ):
        source = os.path.join(work_out, source_name)
        if not os.path.isfile(source):
            continue
        destination = os.path.join(artifact, rel)
        os.makedirs(os.path.dirname(destination), exist_ok=True)
        shutil.copy2(source, destination)
        written.append(rel.replace(os.sep, "/"))
    return sorted(set(written)), run_dirs


def load_original_sums(sup):
    """读取修复前交付 SHA256SUMS.txt（旧产物 hash 基线，已随快照保留）。"""
    original = {}
    path = os.path.join(sup, "old-artifact", "SHA256SUMS.txt")
    if not os.path.isfile(path):
        return original
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if not line:
            continue
        parts = line.split(None, 1)
        if len(parts) == 2:
            original[parts[1].strip().replace("\\", "/")] = parts[0].strip()
    return original


def verify_byte_identity(artifact, sup):
    """FR/RT/CSD/IR-wav/SVG 必须与修复前交付 hash 逐字节一致（旧 hash 已快照保留）。"""
    mismatch = []
    compared = []
    missing = []
    original = load_original_sums(sup)
    for base in ("fr", "rt", "csd", "ir", "plots"):
        source_dir = os.path.join(artifact, base)
        if not os.path.isdir(source_dir):
            continue
        for name in sorted(os.listdir(source_dir)):
            rel = (base + "/" + name)
            old_hash = original.get(rel)
            if old_hash is None:
                continue
            new_hash = sha256_file(os.path.join(source_dir, name))
            compared.append({"file": rel, "old_sha256": old_hash,
                             "new_sha256": new_hash, "identical": new_hash == old_hash})
            if new_hash != old_hash:
                mismatch.append(rel)
    for rel in original:
        if rel.split("/", 1)[0] in ("fr", "rt", "csd", "ir", "plots"):
            if "/" in rel and not os.path.isfile(os.path.join(artifact, rel)):
                missing.append(rel)
    return compared, mismatch, missing


def verify_ir_semantics(artifact, sup):
    """IR 数字不变、只换语义：peak/polarity/xcorr 与旧一致；distance 必须 INVALID。"""
    rows = []
    old_root = os.path.join(sup, "old-artifact", "analysis")
    for name in sorted(os.listdir(old_root)):
        if not name.endswith("-analysis.json"):
            continue
        old = json.load(open(os.path.join(old_root, name), encoding="utf-8"))
        new_path = os.path.join(artifact, "analysis", name)
        if not os.path.isfile(new_path):
            rows.append({"file": name, "status": "MISSING_NEW"})
            continue
        new = json.load(open(new_path, encoding="utf-8"))
        direct = new["ir"]["direct_path"]
        rows.append({
            "file": name,
            "peak_index_same": old["ir"]["peak_index"] == new["ir"]["peak_index"],
            "polarity_same": old["ir"]["polarity"] == new["ir"]["polarity"],
            "xcorr_same": old["xcorr"] == new["xcorr"],
            "old_delay_ms_from_segment_start": old["ir"]["delay_ms_from_segment_start"],
            "new_relative_to_segment_ms": direct["relative_to_segment"]["delay_ms"],
            "relative_delay_same": abs(old["ir"]["delay_ms_from_segment_start"]
                                       - direct["relative_to_segment"]["delay_ms"]) <= 0.006,
            "new_distance_status": direct["distance"]["status"],
            "new_distance_m": direct["distance"]["distance_m"],
            "new_has_legacy_field": "delay_ms_from_segment_start" in json.dumps(new),
        })
    ok = all(row.get("peak_index_same") and row.get("polarity_same") and row.get("xcorr_same")
             and row.get("relative_delay_same")
             and row.get("new_distance_status") == "INVALID_NO_COMMON_TIMEBASE"
             and row.get("new_distance_m") is None
             and not row.get("new_has_legacy_field") for row in rows)
    return rows, ok


def update_sums(artifact):
    rows = []
    for root, _dirs, files in os.walk(artifact):
        for name in files:
            full = os.path.join(root, name)
            rel = os.path.relpath(full, artifact).replace(os.sep, "/")
            if rel == "SHA256SUMS.txt":
                continue
            rows.append((sha256_file(full), rel))
    rows.sort(key=lambda row: row[1])
    with open(os.path.join(artifact, "SHA256SUMS.txt"), "w", encoding="utf-8", newline="\n") as handle:
        for digest, rel in rows:
            handle.write("%s  %s\n" % (digest, rel))
    print("SHA256SUMS.txt: %d files" % len(rows))
    return len(rows)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", default=None)
    parser.add_argument("--work-dir", default=None)
    parser.add_argument("--report-out", default=None)
    parser.add_argument("--skip-snapshot", action="store_true")
    parser.add_argument("--force-snapshot", action="store_true")
    parser.add_argument("--sums-only", action="store_true")
    args = parser.parse_args()
    repo_root = args.repo_root or os.path.abspath(os.path.join(TOOLS_DIR, "..", "..", ".."))
    artifact = os.path.join(repo_root, REL_ARTIFACT)
    evidence = os.path.join(repo_root, REL_EVIDENCE)
    work_dir = args.work_dir or os.path.join(repo_root, "rt-stack", "target", "tmp-ir-distance", "regen-work")
    report_dir = args.report_out or os.path.join(repo_root, REL_REPORT)
    if args.sums_only:
        update_sums(artifact)
        return 0
    if not os.path.isdir(artifact):
        raise SystemExit("artifact dir not found: " + artifact)
    if not os.path.isdir(evidence):
        raise SystemExit("evidence dir not found: " + evidence)
    if not args.skip_snapshot:
        snapshot_old(artifact, evidence, args.force_snapshot)
    sup = os.path.join(artifact, "SUPERSEDED")
    if not os.path.isdir(sup):
        raise SystemExit("SUPERSEDED/ missing; run snapshot first")
    work_out = os.path.join(work_dir, "analysis-new")
    if os.path.isdir(work_out):
        shutil.rmtree(work_out)
    python = sys.executable
    run([python, os.path.join(TOOLS_DIR, "analyze_sweep.py"), "--root", evidence, "--out", work_out], repo_root)
    run([python, os.path.join(TOOLS_DIR, "compare_runs.py"), work_out, work_out], repo_root)
    written, run_dirs = map_outputs(work_out, artifact)
    compared, mismatch, missing = verify_byte_identity(artifact, sup)
    ir_rows, ir_ok = verify_ir_semantics(artifact, sup)
    sums_count = update_sums(artifact)
    report = {
        "task": "task-229",
        "generated_by": "rt-stack/tools/sweep-analysis/regenerate_artifacts.py",
        "evidence_root": evidence,
        "artifact_root": artifact,
        "work_out": work_out,
        "runs": [os.path.basename(run_dir) for run_dir in run_dirs],
        "written_files": written,
        "byte_identity_non_ir": {"compared": compared, "mismatch": mismatch, "missing": missing,
                                 "ok": not mismatch and not missing},
        "ir_semantics": {"rows": ir_rows, "ok": ir_ok},
        "sha256sums_files": sums_count,
    }
    os.makedirs(report_dir, exist_ok=True)
    write_json(os.path.join(report_dir, "regen-report.json"), report)
    print("regen-report.json -> %s" % os.path.join(report_dir, "regen-report.json"))
    print("byte-identity non-IR: %d compared, %d mismatch, %d missing" % (
        len(compared), len(mismatch), len(missing)))
    print("ir semantics ok:", ir_ok)
    if mismatch or missing or not ir_ok:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())