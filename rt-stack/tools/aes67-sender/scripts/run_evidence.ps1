param(
  [string]$Out = 'C:\WorkSpace\Claw\projects\tspi-audio-dsp\rt-stack\evidence\aes67-tx-win',
  [string]$McastIface = '192.168.56.1'
)
$ErrorActionPreference = 'Stop'
# DSH/Node exports duplicate case-variant proxy vars; Start-Process chokes on them.
[Environment]::SetEnvironmentVariable('no_proxy',$null,'Process')
[Environment]::SetEnvironmentVariable('http_proxy',$null,'Process')
[Environment]::SetEnvironmentVariable('https_proxy',$null,'Process')
$Crate = 'C:\WorkSpace\Claw\projects\tspi-audio-dsp\rt-stack\tools\aes67-sender'
Set-Location $Crate
$env:CARGO_NET_OFFLINE='true'
$Run = Join-Path $Out 'runs'
$Sdp = Join-Path $Out 'sdp'
$Build = Join-Path $Out 'build'
Remove-Item $Out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Run,$Sdp,$Build | Out-Null
$Snd = (Resolve-Path '.\target\release\aes67-send.exe').Path
$Rcv = (Resolve-Path '.\target\release\aes67-recv.exe').Path
$Play = (Resolve-Path '.\target\release\aes67-play.exe').Path
$Gen = (Resolve-Path '.\target\release\aes67-gen-wav.exe').Path
$Dev = (Resolve-Path '.\target\release\aes67-devices.exe').Path
$CableId = '{0.0.0.00000000}.{9015d52f-7d46-4a7c-b7b9-3b5bb7d141f3}'  # CABLE Input (VB-Audio Virtual Cable)
$DevArg = '--device=' + $CableId
$WasapiDevArg = '--wasapi-device=' + $CableId
$summary = New-Object System.Collections.ArrayList
function Add-Summary([string]$name, [string]$verdict, [string]$detail) {
  [void]$summary.Add([pscustomobject]@{ kind=$name; verdict=$verdict; detail=$detail })
  Write-Output ("[{0}] {1}: {2}" -f $verdict, $name, $detail)
}
function Get-ExitCode($p) {
  if ($null -eq $p) { return 'n/a' }
  try { $p.Refresh(); $c = $p.ExitCode } catch { $c = $null }
  if ($null -eq $c) { return 'n/a' } else { return $c }
}
function Invoke-SenderSync([string]$txArgs, [string]$txOut, [string]$txErr, [string]$txExitFile) {
  $cmdLine = '"' + $Snd + '" ' + ($txArgs -join ' ') + ' 1> "' + $txOut + '" 2> "' + $txErr + '"'
  cmd /c $cmdLine | Out-Null
  $code = $LASTEXITCODE
  Set-Content -Path $txExitFile -Value $code -Encoding ascii
  return $code
}
function Show-Rx([string]$name, [string]$rxJson, [string]$txJson) {
  $j = Get-Content $rxJson -Raw | ConvertFrom-Json
  $line = ("packets={0} frames={1} verdict={2} missing={3} dup={4} ooo={5} ch={6}" -f $j.packets,$j.frames,$j.verdict,$j.seq.missing,$j.seq.duplicates,$j.seq.out_of_order,(($j.per_channel | ForEach-Object { 'ch{0}:{1}Hz/amp{2}' -f $_.channel,$_.freq_est_hz,$_.own_expected_amp }) -join ','))
  if ($j.verdict -eq 'PASS') { Add-Summary $name 'PASS' $line } else { Add-Summary $name 'FAIL' $line }
  if (Test-Path $txJson) {
    $t = Get-Content $txJson -Raw | ConvertFrom-Json
    Add-Summary ($name + '-pacing') 'INFO' ('packets={0} underruns={1} late_p50={2}us late_p99={3}us late_max={4}us' -f $t.packets_sent,$t.underrun_packets,$t.deadline.p50_us,$t.deadline.p99_us,$t.deadline.max_us)
  }
}

