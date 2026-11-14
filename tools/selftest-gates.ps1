# Negative experiments for the gates that were green while they could not fail.
#
# A gate that cannot fail is worse than no gate: it converts "nobody looked" into
# "verified". The three findings this script exists for (docs/OPTIMIZATION-PLAN-R7
# 2.1 G1-G3) were each confirmed by reading the code *and* by making the wrong
# thing happen, so the proof is kept here and re-run with the gates:
#
#   case shift   - G2: a capture displaced by exactly one pixel scored over = 0.00%
#                  through the comparator, because a pixel counts as equal when any
#                  neighbour is within tolerance. The gate must now say SHIFT.
#   case erase   - the control for the same comparator: a 200x200 block of a
#                  capture replaced with black is a real difference and must say
#                  DIFF, so "SHIFT" cannot be a verdict everything gets.
#   case skip    - G1: a baseline whose sidecar declares another scale cannot judge
#                  the capture. All comparisons skipped means nothing was compared,
#                  so the gate must exit 77 with "0 compared", not print a green
#                  line about how close the captures are.
#   case lint    - G3: the lint step ran cargo check with no -D warnings and then
#                  filtered its output with ^(warning|error), which drops the
#                  per-item "path:line:col: warning: ..." lines --message-format
#                  short emits. That is a static check on tools/gates.ps1: proving
#                  it live needs a deliberate lint in src/, which is the gate's job
#                  to catch, not this script's to plant.
#
# Every fixture is built under target/selftest from a read-only copy of one tracked
# baseline; docs/screenshots is never written. The gate under test is the real
# tools/verify-visual.ps1 run as a child process against those fixtures
# (-SkipCapture), so what is measured is the shipped path and not a copy of its
# logic.
#
# Usage:
#   pwsh -File tools/selftest-gates.ps1
#   pwsh -File tools/selftest-gates.ps1 -VerifyScript target/selftest/regress/verify-visual.ps1
#
# The second form is how the guard itself is proved: point it at a copy of the gate
# with one of the three fixes taken back out, and the matching case must report FAIL.
param(
    [string]$VerifyScript = "tools/verify-visual.ps1",
    [string]$GatesScript = "tools/gates.ps1",
    [string]$WorkDir = "target/selftest",
    [string]$SourcePng = "docs/screenshots/baseline/home-light.png",
    [int]$Tolerance = 6
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)
Add-Type -AssemblyName System.Drawing
try { [void][System.Reflection.Assembly]::Load("System.Private.Windows.GdiPlus") } catch { }

foreach ($required in @($VerifyScript, $GatesScript, $SourcePng)) {
    if (-not (Test-Path $required)) { throw "missing input: $required" }
}
# Absolute from here on: the gate under test is free to change directory (it
# Set-Locations to its own parent), and a fixture path it then cannot find would
# report NOBASE instead of the verdict being tested.
$VerifyScript = (Resolve-Path $VerifyScript).Path
$WorkDir = [System.IO.Path]::GetFullPath((Join-Path (Get-Location).Path $WorkDir))
$GatesScript = (Resolve-Path $GatesScript).Path
$SourcePng = (Resolve-Path $SourcePng).Path

<#
  One fixture directory: a baseline PNG with its sidecar, and the "capture" the gate
  will diff against it. The sidecars are the gate's own format (page, theme, size,
  scale, width, height) so no code path is bypassed: the scale in them is what
  decides whether the comparison runs at all.
