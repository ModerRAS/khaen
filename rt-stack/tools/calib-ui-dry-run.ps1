#requires -Version 5.1
<#
.SYNOPSIS
  TSPI calibration UI one-command dry-run acceptance (no audio).
.DESCRIPTION
  Starts local tspi-rt-daemon (synthetic backend, no sound), then:
  1) POST /api/calibration/run {dry_run:true} -> six-stage plan; unimplemented stages
     MUST be reported pending_host_field, never faked as passed.
  2) POST /api/calibration/jobs for bass/fir/modal/level/delay -> expected 501
     pending_host_field while host fields are not wired.
  3) POST /api/calibration/jobs stage=measure -> 201 synthetic PREDICTED_ONLY (no audio).
  4) GET /calibration page + style.css + app.js.
  Prints JSON summary + elapsed ms. No audio device, no deploy, no push.
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File rt-stack\tools\calib-ui-dry-run.ps1
#>
param(
  [string]$Bin = "",
  [int]$Port = 18090,
  [string]$Password = "dry-run-password",
  [int]$ExitAfterMs = 30000
)
$ErrorActionPreference = 'Stop'
$script:Failures = @()
function Write-Step([string]$text) { Write-Host ("== " + $text) }

$root = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrEmpty($Bin)) {
  $Bin = Join-Path $root 'target\debug\tspi-rt-daemon.exe'
}
if (-not (Test-Path $Bin)) { throw "daemon binary not found: $Bin (run cargo build -p khaen-control --bin tspi-rt-daemon)" }

