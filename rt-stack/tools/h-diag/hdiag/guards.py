
"""h-diag provenance gate (fail-closed) -- reuses the repo's MIMO guards.

Rules are *not* reinvented here.  The module imports the two existing guards from
rt-stack/tools/mimo-prep and copies their policy constants with citations:

  * mimo12k_guard_lite.py        -- stdlib only: sha256_file / load_json /
                                    claim_tags_synthetic (synthetic/dry-run
                                    marker detection, negations ignored).
  * mimo_kit/provenance.py       -- numpy guard: synthetic_registry(),
                                    synthetic_registry_12k() (content-addressed
                                    synthetic roots + file hashes),
                                    ARTIFACT_PEAK_RANGE_DBFS/ARTIFACT_MAX_SNR_DB
                                    (known MA1/ARC S24 byte-mapping artifact),
                                    MIC_MIN_PEAK_DBFS/MIC_MIN_SNR_DB,
                                    IR_MAX_PEAK_MS/IR_MAX_PEAK_SPREAD_MS,
                                    H_MIN_ABS_DB, _is_under().

If either guard cannot be imported the gate FAILS CLOSED: provenance status becomes
UNPROVEN with code hdiag_guard_unavailable; no controllability conclusion is
allowed.  Recorded guard source paths + sha256 are emitted into hdiag.json so the
evidence ties the policy to exact bytes.

h-diag never proves "the data is real" by itself.  It proves:
  1. every input file is inside a declared evidence root (machine-checkable
     provenance boundary; outside -> UNPROVEN),
  2. no file/marker/hash/path matches the repo's registered synthetic fleet
     (synthetic/dry-run -> SYNTHETIC_REFUSED),
  3. no known invalid-measurement fingerprint is present (MA1/ARC byte artifact,
     dead mic / dead actuator row, invalid clock/onset declaration).
"""
from __future__ import annotations

import hashlib
import json
import os
import sys
from datetime import datetime, timezone

TOOL_NAME = "h-diag"
TOOL_VERSION = "0.1.0"

# ---- fallback policy constants (identical values as mimo_kit/provenance.py) ----
# provenance.py:58  ARTIFACT_PEAK_RANGE_DBFS = (-43.0, -41.3)
# provenance.py:59  ARTIFACT_MAX_SNR_DB = 2.0
# provenance.py:56  MIC_MIN_PEAK_DBFS = -90.0
# provenance.py:57  MIC_MIN_SNR_DB = 6.0
# provenance.py:60  IR_MAX_PEAK_MS = 60.0
# provenance.py:61  IR_MAX_PEAK_SPREAD_MS = 40.0
# provenance.py:63  H_MIN_ABS_DB = -120.0
FALLBACK_POLICY = {
    "artifact_peak_range_dbfs": (-43.0, -41.3),
    "artifact_max_snr_db": 2.0,
    "mic_min_peak_dbfs": -90.0,
    "mic_min_snr_db": 6.0,
    "ir_max_peak_ms": 60.0,
    "ir_max_peak_spread_ms": 40.0,
    "h_min_abs_db": -120.0,
}

ACTUATORS = ["FL", "FR", "SL", "SR", "SUB1"]
EXCLUDED_ROLES = ["HFL", "HFR"]
POSITION_ROLES = {"design": "design", "holdout": "holdout", "validation": "holdout"}

SCHEMA_MANIFEST = "tspi-hdiag-positions/v1"
SCHEMA_H_DIAG = "tspi-h-diag/v1"
SCHEMA_SESSION = "tspi-mimo-measurement/v1"
SCHEMA_POSITION = "tspi-mimo-position/v1"
SCHEMA_ANALYSIS = "tspi-channel-measurement/v1"
SCHEMA_ANALYSIS_META = "khaen-calibration-meta/v1"


def utc_now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


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


def repo_root():
    """rt-stack checkout root: this file lives at rt-stack/tools/h-diag/hdiag/guards.py."""
    here = os.path.abspath(__file__)
    root = here
    for _ in range(4):  # hdiag -> h-diag -> tools -> rt-stack
        root = os.path.dirname(root)
    return root


def project_root():
    return os.path.dirname(repo_root())


def evidence_root():
    return os.path.join(repo_root(), "evidence")


def _is_under(path, roots):
    if not path:
        return False
    p = os.path.normcase(os.path.abspath(path))
    for root in roots or []:
        if not root:
            continue
        r = os.path.normcase(os.path.abspath(root))
        if p == r or p.startswith(r + os.sep):
            return True
    return False


def path_segments(path):
    text = str(path or "")
    return [seg.lower() for seg in text.replace(chr(92), "/").split("/") if seg]


