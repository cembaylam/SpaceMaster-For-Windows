# Measures how many distinct video frames actually reach the screen per second.
# Usage: measure-fps.ps1 -Exe <spacemaster.exe> -Video <file> [-Legacy]
param([string]$Exe, [string]$Video, [switch]$Legacy, [switch]$Scan, [switch]$Full)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class M {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, int flags, IntPtr extra);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[void][M]::SetProcessDPIAware()
$d = Join-Path $env:TEMP "fps-test"; Remove-Item $d -Recurse -Force -ErrorAction SilentlyContinue; New-Item -ItemType Directory $d | Out-Null
Copy-Item $Video (Join-Path $d "clip.mp4")
if ($Scan) { Remove-Item Env:SM_NO_DRIVE_SCAN -ErrorAction SilentlyContinue } else { $env:SM_NO_DRIVE_SCAN = "1" }; $env:SM_START_PATH = $d; $env:SM_SETTINGS_FILE = "$d\s.ini"; $env:SM_THEME = "dark"
$env:SM_LIBVLC_DIR = "C:\Projeler\SpaceMaster for Windows\libvlc\win-x64"
if ($Legacy) { $env:SM_VIDEO_LEGACY = "1" } else { Remove-Item Env:SM_VIDEO_LEGACY -ErrorAction SilentlyContinue }
$p = Start-Process $Exe -PassThru
Start-Sleep 2
$h = $p.MainWindowHandle
[M]::keybd_event(0x12,0,0,[IntPtr]::Zero); [M]::keybd_event(0x12,0,2,[IntPtr]::Zero)
[void][M]::SetForegroundWindow($h); Start-Sleep -Milliseconds 400
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.SendKeys]::SendWait("{ESC}"); Start-Sleep -Milliseconds 150
[System.Windows.Forms.SendKeys]::SendWait(" ")
Start-Sleep -Seconds 1
if ($Full) { [System.Windows.Forms.SendKeys]::SendWait("f"); Start-Sleep -Seconds 2 }
$r = New-Object M+RECT; [void][M]::GetWindowRect($h, [ref]$r)
# the video area: middle of the window, below the title bar and above the seek bar
$x = $r.L + 300; $y = $r.T + 120; $w = 500; $hgt = 300
$bmp = New-Object System.Drawing.Bitmap $w, $hgt
$g = [System.Drawing.Graphics]::FromImage($bmp)
$sw = [Diagnostics.Stopwatch]::StartNew()
$prev = $null; $changes = 0; $caps = 0
while ($sw.Elapsed.TotalSeconds -lt 2.5) {
    $g.CopyFromScreen($x, $y, 0, 0, $bmp.Size); $caps++
    # cheap signature: sample a grid of pixels
    $sig = 0L
    for ($i = 0; $i -lt $w; $i += 25) { for ($j = 0; $j -lt $hgt; $j += 25) { $c = $bmp.GetPixel($i, $j); $sig = ($sig * 31 + $c.R + 3 * $c.G + 7 * $c.B) % 1000000007 } }
    if ($prev -ne $null -and $sig -ne $prev) { $changes++ }
    $prev = $sig
}
$secs = $sw.Elapsed.TotalSeconds
"{0}: {1} captures, {2} distinct frames in {3:N1}s = {4:N1} fps seen (capture rate {5:N0}/s)" -f ($(if ($Legacy) {"legacy"} else {"fixed "}), $caps, $changes, $secs, ($changes/$secs), ($caps/$secs))
Stop-Process -Id $p.Id -Force
Remove-Item $d -Recurse -Force -ErrorAction SilentlyContinue
