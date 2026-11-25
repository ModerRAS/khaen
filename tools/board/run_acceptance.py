#!/usr/bin/env python3
"""Offline-first orchestrator for tspi-audio-dsp board acceptance (T1/T2/T4/T5).

Live mode SSHes with paramiko. The import happens only inside ``build_transport``
when a live run constructs a connection. ``--dry-run`` never calls that factory,
never opens a socket, and never reads the live soak state.

Exit codes: 0 procedure finished, 1 tier failure, 2 usage, 3 unsafe soak, 4 unreachable.
Exit 0 does not mean ACCEPTANCE PASS. T1 stays ``partial``.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import posixpath
import re
import shlex
import sys
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Callable, Mapping, Protocol, Sequence

TOOL_NAME = "run_acceptance.py"
TOOL_VERSION = "1.0.0"
SCHEMA = "tspi-board-acceptance-run/v1"

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_KEY = ROOT / "deploy" / "ssh" / "tspi_audio_ed25519"
DEFAULT_HOST = "192.0.2.215"
DEFAULT_USER = "root"
DEFAULT_CALIBRATE_BIN = "/tmp/tspi-calibrate"

SOAK_UNIT = "tspi-soak12.service"
SOAK_SERVICE_ARGV = ("systemctl", "is-active", SOAK_UNIT)
SAFE_SOAK_STATES = frozenset({"inactive", "failed", "unknown"})
RUNNING_SOAK_STATES = frozenset({"active", "activating", "deactivating", "reloading"})

REMOTE_ROOT = "/var/lib/tspi-calib"
SOAK_DIR = "/var/lib/tspi-soak"
CAMILLA_YML = f"{SOAK_DIR}/camilla.yml"
SOAK_SCRIPT = f"{SOAK_DIR}/soak12.sh"
STATUS_CSV = f"{SOAK_DIR}/status.csv"
CAMILLA_LOG = f"{SOAK_DIR}/camilla.log"
HW_PARAMS = "/proc/asound/card4/pcm0p/sub0/hw_params"

# Runbook BOARD-T1-T5-RUNBOOK.md lines 100-102. restart_soak5m.sh shows $1 is hours.
T5_SOAK_ARG = "1"
T5_POLL_INTERVAL_S = 30.0
T5_WAIT_MARGIN_S = 900.0

# Runbook line 93. The reporter reads argv / report.json; it does not re-embed this number.
RUNBOOK_T4_CHUNKSIZE = 128

# Runbook lines 91-92. Used only as a labeled dry-run preview when --pairs is omitted.
RUNBOOK_EXAMPLE_PAIRS = (
    "FL:SUB1:80,FR:SUB1:80,C:SUB1:80,SL:SUB1:80,SR:SUB1:80,FL:HFL:2000,FR:HFR:2000"
)

# 5.3.2 playback roles are physical outputs 1..10 (0-based indices 0..9).
# Task: uncovered = the other 9 of 1..10. Do not copy the runbook's 1..22 device list.
LAYOUT_532_OUTPUTS_1BASED = (1, 2, 3, 4, 5, 6, 7, 8, 9, 10)
T1_PLAY_1BASED = 9
T1_CAPTURE_1BASED = 7

# Runbook line 62. ACCEPTANCE §3.1 has a longer sentence; this file uses the runbook string.
RUNBOOK_T1_CLAIM = (
    "T1: partial (out9->in7 only); 1:1 mapping and other channels not verified"
)

T4_CHANNELS: tuple[tuple[str, int], ...] = (
    ("FL", 0),
    ("FR", 1),
    ("C", 2),
    ("SL", 3),
    ("SR", 4),
    ("SUB1", 5),
    ("SUB2", 6),
    ("SUB3", 7),
    ("HFL", 8),
    ("HFR", 9),
)

DRY_RUN_GATE_NOTE = (
    "dry-run does not open SSH, sockets, or paramiko; "
    "the live soak state of tspi-soak12.service cannot be read; "
    "the gate is printed but not evaluated; "
    "a live run refuses before any measurement unless the state is inactive, failed, or unknown"
)

ASSUMPTIONS: tuple[str, ...] = (
    "uncovered_outputs_1based is the nine 5.3.2 outputs in 1..10 except 9 "
    "(ACCEPTANCE §3.1 example). MOTU still has 22 playback channels; outputs 11..22 "
    "are outside that list and are not claimed verified. Runbook line 29's 1..22 list is not copied.",
    "t1.claim is the runbook line 62 string.",
    "t1.cuemix_routing and t1.final_insert_direct_routes come only from --cuemix-state JSON; "
    "otherwise they are null.",
    "SSH user is root, so runbook sudo is omitted (no tty password prompt).",
    "T5 uses the runbook argv soak12.sh 1 and unit tspi-t5-64. "
    "tools/board/restart_soak5m.sh shows $1 is hours (~3600s). "
    "The runbook comment and ACCEPTANCE §7 say 600s/10min. A human must confirm before a live T5.",
    "The 128-frame run is a separate unit tspi-t5-128 with sed s/chunksize: 64/chunksize: 128/. "
    "The runbook only prints the 64 command.",
    "Per-channel fit is skipped; the runbook says merge refits internally.",
    "--negative-controls is omitted (runbook: report records not_run).",
    "T4 run.json chunksize is report.json environment.chunksize when that file is pulled, "
    "else the --chunksize argv that was actually built.",
    "arecord --dump-hw-params preflight follows runbook lines 48 (hw:4,0) and 79 (hw:3,0).",
    "Exit 0 means the procedure and required artifacts completed. It is not T1 PASS.",
    "Final insert CueMix shutdown is out of scope.",
    "Host-key policy matches tools/board/board.py (paramiko AutoAddPolicy).",
)

_PAIR_RE = re.compile(
    r"^[A-Za-z][A-Za-z0-9]*:[A-Za-z][A-Za-z0-9]*:\d+(?:\.\d+)?"
    r"(?:,[A-Za-z][A-Za-z0-9]*:[A-Za-z][A-Za-z0-9]*:\d+(?:\.\d+)?)*$"
)
_CHUNK_RE = re.compile(r"chunksize:\s*(\d+)")
_PERIOD_RE = re.compile(r"period_size\s*[:=]\s*(\d+)", re.IGNORECASE)
_BUFFER_RE = re.compile(r"buffer_size\s*[:=]\s*(\d+)", re.IGNORECASE)
_BIN_CHARS = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/._-")


class UsageError(Exception):
    """CLI or local-input error. Exit 2."""


class UnsafePrecondition(Exception):
    """Soak is not inactive/failed/unknown. Exit 3. No measurement was started."""


class BoardUnreachable(Exception):
    """SSH connect, auth, or transport timeout. Exit 4."""


class TierFailure(Exception):
    """A tier command failed or a required artifact is missing. Exit 1."""


class CommandTimeout(Exception):
    """Remote command exceeded its timeout."""


class Transport(Protocol):
    def connect(self) -> None: ...

    def close(self) -> None: ...

    def exec(self, argv: Sequence[str], timeout: float) -> tuple[int, str, str]: ...

    def exists(self, remote: str) -> bool: ...

    def get(self, remote: str, local: Path) -> None: ...


@dataclass(frozen=True)
class Options:
    tiers: tuple[str, ...]
    dry_run: bool
    out: Path
    host: str
    user: str
    port: int
    key: Path
    calibrate_bin: str
    pairs: str | None
    pairs_placeholder: bool
    cuemix_routing: dict[str, object] | None
    final_insert_direct_routes: list[object] | None
    cuemix_warning: str | None
    invoked: tuple[str, ...]


@dataclass(frozen=True)
class Step:
    tier: str
    subrun: str
    argv: tuple[str, ...]
    timeout_s: float
    kind: str  # gate, exec, allow_fail, readback, wait
    role: str = ""
    expect_chunksize: int | None = None
    wait_timeout_s: float | None = None
    poll_interval_s: float = T5_POLL_INTERVAL_S


@dataclass(frozen=True)
class Artifact:
    remote: str
    local_rel: str
    required: bool
    tier: str
    subrun: str


@dataclass(frozen=True)
class Phase:
    tier: str
    subrun: str
    steps: tuple[Step, ...]
    artifacts: tuple[Artifact, ...]
    meta: Mapping[str, object]


@dataclass
class CommandRecord:
    argv: list[str]
    remote_command: str
    tier: str
    subrun: str
    role: str
    started_at: str
    finished_at: str
    exit_code: int | None
    stdout: str
    stderr: str
    log_stdout: str
    log_stderr: str
    executed: bool
    stdout_truncated: bool = False
    stderr_truncated: bool = False

    def as_dict(self) -> dict[str, object]:
        return {
            "argv": self.argv,
            "remote_command": self.remote_command,
            "tier": self.tier,
            "subrun": self.subrun,
            "role": self.role,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "exit_code": self.exit_code,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "log_stdout": self.log_stdout,
            "log_stderr": self.log_stderr,
            "executed": self.executed,
            "stdout_truncated": self.stdout_truncated,
            "stderr_truncated": self.stderr_truncated,
        }


def now_iso() -> str:
    return datetime.now().astimezone().isoformat(timespec="seconds")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def remote_shell(argv: Sequence[str]) -> str:
    """Quote argv for the remote POSIX shell. No local shell is invoked."""
    return " ".join(shlex.quote(part) for part in argv)


def soak_state_token(stdout: str) -> str:
    """Return the single systemctl token, or '' when the output is not one line."""
    lines = [line.strip() for line in stdout.splitlines() if line.strip()]
    if len(lines) != 1:
        return ""
    return lines[0].lower()


def decide_soak(stdout: str) -> tuple[bool, str]:
    """Fail closed. Only an exact inactive/failed/unknown token is safe."""
    token = soak_state_token(stdout)
    if token in SAFE_SOAK_STATES:
        return True, token
    if not token:
        return False, "empty-or-multiline"
    return False, token


def validate_calibrate_bin(path: str) -> str | None:
    if not path.startswith("/"):
        return "calibrate bin must be an absolute remote path"
    if posixpath.normpath(path) != path:
        return "calibrate bin must be a normalized absolute path"
    if any(part == ".." for part in path.split("/")):
        return "calibrate bin must not contain .."
    if any(ch not in _BIN_CHARS for ch in path):
        return "calibrate bin contains unsafe characters"
    if posixpath.basename(path) != "tspi-calibrate":
        return "calibrate bin basename must be tspi-calibrate"
    return None


def validate_pairs(pairs: str) -> str | None:
    if not pairs or _PAIR_RE.fullmatch(pairs) is None:
        return "pairs must be Name:Name:Hz comma-separated tokens (example FL:SUB1:80,FR:HFR:2000)"
    return None


def parse_single_chunksize(text: str) -> int | None:
    found = _CHUNK_RE.findall(text)
    if len(found) != 1:
        return None
    return int(found[0])


def chunksize_from_argv(argv: Sequence[str]) -> int | None:
    items = list(argv)
    if "--chunksize" not in items:
        return None
    index = items.index("--chunksize")
    if index + 1 >= len(items):
        return None
    raw = items[index + 1]
    if not raw.isdigit():
        return None
    return int(raw)


def chunksize_from_report(text: str) -> int | None:
    try:
        data = json.loads(text)
    except json.JSONDecodeError:
        return None
    if not isinstance(data, dict):
        return None
    env = data.get("environment")
    if not isinstance(env, dict) or "chunksize" not in env:
        return None
    raw = env["chunksize"]
    if isinstance(raw, bool):
        return None
    if isinstance(raw, int):
        return raw
    if isinstance(raw, str) and raw.isdigit():
        return int(raw)
    return None


def resolve_chunksize(report_text: str | None, argv: Sequence[str]) -> tuple[int | None, str, bool]:
    """Return (chunksize, source, mismatch).

    The report value wins when present. There is no third hardcoded fallback.
    """
    argv_value = chunksize_from_argv(argv)
    if report_text is not None:
        reported = chunksize_from_report(report_text)
        if reported is not None:
            mismatch = argv_value is not None and argv_value != reported
            return reported, "report.environment.chunksize", mismatch
    if argv_value is not None:
        return argv_value, "argv", False
    return None, "missing", False


def parse_period_buffer(text: str) -> tuple[int | None, int | None]:
    periods = _PERIOD_RE.findall(text)
    buffers = _BUFFER_RE.findall(text)
    period = int(periods[-1]) if periods else None
    buffer = int(buffers[-1]) if buffers else None
    return period, buffer


def summarize_status_csv(text: str) -> dict[str, object]:
    """Best-effort xrun view. Unknown layouts stay unparsed (count is null)."""
    lines = [line for line in text.splitlines() if line.strip()]
    if not lines:
        return {"rows": 0, "xrun_count": None, "reason": "empty"}
    header = [cell.strip() for cell in lines[0].split(",")]
    xrun_cols = [i for i, name in enumerate(header) if "xrun" in name.lower()]
    body = lines[1:]
    if xrun_cols:
        values: list[float] = []
        for line in body:
            cells = line.split(",")
            for index in xrun_cols:
                if index >= len(cells):
                    continue
                try:
                    values.append(float(cells[index].strip() or "0"))
                except ValueError:
                    return {
                        "rows": len(body),
                        "xrun_count": None,
                        "reason": "unparsed",
                        "columns": [header[i] for i in xrun_cols],
                    }
        return {
            "rows": len(body),
            "xrun_count": int(sum(values)) if values else 0,
            "xrun_max": max(values) if values else 0,
            "reason": "header",
            "columns": [header[i] for i in xrun_cols],
        }
    positive = 0
    parsed = 0
    maximum: float | None = None
    total = 0.0
    for line in body:
        cells = line.split(",")
        if len(cells) < 4:
            continue
        try:
            value = float(cells[3].strip())
        except ValueError:
            continue
        parsed += 1
        total += value
        maximum = value if maximum is None else max(maximum, value)
        if value > 0:
            positive += 1
    if parsed == 0:
        return {"rows": len(body), "xrun_count": None, "reason": "no_xrun_column"}
    return {
        "rows": len(body),
        "xrun_count": None,
        "reason": "column4_probe_only",
        "column4_positive_rows": positive,
        "column4_sum": total,
        "column4_max": maximum,
        "note": "xrun_probe.sh treats field 4 as numeric; header has no xrun column so this is not a pass/fail",
    }


def t1_report_block(
    routing: dict[str, object] | None,
    routes: list[object] | None,
    warning: str | None,
) -> dict[str, object]:
    covered_outputs = [T1_PLAY_1BASED]
    uncovered = [n for n in LAYOUT_532_OUTPUTS_1BASED if n not in covered_outputs]
    return {
        "status": "partial",
        "loopback_map_1based": [{"play": T1_PLAY_1BASED, "capture": T1_CAPTURE_1BASED}],
        "covered_outputs_1based": covered_outputs,
        "covered_inputs_1based": [T1_CAPTURE_1BASED],
        "uncovered_outputs_1based": uncovered,
        "uncovered_outputs_basis": (
            "5.3.2 physical outputs 1..10. MOTU playback has 22 channels; "
            "outputs 11..22 are outside this list and are not verified."
        ),
        "loopback_mapping": None,
        "isolation_max_other_db": None,
        "cuemix_routing": routing,
        "final_insert_direct_routes": routes,
        "cuemix_warning": warning,
        "mitigation": ["file_backend", "t4_acoustic"],
        "mitigation_note": "remaining outputs via file backend + T4 UMIK acoustic evidence",
        "claim": RUNBOOK_T1_CLAIM,
    }


def _measure_tail(period: str = "128", buffer: str = "512") -> list[str]:
    return [
        "--rate",
        "48000",
        "--seconds",
        "5",
        "--level-db",
        "-30",
        "--pre-seconds",
        "1.5",
        "--post-seconds",
        "1.5",
        "--period-size",
        period,
        "--buffer-size",
        buffer,
    ]


def t1_measure_argv(calibrate_bin: str) -> list[str]:
    """Runbook lines 49-53. Capture is MOTU hw:4,0, not the UMIK."""
    return [
        calibrate_bin,
        "measure-multi",
        "--out-dir",
        f"{REMOTE_ROOT}/t1",
        "--out-prefix",
        f"{REMOTE_ROOT}/t1/t1",
        "--loopback-map",
        "9:7",
        "--names",
        "OUT9_IN7",
        "--playback",
        "hw:4,0",
        "--playback-channels",
        "22",
        "--capture",
        "hw:4,0",
        "--capture-channels",
        "8",
        *_measure_tail(),
        "--verify-mapping",
    ]


def t2_measure_argv(calibrate_bin: str, run_index: int) -> list[str]:
    """Runbook lines 66-70. One directory per repeat."""
    remote = f"{REMOTE_ROOT}/t2/run{run_index}"
    return [
        calibrate_bin,
        "measure-multi",
        "--out-dir",
        remote,
        "--out-prefix",
        remote,
        "--channels",
        "0,1",
        "--names",
        "FL,FR",
        "--playback",
        "hw:4,0",
        "--playback-channels",
        "22",
        "--capture",
        "hw:3,0",
        "--capture-channels",
        "1",
        *_measure_tail(),
        "--mic",
        "umik",
    ]


def t4_measure_argv(calibrate_bin: str) -> list[str]:
    """Runbook lines 82-86. 0-based map FL0 FR1 C2 SL3 SR4 SUB1=5 SUB2=6 SUB3=7 HFL=8 HFR=9."""
    return [
        calibrate_bin,
        "measure-multi",
        "--out-dir",
        f"{REMOTE_ROOT}/t4",
        "--out-prefix",
        f"{REMOTE_ROOT}/t4/t4",
        "--channels",
        "0,1,2,3,4,5,6,7,8,9",
        "--names",
        "FL,FR,C,SL,SR,SUB1,SUB2,SUB3,HFL,HFR",
        "--playback",
        "hw:4,0",
        "--playback-channels",
        "22",
        "--capture",
        "hw:3,0",
        "--capture-channels",
        "1",
        *_measure_tail(),
        "--mic",
        "umik",
        "--polarity-offset",
        "normal",
    ]


def t4_merge_argv(calibrate_bin: str, pairs: str, chunksize: int = RUNBOOK_T4_CHUNKSIZE) -> list[str]:
    """Runbook lines 90-93. pairs is required. negative-controls is intentionally absent."""
    return [
        calibrate_bin,
        "merge",
        "--dir",
        f"{REMOTE_ROOT}/t4",
        "--out",
        f"{REMOTE_ROOT}/t4/calibration.yml",
        "--names",
        "FL,FR,C,SL,SR,SUB1,SUB2,SUB3,HFL,HFR",
        "--layout",
        "5.3.2",
        "--crossovers",
        "80,2000",
        "--pairs",
        pairs,
        "--chunksize",
        str(chunksize),
    ]


def t5_sed_argv(chunksize: int) -> list[str]:
    if chunksize == 64:
        expression = "s/chunksize: 128/chunksize: 64/"
    elif chunksize == 128:
        expression = "s/chunksize: 64/chunksize: 128/"
    else:
        raise UsageError(f"T5 chunksize must be 64 or 128, got {chunksize}")
    return ["sed", "-i", expression, CAMILLA_YML]


def t5_systemd_argv(chunksize: int) -> list[str]:
    unit = f"tspi-t5-{chunksize}"
    return [
        "systemd-run",
        f"--unit={unit}",
        "--property=Type=simple",
        "/bin/bash",
        SOAK_SCRIPT,
        T5_SOAK_ARG,
    ]


def t5_wait_timeout_s() -> float:
    """Upper bound. Polling stops when the unit leaves the running set."""
    return float(T5_SOAK_ARG) * 3600.0 + T5_WAIT_MARGIN_S


def _step(
    tier: str,
    subrun: str,
    argv: Sequence[str],
    timeout_s: float,
    kind: str,
    role: str = "",
    expect_chunksize: int | None = None,
    wait_timeout_s: float | None = None,
) -> Step:
    return Step(
        tier=tier,
        subrun=subrun,
        argv=tuple(argv),
        timeout_s=timeout_s,
        kind=kind,
        role=role,
        expect_chunksize=expect_chunksize,
        wait_timeout_s=wait_timeout_s,
    )


def _gate(tier: str, subrun: str) -> Step:
    return _step(tier, subrun, SOAK_SERVICE_ARGV, 20.0, "gate", role="soak-gate")


def _analysis_artifacts(
    tier: str,
    subrun: str,
    remote_dir: str,
    local_prefix: str,
    names: Sequence[tuple[str, int]],
) -> tuple[Artifact, ...]:
    found: list[Artifact] = []
    for name, index in names:
        stem = f"{name}-ch{index:02}"
        for suffix in ("fr.csv", "analysis.json", "diag.json"):
            filename = f"{stem}.{suffix}"
            found.append(
                Artifact(
                    remote=f"{remote_dir}/{filename}",
                    local_rel=f"{local_prefix}/{filename}",
                    required=True,
                    tier=tier,
                    subrun=subrun,
                )
            )
    return tuple(found)


def _phase_t1(opts: Options) -> Phase:
    subrun = "t1"
    steps = (
        _gate("t1", subrun),
        _step("t1", subrun, ["mkdir", "-p", f"{REMOTE_ROOT}/t1"], 20.0, "exec", role="mkdir"),
        _step(
            "t1",
            subrun,
            ["arecord", "-D", "hw:4,0", "--dump-hw-params", "/dev/null"],
            30.0,
            "exec",
            role="preflight-capture",
        ),
        _step("t1", subrun, t1_measure_argv(opts.calibrate_bin), 180.0, "exec", role="measure"),
    )
    artifacts = _analysis_artifacts(
        "t1",
        subrun,
        f"{REMOTE_ROOT}/t1",
        "t1",
        (("OUT9_IN7", T1_PLAY_1BASED - 1),),
    )
    optional = Artifact(
        remote=f"{REMOTE_ROOT}/t1/cuemix-routing.png",
        local_rel="t1/cuemix-routing.png",
        required=False,
        tier="t1",
        subrun=subrun,
    )
    meta = {
        "t1": t1_report_block(opts.cuemix_routing, opts.final_insert_direct_routes, opts.cuemix_warning),
    }
    return Phase("t1", subrun, steps, artifacts + (optional,), meta)


def _phase_t2(opts: Options, run_index: int) -> Phase:
    subrun = f"run{run_index}"
    remote = f"{REMOTE_ROOT}/t2/{subrun}"
    steps = [
        _gate("t2", subrun),
        _step("t2", subrun, ["mkdir", "-p", remote], 20.0, "exec", role="mkdir"),
    ]
    if run_index == 1:
        steps.append(
            _step(
                "t2",
                subrun,
                ["arecord", "-D", "hw:3,0", "--dump-hw-params", "/dev/null"],
                30.0,
                "exec",
                role="preflight-umik",
            )
        )
    steps.append(_step("t2", subrun, t2_measure_argv(opts.calibrate_bin, run_index), 240.0, "exec", role="measure"))
    artifacts = _analysis_artifacts(
        "t2",
        subrun,
        remote,
        f"t2/{subrun}",
        (("FL", 0), ("FR", 1)),
    )
    return Phase("t2", subrun, tuple(steps), artifacts, {"remote_dir": remote, "repeat": run_index})


def _phase_t4_measure(opts: Options) -> Phase:
    subrun = "measure"
    steps = (
        _gate("t4", subrun),
        _step("t4", subrun, ["mkdir", "-p", f"{REMOTE_ROOT}/t4"], 20.0, "exec", role="mkdir"),
        _step(
            "t4",
            subrun,
            ["arecord", "-D", "hw:3,0", "--dump-hw-params", "/dev/null"],
            30.0,
            "exec",
            role="preflight-umik",
        ),
        _step("t4", subrun, t4_measure_argv(opts.calibrate_bin), 900.0, "exec", role="measure"),
    )
    artifacts = _analysis_artifacts("t4", subrun, f"{REMOTE_ROOT}/t4", "t4", T4_CHANNELS)
    return Phase("t4", subrun, steps, artifacts, {"layout": "5.3.2", "channels": [name for name, _ in T4_CHANNELS]})


def _phase_t4_merge(opts: Options) -> Phase:
    if not opts.pairs:
        raise UsageError("internal: T4 merge built without pairs")
    subrun = "merge"
    argv = t4_merge_argv(opts.calibrate_bin, opts.pairs)
    steps = (
        _gate("t4", subrun),
        _step("t4", subrun, argv, 300.0, "exec", role="merge"),
    )
    remote_base = f"{REMOTE_ROOT}/t4/calibration.yml"
    required = (
        Artifact(remote_base, "t4/calibration.yml", True, "t4", subrun),
        Artifact(f"{remote_base}.pairs.json", "t4/calibration.yml.pairs.json", True, "t4", subrun),
        Artifact(f"{remote_base}.report.json", "t4/calibration.yml.report.json", True, "t4", subrun),
    )
    optional = (
        Artifact(f"{remote_base}.validation.json", "t4/calibration.yml.validation.json", False, "t4", subrun),
        Artifact(f"{remote_base}.camilladsp.yml", "t4/calibration.yml.camilladsp.yml", False, "t4", subrun),
    )
    meta = {
        "pairs": opts.pairs,
        "pairs_placeholder": opts.pairs_placeholder,
        "chunksize_argv": chunksize_from_argv(argv),
        "negative_controls": "omitted",
    }
    return Phase("t4", subrun, steps, required + optional, meta)


def _phase_t5(chunksize: int) -> Phase:
    subrun = f"chunksize-{chunksize}"
    remote_dir = f"{REMOTE_ROOT}/t5/{subrun}"
    unit = f"tspi-t5-{chunksize}"
    service = f"{unit}.service"
    steps = (
        _gate("t5", subrun),
        _step("t5", subrun, ["grep", "-n", "chunksize:", CAMILLA_YML], 20.0, "exec", role="chunksize-before"),
        _step("t5", subrun, t5_sed_argv(chunksize), 20.0, "exec", role="set-chunksize"),
        _step(
            "t5",
            subrun,
            ["grep", "-n", "chunksize:", CAMILLA_YML],
            20.0,
            "readback",
            role="chunksize-readback",
            expect_chunksize=chunksize,
        ),
        _step("t5", subrun, t5_systemd_argv(chunksize), 30.0, "exec", role="systemd-run"),
        _step(
            "t5",
            subrun,
            ["systemctl", "is-active", service],
            20.0,
            "wait",
            role="wait-unit",
            wait_timeout_s=t5_wait_timeout_s(),
        ),
        _step(
            "t5",
            subrun,
            ["systemctl", "show", service, "-p", "ActiveState", "-p", "Result", "-p", "ExecMainStatus"],
            20.0,
            "exec",
            role="systemd-show",
        ),
        _step("t5", subrun, ["mkdir", "-p", remote_dir], 20.0, "exec", role="mkdir"),
        _step("t5", subrun, ["cp", "-a", STATUS_CSV, f"{remote_dir}/status.csv"], 60.0, "exec", role="snapshot-status"),
        _step("t5", subrun, ["cp", "-a", CAMILLA_LOG, f"{remote_dir}/camilla.log"], 60.0, "exec", role="snapshot-log"),
        _step("t5", subrun, ["tail", "-n", "200", CAMILLA_LOG], 20.0, "allow_fail", role="log-tail"),
        _step("t5", subrun, ["ps", "-eLo", "pid,cls,rtprio,psr,comm"], 20.0, "allow_fail", role="rt-threads"),
        _step("t5", subrun, ["cat", HW_PARAMS], 20.0, "allow_fail", role="hw-params"),
        _step("t5", subrun, ["dmesg", "-T"], 20.0, "allow_fail", role="dmesg"),
    )
    artifacts = (
        Artifact(f"{remote_dir}/status.csv", f"t5/{subrun}/status.csv", True, "t5", subrun),
        Artifact(f"{remote_dir}/camilla.log", f"t5/{subrun}/camilla.log", True, "t5", subrun),
    )
    meta = {
        "chunksize_expected": chunksize,
        "chunksize_observed": None,
        "chunksize_source": None,
        "unit": unit,
        "service": service,
        "soak_script": SOAK_SCRIPT,
        "soak_script_arg": T5_SOAK_ARG,
        "soak_script_arg_assumption": "hours (see restart_soak5m.sh); runbook comment says 600s",
        "period_size": None,
        "buffer_size": None,
        "remote_dir": remote_dir,
        "remote_status_csv": f"{remote_dir}/status.csv",
        "remote_camilla_log": f"{remote_dir}/camilla.log",
        "xrun": None,
    }
    return Phase("t5", subrun, steps, artifacts, meta)


def build_phases(opts: Options) -> tuple[Phase, ...]:
    phases: list[Phase] = []
    for tier in opts.tiers:
        if tier == "t1":
            phases.append(_phase_t1(opts))
        elif tier == "t2":
            for index in (1, 2, 3):
                phases.append(_phase_t2(opts, index))
        elif tier == "t4":
            phases.append(_phase_t4_measure(opts))
            phases.append(_phase_t4_merge(opts))
        elif tier == "t5":
            phases.append(_phase_t5(64))
            phases.append(_phase_t5(128))
        else:
            raise UsageError(f"unknown tier {tier}")
    return tuple(phases)


def identity_steps() -> tuple[Step, ...]:
    commands = (
        ("hostname", ["hostname"]),
        ("uname", ["uname", "-a"]),
        ("date", ["date", "+%Y-%m-%dT%H:%M:%S%z"]),
        ("asound_cards", ["cat", "/proc/asound/cards"]),
    )
    return tuple(
        _step("identity", "board", argv, 20.0, "allow_fail", role=role) for role, argv in commands
    )


def _cap_text(text: str, limit: int = 524288) -> tuple[str, bool]:
    if len(text) <= limit:
        return text, False
    return text[:limit] + "\n...[truncated; full text is in the log file]\n", True


class Recorder:
    def __init__(self, opts: Options) -> None:
        self.opts = opts
        self.path = opts.out / "run.json"
        self.log_dir = opts.out / "logs"
        self.started_at = now_iso()
        self.commands: list[CommandRecord] = []
        self.artifacts: list[dict[str, object]] = []
        self.warnings: list[str] = []
        self.precondition_checks: list[dict[str, object]] = []
        self.identity: dict[str, object] = {}
        self.tier_state: dict[str, dict[str, object]] = {}
        self.error: str | None = None
        self._seq = 0
        if opts.cuemix_warning:
            self.warnings.append(opts.cuemix_warning)
        if opts.pairs_placeholder:
            self.warnings.append(
                "dry-run T4 merge uses the runbook example --pairs because none was supplied; "
                "a live run without --pairs exits 2 and does not connect"
            )

    def begin(self) -> None:
        self.log_dir.mkdir(parents=True, exist_ok=True)
        self.write()

    def write(self) -> None:
        payload = self.snapshot(exit_code=None)
        # snapshot exit filled by finish(); begin() writes a partial file too
        self._atomic(payload)

    def snapshot(self, exit_code: int | None) -> dict[str, object]:
        return {
            "schema": SCHEMA,
            "tool": {"name": TOOL_NAME, "version": TOOL_VERSION},
            "invoked_arguments": list(self.opts.invoked),
            "dry_run": self.opts.dry_run,
            "started_at": self.started_at,
            "updated_at": now_iso(),
            "exit_code": exit_code,
            "host": self.opts.host,
            "user": self.opts.user,
            "calibrate_bin": self.opts.calibrate_bin,
            "assumptions": list(ASSUMPTIONS),
            "warnings": list(self.warnings),
            "error": self.error,
            "board": {
                "host": self.opts.host,
                "user": self.opts.user,
                "port": self.opts.port,
                "identity": self.identity,
            },
            "precondition": {
                "unit": SOAK_UNIT,
                "safe_states": sorted(SAFE_SOAK_STATES),
                "checks": self.precondition_checks,
                "evaluated": any(item.get("evaluated") for item in self.precondition_checks),
            },
            "commands": [item.as_dict() for item in self.commands],
            "artifacts": self.artifacts,
            "tiers": self.tier_state,
        }

    def finish(self, exit_code: int) -> None:
        payload = self.snapshot(exit_code)
        payload["exit_code"] = exit_code
        payload["updated_at"] = now_iso()
        self._atomic(payload)

    def _atomic(self, payload: dict[str, object]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        temporary = self.path.with_name(self.path.name + ".tmp")
        text = json.dumps(payload, indent=2, ensure_ascii=False) + "\n"
        temporary.write_text(text, encoding="utf-8", newline="\n")
        os.replace(temporary, self.path)

    def add_command(self, record: CommandRecord) -> None:
        self.commands.append(record)
        if record.tier == "identity":
            self.identity[record.role] = record.as_dict()
        self.write()

    def add_precondition(self, stdout: str, safe: bool, detail: str, evaluated: bool) -> None:
        self.precondition_checks.append(
            {
                "unit": SOAK_UNIT,
                "stdout": stdout,
                "safe": safe,
                "detail": detail,
                "evaluated": evaluated,
                "at": now_iso(),
            }
        )
        self.write()

    def add_warning(self, message: str) -> None:
        self.warnings.append(message)
        self.write()

    def ensure_tier(self, tier: str) -> dict[str, object]:
        state = self.tier_state.get(tier)
        if state is None:
            state = {"status": "pending", "subruns": []}
            self.tier_state[tier] = state
        return state

    def record_subrun(self, phase: Phase, status: str, extra: Mapping[str, object] | None = None) -> None:
        state = self.ensure_tier(phase.tier)
        subruns = state["subruns"]
        assert isinstance(subruns, list)
        meta = dict(phase.meta)
        if extra:
            for key, value in extra.items():
                meta[key] = value
        body: dict[str, object] = {"id": phase.subrun, "status": status, "meta": meta}
        replaced = False
        for index, existing in enumerate(subruns):
            if isinstance(existing, dict) and existing.get("id") == phase.subrun:
                subruns[index] = body
                replaced = True
                break
        if not replaced:
            subruns.append(body)
        statuses = [item.get("status") for item in subruns if isinstance(item, dict)]
        if "failed" in statuses:
            state["status"] = "failed"
        elif statuses and all(item == "ok" for item in statuses):
            state["status"] = "ok"
        elif statuses and all(item == "dry_run" for item in statuses):
            state["status"] = "dry_run"
        elif statuses and all(item == "skipped" for item in statuses):
            state["status"] = "skipped"
        elif statuses:
            state["status"] = "incomplete"
        self.write()

    def mark_skipped(self, phase: Phase) -> None:
        self.record_subrun(phase, "skipped")

    def add_artifact(self, record: dict[str, object]) -> None:
        self.artifacts.append(record)
        self.write()

    def next_log_paths(self, tier: str, argv: Sequence[str]) -> tuple[str, str, Path, Path]:
        self._seq += 1
        head = argv[0].rsplit("/", 1)[-1] if argv else "cmd"
        extra = ""
        if len(argv) > 1 and not str(argv[1]).startswith("-"):
            extra = "-" + str(argv[1])
        raw = f"{self._seq:03d}-{tier}-{head}{extra}"
        slug = re.sub(r"[^A-Za-z0-9._-]+", "_", raw)[:90]
        out_rel = f"logs/{slug}.out.txt"
        err_rel = f"logs/{slug}.err.txt"
        return out_rel, err_rel, self.opts.out / out_rel, self.opts.out / err_rel


def _store_logs(recorder: Recorder, tier: str, argv: Sequence[str], stdout: str, stderr: str) -> tuple[str, str, str, str, bool, bool]:
    out_rel, err_rel, out_path, err_path = recorder.next_log_paths(tier, argv)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(stdout, encoding="utf-8", newline="\n")
    err_path.write_text(stderr, encoding="utf-8", newline="\n")
    capped_out, out_flag = _cap_text(stdout)
    capped_err, err_flag = _cap_text(stderr)
    return out_rel, err_rel, capped_out, capped_err, out_flag, err_flag


def _record_exec(
    recorder: Recorder,
    step: Step,
    started: str,
    exit_code: int | None,
    stdout: str,
    stderr: str,
    executed: bool,
) -> CommandRecord:
    out_rel, err_rel, capped_out, capped_err, out_flag, err_flag = _store_logs(
        recorder, step.tier, step.argv, stdout, stderr
    )
    record = CommandRecord(
        argv=list(step.argv),
        remote_command=remote_shell(step.argv),
        tier=step.tier,
        subrun=step.subrun,
        role=step.role,
        started_at=started,
        finished_at=now_iso(),
        exit_code=exit_code,
        stdout=capped_out,
        stderr=capped_err,
        log_stdout=out_rel,
        log_stderr=err_rel,
        executed=executed,
        stdout_truncated=out_flag,
        stderr_truncated=err_flag,
    )
    recorder.add_command(record)
    return record


def execute_step(
    transport: Transport,
    recorder: Recorder,
    step: Step,
    sleep: Callable[[float], None] = time.sleep,
    monotonic: Callable[[], float] = time.monotonic,
) -> CommandRecord:
    if step.kind == "wait":
        return _wait_unit(transport, recorder, step, sleep, monotonic)
    started = now_iso()
    try:
        exit_code, stdout, stderr = transport.exec(step.argv, step.timeout_s)
    except CommandTimeout as exc:
        _record_exec(recorder, step, started, None, "", str(exc), True)
        if step.kind == "gate":
            raise BoardUnreachable(f"soak gate timed out: {exc}") from exc
        raise TierFailure(f"{step.tier} {step.role} timed out: {exc}") from exc
    except BoardUnreachable:
        _record_exec(recorder, step, started, None, "", "transport lost", True)
        raise
    record = _record_exec(recorder, step, started, exit_code, stdout, stderr, True)
    if step.kind == "gate":
        safe, detail = decide_soak(stdout)
        recorder.add_precondition(stdout, safe, detail, evaluated=True)
        if not safe:
            raise UnsafePrecondition(
                f"{SOAK_UNIT} state {detail!r} is not inactive, failed, or unknown; "
                "refusing before any measurement or config change"
            )
        return record
    if step.kind == "readback":
        if exit_code != 0:
            raise TierFailure(f"{step.tier} chunksize readback exited {exit_code}")
        observed = parse_single_chunksize(stdout)
        if observed is None or observed != step.expect_chunksize:
            raise TierFailure(
                f"{step.tier} {step.subrun} chunksize readback {observed!r} != {step.expect_chunksize}"
            )
        _note_observed_chunksize(recorder, step, observed)
        return record
    if step.role == "systemd-show":
        if exit_code != 0 or not systemd_result_ok(stdout):
            raise TierFailure(f"{step.tier} {step.subrun} systemd result not success: {stdout!r}")
        return record
    if step.kind == "allow_fail":
        return record
    if exit_code != 0:
        raise TierFailure(f"{step.tier} {step.role} exited {exit_code}: {remote_shell(step.argv)}")
    return record


def systemd_result_ok(stdout: str) -> bool:
    fields: dict[str, str] = {}
    for line in stdout.splitlines():
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        fields[key.strip()] = value.strip()
    return fields.get("Result") == "success" and fields.get("ExecMainStatus") == "0"


def _note_observed_chunksize(recorder: Recorder, step: Step, observed: int) -> None:
    state = recorder.ensure_tier(step.tier)
    subruns = state["subruns"]
    assert isinstance(subruns, list)
    for item in subruns:
        if isinstance(item, dict) and item.get("id") == step.subrun:
            meta = item.get("meta")
            if isinstance(meta, dict):
                meta["chunksize_observed"] = observed
                meta["chunksize_source"] = "camilla.yml readback"
                meta["chunksize"] = observed
            recorder.write()
            return


def _wait_unit(
    transport: Transport,
    recorder: Recorder,
    step: Step,
    sleep: Callable[[float], None],
    monotonic: Callable[[], float],
) -> CommandRecord:
    deadline = monotonic() + float(step.wait_timeout_s or 0)
    last: CommandRecord | None = None
    while True:
        started = now_iso()
        try:
            exit_code, stdout, stderr = transport.exec(step.argv, step.timeout_s)
        except CommandTimeout as exc:
            _record_exec(recorder, step, started, None, "", str(exc), True)
            raise TierFailure(f"{step.tier} wait poll timed out: {exc}") from exc
        except BoardUnreachable:
            _record_exec(recorder, step, started, None, "", "transport lost", True)
            raise
        last = _record_exec(recorder, step, started, exit_code, stdout, stderr, True)
        token = soak_state_token(stdout)
        if token in SAFE_SOAK_STATES:
            return last
        if token not in RUNNING_SOAK_STATES:
            raise TierFailure(f"{step.tier} {step.subrun} wait saw unexpected state {token or stdout!r}")
        if monotonic() >= deadline:
            raise TierFailure(
                f"{step.tier} {step.subrun} still {token} after {step.wait_timeout_s}s; "
                "the other chunksize was not started"
            )
        sleep(step.poll_interval_s)


def pull_artifacts(transport: Transport, recorder: Recorder, phase: Phase) -> None:
    missing_required: list[str] = []
    for artifact in phase.artifacts:
        local = recorder.opts.out / artifact.local_rel
        record: dict[str, object] = {
            "tier": artifact.tier,
            "subrun": artifact.subrun,
            "remote": artifact.remote,
            "local": artifact.local_rel,
            "required": artifact.required,
            "present": False,
            "sha256": None,
            "bytes": None,
        }
        try:
            present = transport.exists(artifact.remote)
        except BoardUnreachable:
            recorder.add_artifact(record)
            raise
        if not present:
            recorder.add_artifact(record)
            message = f"missing artifact {artifact.remote}"
            if artifact.required:
                missing_required.append(artifact.remote)
            else:
                recorder.add_warning(message + " (optional)")
            continue
        try:
            transport.get(artifact.remote, local)
        except BoardUnreachable:
            recorder.add_artifact(record)
            raise
        except OSError as exc:
            record["error"] = str(exc)
            recorder.add_artifact(record)
            if artifact.required:
                missing_required.append(artifact.remote)
            else:
                recorder.add_warning(f"optional artifact {artifact.remote} failed to download: {exc}")
            continue
        digest = sha256_file(local)
        record["present"] = True
        record["sha256"] = digest
        record["bytes"] = local.stat().st_size
        recorder.add_artifact(record)
    if missing_required:
        raise TierFailure("missing required artifacts: " + ", ".join(missing_required))


def _commands_for(recorder: Recorder, phase: Phase, role: str) -> list[CommandRecord]:
    return [
        item
        for item in recorder.commands
        if item.tier == phase.tier and item.subrun == phase.subrun and item.role == role and item.executed
    ]


def _enrich_phase(recorder: Recorder, phase: Phase) -> dict[str, object]:
    extra: dict[str, object] = {}
    if phase.tier == "t1":
        block = dict(phase.meta["t1"]) if isinstance(phase.meta.get("t1"), dict) else {}
        measures = _commands_for(recorder, phase, "measure")
        if measures:
            block["loopback_mapping"] = "pass" if measures[-1].exit_code == 0 else "fail"
        diag = recorder.opts.out / "t1" / "OUT9_IN7-ch08.diag.json"
        if diag.is_file():
            try:
                payload = json.loads(diag.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                payload = None
            if isinstance(payload, dict) and "isolation_other_max_db" in payload:
                block["isolation_max_other_db"] = payload["isolation_other_max_db"]
        extra["t1"] = block
    if phase.tier == "t4" and phase.subrun == "merge":
        merges = _commands_for(recorder, phase, "merge")
        argv = merges[-1].argv if merges else []
        report_path = recorder.opts.out / "t4" / "calibration.yml.report.json"
        report_text = report_path.read_text(encoding="utf-8") if report_path.is_file() else None
        chunksize, source, mismatch = resolve_chunksize(report_text, argv)
        extra["chunksize"] = chunksize
        extra["chunksize_source"] = source
        extra["chunksize_mismatch"] = mismatch
        if mismatch:
            recorder.add_warning(
                f"T4 report chunksize {chunksize} differs from merge argv {chunksize_from_argv(argv)}"
            )
    if phase.tier == "t5":
        text_parts: list[str] = []
        for role in ("log-tail", "hw-params", "systemd-show"):
            for command in _commands_for(recorder, phase, role):
                text_parts.append(command.stdout)
        log_path = recorder.opts.out / "t5" / phase.subrun / "camilla.log"
        if log_path.is_file():
            text_parts.append(log_path.read_text(encoding="utf-8", errors="replace"))
        period, buffer = parse_period_buffer("\n".join(text_parts))
        status_path = recorder.opts.out / "t5" / phase.subrun / "status.csv"
        xrun: dict[str, object] | None = None
        if status_path.is_file():
            xrun = summarize_status_csv(status_path.read_text(encoding="utf-8", errors="replace"))
        observed = None
        for command in _commands_for(recorder, phase, "chunksize-readback"):
            observed = parse_single_chunksize(command.stdout)
        extra["chunksize"] = observed
        extra["chunksize_source"] = "camilla.yml readback" if observed is not None else None
        extra["period_size"] = period
        extra["buffer_size"] = buffer
        extra["xrun"] = xrun
        extra["unit"] = phase.meta.get("unit")
        extra["service"] = phase.meta.get("service")
    return extra


def run_phase(
    transport: Transport,
    recorder: Recorder,
    phase: Phase,
    sleep: Callable[[float], None],
    monotonic: Callable[[], float],
) -> None:
    failure: Exception | None = None
    for step in phase.steps:
        if failure is not None:
            started = now_iso()
            _record_exec(
                recorder,
                step,
                started,
                None,
                "",
                "skipped after an earlier failure in this subrun",
                False,
            )
            continue
        try:
            execute_step(transport, recorder, step, sleep=sleep, monotonic=monotonic)
        except (TierFailure, UnsafePrecondition, BoardUnreachable) as exc:
            failure = exc
            if isinstance(exc, (UnsafePrecondition, BoardUnreachable)):
                # Do not keep issuing commands, including artifact pulls, after a hard stop.
                raise
    try:
        pull_artifacts(transport, recorder, phase)
    except TierFailure as exc:
        failure = failure or exc
    extra = _enrich_phase(recorder, phase)
    if failure is not None:
        recorder.record_subrun(phase, "failed", extra)
        if isinstance(failure, TierFailure):
            raise failure
        raise TierFailure(str(failure))
    recorder.record_subrun(phase, "ok", extra)


def print_dry_run(opts: Options, phases: Sequence[Phase], stream: object | None = None) -> None:
    out = stream or sys.stdout
    print("DRY-RUN transport=none", file=out)
    print(f"NOTE: {DRY_RUN_GATE_NOTE}", file=out)
    if opts.pairs_placeholder:
        print(
            "WARNING: --pairs omitted; T4 merge preview uses the runbook example pairs. "
            "A live t4/all without --pairs exits 2 and does not connect.",
            file=out,
        )
    if opts.cuemix_warning:
        print(f"WARNING: {opts.cuemix_warning}", file=out)
    print(f"GATE (not executed) argv: {remote_shell(SOAK_SERVICE_ARGV)}", file=out)
    print("IDENTITY (after the gate passes, not executed)", file=out)
    for step in identity_steps():
        print(f"  argv: {remote_shell(step.argv)}", file=out)
    current = ""
    for phase in phases:
        label = f"{phase.tier}/{phase.subrun}"
        if label != current:
            print(f"TIER {phase.tier} subrun {phase.subrun}", file=out)
            current = label
        for step in phase.steps:
            if step.kind == "wait":
                print(
                    f"  argv: {remote_shell(step.argv)}  "
                    f"(poll every {step.poll_interval_s:.0f}s, timeout {step.wait_timeout_s:.0f}s)",
                    file=out,
                )
            else:
                print(f"  argv: {remote_shell(step.argv)}", file=out)
        if phase.tier == "t5":
            print(
                f"  record: chunksize_expected={phase.meta.get('chunksize_expected')} "
                f"unit={phase.meta.get('unit')} service={phase.meta.get('service')} "
                f"status={phase.meta.get('remote_status_csv')} log={phase.meta.get('remote_camilla_log')} "
                f"soak_arg={phase.meta.get('soak_script_arg')} ({phase.meta.get('soak_script_arg_assumption')})",
                file=out,
            )
    print(
        "T5 NOTE: soak12.sh argument 1 is executed as written in the runbook. "
        "Local soak script evidence treats that argument as hours, not 600 seconds.",
        file=out,
    )


def record_dry_run(recorder: Recorder, phases: Sequence[Phase]) -> None:
    recorder.add_precondition("", False, "not-evaluated", evaluated=False)
    for step in identity_steps():
        _record_exec(recorder, step, now_iso(), None, "", "dry-run; not executed", False)
    for phase in phases:
        for step in phase.steps:
            started = now_iso()
            note = "dry-run; not executed"
            if step.kind == "wait":
                note = (
                    f"dry-run; would poll every {step.poll_interval_s:.0f}s "
                    f"until inactive/failed/unknown or {step.wait_timeout_s:.0f}s"
                )
            _record_exec(recorder, step, started, None, "", note, False)
        recorder.record_subrun(phase, "dry_run")


def execute(
    opts: Options,
    transport_factory: Callable[[Options], Transport] | None = None,
    sleep: Callable[[float], None] = time.sleep,
    monotonic: Callable[[], float] = time.monotonic,
) -> int:
    opts.out.mkdir(parents=True, exist_ok=True)
    recorder = Recorder(opts)
    recorder.begin()
    phases = build_phases(opts)
    if opts.dry_run:
        print_dry_run(opts, phases)
        for phase in phases:
            recorder.ensure_tier(phase.tier)
        record_dry_run(recorder, phases)
        recorder.finish(0)
        return 0
    # Identity is read-only and runs only after the first soak gate passes,
    # so a refused soak issues no other remote command.
    return execute_live_guarded(opts, recorder, phases, transport_factory, sleep, monotonic)


def execute_live_guarded(
    opts: Options,
    recorder: Recorder,
    phases: Sequence[Phase],
    transport_factory: Callable[[Options], Transport] | None,
    sleep: Callable[[float], None],
    monotonic: Callable[[], float],
) -> int:
    factory = transport_factory or build_transport
    for phase in phases:
        recorder.ensure_tier(phase.tier)
    transport: Transport | None = None
    identity_done = False
    try:
        try:
            transport = factory(opts)
            transport.connect()
        except BoardUnreachable as exc:
            recorder.error = str(exc)
            for phase in phases:
                recorder.mark_skipped(phase)
            recorder.finish(4)
            print(f"UNREACHABLE: {exc}", file=sys.stderr)
            return 4
        try:
            for phase in phases:
                if not identity_done:
                    # The phase starts with the soak gate. Identity is inserted
                    # only after that first gate returns safe, and before mkdir.
                    identity_done = True
                    run_phase_with_identity(transport, recorder, phase, sleep, monotonic)
                else:
                    run_phase(transport, recorder, phase, sleep, monotonic)
        except UnsafePrecondition as exc:
            recorder.error = str(exc)
            for phase in phases:
                state = recorder.tier_state.get(phase.tier, {})
                subruns = state.get("subruns", [])
                seen = isinstance(subruns, list) and any(
                    isinstance(item, dict) and item.get("id") == phase.subrun for item in subruns
                )
                if not seen:
                    recorder.mark_skipped(phase)
            recorder.finish(3)
            print(f"UNSAFE: {exc}", file=sys.stderr)
            return 3
        except BoardUnreachable as exc:
            recorder.error = str(exc)
            recorder.finish(4)
            print(f"UNREACHABLE: {exc}", file=sys.stderr)
            return 4
        except TierFailure as exc:
            recorder.error = str(exc)
            for phase in phases:
                state = recorder.tier_state.get(phase.tier, {})
                subruns = state.get("subruns", [])
                seen = isinstance(subruns, list) and any(
                    isinstance(item, dict) and item.get("id") == phase.subrun for item in subruns
                )
                if not seen:
                    recorder.mark_skipped(phase)
            recorder.finish(1)
            print(f"TIER FAIL: {exc}", file=sys.stderr)
            return 1
        recorder.finish(0)
        return 0
    finally:
        if transport is not None:
            try:
                transport.close()
            except Exception:
                pass


def run_phase_with_identity(
    transport: Transport,
    recorder: Recorder,
    phase: Phase,
    sleep: Callable[[float], None],
    monotonic: Callable[[], float],
) -> None:
    """Run the phase, but collect identity only after its leading soak gate passes."""
    if not phase.steps or phase.steps[0].kind != "gate":
        for step in identity_steps():
            execute_step(transport, recorder, step, sleep=sleep, monotonic=monotonic)
        run_phase(transport, recorder, phase, sleep, monotonic)
        return
    gate = phase.steps[0]
    execute_step(transport, recorder, gate, sleep=sleep, monotonic=monotonic)
    for step in identity_steps():
        execute_step(transport, recorder, step, sleep=sleep, monotonic=monotonic)
    failure: Exception | None = None
    for step in phase.steps[1:]:
        if failure is not None:
            _record_exec(recorder, step, now_iso(), None, "", "skipped after an earlier failure in this subrun", False)
            continue
        try:
            execute_step(transport, recorder, step, sleep=sleep, monotonic=monotonic)
        except (TierFailure, UnsafePrecondition, BoardUnreachable) as exc:
            failure = exc
            if isinstance(exc, (UnsafePrecondition, BoardUnreachable)):
                raise
    try:
        pull_artifacts(transport, recorder, phase)
    except TierFailure as exc:
        failure = failure or exc
    extra = _enrich_phase(recorder, phase)
    if failure is not None:
        recorder.record_subrun(phase, "failed", extra)
        if isinstance(failure, TierFailure):
            raise failure
        raise TierFailure(str(failure))
    recorder.record_subrun(phase, "ok", extra)


class ParamikoTransport:
    """Lazy paramiko SSH. Connection pattern matches tools/board/board.py."""

    def __init__(self, opts: Options) -> None:
        self.opts = opts
        self.client: object | None = None
        self._sftp: object | None = None

    def connect(self) -> None:
        import socket

        import paramiko

        try:
            key = paramiko.Ed25519Key.from_private_key_file(str(self.opts.key))
            client = paramiko.SSHClient()
            client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
            client.connect(
                self.opts.host,
                port=self.opts.port,
                username=self.opts.user,
                pkey=key,
                timeout=20.0,
                banner_timeout=20.0,
                auth_timeout=20.0,
            )
        except (paramiko.SSHException, socket.timeout, TimeoutError, OSError) as exc:
            raise BoardUnreachable(f"{type(exc).__name__}: {exc}") from exc
        self.client = client

    def close(self) -> None:
        if self._sftp is not None:
            try:
                self._sftp.close()  # type: ignore[attr-defined]
            except Exception:
                pass
            self._sftp = None
        if self.client is not None:
            try:
                self.client.close()  # type: ignore[attr-defined]
            except Exception:
                pass
            self.client = None

    def exec(self, argv: Sequence[str], timeout: float) -> tuple[int, str, str]:
        import socket

        if self.client is None:
            raise BoardUnreachable("exec before connect")
        command = remote_shell(argv)
        try:
            _stdin, stdout, stderr = self.client.exec_command(command, timeout=timeout)  # type: ignore[attr-defined]
            out = stdout.read().decode("utf-8", "replace")
            err = stderr.read().decode("utf-8", "replace")
            rc = int(stdout.channel.recv_exit_status())
            return rc, out, err
        except (socket.timeout, TimeoutError) as exc:
            raise CommandTimeout(str(exc)) from exc
        except OSError as exc:
            raise BoardUnreachable(f"{type(exc).__name__}: {exc}") from exc
        except Exception as exc:
            # paramiko.SSHException and channel errors. Import stays local to connect().
            if exc.__class__.__name__ in {"SSHException", "ChannelException"} or "paramiko" in type(exc).__module__:
                raise BoardUnreachable(f"{type(exc).__name__}: {exc}") from exc
            raise

    def _open_sftp(self) -> object:
        if self.client is None:
            raise BoardUnreachable("sftp before connect")
        if self._sftp is None:
            self._sftp = self.client.open_sftp()  # type: ignore[attr-defined]
        return self._sftp

    def exists(self, remote: str) -> bool:
        sftp = self._open_sftp()
        try:
            sftp.stat(remote)  # type: ignore[attr-defined]
            return True
        except FileNotFoundError:
            return False
        except OSError as exc:
            if getattr(exc, "errno", None) == 2 or "No such file" in str(exc):
                return False
            raise BoardUnreachable(f"{type(exc).__name__}: {exc}") from exc

    def get(self, remote: str, local: Path) -> None:
        local.parent.mkdir(parents=True, exist_ok=True)
        sftp = self._open_sftp()
        try:
            sftp.get(remote, str(local))  # type: ignore[attr-defined]
        except FileNotFoundError as exc:
            raise OSError(str(exc)) from exc
        except OSError as exc:
            if getattr(exc, "errno", None) == 2 or "No such file" in str(exc):
                raise
            raise BoardUnreachable(f"{type(exc).__name__}: {exc}") from exc


def build_transport(opts: Options) -> Transport:
    """The only factory that imports paramiko. Dry-run must not call it."""
    return ParamikoTransport(opts)


def _load_cuemix(path: Path | None) -> tuple[dict[str, object] | None, list[object] | None, str | None]:
    if path is None:
        return (
            None,
            None,
            "cuemix_routing and final_insert_direct_routes are null because --cuemix-state was not provided",
        )
    if not path.is_file():
        raise UsageError(f"--cuemix-state file not found: {path}")
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise UsageError(f"--cuemix-state is not JSON: {exc}") from exc
    if not isinstance(data, dict):
        raise UsageError("--cuemix-state must be a JSON object")
    if "cuemix_routing" not in data or "final_insert_direct_routes" not in data:
        raise UsageError("--cuemix-state must contain cuemix_routing and final_insert_direct_routes")
    routing = data["cuemix_routing"]
    routes = data["final_insert_direct_routes"]
    if routing is not None and not isinstance(routing, dict):
        raise UsageError("cuemix_routing must be a JSON object or null")
    if routes is not None and not isinstance(routes, list):
        raise UsageError("final_insert_direct_routes must be a JSON list or null")
    warning = None
    if routing is None or routes is None:
        warning = "--cuemix-state contains null cuemix_routing or final_insert_direct_routes"
    return routing, routes, warning


def normalize_argv(argv: Sequence[str]) -> list[str]:
    """Accept ``all --dry-run --out DIR`` and ``--dry-run all --out DIR``.

    Global options are defined on the top-level parser. When the tier name
    comes first, move the following options in front of it.
    """
    args = list(argv)
    commands = {"t1", "t2", "t4", "t5", "all"}
    if not args or args[0] not in commands:
        return args
    if any(token in {"-h", "--help"} for token in args):
        return args
    return [*args[1:], args[0]]


def parse_and_validate(argv: Sequence[str]) -> Options:
    parser = argparse.ArgumentParser(
        prog="run_acceptance.py",
        description=(
            "Board acceptance orchestrator for T1 (partial out9->in7), T2 (UMIK x3), "
            "T4 (5.3.2 merge), and T5 (chunksize 64 and 128). "
            "Live mode checks systemctl is-active tspi-soak12.service first and refuses "
            "unless the state is inactive, failed, or unknown. --dry-run does not connect."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "exit 0  procedure finished (not an ACCEPTANCE PASS; T1 stays partial)\n"
            "exit 1  tier command failed or a required artifact is missing\n"
            "exit 2  usage error\n"
            "exit 3  unsafe soak precondition\n"
            "exit 4  board unreachable (connect, auth, or timeout)\n"
            "\n"
            "Global options may be written before or after the tier name:\n"
            "  run_acceptance.py all --dry-run --out DIR\n"
            "  run_acceptance.py --dry-run --out DIR all\n"
            "\n"
            "There is no flag that overrides an active soak.\n"
            "Exit 0 means the procedure and required files finished. It does not mean T1 PASS."
        ),
    )
    parser.add_argument("--dry-run", action="store_true", help="print planned commands; do not connect")
    parser.add_argument("--out", type=Path, help="local directory for run.json, logs, and pulled artifacts")
    parser.add_argument("--host", default=DEFAULT_HOST)
    parser.add_argument("--user", default=DEFAULT_USER)
    parser.add_argument("--port", type=int, default=22)
    parser.add_argument("--key", type=Path, default=DEFAULT_KEY, help="ed25519 private key")
    parser.add_argument(
        "--calibrate-bin",
        default=DEFAULT_CALIBRATE_BIN,
        help="remote tspi-calibrate path (default /tmp/tspi-calibrate)",
    )
    parser.add_argument(
        "--pairs",
        default=None,
        help="T4 merge pairs, required for a live t4 or all (example FL:SUB1:80,FR:HFR:2000)",
    )
    parser.add_argument(
        "--cuemix-state",
        type=Path,
        default=None,
        help="optional JSON file with cuemix_routing and final_insert_direct_routes for the T1 record",
    )
    sub = parser.add_subparsers(dest="tier", required=True)
    sub.add_parser("t1", help="partial electrical loopback out9 -> in7")
    sub.add_parser("t2", help="UMIK-1 repeats run1 run2 run3")
    sub.add_parser("t4", help="5.3.2 measure + merge; live mode requires --pairs")
    sub.add_parser("t5", help="independent realtime runs at chunksize 64 and 128")
    sub.add_parser("all", help="t1, then t2, then t4, then t5; stop on first failure")
    args = parser.parse_args(normalize_argv(argv))
    tiers = ("t1", "t2", "t4", "t5") if args.tier == "all" else (args.tier,)
    if args.out is None:
        raise UsageError("--out DIR is required")
    bin_error = validate_calibrate_bin(args.calibrate_bin)
    if bin_error:
        raise UsageError(bin_error)
    needs_pairs = "t4" in tiers
    pairs = args.pairs
    placeholder = False
    if needs_pairs and not pairs:
        if args.dry_run:
            pairs = RUNBOOK_EXAMPLE_PAIRS
            placeholder = True
        else:
            raise UsageError("T4 requires --pairs; refusing before any connection")
    if pairs:
        pair_error = validate_pairs(pairs)
        if pair_error:
            raise UsageError(pair_error)
    if not args.key.is_file():
        raise UsageError(f"key file not found: {args.key}")
    if args.out.exists() and not args.out.is_dir():
        raise UsageError(f"--out exists and is not a directory: {args.out}")
    routing, routes, warning = _load_cuemix(args.cuemix_state)
    return Options(
        tiers=tiers,
        dry_run=bool(args.dry_run),
        out=args.out,
        host=str(args.host),
        user=str(args.user),
        port=int(args.port),
        key=args.key,
        calibrate_bin=str(args.calibrate_bin),
        pairs=pairs,
        pairs_placeholder=placeholder,
        cuemix_routing=routing,
        final_insert_direct_routes=routes,
        cuemix_warning=warning,
        invoked=tuple(argv),
    )


def main(argv: Sequence[str] | None = None) -> int:
    if argv is None:
        argv = sys.argv[1:]
    try:
        opts = parse_and_validate(argv)
    except SystemExit as exc:
        code = exc.code
        if code is None:
            return 0
        return int(code)
    except UsageError as exc:
        print(f"USAGE: {exc}", file=sys.stderr)
        return 2
    try:
        return execute(opts)
    except UsageError as exc:
        print(f"USAGE: {exc}", file=sys.stderr)
        return 2
    except UnsafePrecondition as exc:
        print(f"UNSAFE: {exc}", file=sys.stderr)
        return 3
    except BoardUnreachable as exc:
        print(f"UNREACHABLE: {exc}", file=sys.stderr)
        return 4
    except TierFailure as exc:
        print(f"TIER FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
