#!/usr/bin/env python3
"""Unit/negative tests for the 12k [mimo12k] provenance guard (task-255).

Run: python -m pytest tools/mimo-prep/mimo_kit/tests/test_provenance12k.py -q
 or: python tools/mimo-prep/mimo_kit/tests/test_provenance12k.py

Covers the acceptance matrix:
  * measured-H guarded artifact PASSES (redesign reproduces g_pure f0a4a689... exactly);
  * the deliverered task-253 artifact (no provenance record) is REFUSED;
  * a 12k G designed from the dry-run synthetic H is REFUSED (markers, dry-run H source,
    registered synthetic content even after the markers are stripped);
  * content tamper (g f32 sample, f64 copy, params, plan bytes, compile report) REFUSED;
  * missing provenance / missing fields / missing f64 content copy / unbound plan REFUSED;
  * plan <-> guarded artifact <-> compile-report binding enforced;
  * measured H passes h12_source_check, dry-run H and silent/out-of-window/dead-row H fail;
  * the board-side stdlib lite gate mirrors the pass/refuse decisions.
"""
import contextlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
KIT = os.path.dirname(HERE)
sys.path.insert(0, KIT)

import design12k as d12  # noqa: E402
import provenance as pv  # noqa: E402

PROJECT = os.path.abspath(os.path.join(KIT, "..", "..", "..", ".."))
EVIDENCE = os.path.join(PROJECT, "rt-stack", "evidence")
EVID_12K = os.path.join(EVIDENCE, "mimo-12k-v2")
PROV12K = os.path.join(EVIDENCE, "mimo-12k-provenance")
GART = os.path.join(PROV12K, "artifact-12k-guarded")
GPLAN = os.path.join(PROV12K, "plan-12k-guarded")
SYN = os.path.join(PROV12K, "synthetic-12k")
DELIVERED = os.path.join(EVID_12K, "artifacts")
DELIVERED_PLAN = os.path.join(EVID_12K, "runtime", "plan-direct-12k")
REAL_H = os.path.join(PROJECT, "scratch", "lead", "mimo-v2", "probe", "real", "h_identity.npz")
DRYRUN_H = os.path.join(EVIDENCE, "mimo-pilot-3mic", "prep", "dryrun", "out", "h", "h_estimate.npz")
KNOWN_G = "f0a4a689c75a7e06cfc66d8b62ce0db6173337a32e1ded27e9ca96e183804359"
TEST_TMP = os.path.join(PROJECT, "rt-stack", "target", "tmp-12k-guard-tests")
os.makedirs(TEST_TMP, exist_ok=True)
LITE = os.path.join(os.path.dirname(KIT), "mimo12k_guard_lite.py")


def tempdir():
    return tempfile.TemporaryDirectory(dir=TEST_TMP)


def _ensure_fixtures(testcase):
    """Build the guarded evidence fixtures once if they are not on disk yet."""
    try:
        if not os.path.isfile(os.path.join(GART, "provenance.json")):
            d12.redesign_artifact(out_dir=GART, expect_g_pure_sha=KNOWN_G)
        if not os.path.isfile(os.path.join(GPLAN, "plan-provenance.json")):
            d12.stamp_plan(out_dir=GPLAN, artifact_dir=GART)
        if not os.path.isfile(os.path.join(SYN, "provenance.json")):
            d12.make_synthetic_fixture(out_dir=SYN)
    except (FileNotFoundError, pv.ProvenanceRefusal) as exc:
        raise unittest.SkipTest("12k guarded fixtures unavailable: %r" % (exc,))


def _copy(src, dst):
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst)
    return dst


def _load(path):
    with open(path, "r", encoding="utf-8") as fh:
        return json.load(fh)


def _save(path, obj):
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(obj, fh, ensure_ascii=False, indent=1)
        fh.write("\n")


