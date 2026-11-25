#!/usr/bin/env python3
import sys
from pathlib import Path
sys.stdout.reconfigure(encoding="utf-8", errors="replace")
sys.path.insert(0, str(Path(__file__).resolve().parent))
from diagnose import remote_ps
PS = r'''
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Get-Command adb -ErrorAction SilentlyContinue | Format-List Source,Version,Path
Get-ChildItem -Path C:\Users\adqew\scoop\apps -Recurse -Filter adb.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName
'''
rc, out, err = remote_ps(PS, 30)
print("RC", rc); print(out); print(err, file=sys.stderr)
