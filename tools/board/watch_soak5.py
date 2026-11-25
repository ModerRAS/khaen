#!/usr/bin/env python3
import subprocess, sys, time
from pathlib import Path
HERE = Path(__file__).resolve().parent
cmd = [sys.executable, "board.py", "run", "--cmd",
       "date '+%H:%M:%S'; systemctl is-active khaen-soak5.service 2>&1; tail -n 1 /var/lib/khaen-soak/status.csv 2>&1; cat /proc/uptime"]
for i in range(11):
    print(f"--- poll {i} ---", flush=True)
    p = subprocess.run(cmd, cwd=HERE, text=True, encoding="utf-8", errors="replace", capture_output=True, timeout=30)
    print((p.stdout or "").strip(), flush=True)
    if p.returncode != 0:
        print("SSH/board failure:", (p.stderr or "").strip()[:500], flush=True)
        break
    if i < 10:
        time.sleep(30)
