# Icon geometry regression test.
#
# Everything here is measured from a real capture, not from the Slint source. The
# bug this exists to catch is invisible in the source: a `FluentGlyph` given
# `width: 20px; height: 20px` inside a `HorizontalLayout` is aligned to the *start*
# of the cross axis, so it renders ~10 logical px above its label — the declarations
# look perfectly reasonable.
#
# Method: take a client-area capture (tools/shot.ps1), then find the ink runs
# (contiguous rows containing a pixel that differs from the surface behind it)
# inside a narrow column. Each run is one glyph or one text label; comparing run
# centres compares what the user actually sees.
#
# Usage:
#   pwsh -File tools/verify-icons.ps1
#   pwsh -File tools/verify-icons.ps1 -Size 1024x768 -Dark
param(
    [string]$Size   = "1280x800",
    [switch]$Dark,
    [string]$Exe    = "target\debug\pixiv-desktop.exe",
    [string]$OutDir = "docs/screenshots/verify",
    # Physical pixels. A glyph's centre may sit a pixel off its row's centre
    # because Fluent's 24px grid leaves an asymmetric optical inset, and a text
    # label's ink box is not symmetric about its own centre (descenders). Anything
    # beyond this is a positioning bug rather than an optical one.
    [double]$Tolerance = 2.5
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "pixel.ps1")

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$suffix = if ($Dark) { "-dark" } else { "" }
$shot = Join-Path $OutDir ("icons-{0}{1}.png" -f $Size, $suffix)
$shotArgs = @("-File", (Join-Path $PSScriptRoot "shot.ps1"), "-Page", "home", "-Mode", "size",
    "-Size", $Size, "-Out", $shot, "-Exe", $Exe)
if ($Dark) { $shotArgs += "-Dark" }
& pwsh @shotArgs | Out-Null

$metaPath = [System.IO.Path]::ChangeExtension($shot, ".json")
if (-not (Test-Path $metaPath)) { throw "shot.ps1 did not write $metaPath" }
$meta = Get-Content $metaPath -Raw | ConvertFrom-Json
$scale = [double]$meta.scale

# A requested size that the host cannot actually display is not a layout failure.
# At 150% DPI a 1920x1080 logical window needs 2880x1620 physical pixels, which is
# wider than this machine's screen, so the window manager clamps it and the capture
# is missing its bottom edge. Report the clamp and skip rather than measure from a
# truncated frame.
$want = $Size.Split('x')
$wantW = [double]$want[0]
$wantH = [double]$want[1]
$offScreen = ([double]$meta.width -gt [double]$meta.screenW) -or ([double]$meta.height -gt [double]$meta.screenH)
if ($offScreen -or [math]::Abs([double]$meta.logicalW - $wantW) -gt 4 -or [math]::Abs([double]$meta.logicalH - $wantH) -gt 4) {
    if ($offScreen) {
        $msg = "SKIP " + $Size + ": the client area needs " + $meta.width + "x" + $meta.height + " physical px at " + $scale + "x DPI, "
        $msg += "but the screen is only " + $meta.screenW + "x" + $meta.screenH + "."
    } else {
        $msg = "SKIP " + $Size + ": requested " + $wantW + "x" + $wantH + " logical, host delivered " + $meta.logicalW + "x" + $meta.logicalH + "."
    }
    Write-Host $msg -ForegroundColor Yellow
    Write-Host "      Not measured: icon geometry read from a truncated frame would prove nothing."
    exit 0
}
$map = Get-PixelMap $shot

function ToLogical([double]$v) { return [math]::Round($v / $scale, 2) }

$failures = New-Object System.Collections.Generic.List[string]
$report = New-Object System.Collections.Generic.List[string]

