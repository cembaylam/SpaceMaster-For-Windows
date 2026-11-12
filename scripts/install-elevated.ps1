# Installs (upgrades) the newest MSI in dist\ with a basic progress window. Run elevated.
$root = Split-Path -Parent $PSScriptRoot
$msi = Get-ChildItem (Join-Path $root "dist") -Filter "SpaceMaster-for-Windows-*-x64.msi" | Sort-Object LastWriteTime | Select-Object -Last 1
Get-Process spacemaster -ErrorAction SilentlyContinue | Stop-Process -Force
$log = Join-Path $root "target\tmp\msi-install.log"
$p = Start-Process msiexec -ArgumentList "/i `"$($msi.FullName)`" /qb /l*v `"$log`"" -Wait -PassThru
exit $p.ExitCode
