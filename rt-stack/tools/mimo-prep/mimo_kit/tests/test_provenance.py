#!/usr/bin/env python3
"""Unit tests for the MIMO design/plan provenance guard (task-252).

Run: python -m pytest tools/mimo-prep/mimo_kit/tests/test_provenance.py -q
 or: python tools/mimo-prep/mimo_kit/tests/test_provenance.py

Covers:
  * the incident: dry-run synthetic G design dir / planrt support-sweep G / the
    released fixture plan sha (ceb04215...) / dryrun t66 plan are all REFUSED
  * real re-measured identity-arm H passes the H guard and designs a real-room G
    that passes the G guard and packs a plan whose sha differs from the synthetic one
  * invalid H refused: MA1/ARC S24 byte-mapping artifact (-42.14 dBFS, SNR~0),
    silent/unavailable mic, IR peaks outside the direct-arrival window,
    dead participant row, dry-run H source
  * run_0830 live fallback: no real design source -> REFUSE (never prep/dryrun/out/design)
  * reference_design numeric parity with the legacy pilot design (bit-exact)
"""
import copy
import json
import os
import sys
import tempfile
import unittest

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
KIT = os.path.dirname(HERE)
sys.path.insert(0, KIT)

import design_chain as dc  # noqa: E402
import provenance as pv  # noqa: E402
import reference_design as rd  # noqa: E402

PROJECT = os.path.abspath(os.path.join(KIT, "..", "..", "..", ".."))
EVIDENCE = os.path.join(PROJECT, "rt-stack", "evidence")
# NOTE: %TEMP% is on R: (ImDisk); spawned tools (mimo_t66_pack) cannot write there.
# Keep test scratch on C: inside the build tree.
TEST_TMP = os.path.join(PROJECT, "rt-stack", "target", "tmp-provenance-tests")
os.makedirs(TEST_TMP, exist_ok=True)

def tempdir():
    return tempfile.TemporaryDirectory(dir=TEST_TMP)
PILOT = os.path.join(EVIDENCE, "mimo-pilot-3mic")
DRYRUN_OUT = os.path.join(PILOT, "prep", "dryrun", "out")
DRYRUN_DESIGN = os.path.join(DRYRUN_OUT, "design")
PLANRT = os.path.join(PILOT, "planrt-20261119")
PLANRT_G4096 = os.path.join(PLANRT, "plans", "g-4096")
RELEASE_FIXTURE = os.path.join(EVIDENCE, "release-v10", "tools", "fixtures", "g-4096")
ARM = os.path.join(EVIDENCE, "mimo-kit-fix-20261119", "analysis", "remeasure", "arms", "identity")
ISSUED_PLAN_SHA = "ceb04215b3f17f37b93103ee1c4ec48cc173302d5642862ad9fe55c0f084a885"
DRYRUN_G_SHA = "60492af14e972008acf98b1d48cf52db8fffcf2229fa9a0ba44286bb2f3fc4c9"


def _dryrun_h_fixture():
    z = pv.load_npz(os.path.join(DRYRUN_OUT, "h", "h_estimate.npz"))
    return {"freq": z["freq"], "H_norm": z["H_norm"],
            "info": {"synthetic": True, "arm": os.path.join(DRYRUN_OUT, "session"),
                     "mic_names": [str(x) for x in z["mic_names"]]}}


def _small_h(info_overrides=None, ir_peak=100, ir_len=8000):
    """Tiny synthetic H bundle (real-ish H_norm) for fast invalid-H cases."""
    z = pv.load_npz(os.path.join(DRYRUN_OUT, "h", "h_estimate.npz"))
    rng = np.random.RandomState(0)
    Hn = (z["H_norm"] * 0 + rng.normal(size=z["H_norm"].shape)) + 1e-3
    irs = np.zeros((3, 7, ir_len))
    for mi in range(3):
        for oi in (0, 1, 2, 3, 6):
            irs[mi, oi, ir_peak] = 1.0
    stats = {"UMIK": {"peak_dbfs": -20.0, "snr_db": 30.0, "available": True},
             "MA1": {"peak_dbfs": -10.0, "snr_db": 30.0, "available": True},
             "ARC": {"peak_dbfs": -12.0, "snr_db": 28.0, "available": True}}
    info = {"schema": "tspi-mimo-h/test", "synthetic": False, "mic_names": ["UMIK", "MA1", "ARC"],
            "arm": ARM, "fs_hz": 48000, "mic_stats": stats}
    if info_overrides:
        info.update(info_overrides)
    return {"freq": z["freq"], "H_norm": Hn, "irs": irs,
            "mic_names": ["UMIK", "MA1", "ARC"], "info": info}


class TestSyntheticRegistry(unittest.TestCase):
    def test_issued_plan_and_dryrun_design_are_registered(self):
        reg = pv.synthetic_registry()
        plans = set(reg["synthetic_plan_sha256"].values())
        self.assertIn(ISSUED_PLAN_SHA, plans)          # planrt g-4096 AND release fixture
        self.assertIn(DRYRUN_G_SHA, set(reg["synthetic_design_sha256"].values()))
        self.assertIsNotNone(reg["synthetic_G_ideal_sha256"])