# ---------------------------------------------------------------------------
# 1. Navigation rail: glyph vs label centre, and one shared glyph column.
# ---------------------------------------------------------------------------
# Rail metrics, mirroring theme.slint / NavItem:
#   topbar 48 + rail padding-top 8, item height 36, item spacing 2,
#   expanded pane: item inset 12, glyph 16, icon-to-label gap 12;
#   compact pane:  52-wide rail, glyph centred in the item.
# Runs shorter than -MinRun are the hairline Divider above Settings, not a glyph.
$MinRun = [int]([math]::Max(4, 6 * $scale / 1.5))
# The shell compacts the pane below 1000 logical px (src/app.rs NARROW_BREAKPOINT).
$compact = [double]$meta.logicalW -lt 1000
$railWidth = if ($compact) { 52 } else { 232 }
# In compact mode the selected item also draws its 3px leading indicator at the
# item's left edge (rail padding 8, so 8..11 logical); that is chrome, not the
# glyph. The scan therefore starts at 12 and ends at 40, which contains the
# centred 16px glyph (18..34) and excludes the indicator. Folding the indicator in
# reported the selected row as 8px left of centre - a harness artifact that would
# mask the real bug this exists to catch.
$glyphLeft = if ($compact) { 12 } else { 8 + 12 }
$glyphSpan = if ($compact) { 28 } else { 16 }
$glyphX = [int]($glyphLeft * $scale)
$glyphW = [int]($glyphSpan * $scale)
# A narrow window auto-compacts the pane *and* hides the expand/collapse toggle,
# because at that width it could not change anything.
$expectedRows = if ($compact) { 8 } else { 9 }
$labelX = [int]((8 + 12 + 16 + 12) * $scale)
# The rail stops where the status bar starts; scanning the full image height would
# also pick up the status bar's own text as a tenth "glyph".
$railTop = [int](48 * $scale)
$railH = [int](([double]$meta.logicalH - 48 - 26) * $scale)
$glyphRuns = @(Get-InkRuns $map $glyphX $glyphW $railTop $railH |
    Where-Object { ($_.Bottom - $_.Top + 1) -ge $MinRun })

$report.Add("rail mode: $(if ($compact) { 'compact' } else { 'expanded' }); glyph runs: $($glyphRuns.Count)")
if ($glyphRuns.Count -ne $expectedRows) {
    $failures.Add("expected $expectedRows rail glyphs, measured $($glyphRuns.Count)")
}

if ($glyphRuns.Count -ge 2) {
    # Rows 0..6 are the contiguous destinations; Settings and the collapse toggle
    # are pushed to the bottom by the stretch spacer, so only the first group has
    # a constant pitch.
    $pitch = 38 * $scale
    for ($i = 1; $i -lt [math]::Min(7, $glyphRuns.Count); $i++) {
        $d = $glyphRuns[$i].Center - $glyphRuns[$i - 1].Center
        if ([math]::Abs($d - $pitch) -gt $Tolerance) {
            $failures.Add(("rail glyph pitch {0}->{1} is {2} px, expected {3}" -f ($i - 1), $i, [math]::Round($d, 1), [math]::Round($pitch, 1)))
        }
    }
}

