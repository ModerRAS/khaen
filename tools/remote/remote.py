#!/usr/bin/env python3
"""泰山派远程烧录/诊断工具（Windows -> sender-host -> 板子）。

只用标准库；通过 OpenSSH 的 ssh.exe / scp.exe 调用，避免 PowerShell 引号问题。
远端命令统一用 PowerShell -EncodedCommand 执行；板端 Linux 命令直接 ssh。
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

SSH = shutil.which("ssh") or "ssh"
SCP = shutil.which("scp") or "scp"
SSH_OPTS = [
    "-o", "IdentitiesOnly=yes",
    "-o", "StrictHostKeyChecking=accept-new",
    "-o", "UserKnownHostsFile=NUL",
    "-o", "BatchMode=yes",
    "-o", "ConnectTimeout=8",
]

# 默认配置：可用 config.json 覆盖，或命令行参数覆盖。
DEFAULTS = {
    "remote_host": "192.0.2.63",
    "remote_user": "adqew",
    "remote_key": r"C:\WorkSpace\Claw\projects\tspi-audio-dsp\deploy\ssh\tspi_audio_ed25519",
    "remote_workspace": r"D:\WorkSpace\Claw",
    "board_host": "192.0.2.215",
    "board_user": "root",
    "board_key": r"C:\WorkSpace\Claw\projects\tspi-audio-dsp\deploy\ssh\tspi_audio_ed25519",
}


class ToolError(RuntimeError):
    pass


def load_config(path: str | None) -> dict:
    cfg = dict(DEFAULTS)
    if path:
        cfg.update(json.loads(Path(path).read_text(encoding="utf-8")))
    return cfg


def run(cmd: list[str], timeout: int = 60, check: bool = True) -> subprocess.CompletedProcess:
    p = subprocess.run(cmd, text=True, encoding="utf-8", errors="replace",
                       capture_output=True, timeout=timeout)
    if check and p.returncode != 0:
        raise ToolError(f"命令失败 rc={p.returncode}: {' '.join(cmd)}\nSTDOUT:\n{p.stdout[-3000:]}\nSTDERR:\n{p.stderr[-3000:]}")
    return p


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


class Remote:
    def __init__(self, cfg: dict):
        self.cfg = cfg

    def _ssh(self) -> list[str]:
        return [SSH, "-i", self.cfg["remote_key"], *SSH_OPTS,
                f'{self.cfg["remote_user"]}@{self.cfg["remote_host"]}']

    def ps(self, script: str, timeout: int = 60) -> str:
        enc = base64.b64encode(script.encode("utf-16-le")).decode("ascii")
        p = run(self._ssh() + ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-EncodedCommand", enc],
                timeout=timeout)
        return p.stdout

    def scp_to(self, local: str, remote: str, timeout: int = 600) -> None:
        run([SCP, "-i", self.cfg["remote_key"], *SSH_OPTS, local,
             f'{self.cfg["remote_user"]}@{self.cfg["remote_host"]}:{remote}'], timeout=timeout)

    def workspace(self) -> str:
        return self.cfg["remote_workspace"]


class Board:
    def __init__(self, cfg: dict):
        self.cfg = cfg

    def ssh(self, command: str, timeout: int = 30) -> str:
        p = run([SSH, "-i", self.cfg["board_key"], *SSH_OPTS,
                 f'{self.cfg["board_user"]}@{self.cfg["board_host"]}', command], timeout=timeout)
        return p.stdout


def cmd_status(cfg: dict) -> None:
    r = Remote(cfg); b = Board(cfg)
    print("== board USB gadget ==")
    print(b.ssh("cat /sys/class/udc/*/state; cat /sys/class/udc/*/current_speed 2>/dev/null").strip())
    print("\n== remote PnP (VID_2207) ==")
    print(r.ps("Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue | "
               "Where-Object { $_.InstanceId -match 'VID_2207' } | "
               "Format-List Status,Class,FriendlyName,InstanceId | Out-String").strip() or "(none)")
    print("\n== remote tool ==")
    print(r.ps("Get-ChildItem -LiteralPath 'D:\\WorkSpace\\Claw\\vendor\\rockchip-tools' -Recurse -Filter upgrade_tool.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName").strip() or "(none)")
    print("\n== image ==")
    img = Path(cfg["local_image"])
    if img.exists():
        print(f"{img} {img.stat().st_size} {sha256_file(str(img))}")
    else:
        print(f"{img} (missing)")


def cmd_ps(cfg: dict, script: str, timeout: int) -> None:
    print(Remote(cfg).ps(script, timeout=timeout))


def cmd_scp(cfg: dict, local: str, remote: str) -> None:
    Remote(cfg).scp_to(local, remote)
    print(f"copied {local} -> {remote}")


def tool_path(remote: Remote) -> str:
    out = remote.ps("Get-ChildItem -LiteralPath 'D:\\WorkSpace\\Claw\\vendor\\rockchip-tools' -Recurse -Filter upgrade_tool.exe -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName").strip()
    if not out:
        raise ToolError("远端找不到 upgrade_tool.exe")
    return out


def cmd_loader(cfg: dict) -> None:
    print("reboot board -> loader ...")
    Board(cfg).ssh("reboot loader", timeout=10)
    remote = Remote(cfg)
    exe = tool_path(remote)
    for i in range(20):
        time.sleep(2)
        try:
            out = remote.ps(f"& '{exe}' LD 2>&1 | Out-String")
        except Exception as e:
            print(f"[{i}] {e}"); continue
        print(f"[{i}] {out.strip()}")
        if "Loader" in out or "rockusb connected(1)" in out:
            print("loader ready"); return
    raise ToolError("等不到 Loader；检查 USB/驱动")


def cmd_flash(cfg: dict, remote_image: str | None, force: bool) -> None:
    local = Path(cfg["local_image"])
    if not local.exists():
        raise ToolError(f"本地镜像不存在: {local}")
    remote = Remote(cfg)
    dst = remote_image or f'{remote.workspace()}/artifacts/firmware/rt-oem-v14/update.img'
    print(f"local: {local} {local.stat().st_size} {sha256_file(str(local))}")
    # 远端已经存在且哈希一致则跳过复制
    need_copy = True
    try:
        out = remote.ps(f"if (Test-Path '{dst}') {{ (Get-FileHash -Algorithm SHA256 '{dst}').Hash.ToLower() }} else {{ 'MISSING' }}").strip()
        print("remote sha:", out)
        if out.lower() == sha256_file(str(local)):
            need_copy = False
    except Exception as e:
        print("remote hash check failed:", e)
    if need_copy:
        print("scp image ->", dst)
        remote.scp_to(str(local), dst)
        out = remote.ps(f"(Get-FileHash -Algorithm SHA256 '{dst}').Hash.ToLower()").strip()
        if out.lower() != sha256_file(str(local)):
            raise ToolError("远端镜像哈希不一致")
    if not force:
        raise ToolError("烧写会重启板子；确认后加 --force")
    cmd_loader(cfg)
    exe = tool_path(remote)
    print("UF start")
    print(remote.ps(f"& '{exe}' UF '{dst}' 2>&1 | Out-String", timeout=900))
    print("wait boot ...")
    for i in range(30):
        time.sleep(2)
        try:
            out = Board(cfg).ssh("uname -a", timeout=5)
            print("board:", out.strip()); return
        except Exception:
            print(f"[{i}] wait ssh ...")
    raise ToolError("烧完但 SSH 未恢复")


def cmd_copy_scripts(cfg: dict) -> None:
    remote = Remote(cfg)
    base = f'{remote.workspace()}/scratch/lead/remote-flash'
    remote.ps(f"New-Item -ItemType Directory -Force -Path '{base}' | Out-Null")
    here = Path(__file__).parent
    for name in ["remote.py", "README.md"]:
        p = here / name
        if p.exists():
            remote.scp_to(str(p), f"{base}/{name}")
    print("scripts copied to", base)


def main() -> int:
    ap = argparse.ArgumentParser(description="泰山派远程诊断/烧录（Python）")
    ap.add_argument("-c", "--config", help="JSON 配置覆盖")
    sub = ap.add_subparsers(dest="cmd", required=True)

    sub.add_parser("status", help="板端 UDC + 远端 PnP + 工具/镜像状态")
    p = sub.add_parser("ps", help="在远端执行一段 PowerShell")
    p.add_argument("--script", required=True)
    p.add_argument("--timeout", type=int, default=120)
    p = sub.add_parser("scp", help="复制文件到远端")
    p.add_argument("--local", required=True)
    p.add_argument("--remote", required=True)
    sub.add_parser("loader", help="重启板子进入 Loader 并等待")
    p = sub.add_parser("flash", help="烧写镜像（会重启板子，需 --force）")
    p.add_argument("--image", help="远端镜像路径")
    p.add_argument("--force", action="store_true")
    sub.add_parser("copy-scripts", help="把本工具复制到远端工作区")

    args = ap.parse_args()
    cfg = load_config(args.config)
    cfg.setdefault("local_image", r"C:\WorkSpace\Claw\projects\tspi-audio-dsp\firmware\rt-oem-v14\update.img")
    if args.cmd == "status": cmd_status(cfg)
    elif args.cmd == "ps": cmd_ps(cfg, args.script, args.timeout)
    elif args.cmd == "scp": cmd_scp(cfg, args.local, args.remote)
    elif args.cmd == "loader": cmd_loader(cfg)
    elif args.cmd == "flash": cmd_flash(cfg, args.image, args.force)
    elif args.cmd == "copy-scripts": cmd_copy_scripts(cfg)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ToolError as e:
        print("ERROR:", e, file=sys.stderr)
        raise SystemExit(2)
