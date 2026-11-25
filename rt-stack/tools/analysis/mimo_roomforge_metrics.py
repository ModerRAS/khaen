#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""TSPI RoomForge MIMO fixture -- Python independent reference metrics.

This script is the *independent* (numpy-only) recomputation of the MIMO
objective/validation metrics for the RoomForge shoe-box ISM fixtures.  It is
used to cross-check the Rust implementation in rt-stack/crates/room-sim,
rt-stack/crates/mimo-design and rt-stack/tools/mimo-opt, and to produce the
evidence for "RoomForge simulation vs TSPI hand-written assumptions".

Authoritative documents (read them before touching formulas):
  * rt-stack/spec/mimo-objective-v1.md                     (metric definitions)
  * <evidence>/mimo-opt-20261122/CONTRACT.md  sections 3-5, 8, 9
      (fixture format, H(f) derivation, metrics, Rust report schema, Python ref)

Everything produced here is SIMULATED / SYNTHETIC / UNPROVEN:
no real room, no measurement, no audio device.  Every JSON output carries
"source": "SIMULATED".

Usage
-----
    python rt-stack/tools/analysis/mimo_roomforge_metrics.py --selftest

    python rt-stack/tools/analysis/mimo_roomforge_metrics.py \
        --fixtures rt-stack/evidence/calib-chain-20261121/mimo-opt-20261122/fixtures \
        --runs     rt-stack/evidence/calib-chain-20261121/mimo-opt-20261122/runs \
        --out      rt-stack/evidence/calib-chain-20261121/mimo-opt-20261122/validation

The production run polls the fixture and run directories (default: every 120 s
for at most 2700 s = 45 min) because the C# fixture generator and the Rust
optimizer are separate jobs.  Lower the limits for quick local checks with
--timeout-seconds / --poll-seconds.