if ($compact) {
    # No labels in compact mode, so the invariant that matters is horizontal: every
    # glyph has to sit on the rail's centre line, not merely be "somewhere inside".
    $railCentre = ($railWidth / 2.0) * $scale
    $xs = @()
    foreach ($run in $glyphRuns) {
        $box = Get-InkBox $map $glyphX ([int]$run.Top) $glyphW ([int]($run.Bottom - $run.Top + 1))
        if (-not $box) { continue }
        $xs += $box.CenterX
        $dx = $box.CenterX - $railCentre
        $report.Add(("compact row y={0:N1}: glyph_x={1:N1} rail_centre={2:N1} dx={3:N1}px ({4:N2} logical)" -f `
            $run.Center, $box.CenterX, $railCentre, $dx, (ToLogical $dx)))
        if ([math]::Abs($dx) -gt $Tolerance) {
            $failures.Add(("compact rail glyph is {0:N1}px off the rail's centre line" -f $dx))
        }
    }
} else {
    $labelRuns = @(Get-InkRuns $map $labelX ([int](110 * $scale)) $railTop $railH |
        Where-Object { ($_.Bottom - $_.Top + 1) -ge $MinRun })
    $report.Add("expanded rail label runs: $($labelRuns.Count)")
    if ($labelRuns.Count -ne $expectedRows) {
        $failures.Add("expected $expectedRows rail labels, measured $($labelRuns.Count)")
    }
    $pairs = [math]::Min($glyphRuns.Count, $labelRuns.Count)
    for ($i = 0; $i -lt $pairs; $i++) {
        $delta = $glyphRuns[$i].Center - $labelRuns[$i].Center
        $report.Add(("rail row {0}: glyph_y={1:N1} label_y={2:N1} delta={3:N1}px ({4:N2} logical)" -f `
            $i, $glyphRuns[$i].Center, $labelRuns[$i].Center, $delta, (ToLogical $delta)))
        $limit = $Tolerance + 1.5
        if ([math]::Abs($delta) -gt $limit) {
            $failures.Add(("rail row {0}: glyph is {1:N1}px off the label centre line" -f $i, $delta))
        }
    }

    # All rail glyphs must share one glyph column.
    $xs = @()
    foreach ($run in $glyphRuns) {
        $box = Get-InkBox $map $glyphX ([int]$run.Top) $glyphW ([int]($run.Bottom - $run.Top + 1))
        if ($box) { $xs += $box.CenterX }
    }
    if ($xs.Count -ge 2) {
        $spread = ($xs | Measure-Object -Maximum).Maximum - ($xs | Measure-Object -Minimum).Minimum
        $report.Add(("rail glyph column spread: {0:N1}px" -f $spread))
        if ($spread -gt 3) {
            $failures.Add(("rail glyphs are not on one vertical rule: spread {0:N1}px" -f $spread))
        }
    }
}

# ---------------------------------------------------------------------------
# 2. Top command bar: back / forward glyphs must sit on the bar's centre line.
# ---------------------------------------------------------------------------
# 8 (padding) + 28 (wordmark) + 4 (gap) + 4 (spacer) + 4 (gap) = 48, then two
# 32px buttons separated by a 4px gap.
$barCentre = 24 * $scale
foreach ($slot in @(@(48, 32, "back"), @(84, 32, "forward"))) {
    $x = [int]($slot[0] * $scale)
    $w = [int]($slot[1] * $scale)
    $box = Get-InkBox $map $x 0 $w ([int](48 * $scale)) -RefX ([int]($slot[0] * $scale) - 6) -RefY 4
    if ($null -eq $box) {
        $failures.Add("$($slot[2]) button: no ink found in its slot")
        continue
    }
    $delta = $box.CenterY - $barCentre
    $report.Add(("top bar {0}: centre_y={1:N1} delta_from_bar_centre={2:N1}px ({3:N2} logical)" -f `
        $slot[2], $box.CenterY, $delta, (ToLogical $delta)))
    if ([math]::Abs($delta) -gt $Tolerance) {
        $failures.Add(("$($slot[2]) glyph is {0:N1}px off the command bar's centre" -f $delta))
    }
}

# ---------------------------------------------------------------------------
$report | ForEach-Object { Write-Host "  $_" }
if ($failures.Count -gt 0) {
    Write-Host ""
    $failures | ForEach-Object { Write-Host "  FAIL $_" -ForegroundColor Red }
    Write-Host ("{0} icon-geometry failures at {1}{2}" -f $failures.Count, $Size, $suffix) -ForegroundColor Red
    exit 1
}
Write-Host ("icon geometry OK at {0}{1}: {2} glyph centres within {3}px of their row centre" -f `
        $Size, $suffix, ($glyphRuns.Count + 2), $Tolerance) -ForegroundColor Green
