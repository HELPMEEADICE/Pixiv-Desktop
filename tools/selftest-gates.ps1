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
# Round eleven adds two, both about lists and numbers that drifted because nothing read
# them:
#   case page-set      - G32: the destination enum, the four page lists and the budget's
#                        keys are one set (tools/page-set.ps1). A consumer that writes
#                        its own list again, or a budget key the set does not contain,
#                        is a difference that names the pair ("a11y\budget") and the page.
#   case prose-numbers - G35: README states none of the four counts a gate prints (tests,
#                        uiaudit rules, captures, layout checks). It printed two of them
#                        and both were wrong; the case names the line, so "which sentence"
#                        is answered before "which number".
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
    [string]$PageSetLib = "tools/page-set.ps1",
    [string]$Readme = "README.md",
    # E9: the rule read README only, while docs/ carries the same kind of sentence. The
    # two live documents below are swept too; the round archives under docs/
    # (OPTIMIZATION-PLAN-R7..R12) are exempt on purpose, because each of their sections
    # quotes the count its own round printed - that is history, not a claim about today.
    [string]$ArchDoc = "docs/ARCHITECTURE.md",
    [string]$NovelDoc = "docs/NOVEL-READER.md",
    [string]$CiConfig = ".github/workflows/ci.yml",
    [string]$WorkDir = "target/selftest",
    [string]$SourcePng = "docs/screenshots/baseline/home-light.png",
    [int]$Tolerance = 6
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)
Add-Type -AssemblyName System.Drawing
try { [void][System.Reflection.Assembly]::Load("System.Private.Windows.GdiPlus") } catch { }

foreach ($required in @($VerifyScript, $GatesScript, $A11yScript, $FocusScript, $FixtureScript, $WalkLib, $FocusLib, $PageSetLib, $Readme, $ArchDoc, $NovelDoc, $CiConfig, $SourcePng)) {
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
$Readme = (Resolve-Path $Readme).Path
$ArchDoc = (Resolve-Path $ArchDoc).Path
$NovelDoc = (Resolve-Path $NovelDoc).Path
$CiConfig = (Resolve-Path $CiConfig).Path

# G32: the page set and its comparison are the shipped ones. Compare-PageSet reads each
# consumer's file and reports the pairs that disagree, so the case below is a live
# measurement of the tree as well as a planted experiment.
. (Resolve-Path $PageSetLib).Path

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

# ---- case locked-cargo: G19/E5 - every cargo command that resolves the graph locks ----
# Read, not run: proving it live needs a stale Cargo.lock, and Cargo.lock is not this
# script's to bend. What is decided is the shape of tools/gates.ps1 - each invocation
# that resolves the dependency graph carries --locked, so a lock file that would not
# resolve fails here exactly as it fails on a CI runner. The one exemption is `cargo
# fmt`, a rustfmt wrapper that takes no lock; it is named, not silently allowed, because
# a rule with an invisible exception is a rule that stops being read.
# The control appends the line round twelve removed, so a scan that matched nothing
# cannot satisfy the case.
function Find-UnlockedCargo {
    param([string]$Text)
    $hits = New-Object System.Collections.Generic.List[string]
    $number = 0
    foreach ($line in ($Text -split "\r?\n")) {
        $number++
        $t = $line.Trim()
        if ($t -eq "" -or $t -like "#*") { continue }
        # A gate's *name* is not a command: tools/gates.ps1 names each row after the
        # command it runs ("cargo test --workspace"), and the name carries no flags.
        # This rule therefore reads the invocation lines, which is the shape that file
        # uses throughout - one command per line, inside the block.
        if ($t -like "Invoke-Gate*") { continue }
        if ($t -notmatch 'cargo(\.exe)?["\)]*\s+(build|check|test|clippy|run)\b') { continue }
        if ($t -match '\bfmt\b') { continue }
        if ($t -match '--locked') { continue }
        $hits.Add(("line {0}: {1}" -f $number, $t))
    }
    return $hits.ToArray()
}
$gatesOnce = Get-Content $GatesScript -Raw
$unlockedNow = @(Find-UnlockedCargo -Text $gatesOnce)
$unlockedPlanted = @(Find-UnlockedCargo -Text ($gatesOnce + [Environment]::NewLine + 'cargo build --release 2>&1 | Out-Host' + [Environment]::NewLine))
$unlockedShown = if ($unlockedNow.Count) { $unlockedNow -join " | " } else { "none" }
Add-Case -Name "locked-cargo" -Wanted "every graph-resolving cargo step in tools/gates.ps1 passes --locked; fmt exempt" -Ok ($unlockedNow.Count -eq 0 -and $unlockedPlanted.Count -eq 1) -Evidence ("unlocked={0} [{1}]; planted control={2}" -f $unlockedNow.Count, $unlockedShown, $unlockedPlanted.Count)

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

# ---- case child-exit: G57 - no gate discards a child's exit code ----
# Two halves, and the second is the experiment. The static one is a rule over every
# shipped script under tools/: a line that runs a child pwsh pipeline into Out-Null has
# to be followed by a read of $LASTEXITCODE before the next two statements, or that
# child's verdict is gone. The report is per line, because "one gate throws it away" is
# only actionable with the file and the line.
# The dynamic half runs both capture verifiers against an executable that does not
# exist. No window is opened - the child fails before it can start one - so this needs
# no desktop, and each verifier has to say CAPTURE and exit 1 instead of judging the
# PNG and the sidecar the previous run left in the output directory.
function Find-UncheckedChild {
    param([string]$Text)
    $lines = $Text -split "\r?\n"
    $hits = New-Object System.Collections.Generic.List[string]
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line.Trim() -like "#*") { continue }
        if ($line -notmatch '&\s*pwsh\b') { continue }
        if ($line -notmatch '\|\s*Out-Null') { continue }
        # The next two statements, comments and blank lines skipped: the fix in
        # tools/verify-visual.ps1 carries a three-line comment before its
        # `$shotExit = $LASTEXITCODE`, and a rule that counted lines would call that
        # correct code a defect.
        $statements = 0
        $checked = $false
        for ($j = $i + 1; $j -lt $lines.Count -and $statements -lt 2 -and -not $checked; $j++) {
            $next = $lines[$j]
            if ($next.Trim() -eq "" -or $next.Trim() -like "#*") { continue }
            $statements++
            if ($next -match '\$LASTEXITCODE') { $checked = $true }
        }
        if (-not $checked) { $hits.Add(("line {0}: {1}" -f ($i + 1), $line.Trim())) }
    }
    return $hits.ToArray()
}
# This file is the one place that has to carry the shape the rule forbids - it is where
# the planted control below lives - so it is out of its own sweep. Every other shipped
# script is in it, which is what makes the rule about the gates rather than about a list.
$toolsDir = Split-Path -Parent $VerifyScript
$scanHits = @()
$selfName = Split-Path -Leaf $PSCommandPath
foreach ($script in @(Get-ChildItem (Join-Path $toolsDir "*.ps1") | Where-Object { $_.Name -notlike "_*" -and $_.Name -ne $selfName } | Sort-Object Name)) {
    foreach ($hit in @(Find-UncheckedChild -Text (Get-Content $script.FullName -Raw))) {
        $scanHits += ("{0} {1}" -f $script.Name, $hit)
    }
}
# The control: the shape the two fixed scripts used to have. One hit, named by line, so
# the rule cannot be satisfied by a scan that matches nothing.
$plantedChild = (Get-Content $GatesScript -Raw) + [Environment]::NewLine +
'& pwsh -File tools/nope.ps1 | Out-Null' + [Environment]::NewLine +
'Write-Host "kept going"' + [Environment]::NewLine
$plantedHits = @(Find-UncheckedChild -Text $plantedChild)