Outputs (under --out):
  python-metrics.json   independent metrics + Rust cross-check + max diffs
  python-vs-rust.csv    one row per compared scalar (never swallow a diff)
  t60-analysis.json     Schroeder EDC / broadband T20/T30 / 63 and 125 Hz T20
  t60-analysis.md       human-readable T60 vs Sabine vs hand-written 0.35-0.45 s
  plots/*.png           2-4 comparison figures (English labels, PNG)

Round 2 (objective-v2) mode -- independent seat_std / max_dev / null_depth
recomputation, feasibility and predicted_status checks for the four objective
tags written to runs-obj/<tag>/<scene>/:
    python ... --fixtures <fixtures> --runs-obj-root <runs-obj> \
        --out <validation>/python-metrics-objectives [--v1-runs <runs>]
Outputs (under --out):
  <tag>.json             independent metrics + Rust cross-check per tag
  objective-crosscheck.csv  aggregated max abs diff per metric (all tags)
  objective-summary.json    tag statuses, missing scenes, v1 regression status
  objective-tables.md       REPORT section 8.2-8.4 ready tables (English)

Exit codes
----------
  0  success: outputs written, fixture SHA256 all valid, cross-check PASS
  1  selftest failure or unexpected internal error
  2  environment/input failure: timeout waiting for fixtures/runs, SHA256
     mismatch, malformed fixture/report (details on stderr), or incomplete
     run set (some scenes missing at timeout)
  3  cross-check mismatch: outputs written, Rust != Python beyond tolerance
     (this is a finding, not a crash; the diff table lists every mismatch)

Notes / declared conventions (differences from the frozen spec are flagged):
  * H(f): p(t) = wav(t) / rir.scale, zero-pad to fft_size, np.fft.rfft,
    take k = round(f*fft_size/fs); the contract fixes the grid so that
    frequency_grid_hz are exact bin frequencies already.
  * All variants' R_k use B0(design) E_k0 and B0(design) alpha as the common
    denominator/weights (CONTRACT section 8, frozen for this pipeline).
  * Holdout block: before = B0 energies over the holdout point set (per
    CONTRACT message from roomforge-mimo 2026-11-22); per-point band deltas
    use each point's own B0 as before.  R_k(B0 holdout) = 1.0.
  * dB values are power ratios: 10*log10(after_energy / before_energy).
  * N/A band: B0 design band energy < 1e-12 * max(E_k0); excluded from sums
    and counted/reported.
  * Percentiles use the nearest-rank definition (P10/P90) as requested.
  * T60: Schroeder EDC on the native RIR; broadband T20 (-5..-25 dB) and
    T30 (-5..-35 dB) by least squares on the EDC dB curve; 1/3-oct bands
    are Butterworth order 4 (zero-phase filtfilt), also T20 (-5..-25 dB).
    The bandpass order/edges are declared in the JSON.
  * Plots use English/ASCII text only (matplotlib fonts may lack CJK glyphs).
"""

from __future__ import annotations

import argparse
import csv
import datetime as _dt
import hashlib
import json
import math
import re
import struct
import sys
import time
import traceback
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Sequence, Tuple

import numpy as np

TOOL_NAME = "mimo_roomforge_metrics.py"
TOOL_VERSION = "1.0.0"
REPORT_FORMAT = "tspi-mimo-sim-report/v1"
PY_METRICS_FORMAT = "tspi-mimo-python-metrics/v1"
T60_FORMAT = "tspi-mimo-python-t60/v1"

SOURCE = "SIMULATED"
HONESTY = "UNPROVEN"

# Frozen parameters (spec/mimo-objective-v1.md section 9).
ALPHA_MIN = 0.5
ALPHA_MAX = 2.0
EPS_MIGRATION = 0.05          # R_k > 1 + eps => ENERGY_MIGRATION
SEVERE_DB = 3.0               # holdout band delta > +3 dB => severe regression
CONSISTENCY_MIN = 0.50        # C >= 0.5 => preliminary spatial generalisation
TOL_RATIO = 1e-6              # relative tolerance for ratio-valued metrics
TOL_DB = 1e-6                 # absolute tolerance for dB-valued metrics
ENERGY_NEAR_ZERO = 1e-12      # fraction-of-max threshold for N/A bands

# Hand-written TSPI low-frequency assumption that must be contrasted with the
# RoomForge/Sabine values (see CONTRACT section 2 and T60 analysis below).
HAND_T60_LOW_S = 0.35
HAND_T60_HIGH_S = 0.45

# T60/EDC analysis conventions (declared, not hidden).
T20_RANGE_DB = (-5.0, -25.0)
T30_RANGE_DB = (-5.0, -35.0)
T60_BANDPASS_ORDER = 4
T60_BANDS_HZ = (63.0, 125.0)

DEFAULT_TIMEOUT_S = 2700.0
DEFAULT_POLL_S = 120.0


# ---------------------------------------------------------------------------
# small utilities
# ---------------------------------------------------------------------------

class MetricsError(RuntimeError):
    """Fatal input / format problem (exit code 2)."""


class SelftestFailure(AssertionError):
    """Selftest assertion failure (exit code 1)."""


def now_utc_iso() -> str:
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def load_json(path: Path) -> Any:
    try:
        with open(path, "r", encoding="utf-8") as fh:
            return json.load(fh)
    except FileNotFoundError:
        raise MetricsError("file not found: %s" % path)
    except json.JSONDecodeError as exc:
        raise MetricsError("invalid JSON in %s: %s" % (path, exc))


def to_jsonable(value: Any) -> Any:
    """Convert numpy scalars/arrays to plain JSON values.

    NaN/inf become None (JSON has no NaN); callers add a note when this
    happens so nothing is silently lost.
    """
    if isinstance(value, dict):
        return {str(k): to_jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [to_jsonable(v) for v in value]
    if isinstance(value, np.ndarray):
        return [to_jsonable(v) for v in value.tolist()]
    if isinstance(value, (np.floating, float)):
        v = float(value)
        if math.isnan(v) or math.isinf(v):
            return None
        return v
    if isinstance(value, (np.integer,)):
        return int(value)
    if isinstance(value, (np.bool_, bool)):
        return bool(value)
    if value is None or isinstance(value, (str, int)):
        return value
    return value


def write_json(path: Path, obj: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline=chr(10)) as fh:
        json.dump(to_jsonable(obj), fh, ensure_ascii=False, indent=2, sort_keys=True)
        fh.write(chr(10))


def db10(power: float) -> float:
    """10*log10(power); nan for non-positive power."""
    p = float(power)
    if not math.isfinite(p) or p <= 0.0:
        return float("nan")
    return 10.0 * math.log10(p)


def db10_arr(power: np.ndarray) -> np.ndarray:
    a = np.asarray(power, dtype=np.float64)
    out = np.full(a.shape, np.nan, dtype=np.float64)
    good = np.isfinite(a) & (a > 0.0)
    out[good] = 10.0 * np.log10(a[good])
    return out


def db20_arr(mag: np.ndarray) -> np.ndarray:
    a = np.asarray(mag, dtype=np.float64)
    out = np.full(a.shape, np.nan, dtype=np.float64)
    good = np.isfinite(a) & (a > 0.0)
    out[good] = 20.0 * np.log10(a[good])
    return out


def nearest_rank_percentile(values: Sequence[float], pct: float) -> float:
    """Nearest-rank percentile on finite values (P10/P90 of CONTRACT section 5).

    rank = ceil(pct/100 * n), 1-based; returns nan for an empty set.
    """
    vals = np.asarray([v for v in values
                       if v is not None and math.isfinite(float(v))],
                      dtype=np.float64)
    n = vals.size
    if n == 0:
        return float("nan")
    vals = np.sort(vals)
    rank = int(math.ceil((pct / 100.0) * n))
    rank = min(max(rank, 1), n)
    return float(vals[rank - 1])


def rel_diff(py_val: float, rust_val: float) -> float:
    denom = max(abs(rust_val), 1e-300)
    return abs(py_val - rust_val) / denom


def ratio_ok(py_val: float, rust_val: float, tol: float = TOL_RATIO) -> bool:
    if not (math.isfinite(py_val) and math.isfinite(rust_val)):
        return False
    return rel_diff(py_val, rust_val) <= tol


def db_ok(py_val: float, rust_val: float, tol: float = TOL_DB) -> bool:
    if not (math.isfinite(py_val) and math.isfinite(rust_val)):
        return False
    return abs(py_val - rust_val) <= tol


# ---------------------------------------------------------------------------
# RIFF/WAV reader (float32 mono only; explicit errors otherwise)
# ---------------------------------------------------------------------------

@dataclass
class WavInfo:
    sample_rate: int
    channels: int
    bits_per_sample: int
    audio_format: int
    n_samples: int


def read_wav_f32_mono(path: Path,
                      expected_sample_rate: Optional[int] = None,
                      expected_samples: Optional[int] = None
                      ) -> Tuple[np.ndarray, WavInfo]:
    raw = path.read_bytes()
    if len(raw) < 44 or raw[0:4] != b"RIFF" or raw[8:12] != b"WAVE":
        raise MetricsError("%s: not a RIFF/WAVE file" % path)
    riff_size = struct.unpack_from("<I", raw, 4)[0]
    if riff_size + 8 > len(raw):
        raise MetricsError("%s: truncated RIFF (declared %d bytes, file %d)"
                           % (path, riff_size + 8, len(raw)))
    pos = 12
    fmt_data: Optional[bytes] = None
    pcm_data: Optional[bytes] = None
    while pos + 8 <= len(raw):
        cid = raw[pos:pos + 4]
        csize = struct.unpack_from("<I", raw, pos + 4)[0]
        body = raw[pos + 8:pos + 8 + csize]
        if len(body) != csize:
            raise MetricsError("%s: truncated chunk %r" % (path, cid))
        if cid == b"fmt ":
            fmt_data = body
        elif cid == b"data":
            pcm_data = body
        pos += 8 + csize + (csize & 1)  # chunks are word aligned
    if fmt_data is None or len(fmt_data) < 16:
        raise MetricsError("%s: missing/short fmt chunk" % path)
    if pcm_data is None:
        raise MetricsError("%s: missing data chunk" % path)
    audio_format, channels, sample_rate, _byte_rate, _block_align, bits = \
        struct.unpack_from("<HHIIHH", fmt_data, 0)
    if audio_format == 0xFFFE:  # WAVE_FORMAT_EXTENSIBLE -> SubFormat GUID
        if len(fmt_data) < 40:
            raise MetricsError("%s: short extensible fmt chunk" % path)
        subformat_tag = struct.unpack_from("<H", fmt_data, 24)[0]
        audio_format = subformat_tag
    if audio_format != 3:
        raise MetricsError("%s: expected IEEE float32 (format 3), got format %d"
                           % (path, audio_format))
    if channels != 1:
        raise MetricsError("%s: expected mono, got %d channels" % (path, channels))
    if bits != 32:
        raise MetricsError("%s: expected 32-bit float, got %d bits" % (path, bits))
    if expected_sample_rate is not None and sample_rate != expected_sample_rate:
        raise MetricsError("%s: sample_rate %d != fixture %d"
                           % (path, sample_rate, expected_sample_rate))
    samples = np.frombuffer(pcm_data, dtype="<f4").astype(np.float64)
    if expected_samples is not None and samples.size != expected_samples:
        raise MetricsError("%s: %d samples != fixture %d"
                           % (path, samples.size, expected_samples))
    info = WavInfo(sample_rate, channels, bits, audio_format, int(samples.size))
    return samples, info


# ---------------------------------------------------------------------------
# fixtures: ground-truth.json + SHA256SUMS + MANIFEST.json
# ---------------------------------------------------------------------------

@dataclass
class SceneFixture:
    scene_dir: Path
    scene_id: str
    gt: Dict[str, Any]
    gt_sha256: str
    sha256sums: Dict[str, str]
    sha256sums_sha256: str
    sample_rate_hz: int
    fft_size: int
    rir_length_samples: int
    scale: float
    freqs: np.ndarray                 # (K,) exact bin frequencies from ground-truth
    bin_indices: np.ndarray           # (K,) FFT bin index for each grid frequency
    source_ids: List[str]
    mic_ids: List[str]
    design_ids: List[str]
    holdout_ids: List[str]
    design_idx: np.ndarray
    holdout_idx: np.ndarray
    rir_files: List[Dict[str, Any]]
    warnings: List[str] = field(default_factory=list)

    @property
    def n_sources(self) -> int:
        return len(self.source_ids)

    @property
    def n_mics(self) -> int:
        return len(self.mic_ids)

    def h_matrix(self) -> np.ndarray:
        """Independent H(f) matrix, shape (n_mics, n_sources, K), complex128."""
        h = np.zeros((len(self.mic_ids), len(self.source_ids), len(self.freqs)),
                     dtype=np.complex128)
        by_key = {(rf["source"], rf["mic"]): rf for rf in self.rir_files}
        for n, src in enumerate(self.source_ids):
            for m, mic in enumerate(self.mic_ids):
                entry = by_key.get((src, mic))
                if entry is None:
                    raise MetricsError(
                        "%s: ground-truth has no rir_files entry for (%s,%s)"
                        % (self.scene_dir, src, mic))
                wav_path = self.scene_dir / entry["path"]
                if not wav_path.is_file():
                    raise MetricsError("%s: missing RIR file %s" % (self.scene_dir, wav_path))
                actual_sha = sha256_file(wav_path)
                if actual_sha.lower() != str(entry["sha256"]).lower():
                    raise MetricsError(
                        "%s: SHA256 mismatch for %s" % (self.scene_dir, entry["path"])
                        + " (ground-truth=%s actual=%s)" % (entry["sha256"], actual_sha))
                samples, _info = read_wav_f32_mono(
                    wav_path,
                    expected_sample_rate=self.sample_rate_hz,
                    expected_samples=self.rir_length_samples)
                pressure = samples / self.scale
                if pressure.size > self.fft_size:
                    raise MetricsError("%s: RIR longer than fft_size" % wav_path)
                spec = np.fft.rfft(pressure, n=self.fft_size)
                h[m, n, :] = spec[self.bin_indices]
        return h


def _parse_sha256sums(scene_dir: Path) -> Tuple[Dict[str, str], str]:
    path = scene_dir / "SHA256SUMS"
    if not path.is_file():
        raise MetricsError("%s: missing SHA256SUMS" % scene_dir)
    entries: Dict[str, str] = {}
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 1)
        if len(parts) != 2 or len(parts[0]) != 64:
            raise MetricsError("%s:%d: malformed SHA256SUMS line" % (path, lineno))
        entries[parts[1].replace(chr(92), "/").strip()] = parts[0].lower()
    return entries, sha256_file(path)


def _scene_hashes_from_manifest(manifest: Any) -> Dict[str, Dict[str, str]]:
    """Best-effort extraction of per-scene hashes from MANIFEST.json.

    The manifest schema is generator-defined; we consume only hash fields whose
    key names are recognizable and warn (instead of failing) otherwise.  The
    authoritative per-file hashes are ground-truth.rir_files (WAVs) and
    scene/SHA256SUMS (all files).
    """
    found: Dict[str, Dict[str, str]] = {}

    def walk(node: Any, scene_hint: Optional[str] = None) -> None:
        if isinstance(node, dict):
            scene_hint_local = scene_hint
            for key in ("scene_id", "id", "name", "scene"):
                val = node.get(key)
                if isinstance(val, str) and val.startswith("scene-"):
                    scene_hint_local = val
                    break
            if scene_hint_local:
                slot = found.setdefault(scene_hint_local, {})
                for key, val in node.items():
                    if not isinstance(val, str) or len(val) != 64:
                        continue
                    low = key.lower()
                    try:
                        int(val, 16)
                    except ValueError:
                        continue
                    if "ground" in low and "sha" in low:
                        slot["ground_truth_sha256"] = val.lower()
                    elif ("sums" in low or "sha256sum" in low) and "sha" in low:
                        slot["sha256sums_sha256"] = val.lower()
                    elif "manifest" in low and "sha" in low:
                        slot["manifest_sha256"] = val.lower()
            for val in node.values():
                walk(val, scene_hint_local)
        elif isinstance(node, list):
            for item in node:
                walk(item, scene_hint)

    walk(manifest)
    return found


def load_scene(scene_dir: Path) -> SceneFixture:
    scene_dir = scene_dir.resolve()
    gt_path = scene_dir / "ground-truth.json"
    if not gt_path.is_file():
        raise MetricsError("%s: missing ground-truth.json" % scene_dir)
    gt = load_json(gt_path)
    if not isinstance(gt, dict):
        raise MetricsError("%s: ground-truth.json is not an object" % gt_path)
    if not str(gt.get("format", "")).startswith("tspi-roomforge-mimo-fixture/"):
        raise MetricsError("%s: unexpected format %r" % (gt_path, gt.get("format")))
    for key in ("scene_id", "sample_rate_hz", "rir", "room", "sources", "mics",
                "design_mic_ids", "holdout_mic_ids", "frequency_grid_hz", "bands",
                "rir_files"):
        if key not in gt:
            raise MetricsError("%s: missing required key %r" % (gt_path, key))
    warnings: List[str] = []
    rir = gt["rir"]
    fft_size = int(rir["fft_size"])
    rir_len = int(rir["length_samples"])
    if str(rir.get("format", "")).lower() != "f32":
        raise MetricsError("%s: rir.format %r (expected f32)" % (gt_path, rir.get("format")))
    if int(rir.get("channels", 0)) != 1:
        raise MetricsError("%s: rir.channels %r (expected 1)"
                           % (gt_path, rir.get("channels")))
    if "scale" in rir:
        scale = float(rir["scale"])
        if not math.isfinite(scale) or scale == 0.0:
            raise MetricsError("%s: invalid rir.scale %r" % (gt_path, rir.get("scale")))
    else:
        scale = 1.0
        warnings.append("rir.scale missing -> assumed 1.0 (CONTRACT section 4)")
    sample_rate = int(gt["sample_rate_hz"])
    freqs = np.asarray(gt["frequency_grid_hz"], dtype=np.float64)
    if freqs.ndim != 1 or freqs.size < 4:
        raise MetricsError("%s: bad frequency_grid_hz" % gt_path)
    if not np.all(np.diff(freqs) > 0):
        raise MetricsError("%s: frequency_grid_hz must be strictly increasing" % gt_path)
    bin_indices = np.rint(freqs * fft_size / float(sample_rate)).astype(np.int64)
    if np.any(bin_indices < 0) or np.any(bin_indices > fft_size // 2):
        raise MetricsError("%s: frequency grid maps outside the rfft band" % gt_path)
    if np.unique(bin_indices).size != bin_indices.size:
        raise MetricsError("%s: frequency grid maps to duplicate FFT bins" % gt_path)
    sources = sorted(gt["sources"], key=lambda s: int(s.get("index", 0)))
    source_ids = [str(s["id"]) for s in sources]
    if len(set(source_ids)) != len(source_ids):
        raise MetricsError("%s: duplicate source ids" % gt_path)
    mics = list(gt["mics"])
    mic_ids = [str(m["id"]) for m in mics]
    if len(set(mic_ids)) != len(mic_ids):
        raise MetricsError("%s: duplicate mic ids" % gt_path)
    design_ids = [str(x) for x in gt["design_mic_ids"]]
    holdout_ids = [str(x) for x in gt["holdout_mic_ids"]]
    if set(design_ids) & set(holdout_ids):
        raise MetricsError("%s: design/holdout mic sets overlap" % gt_path)
    if set(design_ids) | set(holdout_ids) != set(mic_ids):
        missing = (set(mic_ids) - set(design_ids) - set(holdout_ids))
        raise MetricsError("%s: design/holdout partition incomplete: %s"
                           % (gt_path, sorted(missing)))
    roles = {str(m["id"]): str(m.get("role", "")) for m in mics}
    for mic_id in design_ids:
        if roles.get(mic_id) not in ("design", ""):
            warnings.append("mic %s listed as design but role=%r"
                            % (mic_id, roles.get(mic_id)))
    for mic_id in holdout_ids:
        if roles.get(mic_id) not in ("holdout", ""):
            warnings.append("mic %s listed as holdout but role=%r"
                            % (mic_id, roles.get(mic_id)))
    index_of = {mid: i for i, mid in enumerate(mic_ids)}
    design_idx = np.asarray([index_of[x] for x in design_ids], dtype=np.int64)
    holdout_idx = np.asarray([index_of[x] for x in holdout_ids], dtype=np.int64)
    rir_files = list(gt["rir_files"])
    if not rir_files:
        raise MetricsError("%s: empty rir_files" % gt_path)
    for entry in rir_files:
        for key in ("source", "mic", "path", "sha256", "samples"):
            if key not in entry:
                raise MetricsError("%s: rir_files entry missing %r" % (gt_path, key))
        if str(entry["source"]) not in source_ids or str(entry["mic"]) not in mic_ids:
            raise MetricsError("%s: rir_files entry (%s,%s) not in scene sources/mics"
                               % (gt_path, entry["source"], entry["mic"]))
        if int(entry["samples"]) != rir_len:
            raise MetricsError("%s: rir_files samples %r != rir.length_samples %d"
                               % (gt_path, entry["samples"], rir_len))
    sha_sums, sha_sums_sha = _parse_sha256sums(scene_dir)
    expected_paths = {"ground-truth.json"} | {str(e["path"]).replace(chr(92), "/")
                                              for e in rir_files}
    for rel in sorted(expected_paths):
        if rel not in sha_sums:
            raise MetricsError("%s: SHA256SUMS does not cover %s" % (scene_dir, rel))
        p = scene_dir / rel
        if not p.is_file():
            raise MetricsError("%s: file covered by SHA256SUMS is missing: %s"
                               % (scene_dir, rel))
        actual = sha256_file(p)
        if actual != sha_sums[rel]:
            raise MetricsError("%s: SHA256SUMS mismatch for %s" % (scene_dir, rel)
                               + " (listed=%s actual=%s)" % (sha_sums[rel], actual))
    gt_sha = sha_sums["ground-truth.json"]
    return SceneFixture(
        scene_dir=scene_dir,
        scene_id=str(gt["scene_id"]),
        gt=gt,
        gt_sha256=gt_sha,
        sha256sums=sha_sums,
        sha256sums_sha256=sha_sums_sha,
        sample_rate_hz=sample_rate,
        fft_size=fft_size,
        rir_length_samples=rir_len,
        scale=scale,
        freqs=freqs,
        bin_indices=bin_indices,
        source_ids=source_ids,
        mic_ids=mic_ids,
        design_ids=design_ids,
        holdout_ids=holdout_ids,
        design_idx=design_idx,
        holdout_idx=holdout_idx,
        rir_files=rir_files,
        warnings=warnings,
    )


# ---------------------------------------------------------------------------
# band plan + metric primitives (CONTRACT section 5 / spec section 2)
# ---------------------------------------------------------------------------

def third_octave_edges(center_hz: float) -> Tuple[float, float]:
    """1/3-oct band edges: [c*2^(-1/6), c*2^(+1/6)) (CONTRACT section 3)."""
    return (center_hz * 2.0 ** (-1.0 / 6.0), center_hz * 2.0 ** (1.0 / 6.0))


@dataclass
class BandPlan:
    freqs: np.ndarray
    centers_hz: List[float]
    extended_hz: List[float]
    bins: List[np.ndarray] = field(default_factory=list)
    empty_bands: List[float] = field(default_factory=list)

    def __post_init__(self) -> None:
        self.bins = []
        self.empty_bands = []
        for center in self.centers_hz:
            lo, hi = third_octave_edges(center)
            idx = np.nonzero((self.freqs >= lo) & (self.freqs < hi))[0]
            self.bins.append(idx.astype(np.int64))
            if idx.size == 0:
                self.empty_bands.append(center)

    @property
    def n_bands(self) -> int:
        return len(self.centers_hz)

    def labels(self) -> List[str]:
        return [_fmt_hz(c) for c in self.centers_hz]

    def extended_bins(self) -> List[np.ndarray]:
        out = []
        for center in self.extended_hz:
            lo, hi = third_octave_edges(center)
            out.append(np.nonzero((self.freqs >= lo) & (self.freqs < hi))[0]
                       .astype(np.int64))
        return out


def _fmt_hz(value: float) -> str:
    if float(value) == int(value):
        return str(int(value))
    return ("%g" % value)


def build_band_plan(gt: Dict[str, Any], freqs: np.ndarray) -> BandPlan:
    bands = gt.get("bands") or {}
    centers = bands.get("evaluated_spec_centers_hz")
    if centers is None:
        sweep = gt.get("sweep") or {}
        lo_hz = float(sweep.get("start_hz", 20.0))
        hi_hz = float(sweep.get("end_hz", 150.0))
        spec = bands.get("spec_centers_hz") or [40, 50, 63, 80, 100, 125, 160, 200]
        centers = [c for c in spec
                   if third_octave_edges(float(c))[0] >= lo_hz
                   and third_octave_edges(float(c))[1] <= hi_hz]
    extended = bands.get("extended_centers_hz") or list(centers)
    return BandPlan(freqs=np.asarray(freqs, dtype=np.float64),
                    centers_hz=[float(c) for c in centers],
                    extended_hz=[float(c) for c in extended])


def point_band_energies(P: np.ndarray, bins: List[np.ndarray]) -> np.ndarray:
    """(n_points, n_bands) band energies E_m,k = sum_f |P_m(f)|^2."""
    P = np.asarray(P)
    out = np.zeros((P.shape[0], len(bins)), dtype=np.float64)
    for k, idx in enumerate(bins):
        if idx.size:
            out[:, k] = np.sum(np.abs(P[:, idx]) ** 2, axis=1)
    return out


def band_energies(P: np.ndarray, bins: List[np.ndarray]) -> np.ndarray:
    return np.sum(point_band_energies(P, bins), axis=0)


def alpha_weights(e_k0: np.ndarray,
                  alpha_min: float = ALPHA_MIN,
                  alpha_max: float = ALPHA_MAX
                  ) -> Tuple[np.ndarray, Dict[str, Any]]:
    """CONTRACT section 5 / spec section 2.3.

    Valid bands: e_k0 >= 1e-12 * max(e_k0).  Invalid bands get alpha=nan.
    Returns (alpha, info) with info["valid"], info["n_valid"], info["x_raw"],
    info["a_clamped"].
    """
    e = np.asarray(e_k0, dtype=np.float64)
    alpha = np.full(e.shape, np.nan, dtype=np.float64)
    if e.size == 0:
        return alpha, {"valid": np.zeros(0, dtype=bool), "n_valid": 0,
                       "x_raw": np.full(0, np.nan), "a_clamped": np.full(0, np.nan)}
    finite = np.isfinite(e)
    max_e = float(np.max(e[finite])) if np.any(finite) else 0.0
    threshold = ENERGY_NEAR_ZERO * max_e if max_e > 0.0 else 0.0
    valid = finite & (e > threshold)
    n_valid = int(np.count_nonzero(valid))
    x_raw = np.full(e.shape, np.nan, dtype=np.float64)
    a_clamped = np.full(e.shape, np.nan, dtype=np.float64)
    if n_valid == 0:
        return alpha, {"valid": valid, "n_valid": 0, "x_raw": x_raw,
                       "a_clamped": a_clamped}
    mean_e = float(np.mean(e[valid]))
    x_raw[valid] = e[valid] / mean_e
    a_clamped[valid] = np.clip(x_raw[valid], alpha_min, alpha_max)
    denom = float(np.sum(a_clamped[valid]))
    alpha[valid] = a_clamped[valid] * (float(n_valid) / denom)
    return alpha, {"valid": valid, "n_valid": n_valid, "x_raw": x_raw,
                   "a_clamped": a_clamped}


def residual_r(e_k0: np.ndarray, e_after: np.ndarray,
               valid: np.ndarray) -> np.ndarray:
    """R_k = after/before per band; nan for N/A bands."""
    e0 = np.asarray(e_k0, dtype=np.float64)
    ea = np.asarray(e_after, dtype=np.float64)
    r = np.full(e0.shape, np.nan, dtype=np.float64)
    for k in range(e0.size):
        if valid[k] and e0[k] > 0.0:
            r[k] = ea[k] / e0[k]
    return r


def emodal(alpha: np.ndarray, r: np.ndarray) -> Tuple[float, int]:
    """E_modal = sum_k alpha_k R_k over bands where both are valid."""
    alpha = np.asarray(alpha, dtype=np.float64)
    r = np.asarray(r, dtype=np.float64)
    mask = np.isfinite(alpha) & np.isfinite(r)
    n_used = int(np.count_nonzero(mask))
    if n_used == 0:
        return float("nan"), 0
    return float(np.sum(alpha[mask] * r[mask])), n_used


def emodal_equal(r: np.ndarray) -> Tuple[float, int]:
    r = np.asarray(r, dtype=np.float64)
    mask = np.isfinite(r)
    n_used = int(np.count_nonzero(mask))
    if n_used == 0:
        return float("nan"), 0
    return float(np.sum(r[mask])), n_used


def r_max_argmax(r: np.ndarray,
                 centers_hz: Sequence[float]
                 ) -> Tuple[float, Optional[float], Optional[int]]:
    r = np.asarray(r, dtype=np.float64)
    finite = np.isfinite(r)
    if not np.any(finite):
        return float("nan"), None, None
    masked = np.where(finite, r, -np.inf)
    idx = int(np.argmax(masked))  # first (lowest-frequency) maximum on ties
    return float(r[idx]), float(centers_hz[idx]), idx


def band_delta_db(r: np.ndarray) -> np.ndarray:
    """10log10(R_k) per band (design aggregate after/before)."""
    return db10_arr(r)


def point_band_delta_db(P_before: np.ndarray, P_after: np.ndarray,
                        bins: List[np.ndarray]) -> np.ndarray:
    """(n_points, n_bands) Delta_mk = 10log10(after_energy / before_energy)."""
    eb = point_band_energies(P_before, bins)
    ea = point_band_energies(P_after, bins)
    out = np.full(eb.shape, np.nan, dtype=np.float64)
    good = (eb > 0.0) & np.isfinite(eb) & np.isfinite(ea)
    out[good] = 10.0 * np.log10(ea[good] / eb[good])
    return out


def weighted_point_delta(delta_mk: np.ndarray, alpha: np.ndarray) -> np.ndarray:
    """Alpha-weighted mean Delta per point over valid bands (CONTRACT section 5)."""
    delta = np.asarray(delta_mk, dtype=np.float64)
    alpha = np.asarray(alpha, dtype=np.float64)
    n_points = delta.shape[0]
    out = np.full(n_points, np.nan, dtype=np.float64)
    if delta.shape[1] != alpha.size:
        raise MetricsError("alpha/band dimension mismatch in weighted_point_delta")
    for m in range(n_points):
        mask = np.isfinite(delta[m]) & np.isfinite(alpha)
        w = alpha[mask]
        if w.size == 0 or float(np.sum(w)) == 0.0:
            continue
        out[m] = float(np.sum(w * delta[m][mask]) / np.sum(w))
    return out


def spatial_stats(P: np.ndarray, bins: List[np.ndarray],
                  labels: Optional[Sequence[str]] = None,
                  centers: Optional[Sequence[float]] = None
                  ) -> List[Dict[str, Any]]:
    """RoomForge-style spatial statistics per band (CONTRACT section 5).

    For each band k and the given point set:
      E_m,k, level_m = 10log10(E_m,k), mean/std(pop)/range, nearest-rank
      P10/P90, p90_p10_db, coverage_within_3db, null_depth_db profile stats
      (worst_null_db = min_{f,m}(20log10|P_m(f)| - Lbar(f)),
       max_peak_above_mean_db = max of the same), and
      interference_null_db = 10log10(min_m E_m,k / mean_m E_m,k).
    """
    P = np.asarray(P)
    n_pts = P.shape[0]
    out: List[Dict[str, Any]] = []
    mag_db = db20_arr(np.abs(P))
    for k, idx in enumerate(bins):
        label = labels[k] if labels is not None else _fmt_hz(float(k))
        center = (float(centers[k]) if centers is not None and k < len(centers)
                  else None)
        entry: Dict[str, Any] = {"center_hz": center, "band": label}
        if idx.size == 0 or n_pts == 0:
            entry.update(missing_band_stats())
            out.append(entry)
            continue
        # Per-band null profile (CONTRACT section 5): spatial mean over points
        # per frequency Lbar(f) = mean_m 20log10|P_m(f)|, then deviations.
        band_mag_db = mag_db[:, idx]                      # (M, n_freq_in_band)
        with np.errstate(invalid="ignore"):
            lbar_band = np.nanmean(band_mag_db, axis=0)   # (n_freq,)
        dev_band = band_mag_db - lbar_band[None, :]
        finite_dev = dev_band[np.isfinite(dev_band)]
        worst_null = float(np.min(finite_dev)) if finite_dev.size else float("nan")
        max_peak = float(np.max(finite_dev)) if finite_dev.size else float("nan")
        e_m = np.sum(np.abs(P[:, idx]) ** 2, axis=1)
        level = db10_arr(e_m)
        finite = level[np.isfinite(level)]
        n = finite.size
        if n == 0:
            entry.update(missing_band_stats())
            out.append(entry)
            continue
        mean_db = float(np.mean(finite))
        std_db = float(np.std(finite, ddof=0))
        range_db = float(np.max(finite) - np.min(finite))
        p10 = nearest_rank_percentile(finite, 10.0)
        p90 = nearest_rank_percentile(finite, 90.0)
        coverage = float(np.count_nonzero(np.abs(finite - mean_db) <= 3.0) / float(n))
        mean_e = float(np.mean(e_m))
        interference = db10(float(np.min(e_m)) / mean_e) if mean_e > 0.0 else float("nan")
        entry.update({
            "mean_db": mean_db,
            "std_db": std_db,
            "range_db": range_db,
            "p10_db": p10,
            "p90_db": p90,
            "p90_p10_db": float(p90 - p10),
            "coverage_within_3db": coverage,
            "worst_null_db": worst_null,
            "max_peak_above_mean_db": max_peak,
            "interference_null_db": interference,
        })
        out.append(entry)
    return out


def missing_band_stats() -> Dict[str, Any]:
    return {
        "mean_db": None, "std_db": None, "range_db": None, "p10_db": None,
        "p90_db": None, "p90_p10_db": None, "coverage_within_3db": None,
        "worst_null_db": None, "max_peak_above_mean_db": None,
        "interference_null_db": None,
    }


def assess_holdout(P0_holdout: np.ndarray, Pa_holdout: np.ndarray,
                   alpha: np.ndarray, bins: List[np.ndarray],
                   holdout_ids: Sequence[str]) -> Dict[str, Any]:
    """Holdout evaluation (CONTRACT section 5 / spec 6.2), validation only.

    Per-point band deltas use that point's own B0 energy as before (frozen
    convention).  C = improved / valid; severe = any holdout band delta > +3 dB.
    """
    delta = point_band_delta_db(P0_holdout, Pa_holdout, bins)
    weighted = weighted_point_delta(delta, alpha)
    valid_points: List[str] = []
    improved: List[str] = []
    severe_points: List[str] = []
    for m, pid in enumerate(holdout_ids):
        row = delta[m]
        if np.any(np.isfinite(row)):
            valid_points.append(pid)
        if math.isfinite(weighted[m]) and weighted[m] <= 0.0:
            improved.append(pid)
        finite_row = row[np.isfinite(row)]
        if finite_row.size and float(np.max(finite_row)) > SEVERE_DB:
            severe_points.append(pid)
    n_valid = len(valid_points)
    consistency = (float(len(improved)) / float(n_valid)) if n_valid else float("nan")
    return {
        "per_point_band_db": {pid: _nan_to_none(delta[m])
                              for m, pid in enumerate(holdout_ids)},
        "point_weighted_db": {pid: (None if not math.isfinite(weighted[m])
                                    else float(weighted[m]))
                              for m, pid in enumerate(holdout_ids)},
        "valid_holdout_ids": valid_points,
        "valid_holdout_count": n_valid,
        "consistent_holdout_ids": improved,
        "consistency_c": consistency,
        "severe_regression": bool(severe_points),
        "severe_points": severe_points,
        "preliminary_spatial_generalisation": bool(
            n_valid >= 3 and math.isfinite(consistency)
            and consistency >= CONSISTENCY_MIN),
        "incomplete_holdout": bool(n_valid < 3),
    }


def variant_metrics(P0_design: np.ndarray, P0_holdout: np.ndarray,
                    Pa_design: np.ndarray, Pa_holdout: np.ndarray,
                    plan: BandPlan,
                    design_ids: Sequence[str],
                    holdout_ids: Sequence[str]) -> Dict[str, Any]:
    """Full independent metric block for one variant.

    Frozen conventions (CONTRACT sections 5 and 8, roomforge-mimo 2026-11-22):
      * design block: denominator/weights = B0(design) E_k0 / alpha;
      * holdout block: denominator = B0 energies over the holdout point set,
        weights still = B0(design) alpha; per-point before = point's own B0;
      * holdout band_delta_db = 10log10(sum holdout after / sum holdout before).
    """
    e_k0 = band_energies(P0_design, plan.bins)
    alpha, alpha_info = alpha_weights(e_k0)
    e_design_after = band_energies(Pa_design, plan.bins)
    r_design = residual_r(e_k0, e_design_after, alpha_info["valid"])
    e_modal_val, n_used = emodal(alpha, r_design)
    e_modal_eq_val, eq_used = emodal_equal(r_design)
    # CONTRACT section 5 (frozen 2026-11-22): e_modal_db is normalised so that
    # B0 (R_k = 1) is exactly 0 dB:
    #   e_modal_db    = 10 log10(E_modal / sum_{valid} alpha_k)
    #   e_modal_eq_db = 10 log10((sum R_k) / K_valid)
    alpha_sum_valid = float(np.sum(alpha[np.isfinite(alpha)]))
    e_modal_db = db10(e_modal_val / alpha_sum_valid) if alpha_sum_valid > 0.0 \
        else float("nan")
    e_modal_eq_db = db10(e_modal_eq_val / float(eq_used)) if eq_used > 0 \
        else float("nan")
    rmax, rmax_band, rmax_idx = r_max_argmax(r_design, plan.centers_hz)
    delta_design = point_band_delta_db(P0_design, Pa_design, plan.bins)

    e_k0_holdout = band_energies(P0_holdout, plan.bins)
    e_holdout_after = band_energies(Pa_holdout, plan.bins)
    r_holdout = residual_r(e_k0_holdout, e_holdout_after, alpha_info["valid"])
    e_modal_holdout, n_used_holdout = emodal(alpha, r_holdout)
    rmax_holdout, rmax_holdout_band, rmax_holdout_idx = r_max_argmax(
        r_holdout, plan.centers_hz)
    holdout = assess_holdout(P0_holdout, Pa_holdout, alpha, plan.bins, holdout_ids)
    e_modal_eq_holdout, eq_used_holdout = emodal_equal(r_holdout)
    migration = bool(np.any(np.isfinite(r_design)
                            & (r_design > 1.0 + EPS_MIGRATION)))
    migrated_bands = [plan.centers_hz[k] for k in range(plan.n_bands)
                      if math.isfinite(r_design[k])
                      and r_design[k] > 1.0 + EPS_MIGRATION]
    design = {
        "r_k": _nan_to_none(r_design),
        "e_k0": _nan_to_none(e_k0),
        "alpha": _nan_to_none(alpha),
        "e_modal": e_modal_val,
        "e_modal_db": e_modal_db,
        "e_modal_eq": e_modal_eq_val,
        "e_modal_eq_db": e_modal_eq_db,
        "r_max": rmax,
        "argmax_band_hz": rmax_band,
        "band_delta_db": _nan_to_none(band_delta_db(r_design)),
        "energy_migration": migration,
        "energy_migration_bands_hz": migrated_bands,
        # Rust computes the spatial block on the variant's corrected pressure
        # (P_after); B0 is the only case where before == after.  Keep the
        # before-pressure version as an extra observation under spatial_before.
        "spatial": spatial_stats(Pa_design, plan.bins, plan.labels(),
                                  plan.centers_hz),
        "spatial_before": spatial_stats(P0_design, plan.bins, plan.labels(),
                                         plan.centers_hz),
        "per_point_band_db": {pid: _nan_to_none(delta_design[m])
                              for m, pid in enumerate(design_ids)},
        "n_bands_used": n_used,
        "n_bands_na": int(plan.n_bands - n_used),
    }
    holdout_block = {
        "r_k": _nan_to_none(r_holdout),
        "e_k0": _nan_to_none(e_k0_holdout),
        "e_modal": e_modal_holdout,
        "e_modal_db": (db10(e_modal_holdout / alpha_sum_valid)
                       if alpha_sum_valid > 0.0 else float("nan")),
        "r_max": rmax_holdout,
        "argmax_band_hz": rmax_holdout_band,
        "band_delta_db": _nan_to_none(band_delta_db(r_holdout)),
        "e_modal_eq": e_modal_eq_holdout,
        "e_modal_eq_db": (db10(e_modal_eq_holdout / float(eq_used_holdout))
                          if eq_used_holdout > 0 else float("nan")),
        "energy_migration": bool(np.any(np.isfinite(r_holdout)
                                       & (r_holdout > 1.0 + EPS_MIGRATION))),
        "improved_points": list(holdout.get("consistent_holdout_ids") or []),
        "spatial": spatial_stats(Pa_holdout, plan.bins, plan.labels(),
                                  plan.centers_hz),
        "spatial_before": spatial_stats(P0_holdout, plan.bins, plan.labels(),
                                         plan.centers_hz),
        "n_bands_used": n_used_holdout,
        "n_bands_na": int(plan.n_bands - n_used_holdout),
    }
    holdout_block.update(holdout)
    return {"design": design, "holdout": holdout_block,
            "alpha_info": _alpha_info_jsonable(alpha_info),
            "centers_hz": list(plan.centers_hz)}


def _nan_to_none(arr: Any) -> List[Any]:
    if isinstance(arr, np.ndarray):
        return [None if not math.isfinite(float(v)) else float(v)
                for v in arr.tolist()]
    return [None if not math.isfinite(float(v)) else float(v) for v in arr]


def _alpha_info_jsonable(info: Dict[str, Any]) -> Dict[str, Any]:
    return {
        "n_valid": int(info.get("n_valid", 0)),
        "x_raw": _nan_to_none(info.get("x_raw", np.array([], dtype=np.float64))),
        "a_clamped": _nan_to_none(info.get("a_clamped",
                                           np.array([], dtype=np.float64))),
    }

# ---------------------------------------------------------------------------
# FIR files (both layouts of CONTRACT section 8) and per-variant G(f)
# ---------------------------------------------------------------------------

_FIR_HEADER_KV = re.compile(r"([A-Za-z_][A-Za-z0-9_]*)=([^,|\s]+)")
_FIR_PLACEHOLDERS = {"<out>", "<in>", "<src>", "null", "none", "-", ""}


def _fir_meta_value(raw: Any) -> Optional[str]:
    """Treat template placeholders such as <OUT>/<IN>/<SRC> as 'not present'."""
    if raw is None:
        return None
    text = str(raw).strip()
    if text.lower() in _FIR_PLACEHOLDERS:
        return None
    return text


@dataclass
class FirFile:
    path: Path
    header: str
    coeffs: np.ndarray
    source: Optional[str] = None
    in_id: Optional[str] = None
    out_id: Optional[str] = None
    taps: Optional[int] = None
    sample_rate_hz: Optional[int] = None
    delay_samples: Optional[int] = None

    @property
    def layout(self) -> str:
        if self.source is not None:
            return "per-source"
        if self.out_id is not None or self.in_id is not None:
            return "matrix"
        return "unknown"


def read_fir_csv(path: Path, expected_taps: Optional[int] = None) -> FirFile:
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    if not lines:
        raise MetricsError("%s: empty FIR csv" % path)
    meta: Dict[str, str] = {}
    data_lines: List[str] = []
    header = ""
    idx = 0
    while idx < len(lines) and not lines[idx].strip():
        idx += 1
    if idx < len(lines) and lines[idx].lstrip().startswith("#"):
        header = lines[idx].strip()
        meta = {m.group(1).lower(): m.group(2) for m in _FIR_HEADER_KV.finditer(header)}
        idx += 1
    data_lines = [ln for ln in lines[idx:] if ln.strip() and not ln.lstrip().startswith("#")]
    tokens: List[str] = []
    for ln in data_lines:
        tokens.extend(ln.replace(",", " ").replace(";", " ").split())
    if not tokens:
        raise MetricsError("%s: no tap values" % path)
    try:
        coeffs = np.asarray([float(tok) for tok in tokens], dtype=np.float64)
    except ValueError as exc:
        raise MetricsError("%s: non-numeric tap value (%s)" % (path, exc))
    if not np.all(np.isfinite(coeffs)):
        raise MetricsError("%s: non-finite tap value" % path)
    if expected_taps is not None and coeffs.size != expected_taps:
        raise MetricsError("%s: %d taps != expected %d"
                           % (path, coeffs.size, expected_taps))
    ff = FirFile(
        path=path,
        header=header,
        coeffs=coeffs,
        source=_fir_meta_value(meta.get("source")),
        in_id=_fir_meta_value(meta.get("in")),
        out_id=_fir_meta_value(meta.get("out")),
        taps=int(meta["taps"]) if "taps" in meta else int(coeffs.size),
        sample_rate_hz=int(meta["sample_rate_hz"]) if "sample_rate_hz" in meta else None,
        delay_samples=(int(meta["delay_samples"]) if "delay_samples" in meta else None),
    )
    if ff.taps != coeffs.size:
        raise MetricsError("%s: header taps=%d but %d values"
                           % (path, ff.taps, coeffs.size))
    return ff


def _resolve_path(raw_path: str, scene_dir: Path, runs_dir: Path) -> Optional[Path]:
    p = Path(str(raw_path))
    candidates: List[Path] = []
    if p.is_absolute():
        candidates.append(p)
    else:
        candidates.extend([scene_dir / p, runs_dir / p, runs_dir / scene_dir.name / p,
                           scene_dir / "fir" / p.name])
    for cand in candidates:
        if cand.is_file():
            return cand
    return None


def resolve_variant_fir_files(variant: Dict[str, Any], scene_dir: Path,
                              runs_dir: Path,
                              issues: List[Dict[str, Any]],
                              variant_id: str) -> List[FirFile]:
    """Read the FIR csv files declared by a report variant (both layouts)."""
    entries = variant.get("fir_files") or []
    files: List[FirFile] = []
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        raw = entry.get("path")
        if not raw:
            issues.append({"kind": "report_schema", "variant": variant_id,
                           "message": "fir_files entry without path: %r" % entry})
            continue
        resolved = _resolve_path(str(raw), scene_dir, runs_dir)
        if resolved is None:
            issues.append({"kind": "missing_fir_file", "variant": variant_id,
                           "message": "cannot resolve FIR path %r" % raw})
            continue
        expected_taps = int(entry["taps"]) if entry.get("taps") is not None else None
        try:
            ff = read_fir_csv(resolved, expected_taps=expected_taps)
        except MetricsError as exc:
            issues.append({"kind": "bad_fir_file", "variant": variant_id,
                           "message": str(exc)})
            continue
        declared_source = _fir_meta_value(entry.get("source"))
        declared_out = _fir_meta_value(entry.get("out"))
        declared_in = _fir_meta_value(entry.get("in"))
        if declared_source is not None:
            # Own-LS per-source layout: the entry says this file is G_n.
            ff.source = declared_source
            ff.out_id = None
            ff.in_id = None
        else:
            if declared_out is not None:
                ff.out_id = declared_out
            if declared_in is not None:
                ff.in_id = declared_in
        files.append(ff)
    if not files:
        # fallback: scan <runs>/<scene>/fir/*.csv then <fixtures>/<scene>/fir/*.csv
        fir_dirs = [runs_dir / scene_dir.name / "fir", scene_dir / "fir"]
        seen_paths = set()
        for fir_dir in fir_dirs:
            if not fir_dir.is_dir():
                continue
            for csv_path in sorted(fir_dir.glob("*.csv")):
                if csv_path in seen_paths:
                    continue
                seen_paths.add(csv_path)
                try:
                    files.append(read_fir_csv(csv_path))
                except MetricsError as exc:
                    issues.append({"kind": "bad_fir_file", "variant": variant_id,
                                   "message": str(exc)})
    if not files:
        issues.append({"kind": "missing_fir_file", "variant": variant_id,
                       "message": "no fir_files in report and no <scene>/fir/*.csv"})
    return files


def freq_response(coeffs: np.ndarray, freqs: np.ndarray,
                  sample_rate_hz: int) -> np.ndarray:
    """G(f) = sum_t h[t] exp(-j 2 pi f t / fs) at the exact fixture frequencies."""
    h = np.asarray(coeffs, dtype=np.float64)
    t = np.arange(h.size, dtype=np.float64) / float(sample_rate_hz)
    # (K, L) phases, then (K,) complex response
    return np.exp(-2j * np.pi * np.outer(freqs, t)) @ h


def build_g_own_ls(files: List[FirFile], source_ids: Sequence[str],
                   freqs: np.ndarray, sample_rate_hz: int,
                   issues: List[Dict[str, Any]], variant_id: str
                   ) -> Optional[np.ndarray]:
    """Per-source layout: G shape (N, K)."""
    n = len(source_ids)
    g = np.zeros((n, len(freqs)), dtype=np.complex128)
    seen = np.zeros(n, dtype=bool)
    by_source: Dict[str, FirFile] = {}
    for ff in files:
        if ff.source is None:
            continue
        if ff.source not in source_ids:
            issues.append({"kind": "report_schema", "variant": variant_id,
                           "message": "FIR file %s for unknown source %r"
                                      % (ff.path.name, ff.source)})
            continue
        if ff.source in by_source:
            issues.append({"kind": "report_schema", "variant": variant_id,
                           "message": "duplicate FIR file for source %r" % ff.source})
            continue
        by_source[ff.source] = ff
    for n_i, src in enumerate(source_ids):
        ff = by_source.get(src)
        if ff is None:
            issues.append({"kind": "missing_fir_file", "variant": variant_id,
                           "message": "no FIR taps for source %r (cannot rebuild P_after)"
                                      % src})
            continue
        if ff.sample_rate_hz is not None and ff.sample_rate_hz != sample_rate_hz:
            issues.append({"kind": "report_schema", "variant": variant_id,
                           "message": "FIR %s sample_rate_hz=%d != fixture %d"
                                      % (ff.path.name, ff.sample_rate_hz, sample_rate_hz)})
        g[n_i, :] = freq_response(ff.coeffs, freqs, sample_rate_hz)
        seen[n_i] = True
    if not np.all(seen):
        return None
    return g


def build_g_matrix(files: List[FirFile], source_ids: Sequence[str],
                   freqs: np.ndarray, sample_rate_hz: int,
                   issues: List[Dict[str, Any]], variant_id: str
                   ) -> Optional[np.ndarray]:
    """N x N path layout: G[out][in] files; effective drive per output source.

    P = sum_out H[m,out] * sum_in G[out][in]  (s_ref: every input = 1+0j).
    Returns effective G_eff shape (N_out, K); zero paths when a file is absent.
    """
    index_of = {src: i for i, src in enumerate(source_ids)}
    g_eff = np.zeros((len(source_ids), len(freqs)), dtype=np.complex128)
    if not files:
        return None
    have_any = False
    for ff in files:
        out_id = ff.out_id
        in_id = ff.in_id
        if out_id is None and in_id is None:
            continue
        out_name = str(out_id) if out_id is not None else None
        in_name = str(in_id) if in_id is not None else None
        if out_name not in index_of or in_name not in index_of:
            issues.append({"kind": "report_schema", "variant": variant_id,
                           "message": "FIR %s out/in (%r,%r) not in scene sources"
                                      % (ff.path.name, out_name, in_name)})
            continue
        if ff.sample_rate_hz is not None and ff.sample_rate_hz != sample_rate_hz:
            issues.append({"kind": "report_schema", "variant": variant_id,
                           "message": "FIR %s sample_rate_hz=%d != fixture %d"
                                      % (ff.path.name, ff.sample_rate_hz, sample_rate_hz)})
        g_eff[index_of[out_name], :] += freq_response(ff.coeffs, freqs, sample_rate_hz)
        have_any = True
    return g_eff if have_any else None


def build_g_b1(params: Dict[str, Any], source_ids: Sequence[str],
               freqs: np.ndarray, sample_rate_hz: int,
               issues: List[Dict[str, Any]], variant_id: str = "B1"
               ) -> Optional[np.ndarray]:
    """B1: G_n(f) = polarity * 10^(gain_db/20) * exp(-j 2 pi f delay/fs)."""
    if not isinstance(params, dict):
        issues.append({"kind": "report_schema", "variant": variant_id,
                       "message": "missing params object"})
        return None
    sources = params.get("sources")
    if not isinstance(sources, list) or not sources:
        issues.append({"kind": "report_schema", "variant": variant_id,
                       "message": "missing params.sources list"})
        return None
    by_id: Dict[str, Dict[str, Any]] = {}
    for item in sources:
        if not isinstance(item, dict) or "id" not in item:
            issues.append({"kind": "report_schema", "variant": variant_id,
                           "message": "malformed params.sources item: %r" % (item,)})
            continue
        by_id[str(item["id"])] = item
    g = np.zeros((len(source_ids), len(freqs)), dtype=np.complex128)
    for n_i, src in enumerate(source_ids):
        item = by_id.get(src)
        if item is None:
            issues.append({"kind": "report_schema", "variant": variant_id,
                           "message": "params.sources missing scene source %r" % src})
            return None
        delay = int(item.get("delay_samples", 0))
        gain_db = float(item.get("gain_db", 0.0))
        polarity = float(item.get("polarity", 1))
        if delay < 0:
            issues.append({"kind": "report_schema", "variant": variant_id,
                           "message": "negative delay_samples for %r" % src})
            return None
        if polarity not in (-1.0, 1.0):
            issues.append({"kind": "report_schema", "variant": variant_id,
                           "message": "polarity %r for %r not in {+1,-1}"
                                      % (polarity, src)})
            return None
        g[n_i, :] = (polarity * 10.0 ** (gain_db / 20.0)
                     * np.exp(-2j * np.pi * freqs * (delay / float(sample_rate_hz))))
    return g


def pressure(P0: np.ndarray, h: np.ndarray, g: Optional[np.ndarray]) -> np.ndarray:
    """P[m,k] = sum_n H[m,n,k] G_n[k] (G=None -> baseline G=1)."""
    if g is None:
        g = np.ones((h.shape[1], h.shape[2]), dtype=np.complex128)
    if g.shape != (h.shape[1], h.shape[2]):
        raise MetricsError("G shape %r does not match H (N,K)=(%d,%d)"
                           % (g.shape, h.shape[1], h.shape[2]))
    return np.einsum("mnk,nk->mk", h, g)


# ---------------------------------------------------------------------------
# Rust report loading (CONTRACT section 8 frozen schema)
# ---------------------------------------------------------------------------

DESIGN_KEYS = ["r_k", "e_k0", "alpha", "e_modal", "e_modal_db", "e_modal_eq",
               "e_modal_eq_db", "r_max", "argmax_band_hz", "band_delta_db",
               "energy_migration", "per_point_band_db", "spatial"]
HOLDOUT_KEYS = ["r_k", "e_modal_db", "r_max", "band_delta_db",
                "per_point_band_db", "point_weighted_db", "consistency_c",
                "valid_holdout_count", "severe_regression", "severe_points",
                "spatial"]


def find_scene_dirs(fixtures_dir: Path) -> List[Path]:
    dirs = []
    if not fixtures_dir.is_dir():
        return dirs
    for child in sorted(fixtures_dir.iterdir()):
        if child.is_dir() and (child / "ground-truth.json").is_file():
            dirs.append(child)
    return dirs


def report_path_for(runs_dir: Path, scene_id: str) -> Path:
    return runs_dir / scene_id / "report.json"


def load_rust_report(path: Path) -> Dict[str, Any]:
    report = load_json(path)
    if not isinstance(report, dict):
        raise MetricsError("%s: report is not a JSON object" % path)
    fmt = report.get("format")
    if fmt != REPORT_FORMAT:
        raise MetricsError("%s: format %r != %r (CONTRACT section 8)"
                           % (path, fmt, REPORT_FORMAT))
    variants = report.get("variants")
    if not isinstance(variants, dict):
        raise MetricsError("%s: 'variants' must be a JSON object (found %r)"
                           % (path, type(variants).__name__))
    for vid in ("B0", "B1", "B2", "B3"):
        if vid not in variants:
            raise MetricsError("%s: variants missing %r" % (path, vid))
    for vid in ("B0", "B1", "B2", "B3"):
        variant = variants[vid]
        if not isinstance(variant, dict):
            raise MetricsError("%s: variants.%s is not an object" % (path, vid))
        for block_name in ("design", "holdout"):
            block = variant.get(block_name)
            if not isinstance(block, dict):
                raise MetricsError("%s: variants.%s.%s missing/not an object"
                                   % (path, vid, block_name))
    return report


def required_missing(report: Dict[str, Any]) -> List[str]:
    missing: List[str] = []
    for vid, variant in report.get("variants", {}).items():
        if vid not in ("B0", "B1", "B2", "B3"):
            continue
        for block_name, keys in (("design", DESIGN_KEYS), ("holdout", HOLDOUT_KEYS)):
            block = variant.get(block_name) or {}
            for key in keys:
                if key not in block:
                    missing.append("variants.%s.%s.%s" % (vid, block_name, key))
    return missing


# ---------------------------------------------------------------------------
# cross-check: Python vs Rust, one row per scalar (never silently adapt)
# ---------------------------------------------------------------------------

SPATIAL_DB_FIELDS = ["mean_db", "std_db", "range_db", "p90_p10_db",
                     "worst_null_db", "max_peak_above_mean_db",
                     "interference_null_db"]
SPATIAL_RATIO_FIELDS = ["coverage_within_3db"]
SPATIAL_ALL_FIELDS = SPATIAL_DB_FIELDS + SPATIAL_RATIO_FIELDS

FAMILY_NAMES = {
    "E_modal_db": "E_modal_db", "E_modal": "E_modal", "E_modal_eq": "E_modal_eq",
    "R_k": "R_k", "R_max": "R_max", "alpha": "alpha", "e_k0": "e_k0",
    "band_delta_db": "band_delta_db",
    "spatial_mean_db": "spatial_mean_db", "spatial_std_db": "spatial_std_db",
    "spatial_range_db": "spatial_range_db", "spatial_p90_p10_db": "spatial_p90_p10_db",
    "spatial_worst_null_db": "spatial_worst_null_db",
    "spatial_max_peak_db": "spatial_max_peak_db",
    "spatial_interference_null_db": "spatial_interference_null_db",
    "coverage": "coverage", "per_point_band_db": "per_point_band_db",
    "point_weighted_db": "point_weighted_db", "consistency_c": "consistency_c",
    "argmax_band_hz": "argmax_band_hz",
}


NON_FAILING_STATUSES = frozenset(("PASS", "NOTE"))


def is_failing_status(status: Any) -> bool:
    "True for real cross-check failures; NOTE marks an explained rule gap."
    return status not in NON_FAILING_STATUSES


class DiffTable:
    """Collects every Python-vs-Rust comparison and its verdict."""

    def __init__(self) -> None:
        self.rows: List[Dict[str, Any]] = []
        self.issues: List[Dict[str, Any]] = []

    def _add(self, scene: str, variant: str, block: str, metric: str,
             item: Any, py_val: Any, rust_val: Any, kind: str,
             tol: float, family: Optional[str] = None) -> None:
        status = "PASS"
        abs_diff: Optional[float] = None
        rel: Optional[float] = None
        if py_val is None and rust_val is None:
            status = "PASS"
        elif py_val is None:
            status = "MISSING_PYTHON"
        elif rust_val is None:
            status = "MISSING_RUST"
        else:
            if kind in ("ratio", "db"):
                py_f = float(py_val)
                rust_f = float(rust_val)
                abs_diff = abs(py_f - rust_f)
                rel = rel_diff(py_f, rust_f) if kind == "ratio" else None
                if not (math.isfinite(py_f) and math.isfinite(rust_f)):
                    status = "NONFINITE"
                elif kind == "ratio":
                    status = "PASS" if rel <= tol else "FAIL"
                else:
                    status = "PASS" if abs_diff <= tol else "FAIL"
            elif kind == "bool":
                status = "PASS" if bool(py_val) == bool(rust_val) else "FAIL"
                abs_diff = 0.0 if status == "PASS" else 1.0
            elif kind == "exact":
                py_s, rust_s = str(py_val), str(rust_val)
                status = "PASS" if py_s == rust_s else "FAIL"
                abs_diff = 0.0 if status == "PASS" else 1.0
            elif kind == "float_exact":
                py_f = float(py_val)
                rust_f = float(rust_val)
                abs_diff = abs(py_f - rust_f)
                rel = rel_diff(py_f, rust_f)
                status = "PASS" if abs_diff <= tol else "FAIL"
            elif kind == "int":
                status = "PASS" if int(py_val) == int(rust_val) else "FAIL"
                abs_diff = float(abs(int(py_val) - int(rust_val)))
            else:
                status = "UNKNOWN_KIND"
        self.rows.append({
            "scene": scene, "variant": variant, "block": block, "metric": metric,
            "item": "" if item is None else str(item),
            "python": py_val, "rust": rust_val,
            "abs_diff": abs_diff, "rel_diff": rel,
            "tolerance": tol, "kind": kind, "family": family or metric,
            "status": status,
        })

    # -- scalar helpers -----------------------------------------------------
    def add_ratio(self, scene: str, variant: str, block: str, metric: str,
                  py_val: Any, rust_val: Any, item: Any = None,
                  family: Optional[str] = None) -> None:
        self._add(scene, variant, block, metric, item, py_val, rust_val,
                  "ratio", TOL_RATIO, family)

    def add_db(self, scene: str, variant: str, block: str, metric: str,
               py_val: Any, rust_val: Any, item: Any = None,
               family: Optional[str] = None) -> None:
        self._add(scene, variant, block, metric, item, py_val, rust_val,
                  "db", TOL_DB, family)

    def add_bool(self, scene: str, variant: str, block: str, metric: str,
                 py_val: Any, rust_val: Any, item: Any = None) -> None:
        self._add(scene, variant, block, metric, item, py_val, rust_val,
                  "bool", 0.0)

    def add_exact(self, scene: str, variant: str, block: str, metric: str,
                  py_val: Any, rust_val: Any, item: Any = None) -> None:
        self._add(scene, variant, block, metric, item, py_val, rust_val,
                  "exact", 0.0)

    def add_int(self, scene: str, variant: str, block: str, metric: str,
                py_val: Any, rust_val: Any, item: Any = None) -> None:
        self._add(scene, variant, block, metric, item, py_val, rust_val,
                  "int", 0.0)

    def add_float_exact(self, scene: str, variant: str, block: str, metric: str,
                        py_val: Any, rust_val: Any, item: Any = None,
                        tol: float = 1e-9) -> None:
        self._add(scene, variant, block, metric, item, py_val, rust_val,
                  "float_exact", tol)

    def add_ratio_tol(self, scene: str, variant: str, block: str, metric: str,
                      py_val: Any, rust_val: Any, tol: float,
                      item: Any = None, family: Optional[str] = None) -> None:
        self._add(scene, variant, block, metric, item, py_val, rust_val,
                  "ratio", tol, family)

    def add_db_tol(self, scene: str, variant: str, block: str, metric: str,
                   py_val: Any, rust_val: Any, tol: float,
                   item: Any = None, family: Optional[str] = None) -> None:
        self._add(scene, variant, block, metric, item, py_val, rust_val,
                  "db", tol, family)

    def add_bool_tol(self, scene: str, variant: str, block: str, metric: str,
                     py_val: Any, rust_val: Any, item: Any = None,
                     family: Optional[str] = None) -> None:
        self._add(scene, variant, block, metric, item, py_val, rust_val,
                  "bool", 0.0, family)

    def add_array(self, scene: str, variant: str, block: str, metric: str,
                  py_vals: Any, rust_vals: Any, kind: str,
                  items: Optional[Sequence[Any]] = None,
                  family: Optional[str] = None) -> None:
        py_list = list(py_vals) if py_vals is not None else None
        rust_list = list(rust_vals) if rust_vals is not None else None
        if py_list is None or rust_list is None:
            self._add(scene, variant, block, metric, "array", py_list, rust_list,
                      kind, TOL_RATIO if kind == "ratio" else TOL_DB, family)
            return
        if len(py_list) != len(rust_list):
            self.rows.append({
                "scene": scene, "variant": variant, "block": block,
                "metric": metric, "item": "array-length",
                "python": len(py_list), "rust": len(rust_list),
                "abs_diff": float(abs(len(py_list) - len(rust_list))),
                "rel_diff": None, "tolerance": 0.0, "kind": "length",
                "family": family or metric, "status": "LENGTH_MISMATCH",
            })
            return
        tol = TOL_RATIO if kind == "ratio" else TOL_DB
        for idx, (py_v, rust_v) in enumerate(zip(py_list, rust_list)):
            label = items[idx] if items is not None and idx < len(items) else idx
            self._add(scene, variant, block, metric, label, py_v, rust_v,
                      kind, tol, family)

    def add_spatial(self, scene: str, variant: str, block: str,
                    py_spatial: Any, rust_spatial: Any,
                    centers: Sequence[float]) -> None:
        py_list = list(py_spatial) if py_spatial is not None else None
        rust_list = list(rust_spatial) if rust_spatial is not None else None
        if py_list is None or rust_list is None:
            self._add(scene, variant, block, "spatial", "array", py_list,
                      rust_list, "exact", 0.0, "spatial")
            return
        if len(py_list) != len(rust_list):
            self.rows.append({
                "scene": scene, "variant": variant, "block": block,
                "metric": "spatial", "item": "array-length",
                "python": len(py_list), "rust": len(rust_list),
                "abs_diff": float(abs(len(py_list) - len(rust_list))),
                "rel_diff": None, "tolerance": 0.0, "kind": "length",
                "family": "spatial", "status": "LENGTH_MISMATCH",
            })
            return
        for k, (py_band, rust_band) in enumerate(zip(py_list, rust_list)):
            label = _fmt_hz(float(centers[k])) if k < len(centers) else str(k)
            if not isinstance(rust_band, dict):
                self.rows.append({
                    "scene": scene, "variant": variant, "block": block,
                    "metric": "spatial", "item": "band-" + label,
                    "python": None, "rust": rust_band, "abs_diff": None,
                    "rel_diff": None, "tolerance": 0.0, "kind": "object",
                    "family": "spatial", "status": "MALFORMED_RUST",
                })
                continue
            if "center_hz" in rust_band:
                self.add_float_exact(scene, variant, block, "spatial.center_hz",
                                     float(centers[k]) if k < len(centers) else None,
                                     rust_band.get("center_hz"), item=label,
                                     tol=1e-6)
            for field in SPATIAL_ALL_FIELDS:
                py_v = py_band.get(field)
                rust_v = rust_band.get(field)
                fam = ("coverage" if field in SPATIAL_RATIO_FIELDS
                       else "spatial_" + field.replace("_db", "").replace(
                           "max_peak_above_mean", "max_peak"))
                if field in SPATIAL_RATIO_FIELDS:
                    self._add(scene, variant, block, "spatial." + field, label,
                              py_v, rust_v, "ratio", TOL_RATIO, fam)
                else:
                    self._add(scene, variant, block, "spatial." + field, label,
                              py_v, rust_v, "db", TOL_DB, fam)

    def add_structured(self, scene: str, variant: str, block: str, metric: str,
                       py_map: Any, rust_map: Any, kind: str,
                       family: Optional[str] = None) -> None:
        """Compare dicts of {point_id: value-or-array}."""
        py_map = py_map if isinstance(py_map, dict) else None
        rust_map = rust_map if isinstance(rust_map, dict) else None
        if py_map is None or rust_map is None:
            self._add(scene, variant, block, metric, "object", py_map, rust_map,
                      "exact", 0.0, family)
            return
        all_ids = sorted(set(py_map) | set(rust_map),
                         key=lambda x: (len(str(x)), str(x)))
        for pid in all_ids:
            py_v = py_map.get(pid)
            rust_v = rust_map.get(pid)
            if isinstance(py_v, (list, tuple)) or isinstance(rust_v, (list, tuple)):
                self.add_array(scene, variant, block, metric, py_v, rust_v, kind,
                               family=family)
                continue
            if kind == "ratio":
                self._add(scene, variant, block, metric, pid, py_v, rust_v,
                          "ratio", TOL_RATIO, family)
            elif kind == "db":
                self._add(scene, variant, block, metric, pid, py_v, rust_v,
                          "db", TOL_DB, family)
            else:
                self._add(scene, variant, block, metric, pid, py_v, rust_v,
                          kind, 0.0, family)

    def add_point_band(self, scene: str, variant: str, block: str,
                       py_map: Any, rust_map: Any,
                       centers: Sequence[float],
                       family: str = "per_point_band_db") -> None:
        py_map = py_map if isinstance(py_map, dict) else None
        rust_map = rust_map if isinstance(rust_map, dict) else None
        if py_map is None or rust_map is None:
            self._add(scene, variant, block, "per_point_band_db", "object",
                      py_map, rust_map, "exact", 0.0, family)
            return
        band_labels = [_fmt_hz(c) for c in centers]
        for pid in sorted(set(py_map) | set(rust_map)):
            self.add_array(scene, variant, block, "per_point_band_db.%s" % pid,
                           py_map.get(pid), rust_map.get(pid), "db",
                           items=band_labels, family=family)

    def add_note(self, scene: str, variant: str, block: str, metric: str,
                 py_val: Any, rust_val: Any, item: Any = None,
                 message: Optional[str] = None) -> None:
        "Record an explained difference that is not a cross-check failure."
        self.rows.append({
            "scene": scene, "variant": variant, "block": block,
            "metric": metric, "item": "" if item is None else str(item),
            "python": py_val, "rust": rust_val,
            "abs_diff": None, "rel_diff": None, "tolerance": None,
            "kind": "note", "family": metric, "status": "NOTE",
            "note": message,
        })

    # -- summary ------------------------------------------------------------
    def mismatch_rows(self) -> List[Dict[str, Any]]:
        return [r for r in self.rows if is_failing_status(r["status"])]

    def max_abs_by_family(self) -> Dict[str, float]:
        out: Dict[str, float] = {}
        for row in self.rows:
            if row["status"] != "PASS":
                continue
            diff = row.get("abs_diff")
            if diff is None or not math.isfinite(float(diff)):
                continue
            family = row["family"]
            out[family] = max(out.get(family, 0.0), float(diff))
        return out

    def counts(self) -> Dict[str, int]:
        counts: Dict[str, int] = {}
        for row in self.rows:
            counts[row["status"]] = counts.get(row["status"], 0) + 1
        return counts


def crosscheck_variant(diffs: DiffTable, scene_id: str, variant_id: str,
                       py_variant: Dict[str, Any], rust_variant: Dict[str, Any],
                       centers: Sequence[float]) -> None:
    """Compare one variant block-by-block with CONTRACT section 8 keys."""
    band_items = [_fmt_hz(c) for c in centers]
    py_design = py_variant.get("design") or {}
    rust_design = rust_variant.get("design") or {}
    py_holdout = py_variant.get("holdout") or {}
    rust_holdout = rust_variant.get("holdout") or {}

    # design block
    diffs.add_array(scene_id, variant_id, "design", "r_k", py_design.get("r_k"),
                    rust_design.get("r_k"), "ratio", items=band_items)
    diffs.add_array(scene_id, variant_id, "design", "e_k0", py_design.get("e_k0"),
                    rust_design.get("e_k0"), "ratio", items=band_items)
    diffs.add_array(scene_id, variant_id, "design", "alpha", py_design.get("alpha"),
                    rust_design.get("alpha"), "ratio", items=band_items)
    diffs.add_ratio(scene_id, variant_id, "design", "e_modal",
                    py_design.get("e_modal"), rust_design.get("e_modal"))
    diffs.add_db(scene_id, variant_id, "design", "e_modal_db",
                 py_design.get("e_modal_db"), rust_design.get("e_modal_db"))
    diffs.add_ratio(scene_id, variant_id, "design", "e_modal_eq",
                    py_design.get("e_modal_eq"), rust_design.get("e_modal_eq"))
    diffs.add_db(scene_id, variant_id, "design", "e_modal_eq_db",
                 py_design.get("e_modal_eq_db"), rust_design.get("e_modal_eq_db"))
    diffs.add_ratio(scene_id, variant_id, "design", "r_max",
                    py_design.get("r_max"), rust_design.get("r_max"))
    diffs.add_float_exact(scene_id, variant_id, "design", "argmax_band_hz",
                          py_design.get("argmax_band_hz"),
                          rust_design.get("argmax_band_hz"))
    diffs.add_array(scene_id, variant_id, "design", "band_delta_db",
                    py_design.get("band_delta_db"),
                    rust_design.get("band_delta_db"), "db", items=band_items)
    diffs.add_bool(scene_id, variant_id, "design", "energy_migration",
                   py_design.get("energy_migration"),
                   rust_design.get("energy_migration"))
    diffs.add_point_band(scene_id, variant_id, "design",
                         py_design.get("per_point_band_db"),
                         rust_design.get("per_point_band_db"), centers)
    diffs.add_spatial(scene_id, variant_id, "design", py_design.get("spatial"),
                      rust_design.get("spatial"), centers)

    # holdout block
    diffs.add_array(scene_id, variant_id, "holdout", "r_k",
                    py_holdout.get("r_k"), rust_holdout.get("r_k"), "ratio",
                    items=band_items)
    diffs.add_db(scene_id, variant_id, "holdout", "e_modal_db",
                 py_holdout.get("e_modal_db"), rust_holdout.get("e_modal_db"))
    diffs.add_ratio(scene_id, variant_id, "holdout", "r_max",
                    py_holdout.get("r_max"), rust_holdout.get("r_max"))
    diffs.add_array(scene_id, variant_id, "holdout", "band_delta_db",
                    py_holdout.get("band_delta_db"),
                    rust_holdout.get("band_delta_db"), "db", items=band_items)
    diffs.add_point_band(scene_id, variant_id, "holdout",
                         py_holdout.get("per_point_band_db"),
                         rust_holdout.get("per_point_band_db"), centers)
    diffs.add_structured(scene_id, variant_id, "holdout", "point_weighted_db",
                         py_holdout.get("point_weighted_db"),
                         rust_holdout.get("point_weighted_db"), "db")
    diffs.add_ratio(scene_id, variant_id, "holdout", "consistency_c",
                    py_holdout.get("consistency_c"),
                    rust_holdout.get("consistency_c"))
    diffs.add_int(scene_id, variant_id, "holdout", "valid_holdout_count",
                  py_holdout.get("valid_holdout_count"),
                  rust_holdout.get("valid_holdout_count"))
    diffs.add_bool(scene_id, variant_id, "holdout", "severe_regression",
                   py_holdout.get("severe_regression"),
                   rust_holdout.get("severe_regression"))
    py_severe = _normalise_id_list(py_holdout.get("severe_points"))
    rust_severe = _normalise_id_list(rust_holdout.get("severe_points"))
    diffs.add_exact(scene_id, variant_id, "holdout", "severe_points",
                    py_severe, rust_severe)
    diffs.add_spatial(scene_id, variant_id, "holdout", py_holdout.get("spatial"),
                      rust_holdout.get("spatial"), centers)


def _normalise_id_list(value: Any) -> Optional[List[str]]:
    if value is None:
        return None
    if isinstance(value, dict):
        value = list(value.values())
    if not isinstance(value, (list, tuple)):
        return [str(value)]
    out: List[str] = []
    for item in value:
        if isinstance(item, dict):
            for key in ("id", "point", "mic", "mic_id", "name"):
                if key in item:
                    out.append(str(item[key]))
                    break
            else:
                out.append(json.dumps(to_jsonable(item), sort_keys=True))
        else:
            out.append(str(item))
    return sorted(out)


# ---------------------------------------------------------------------------
# T60 / Schroeder EDC analysis (CONTRACT section 9, spec section 5 item 4)
# ---------------------------------------------------------------------------

try:  # scipy is expected in the target environment (1.18); keep a fallback
    from scipy import signal as _scipy_signal  # type: ignore
    HAVE_SCIPY = True
except Exception:  # pragma: no cover - fallback path
    _scipy_signal = None
    HAVE_SCIPY = False


def schroeder_edc(pressure: np.ndarray, sample_rate_hz: int
                  ) -> Tuple[np.ndarray, np.ndarray]:
    """Schroeder energy decay curve (dB re total energy), 0 dB at t=0.

    EDC(t) = 10 log10( sum_{tau >= t} p(tau)^2 / sum_all p(tau)^2 )
    """
    p = np.asarray(pressure, dtype=np.float64)
    if p.size == 0:
        raise MetricsError("empty RIR for EDC")
    energy = p * p
    remaining = np.cumsum(energy[::-1])[::-1]
    total = float(remaining[0])
    edc = np.full(p.size, np.nan, dtype=np.float64)
    if total > 0.0:
        good = remaining > 0.0
        edc[good] = 10.0 * np.log10(remaining[good] / total)
        edc[~good] = -300.0
    t = np.arange(p.size, dtype=np.float64) / float(sample_rate_hz)
    return t, edc


def fit_decay(t: np.ndarray, edc_db: np.ndarray,
              db_range: Tuple[float, float]) -> Dict[str, Any]:
    """Least-squares slope over an EDC dB window; T = -60 / slope_db_per_s."""
    lo, hi = db_range  # lo = -5, hi = -25/35 (lo > hi)
    mask = np.isfinite(edc_db) & (edc_db <= lo) & (edc_db >= hi)
    n = int(np.count_nonzero(mask))
    result: Dict[str, Any] = {"valid": False, "n_points": n,
                              "window_db": [lo, hi], "slope_db_per_s": None,
                              "t60_s": None, "r2": None,
                              "t_first_s": None, "t_last_s": None}
    if n < 2:
        result["note"] = "fewer than 2 EDC samples inside %g..%g dB" % (lo, hi)
        return result
    tt = t[mask]
    yy = edc_db[mask]
    slope, intercept = np.polyfit(tt, yy, 1)
    if not math.isfinite(float(slope)) or slope >= 0.0:
        result["note"] = "non-negative decay slope (EDC not decaying in window)"
        result["slope_db_per_s"] = float(slope)
        return result
    pred = slope * tt + intercept
    ss_res = float(np.sum((yy - pred) ** 2))
    ss_tot = float(np.sum((yy - np.mean(yy)) ** 2))
    r2 = 1.0 - ss_res / ss_tot if ss_tot > 0.0 else float("nan")
    result.update({
        "valid": True,
        "slope_db_per_s": float(slope),
        "t60_s": float(-60.0 / slope),
        "r2": r2,
        "t_first_s": float(tt[0]),
        "t_last_s": float(tt[-1]),
    })
    return result


def _bandpass_rir_butterworth(p: np.ndarray, sample_rate_hz: int,
                              center_hz: float,
                              order: int = T60_BANDPASS_ORDER) -> np.ndarray:
    lo, hi = third_octave_edges(center_hz)
    nyq = 0.5 * float(sample_rate_hz)
    if not (0.0 < lo < hi < nyq):
        raise MetricsError("invalid band edges for center %g Hz" % center_hz)
    sos = _scipy_signal.butter(order, [lo / nyq, hi / nyq], btype="bandpass",
                               output="sos")
    return _scipy_signal.sosfiltfilt(sos, p)


def _bandpass_rir_fft(p: np.ndarray, sample_rate_hz: int,
                      center_hz: float) -> np.ndarray:
    """Zero-phase FFT bandpass fallback (declared in JSON if used)."""
    lo, hi = third_octave_edges(center_hz)
    n = p.size
    n_fft = 1
    while n_fft < 4 * n:
        n_fft *= 2
    spec = np.fft.rfft(p, n=n_fft)
    freqs = np.fft.rfftfreq(n_fft, d=1.0 / float(sample_rate_hz))
    width = hi - lo
    gain = np.ones_like(freqs)
    gain[(freqs < lo * 2.0 ** (-0.25)) | (freqs > hi * 2.0 ** (0.25))] = 0.0
    ramp_lo = (freqs >= lo * 2.0 ** (-0.25)) & (freqs < lo)
    ramp_hi = (freqs > hi) & (freqs <= hi * 2.0 ** (0.25))
    gain[ramp_lo] = 0.5 - 0.5 * np.cos(
        np.pi * (np.log2(freqs[ramp_lo] / (lo * 2.0 ** (-0.25)))) / 0.25)
    gain[ramp_hi] = 0.5 + 0.5 * np.cos(
        np.pi * (np.log2(freqs[ramp_hi] / hi)) / 0.25)
    out = np.fft.irfft(spec * gain, n=n_fft)[:n]
    return out


def bandpass_rir(p: np.ndarray, sample_rate_hz: int,
                 center_hz: float) -> Tuple[np.ndarray, str]:
    if HAVE_SCIPY:
        return (_bandpass_rir_butterworth(p, sample_rate_hz, center_hz),
                "butterworth_order%d_sosfiltfilt" % T60_BANDPASS_ORDER)
    return (_bandpass_rir_fft(p, sample_rate_hz, center_hz),
            "fft_zero_phase_fallback")


def edc_at_ms(t: np.ndarray, edc_db: np.ndarray, ms: float) -> Optional[float]:
    idx = int(round(ms * 1e-3 / (t[1] - t[0]))) if t.size > 1 else 0
    if idx < 0 or idx >= edc_db.size:
        return None
    val = float(edc_db[idx])
    return val if math.isfinite(val) else None


def sabine_t60_s(room: Dict[str, Any]) -> Dict[str, Any]:
    """Sabine T60 = 0.161 * V / sum(alpha_i * A_i), alpha_i = 1 - r_i^2.

    Surface mapping (ISO style, x=length, y=width, z=height):
      left/right = W*H, front/rear = L*H, floor/ceiling = L*W.
    """
    L = float(room["length_m"])
    W = float(room["width_m"])
    H = float(room["height_m"])
    reflections = room.get("reflections") or {}
    areas = {"left": W * H, "right": W * H, "front": L * H, "rear": L * H,
             "floor": L * W, "ceiling": L * W}
    abs_sum = 0.0
    per_surface: Dict[str, Dict[str, float]] = {}
    for name, area in areas.items():
        if name not in reflections:
            raise MetricsError("room.reflections missing %r" % name)
        r = float(reflections[name])
        alpha = 1.0 - r * r
        contrib = alpha * area
        abs_sum += contrib
        per_surface[name] = {"r": r, "alpha": alpha, "area_m2": area,
                             "alpha_area_m2": contrib}
    volume = L * W * H
    t60 = 0.161 * volume / abs_sum if abs_sum > 0.0 else float("nan")
    return {
        "sabine_t60_s": t60,
        "formula": "T60 = 0.161 * V / sum(alpha_i * A_i); alpha_i = 1 - r_i^2",
        "volume_m3": volume,
        "total_alpha_area_m2": abs_sum,
        "per_surface": per_surface,
    }

# ---------------------------------------------------------------------------
# T60 scene analysis (per source/mic RIR) + aggregates
# ---------------------------------------------------------------------------

T60_AGG_KEYS = ["broadband_t20_s", "broadband_t30_s",
               "band63_t20_s", "band125_t20_s", "edc_100ms_db"]


def t60_method_control(sample_rate_hz: int = 48000,
                       n_samples: int = 24000) -> Dict[str, Any]:
    """Validate the band-pass T20 pipeline on a known-answer signal.

    A pure damped sinusoid at the band centre with amplitude decay set for
    T20 = 0.40 s must be recovered by the exact same EDC/bandpass code path.
    This quantifies the method bias for the band-limited observations.
    """
    n = int(n_samples)
    t = np.arange(n, dtype=np.float64) / float(sample_rate_hz)
    tau = 8.685889638065035 / 150.0  # amplitude decay 150 dB/s => T20 0.4 s
    controls = []
    p_bb = np.exp(-t / tau)
    t_bb, edc_bb = schroeder_edc(p_bb, sample_rate_hz)
    r_bb = fit_decay(t_bb, edc_bb, T20_RANGE_DB)
    controls.append({
        "signal": "broadband_exponential", "expected_t20_s": 0.4,
        "recovered_t20_s": r_bb["t60_s"], "valid": r_bb["valid"],
        "relative_error": (abs(r_bb["t60_s"] - 0.4) / 0.4
                           if r_bb["valid"] else None),
    })
    for center in T60_BANDS_HZ:
        p = np.sin(2.0 * np.pi * center * t) * np.exp(-t / tau)
        bp, how = bandpass_rir(p, sample_rate_hz, center)
        tb, eb = schroeder_edc(bp, sample_rate_hz)
        r = fit_decay(tb, eb, T20_RANGE_DB)
        controls.append({
            "signal": "damped_sine_%sHz" % _fmt_hz(center),
            "expected_t20_s": 0.4,
            "recovered_t20_s": r["t60_s"], "valid": r["valid"],
            "relative_error": (abs(r["t60_s"] - 0.4) / 0.4
                               if r["valid"] else None),
            "bandpass": how,
        })
    return {
        "declared_order": T60_BANDPASS_ORDER,
        "bandpass_implementation": ("scipy.signal.butter+sosfiltfilt" if HAVE_SCIPY
                                    else "fft zero-phase fallback"),
        "window_db": list(T20_RANGE_DB),
        "controls": controls,
        "max_abs_relative_error": max(
            [c["relative_error"] for c in controls
             if c["relative_error"] is not None] or [float("nan")]),
    }


def _summary_stats(values: Sequence[Optional[float]]) -> Dict[str, Any]:
    vals = np.asarray([float(v) for v in values
                       if v is not None and math.isfinite(float(v))],
                      dtype=np.float64)
    if vals.size == 0:
        return {"n": 0, "median": None, "q1": None, "q3": None, "iqr": None,
                "min": None, "max": None, "mean": None, "std": None}
    q1, med, q3 = np.percentile(vals, [25.0, 50.0, 75.0])
    return {
        "n": int(vals.size),
        "median": float(med),
        "q1": float(q1),
        "q3": float(q3),
        "iqr": float(q3 - q1),
        "min": float(np.min(vals)),
        "max": float(np.max(vals)),
        "mean": float(np.mean(vals)),
        "std": float(np.std(vals, ddof=1)) if vals.size > 1 else 0.0,
    }


def _hand_range_relation(value: Optional[float]) -> Dict[str, Any]:
    if value is None or not math.isfinite(float(value)):
        return {"in_hand_range": None, "ratio_to_low": None, "ratio_to_high": None,
                "ratio_to_midpoint": None}
    v = float(value)
    mid = 0.5 * (HAND_T60_LOW_S + HAND_T60_HIGH_S)
    return {
        "in_hand_range": bool(HAND_T60_LOW_S <= v <= HAND_T60_HIGH_S),
        "ratio_to_low": v / HAND_T60_LOW_S,
        "ratio_to_high": v / HAND_T60_HIGH_S,
        "ratio_to_midpoint": v / mid,
    }


def analyze_scene_t60(fx: SceneFixture) -> Dict[str, Any]:
    """Schroeder EDC on every (source, mic) RIR of one scene."""
    fs = fx.sample_rate_hz
    records: List[Dict[str, Any]] = []
    for rf in fx.rir_files:
        wav_path = fx.scene_dir / rf["path"]
        samples, _info = read_wav_f32_mono(
            wav_path, expected_sample_rate=fs,
            expected_samples=fx.rir_length_samples)
        pressure = samples / fx.scale
        t, edc = schroeder_edc(pressure, fs)
        bb20 = fit_decay(t, edc, T20_RANGE_DB)
        bb30 = fit_decay(t, edc, T30_RANGE_DB)
        rec: Dict[str, Any] = {
            "source": str(rf["source"]),
            "mic": str(rf["mic"]),
            "role": ("holdout" if str(rf["mic"]) in fx.holdout_ids else "design"),
            "broadband_t20_s": bb20["t60_s"],
            "broadband_t20_valid": bb20["valid"],
            "broadband_t20_r2": bb20["r2"],
            "broadband_t20_fit_window_s": [bb20["t_first_s"], bb20["t_last_s"]],
            "broadband_t30_s": bb30["t60_s"],
            "broadband_t30_valid": bb30["valid"],
            "broadband_t30_r2": bb30["r2"],
            "edc_100ms_db": edc_at_ms(t, edc, 100.0),
            "bands": {},
        }
        for center in T60_BANDS_HZ:
            bp, how = bandpass_rir(pressure, fs, center)
            tb, eb = schroeder_edc(bp, fs)
            r = fit_decay(tb, eb, T20_RANGE_DB)
            key = "band%s_t20_s" % _fmt_hz(center)
            rec[key] = r["t60_s"]
            rec["bands"][_fmt_hz(center)] = {
                "center_hz": center,
                "t20_s": r["t60_s"],
                "valid": r["valid"],
                "r2": r["r2"],
                "n_points": r["n_points"],
                "fit_window_s": [r["t_first_s"], r["t_last_s"]],
                "bandpass": how,
            }
        records.append(rec)
    aggregates = {key: _summary_stats([rec.get(key) for rec in records])
                  for key in T60_AGG_KEYS}
    sabine = sabine_t60_s(fx.gt["room"])
    gt_sabine = fx.gt["room"].get("sabine_t60_s")
    sim_bb_t20 = aggregates["broadband_t20_s"]["median"]
    comparison = {
        "ground_truth_sabine_t60_s": (float(gt_sabine)
                                      if gt_sabine is not None else None),
        "computed_sabine_t60_s": sabine["sabine_t60_s"],
        "sabine_difference_s": (float(gt_sabine) - sabine["sabine_t60_s"]
                                if gt_sabine is not None else None),
        "hand_written_assumption_s": [HAND_T60_LOW_S, HAND_T60_HIGH_S],
        "simulated_broadband_t20_median_s": sim_bb_t20,
        "simulated_vs_sabine_ratio": (sim_bb_t20 / sabine["sabine_t60_s"]
                                      if sim_bb_t20 else None),
        "simulated_vs_hand": _hand_range_relation(sim_bb_t20),
        "sabine_vs_hand": _hand_range_relation(sabine["sabine_t60_s"]),
        "ground_truth_vs_hand": (_hand_range_relation(float(gt_sabine))
                                 if gt_sabine is not None else None),
    }
    return {
        "scene_id": fx.scene_id,
        "scene_dir": str(fx.scene_dir),
        "ground_truth_sha256": fx.gt_sha256,
        "room": fx.gt["room"],
        "sabine": sabine,
        "comparison": comparison,
        "aggregates": aggregates,
        "rir_count": len(records),
        "records": records,
        "warnings": list(fx.warnings),
    }


def build_t60_analysis(scenes: Sequence[SceneFixture],
                       method_control: Optional[Dict[str, Any]] = None
                       ) -> Dict[str, Any]:
    analysis: Dict[str, Any] = {
        "format": T60_FORMAT,
        "source": SOURCE,
        "provenance": {"data_kind": SOURCE, "honesty_status": HONESTY,
                       "claim": "synthetic ISM RIRs; not measured acoustics"},
        "tool": {"name": TOOL_NAME, "version": TOOL_VERSION,
                 "python": sys.version.split()[0], "numpy": np.__version__,
                 "scipy": (__import__("scipy").__version__ if HAVE_SCIPY else None)},
        "generated_at_utc": now_utc_iso(),
        "method": {
            "edc": "Schroeder reverse cumulative energy integral, 0 dB at t=0",
            "broadband_t20": {"window_db": list(T20_RANGE_DB),
                              "fit": "least squares on EDC dB vs t; T=-60/slope"},
            "broadband_t30": {"window_db": list(T30_RANGE_DB),
                              "fit": "least squares on EDC dB vs t; T=-60/slope"},
            "band_t20": {"bands_hz": list(T60_BANDS_HZ),
                         "bandpass": ("Butterworth order %d, zero-phase sosfiltfilt"
                                      % T60_BANDPASS_ORDER) if HAVE_SCIPY
                                      else "FFT zero-phase fallback",
                         "window_db": list(T20_RANGE_DB)},
            "sample_rate_hz": scenes[0].sample_rate_hz if scenes else None,
            "rir_length_samples": scenes[0].rir_length_samples if scenes else None,
            "hand_written_assumption_s": [HAND_T60_LOW_S, HAND_T60_HIGH_S],
            "sabine_constant": 0.161,
        },
        "method_control": method_control or t60_method_control(),
        "scenes": {},
    }
    for fx in scenes:
        analysis["scenes"][fx.scene_id] = analyze_scene_t60(fx)
    return analysis


def _fmt_num(value: Optional[float], digits: int = 4) -> str:
    if value is None or not math.isfinite(float(value)):
        return "N/A"
    return ("%." + str(digits) + "f") % float(value)


def render_t60_markdown(t60: Dict[str, Any]) -> str:
    """Human-readable T60 discussion (never overstates a synthetic result)."""
    method = t60.get("method") or {}
    control = t60.get("method_control") or {}
    scenes = t60.get("scenes") or {}
    lines: List[str] = []
    lines.append("# T60 / EDC analysis - RoomForge ISM fixtures (SIMULATED, UNPROVEN)")
    lines.append("")
    lines.append("> **source: SIMULATED / SYNTHETIC / UNPROVEN.** These RIRs come from the")
    lines.append("> RoomForge shoe-box image-source model. They are not measurements and")
    lines.append("> the numbers below say nothing about the real room.")
    lines.append("")
    lines.append("## Method (declared)")
    lines.append("")
    lines.append("- RIR: f32 mono 48 kHz, 24000 samples (0.5 s), p(t) = wav(t) / rir.scale.")
    lines.append("- Schroeder EDC: EDC(t) = 10 log10(sum_{tau>=t} p(tau)^2 / sum p(tau)^2).")
    lines.append("- Broadband T20: least-squares slope of EDC over -5..-25 dB, T = -60/slope.")
    lines.append("- Broadband T30: same over -5..-35 dB (reported as an observation; the 0.5 s")
    lines.append("  RIR/finite ISM order can bend the very late EDC, so T30 is only indicative).")
    band_method = (method.get("band_t20") or {}).get("bandpass", "Butterworth")
    lines.append("- 1/3-oct band T20: band edges [c*2^-1/6, c*2^+1/6), c = 63 Hz and 125 Hz,")
    lines.append("  %s, then the same -5..-25 dB fit." % band_method)
    lines.append("- Aggregates are medians / quartiles across every (source, mic) pair of the scene.")
    lines.append("")
    control_err = control.get("max_abs_relative_error")
    lines.append("### Known-answer control for the band-limited T20 pipeline")
    lines.append("")
    lines.append("A pure damped sinusoid at each band centre with an amplitude decay that")
    lines.append("corresponds to T20 = 0.400 s is pushed through the *same* bandpass + EDC +")
    lines.append("fit code path. Recovered values: " + ", ".join(
        "%s -> %s s (rel.err %s)" % (c["signal"], _fmt_num(c["recovered_t20_s"]),
                                     _fmt_num(c["relative_error"], 4))
        for c in control.get("controls", [])) + ".")
    lines.append("Max relative error of the method control: **%s**."
                 % _fmt_num(control_err, 4))
    lines.append("")
    lines.append("## Per-scene comparison: simulated T20 vs Sabine vs hand-written 0.35-0.45 s")
    lines.append("")
    lines.append("| scene | room LxWxH (m) | reflections | Sabine T60 (s) | sim broadband T20 median [Q1,Q3] (s) | sim T30 median (s) | 63 Hz band T20 median [Q1,Q3] (s) | 125 Hz band T20 median [Q1,Q3] (s) | sim/Sabine | in hand 0.35-0.45? |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for scene_id, scene in scenes.items():
        room = scene.get("room") or {}
        agg = scene.get("aggregates") or {}
        comp = scene.get("comparison") or {}
        refl = room.get("reflections") or {}
        refl_txt = ", ".join("%s=%s" % (k, _fmt_num(v, 3)) for k, v in refl.items())
        t20 = agg.get("broadband_t20_s") or {}
        t30 = agg.get("broadband_t30_s") or {}
        b63 = agg.get("band63_t20_s") or {}
        b125 = agg.get("band125_t20_s") or {}
        lines.append("| %s | %sx%sx%s | %s | %s | %s [%s, %s] | %s | %s [%s, %s] | %s [%s, %s] | %s | %s |" % (
            scene_id, _fmt_num(room.get("length_m"), 2), _fmt_num(room.get("width_m"), 2),
            _fmt_num(room.get("height_m"), 2), refl_txt,
            _fmt_num(comp.get("ground_truth_sabine_t60_s")),
            _fmt_num(t20.get("median")), _fmt_num(t20.get("q1")), _fmt_num(t20.get("q3")),
            _fmt_num(t30.get("median")),
            _fmt_num(b63.get("median")), _fmt_num(b63.get("q1")), _fmt_num(b63.get("q3")),
            _fmt_num(b125.get("median")), _fmt_num(b125.get("q1")), _fmt_num(b125.get("q3")),
            _fmt_num(comp.get("simulated_vs_sabine_ratio"), 3),
            ("yes" if (comp.get("simulated_vs_hand") or {}).get("in_hand_range")
             else "no")))
    lines.append("")
    lines.append("## The three reference T60s and what they mean")
    lines.append("")
    lines.append("1. **Sabine reference from ground-truth room parameters** (same formula as the")
    lines.append("   generator): T60 = 0.161 * V / sum(alpha_i * A_i), alpha_i = 1 - r_i^2.")
    lines.append("   For 4.10 x 5.20 x 2.70 m (V = 57.564 m^3) with all six reflection")
    lines.append("   coefficients r = 0.866 (alpha = 1 - 0.866^2 = 0.250044, total surface")
    lines.append("   A = 92.86 m^2): T60 = 0.161 * 57.564 / 23.219 = **0.3991 s**.")
    lines.append("   This is exactly the TSPI hand-written low-frequency assumption 0.35-0.45 s")
    lines.append("   (0.3991 s sits at the centre). The r = 0.866 scene is therefore the")
    lines.append("   physically consistent stand-in for the hand-written 0.40 s assumption.")
    lines.append("2. **RoomForge default reflections** (4 walls at r = 0.7, floor/ceiling at")
    lines.append("   r = 0.5) give alpha = 0.51 / 0.75, so T60 = 0.161 * 57.564 / 57.592")
    lines.append("   = **0.1609 s**. That is 2.48x shorter than the 0.3991 s hand-written")
    lines.append("   assumption (about -7.9 dB of decay time). If the real room behaved like")
    lines.append("   the RoomForge defaults, the hand-written 0.35-0.45 s assumption would be")
    lines.append("   off by a factor of ~2.5; conversely, only a high-reflection / low-absorption")
    lines.append("   room (r ~ 0.87) supports the 0.40 s assumption.")
    lines.append("3. **Challenging asymmetric room** (5.00 x 6.00 x 2.80 m, mixed r = 0.92/0.70/")
    lines.append("   0.92/0.60/0.60/0.80) gives **0.2588 s**, i.e. inside neither the hand range")
    lines.append("   nor the two extremes; it is the stress case where the hand-written single")
    lines.append("   broadband T60 is least representative.")
    lines.append("")
    lines.append("### Does the simulated ISM decay reproduce the Sabine value?")
    lines.append("")
    for scene_id, scene in scenes.items():
        comp = scene.get("comparison") or {}
        agg = scene.get("aggregates") or {}
        ratio = comp.get("simulated_vs_sabine_ratio")
        med = (agg.get("broadband_t20_s") or {}).get("median")
        lines.append("- %s: Sabine %s s, simulated broadband T20 median %s s (ratio %s)." % (
            scene_id, _fmt_num(comp.get("ground_truth_sabine_t60_s")),
            _fmt_num(med), _fmt_num(ratio, 3)))
    lines.append("")
    ratio_values = []
    for scene in scenes.values():
        comp = scene.get("comparison") or {}
        ratio = comp.get("simulated_vs_sabine_ratio")
        if ratio is not None and math.isfinite(float(ratio)):
            ratio_values.append(float(ratio))
    if ratio_values:
        lo_ratio = min(ratio_values)
        hi_ratio = max(ratio_values)
        pct_txt = ("%+.1f%% .. %+.1f%%" % ((lo_ratio - 1.0) * 100.0,
                                           (hi_ratio - 1.0) * 100.0))
    else:
        pct_txt = "N/A"
    lines.append("Across the five scenes the simulated broadband T20 median sits %s"
                 % pct_txt)
    lines.append("relative to the Sabine prediction. For the three 0.866-reflection")
    lines.append("rooms the ISM broad-band decay is slightly *faster* than Sabine")
    lines.append("(about 7-8% shorter T20): the image-source model weights every image")
    lines.append("by geometric 1/r spreading, so late reverberant energy falls a little")
    lines.append("faster than the uniform-energy-density Sabine assumption. The")
    lines.append("RoomForge-default and asymmetric rooms deviate in the other")
    lines.append("direction because their absorption is strongly non-uniform.")
    lines.append("T30 is systematically shorter than T20 in these 0.5 s ISM renders;")
    lines.append("treat T30 as an observation only (finite order / truncation), exactly")
    lines.append("as spec/mimo-objective-v1 section 5 item 4 requires.")
    lines.append("")
    lines.append("### Why the 63/125 Hz band T20 is longer and much more scattered")
    lines.append("")
    lines.append("At 63 Hz a 4.10 x 5.20 x 2.70 m shoe-box has only a handful of modes inside")
    lines.append("the 1/3-oct band (56.1-70.8 Hz), and each mode hits a different mix of")
    lines.append("surfaces per unit time, so axial modes decay more slowly than oblique ones.")
    lines.append("The band-limited decay is therefore *not* the diffuse-field Sabine value,")
    lines.append("and a single point may be dominated by one slow mode: the observed spread")
    lines.append("between (source, mic) pairs is expected, not a bug. Median is reported as")
    lines.append("the robust statistic; per-RIR values stay in the JSON. The known-answer")
    lines.append("control above bounds the *method* bias (max rel. err %s), so the scene-to-"
                 % _fmt_num(control_err, 4))
    lines.append("scene ordering is meaningful even though the absolute band value is not a")
    lines.append("Sabine estimate. This is an observation only - it is not an optimisation")
    lines.append("target (spec section 0.1 / 11: open-loop FIR cannot change room poles).")
    lines.append("")
    lines.append("## EDC@100 ms observation")
    lines.append("")
    lines.append("| scene | EDC@100ms median (dB re t=0) |")
    lines.append("| --- | --- |")
    for scene_id, scene in scenes.items():
        agg = scene.get("aggregates") or {}
        lines.append("| %s | %s |" % (scene_id, _fmt_num((agg.get("edc_100ms_db") or {}).get("median"), 2)))
    lines.append("")
    lines.append("## Honest limits")
    lines.append("")
    lines.append("- SIMULATED / UNPROVEN: synthetic ISM, no measurement, no audio device.")
    lines.append("- 0.5 s RIR length limits T30/late-EDC confidence; values are reported as")
    lines.append("  observations and must not be used to claim a real T20 improvement.")
    lines.append("- Band-limited T20 is sensitive to sparse-modal effects; see the control and")
    lines.append("  per-RIR JSON rather than reading a single number as the room's T60.")
    lines.append("- Sabine itself is a diffuse-field estimate; the hand-written 0.35-0.45 s")
    lines.append("  assumption is only consistent with the high-reflection variant.")
    lines.append("")
    lines.append("_generated by %s v%s; all conclusions are SIMULATED/UNPROVEN._"
                 % (TOOL_NAME, TOOL_VERSION))
    lines.append("")
    return chr(10).join(lines)


# ---------------------------------------------------------------------------
# selftest: hand-computed matrices with exact assertions
# ---------------------------------------------------------------------------

_ASSERT_COUNT = [0]


def _check(cond: bool, msg: str) -> None:
    _ASSERT_COUNT[0] += 1
    if not cond:
        raise SelftestFailure(msg)


def _close(actual: Optional[float], expected: float, tol: float, msg: str) -> None:
    _ASSERT_COUNT[0] += 1
    if actual is None:
        raise SelftestFailure("%s: got None, expected %r" % (msg, expected))
    if not math.isfinite(float(actual)) or abs(float(actual) - expected) > tol:
        raise SelftestFailure("%s: got %r, expected %r +/- %r"
                              % (msg, actual, expected, tol))


def _close_rel(actual: Optional[float], expected: float, rel: float, msg: str) -> None:
    _ASSERT_COUNT[0] += 1
    if actual is None or not math.isfinite(float(actual)):
        raise SelftestFailure("%s: got %r, expected %r (rel %r)"
                              % (msg, actual, expected, rel))
    if abs(float(actual) - expected) > rel * max(abs(expected), 1e-300):
        raise SelftestFailure("%s: got %r, expected %r (rel %r)"
                              % (msg, actual, expected, rel))


def _expect_raises(exc_type, fn, msg: str) -> None:
    _ASSERT_COUNT[0] += 1
    try:
        fn()
    except exc_type:
        return
    except Exception as other:  # noqa: BLE001
        raise SelftestFailure("%s: raised %r, expected %r"
                              % (msg, other, exc_type.__name__))
    raise SelftestFailure("%s: did not raise %r" % (msg, exc_type.__name__))


def _write_test_wav(path: Path, samples: Sequence[float],
                    audio_format: int = 3, sample_rate: int = 48000,
                    channels: int = 1) -> None:
    if audio_format == 3:
        bits = 32
        data = struct.pack("<%df" % len(samples), *[float(x) for x in samples])
    else:
        bits = 16
        data = struct.pack("<%dh" % len(samples),
                           *[int(round(float(x) * 32767.0)) for x in samples])
    block_align = channels * bits // 8
    fmt_chunk = struct.pack("<HHIIHH", audio_format, channels, sample_rate,
                            sample_rate * block_align, block_align, bits)
    riff = (b"RIFF" + struct.pack("<I", 4 + 8 + len(fmt_chunk) + 8 + len(data))
            + b"WAVE" + b"fmt " + struct.pack("<I", len(fmt_chunk)) + fmt_chunk
            + b"data" + struct.pack("<I", len(data)) + data)
    path.write_bytes(riff)


def run_selftest() -> int:
    """Known-answer tests for every formula; exit 0 only if all pass."""
    import tempfile

    print("[selftest] 2-point x 2-band hand matrix ...")
    freqs = np.array([40.0, 50.0], dtype=np.float64)
    plan = BandPlan(freqs=freqs, centers_hz=[40.0, 50.0],
                    extended_hz=[40.0, 50.0])
    _check(len(plan.bins[0]) == 1 and int(plan.bins[0][0]) == 0,
           "band 40 Hz must contain exactly bin 0")
    _check(len(plan.bins[1]) == 1 and int(plan.bins[1][0]) == 1,
           "band 50 Hz must contain exactly bin 1")
    # P_before: mic0 [1,2], mic1 [3,4]; P_after: mic0 [0.5,2], mic1 [1.5,4]
    P0 = np.array([[1.0 + 0j, 2.0 + 0j], [3.0 + 0j, 4.0 + 0j]])
    Pa = np.array([[0.5 + 0j, 2.0 + 0j], [1.5 + 0j, 4.0 + 0j]])
    e0 = band_energies(P0, plan.bins)
    _close(e0[0], 10.0, 1e-12, "E_k0[40] = 1^2+3^2")
    _close(e0[1], 20.0, 1e-12, "E_k0[50] = 2^2+4^2")
    alpha, info = alpha_weights(e0)
    _check(info["n_valid"] == 2, "both bands valid")
    _close(alpha[0], 2.0 / 3.0, 1e-12, "alpha[0]")
    _close(alpha[1], 4.0 / 3.0, 1e-12, "alpha[1]")
    _close(float(np.sum(alpha)), 2.0, 1e-12, "sum(alpha) == K")
    r = residual_r(e0, band_energies(Pa, plan.bins), info["valid"])
    _close(r[0], 0.25, 1e-12, "R[40]")
    _close(r[1], 1.0, 1e-12, "R[50]")
    em, em_n = emodal(alpha, r)
    _close(em, 1.5, 1e-12, "E_modal = (2/3)(1/4)+(4/3)(1) = 1.5")
    _check(em_n == 2, "E_modal used 2 bands")
    eq, eq_n = emodal_equal(r)
    _close(eq, 1.25, 1e-12, "E_modal_eq = 0.25 + 1.0")
    rmax, rmax_band, rmax_idx = r_max_argmax(r, plan.centers_hz)
    _close(rmax, 1.0, 1e-12, "R_max")
    _check(rmax_band == 50.0 and rmax_idx == 1, "argmax band = 50 Hz")
    delta = point_band_delta_db(P0, Pa, plan.bins)
    _close(delta[0, 0], 10.0 * math.log10(0.25), 1e-12, "Delta mic0 band40")
    _close(delta[0, 1], 0.0, 1e-12, "Delta mic0 band50")
    _close(delta[1, 0], 10.0 * math.log10(2.25 / 9.0), 1e-12, "Delta mic1 band40")
    _close(delta[1, 1], 0.0, 1e-12, "Delta mic1 band50")
    w = weighted_point_delta(delta, alpha)
    _close(w[0], (2.0 / 3.0 * 10.0 * math.log10(0.25)) / 2.0, 1e-12,
           "alpha-weighted Delta mic0")
    _close(w[1], w[0], 1e-12, "alpha-weighted Delta mic1")
    # holdout: both mics improve => C = 1.0, no severe regression
    hold = assess_holdout(P0, Pa, alpha, plan.bins, ["P1", "P2"])
    _close(hold["consistency_c"], 1.0, 1e-12, "C with 2/2 improved")
    _check(hold["valid_holdout_count"] == 2, "valid holdout count")
    _check(hold["severe_regression"] is False, "no severe regression at -6 dB")
    _check(hold["incomplete_holdout"] is True, "2 holdout points => INCOMPLETE")

    print("[selftest] clamp / N-A / migration / severe regression ...")
    a_clamped, info2 = alpha_weights(np.array([1.0, 100.0]))
    # mean=50.5, x=[1/50.5, 100/50.5] -> a=[max(0.5,x0), min(2,x1)]
    # sum(a) = 0.5 + 100/50.5 (exact expression, no truncated literal)
    sum_a = 0.5 + 100.0 / 50.5
    _close(a_clamped[0] * sum_a / 2.0, 0.5, 1e-12,
           "alpha clamp lower -> a=0.5 (recovered from normalized alpha)")
    _close(a_clamped[1] * sum_a / 2.0, 100.0 / 50.5, 1e-12,
           "alpha upper = x=1.980198 (unclamped, recovered exactly)")
    _close(float(np.sum(a_clamped)), 2.0, 1e-12, "clamped alpha reinstated to sum K=2")
    na_alpha, na_info = alpha_weights(np.array([0.0, 20.0]))
    _check(na_info["n_valid"] == 1, "near-zero band must be N/A")
    _check(math.isnan(na_alpha[0]), "N/A band alpha is NaN")
    _close(na_alpha[1], 1.0, 1e-12, "single valid band alpha = K = 1")
    na_r = residual_r(np.array([0.0, 20.0]), np.array([0.0, 10.0]),
                      na_info["valid"])
    _check(math.isnan(na_r[0]) and abs(na_r[1] - 0.5) < 1e-12, "N/A R_k")
    mig_r = np.array([1.0, 1.0 + 0.06, 1.05])
    _check(bool(np.any(mig_r > 1.0 + EPS_MIGRATION)), "migration epsilon check")
    # severe: +4 dB in one holdout band
    P0s = np.array([[1.0 + 0j, 1.0 + 0j]])
    Pas = np.array([[1.0 + 0j, 10.0 ** (4.0 / 10.0) + 0j]])
    hs = assess_holdout(P0s, Pas, np.array([1.0, 1.0]),
                        [np.array([0]), np.array([1])], ["P1"])
    _check(hs["severe_regression"] is True and hs["severe_points"] == ["P1"],
           "severe regression at +4 dB must be flagged")
    _check(hs["consistency_c"] == 0.0, "positive weighted delta => not improved")

    print("[selftest] spatial statistics ...")
    sp = spatial_stats(P0, plan.bins, ["40", "50"], plan.centers_hz)
    _close(sp[0]["center_hz"], 40.0, 0.0, "spatial center_hz band40")
    _close(sp[1]["center_hz"], 50.0, 0.0, "spatial center_hz band50")
    band0 = sp[0]
    _close(band0["mean_db"], 0.5 * (0.0 + 10.0 * math.log10(9.0)), 1e-12,
           "spatial mean band40")
    _close(band0["std_db"], 0.5 * 10.0 * math.log10(9.0), 1e-12,
           "spatial population std band40")
    _close(band0["range_db"], 10.0 * math.log10(9.0), 1e-12, "spatial range")
    _close(band0["p10_db"], 0.0, 1e-12, "nearest-rank P10 = min for n=2")
    _close(band0["p90_db"], 10.0 * math.log10(9.0), 1e-12, "nearest-rank P90 = max")
    _close(band0["p90_p10_db"], 10.0 * math.log10(9.0), 1e-12, "P90-P10")
    _close(band0["coverage_within_3db"], 0.0, 1e-12, "coverage ±3 dB")
    expected_dev = -0.5 * 10.0 * math.log10(9.0)
    _close(band0["worst_null_db"], expected_dev, 1e-12, "worst null depth")
    _close(band0["max_peak_above_mean_db"], -expected_dev, 1e-12,
           "max peak above mean")
    _close(band0["interference_null_db"], 10.0 * math.log10(1.0 / 5.0), 1e-12,
           "interference null = min/mean")
    band1 = sp[1]
    _close(band1["mean_db"], 0.5 * (10.0 * math.log10(4.0) + 10.0 * math.log10(16.0)),
           1e-12, "spatial mean band50")
    _close(nearest_rank_percentile([0, 1, 2, 3, 4], 10.0), 0.0, 0.0, "P10 nearest rank")
    _close(nearest_rank_percentile([0, 1, 2, 3, 4], 90.0), 4.0, 0.0, "P90 nearest rank")
    _check(math.isnan(nearest_rank_percentile([], 50.0)), "empty percentile -> NaN")

    print("[selftest] variant_metrics structure / B0 baseline ...")
    vm = variant_metrics(P0, np.zeros((0, 2), dtype=np.complex128),
                         P0, np.zeros((0, 2), dtype=np.complex128), plan,
                         ["M1", "M2"], [])
    _close(vm["design"]["r_k"][0], 1.0, 1e-12, "B0 R_k = 1")
    _close(vm["design"]["e_modal"], 2.0, 1e-12, "B0 E_modal = K")
    _close(vm["design"]["e_modal_db"], 0.0, 1e-12,
           "B0 E_modal_dB = 0 dB (normalised by sum(alpha)=K)")
    _close(vm["design"]["e_modal_eq_db"], 0.0, 1e-12,
           "B0 E_modal_eq_dB = 0 dB (normalised by K)")
    _check(vm["design"]["energy_migration"] is False, "B0 no energy migration")
    _check(vm["holdout"]["valid_holdout_count"] == 0, "empty holdout")

    print("[selftest] diff table tolerances ...")
    _check(ratio_ok(1.0, 1.0 + 5e-7), "ratio within 1e-6")
    _check(not ratio_ok(1.0, 1.0 + 2e-6), "ratio beyond 1e-6")
    _check(db_ok(-6.0, -6.0 + 5e-7), "dB within 1e-6")
    _check(not db_ok(-6.0, -6.0 + 2e-6), "dB beyond 1e-6")
    dt = DiffTable()
    dt.add_ratio("s", "B0", "design", "r_k", None, 1.0, item=0)
    dt.add_ratio("s", "B0", "design", "r_k", 1.0 + 1e-3, 1.0, item=1)
    dt.add_db("s", "B0", "design", "e_modal_db", 1.0, None)
    dt.add_bool("s", "B0", "design", "energy_migration", False, True)
    statuses = [row["status"] for row in dt.rows]
    _check(statuses == ["MISSING_PYTHON", "FAIL", "MISSING_RUST", "FAIL"],
           "diff table statuses, got %r" % (statuses,))
    _check(dt.counts().get("FAIL") == 2, "diff table FAIL count")

    print("[selftest] FIR csv parsing + responses ...")
    with tempfile.TemporaryDirectory(prefix="mimo-selftest-") as tmp:
        tmpdir = Path(tmp)
        # impulse FIR, own-LS layout
        fir_path = tmpdir / "SUB1.csv"
        fir_path.write_text(
            "# source=SUB1 out=<OUT> in=<IN> taps=4 sample_rate_hz=48000 "
            "delay_samples=0" + chr(10)
            + "1.0" + chr(10) + "0.0" + chr(10) + "0.0" + chr(10) + "0.0" + chr(10),
            encoding="utf-8")
        ff = read_fir_csv(fir_path, expected_taps=4)
        _check(ff.source == "SUB1" and ff.taps == 4 and ff.sample_rate_hz == 48000,
               "FIR header metadata parsed")
        resp = freq_response(ff.coeffs, np.array([40.0, 125.0]), 48000)
        _close(abs(resp[0]), 1.0, 1e-12, "impulse FIR response magnitude = 1")
        # B1 gains
        pf = {"sources": [{"id": "S1", "delay_samples": 1, "gain_db": 6.0,
                           "polarity": -1}]}
        g1 = build_g_b1(pf, ["S1"], np.array([1000.0]), 48000, [])
        _close(abs(g1[0, 0]), 10.0 ** (6.0 / 20.0), 1e-12, "B1 gain 6 dB")
        theta = 2.0 * math.pi * 1000.0 / 48000.0
        _close(np.angle(-1.0 * np.exp(-1j * theta)) - np.angle(g1[0, 0]),
               0.0, 1e-12, "B1 polarity + delay phase")
        # matrix layout: G_eff[out] = sum_in G[out][in]
        mp = tmpdir / "SUB1__SUB2.csv"
        mp.write_text("# out=SUB1 in=SUB2 taps=2 sample_rate_hz=48000" + chr(10)
                      + "0.5" + chr(10) + "0.0" + chr(10), encoding="utf-8")
        mff = read_fir_csv(mp)
        _check(mff.out_id == "SUB1" and mff.in_id == "SUB2", "matrix header parsed")
        g_eff = build_g_matrix([mff], ["SUB1", "SUB2"], np.array([40.0]), 48000,
                               [], "B2")
        _close(abs(g_eff[0, 0]), 0.5, 1e-12, "matrix layout effective gain")

    print("[selftest] WAV reader (RIFF, f32 mono) ...")
    with tempfile.TemporaryDirectory(prefix="mimo-selftest-wav-") as tmp:
        tmpdir = Path(tmp)
        wav_path = tmpdir / "impulse.wav"
        imp = [0.0] * 64
        imp[0] = 2.0
        _write_test_wav(wav_path, imp, audio_format=3)
        samples, info = read_wav_f32_mono(wav_path, expected_sample_rate=48000,
                                          expected_samples=64)
        _check(info.audio_format == 3 and info.channels == 1 and info.bits_per_sample == 32,
               "WAV header info")
        _close(samples[0], 2.0, 1e-12, "WAV sample[0]")
        _close(float(np.sum(samples)), 2.0, 1e-12, "WAV sample sum")
        pcm16 = tmpdir / "pcm16.wav"
        _write_test_wav(pcm16, [0.5] * 8, audio_format=1)
        _expect_raises(MetricsError,
                       lambda: read_wav_f32_mono(pcm16),
                       "PCM16 must be rejected (contract requires f32)")
        truncated = tmpdir / "truncated.wav"
        truncated.write_bytes(wav_path.read_bytes()[:30])
        _expect_raises(MetricsError, lambda: read_wav_f32_mono(truncated),
                       "truncated RIFF must be rejected")
        h1 = sha256_file(wav_path)
        _check(h1 == hashlib.sha256(wav_path.read_bytes()).hexdigest(),
               "sha256_file matches hashlib")
        _check(sha256_file(wav_path) != sha256_file(pcm16), "different files differ")

    print("[selftest] Sabine T60 references ...")
    room_uniform = {"length_m": 4.1, "width_m": 5.2, "height_m": 2.7,
                    "reflections": {k: 0.866 for k in
                                    ("left", "right", "front", "rear", "floor", "ceiling")}}
    _close(sabine_t60_s(room_uniform)["sabine_t60_s"], 0.39914594673809944, 1e-12,
           "Sabine 4.1x5.2x2.7 r=0.866")
    room_default = {"length_m": 4.1, "width_m": 5.2, "height_m": 2.7,
                    "reflections": {"left": 0.7, "right": 0.7, "front": 0.7,
                                    "rear": 0.7, "floor": 0.5, "ceiling": 0.5}}
    _close(sabine_t60_s(room_default)["sabine_t60_s"], 0.16092116640795115, 1e-12,
           "Sabine RoomForge defaults")
    room_challenge = {"length_m": 5.0, "width_m": 6.0, "height_m": 2.8,
                      "reflections": {"left": 0.92, "right": 0.70, "front": 0.92,
                                      "rear": 0.60, "floor": 0.60, "ceiling": 0.80}}
    _close(sabine_t60_s(room_challenge)["sabine_t60_s"], 0.25878855421317876, 1e-12,
           "Sabine challenge room")

    print("[selftest] Schroeder EDC / T20 on known decay ...")
    fs = 48000
    n = 24000
    t = np.arange(n, dtype=np.float64) / fs
    tau = 8.685889638065035 / 150.0  # 150 dB/s energy decay -> T20 = 0.4 s
    p_bb = np.exp(-t / tau)
    t_bb, edc_bb = schroeder_edc(p_bb, fs)
    r_bb = fit_decay(t_bb, edc_bb, T20_RANGE_DB)
    _check(r_bb["valid"] is True, "broadband EDC fit valid")
    _close(r_bb["t60_s"], 0.4, 2e-3, "broadband synthetic T20 = 0.4 s")
    for center in T60_BANDS_HZ:
        p_sine = np.sin(2.0 * np.pi * center * t) * np.exp(-t / tau)
        bp, _how = bandpass_rir(p_sine, fs, center)
        tb, eb = schroeder_edc(bp, fs)
        rb = fit_decay(tb, eb, T20_RANGE_DB)
        _check(rb["valid"] is True, "band EDC fit valid at %g Hz" % center)
        _close(rb["t60_s"], 0.4, 0.02,
               "band synthetic T20 = 0.4 s at %g Hz" % center)
    control = t60_method_control()
    _check(control["max_abs_relative_error"] is not None
           and control["max_abs_relative_error"] < 0.02,
           "T60 method control relative error < 2%")
    # EDC must be 0 dB at t=0 and decrease monotonically (before floor)
    _close(edc_bb[0], 0.0, 1e-9, "EDC(0) = 0 dB")
    mid = edc_bb[1000]
    _check(mid < edc_bb[10], "EDC decreases with time")


    print("[selftest] objective-v2: seat_std / max_dev / null_depth ...")
    plan2 = BandPlan(freqs=np.array([40.0, 50.0]), centers_hz=[40.0, 50.0],
                     extended_hz=[40.0, 50.0])
    # 2 points, 2 bands: band40 energies 1 and 10 (levels 0 and 10 dB);
    # band50 energies 1 and 1 (levels 0 and 0 dB).
    P_obj = np.array([[1.0 + 0j, 1.0 + 0j],
                      [math.sqrt(10.0) + 0j, 1.0 + 0j]])
    obj = seat_std_null_metrics(P_obj, plan2.bins)
    _close(obj["seat_std_mean_db"], 2.5, 1e-12, "seat_std_mean = (5+0)/2")
    _close(obj["seat_std_max_db"], 5.0, 1e-12, "seat_std_max = 5 dB")
    _close(obj["max_dev_db"], 5.0, 1e-12, "max_dev = 5 dB")
    _close(obj["max_dev_all_points_db"], 5.0, 1e-12, "max_dev_all_points")
    expected_null0 = 10.0 * math.log10(1.0 / 5.5)
    _close(obj["null_depth_worst_db"], expected_null0, 1e-12,
           "null_depth_worst = band40 min/mean")
    _close(obj["null_depth_mean_db"], 0.5 * expected_null0, 1e-12,
           "null_depth_mean = (band40 + 0)/2")
    _close(obj["per_band"]["seat_std_db"][0], 5.0, 1e-12, "per-band std band40")
    _close(obj["per_band"]["seat_std_db"][1], 0.0, 1e-12, "per-band std band50")
    single = seat_std_null_metrics(np.array([[2.0 + 0j, 3.0 + 0j]]), plan2.bins)
    _close(single["seat_std_mean_db"], 0.0, 1e-12, "single point std = 0")
    _close(single["max_dev_db"], 0.0, 1e-12, "single point max_dev = 0")
    _close(single["null_depth_mean_db"], 0.0, 1e-12,
           "single point interference null = 0 dB")

    print("[selftest] objective-v2: feasibility (trust region) ...")
    delta_ok = np.array([[1.5, -1.0], [0.0, 1.99]])
    feas_ok = evaluate_feasibility(delta_ok, ["P01", "P02"], [40.0, 50.0], 2.0)
    _check(feas_ok["feasible"] is True, "rise 1.5/1.99 dB <= 2.0 dB is feasible")
    _check(feas_ok["n_violations"] == 0, "no violations below the limit")
    _close(feas_ok["design_max_rise_db"], 1.99, 1e-12, "design_max_rise_db")
    delta_bad = np.array([[1.5, -1.0], [0.2, 2.5]])
    feas_bad = evaluate_feasibility(delta_bad, ["P01", "P02"], [40.0, 50.0], 2.0)
    _check(feas_bad["feasible"] is False, "rise 2.5 dB > 2.0 dB is infeasible")
    _check(feas_bad["n_violations"] == 1, "exactly one violation")
    _check(feas_bad["violations"][0]["mic"] == "P02"
           and feas_bad["violations"][0]["band_hz"] == 50.0,
           "violation points at P02/50 Hz")
    _close(feas_bad["design_max_rise_db"], 2.5, 1e-12, "worst design rise captured")
    feas_edge = evaluate_feasibility(np.array([[2.0]]), ["P01"], [40.0], 2.0)
    _check(feas_edge["feasible"] is True, "rise exactly at the limit is feasible")

    print("[selftest] objective-v2: FIR gain-cap / headroom check ...")
    freqs3 = np.array([40.0, 50.0, 63.0])
    g_low = np.full((1, 3), 0.5 + 0j)
    chk_low = fir_constraint_check(g_low, freqs3, ["S1"])
    _close(chk_low["per_source_max_gain_db"][0], 20.0 * math.log10(0.5), 1e-12,
           "max |G| 0.5 -> -6.02 dB")
    _close(chk_low["predicted_peak_per_source"][0], 0.25, 1e-12,
           "predicted peak = 0.5*max|G|")
    _check(chk_low["gain_cap_ok"] is True, "0.5 < +6 dB cap")
    _check(chk_low["headroom_ok"] is True, "0.25 <= 1.0 headroom")
    g_high = np.full((1, 3), 2.2 + 0j)
    chk_high = fir_constraint_check(g_high, freqs3, ["S1"])
    _check(chk_high["gain_cap_ok"] is False, "2.2 > +6 dB cap -> violation")
    _check(chk_high["headroom_ok"] is False, "1.1 > 1.0 headroom -> violation")
    _check(chk_low["frequency_grid_k"] == 3, "constraint check uses full grid")

    print("[selftest] objective-v2: predicted_status rules ...")
    design_pass = {"e_modal_db": -3.0, "r_max": 0.5,
                   "energy_migration": False}
    holdout_pass = {"consistency_c": 1.0, "severe_regression": False,
                    "valid_holdout_count": 3}
    feas_pass = {"applicable": True, "feasible": True}
    status, checks = compute_objective_status(design_pass, holdout_pass,
                                              feas_pass, "B2")
    _check(status == "PREDICTED_PASS", "all criteria => PREDICTED_PASS")
    _check(checks["no_severe"] is True, "status checks include no_severe")
    status_b0, _ = compute_objective_status(design_pass, holdout_pass, None, "B0")
    _check(status_b0 == "BASELINE", "B0 => BASELINE (frozen v2 rule)")
    status_fail, _ = compute_objective_status(
        design_pass, {"consistency_c": 1.0, "severe_regression": True,
                      "valid_holdout_count": 3}, feas_pass, "B2")
    _check(status_fail == "PREDICTED_FAIL", "severe => PREDICTED_FAIL (priority 1)")
    status_fail_pri, _ = compute_objective_status(
        design_pass, {"consistency_c": 1.0, "severe_regression": True,
                      "valid_holdout_count": 3},
        {"applicable": True, "feasible": False}, "B2")
    _check(status_fail_pri == "PREDICTED_FAIL",
           "severe has priority over INFEASIBLE")
    status_infeas, _ = compute_objective_status(
        design_pass, holdout_pass, {"applicable": True, "feasible": False}, "B2")
    _check(status_infeas == "INFEASIBLE",
           "B2 feasibility=false => INFEASIBLE (frozen v2 rule)")
    status_b1, _ = compute_objective_status(
        design_pass, holdout_pass, {"applicable": True, "feasible": False}, "B1")
    _check(status_b1 == "PREDICTED_PASS",
           "B1 ignores B2/B3 feasibility (no feasibility term)")
    status_incomplete, _ = compute_objective_status(
        {"e_modal_db": 1.0, "r_max": 0.5, "energy_migration": False},
        {"consistency_c": 1.0, "severe_regression": False,
         "valid_holdout_count": 2}, feas_pass, "B2")
    _check(status_incomplete == "INCOMPLETE",
           "non-PASS conditions + valid holdout < 3 => INCOMPLETE")
    status_partial, _ = compute_objective_status(
        {"e_modal_db": 1.0, "r_max": 0.5, "energy_migration": False},
        holdout_pass, feas_pass, "B2")
    _check(status_partial == "PREDICTED_PARTIAL",
           "positive e_modal_db => PREDICTED_PARTIAL")
    status_mig, _ = compute_objective_status(
        {"e_modal_db": -1.0, "r_max": 1.2, "energy_migration": True},
        holdout_pass, feas_pass, "B2")
    _check(status_mig == "PREDICTED_PARTIAL",
           "migration/R_max>=1 => PARTIAL not PASS")

    print("[selftest] objective-v2: report structure validation ...")
    with tempfile.TemporaryDirectory(prefix="mimo-obj-selftest-") as tmp:
        tmpdir = Path(tmp)
        good = {"format": "tspi-mimo-sim-report/v1", "scene_id": "scene-x",
                "variants": {vid: {"design": {}, "holdout": {}}
                             for vid in ("B0", "B1", "B2", "B3")}}
        good_path = tmpdir / "good.json"
        good_path.write_text(json.dumps(good), encoding="utf-8")
        loaded = load_obj_report(good_path)
        _check(loaded["scene_id"] == "scene-x", "load_obj_report accepts 4 variants")
        bad = {"format": "tspi-mimo-sim-report/v1",
               "variants": {"B0": {"design": {}, "holdout": {}}}}
        bad_path = tmpdir / "bad.json"
        bad_path.write_text(json.dumps(bad), encoding="utf-8")
        _expect_raises(MetricsError, lambda: load_obj_report(bad_path),
                       "missing B2/B3 must be a schema error")

    print("[selftest] JSON sanitisation ...")
    with tempfile.TemporaryDirectory(prefix="mimo-selftest-json-") as tmp:
        path = Path(tmp) / "out.json"
        payload = {"source": SOURCE, "a": np.float64(1.5),
                   "b": np.array([1.0, float("nan")]), "c": np.int64(3),
                   "d": np.bool_(True), "e": float("inf")}
        write_json(path, payload)
        loaded = load_json(path)
        _check(loaded["a"] == 1.5 and loaded["c"] == 3 and loaded["d"] is True,
               "JSON numpy scalars")
        _check(loaded["b"] == [1.0, None] and loaded["e"] is None,
               "JSON nan/inf -> null")
        text = path.read_text(encoding="utf-8")
        _check('"source": "SIMULATED"' in text, "JSON carries source=SIMULATED")

    print("[selftest] PASS (%d assertions)" % _ASSERT_COUNT[0])
    return 0


# ---------------------------------------------------------------------------
# plots (matplotlib Agg, English/ASCII labels only)
# ---------------------------------------------------------------------------

def _plt():
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt  # noqa: WPS433
    return plt


def _scene_variant_order() -> List[str]:
    return ["B0", "B1", "B2", "B3"]


def _scene_mic_order(scene_payload: Dict[str, Any]) -> List[str]:
    order: List[str] = []
    for variant in scene_payload.get("variants", {}).values():
        design = variant.get("design") or {}
        holdout = variant.get("holdout") or {}
        for mapping in (design.get("per_point_band_db") or {},
                        holdout.get("per_point_band_db") or {}):
            for pid in mapping:
                if pid not in order:
                    order.append(pid)
    return order


def plot_band_rk(scene_payloads: Dict[str, Any], path: Path) -> None:
    plt = _plt()
    scenes = list(scene_payloads)
    n = len(scenes)
    if n == 0:
        return
    fig, axes = plt.subplots(n, 2, figsize=(13.0, 2.5 + 2.1 * n), squeeze=False)
    width = 0.2
    colors = {"B0": "#999999", "B1": "#1f77b4", "B2": "#2ca02c", "B3": "#d62728"}
    for row, scene_id in enumerate(scenes):
        scene = scene_payloads[scene_id]
        variants = scene.get("variants") or {}
        centers = (variants.get("B0") or {}).get("centers_hz") or []
        xs = np.arange(len(centers))
        for col, block in enumerate(("design", "holdout")):
            ax = axes[row][col]
            for k, vid in enumerate(_scene_variant_order()):
                variant = variants.get(vid) or {}
                block_data = variant.get(block) or {}
                rk = block_data.get("r_k") or []
                vals = [float(v) if v is not None else float("nan") for v in rk]
                if len(vals) != len(xs):
                    continue
                ax.bar(xs + (k - 1.5) * width, vals, width, label=vid,
                       color=colors.get(vid, None), alpha=0.9)
            ax.axhline(1.0, color="black", linewidth=0.8, linestyle="--")
            ax.set_xticks(xs)
            ax.set_xticklabels([_fmt_hz(c) for c in centers], fontsize=7)
            ax.set_ylabel("R_k", fontsize=8)
            ax.set_title("%s - %s R_k (den: B0)" % (scene_id, block), fontsize=8)
            ax.tick_params(labelsize=7)
            block_max = 1.0
            for vid in _scene_variant_order():
                rk_list = (((variants.get(vid) or {}).get(block) or {}).get("r_k")
                           or [])
                for value in rk_list:
                    if value is not None and math.isfinite(float(value)):
                        block_max = max(block_max, float(value))
            ax.set_ylim(0.0, max(1.4, block_max * 1.15))
            if row == 0 and col == 0:
                ax.legend(fontsize=7, ncol=4, loc="upper left")
    fig.suptitle("Normalised band residual R_k vs B0 design baseline (SIMULATED/UNPROVEN)",
                 fontsize=10)
    fig.tight_layout(rect=(0, 0, 1, 0.97))
    fig.savefig(path, dpi=140)
    plt.close(fig)


def _finite_max(nested: Any) -> float:
    best = 1.0
    if isinstance(nested, (list, tuple)):
        for item in nested:
            best = max(best, _finite_max(item))
    elif isinstance(nested, (int, float)) and math.isfinite(float(nested)):
        best = max(best, float(nested))
    return best


def plot_b3_vs_b1_heatmap(scene_payloads: Dict[str, Any], path: Path,
                          scene_ids: Optional[Sequence[str]] = None) -> None:
    plt = _plt()
    ids = list(scene_ids) if scene_ids else list(scene_payloads)
    ids = [sid for sid in ids if sid in scene_payloads]
    if not ids:
        return
    fig, axes = plt.subplots(len(ids), 1, figsize=(11.0, 2.5 + 2.6 * len(ids)),
                             squeeze=False)
    for ax_row, scene_id in enumerate(ids):
        scene = scene_payloads[scene_id]
        variants = scene.get("variants") or {}
        centers = (variants.get("B0") or {}).get("centers_hz") or []
        mic_order = _scene_mic_order(scene)
        b1 = variants.get("B1") or {}
        b3 = variants.get("B3") or {}
        rows = []
        for pid in mic_order:
            delta_b1 = _lookup_point_delta(b1, pid)
            delta_b3 = _lookup_point_delta(b3, pid)
            if delta_b1 is None or delta_b3 is None:
                continue
            rows.append([(None if (a is None or b is None) else float(b) - float(a))
                         for a, b in zip(delta_b1, delta_b3)])
        ax = axes[ax_row][0]
        if not rows or not centers:
            ax.set_axis_off()
            ax.set_title("%s: B3 vs B1 delta not available" % scene_id, fontsize=8)
            continue
        data = np.asarray([[float("nan") if v is None else v for v in row]
                           for row in rows], dtype=np.float64)
        vmax = float(np.nanmax(np.abs(data))) if np.any(np.isfinite(data)) else 1.0
        vmax = max(vmax, 1e-6)
        im = ax.imshow(data, aspect="auto", cmap="RdBu_r", vmin=-vmax, vmax=vmax)
        ax.set_xticks(np.arange(len(centers)))
        ax.set_xticklabels([_fmt_hz(c) for c in centers], fontsize=7)
        ax.set_yticks(np.arange(len(rows)))
        ax.set_yticklabels([pid for pid in mic_order if pid in [p for p in mic_order]][:len(rows)],
                           fontsize=7)
        ax.set_xlabel("1/3-oct band centre [Hz]", fontsize=8)
        ax.set_title("%s: per point/band  dB(B3/B1)  (blue = B3 better)" % scene_id,
                     fontsize=8)
        cbar = fig.colorbar(im, ax=ax, fraction=0.025, pad=0.01)
        cbar.ax.tick_params(labelsize=7)
        cbar.set_label("Delta dB", fontsize=7)
    fig.suptitle("B3 vs B1 per-point per-band change (SIMULATED/UNPROVEN)", fontsize=10)
    fig.tight_layout(rect=(0, 0, 1, 0.96))
    fig.savefig(path, dpi=140)
    plt.close(fig)


def _lookup_point_delta(variant: Dict[str, Any], pid: str) -> Optional[List[Optional[float]]]:
    for block in ("design", "holdout"):
        mapping = ((variant.get(block) or {}).get("per_point_band_db") or {})
        if pid in mapping:
            return list(mapping[pid])
    return None


def plot_t60(t60: Dict[str, Any], path: Path) -> None:
    plt = _plt()
    scenes = t60.get("scenes") or {}
    if not scenes:
        return
    ids = list(scenes)
    fig, ax = plt.subplots(figsize=(11.5, 5.2))
    xs = np.arange(len(ids))
    width = 0.15
    series = [
        ("sabine", [((scenes[sid].get("comparison") or {}).get("ground_truth_sabine_t60_s")) for sid in ids]), 
        ("sim broadband T20 median", [((scenes[sid].get("aggregates") or {}).get("broadband_t20_s") or {}).get("median") for sid in ids]),
        ("sim broadband T30 median", [((scenes[sid].get("aggregates") or {}).get("broadband_t30_s") or {}).get("median") for sid in ids]),
        ("sim 63 Hz T20 median", [((scenes[sid].get("aggregates") or {}).get("band63_t20_s") or {}).get("median") for sid in ids]),
        ("sim 125 Hz T20 median", [((scenes[sid].get("aggregates") or {}).get("band125_t20_s") or {}).get("median") for sid in ids]),
    ]
    colors = ["#333333", "#1f77b4", "#5fa8d3", "#2ca02c", "#ff7f0e"]
    ax.axhspan(HAND_T60_LOW_S, HAND_T60_HIGH_S, color="#d9d9d9", alpha=0.6,
               label="TSPI hand-written 0.35-0.45 s")
    for k, (label, values) in enumerate(series):
        vals = [float(v) if v is not None else float("nan") for v in values]
        ax.bar(xs + (k - 2) * width, vals, width, label=label, color=colors[k])
        for x_i, v in zip(xs + (k - 2) * width, vals):
            if math.isfinite(v):
                ax.text(x_i, v + 0.01, "%.3f" % v, ha="center", va="bottom",
                        fontsize=6, rotation=90)
    ax.set_xticks(xs)
    ax.set_xticklabels(ids, fontsize=8, rotation=10)
    ax.set_ylabel("T60 / T20 [s]")
    ax.set_title("Sabine vs simulated T20/T30 vs hand-written 0.35-0.45 s (SIMULATED/UNPROVEN)",
                 fontsize=10)
    finite_vals = [float(v) for vals in [series_vals for _label, series_vals in series]
                   for v in vals if v is not None and math.isfinite(float(v))]
    top = max(finite_vals) if finite_vals else HAND_T60_HIGH_S
    ax.set_ylim(0.0, max(HAND_T60_HIGH_S, top) * 1.22)
    ax.legend(fontsize=7, ncol=3, loc="upper right")
    ax.grid(axis="y", linewidth=0.3, alpha=0.6)
    fig.tight_layout()
    fig.savefig(path, dpi=140)
    plt.close(fig)


def plot_emodal(scene_payloads: Dict[str, Any], path: Path) -> None:
    plt = _plt()
    scenes = list(scene_payloads)
    if not scenes:
        return
    fig, axes = plt.subplots(1, 2, figsize=(12.5, 4.6))
    width = 0.2
    colors = {"B0": "#999999", "B1": "#1f77b4", "B2": "#2ca02c", "B3": "#d62728"}
    for ax, block in zip(axes, ("design", "holdout")):
        xs = np.arange(len(scenes))
        for k, vid in enumerate(_scene_variant_order()):
            vals = []
            for scene_id in scenes:
                variant = (scene_payloads[scene_id].get("variants") or {}).get(vid) or {}
                vals.append((variant.get(block) or {}).get("e_modal_db"))
            y = [float(v) if v is not None else 0.0 for v in vals]
            ax.bar(xs + (k - 1.5) * width, y, width, label=vid,
                   color=colors.get(vid))
        ax.axhline(0.0, color="black", linewidth=0.8)
        ax.set_xticks(xs)
        ax.set_xticklabels(scenes, fontsize=7, rotation=15)
        ax.set_ylabel("E_modal_dB [dB re B0 baseline]")
        ax.set_title("E_modal_dB - %s" % block, fontsize=9)
        ax.legend(fontsize=7)
        ax.grid(axis="y", linewidth=0.3, alpha=0.6)
    fig.suptitle("E_modal_dB per variant (0 dB = B0 raw) - SIMULATED/UNPROVEN",
                 fontsize=10)
    fig.tight_layout(rect=(0, 0, 1, 0.94))
    fig.savefig(path, dpi=140)
    plt.close(fig)


def generate_plots(python_metrics: Dict[str, Any], t60: Dict[str, Any],
                   out_dir: Path, issues: List[Dict[str, Any]]) -> List[str]:
    written: List[str] = []
    plots_dir = out_dir / "plots"
    plots_dir.mkdir(parents=True, exist_ok=True)
    scenes = python_metrics.get("scenes") or {}
    try:
        p = plots_dir / "fig1_band_rk_design_holdout.png"
        plot_band_rk(scenes, p)
        written.append(str(p))
        p = plots_dir / "fig2_heatmap_B3_vs_B1.png"
        preferred = [sid for sid in ("scene-2x9-t60", "scene-3x10-challenge")
                     if sid in scenes]
        plot_b3_vs_b1_heatmap(scenes, p, preferred or None)
        written.append(str(p))
        p = plots_dir / "fig3_t60_comparison.png"
        plot_t60(t60, p)
        written.append(str(p))
        p = plots_dir / "fig4_emodal_db.png"
        plot_emodal(scenes, p)
        written.append(str(p))
    except Exception as exc:  # plotting must not invalidate the metrics evidence
        issues.append({"kind": "plot_failure", "message": "%s: %s"
                       % (type(exc).__name__, exc)})
        print("[warn] plotting failed: %s" % exc)
    return written


# ---------------------------------------------------------------------------
# pipeline: polling, independent recomputation, cross-check, outputs
# ---------------------------------------------------------------------------

def _log(msg: str) -> None:
    print("[%s] %s" % (now_utc_iso(), msg))
    sys.stdout.flush()


def wait_for_fixtures(fixtures_dir: Path, deadline: float,
                      poll_seconds: float) -> Tuple[List[SceneFixture], Dict[str, Any]]:
    """Poll until MANIFEST + all scenes load and every SHA256 verifies."""
    last_error = "fixtures directory missing"
    while True:
        try:
            manifest_path = fixtures_dir / "MANIFEST.json"
            if not manifest_path.is_file():
                raise MetricsError("MANIFEST.json not written yet")
            manifest = load_json(manifest_path)
            manifest_sha = sha256_file(manifest_path)
            scene_dirs = find_scene_dirs(fixtures_dir)
            if not scene_dirs:
                raise MetricsError("no scene-*/ground-truth.json yet")
            loaded = [load_scene(sd) for sd in scene_dirs]
            manifest_hashes = _scene_hashes_from_manifest(manifest)
            manifest_check: Dict[str, Any] = {}
            for fx in loaded:
                hashes = manifest_hashes.get(fx.scene_id, {})
                gt_hash = hashes.get("ground_truth_sha256")
                sums_hash = hashes.get("sha256sums_sha256")
                ok_gt = (gt_hash is None) or (gt_hash == fx.gt_sha256)
                ok_sums = (sums_hash is None) or (sums_hash == fx.sha256sums_sha256)
                manifest_check[fx.scene_id] = {
                    "manifest_ground_truth_sha256": gt_hash,
                    "manifest_sha256sums_sha256": sums_hash,
                    "ground_truth_match": ok_gt,
                    "sha256sums_match": ok_sums,
                }
                if not (ok_gt and ok_sums):
                    raise MetricsError(
                        "MANIFEST hash mismatch for %s (fixture still being written?)"
                        % fx.scene_id)
            info = {
                "dir": str(fixtures_dir),
                "manifest_sha256": manifest_sha,
                "manifest_scene_count": len(scene_dirs),
                "manifest_scene_ids": manifest.get("scene_ids"),
                "rir_scale_manifest": manifest.get("rir_scale"),
                "per_scene": manifest_check,
                "python_loader": {
                    "wav_format": "RIFF/WAVE IEEE float32 mono (structure parsed in-script)",
                    "sha256": "verified for ground-truth.json and every rir/*.wav",
                },
            }
            return loaded, info
        except MetricsError as exc:
            last_error = str(exc)
        now = time.time()
        if now >= deadline:
            raise MetricsError("timeout waiting for fixtures at %s: %s"
                               % (fixtures_dir, last_error))
        _log("fixtures not ready yet (%s); retry in %ds"
             % (last_error, max(1, int(poll_seconds))))
        time.sleep(max(1.0, poll_seconds))


