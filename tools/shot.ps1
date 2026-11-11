# Ground-truth visual capture: what the user actually sees on screen.
#
# Prints the *window rect* screen region with BitBlt instead of PrintWindow.
# PrintWindow renders the app's internal surface, which can be a different size
# from the visible window and therefore lies about layout; a screen copy cannot.
#
# Usage:
#   pwsh -File tools/shot.ps1 -Page home -Mode maximize -Out x.png
#   pwsh -File tools/shot.ps1 -Page settings -Mode size -Size 1000x600 -Out y.png
param(
    [string]$Page  = "home",
    [string]$Mode  = "maximize",      # maximize | size | asis
    [string]$Size  = "1140x886",
    [string]$Out   = "docs/screenshots/shot.png",
    [string]$Exe   = "target\debug\pixiv-desktop.exe",
    [int]$WaitMs   = 4500,
    [switch]$Dark,
    [switch]$Trace
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing, System.Windows.Forms

Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Shot {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
}
'@
[void][Shot]::SetProcessDPIAware()

$env:PIXIV_DESKTOP_START_PAGE = $Page
Remove-Item Env:\PIXIV_DESKTOP_START_ILLUST -ErrorAction SilentlyContinue
Remove-Item Env:\PIXIV_DESKTOP_SIZE -ErrorAction SilentlyContinue
if ($Trace) { $env:PIXIV_DESKTOP_TRACE = "1" } else { Remove-Item Env:\PIXIV_DESKTOP_TRACE -ErrorAction SilentlyContinue }
if ($Dark) { $env:PIXIV_DESKTOP_DARK = "1" } else { Remove-Item Env:\PIXIV_DESKTOP_DARK -ErrorAction SilentlyContinue }

$log = [System.IO.Path]::GetTempFileName()
$proc = Start-Process -FilePath $Exe -PassThru -RedirectStandardError $log
try {
    Start-Sleep -Milliseconds $WaitMs
    $proc.Refresh()
    if ($proc.HasExited) { Write-Host "exited early ($($proc.ExitCode))" -ForegroundColor Red; exit 1 }
    for ($i = 0; $i -lt 40 -and $proc.MainWindowHandle -eq [IntPtr]::Zero; $i++) {
        Start-Sleep -Milliseconds 250; $proc.Refresh()
    }
    $hwnd = $proc.MainWindowHandle
    if ($hwnd -eq [IntPtr]::Zero) { Write-Host "no window" -ForegroundColor Red; exit 1 }

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
    Start-Sleep -Milliseconds 2500

    $wr = New-Object Shot+RECT; [void][Shot]::GetWindowRect($hwnd, [ref]$wr)
    $cr = New-Object Shot+RECT; [void][Shot]::GetClientRect($hwnd, [ref]$cr)
    $w = $wr.Right - $wr.Left; $h = $wr.Bottom - $wr.Top
    if ($w -le 0 -or $h -le 0) { Write-Host "bad rect" -ForegroundColor Red; exit 1 }

    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($wr.Left, $wr.Top, 0, 0, (New-Object System.Drawing.Size $w, $h), [System.Drawing.CopyPixelOperation]::SourceCopy)
    $g.Dispose()
    $dir = Split-Path -Parent $Out
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Host "shot $Page mode=$Mode rect=${w}x${h} client=$($cr.Right)x$($cr.Bottom) dpi-aware -> $Out"
    if ($Trace) {
        Write-Host "--- app trace (last 4) ---"
        Get-Content $log | Select-String 'trace' | Select-Object -Last 4 | ForEach-Object { $_.Line }
    }
}
finally {
    if ($proc -and -not $proc.HasExited) { $proc | Stop-Process -Force }
    Start-Sleep -Milliseconds 300
    Remove-Item $log -ErrorAction SilentlyContinue
}
