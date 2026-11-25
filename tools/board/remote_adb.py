#!/usr/bin/env python3
import sys
from pathlib import Path
sys.stdout.reconfigure(encoding="utf-8", errors="replace")
sys.path.insert(0, str(Path(__file__).resolve().parent))
from diagnose import remote_ps
PS = r'''
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$adb = 'C:\Users\adqew\scoop\apps\adb\37.0.1\platform-tools\adb.exe'
& $adb start-server 2>&1
& $adb devices -l 2>&1
& $adb shell 'hostname; ip -br addr; systemctl is-active tspi-soak12.service 2>&1; tail -n 2 /var/lib/tspi-soak/status.csv 2>&1' 2>&1
'''
rc, out, err = remote_ps(PS, 45)
print("RC", rc); print(out); print(err, file=sys.stderr)