SYNTHETIC_PATH_TOKENS = ("dryrun", "synthetic", "fixtures", "fixture")


# ---------------------------------------------------------------- guard import
class GuardBundle(object):
    """Loads the repo guards once; records which bytes supplied the policy."""

    def __init__(self):
        self.lite = None
        self.prov = None
        self.errors = []
        self.sources = []
        self.policy = dict(FALLBACK_POLICY)
        self.synthetic_roots = []
        self.synthetic_h_sha256 = []
        self.synthetic_file_sha256 = []
        self.claim_tags_synthetic = self._fallback_claim_tags
        self._load()

    # -- fallback (only used when the repo guard is unavailable) --
    @staticmethod
    def _fallback_claim_tags(text):
        t = str(text or "").lower()
        for neg in ("not a dry-run", "not dry-run", "not a synthetic", "not synthetic",
                    "no dry-run", "no synthetic", "non-synthetic", "non synthetic",
                    "never synthetic", "not from the dry-run", "not from dry-run"):
            t = t.replace(neg, "")
        return ("dry-run" in t) or ("dryrun" in t) or ("synthetic" in t)

    def _load(self):
        mp = os.path.join(repo_root(), "tools", "mimo-prep")
        if mp not in sys.path:
            sys.path.insert(0, mp)
        lite_path = os.path.join(mp, "mimo12k_guard_lite.py")
        prov_path = os.path.join(mp, "mimo_kit", "provenance.py")
        try:
            import mimo12k_guard_lite as lite  # type: ignore
            self.lite = lite
            if hasattr(lite, "claim_tags_synthetic"):
                self.claim_tags_synthetic = lite.claim_tags_synthetic
            self.sources.append({"path": lite_path, "sha256": sha256_file(lite_path),
                                 "role": "claim_tags_synthetic/sha256_file/load_json"})
        except Exception as exc:  # pragma: no cover - only when repo layout is broken
            self.errors.append("mimo12k_guard_lite import failed: %s" % (exc,))
        try:
            import mimo_kit.provenance as prov  # type: ignore
            self.prov = prov
            self.policy = {
                "artifact_peak_range_dbfs": tuple(prov.ARTIFACT_PEAK_RANGE_DBFS),
                "artifact_max_snr_db": float(prov.ARTIFACT_MAX_SNR_DB),
                "mic_min_peak_dbfs": float(prov.MIC_MIN_PEAK_DBFS),
                "mic_min_snr_db": float(prov.MIC_MIN_SNR_DB),
                "ir_max_peak_ms": float(prov.IR_MAX_PEAK_MS),
                "ir_max_peak_spread_ms": float(prov.IR_MAX_PEAK_SPREAD_MS),
                "h_min_abs_db": float(prov.H_MIN_ABS_DB),
            }
            self.sources.append({"path": prov_path, "sha256": sha256_file(prov_path),
                                 "role": "synthetic registries + invalid-measurement policy"})
            reg = prov.synthetic_registry()
            reg12 = prov.synthetic_registry_12k()
            roots = []
            for r in (reg.get("synthetic_roots") or []) + (reg12.get("synthetic_roots") or []):
                if r and r not in roots:
                    roots.append(r)
            self.synthetic_roots = roots
            for mapping in (reg.get("synthetic_h_sha256") or {},
                            reg12.get("synthetic_h_sha256") or {}):
                for value in mapping.values():
                    if value:
                        self.synthetic_h_sha256.append(str(value))
            for mapping in (reg.get("synthetic_design_sha256") or {},
                            reg.get("synthetic_plan_sha256") or {},
                            reg12.get("synthetic_g12_file_sha256") or {},
                            reg12.get("synthetic_params_sha256") or {}):
                for value in mapping.values():
                    if value:
                        self.synthetic_file_sha256.append(str(value))
        except Exception as exc:  # pragma: no cover
            self.errors.append("mimo_kit.provenance import failed: %s" % (exc,))

    @property
    def ok(self):
        return self.lite is not None and self.prov is not None

    def as_json(self):
        return {
            "ok": self.ok,
            "errors": self.errors,
            "sources": self.sources,
            "policy": {
                "artifact_peak_range_dbfs": list(self.policy["artifact_peak_range_dbfs"]),
                "artifact_max_snr_db": self.policy["artifact_max_snr_db"],
                "mic_min_peak_dbfs": self.policy["mic_min_peak_dbfs"],
                "mic_min_snr_db": self.policy["mic_min_snr_db"],
                "ir_max_peak_ms": self.policy["ir_max_peak_ms"],
                "ir_max_peak_spread_ms": self.policy["ir_max_peak_spread_ms"],
                "h_min_abs_db": self.policy["h_min_abs_db"],
            },
            "synthetic_roots": self.synthetic_roots,
            "n_synthetic_h_sha256": len(self.synthetic_h_sha256),
            "n_synthetic_file_sha256": len(self.synthetic_file_sha256),
        }


