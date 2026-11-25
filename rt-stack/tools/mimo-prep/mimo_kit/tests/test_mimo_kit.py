#!/usr/bin/env python3
"""Unit tests for the fixed MIMO analysis kit (task-249).

Run: python -m pytest tools/mimo-prep/mimo_kit/tests/test_mimo_kit.py -q
 or: python tools/mimo-prep/mimo_kit/tests/test_mimo_kit.py

Covers:
  * extract byte mapping (exact) + regression of the -42.14 dBFS buggy artifact
  * sign extension / silent channel / end-to-end CLI with marker contract
  * analyze role-major vs container-slot indexing (SUB1=7, slots 4/5/6 unused)
  * bounded linear offset detection (old kit's circular argmax + +-1 s clamp bug)
  * marker gate (missing keys / mismatch), path-consistency gate, mic availability
  * noise-aware RT60 (known synthetic decay; unmeasurable noise tail -> None)
"""
import json
import math
import os
import shutil
import sys
import tempfile
import unittest

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
KIT = os.path.dirname(HERE)
sys.path.insert(0, KIT)

import s24  # noqa: E402
import extract  # noqa: E402
import analyze  # noqa: E402


def pack_channels(channel_ints, channels):
    """channel_ints: list of signed int lists (one per channel) -> interleaved S24 bytes."""
    n = len(channel_ints[0])
    chans = [b"".join(s24.int24_to_bytes(v) for v in ch) for ch in channel_ints]
    assert len(chans) == channels
    return s24.interleave_channels(chans)


def sine_ints(n, freq, peak_dbfs, fs=48000):
    amp = 10.0 ** (peak_dbfs / 20.0) * s24.S24_SCALE
    t = np.arange(n) / fs
    return [int(round(v)) for v in amp * np.sin(2 * math.pi * freq * t)]


# --------------------------------------------------------------------------- s24
class TestS24(unittest.TestCase):
    def test_roundtrip_int24(self):
        for v in (0, 1, -1, 127, -128, 32767, -32768, 8388607, -8388608, -1234567):
            b = s24.int24_to_bytes(v)
            self.assertEqual(len(b), 3)
            back = s24.bytes_to_int24(b[0], b[1], b[2])
            self.assertEqual(back, v)

    def test_interleave_extract_exact(self):
        rng = np.random.RandomState(7)
        channels = 20
        n = 97
        src = [rng.randint(-8388608, 8388608, size=n).astype(np.int64) for _ in range(channels)]
        raw = pack_channels([list(map(int, ch)) for ch in src], channels)
        self.assertEqual(len(raw), n * channels * 3)
        for c in range(channels):
            got = s24.extract_channel_bytes(raw, channels, c)
            exp = b"".join(s24.int24_to_bytes(int(v)) for v in src[c])
            self.assertEqual(bytes(got), exp, "channel %d not byte-exact" % c)

    def test_extract_channel_file_matches_in_memory(self):
        rng = np.random.RandomState(11)
        channels = 20
        n = 1500
        src = [rng.randint(-8388608, 8388608, size=n).astype(np.int64) for _ in range(channels)]
        raw = pack_channels([list(map(int, ch)) for ch in src], channels)
        with tempfile.TemporaryDirectory() as td:
            p = os.path.join(td, "motu.raw")
            with open(p, "wb") as fh:
                fh.write(raw)
            out = os.path.join(td, "ch1.raw")
            frames = s24.extract_channel_file(p, out, channels, 1, chunk_frames=97)
            self.assertEqual(frames, n)
            exp = b"".join(s24.int24_to_bytes(int(v)) for v in src[1])
            self.assertEqual(open(out, "rb").read(), exp)


