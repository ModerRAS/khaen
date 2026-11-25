
"""Multipoint measurement ingestion for h-diag: manifest, analysis parsing,
measurement-consistency gate, complex H assembly.

Input contract (manifest: tspi-hdiag-positions/v1)
--------------------------------------------------
The design/holdout split and the file -> (position, actuator) mapping come ONLY from
the pre-registered manifest.  h-diag never guesses a role from a file name.  File
names are only *cross-checked* against the convention <ROLE>-ch<NN>.analysis.json
and a mismatch is reported (INCONSISTENT) instead of silently accepted.
"""
from __future__ import annotations

import math
import os

import numpy as np

from . import guards as G


class InputError(Exception):
    pass


REQUIRED_ANALYSIS_FIELDS = ("schema", "channel", "delay_samples", "delay_ms",
                            "delay_coarse_samples", "polarity", "peak_db", "snr_db",
                            "phase_ref", "fr", "meta")
REQUIRED_META_FIELDS = ("schema", "sample_rate", "sweep_start_hz", "sweep_end_hz",
                        "sweep_seconds", "level_db", "pre_seconds", "post_seconds")
CONSISTENCY_FIELDS = ("sample_rate", "sweep_start_hz", "sweep_end_hz", "sweep_seconds",
                      "level_db", "pre_seconds", "post_seconds", "playback_device",
                      "playback_channels", "capture_device", "capture_channels")
FLOAT_TOL_REL = 1.0e-9
GRID_TOL_REL = 1.0e-9


def rel_equal(a, b, tol=FLOAT_TOL_REL):
    try:
        a = float(a)
        b = float(b)
    except (TypeError, ValueError):
        return a == b
    if a == b:
        return True
    scale = max(abs(a), abs(b), 1.0e-300)
    return abs(a - b) / scale <= tol