class TestSyntheticGRejected(unittest.TestCase):
    def test_dryrun_design_dir_rejected(self):
        v = pv.g_guard(DRYRUN_DESIGN, live=True)
        self.assertFalse(v["ok"])
        for code in ("g_synthetic_file_sha", "g_synthetic_content", "g_synthetic_marker",
                     "g_missing_provenance"):
            self.assertIn(code, v["codes"])

    def test_planrt_fir_contents_rejected(self):
        z = pv.load_npz(os.path.join(PLANRT_G4096, "fir.npz"))
        codes, metrics = pv.g_content_checks(dict(z))
        self.assertIn("g_synthetic_content", codes)
        self.assertLessEqual(metrics["G_fir_max_abs_vs_dryrun_realized"], 1e-12)

    def test_issued_and_release_plans_rejected(self):
        for d in (PLANRT_G4096, RELEASE_FIXTURE):
            plan = os.path.join(d, "plan.bin")
            v = pv.g_guard_plan(plan)
            self.assertFalse(v["ok"], d)
            self.assertIn("g_plan_synthetic_sha", v["codes"])
            self.assertEqual(v["metrics"]["plan_sha256"], ISSUED_PLAN_SHA)

    def test_dryrun_t66_plan_rejected(self):
        v = pv.g_guard_plan(os.path.join(DRYRUN_OUT, "t66", "plan.bin"))
        self.assertFalse(v["ok"])
        self.assertIn("g_plan_synthetic_sha", v["codes"])


class TestWindowDesignPretestHRejected(unittest.TestCase):
    """The actual 08:30 design H must be refused by the new H guard.

    rt-stack/evidence/mimo-pilot-3mic/run-20261119/_tools/design-pretest/h_estimate.npz
    is the H that drove the window's "real" design attempt.  Its MA1/ARC IRs are the
    -42.14 dBFS S24 byte-mapping artifact, so peaks sit anywhere in the 11.5 s IR window.
    """

    def test_window_design_pretest_h_rejected(self):
        p = os.path.join(PILOT, "run-20261119", "_tools", "design-pretest", "h_estimate.npz")
        if not os.path.isfile(p):
            self.skipTest("window design-pretest h_estimate.npz not present")
        z = pv.load_npz(p)
        info = pv.load_json(os.path.join(os.path.dirname(p), "h_estimate.json"), {}) or {}
        info.setdefault("mic_names", [str(x) for x in z.get("mic_names", [])])
        h = {"freq": z["freq"], "H_norm": z["H_norm"], "irs": z["irs"],
             "mic_names": info["mic_names"], "info": info}
        v = pv.h_guard(h)
        self.assertFalse(v["ok"])
        self.assertIn("h_ir_peak_out_of_window", v["codes"])


