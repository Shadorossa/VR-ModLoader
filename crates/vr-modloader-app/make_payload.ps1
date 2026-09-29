# Builds the ModLoader payload zip that VR-ModLoader.exe installs (see sdk/README.md §2.1):
#   winmm.dll (the vr-loader DLL), steam_appid.txt, evt_loader\config.toml ([modules] mods = true), modloader.toml
# Then either put it next to the exe as modloader.zip, or build it into the exe:
#   $env:VRML_LOADER_PAYLOAD = (Resolve-Path target\vr-modloader-payload.zip); cargo build --release -p vr-modloader-app
param(
    [string]$Dll = "target\release\vr_loader.dll",
    [string]$Version = "",
    [string]$Out = "target\vr-modloader-payload.zip",
    [switch]$Pdb
)
$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
Set-Location $root
if (-not (Test-Path $Dll)) { throw "$Dll not found: build the loader first (cargo build --release -p vr-loader)" }
if ($Version -eq "") {
    # the ModLoader version = the package version of crates\vr-loader (embedded in the DLL as EVT_MODLOADER_VERSION=)
    $m = Select-String -Path "crates\vr-loader\Cargo.toml" -Pattern '^version = "([^"]+)"' | Select-Object -First 1
    if (-not $m) { throw "version not found in crates\vr-loader\Cargo.toml; pass -Version" }
    $Version = $m.Matches[0].Groups[1].Value
}
$stage = Join-Path $env:TEMP ("vrml_payload_" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force (Join-Path $stage "evt_loader") | Out-Null
Copy-Item $Dll (Join-Path $stage "winmm.dll")
if ($Pdb) {
    $p = [IO.Path]::ChangeExtension($Dll, ".pdb")
    if (Test-Path $p) { Copy-Item $p (Join-Path $stage "vr_loader.pdb") }
}
Set-Content -Encoding ascii (Join-Path $stage "steam_appid.txt") "2799860"
# Only the switch the public release needs; every other key takes the loader's default (serde defaults), and an
# update never overwrites the player's config.toml.
$utf8 = New-Object System.Text.UTF8Encoding($false)   # no BOM
[IO.File]::WriteAllText((Join-Path $stage "evt_loader\config.toml"), "# VR-ModLoader settings (missing keys use their defaults; see sdk/README.md)`r`n[modules]`r`nmods = true`r`n", $utf8)
[IO.File]::WriteAllText((Join-Path $stage "modloader.toml"), "version = `"$Version`"`r`n", $utf8)
if (Test-Path $Out) { Remove-Item $Out }
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $Out
Remove-Item -Recurse -Force $stage
"payload $Version -> $Out"