# ================================================================ manifest
def load_manifest(path):
    path = os.path.abspath(path)
    raw = G.load_json(path, None)
    if not isinstance(raw, dict):
        raise InputError("manifest is not a JSON object: %s" % path)
    if raw.get("schema") != G.SCHEMA_MANIFEST:
        raise InputError("manifest schema %r != %r" %
                         (raw.get("schema"), G.SCHEMA_MANIFEST))
    base = os.path.dirname(path)
    manifest = dict(raw)
    manifest["path"] = path
    manifest["sha256"] = G.sha256_file(path)
    manifest["base_dir"] = base
    manifest["dirty"] = []

    def resolve(rel):
        if rel is None:
            return None
        if os.path.isabs(str(rel)):
            return os.path.normpath(str(rel))
        return os.path.normpath(os.path.join(base, str(rel)))

    session_json = resolve(raw.get("session_json"))
    manifest["session_json"] = session_json
    manifest["session"] = G.load_json(session_json, None) if session_json else None
    if manifest["session"] is not None and not isinstance(manifest["session"], dict):
        manifest["session"] = None
        manifest["dirty"].append("session_json_not_object")

    actuators = raw.get("actuators")
    if not isinstance(actuators, list) or sorted(actuators) != sorted(G.ACTUATORS):
        raise InputError("manifest.actuators must be exactly %s (got %r)" %
                         (G.ACTUATORS, actuators))
    manifest["actuators"] = list(G.ACTUATORS)
    excluded = raw.get("excluded_roles") or list(G.EXCLUDED_ROLES)
    manifest["excluded_roles"] = list(excluded)

    positions = raw.get("positions")
    if not isinstance(positions, list) or not positions:
        raise InputError("manifest.positions must be a non-empty list")
    labels = set()
    normalised = []
    for index, pos in enumerate(positions):
        if not isinstance(pos, dict):
            raise InputError("manifest.positions[%d] is not an object" % index)
        label = str(pos.get("label") or "").strip()
        if not label:
            raise InputError("manifest.positions[%d] has no label" % index)
        if label in labels:
            raise InputError("duplicate position label %r" % label)
        labels.add(label)
        role_raw = str(pos.get("role") or "").strip().lower()
        if role_raw not in G.POSITION_ROLES:
            raise InputError("position %r role %r is not one of %s" %
                             (label, role_raw, sorted(G.POSITION_ROLES)))
        files = pos.get("files")
        if not isinstance(files, dict):
            raise InputError("position %r has no files map" % label)
        missing = [a for a in G.ACTUATORS if a not in files]
        if missing:
            raise InputError("position %r is missing actuator files for %s" %
                             (label, missing))
        unknown = [a for a in files if a not in G.ACTUATORS]
        if unknown:
            raise InputError("position %r declares non-participant actuators %s "
                             "(HFL/HFR must not enter the low-band matrix)" %
                             (label, unknown))
        file_map = {}
        declared_sha = {}
        for actuator in G.ACTUATORS:
            entry = files[actuator]
            if isinstance(entry, dict):
                file_map[actuator] = resolve(entry.get("path"))
                declared = entry.get("sha256")
                declared_sha[actuator] = str(declared) if declared else None
            else:
                file_map[actuator] = resolve(entry)
                declared_sha[actuator] = None
        weight = pos.get("weight", 1.0)
        try:
            weight = float(weight)
        except (TypeError, ValueError):
            raise InputError("position %r weight %r is not a number" % (label, weight))
        if not (weight > 0.0) or not math.isfinite(weight):
            raise InputError("position %r weight must be finite and > 0" % label)
        position_json = resolve(pos.get("position_json")) if pos.get("position_json") else None
        position_meta = None
        position_meta_mismatch = []
        position_json_sha256 = G.sha256_file(position_json) if (
            position_json and os.path.isfile(position_json)) else None
        declared_metadata_sha = pos.get("metadata_sha256")
        if declared_metadata_sha and position_json_sha256 and \
                str(declared_metadata_sha) != position_json_sha256:
            position_meta_mismatch.append(
                "declared metadata_sha256 %s != sha256(position.json) %s"
                % (declared_metadata_sha, position_json_sha256))
        if position_json:
            meta = G.load_json(position_json, None)
            if isinstance(meta, dict):
                position_meta = {
                    "schema": meta.get("schema"),
                    "name": meta.get("name"),
                    "role": meta.get("role"),
                    "weight": meta.get("weight"),
                }
                if meta.get("name") is not None and str(meta["name"]) != label:
                    position_meta_mismatch.append(
                        "position.json name %r != manifest label %r"
                        % (meta["name"], label))
                if meta.get("role") is not None:
                    declared = str(meta["role"]).strip().lower()
                    expected = G.POSITION_ROLES.get(declared)
                    if expected != G.POSITION_ROLES[role_raw]:
                        position_meta_mismatch.append(
                            "position.json role %r maps to %r != manifest role %r"
                            % (meta["role"], expected, G.POSITION_ROLES[role_raw]))
                if meta.get("weight") is not None:
                    try:
                        if not rel_equal(meta["weight"], weight, 1.0e-9):
                            position_meta_mismatch.append(
                                "position.json weight %r != manifest weight %r"
                                % (meta["weight"], weight))
                    except (TypeError, ValueError):
                        position_meta_mismatch.append("position.json weight is not numeric")
            else:
                position_meta_mismatch.append("position.json is missing or not a JSON object")
        normalised.append({
            "label": label,
            "role": G.POSITION_ROLES[role_raw],
            "role_declared": role_raw,
            "weight": weight,
            "files": file_map,
            "position_json": position_json,
            "position_meta": position_meta,
            "position_meta_mismatch": position_meta_mismatch,
            "position_json_sha256": position_json_sha256,
            "declared_metadata_sha256": declared_metadata_sha,
            "declared_sha256": declared_sha,
            "bands_hz": pos.get("bands_hz"),
            "note": pos.get("note"),
        })
    manifest["positions"] = normalised

    expected = raw.get("expected") or {}
    manifest["expected"] = {
        "design_points": int(expected.get("design_points", 6)),
        "holdout_points": int(expected.get("holdout_points", 3)),
    }
    frozen = raw.get("frozen_at_unix")
    if frozen is None and raw.get("frozen_at_utc"):
        frozen = _parse_utc(raw.get("frozen_at_utc"))
    manifest["frozen_at_unix"] = frozen
    manifest["demo_of_existing_data"] = bool(raw.get("demo_of_existing_data", False))
    return manifest


def _parse_utc(text):
    try:
        from datetime import datetime, timezone
        return int(datetime.strptime(str(text), "%Y-%m-%dT%H:%M:%SZ")
                   .replace(tzinfo=timezone.utc).timestamp())
    except (TypeError, ValueError):
        return None