#>
function New-Case {
    param([string]$Name, [double]$BaselineScale = 1.5, [double]$LiveScale = 1.5)
    $dir = [System.IO.Path]::GetFullPath((Join-Path $WorkDir $Name))
    if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
    $baseline = Join-Path $dir "baseline"
    $out = Join-Path $dir "out"
    $current = Join-Path $out "current"
    New-Item -ItemType Directory -Force -Path $baseline | Out-Null
    New-Item -ItemType Directory -Force -Path $current | Out-Null

    Copy-Item $SourcePng (Join-Path $baseline "home-light.png") -Force
    Copy-Item $SourcePng (Join-Path $current "home-light.png") -Force

    $image = [System.Drawing.Image]::FromFile($SourcePng)
    $width = $image.Width
    $height = $image.Height
    $image.Dispose()
    foreach ($pair in @(@($baseline, $BaselineScale), @($current, $LiveScale))) {
        $meta = [ordered]@{ page = "home"; theme = "light"; size = "1024x768"; scale = $pair[1]; width = $width; height = $height }
        ($meta | ConvertTo-Json -Compress) | Set-Content -Path (Join-Path $pair[0] "home-light.json") -Encoding utf8
    }
    return [pscustomobject]@{ Dir = $dir; Baseline = $baseline; Out = $out; Current = $current }
}

# The real gate, as a child process, judging a fixture instead of the application.
function Invoke-VisualGate {
    param([pscustomobject]$Case)
    $text = & pwsh -NoProfile -File $VerifyScript -SkipCapture -Pages home -Themes light -Tolerance $Tolerance -BaselineDir $Case.Baseline -OutDir $Case.Out 2>&1 | Out-String
    return [pscustomobject]@{ Code = $LASTEXITCODE; Output = $text }
}

<#
  Displace the whole capture by one pixel and fill what the move uncovers with
  black. This is UI-NOTES 12d as a picture: nothing changed size, every element sits
  one pixel off, and a neighbour-tolerant comparator cannot see it.
