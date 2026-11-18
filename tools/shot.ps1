# Ground-truth visual capture: what the user actually sees on screen.
#
# Prints the *window rect* screen region with BitBlt instead of PrintWindow.
# PrintWindow renders the app's internal surface, which can be a different size
# from the visible window and therefore lies about layout; a screen copy cannot.
#
# The capture is then cropped to the *client area* and a sidecar JSON records the
# scale factor and the client size, so downstream checks (tools/verify-icons.ps1,
# tools/pixel.ps1) can convert a pixel coordinate to a logical one without
# guessing at the frame width. A Win11 window draws a rounded 1px frame plus a
# transparent resize border, so an uncropped window-rect capture contains ~11px of
# desktop bleed-through on every side — it is not app content and must not be
# measured as such.
#
# Usage:
#   pwsh -File tools/shot.ps1 -Page home -Mode maximize -Out x.png
#   pwsh -File tools/shot.ps1 -Page settings -Mode size -Size 1024x768 -Dark -Out y.png
#   pwsh -File tools/shot.ps1 -Page home -WholeWindow   # keep the frame
param(
    [string]$Page  = "home",
    [string]$Mode  = "maximize",      # maximize | size | asis
    [string]$Size  = "1140x886",
    [string]$Out   = "docs/screenshots/shot.png",
    [string]$Exe   = "target\debug\pixiv-desktop.exe",
    [int]$WaitMs   = 0,                # optional extra startup time for a slow scenario
    [switch]$Dark,
    [switch]$Trace,
    [switch]$WholeWindow,
    # Opens a specific artwork on the detail page (demo ids start at 700000001).
    [string]$Illust = ""
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing, System.Windows.Forms

Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Shot {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
}
'@
[void][Shot]::SetProcessDPIAware()

# G59: the launch keys come from tools/page-set.ps1's recipe for the page, applied by
# Set-PageLaunch, which writes or clears every key the shell reads. This script used to
# set the page name, clear START_ILLUST and touch nothing else - so a leftover
# PIXIV_DESKTOP_START_NOVEL in the calling shell made `-Page home` photograph the reader,
# with a real capture under a name that described a different page.
. (Join-Path $PSScriptRoot "page-set.ps1") | Out-Null
if ((Get-PageLaunch -Page $Page).Count -gt 0) { Set-PageLaunch -Page $Page }
else {
    # A name the page set does not know keeps the old standalone behaviour - start there -
    # but the sweep happens first, so an ambient deep link cannot outlive it.
    Set-PageLaunch -Clear
    $env:PIXIV_DESKTOP_START_PAGE = $Page
}
# The one key a caller can still override by hand, applied after the recipe.
if ($Illust) { $env:PIXIV_DESKTOP_START_ILLUST = $Illust }
Remove-Item Env:\PIXIV_DESKTOP_SIZE -ErrorAction SilentlyContinue
if ($Trace) { $env:PIXIV_DESKTOP_TRACE = "1" } else { Remove-Item Env:\PIXIV_DESKTOP_TRACE -ErrorAction SilentlyContinue }
if ($Dark) { $env:PIXIV_DESKTOP_THEME = "dark" } else { Remove-Item Env:\PIXIV_DESKTOP_THEME -ErrorAction SilentlyContinue }

$log = [System.IO.Path]::GetTempFileName()
$proc = Start-Process -FilePath $Exe -PassThru -RedirectStandardError $log
try {
    if ($WaitMs -gt 0) { Start-Sleep -Milliseconds $WaitMs }
    # Winit may expose a hidden event-target window before the app window.
    $deadline = (Get-Date).AddSeconds(15)
    do {
        Start-Sleep -Milliseconds 250
        $proc.Refresh()
        if ($proc.HasExited) { Write-Host "exited early ($($proc.ExitCode))" -ForegroundColor Red; exit 1 }
        if ($proc.MainWindowHandle -ne [IntPtr]::Zero -and $proc.MainWindowTitle -eq "Pixiv Desktop") { break }
    } while ((Get-Date) -lt $deadline)
    $hwnd = $proc.MainWindowHandle
    if ($hwnd -eq [IntPtr]::Zero -or $proc.MainWindowTitle -ne "Pixiv Desktop") {
        Write-Host "no Pixiv Desktop window" -ForegroundColor Red
        exit 1
    }

    # Restoring first is what makes the platform deliver the resize: a resize
    # applied to a never-restored window is dropped by the backend.
    [void][Shot]::ShowWindow($hwnd, 9)      # SW_RESTORE
    Start-Sleep -Milliseconds 600

    if ($Mode -eq "maximize") {
        [void][Shot]::ShowWindow($hwnd, 3)  # SW_MAXIMIZE
    } elseif ($Mode -eq "size") {
        $dpi = (New-Object System.Windows.Forms.Form).CreateGraphics().DpiX
        if (-not $dpi -or $dpi -le 0) { $dpi = 96 }
        $scale = $dpi / 96.0
        $parts = $Size.Split("x")
        $wantW = [int]([double]$parts[0] * $scale)
        $wantH = [int]([double]$parts[1] * $scale)
        $wr = New-Object Shot+RECT; [void][Shot]::GetWindowRect($hwnd, [ref]$wr)
        $cr = New-Object Shot+RECT; [void][Shot]::GetClientRect($hwnd, [ref]$cr)
        $frameW = ($wr.Right - $wr.Left) - $cr.Right
        $frameH = ($wr.Bottom - $wr.Top) - $cr.Bottom
        [void][Shot]::SetWindowPos($hwnd, [IntPtr]::Zero, 20, 20, $wantW + $frameW, $wantH + $frameH, 0x0004)
    }
    [void][Shot]::SetForegroundWindow($hwnd)
    # The pointer is part of the capture. A pointer resting over the masonry puts a
    # #00000012 hover scrim on one card, which is a 18/255 difference on that card
    # and nothing anywhere else — measured, after a probe left the cursor over the
    # grid and two capture runs disagreed by exactly that. Park it in the corner
    # the window is not using before the frame is copied.
    [void][Shot]::SetCursorPos(4, 4)
    Start-Sleep -Milliseconds 2500

    $cr = New-Object Shot+RECT; [void][Shot]::GetClientRect($hwnd, [ref]$cr)
    $origin = New-Object Shot+POINT
    [void][Shot]::ClientToScreen($hwnd, [ref]$origin)
    $cw = $cr.Right - $cr.Left
    $ch = $cr.Bottom - $cr.Top
    if ($cw -le 0 -or $ch -le 0) { Write-Host "bad client rect" -ForegroundColor Red; exit 1 }

    $dpi = (New-Object System.Windows.Forms.Form).CreateGraphics().DpiX
    if (-not $dpi -or $dpi -le 0) { $dpi = 96 }
    $scale = $dpi / 96.0

    if ($WholeWindow) {
        $wr = New-Object Shot+RECT; [void][Shot]::GetWindowRect($hwnd, [ref]$wr)
        $sx = $wr.Left; $sy = $wr.Top
        $w = $wr.Right - $wr.Left; $h = $wr.Bottom - $wr.Top
        $ox = $origin.X - $wr.Left; $oy = $origin.Y - $wr.Top
    } else {
        $sx = $origin.X; $sy = $origin.Y; $w = $cw; $h = $ch; $ox = 0; $oy = 0
    }

    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($sx, $sy, 0, 0, (New-Object System.Drawing.Size $w, $h), [System.Drawing.CopyPixelOperation]::SourceCopy)
    $g.Dispose()
    $dir = Split-Path -Parent $Out
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()

    # Last complete trace line, so the layout harness does not have to re-implement
    # window capture just to read the shell's own measurements.
    $traceLine = $null
    if ($Trace) {
        $traceLine = (Get-Content $log | Select-String 'trace' | Where-Object { $_.Line -match 'gap=' } |
            Select-Object -Last 1).Line
        if (-not $traceLine) {
            $traceLine = (Get-Content $log | Select-String 'trace' | Select-Object -Last 1).Line
        }
    }

    # Sidecar: everything a pixel probe needs to reason in logical units.
    $meta = [ordered]@{
        trace       = $traceLine
        page        = $Page
        mode        = $Mode
        dark        = [bool]$Dark
        scale       = $scale
        width       = $w
        height      = $h
        logicalW    = [math]::Round($w / $scale, 2)
        logicalH    = [math]::Round($h / $scale, 2)
        clientX     = $ox
        clientY     = $oy
        # Virtual screen bounds: a capture region that extends past them comes back
        # with black bands, which is a host limitation rather than a layout problem.
        screenLeft  = [System.Windows.Forms.SystemInformation]::VirtualScreen.Left
        screenTop   = [System.Windows.Forms.SystemInformation]::VirtualScreen.Top
        screenW     = [System.Windows.Forms.SystemInformation]::VirtualScreen.Width
        screenH     = [System.Windows.Forms.SystemInformation]::VirtualScreen.Height
        capturedAt  = (Get-Date).ToString("o")
    }
    $metaPath = [System.IO.Path]::ChangeExtension($Out, ".json")
    ($meta | ConvertTo-Json -Compress) | Set-Content -Path $metaPath -Encoding utf8

    Write-Host "shot $Page mode=$Mode scale=$scale client=${cw}x${ch} -> $Out"
    if ($Trace) {
        Write-Host "--- app trace (last 2) ---"
        Get-Content $log | Select-String 'trace' | Select-Object -Last 2 | ForEach-Object { $_.Line }
    }
}
finally {
    if ($proc -and -not $proc.HasExited) { $proc | Stop-Process -Force }
    Start-Sleep -Milliseconds 300
    Remove-Item $log -ErrorAction SilentlyContinue
}
