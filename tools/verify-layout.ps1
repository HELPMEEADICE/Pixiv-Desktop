# Layout verification matrix: for every page and window size, assert the shell's
# invariant (status bar bottom == shell bottom) and capture a ground-truth shot.
#
# Usage:
#   pwsh -File tools/verify-layout.ps1                 # full matrix
#   pwsh -File tools/verify-layout.ps1 -Pages settings,downloads -Sizes 1024x768
param(
    [string[]]$Pages = @("home", "discovery", "ranking", "search", "bookmarks", "history", "downloads", "settings"),
    [string[]]$Sizes = @("maximize", "1024x768", "800x600"),
    [string]$Exe     = "target\debug\pixiv-desktop.exe",
    [string]$OutDir  = "docs/screenshots/verify",
    [int]$WaitMs     = 4500
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

Add-Type @'
using System; using System.Runtime.InteropServices;
public class VL {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
}
'@
[void][VL]::SetProcessDPIAware()
$dpi = (New-Object System.Windows.Forms.Form).CreateGraphics().DpiX
if (-not $dpi -or $dpi -le 0) { $dpi = 96 }
$scale = $dpi / 96.0

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$env:PIXIV_DESKTOP_TRACE = "1"
$env:PIXIV_DESKTOP_DEMO = "1"
Remove-Item Env:\PIXIV_DESKTOP_SIZE -ErrorAction SilentlyContinue
Remove-Item Env:\PIXIV_DESKTOP_START_ILLUST -ErrorAction SilentlyContinue

$results = @()
# `pwsh -File` passes `a,b,c` as one token, so split explicitly.
$Pages = @($Pages | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$Sizes = @($Sizes | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
Write-Host "pages: $($Pages -join ', ')"
Write-Host "sizes: $($Sizes -join ', ')"
foreach ($size in $Sizes) {
    foreach ($page in $Pages) {
        $env:PIXIV_DESKTOP_START_PAGE = $page
        $log = [System.IO.Path]::GetTempFileName()
        $proc = Start-Process -FilePath $Exe -PassThru -RedirectStandardError $log
        try {
            Start-Sleep -Milliseconds $WaitMs
            $proc.Refresh()
            if ($proc.HasExited) { Write-Host "  $page/$size exited early" -ForegroundColor Red; continue }
            for ($i = 0; $i -lt 40 -and $proc.MainWindowHandle -eq [IntPtr]::Zero; $i++) {
                Start-Sleep -Milliseconds 250; $proc.Refresh()
            }
            $hwnd = $proc.MainWindowHandle
            if ($hwnd -eq [IntPtr]::Zero) { Write-Host "  $page/$size no window" -ForegroundColor Red; continue }

            [void][VL]::ShowWindow($hwnd, 9)      # restore first: a resize on a
            Start-Sleep -Milliseconds 600         # never-restored window is dropped
            $sz = "$size".Trim()
            if ($sz -eq "maximize") {
                [void][VL]::ShowWindow($hwnd, 3)
            } else {
                $parts = $sz.Split("x")
                $wantW = [int]([double]$parts[0] * $scale)
                $wantH = [int]([double]$parts[1] * $scale)
                $wr = New-Object VL+RECT; [void][VL]::GetWindowRect($hwnd, [ref]$wr)
                $cr = New-Object VL+RECT; [void][VL]::GetClientRect($hwnd, [ref]$cr)
                $fw = ($wr.Right - $wr.Left) - $cr.Right
                $fh = ($wr.Bottom - $wr.Top) - $cr.Bottom
                [void][VL]::SetWindowPos($hwnd, [IntPtr]::Zero, 20, 20, $wantW + $fw, $wantH + $fh, 0x0004)
            }
            [void][VL]::SetForegroundWindow($hwnd)
            Start-Sleep -Milliseconds 2200

            $line = (Get-Content $log | Select-String 'trace' | Select-Object -Last 1).Line
            $gapOk = $line -match 'gap=(-?\d+)'
            $gap = if ($gapOk) { [double]$Matches[1] } else { [double]::NaN }
            $wr = New-Object VL+RECT; [void][VL]::GetWindowRect($hwnd, [ref]$wr)
            $w = $wr.Right - $wr.Left; $h = $wr.Bottom - $wr.Top
            $shot = Join-Path $OutDir ("{0}-{1}.png" -f $page, $size)
            $bmp = New-Object System.Drawing.Bitmap $w, $h
            $g = [System.Drawing.Graphics]::FromImage($bmp)
            $g.CopyFromScreen($wr.Left, $wr.Top, 0, 0, (New-Object System.Drawing.Size $w, $h), [System.Drawing.CopyPixelOperation]::SourceCopy)
            $g.Dispose(); $bmp.Save($shot); $bmp.Dispose()

            $verdict = if ([math]::Abs($gap) -lt 1.0) { "OK" } else { "GAP" }
            $color = if ($verdict -eq "OK") { "Green" } else { "Red" }
            Write-Host ("  {0,-10} {1,-10} {2}  {3}" -f $page, $size, $verdict, $line) -ForegroundColor $color
            $results += [pscustomobject]@{ Page = $page; Size = $size; Gap = $gap; Verdict = $verdict; Line = $line }
        }
        finally {
            if ($proc -and -not $proc.HasExited) { $proc | Stop-Process -Force }
            Start-Sleep -Milliseconds 400
            Remove-Item $log -ErrorAction SilentlyContinue
        }
    }
}

$bad = $results | Where-Object { $_.Verdict -ne "OK" }
Write-Host ""
Write-Host ("{0} checks, {1} with a bottom gap" -f $results.Count, $bad.Count) -ForegroundColor $(if ($bad.Count) { "Red" } else { "Green" })
if ($bad.Count) { $bad | Format-Table -AutoSize | Out-String | Write-Host; exit 1 }