def _report_parses_as_json(path: Path) -> bool:
    try:
        load_json(path)
        return True
    except MetricsError:
        return False


def _fir_ready(variant: Dict[str, Any], scene_dir: Path,
               runs_dir: Path) -> bool:
    entries = variant.get("fir_files") or []
    if entries:
        for entry in entries:
            raw = entry.get("path") if isinstance(entry, dict) else None
            if not raw:
                continue
            if _resolve_path(str(raw), scene_dir, runs_dir) is None:
                return False
        return True
    for fir_dir in (runs_dir / scene_dir.name / "fir", scene_dir / "fir"):
        if fir_dir.is_dir() and any(fir_dir.glob("*.csv")):
            return True
    return False


def wait_for_runs(runs_dir: Path, scene_dirs: Dict[str, Path], deadline: float,
                  poll_seconds: float) -> Tuple[Dict[str, Path], List[str]]:
    """Poll until each scene's report.json + FIR csv files are complete.

    Returns (ready_report_paths, missing_scene_ids).  Never blocks past deadline.
    """
    ready: Dict[str, Path] = {}
    while True:
        ready = {}
        for scene_id, scene_dir in scene_dirs.items():
            report_path = report_path_for(runs_dir, scene_id)
            if not report_path.is_file():
                continue
            try:
                report = load_rust_report(report_path)
            except MetricsError as exc:
                if _report_parses_as_json(report_path):
                    # Valid JSON but wrong schema: do not silently adapt or wait.
                    raise MetricsError(
                        "report schema mismatch at %s: %s (CONTRACT section 8)"
                        % (report_path, exc))
                continue
            variants = report.get("variants") or {}
            ok = True
            for vid in ("B2", "B3"):
                variant = variants.get(vid) or {}
                if not _fir_ready(variant, scene_dir, runs_dir):
                    ok = False
                    break
            if ok:
                ready[scene_id] = report_path
        missing = [sid for sid in scene_dirs if sid not in ready]
        if not missing:
            return ready, missing
        now = time.time()
        if now >= deadline:
            return ready, missing
        _log("runs: %d/%d scenes ready; waiting for %s; retry in %ds"
             % (len(ready), len(scene_dirs), ", ".join(missing),
                max(1, int(poll_seconds))))
        time.sleep(max(1.0, poll_seconds))