$childDir = Join-Path $WorkDir "child"
if (Test-Path $childDir) { Remove-Item $childDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $childDir | Out-Null
$noExe = "target\debug\nope.exe"
$layoutText = & pwsh -NoProfile -File (Join-Path $toolsDir "verify-layout.ps1") -Pages home -Sizes 1024x768 -Exe $noExe -OutDir (Join-Path $childDir "layout") 2>&1 | Out-String
$layoutCode = $LASTEXITCODE
$iconsText = & pwsh -NoProfile -File (Join-Path $toolsDir "verify-icons.ps1") -Exe $noExe -OutDir (Join-Path $childDir "icons") 2>&1 | Out-String
$iconsCode = $LASTEXITCODE
$layoutRow = @($layoutText -split "\r?\n" | Where-Object { $_ -match "CAPTURE" }) -join " / "
$iconsRow = @($iconsText -split "\r?\n" | Where-Object { $_ -match "CAPTURE" }) -join " / "
$scanShown = if ($scanHits.Count) { $scanHits -join " | " } else { "none" }
Add-Case -Name "child-exit" -Wanted "no child pipe dropped; layout and icons say CAPTURE and exit 1 without an exe" -Ok ($scanHits.Count -eq 0 -and $plantedHits.Count -eq 1 -and $layoutCode -eq 1 -and $layoutText -match "CAPTURE" -and $iconsCode -eq 1 -and $iconsText -match "CAPTURE") -Evidence ("unchecked child pipes={0} [{1}]; planted control={2}; layout exit={3} [{4}]; icons exit={5} [{6}]" -f $scanHits.Count, $scanShown, $plantedHits.Count, $layoutCode, $layoutRow.Trim(), $iconsCode, $iconsRow.Trim())


# ---- case icon-geometry: E10 - the generated icon file matches its generator ----
# ui/icons.slint is checked in and every glyph in the app draws from it, and until round
# twelve the script that emits it was referenced by nothing. -Check derives the file from
# the tracked raw SVGs (no network, no writes) and compares; this case runs it on the
# shipped file, then on a copy with one attribute changed, so "matches" cannot be a
# sentence the script prints unconditionally.
$iconsScript = Join-Path (Split-Path -Parent $VerifyScript) "fetch_icons.ps1"
$iconOut = Join-Path $WorkDir "icons"
if (Test-Path $iconOut) { Remove-Item $iconOut -Recurse -Force }
New-Item -ItemType Directory -Force -Path $iconOut | Out-Null
$iconClean = & pwsh -NoProfile -File $iconsScript -Check 2>&1 | Out-String
$iconCleanCode = $LASTEXITCODE
$iconDrift = Join-Path $iconOut "icons-drift.slint"
$iconText = Get-Content "ui/icons.slint" -Raw
$iconDrifted = $iconText.Replace("viewbox-width: 24", "viewbox-width: 25")
if ($iconDrifted -eq $iconText) {
    throw "case icon-geometry: ui/icons.slint no longer carries 'viewbox-width: 24', so the drift cannot be planted"
}
Set-Content -Path $iconDrift -Value $iconDrifted -Encoding utf8
$iconRun = & pwsh -NoProfile -File $iconsScript -Check -OutSlint $iconDrift 2>&1 | Out-String
$iconRunCode = $LASTEXITCODE
$iconRow = @($iconRun -split "\r?\n" | Where-Object { $_ -match "first difference|icons match" }) -join " / "
Add-Case -Name "icon-geometry" -Wanted "the shipped ui/icons.slint matches its generator; one changed attribute is a FAIL naming the line" -Ok ($iconCleanCode -eq 0 -and $iconClean -match "icons match" -and $iconRunCode -eq 1 -and $iconRun -match "first difference at line") -Evidence ("shipped exit={0} [{1}]; drifted copy exit={2} [{3}]" -f $iconCleanCode, $iconClean.Trim(), $iconRunCode, $iconRow.Trim())



# ---- case launch-keys: G59/E11 - one sweep of the launch keys, and it is page-set's ----
# tools/shot.ps1 used to set the page name, clear PIXIV_DESKTOP_START_ILLUST and leave the
# other three keys as it found them, so a leftover PIXIV_DESKTOP_START_NOVEL in the calling
# shell made "-Page home" photograph the reader. The fix routes the sweep through
# tools/page-set.ps1 Set-PageLaunch, which is the one function that writes or clears every
# key the shell reads.
#
# The experiment is dynamic but needs no window: an env-printing probe is spliced in front
# of the line that starts the app, so each copy reports exactly what the app would inherit.
# One copy is the shipped script; the other is the shipped script with the Set-PageLaunch
# call replaced by a no-op, which is the regression - and it has to show the leak, or the
# case would be satisfied by a probe that prints nothing.
$shotScript = Join-Path (Split-Path -Parent $VerifyScript) "shot.ps1"
$shotDir = Join-Path $WorkDir "shot"
if (Test-Path $shotDir) { Remove-Item $shotDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $shotDir | Out-Null
Copy-Item (Join-Path (Split-Path -Parent $VerifyScript) "page-set.ps1") $shotDir -Force
$spliceMarker = '$log = [System.IO.Path]::GetTempFileName()'
$probeLine = 'Write-Host ("ENV novel=[{0}] reader=[{1}] user=[{2}] illust=[{3}] page=[{4}]" -f $env:PIXIV_DESKTOP_START_NOVEL, $env:PIXIV_DESKTOP_START_READER, $env:PIXIV_DESKTOP_START_USER, $env:PIXIV_DESKTOP_START_ILLUST, $env:PIXIV_DESKTOP_START_PAGE); exit 0'
$shotText = Get-Content $shotScript -Raw
if (-not $shotText.Contains($spliceMarker)) { throw "case launch-keys: tools/shot.ps1 no longer starts the app at the GetTempFileName line, so the probe cannot be spliced" }
function Add-EnvProbe {
    param([string]$Text, [string]$Path)
    $spliced = $Text.Replace($spliceMarker, ($probeLine + [Environment]::NewLine + $spliceMarker))
    [System.IO.File]::WriteAllText($Path, $spliced, (New-Object System.Text.UTF8Encoding($false)))
}
$probeShipped = Join-Path $shotDir "shot-probe.ps1"
Add-EnvProbe -Text $shotText -Path $probeShipped
# The regression is "no sweep at all": both Set-PageLaunch calls become no-ops, which is
# what the file did before E11 in every respect that matters (it set the page name and
# cleared one key). The first replacement keeps the shape so the else branch still binds.
$regressed = $shotText.Replace('Set-PageLaunch -Page $Page', 'Write-Verbose "sweep removed"').Replace('Set-PageLaunch -Clear', 'Write-Verbose "sweep removed"')
if ($regressed -eq $shotText) { throw "case launch-keys: no Set-PageLaunch call was found in tools/shot.ps1, so the regression cannot be planted" }
$probeRegressed = Join-Path $shotDir "shot-regressed.ps1"
Add-EnvProbe -Text $regressed -Path $probeRegressed

$previousEnv = @{}
foreach ($key in @("PIXIV_DESKTOP_START_NOVEL", "PIXIV_DESKTOP_START_READER", "PIXIV_DESKTOP_START_USER", "PIXIV_DESKTOP_START_ILLUST", "PIXIV_DESKTOP_START_PAGE", "PIXIV_DESKTOP_FIXTURE")) {
    $previousEnv[$key] = [Environment]::GetEnvironmentVariable($key)
}
$noExePath = "target\debug\nope.exe"
try {
    $env:PIXIV_DESKTOP_START_NOVEL = "810000001"
    $env:PIXIV_DESKTOP_START_READER = "1"
    $env:PIXIV_DESKTOP_START_USER = "900000"
    Remove-Item Env:\PIXIV_DESKTOP_START_ILLUST -ErrorAction SilentlyContinue
    $leakText = & pwsh -NoProfile -File $probeRegressed -Page home -Mode size -Size 1024x768 -Out (Join-Path $shotDir "r.png") -Exe $noExePath 2>&1 | Out-String
    $sweptText = & pwsh -NoProfile -File $probeShipped -Page home -Mode size -Size 1024x768 -Out (Join-Path $shotDir "s.png") -Exe $noExePath 2>&1 | Out-String
    $recipeText = & pwsh -NoProfile -File $probeShipped -Page novel-reader -Mode size -Size 1024x768 -Out (Join-Path $shotDir "n.png") -Exe $noExePath 2>&1 | Out-String
}
finally {
    foreach ($key in $previousEnv.Keys) { [Environment]::SetEnvironmentVariable($key, $previousEnv[$key]) }
}
$leaks = ($leakText -match "novel=\[810000001\]") -and ($leakText -match "reader=\[1\]")
$swept = ($sweptText -match "novel=\[\]") -and ($sweptText -match "reader=\[\]") -and ($sweptText -match "user=\[\]") -and ($sweptText -match "page=\[home\]")
$recipeOk = ($recipeText -match "novel=\[810000001\]") -and ($recipeText -match "reader=\[1\]") -and ($recipeText -match "page=\[home\]")
# The static half: the script does not own a launch-key list. The only two keys it may
# write by hand are the page name it was given and the -Illust override; every other key is
# Set-PageLaunch's business. Before E11 the hand-written set was "page + illust" too, and
# the difference is that the *clearing* is now a function call rather than a list that
# only remembered one key.
$directKeys = @([regex]::Matches($shotText, '[$]env:PIXIV_DESKTOP_START_([A-Z]+)') | ForEach-Object { $_.Groups[1].Value } | Select-Object -Unique)
$oneKey = (@($directKeys | Where-Object { $_ -ne "PAGE" -and $_ -ne "ILLUST" }).Count -eq 0) -and ($shotText -match 'Set-PageLaunch')
Add-Case -Name "launch-keys" -Wanted "a leftover START_NOVEL survives the regressed script and not the shipped one; a page's own recipe still applies; one direct key" -Ok ($leaks -and $swept -and $recipeOk -and $oneKey) -Evidence ("regressed: {0} | shipped -Page home: {1} | shipped -Page novel-reader: {2} | direct launch keys in shot.ps1: [{3}]" -f $leakText.Trim(), $sweptText.Trim(), $recipeText.Trim(), ($directKeys -join ", "))


# ---- case dpi-numeric: G60/E12 - -NumericOnly can be green, and can be red ----
# The numeric half of tools/verify-dpi.ps1 needs no window, which makes it the one part of
# that gate a runner could execute - and until round twelve it could never be green: the
# mode fell through to the ladder's "every requested scale was measured" rule, a test it
# deliberately fails, so it always left by 77. The experiment is the plan's own: the same
# script with one pixel taken out of Get-TargetWidth's clamp has to fail, in the same mode.
# The mutant lives one directory under the repository root because the script re-reads
# src/layout.rs and src/images.rs relative to its parent directory - a copy anywhere else
# would fail for the wrong reason.
$dpiScript = Join-Path (Split-Path -Parent $VerifyScript) "verify-dpi.ps1"
$dpiNumberOnly = & pwsh -NoProfile -File $dpiScript -NumericOnly 2>&1 | Out-String
$dpiNumberCode = $LASTEXITCODE
$dpiMutant = Join-Path (Split-Path -Parent $WorkDir) "dpi-mutant.ps1"
try {
    $dpiText = Get-Content $dpiScript -Raw
    $clampLine = 'return [int][math]::Min([math]::Max($rounded, $floor), $cap)'
    if (-not $dpiText.Contains($clampLine)) {
        throw "case dpi-numeric: Get-TargetWidth no longer ends in '$clampLine', so the clamp cannot be shortened"
    }
    [System.IO.File]::WriteAllText($dpiMutant, $dpiText.Replace($clampLine, ($clampLine + ' - 1')), (New-Object System.Text.UTF8Encoding($false)))
    $dpiMutantText = & pwsh -NoProfile -File $dpiMutant -NumericOnly 2>&1 | Out-String
    $dpiMutantCode = $LASTEXITCODE
}
finally {
    if (Test-Path $dpiMutant) { Remove-Item $dpiMutant -Force }
}
$dpiRow = @($dpiMutantText -split "\r?\n" | Where-Object { $_ -match "below the draw size|FAIL sweep" } | Select-Object -First 2) -join " / "
Add-Case -Name "dpi-numeric" -Wanted "-NumericOnly exits 0 on the shipped clamp and 1 when the clamp is one pixel short" -Ok ($dpiNumberCode -eq 0 -and $dpiNumberOnly -match "numeric half only" -and $dpiMutantCode -eq 1 -and $dpiMutantText -match "below the draw size") -Evidence ("shipped exit={0} [{1}]; mutant exit={2} [{3}]" -f $dpiNumberCode, (@($dpiNumberOnly -split "\r?\n" | Where-Object { $_ -match "numeric half only" })[0]).Trim(), $dpiMutantCode, $dpiRow.Trim())


# ---- case rail-groups: G61 - the rail's pinned tail is not a row in the pitch it is measured by ----
# The rail is not one equidistant chain: eight destination rows sit above a stretch spacer,
# and Settings plus the collapse toggle are pinned to the bottom (ui/main.slint). Round twelve
# measured the cost of pretending otherwise - the gate reported "expected 9 rail glyphs,
# measured 10" on a rail that was correct, because it counted the pinned toggle as the
# ninth row of a 38 px pitch.
#
# The experiment is synthetic, and it has to be: the question is what the *gate* decides
# about a given arrangement of rows, so the fixture is the arrangement. Each case draws a
# 1280x800 or 900x800 client area at 1.5x - rail glyph squares at the mirrored pitch, label
# bars in the expanded column, two glyphs in the command bar's slots - writes the sidecar
# verify-icons.ps1 reads, and runs the shipped gate with -SkipCapture. No window, no
# pointer, no dependency on an untracked capture.
function New-RailCapture {
    param(
        [Parameter(Mandatory)][string]$Dir,
        [double]$LogicalW = 1280,
        [double]$LogicalH = 800,
        [int]$MainRows = 8,
        [int]$TailGlyphs = 2,
        [int]$TailLabels = 2,
        [switch]$NoLabels,
        [int]$ShiftMainRow = -1,
        [double]$ShiftLogical = 0
    )
    $scale = 1.5
    $compact = $LogicalW -lt 1000
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    $w = [int][math]::Round($LogicalW * $scale)
    $h = [int][math]::Round($LogicalH * $scale)
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $graphics = [System.Drawing.Graphics]::FromImage($bmp)
    $graphics.Clear([System.Drawing.Color]::White)
    $ink = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::Black)
    $toPx = { param([double]$v) [int][math]::Round($v * $scale) }
    # The glyph column the gate scans: expanded is a 16 px glyph at x 20..36, compact is the
    # same glyph centred in a 52 px pane (12..40). Centred here too, so the spread is 0.
    $glyphCx = if ($compact) { 26.0 } else { 28.0 }
    $labelLeft = 48.0
    $glyphEdge = 7.0
    # Main rows: item height 36 + spacing 2, first item top = topbar 48 + rail padding 8.
    $mainCy = @()
    for ($k = 0; $k -lt $MainRows; $k++) { $mainCy += (74.0 + 38.0 * $k) }
    # Pinned rows: bottom-up from the status bar and the rail's own bottom padding.
    $tailCy = @()
    for ($k = 0; $k -lt [math]::Max($TailGlyphs, $TailLabels); $k++) { $tailCy += ($LogicalH - 52.0 - 38.0 * $k) }
    $tailCy = @($tailCy | Sort-Object)
    for ($k = 0; $k -lt $mainCy.Count; $k++) {
        $cy = $mainCy[$k]
        if ($k -eq $ShiftMainRow) { $cy += $ShiftLogical }
        $graphics.FillRectangle($ink, (& $toPx ($glyphCx - $glyphEdge)), (& $toPx ($cy - $glyphEdge)), (& $toPx (2 * $glyphEdge)), (& $toPx (2 * $glyphEdge)))
        if (-not $NoLabels) { $graphics.FillRectangle($ink, (& $toPx $labelLeft), (& $toPx ($mainCy[$k] - 5)), (& $toPx 60), (& $toPx 10)) }
    }
    for ($k = 0; $k -lt $tailCy.Count; $k++) {
        # The tail is drawn top-down, so $tailCy[0] is Settings and the last entry is the
        # toggle. Omitting the last glyph is therefore "the toggle draws no glyph", which is
        # the scenario the case is about - not "a pinned row vanished".
        $cy = $tailCy[$k]
        if ($k -lt $TailGlyphs) { $graphics.FillRectangle($ink, (& $toPx ($glyphCx - $glyphEdge)), (& $toPx ($cy - $glyphEdge)), (& $toPx (2 * $glyphEdge)), (& $toPx (2 * $glyphEdge))) }
        if (-not $NoLabels -and $k -lt $TailLabels) { $graphics.FillRectangle($ink, (& $toPx $labelLeft), (& $toPx ($cy - 5)), (& $toPx 60), (& $toPx 10)) }
    }
    # The command bar's two slots, on the bar's centre line (24 logical).
    $graphics.FillRectangle($ink, (& $toPx 56.0), (& $toPx 16.0), (& $toPx 16), (& $toPx 16))
    $graphics.FillRectangle($ink, (& $toPx 92.0), (& $toPx 16.0), (& $toPx 16), (& $toPx 16))
    $graphics.Dispose()
    $ink.Dispose()
    $size = "{0}x{1}" -f [int]$LogicalW, [int]$LogicalH
    $png = Join-Path $Dir ("icons-{0}.png" -f $size)
    $bmp.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    $meta = [ordered]@{
        trace = $null; page = "home"; mode = "size"; dark = $false; scale = $scale
        width = $w; height = $h; logicalW = [double]$LogicalW; logicalH = [double]$LogicalH
        clientX = 0; clientY = 0; screenLeft = 0; screenTop = 0; screenW = 2560; screenH = 1600
    }
    ($meta | ConvertTo-Json -Compress) | Set-Content -Path ([System.IO.Path]::ChangeExtension($png, ".json")) -Encoding utf8
    return [pscustomobject]@{ Dir = $Dir; Size = $size; Png = $png }
}

$iconsRail = Join-Path (Split-Path -Parent $VerifyScript) "verify-icons.ps1"
$railRoot = Join-Path $WorkDir "rail"
if (Test-Path $railRoot) { Remove-Item $railRoot -Recurse -Force }
function Invoke-RailGate {
    param([pscustomobject]$Case)
    $text = & pwsh -NoProfile -File $iconsRail -SkipCapture -Size $Case.Size -OutDir $Case.Dir 2>&1 | Out-String
    return [pscustomobject]@{ Code = $LASTEXITCODE; Output = $text }
}
function Get-RailLine {
    param([string]$Text, [string]$Pattern)
    return (@($Text -split "\r?\n" | Where-Object { $_ -match $Pattern } | Select-Object -First 1) -join "").Trim()
}
$railExpanded = New-RailCapture -Dir (Join-Path $railRoot "expanded") -TailGlyphs 2 -TailLabels 2
$runExpanded = Invoke-RailGate -Case $railExpanded
$railNoToggle = New-RailCapture -Dir (Join-Path $railRoot "no-toggle") -TailGlyphs 1 -TailLabels 2
$runNoToggle = Invoke-RailGate -Case $railNoToggle
$railShifted = New-RailCapture -Dir (Join-Path $railRoot "shifted") -TailGlyphs 2 -TailLabels 2 -ShiftMainRow 3 -ShiftLogical 3
$runShifted = Invoke-RailGate -Case $railShifted
$railCompact = New-RailCapture -Dir (Join-Path $railRoot "compact") -LogicalW 900 -TailGlyphs 1 -TailLabels 0 -NoLabels
$runCompact = Invoke-RailGate -Case $railCompact
$railCompactToggle = New-RailCapture -Dir (Join-Path $railRoot "compact-toggle") -LogicalW 900 -TailGlyphs 2 -TailLabels 0 -NoLabels
$runCompactToggle = Invoke-RailGate -Case $railCompactToggle
# Expanded: 8 above the spacer + Settings + the toggle, all from ui/main.slint.
# No toggle glyph: the tail has one row where the source says two - named by the tail assertion.
# Row 3 three logical pixels lower: the pitch in the main group is the assertion that owns it.
# Compact: the toggle is not drawn at all when the pane is narrow, so the tail is Settings alone.
# Compact with a toggle drawn anyway: the tail has one row too many.
$railOk = ($runExpanded.Code -eq 0) -and
    ($runExpanded.Output -match "expect 8 above the spacer \+ 2 pinned") -and
    ($runExpanded.Output -match "8\+2") -and
    ($runNoToggle.Code -eq 1) -and ($runNoToggle.Output -match "rail pinned tail: expected 2 glyph\(s\), measured 1") -and
    ($runShifted.Code -eq 1) -and ($runShifted.Output -match "rail main group: pitch between row 3 and row 4") -and
    ($runCompact.Code -eq 0) -and ($runCompact.Output -match "expect 8 above the spacer \+ 1 pinned") -and
    ($runCompactToggle.Code -eq 1) -and ($runCompactToggle.Output -match "rail pinned tail: expected 1 glyph\(s\), measured 2")
Add-Case -Name "rail-groups" -Wanted "expanded 8+2 and compact 8+1 pass; a missing toggle glyph and a 3 px row offset each fail by name" -Ok $railOk -Evidence ("expanded exit={0} [{1}]; no toggle glyph exit={2} [{3}]; row 3 shifted exit={4} [{5}]; compact exit={6} [{7}]; compact with toggle exit={8} [{9}]" -f
    $runExpanded.Code, (Get-RailLine $runExpanded.Output "rail source"), $runNoToggle.Code, (Get-RailLine $runNoToggle.Output "pinned tail"),
    $runShifted.Code, (Get-RailLine $runShifted.Output "pitch between"), $runCompact.Code, (Get-RailLine $runCompact.Output "rail source"),
    $runCompactToggle.Code, (Get-RailLine $runCompactToggle.Output "pinned tail"))

# ---------------------------------------------------------------- round nine

<#
  A recorded a11y measurement shaped like today's bookmarks page: 31 focusable
  Button/ListItem/Edit stops, two pairs of them sharing a name, and a walk that stopped
  after -WalkedCount of them. This is what tools/verify-a11y.ps1 -Replay asserts
  against, so the gate's own failure path runs with no window, no desktop and no app -
  which is also what lets the CI job (G25) run this file at all.
#>
function New-A11yFixture {
    # -WalkEnd "" omits the field, which is the shape every fixture written before
    # round twelve has - and G58 (E4) is about what the reader does with that shape.
    param([string]$Name, [int]$WalkedCount, [string]$WalkEnd = "cycle")
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
        required  = $needStops
        walked    = @($needStops | Select-Object -First $WalkedCount)
    }
    if ($WalkEnd) { $page["walkEnd"] = $WalkEnd }
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

# ---- case a11y-skip: G58 - a skip is not a pass, and neither is a walk that never cycled ----
# Both halves are shipped code. The predicates are tools/walk-lib.ps1's, which is what
# tools/verify-a11y.ps1 applies to what it measured, so the experiment drives the rule
# itself rather than a copy of it: two pages asked for, one measured, one with a recorded
# reason - the exact shape that printed the budget sentence and exited 0 before.
# The replay half is the other direction: a recorded measurement whose walk did not come
# back round (and a fixture that predates the field, which must not be read as a cycle)
# has to fail and name what ended the walk.
$partialPages = @(Get-UnmeasuredFailure -Pages @("home", "settings") -Results @([pscustomobject]@{ page = "home" }) -Skips @("settings: no binary at target\debug\pixiv-desktop.exe"))
$wholePages = @(Get-UnmeasuredFailure -Pages @("home") -Results @([pscustomobject]@{ page = "home" }) -Skips @())
$cappedWalk = @(Get-WalkEndFailure -Page "home" -WalkEnd "cap")
$cyclicWalk = @(Get-WalkEndFailure -Page "home" -WalkEnd "cycle")
$noCycle = New-A11yFixture -Name "walk-nocycle" -WalkedCount 31 -WalkEnd ""
$runNoCycle = Invoke-A11yReplay -Case $noCycle
$rowNoCycle = @($runNoCycle.Output -split "\r?\n" | Where-Object { $_ -match "reached=|ended at" }) -join " / "
# The wiring, read rather than assumed: the rules above are only worth their text if the
# gate applies them. Reading the script is the same shape case focus-full uses, because
# the live half - a page that could not be measured beside one that could - needs a
# window, and that experiment is the Lead's Phase 3 run.
$a11yWired = ((Get-Content $A11yScript -Raw) -match 'Get-UnmeasuredFailure') -and ((Get-Content $A11yScript -Raw) -match 'Get-WalkEndFailure')
Add-Case -Name "a11y-skip" -Wanted "one of two pages measured is a failure naming settings; a walk with no cycle fails and names cap" -Ok ($partialPages.Count -eq 1 -and $partialPages[0] -match "settings" -and $partialPages[0] -match "no binary" -and $wholePages.Count -eq 0 -and $cappedWalk.Count -eq 1 -and $cyclicWalk.Count -eq 0 -and $runNoCycle.Code -eq 1 -and $runNoCycle.Output -match "ended at 'cap'" -and $a11yWired) -Evidence ("2 asked/1 measured -> [{0}]; both measured -> {1}; cap -> {2}; cycle -> {3}; replay without walkEnd: exit={4} {5}; wired={6}" -f ($partialPages -join " / "), $wholePages.Count, $cappedWalk.Count, $cyclicWalk.Count, $runNoCycle.Code, $rowNoCycle.Trim(), $a11yWired)

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
    [pscustomobject]@{ Name = "Search";     Verdict = (Get-FocusVerdict -RingPixels 0 -Alpha 0.0 -AccentPixels 40 -Type "Edit" -MinPixels 6) },
    # The window root, planted with the numbers round nine's full walk measured on it:
    # no ring, some accent pixels from the search field inside it. It must NOT be
    # invisible, or every page with a full walk fails for a node nobody can focus.
    [pscustomobject]@{ Name = "Window";     Verdict = (Get-FocusVerdict -RingPixels 0 -Alpha 0.0 -AccentPixels 1629 -Type "Window" -MinPixels 6) }
)
$invisible = @(Get-InvisibleStops -Stops $planted)
$focusText = Get-Content $FocusScript -Raw
$wired = (($focusText -match "Get-WalkLimit") -and ($focusText -match "Get-FocusVerdict") -and ($focusText -match "Get-InvisibleStops") -and ($focusText -match "s total"))
Add-Case -Name "focus-full" -Wanted "home walks past 10, others keep 10, planted stop NONE, duration printed" -Ok ($limitFull -gt 10 -and $limitOld -eq 10 -and $limitRest -eq 10 -and $invisible.Count -eq 1 -and $planted[1].Verdict -eq "NONE" -and $planted[0].Verdict -eq "RING" -and $planted[2].Verdict -eq "ACCENT" -and $planted[3].Verdict -eq "WINDOW" -and $wired) -Evidence ("full={0} old={1} other={2}; verdicts={3}; invisible={4}; wired={5}" -f $limitFull, $limitOld, $limitRest, (($planted | ForEach-Object { $_.Verdict }) -join ","), $invisible.Count, $wired)

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

