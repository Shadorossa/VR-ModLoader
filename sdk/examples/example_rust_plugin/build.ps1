# Builds the plugin and assembles the installable mod folder: dist\example_rust_plugin\
#   powershell -ExecutionPolicy Bypass -File build.ps1
# Optional: $env:CARGO_TARGET_DIR to keep the build output elsewhere.
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $PSScriptRoot "target" }
$dll = Join-Path $target "release\example_rust_plugin.dll"
$out = Join-Path $PSScriptRoot "dist\example_rust_plugin"
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item (Join-Path $PSScriptRoot "mod\*") $out -Recurse -Force
Copy-Item $dll $out -Force
Write-Host "Mod folder ready: $out"
Write-Host "Copy it to <game>\mods\example_rust_plugin\ (the mod system, [modules] mods, is on by default)"