# ================================================================ analyses
def parse_analysis(path, position, actuator, role, freq_field="auto"):
    rec = {"path": os.path.abspath(path), "position": position, "actuator": actuator,
           "role": role, "json": None, "meta": None, "error": None,
           "sha256": None, "fr": [], "fr_field": None}
    if not os.path.isfile(path):
        rec["error"] = "file missing"
        return rec
    rec["sha256"] = G.sha256_file(path)
    raw = G.load_json(path, None)
    if not isinstance(raw, dict):
        rec["error"] = "not a JSON object"
        return rec
    rec["json"] = raw
    meta = raw.get("meta")
    if isinstance(meta, dict):
        rec["meta"] = meta

    def fail(msg):
        if rec["error"] is None:
            rec["error"] = msg

    if raw.get("schema") != G.SCHEMA_ANALYSIS:
        fail("schema %r != %r" % (raw.get("schema"), G.SCHEMA_ANALYSIS))
    missing = [k for k in REQUIRED_ANALYSIS_FIELDS if k not in raw]
    if missing:
        fail("missing analysis fields %s" % missing)
    if not isinstance(meta, dict):
        fail("missing meta object")
        return rec
    if meta.get("schema") != G.SCHEMA_ANALYSIS_META:
        fail("meta schema %r != %r" % (meta.get("schema"), G.SCHEMA_ANALYSIS_META))
    missing_meta = [k for k in REQUIRED_META_FIELDS if k not in meta]
    if missing_meta:
        fail("missing meta fields %s" % missing_meta)

    try:
        channel = int(raw.get("channel"))
    except (TypeError, ValueError):
        channel = None
        fail("channel is not an integer")
    rec["channel"] = channel
    try:
        polarity = int(raw.get("polarity"))
    except (TypeError, ValueError):
        polarity = None
        fail("polarity is not an integer")
    if polarity not in (-1, 0, 1):
        fail("polarity %r must be -1, 0 or 1" % (polarity,))
    rec["polarity"] = polarity

    # delay-field self consistency (mirrors ingest.rs validate_analysis_delay_fields).
    # A self-inconsistent producer field is a *measurement consistency* finding
    # (INCONSISTENT), not a parse failure: the FR itself is still usable.
    rec["delay_field_consistent"] = True
    rec["delay_field_issue"] = None
    try:
        fs = float(meta.get("sample_rate"))
        delay = float(raw.get("delay_samples"))
        delay_ms = float(raw.get("delay_ms"))
        coarse = int(raw.get("delay_coarse_samples"))
        if not math.isfinite(delay) or not (-1_000_000.0 <= delay <= 1_000_000.0):
            rec["delay_field_consistent"] = False
            rec["delay_field_issue"] = "delay_samples out of range"
        elif abs(delay_ms - delay / fs * 1000.0) > 1.0e-6 * max(1.0, abs(delay_ms)):
            rec["delay_field_consistent"] = False
            rec["delay_field_issue"] = ("delay_ms %.9g is not delay_samples/fs*1000 %.9g"
                                        % (delay_ms, delay / fs * 1000.0))
        elif coarse != int(math.floor(delay)):
            rec["delay_field_consistent"] = False
            rec["delay_field_issue"] = ("delay_coarse_samples %d != floor(delay_samples) %d"
                                        % (coarse, int(math.floor(delay))))
    except (TypeError, ValueError):
        fail("delay fields are not numeric")

    phase_ref = str(raw.get("phase_ref") or "")
    low = phase_ref.lower()
    if "aligned" in low:
        rec["phase_basis"] = "aligned_segment"
    elif low.startswith("absolute") or "absolute_phase" in low:
        rec["phase_basis"] = "absolute"
    elif "delay_removed" in low or "delay-removed" in low:
        rec["phase_basis"] = "delay_removed"
        fail("phase_ref=delay_removed is unsupported (no machine-readable removed delay)")
    else:
        rec["phase_basis"] = "unknown"
        fail("phase_ref %r is not a recognised phase basis" % phase_ref)

    field = _pick_field(raw, freq_field)
    rec["fr_field"] = field
    points = raw.get(field) if field else None
    if not isinstance(points, list) or len(points) < 8:
        fail("FR field %r missing or shorter than 8 points" % field)
        return rec
    freq, db, phase = [], [], []
    for index, point in enumerate(points):
        if not isinstance(point, dict):
            fail("FR[%d] is not an object" % index)
            continue
        try:
            f = float(point.get("freq_hz"))
            d = float(point.get("db"))
            p = float(point.get("phase_deg"))
        except (TypeError, ValueError):
            fail("FR[%d] has non-numeric freq/db/phase" % index)
            continue
        if not (math.isfinite(f) and math.isfinite(d) and math.isfinite(p)):
            fail("FR[%d] has non-finite values" % index)
            continue
        if f <= 0.0 or f > fs / 2.0:
            fail("FR[%d] freq %.6g outside (0, fs/2]" % (index, f))
            continue
        freq.append(f)
        db.append(d)
        phase.append(p)
    if len(freq) >= 2:
        if any(freq[i + 1] <= freq[i] for i in range(len(freq) - 1)):
            fail("FR frequencies are not strictly increasing")
        rec["freq"] = np.asarray(freq, dtype=np.float64)
        rec["db"] = np.asarray(db, dtype=np.float64)
        rec["phase_deg"] = np.asarray(phase, dtype=np.float64)
        rec["fr"] = [{"freq_hz": f, "db": d, "phase_deg": p}
                     for f, d, p in zip(freq, db, phase)]
    else:
        fail("fewer than 2 usable FR points")
    return rec


