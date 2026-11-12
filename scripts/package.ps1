# Builds the release exe and zips it: dist\SpaceMaster-<version>-win-x64.zip
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "build failed" }
$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$stage = Join-Path $root "dist\SpaceMaster-$version-win-x64"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $stage -Force | Out-Null
Copy-Item target\release\spacemaster.exe, README.md, LICENSE $stage
Copy-Item target\release\sm-bench.exe $stage
$zip = "$stage.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Write-Host "wrote $zip ($([math]::Round((Get-Item $zip).Length / 1KB)) KB)"