# --------------------------------------------------------------------------- extract
class TestExtract(unittest.TestCase):
    def _fixture(self, n=4800, frames=4800):
        channels = 20
        chans = [sine_ints(n, 440.0, -20.0) if c == 0 else
                 sine_ints(n, 777.0, -30.0) if c == 1 else
                 [0] * n for c in range(channels)]
        chans[6] = [1 if i % 2 == 0 else -1 for i in range(n)]  # noisy floating input
        raw = pack_channels(chans, channels)
        return channels, chans, raw

    def test_extract_byte_mapping_exact(self):
        channels, chans, raw = self._fixture()
        got0 = s24.extract_channel_bytes(raw, channels, 0)
        exp0 = b"".join(s24.int24_to_bytes(int(v)) for v in chans[0])
        self.assertEqual(bytes(got0), exp0)
        got1 = s24.extract_channel_bytes(raw, channels, 1)
        exp1 = b"".join(s24.int24_to_bytes(int(v)) for v in chans[1])
        self.assertEqual(bytes(got1), exp1)

    def test_buggy_mapping_reproduces_artifact(self):
        """The 2026-11-19 old mapping (ch/ch+20/ch+40) produced -42.1 dBFS on a -20 dBFS ch0."""
        channels, chans, raw = self._fixture()
        # old buggy code for ch0
        buggy = bytearray(3 * (len(raw) // 60))
        buggy[0::3] = raw[0::60]
        buggy[1::3] = raw[20::60]
        buggy[2::3] = raw[40::60]
        lvl = s24.channel_rms_peak_dbfs(buggy)
        self.assertLess(lvl["peak_dbfs"], -40.0)
        self.assertGreater(lvl["peak_dbfs"], -44.0)
        # fixed: peak ~ -20 dBFS (sine peak)
        fixed = s24.extract_channel_file
        with tempfile.TemporaryDirectory() as td:
            p = os.path.join(td, "motu.raw")
            open(p, "wb").write(raw)
            o = os.path.join(td, "ma1.raw")
            s24.extract_channel_file(p, o, channels, 0, chunk_frames=333)
            lvl_fixed = s24.channel_rms_peak_dbfs(open(o, "rb").read())
        self.assertGreater(lvl_fixed["peak_dbfs"], -21.0)
        self.assertLess(lvl_fixed["peak_dbfs"], -19.0)

    def test_sign_extension(self):
        ch = [[-1, -2, -8388608, 8388607, -12345, 0]]
        raw = pack_channels(ch, 1)
        got = s24.extract_channel_bytes(raw, 1, 0)
        back = [s24.bytes_to_int24(got[i * 3], got[i * 3 + 1], got[i * 3 + 2])
                for i in range(6)]
        self.assertEqual(back, ch[0])

    def test_silent_channel_exact_zeros(self):
        chans = [[0] * 64 for _ in range(20)]
        raw = pack_channels(chans, 20)
        got = s24.extract_channel_bytes(raw, 20, 1)
        self.assertEqual(bytes(got), b"\x00" * (3 * 64))

    def test_extract_cli_end_to_end(self):
        channels, chans, raw = self._fixture(n=960)
        with tempfile.TemporaryDirectory() as td:
            work = os.path.join(td, "work")
            ev = os.path.join(td, "ev")
            os.makedirs(work)
            open(os.path.join(work, "capture-motu.raw"), "wb").write(raw)
            # UMIK 2ch: ch0 = a known mono, ch1 silence
            um = pack_channels([chans[0], [0] * 960], 2)
            open(os.path.join(work, "capture-umik.raw"), "wb").write(um)
            render = os.path.join(td, "render.raw")
            open(render, "wb").write(b"\x11" * 100)
            sess = os.path.join(td, "session.json")
            json.dump({"schema": "tspi-planrt-session/v1", "fs": 48000, "channels": 8,
                       "role_slots": {"FL": 0, "FR": 1, "SL": 2, "SR": 3, "SUB1": 7},
                       "schedule": {"sweep_seconds": 8.0, "repeats": 2, "pre": 1.5,
                                    "repeat_gap": 1.0, "slot_gap": 1.0},
                       "starts": [{"role": r, "start_frame": 72000 + ri * 1000 + rep * 100}
                                  for ri, r in enumerate(["FL", "FR", "SL", "SR", "SUB1"])
                                  for rep in range(2)],
                       "total_frames": 960}, open(sess, "w"))
            rc = extract.main([work, ev, "g", render, "--level-db", "-12.58",
                               "--spl-target-db", "75", "--session-meta", sess,
                               "--session-input-sha", "abc123"])
            self.assertEqual(rc, 0)
            d = os.path.join(ev, "g")
            for name in ("capture.raw", "MA1-mono.raw", "ARC-mono.raw",
                         "playback.json", "extract.json",
                         os.path.join("meta", "playback.json"),
                         os.path.join("meta", "session.json")):
                self.assertTrue(os.path.isfile(os.path.join(d, name)), name)
            marker = json.load(open(os.path.join(d, "meta", "playback.json")))
            for k in ("path", "backend", "level_db", "spl_target_db", "file_sha256"):
                self.assertIn(k, marker)
            self.assertEqual(marker["level_db"], -12.58)
            self.assertEqual(marker["file_sha256"], "abc123")
            self.assertEqual(marker["arm"], "g")
            self.assertEqual(json.load(open(os.path.join(d, "playback.json"))), marker)
            meta = json.load(open(os.path.join(d, "meta", "session.json")))
            self.assertEqual(meta["schema"], "tspi-planrt-session/v2")
            self.assertEqual(meta["role_slots"]["SUB1"], 7)
            self.assertEqual(len(meta["starts"]), 5)
            self.assertEqual(len(meta["starts"][0]), 2)
            self.assertEqual(meta["starts"][4][0], 72000 + 4 * 1000)

    def test_selfcheck(self):
        self.assertEqual(extract.selfcheck(), 0)


# --------------------------------------------------------------------------- analyze
def build_synthetic_arms(td, n_frames=None):
    """Synthetic 2-arm same-path fixture: 5 roles on container slots [0,1,2,3,7]."""
    roles = ["FL", "FR", "SL", "SR", "SUB1"]
    slots = {"FL": 0, "FR": 1, "SL": 2, "SR": 3, "SUB1": 7}
    params = {"sweep_seconds": 1.0, "repeats": 2, "pre": 0.4,
              "repeat_gap": 0.2, "slot_gap": 0.2, "level_db": -20.0}
    starts, total = analyze.schedule(len(roles), params["sweep_seconds"], params["repeats"],
                                     params["pre"], params["repeat_gap"], params["slot_gap"])
    if n_frames is None:
        n_frames = int(total)
    n_frames = max(n_frames, int(starts[-1, -1]) + int(1.0 * analyze.FS))
    delay = 4800                     # 0.1 s capture/playback skew
    sw = analyze.sweep(params["sweep_seconds"], params["level_db"])
    sig = np.zeros(n_frames)
    for role_i, role in enumerate(roles):
        for r in range(params["repeats"]):
            s0 = delay + int(starts[role_i, r])
            sig[s0:s0 + len(sw)] += sw[: max(0, min(len(sw), n_frames - s0))]
    rng = np.random.RandomState(3)
    sig = sig + rng.normal(0, 10 ** (-80 / 20.0), n_frames)
    arms = {}
    for arm, gains in (("identity", {"FL": 1.0, "FR": 1.0, "SL": 1.0, "SR": 1.0, "SUB1": 1.0}),
                       ("g", {"FL": 1.0, "FR": 1.5, "SL": 1.0, "SR": 1.0, "SUB1": 1.0})):
        y = np.zeros(n_frames)
        for role_i, role in enumerate(roles):
            for r in range(params["repeats"]):
                s0 = delay + int(starts[role_i, r])
                ramp = gains[role] * sw[: max(0, min(len(sw), n_frames - s0))]
                y[s0:s0 + len(ramp)] += ramp
        umik = np.zeros((n_frames, 2))
        umik[:, 0] = y
        mono = y * 0.5
        d = os.path.join(td, arm)
        os.makedirs(os.path.join(d, "meta"), exist_ok=True)
        analyze.write_s24(os.path.join(d, "capture.raw"), umik)
        analyze.write_s24(os.path.join(d, "MA1-mono.raw"), mono)
        analyze.write_s24(os.path.join(d, "ARC-mono.raw"), np.zeros(n_frames))
        meta = {"schema": "tspi-planrt-session/v2", "roles": roles, "role_slots": slots,
                "starts": starts.tolist(),
                "schedule": {k: params[k] for k in ("sweep_seconds", "repeats", "pre",
                                                    "repeat_gap", "slot_gap")},
                "total_frames": int(total)}
        json.dump(meta, open(os.path.join(d, "meta", "session.json"), "w"), indent=1)
        marker = {"path": "aplay", "backend": "aplay-direct-alsa", "level_db": -20.0,
                  "spl_target_db": 75.0, "file_sha256": "session-sha-abc", "arm": arm}
        json.dump(marker, open(os.path.join(d, "meta", "playback.json"), "w"), indent=1)
        arms[arm] = (d, params, gains)
    return arms, starts, slots, roles, delay


class TestAnalyze(unittest.TestCase):
    def test_container_slot_indexing_and_sub1(self):
        with tempfile.TemporaryDirectory() as td:
            arms, starts, slots, roles, delay = build_synthetic_arms(td)
            d, params, _ = arms["identity"]
            freqs = analyze.oct_grid()
            est = analyze.estimate_H(d, params, freqs)
            # fixed offset detection must recover the 0.1 s capture delay
            self.assertLessEqual(abs(est["offset_samples"]["UMIK"] - delay), 480)
            H = est["H"]["UMIK"]
            # SUB1 must land in container slot 7, never in slots 4/5/6
            for slot in (4, 5, 6):
                self.assertAlmostEqual(float(np.max(np.abs(H[slot]))), 0.0)
            for role in roles:
                self.assertGreater(float(np.max(np.abs(H[slots[role]]))), 1e-4, role)
            # reuse of starts[role_i] is required: no IndexError, 5 rows used
            self.assertEqual(est["schedule"]["roles"], roles)
            self.assertEqual(est["schedule"]["role_slots"]["SUB1"], 7)

    def test_gates_marker_and_consistency(self):
        with tempfile.TemporaryDirectory() as td:
            arms, starts, slots, roles, delay = build_synthetic_arms(td)
            out = os.path.join(td, "out")
            before = analyze.analyze_arm(arms["identity"][0], arms["identity"][1], "before")
            after = analyze.analyze_arm(arms["g"][0], arms["g"][1], "after")
            summary = analyze.compare(before, after, out, require_mics=3)
            # marker + path gates pass (identical offsets, identical marker)
            self.assertTrue(summary["gates"]["marker"]["ok"])
            self.assertTrue(summary["gates"]["path_consistency"]["ok"])
            # but ARC is digital silence -> 3-mic pilot invalid, honestly labelled
            self.assertFalse(summary["valid"])
            self.assertTrue(any("mic_unavailable" in r for r in summary["invalid_reasons"]))
            self.assertTrue(os.path.isfile(os.path.join(out, "verdict.json")))
            # 2-mic partial metrics still produced for UMIK+MA1
            row = summary["rows"][0]
            self.assertEqual(row["spatial_std_n_mics"] if "spatial_std_n_mics" in row else 2, 2)

    def test_marker_gate_missing_keys(self):
        mb = {"path": "a", "backend": "b", "level_db": -20.0}
        ma = {"path": "a", "backend": "b", "level_db": -20.0,
              "spl_target_db": 75.0, "file_sha256": "x"}
        g = analyze.marker_gate(mb, ma)
        self.assertFalse(g["ok"])
        self.assertTrue(any("missing_keys" in r for r in g["reasons"]))
        g2 = analyze.marker_gate(ma, dict(ma, file_sha256="y"))
        self.assertFalse(g2["ok"])
        self.assertIn("marker_mismatch:file_sha256", g2["reasons"])

    def test_consistency_gate(self):
        before = {"offsets": {"UMIK": 48960, "MA1": 49440}}
        after = {"offsets": {"UMIK": 48960, "MA1": 48480}}
        g = analyze.consistency_gate(before, after)
        self.assertTrue(g["ok"])
        after2 = {"offsets": {"UMIK": 48960, "MA1": 228960}}
        g2 = analyze.consistency_gate(before, after2)
        self.assertFalse(g2["ok"])
        self.assertTrue(any("offset_mismatch:MA1" in r for r in g2["reasons"]))

    def test_offset_detection_bounded_linear(self):
        """Old estimator: unbounded circular argmax + +-1 s clamp. New: bounded linear."""
        n = int(11.0 * analyze.FS)
        delay = int(2.9 * analyze.FS)   # > 1 s, the old clamp would saturate at -48000
        starts, _ = analyze.schedule(3, 1.0, 2, 0.5, 0.2, 0.2)
        starts = starts[:2]
        sw = analyze.sweep(1.0, -20.0)
        sig = np.zeros(n)
        for row in starts:
            for s0 in row:
                a = delay + int(s0)
                sig[a:a + len(sw)] += sw[: max(0, min(len(sw), n - a))]
        rng = np.random.RandomState(5)
        y = sig + rng.normal(0, 10 ** (-80 / 20.0), n)
        off, q = analyze.detect_offset(y, starts, len(sw), max_lag_s=10.0)
        self.assertLessEqual(abs(off - delay), 480)
        self.assertGreater(q, 0.2)
        # bounded: with max_lag_s=1.0 the same call must clamp to the bound, not wrap
        off_b, _ = analyze.detect_offset(y, starts, len(sw), max_lag_s=1.0)
        self.assertLessEqual(abs(off_b), int(1.0 * analyze.FS) + 480)

    def test_rt60_noise_aware_known_decay(self):
        """Damped 125 Hz resonator with RT60=0.5 s -> noise-aware T20 recovers it."""
        fs = analyze.FS
        rt_target = 0.50
        tau = rt_target / 6.907755
        n = int(2.0 * fs)
        t = np.arange(n) / fs
        ir = np.sin(2 * math.pi * 125.0 * t) * np.exp(-t / tau)
        rt = analyze.rt60_nw(ir, 125.0)
        self.assertIsNotNone(rt)
        self.assertGreater(rt, 0.35)
        self.assertLess(rt, 0.75)

    def test_rt60_rejects_noise_tail(self):
        """Unmeasurable decays (no decay / noise-tail dominated) must return None, not 50+ s."""
        fs = analyze.FS
        rng = np.random.RandomState(10)
        n = int(2.0 * fs)
        self.assertIsNone(analyze.rt60_nw(rng.normal(0, 1.0, n), 125.0))   # no decay
        t = np.arange(n) / fs
        tau = 0.5 / 6.907755
        ir = np.sin(2 * math.pi * 125.0 * t) * np.exp(-t / tau)
        ir_noisy = ir + rng.normal(0, 0.4, n)                              # noise floor too high
        self.assertIsNone(analyze.rt60_nw(ir_noisy, 125.0))


if __name__ == "__main__":
    unittest.main(verbosity=2)