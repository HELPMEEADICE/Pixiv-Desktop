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
#   case blob    - G16/G10: a 12x12 rectangle (144 px, 0.0081 % of a 1536x1152
#                  capture) replaced with black is exactly the difference the
#                  percentage budget used to admit - 0.02 % is 354 px - so the gate
#                  must say DIFF on the pixel ceiling. Proved against the old
#                  comparator with -VerifyScript (see the usage note below).
#   case capture - G7: a capture whose child process failed must be that row's FAIL,
#                  never a comparison against the PNG the previous run left behind.
#   case noscale - G14: a sidecar that declares no scale cannot judge anything, so it
#                  must be SKIP instead of switching the scale guard off.
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
# Round nine added four gates and one seam, and each is accepted on a case here:
#   case walk       - G21: a Tab stop's identity is (Name, Type, rectangle). The old
#                     identity - the pair alone - is run through the same engine and
#                     must end the walk at the second control sharing a name.
#   case cover      - G21: every focusable Button/ListItem/Edit has to be visited, with
#                     multiplicity, and the failure names the control.
#   case a11y-red   - G21 end to end: the shipped tools/verify-a11y.ps1, fed a recorded
#                     measurement shaped like today's bookmarks (31 focusable, 24
#                     walked), must exit 1 and name the stops. No window involved.
#   case a11y-green - the control for the line above: the same fixture with all 31
#                     walked must exit 0, so the case cannot be satisfied by a gate that
#                     fails everything.
#   case focus-full - G22: one page walks every stop, the rest keep ten, and a planted
#                     stop with no ring is a FAIL for that page.
#   case fixture    - G24: PIXIV_DESKTOP_FIXTURE maps ugoira/selection/palette/downloads
#                     to the state to capture, and an unknown value exits 77, not 0.
#   case ci-gates   - G25: .github/workflows/ci.yml runs the gate suite in its own job,
#                     without -Ui, and the summary job depends on it.
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
    [string]$A11yScript = "tools/verify-a11y.ps1",
    [string]$FocusScript = "tools/verify-focus.ps1",
    [string]$FixtureScript = "tools/fixture.ps1",
    [string]$WalkLib = "tools/walk-lib.ps1",
    [string]$FocusLib = "tools/focus-lib.ps1",
    [string]$CiConfig = ".github/workflows/ci.yml",
    [string]$WorkDir = "target/selftest",
    [string]$SourcePng = "docs/screenshots/baseline/home-light.png",
    [int]$Tolerance = 6
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)
Add-Type -AssemblyName System.Drawing
try { [void][System.Reflection.Assembly]::Load("System.Private.Windows.GdiPlus") } catch { }

foreach ($required in @($VerifyScript, $GatesScript, $A11yScript, $FocusScript, $FixtureScript, $WalkLib, $FocusLib, $CiConfig, $SourcePng)) {
    if (-not (Test-Path $required)) { throw "missing input: $required" }
}
# Absolute from here on: the gate under test is free to change directory (it
# Set-Locations to its own parent), and a fixture path it then cannot find would
# report NOBASE instead of the verdict being tested.
$VerifyScript = (Resolve-Path $VerifyScript).Path
$WorkDir = [System.IO.Path]::GetFullPath((Join-Path (Get-Location).Path $WorkDir))
$GatesScript = (Resolve-Path $GatesScript).Path
$SourcePng = (Resolve-Path $SourcePng).Path
$A11yScript = (Resolve-Path $A11yScript).Path
$FocusScript = (Resolve-Path $FocusScript).Path
$FixtureScript = (Resolve-Path $FixtureScript).Path
$CiConfig = (Resolve-Path $CiConfig).Path

# G21/G22: the stop identity, the walk engine and the two focus decisions are the
# shipped functions, not a copy of them. The cases below drive these directly, which is
# the whole point of their living in files instead of inside a capture loop.
. (Resolve-Path $WalkLib).Path
. (Resolve-Path $FocusLib).Path

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