def _pick_field(raw, freq_field):
    if freq_field in ("fr", "fr_modal"):
        return freq_field
    if freq_field not in (None, "auto"):
        return freq_field
    if isinstance(raw.get("fr_modal"), list) and len(raw.get("fr_modal")) >= 8:
        return "fr_modal"
    return "fr"


def parse_all(manifest, freq_field="auto"):
    analyses = []
    for pos in manifest["positions"]:
        for actuator in G.ACTUATORS:
            rec = parse_analysis(pos["files"][actuator], pos["label"],
                                 actuator, pos["role"], freq_field)
            declared = (pos.get("declared_sha256") or {}).get(actuator)
            rec["declared_sha256"] = declared
            rec["sha_mismatch"] = bool(declared and rec.get("sha256")
                                       and str(declared) != rec["sha256"])
            analyses.append(rec)
    return analyses


def pick_freq_field(analyses, requested, freq_range, min_points=24):
    """Resolve 'auto' to a field that exists for every file on one common grid.

    Inspects the raw JSON (not the parsed rec['freq']) so a mixed fr/fr_modal set
    falls back to 'fr' consistently instead of silently mixing grids.
    """
    if requested in ("fr", "fr_modal"):
        return requested
    for field in ("fr_modal", "fr"):
        grids = []
        complete = True
        for rec in analyses:
            raw = rec.get("json") or {}
            points = raw.get(field)
            if not isinstance(points, list) or len(points) < 8:
                complete = False
                break
            try:
                arr = np.asarray([float(p.get("freq_hz")) for p in points],
                                 dtype=np.float64)
            except (TypeError, ValueError):
                complete = False
                break
            grids.append(arr)
        if not complete or not grids:
            continue
        ref = grids[0]
        in_range = int(np.count_nonzero((ref >= freq_range[0]) & (ref <= freq_range[1])))
        if in_range < min_points:
            continue
        if all(_grids_equal(ref, g) for g in grids[1:]):
            return field
    return "fr"


def _grids_equal(left, right):
    if left is None or right is None or left.shape != right.shape:
        return False
    scale = np.maximum(np.maximum(np.abs(left), np.abs(right)), 1.0e-300)
    return bool(np.all(np.abs(left - right) / scale <= GRID_TOL_REL))


