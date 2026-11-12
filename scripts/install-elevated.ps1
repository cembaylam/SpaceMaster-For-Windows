# Installs the newest MSI in dist\ with a basic progress window. Run elevated.
# An installed copy with the same version number cannot be upgraded in place, so it is uninstalled first.
$root = Split-Path -Parent $PSScriptRoot
$msi = Get-ChildItem (Join-Path $root "dist") -Filter "SpaceMaster-for-Windows-*-x64.msi" | Sort-Object LastWriteTime | Select-Object -Last 1
Get-Process spacemaster -ErrorAction SilentlyContinue | Stop-Process -Force
$old = Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*" -ErrorAction SilentlyContinue |
    Where-Object { $_.DisplayName -eq "SpaceMaster for Windows" }
foreach ($o in $old) {
    if ($o.PSChildName -match '^\{.*\}$') {
        $u = Start-Process msiexec -ArgumentList "/x $($o.PSChildName) /qb" -Wait -PassThru
        Write-Host "uninstall $($o.PSChildName): exit $($u.ExitCode)"
    }
}
$log = Join-Path $root "target\tmp\msi-install.log"
$p = Start-Process msiexec -ArgumentList "/i `"$($msi.FullName)`" /qb /l*v `"$log`"" -Wait -PassThru
exit $p.ExitCode