# ---- case gate-independence: G37 - no gate inherits another gate's verdict ----
# Round ten measured the hole: with verify-visual failing on one capture state, the four
# gates after it never ran and the run printed "6 gates, 1 failed" for a mode that owns
# ten rows. This case reads the shape of tools/gates.ps1 rather than running it, because
# proving it live needs a deliberately failing gate - which is the gate's own job, not
# this script's. The negative control is the same predicate applied to the guard re-planted
# into a copy: a filter that matched nothing would satisfy the assertion without deciding
# anything, and this is the line that stops that.
function Get-GateGuards {
    param([string]$Text)
    # A comment that quotes the guard is not a guard: the header explains the defect by
    # naming it, so comment lines are excluded before the pattern is applied.
    return @($Text -split "\r?\n" | Where-Object {
        ($_.Trim() -notlike "#*") -and ($_ -match 'if\s*\(\s*-not\s+\$failed|-not\s+\$failed\s*-and')
    })
}
$gatesText = Get-Content $GatesScript -Raw
$guardsNow = @(Get-GateGuards -Text $gatesText).Count
$plantedGuard = $gatesText + [Environment]::NewLine + 'if (-not $failed) { }' + [Environment]::NewLine
$guardsPlanted = @(Get-GateGuards -Text $plantedGuard).Count
# The summary has to count rows attempted, not rows reached: "6 gates" after a failure at
# row five reads as a six-gate suite that was satisfied.
$countsAttempts = ($gatesText -match 'gates attempted')
Add-Case -Name "gate-independence" -Wanted "no gate guarded by a previous verdict; summary counts attempts" -Ok ($guardsNow -eq 0 -and $guardsPlanted -eq 1 -and $countsAttempts) -Evidence ("guards in tools/gates.ps1={0}; planted control={1}; summary says attempted={2}" -f $guardsNow, $guardsPlanted, $countsAttempts)

