# Copies the LibVLC engine (libvlc.dll, libvlccore.dll, plugins\) used by the video player into .\libvlc\win-x64
# (git-ignored; the installer bundles it). Source: a VLC 3.x "win-x64" libvlc folder, e.g. the one shipped by
# FileMaster / the VideoLAN.LibVLC.Windows NuGet package. Usage: fetch-libvlc.ps1 [-Source <dir>]
param([string]$Source = "")
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "libvlc\win-x64"
if (-not $Source) {
    $Source = $env:SM_LIBVLC_DIR
}
if (-not $Source) {
    $nuget = Get-ChildItem "$env:USERPROFILE\.nuget\packages\videolan.libvlc.windows" -Directory -ErrorAction SilentlyContinue | Sort-Object Name | Select-Object -Last 1
    if ($nuget) { $Source = Join-Path $nuget.FullName "build\x64" }
}
if (-not $Source -or -not (Test-Path (Join-Path $Source "libvlc.dll"))) {
    throw "Pass -Source <folder containing libvlc.dll, libvlccore.dll and plugins\>"
}
New-Item -ItemType Directory $dest -Force | Out-Null
Copy-Item (Join-Path $Source "*") $dest -Recurse -Force
Write-Host ("libvlc copied to {0} ({1:N0} MB)" -f $dest, ((Get-ChildItem $dest -Recurse -File | Measure-Object Length -Sum).Sum / 1MB))