# TEMP may point at R: (ImDisk) where mkdir can be denied; use C: workspace temp.
$runRoot = 'C:\WorkSpace\Claw\scratch\lead\cal-ui-tmp'
New-Item -ItemType Directory -Force -Path $runRoot | Out-Null
$runDir = Join-Path $runRoot ("dry-run-" + $PID)
New-Item -ItemType Directory -Force -Path (Join-Path $runDir 'jobs'), (Join-Path $runDir 'state') | Out-Null
$configPath = Join-Path $runDir 'rt-daemon.toml'
$escaped = $runDir.Replace('\', '/')
$configText = @"
listen = "127.0.0.1:$Port"
[paths]
state_dir = "$escaped/state"
[daemon]
job_dir = "$escaped/jobs"
auth_file = "$escaped/state/daemon-auth.json"
[calibration]
stage = "synthetic"
"@
Set-Content -Path $configPath -Value $configText -Encoding ASCII
$stdout = Join-Path $runDir 'daemon.out.log'
$stderr = Join-Path $runDir 'daemon.err.log'
$base = "http://127.0.0.1:$Port"
$session = New-Object Microsoft.PowerShell.Commands.WebRequestSession
$csrf = $null
$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()

function Convert-ResponseBody($response) {
  if ($null -eq $response) { return $null }
  try { return ($response.Content | ConvertFrom-Json) } catch { return $response.Content }
}
function Invoke-Api {
  param([string]$Method, [string]$Path, $Body = $null, [bool]$Auth = $true)
  $headers = @{ 'Accept' = 'application/json' }
  if ($Auth -and $csrf) { $headers['X-TSPI-CSRF'] = $csrf }
  $params = @{ Uri = ($base + $Path); Method = $Method; UseBasicParsing = $true; WebSession = $session; Headers = $headers; TimeoutSec = 10 }
  if ($null -ne $Body) {
    $params['ContentType'] = 'application/json'
    $params['Body'] = ($Body | ConvertTo-Json -Depth 12 -Compress)
  }
  try {
    $resp = Invoke-WebRequest @params
    return [pscustomobject]@{ status = [int]$resp.StatusCode; json = (Convert-ResponseBody $resp) }
  } catch {
    $status = 0
    $text = $_.Exception.Message
    if ($_.Exception.Response) {
      $status = [int]$_.Exception.Response.StatusCode
      try {
        $stream = $_.Exception.Response.GetResponseStream()
        if ($stream) { $text = (New-Object System.IO.StreamReader($stream)).ReadToEnd() }
      } catch { }
    }
    $json = $null
    try { $json = ($text | ConvertFrom-Json) } catch { }
    return [pscustomobject]@{ status = $status; json = $json; raw = $text }
  }
}
function Add-Failure([string]$text) { $script:Failures += $text; Write-Host ("FAIL: " + $text) }

$daemon = $null
try {
  Write-Step "start daemon synthetic (no audio) port=$Port"
  # Start-Process fails in this environment (NO_PROXY/no_proxy duplicate); use .NET ProcessStartInfo.
  $psi = New-Object -TypeName System.Diagnostics.ProcessStartInfo
  $psi.FileName = $Bin
  $psi.Arguments = ('serve --config "' + $configPath + '" --exit-after-ms ' + $ExitAfterMs)
  $psi.UseShellExecute = $false
  $psi.CreateNoWindow = $true
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  $daemon = [System.Diagnostics.Process]::Start($psi)
  $daemon.StandardOutput.ReadToEndAsync() | Out-Null
  $daemon.StandardError.ReadToEndAsync() | Out-Null
  $ready = $false
  for ($i = 0; $i -lt 100; $i++) {
    Start-Sleep -Milliseconds 100
    try { $h = Invoke-WebRequest -Uri ($base + '/healthz') -UseBasicParsing -TimeoutSec 2; if ($h.StatusCode -eq 200) { $ready = $true; break } } catch { }
    if ($daemon.HasExited) { break }
  }
  if (-not $ready) { throw "daemon did not become ready" }
  Write-Step "daemon ready"

  $setup = Invoke-Api -Method 'POST' -Path '/api/auth/setup' -Body @{ password = $Password } -Auth $false
  if ($setup.status -eq 200 -and $setup.json.csrf_token) { $csrf = $setup.json.csrf_token }
  else {
    $login = Invoke-Api -Method 'POST' -Path '/api/auth/login' -Body @{ password = $Password } -Auth $false
    if ($login.json.csrf_token) { $csrf = $login.json.csrf_token }
  }
  $sessionResp = Invoke-Api -Method 'GET' -Path '/api/auth/session'
  if ($sessionResp.json.csrf_token) { $csrf = $sessionResp.json.csrf_token }
  if (-not $csrf) { Add-Failure 'no CSRF token from auth session' }

  $settings = @{
    mic = @{ kind = 'usb'; device = 'hw:U18dB,0'; channel = 0; phantom_48v = $false; gain_db = 18 }
    cal_file = $null; orientation_deg = 90; target_mode = 'flat'
    bass = @{ enabled = $true; crossover_hz = 80; main_slope = 'LR4'; sub_slope = 'LR4'; linear_phase = $true; sub_gain_db = 0; strategy = 'multi_b' }
    fir = @{ enabled = $true; target_curve = 'flat'; taps = 4096; phase = 'mixed' }
    modal = @{ enabled = $true; auto_detect = $true; max_notches = 4 }
  }
  $saved = Invoke-Api -Method 'POST' -Path '/api/calibration/settings' -Body $settings
  if ($saved.status -ne 200) { Add-Failure ("settings save status " + $saved.status + " " + $saved.raw) }

  Write-Step "one-command dry-run: POST /api/calibration/run {dry_run:true}"
  $dry = Invoke-Api -Method 'POST' -Path '/api/calibration/run' -Body @{ dry_run = $true }
  if ($dry.status -ne 200) { Add-Failure ("dry-run status " + $dry.status + " body=" + $dry.raw) }
  $stageSummary = @()
  if ($dry.json -and $dry.json.stages) {
    foreach ($stage in $dry.json.stages) {
      $state = if ($stage.available) { 'available' } else { 'pending_host_field (NOT implemented; not faked)' }
      $stageSummary += [ordered]@{ order = $stage.order; stage = $stage.stage; state = $state; pending_host_fields = @($stage.pending_host_fields).Count }
    }
  }

  Write-Step "six-stage execution path (honest: host fields pending => 501)"
  $flow = @()
  foreach ($stage in @('bass','fir','modal','level','delay')) {
    $r = Invoke-Api -Method 'POST' -Path '/api/calibration/jobs' -Body @{ kind = 'emperor'; stage = $stage; audible_sweep_authorized = $true }
    $code = if ($r.json -and $r.json.error) { $r.json.error.code } else { '' }
    $flow += [ordered]@{ stage = $stage; http = $r.status; code = $code; note = $(if ($r.status -eq 501) { 'pending host fields (expected)' } elseif ($r.status -eq 201) { 'job created' } else { 'unexpected' }) }
    if ($r.status -ne 501) { Add-Failure ("stage $stage expected 501 pending_host_field, got " + $r.status + " " + $code) }
  }
  $emperor = Invoke-Api -Method 'POST' -Path '/api/calibration/jobs' -Body @{ kind = 'emperor'; stage = 'measure'; audible_sweep_authorized = $true }
  $flow += [ordered]@{ stage = 'measure/emperor'; http = $emperor.status; code = ''; note = $(if ($emperor.status -eq 201) { 'synthetic PREDICTED_ONLY (no audio)' } else { 'unexpected' }) }
  if ($emperor.status -ne 201) { Add-Failure ("emperor measure expected 201 synthetic, got " + $emperor.status) }

  Write-Step "GET /calibration page + assets"
  $page = Invoke-WebRequest -Uri ($base + '/calibration') -UseBasicParsing -TimeoutSec 5
  $css = Invoke-WebRequest -Uri ($base + '/calibration/style.css') -UseBasicParsing -TimeoutSec 5
  $js = Invoke-WebRequest -Uri ($base + '/calibration/app.js') -UseBasicParsing -TimeoutSec 5
  $pageOk = ($page.StatusCode -eq 200 -and $page.Content -match 'TSPI' -and $page.Content -match '/calibration/app.js')
  if (-not $pageOk) { Add-Failure 'calibration page content check failed' }
  if ($css.StatusCode -ne 200 -or $js.StatusCode -ne 200) { Add-Failure 'calibration assets not 200' }

  $stopwatch.Stop()
  $summary = [ordered]@{
    ok = ($script:Failures.Count -eq 0)
    mode = 'dry-run'
    audible = $false
    daemon = $Bin
    listen = $base
    elapsed_ms = $stopwatch.ElapsedMilliseconds
    dry_run_http = $dry.status
    dry_run_audible = $dry.json.audible
    dry_run_stages = $stageSummary
    six_stage = $flow
    page = [ordered]@{ status = $page.StatusCode; app_js_bytes = $js.RawContentLength; style_css_bytes = $css.RawContentLength; title_ok = $pageOk }
    failures = $script:Failures
    note = 'No audio device opened; no host/ALSA/MOTU/UMIK access; no deploy/push. Pending stages are reported as pending_host_field, never faked as passed.'
    run_dir = $runDir
  }
  $summary | ConvertTo-Json -Depth 8
} finally {
  if ($daemon -and -not $daemon.HasExited) {
    try { Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue } catch { }
  }
}
if ($script:Failures.Count -gt 0) { exit 2 } else { exit 0 }