# ---------------------------------------------------------------- decisions
def classify_file_location(path, bundle, extra_evidence_roots):
    """Return one of: evidence | synthetic | external."""
    segs = path_segments(path)
    if any(tok in seg for seg in segs for tok in SYNTHETIC_PATH_TOKENS):
        return "synthetic"
    if _is_under(path, bundle.synthetic_roots):
        return "synthetic"
    if _is_under(path, extra_evidence_roots):
        return "evidence"
    if _is_under(path, [evidence_root()]):
        return "evidence"
    return "external"


def evaluate_provenance(manifest, analyses, bundle, extra_evidence_roots=None,
                        require_session=True):
    """Fail-closed provenance decision for a parsed multipoint input set.

    manifest: normalised manifest dict (paths absolute)
    analyses: list of analysis records (dict with path/sha256/json/meta/fr)
    Returns dict with status in {MEASURED, UNPROVEN, SYNTHETIC_REFUSED,
    INVALID_MEASUREMENT}, codes, reasons, file table and policy snapshot.
    """
    extra_evidence_roots = extra_evidence_roots or []
    codes, reasons = [], {}

    def bad(code, msg):
        if code not in codes:
            codes.append(code)
            reasons[code] = msg

    files = []
    synthetic_seen = False
    invalid_seen = False

    if not bundle.ok:
        bad("hdiag_guard_unavailable",
            "repo provenance guards could not be imported; fail-closed UNPROVEN "
            "(errors: %s)" % ("; ".join(bundle.errors) or "unknown"))

    if manifest.get("synthetic") is True:
        synthetic_seen = True
        bad("hdiag_synthetic_marker", "manifest declares synthetic=true")
    if bundle.claim_tags_synthetic(manifest.get("claim")):
        synthetic_seen = True
        bad("hdiag_synthetic_marker", "manifest claim tags synthetic/dry-run")
    if manifest.get("demo_of_existing_data") is True:
        bad("hdiag_demo_not_preregistered",
            "manifest declares demo_of_existing_data=true (ad-hoc re-use of an "
            "existing measurement set, not a frozen design/holdout pre-registration)")
    if require_session:
        sess = manifest.get("session") or {}
        mic = sess.get("mic") or manifest.get("mic") or {}
        if not mic.get("model"):
            bad("hdiag_mic_identity_unknown",
                "no session.json/mic identity (model) available; cannot confirm which "
                "microphone produced the data (fail-closed)")

    for rec in analyses:
        path = rec.get("path") or ""
        loc = classify_file_location(path, bundle, extra_evidence_roots)
        fj = rec.get("json") or {}
        meta = rec.get("meta") or {}
        entry = {
            "path": path,
            "sha256": rec.get("sha256"),
            "location": loc,
            "label": rec.get("position"),
            "role": rec.get("role"),
            "actuator": rec.get("actuator"),
            "channel": fj.get("channel"),
            "sample_rate": meta.get("sample_rate"),
            "sweep": {k: meta.get(k) for k in ("sweep_start_hz", "sweep_end_hz",
                                               "sweep_seconds", "level_db",
                                               "pre_seconds", "post_seconds")},
            "snr_db": fj.get("snr_db"),
            "peak_db": fj.get("peak_db"),
            "polarity": fj.get("polarity"),
            "phase_ref": fj.get("phase_ref"),
            "checks": [],
        }
        if loc == "synthetic":
            synthetic_seen = True
            bad("hdiag_synthetic_path",
                "%s is under a synthetic/dry-run/fixture path" % path)
        elif loc == "external":
            bad("hdiag_outside_evidence_root",
                "%s is outside the evidence root; provenance cannot be confirmed" % path)

        if rec.get("json") is None:
            bad("hdiag_bad_json", "%s is not valid JSON" % path)
            entry["checks"].append("bad_json")
            files.append(entry)
            continue
        if rec.get("error"):
            bad("hdiag_bad_analysis", "%s: %s" % (path, rec["error"]))
            entry["checks"].append("bad_analysis")
        if bundle.claim_tags_synthetic(fj.get("claim")):
            synthetic_seen = True
            bad("hdiag_synthetic_marker", "%s claim tags synthetic/dry-run" % path)
        if rec.get("sha256") in (bundle.synthetic_h_sha256 + bundle.synthetic_file_sha256):
            synthetic_seen = True
            bad("hdiag_synthetic_sha", "%s sha256 is a registered synthetic fixture" % path)
        if meta.get("synthetic") is True:
            synthetic_seen = True
            bad("hdiag_synthetic_marker", "%s meta.synthetic=true" % path)

        peak = _num(fj.get("peak_db"))
        snr = _num(fj.get("snr_db"))
        lo, hi = bundle.policy["artifact_peak_range_dbfs"]
        if peak is not None and lo <= peak <= hi and snr is not None                 and snr < bundle.policy["artifact_max_snr_db"]:
            invalid_seen = True
            bad("hdiag_mic_byte_artifact",
                "%s shows the S24 byte-mapping artifact signature (peak %.2f dBFS, "
                "SNR %.2f dB; policy range [%.1f, %.1f] / < %.1f dB)"
                % (path, peak, snr, lo, hi, bundle.policy["artifact_max_snr_db"]))
        if (peak is not None and peak <= bundle.policy["mic_min_peak_dbfs"])                 or (snr is not None and snr < bundle.policy["mic_min_snr_db"]):
            invalid_seen = True
            bad("hdiag_mic_unavailable",
                "%s unusable measurement (peak=%s dBFS snr=%s dB; policy peak>%.0f, "
                "snr>=%.0f)" % (path, peak, snr, bundle.policy["mic_min_peak_dbfs"],
                                bundle.policy["mic_min_snr_db"]))
        if fj.get("clock_ppm_ok") is False:
            invalid_seen = True
            bad("hdiag_clock_invalid", "%s declares clock_ppm_ok=false" % path)
        if fj.get("clock_ppm_significant") is True and fj.get("drift_corrected") is not True:
            invalid_seen = True
            bad("hdiag_drift_not_corrected",
                "%s declares significant clock drift without drift_corrected=true" % path)
        if fj.get("onset_invalid") is True:
            invalid_seen = True
            bad("hdiag_onset_invalid", "%s declares onset_invalid=true" % path)
        files.append(entry)

    if analyses:
        dead = _dead_actuators(analyses, bundle.policy["h_min_abs_db"])
        for actuator, max_db in dead.items():
            invalid_seen = True
            bad("hdiag_dead_actuator_row",
                "actuator %s has no measured energy at some positions (max |H| %.1f dB "
                "<= %.1f dB)" % (actuator, max_db, bundle.policy["h_min_abs_db"]))

    if synthetic_seen:
        status = "SYNTHETIC_REFUSED"
        verdict = "UNPROVEN"
    elif invalid_seen:
        status = "INVALID_MEASUREMENT"
        verdict = "INVALID_MEASUREMENT"
    elif codes:
        status = "UNPROVEN"
        verdict = "UNPROVEN"
    else:
        status = "MEASURED"
        verdict = "MEASURED"

    return {
        "status": status,
        "verdict": verdict,
        "ok": status == "MEASURED",
        "codes": codes,
        "reasons": reasons,
        "files": files,
        "policy": bundle.as_json(),
        "evidence_roots": [os.path.abspath(p) for p in
                           ([evidence_root()] + list(extra_evidence_roots))],
        "rules": {
            "measured": "all analysis files under an evidence root AND no synthetic "
                        "marker/path/hash AND no invalid-measurement fingerprint AND "
                        "mic identity + session metadata present",
            "synthetic_refused": "synthetic/dry-run/fixture path or marker or registered "
                                 "synthetic sha -> no real conclusion (UNPROVEN)",
            "invalid": "known MA1/ARC byte artifact, dead mic/actuator row, invalid "
                       "clock/onset/drift declarations -> REFUSED",
            "outside_evidence_root": "cannot confirm provenance -> UNPROVEN (fail-closed)",
        },
    }


def _num(value):
    try:
        out = float(value)
    except (TypeError, ValueError):
        return None
    if out != out or out in (float("inf"), float("-inf")):
        return None
    return out


def _dead_actuators(analyses, h_min_abs_db):
    """max |H| per actuator across positions; flag columns with no energy.

    Uses the analysis FR (db) on the analysis grid, mirroring h_guard's dead
    participant row check (H_MAX_ABS_DB).
    """
    in_band = (20.0, 300.0)
    best = {}
    for rec in analyses:
        actuator = rec.get("actuator")
        pts = rec.get("fr") or []
        if not actuator or not pts:
            continue
        peak = None
        for p in pts:
            f = _num(p.get("freq_hz"))
            db = _num(p.get("db"))
            if f is None or db is None or not (in_band[0] <= f <= in_band[1]):
                continue
            if peak is None or db > peak:
                peak = db
        if peak is None:
            continue
        if actuator not in best or peak > best[actuator]:
            best[actuator] = peak
    return {a: v for a, v in best.items() if v <= h_min_abs_db}
