#!/usr/bin/env python3
"""task-255 12k provenance guard CLI (offline, no board/sound/git).

Commands:
  registry          dump the synthetic 12k fixture registry
  redesign          re-run the measured-H 12k design AND package it with provenance;
                    refuses unless g_pure/G_fir reproduce the delivered values and
                    provenance.g12_artifact_guard() passes
  check-artifact    fail-closed guard on a [mimo12k] artifact directory
  make-synthetic    generate the dry-run-H derived negative fixture (guard must refuse)
  stamp-plan        bind the delivered 12k plan to the guarded artifact
  check-plan        fail-closed guard on a 12k plan directory
  smoke             run the pass/negative acceptance matrix into guard-results/

Exit codes: 0 = pass / expected, 3 = REFUSE (provenance violation), 2 = usage/IO.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

from mimo_kit import design12k as d12  # noqa: E402
from mimo_kit import provenance as pv  # noqa: E402

WORK_DEFAULT = os.path.join(pv.PROJECT, "rt-stack", "target", "tmp-12k-guard")
RESULTS_DEFAULT = os.path.join(d12.EVID_PROV12K, "guard-results")
DELIVERED_ARTIFACT = d12.DELIVERED_ARTIFACT
GUARDED_ARTIFACT = d12.GUARDED_ARTIFACT
GUARDED_PLAN = d12.GUARDED_PLAN
SYNTHETIC_FIXTURE = d12.SYNTHETIC_FIXTURE


def _emit(obj, json_out=None):
    text = json.dumps(obj, ensure_ascii=False, indent=2)
    if json_out:
        os.makedirs(os.path.dirname(os.path.abspath(json_out)), exist_ok=True)
        with open(json_out, "w", encoding="utf-8") as fh:
            fh.write(text + "\n")
    print(text)


def _verdict_brief(v):
    return {"ok": bool(v.get("ok")), "codes": list(v.get("codes") or []),
            "reasons": {k: v["reasons"][k] for k in (v.get("codes") or []) if k in v.get("reasons", {})}}


def _cmd_registry(args):
    reg = pv.synthetic_registry_12k()
    _emit(reg, args.out)
    return 0


def _cmd_redesign(args):
    try:
        r = d12.redesign_artifact(out_dir=args.out, h_npz=args.h, design_script=args.design_script,
                                  taps=args.taps, d12=args.d12, lam_rel=args.lam,
                                  ratio=args.ratio, delivered_artifact=args.delivered_artifact,
                                  delivered_summary=args.delivered_summary,
                                  expect_g_pure_sha=args.expect_g_pure_sha,
                                  expect_g_fir_sha=args.expect_g_fir_sha,
                                  strict_delivered=not args.allow_delivered_mismatch)
    except pv.ProvenanceRefusal as exc:
        _emit({"ok": False, "refused": exc.codes, "detail": exc.detail}, args.json_out)
        return 3
    out = {"ok": True, "artifact_dir": r["artifact_dir"],
           "g_pure_sha256": r["g_pure_sha256"], "G_fir_sha256": r["G_fir_sha256"],
           "reproduced_delivered": (r["parity"] or {}).get("matched"),
           "guard": _verdict_brief(r["guard"]),
           "provenance": os.path.join(r["artifact_dir"], "provenance.json")}
    _emit(out, args.json_out)
    return 0


def _cmd_check_artifact(args):
    v = pv.g12_artifact_guard(args.artifact, provenance_path=args.provenance, live=True)
    _emit(v, args.json_out)
    return 0 if v["ok"] else 3


def _cmd_make_synthetic(args):
    r = d12.make_synthetic_fixture(out_dir=args.out, dryrun_h=args.dryrun_h)
    out = {"ok": True, "fixture_dir": r["fixture_dir"], "refused_as_expected": not r["guard"]["ok"],
           "guard": _verdict_brief(r["guard"]),
           "g_pure_sha256": r["g_pure_sha256"]}
    _emit(out, args.json_out)
    return 0 if out["refused_as_expected"] else 1


def _cmd_stamp_plan(args):
    try:
        r = d12.stamp_plan(plan_dir=args.plan_dir, artifact_dir=args.artifact, out_dir=args.out)
    except pv.ProvenanceRefusal as exc:
        _emit({"ok": False, "refused": exc.codes, "detail": exc.detail}, args.json_out)
        return 3
    _emit({"ok": True, "plan_dir": r["plan_dir"], "guard": _verdict_brief(r["guard"]),
           "provenance": os.path.join(r["plan_dir"], "plan-provenance.json")}, args.json_out)
    return 0


def _cmd_check_plan(args):
    v = pv.g12_plan_guard(args.plan_dir, artifact_dir=args.artifact,
                          provenance_path=args.provenance, live=True)
    _emit(v, args.json_out)
    return 0 if v["ok"] else 3


# ---------------------------------------------------------------- smoke matrix
def _reset(path):
    shutil.rmtree(path, ignore_errors=True)
    os.makedirs(path, exist_ok=True)
    return path


def _copy_tree(src, dst):
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst)
    return dst


def _load(p):
    with open(p, "r", encoding="utf-8") as fh:
        return json.load(fh)


def _save(p, obj, indent=1):
    with open(p, "w", encoding="utf-8") as fh:
        json.dump(obj, fh, ensure_ascii=False, indent=indent)
        fh.write("\n")


def _flip_f32(path, index=1234):
    with open(path, "r+b") as fh:
        fh.seek(index * 4)
        raw = bytearray(fh.read(4))
        raw[0] ^= 0x01
        fh.seek(index * 4)
        fh.write(bytes(raw))


def _smoke_record(records, results_dir, name, verdict, expect_ok, required_codes=(),
                  forbidden_codes=(), extra=None):
    codes = list(verdict.get("codes") or [])
    ok = bool(verdict.get("ok"))
    missing = [c for c in required_codes if c not in codes]
    forbidden = [c for c in forbidden_codes if c in codes]
    passed = (ok == expect_ok) and not missing and not forbidden
    rec = {"case": name, "expected_ok": expect_ok, "actual_ok": ok,
           "required_codes": list(required_codes),
           "missing_codes": missing, "forbidden_codes": forbidden,
           "codes": codes, "passed": passed,
           "reasons": {k: verdict.get("reasons", {}).get(k) for k in codes}}
    if extra:
        rec.update(extra)
    _save(os.path.join(results_dir, name + ".json"), rec)
    records.append(rec)
    print("[%s] %-34s expected_ok=%-5s actual_ok=%-5s codes=%s"
          % ("PASS" if passed else "FAIL", name, expect_ok, ok, codes))
    return passed


def _cmd_smoke(args):
    work = _reset(args.work or WORK_DEFAULT)
    results = _reset(args.out_results or RESULTS_DEFAULT)
    records = []
    all_ok = True

    def rec(name, verdict, expect_ok, required=(), forbidden=(), extra=None):
        nonlocal all_ok
        all_ok = _smoke_record(records, results, name, verdict, expect_ok, required,
                               forbidden, extra) and all_ok

    # 1) positive: guarded redesign from the measured H (reproduces the delivered artifact)
    art = args.artifact or GUARDED_ARTIFACT
    try:
        r = d12.redesign_artifact(out_dir=art, expect_g_pure_sha=d12.KNOWN_12K_G_PURE_SHA,
                                  expect_g_fir_sha=d12.KNOWN_12K_G_FIR_SHA)
        rec("artifact-pass-measured-h", r["guard"], True,
            extra={"artifact_dir": art, "g_pure_sha256": r["g_pure_sha256"],
                   "G_fir_sha256": r["G_fir_sha256"],
                   "reproduced_delivered": (r["parity"] or {}).get("matched")})
    except pv.ProvenanceRefusal as exc:  # pragma: no cover - smoke must fail loudly
        rec("artifact-pass-measured-h", {"ok": False, "codes": exc.codes,
                                         "reasons": {}}, True,
            extra={"detail": exc.detail})
    # 2) positive: plan stamped against the guarded artifact
    try:
        p = d12.stamp_plan(plan_dir=args.plan_dir or d12.DELIVERED_PLAN_DIR,
                           artifact_dir=art, out_dir=args.plan_out or GUARDED_PLAN)
        rec("plan-pass-bound-to-artifact", p["guard"], True,
            extra={"plan_dir": p["plan_dir"]})
    except pv.ProvenanceRefusal as exc:  # pragma: no cover
        rec("plan-pass-bound-to-artifact", {"ok": False, "codes": exc.codes,
                                            "reasons": {}}, True, extra={"detail": exc.detail})
    # 3) the delivered task-253 artifact (no provenance at all) must be refused
    v = pv.g12_artifact_guard(DELIVERED_ARTIFACT)
    rec("artifact-unguarded-delivered-refused", v, False,
        required=("a12_missing_provenance",), extra={"artifact_dir": DELIVERED_ARTIFACT})
    # 4) synthetic 12k G from the dry-run H must be refused (markers + registered content)
    syn = SYNTHETIC_FIXTURE
    if not os.path.isfile(os.path.join(syn, "provenance.json")):
        d12.make_synthetic_fixture(out_dir=syn)
    v = pv.g12_artifact_guard(syn)
    rec("artifact-synthetic-12k-refused", v, False,
        required=("a12_synthetic_marker", "a12_dryrun_source", "a12_synthetic_h_sha",
                  "a12_h_guard_not_pass", "a12_synthetic_content"),
        extra={"artifact_dir": syn})
    # 5) content tamper: flip one f32 coefficient (file sha + f64 binding must catch it)
    tam = _copy_tree(art, os.path.join(work, "tamper-g-content"))
    _flip_f32(os.path.join(tam, "g12_v2_4096.f32"))
    v = pv.g12_artifact_guard(tam)
    rec("artifact-content-tamper-refused", v, False,
        required=("a12_g_file_sha_mismatch", "a12_g_content_mismatch"))
    # 6) missing provenance fields must be refused
    mf = _copy_tree(art, os.path.join(work, "missing-fields"))
    params = _load(os.path.join(mf, "params.json"))
    params.pop("g_sha256", None)
    params.pop("taps", None)
    _save(os.path.join(mf, "params.json"), params)
    v = pv.g12_artifact_guard(mf)
    rec("artifact-missing-fields-refused", v, False,
        required=("a12_params_missing_fields", "a12_params_sha_mismatch"),
        extra={"missing": ["g_sha256", "taps"]})
    # 7) params tamper only (design-record drift)
    pt = _copy_tree(art, os.path.join(work, "tamper-params"))
    params = _load(os.path.join(pt, "params.json"))
    params["notes"] = "tampered notes"
    _save(os.path.join(pt, "params.json"), params)
    v = pv.g12_artifact_guard(pt)
    rec("artifact-params-tamper-refused", v, False, required=("a12_params_sha_mismatch",))
    # 8) synthetic markers stripped but content unchanged -> registered content sha still bites
    st = _copy_tree(syn, os.path.join(work, "synthetic-markers-stripped"))
    prov = _load(os.path.join(st, "provenance.json"))
    prov["synthetic"] = False
    prov["claim"] = "measured arm reprocessed for offline wiring"
    prov["h"]["source"] = art + "\\..\\real-h.npz"
    _save(os.path.join(st, "provenance.json"), prov)
    v = pv.g12_artifact_guard(st)
    rec("artifact-synthetic-content-refused", v, False,
        required=("a12_synthetic_content", "a12_synthetic_h_sha"))
    # 9) plan bytes tampered -> compile-report binding + provenance sha mismatch
    pl = _copy_tree(args.plan_out or GUARDED_PLAN, os.path.join(work, "plan-tamper"))
    _flip_f32(os.path.join(pl, "plan.bin"), index=0)
    v = pv.g12_plan_guard(pl, artifact_dir=art)
    rec("plan-bytes-tamper-refused", v, False,
        required=("a12_plan_sha_mismatch", "a12_plan_provenance_sha_mismatch"))
    # 10) compile-report tamper only
    cr = _copy_tree(args.plan_out or GUARDED_PLAN, os.path.join(work, "plan-compile-report-tamper"))
    rep = _load(os.path.join(cr, "compile-report.json"))
    rep["plan_sha256"] = "0" * 64
    _save(os.path.join(cr, "compile-report.json"), rep)
    v = pv.g12_plan_guard(cr, artifact_dir=art)
    rec("plan-compile-report-tamper-refused", v, False,
        required=("a12_plan_sha_mismatch", "a12_plan_compile_report_mismatch"))
    # 11) plan provenance missing -> refused
    pm = _copy_tree(args.plan_out or GUARDED_PLAN, os.path.join(work, "plan-missing-provenance"))
    os.remove(os.path.join(pm, "plan-provenance.json"))
    v = pv.g12_plan_guard(pm, artifact_dir=art)
    rec("plan-missing-provenance-refused", v, False, required=("a12_plan_missing_provenance",))
    # 12) plan bound to a different artifact sha -> refused
    pa = _copy_tree(args.plan_out or GUARDED_PLAN, os.path.join(work, "plan-artifact-mismatch"))
    prov = _load(os.path.join(pa, "plan-provenance.json"))
    prov["mimo12k_artifact"]["g_pure_sha256"] = "0" * 64
    _save(os.path.join(pa, "plan-provenance.json"), prov)
    v = pv.g12_plan_guard(pa, artifact_dir=art)
    rec("plan-artifact-mismatch-refused", v, False, required=("a12_plan_artifact_mismatch",))
    # 13) H source checks: measured H passes, dry-run H is refused
    hv = pv.h12_source_check(d12.REAL_H)
    rec("h12-measured-h-passes", hv, True,
        extra={"h": d12.REAL_H, "mic_names": hv["metrics"].get("mic_names")})
    hv = pv.h12_source_check(d12.DRYRUN_H)
    rec("h12-dryrun-h-refused", hv, False, required=("h12_ir_peak_spread",))
    # 14) board-side stdlib lite gate (no numpy) on the guarded artifact/plan
    lite = os.path.join(HERE, "mimo12k_guard_lite.py")
    sub = subprocess.run([sys.executable, lite, "check", "--artifact", art,
                          "--plan-dir", args.plan_out or GUARDED_PLAN],
                         capture_output=True, text=True, encoding="utf-8", errors="replace")
    lite_out = {"ok": sub.returncode == 0, "exit_code": sub.returncode,
                "stdout_tail": (sub.stdout or "")[-400:]}
    _save(os.path.join(results, "lite-gate-pass.json"), lite_out)
    rec("lite-gate-pass", {"ok": lite_out["ok"], "codes": [], "reasons": {}}, True,
        extra={"exit_code": sub.returncode})
    sub = subprocess.run([sys.executable, lite, "check", "--artifact", syn],
                         capture_output=True, text=True, encoding="utf-8", errors="replace")
    lite_bad = {"ok": sub.returncode == 0, "exit_code": sub.returncode,
                "stdout_tail": (sub.stdout or "")[-400:]}
    _save(os.path.join(results, "lite-gate-synthetic-refused.json"), lite_bad)
    rec("lite-gate-synthetic-refused", {"ok": lite_bad["ok"], "codes": [], "reasons": {}}, False,
        extra={"exit_code": sub.returncode})
    summary = {"schema": "tspi-mimo-12k-guard-smoke/v1", "task": "task-255",
               "passed": int(all_ok), "cases": len(records),
               "failed": [r["case"] for r in records if not r["passed"]],
               "records": records, "work_dir": work, "results_dir": results,
               "claim": "offline pass/negative matrix: measured-H artifact PASS, "
                        "synthetic/unguarded/tampered/partial-provenance artifacts and "
                        "unbound/tampered plans REFUSED; no board/sound/git"}
    _save(os.path.join(results, "smoke-summary.json"), summary)
    print("\nSMOKE %s (%d/%d cases)" % ("PASS" if all_ok else "FAIL",
                                        sum(1 for r in records if r["passed"]), len(records)))
    if args.json_out:
        _save(args.json_out, summary)
    return 0 if all_ok else 1


def main(argv=None):
    ap = argparse.ArgumentParser(description="task-255 12k MIMO provenance guard (offline)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("registry")
    p.add_argument("--out", default=None)
    p.set_defaults(func=_cmd_registry)

    p = sub.add_parser("redesign")
    p.add_argument("--out", default=GUARDED_ARTIFACT)
    p.add_argument("--h", default=None)
    p.add_argument("--design-script", default=None)
    p.add_argument("--delivered-artifact", default=None)
    p.add_argument("--delivered-summary", default=None)
    p.add_argument("--taps", type=int, default=pv.A12_TAPS)
    p.add_argument("--d12", type=int, default=pv.A12_D12)
    p.add_argument("--lam", type=float, default=d12.DESIGN_PARAMS["lam_rel"])
    p.add_argument("--ratio", type=float, default=d12.DESIGN_PARAMS["ratio"])
    p.add_argument("--expect-g-pure-sha", default=None)
    p.add_argument("--expect-g-fir-sha", default=None)
    p.add_argument("--allow-delivered-mismatch", action="store_true")
    p.add_argument("--json-out", default=None)
    p.set_defaults(func=_cmd_redesign)

    p = sub.add_parser("check-artifact")
    p.add_argument("--artifact", required=True)
    p.add_argument("--provenance", default=None)
    p.add_argument("--json-out", default=None)
    p.set_defaults(func=_cmd_check_artifact)

    p = sub.add_parser("make-synthetic")
    p.add_argument("--out", default=SYNTHETIC_FIXTURE)
    p.add_argument("--dryrun-h", default=None)
    p.add_argument("--json-out", default=None)
    p.set_defaults(func=_cmd_make_synthetic)

    p = sub.add_parser("stamp-plan")
    p.add_argument("--plan-dir", default=d12.DELIVERED_PLAN_DIR)
    p.add_argument("--artifact", default=GUARDED_ARTIFACT)
    p.add_argument("--out", default=GUARDED_PLAN)
    p.add_argument("--json-out", default=None)
    p.set_defaults(func=_cmd_stamp_plan)

    p = sub.add_parser("check-plan")
    p.add_argument("--plan-dir", required=True)
    p.add_argument("--artifact", default=None)
    p.add_argument("--provenance", default=None)
    p.add_argument("--json-out", default=None)
    p.set_defaults(func=_cmd_check_plan)

    p = sub.add_parser("smoke")
    p.add_argument("--work", default=None)
    p.add_argument("--out-results", default=None)
    p.add_argument("--artifact", default=None)
    p.add_argument("--plan-dir", default=None)
    p.add_argument("--plan-out", default=None)
    p.add_argument("--json-out", default=None)
    p.set_defaults(func=_cmd_smoke)

    args = ap.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