# ---- case page-set: G32 - five lists, one set ----
# Two halves, and the second is the experiment the plan names. The live tree is measured
# first: all four verifiers derive their list from tools/page-set.ps1 and the budget is
# keyed by the accessibility set, so the comparison reports nothing. Then one consumer is
# handed a literal list with one page deleted from it - "delete settings from
# verify-a11y.ps1 only", in the shape the file would have if somebody stopped reading the
# set - and the same shipped parser has to report the pair ("a11y\budget") and the page
# ("settings"). Feeding the untouched text back has to be clean again, so the case cannot
# be satisfied by a comparison that always finds something.
$agreeNow = @(Compare-PageSet)
$a11yText = Get-Content $A11yScript -Raw
# A literal replacement, not -replace: the planted text is a PowerShell expression and
# its parentheses are regex metacharacters. The whole derivation goes, list included, so
# what the parser sees is exactly "this file carries its own list".
$plantedA11y = $a11yText.Replace('@(Get-PageSet -For a11y)', '@("home","discovery","ranking","search","bookmarks","history","downloads")')
if ($plantedA11y -eq $a11yText) {
    throw "case page-set: $A11yScript no longer derives its list with Get-PageSet -For a11y, so the defect cannot be planted"
}
$plantedDiffs = @(Compare-PageSet -Text @{ a11y = $plantedA11y })
$revertedDiffs = @(Compare-PageSet -Text @{ a11y = $a11yText })
$namesPair = @($plantedDiffs | Where-Object { $_.Pair -eq "a11y\budget" -and $_.Message -match "'settings'" }).Count -gt 0
# G56 (E6): the other half of "the page set agrees with itself" - a page the capture set
# photographs may not be excluded from every measurement set. Live, that is the shape
# round twelve fixed (detail had a tracked baseline and no accessibility floor); the
# plant hands the pre-E6 exclusion list back to the shipped predicate, so a re-merge of
# the two lists is a FAIL that names the page, not a list that quietly agrees with
# itself again.
$completeNow = @(Test-PageSetCompleteness)
$completePlanted = @(Test-PageSetCompleteness -Unmeasured @("detail", "user-profile"))
$detailMeasured = ((Get-PageSet -For a11y) -contains "detail") -and ((Get-PageSet -For layout) -contains "detail") -and ((Get-PageSet -For budget) -contains "detail")
$plantedShown = ($plantedDiffs | ForEach-Object { $_.Message }) -join " / "
$agreeShown = if ($agreeNow.Count) { ($agreeNow | ForEach-Object { $_.Message }) -join " / " } else { "none" }
$completePlantedShown = if ($completePlanted.Count) { $completePlanted -join " / " } else { "none" }
Add-Case -Name "page-set" -Wanted "five sets agree; planted a11y list names a11y\budget + settings; a photographed page is never unmeasured" -Ok ($agreeNow.Count -eq 0 -and $namesPair -and $revertedDiffs.Count -eq 0 -and $completeNow.Count -eq 0 -and $completePlanted.Count -eq 1 -and $detailMeasured) -Evidence ("agree now={0} [{1}]; planted difference=[{2}]; reverted={3}; completeness now={4}; pre-E6 exclusion planted=[{5}]; detail in a11y/layout/budget={6}" -f $agreeNow.Count, $agreeShown, $plantedShown, $revertedDiffs.Count, $completeNow.Count, $completePlantedShown, $detailMeasured)

