"""Offline tests for tools/board/run_acceptance.py. No board, no network."""
from __future__ import annotations

import json
import socket
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import run_acceptance as ra  # noqa: E402

BIN = "/tmp/khaen-calibrate"
PAIRS = ra.RUNBOOK_EXAMPLE_PAIRS
ISO = r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}[+-]\d{2}:\d{2}$"
T1_MEASURE = [
    BIN,
    "measure-multi",
    "--out-dir",
    "/var/lib/tspi-calib/t1",
    "--out-prefix",
    "/var/lib/tspi-calib/t1/t1",
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
    "128",
    "--buffer-size",
    "512",
    "--verify-mapping",
]


def _key(directory: Path) -> Path:
    path = directory / "id_ed25519"
    path.write_text("not-used-by-dry-run\n", encoding="utf-8")
    return path


class FakeTransport:
    def __init__(
        self,
        out: Path,
        soak: str = "inactive\n",
        *,
        missing: set[str] | None = None,
        files: dict[str, bytes] | None = None,
        t1_measure_rc: int = 0,
        show_stdout: str = "Result=success\nExecMainStatus=0\nActiveState=inactive\n",
        lock_chunk: int | None = None,
        fail_connect: str | None = None,
    ) -> None:
        self.out = out
        self.soak = soak
        self.missing = missing or set()
        self.files = files or {}
        self.t1_measure_rc = t1_measure_rc
        self.show_stdout = show_stdout
        self.lock_chunk = lock_chunk
        self.fail_connect = fail_connect
        self.calls: list[list[str]] = []
        self.closed = False
        self.saw_prior_gate = False
        self.chunk = 128

    def connect(self) -> None:
        if self.fail_connect:
            raise ra.BoardUnreachable(self.fail_connect)

    def close(self) -> None:
        self.closed = True

    def exec(self, argv: list[str], timeout: float) -> tuple[int, str, str]:
        argv = list(argv)
        self.calls.append(argv)
        if len(self.calls) == 2:
            payload = json.loads((self.out / "run.json").read_text(encoding="utf-8"))
            self.saw_prior_gate = any(
                cmd.get("argv", [])[:3] == ["systemctl", "is-active", "tspi-soak12.service"]
                for cmd in payload["commands"]
            )
        if argv[:3] == ["systemctl", "is-active", "tspi-soak12.service"]:
            token = argv and self.soak.strip()
            rc = 0 if token == "active" else 3
            text = self.soak if self.soak.endswith("\n") or self.soak == "" else self.soak + "\n"
            return rc, text, ""
        if argv[:2] == ["systemctl", "is-active"] and argv[-1].startswith("tspi-t5-"):
            return 3, "inactive\n", ""
        if argv[:2] == ["systemctl", "show"]:
            return 0, self.show_stdout, ""
        if argv and argv[0] == "sed":
            expr = argv[2]
            if self.lock_chunk is None:
                if expr == "s/chunksize: 128/chunksize: 64/":
                    self.chunk = 64
                elif expr == "s/chunksize: 64/chunksize: 128/":
                    self.chunk = 128
            return 0, "", ""
        if argv and argv[0] == "grep" and "chunksize:" in argv:
            shown = self.lock_chunk if self.lock_chunk is not None else self.chunk
            return 0, f"4:  chunksize: {shown}\n", ""
        if len(argv) > 1 and argv[1] == "measure-multi" and "/var/lib/tspi-calib/t1" in argv:
            return self.t1_measure_rc, "measure-multi done\n", ""
        return 0, "", ""

    def exists(self, remote: str) -> bool:
        return remote not in self.missing

    def get(self, remote: str, local: Path) -> None:
        local.parent.mkdir(parents=True, exist_ok=True)
        local.write_bytes(self.files.get(remote, b"artifact\n"))