# The same gate with the capture step enabled against an executable that does not
# exist: the child fails, and what the gate then says about the comparison is the
# question case capture asks.
function Invoke-VisualCapture {
    param([pscustomobject]$Case, [string]$Exe)
    $text = & pwsh -NoProfile -File $VerifyScript -SkipCapture:$false -Exe $Exe -Pages home -Themes light -Tolerance $Tolerance -BaselineDir $Case.Baseline -OutDir $Case.Out 2>&1 | Out-String
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
# can absorb, so a comparator that passes this is not comparing anything. The case
# blob reuses it with a 12x12 rectangle - same operation, different area, and that
# area is the whole point.
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

# ---- case blob: a 12x12 icon-sized difference must not fit inside the budget ----
# 144 px on a 1536x1152 capture is 0.0081 %, which the percentage budget (0.02 % =
# 354 px) admits with room to spare. This is the case the absolute pixel ceiling
# exists for; against the percentage-only comparator it reports OK and this case
# FAILs, which is how the ceiling is proved to be doing work.
$case = New-Case -Name "blob"
New-ErasedPng -Source (Join-Path $case.Baseline "home-light.png") -Target (Join-Path $case.Current "home-light.png") -X 200 -Y 200 -Width 12 -Height 12
$run = Invoke-VisualGate -Case $case
$row = @($run.Output -split "\r?\n" | Where-Object { $_ -match "^\s+home\s+light" }) -join " / "
Add-Case -Name "blob" -Wanted "exit 1 and DIFF on the 144 px blob" -Ok ($run.Code -eq 1 -and $run.Output -match "DIFF" -and $run.Output -match "pixel ceiling") -Evidence ("exit={0} {1}" -f $run.Code, $row.Trim())

# ---- case capture: a failed capture is that row's FAIL, not the old PNG ----
# G7. The fixture already holds a perfectly matching "capture" - that is the previous
# run's file, which is what the broken gate diffed and reported as OK.
$case = New-Case -Name "capture"
$run = Invoke-VisualCapture -Case $case -Exe "target\debug\nope.exe"
$row = @($run.Output -split "\r?\n" | Where-Object { $_ -match "^\s+home\s+light" }) -join " / "
Add-Case -Name "capture" -Wanted "exit 1 and CAPTURE" -Ok ($run.Code -eq 1 -and $run.Output -match "CAPTURE" -and $run.Output -match "nothing was compared") -Evidence ("exit={0} {1}" -f $run.Code, $row.Trim())

# ---- case noscale: an unreadable sidecar is SKIP, never "same scale" ----
# G14. The capture sidecar is rewritten without a scale: the old guard read
# "$baseScale -ne 0 -and ..." on a value that had defaulted to 0, so it switched
# itself off and compared anyway.
$case = New-Case -Name "noscale"
$metaNoScale = [ordered]@{ page = "home"; theme = "light"; size = "1024x768"; width = 1536; height = 1152 }
($metaNoScale | ConvertTo-Json -Compress) | Set-Content -Path (Join-Path $case.Current "home-light.json") -Encoding utf8
$run = Invoke-VisualGate -Case $case
$row = @($run.Output -split "\r?\n" | Where-Object { $_ -match "^\s+home\s+light" }) -join " / "
Add-Case -Name "noscale" -Wanted "exit 77 and SKIP: unreadable sidecar" -Ok ($run.Code -eq 77 -and $run.Output -match "unreadable sidecar") -Evidence ("exit={0} {1}" -f $run.Code, $row.Trim())

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


# ---------------------------------------------------------------- round nine

<#
  A recorded a11y measurement shaped like today's bookmarks page: 31 focusable
  Button/ListItem/Edit stops, two pairs of them sharing a name, and a walk that stopped
  after -WalkedCount of them. This is what tools/verify-a11y.ps1 -Replay asserts
  against, so the gate's own failure path runs with no window, no desktop and no app -
  which is also what lets the CI job (G25) run this file at all.
#>
function New-A11yFixture {
    param([string]$Name, [int]$WalkedCount)
    $dir = Join-Path $WorkDir "a11y"
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $needStops = @()
    for ($i = 0; $i -lt 31; $i++) {
        $stopName = if ($i -lt 2) { "Bookmark" } elseif ($i -lt 4) { "Tile 2" } else { "Tile $i" }
        $stopType = if ($i -eq 4) { "ListItem" } elseif ($i -eq 5) { "Edit" } else { "Button" }
        $needStops += [pscustomobject]@{
            name   = $stopName
            type   = $stopType
            x      = 40 + ($i % 6) * 160
            y      = 120 + [math]::Floor($i / 6) * 180
            width  = 152
            height = 168
        }
    }
    $page = [ordered]@{
        page      = "bookmarks"
        total     = 120
        byType    = [ordered]@{ Button = 24; ListItem = 2; Edit = 1; Text = 60; Image = 33 }
        focusable = 31
        invokable = 20
        named     = 90
        walkEnd   = "cycle"
        required  = $needStops
        walked    = @($needStops | Select-Object -First $WalkedCount)
    }
    $path = Join-Path $dir "$Name.json"
    (@($page) | ConvertTo-Json -Depth 6) | Set-Content -Path $path -Encoding utf8
    # Floors of 1: the only question a replay here asks is coverage, so a budget failure
    # cannot be what makes the negative case red.
    $budgetPath = Join-Path $dir "budget.json"
    if (-not (Test-Path $budgetPath)) {
        $budget = [ordered]@{
            _comment = "selftest fixture: floors of 1, so the only question either replay asks is coverage"
            default  = [ordered]@{ minInvokable = 1; minFocusable = 1; minTabStops = 1; roles = @("Button") }
            pages    = [ordered]@{ bookmarks = [ordered]@{ minInvokable = 1; minFocusable = 1; minTabStops = 1; roles = @("Button") } }
        }
        ($budget | ConvertTo-Json -Depth 6) | Set-Content -Path $budgetPath -Encoding utf8
    }
    return [pscustomobject]@{ Path = $path; Budget = $budgetPath; Out = (Join-Path $dir "out") }
}

# The shipped gate, as a child process, judging a recorded measurement.
function Invoke-A11yReplay {
    param([pscustomobject]$Case)
    $text = & pwsh -NoProfile -File $A11yScript -Replay $Case.Path -Budget $Case.Budget -OutDir $Case.Out 2>&1 | Out-String
    return [pscustomobject]@{ Code = $LASTEXITCODE; Output = $text }
}

# ---- case walk: G21 - a stop is (Name, Type, rectangle), not (Name, Type) ----
# Four buttons, two of them named "Bookmark". The pre-G21 identity is run through the
# same engine (-PairOnly), so the control is the old rule itself and not a paraphrase of
# it: that is what makes this an experiment rather than an illustration.
$synthetic = @(
    [pscustomobject]@{ Name = "Bookmark"; Type = "Button"; X = 400; Y = 120; Width = 32; Height = 32 },
    [pscustomobject]@{ Name = "Bookmark"; Type = "Button"; X = 440; Y = 120; Width = 32; Height = 32 },
    [pscustomobject]@{ Name = "Download"; Type = "Button"; X = 480; Y = 120; Width = 32; Height = 32 },
    [pscustomobject]@{ Name = "Like";     Type = "Button"; X = 520; Y = 120; Width = 32; Height = 32 }
)
$cursor = @{ i = 0 }
$nextStop = {
    $k = $cursor.i
    $cursor.i++
    if ($k -lt $synthetic.Count) { $synthetic[$k] } else { $synthetic[0] }
}
$oldWalk = Measure-TabWalk -MaxSteps 50 -PairOnly -NextStop $nextStop
$cursor.i = 0
$newWalk = Measure-TabWalk -MaxSteps 50 -NextStop $nextStop
$oldCover = Compare-StopCoverage -Required $synthetic -Walked $oldWalk.Stops
$newCover = Compare-StopCoverage -Required $synthetic -Walked $newWalk.Stops
Add-Case -Name "walk" -Wanted "(Name,Type) ends at 1 stop, (Name,Type,rect) walks 4 to a cycle" -Ok ($oldWalk.Distinct -eq 1 -and $oldWalk.Cycle -and $newWalk.Distinct -eq 4 -and $newWalk.Cycle -and $newWalk.End -eq "cycle") -Evidence ("pair-only: {0} stop(s) cycle={1}; identity: {2} stop(s) cycle={3}; unreached stops {4} -> {5}" -f $oldWalk.Distinct, $oldWalk.Cycle, $newWalk.Distinct, $newWalk.Cycle, $oldCover.Missing.Count, $newCover.Missing.Count)

# ---- case cover: G21 - a focusable control the walk never reached is a FAIL ----
# Two buttons named "Bookmark": walking one of them is not walking the other. The failure
# names the one that was missed, by rectangle, so the message is actionable.
$triple = @(
    [pscustomobject]@{ Name = "Bookmark"; Type = "Button"; X = 400; Y = 120; Width = 32; Height = 32 },
    [pscustomobject]@{ Name = "Bookmark"; Type = "Button"; X = 440; Y = 120; Width = 32; Height = 32 },
    [pscustomobject]@{ Name = "Like";     Type = "Button"; X = 480; Y = 120; Width = 32; Height = 32 }
)
$partialWalk = @($triple[0], $triple[2])
$gap = Compare-StopCoverage -Required $triple -Walked $partialWalk
$covered = Compare-StopCoverage -Required $triple -Walked $triple
Add-Case -Name "cover" -Wanted "one Bookmark missed at x=440, reached=2/3; all walked covers 3" -Ok ($gap.Missing.Count -eq 1 -and $gap.Missing[0].Name -eq "Bookmark" -and $gap.Missing[0].X -eq 440 -and $gap.Reached -eq 2 -and $covered.Ok) -Evidence ("missing={0} [{1}] reached={2}/{3}; all-walked missing={4}" -f $gap.Missing.Count, (($gap.Missing | ForEach-Object { $_.Label }) -join " | "), $gap.Reached, $gap.Required, $covered.Missing.Count)

# ---- case a11y-red: G21 end to end - the shipped gate on 24 of 31 visited stops ----
# The fixture is the shape today's bookmarks page reports: 31 focusable, a walk that
# ended after 24. The old identity called that a cycle; the gate now fails it and names
# the stops it never reached.
$red = New-A11yFixture -Name "walk-red" -WalkedCount 24
$runRed = Invoke-A11yReplay -Case $red
$rowRed = @($runRed.Output -split "\r?\n" | Where-Object { $_ -match "reached=" }) -join " / "
Add-Case -Name "a11y-red" -Wanted "exit 1, reached=24/31, never reached by Tab" -Ok ($runRed.Code -eq 1 -and $runRed.Output -match "reached=24/31" -and $runRed.Output -match "never reached by Tab") -Evidence ("exit={0} {1}" -f $runRed.Code, $rowRed.Trim())

# ---- case a11y-green: the control - all 31 walked has to pass ----
# Without it the case above could be satisfied by a gate that fails everything.
$green = New-A11yFixture -Name "walk-green" -WalkedCount 31
$runGreen = Invoke-A11yReplay -Case $green
$rowGreen = @($runGreen.Output -split "\r?\n" | Where-Object { $_ -match "reached=" }) -join " / "
Add-Case -Name "a11y-green" -Wanted "exit 0 and 1 page meets the budget" -Ok ($runGreen.Code -eq 0 -and $runGreen.Output -match "reached=31/31" -and $runGreen.Output -match "1 pages meet") -Evidence ("exit={0} {1}" -f $runGreen.Code, $rowGreen.Trim())

# ---- case focus-full: G22 - one page walks every stop; a ringless stop is invisible ----
# The old behaviour is the same function with -FullPages @(), checked beside the new one:
# ten stops on home then, every stop now. The planted stop in the middle has ring=0 and
# accent=0 - UI-NOTES 12a as a number - and must be the one the gate reports.
$limitFull = Get-WalkLimit -Page "home" -FullPages @("home") -Stops 10 -MaxStops 200
$limitOld = Get-WalkLimit -Page "home" -FullPages @() -Stops 10 -MaxStops 200
$limitRest = Get-WalkLimit -Page "settings" -FullPages @("home") -Stops 10 -MaxStops 200
$planted = @(
    [pscustomobject]@{ Name = "Back";       Verdict = (Get-FocusVerdict -RingPixels 512 -Alpha 1.0 -Type "Button" -MinPixels 6) },
    [pscustomobject]@{ Name = "IllustCard"; Verdict = (Get-FocusVerdict -RingPixels 0 -Alpha 0.0 -AccentPixels 0 -Type "ListItem" -MinPixels 6) },
    [pscustomobject]@{ Name = "Search";     Verdict = (Get-FocusVerdict -RingPixels 0 -Alpha 0.0 -AccentPixels 40 -Type "Edit" -MinPixels 6) }
)
$invisible = @(Get-InvisibleStops -Stops $planted)
$focusText = Get-Content $FocusScript -Raw
$wired = (($focusText -match "Get-WalkLimit") -and ($focusText -match "Get-FocusVerdict") -and ($focusText -match "Get-InvisibleStops") -and ($focusText -match "s total"))
Add-Case -Name "focus-full" -Wanted "home walks past 10, others keep 10, planted stop NONE, duration printed" -Ok ($limitFull -gt 10 -and $limitOld -eq 10 -and $limitRest -eq 10 -and $invisible.Count -eq 1 -and $planted[1].Verdict -eq "NONE" -and $planted[0].Verdict -eq "RING" -and $planted[2].Verdict -eq "ACCENT" -and $wired) -Evidence ("full={0} old={1} other={2}; verdicts={3}; invisible={4}; wired={5}" -f $limitFull, $limitOld, $limitRest, (($planted | ForEach-Object { $_.Verdict }) -join ","), $invisible.Count, $wired)

# ---- case fixture: G24 - an unknown PIXIV_DESKTOP_FIXTURE exits 77, never 0 ----
# The value the app and the capture gates actually read is the ambient one, so that is
# what is set here. The last call is the consumer path: dot-sourced with a nonsense value
# in the environment, the file still has to load and hand out its functions.
$previousFixture = $env:PIXIV_DESKTOP_FIXTURE
try {
    $env:PIXIV_DESKTOP_FIXTURE = "no-such-fixture"
    $unknownText = & pwsh -NoProfile -File $FixtureScript 2>&1 | Out-String
    $unknownCode = $LASTEXITCODE
    $env:PIXIV_DESKTOP_FIXTURE = "ugoira"
    $knownText = & pwsh -NoProfile -File $FixtureScript 2>&1 | Out-String
    $knownCode = $LASTEXITCODE
    $listText = & pwsh -NoProfile -File $FixtureScript -List 2>&1 | Out-String
    $listCode = $LASTEXITCODE
    $env:PIXIV_DESKTOP_FIXTURE = "still-nonsense"
    $dotText = & pwsh -NoProfile -Command ". '$FixtureScript'; if ((Get-FixtureState -Name 'palette').Page -eq 'home') { 'dot-source ok' } else { 'dot-source wrong'; exit 1 }" 2>&1 | Out-String
    $dotCode = $LASTEXITCODE
}
finally { $env:PIXIV_DESKTOP_FIXTURE = $previousFixture }
$allFour = (($listText -match "ugoira") -and ($listText -match "selection") -and ($listText -match "palette") -and ($listText -match "downloads"))
Add-Case -Name "fixture" -Wanted "unknown -> 77, known -> 0 (page=detail), -List names four, dot-source safe" -Ok ($unknownCode -eq 77 -and $knownCode -eq 0 -and $knownText -match "page=detail" -and $listCode -eq 0 -and $allFour -and $dotCode -eq 0) -Evidence ("unknown={0} known={1} list={2} four={3} dot={4}" -f $unknownCode, $knownCode, $listCode, $allFour, $dotCode)

# ---- case ci-gates: G25 - the gate suite runs in its own CI job ----
# Read, not run: the job cannot be executed here. What is decided is the wiring G25 is
# about - the job exists on Windows, it runs tools/gates.ps1, its run: lines do not ask
# for -Ui (a runner has no desktop), and the summary job depends on it, so a failing gate
# fails the one status branch protection watches.
$ci = Get-Content $CiConfig -Raw
$gateBlock = ""
if ($ci -match "(?ms)^  gates:\r?\n(.*?)(?=^  ci:\r?\n)") { $gateBlock = $Matches[1] }
$ciTail = if ($ci -match "(?ms)^  ci:\r?\n(.*)$") { $Matches[1] } else { "" }
$hasJob = ($gateBlock -ne "")
$onWindows = ($gateBlock -match "runs-on:\s*windows-latest")
$runsScript = ($gateBlock -match "tools/gates\.ps1")
$gateRunLines = @($gateBlock -split "\r?\n" | Where-Object { $_ -match "^\s*run:" })
$noUi = (@($gateRunLines | Where-Object { $_ -match "(^|\s)-Ui(\s|$)" }).Count -eq 0)
$needsGates = ($ciTail -match "needs:\s*\[[^\]]*\bgates\b")
$failsOnFailure = ($ciTail -match "contains\(needs\.\*\.result, 'failure'\)")
Add-Case -Name "ci-gates" -Wanted "gates job on windows-latest runs tools/gates.ps1, no -Ui, ci needs it" -Ok ($hasJob -and $onWindows -and $runsScript -and $noUi -and $needsGates -and $failsOnFailure) -Evidence ("job={0} windows={1} script={2} runLines={3} noUi={4} needs={5} failsOnFailure={6}" -f $hasJob, $onWindows, $runsScript, $gateRunLines.Count, $noUi, $needsGates, $failsOnFailure)

Write-Host ""
$bad = @($cases | Where-Object { -not $_.Ok })
$colour = if ($bad.Count) { "Red" } else { "Green" }
Write-Host ("{0} cases, {1} failed (fixtures in {2})" -f $cases.Count, $bad.Count, $WorkDir) -ForegroundColor $colour
if ($bad.Count) { exit 1 }
exit 0