<#
  A number a gate prints, restated in prose. Six of them exist and each has one owner: the
  test count (cargo test), the uiaudit rule count (src/uiaudit.rs, printed by its own
  test), the capture count (tools/verify-visual.ps1 prints "N captures within ..."), the
  layout check count (tools/verify-layout.ps1 prints "N checks, M not OK"), the size count
  the same run prints, and the point count of tools/verify-dpi.ps1's numeric sweep. README
  stated two of them - "diffs 20 forced-demo captures" and "24 checks: 8 pages x 3 sizes" -
  and both had been wrong for rounds, because nothing compared them with the run. Round
  twelve found a third, "1060 points", in the same state.

  The rule is narrow on purpose: prose carries none of the four. A gate cannot know the
  live count without running the thing it counts, so "update the number" is not a fix that
  stays fixed - the run prints its own count and the README says so, which is what
  README.md:229 already does for the captures. One hit per (line, count), so a planted
  sentence that states all four names its line four times instead of once.
#>
function Find-ProseNumber {
    param([string]$Text)
    $patterns = @(
        @{ Kind = "capture count"; Pattern = '\b\d+\s+(?:[A-Za-z-]+\s+){0,2}captures?\b' }
        @{ Kind = "layout check count"; Pattern = '\b\d+\s+checks?\b' }
        @{ Kind = "test count"; Pattern = '\b\d+\s+tests?\b' }
        @{ Kind = "uiaudit rule count"; Pattern = '\b\d+\s+(?:uiaudit\s+)?rules?\b' }
        # A fifth, for the same reason: verify-a11y prints how many pages met the budget
        # and verify-layout's check count is pages x sizes, so "N pages" restates a number
        # a run already printed. Digits only, on purpose - "three novel captures" is prose
        # about three pictures, not a measurement, and a rule that models English numerals
        # is a rule nobody can satisfy. The two word-number restatements that had survived
        # in README were removed by hand; a digit one cannot come back.
        @{ Kind = "page count"; Pattern = '\b\d+\s+pages?\b' }
        # Round twelve (E8) adds two more on the strength of the same question: does any
        # prose sentence restate a number a run prints? tools/verify-dpi.ps1 prints the
        # size of its numeric sweep on every run ("N sampled rows, M-point sweep"), and
        # tools/verify-layout.ps1 prints the sizes it walked, so "1060 points" and
        # "8 pages x 3 sizes" are the same class of claim as "24 checks" was.
        @{ Kind = "sweep point count"; Pattern = '\b\d+\s*-?\s*points?\b' }
        @{ Kind = "size count"; Pattern = '\b\d+\s+sizes?\b' }
    )
    $hits = New-Object System.Collections.Generic.List[object]
    $number = 0
    foreach ($line in ($Text -split "\r?\n")) {
        $number++
        foreach ($p in $patterns) {
            if ($line -match $p.Pattern) {
                $hits.Add([pscustomobject]@{ Line = $number; Kind = $p.Kind; Text = $line.Trim() })
            }
        }
    }
    return $hits.ToArray()
}

