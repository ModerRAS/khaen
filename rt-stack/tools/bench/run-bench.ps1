# KHAEN RT-first DSP offline perf harness runner (WBS T29/T46/T30; task-156)
#
# Discipline (rt-stack/README.md section 1/3): the rt-stack repo allows exactly ONE Cargo root,
# so this script does NOT create any crate/workspace/Cargo.toml inside rt-stack. It copies
# tools/bench/src/perf_harness.rs + Cargo.toml.template into a TEMP staging crate that only
# path-depends (read-only) on rt-stack/crates/core, then builds/runs with cargo --release and
# writes CSV/JSON/logs into the evidence directory.
#
# Usage (from anywhere):
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/bench/run-bench.ps1
#   ... -OutDir evidence/perf-harness/local-x86-2 -Blocks 64 -Reps 3
#   ... -Quick          # 16 blocks / 1 rep, quick smoke
#
# NOT_RUN by design (this script never touches the board): RT/IRQ/cyclictest/osnoise, real
# ALSA/MOTU, A72 speedup, 1h soak, PMU, kernel affinity/FIFO writes, git. x86 numbers are
# harness/reference only; board gates remain NOT_RUN.

param(
    [string]$OutDir = "evidence/perf-harness/local-x86",
    [int]$Blocks = 64,
    [int]$Warmup = 8,
    [int]$Reps = 3,
    [switch]$Quick,
    [switch]$NoOffline
)

$ErrorActionPreference = "Stop"
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$benchDir = $PSScriptRoot
$rtStack = (Resolve-Path (Join-Path $benchDir "..\..")).Path
$corePath = (Resolve-Path (Join-Path $rtStack "crates\core")).Path
if (-not (Test-Path (Join-Path $corePath "Cargo.toml"))) {
    throw "core crate not found at $corePath"
}
if (-not (Test-Path $OutDir)) {
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
}
$OutDir = (Resolve-Path $OutDir).Path
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"

