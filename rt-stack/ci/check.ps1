#!/usr/bin/env pwsh
# TSPI RT stack offline check (WBS T06)
#
# Usage (from rt-stack/ or anywhere):
#   powershell -NoProfile -File ci/check.ps1
#   powershell -NoProfile -File ci/check.ps1 -Offline   # force cargo --offline
#   pwsh -File ci/check.ps1                             # PowerShell 5.1+/7+ compatible
#
# Checks: cargo fmt --all --check / cargo test --workspace / rt-check abi-layout /
#         rt-check forbid / rt-check drop-audit. Offline, no board actions.
# Exit: 0 = all pass; non-zero = a step failed (step name and exit code are printed).
# NOTE: keep this file ASCII-only so Windows PowerShell 5.1 parses it with any locale.

param(
    [switch]$Offline
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root

$cargoArgs = @()
if ($Offline) { $cargoArgs += '--offline' }

try {
    Write-Host "==> cargo fmt --all --check"
    & cargo fmt --all --check
    if ($LASTEXITCODE -ne 0) { throw "cargo fmt --all --check failed (exit $LASTEXITCODE)" }

    Write-Host "==> cargo test --workspace"
    & cargo test @cargoArgs --workspace
    if ($LASTEXITCODE -ne 0) { throw "cargo test --workspace failed (exit $LASTEXITCODE)" }

    Write-Host "==> rt-check abi-layout"
    & cargo run @cargoArgs --quiet -p khaen-rt-check --bin rt-check -- abi-layout
    if ($LASTEXITCODE -ne 0) { throw "rt-check abi-layout failed (exit $LASTEXITCODE)" }

    Write-Host "==> rt-check forbid"
    & cargo run @cargoArgs --quiet -p khaen-rt-check --bin rt-check -- forbid --root crates
    if ($LASTEXITCODE -ne 0) { throw "rt-check forbid failed (exit $LASTEXITCODE)" }

    Write-Host "==> rt-check drop-audit"
    & cargo run @cargoArgs --quiet -p khaen-rt-check --bin rt-check -- drop-audit --root crates
    if ($LASTEXITCODE -ne 0) { throw "rt-check drop-audit failed (exit $LASTEXITCODE)" }

    Write-Host "ALL CHECKS PASS"
    exit 0
}
catch {
    Write-Error "CHECK FAILED: $($_.Exception.Message)"
    exit 1
}
finally {
    Pop-Location
}
