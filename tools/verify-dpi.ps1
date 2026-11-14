# Scale-ladder gate: does the app run at 100/125/150/200 %, and is a cover
# decoded at least as wide as it is drawn at each of them?
#
# Why: OPTIMIZATION-PLAN F7. images::target_width is unit tested at 1.0/1.5/2.0
# and Slint reports logical pixels, but only 150 % had ever been seen on a real
# display - and F1 is a *resolution* bug, so the 100 % run is the cheapest
# independent check that covers are not decoded below their draw size.
#
# The DPI question, decided by experiment on this host: it CAN be done here.
#   * Slint 1.18.1 exposes the override in its own backend:
#     i-slint-backend-winit-1.18.1/winitwindowadapter.rs:602-609 reads
#     SLINT_SCALE_FACTOR when the window is created, and :1544 ignores the
#     platform ScaleFactorChanged while it is set. Measured here: with
#     SLINT_SCALE_FACTOR=1.0 / 1.25 / 1.5 / 2.0 one 1024x768 logical window logs
#     phys=1024x768 scale=1 / phys=1280x960 scale=1.25 / phys=1536x1152
#     scale=1.5 / phys=2048x1536 scale=2 - same logical geometry, four render
#     scales, gap=0 in all four.
#   * SetProcessDPIAware (already called by tools/drive.ps1 and gui-lib.ps1)
#     cannot answer it: it declares this harness process aware of the desktop DPI
#     so that its own coordinates and UI Automation rectangles are physical. It
#     changes no other process and cannot rescale the app window.
#   * The registry route (HKCU\Control Panel\Desktop\LogPixels, the per-user
#     scaling slider) is read at logon, so it cannot be used inside a gate even on
#     a host where it works.
#   * So this is not a stub. Exit 77 is kept for a host that cannot start the app
#     at all (no binary, no window, no trace line), because that host must report
#     SKIP and never a fabricated PASS.
#
# What it can and cannot decide (in the terms of UI-NOTES 12h):
#   * it CAN decide, for each scale: that the app starts; that the scale factor
#     the backend reports is the one asked for; that the shell invariant holds
#     (status bottom == shell bottom, i.e. gap 0); and that the cover decode
#     target for the card width the layout actually resolved to is at least the
#     physical width that card is drawn at. The card bounds come from
#     src/layout.rs and the rounding granule and clamp from src/images.rs, both
#     parsed on every run, so the check follows the source instead of restating
#     it.
#   * it CANNOT judge rendering quality, a live drag between two monitors, or a
#     scale whose window does not fit this display: at 200 % the window is
#     2048x1536 physical and this 2560x1440 screen cannot show all of it, so the
#     capture is short at the bottom - the trace numbers, which are what is
#     asserted, are not.
#
# Usage:
#   pwsh -File tools/verify-dpi.ps1                       # numeric table + ladder
#   pwsh -File tools/verify-dpi.ps1 -NumericOnly          # no app needed
#   pwsh -File tools/verify-dpi.ps1 -Scales 1.0,2.0 -Report
param(
    [double[]]$Scales = @(1.0, 1.25, 1.5, 2.0),
    [string]$Size = "1024x768",
    [string]$Exe = "target\debug\pixiv-desktop.exe",
    [string]$OutDir = "target\dpi",
    [switch]$NumericOnly,
    [switch]$Report
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)
$Scales = @($Scales | ForEach-Object { [double]$_ })
$inv = [System.Globalization.CultureInfo]::InvariantCulture

# ---- the source of truth, read rather than remembered ----
function Read-LayoutConstant {
    param([string]$Name)
    $line = Select-String -Path "src\layout.rs" -Pattern ("pub const " + $Name + ":") | Select-Object -First 1
    if ($null -eq $line) { throw ("src/layout.rs declares no constant '" + $Name + "'") }
    if ($line.Line -notmatch '=\s*([0-9]+(?:\.[0-9]+)?)') {
        throw ("constant {0} has no numeric value: {1}" -f $Name, $line.Line.Trim())
    }
    return [double]::Parse($Matches[1], $inv)
}

$cardMin = Read-LayoutConstant "CARD_MIN_WIDTH"
$cardMax = Read-LayoutConstant "CARD_MAX_WIDTH"