function Invoke-BuildAndRun([string]$StageRoot) {
    $stage = Join-Path $StageRoot ("stage-" + $stamp + "-" + $PID)
    New-Item -ItemType Directory -Force -Path (Join-Path $stage "src") | Out-Null
    $targetDir = Join-Path $stage "target"

    Copy-Item (Join-Path $benchDir "src\perf_harness.rs") (Join-Path $stage "src\main.rs") -Force
    $template = Get-Content (Join-Path $benchDir "Cargo.toml.template") -Raw
    $coreForward = $corePath.Replace("\", "/")
    $template = $template.Replace("@CORE_PATH@", $coreForward)
    [System.IO.File]::WriteAllText((Join-Path $stage "Cargo.toml"), $template, $utf8NoBom)

    # environment snapshot
    $cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
    $os = Get-CimInstance Win32_OperatingSystem
    $rustc = (& rustc --version) 2>&1
    $cargo = (& cargo --version) 2>&1
    $envObj = [ordered]@{
        collected_at_utc    = (Get-Date).ToUniversalTime().ToString("o")
        collected_at_local  = (Get-Date).ToString("o")
        hostname            = $env:COMPUTERNAME
        os                  = ($os.Caption + " " + $os.Version)
        arch                = $env:PROCESSOR_ARCHITECTURE
        cpu_name            = $cpu.Name.Trim()
        cpu_physical_cores  = $cpu.NumberOfCores
        cpu_logical_processors = $cpu.NumberOfLogicalProcessors
        rustc               = ($rustc -join " ")
        cargo               = ($cargo -join " ")
        powershell          = $PSVersionTable.PSVersion.ToString()
        cargo_target_dir    = $targetDir
        stage_dir           = $stage
        offline             = (-not $NoOffline)
        board               = $false
    }
    $envPath = Join-Path $OutDir "environment.json"
    [System.IO.File]::WriteAllText($envPath, ($envObj | ConvertTo-Json -Depth 4), $utf8NoBom)

    $buildLog = Join-Path $OutDir "cargo-build.log"
    $runLog = Join-Path $OutDir "run.log"
    $toolchainLog = Join-Path $OutDir "toolchain.txt"
    @(
        "collected_at=$((Get-Date).ToString('o'))",
        "rustc=$($rustc -join ' ')",
        "cargo=$($cargo -join ' ')",
        "hostname=$env:COMPUTERNAME",
        "os=$($os.Caption) $($os.Version)",
        "cpu=$($cpu.Name.Trim())",
        "cores=$($cpu.NumberOfCores) logical=$($cpu.NumberOfLogicalProcessors)",
        "stage_dir=$stage",
        "cargo_target_dir=$targetDir",
        "rt_stack=$rtStack",
        "board=false (no board / ALSA / RT kernel / IRQ measurement in this run)"
    ) | Set-Content -Path $toolchainLog -Encoding utf8

    $env:CARGO_TARGET_DIR = $targetDir
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $offlineArgs = @()
        if (-not $NoOffline) { $offlineArgs += "--offline" }
        $buildArgs = @("build", "--release", "--manifest-path", (Join-Path $stage "Cargo.toml")) + $offlineArgs
        Write-Host ("cargo " + ($buildArgs -join " "))
        $buildOut = (& cargo @buildArgs 2>&1 | Out-String)
        $buildExit = $LASTEXITCODE
        [System.IO.File]::WriteAllText($buildLog, $buildOut, $utf8NoBom)
        Write-Host $buildOut
        if ($buildExit -ne 0 -and -not $NoOffline) {
            Write-Warning "offline build failed (exit $buildExit); retrying without --offline"
            $buildArgs = @("build", "--release", "--manifest-path", (Join-Path $stage "Cargo.toml"))
            $buildOut = (& cargo @buildArgs 2>&1 | Out-String)
            $buildExit = $LASTEXITCODE
            [System.IO.File]::WriteAllText(($buildLog + ".retry"), $buildOut, $utf8NoBom)
            Write-Host $buildOut
        }
        if ($buildExit -ne 0) {
            throw "cargo build failed (exit $buildExit); see $buildLog"
        }

        $exe = Join-Path $targetDir "release\khaen-perf-harness.exe"
        if (-not (Test-Path $exe)) {
            $exe = (Get-ChildItem (Join-Path $targetDir "release") -Filter "*.exe" -ErrorAction SilentlyContinue |
                    Where-Object { $_.Name -notmatch "^(cargo|build)" } |
                    Sort-Object Length -Descending | Select-Object -First 1).FullName
        }
        if (-not $exe) { throw "harness exe not found under $targetDir\release" }
        $runArgs = @(
            "--out", $OutDir,
            "--blocks", "$Blocks",
            "--warmup", "$Warmup",
            "--reps", "$Reps",
            "--env-json", $envPath,
            "--timestamp", (Get-Date).ToString("o")
        )
        if ($Quick) { $runArgs += "--quick" }
        Write-Host ("run: " + $exe + " " + ($runArgs -join " "))
        $runOut = (& $exe @runArgs 2>&1 | Out-String)
        $runExit = $LASTEXITCODE
        [System.IO.File]::WriteAllText($runLog, $runOut, $utf8NoBom)
        Write-Host $runOut
        Write-Host "run exit=$runExit"
        if ($runExit -ne 0) {
            throw "harness run failed (exit $runExit); see $runLog"
        }
        "stage=$stage" | Set-Content -Path (Join-Path $OutDir "stage-path.txt") -Encoding utf8
        Write-Host "OUT dir: $OutDir"
        Get-ChildItem $OutDir | Select-Object Name, Length | Format-Table -AutoSize | Out-String -Width 200
        Write-Host "OK (local x86 only; board performance gates remain NOT_RUN)"
        return 0
    }
    finally {
        $ErrorActionPreference = $prevEap
    }
}

# Prefer RAM disk (R:) for the throwaway build; fall back to C: LOCALAPPDATA if staging fails.
$stageRoots = @((Join-Path $env:TEMP "khaen-perf-harness"))
if ($env:TEMP -match '^[A-Za-z]:\\') {
    $stageRoots += (Join-Path $env:LOCALAPPDATA "Temp\khaen-perf-harness")
}
$lastError = $null
foreach ($root in $stageRoots) {
    try {
        $code = Invoke-BuildAndRun $root
        exit $code
    } catch {
        $lastError = $_
        Write-Warning ("staging under {0} failed: {1}" -f $root, $_.Exception.Message)
    }
}
throw "all staging attempts failed: $($lastError.Exception.Message)"