Write-Output '=== environment / build ==='
(& rustc -Vv) | Out-File -Encoding utf8 (Join-Path $Build 'rustc.txt')
(& cargo -V) | Out-File -Encoding utf8 (Join-Path $Build 'cargo.txt')
(& rustup show) | Out-File -Encoding utf8 (Join-Path $Build 'rustup.txt')
((Get-CimInstance Win32_OperatingSystem).Caption + ' build ' + [System.Environment]::OSVersion.Version.Build) | Out-File -Encoding utf8 (Join-Path $Build 'os.txt')
cmd /c "cargo build --release 2> $(Join-Path $Build 'build-stderr.log') > $(Join-Path $Build 'build-stdout.log')"
$buildExit = $LASTEXITCODE
Add-Summary 'cargo-build-release' $(if ($buildExit -eq 0) {'PASS'} else {'FAIL'}) ("exit=$buildExit")
cmd /c "cargo test 2> $(Join-Path $Build 'test-stderr.log') > $(Join-Path $Build 'test-stdout.log')"
$testExit = $LASTEXITCODE
$testLines = Select-String -Path (Join-Path $Build 'test-stdout.log') -Pattern 'test result:' | ForEach-Object { $_.Line.Trim() }
$testLines | Out-File -Encoding utf8 (Join-Path $Build 'test-summary.txt')
Add-Summary 'cargo-test' $(if ($testExit -eq 0) {'PASS'} else {'FAIL'}) ("exit=$testExit :: " + (($testLines | Where-Object { $_ -match 'passed' }) -join ' | '))
cmd /c "cargo fmt --check 2> $(Join-Path $Build 'fmt-stderr.log') > $(Join-Path $Build 'fmt-stdout.log')"
$fmtExit = $LASTEXITCODE
Add-Summary 'cargo-fmt-check' $(if ($fmtExit -eq 0) {'PASS'} else {'FAIL'}) ("exit=$fmtExit")
$binLines = Get-ChildItem target\release\aes67-*.exe | ForEach-Object { $h = (Get-FileHash $_.FullName -Algorithm SHA256).Hash; ('{0} | {1} bytes | sha256 {2}' -f $_.Name, $_.Length, $h) }
$binLines | Out-File -Encoding utf8 (Join-Path $Build 'binaries.txt')
$binLines | ForEach-Object { Write-Output $_ }

Write-Output '=== endpoints ==='
& $Dev --formats 2>&1 | Out-File -Encoding utf8 (Join-Path $Run 'devices.txt')
Add-Summary 'wasapi-devices' 'INFO' 'see runs/devices.txt (default render endpoint = CABLE Input, 48k/2ch/f32)'

$toneWav = Join-Path $Run 'tone_1k_2k_3s_l24.wav'
$toneRaw = Join-Path $Run 'tone_1k_2k_3s_l24.raw'
& $Gen --out $toneWav --duration 3 --channels 2 --bits 24 --freq-l 1000 --freq-r 2000 --amp 0.5 --raw-out $toneRaw 2>&1 | Out-File -Encoding utf8 (Join-Path $Run 'gen-wav.log')
Add-Summary 'gen-wav' $(if ($LASTEXITCODE -eq 0) {'PASS'} else {'FAIL'}) 'stereo 1k/2k 3s L24 input fixture'

function Run-FileCase([string]$name, [string]$rxArgs, [string]$txArgs, [string]$sdpCopy, [string]$sdpName) {
  $d = Join-Path $Run $name; New-Item -ItemType Directory -Force -Path $d | Out-Null
  $rx = Start-Process -FilePath $Rcv -ArgumentList $rxArgs -RedirectStandardOutput "$d\rx.out" -RedirectStandardError "$d\rx.err" -PassThru -NoNewWindow
  Start-Sleep -Milliseconds 400
  $txCode = Invoke-SenderSync $txArgs "$d\tx.out" "$d\tx.err" "$d\tx.exit.txt"
  Start-Sleep -Milliseconds 200
  if (-not $rx.WaitForExit(30000)) { $rx.Kill() }
  Add-Summary ($name + '-exit') 'INFO' ("tx=$txCode rx=$(Get-ExitCode $rx)")
  Show-Rx $name "$d\rx.json" "$d\tx.json"
  Copy-Item (Join-Path $d $sdpName) (Join-Path $Sdp $sdpCopy) -Force
}

Write-Output '=== 1) unicast WAV -> RTP -> Rust receiver ==='
$d0 = Join-Path $Run 'unicast-wav'
Run-FileCase 'unicast-wav' @('--port','5510','--channels','2','--silence-ms','1000','--duration','5','--wav-out',"$d0\rx.wav",'--stats-json',"$d0\rx.json",'--expect-tone','1000','--expect-tone','2000','--dump-rtp','3') @('--source','wav','--wav',$toneWav,'--dest','127.0.0.1','--port','5510','--sdp-out',"$d0\unicast.sdp",'--stats-json',"$d0\tx.json") 'unicast.sdp' 'unicast.sdp'

Write-Output '=== 2) multicast (host-only NIC, TTL=1, loop enabled; never reaches the board subnet) ==='
$d0 = Join-Path $Run 'multicast'
Run-FileCase 'multicast' @('--port','5504','--multicast','239.69.63.10','--iface-ip',$McastIface,'--channels','2','--silence-ms','900','--duration','5','--wav-out',"$d0\rx.wav",'--stats-json',"$d0\rx.json",'--expect-tone','1000','--expect-tone','2000','--dump-rtp','2') @('--source','wav','--wav',$toneWav,'--dest','239.69.63.10','--port','5504','--iface-ip',$McastIface,'--ttl','1','--sdp-out',"$d0\multicast.sdp",'--stats-json',"$d0\tx.json") 'multicast.sdp' 'multicast.sdp'