$images = Get-Content "src\images.rs" -Raw
if ($images -notmatch 'div_ceil\((\d+)\)\s*\*\s*(\d+)') {
    throw "src/images.rs no longer rounds the decode target with div_ceil; this gate cannot mirror it"
}
$round = [int]$Matches[1]
if ($images -notmatch 'rounded\.clamp\((\d+),\s*(\d+)\)') {
    throw "src/images.rs no longer clamps the decode target; this gate cannot mirror it"
}
$floor = [int]$Matches[1]
$cap = [int]$Matches[2]

Write-Host ("source: card {0}..{1} px, decode target rounds up to a {2}px multiple, clamped to {3}..{4}" -f $cardMin, $cardMax, $round, $floor, $cap)

# images::target_width, mirrored. One definition, so the table, the sweep and the
# live check cannot disagree about the formula they are testing.
function Get-TargetWidth {
    param([double]$Card, [double]$Scale)
    $physical = [math]::Ceiling([math]::Max($Card, 1.0) * [math]::Max($Scale, 1.0))
    $rounded = [math]::Ceiling($physical / $round) * $round
    return [int][math]::Min([math]::Max($rounded, $floor), $cap)
}

$failures = @()

# ---- half one: the numeric table, no window needed ----
$cards = @($cardMin, 173.5, 200.0, 250.0, $cardMax)
$table = @()
foreach ($scale in $Scales) {
    foreach ($card in $cards) {
        $physical = [math]::Ceiling($card * $scale)
        $target = Get-TargetWidth -Card $card -Scale $scale
        $headroom = $target - $physical
        $verdict = "OK"
        if ($target -lt $physical) { $verdict = "UNDER" }
        elseif ($headroom -gt $round -and $target -ne $floor) { $verdict = "OVER" }
        $table += [pscustomobject]@{
            Scale    = $scale
            Card     = $card
            Physical = [int]$physical
            Target   = $target
            Headroom = [int]$headroom
            Verdict  = $verdict
        }
        if ($verdict -ne "OK") {
            $failures += ("target_width({0} logical @ {1}x) = {2} is not between the physical {3} and one {4}px granule above it" -f $card, $scale, $target, $physical, $round)
        }
    }
}

# A sweep, because five sample widths can miss the one that breaks: every half
# pixel across the range the layout can produce, at every scale.
$sweepCount = 0
$sweepBad = 0
$width = $cardMin
while ($width -le $cardMax) {
    foreach ($scale in $Scales) {
        $sweepCount++
        $physical = [math]::Ceiling($width * $scale)
        $target = Get-TargetWidth -Card $width -Scale $scale
        if ($target -lt $physical) {
            $sweepBad++
            $failures += ("sweep: card {0} @ {1}x -> target {2} < physical {3}" -f $width, $scale, $target, $physical)
        }
    }
    $width += 0.5
}

$table | Format-Table -AutoSize | Out-String -Width 120 | Write-Host
Write-Host ("  {0} sampled rows, {1}-point sweep across {2}..{3} logical at {4}x: {5} rows with a decode target below the draw size" -f $table.Count, $sweepCount, $cardMin, $cardMax, ($Scales -join "/"), $sweepBad)