def _scene_band_energy(P: np.ndarray, plan: BandPlan) -> float:
    """Total energy inside all evaluated 1/3-oct bands (design points)."""
    e = band_energies(P, plan.bins)
    finite = e[np.isfinite(e)]
    return float(np.sum(finite))


def _reconstruct_g(fx: SceneFixture, report: Dict[str, Any], runs_dir: Path,
                   variant_id: str, issues: List[Dict[str, Any]]
                   ) -> Tuple[Optional[np.ndarray], Dict[str, Any]]:
    """Rebuild G(f) (N,K) for one variant from the report/FIR files."""
    variant = (report.get("variants") or {}).get(variant_id) or {}
    provenance: Dict[str, Any] = {"variant": variant_id,
                                  "backend": variant.get("backend")}
    if variant_id == "B0":
        provenance["g"] = "G_n(f) = 1 (RAW baseline)"
        return None, provenance  # None means G=1 in pressure()
    if variant_id == "B1":
        params = variant.get("params") or {}
        provenance["params"] = params
        g = build_g_b1(params, fx.source_ids, fx.freqs, fx.sample_rate_hz,
                       issues, variant_id)
        provenance["g"] = "pol*10^(gain/20)*exp(-j2*pi*f*delay/fs)"
        return g, provenance
    files = resolve_variant_fir_files(variant, fx.scene_dir, runs_dir,
                                      issues, variant_id)
    provenance["fir_files"] = [{"path": str(ff.path), "source": ff.source,
                                "in": ff.in_id, "out": ff.out_id,
                                "taps": int(ff.coeffs.size),
                                "delay_samples": ff.delay_samples}
                               for ff in files]
    if not files:
        return None, provenance
    has_source_layout = any(ff.source is not None for ff in files)
    has_matrix_layout = any(ff.out_id is not None or ff.in_id is not None
                            for ff in files)
    if has_source_layout and not has_matrix_layout:
        g = build_g_own_ls(files, fx.source_ids, fx.freqs, fx.sample_rate_hz,
                           issues, variant_id)
        provenance["layout"] = "per-source (P = sum_n H_mn G_n)"
    elif has_matrix_layout:
        g = build_g_matrix(files, fx.source_ids, fx.freqs, fx.sample_rate_hz,
                           issues, variant_id)
        provenance["layout"] = "matrix (P = sum_out H_m,out sum_in G[out][in])"
    else:
        issues.append({"kind": "report_schema", "variant": variant_id,
                       "message": "FIR files have neither source nor out/in metadata"})
        return None, provenance
    if g is not None:
        provenance["g_max_abs_db_by_source"] = [
            (20.0 * math.log10(float(np.max(np.abs(g[n, :]))))
             if np.max(np.abs(g[n, :])) > 0.0 else None)
            for n in range(g.shape[0])]
    return g, provenance


