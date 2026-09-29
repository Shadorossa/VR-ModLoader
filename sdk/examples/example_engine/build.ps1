# Builds the engine and assembles the installable mod folders: dist\example_engine\ and dist\example_engine_data\
#   powershell -ExecutionPolicy Bypass -File build.ps1
# Optional: $env:CARGO_TARGET_DIR to keep the build output elsewhere.
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $PSScriptRoot "target" }
$dll = Join-Path $target "release\example_engine.dll"
$out = Join-Path $PSScriptRoot "dist\example_engine"
$data = Join-Path $PSScriptRoot "dist\example_engine_data"
New-Item -ItemType Directory -Force $out, $data | Out-Null
Copy-Item (Join-Path $PSScriptRoot "mod\*") $out -Recurse -Force
Copy-Item $dll $out -Force
Copy-Item (Join-Path $PSScriptRoot "example_data_mod\*") $data -Recurse -Force
Write-Host "Mod folders ready: $out and $data"
Write-Host "Copy them to <game>\mods\ (example_engine\ and example_engine_data\)"