# ================================================================ consistency
def check_consistency(manifest, analyses, options):
    codes, reasons, warnings = [], {}, []
    drifts = []

    def bad(code, msg):
        if code not in codes:
            codes.append(code)
            reasons[code] = msg

    usable = [rec for rec in analyses if rec.get("freq") is not None]
    if not usable:
        bad("hdiag_no_usable_analysis", "no analysis file parsed")
        return _consistency_result(codes, reasons, warnings, drifts, manifest, analyses, [])

    # 1) metadata drift across every file
    for field in CONSISTENCY_FIELDS:
        entries = []
        for rec in usable:
            meta = rec.get("meta") or {}
            if field in meta:
                entries.append((rec, meta.get(field)))
        values = []
        for _, value in entries:
            if not any(_same_meta_value(value, seen) for seen in values):
                values.append(value)
        if len(values) > 1:
            code = "hdiag_%s_drift" % field
            bad(code, "%s differs across measurements: %r" % (field, values))
            drifts.append({
                "field": field,
                "values": _jsonable(values),
                "files": [{"path": rec["path"], "value": _jsonable(value)}
                          for rec, value in entries],
            })

    # 2) common frequency grid
    ref = usable[0]
    mismatched = [rec["path"] for rec in usable[1:]
                  if not _grids_equal(ref.get("freq"), rec.get("freq"))]
    if mismatched:
        bad("hdiag_freq_grid_mismatch",
            "analysis frequency grids differ from %s (files: %s)"
            % (ref["path"], ", ".join(mismatched[:4])))
        drifts.append({"field": "freq_grid", "values": "mismatch",
                       "files": [{"path": p} for p in mismatched]})

    # 3) polarity must be known (+/-1) for the H matrix
    for rec in usable:
        if rec.get("polarity") == 0:
            bad("hdiag_polarity_unknown",
                "%s declares polarity 0 (unknown); the complex H sign cannot be "
                "established" % rec["path"])

    # 4) channel / filename convention cross-check (never used to infer roles)
    for rec in usable:
        base = os.path.basename(rec["path"])
        parts = base.split("-ch")
        if len(parts) == 2:
            role_token = parts[0]
            number = parts[1].split(".")[0]
            if role_token in G.ACTUATORS and rec.get("channel") is not None:
                try:
                    channel = int(number)
                except ValueError:
                    channel = None
                if channel is not None and channel != rec["channel"]:
                    bad("hdiag_channel_filename_mismatch",
                        "%s: filename channel %d != analysis.channel %r"
                        % (rec["path"], channel, rec["channel"]))

    # 3b) declared per-file sha256 binding (manifest metadata hash)
    for rec in usable:
        if rec.get("sha_mismatch"):
            bad("hdiag_analysis_sha_mismatch",
                "%s: declared sha256 %s != file sha256 %s"
                % (rec["path"], rec.get("declared_sha256"), rec.get("sha256")))

    # 4a) optional position.json cross-check (role/label/weight)
    for pos in manifest["positions"]:
        for issue in pos.get("position_meta_mismatch") or []:
            bad("hdiag_position_meta_mismatch",
                "position %r: %s" % (pos["label"], issue))

    # 4b) producer delay-field self consistency (see parse_analysis)
    for rec in usable:
        if rec.get("delay_field_consistent") is False:
            bad("hdiag_delay_field_mismatch",
                "%s: %s" % (rec["path"], rec.get("delay_field_issue")))

    # 5) session.json cross-check when present
    session = manifest.get("session")
    if isinstance(session, dict):
        participants = session.get("participants")
        if participants is not None and sorted(participants) != sorted(G.ACTUATORS):
            bad("hdiag_session_participants",
                "session.participants %r != low-band participants %r"
                % (participants, G.ACTUATORS))
        if session.get("sample_rate_hz") is not None:
            for rec in usable:
                fs = (rec.get("meta") or {}).get("sample_rate")
                if fs is not None and not rel_equal(fs, session["sample_rate_hz"]):
                    bad("hdiag_session_sample_rate_drift",
                        "%s sample_rate %r != session %r"
                        % (rec["path"], fs, session["sample_rate_hz"]))
        stimulus = session.get("stimulus") or {}
        mapping = {"start_hz": "sweep_start_hz", "end_hz": "sweep_end_hz",
                   "seconds": "sweep_seconds", "level_dbfs": "level_db"}
        for src_key, meta_key in mapping.items():
            expected = stimulus.get(src_key)
            if expected is None:
                continue
            for rec in usable:
                actual = (rec.get("meta") or {}).get(meta_key)
                if actual is not None and not rel_equal(actual, expected):
                    bad("hdiag_session_stimulus_drift",
                        "%s meta.%s %r != session.stimulus.%s %r"
                        % (rec["path"], meta_key, actual, src_key, expected))
                    break

    # 6) pre-registration freeze must predate every holdout capture
    frozen = manifest.get("frozen_at_unix")
    if frozen is None:
        warnings.append("manifest has no frozen_at_unix; holdout pre-registration "
                        "ordering cannot be checked")
    else:
        holdout = [rec for rec in usable if G.POSITION_ROLES.get(rec.get("role")) == "holdout"]
        for rec in holdout:
            created = (rec.get("meta") or {}).get("created_unix")
            if created is not None and float(created) < float(frozen):
                bad("hdiag_holdout_before_freeze",
                    "%s was captured at %r, before the declared freeze %r"
                    % (rec["path"], created, frozen))

    # 7) report low SNR as a warning (not a consistency failure)
    low = [rec for rec in usable
           if _num(rec.get("json", {}).get("snr_db")) is not None
           and _num(rec["json"].get("snr_db")) < options.get("min_report_snr_db", 20.0)]
    if low:
        warnings.append("%d analysis files have SNR < %.1f dB (reported, bands may be "
                        "noise-limited): %s" %
                        (len(low), options.get("min_report_snr_db", 20.0),
                         ", ".join(sorted(os.path.basename(r["path"]) for r in low)[:6])))

    # 8) role counts (reported here, enforced by completeness gate)
    design = [rec for rec in usable if rec.get("role") == "design"]
    holdout = [rec for rec in usable if rec.get("role") == "holdout"]
    counts = {"design_positions": len({r["position"] for r in design}),
              "holdout_positions": len({r["position"] for r in holdout})}
    return _consistency_result(codes, reasons, warnings, drifts, manifest, analyses, counts)