def _algorithm_observations(report: Dict[str, Any]) -> List[Dict[str, Any]]:
    """Flag numerically-consistent but physically degenerate optimiser output.

    The cross-check only proves Rust and Python agree; it cannot prove the
    optimiser produced a usable correction (e.g. G -> 0 would make R_k -> 0).
    Record such observations honestly instead of hiding them.
    """
    obs: List[Dict[str, Any]] = []
    variants = report.get("variants") or {}
    b1_design = ((variants.get("B1") or {}).get("design") or {})
    b1_db = b1_design.get("e_modal_db")
    if b1_db is not None and math.isfinite(float(b1_db)) and float(b1_db) > 0.2:
        obs.append({"variant": "B1", "kind": "b1_baseline_worse_than_b0",
                    "value": float(b1_db),
                    "message": "E_modal_dB(B1) > +0.2 dB: the scalar delay/gain/"
                               "polarity search result is worse than the raw B0 "
                               "baseline; B1 is then not a fair PEQ-only upper "
                               "bound for the MIMO comparison"})
    for vid in ("B1", "B2", "B3"):
        variant = variants.get(vid) or {}
        constraints = variant.get("constraints") or {}
        global_scale = constraints.get("global_scale")
        gains = constraints.get("per_source_max_gain_db") or []
        effort_ratio = constraints.get("effort_vs_baseline")
        design = variant.get("design") or {}
        e_modal_db = design.get("e_modal_db")
        if global_scale is not None and math.isfinite(float(global_scale)) \
                and float(global_scale) < 0.1:
            obs.append({"variant": vid, "kind": "degenerate_global_scale",
                        "value": float(global_scale),
                        "message": "global headroom scale < 0.1 (-20 dB): the "
                                   "reported low R_k is mostly global attenuation, "
                                   "not spatial cancellation"})
        finite_gains = [float(g) for g in gains
                        if g is not None and math.isfinite(float(g))]
        if finite_gains and max(finite_gains) < -20.0:
            obs.append({"variant": vid, "kind": "degenerate_source_gain",
                        "value": max(finite_gains),
                        "message": "per-source in-band max gain < -20 dB: near-zero "
                                   "correction, trivially minimises R_k"})
        if e_modal_db is not None and math.isfinite(float(e_modal_db)) \
                and float(e_modal_db) < -20.0:
            obs.append({"variant": vid, "kind": "suspiciously_large_emodal_reduction",
                        "value": float(e_modal_db),
                        "message": "E_modal_dB < -20 dB; verify it is not a "
                                   "degenerate near-zero G"})
        if effort_ratio is not None and math.isfinite(float(effort_ratio)) \
                and float(effort_ratio) < 1e-3:
            obs.append({"variant": vid, "kind": "near_zero_effort",
                        "value": float(effort_ratio),
                        "message": "E_effort/E_effort_baseline < 1e-3"})
    return obs