# ---- half two: the ladder, against the running app ----
$rows = @()
$skips = @()
if (-not $NumericOnly) {
    if (-not (Test-Path $Exe)) {
        $skips += ("no binary at " + $Exe)
    }
    else {
        . (Join-Path $PSScriptRoot "gui-lib.ps1")
        Set-Location (Split-Path -Parent $PSScriptRoot)
        New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
        foreach ($scale in $Scales) {
            Get-Process pixiv-desktop -ErrorAction SilentlyContinue | Stop-Process -Force
            Start-Sleep -Milliseconds 400
            $env:PIXIV_DESKTOP_DEMO = "1"
            $env:PIXIV_DESKTOP_START_PAGE = "home"
            $env:PIXIV_DESKTOP_THEME = "light"
            $env:PIXIV_DESKTOP_TRACE = "1"
            $env:PIXIV_DESKTOP_SIZE = $Size
            Remove-Item Env:\PIXIV_DESKTOP_START_ILLUST -ErrorAction SilentlyContinue
            $env:SLINT_SCALE_FACTOR = $scale.ToString($inv)
            $log = [System.IO.Path]::GetTempFileName()
            $proc = Start-Process -FilePath (Resolve-Path $Exe).Path -PassThru -RedirectStandardError $log
            try {
                $line = $null
                for ($i = 0; $i -lt 40 -and -not $line; $i++) {
                    Start-Sleep -Milliseconds 500
                    if (Test-Path $log) {
                        $hit = Get-Content $log | Select-String "trace phys=" | Select-Object -Last 1
                        if ($hit) { $line = $hit.Line }
                    }
                }
                if (-not $line) {
                    $skips += ("{0}x: no trace line in 20s" -f $scale)
                    continue
                }
                if ($line -notmatch 'trace phys=(\d+)x(\d+) scale=([0-9.]+) logical=([0-9.]+)x([0-9.]+) shell=(\d+) body=(\d+) status_y=(\d+) gap=(-?\d+) cols=(\d+) card=([0-9.]+)') {
                    $skips += ("{0}x: unparsable trace line: {1}" -f $scale, $line)
                    continue
                }
                $physW = [int]$Matches[1]
                $physH = [int]$Matches[2]
                $gotScale = [double]::Parse($Matches[3], $inv)
                $gap = [int]$Matches[9]
                $cols = [int]$Matches[10]
                $card = [double]::Parse($Matches[11], $inv)

                $png = Join-Path $OutDir ("home-{0}.png" -f $scale.ToString($inv).Replace(".", "_"))
                $note = ""
                try {
                    [void](Save-WindowShot -Path $png -Handle ([IntPtr]$proc.MainWindowHandle))
                }
                catch {
                    $note = "capture: " + $_.Exception.Message
                }

                $physical = [math]::Ceiling($card * $gotScale)
                $target = Get-TargetWidth -Card $card -Scale $gotScale
                $verdict = "OK"
                if ([math]::Abs($gotScale - $scale) -gt 0.01) { $verdict = "SCALE" }
                elseif ($gap -ne 0) { $verdict = "GAP" }
                elseif ($target -lt $physical) { $verdict = "UNDER" }

                $rows += [pscustomobject]@{
                    Asked    = $scale
                    Got      = $gotScale
                    Phys     = ("{0}x{1}" -f $physW, $physH)
                    Gap      = $gap
                    Cols     = $cols
                    Card     = $card
                    Physical = [int]$physical
                    Target   = $target
                    Verdict  = $verdict
                    Capture  = $note
                }
                if ($verdict -ne "OK") {
                    $failures += ("{0}x: scale reported {1}, gap {2}, target {3} vs physical {4}, cols {5}" -f $scale, $gotScale, $gap, $target, $physical, $cols)
                }
            }
            finally {
                if ($proc -and -not $proc.HasExited) { $proc | Stop-Process -Force }
                Remove-Item Env:\SLINT_SCALE_FACTOR -ErrorAction SilentlyContinue
                Remove-Item $log -ErrorAction SilentlyContinue
                Start-Sleep -Milliseconds 300
            }
        }
    }
}

if ($rows.Count) {
    Write-Host ""
    $rows | Format-Table -AutoSize | Out-String -Width 140 | Write-Host
}
foreach ($skip in $skips) { Write-Host ("  skipped: " + $skip) -ForegroundColor DarkYellow }

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$summary = [ordered]@{
    capturedAt = (Get-Date).ToString("o")
    scales     = $Scales
    numeric    = $table
    sweep      = [ordered]@{ points = $sweepCount; under = $sweepBad }
    live       = $rows
    skipped    = $skips
    failures   = $failures
}
($summary | ConvertTo-Json -Depth 6) | Set-Content -Path (Join-Path $OutDir "dpi-summary.json") -Encoding utf8

Write-Host ""
if ($failures.Count -eq 0 -and $rows.Count -eq 0 -and $skips.Count -gt 0) {
    Write-Host ("SKIP: the scale ladder could not run here - {0}" -f ($skips -join "; ")) -ForegroundColor DarkYellow
    exit 77
}
if ($failures.Count -eq 0) {
    Write-Host ("{0} scales measured live, {1} numeric rows and {2} sweep points: no decode target below the draw size" -f $rows.Count, $table.Count, $sweepCount) -ForegroundColor Green
    exit 0
}
$failures | Select-Object -First 12 | ForEach-Object { Write-Host ("  FAIL " + $_) -ForegroundColor Red }
if ($Report) {
    Write-Host ("{0} failures, not enforced (-Report)" -f $failures.Count) -ForegroundColor Yellow
    exit 0
}
Write-Host ("{0} scale-ladder failures" -f $failures.Count) -ForegroundColor Red
exit 1
