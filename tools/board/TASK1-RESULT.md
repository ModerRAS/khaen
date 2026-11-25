STATUS: DONE

## Files created
- `tools/board/run_acceptance.py` — 1808 lines
- `tools/board/tests/test_run_acceptance.py` — 684 lines
- `tools/board/README.md` — 104 lines
- `tools/board/TASK1-RESULT.md` — 61 lines
No stale `run_acceptance.py` existed. `tools/board/` already had the older SSH/soak scripts; they were left as-is.

## Tests
`python -m unittest discover -s tools/board/tests -v`
`Ran 25 tests in 0.513s` — OK
`python tools/board/run_acceptance.py --help` — exit 0

## Dry-run
`python tools/board/run_acceptance.py all --dry-run --out C:\WorkSpace\Claw\scratch\lead\cursor-tasks\dryrun` — exit 0
Gate handling: dry-run does not open SSH, sockets, or paramiko, so it cannot read live `tspi-soak12.service`. It prints `systemctl is-active tspi-soak12.service` as not executed. A live run evaluates that command first and exits 3 unless the token is exactly `inactive`, `failed`, or `unknown`.
This invocation omitted `--pairs` and `--cuemix-state`. The merge line is a labeled preview of the runbook example pairs; a live `t4`/`all` without `--pairs` exits 2 and does not connect. CueMix fields are null.
Printed tiers and commands:
- identity (after a passing gate): `hostname`, `uname -a`, `date +%Y-%m-%dT%H:%M:%S%z`, `cat /proc/asound/cards`
- t1: mkdir `/var/lib/tspi-calib/t1`; `arecord -D hw:4,0 --dump-hw-params`; `measure-multi --loopback-map 9:7 --names OUT9_IN7 --playback hw:4,0 --playback-channels 22 --capture hw:4,0 --capture-channels 8 --rate 48000 --seconds 5 --level-db -30 --pre-seconds 1.5 --post-seconds 1.5 --period-size 128 --buffer-size 512 --verify-mapping`
- t2 run1, run2, run3: separate `--out-dir /var/lib/tspi-calib/t2/runN`; `--channels 0,1 --names FL,FR --capture hw:3,0 --capture-channels 1 --mic umik` (run1 also dumps `hw:3,0`)
- t4 measure: `--channels 0,1,2,3,4,5,6,7,8,9 --names FL,FR,C,SL,SR,SUB1,SUB2,SUB3,HFL,HFR --capture hw:3,0 --capture-channels 1 --mic umik --polarity-offset normal`
- t4 merge: `--layout 5.3.2 --crossovers 80,2000 --pairs FL:SUB1:80,FR:SUB1:80,C:SUB1:80,SL:SUB1:80,SR:SUB1:80,FL:HFL:2000,FR:HFR:2000 --chunksize 128` (no `--negative-controls`)
- t5 chunksize-64: `sed` `chunksize: 128`→`64`, `systemd-run --unit=tspi-t5-64 --property=Type=simple /bin/bash /var/lib/tspi-soak/soak12.sh 1`, copy status/log to `/var/lib/tspi-calib/t5/chunksize-64`
- t5 chunksize-128: inverse `sed`, `--unit=tspi-t5-128`, copy to `t5/chunksize-128` (not merged with the 64 run)
Each subrun also reprints the soak gate before its own commands.

## git status --porcelain=v1
Repo-wide porcelain is already dirty (tracked edits under `calibrate/`, `audio/`, `web/`, `firmware/`, plus untracked `calibration-spec/`, `docs/`, and the rest of `tools/`). This session did not modify those paths. `git ls-files tools/board` is empty, so a short status collapses the directory to `?? tools/`.
Paths created here:
```
?? tools/board/README.md
?? tools/board/TASK1-RESULT.md
?? tools/board/run_acceptance.py
?? tools/board/tests/test_run_acceptance.py
```

## Assumptions
- `uncovered_outputs_1based` is `[1,2,3,4,5,6,7,8,10]` (5.3.2 outputs 1..10 except 9; ACCEPTANCE §3.1). MOTU outputs 11..22 are outside that list and are not claimed. Runbook line 29's 1..22 list was not copied.
- `claim` is runbook line 62: `T1: partial (out9->in7 only); 1:1 mapping and other channels not verified`.
- CueMix fields stay null unless `--cuemix-state` contains both keys. `sudo` is omitted because the SSH user is root.
- The 128-frame run uses the inverse sed and unit `tspi-t5-128`. The runbook only prints the 64 command. `soak12.sh 1` is passed literally; `restart_soak5m.sh` shows `$1` is hours, while the runbook comment says 600s.
- Per-channel `fit` is skipped. `--negative-controls` is omitted. `arecord --dump-hw-params` follows runbook lines 48 and 79.
- Recorded T4 chunksize is `report.json` `environment.chunksize` when that file is pulled, otherwise the `--chunksize` argv actually sent.

## Risks
- Live T5 can occupy the board for about an hour twice if argument `1` means hours. Confirm before starting.
- `sed` matches the literal strings `chunksize: 128` and `chunksize: 64`. Other spacing fails readback and does not start `systemd-run`.
- Logs are copied after each run. If `soak12.sh` appends to the 12h `status.csv` instead of rotating it, the xrun total can include earlier rows.
- A parsed xrun count above zero is stored and does not by itself change the exit code. Exit 0 is not T1 PASS and not T5 PASS.
- `period_size` / `buffer_size` stay null when the pulled logs do not contain those keys. Host-key policy matches `board.py` (`AutoAddPolicy`). Final insert is out of scope.

No board connection was attempted or made.

## Needs a human before board execution
1. Confirm T5 duration (about 1 hour times 2, versus 10 minutes) and the exact `camilla.yml` chunksize spacing.
2. Pass the real `--pairs`. The dry-run example is not a live default.
3. Pass `--cuemix-state` from the post-soak CueMix text/screenshot, and keep out7/out8 off any downstream load.
4. If T1 polarity is invert, decide whether to rerun T2/T4 with `--polarity-offset invert`. This tool keeps T4 at the runbook value `normal`.
5. Read `xrun` in `run.json` before calling T5 passed. Close all CueMix input-to-output routes in a separate final-insert step.
