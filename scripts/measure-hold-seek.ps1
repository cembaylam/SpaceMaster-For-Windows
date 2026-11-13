# Holds the Right arrow on a playing video and reports picture changes on screen and where playback ended up.
# Usage: measure-hold-seek.ps1 -Exe <spacemaster.exe> -Video <file> [-HoldSeconds 1.5] [-Out <png>]
param([string]$Exe, [string]$Video, [double]$HoldSeconds = 1.5, [string]$Out = "")
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class H {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, int flags, IntPtr extra);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[void][H]::SetProcessDPIAware()
$d = Join-Path $env:TEMP "hold-test"; Remove-Item $d -Recurse -Force -ErrorAction SilentlyContinue; New-Item -ItemType Directory $d | Out-Null
Copy-Item $Video (Join-Path $d "clip.mp4")
$env:SM_NO_DRIVE_SCAN = "1"; $env:SM_START_PATH = $d; $env:SM_SETTINGS_FILE = "$d\s.ini"; $env:SM_THEME = "dark"
$env:SM_LIBVLC_DIR = "C:\Projeler\SpaceMaster for Windows\libvlc\win-x64"
$p = Start-Process $Exe -PassThru
Start-Sleep 2
$h = $p.MainWindowHandle
[H]::keybd_event(0x12,0,0,[IntPtr]::Zero); [H]::keybd_event(0x12,0,2,[IntPtr]::Zero)
[void][H]::SetForegroundWindow($h); Start-Sleep -Milliseconds 400
[System.Windows.Forms.SendKeys]::SendWait("{ESC}"); Start-Sleep -Milliseconds 150
[System.Windows.Forms.SendKeys]::SendWait(" "); Start-Sleep -Seconds 2
$r = New-Object H+RECT; [void][H]::GetWindowRect($h, [ref]$r)
$x = $r.L + 300; $y = $r.T + 120; $w = 500; $hgt = 300
$bmp = New-Object System.Drawing.Bitmap $w, $hgt
$g = [System.Drawing.Graphics]::FromImage($bmp)
function Sig($b) { $s = 0L; for ($i = 0; $i -lt $w; $i += 25) { for ($j = 0; $j -lt $hgt; $j += 25) { $c = $b.GetPixel($i, $j); $s = ($s * 31 + $c.R + 3 * $c.G + 7 * $c.B) % 1000000007 } }; $s }
[H]::keybd_event(0x27, 0, 1, [IntPtr]::Zero)            # Right down (extended key)
$sw = [Diagnostics.Stopwatch]::StartNew(); $prev = $null; $changes = 0; $caps = 0
while ($sw.Elapsed.TotalSeconds -lt $HoldSeconds) {
    $g.CopyFromScreen($x, $y, 0, 0, $bmp.Size); $caps++
    $sig = Sig $bmp
    if ($prev -ne $null -and $sig -ne $prev) { $changes++ }
    $prev = $sig
}
[H]::keybd_event(0x27, 0, 3, [IntPtr]::Zero)            # Right up
Start-Sleep -Milliseconds 700
"held {0:N1}s: {1} captures, {2} picture changes ({3:N0}/s) while scrubbing" -f $sw.Elapsed.TotalSeconds, $caps, $changes, ($changes / $HoldSeconds)
if ($Out) {
    [void][H]::GetWindowRect($h, [ref]$r)
    $shot = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
    $gg = [System.Drawing.Graphics]::FromImage($shot); $gg.CopyFromScreen($r.L, $r.T, 0, 0, $shot.Size); $shot.Save($Out)
}
Stop-Process -Id $p.Id -Force
Remove-Item $d -Recurse -Force -ErrorAction SilentlyContinue
