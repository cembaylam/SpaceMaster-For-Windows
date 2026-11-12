# Builds dist\SpaceMaster-for-Windows-<version>-x64.msi (WiX 5, installed as a project-local dotnet tool).
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value

# The installer's license page needs RTF: wrap the MIT text.
$lic = (Get-Content LICENSE -Raw) -replace '\\', '\\\\' -replace '\{', '\{' -replace '\}', '\}' -replace "`r?`n", "\par`n"
Set-Content installer\license.rtf ("{\rtf1\ansi\deff0{\fonttbl{\f0 Segoe UI;}}\f0\fs20 " + $lic + "}") -Encoding ASCII

dotnet tool restore | Out-Null
dotnet tool run wix extension add -g WixToolset.UI.wixext/5.0.2 | Out-Null
dotnet tool run wix extension add -g WixToolset.Util.wixext/5.0.2 | Out-Null
New-Item -ItemType Directory dist -Force | Out-Null
$msi = Join-Path $root "dist\SpaceMaster-for-Windows-$version-x64.msi"
dotnet tool run wix build installer\Package.wxs -arch x64 -d Version=$version `
    -ext WixToolset.UI.wixext -ext WixToolset.Util.wixext -o $msi
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }
Write-Host "wrote $msi ($([math]::Round((Get-Item $msi).Length / 1KB)) KB)"
