# Builds and packages a VR-ModLoader release:
#   dist\VR-ModLoader-<version>\      the files a player unzips into the game folder (next to nie.exe)
#   dist\VR-ModLoader-<version>.zip   the same folder's contents, zipped (forward-slash entries), + its SHA-256
#
#   powershell -ExecutionPolicy Bypass -File tools\release.ps1 [-Version x.y.z] [-NoBuild]
#
# <version> = the package version of crates\vr-loader (the ModLoader version embedded in winmm.dll as
# EVT_MODLOADER_VERSION=). Steps: build vr_loader.dll + the engine plugins (release), make the ModLoader payload
# (make_payload.ps1), build VR-ModLoader.exe with the payload embedded (VRML_LOADER_PAYLOAD), assemble, zip.
# Honours $env:CARGO_TARGET_DIR. -NoBuild only assembles from an earlier build.
param(
    [string]$Version = "",
    [switch]$NoBuild
)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $root

if ($Version -eq "") {
    $m = Select-String -Path "crates\vr-loader\Cargo.toml" -Pattern '^version = "([^"]+)"' | Select-Object -First 1
    if (-not $m) { throw "version not found in crates\vr-loader\Cargo.toml; pass -Version" }
    $Version = $m.Matches[0].Groups[1].Value
}
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
$rel = Join-Path $target "release"

# The framework engine mods shipped in mods\ (mod folder under mods\, plugin crate, DLL name).
$engines = @(
    @{ Mod = "audio_engine"; Pkg = "evt-plugin-audio-engine"; Dll = "audio_engine.dll" },
    @{ Mod = "text_engine"; Pkg = "evt-plugin-text-engine"; Dll = "text_engine.dll" },
    @{ Mod = "match_engine"; Pkg = "evt-plugin-match-engine"; Dll = "match_engine.dll" },
    @{ Mod = "save_engine"; Pkg = "evt-plugin-save-engine"; Dll = "save_engine.dll" }
)
# Generated / local files never shipped from a mod folder.
$skipNames = @("audio_index.json", "enabled.toml", "load_order.toml")
$skipDirs = @("cache", "target", "dist", "index")

function Invoke-Cargo([string[]]$CargoArgs) {
    & cargo @CargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo $($CargoArgs -join ' ') failed" }
}

$payload = Join-Path $target "vr-modloader-payload-$Version.zip"
if (-not $NoBuild) {
    $pkgs = @("-p", "vr-loader")
    foreach ($e in $engines) { $pkgs += @("-p", $e.Pkg) }
    Invoke-Cargo (@("build", "--release") + $pkgs)
    & (Join-Path $root "crates\vr-modloader-app\make_payload.ps1") -Dll (Join-Path $rel "vr_loader.dll") -Version $Version -Out $payload
    Set-Location $root
    $env:VRML_LOADER_PAYLOAD = $payload
    try {
        Invoke-Cargo @("build", "--release", "-p", "vr-modloader-app")
    } finally {
        Remove-Item Env:VRML_LOADER_PAYLOAD -ErrorAction SilentlyContinue
    }
}

# ---- assemble
$distRoot = Join-Path $root "dist"
$name = "VR-ModLoader-$Version"
$out = Join-Path $distRoot $name
$zip = Join-Path $distRoot "$name.zip"
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
if (Test-Path $zip) { Remove-Item -Force $zip }
New-Item -ItemType Directory -Force $out | Out-Null

function Copy-Need([string]$From, [string]$To) {
    if (-not (Test-Path $From)) { throw "missing $From (build first, or drop -NoBuild)" }
    New-Item -ItemType Directory -Force (Split-Path $To) | Out-Null
    Copy-Item $From $To -Force
}

# the ModLoader and the manager, in the game folder
Copy-Need (Join-Path $rel "vr_loader.dll") (Join-Path $out "winmm.dll")
Copy-Need (Join-Path $rel "VR-ModLoader.exe") (Join-Path $out "VR-ModLoader.exe")
$studio = Join-Path $rel "VR-ModLoader-Studio.exe"
if (Test-Path $studio) { Copy-Item $studio (Join-Path $out "VR-ModLoader-Studio.exe") }

