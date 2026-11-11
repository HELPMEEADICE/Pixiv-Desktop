# Smoke-captures a page of the running app so layout can be inspected without
# a human at the keyboard. Used during development to verify every page actually
# renders; kept because it makes visual regressions checkable.
#
# Usage: pwsh -File tools/capture.ps1 -Page settings -Out docs/screenshots/07-settings.png
param(
    [string]$Page    = "home",
    [string]$Illust  = "",
    [string]$Out     = "docs/screenshots/capture.png",
    [int]$WaitSec    = 7,
    [string]$Exe     = "target\debug\pixiv-desktop.exe"
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms, System.Drawing

$env:PIXIV_DESKTOP_START_PAGE = $Page
if ($Illust -ne "") { $env:PIXIV_DESKTOP_START_ILLUST = $Illust } else { Remove-Item Env:\PIXIV_DESKTOP_START_ILLUST -ErrorAction SilentlyContinue }

$proc = Start-Process -FilePath $Exe -PassThru
Start-Sleep -Seconds $WaitSec

if ($proc.HasExited) {
    Write-Host "process exited early with code $($proc.ExitCode)" -ForegroundColor Red
    exit 1
}

# Capture the app's own window with PrintWindow instead of a screen grab: a
# screen grab picks up whatever window happens to be on top, and background
# processes are not allowed to steal the foreground.
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Cap {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
}
"@

$proc.Refresh()
for ($i = 0; $i -lt 24 -and $proc.MainWindowHandle -eq [IntPtr]::Zero; $i++) {
    Start-Sleep -Milliseconds 250
    $proc.Refresh()
}
if ($proc.MainWindowHandle -eq [IntPtr]::Zero) {
    Write-Host "no main window handle" -ForegroundColor Red
    $proc | Stop-Process -Force
    exit 1
}
$hwnd = $proc.MainWindowHandle
[void][Cap]::ShowWindow($hwnd, 9)
Start-Sleep -Milliseconds 700
[void][Cap]::ShowWindow($hwnd, 5)   # SW_SHOW / restore without stealing focus

$rect = New-Object Cap+RECT
[void][Cap]::GetWindowRect($hwnd, [ref]$rect)
$w = $rect.Right - $rect.Left
$h = $rect.Bottom - $rect.Top
if ($w -le 0 -or $h -le 0) { Write-Host "bad window rect" -ForegroundColor Red; $proc | Stop-Process -Force; exit 1 }

$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
# 2 = PW_RENDERFULLCONTENT, required for composited / GPU-rendered windows.
$ok = [Cap]::PrintWindow($hwnd, $hdc, 2)
$g.ReleaseHdc($hdc)
$g.Dispose()

if (-not $ok) { Write-Host "PrintWindow failed" -ForegroundColor Red }

$dir = Split-Path -Parent $Out
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

$proc | Stop-Process -Force
Write-Host "captured $Page -> $Out ($w x $h)"
