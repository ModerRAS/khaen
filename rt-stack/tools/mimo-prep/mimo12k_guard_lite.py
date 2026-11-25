#!/usr/bin/env python3
"""task-255 board-side 12k provenance gate -- stdlib only (no numpy, no build tree).

The full guard (mimo_kit/provenance.py) does coefficient content checks with numpy on the
dev box.  The board window only needs a portable fail-closed gate that answers:
  * is a provenance record present and self-consistent?
  * do the recorded hashes match the bytes that would be rendered/applied?
  * is the plan bound to that artifact and its compile report?
  * does anything claim synthetic/dry-run?

Usage (board / any python3):
  python3 mimo12k_guard_lite.py check --artifact /var/tmp/khaen-12k-offline/artifacts \
      --plan-dir /var/tmp/khaen-12k-offline/plan [--expect-g-pure-sha SHA]
      [--expect-plan-sha SHA] [--json-out gate.json]
Exit codes: 0 = PASS, 3 = REFUSE (do not render/apply), 2 = usage/IO.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys

PARAMS_REQUIRED = ("schema", "fs_hz", "lowrate_hz", "group", "participants", "taps",
                   "d12", "resamp_taps", "resamp_delay", "dsys_samples", "g_file",
                   "resamp_file", "g_sha256")
A12_PARTICIPANTS = ["FL", "FR", "SL", "SR", "SUB1"]
A12_TAPS = 4096
A12_D12 = 120
A12_GROUP = 4
A12_DSYS = 574
A12_RESAMP_TAPS = 95
ART_PROV_SCHEMA = "khaen-mimo-12k-artifact-provenance/v1"
PLAN_PROV_SCHEMA = "khaen-mimo-12k-plan-provenance/v1"


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def load_json(path, default=None):
    try:
        with open(path, "r", encoding="utf-8") as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return default


def claim_tags_synthetic(text):
    t = str(text or "").lower()
    for neg in ("not a dry-run", "not dry-run", "not a synthetic", "not synthetic",
                "no dry-run", "no synthetic", "non-synthetic", "non synthetic",
                "never synthetic", "not from the dry-run", "not from dry-run"):
        t = t.replace(neg, "")
    return ("dry-run" in t) or ("dryrun" in t) or ("synthetic" in t)


def absent_from_dryrun(path):
    p = str(path or "").replace("\\", "/").lower()
    return ("/prep/dryrun/" in p) or ("/dryrun/" in p and "probe/real" not in p)


class Gate(object):
    def __init__(self):
        self.codes = []
        self.reasons = {}
        self.metrics = {}

    def fail(self, code, msg):
        if code not in self.codes:
            self.codes.append(code)
        self.reasons.setdefault(code, msg)

    def verdict(self):
        return {"ok": not self.codes, "codes": self.codes, "reasons": self.reasons,
                "metrics": self.metrics, "schema": "khaen-mimo-12k-guard-lite/v1"}


def check_artifact(g, adir, expect_g_pure_sha=None):
    adir = os.path.abspath(adir)
    g.metrics["artifact_dir"] = adir
    pj = os.path.join(adir, "params.json")
    params = load_json(pj, None)
    if not isinstance(params, dict):
        g.fail("a12_missing_params", "artifact has no params.json")
        return
    g.metrics["params_sha256"] = sha256_file(pj)
    missing = [k for k in PARAMS_REQUIRED if k not in params]
    if missing:
        g.fail("a12_params_missing_fields", "params.json missing fields: %s" % missing)
    if params.get("participants") != A12_PARTICIPANTS:
        g.fail("a12_params_participants", "participants are not the 5 participant design set")
    if int(params.get("taps") or 0) != A12_TAPS or int(params.get("d12") or 0) != A12_D12:
        g.fail("a12_params_inconsistent", "taps/D12 are not 4096/120")
    if int(params.get("group") or 0) != A12_GROUP or int(params.get("fs_hz") or 0) != 48000 \
            or int(params.get("lowrate_hz") or 0) != 12000:
        g.fail("a12_params_inconsistent", "fs/lowrate/group are not 48000/12000/4")
    if int(params.get("resamp_taps") or 0) != A12_RESAMP_TAPS:
        g.fail("a12_params_inconsistent", "resamp_taps is not 95")
    try:
        if int(params.get("dsys_samples")) != 2 * int(params.get("resamp_delay")) \
                + int(params.get("group")) * int(params.get("d12")):
            g.fail("a12_params_delay", "dsys_samples != 2*resamp_delay + group*d12")
    except (TypeError, ValueError):
        g.fail("a12_params_delay", "dsys/resamp_delay/group/d12 are not integers")
    g_file = str(params.get("g_file") or "")
    gp = os.path.join(adir, g_file) if g_file else ""
    if not g_file or not os.path.isfile(gp):
        g.fail("a12_missing_g_file", "g coefficient file missing")
    else:
        g.metrics["g_file_sha256"] = sha256_file(gp)
        try:
            n_parts = len(params.get("participants") or [])
            n_taps = int(params.get("taps"))
            if os.path.getsize(gp) != n_parts * n_parts * n_taps * 4:
                g.fail("a12_g_file_size", "g file size is not M*M*taps*4")
        except (TypeError, ValueError):
            g.fail("a12_g_file_size", "cannot size-check g file")
    r_file = str(params.get("resamp_file") or "")
    rp = os.path.join(adir, r_file) if r_file else ""
    if not r_file or not os.path.isfile(rp):
        g.fail("a12_missing_resamp_file", "resamp filter file missing")
    else:
        g.metrics["resamp_sha256"] = sha256_file(rp)
        try:
            if os.path.getsize(rp) != int(params.get("resamp_taps")) * 4:
                g.fail("a12_resamp_file_size", "resamp file size is not resamp_taps*4")
        except (TypeError, ValueError):
            g.fail("a12_resamp_file_size", "cannot size-check resamp file")
    prov = load_json(os.path.join(adir, "provenance.json"))
    if not isinstance(prov, dict):
        g.fail("a12_missing_provenance", "artifact has no provenance.json")
        return
    g.metrics["provenance_present"] = True
    if prov.get("schema") not in (None, ART_PROV_SCHEMA):
        g.fail("a12_provenance_schema", "artifact provenance schema mismatch")
    if prov.get("synthetic") is True or claim_tags_synthetic(prov.get("claim")):
        g.fail("a12_synthetic_marker", "artifact provenance is tagged synthetic/dry-run")
    art = prov.get("artifact") or {}
    if art.get("params_sha256") and art["params_sha256"] != g.metrics.get("params_sha256"):
        g.fail("a12_params_sha_mismatch", "params.json bytes do not match provenance")
    if g.metrics.get("g_file_sha256") and art.get("g_file_sha256") \
            and art["g_file_sha256"] != g.metrics["g_file_sha256"]:
        g.fail("a12_g_file_sha_mismatch", "g f32 bytes do not match provenance")
    if g.metrics.get("resamp_sha256") and art.get("resamp_sha256") \
            and art["resamp_sha256"] != g.metrics["resamp_sha256"]:
        g.fail("a12_resamp_sha_mismatch", "resamp bytes do not match provenance")
    params_g = str(params.get("g_sha256") or "")
    prov_g = str(art.get("g_pure_sha256") or "")
    if not params_g:
        g.fail("a12_missing_g_sha", "params.json has no g_sha256")
    if params_g and prov_g and params_g != prov_g:
        g.fail("a12_g_pure_sha_mismatch", "params.g_sha256 != provenance g_pure_sha256")
    if expect_g_pure_sha and params_g and params_g != expect_g_pure_sha:
        g.fail("a12_g_pure_sha_mismatch", "g_pure sha256 is not the windowed expected value")
    f64 = str(art.get("g_f64_npz") or "")
    if not f64:
        g.fail("a12_missing_g_f64", "artifact has no g f64 content copy binding the f32 samples")
    else:
        fp = os.path.join(adir, f64)
        if not os.path.isfile(fp):
            g.fail("a12_missing_g_f64", "g f64 copy missing")
        elif art.get("g_f64_npz_sha256") and art["g_f64_npz_sha256"] != sha256_file(fp):
            g.fail("a12_g_f64_sha_mismatch", "g f64 npz bytes do not match provenance")
    h = prov.get("h") or {}
    h_sha = str(h.get("sha256") or "")
    if not h_sha:
        g.fail("a12_missing_h_provenance", "provenance has no H record")
    hsrc = str(h.get("source") or h.get("npz") or "")
    if hsrc and absent_from_dryrun(hsrc):
        g.fail("a12_dryrun_source", "H source is under the dry-run root")
    if (h.get("guard") or {}).get("ok") is not True:
        g.fail("a12_h_guard_not_pass", "provenance H guard is missing or not ok")
    d = prov.get("design") or {}
    if not d.get("summary_json"):
        g.fail("a12_missing_design_summary", "provenance has no design-summary binding")
    g.metrics["h_sha256"] = h_sha or None
    g.metrics["g_pure_sha256"] = params_g or None
    g.metrics["g_f32_content_sha256"] = art.get("g_f32_content_sha256")


def check_plan(g, pdir, adir_metrics=None, expect_plan_sha=None,
               expect_artifact_params_sha=None, expect_g_pure_sha=None):
    pdir = os.path.abspath(pdir)
    g.metrics["plan_dir"] = pdir
    plan_path = os.path.join(pdir, "plan.bin")
    if not os.path.isfile(plan_path):
        g.fail("a12_missing_plan", "plan.bin missing")
        return
    plan_sha = sha256_file(plan_path)
    g.metrics["plan_sha256"] = plan_sha
    if expect_plan_sha and plan_sha != expect_plan_sha:
        g.fail("a12_plan_sha_mismatch", "plan.bin sha is not the windowed expected value")
    rep_path = os.path.join(pdir, "compile-report.json")
    rep = load_json(rep_path)
    if not isinstance(rep, dict):
        g.fail("a12_plan_missing_compile_report", "compile-report.json missing")
    else:
        g.metrics["compile_report_sha256"] = sha256_file(rep_path)
        claimed = ((rep.get("plan") or {}).get("plan_sha256") or rep.get("plan_sha256"))
        if not claimed:
            g.fail("a12_plan_missing_compile_report", "compile-report carries no plan_sha256")
        elif str(claimed).lower() != plan_sha.lower():
            g.fail("a12_plan_sha_mismatch", "compile-report plan_sha256 != plan.bin bytes")
        for key in ("claim", "design_claim", "compiler_note"):
            if claim_tags_synthetic(rep.get(key)):
                g.fail("a12_plan_report_synthetic", "compile-report tags the plan synthetic")
    prov = load_json(os.path.join(pdir, "plan-provenance.json"))
    if not isinstance(prov, dict):
        g.fail("a12_plan_missing_provenance", "plan-provenance.json missing")
        return
    g.metrics["plan_provenance_present"] = True
    if prov.get("schema") not in (None, PLAN_PROV_SCHEMA):
        g.fail("a12_plan_provenance_schema", "plan provenance schema mismatch")
    if prov.get("synthetic") is True or claim_tags_synthetic(prov.get("claim")):
        g.fail("a12_plan_synthetic", "plan provenance is tagged synthetic")
    if prov.get("plan_sha256") and str(prov["plan_sha256"]).lower() != plan_sha.lower():
        g.fail("a12_plan_provenance_sha_mismatch", "plan provenance plan_sha256 != plan.bin")
    if g.metrics.get("compile_report_sha256") and prov.get("compile_report_sha256") \
            and prov["compile_report_sha256"] != g.metrics["compile_report_sha256"]:
        g.fail("a12_plan_compile_report_mismatch",
               "plan provenance compile_report_sha256 != compile-report.json")
    art = prov.get("mimo12k_artifact") or {}
    if (prov.get("artifact_guard") or {}).get("ok") is not True:
        g.fail("a12_plan_artifact_guard_not_pass", "plan provenance artifact guard not ok")
    if not art.get("artifact_dir"):
        g.fail("a12_plan_missing_artifact_binding", "plan provenance has no artifact binding")
    if not art.get("params_sha256") or not art.get("g_pure_sha256"):
        g.fail("a12_plan_artifact_mismatch", "plan provenance artifact hashes incomplete")
    if adir_metrics:
        for key, actual in (("params_sha256", adir_metrics.get("params_sha256")),
                            ("g_file_sha256", adir_metrics.get("g_file_sha256")),
                            ("g_pure_sha256", adir_metrics.get("g_pure_sha256")),
                            ("resamp_sha256", adir_metrics.get("resamp_sha256")),
                            ("h_sha256", adir_metrics.get("h_sha256"))):
            if art.get(key) and actual and str(art[key]) != str(actual):
                g.fail("a12_plan_artifact_mismatch",
                       "plan provenance %s != artifact provenance" % key)
        if expect_g_pure_sha and adir_metrics.get("g_pure_sha256") \
                and adir_metrics["g_pure_sha256"] != expect_g_pure_sha:
            g.fail("a12_g_pure_sha_mismatch", "artifact g_pure sha is not the expected value")
        if expect_artifact_params_sha \
                and adir_metrics.get("params_sha256") != expect_artifact_params_sha:
            g.fail("a12_params_sha_mismatch", "artifact params sha is not the expected value")


def cmd_check(args):
    g = Gate()
    if not args.artifact and not args.plan_dir:
        g.fail("lite_usage", "at least one of --artifact/--plan-dir is required")
    adir_metrics = None
    if args.artifact:
        check_artifact(g, args.artifact, expect_g_pure_sha=args.expect_g_pure_sha)
        adir_metrics = dict(g.metrics)
    if args.plan_dir:
        check_plan(g, args.plan_dir, adir_metrics=adir_metrics,
                   expect_plan_sha=args.expect_plan_sha,
                   expect_artifact_params_sha=args.expect_artifact_params_sha,
                   expect_g_pure_sha=args.expect_g_pure_sha)
    v = g.verdict()
    text = json.dumps(v, ensure_ascii=False, indent=2)
    if args.json_out:
        os.makedirs(os.path.dirname(os.path.abspath(args.json_out)), exist_ok=True)
        with open(args.json_out, "w", encoding="utf-8") as fh:
            fh.write(text + "\n")
    print(text)
    return 0 if v["ok"] else 3


def main(argv=None):
    ap = argparse.ArgumentParser(description="task-255 board-side 12k provenance gate (stdlib)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("check")
    p.add_argument("--artifact", default=None)
    p.add_argument("--plan-dir", default=None)
    p.add_argument("--expect-g-pure-sha", default=None)
    p.add_argument("--expect-plan-sha", default=None)
    p.add_argument("--expect-artifact-params-sha", default=None)
    p.add_argument("--json-out", default=None)
    p.set_defaults(func=cmd_check)
    args = ap.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
