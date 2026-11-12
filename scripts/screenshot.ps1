# Captures the SpaceMaster window to a PNG: screenshot.ps1 -Out <png> [-Exe <exe>] [-Wait <s>] [-Keys <SendKeys string>]
# Keys are sent ONLY after the SpaceMaster window has been verified to be the foreground window.
param([string]$Out = "$env:TEMP\spacemaster.png", [string]$Exe = "", [int]$Wait = 6, [string]$Keys = "")
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class W {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, int flags, IntPtr extra);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[void][W]::SetProcessDPIAware()
if ($Exe) { $proc = Start-Process -FilePath $Exe -PassThru } else { $proc = Get-Process spacemaster | Select-Object -First 1 }
Start-Sleep -Seconds $Wait
$proc.Refresh()
$h = $proc.MainWindowHandle
if ($h -eq 0) { Write-Error "no window"; exit 1 }
[void][W]::ShowWindow($h, 9)
# Windows only lets the foreground process steal focus; a synthetic Alt press lifts that restriction.
[W]::keybd_event(0x12, 0, 0, [IntPtr]::Zero); [W]::keybd_event(0x12, 0, 2, [IntPtr]::Zero)
[void][W]::SetForegroundWindow($h)
Start-Sleep -Milliseconds 500
if ($Keys) {
    if ([W]::GetForegroundWindow() -ne $h) { Write-Error "SpaceMaster is not the foreground window; refusing to send keys"; exit 2 }
    # The Alt tap above leaves the window in system-menu mode, which would swallow the first key: leave it with Esc.
    [System.Windows.Forms.SendKeys]::SendWait("{ESC}")
    Start-Sleep -Milliseconds 150
    [System.Windows.Forms.SendKeys]::SendWait($Keys)
    Start-Sleep -Milliseconds 800
}
$r = New-Object W+RECT
[void][W]::GetWindowRect($h, [ref]$r)
$bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Host "saved $Out ($($bmp.Width)x$($bmp.Height)) pid=$($proc.Id) foreground=$([W]::GetForegroundWindow() -eq $h)"