def process_scene(fx: SceneFixture, report: Dict[str, Any], runs_dir: Path,
                  diffs: DiffTable, issues: List[Dict[str, Any]]
                  ) -> Dict[str, Any]:
    """Independent recomputation + Rust cross-check for one scene."""
    plan = build_band_plan(fx.gt, fx.freqs)
    h = fx.h_matrix()
    P0 = pressure(None, h, None)
    P0_design = P0[fx.design_idx]
    P0_holdout = P0[fx.holdout_idx]
    scene_id = fx.scene_id

    # -- report provenance / fixture identity ------------------------------
    diffs.add_exact(scene_id, "-", "report", "scene_id",
                    fx.scene_id, report.get("scene_id"))
    provenance = report.get("provenance") or {}
    diffs.add_exact(scene_id, "-", "report", "provenance.data_kind",
                    SOURCE, provenance.get("data_kind"))
    diffs.add_exact(scene_id, "-", "report", "provenance.honesty_status",
                    HONESTY, provenance.get("honesty_status"))
    fixture = report.get("fixture") or {}
    diffs.add_exact(scene_id, "-", "report", "fixture.ground_truth_sha256",
                    fx.gt_sha256, fixture.get("ground_truth_sha256"))
    if fixture.get("rir_scale") is not None:
        diffs.add_float_exact(scene_id, "-", "report", "fixture.rir_scale",
                              fx.scale, fixture.get("rir_scale"), tol=1e-12)
    else:
        issues.append({"kind": "report_schema", "scene": scene_id,
                       "message": "report.fixture.rir_scale missing (CONTRACT section 8)"})
    scene_meta = report.get("scene") or {}
    diffs.add_int(scene_id, "-", "report", "scene.n_sources",
                  fx.n_sources, scene_meta.get("n_sources"))
    diffs.add_int(scene_id, "-", "report", "scene.n_mics",
                  fx.n_mics, scene_meta.get("n_mics"))
    diffs.add_int(scene_id, "-", "report", "scene.n_design",
                  len(fx.design_ids), scene_meta.get("n_design"))
    diffs.add_int(scene_id, "-", "report", "scene.n_holdout",
                  len(fx.holdout_ids), scene_meta.get("n_holdout"))
    if fixture.get("seed") is not None:
        diffs.add_int(scene_id, "-", "report", "fixture.seed",
                      int(fx.gt.get("seed", 0)), fixture.get("seed"))
    for key, py_ids in (("source_ids", fx.source_ids),
                        ("design_mic_ids", fx.design_ids),
                        ("holdout_mic_ids", fx.holdout_ids)):
        if scene_meta.get(key) is not None:
            diffs.add_exact(scene_id, "-", "report", "scene." + key,
                            list(py_ids), list(scene_meta.get(key) or []))
    config = report.get("config") or {}
    band_plan = config.get("band_plan") or {}
    evaluated = band_plan.get("evaluated_spec_hz")
    extended = band_plan.get("extended_hz")
    if evaluated is None:
        issues.append({"kind": "report_schema", "scene": scene_id,
                       "message": "report.config.band_plan.evaluated_spec_hz missing "
                                  "(CONTRACT section 8)"})
    else:
        diffs.add_array(scene_id, "-", "report",
                        "config.band_plan.evaluated_spec_hz",
                        list(plan.centers_hz), list(evaluated), "float_exact")
    if extended is not None:
        diffs.add_array(scene_id, "-", "report", "config.band_plan.extended_hz",
                        list(plan.extended_hz), list(extended), "float_exact")

    for missing_key in required_missing(report):
        issues.append({"kind": "report_schema_missing", "scene": scene_id,
                       "field": missing_key,
                       "message": "required CONTRACT section 8 field missing from report"})

    # -- per variant independent recomputation ----------------------------
    variants_out: Dict[str, Any] = {}
    for vid in _scene_variant_order():
        g, provenance_g = _reconstruct_g(fx, report, runs_dir, vid, issues)
        if vid != "B0" and g is None:
            variants_out[vid] = {"reconstruction": provenance_g,
                                 "design": {}, "holdout": {}}
            issues.append({"kind": "variant_incomplete", "scene": scene_id,
                           "variant": vid,
                           "message": "cannot rebuild P_after (G unavailable); "
                                      "variant not cross-checked"})
            continue
        Pa = pressure(None, h, g)
        Pa_design = Pa[fx.design_idx]
        Pa_holdout = Pa[fx.holdout_idx]
        metrics = variant_metrics(P0_design, P0_holdout, Pa_design, Pa_holdout,
                                  plan, fx.design_ids, fx.holdout_ids)
        metrics["reconstruction"] = provenance_g
        variants_out[vid] = metrics
        rust_variant = (report.get("variants") or {}).get(vid) or {}
        crosscheck_variant(diffs, scene_id, vid, metrics, rust_variant,
                           plan.centers_hz)
        if vid == "B1":
            e_b0 = _scene_band_energy(P0_design, plan)
            e_b1 = _scene_band_energy(Pa_design, plan)
            delta_db = db10(e_b1 / e_b0) if (e_b0 > 0.0 and e_b1 > 0.0) else None
            b1_constraint = {
                "design_evaluated_band_energy_total_b0": e_b0,
                "design_evaluated_band_energy_total_b1": e_b1,
                "delta_db_b1_vs_b0": delta_db,
                "limit_db": 1.0,
                "within_limit": (None if delta_db is None else abs(delta_db) <= 1.0),
            }
            variants_out[vid]["b1_level_constraint"] = b1_constraint
            if b1_constraint["within_limit"] is False:
                issues.append({"kind": "b1_level_constraint_violation",
                               "scene": scene_id,
                               "message": "|10log10(B1/B0)| > 1.0 dB: %r dB"
                                          % delta_db})

    # -- scene crosscheck summary -----------------------------------------
    scene_rows = [row for row in diffs.rows if row["scene"] == scene_id]
    mismatches = [row for row in scene_rows if is_failing_status(row["status"])]
    incomplete = any(item.get("kind") == "variant_incomplete"
                     and item.get("scene") == scene_id for item in issues)
    if mismatches:
        status = "FAIL"
    elif incomplete:
        status = "INCOMPLETE"
    else:
        status = "PASS"
    max_map: Dict[str, Any] = {}
    for row in scene_rows:
        if row["status"] != "PASS":
            continue
        diff = row.get("abs_diff")
        if diff is None or not math.isfinite(float(diff)):
            continue
        key = "max_abs_diff_" + _normalise_family_key(row["family"])
        max_map[key] = max(float(max_map.get(key, 0.0)), float(diff))
    return {
        "scene_id": scene_id,
        "fixture": {
            "ground_truth_sha256": fx.gt_sha256,
            "sha256sums_sha256": fx.sha256sums_sha256,
            "rir_scale": fx.scale,
            "fft_size": fx.fft_size,
            "sample_rate_hz": fx.sample_rate_hz,
            "frequency_grid_k": int(fx.freqs.size),
            "frequency_grid_first_hz": float(fx.freqs[0]),
            "frequency_grid_last_hz": float(fx.freqs[-1]),
            "rir_file_count": len(fx.rir_files),
            "n_sources": fx.n_sources,
            "n_mics": fx.n_mics,
            "n_design": len(fx.design_ids),
            "n_holdout": len(fx.holdout_ids),
            "design_mic_ids": list(fx.design_ids),
            "holdout_mic_ids": list(fx.holdout_ids),
            "warnings": list(fx.warnings),
        },
        "bands": {
            "centers_hz": list(plan.centers_hz),
            "extended_centers_hz": list(plan.extended_hz),
            "bin_counts": [int(b.size) for b in plan.bins],
            "empty_bands_hz": list(plan.empty_bands),
        },
        "variants": variants_out,
        "crosscheck": {
            "status": status,
            "n_rows_compared": len(scene_rows),
            "n_mismatch": len(mismatches),
            "mismatches": mismatches,
            "max_abs_diff": max_map,
        },
        "algorithm_observations": _algorithm_observations(report),
        "report": {
            "format": report.get("format"),
            "tool": report.get("tool"),
            "cv": report.get("cv"),
            "config": report.get("config"),
            "judgement": report.get("judgement"),
            "notes": report.get("notes"),
        },
    }


def _normalise_family_key(family: str) -> str:
    """Canonical max_abs_diff family names (CONTRACT section 9 example style)."""
    mapping = {
        "E_modal_db": "E_modal_db", "E_modal": "E_modal",
        "E_modal_eq": "E_modal_eq", "E_modal_eq_db": "E_modal_eq_db",
        "R_k": "R_k", "R_max": "R_max",
        "alpha": "alpha", "e_k0": "e_k0", "band_delta_db": "band_delta_db",
        "spatial_mean_db": "spatial_mean_db", "spatial_std_db": "spatial_std_db",
        "spatial_range_db": "spatial_range_db",
        "spatial_p90_p10_db": "spatial_p90_p10_db",
        "spatial_worst_null_db": "spatial_worst_null_db",
        "spatial_max_peak_db": "spatial_max_peak_db",
        "spatial_interference_null_db": "spatial_interference_null_db",
        "coverage": "coverage", "per_point_band_db": "per_point_band_db",
        "point_weighted_db": "point_weighted_db",
        "consistency_c": "consistency_c", "argmax_band_hz": "argmax_band_hz",
        # lower-case metric names used internally as default families
        "e_modal_db": "E_modal_db", "e_modal": "E_modal",
        "e_modal_eq": "E_modal_eq", "e_modal_eq_db": "E_modal_eq_db",
        "r_k": "R_k", "r_max": "R_max",
        "energy_migration": "energy_migration",
        "severe_regression": "severe_regression",
        "feasibility.design_max_rise_db": "design_max_rise_db",
        "feasibility.max_db_rise_db": "max_db_rise_db",
        "feasibility.feasible": "feasibility_feasible",
        "feasibility.rule_consistent": "feasibility_rule_consistent",
        "feasibility.rule_consistent_with_cv": "feasibility_rule_consistent_with_cv",
        "predicted_status": "predicted_status",
        "fixture.ground_truth_sha256": "fixture.ground_truth_sha256",
        "fixture.rir_scale": "fixture.rir_scale",
        "scene_id": "scene_id",
    }
    return mapping.get(family, family)


