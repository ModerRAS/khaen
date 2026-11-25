#!/usr/bin/env python3
import sys
from pathlib import Path
sys.stdout.reconfigure(encoding="utf-8", errors="replace")
sys.path.insert(0, str(Path(__file__).resolve().parent))
from diagnose import remote_ps

PS = r'''
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
adb kill-server
adb start-server
adb devices -l
Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue |
  Where-Object { $_.InstanceId -match 'VID_2207' } |
  Select-Object Status,Class,FriendlyName,InstanceId | Format-List
Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue |
  Where-Object { $_.InstanceId -match 'VID_2207' } |
  ForEach-Object { Get-PnpDeviceProperty -InstanceId $_.InstanceId } |
  Where-Object { $_.KeyName -in @('DEVPKEY_Device_Service','DEVPKEY_Device_CompatibleIds','DEVPKEY_Device_HardwareIds','DEVPKEY_Device_Parent') } |
  Select-Object KeyName,Data | Format-List
Get-NetAdapter | Select-Object Name,InterfaceDescription,Status,MacAddress | Format-Table -AutoSize
'''
rc, out, err = remote_ps(PS, 45)
print("RC", rc)
print(out)
print(err, file=sys.stderr)