def _consistency_result(codes, reasons, warnings, drifts, manifest, analyses, counts):
    return {
        "status": "INCONSISTENT" if codes else "CONSISTENT",
        "ok": not codes,
        "codes": codes,
        "reasons": reasons,
        "warnings": warnings,
        "drifts": drifts,
        "counts": counts,
        "policy": {
            "fields_checked": list(CONSISTENCY_FIELDS),
            "float_tolerance_rel": FLOAT_TOL_REL,
            "grid_tolerance_rel": GRID_TOL_REL,
            "notes": [
                "sweep source/level/pre-post/sample_rate/device and analysis grid must "
                "match across every file; any drift is INCONSISTENT and blocks the "
                "controllability conclusion (drifted delay/level/processing chain "
                "manufactures fake rank and fake phase)",
                "roles and the file -> position/actuator mapping come only from the "
                "pre-registered positions.json; file names are cross-checked, never used "
                "to infer roles",
            ],
        },
    }


def _same_meta_value(left, right):
    if isinstance(left, (int, float)) and isinstance(right, (int, float)):
        return rel_equal(left, right)
    return left == right


def _num(value):
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def _jsonable(value):
    if isinstance(value, (np.floating, np.integer)):
        return value.item()
    if isinstance(value, (list, tuple)):
        return [_jsonable(v) for v in value]
    return value