# winmm.dll must carry this version (marker) and the manager the ModLoader payload (a zip with modloader.toml)
$dllText = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes((Join-Path $out "winmm.dll")))
$m = [regex]::Match($dllText, 'EVT_MODLOADER_VERSION=([0-9A-Za-z.+-]+)')
if (-not $m.Success -or $m.Groups[1].Value -ne $Version) { throw "winmm.dll does not carry EVT_MODLOADER_VERSION=$Version" }
$exeText = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes((Join-Path $out "VR-ModLoader.exe")))
# a zip local file header (PK 3 4 + 26 bytes) naming modloader.toml = the embedded payload
if (-not [regex]::IsMatch($exeText, 'PK\x03\x04.{26}modloader\.toml', [Text.RegularExpressions.RegexOptions]::Singleline)) {
    throw "VR-ModLoader.exe has no embedded ModLoader payload (build it with VRML_LOADER_PAYLOAD, i.e. without -NoBuild)"
}

# the engine mods (their folder minus generated files, plus the DLL)
foreach ($e in $engines) {
    $src = Join-Path $root ("mods\" + $e.Mod)
    $dst = Join-Path $out ("mods\" + $e.Mod)
    Get-ChildItem -Recurse -File $src | ForEach-Object {
        $r = $_.FullName.Substring($src.Length + 1)
        $parts = $r.Split('\')
        $dirs = if ($parts.Length -gt 1) { $parts[0..($parts.Length - 2)] } else { @() }
        $skip = ($skipNames -contains $_.Name) -or ($_.Extension -in @(".vri", ".pdb")) -or
            (@($dirs | Where-Object { $skipDirs -contains $_ }).Count -gt 0)
        if (-not $skip) { Copy-Need $_.FullName (Join-Path $dst $r) }
    }
    Copy-Need (Join-Path $rel $e.Dll) (Join-Path $dst $e.Dll)
}

# texts: README.txt (players), LICENSE, NOTICE (+ the OFL licences of the fonts built into VR-ModLoader.exe), CHANGELOG
$utf8 = New-Object System.Text.UTF8Encoding($false)
$readme = [IO.File]::ReadAllText((Join-Path $root "tools\release-README.txt")).Replace("{version}", $Version)
[IO.File]::WriteAllText((Join-Path $out "README.txt"), $readme.Replace("`r`n", "`n").Replace("`n", "`r`n"), $utf8)
Copy-Need (Join-Path $root "LICENSE") (Join-Path $out "LICENSE")
Copy-Need (Join-Path $root "CHANGELOG.md") (Join-Path $out "CHANGELOG.md")
$notice = [IO.File]::ReadAllText((Join-Path $root "NOTICE"))
$fonts = Join-Path $root "crates\vr-modloader-app\assets\fonts"
$notice += "`n`nFonts built into VR-ModLoader.exe (SIL Open Font License 1.1)`n--------------------------------------------------------------`n"
foreach ($f in Get-ChildItem (Join-Path $fonts "OFL-*.txt") | Sort-Object Name) {
    $notice += "`n==== " + $f.BaseName.Substring(4) + " ====`n`n" + [IO.File]::ReadAllText($f.FullName).Replace("`r`n", "`n")
}
[IO.File]::WriteAllText((Join-Path $out "NOTICE"), $notice.Replace("`r`n", "`n"), $utf8)

# ---- zip (entries relative to the game folder, '/' separators)
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$fs = [IO.File]::Open($zip, [IO.FileMode]::CreateNew)
try {
    $za = New-Object System.IO.Compression.ZipArchive($fs, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        Get-ChildItem -Recurse -File $out | Sort-Object FullName | ForEach-Object {
            $entry = $_.FullName.Substring($out.Length + 1).Replace('\', '/')
            [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($za, $_.FullName, $entry, [System.IO.Compression.CompressionLevel]::Optimal)
        }
    } finally { $za.Dispose() }
} finally { $fs.Dispose() }

$hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
Set-Content -Encoding ascii -Path "$zip.sha256" -Value "$hash  $name.zip"
$size = (Get-Item $zip).Length
Write-Host ""
Write-Host "Release $Version"
Get-ChildItem -Recurse -File $out | Sort-Object FullName | ForEach-Object {
    "{0,10}  {1}" -f $_.Length, $_.FullName.Substring($out.Length + 1)
}
Write-Host ("{0}  ({1:N0} bytes)" -f $zip, $size)
Write-Host "SHA-256  $hash"