class TestRealHAndDesign(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.h = dc.build_h7_from_arm(ARM)

    def test_real_h_guard_passes(self):
        v = pv.h_guard(self.h)
        self.assertTrue(v["ok"], v["codes"])
        for mic in ("UMIK", "MA1", "ARC"):
            self.assertIn(mic, v["metrics"]["mic_stats"])
            self.assertGreater(v["metrics"]["mic_stats"][mic]["snr_db"], 6.0)

    def test_real_design_passes_and_plan_differs_from_synthetic(self):
        with tempdir() as td:
            h_dir = os.path.join(td, "design")
            npz = dc.save_h(self.h, h_dir)
            h = dc.load_h_npz(npz)
            d = dc.design_real(h, td)
            v = pv.g_guard(d["design_dir"], live=True)
            self.assertTrue(v["ok"], v["codes"])
            man = dc.run_live_design(h_npz=npz, out=os.path.join(td, "real"))
            self.assertNotEqual(man["plan"]["sha256"], ISSUED_PLAN_SHA)
            self.assertNotIn(man["design_g_sha256"],
                             set(pv.synthetic_registry()["synthetic_design_sha256"].values()))
            self.assertTrue(man["synthetic_differs"])
            gv = pv.g_guard_plan(man["plan"]["path"], design=man["design_dir"])
            self.assertTrue(gv["ok"], gv["codes"])


class TestInvalidHRejected(unittest.TestCase):
    def _codes(self, h):
        v = pv.h_guard(h)
        self.assertFalse(v["ok"])
        return v["codes"]

    def test_ma1_arc_byte_mapping_artifact_rejected(self):
        h = _small_h({"mic_stats": dict(_small_h()["info"]["mic_stats"],
                                        MA1={"peak_dbfs": -42.144, "snr_db": 0.18, "available": True},
                                        ARC={"peak_dbfs": -42.247, "snr_db": 0.53, "available": True})})
        codes = self._codes(h)
        self.assertIn("h_mic_byte_artifact", codes)

    def test_silent_mic_rejected(self):
        h = _small_h({"mic_stats": dict(_small_h()["info"]["mic_stats"],
                                        ARC={"peak_dbfs": -116.5, "snr_db": -0.11, "available": False})})
        self.assertIn("h_mic_unavailable", self._codes(h))

    def test_ir_peak_outside_direct_window_rejected(self):
        h = _small_h()
        h["irs"] = h["irs"].copy()
        for mi in range(3):
            for oi in (0, 1, 2, 3, 6):
                h["irs"][mi, oi, :] = 0.0
                h["irs"][mi, oi, 5000] = 1.0        # 5000 smp = 104 ms >> 60 ms window
        self.assertIn("h_ir_peak_out_of_window", self._codes(h))

    def test_ir_peak_spread_across_roles_rejected(self):
        h = _small_h()
        h["irs"] = h["irs"].copy()
        for mi, pos in enumerate((100, 1200, 2400)):
            for oi in (0, 1, 2, 3, 6):
                h["irs"][mi, oi, :] = 0.0
                h["irs"][mi, oi, pos] = 1.0
        codes = self._codes(h)
        self.assertTrue("h_ir_peak_spread" in codes or "h_ir_peak_out_of_window" in codes)

    def test_dead_participant_row_rejected(self):
        h = _small_h()
        h["H_norm"] = h["H_norm"].copy()
        h["H_norm"][:, rd.participant_index("SUB1"), :] = 0.0
        self.assertIn("h_dead_participant_row", self._codes(h))

    def test_dryrun_h_source_rejected(self):
        h = _small_h({"synthetic": True, "arm": os.path.join(DRYRUN_OUT, "session")})
        codes = self._codes(h)
        self.assertIn("h_synthetic_source", codes)

    def test_design_real_refuses_invalid_h(self):
        h = _small_h({"mic_stats": {"UMIK": {"peak_dbfs": -20.0, "snr_db": 30.0, "available": True},
                                    "MA1": {"peak_dbfs": -42.144, "snr_db": 0.18, "available": True},
                                    "ARC": {"peak_dbfs": -12.0, "snr_db": 28.0, "available": True}}})
        with tempdir() as td:
            with self.assertRaises(pv.ProvenanceRefusal) as ctx:
                dc.design_real(h, td)
            self.assertIn("h_mic_byte_artifact", ctx.exception.codes)


class TestLiveFallbackRefused(unittest.TestCase):
    def test_live_without_design_source_refused(self):
        with self.assertRaises(pv.ProvenanceRefusal) as ctx:
            pv.resolve_design_source({}, live=True, dryrun_design_dir=DRYRUN_DESIGN)
        self.assertIn("live_no_design_source", ctx.exception.codes)

    def test_live_with_dryrun_design_source_refused(self):
        with self.assertRaises(pv.ProvenanceRefusal) as ctx:
            pv.resolve_design_source({"design": {"source": DRYRUN_DESIGN}},
                                     live=True, dryrun_design_dir=DRYRUN_DESIGN)
        self.assertIn("g_h_dryrun_source", ctx.exception.codes)

    def test_dry_run_may_use_dryrun_design(self):
        got = pv.resolve_design_source({}, live=False, dryrun_design_dir=DRYRUN_DESIGN)
        self.assertEqual(os.path.abspath(got), os.path.abspath(DRYRUN_DESIGN))


class TestReferenceDesignParity(unittest.TestCase):
    def test_reference_design_parity_with_dryrun(self):
        ref = pv.load_npz(os.path.join(DRYRUN_DESIGN, "g_design.npz"))
        h = _dryrun_h_fixture()
        with tempdir() as td:
            res = rd.design_reference(h, td, taps=8192, delay=4096)
            for key in ("G_ideal", "G_fir"):
                self.assertTrue(np.array_equal(res[key], ref[key]), key)
            rep = json.load(open(os.path.join(td, "design-report.json"), encoding="utf-8"))
            refrep = json.load(open(os.path.join(DRYRUN_DESIGN, "design-report.json"), encoding="utf-8"))
            self.assertEqual(rep["per_dac_gain_ok"], refrep["per_dac_gain_ok"])
            self.assertEqual(rep["max_path_gain_ok"], refrep["max_path_gain_ok"])
            self.assertEqual(rep["per_dac_scaled_by"], refrep["per_dac_scaled_by"])


class TestPlanGuardTamper(unittest.TestCase):
    def test_plan_sha_mismatch_detected(self):
        with tempdir() as td:
            plan = os.path.join(td, "plan.bin")
            with open(os.path.join(PLANRT_G4096, "plan.bin"), "rb") as fh:
                data = bytearray(fh.read())
            data[-1] ^= 0xFF
            with open(plan, "wb") as fh:
                fh.write(bytes(data))
            rep = pv.load_json(os.path.join(PLANRT_G4096, "compile-report.json"), {})
            with open(os.path.join(td, "compile-report.json"), "w", encoding="utf-8") as fh:
                json.dump(rep, fh)
            v = pv.g_guard_plan(plan, os.path.join(td, "compile-report.json"))
            self.assertFalse(v["ok"])
            self.assertIn("g_plan_sha_mismatch", v["codes"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