# ================================================================ H assembly
def build_h(manifest, analyses, freq_field, freq_range, common_d0_samples=0.0):
    """Assemble complex H per role: design (P_d,N,F), holdout (P_h,N,F).

    H[p,n,f] = 10**(db/20) * exp(j*radians(phase - 360*f*D0/fs)) * polarity.

    phase_ref aligned_segment: analysis phase is referenced to the aligned sweep
    start; one common integer D0 may be subtracted.  D0 is a *per-position common*
    rotation applied identically to all actuators, therefore every reported metric
    (singular values, condition number, coherence magnitude/phase differences,
    subspace angles, achievable-depth estimates) is invariant to it; raw H is
    emitted with the declared D0 applied for completeness only.
    phase_ref absolute: no rotation.
    """
    usable = {(rec["position"], rec["actuator"]): rec for rec in analyses
              if rec.get("freq") is not None and rec.get("error") is None}
    design_positions = [p for p in manifest["positions"] if p["role"] == "design"]
    holdout_positions = [p for p in manifest["positions"] if p["role"] == "holdout"]
    if not design_positions:
        raise InputError("no design position in manifest")

    # common grid: design files define it; holdout must match (consistency gate)
    ref = usable.get((design_positions[0]["label"], G.ACTUATORS[0]))
    if ref is None:
        for key, rec in usable.items():
            ref = rec
            break
    if ref is None:
        raise InputError("no usable analysis to define the frequency grid")
    full = np.asarray(ref["freq"], dtype=np.float64)
    mask = (full >= freq_range[0]) & (full <= freq_range[1])
    grid = full[mask]

    polarity_assumed = []

    def assemble(positions):
        rows, labels, weights, missing = [], [], [], []
        for pos in positions:
            matrix = np.full((len(G.ACTUATORS), len(grid)), np.nan + 1j * np.nan,
                             dtype=np.complex128)
            complete = True
            for n, actuator in enumerate(G.ACTUATORS):
                rec = usable.get((pos["label"], actuator))
                if rec is None or not _grids_equal(rec["freq"], full):
                    complete = False
                    missing.append([pos["label"], actuator])
                    continue
                fs = float((rec.get("meta") or {}).get("sample_rate", 48000.0))
                amp = np.power(10.0, rec["db"][mask] / 20.0)
                rotation = -2.0 * math.pi * grid * float(common_d0_samples) / fs
                phase = np.radians(rec["phase_deg"][mask]) + rotation
                polarity_raw = rec.get("polarity")
                if polarity_raw in (-1, 1):
                    polarity = float(polarity_raw)
                elif polarity_raw == 0:
                    # unknown polarity: every reported metric (singular values,
                    # condition number, |coherence|, subspace angles, R_k energies,
                    # ridge residual/effort) is invariant to a per-column sign flip.
                    # Use +1 for the diagnostic complex H, flag the assumption, and
                    # keep the measurement INCONSISTENT so no conclusion is released.
                    polarity = 1.0
                    polarity_assumed.append({"position": pos["label"],
                                             "actuator": actuator})
                else:
                    polarity = np.nan
                matrix[n] = amp * np.exp(1j * phase) * polarity
            rows.append(matrix)
            labels.append(pos["label"])
            weights.append(pos["weight"])
        if not rows:
            return {"labels": [], "weights": [], "H": np.zeros((0, len(G.ACTUATORS), len(grid)),
                                                              dtype=np.complex128),
                    "missing": missing}
        return {"labels": labels, "weights": np.asarray(weights, dtype=np.float64),
                "H": np.stack(rows, axis=0), "missing": missing,
                "polarity_assumed": list(polarity_assumed)}

    design = assemble(design_positions)
    holdout = assemble(holdout_positions)
    return {
        "freq_hz": grid,
        "actuators": list(G.ACTUATORS),
        "design": design,
        "holdout": holdout,
        "freq_field": freq_field,
        "grid_full_points": int(full.size),
        "grid_points_in_range": int(grid.size),
        "common_d0_samples": float(common_d0_samples),
        "polarity_assumed": design.get("polarity_assumed", []) + holdout.get("polarity_assumed", []),
        "polarity_note": ("polarity=0 (unknown) is rendered as +1 for diagnostics; all "
                          "reported metrics are invariant to a per-column sign flip; the "
                          "measurement stays INCONSISTENT and conclusions are withheld"),
        "phase_model": ("H = 10^(db/20) * exp(j*(phase_deg - 360*f*D0/fs)) * polarity; "
                        "aligned_segment phases, per-position common D0 (metric-invariant)"),
    }


# ================================================================ completeness
def check_completeness(manifest, consistency, provenance, options):
    codes, reasons = [], {}
    expected = manifest.get("expected") or {}
    design_required = int(options.get("min_design_points", expected.get("design_points", 6)))
    holdout_required = int(options.get("min_holdout_points", expected.get("holdout_points", 3)))
    counts = consistency.get("counts") or {}
    design = int(counts.get("design_positions", 0))
    holdout = int(counts.get("holdout_positions", 0))

    if design < design_required:
        codes.append("hdiag_design_points_short")
        reasons["hdiag_design_points_short"] = (
            "design positions %d < required %d (spec mimo-objective-v1: design >= 6)"
            % (design, design_required))
    if holdout < holdout_required:
        codes.append("hdiag_holdout_points_short")
        reasons["hdiag_holdout_points_short"] = (
            "holdout positions %d < required %d (spec: valid holdout < 3 -> INCOMPLETE)"
            % (holdout, holdout_required))
    if manifest.get("frozen_at_unix") is None:
        codes.append("hdiag_missing_freeze")
        reasons["hdiag_missing_freeze"] = (
            "manifest has no frozen_at_unix: design/holdout pre-registration ordering "
            "cannot be verified")
    if not isinstance(manifest.get("session"), dict):
        codes.append("hdiag_missing_session")
        reasons["hdiag_missing_session"] = (
            "no session.json: microphone/sweep/session provenance and cross-checks are "
            "not machine-verifiable")

    status = "OK" if not codes else "INCOMPLETE"
    return {
        "status": status,
        "ok": status == "OK",
        "codes": codes,
        "reasons": reasons,
        "design_positions": design,
        "holdout_positions": holdout,
        "required": {"design_points": design_required, "holdout_points": holdout_required,
                     "freeze": True, "session_json": True},
    }
