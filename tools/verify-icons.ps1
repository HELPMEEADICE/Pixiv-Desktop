# Icon geometry regression test.
#
# Everything here is measured from a real capture, not from the Slint source. The
# bug this exists to catch is invisible in the source: a `FluentGlyph` given
# `width: 20px; height: 20px` inside a `HorizontalLayout` is aligned to the *start*
# of the cross axis, so it renders ~10 logical px above its label — the declarations
# look perfectly reasonable.
#
# G57: the capture's exit code and this run's own PNG decide the measurement. The
# child used to go into "| Out-Null" and the only question asked afterwards was whether
# a sidecar existed - so a failed capture measured last round's PNG and reported OK.
# Both files are removed first and a child that fails is a named CAPTURE failure.
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
    [double]$Tolerance = 2.5,
    # Judge the capture already in -OutDir instead of taking a new one. This is how
    # tools/selftest-gates.ps1 drives the gate with no window: it writes a fixture PNG
    # and the sidecar this script reads, and every measurement below is the shipped one.
    # tools/verify-visual.ps1 has had the same switch since round seven for the same
    # reason - a gate whose regression can only be re-run with a window in front of it is
    # a gate whose regression nobody re-runs.
    [switch]$SkipCapture
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "pixel.ps1")

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$suffix = if ($Dark) { "-dark" } else { "" }
$shot = Join-Path $OutDir ("icons-{0}{1}.png" -f $Size, $suffix)
$metaPath = [System.IO.Path]::ChangeExtension($shot, ".json")
if (-not $SkipCapture) {
    # G57: this run's own files, gone before the capture. A sidecar on disk afterwards
    # was written by this run, not by the one before it.
    Remove-Item -Force $shot, $metaPath -ErrorAction SilentlyContinue
    $shotArgs = @("-File", (Join-Path $PSScriptRoot "shot.ps1"), "-Page", "home", "-Mode", "size",
        "-Size", $Size, "-Out", $shot, "-Exe", $Exe)
    if ($Dark) { $shotArgs += "-Dark" }
    & pwsh @shotArgs | Out-Null
    $shotExit = $LASTEXITCODE
    if ($shotExit -ne 0) {
        Write-Host ("CAPTURE: shot.ps1 exited {0} - no frame was captured, so no glyph geometry was measured" -f $shotExit) -ForegroundColor Red
        exit 1
    }
}
# The capture and its sidecar are the input in both modes, so a missing one is a named
# failure rather than a throw: a throw is indistinguishable from a defect in the gate.
if (-not (Test-Path $shot)) {
    Write-Host ("CAPTURE: no capture at {0} - nothing to measure" -f $shot) -ForegroundColor Red
    exit 1
}
if (-not (Test-Path $metaPath)) {
    Write-Host ("CAPTURE: {0} has no sidecar, so the scale factor and the window size it was taken at are unknown" -f $shot) -ForegroundColor Red
    exit 1
}
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
    # G8: this path used to exit 0, and tools/gates.ps1 reads 0 as PASS, so the one
    # gate that measures glyph ink reported a pass for a run that measured nothing.
    # 77 is the documented "this host cannot run this gate" code (gates.ps1:101).
    exit 77
}
$map = Get-PixelMap $shot

function ToLogical([double]$v) { return [math]::Round($v / $scale, 2) }

$failures = New-Object System.Collections.Generic.List[string]
$report = New-Object System.Collections.Generic.List[string]

