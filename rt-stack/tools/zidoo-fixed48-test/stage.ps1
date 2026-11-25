#Requires -Version 5.1
<#
.SYNOPSIS
    Stage the fixed-48k test APK onto the Zidoo box over its SMB share (no adb needed),
    verify the copy by hash, and optionally open the Android package installer.

.DESCRIPTION
    The Zidoo Z9X exposes \\<host>\Share\Storage (usually mapped as a drive letter).
    Copying the APK into Download/ is harmless; installing still requires pressing
    "Install" in the Android package installer dialog on the TV (user confirmation).

    -OpenInstaller calls the Zidoo 9529 API to open the APK on screen; it does NOT
    install anything by itself.

.EXAMPLE
    powershell -File stage.ps1
    powershell -File stage.ps1 -OpenInstaller
#>
[CmdletBinding()]
param(
    [string]$ApkPath = '',
    [string]$ZidooHost = '192.0.2.158',
    [string]$SmbDrive = '',
    [string]$RemoteDir = 'Download',
    [switch]$OpenInstaller
)

$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$repoRoot = (Resolve-Path (Join-Path $here '..\..\..')).Path
if (-not $ApkPath) {
    $ApkPath = Join-Path $repoRoot 'artifacts\tspi-audio\zidoo-test\zidoo-fixed48-test-v1.0.apk'
}
if (-not (Test-Path $ApkPath)) { throw ('APK not found: ' + $ApkPath) }
$apkName = Split-Path -Leaf $ApkPath

if (-not $SmbDrive) {
    $drives = @(Get-PSDrive -PSProvider FileSystem | Where-Object {
        $_.DisplayRoot -and ($_.DisplayRoot -like ('\\' + $ZidooHost + '\Share*'))
    })
    if ($drives.Count -eq 0) {
        throw ('no mapped SMB drive for \\' + $ZidooHost + '\Share; map it first or pass -SmbDrive')
    }
    $SmbDrive = $drives[0].Name
}
$driveRoot = $SmbDrive
if (-not $driveRoot.EndsWith(':')) { $driveRoot = $driveRoot + ':' }
$destDir = Join-Path ($driveRoot + '\') $RemoteDir
if (-not (Test-Path $destDir)) { throw ('remote SMB directory not reachable: ' + $destDir) }

Write-Host ('stage: ' + $ApkPath)
Write-Host ('   ->  ' + (Join-Path $destDir $apkName))
$localHash = (Get-FileHash -Algorithm SHA256 $ApkPath).Hash.ToLower()
$remotePath = Join-Path $destDir $apkName
Copy-Item -Path $ApkPath -Destination $remotePath -Force
$remoteHash = (Get-FileHash -Algorithm SHA256 $remotePath).Hash.ToLower()
if ($localHash -ne $remoteHash) { throw 'SMB copy hash mismatch' }
Write-Host ('   sha256 ' + $localHash + ' (verified over SMB)')

$remoteAndroidPath = '/storage/emulated/0/' + $RemoteDir + '/' + $apkName
$listUrl = 'http://' + $ZidooHost + ':9529/ZidooFileControl/getFileList?path=/storage/emulated/0/' + $RemoteDir + '&type=1000'
try {
    $listing = Invoke-RestMethod -Uri $listUrl -TimeoutSec 10
    $hit = @($listing.filelist | Where-Object { $_.name -eq $apkName })
    if ($hit.Count -gt 0) {
        Write-Host ('   zidoo api: ' + $hit[0].name + ' length=' + $hit[0].length + ' (visible in Download/)')
    } else {
        Write-Host '   zidoo api: file not listed yet (SMB copy exists; media index may lag)'
    }
} catch {
    Write-Host ('   zidoo api check failed: ' + $_.Exception.Message)
}

try {
    $apps = Invoke-RestMethod -Uri ('http://' + $ZidooHost + ':9529/ZidooControlCenter/Apps/getApps') -TimeoutSec 10
    $ours = @($apps.apps | Where-Object { $_.packageName -eq 'com.tspi.fixed48test' })
    if ($ours.Count -gt 0) {
        Write-Host ('   installed: com.tspi.fixed48test versionName=' + $ours[0].versionName + ' versionCode=' + $ours[0].versionCode)
    } else {
        Write-Host '   installed: com.tspi.fixed48test NOT installed yet'
    }
} catch {
    Write-Host ('   app list check failed: ' + $_.Exception.Message)
}

if ($OpenInstaller) {
    $encoded = [uri]::EscapeDataString($remoteAndroidPath)
    $url = 'http://' + $ZidooHost + ':9529/ZidooFileControl/openFile?path=' + $encoded
    Write-Host ('   opening package installer: ' + $url)
    $resp = Invoke-WebRequest -Uri $url -TimeoutSec 10 -UseBasicParsing
    Write-Host ('   openFile status: ' + $resp.StatusCode + ' ' + $resp.Content)
    Write-Host '   -> press the on-screen Install/OK prompt on the Zidoo to finish.'
} else {
    Write-Host ''
    Write-Host ('Next (one line): on the Zidoo open Download/' + $apkName + ', press Install/OK, then launch "Fixed48 AudioTrack Test".')
    Write-Host 'Or rerun with -OpenInstaller to bring the installer to the screen via the 9529 API.'
}