Write-Output '=== 3) raw PCM L24 input -> RTP -> Rust receiver ==='
$d0 = Join-Path $Run 'raw-pcm'
Run-FileCase 'raw-pcm' @('--port','5511','--channels','2','--silence-ms','900','--duration','5','--wav-out',"$d0\rx.wav",'--stats-json',"$d0\rx.json",'--expect-tone','1000','--expect-tone','2000') @('--source','raw','--raw',$toneRaw,'--raw-format','l24','--channels','2','--dest','127.0.0.1','--port','5511','--sdp-out',"$d0\raw.sdp",'--stats-json',"$d0\tx.json") 'raw-pcm-unicast.sdp' 'raw.sdp'

Write-Output '=== 4) WASAPI system loopback (any-app endpoint capture; virtual cable render = no audible output) ==='
$d = Join-Path $Run 'wasapi-system-loopback'; New-Item -ItemType Directory -Force -Path $d | Out-Null
$rx = Start-Process -FilePath $Rcv -ArgumentList @('--port','5512','--channels','2','--silence-ms','1000','--duration','6','--wav-out',"$d\rx.wav",'--stats-json',"$d\rx.json",'--expect-tone','1000','--expect-tone','2000') -RedirectStandardOutput "$d\rx.out" -RedirectStandardError "$d\rx.err" -PassThru -NoNewWindow
Start-Sleep -Milliseconds 300
$pl = Start-Process -FilePath $Play -ArgumentList @('--gen-tone','--freq-l','1000','--freq-r','2000','--amp','0.5','--duration','6',$DevArg) -RedirectStandardOutput "$d\play.out" -RedirectStandardError "$d\play.err" -PassThru -NoNewWindow
Start-Sleep -Milliseconds 400
$txCode = Invoke-SenderSync @('--source','wasapi-loopback',$WasapiDevArg,'--duration','3','--dest','127.0.0.1','--port','5512','--warmup-ms','100','--sdp-out',"$d\wasapi-system.sdp",'--stats-json',"$d\tx.json") "$d\tx.out" "$d\tx.err" "$d\tx.exit.txt"
$pl.WaitForExit(); $playCode = Get-ExitCode $pl
Start-Sleep -Milliseconds 200
if (-not $rx.WaitForExit(30000)) { $rx.Kill() }
Add-Summary 'wasapi-system-exit' 'INFO' ("tx=$txCode rx=$(Get-ExitCode $rx) play=$playCode")
Show-Rx 'wasapi-system-loopback' "$d\rx.json" "$d\tx.json"
Copy-Item "$d\wasapi-system.sdp" (Join-Path $Sdp 'wasapi-system.sdp') -Force

Write-Output '=== 5) WASAPI process loopback (PID-targeted process tree; virtual cable render) ==='
$d = Join-Path $Run 'wasapi-process-loopback'; New-Item -ItemType Directory -Force -Path $d | Out-Null
$rx = Start-Process -FilePath $Rcv -ArgumentList @('--port','5513','--channels','2','--silence-ms','1000','--duration','6','--wav-out',"$d\rx.wav",'--stats-json',"$d\rx.json",'--expect-tone','1000','--expect-tone','2000') -RedirectStandardOutput "$d\rx.out" -RedirectStandardError "$d\rx.err" -PassThru -NoNewWindow
Start-Sleep -Milliseconds 300
$pl = Start-Process -FilePath $Play -ArgumentList @('--gen-tone','--freq-l','1000','--freq-r','2000','--amp','0.5','--duration','12',$DevArg) -RedirectStandardOutput "$d\play.out" -RedirectStandardError "$d\play.err" -PassThru -NoNewWindow
Start-Sleep -Milliseconds 600
$txCode = Invoke-SenderSync @('--source','wasapi-process','--pid',"$($pl.Id)",'--duration','3','--dest','127.0.0.1','--port','5513','--warmup-ms','100','--sdp-out',"$d\wasapi-process.sdp",'--stats-json',"$d\tx.json") "$d\tx.out" "$d\tx.err" "$d\tx.exit.txt"
if (-not $pl.HasExited) { $pl.Kill() }
Start-Sleep -Milliseconds 200
if (-not $rx.WaitForExit(30000)) { $rx.Kill() }
Add-Summary 'wasapi-process-exit' 'INFO' ("tx=$txCode rx=$(Get-ExitCode $rx) play_pid=$($pl.Id)")
Show-Rx 'wasapi-process-loopback' "$d\rx.json" "$d\tx.json"
Copy-Item "$d\wasapi-process.sdp" (Join-Path $Sdp 'wasapi-process.sdp') -Force

Write-Output '=== summary ==='
$summary | ConvertTo-Json -Depth 4 | Out-File -Encoding utf8 (Join-Path $Run 'summary.json')
$summary | Format-Table -AutoSize | Out-String -Width 220 | Write-Output
$fails = @($summary | Where-Object { $_.verdict -eq 'FAIL' })
if ($fails.Count -gt 0) { Write-Output ('RESULT=FAIL failures=' + $fails.Count); exit 2 } else { Write-Output 'RESULT=PASS'; exit 0 }