# ---------------------------------------------------------------------------
# 1. Navigation rail: two groups, not one chain.
# ---------------------------------------------------------------------------
# Round twelve: the rail is not one equidistant chain. It is
#
#     the destination rows          (the group above the stretch spacer)
#     ... `Rectangle { vertical-stretch: 1; }` ...
#     Settings                      (pinned to the bottom)
#     the collapse toggle           (pinned, and not drawn at all when the host has
#                                    auto-compacted the pane: ui/main.slint's
#                                    `if !root.narrow : NavItem`)
#
# so one capture measures two clusters, and the pitch this gate asserts is a property
# of the first one only. Measuring them as one chain is what made the gate report
# "expected 9 rail glyphs, measured 10" on a rail that was correct: the tenth row was
# the pinned toggle, 220 physical px below the eighth destination.
#
# The counts come from the source, never from a constant here.
#
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
$labelX = [int]((8 + 12 + 16 + 12) * $scale)
# The rail stops where the status bar starts; scanning the full image height would
# also pick up the status bar's own text as a tenth glyph.
$railTop = [int](48 * $scale)
$railH = [int](([double]$meta.logicalH - 48 - 26) * $scale)
$glyphRuns = @(Get-InkRuns $map $glyphX $glyphW $railTop $railH |
    Where-Object { ($_.Bottom - $_.Top + 1) -ge $MinRun })

<#
  How many rows the rail declares, read from ui/main.slint.

  Total is every NavItem block in the rail's VerticalLayout; Pinned is how many of
  them sit after the `vertical-stretch: 1` spacer, which is what makes a row pinned to
  the bottom rather than a member of the equidistant group; PlainPinned is the pinned
  rows with no condition; Guards is the condition of every pinned row that has one.

  A brace count is enough to walk the block here - no NavItem in this file carries a
  brace inside a string - but a parse that finds nothing throws rather than returning
  zero, because "no NavItems" would pass a rail with no rows at all. The guard text is
  reported verbatim for the caller to evaluate: this function does not decide what
  `!root.narrow` means at a given window width.
#>
function Get-RailNavItems {
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path $Path)) { throw "no $Path to read the rail's rows from" }
    $lines = Get-Content $Path
    $pane = -1
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -match 'rail-pane\s*:=\s*Rectangle') { $pane = $i; break }
    }
    if ($pane -lt 0) { throw "$Path has no 'rail-pane := Rectangle': the rail block was renamed or moved" }
    $start = -1
    for ($i = $pane; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -match '^\s*VerticalLayout\s*\{') { $start = $i; break }
    }
    if ($start -lt 0) { throw "$Path has no VerticalLayout inside rail-pane" }
    $items = New-Object System.Collections.Generic.List[object]
    $pinned = $false
    $depth = 0
    for ($i = $start; $i -lt $lines.Count; $i++) {
        $code = ($lines[$i] -split '//')[0]
        if ($code -match 'vertical-stretch:\s*1') { $pinned = $true }
        # One NavItem block opens on one line, and its body never repeats the name.
        $m = [regex]::Match($code, '(?:if\s+(?<guard>[^:]+?)\s*:\s*)?\bNavItem\s*\{')
        if ($m.Success) {
            $guard = ""
            if ($m.Groups['guard'].Success) { $guard = $m.Groups['guard'].Value.Trim() }
            $items.Add([pscustomobject]@{ Line = $i + 1; Pinned = $pinned; Guard = $guard })
        }
        $depth += ([regex]::Matches($code, '\{')).Count - ([regex]::Matches($code, '\}')).Count
        if ($depth -le 0 -and $i -gt $start) { break }
    }
    if ($items.Count -eq 0) { throw "$Path's rail block declares no NavItem: the gate cannot tell how many rows to expect" }
    return [pscustomobject]@{
        Total       = $items.Count
        Pinned      = @($items | Where-Object { $_.Pinned }).Count
        PlainPinned = @($items | Where-Object { $_.Pinned -and -not $_.Guard }).Count
        Guards      = @($items | Where-Object { $_.Guard } | ForEach-Object { $_.Guard } | Select-Object -Unique)
        Rows        = $items
    }
}

