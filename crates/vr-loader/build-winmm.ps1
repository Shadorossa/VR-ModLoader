# Builds the loader (release, default host target) and copies it as target\release\winmm.dll, ready to be placed next
# to nie.exe. Honours $env:CARGO_TARGET_DIR.
# Usage (from anywhere):  powershell -ExecutionPolicy Bypass -File crates\vr-loader\build-winmm.ps1
#        -NoBuild  : only refresh the copy from vr_loader.dll (after a plain `cargo build -p vr-loader --release`)
param([switch]$NoBuild)
$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
Push-Location $root
try {
    if (-not $NoBuild) {
        cargo build -p vr-loader --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    }
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
    $src = Join-Path $target "release\vr_loader.dll"
    if (-not (Test-Path $src)) { throw "missing $src (run cargo build -p vr-loader --release)" }
    $dst = Join-Path $target "release\winmm.dll"
    Copy-Item $src $dst -Force
    Write-Host "OK: $dst (copy it next to nie.exe)"
} finally { Pop-Location }
