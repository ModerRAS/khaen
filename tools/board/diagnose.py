#!/usr/bin/env python3
"""Diagnose board / remote Windows / adb reachability without pwsh quoting.

Examples:
  python diagnose.py
  python diagnose.py --ssh-board "systemctl is-active khaen-soak12.service"
  python diagnose.py --ssh-remote "adb devices -l"
  python diagnose.py --ssh-remote "adb shell 'ip -br addr; uptime'"
"""
from __future__ import annotations

import argparse
import base64
import socket
import subprocess
import sys
from pathlib import Path

import paramiko

ROOT = Path(__file__).resolve().parents[2]
KEY = ROOT / "deploy" / "ssh" / "khaen_audio_ed25519"
BOARD = ("192.0.2.215", "root")
REMOTE = ("192.0.2.63", "adqew")
GATEWAY = "192.0.2.1"


def tcp(host: str, port: int = 22, timeout: float = 3.0) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def arp(host: str) -> str:
    try:
        out = subprocess.run(["arp", "-a", host], capture_output=True, text=True, timeout=5).stdout
        return out.strip() or "(no arp entry)"
    except Exception as exc:
        return f"(arp failed: {exc})"


def ssh_connect(host: str, user: str, timeout: float = 8.0) -> paramiko.SSHClient:
    key = paramiko.Ed25519Key.from_private_key_file(str(KEY))
    c = paramiko.SSHClient()
    c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    c.connect(host, username=user, pkey=key, timeout=timeout,
              banner_timeout=timeout, auth_timeout=timeout)
    return c


def ssh_exec(host: str, user: str, cmd: str, timeout: float = 20.0) -> tuple[int, str, str]:
    c = ssh_connect(host, user, timeout)
    try:
        _, out, err = c.exec_command(cmd, timeout=timeout)
        return out.channel.recv_exit_status(), out.read().decode("utf-8", "replace"), err.read().decode("utf-8", "replace")
    finally:
        c.close()


def remote_ps(script: str, timeout: float = 20.0) -> tuple[int, str, str]:
    enc = base64.b64encode(script.encode("utf-16-le")).decode("ascii")
    return ssh_exec(*REMOTE, f"powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand {enc}", timeout)


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--ssh-board", metavar="CMD")
    p.add_argument("--ssh-remote", metavar="CMD")
    args = p.parse_args()

    if args.ssh_board:
        try:
            rc, out, err = ssh_exec(*BOARD, args.ssh_board)
        except Exception as exc:
            print(f"board ssh failed: {exc}", file=sys.stderr)
            return 1
        sys.stdout.write(out); sys.stderr.write(err)
        return rc

    if args.ssh_remote:
        try:
            rc, out, err = remote_ps(args.ssh_remote)
        except Exception as exc:
            print(f"remote ssh failed: {exc}", file=sys.stderr)
            return 1
        sys.stdout.write(out); sys.stderr.write(err)
        return rc

    print("== reachability ==")
    for label, host, port in [("gateway", GATEWAY, 80), ("board", BOARD[0], 22), ("remote", REMOTE[0], 22)]:
        print(f"{label:8s} {host:16s} tcp/{port}: {'OK' if tcp(host, port) else 'FAIL'}  arp: {arp(host)}")

    print("\n== board SSH ==")
    try:
        rc, out, err = ssh_exec(*BOARD, "hostname; date; uptime; systemctl is-active khaen-soak12.service 2>&1; tail -n 2 /var/lib/khaen-soak/status.csv 2>&1", 10)
        print(out.rstrip() or err.rstrip() or f"(rc={rc})")
    except Exception as exc:
        print(f"FAIL {exc}")

    print("\n== remote adb ==")
    try:
        rc, out, err = remote_ps("adb devices -l; Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue | Where-Object { $_.InstanceId -match 'VID_2207' } | Select-Object Status,Class,FriendlyName,InstanceId | Format-List", 20)
        print(out.rstrip() or err.rstrip() or f"(rc={rc})")
    except Exception as exc:
        print(f"FAIL {exc}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