$rail = Get-RailNavItems -Path "ui/main.slint"
$expectedMain = $rail.Total - $rail.Pinned
# Which pinned rows are drawn now. The unconditional ones always are; a conditional one
# only when its guard is true. Exactly one guard is understood, because exactly one
# exists - a new guard has to be taught here rather than guessed, which is the whole
# reason the text is read instead of counted by hand.
$drawnPinned = $rail.PlainPinned
foreach ($guard in $rail.Guards) {
    $guardLine = @($rail.Rows | Where-Object { $_.Guard -eq $guard } | ForEach-Object { $_.Line }) -join ", "
    $value = switch -regex ($guard) {
        '^!\s*root\.narrow$' { -not $compact }
        default { $null }
    }
    if ($null -eq $value) {
        $failures.Add(("rail: ui/main.slint:{0} guards a pinned row with '{1}', which this gate cannot evaluate - teach the guard table above before trusting the tail count" -f $guardLine, $guard))
        continue
    }
    if ($value) { $drawnPinned++ }
}
$expectedTail = $drawnPinned
$expectedRuns = $expectedMain + $expectedTail
$report.Add(("rail source (ui/main.slint): {0} NavItem(s), {1} pinned after the stretch spacer, {2} of them drawn at this width -> expect {3} above the spacer + {4} pinned" -f $rail.Total, $rail.Pinned, $drawnPinned, $expectedMain, $expectedTail))

# --- split what was measured into the equidistant group and the pinned tail ---
# A cut is taken where two neighbouring rows are more than twice the pitch apart. The
# spacer between the groups is worth many pitches; nothing inside either group is worth
# two. The pitch is the item height plus the item spacing, mirrored from theme.slint and
# NavItem - it is the value the main-group assertion below measures against.
$pitch = 38 * $scale
# A narrow window auto-compacts the pane *and* hides the expand/collapse toggle,
# because at that width it could not change anything.
$clusters = New-Object System.Collections.Generic.List[object]
$current = New-Object System.Collections.Generic.List[object]
for ($i = 0; $i -lt $glyphRuns.Count; $i++) {
    if ($i -gt 0 -and ($glyphRuns[$i].Center - $glyphRuns[$i - 1].Center) -gt (2 * $pitch)) {
        $clusters.Add($current)
        $current = New-Object System.Collections.Generic.List[object]
    }
    $current.Add($glyphRuns[$i])
}
if ($current.Count -gt 0) { $clusters.Add($current) }
$main = if ($clusters.Count -ge 1) { @($clusters[0]) } else { @() }
$tail = if ($clusters.Count -ge 2) { @($clusters[1]) } else { @() }
$logicalYs = @($glyphRuns | ForEach-Object { [math]::Round($_.Center / $scale, 1) })
$report.Add(("rail mode: {0}; glyph runs: {1} in {2} group(s) ({3}); logical y {4}" -f $(if ($compact) { "compact" } else { "expanded" }), $glyphRuns.Count, $clusters.Count, (($clusters | ForEach-Object { $_.Count }) -join "+"), ($logicalYs -join ", ")))

if ($glyphRuns.Count -ne $expectedRuns) {
    $failures.Add(("rail: expected {0} glyph rows ({1} above the spacer + {2} pinned), measured {3}" -f $expectedRuns, $expectedMain, $expectedTail, $glyphRuns.Count))
}
if ($clusters.Count -gt 2) {
    $failures.Add(("rail: the glyph rows fall into {0} groups rather than 2 ({1}) - a row is off the pitch, or something that is not a rail row was measured" -f $clusters.Count, (($clusters | ForEach-Object { $_.Count }) -join "+")))
}
if ($main.Count -ne $expectedMain) {
    $failures.Add(("rail main group: expected {0} glyphs, measured {1}" -f $expectedMain, $main.Count))
}
if ($tail.Count -ne $expectedTail) {
    $failures.Add(("rail pinned tail: expected {0} glyph(s), measured {1} - the tail is Settings plus the collapse toggle, and the toggle is drawn only when the pane is not narrow (ui/main.slint)" -f $expectedTail, $tail.Count))
}
# The pitch belongs to the main group alone: the tail is pinned to the bottom, so the
# space above it is whatever the stretch spacer leaves, not an item pitch. Asserting
# one chain over both is the defect this section replaced.
for ($i = 1; $i -lt $main.Count; $i++) {
    $d = $main[$i].Center - $main[$i - 1].Center
    if ([math]::Abs($d - $pitch) -gt $Tolerance) {
        $failures.Add(("rail main group: pitch between row {0} and row {1} is {2} px, expected {3}" -f ($i - 1), $i, [math]::Round($d, 1), [math]::Round($pitch, 1)))
    }
}