#>
function New-ShiftedPng {
    param([string]$Source, [string]$Target, [int]$Dx, [int]$Dy)
    $src = [System.Drawing.Image]::FromFile($Source)
    $dst = [System.Drawing.Bitmap]::new($src.Width, $src.Height, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($dst)
    $graphics.Clear([System.Drawing.Color]::Black)
    # Rectangle, not DrawImageUnscaled: "unscaled" means the source's *physical*
    # size, and these baselines carry 144 dpi (captured on a 150% host), so the
    # overload silently draws them at 96/144 scale into a 96 dpi bitmap. Measured:
    # the shifted copy then differs from the baseline at 77% of its pixels instead
    # of one pixel. A destination rectangle is in pixels and cannot rescale.
    $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
    $graphics.DrawImage($src, [System.Drawing.Rectangle]::new($Dx, $Dy, $src.Width, $src.Height))
    $graphics.Dispose()
    $src.Dispose()
    $dst.Save($Target, [System.Drawing.Imaging.ImageFormat]::Png)
    $dst.Dispose()
}

# The control: a region of the capture erased to black. A difference no comparator
# can absorb, so a comparator that passes this is not comparing anything.
function New-ErasedPng {
    param([string]$Source, [string]$Target, [int]$X, [int]$Y, [int]$Width, [int]$Height)
    $src = [System.Drawing.Image]::FromFile($Source)
    $dst = [System.Drawing.Bitmap]::new($src.Width, $src.Height, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($dst)
    # Same Rectangle rule as New-ShiftedPng: 144 dpi sources must not be rescaled.
    $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
    $graphics.DrawImage($src, [System.Drawing.Rectangle]::new(0, 0, $src.Width, $src.Height))
    $src.Dispose()
    $brush = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::Black)
    $graphics.FillRectangle($brush, $X, $Y, $Width, $Height)
    $brush.Dispose()
    $graphics.Dispose()
    $dst.Save($Target, [System.Drawing.Imaging.ImageFormat]::Png)
    $dst.Dispose()
}

$cases = @()
function Add-Case {
    param([string]$Name, [string]$Wanted, [bool]$Ok, [string]$Evidence)
    $script:cases += [pscustomobject]@{ Name = $Name; Wanted = $Wanted; Ok = $Ok; Evidence = $Evidence }
    $label = if ($Ok) { "PASS" } else { "FAIL" }
    $colour = if ($Ok) { "Green" } else { "Red" }
    Write-Host ("  {0}  {1,-6} wanted {2,-52} {3}" -f $label, $Name, $Wanted, $Evidence) -ForegroundColor $colour
}

Write-Host ""
Write-Host ("selftest against {0}" -f $VerifyScript) -ForegroundColor Cyan

# ---- case match: the positive control - an unchanged capture is still green ----
# Without this the other cases could be satisfied by a gate that fails everything,
# and deliverable one is explicit that what an existing OK means must not move.
$case = New-Case -Name "match"
$run = Invoke-VisualGate -Case $case
$last = @($run.Output -split "?
" | Where-Object { $_.Trim() })[-1]
Add-Case -Name "match" -Wanted "exit 0 and 1 compared" -Ok ($run.Code -eq 0 -and $run.Output -match "1 captures within") -Evidence ("exit={0} {1}" -f $run.Code, $last.Trim())

# ---- case shift: one pixel of displacement must not read as a match ----
$case = New-Case -Name "shift"
New-ShiftedPng -Source (Join-Path $case.Baseline "home-light.png") -Target (Join-Path $case.Current "home-light.png") -Dx 1 -Dy 0
$run = Invoke-VisualGate -Case $case
$row = @($run.Output -split "\r?\n" | Where-Object { $_ -match "^\s+home\s+light" }) -join " / "
Add-Case -Name "shift" -Wanted "exit 1 and SHIFT" -Ok ($run.Code -eq 1 -and $run.Output -match "SHIFT") -Evidence ("exit={0} {1}" -f $run.Code, $row.Trim())

# ---- case erase: a real difference must still fail, so SHIFT is not a catch-all ----
$case = New-Case -Name "erase"
New-ErasedPng -Source (Join-Path $case.Baseline "home-light.png") -Target (Join-Path $case.Current "home-light.png") -X 200 -Y 200 -Width 200 -Height 200
$run = Invoke-VisualGate -Case $case
$row = @($run.Output -split "\r?\n" | Where-Object { $_ -match "^\s+home\s+light" }) -join " / "
Add-Case -Name "erase" -Wanted "exit 1 and DIFF" -Ok ($run.Code -eq 1 -and $run.Output -match "DIFF") -Evidence ("exit={0} {1}" -f $run.Code, $row.Trim())

# ---- case skip: a baseline that cannot judge the capture must not be green ----
$case = New-Case -Name "skip" -BaselineScale 1.0 -LiveScale 1.5
$run = Invoke-VisualGate -Case $case
$last = @($run.Output -split "\r?\n" | Where-Object { $_.Trim() })[-1]
Add-Case -Name "skip" -Wanted "exit 77 and 0 compared" -Ok ($run.Code -eq 77 -and $run.Output -match "0 compared" -and $run.Output -match "skipped") -Evidence ("exit={0} {1}" -f $run.Code, $last.Trim())

# ---- case lint: the lint step must be able to fail, and must not swallow lines ----
$gates = Get-Content $GatesScript -Raw
$declares = $gates -match '\$env:RUSTFLAGS\s*=\s*"-D warnings"'
$restores = $gates -match '\$env:RUSTFLAGS\s*=\s*\$previousFlags'
$filters = $gates -match 'Select-String -Pattern "\^\(warning\|error\)"'
$why = @()
if (-not $declares) { $why += "no RUSTFLAGS '-D warnings'" }
if (-not $restores) { $why += "RUSTFLAGS not restored" }
if ($filters) { $why += "per-item lines filtered by ^(warning|error)" }
Add-Case -Name "lint" -Wanted "declares -D warnings, restores it, no lossy filter" -Ok ($why.Count -eq 0) -Evidence ("declares={0} restores={1} lossyFilter={2}{3}" -f $declares, $restores, $filters, $(if ($why.Count) { " -> " + ($why -join "; ") } else { "" }))

Write-Host ""
$bad = @($cases | Where-Object { -not $_.Ok })
$colour = if ($bad.Count) { "Red" } else { "Green" }
Write-Host ("{0} cases, {1} failed (fixtures in {2})" -f $cases.Count, $bad.Count, $WorkDir) -ForegroundColor $colour
if ($bad.Count) { exit 1 }
exit 0
