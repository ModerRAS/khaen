#!/usr/bin/env python3
"""Board SSH helper for khaen-audio-dsp (paramiko, no ssh.exe quoting).

Usage:
  python board.py status
  python board.py run --cmd "systemctl is-active khaen-soak12.service"
  python board.py put --local file --remote /tmp/file
  python board.py get --remote /var/lib/khaen-soak/status.csv --local status.csv
  python board.py reboot
Environment/defaults: host 192.0.2.215, user root, key deploy/ssh/khaen_audio_ed25519.
"""
from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

import paramiko

ROOT = Path(__file__).resolve().parents[2]  # projects/khaen-audio-dsp
DEFAULT_KEY = ROOT / "deploy" / "ssh" / "khaen_audio_ed25519"
DEFAULT_HOST = os.environ.get("KHAEN_HOST", "192.0.2.215")
DEFAULT_USER = os.environ.get("KHAEN_USER", "root")


def connect(args):
    key = paramiko.Ed25519Key.from_private_key_file(str(args.key))
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(
        args.host,
        port=args.port,
        username=args.user,
        pkey=key,
        timeout=args.timeout,
        banner_timeout=args.timeout,
        auth_timeout=args.timeout,
    )
    return client


def run(client, cmd: str, timeout: float = 60.0) -> tuple[int, str, str]:
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    rc = stdout.channel.recv_exit_status()
    return rc, out, err


def cmd_status(client, args) -> int:
    cmds = [
        ("host", "hostname; date '+%F %T %z'; uptime"),
        ("soak", "systemctl is-active khaen-soak12.service 2>&1; systemctl show khaen-soak12.service -p ActiveState -p SubState -p ExecMainStartTimestamp 2>&1"),
        ("xrun", "tail -n 3 /var/lib/khaen-soak/status.csv 2>&1"),
        ("audio", "cat /proc/asound/cards 2>&1"),
        ("kernel", "uname -a; cat /sys/kernel/realtime 2>&1"),
    ]
    rc_all = 0
    for label, cmd in cmds:
        rc, out, err = run(client, cmd, args.timeout)
        print(f"--- {label} ---")
        print(out.rstrip() or err.rstrip() or f"(no output, rc={rc})")
        rc_all = rc_all or rc
    return rc_all


def cmd_run(client, args) -> int:
    rc, out, err = run(client, args.cmd, args.timeout)
    sys.stdout.write(out)
    sys.stderr.write(err)
    return rc


def cmd_put(client, args) -> int:
    sftp = client.open_sftp()
    try:
        sftp.put(str(args.local), args.remote)
        print(f"put {args.local} -> {args.remote}")
    finally:
        sftp.close()
    return 0


def cmd_get(client, args) -> int:
    sftp = client.open_sftp()
    try:
        sftp.get(args.remote, str(args.local))
        print(f"get {args.remote} -> {args.local}")
    finally:
        sftp.close()
    return 0


def cmd_reboot(client, args) -> int:
    rc, out, err = run(client, "sync; (sleep 1; reboot) >/dev/null 2>&1 &", 10.0)
    print("reboot requested")
    return rc


def main() -> int:
    p = argparse.ArgumentParser(description="khaen-audio-dsp board SSH helper")
    p.add_argument("--host", default=DEFAULT_HOST)
    p.add_argument("--user", default=DEFAULT_USER)
    p.add_argument("--port", type=int, default=22)
    p.add_argument("--key", type=Path, default=DEFAULT_KEY)
    p.add_argument("--timeout", type=float, default=20.0)
    sub = p.add_subparsers(dest="action", required=True)

    sub.add_parser("status")

    r = sub.add_parser("run")
    r.add_argument("--cmd", required=True)

    put = sub.add_parser("put")
    put.add_argument("--local", type=Path, required=True)
    put.add_argument("--remote", required=True)

    get = sub.add_parser("get")
    get.add_argument("--remote", required=True)
    get.add_argument("--local", type=Path, required=True)

    sub.add_parser("reboot")

    args = p.parse_args()
    if not args.key.exists():
        print(f"missing key: {args.key}", file=sys.stderr)
        return 2
    try:
        client = connect(args)
    except Exception as exc:
        print(f"connect failed: {exc}", file=sys.stderr)
        return 1
    try:
        if args.action == "status":
            return cmd_status(client, args)
        if args.action == "run":
            return cmd_run(client, args)
        if args.action == "put":
            return cmd_put(client, args)
        if args.action == "get":
            return cmd_get(client, args)
        if args.action == "reboot":
            return cmd_reboot(client, args)
        return 2
    finally:
        client.close()


if __name__ == "__main__":
    raise SystemExit(main())
