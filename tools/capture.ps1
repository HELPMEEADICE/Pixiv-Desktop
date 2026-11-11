# Visual regression capture.
#
# Launches the app, resizes its window to an exact *client* size, waits for the
# layout to settle, then grabs the window with PrintWindow (PW_RENDERFULLCONTENT,
# which also works for GPU-composited windows).
#
# Why the resize is done from outside instead of via an app-side set_size: the
# window backend applies an early `set_size` by resizing the HWND without
# delivering a matching resize to the renderer, which paints a smaller surface and
# leaves the bottom of the window blank. Driving it with SetWindowPos is also what
# a real user does, so it exercises the production path.
#
# Usage:
#   pwsh -File tools/capture.ps1 -Page home -Out docs/screenshots/01-home.png
#   pwsh -File tools/capture.ps1 -Page settings -Size 1920x1080 -Out .../x.png
param(
    [string]$Page   = "home",
    [string]$Illust = "",
    [string]$Size   = "1140x886",
    [string]$Out    = "docs/screenshots/capture.png",
    [int]$WaitMs    = 5000,
    [string]$Exe    = "target\debug\pixiv-desktop.exe"
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms, System.Drawing

# DPI awareness: without it, GetWindowRect and GDI are virtualised and a
# 1710x1329 window at 150% comes back as a 1140x886 bitmap showing only the
# top-left two thirds of the real frame.
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Cap {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
}
'@
[void][Cap]::SetProcessDPIAware()

$env:PIXIV_DESKTOP_START_PAGE = $Page
Remove-Item Env:\PIXIV_DESKTOP_SIZE -ErrorAction SilentlyContinue
if ($Illust -ne "") {
    $env:PIXIV_DESKTOP_START_ILLUST = $Illust
} else {
    Remove-Item Env:\PIXIV_DESKTOP_START_ILLUST -ErrorAction SilentlyContinue
}

$proc = Start-Process -FilePath $Exe -PassThru
try {
    Start-Sleep -Milliseconds $WaitMs
    $proc.Refresh()
    if ($proc.HasExited) {
        Write-Host "process exited early with code $($proc.ExitCode)" -ForegroundColor Red
        exit 1
    }
    for ($i = 0; $i -lt 40 -and $proc.MainWindowHandle -eq [IntPtr]::Zero; $i++) {
        Start-Sleep -Milliseconds 250
        $proc.Refresh()
    }
    if ($proc.MainWindowHandle -eq [IntPtr]::Zero) {
        Write-Host "no main window handle" -ForegroundColor Red
        exit 1
    }
    $hwnd = $proc.MainWindowHandle
    [void][Cap]::ShowWindow($hwnd, 5)   # SW_SHOW / restore without stealing focus
    Start-Sleep -Milliseconds 400

    # --- resize the *client* area to the requested logical size ---
    $dpi = (New-Object System.Windows.Forms.Form).CreateGraphics().DpiX
    if (-not $dpi -or $dpi -le 0) { $dpi = 96 }
    $scale = $dpi / 96.0
    $parts = $Size.Split("x")
    $wantW = [int]([double]$parts[0] * $scale)
    $wantH = [int]([double]$parts[1] * $scale)

    $wr = New-Object Cap+RECT; [void][Cap]::GetWindowRect($hwnd, [ref]$wr)
    $cr = New-Object Cap+RECT; [void][Cap]::GetClientRect($hwnd, [ref]$cr)
    $frameW = ($wr.Right - $wr.Left) - $cr.Right
    $frameH = ($wr.Bottom - $wr.Top) - $cr.Bottom
    [void][Cap]::SetWindowPos($hwnd, [IntPtr]::Zero, 40, 40, $wantW + $frameW, $wantH + $frameH, 0x0004)
    Start-Sleep -Milliseconds 1800

    $wr = New-Object Cap+RECT; [void][Cap]::GetWindowRect($hwnd, [ref]$wr)
    $cr = New-Object Cap+RECT; [void][Cap]::GetClientRect($hwnd, [ref]$cr)
    $w = $wr.Right - $wr.Left
    $h = $wr.Bottom - $wr.Top
    if ($w -le 0 -or $h -le 0) { Write-Host "bad window rect" -ForegroundColor Red; exit 1 }

    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    $ok = [Cap]::PrintWindow($hwnd, $hdc, 2)   # PW_RENDERFULLCONTENT
    $g.ReleaseHdc($hdc)
    $g.Dispose()
    if (-not $ok) { Write-Host "PrintWindow failed" -ForegroundColor Yellow }

    $dir = Split-Path -Parent $Out
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Host "captured $Page @ $Size logical (client $($cr.Right)x$($cr.Bottom) px, dpi $dpi) -> $Out"
}
finally {
    if ($proc -and -not $proc.HasExited) { $proc | Stop-Process -Force }
}