def _flip(path, index=777):
    with open(path, "r+b") as fh:
        fh.seek(index * 4)
        raw = bytearray(fh.read(4))
        raw[0] ^= 0x01
        fh.seek(index * 4)
        fh.write(bytes(raw))


class Test12kGuardPass(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        _ensure_fixtures(cls)

    def test_measured_h_artifact_passes(self):
        v = pv.g12_artifact_guard(GART)
        self.assertTrue(v["ok"], v["codes"])
        self.assertEqual(v["metrics"]["g_pure_sha256"], KNOWN_G)
        self.assertTrue(v["metrics"]["h_source_check"]["ok"])
        self.assertGreater(v["metrics"]["offdiag_peak"], pv.A12_G_MATCH_ATOL)

    def test_redesign_reproduces_delivered_artifact(self):
        if not (os.path.isfile(REAL_H) and os.path.isfile(d12.DELIVERED_DESIGN_SCRIPT)):
            self.skipTest("delivered design script/H not available")
        with tempdir() as td:
            r = d12.redesign_artifact(out_dir=os.path.join(td, "art"),
                                      expect_g_pure_sha=KNOWN_G,
                                      expect_g_fir_sha=d12.KNOWN_12K_G_FIR_SHA)
            self.assertEqual(r["g_pure_sha256"], KNOWN_G)
            self.assertTrue(r["guard"]["ok"], r["guard"]["codes"])
            self.assertTrue(r["parity"]["matched"])

    def test_measured_h_source_check_passes(self):
        v = pv.h12_source_check(REAL_H)
        self.assertTrue(v["ok"], v["codes"])
        for mic in ("UMIK", "MA1", "ARC"):
            self.assertIn(mic, v["metrics"]["mic_names"])

    def test_plan_passes_and_binds_artifact(self):
        v = pv.g12_plan_guard(GPLAN, artifact_dir=GART)
        self.assertTrue(v["ok"], v["codes"])
        self.assertTrue(v["metrics"]["artifact_guard"]["ok"])
        self.assertTrue(v["metrics"]["provenance_present"])

    def test_lite_gate_passes(self):
        r = subprocess.run([sys.executable, LITE, "check", "--artifact", GART,
                            "--plan-dir", GPLAN, "--expect-g-pure-sha", KNOWN_G],
                           capture_output=True, text=True, encoding="utf-8", errors="replace")
        self.assertEqual(r.returncode, 0, r.stdout[-1200:] + r.stderr[-800:])

    def test_cli_check_artifact_rc(self):
        with contextlib.redirect_stdout(io.StringIO()):
            rc = pv.main(["check-12k-artifact", "--artifact", GART])
        self.assertEqual(rc, 0)


class Test12kSyntheticRefused(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        _ensure_fixtures(cls)

    def test_synthetic_12k_fixture_refused(self):
        v = pv.g12_artifact_guard(SYN)
        self.assertFalse(v["ok"])
        for code in ("a12_synthetic_marker", "a12_dryrun_source", "a12_synthetic_h_sha",
                     "a12_h_guard_not_pass", "a12_synthetic_content"):
            self.assertIn(code, v["codes"])

    def test_synthetic_content_caught_after_marker_strip(self):
        with tempdir() as td:
            d = _copy(SYN, os.path.join(td, "stripped"))
            prov = _load(os.path.join(d, "provenance.json"))
            prov["synthetic"] = False
            prov["live"] = True
            prov["claim"] = "measured arm reprocessed for offline wiring"
            prov["h"]["source"] = REAL_H
            _save(os.path.join(d, "provenance.json"), prov)
            v = pv.g12_artifact_guard(d)
            self.assertFalse(v["ok"])
            self.assertIn("a12_synthetic_content", v["codes"])

    def test_dryrun_h_source_refused(self):
        v = pv.h12_source_check(DRYRUN_H)
        self.assertFalse(v["ok"])
        self.assertIn("h12_ir_peak_spread", v["codes"])

    def test_lite_gate_refuses_synthetic(self):
        r = subprocess.run([sys.executable, LITE, "check", "--artifact", SYN],
                           capture_output=True, text=True, encoding="utf-8", errors="replace")
        self.assertEqual(r.returncode, 3, r.stdout[-1200:])


class Test12kMissingProvenanceRefused(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        _ensure_fixtures(cls)

    def test_delivered_task253_artifact_refused(self):
        v = pv.g12_artifact_guard(DELIVERED)
        self.assertFalse(v["ok"])
        self.assertIn("a12_missing_provenance", v["codes"])

    def test_missing_provenance_copy_refused(self):
        with tempdir() as td:
            d = _copy(GART, os.path.join(td, "noprov"))
            os.remove(os.path.join(d, "provenance.json"))
            v = pv.g12_artifact_guard(d)
            self.assertFalse(v["ok"])
            self.assertIn("a12_missing_provenance", v["codes"])

    def test_missing_required_fields_refused(self):
        with tempdir() as td:
            d = _copy(GART, os.path.join(td, "missing-fields"))
            params = _load(os.path.join(d, "params.json"))
            for key in ("g_sha256", "taps", "resamp_file"):
                params.pop(key, None)
            _save(os.path.join(d, "params.json"), params)
            v = pv.g12_artifact_guard(d)
            self.assertFalse(v["ok"])
            self.assertIn("a12_params_missing_fields", v["codes"])
            self.assertIn("a12_params_sha_mismatch", v["codes"])

    def test_missing_f64_content_copy_refused(self):
        with tempdir() as td:
            d = _copy(GART, os.path.join(td, "nof64"))
            os.remove(os.path.join(d, "g12_v2_4096.f64.npz"))
            v = pv.g12_artifact_guard(d)
            self.assertFalse(v["ok"])
            self.assertIn("a12_missing_g_f64", v["codes"])

    def test_plan_missing_provenance_refused(self):
        with tempdir() as td:
            d = _copy(GPLAN, os.path.join(td, "plan-noprov"))
            os.remove(os.path.join(d, "plan-provenance.json"))
            v = pv.g12_plan_guard(d, artifact_dir=GART)
            self.assertFalse(v["ok"])
            self.assertIn("a12_plan_missing_provenance", v["codes"])

    def test_plan_missing_compile_report_refused(self):
        with tempdir() as td:
            d = _copy(GPLAN, os.path.join(td, "plan-norep"))
            os.remove(os.path.join(d, "compile-report.json"))
            v = pv.g12_plan_guard(d, artifact_dir=GART)
            self.assertFalse(v["ok"])
            self.assertIn("a12_plan_missing_compile_report", v["codes"])


class Test12kContentMismatchRefused(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        _ensure_fixtures(cls)

    def test_g_f32_tamper_refused(self):
        with tempdir() as td:
            d = _copy(GART, os.path.join(td, "tamper-g"))
            _flip(os.path.join(d, "g12_v2_4096.f32"))
            v = pv.g12_artifact_guard(d)
            self.assertFalse(v["ok"])
            self.assertIn("a12_g_file_sha_mismatch", v["codes"])
            self.assertIn("a12_g_content_mismatch", v["codes"])

    def test_f64_copy_tamper_refused(self):
        with tempdir() as td:
            d = _copy(GART, os.path.join(td, "tamper-f64"))
            with open(os.path.join(d, "g12_v2_4096.f64.npz"), "ab") as fh:
                fh.write(b"\x00")
            v = pv.g12_artifact_guard(d)
            self.assertFalse(v["ok"])
            self.assertIn("a12_g_f64_sha_mismatch", v["codes"])

    def test_params_tamper_refused(self):
        with tempdir() as td:
            d = _copy(GART, os.path.join(td, "tamper-params"))
            params = _load(os.path.join(d, "params.json"))
            params["notes"] = "tampered"
            _save(os.path.join(d, "params.json"), params)
            v = pv.g12_artifact_guard(d)
            self.assertFalse(v["ok"])
            self.assertIn("a12_params_sha_mismatch", v["codes"])

    def test_plan_bytes_tamper_refused(self):
        with tempdir() as td:
            d = _copy(GPLAN, os.path.join(td, "tamper-plan"))
            _flip(os.path.join(d, "plan.bin"), index=0)
            v = pv.g12_plan_guard(d, artifact_dir=GART)
            self.assertFalse(v["ok"])
            self.assertIn("a12_plan_sha_mismatch", v["codes"])

    def test_compile_report_tamper_refused(self):
        with tempdir() as td:
            d = _copy(GPLAN, os.path.join(td, "tamper-rep"))
            rep = _load(os.path.join(d, "compile-report.json"))
            rep["plan_sha256"] = "0" * 64
            _save(os.path.join(d, "compile-report.json"), rep)
            v = pv.g12_plan_guard(d, artifact_dir=GART)
            self.assertFalse(v["ok"])
            self.assertIn("a12_plan_sha_mismatch", v["codes"])
            self.assertIn("a12_plan_compile_report_mismatch", v["codes"])

    def test_plan_artifact_binding_mismatch_refused(self):
        with tempdir() as td:
            d = _copy(GPLAN, os.path.join(td, "tamper-bind"))
            prov = _load(os.path.join(d, "plan-provenance.json"))
            prov["mimo12k_artifact"]["g_pure_sha256"] = "0" * 64
            _save(os.path.join(d, "plan-provenance.json"), prov)
            v = pv.g12_plan_guard(d, artifact_dir=GART)
            self.assertFalse(v["ok"])
            self.assertIn("a12_plan_artifact_mismatch", v["codes"])


class Test12kHValidity(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not os.path.isfile(REAL_H):
            raise unittest.SkipTest("measured H not available")
        z = pv.load_npz(REAL_H)
        cls.z = {k: np.array(z[k]) for k in z}

    def _variant(self, td, mutate):
        z = {k: np.array(v) for k, v in self.z.items()}
        mutate(z)
        p = os.path.join(td, "h_variant.npz")
        np.savez(p, **z)
        return p

    def test_silent_mic_refused(self):
        def mutate(z):
            for key in ("ir_ARC",):
                z[key] = np.zeros_like(z[key])
        with tempdir() as td:
            v = pv.h12_source_check(self._variant(td, mutate))
        self.assertFalse(v["ok"])
        self.assertIn("h12_silent_mic", v["codes"])

    def test_ir_peak_out_of_window_refused(self):
        def mutate(z):
            for mic in ("UMIK", "MA1", "ARC"):
                ir = np.zeros_like(z["ir_" + mic])
                for slot in (0, 1, 2, 3, 7):
                    ir[slot, 5000] = 0.01  # 104 ms >> 60 ms
                z["ir_" + mic] = ir
        with tempdir() as td:
            v = pv.h12_source_check(self._variant(td, mutate))
        self.assertFalse(v["ok"])
        self.assertIn("h12_ir_peak_out_of_window", v["codes"])

    def test_dead_participant_row_refused(self):
        def mutate(z):
            for mic in ("UMIK", "MA1", "ARC"):
                H = np.array(z["H_" + mic])
                H[7, :] = 0.0
                z["H_" + mic] = H
        with tempdir() as td:
            v = pv.h12_source_check(self._variant(td, mutate))
        self.assertFalse(v["ok"])
        self.assertIn("h12_dead_participant_row", v["codes"])

    def test_unrecognised_h_format_refused(self):
        with tempdir() as td:
            p = os.path.join(td, "bad.npz")
            np.savez(p, something=np.zeros(4))
            v = pv.h12_source_check(p)
        self.assertFalse(v["ok"])
        self.assertIn("h12_bad_format", v["codes"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