# ---- case prose-numbers: G35/E9 - no gate-printed number is restated in the swept docs ----
# The live half reads every document the rule owns - README.md and the two live documents
# under docs/ (E9: the rule read README only, so a restatement in ARCHITECTURE.md was
# invisible), and reports the file as well as the line. The experiment appends the
# sentence the plan names to README and requires the same function to name the line it
# landed on; a second plant puts the same sentence into ARCHITECTURE.md, which is the file
# the rule could not read at all before E9. "Name the file and the line" is the whole
# difference between a rule somebody can act on and a count of hits.
$proseDocs = @(
    [pscustomobject]@{ Name = "README.md"; Text = (Get-Content $Readme -Raw); Raw = (Get-Content $Readme -Raw) },
    [pscustomobject]@{ Name = "docs/ARCHITECTURE.md"; Text = (Get-Content $ArchDoc -Raw); Raw = (Get-Content $ArchDoc -Raw) },
    [pscustomobject]@{ Name = "docs/NOVEL-READER.md"; Text = (Get-Content $NovelDoc -Raw); Raw = (Get-Content $NovelDoc -Raw) }
)
$proseNow = @()
foreach ($doc in $proseDocs) {
    foreach ($hit in @(Find-ProseNumber -Text $doc.Text)) {
        $proseNow += [pscustomobject]@{ Doc = $doc.Name; Line = $hit.Line; Kind = $hit.Kind; Text = $hit.Text }
    }
}
$readmeText = (Get-Content $Readme -Raw)
# The second plant goes into a document the rule could not read at all before E9. Its
# purpose is not the count - three of the six patterns match that sentence - but that the
# sweep reaches this file: git shows HEAD~1's case reading $Readme and nothing else.
$archRestatement = @(Find-ProseNumber -Text ((Get-Content $ArchDoc -Raw) + [Environment]::NewLine + "The DPI gate sweeps 1060 points at 8 pages x 3 sizes."))
$archName = $archRestatement.Count
$archUnder = ($archName -gt 0)
$plantedProse = $readmeText + [Environment]::NewLine + "The suite runs 327 tests and 17 uiaudit rules, diffs 20 forced-demo captures, and reports 24 checks: 8 pages x 3 sizes."
$prosePlanted = @(Find-ProseNumber -Text $plantedProse)
# The sentence lands on the last line, so that is the line number the four hits have to
# carry - and every hit above it has to be one the live README produced, which is how the
# case tells "the line was named" apart from "the count changed".
$plantedLine = ($plantedProse -split "\r?\n").Count
$onPlantedLine = @($prosePlanted | Where-Object { $_.Line -eq $plantedLine })
$lineReported = ($onPlantedLine.Count -ge 6) -and (@($prosePlanted | Where-Object { $_.Line -ne $plantedLine }).Count -eq $proseNow.Count)
$liveShown = if ($proseNow.Count) { ($proseNow | ForEach-Object { "{0}:{1} {2}" -f $_.Doc, $_.Line, $_.Text }) -join " / " } else { "none" }
Add-Case -Name "prose-numbers" -Wanted "none of the three swept documents states a gate-printed number; a planted line is named by file and line" -Ok ($proseNow.Count -eq 0 -and $lineReported -and $archUnder) -Evidence ("live restatements={0} in {1} documents [{2}]; planted in README={3} hit(s), {4} on line {5}; planted in docs/ARCHITECTURE.md={6} (the README-only sweep could not see it)" -f $proseNow.Count, $proseDocs.Count, $liveShown, $prosePlanted.Count, $onPlantedLine.Count, $plantedLine, $archName)

Write-Host ""
$bad = @($cases | Where-Object { -not $_.Ok })
$colour = if ($bad.Count) { "Red" } else { "Green" }
Write-Host ("{0} cases, {1} failed (fixtures in {2})" -f $cases.Count, $bad.Count, $WorkDir) -ForegroundColor $colour
if ($bad.Count) { exit 1 }
exit 0