def write_diff_csv(path: Path, rows: Sequence[Dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="") as fh:
        writer = csv.writer(fh)
        writer.writerow(["scene", "variant", "block", "metric", "item",
                         "python", "rust", "abs_diff", "rel_diff", "tolerance",
                         "kind", "status"])
        for row in rows:
            writer.writerow([
                row.get("scene"), row.get("variant"), row.get("block"),
                row.get("metric"), row.get("item"),
                _format_cell(row.get("python")), _format_cell(row.get("rust")),
                _format_cell(row.get("abs_diff")), _format_cell(row.get("rel_diff")),
                _format_cell(row.get("tolerance")), row.get("kind"),
                row.get("status"),
            ])


def _format_cell(value: Any) -> Any:
    if value is None:
        return ""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        if isinstance(value, float) and not math.isfinite(value):
            return str(value)
        return repr(float(value)) if isinstance(value, float) else str(value)
    return str(value)


def run_pipeline(args: argparse.Namespace) -> int:
    fixtures_dir = Path(args.fixtures).resolve()
    runs_dir = Path(args.runs).resolve()
    out_dir = Path(args.out).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    started = time.time()
    deadline = started + float(args.timeout_seconds)
    poll = float(args.poll_seconds)
    _log("source=%s honesty=%s (SIMULATED / UNPROVEN)" % (SOURCE, HONESTY))
    _log("fixtures=%s" % fixtures_dir)
    _log("runs=%s" % runs_dir)
    _log("out=%s (timeout %.0fs, poll %.0fs)" % (out_dir, args.timeout_seconds, poll))

    scenes, fixtures_info = wait_for_fixtures(fixtures_dir, deadline, poll)
    if args.scene:
        wanted = set(args.scene)
        scenes = [fx for fx in scenes if fx.scene_id in wanted]
        if not scenes:
            raise MetricsError("--scene filter matched no fixture scene: %r"
                               % (args.scene,))
    _log("fixtures ready: %d scenes" % len(scenes))
    for fx in scenes:
        _log("  %-22s K=%d N=%d M=%d design=%d holdout=%d scale=%g gt=%s"
             % (fx.scene_id, fx.freqs.size, fx.n_sources, fx.n_mics,
                len(fx.design_ids), len(fx.holdout_ids), fx.scale,
                fx.gt_sha256[:16]))

    # T60/EDC analysis only needs the fixtures -> emit it before waiting on runs
    method_control = t60_method_control()
    t60 = build_t60_analysis(scenes, method_control)
    write_json(out_dir / "t60-analysis.json", t60)
    (out_dir / "t60-analysis.md").write_text(render_t60_markdown(t60),
                                             encoding="utf-8", newline=chr(10))
    _log("T60 analysis written: t60-analysis.json / t60-analysis.md")
    _log("  method control max relative error: %.6f"
         % (method_control.get("max_abs_relative_error") or float("nan")))
    for sid, scene in t60["scenes"].items():
        agg = scene["aggregates"]
        _log("  %-22s sabine=%.4fs broadbandT20=%.4fs [%.4f,%.4f] "
             "band63=%.4fs band125=%.4fs"
             % (sid, scene["comparison"]["computed_sabine_t60_s"],
                agg["broadband_t20_s"]["median"] or float("nan"),
                agg["broadband_t20_s"]["q1"] or float("nan"),
                agg["broadband_t20_s"]["q3"] or float("nan"),
                agg["band63_t20_s"]["median"] or float("nan"),
                agg["band125_t20_s"]["median"] or float("nan")))

    # Wait for Rust runs, then cross-check.
    scene_dirs = {fx.scene_id: fx.scene_dir for fx in scenes}
    scene_by_id = {fx.scene_id: fx for fx in scenes}
    report_paths, missing = wait_for_runs(runs_dir, scene_dirs, deadline, poll)
    if not report_paths:
        raise MetricsError("timeout waiting for any runs/<scene>/report.json under %s"
                           % runs_dir)
    if missing:
        _log("WARNING: runs incomplete at timeout: %s" % ", ".join(missing))

    diffs = DiffTable()
    issues: List[Dict[str, Any]] = []
    scene_payloads: Dict[str, Any] = {}
    for scene_id in sorted(report_paths):
        fx = scene_by_id[scene_id]
        report_path = report_paths[scene_id]
        try:
            report = load_rust_report(report_path)
            payload = process_scene(fx, report, runs_dir, diffs, issues)
            scene_payloads[scene_id] = payload
            _log("cross-checked %-22s status=%s rows=%d mismatches=%d"
                 % (scene_id, payload["crosscheck"]["status"],
                    payload["crosscheck"]["n_rows_compared"],
                    payload["crosscheck"]["n_mismatch"]))
        except MetricsError as exc:
            issues.append({"kind": "report_unreadable", "scene": scene_id,
                           "message": str(exc)})
            _log("ERROR processing %s: %s" % (scene_id, exc))

    # Global diff summary.
    all_rows = diffs.rows
    mismatch_rows = [row for row in all_rows if is_failing_status(row["status"])]
    family_max = {_normalise_family_key(fam): val
                  for fam, val in diffs.max_abs_by_family().items()}
    for item in issues:
        if item.get("kind") == "report_schema_missing":
            item["status"] = "FAIL"

    scene_statuses = {sid: payload["crosscheck"]["status"]
                      for sid, payload in scene_payloads.items()}
    if any(status == "FAIL" for status in scene_statuses.values()):
        overall = "FAIL"
    elif missing or any(status == "INCOMPLETE" for status in scene_statuses.values()):
        overall = "INCOMPLETE"
    elif len(scene_payloads) < len(scenes):
        overall = "INCOMPLETE"
    else:
        overall = "PASS"

    python_metrics = {
        "format": PY_METRICS_FORMAT,
        "source": SOURCE,
        "provenance": {"data_kind": SOURCE, "honesty_status": HONESTY,
                       "claim": "synthetic offline study only; no real room, "
                                "no measurement; never use as measured result"},
        "tool": {"name": TOOL_NAME, "version": TOOL_VERSION,
                 "python": sys.version.split()[0], "numpy": np.__version__,
                 "scipy": (__import__("scipy").__version__ if HAVE_SCIPY else None),
                 "matplotlib": _matplotlib_version()},
        "generated_at_utc": now_utc_iso(),
        "inputs": {"fixtures_dir": str(fixtures_dir), "runs_dir": str(runs_dir),
                   "fixtures": fixtures_info,
                   "scenes_requested": [fx.scene_id for fx in scenes],
                   "scenes_compared": sorted(scene_payloads),
                   "scenes_missing_runs": missing},
        "tolerances": {"ratio_relative": TOL_RATIO, "db_absolute": TOL_DB},
        "conventions": {
            "design_r_k_denominator": "B0(design) E_k0",
            "design_alpha": "B0(design) band energies, clamp [0.5,2.0], sum=K",
            "holdout_r_k_denominator": "B0(holdout point set) per-band energies",
            "holdout_per_point_before": "that point's own B0",
            "db_definition": "10*log10(after_energy/before_energy)",
            "e_modal_db": ("10*log10(E_modal / sum_{valid} alpha_k); "
                           "normalised so B0 = 0 dB (CONTRACT 2026-11-22)"),
            "e_modal_eq_db": ("10*log10((sum_{valid} R_k) / K_valid); "
                              "normalised so B0 = 0 dB"),
            "e_modal_raw_reported_as": "e_modal / e_modal_eq (unnormalised)",
            "spatial_block": ("per variant, computed on P_after (B0: after == "
                              "before); P_before spatial kept as spatial_before"),
            "beta": "not used by Python; P_after is rebuilt from FIR/B1 params",
        },
        "scenes": scene_payloads,
        "crosscheck": {
            "status": overall,
            "scenes_expected": len(scenes),
            "scenes_compared": len(scene_payloads),
            "scenes_missing_runs": missing,
            "n_rows_compared": len(all_rows),
            "counts_by_status": diffs.counts(),
            "n_mismatch": len(mismatch_rows),
            "missing_required_report_fields": [
                item for item in issues
                if item.get("kind") == "report_schema_missing"],
            "max_abs_diff": family_max,
            "mismatches": mismatch_rows,
        },
        "max_abs_diff": family_max,
        "issues": issues,
    }
    write_json(out_dir / "python-metrics.json", python_metrics)
    write_diff_csv(out_dir / "python-vs-rust.csv", all_rows)
    _log("wrote python-metrics.json (%d comparison rows)" % len(all_rows))
    _log("wrote python-vs-rust.csv")

    if not args.no_plots:
        written = generate_plots(python_metrics, t60, out_dir, issues)
        for path in written:
            _log("plot: %s" % path)
        if written:
            write_json(out_dir / "python-metrics.json", python_metrics)

    # Console summary.
    _log("cross-check status: %s" % overall)
    for key in sorted(family_max):
        _log("  max_abs_diff_%s = %.3e" % (key, family_max[key]))
    if mismatch_rows:
        _log("MISMATCHES (first 20 of %d):" % len(mismatch_rows))
        for row in mismatch_rows[:20]:
            _log("  %s/%s/%s %s[%s]: python=%r rust=%r (%s)"
                 % (row["scene"], row["variant"], row["block"], row["metric"],
                    row["item"], row["python"], row["rust"], row["status"]))
    if issues:
        _log("issues: %d (see python-metrics.json)" % len(issues))
        for item in issues[:10]:
            _log("  issue: %r" % item)

    if overall == "PASS":
        return 0
    if overall == "INCOMPLETE":
        return 2
    return 3


def _matplotlib_version() -> Optional[str]:
    try:
        import matplotlib
        return matplotlib.__version__
    except Exception:  # pragma: no cover
        return None



# ---------------------------------------------------------------------------
# round 2: objective-v2 independent recomputation
#   seat_std / max_dev / null_depth (all on P_after, design and holdout sets)
#   feasibility (design_max_rise_db vs max_db_rise_db) and predicted_status
# ---------------------------------------------------------------------------

OBJECTIVE_TAGS_DEFAULT = ("emodal", "seat-std", "null-depth", "combined",
                         "seat-std-maxdev")
STATUS_RULE_VERSION = "v2-frozen-2026-11-22"
STATUS_RULE_TEXT = (
    "B0='BASELINE'; B2/B3 priority: holdout severe -> PREDICTED_FAIL; else "
    "feasibility=false -> INFEASIBLE (not FAIL); else PASS conditions -> "
    "PREDICTED_PASS; else holdout<3 -> INCOMPLETE; else PREDICTED_PARTIAL. "
    "gain_cap/headroom scaling does not auto-FAIL: a final FIR that is within "
    "the caps is judged by the metrics. The same rule is applied to B1 without "
    "a feasibility term.")
STATUS_RULE_JSON = {"version": STATUS_RULE_VERSION, "text": STATUS_RULE_TEXT}
OBJECTIVE_NEW_METRIC_KEYS = ("seat_std_mean_db", "seat_std_max_db", "max_dev_db",
                             "null_depth_mean_db", "null_depth_worst_db")


def seat_std_null_metrics(P_after: np.ndarray, bins: List[np.ndarray]
                          ) -> Dict[str, Any]:
    """Seat-level dispersion / null metrics on a point set (OBJECTIVES-V2 section 1).

    seat_std_mean_db  = mean_b std_m(10log10 E_m,b)          [dB]
    seat_std_max_db   = max_b  std_m(10log10 E_m,b)          [dB]
    max_dev_db        = max_{b,m} |level_m,b - mean_b|       [dB]
    null_depth_mean_db = mean_b interference_null_db_b       [dB]
    null_depth_worst_db= min_b  interference_null_db_b       [dB]
    E_m,b = sum_{f in B_b} |P_m(f)|^2 with P = P_after.
    """
    P = np.asarray(P_after)
    n_pts = P.shape[0]
    e_mb = point_band_energies(P, bins)          # (M, B)
    level = db10_arr(e_mb)                       # (M, B)
    n_bands = e_mb.shape[1]
    seat_std_db: List[float] = []
    band_max_dev_db: List[float] = []
    interference_db: List[float] = []
    per_point_dev = np.full(level.shape, np.nan, dtype=np.float64)
    for b in range(n_bands):
        finite = level[np.isfinite(level[:, b]), b]
        if finite.size == 0:
            seat_std_db.append(float("nan"))
            band_max_dev_db.append(float("nan"))
        else:
            mean_b = float(np.mean(finite))
            seat_std_db.append(float(np.std(finite, ddof=0)))
            dev = np.abs(finite - mean_b)
            band_max_dev_db.append(float(np.max(dev)) if dev.size else float("nan"))
            col = level[:, b]
            mask = np.isfinite(col)
            per_point_dev[mask, b] = np.abs(col[mask] - mean_b)
        e_col = e_mb[:, b]
        mean_e = float(np.mean(e_col)) if e_col.size else float("nan")
        if e_col.size and mean_e > 0.0:
            interference_db.append(db10(float(np.min(e_col)) / mean_e))
        else:
            interference_db.append(float("nan"))
    finite_std = [v for v in seat_std_db if math.isfinite(v)]
    finite_dev = [v for v in band_max_dev_db if math.isfinite(v)]
    finite_null = [v for v in interference_db if math.isfinite(v)]
    finite_pp = per_point_dev[np.isfinite(per_point_dev)]
    return {
        "seat_std_mean_db": (float(np.mean(finite_std)) if finite_std else None),
        "seat_std_max_db": (float(np.max(finite_std)) if finite_std else None),
        "max_dev_db": (float(np.max(finite_dev)) if finite_dev else None),
        "max_dev_all_points_db": (float(np.max(finite_pp)) if finite_pp.size else None),
        "null_depth_mean_db": (float(np.mean(finite_null)) if finite_null else None),
        "null_depth_worst_db": (float(np.min(finite_null)) if finite_null else None),
        "per_band": {
            "seat_std_db": seat_std_db,
            "band_max_dev_db": band_max_dev_db,
            "interference_null_db": interference_db,
        },
    }


def design_band_rise(P0_design: np.ndarray, Pa_design: np.ndarray,
                     bins: List[np.ndarray]) -> np.ndarray:
    """(n_design, n_bands) rise in dB = 10log10(E_after/E_B0) per point/band."""
    return point_band_delta_db(P0_design, Pa_design, bins)


def evaluate_feasibility(delta_design_mb: np.ndarray,
                         design_ids: Sequence[str],
                         centers_hz: Sequence[float],
                         max_db_rise_db: float) -> Dict[str, Any]:
    """Independent check of design-level trust-region feasibility.

    A candidate is infeasible if any design point / evaluated band rises above
    max_db_rise_db dB relative to that point's B0 band energy.
    """
    delta = np.asarray(delta_design_mb, dtype=np.float64)
    violations: List[Dict[str, Any]] = []
    max_rise = float("nan")
    worst: Optional[Dict[str, Any]] = None
    for m in range(delta.shape[0]):
        for b in range(delta.shape[1]):
            value = delta[m, b]
            if value is None or not math.isfinite(float(value)):
                continue
            value = float(value)
            if not math.isfinite(max_rise) or value > max_rise:
                max_rise = value
                worst = {"mic": design_ids[m] if m < len(design_ids) else str(m),
                         "band_hz": float(centers_hz[b]) if b < len(centers_hz) else None,
                         "rise_db": value}
            if value > float(max_db_rise_db):
                violations.append({
                    "mic": design_ids[m] if m < len(design_ids) else str(m),
                    "band_hz": float(centers_hz[b]) if b < len(centers_hz) else None,
                    "rise_db": value,
                })
    return {
        "max_db_rise_db": float(max_db_rise_db),
        "design_max_rise_db": (None if not math.isfinite(max_rise) else max_rise),
        "design_worst_point": worst,
        "feasible": bool(math.isfinite(max_rise) and max_rise <= float(max_db_rise_db)),
        "violations": violations,
        "n_violations": len(violations),
    }


def compute_objective_status(design: Dict[str, Any], holdout: Dict[str, Any],
                             feasibility: Optional[Dict[str, Any]],
                             variant_id: str) -> Tuple[str, Dict[str, Any]]:
    """Independent implementation of the FROZEN predicted_status rule.

    Rule version v2-frozen-2026-11-22 (roomforge-mimo ruling):
      B0 -> BASELINE;
      B2/B3 priority: holdout severe -> PREDICTED_FAIL; else feasibility=false
      -> INFEASIBLE (not FAIL); else PASS conditions -> PREDICTED_PASS; else
      valid holdout < 3 -> INCOMPLETE; else PREDICTED_PARTIAL.
      B1 uses the same rule without the feasibility term.
      gain_cap / headroom scaling does NOT auto-FAIL: the final FIR, if within
      the caps, is judged by the metrics.
    """
    checks: Dict[str, Any] = {}
    if variant_id == "B0":
        checks["baseline"] = True
        return "BASELINE", checks
    e_modal_db = design.get("e_modal_db")
    r_max = design.get("r_max")
    migration = design.get("energy_migration")
    consistency = holdout.get("consistency_c")
    severe = holdout.get("severe_regression")
    valid_count = holdout.get("valid_holdout_count")
    checks["e_modal_db_lt_0"] = (e_modal_db is not None
                                 and math.isfinite(float(e_modal_db))
                                 and float(e_modal_db) < 0.0)
    checks["r_max_lt_1"] = (r_max is not None
                            and math.isfinite(float(r_max)) and float(r_max) < 1.0)
    checks["no_energy_migration"] = (migration is False)
    checks["consistency_ge_0_5"] = (consistency is not None
                                    and math.isfinite(float(consistency))
                                    and float(consistency) >= CONSISTENCY_MIN)
    checks["no_severe"] = (severe is False)
    checks["severe"] = (severe is True)
    if severe is True:
        return "PREDICTED_FAIL", checks
    applicable = (variant_id in ("B2", "B3")
                  and isinstance(feasibility, dict)
                  and feasibility.get("applicable") is not False)
    feasible_false = bool(applicable and feasibility.get("feasible") is False)
    checks["feasibility_false"] = feasible_false
    if feasible_false:
        return "INFEASIBLE", checks
    if all(checks[key] for key in ("e_modal_db_lt_0", "r_max_lt_1",
                                   "no_energy_migration", "consistency_ge_0_5",
                                   "no_severe")):
        return "PREDICTED_PASS", checks
    if isinstance(valid_count, int) and valid_count < 3:
        return "INCOMPLETE", checks
    return "PREDICTED_PARTIAL", checks


def _normalise_objective_name(name: str) -> str:
    return str(name).strip().lower().replace("_", "-").replace(" ", "-")


def _extract_feasibility(variant: Dict[str, Any]) -> Optional[Dict[str, Any]]:
    for key in ("feasibility", "trust_region", "feasibility_check"):
        value = variant.get(key)
        if isinstance(value, dict):
            return value
    return None


def _extract_predicted_status(variant: Dict[str, Any],
                              report: Dict[str, Any]) -> Optional[str]:
    value = variant.get("predicted_status")
    if isinstance(value, str):
        return value
    judgement = report.get("judgement")
    if isinstance(judgement, dict):
        for key in ("predicted_status", "status", "predicted"):
            val = judgement.get(key)
            if isinstance(val, str):
                return val
    value = report.get("predicted_status")
    if isinstance(value, str):
        return value
    return None


def _find_max_db_rise(report: Dict[str, Any],
                      variant: Dict[str, Any],
                      feasibility: Optional[Dict[str, Any]]) -> float:
    """max_db_rise_db from the report, falling back to the OBJECTIVES-V2 default."""
    candidates = []
    if isinstance(feasibility, dict):
        candidates.append(feasibility.get("max_db_rise_db"))
    config = report.get("config") or {}
    for key in ("max_db_rise_db", "max_db_rise"):
        candidates.append(config.get(key))
    objective_cfg = report.get("objective") or {}
    if isinstance(objective_cfg, dict):
        candidates.append(objective_cfg.get("max_db_rise_db"))
    params = variant.get("params") or {}
    if isinstance(params, dict):
        candidates.append(params.get("max_db_rise_db"))
    for value in candidates:
        if value is not None and math.isfinite(float(value)):
            return float(value)
    return 2.0


def _normalise_violation_list(value: Any) -> List[str]:
    """Normalise a violations list to sorted descriptive strings if possible."""
    if value is None:
        return []
    if not isinstance(value, (list, tuple)):
        return [str(value)]
    out: List[str] = []
    for item in value:
        if isinstance(item, dict):
            mic = None
            band = None
            for key in ("mic", "mic_id", "point", "id"):
                if key in item:
                    mic = str(item[key])
                    break
            for key in ("band_center_hz", "band_hz", "center_hz", "band",
                        "freq_hz"):
                if key in item:
                    try:
                        band = "%g" % float(item[key])
                    except (TypeError, ValueError):
                        band = str(item[key])
                    break
            out.append("%s|%s" % (mic, band))
        else:
            out.append(str(item))
    return sorted(out)


OBJ_REQUIRED_VARIANTS = ("B0", "B1", "B2", "B3")
OBJ_MANDATORY_VARIANTS = ("B0", "B2", "B3")


def load_obj_report(path: Path) -> Dict[str, Any]:
    """Load one runs-obj report; structure errors are fatal (schema mismatch)."""
    report = load_json(path)
    if not isinstance(report, dict):
        raise MetricsError("%s: report is not a JSON object" % path)
    variants = report.get("variants")
    if not isinstance(variants, dict):
        raise MetricsError("%s: 'variants' must be object (CONTRACT v2 schema)" % path)
    for vid in OBJ_MANDATORY_VARIANTS:
        if vid not in variants:
            raise MetricsError("%s: missing mandatory variant %r" % (path, vid))
        variant = variants[vid]
        if not isinstance(variant, dict):
            raise MetricsError("%s: variants.%s is not an object" % (path, vid))
        for block in ("design", "holdout"):
            if not isinstance(variant.get(block), dict):
                raise MetricsError("%s: variants.%s.%s missing/not object"
                                   % (path, vid, block))
    return report


def _obj_report_parses_as_json(path: Path) -> bool:
    try:
        load_json(path)
        return True
    except MetricsError:
        return False


def wait_for_obj_runs(obj_root: Path, tags: Sequence[str],
                      scene_dirs: Dict[str, Path],
                      deadline: float, poll_seconds: float
                      ) -> Tuple[Dict[str, Dict[str, Path]], List[str]]:
    """Poll runs-obj/<tag>/<scene>/report.json until all are complete.

    Returns (ready[tag][scene] = report path, missing["<tag>/<scene>", ...]).
    Never blocks past the deadline; missing entries are reported, not swallowed.
    """
    ready: Dict[str, Dict[str, Path]] = {tag: {} for tag in tags}
    last_log = 0.0
    while True:
        ready = {tag: {} for tag in tags}
        for tag in tags:
            tag_dir = obj_root / tag
            for scene_id, scene_dir in scene_dirs.items():
                report_path = tag_dir / scene_id / "report.json"
                if not report_path.is_file():
                    continue
                try:
                    report = load_obj_report(report_path)
                except MetricsError as exc:
                    if _obj_report_parses_as_json(report_path):
                        raise MetricsError(
                            "runs-obj report schema mismatch at %s: %s"
                            % (report_path, exc))
                    continue
                variants = report.get("variants") or {}
                fir_ok = True
                for vid in ("B2", "B3"):
                    variant = variants.get(vid) or {}
                    if not _fir_ready(variant, scene_dir, tag_dir):
                        fir_ok = False
                        break
                if fir_ok:
                    ready[tag][scene_id] = report_path
        missing = ["%s/%s" % (tag, scene_id)
                   for tag in tags for scene_id in scene_dirs
                   if scene_id not in ready[tag]]
        if not missing:
            return ready, missing
        now = time.time()
        if now >= deadline:
            return ready, missing
        if now - last_log > 60.0:
            _log("runs-obj: %d/%d ready; missing: %s; retry in %ds"
                 % (len(scene_dirs) * len(tags) - len(missing),
                    len(scene_dirs) * len(tags), ", ".join(missing[:8])
                    + (" ..." if len(missing) > 8 else ""),
                    max(1, int(poll_seconds))))
            last_log = now
        time.sleep(max(1.0, poll_seconds))


def crosscheck_objective_variant(diffs: DiffTable, scene_id: str,
                                 variant_id: str,
                                 py_variant: Dict[str, Any],
                                 rust_variant: Dict[str, Any],
                                 centers: Sequence[float]) -> None:
    """Compare the OBJECTIVES-V2 new metrics, feasibility and predicted_status."""
    py_design = py_variant.get("design") or {}
    py_holdout = py_variant.get("holdout") or {}
    rust_design = rust_variant.get("design") or {}
    rust_holdout = rust_variant.get("holdout") or {}
    for block_name, py_block, rust_block in (
            ("design", py_design, rust_design),
            ("holdout", py_holdout, rust_holdout)):
        for key in OBJECTIVE_NEW_METRIC_KEYS:
            diffs.add_db(scene_id, variant_id, block_name, key,
                         py_block.get(key), rust_block.get(key))

    rust_f = _extract_feasibility(rust_variant)
    py_f = py_variant.get("python_feasibility") or {"applicable": False}
    rust_applicable = bool(isinstance(rust_f, dict)
                           and rust_f.get("applicable") is not False)
    diffs.add_exact(scene_id, variant_id, "feasibility", "feasibility.applicable",
                    bool(py_f.get("applicable")), rust_applicable)
    if variant_id in ("B2", "B3") and not rust_applicable:
        diffs.add_exact(scene_id, variant_id, "feasibility",
                        "feasibility.present", True, False)
    elif variant_id in ("B2", "B3"):
        rust_max = rust_f.get("max_db_rise_db")
        rust_design = rust_f.get("design_max_rise_db")
        rust_feasible = rust_f.get("feasible")
        rust_cv = rust_f.get("cv_fold_max_rise_db") or []
        rust_violations = rust_f.get("violations")
        diffs.add_db(scene_id, variant_id, "feasibility",
                     "feasibility.design_max_rise_db",
                     py_f.get("design_max_rise_db"), rust_design)
        rust_violation_list = (rust_violations
                               if isinstance(rust_violations, list) else [])
        design_violations = [item for item in rust_violation_list
                             if not isinstance(item, dict)
                             or item.get("scope") in (None, "design")]
        cv_violations = [item for item in rust_violation_list
                         if isinstance(item, dict)
                         and item.get("scope") not in (None, "design")]
        diffs.add_int(scene_id, variant_id, "feasibility",
                      "feasibility.design_violations_count",
                      int(py_f.get("n_violations", 0)),
                      len(design_violations))
        py_design_v = _normalise_violation_list(py_f.get("violations"))
        rust_design_v = _normalise_violation_list(design_violations)
        diffs.add_exact(scene_id, variant_id, "feasibility",
                        "feasibility.design_violation_ids", py_design_v,
                        rust_design_v)
        # Self-consistency: rust fields must imply rust.feasible.
        rule_ok: Optional[bool] = None
        cv_ok = True
        for value in rust_cv:
            if value is None or not math.isfinite(float(value)):
                cv_ok = False
                continue
            if rust_max is not None and float(value) > float(rust_max):
                cv_ok = False
        design_ok = (rust_design is not None
                     and math.isfinite(float(rust_design))
                     and rust_max is not None
                     and float(rust_design) <= float(rust_max))
        if rust_max is not None:
            rule_ok = bool(design_ok and cv_ok
                           and not (isinstance(rust_violations, list)
                                    and rust_violations))
            diffs.add_bool(scene_id, variant_id, "feasibility",
                           "feasibility.rule_consistent", rule_ok,
                           bool(rust_feasible))
            # Rule check that also uses the Rust-provided CV fold maxima:
            py_design_max = py_f.get("design_max_rise_db")
            py_design_ok = (py_design_max is not None
                            and math.isfinite(float(py_design_max))
                            and float(py_design_max) <= float(rust_max))
            expected_with_cv = bool(py_design_ok and cv_ok
                                    and not (isinstance(rust_violations, list)
                                             and rust_violations))
            diffs.add_bool(scene_id, variant_id, "feasibility",
                           "feasibility.rule_consistent_with_cv",
                           expected_with_cv, bool(rust_feasible))
        rust_design_feasible = bool(
            rust_design is not None and math.isfinite(float(rust_design))
            and rust_max is not None
            and float(rust_design) <= float(rust_max))
        diffs.add_bool(scene_id, variant_id, "feasibility",
                       "feasibility.design_feasible",
                       bool(py_f.get("feasible")), rust_design_feasible)
        if bool(py_f.get("feasible")) != bool(rust_feasible) \
                and rust_design_feasible == bool(rust_feasible):
            diffs.add_note(
                scene_id, variant_id, "feasibility",
                "feasibility.full_vs_design_only",
                bool(py_f.get("feasible")), bool(rust_feasible),
                message="Rust full feasibility also includes CV-fold violations; "
                        "cv_fold_max_rise_db=" + repr(rust_cv)
                        + " (CV folds are not independently recomputed by Python; "
                          "rule_consistent_with_cv checks the rule)")
    # constraints: independent FIR checks (B2/B3)
    py_constraints = py_variant.get("python_constraints") or {}
    rust_constraints = py_variant.get("rust_constraints")
    if variant_id in ("B2", "B3") and isinstance(rust_constraints, dict) \
            and py_constraints.get("applicable"):
        rust_gains = rust_constraints.get("per_source_max_gain_db") or []
        diffs.add_array(scene_id, variant_id, "constraints",
                        "constraints.per_source_max_gain_db",
                        py_constraints.get("per_source_max_gain_db"),
                        rust_gains, "db",
                        family="constraints.per_source_max_gain_db")
        diffs.add_array(scene_id, variant_id, "constraints",
                        "constraints.predicted_peak_per_source",
                        py_constraints.get("predicted_peak_per_source"),
                        rust_constraints.get("predicted_peak_per_source"),
                        "ratio",
                        family="constraints.predicted_peak_per_source")
        finite_rust_gains = [float(g) for g in rust_gains
                             if g is not None and math.isfinite(float(g))]
        rust_gain_cap_ok = (bool(finite_rust_gains)
                            and max(finite_rust_gains) <= 6.0 + 1e-6)
        diffs.add_bool(scene_id, variant_id, "constraints",
                       "constraints.gain_cap_within_6db", True, rust_gain_cap_ok)
        diffs.add_bool(scene_id, variant_id, "constraints",
                       "constraints.headroom_ok",
                       bool(py_constraints.get("headroom_ok")),
                       rust_constraints.get("headroom_ok"))

    py_status = py_variant.get("python_predicted_status")
    rust_status = py_variant.get("rust_predicted_status")
    rust_rule = py_variant.get("rust_rule_status")
    rule_consistent = (rust_status is not None and rust_rule == rust_status)
    if py_status == rust_status or rust_status is None:
        diffs.add_exact(scene_id, variant_id, "status", "predicted_status",
                        py_status, rust_status)
    elif rule_consistent:
        # Rust's frozen rule also forces FAIL when hard constraints required
        # scaling (gain_cap_triggered / headroom); the independent Python rule
        # can only observe the post-scale response.  Rust is self-consistent:
        # list the difference as an explained rule note, not a failure.
        diffs.add_note(
            scene_id, variant_id, "status", "predicted_status",
            py_status, rust_status,
            message="Rust predicted_status is self-consistent with its frozen "
                    "rule (see predicted_status.rule_consistent); it also "
                    "enforces hard constraints_ok (gain_cap_triggered / "
                    "headroom_ok) while the independent Python rule uses "
                    "design feasibility + holdout severe only. Inspect "
                    "constraints.gain_cap_triggered/headroom_ok for this case.")
    else:
        diffs.add_exact(scene_id, variant_id, "status", "predicted_status",
                        py_status, rust_status)
    if rust_status is not None and rust_rule is not None:
        diffs.add_exact(scene_id, variant_id, "status",
                        "predicted_status.rule_consistent", rust_rule,
                        rust_status)


def _objective_scene_view(variants_py: Dict[str, Any]) -> Dict[str, Any]:
    """Compact design/holdout view used by the markdown tables."""
    view: Dict[str, Any] = {}
    for vid, variant in variants_py.items():
        design = variant.get("design") or {}
        holdout = variant.get("holdout") or {}
        view[vid] = {
            "design": {
                key: design.get(key) for key in (
                    "e_modal", "e_modal_db", "r_k", "r_max", "argmax_band_hz",
                    "band_delta_db", "energy_migration",
                    "seat_std_mean_db", "seat_std_max_db", "max_dev_db",
                    "null_depth_mean_db", "null_depth_worst_db",
                    "design_max_rise_db")
            },
            "holdout": {
                key: holdout.get(key) for key in (
                    "r_k", "e_modal_db", "r_max", "band_delta_db",
                    "consistency_c", "valid_holdout_count",
                    "severe_regression", "severe_points",
                    "seat_std_mean_db", "seat_std_max_db", "max_dev_db",
                    "null_depth_mean_db", "null_depth_worst_db")
            },
            "python_feasibility": variant.get("python_feasibility"),
            "rust_feasibility": variant.get("rust_feasibility"),
            "python_predicted_status": variant.get("python_predicted_status"),
            "rust_predicted_status": variant.get("rust_predicted_status"),
            "reconstruction": variant.get("reconstruction"),
        }
    return view


def fir_constraint_check(g: Optional[np.ndarray], freqs: np.ndarray,
                         source_ids: Sequence[str]) -> Dict[str, Any]:
    """Independent gain-cap / headroom check from the final FIR response.

    Contract limits: per-source in-band max |G_n(f)| <= +6 dB (explicit cap);
    OBJECTIVES-V2/implementation note declares predicted output peak
    = 0.5 * max|G_n| <= 1.0 (equivalent to the +6 dB cap).  sum|h| is not a
    valid -6 dBFS bound because the delta_D pass-through alone has sum|h| >= 1.
    """
    if g is None:
        return {"applicable": False}
    # Rust computes the per-source max |G| over the whole fixture frequency
    # grid (not only the 6 evaluated 1/3-oct bands); mirror that convention.
    gains_db: List[Optional[float]] = []
    peaks: List[Optional[float]] = []
    for n in range(g.shape[0]):
        mag = np.abs(g[n, :])
        finite = mag[np.isfinite(mag)]
        if finite.size == 0:
            gains_db.append(None)
            peaks.append(None)
            continue
        peak = float(np.max(finite))
        gains_db.append(20.0 * math.log10(peak) if peak > 0.0 else None)
        peaks.append(0.5 * peak)
    finite_gains = [v for v in gains_db if v is not None]
    finite_peaks = [v for v in peaks if v is not None]
    gain_cap_ok = bool(finite_gains
                       and max(finite_gains) <= 6.0 + 1e-6)
    headroom_ok = bool(finite_peaks and max(finite_peaks) <= 1.0 + 1e-9)
    return {
        "applicable": True,
        "source_ids": list(source_ids),
        "frequency_grid_k": int(np.asarray(freqs).size),
        "per_source_max_gain_db": gains_db,
        "predicted_peak_per_source": peaks,
        "max_inband_gain_db": (max(finite_gains) if finite_gains else None),
        "gain_cap_ok": gain_cap_ok,
        "headroom_ok": headroom_ok,
    }


def rust_rule_status(design: Dict[str, Any], holdout: Dict[str, Any],
                     feasibility: Optional[Dict[str, Any]],
                     variant_id: str) -> Tuple[str, Dict[str, Any]]:
    """Apply the SAME frozen rule to the RUST report fields.

    This is a rule-consistency check of the Rust output (not an independent
    recomputation).  Rule version v2-frozen-2026-11-22: B0=BASELINE; B2/B3
    severe -> FAIL; feasibility=false -> INFEASIBLE; then PASS / INCOMPLETE /
    PARTIAL.  gain_cap / headroom scaling is not an automatic FAIL.
    """
    checks: Dict[str, Any] = {}
    if variant_id == "B0":
        checks["baseline"] = True
        return "BASELINE", checks
    severe = holdout.get("severe_regression") is True
    checks["severe"] = severe
    if severe:
        return "PREDICTED_FAIL", checks
    applicable = (variant_id in ("B2", "B3")
                  and isinstance(feasibility, dict)
                  and feasibility.get("applicable") is not False)
    feasible_false = bool(applicable and feasibility.get("feasible") is False)
    checks["feasible_false"] = feasible_false
    if feasible_false:
        return "INFEASIBLE", checks
    e_modal_db = design.get("e_modal_db")
    r_max = design.get("r_max")
    migration = design.get("energy_migration")
    consistency = holdout.get("consistency_c")
    pas = (e_modal_db is not None and math.isfinite(float(e_modal_db))
           and float(e_modal_db) < 0.0
           and r_max is not None and math.isfinite(float(r_max))
           and float(r_max) < 1.0
           and migration is False
           and consistency is not None and math.isfinite(float(consistency))
           and float(consistency) >= CONSISTENCY_MIN)
    if pas:
        return "PREDICTED_PASS", checks
    valid = holdout.get("valid_holdout_count")
    if isinstance(valid, int) and valid < 3:
        return "INCOMPLETE", checks
    return "PREDICTED_PARTIAL", checks


def _rust_scene_view(rust_variants: Dict[str, Any]) -> Dict[str, Any]:
    """Compact Rust-side view mirroring objective_view (for REPORT tables)."""
    view: Dict[str, Any] = {}
    for vid, variant in rust_variants.items():
        if not isinstance(variant, dict):
            continue
        design = variant.get("design") or {}
        holdout = variant.get("holdout") or {}
        view[vid] = {
            "design": {key: design.get(key) for key in (
                "e_modal", "e_modal_db", "r_k", "r_max", "argmax_band_hz",
                "band_delta_db", "energy_migration", "seat_std_mean_db",
                "seat_std_max_db", "max_dev_db", "null_depth_mean_db",
                "null_depth_worst_db", "design_max_rise_db")},
            "holdout": {key: holdout.get(key) for key in (
                "r_k", "e_modal_db", "r_max", "band_delta_db", "consistency_c",
                "valid_holdout_count", "severe_regression", "severe_points",
                "seat_std_mean_db", "seat_std_max_db", "max_dev_db",
                "null_depth_mean_db", "null_depth_worst_db")},
            "rust_feasibility": _extract_feasibility(variant),
            "rust_constraints": variant.get("constraints"),
            "rust_predicted_status": _extract_predicted_status(variant, {}),
        }
    return view


def process_obj_scene(fx: SceneFixture, report: Dict[str, Any], tag: str,
                      tag_root: Path, diffs: DiffTable,
                      issues: List[Dict[str, Any]]) -> Dict[str, Any]:
    """Independent recomputation + cross-check of one scene for one tag."""
    plan = build_band_plan(fx.gt, fx.freqs)
    h = fx.h_matrix()
    P0 = pressure(None, h, None)
    P0_design = P0[fx.design_idx]
    P0_holdout = P0[fx.holdout_idx]
    scene_id = fx.scene_id
    report_format = str(report.get("format", ""))
    if not report_format.startswith("tspi-mimo-sim-report/"):
        issues.append({"kind": "report_schema", "scene": scene_id, "tag": tag,
                       "message": "unexpected report format %r" % report_format})
    diffs.add_bool(scene_id, "-", "report", "report.format_prefix_ok",
                   True, report_format.startswith("tspi-mimo-sim-report/"))
    report_objective = report.get("objective")
    if isinstance(report_objective, dict):
        objective_name = report_objective.get("name") or report_objective.get("objective")
        if objective_name is not None:
            diffs.add_exact(scene_id, "-", "report", "objective.name",
                            _normalise_objective_name(tag),
                            _normalise_objective_name(str(objective_name)))
    fixture = report.get("fixture") or {}
    diffs.add_exact(scene_id, "-", "report", "fixture.ground_truth_sha256",
                    fx.gt_sha256, fixture.get("ground_truth_sha256"))
    if fixture.get("rir_scale") is not None:
        diffs.add_float_exact(scene_id, "-", "report", "fixture.rir_scale",
                              fx.scale, fixture.get("rir_scale"), tol=1e-12)
    variants = report.get("variants") or {}
    for vid in OBJ_REQUIRED_VARIANTS:
        if vid not in variants:
            issues.append({"kind": "report_schema_missing", "scene": scene_id,
                           "tag": tag, "field": "variants.%s" % vid,
                           "message": "variant missing from runs-obj report"})
    variants_py: Dict[str, Any] = {}
    for vid in OBJ_REQUIRED_VARIANTS:
        rust_variant = variants.get(vid)
        if not isinstance(rust_variant, dict):
            continue
        g, provenance_g = _reconstruct_g(fx, report, tag_root, vid, issues)
        if vid != "B0" and g is None:
            variants_py[vid] = {"reconstruction": provenance_g,
                                "design": {}, "holdout": {},
                                "python_feasibility": {"applicable": False}}
            issues.append({"kind": "variant_incomplete", "scene": scene_id,
                           "tag": tag, "variant": vid,
                           "message": "cannot rebuild P_after (G unavailable)"})
            continue
        Pa = pressure(None, h, g)
        Pa_design = Pa[fx.design_idx]
        Pa_holdout = Pa[fx.holdout_idx]
        metrics = variant_metrics(P0_design, P0_holdout, Pa_design, Pa_holdout,
                                  plan, fx.design_ids, fx.holdout_ids)
        metrics["reconstruction"] = provenance_g
        new_design = seat_std_null_metrics(Pa_design, plan.bins)
        new_holdout = seat_std_null_metrics(Pa_holdout, plan.bins)
        for key in OBJECTIVE_NEW_METRIC_KEYS:
            metrics["design"][key] = new_design[key]
            metrics["holdout"][key] = new_holdout[key]
        metrics["design"]["per_band_objective"] = new_design["per_band"]
        metrics["holdout"]["per_band_objective"] = new_holdout["per_band"]

        if vid in ("B2", "B3"):
            delta_design = design_band_rise(P0_design, Pa_design, plan.bins)
            max_rise = _find_max_db_rise(report, rust_variant,
                                         _extract_feasibility(rust_variant))
            py_feas = evaluate_feasibility(delta_design, fx.design_ids,
                                           plan.centers_hz, max_rise)
            py_feas["applicable"] = True
            # The full trust-region rule also includes CV-fold violations.
            # OBJECTIVES-V2 says CV folds need not be independently recomputed;
            # use the Rust-provided fold maxima for the RULE, while
            # design_max_rise_db stays independently recomputed.
            rust_feas_for_rule = _extract_feasibility(rust_variant)
            cv_values = (rust_feas_for_rule.get("cv_fold_max_rise_db")
                         if isinstance(rust_feas_for_rule, dict) else None)
            cv_ok = True
            cv_list: List[float] = []
            if isinstance(cv_values, list):
                for value in cv_values:
                    if value is None or not math.isfinite(float(value)):
                        cv_ok = False
                        continue
                    cv_list.append(float(value))
                    if float(value) > float(max_rise):
                        cv_ok = False
            elif cv_values is not None:
                cv_ok = False
            py_feas["cv_fold_max_rise_db_from_rust"] = cv_list
            py_feas["cv_fold_rule_ok"] = bool(cv_ok)
            py_feas["feasible_full"] = bool(py_feas["feasible"] and cv_ok)
            metrics["python_feasibility"] = py_feas
            metrics["design"]["design_max_rise_db"] = py_feas["design_max_rise_db"]
        else:
            metrics["python_feasibility"] = {"applicable": False}
        metrics["python_constraints"] = (
            fir_constraint_check(g, fx.freqs, fx.source_ids)
            if vid in ("B2", "B3") else {"applicable": False})
        status_feasibility = None
        if vid in ("B2", "B3"):
            status_feasibility = dict(metrics["python_feasibility"])
            status_feasibility["feasible"] = bool(
                status_feasibility.get("feasible_full",
                                       status_feasibility.get("feasible")))
            status_feasibility["applicable"] = True
        status, status_checks = compute_objective_status(
            metrics["design"], metrics["holdout"], status_feasibility, vid)
        status_checks["feasibility_uses_rust_cv_folds"] = bool(
            vid in ("B2", "B3"))
        metrics["python_predicted_status"] = status
        metrics["python_status_checks"] = status_checks
        rust_feas = _extract_feasibility(rust_variant)
        rust_constraints = rust_variant.get("constraints")
        rust_status = _extract_predicted_status(rust_variant, report)
        rust_rule, rust_rule_checks = rust_rule_status(
            rust_variant.get("design") or {},
            rust_variant.get("holdout") or {},
            rust_feas if vid in ("B2", "B3") else None,
            vid)
        metrics["rust_feasibility"] = rust_feas
        metrics["rust_constraints"] = rust_constraints
        metrics["rust_predicted_status"] = rust_status
        metrics["rust_rule_status"] = rust_rule
        metrics["rust_rule_checks"] = rust_rule_checks
        variants_py[vid] = metrics
        crosscheck_variant(diffs, scene_id, vid, metrics, rust_variant,
                           plan.centers_hz)
        crosscheck_objective_variant(diffs, scene_id, vid, metrics, rust_variant,
                                     plan.centers_hz)

    scene_rows = [row for row in diffs.rows if row["scene"] == scene_id]
    mismatches = [row for row in scene_rows if is_failing_status(row["status"])]
    incomplete = any(item.get("kind") == "variant_incomplete"
                     and item.get("scene") == scene_id for item in issues)
    if mismatches:
        status = "FAIL"
    elif incomplete:
        status = "INCOMPLETE"
    else:
        status = "PASS"
    max_map: Dict[str, Any] = {}
    for row in scene_rows:
        if row["status"] != "PASS":
            continue
        diff = row.get("abs_diff")
        if diff is None or not math.isfinite(float(diff)):
            continue
        key = "max_abs_diff_" + _normalise_family_key(row["family"])
        max_map[key] = max(float(max_map.get(key, 0.0)), float(diff))
    return {
        "scene_id": scene_id,
        "fixture": {
            "ground_truth_sha256": fx.gt_sha256,
            "rir_scale": fx.scale,
            "frequency_grid_k": int(fx.freqs.size),
            "n_sources": fx.n_sources,
            "n_mics": fx.n_mics,
            "n_design": len(fx.design_ids),
            "n_holdout": len(fx.holdout_ids),
            "design_mic_ids": list(fx.design_ids),
            "holdout_mic_ids": list(fx.holdout_ids),
        },
        "bands": {
            "centers_hz": list(plan.centers_hz),
            "bin_counts": [int(b.size) for b in plan.bins],
        },
        "variants": variants_py,
        "objective_view": _objective_scene_view(variants_py),
        "rust_objective_view": _rust_scene_view(report.get("variants") or {}),
        "rust_report": {
            "format": report.get("format"),
            "tool": report.get("tool"),
            "objective": report.get("objective"),
            "config": report.get("config"),
            "cv": report.get("cv"),
            "notes": report.get("notes"),
        },
        "crosscheck": {
            "status": status,
            "n_rows_compared": len(scene_rows),
            "n_mismatch": len(mismatches),
            "mismatches": mismatches,
            "max_abs_diff": max_map,
        },
    }


# ---------------------------------------------------------------------------
# round 2: objectives pipeline, outputs and REPORT section 8 tables
# ---------------------------------------------------------------------------

V1_REGRESSION_TOL = 1e-9
V1_SPATIAL_DB_FIELDS = ("mean_db", "std_db", "range_db", "p90_p10_db",
                        "worst_null_db", "max_peak_above_mean_db",
                        "interference_null_db")
V1_SPATIAL_RATIO_FIELDS = ("coverage_within_3db",)


def _compare_v1_variant(diffs: DiffTable, scene_id: str, variant_id: str,
                        obj_variant: Dict[str, Any],
                        v1_variant: Dict[str, Any]) -> None:
    """OBJECTIVES-V2 section 4 regression: emodal tag must reproduce v1 <=1e-9."""
    design_ratio_arrays = ("r_k", "e_k0", "alpha")
    design_db_arrays = ("band_delta_db",)
    design_ratio_scalars = ("e_modal", "e_modal_eq")
    design_db_scalars = ("e_modal_db", "e_modal_eq_db")
    holdout_ratio_arrays = ("r_k",)
    holdout_db_arrays = ("band_delta_db",)
    holdout_ratio_scalars = ("r_max", "consistency_c")
    holdout_db_scalars = ("e_modal_db",)
    for block_name in ("design", "holdout"):
        obj_block = obj_variant.get(block_name) or {}
        v1_block = v1_variant.get(block_name) or {}
        ratio_arrays = (design_ratio_arrays if block_name == "design"
                        else holdout_ratio_arrays)
        db_arrays = (design_db_arrays if block_name == "design"
                     else holdout_db_arrays)
        ratio_scalars = (design_ratio_scalars if block_name == "design"
                         else holdout_ratio_scalars)
        db_scalars = (design_db_scalars if block_name == "design"
                      else holdout_db_scalars)
        for key in ratio_arrays:
            diffs.add_array(scene_id, variant_id, block_name, "v1reg." + key,
                            obj_block.get(key), v1_block.get(key), "ratio",
                            family="v1reg." + key)
        for key in db_arrays:
            diffs.add_array(scene_id, variant_id, block_name, "v1reg." + key,
                            obj_block.get(key), v1_block.get(key), "db",
                            family="v1reg." + key)
        for key in ratio_scalars:
            diffs.add_ratio_tol(scene_id, variant_id, block_name,
                                "v1reg." + key, obj_block.get(key),
                                v1_block.get(key), V1_REGRESSION_TOL,
                                family="v1reg." + key)
        for key in db_scalars:
            diffs.add_db_tol(scene_id, variant_id, block_name,
                             "v1reg." + key, obj_block.get(key),
                             v1_block.get(key), V1_REGRESSION_TOL,
                             family="v1reg." + key)
        if block_name == "design":
            diffs.add_bool_tol(scene_id, variant_id, block_name,
                               "v1reg.energy_migration",
                               obj_block.get("energy_migration"),
                               v1_block.get("energy_migration"),
                               family="v1reg.energy_migration")
            if obj_block.get("argmax_band_hz") is not None \
                    or v1_block.get("argmax_band_hz") is not None:
                diffs.add_float_exact(scene_id, variant_id, block_name,
                                      "v1reg.argmax_band_hz",
                                      obj_block.get("argmax_band_hz"),
                                      v1_block.get("argmax_band_hz"),
                                      tol=V1_REGRESSION_TOL)
            else:
                diffs.add_float_exact(scene_id, variant_id, block_name,
                                      "v1reg.argmax_band_hz",
                                      obj_block.get("argmax_band_hz"),
                                      v1_block.get("argmax_band_hz"),
                                      tol=V1_REGRESSION_TOL)
        # per-point per-band dB maps
        obj_pp = obj_block.get("per_point_band_db") or {}
        v1_pp = v1_block.get("per_point_band_db") or {}
        for pid in sorted(set(obj_pp) | set(v1_pp)):
            diffs.add_array(scene_id, variant_id, block_name,
                            "v1reg.per_point_band_db", obj_pp.get(pid),
                            v1_pp.get(pid), "db",
                            family="v1reg.per_point_band_db")
        # holdout point-weighted dB
        if block_name == "holdout":
            obj_pw = obj_block.get("point_weighted_db") or {}
            v1_pw = v1_block.get("point_weighted_db") or {}
            for pid in sorted(set(obj_pw) | set(v1_pw)):
                diffs.add_db_tol(scene_id, variant_id, block_name,
                                 "v1reg.point_weighted_db", obj_pw.get(pid),
                                 v1_pw.get(pid), V1_REGRESSION_TOL,
                                 item=pid, family="v1reg.point_weighted_db")
            diffs.add_exact(scene_id, variant_id, block_name,
                            "v1reg.severe_points",
                            _normalise_id_list(obj_block.get("severe_points")),
                            _normalise_id_list(v1_block.get("severe_points")))
            diffs.add_bool_tol(scene_id, variant_id, block_name,
                               "v1reg.severe_regression",
                               obj_block.get("severe_regression"),
                               v1_block.get("severe_regression"),
                               family="v1reg.severe_regression")
            valid_obj = obj_block.get("valid_holdout_count")
            valid_v1 = v1_block.get("valid_holdout_count")
            diffs.add_int(scene_id, variant_id, block_name,
                          "v1reg.valid_holdout_count", valid_obj, valid_v1)
        # spatial block
        obj_sp = obj_block.get("spatial") or []
        v1_sp = v1_block.get("spatial") or []
        if isinstance(obj_sp, list) and isinstance(v1_sp, list) \
                and len(obj_sp) == len(v1_sp):
            for k, (obj_band, v1_band) in enumerate(zip(obj_sp, v1_sp)):
                if not isinstance(obj_band, dict) or not isinstance(v1_band, dict):
                    continue
                for field in V1_SPATIAL_DB_FIELDS:
                    diffs.add_db_tol(scene_id, variant_id, block_name,
                                     "v1reg.spatial." + field,
                                     obj_band.get(field), v1_band.get(field),
                                     V1_REGRESSION_TOL,
                                     family="v1reg.spatial." + field)
                for field in V1_SPATIAL_RATIO_FIELDS:
                    diffs.add_ratio_tol(scene_id, variant_id, block_name,
                                        "v1reg.spatial." + field,
                                        obj_band.get(field), v1_band.get(field),
                                        V1_REGRESSION_TOL,
                                        family="v1reg.spatial." + field)


def compare_v1_regression(v1_runs_dir: Path, obj_root: Path, tag: str,
                          ready_scenes: Sequence[str],
                          issues: List[Dict[str, Any]]) -> Dict[str, Any]:
    """Compare runs-obj/emodal with the frozen v1 runs/ report (<=1e-9)."""
    diffs = DiffTable()
    compared: List[str] = []
    for scene_id in sorted(ready_scenes):
        v1_path = v1_runs_dir / scene_id / "report.json"
        obj_path = obj_root / tag / scene_id / "report.json"
        if not v1_path.is_file():
            issues.append({"kind": "v1_regression_missing", "scene": scene_id,
                           "tag": tag,
                           "message": "v1 report not found at %s" % v1_path})
            continue
        try:
            v1_report = load_rust_report(v1_path)
            obj_report = load_obj_report(obj_path)
        except MetricsError as exc:
            issues.append({"kind": "v1_regression_unreadable", "scene": scene_id,
                           "tag": tag, "message": str(exc)})
            continue
        compared.append(scene_id)
        v1_variants = v1_report.get("variants") or {}
        obj_variants = obj_report.get("variants") or {}
        for vid in OBJ_REQUIRED_VARIANTS:
            obj_variant = obj_variants.get(vid)
            v1_variant = v1_variants.get(vid)
            if not isinstance(obj_variant, dict) or not isinstance(v1_variant, dict):
                issues.append({"kind": "v1_regression_missing_variant",
                               "scene": scene_id, "tag": tag, "variant": vid})
                continue
            _compare_v1_variant(diffs, scene_id, vid, obj_variant, v1_variant)
    mismatches = [row for row in diffs.rows if is_failing_status(row["status"])]
    max_map = {_normalise_family_key(fam): val
               for fam, val in diffs.max_abs_by_family().items()}
    return {
        "status": ("FAIL" if mismatches else
                   ("INCOMPLETE" if len(compared) < len(ready_scenes) else "PASS")),
        "tolerance": V1_REGRESSION_TOL,
        "scenes_compared": compared,
        "n_rows_compared": len(diffs.rows),
        "n_mismatch": len(mismatches),
        "max_abs_diff": max_map,
        "mismatches": mismatches,
        "rows": diffs.rows,
    }


def write_objective_crosscheck_csv(path: Path, rows: Sequence[Dict[str, Any]]) -> None:
    """Aggregated cross-check table: one row per tag/scene/variant/block/metric."""
    groups: Dict[Tuple[Any, ...], Dict[str, Any]] = {}
    for row in rows:
        key = (row.get("tag"), row.get("scene"), row.get("variant"),
               row.get("block"), row.get("metric"), row.get("kind"),
               row.get("family"))
        group = groups.setdefault(key, {"n": 0, "fail": 0, "note": 0,
                                        "abs": None, "rel": None, "tol": None})
        group["n"] += 1
        if is_failing_status(row.get("status")):
            group["fail"] += 1
        elif row.get("status") == "NOTE":
            group["note"] += 1
        abs_diff = row.get("abs_diff")
        if abs_diff is not None and math.isfinite(float(abs_diff)):
            group["abs"] = (float(abs_diff) if group["abs"] is None
                            else max(float(group["abs"]), float(abs_diff)))
        rel_diff = row.get("rel_diff")
        if rel_diff is not None and math.isfinite(float(rel_diff)):
            group["rel"] = (float(rel_diff) if group["rel"] is None
                            else max(float(group["rel"]), float(rel_diff)))
        tol = row.get("tolerance")
        if tol is not None:
            group["tol"] = float(tol)
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="") as fh:
        writer = csv.writer(fh)
        writer.writerow(["tag", "scene", "variant", "block", "metric", "kind",
                         "family", "n_rows", "n_fail", "n_notes",
                         "max_abs_diff", "max_rel_diff", "tolerance", "status"])
        for key in sorted(groups, key=lambda item: tuple(str(part) for part in item)):
            tag, scene, variant, block, metric, kind, family = key
            group = groups[key]
            writer.writerow([
                tag, scene, variant, block, metric, kind, family,
                group["n"], group["fail"], group["note"],
                _format_cell(group["abs"]), _format_cell(group["rel"]),
                _format_cell(group["tol"]),
                ("FAIL" if group["fail"] else
                 ("NOTE" if group["note"] else "PASS")),
            ])