class AcceptanceTests(unittest.TestCase):
    def run_main(self, args: list[str], fake: FakeTransport | None = None) -> tuple[int, str, str]:
        stdout, stderr = StringIO(), StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            if fake is None:
                code = ra.main(args)
            else:
                with mock.patch.object(ra, "build_transport", lambda opts: fake):
                    code = ra.main(args)
        return code, stdout.getvalue(), stderr.getvalue()

    def test_t1_measure_argv_exact(self) -> None:
        self.assertEqual(ra.t1_measure_argv(BIN), T1_MEASURE)
        self.assertNotIn("--mic", T1_MEASURE)
        self.assertNotIn("hw:3,0", T1_MEASURE)

    def test_t2_argv_three_distinct_dirs(self) -> None:
        commands = [ra.t2_measure_argv(BIN, index) for index in (1, 2, 3)]
        for index, argv in enumerate(commands, start=1):
            self.assertEqual(argv[1], "measure-multi")
            self.assertEqual(argv[argv.index("--channels") + 1], "0,1")
            self.assertEqual(argv[argv.index("--names") + 1], "FL,FR")
            self.assertEqual(argv[argv.index("--capture") + 1], "hw:3,0")
            self.assertEqual(argv[argv.index("--capture-channels") + 1], "1")
            self.assertEqual(argv[argv.index("--mic") + 1], "umik")
            self.assertEqual(argv[argv.index("--playback") + 1], "hw:4,0")
            remote = f"/var/lib/tspi-calib/t2/run{index}"
            self.assertEqual(argv[argv.index("--out-dir") + 1], remote)
            self.assertEqual(argv[argv.index("--out-prefix") + 1], remote)
            self.assertNotIn("--loopback-map", argv)
        self.assertEqual(len({tuple(item) for item in commands}), 3)

    def test_t4_measure_and_merge_argv(self) -> None:
        measure = ra.t4_measure_argv(BIN)
        self.assertEqual(measure[measure.index("--channels") + 1], "0,1,2,3,4,5,6,7,8,9")
        self.assertEqual(
            measure[measure.index("--names") + 1],
            "FL,FR,C,SL,SR,SUB1,SUB2,SUB3,HFL,HFR",
        )
        self.assertEqual(measure[measure.index("--capture") + 1], "hw:3,0")
        self.assertEqual(measure[measure.index("--capture-channels") + 1], "1")
        self.assertEqual(measure[measure.index("--mic") + 1], "umik")
        self.assertEqual(measure[measure.index("--polarity-offset") + 1], "normal")
        self.assertEqual(measure[measure.index("--playback-channels") + 1], "22")
        merge = ra.t4_merge_argv(BIN, PAIRS)
        self.assertEqual(merge[merge.index("--layout") + 1], "5.3.2")
        self.assertEqual(merge[merge.index("--crossovers") + 1], "80,2000")
        self.assertEqual(merge[merge.index("--pairs") + 1], PAIRS)
        self.assertEqual(merge[merge.index("--chunksize") + 1], "128")
        self.assertEqual(merge[merge.index("--names") + 1], "FL,FR,C,SL,SR,SUB1,SUB2,SUB3,HFL,HFR")
        self.assertNotIn("--negative-controls", merge)
        self.assertNotIn("--loopback-map", measure)

    def test_t4_chunksize_comes_from_argv_or_report_not_a_hidden_constant(self) -> None:
        argv = ra.t4_merge_argv(BIN, PAIRS, chunksize=64)
        self.assertEqual(ra.chunksize_from_argv(argv), 64)
        reported, source, mismatch = ra.resolve_chunksize(
            '{"environment": {"chunksize": 64}}',
            ra.t4_merge_argv(BIN, PAIRS, chunksize=128),
        )
        self.assertEqual((reported, source, mismatch), (64, "report.environment.chunksize", True))
        fallback, source, mismatch = ra.resolve_chunksize(None, argv)
        self.assertEqual((fallback, source, mismatch), (64, "argv", False))
        missing, source, _mismatch = ra.resolve_chunksize("{}", ["merge"])
        self.assertIsNone(missing)
        self.assertEqual(source, "missing")

    def test_t5_argv_64_and_128_are_separate(self) -> None:
        self.assertEqual(
            ra.t5_sed_argv(64),
            ["sed", "-i", "s/chunksize: 128/chunksize: 64/", "/var/lib/tspi-soak/camilla.yml"],
        )
        self.assertEqual(
            ra.t5_sed_argv(128),
            ["sed", "-i", "s/chunksize: 64/chunksize: 128/", "/var/lib/tspi-soak/camilla.yml"],
        )
        self.assertEqual(
            ra.t5_systemd_argv(64),
            [
                "systemd-run",
                "--unit=tspi-t5-64",
                "--property=Type=simple",
                "/bin/bash",
                "/var/lib/tspi-soak/soak12.sh",
                "1",
            ],
        )
        self.assertEqual(ra.t5_systemd_argv(128)[1], "--unit=tspi-t5-128")
        self.assertNotEqual(ra.t5_systemd_argv(64), ra.t5_systemd_argv(128))

    def test_t1_partial_block_uses_ten_layout_outputs(self) -> None:
        block = ra.t1_report_block(None, None, "missing")
        self.assertEqual(block["status"], "partial")
        self.assertEqual(block["loopback_map_1based"], [{"play": 9, "capture": 7}])
        self.assertEqual(block["covered_outputs_1based"], [9])
        self.assertEqual(block["covered_inputs_1based"], [7])
        self.assertEqual(block["uncovered_outputs_1based"], [1, 2, 3, 4, 5, 6, 7, 8, 10])
        self.assertEqual(len(block["uncovered_outputs_1based"]), 9)
        self.assertNotIn(11, block["uncovered_outputs_1based"])
        self.assertNotIn(22, block["uncovered_outputs_1based"])
        self.assertIsNone(block["cuemix_routing"])
        self.assertIsNone(block["final_insert_direct_routes"])
        self.assertEqual(block["mitigation"], ["file_backend", "t4_acoustic"])
        self.assertEqual(block["claim"], "T1: partial (out9->in7 only); 1:1 mapping and other channels not verified")
        self.assertNotIn("T1 PASS", block["claim"])

    def test_planned_phases_match_builders(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            opts = ra.parse_and_validate(
                ["all", "--dry-run", "--out", str(root / "out"), "--key", str(_key(root)), "--pairs", PAIRS]
            )
        phases = ra.build_phases(opts)
        self.assertEqual([phase.tier for phase in phases], ["t1", "t2", "t2", "t2", "t4", "t4", "t5", "t5"])
        t1 = phases[0]
        measure = next(step.argv for step in t1.steps if step.role == "measure")
        self.assertEqual(list(measure), T1_MEASURE)
        required = [item.local_rel for item in t1.artifacts if item.required]
        self.assertEqual(
            required,
            [
                "t1/OUT9_IN7-ch08.fr.csv",
                "t1/OUT9_IN7-ch08.analysis.json",
                "t1/OUT9_IN7-ch08.diag.json",
            ],
        )
        t2_dirs = [phase.meta["remote_dir"] for phase in phases if phase.tier == "t2"]
        self.assertEqual(
            t2_dirs,
            [
                "/var/lib/tspi-calib/t2/run1",
                "/var/lib/tspi-calib/t2/run2",
                "/var/lib/tspi-calib/t2/run3",
            ],
        )
        self.assertIn("t2/run1/FL-ch00.fr.csv", [item.local_rel for item in phases[1].artifacts])
        self.assertIn("t2/run2/FR-ch01.diag.json", [item.local_rel for item in phases[2].artifacts])
        merge = next(phase for phase in phases if phase.tier == "t4" and phase.subrun == "merge")
        merge_argv = next(step.argv for step in merge.steps if step.role == "merge")
        self.assertNotIn("--negative-controls", merge_argv)
        t5 = [phase for phase in phases if phase.tier == "t5"]
        self.assertEqual([phase.subrun for phase in t5], ["chunksize-64", "chunksize-128"])
        self.assertEqual(t5[0].meta["unit"], "tspi-t5-64")
        self.assertEqual(t5[1].meta["unit"], "tspi-t5-128")
        self.assertNotEqual(t5[0].meta["remote_dir"], t5[1].meta["remote_dir"])
        for phase in phases:
            for step in phase.steps:
                self.assertNotIn("--negative-controls", step.argv)

    def test_all_selects_four_tiers_and_t2_selects_only_t2(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            key = _key(root)
            all_opts = ra.parse_and_validate(
                ["all", "--dry-run", "--out", str(root / "a"), "--key", str(key), "--pairs", PAIRS]
            )
            t2_opts = ra.parse_and_validate(
                ["t2", "--dry-run", "--out", str(root / "b"), "--key", str(key)]
            )
        self.assertEqual(all_opts.tiers, ("t1", "t2", "t4", "t5"))
        self.assertEqual(t2_opts.tiers, ("t2",))
        self.assertEqual([phase.subrun for phase in ra.build_phases(t2_opts)], ["run1", "run2", "run3"])

    def test_soak_decision_matrix(self) -> None:
        self.assertEqual(ra.decide_soak("inactive\n"), (True, "inactive"))
        self.assertEqual(ra.decide_soak("failed"), (True, "failed"))
        self.assertEqual(ra.decide_soak("unknown\n"), (True, "unknown"))
        self.assertEqual(ra.decide_soak("active\n"), (False, "active"))
        self.assertEqual(ra.decide_soak("activating\n"), (False, "activating"))
        self.assertEqual(ra.decide_soak("deactivating\n"), (False, "deactivating"))
        self.assertEqual(ra.decide_soak(""), (False, "empty-or-multiline"))
        self.assertEqual(ra.decide_soak("inactive\nactive\n"), (False, "empty-or-multiline"))
        self.assertEqual(ra.decide_soak("  \n"), (False, "empty-or-multiline"))

    def test_soak_active_refuses_before_any_other_command(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            fake = FakeTransport(out, soak="active\n")
            code, _stdout, stderr = self.run_main(
                ["t1", "--out", str(out), "--key", str(_key(root))],
                fake,
            )
            payload = json.loads((out / "run.json").read_text(encoding="utf-8"))
        self.assertEqual(code, 3)
        self.assertEqual(fake.calls, [["systemctl", "is-active", "tspi-soak12.service"]])
        self.assertIn("UNSAFE", stderr)
        self.assertTrue(fake.closed)
        self.assertEqual(payload["exit_code"], 3)
        self.assertTrue(payload["precondition"]["evaluated"])
        self.assertFalse(payload["precondition"]["checks"][-1]["safe"])
        self.assertNotIn("measure-multi", json.dumps(payload["commands"]))

    def test_soak_inactive_runs_t1_and_stays_partial(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            diag = json.dumps({"isolation_other_max_db": -62.5}).encode("utf-8")
            fake = FakeTransport(
                out,
                files={"/var/lib/tspi-calib/t1/OUT9_IN7-ch08.diag.json": diag},
            )
            code, _stdout, stderr = self.run_main(
                ["t1", "--out", str(out), "--key", str(_key(root))],
                fake,
            )
            self.assertEqual(code, 0, stderr)
            self.assertTrue(fake.saw_prior_gate)
            self.assertTrue(fake.closed)
            self.assertTrue(any(call[1:2] == ["measure-multi"] for call in fake.calls))
            payload = json.loads((out / "run.json").read_text(encoding="utf-8"))
            self.assertFalse((out / "run.json.tmp").exists())
        self.assertEqual(payload["exit_code"], 0)
        self.assertEqual(payload["precondition"]["checks"][0]["detail"], "inactive")
        block = payload["tiers"]["t1"]["subruns"][0]["meta"]["t1"]
        self.assertEqual(block["status"], "partial")
        self.assertEqual(block["loopback_mapping"], "pass")
        self.assertEqual(block["isolation_max_other_db"], -62.5)
        self.assertIsNone(block["cuemix_routing"])
        self.assertNotIn("T1 PASS", json.dumps(block))
        artifact = next(item for item in payload["artifacts"] if item["local"].endswith("diag.json"))
        self.assertEqual(artifact["sha256"], ra.sha256_bytes(diag))

    def test_unreachable_connect_makes_no_exec(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            fake = FakeTransport(out, fail_connect="timed out")
            code, _stdout, stderr = self.run_main(
                ["all", "--out", str(out), "--key", str(_key(root)), "--pairs", PAIRS],
                fake,
            )
            payload = json.loads((out / "run.json").read_text(encoding="utf-8"))
        self.assertEqual(code, 4)
        self.assertEqual(fake.calls, [])
        self.assertIn("UNREACHABLE", stderr)
        self.assertTrue(fake.closed)
        self.assertEqual(payload["exit_code"], 4)
        self.assertEqual(payload["commands"], [])
        self.assertIn("timed out", payload["error"])

    def test_dry_run_makes_zero_transport_calls(self) -> None:
        sys.modules.pop("paramiko", None)

        def explode(*_args: object, **_kwargs: object) -> None:
            raise AssertionError("transport or socket called")

        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            stdout, stderr = StringIO(), StringIO()
            with (
                mock.patch.object(ra, "build_transport", explode),
                mock.patch.object(socket, "create_connection", explode),
                redirect_stdout(stdout),
                redirect_stderr(stderr),
            ):
                code = ra.main(["all", "--dry-run", "--out", str(out), "--key", str(_key(root))])
            dry_payload = json.loads((out / "run.json").read_text(encoding="utf-8"))
        self.assertEqual(code, 0, stderr.getvalue())
        self.assertNotIn("paramiko", sys.modules)
        text = stdout.getvalue()
        self.assertIn("DRY-RUN transport=none", text)
        self.assertIn(ra.DRY_RUN_GATE_NOTE, text)
        for tier in ("t1", "t2", "t4", "t5"):
            self.assertIn(f"TIER {tier}", text)
        self.assertIn("--loopback-map 9:7", text)
        self.assertIn("--unit=tspi-t5-64", text)
        self.assertIn("--unit=tspi-t5-128", text)
        self.assertIn("soak12.sh 1", text)
        self.assertNotIn("--negative-controls", text)
        self.assertTrue(dry_payload["dry_run"])
        self.assertFalse(dry_payload["precondition"]["evaluated"])
        self.assertEqual(dry_payload["precondition"]["checks"][0]["detail"], "not-evaluated")
        self.assertTrue(dry_payload["commands"])
        self.assertTrue(all(item["executed"] is False for item in dry_payload["commands"]))
        self.assertTrue(all(item["exit_code"] is None for item in dry_payload["commands"]))
        self.assertEqual(
            [item["id"] for item in dry_payload["tiers"]["t5"]["subruns"]],
            ["chunksize-64", "chunksize-128"],
        )

    def test_t4_missing_or_bad_pairs_is_usage_error(self) -> None:
        def explode(*_args: object, **_kwargs: object) -> None:
            raise AssertionError("transport called")

        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            with mock.patch.object(ra, "build_transport", explode):
                missing_code, _stdout, missing_err = self.run_main(["t4", "--out", str(out), "--key", str(_key(root))])
                bad_code, _stdout, bad_err = self.run_main(
                    ["all", "--out", str(out), "--key", str(_key(root)), "--pairs", "FL:SUB1"]
                )
                bin_code, _stdout, bin_err = self.run_main(
                    ["t1", "--out", str(out), "--key", str(_key(root)), "--calibrate-bin", "/tmp/khaen-calibrate;id"]
                )
            out_was_created = out.exists()
        self.assertEqual(missing_code, 2)
        self.assertIn("USAGE", missing_err)
        self.assertIn("--pairs", missing_err)
        self.assertEqual(bad_code, 2)
        self.assertIn("USAGE", bad_err)
        self.assertEqual(bin_code, 2)
        self.assertFalse(out_was_created)

    def test_t5_runs_are_recorded_separately(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            files = {
                "/var/lib/tspi-calib/t5/chunksize-64/camilla.log": b"period_size: 64\nbuffer_size: 256\n",
                "/var/lib/tspi-calib/t5/chunksize-128/camilla.log": b"period_size: 128\nbuffer_size: 512\n",
                "/var/lib/tspi-calib/t5/chunksize-64/status.csv": b"ts,xrun_total\n0,0\n",
                "/var/lib/tspi-calib/t5/chunksize-128/status.csv": b"ts,xrun_total\n0,1\n",
            }
            fake = FakeTransport(out, files=files)
            code, _stdout, stderr = self.run_main(["t5", "--out", str(out), "--key", str(_key(root))], fake)
            self.assertEqual(code, 0, stderr)
            units = [call for call in fake.calls if call and call[0] == "systemd-run"]
            self.assertEqual(
                units,
                [ra.t5_systemd_argv(64), ra.t5_systemd_argv(128)],
            )
            seds = [call[2] for call in fake.calls if call and call[0] == "sed"]
            self.assertEqual(
                seds,
                ["s/chunksize: 128/chunksize: 64/", "s/chunksize: 64/chunksize: 128/"],
            )
            payload = json.loads((out / "run.json").read_text(encoding="utf-8"))
        runs = payload["tiers"]["t5"]["subruns"]
        self.assertEqual([item["id"] for item in runs], ["chunksize-64", "chunksize-128"])
        self.assertEqual(runs[0]["meta"]["chunksize"], 64)
        self.assertEqual(runs[1]["meta"]["chunksize"], 128)
        self.assertEqual(runs[0]["meta"]["chunksize_source"], "camilla.yml readback")
        self.assertEqual(runs[0]["meta"]["unit"], "tspi-t5-64")
        self.assertEqual(runs[1]["meta"]["service"], "tspi-t5-128.service")
        self.assertEqual(runs[0]["meta"]["period_size"], 64)
        self.assertEqual(runs[0]["meta"]["buffer_size"], 256)
        self.assertEqual(runs[1]["meta"]["period_size"], 128)
        self.assertEqual(runs[1]["meta"]["buffer_size"], 512)
        self.assertNotEqual(runs[0]["meta"]["remote_status_csv"], runs[1]["meta"]["remote_status_csv"])
        self.assertEqual(runs[0]["meta"]["xrun"]["xrun_count"], 0)
        self.assertEqual(runs[1]["meta"]["xrun"]["xrun_count"], 1)
        self.assertEqual(runs[0]["status"], "ok")
        self.assertEqual(runs[1]["status"], "ok")

    def test_t5_bad_readback_does_not_start_systemd(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            fake = FakeTransport(out, lock_chunk=999)
            code, _stdout, stderr = self.run_main(["t5", "--out", str(out), "--key", str(_key(root))], fake)
        self.assertEqual(code, 1)
        self.assertIn("TIER FAIL", stderr)
        self.assertFalse(any(call and call[0] == "systemd-run" for call in fake.calls))
        self.assertTrue(any(call and call[0] == "sed" for call in fake.calls))

    def test_t5_failed_unit_does_not_start_the_other_chunksize(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            fake = FakeTransport(out, show_stdout="Result=failed\nExecMainStatus=1\nActiveState=inactive\n")
            code, _stdout, _stderr = self.run_main(["t5", "--out", str(out), "--key", str(_key(root))], fake)
        self.assertEqual(code, 1)
        units = [call[1] for call in fake.calls if call and call[0] == "systemd-run"]
        self.assertEqual(units, ["--unit=tspi-t5-64"])

    def test_run_json_schema_and_atomic_update(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            fake = FakeTransport(out)
            code, _stdout, stderr = self.run_main(["t1", "--out", str(out), "--key", str(_key(root))], fake)
            self.assertEqual(code, 0, stderr)
            self.assertTrue(fake.saw_prior_gate)
            text = (out / "run.json").read_text(encoding="utf-8")
            payload = json.loads(text)
        for key in (
            "schema",
            "tool",
            "invoked_arguments",
            "board",
            "precondition",
            "commands",
            "artifacts",
            "tiers",
            "started_at",
            "updated_at",
            "exit_code",
            "assumptions",
        ):
            self.assertIn(key, payload)
        self.assertEqual(payload["schema"], ra.SCHEMA)
        self.assertEqual(payload["tool"]["name"], ra.TOOL_NAME)
        self.assertEqual(payload["tool"]["version"], ra.TOOL_VERSION)
        self.assertEqual(set(payload["board"]["identity"]), {"hostname", "uname", "date", "asound_cards"})
        self.assertRegex(payload["started_at"], ISO)
        self.assertRegex(payload["updated_at"], ISO)
        self.assertGreaterEqual(len(payload["commands"]), 1)
        for command in payload["commands"]:
            for key in ("argv", "tier", "started_at", "finished_at", "exit_code", "stdout", "stderr"):
                self.assertIn(key, command)
            if command["executed"]:
                self.assertRegex(command["started_at"], ISO)
                self.assertRegex(command["finished_at"], ISO)
        self.assertTrue(payload["artifacts"])
        self.assertTrue(all(item["sha256"] for item in payload["artifacts"] if item["present"]))
        self.assertNotIn("\r\n", text)

    def test_missing_required_artifact_fails_and_optional_does_not(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            required_out = root / "req"
            optional_out = root / "opt"
            required = FakeTransport(required_out, missing={"/var/lib/tspi-calib/t1/OUT9_IN7-ch08.fr.csv"})
            optional = FakeTransport(optional_out, missing={"/var/lib/tspi-calib/t1/cuemix-routing.png"})
            required_code, _stdout, required_err = self.run_main(
                ["t1", "--out", str(required_out), "--key", str(_key(root))],
                required,
            )
            optional_code, _stdout, optional_err = self.run_main(
                ["t1", "--out", str(optional_out), "--key", str(_key(root))],
                optional,
            )
            optional_payload = json.loads((optional_out / "run.json").read_text(encoding="utf-8"))
        self.assertEqual(required_code, 1, required_err)
        self.assertIn("TIER FAIL", required_err)
        self.assertEqual(optional_code, 0, optional_err)
        self.assertTrue(
            any("cuemix-routing.png" in item and "optional" in item for item in optional_payload["warnings"])
        )

    def test_t4_report_chunksize_overrides_argv(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            report = json.dumps({"environment": {"chunksize": 64}, "schema": "khaen-calibration-report/v1"}).encode()
            fake = FakeTransport(out, files={"/var/lib/tspi-calib/t4/calibration.yml.report.json": report})
            code, _stdout, stderr = self.run_main(
                ["t4", "--out", str(out), "--key", str(_key(root)), "--pairs", PAIRS],
                fake,
            )
            self.assertEqual(code, 0, stderr)
            payload = json.loads((out / "run.json").read_text(encoding="utf-8"))
        merge = next(item for item in payload["tiers"]["t4"]["subruns"] if item["id"] == "merge")
        self.assertEqual(merge["meta"]["chunksize"], 64)
        self.assertEqual(merge["meta"]["chunksize_source"], "report.environment.chunksize")
        self.assertTrue(merge["meta"]["chunksize_mismatch"])
        merge_cmd = next(item for item in payload["commands"] if item["role"] == "merge")
        self.assertEqual(merge_cmd["argv"][merge_cmd["argv"].index("--chunksize") + 1], "128")
        self.assertNotIn("--negative-controls", merge_cmd["argv"])

    def test_all_stops_after_first_tier_failure(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            fake = FakeTransport(out, t1_measure_rc=1)
            code, _stdout, stderr = self.run_main(
                ["all", "--out", str(out), "--key", str(_key(root)), "--pairs", PAIRS],
                fake,
            )
            stopped = json.loads((out / "run.json").read_text(encoding="utf-8"))
        self.assertEqual(code, 1)
        self.assertIn("TIER FAIL", stderr)
        self.assertFalse(any("/var/lib/tspi-calib/t2/" in " ".join(call) for call in fake.calls))
        self.assertEqual(stopped["tiers"]["t1"]["status"], "failed")
        self.assertEqual(stopped["tiers"]["t2"]["status"], "skipped")

    def test_cuemix_file_is_copied_and_missing_file_is_usage(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            state = root / "cuemix.json"
            state.write_text(
                json.dumps(
                    {
                        "cuemix_routing": {
                            "in7_to_out7": "on",
                            "in8_to_out8": "on",
                            "in7_to_out9": "none",
                            "other_input_to_output": "none",
                            "out7_out8_downstream": "disconnected",
                        },
                        "final_insert_direct_routes": ["in7->out7", "in8->out8"],
                    }
                ),
                encoding="utf-8",
            )
            out = root / "out"
            fake = FakeTransport(out)
            code, _stdout, stderr = self.run_main(
                ["t1", "--out", str(out), "--key", str(_key(root)), "--cuemix-state", str(state)],
                fake,
            )
            self.assertEqual(code, 0, stderr)
            payload = json.loads((out / "run.json").read_text(encoding="utf-8"))
            bad_code, _stdout, bad_err = self.run_main(
                ["t1", "--dry-run", "--out", str(root / "bad"), "--key", str(_key(root)), "--cuemix-state", str(root / "nope.json")]
            )
        block = payload["tiers"]["t1"]["subruns"][0]["meta"]["t1"]
        self.assertEqual(block["cuemix_routing"]["in7_to_out9"], "none")
        self.assertEqual(block["final_insert_direct_routes"], ["in7->out7", "in8->out8"])
        self.assertIsNone(block["cuemix_warning"])
        self.assertEqual(bad_code, 2)
        self.assertIn("USAGE", bad_err)

    def test_sha256_helper_and_remote_quoting(self) -> None:
        self.assertEqual(
            ra.sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )
        with TemporaryDirectory() as tmp:
            path = Path(tmp) / "blob.bin"
            path.write_bytes(b"abc")
            self.assertEqual(ra.sha256_file(path), ra.sha256_bytes(b"abc"))
        self.assertEqual(ra.remote_shell(["echo", "a b", "x"]), "echo 'a b' x")
        summary = ra.summarize_status_csv("ts,xrun_delta\n0,2\n0,3\n")
        self.assertEqual(summary["xrun_count"], 5)
        self.assertEqual(summary["reason"], "header")
        unparsed = ra.summarize_status_csv("a,b,c,d\n0,0,0,1\n")
        self.assertIsNone(unparsed["xrun_count"])
        self.assertEqual(unparsed["column4_positive_rows"], 1)
        self.assertEqual(ra.parse_period_buffer("period_size: 64\nbuffer_size: 256\n"), (64, 256))

    def test_help_and_import_do_not_need_paramiko(self) -> None:
        sys.modules.pop("paramiko", None)
        stdout, stderr = StringIO(), StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            code = ra.main(["--help"])
        self.assertEqual(code, 0)
        self.assertIn("--dry-run", stdout.getvalue())
        self.assertIn("tspi-soak12", stdout.getvalue())
        self.assertNotIn("paramiko", sys.modules)
        source = Path(ra.__file__).read_text(encoding="utf-8")
        self.assertNotIn("shell=True", source)
        self.assertNotIn("subprocess", source)

    def test_source_has_no_partial_run_acceptance_elsewhere(self) -> None:
        self.assertEqual(ra.DEFAULT_HOST, "192.0.2.215")
        self.assertEqual(ra.DEFAULT_CALIBRATE_BIN, "/tmp/khaen-calibrate")
        self.assertEqual(ra.validate_calibrate_bin("/tmp/khaen-calibrate"), None)
        self.assertIsNotNone(ra.validate_calibrate_bin("khaen-calibrate"))
        self.assertIsNotNone(ra.validate_calibrate_bin("/tmp/../tmp/khaen-calibrate"))


if __name__ == "__main__":
    unittest.main()