if ($compact) {
    # No labels in compact mode, so the invariant that matters is horizontal: every
    # glyph has to sit on the rail's centre line, not merely be "somewhere inside".
    $railCentre = ($railWidth / 2.0) * $scale
    foreach ($run in $glyphRuns) {
        $box = Get-InkBox $map $glyphX ([int]$run.Top) $glyphW ([int]($run.Bottom - $run.Top + 1))
        if (-not $box) { continue }
        $dx = $box.CenterX - $railCentre
        $which = if ($main -contains $run) { "main" } else { "pinned" }
        $report.Add(("compact row ({0}) y={1:N1}: glyph_x={2:N1} rail_centre={3:N1} dx={4:N1}px ({5:N2} logical)" -f $which, $run.Center, $box.CenterX, $railCentre, $dx, (ToLogical $dx)))
        if ([math]::Abs($dx) -gt $Tolerance) {
            $failures.Add(("compact rail {0} group: glyph at y={1:N1} is {2:N1}px off the rail's centre line" -f $which, $run.Center, $dx))
        }
    }
} else {
    $labelRuns = @(Get-InkRuns $map $labelX ([int](110 * $scale)) $railTop $railH |
        Where-Object { ($_.Bottom - $_.Top + 1) -ge $MinRun })
    $report.Add("expanded rail label runs: $($labelRuns.Count)")
    if ($labelRuns.Count -ne $expectedRuns) {
        $failures.Add(("rail: expected {0} labels ({1} above the spacer + {2} pinned), measured {3}" -f $expectedRuns, $expectedMain, $expectedTail, $labelRuns.Count))
    }
    $pairs = [math]::Min($glyphRuns.Count, $labelRuns.Count)
    for ($i = 0; $i -lt $pairs; $i++) {
        $delta = $glyphRuns[$i].Center - $labelRuns[$i].Center
        $which = if ($i -lt $main.Count) { "main" } else { "pinned" }
        $within = if ($i -lt $main.Count) { $i } else { $i - $main.Count }
        $report.Add(("rail {0} row {1}: glyph_y={2:N1} label_y={3:N1} delta={4:N1}px ({5:N2} logical)" -f $which, $within, $glyphRuns[$i].Center, $labelRuns[$i].Center, $delta, (ToLogical $delta)))
        $limit = $Tolerance + 1.5
        if ([math]::Abs($delta) -gt $limit) {
            $failures.Add(("rail {0} row {1}: glyph is {2:N1}px off the label centre line" -f $which, $within, $delta))
        }
    }

    # Every rail glyph, in both groups, has to share one column: the tail is pinned
    # vertically and not horizontally, so a toggle half a pixel off Settings would be a
    # real defect. Reported per group so a failure names which one moved.
    $xs = @()
    $mainXs = @()
    $tailXs = @()
    foreach ($run in $glyphRuns) {
        $box = Get-InkBox $map $glyphX ([int]$run.Top) $glyphW ([int]($run.Bottom - $run.Top + 1))
        if (-not $box) { continue }
        $xs += $box.CenterX
        if ($main -contains $run) { $mainXs += $box.CenterX } else { $tailXs += $box.CenterX }
    }
    function Get-Spread([double[]]$Values) {
        if ($Values.Count -lt 2) { return 0.0 }
        return (($Values | Measure-Object -Maximum).Maximum - ($Values | Measure-Object -Minimum).Minimum)
    }
    if ($xs.Count -ge 2) {
        $spread = Get-Spread $xs
        $report.Add(("rail glyph column spread: {0:N1}px overall (main {1:N1}, pinned {2:N1})" -f $spread, (Get-Spread $mainXs), (Get-Spread $tailXs)))
        if ($spread -gt 3) {
            $failures.Add(("rail glyphs are not on one vertical rule: spread {0:N1}px (main {1:N1}, pinned {2:N1})" -f $spread, (Get-Spread $mainXs), (Get-Spread $tailXs)))
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