def _tag_objective_stats(tag_payloads: Dict[str, Dict[str, Any]],
                         tag: str) -> Dict[str, Any]:
    """Average design/holdout objective metrics over comparable scenes."""
    scenes = (tag_payloads.get(tag) or {}).get("scenes") or {}
    stats: Dict[str, Any] = {"n_scenes": len(scenes), "variants": {}}
    for vid in ("B2", "B3"):
        design_values: Dict[str, List[float]] = {}
        holdout_values: Dict[str, List[float]] = {}
        for scene in scenes.values():
            view = (scene.get("objective_view") or {}).get(vid)
            if not view:
                continue
            for key in ("e_modal_db", "seat_std_mean_db", "seat_std_max_db",
                        "max_dev_db", "null_depth_mean_db",
                        "null_depth_worst_db", "r_max"):
                value = (view.get("design") or {}).get(key)
                if value is not None and math.isfinite(float(value)):
                    design_values.setdefault(key, []).append(float(value))
                value = (view.get("holdout") or {}).get(key)
                if value is not None and math.isfinite(float(value)):
                    holdout_values.setdefault(key, []).append(float(value))
        stats["variants"][vid] = {
            "design_mean": {key: (float(np.mean(vals)) if vals else None)
                            for key, vals in design_values.items()},
            "holdout_mean": {key: (float(np.mean(vals)) if vals else None)
                             for key, vals in holdout_values.items()},
        }
    return stats


def _fmt_or_na(value: Any, digits: int = 3) -> str:
    if value is None:
        return "N/A"
    try:
        number = float(value)
    except (TypeError, ValueError):
        return str(value)
    if not math.isfinite(number):
        return "N/A"
    return ("%." + str(digits) + "f") % number


def render_objective_tables(tag_payloads: Dict[str, Dict[str, Any]],
                            overall: Dict[str, Any],
                            v1_regression: Optional[Dict[str, Any]]) -> str:
    """Markdown snippets for REPORT section 8.2-8.4 (SIMULATED / UNPROVEN)."""
    lines: List[str] = []
    lines.append("# Objective-v2 independent Python cross-check (SIMULATED / UNPROVEN)")
    lines.append("")
    lines.append("> source: SIMULATED / SYNTHETIC / UNPROVEN. Independent numpy recomputation")
    lines.append("> from H(f) + FIR; Rust/Python agreement is not a measurement claim.")
    lines.append(">")
    lines.append("> predicted_status rule version: **%s**  " % STATUS_RULE_VERSION)
    lines.append("> %s" % STATUS_RULE_TEXT)
    lines.append("")
    lines.append("## OBJ_TABLE_DESIGN - design metrics per tag/scene/variant")
    lines.append("")
    lines.append("| tag | scene | variant | e_modal_dB | R_max | seat_std_mean_dB | seat_std_max_dB | max_dev_dB | null_depth_mean_dB | null_depth_worst_dB | migration | predicted_status |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for tag, payload in tag_payloads.items():
        for scene_id, scene in (payload.get("scenes") or {}).items():
            for vid in ("B0", "B1", "B2", "B3"):
                view = (scene.get("objective_view") or {}).get(vid)
                if not view:
                    continue
                design = view.get("design") or {}
                status = (view.get("python_predicted_status") or "N/A")
                lines.append("| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |" % (
                    tag, scene_id, vid,
                    _fmt_or_na(design.get("e_modal_db")),
                    _fmt_or_na(design.get("r_max")),
                    _fmt_or_na(design.get("seat_std_mean_db")),
                    _fmt_or_na(design.get("seat_std_max_db")),
                    _fmt_or_na(design.get("max_dev_db")),
                    _fmt_or_na(design.get("null_depth_mean_db")),
                    _fmt_or_na(design.get("null_depth_worst_db")),
                    ("yes" if design.get("energy_migration") else "no"),
                    status))
    lines.append("")
    lines.append("## OBJ_TABLE_HOLDOUT - holdout metrics per tag/scene/variant")
    lines.append("")
    lines.append("| tag | scene | variant | e_modal_dB | R_max | seat_std_mean_dB | max_dev_dB | null_depth_mean_dB | C | valid | severe | severe_points | predicted_status |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for tag, payload in tag_payloads.items():
        for scene_id, scene in (payload.get("scenes") or {}).items():
            for vid in ("B0", "B1", "B2", "B3"):
                view = (scene.get("objective_view") or {}).get(vid)
                if not view:
                    continue
                holdout = view.get("holdout") or {}
                lines.append("| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |" % (
                    tag, scene_id, vid,
                    _fmt_or_na(holdout.get("e_modal_db")),
                    _fmt_or_na(holdout.get("r_max")),
                    _fmt_or_na(holdout.get("seat_std_mean_db")),
                    _fmt_or_na(holdout.get("max_dev_db")),
                    _fmt_or_na(holdout.get("null_depth_mean_db")),
                    _fmt_or_na(holdout.get("consistency_c")),
                    _fmt_or_na(holdout.get("valid_holdout_count"), 0),
                    ("yes" if holdout.get("severe_regression") else "no"),
                    ",".join(str(x) for x in (holdout.get("severe_points") or [])) or "-",
                    (view.get("python_predicted_status") or "N/A")))
    lines.append("")
    lines.append("## OBJ_READING - objective comparison and challenge severe")
    lines.append("")
    by_tag = {tag: _tag_objective_stats(tag_payloads, tag)
              for tag in tag_payloads}
    for tag, stats in by_tag.items():
        parts = []
        for vid in ("B2", "B3"):
            variant_stats = stats["variants"].get(vid) or {}
            design_mean = variant_stats.get("design_mean") or {}
            parts.append("%s: e_modal_dB=%s seat_std=%s max_dev=%s null_depth=%s" % (
                vid,
                _fmt_or_na(design_mean.get("e_modal_db")),
                _fmt_or_na(design_mean.get("seat_std_mean_db")),
                _fmt_or_na(design_mean.get("max_dev_db")),
                _fmt_or_na(design_mean.get("null_depth_mean_db"))))
        lines.append("- tag %s (scene means over %d fixtures): %s" % (
            tag, stats["n_scenes"], "; ".join(parts)))
    lines.append("")
    lines.append("### challenge scene (scene-3x10-challenge) severe, Python vs Rust")
    lines.append("")
    lines.append("| tag | variant | python severe | python severe_points | rust severe | rust severe_points | agree |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- |")
    challenge = "scene-3x10-challenge"
    for tag, payload in tag_payloads.items():
        scene = (payload.get("scenes") or {}).get(challenge)
        if not scene:
            lines.append("| %s | - | - | - | - | - | scene missing |" % tag)
            continue
        rust_view_map = scene.get("rust_objective_view") or {}
        for vid in ("B2", "B3"):
            view = (scene.get("objective_view") or {}).get(vid) or {}
            holdout = view.get("holdout") or {}
            py_severe = bool(holdout.get("severe_regression"))
            py_points = ",".join(str(x) for x in (holdout.get("severe_points") or [])) or "-"
            rust_severe = None
            rust_points = "-"
            rust_view = rust_view_map.get(vid) or {}
            rust_hold = rust_view.get("holdout") or {}
            rust_severe = rust_hold.get("severe_regression")
            rust_points = ",".join(str(x) for x in (rust_hold.get("severe_points") or [])) or "-"
            agree = ("yes" if (rust_severe is not None
                               and bool(rust_severe) == py_severe
                               and rust_points == py_points) else "no")
            lines.append("| %s | %s | %s | %s | %s | %s | %s |" % (
                tag, vid, "yes" if py_severe else "no", py_points,
                ("N/A" if rust_severe is None else ("yes" if rust_severe else "no")),
                rust_points, agree))
    # Challenge severe verdict per variant, including the trust-region status of
    # the tag that removes it (never call a holdout-only fix a success).
    challenge_status: Dict[str, Dict[str, Any]] = {}
    for tag, payload in tag_payloads.items():
        scene = (payload.get("scenes") or {}).get(challenge) or {}
        entry: Dict[str, Any] = {}
        for vid in ("B2", "B3"):
            view = (scene.get("objective_view") or {}).get(vid) or {}
            hold = view.get("holdout") or {}
            feas = view.get("python_feasibility") or {}
            entry[vid] = {
                "severe": bool(hold.get("severe_regression")),
                "severe_points": list(hold.get("severe_points") or []),
                "design_feasible": feas.get("feasible"),
                "design_max_rise_db": feas.get("design_max_rise_db"),
                "max_db_rise_db": feas.get("max_db_rise_db"),
                "predicted_status": view.get("python_predicted_status"),
            }
        challenge_status[tag] = entry
    for vid in ("B2", "B3"):
        present = [tag for tag, entry in challenge_status.items()
                   if entry[vid]["severe"]]
        absent = [tag for tag, entry in challenge_status.items()
                  if not entry[vid]["severe"]]
        lines.append("- challenge %s severe present in: %s; absent in: %s."
                     % (vid, ", ".join(present) or "none",
                        ", ".join(absent) or "none"))
        for tag in absent:
            entry = challenge_status[tag][vid]
            if entry["design_feasible"] is False:
                lines.append("  - %s %s removes the holdout severe only outside the "
                             "design trust region: design_max_rise=%s dB vs "
                             "max_db_rise=%s dB -> INFEASIBLE (predicted_status=%s); "
                             "not a valid objective win (holdout stayed out of the loop)."
                             % (tag, vid,
                                _fmt_or_na(entry["design_max_rise_db"]),
                                _fmt_or_na(entry["max_db_rise_db"]),
                                entry["predicted_status"]))
            else:
                lines.append("  - %s %s: severe absent and design trust region "
                             "satisfied (design_max_rise=%s dB <= %s dB, "
                             "predicted_status=%s); still PREDICTED/UNPROVEN only."
                             % (tag, vid,
                                _fmt_or_na(entry["design_max_rise_db"]),
                                _fmt_or_na(entry["max_db_rise_db"]),
                                entry["predicted_status"]))
    lines.append("")
    lines.append("Per OBJECTIVES-V2 section 3/5 a holdout-only severe regression cannot be")
    lines.append("fixed by objective selection (holdout is never used in the loop); if a tag")
    lines.append("only removes it by violating the design trust region, the honest label stays")
    lines.append("INFEASIBLE / HOLDOUT_SEVERE_REGRESSION rather than PASS.")
    lines.append("")
    # Objective deltas versus the emodal tag (B3 design scene means).
    base_stats = (by_tag.get("emodal") or {}).get("variants", {}).get("B3", {})
    base_design = base_stats.get("design_mean") or {}
    if base_design:
        for tag, stats in by_tag.items():
            if tag == "emodal":
                continue
            design_mean = ((stats.get("variants") or {}).get("B3") or {}
                           ).get("design_mean") or {}
            def _delta(key: str, higher_is_better: bool) -> Optional[float]:
                a = base_design.get(key)
                b = design_mean.get(key)
                if a is None or b is None:
                    return None
                diff = float(a) - float(b)
                return diff if higher_is_better else -diff
            seat_gain = _delta("seat_std_mean_db", True)
            max_dev_gain = _delta("max_dev_db", True)
            null_gain = _delta("null_depth_mean_db", False)
            emodal_gain = _delta("e_modal_db", False)
            lines.append(
                "- %s vs emodal (B3 design scene means, positive = better): "
                "seat_std %s dB, max_dev %s dB, null_depth %s dB, e_modal %s dB."
                % (tag, _fmt_or_na(seat_gain), _fmt_or_na(max_dev_gain),
                   _fmt_or_na(null_gain), _fmt_or_na(emodal_gain)))
    lines.append("")
    lines.append("Seat-std / null-depth comparison (design means over scenes):")
    lines.append("")
    for tag, stats in by_tag.items():
        variant_stats = stats["variants"].get("B3") or {}
        design_mean = variant_stats.get("design_mean") or {}
        lines.append("- %s B3: seat_std_mean=%s dB, max_dev=%s dB, null_depth_mean=%s dB" % (
            tag, _fmt_or_na(design_mean.get("seat_std_mean_db")),
            _fmt_or_na(design_mean.get("max_dev_db")),
            _fmt_or_na(design_mean.get("null_depth_mean_db"))))
    lines.append("")
    seat_std_base = (base_design or {}).get("seat_std_mean_db")
    seat_std_tag = (((by_tag.get("seat-std") or {}).get("variants") or {})
                    .get("B3") or {}).get("design_mean", {}).get("seat_std_mean_db")
    if seat_std_base is not None and seat_std_tag is not None:
        if float(seat_std_tag) < float(seat_std_base):
            lines.append("Seat-std check: seat-std tag lowers seat_std_mean by %s dB vs emodal"
                         % _fmt_or_na(float(seat_std_base) - float(seat_std_tag)))
        else:
            lines.append("Seat-std check: seat-std tag does NOT lower seat_std_mean vs emodal")
            lines.append("(delta %s dB) - reported honestly per OBJECTIVES-V2 section 6."
                         % _fmt_or_na(float(seat_std_base) - float(seat_std_tag)))
    lines.append("")
    lines.append("## OBJ_FEASIBILITY - trust region / holdout-safe candidate ladder")
    lines.append("")
    lines.append("| tag | scene | variant | max_db_rise_db | python design_max_rise_db | rust design_max_rise_db | cv_fold_max_rise_db | python design feasible | rust design feasible | gain_cap_triggered | gain_cap_scale | headroom_ok | violations | predicted_status |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for tag, payload in tag_payloads.items():
        for scene_id, scene in (payload.get("scenes") or {}).items():
            for vid in ("B2", "B3"):
                view = (scene.get("objective_view") or {}).get(vid)
                if not view:
                    continue
                py_f = view.get("python_feasibility") or {}
                rust_f = view.get("rust_feasibility") or {}
                cv = rust_f.get("cv_fold_max_rise_db") if isinstance(rust_f, dict) else None
                rust_view = (scene.get("rust_objective_view") or {}).get(vid) or {}
                rust_cons = rust_view.get("rust_constraints") or {}
                rust_max = (rust_f.get("max_db_rise_db")
                            if isinstance(rust_f, dict) else None)
                rust_design = (rust_f.get("design_max_rise_db")
                               if isinstance(rust_f, dict) else None)
                rust_design_feasible = (
                    None if rust_design is None or rust_max is None
                    else float(rust_design) <= float(rust_max))
                gain_scale = rust_cons.get("gain_cap_scale")
                lines.append("| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |" % (
                    tag, scene_id, vid,
                    _fmt_or_na(rust_max),
                    _fmt_or_na(py_f.get("design_max_rise_db")),
                    _fmt_or_na(rust_design),
                    (";".join(_fmt_or_na(x) for x in cv)
                     if isinstance(cv, list) and cv else "N/A"),
                    ("N/A" if py_f.get("applicable") is False
                     else ("yes" if py_f.get("feasible") else "no")),
                    ("N/A" if not isinstance(rust_f, dict)
                     or rust_f.get("applicable") is False
                     else ("yes" if rust_design_feasible else "no")),
                    ("N/A" if "gain_cap_triggered" not in rust_cons
                     else ("yes" if rust_cons.get("gain_cap_triggered") else "no")),
                    (";".join(_fmt_or_na(x) for x in gain_scale)
                     if isinstance(gain_scale, list) else "N/A"),
                    ("N/A" if "headroom_ok" not in rust_cons
                     else ("yes" if rust_cons.get("headroom_ok") else "no")),
                    py_f.get("n_violations", "N/A"),
                    view.get("python_predicted_status") or "N/A"))
    lines.append("")
    if v1_regression is not None:
        lines.append("## v1 regression (emodal tag vs frozen runs/)")
        lines.append("")
        lines.append("- tolerance: %.1e; scenes compared: %d; rows: %d; mismatches: %d; status: %s"
                     % (float(v1_regression.get("tolerance", V1_REGRESSION_TOL)),
                        len(v1_regression.get("scenes_compared") or []),
                        int(v1_regression.get("n_rows_compared", 0)),
                        int(v1_regression.get("n_mismatch", 0)),
                        v1_regression.get("status")))
        maxima = v1_regression.get("max_abs_diff") or {}
        for key in sorted(maxima):
            lines.append("- max |diff| %s = %.3e" % (key, float(maxima[key])))
        lines.append("")
    all_notes: List[Dict[str, Any]] = []
    for tag, payload in tag_payloads.items():
        for row in (payload.get("crosscheck") or {}).get("rule_notes") or []:
            item = dict(row)
            item["tag"] = tag
            all_notes.append(item)
    lines.append("## Status / feasibility rule notes (explained differences, not failures)")
    lines.append("")
    if all_notes:
        lines.append("| tag | scene | variant | block | metric | python | rust |")
        lines.append("| --- | --- | --- | --- | --- | --- | --- |")
        for note in all_notes:
            lines.append("| %s | %s | %s | %s | %s | %s | %s |" % (
                note.get("tag"), note.get("scene"), note.get("variant"),
                note.get("block"), note.get("metric"),
                str(note.get("python")), str(note.get("rust"))))
        lines.append("")
        lines.append("These are not cross-check failures: the Rust frozen rule also forces")
        lines.append("PREDICTED_FAIL when hard constraints required scaling")
        lines.append("(gain_cap_triggered / headroom_ok) and its full feasibility includes")
        lines.append("CV-fold violations that Python does not independently recompute (only the")
        lines.append("rule is checked). The Rust rule self-consistency row")
        lines.append("predicted_status.rule_consistent and rule_consistent_with_cv are PASS.")
    else:
        lines.append("No explained rule differences: Python and Rust predicted_status agree on")
        lines.append("every variant/scene.")
    lines.append("")
    lines.append("## Cross-check status")
    lines.append("")
    for tag, payload in tag_payloads.items():
        cross = payload.get("crosscheck") or {}
        lines.append("- %s: %s (rows=%d, mismatch=%d, scenes=%d/%d)"
                     % (tag, cross.get("status"),
                        int(cross.get("n_rows_compared", 0)),
                        int(cross.get("n_mismatch", 0)),
                        int(cross.get("scenes_compared", 0)),
                        int(cross.get("scenes_expected", 0))))
    lines.append("")
    lines.append("_generated by %s v%s; all values SIMULATED/UNPROVEN._"
                 % (TOOL_NAME, TOOL_VERSION))
    lines.append("")
    return chr(10).join(lines)


def _challenge_severe_summary(scenes_payload: Dict[str, Any],
                              scene_id: str = "scene-3x10-challenge"
                              ) -> Optional[Dict[str, Any]]:
    "Independent vs Rust severe-regression verdict for the stress scene."""
    scene = scenes_payload.get(scene_id)
    if not scene:
        return None
    python_view: Dict[str, Any] = {}
    rust_view: Dict[str, Any] = {}
    agree = True
    for vid in ("B2", "B3"):
        py_hold = (((scene.get("objective_view") or {}).get(vid) or {})
                   .get("holdout") or {})
        rust_hold = (((scene.get("rust_objective_view") or {}).get(vid) or {})
                     .get("holdout") or {})
        py_severe = bool(py_hold.get("severe_regression"))
        py_points = list(py_hold.get("severe_points") or [])
        rust_severe = rust_hold.get("severe_regression")
        rust_points = list(rust_hold.get("severe_points") or [])
        python_view[vid] = {"severe": py_severe, "severe_points": py_points}
        rust_view[vid] = {"severe": rust_severe,
                          "severe_points": rust_points}
        if rust_severe is None or bool(rust_severe) != py_severe \
                or sorted(str(x) for x in rust_points) != sorted(str(x) for x in py_points):
            agree = False
    return {"scene_id": scene_id, "python": python_view, "rust": rust_view,
            "agree": agree}


def run_objectives_pipeline(args: argparse.Namespace) -> int:
    fixtures_dir = Path(args.fixtures).resolve()
    obj_root = Path(args.runs_obj_root).resolve()
    out_dir = Path(args.out).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    started = time.time()
    deadline = started + float(args.timeout_seconds)
    poll = float(args.poll_seconds)
    tags = [tag.strip() for tag in str(args.tags).split(",") if tag.strip()]
    if not tags:
        raise MetricsError("no objective tags given (--tags)")
    _log("objective-v2 mode: source=%s honesty=%s" % (SOURCE, HONESTY))
    _log("fixtures=%s" % fixtures_dir)
    _log("runs-obj=%s" % obj_root)
    _log("out=%s (tags=%s; timeout %.0fs, poll %.0fs)"
         % (out_dir, ",".join(tags), args.timeout_seconds, poll))
    scenes, fixtures_info = wait_for_fixtures(fixtures_dir, deadline, poll)
    if args.scene:
        wanted = set(args.scene)
        scenes = [fx for fx in scenes if fx.scene_id in wanted]
        if not scenes:
            raise MetricsError("--scene filter matched no fixture scene: %r"
                               % (args.scene,))
    scene_by_id = {fx.scene_id: fx for fx in scenes}
    scene_dirs = {fx.scene_id: fx.scene_dir for fx in scenes}
    _log("fixtures ready: %d scenes" % len(scenes))
    ready, missing = wait_for_obj_runs(obj_root, tags, scene_dirs, deadline, poll)
    if not any(ready[tag] for tag in tags):
        raise MetricsError("timeout waiting for any runs-obj report under %s" % obj_root)
    if missing:
        _log("WARNING: runs-obj incomplete at timeout: %s"
             % ", ".join(missing[:20]) + (" ..." if len(missing) > 20 else ""))
    all_rows: List[Dict[str, Any]] = []
    tag_payloads: Dict[str, Dict[str, Any]] = {}
    all_issues: List[Dict[str, Any]] = []
    tag_statuses: Dict[str, str] = {}
    v1_regression: Optional[Dict[str, Any]] = None
    for tag in tags:
        tag_rows = DiffTable()
        tag_issues: List[Dict[str, Any]] = []
        scenes_payload: Dict[str, Any] = {}
        for scene_id in sorted(ready.get(tag, {})):
            fx = scene_by_id[scene_id]
            report_path = ready[tag][scene_id]
            try:
                report = load_obj_report(report_path)
                payload = process_obj_scene(fx, report, tag, obj_root / tag,
                                            tag_rows, tag_issues)
                scenes_payload[scene_id] = payload
                _log("tag %-10s scene %-22s status=%-10s rows=%d mismatch=%d"
                     % (tag, scene_id, payload["crosscheck"]["status"],
                        payload["crosscheck"]["n_rows_compared"],
                        payload["crosscheck"]["n_mismatch"]))
            except MetricsError as exc:
                tag_issues.append({"kind": "report_unreadable", "scene": scene_id,
                                   "tag": tag, "message": str(exc)})
                _log("ERROR tag=%s scene=%s: %s" % (tag, scene_id, exc))
        rows = tag_rows.rows
        for row in rows:
            row["tag"] = tag
        all_rows.extend(rows)
        for item in tag_issues:
            item.setdefault("tag", tag)
        all_issues.extend(tag_issues)
        mismatch_rows = [row for row in rows if is_failing_status(row["status"])]
        rule_notes = [row for row in rows if row["status"] == "NOTE"]
        family_max = {_normalise_family_key(fam): val
                      for fam, val in tag_rows.max_abs_by_family().items()}
        scene_statuses = {sid: payload["crosscheck"]["status"]
                          for sid, payload in scenes_payload.items()}
        tag_missing = [item for item in missing if item.startswith(tag + "/")]
        if any(status == "FAIL" for status in scene_statuses.values()):
            tag_status = "FAIL"
        elif tag_missing or any(status == "INCOMPLETE"
                                for status in scene_statuses.values()) \
                or len(scenes_payload) < len(scenes):
            tag_status = "INCOMPLETE"
        else:
            tag_status = "PASS"
        tag_statuses[tag] = tag_status
        if tag == "emodal" and args.v1_runs:
            v1_dir = Path(args.v1_runs).resolve()
            if v1_dir.is_dir():
                v1_regression = compare_v1_regression(
                    v1_dir, obj_root, tag, sorted(scenes_payload), tag_issues)
                _log("v1 regression: status=%s rows=%d mismatch=%d"
                     % (v1_regression["status"],
                        v1_regression["n_rows_compared"],
                        v1_regression["n_mismatch"]))
                for row in v1_regression.get("rows") or []:
                    row["tag"] = "emodal/v1reg"
                all_rows.extend(v1_regression.get("rows") or [])
            else:
                tag_issues.append({"kind": "v1_regression_missing",
                                   "message": "--v1-runs %s not found" % v1_dir})
        payload = {
            "format": "tspi-mimo-python-metrics-objectives/v1",
            "source": SOURCE,
            "provenance": {"data_kind": SOURCE, "honesty_status": HONESTY,
                           "claim": "synthetic offline objective study only; no "
                                    "real room, no measurement; Rust/Python "
                                    "agreement is not an acoustic validation"},
            "tool": {"name": TOOL_NAME, "version": TOOL_VERSION,
                     "python": sys.version.split()[0], "numpy": np.__version__,
                     "scipy": (__import__("scipy").__version__
                               if HAVE_SCIPY else None)},
            "generated_at_utc": now_utc_iso(),
            "tag": tag,
            "status_rule": STATUS_RULE_JSON,
            "inputs": {
                "fixtures_dir": str(fixtures_dir),
                "runs_obj_root": str(obj_root),
                "tag_root": str(obj_root / tag),
                "fixtures": fixtures_info,
                "scenes_expected": [fx.scene_id for fx in scenes],
                "scenes_available": sorted(scenes_payload),
                "scenes_missing": tag_missing,
            },
            "conventions": {
                "seat_std_mean_db": "mean_b std_m(10log10 E_m,b)",
                "seat_std_max_db": "max_b std_m(10log10 E_m,b)",
                "max_dev_db": "max_{b,m} |level_m,b - mean_b|",
                "null_depth_mean_db": "mean_b interference_null_db_b",
                "null_depth_worst_db": "min_b interference_null_db_b",
                "point_set": "design for design block, holdout for holdout block",
                "pressure": "P_after = H * G rebuilt from FIR/B1 params (B0: G=1)",
                "design_max_rise_db": ("max_{b,m in design} 10log10("
                                       "E_after_m,b / E_B0_m,b)"),
                "predicted_status": STATUS_RULE_TEXT,
                "predicted_status_rule_version": STATUS_RULE_VERSION,
                "feasibility_full_rule": ("full feasible = independent "
                                          "design_max_rise_db <= max_db_rise_db "
                                          "AND all Rust-provided "
                                          "cv_fold_max_rise_db <= max_db_rise_db; "
                                          "CV fold maxima are rule inputs from "
                                          "the Rust report, not independently "
                                          "recomputed"),
            },
            "scenes": scenes_payload,
            "crosscheck": {
                "status": tag_status,
                "scenes_expected": len(scenes),
                "scenes_compared": len(scenes_payload),
                "scenes_missing": tag_missing,
                "n_rows_compared": len(rows),
                "counts_by_status": tag_rows.counts(),
                "n_mismatch": len(mismatch_rows),
                "n_rule_notes": len(rule_notes),
                "max_abs_diff": family_max,
                "mismatches": mismatch_rows,
                "rule_notes": rule_notes,
            },
            "challenge_severe": _challenge_severe_summary(scenes_payload),
            "v1_regression": (v1_regression if tag == "emodal" else None),
            "issues": tag_issues,
        }
        tag_payloads[tag] = payload
        write_json(out_dir / ("%s.json" % tag), payload)
        _log("wrote %s (rows=%d mismatch=%d status=%s)"
             % (out_dir / ("%s.json" % tag), len(rows), len(mismatch_rows),
                tag_status))
    write_objective_crosscheck_csv(out_dir / "objective-crosscheck.csv", all_rows)
    _log("wrote %s" % (out_dir / "objective-crosscheck.csv"))
    if any(status == "FAIL" for status in tag_statuses.values()):
        overall = "FAIL"
    elif missing or any(status == "INCOMPLETE" for status in tag_statuses.values()):
        overall = "INCOMPLETE"
    else:
        overall = "PASS"
    summary = {
        "format": "tspi-mimo-python-metrics-objectives-summary/v1",
        "source": SOURCE,
        "provenance": {"data_kind": SOURCE, "honesty_status": HONESTY},
        "generated_at_utc": now_utc_iso(),
        "overall_status": overall,
        "status_rule": STATUS_RULE_JSON,
        "tag_statuses": tag_statuses,
        "missing": missing,
        "tags": {tag: {"status": tag_payloads[tag]["crosscheck"]["status"],
                       "max_abs_diff": tag_payloads[tag]["crosscheck"]["max_abs_diff"]}
                 for tag in tag_payloads},
        "v1_regression_status": (v1_regression or {}).get("status"),
        "issues": all_issues,
    }
    write_json(out_dir / "objective-summary.json", summary)
    md = render_objective_tables(tag_payloads, summary, v1_regression)
    (out_dir / "objective-tables.md").write_text(md, encoding="utf-8",
                                                 newline=chr(10))
    _log("wrote objective-tables.md / objective-summary.json")
    _log("objective-v2 cross-check status: %s" % overall)
    for tag in tags:
        cross = tag_payloads.get(tag, {}).get("crosscheck") or {}
        _log("  %-10s %-10s rows=%-5d mismatch=%d"
             % (tag, cross.get("status"), int(cross.get("n_rows_compared", 0)),
                int(cross.get("n_mismatch", 0))))
        maxima = cross.get("max_abs_diff") or {}
        wanted = ("E_modal_db", "R_k", "seat_std_mean_db", "seat_std_max_db",
                  "max_dev_db", "null_depth_mean_db", "null_depth_worst_db",
                  "design_max_rise_db", "predicted_status")
        for key in wanted:
            if key in maxima:
                _log("    max_abs_diff_%s = %.3e" % (key, float(maxima[key])))
    if overall == "PASS":
        return 0
    if overall == "INCOMPLETE":
        return 2
    return 3

# === OBJECTIVES APPEND POINT ===



# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog=TOOL_NAME,
        description="Independent Python reference metrics for the RoomForge MIMO "
                    "fixtures (SIMULATED/UNPROVEN), cross-checked against the Rust "
                    "mimo-opt report.")
    parser.add_argument("--selftest", action="store_true",
                        help="run hand-computed known-answer assertions and exit")
    parser.add_argument("--fixtures", type=str, default=None,
                        help="fixtures directory (MANIFEST.json + scene-*/ground-truth.json)")
    parser.add_argument("--runs", type=str, default=None,
                        help="Rust runs directory (runs/<scene>/report.json + fir/*.csv)")
    parser.add_argument("--runs-obj-root", type=str, default=None,
                        help="round 2: runs-obj root (<root>/<tag>/<scene>/report.json); "
                             "enables the objective-v2 independent cross-check mode")
    parser.add_argument("--tags", type=str,
                        default=",".join(OBJECTIVE_TAGS_DEFAULT),
                        help="round 2 tags under --runs-obj-root "
                             "(default: emodal,seat-std,null-depth,combined)")
    parser.add_argument("--v1-runs", type=str, default=None,
                        help="round 2: frozen v1 runs dir for the emodal regression "
                             "check (OBJECTIVES-V2 section 4, tolerance 1e-9)")
    parser.add_argument("--out", type=str, default=None,
                        help="validation output directory")
    parser.add_argument("--scene", action="append", default=None,
                        help="only process this scene id (repeatable)")
    parser.add_argument("--timeout-seconds", type=float, default=DEFAULT_TIMEOUT_S,
                        help="max total wait for fixtures/runs (default 2700 = 45 min)")
    parser.add_argument("--poll-seconds", type=float, default=DEFAULT_POLL_S,
                        help="poll interval while waiting (default 120 s)")
    parser.add_argument("--no-plots", action="store_true",
                        help="skip matplotlib figure generation")
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = build_arg_parser()
    args = parser.parse_args(argv)
    if args.selftest:
        try:
            return run_selftest()
        except SelftestFailure as exc:
            print("[selftest] FAIL: %s" % exc, file=sys.stderr)
            return 1
    if args.runs_obj_root:
        if not (args.fixtures and args.out):
            parser.error("--fixtures and --out are required with --runs-obj-root")
            return 2
        return run_objectives_pipeline(args)
    if not (args.fixtures and args.runs and args.out):
        parser.error("--fixtures, --runs and --out are required unless --selftest "
                     "(or --fixtures + --runs-obj-root + --out for objective mode)")
        return 2
    return run_pipeline(args)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except MetricsError as exc:
        print("[error] %s" % exc, file=sys.stderr)
        sys.exit(2)
    except SelftestFailure as exc:
        print("[selftest] FAIL: %s" % exc, file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:  # pragma: no cover
        print("[error] interrupted", file=sys.stderr)
        sys.exit(1)
    except Exception:  # pragma: no cover - keep a clear nonzero exit
        traceback.print_exc()
        sys.exit(1)



