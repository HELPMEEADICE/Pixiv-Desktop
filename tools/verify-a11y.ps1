# Accessibility gate: what the app exposes to assistive technology, measured.
#
# The app's widgets were built for the pointer. This script reads the UI
# Automation tree the way a screen reader or a Windows accessibility tool sees it
# — control roles, names, and what can be invoked — and then walks the keyboard
# with real Tab keystrokes, recording which element ends up focused each time.
#
# Before round five the tree held 47 Text nodes, 16 Image nodes and one Edit: no
# Button, no ListItem, no InvokePattern anywhere, and exactly one keyboard-focusable
# element. A gate that only counted nodes would have been happy with that, so the
# numbers the budget pins are about *semantics*: how many things can be invoked,
# how many distinct stops Tab reaches, and whether a semantic role exists at all.
#
# G21 (round nine): the walk identifies a stop by (Name, Type, rectangle), not by
# (Name, Type). The old rule ended the walk at the *second* control with a repeated
# name and then printed cycle=yes - the walk had come back round, to the wrong stop.
# Measured today: bookmarks exposes 31 focusable elements and walked 24, search 22/21,
# history 15/14, downloads 19/18, so four pages were short by exactly the duplicates
# they contained. A second assertion covers what "reached" has to mean: every
# focusable Button, ListItem and Edit in the tree must have been visited by that walk,
# or the page is a FAIL that names the control (tools/walk-lib.ps1,
# Compare-StopCoverage). Neither rule is accepted on this script passing: both are
# re-run headlessly, against the old rule as the control, by tools/selftest-gates.ps1.
#
# Usage:
#   pwsh -File tools/verify-a11y.ps1 -Report        # measure, never fail
#   pwsh -File tools/verify-a11y.ps1                # measure and assert
#   pwsh -File tools/verify-a11y.ps1 -Update        # re-pin the budget to today
#   pwsh -File tools/verify-a11y.ps1 -Pages home,settings -Dark
#   pwsh -File tools/verify-a11y.ps1 -Replay walk.json   # assert a recorded
#                                                   # measurement: no window, no
#                                                   # desktop, so a runner can (G25)
param(
    # G32: empty means "ask tools/page-set.ps1 for the accessibility selection", which is
    # resolved below; an explicit -Pages, including an empty one, still wins.
    [string[]]$Pages = @(),
    [string]$Size = "1024x768",
    [string]$Exe = "target\debug\pixiv-desktop.exe",
    [string]$Budget = "tools\a11y-budget.json",
    [string]$OutDir = "target\a11y",
    # G5: the walk used to be exactly this many presses, and -Update then pinned that
    # same number as the requirement - the budget and the sample were one number, so
    # the gate could not see a stop it never walked. The walk now runs until the
    # (Name, Type, rectangle) identity it reads repeats (that is the cycle); this is
    # only the safety cap for a page whose focus never comes back round - a run that
    # ends on the cap is printed as cycle=cap, never as cycle=yes. -TabSteps is kept as
    # an alias so a documented invocation still binds.
    [Alias("TabSteps")]
    [int]$MaxTabSteps = 200,
    [switch]$Dark,
    [switch]$SkipTab,
    [switch]$Report,
    [switch]$Update,
    # G21/G25: a recorded measurement, as JSON, instead of a live window. This is how
    # the two assertions above are re-run where there is no desktop (the selftest, and
    # so the CI job that runs it). The file carries a *measurement* - the tree counts,
    # the focusable stops, the stops that were walked - and every comparison is still
    # made below, by this script: a fixture cannot pre-compute a verdict.
    [string]$Replay = ""
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)
. (Join-Path $PSScriptRoot "drive.ps1") | Out-Null
# G21: the stop identity, the walk engine and the coverage check live in one file that
# tools/verify-focus.ps1 walks with too, and that the selftest can drive without a
# window.
# G32: the page list, the launch recipe for each page and the budget key set all come
# from here, so this script cannot drift from the three other verifiers or from
# tools/a11y-budget.json.
. (Join-Path $PSScriptRoot "walk-lib.ps1") | Out-Null
. (Join-Path $PSScriptRoot "page-set.ps1") | Out-Null
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# G32: eleven pages, one of them which this script has never measured: the three novel
# destinations joined the accessibility set with the page set itself. An explicit -Pages
# overrides it, and an empty one is still "measure nothing" (the 77 below).
if (-not $PSBoundParameters.ContainsKey("Pages")) { $Pages = @(Get-PageSet -For a11y) }
$Pages = @($Pages | ForEach-Object { $_ -split "," } | ForEach-Object { $_.Trim() } | Where-Object { $_ })

function Stop-App {
    Get-Process pixiv-desktop -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400
}

# Pages this host could not measure. G18: the gate had no "cannot run here" path at
# all - it threw when no window appeared, and tools/gates.ps1 reports a throw as FAIL
# in every case. 77 means SKIP (no desktop, no window, UI Automation silent), and a
# run in which *no* page produced a measurement is that case, not a pass and not a
# crash.
$skips = @()

function Measure-Page {
    param([string]$Page)

    # G32: a name the page set does not know has no recipe, and the shell would fall back
    # to the home page - a photograph of home reported under another page's name. Name it
    # and measure nothing instead.
    if ((Get-PageLaunch -Page $Page).Count -eq 0) {
        $script:skips += ("{0}: no launch recipe in tools/page-set.ps1" -f $Page)
        return $null
    }
    if (-not (Test-Path $Exe)) {
        $script:skips += ("{0}: no binary at {1}" -f $Page, $Exe)
        return $null
    }
    Stop-App
    $env:PIXIV_DESKTOP_DEMO = "1"
    # G32: one page, one recipe. Set-PageLaunch writes every deep-link key the shell
    # reads and removes the rest, so the illust/user/novel link a previous page set
    # cannot survive into this measurement - the reader page is opened by
    # PIXIV_DESKTOP_START_NOVEL plus START_READER, and nothing here has to know that.
    Set-PageLaunch -Page $Page
    $env:PIXIV_DESKTOP_SIZE = $Size
    Remove-Item Env:\PIXIV_DESKTOP_TIP -ErrorAction SilentlyContinue
    if ($Dark) { $env:PIXIV_DESKTOP_THEME = "dark" } else { Remove-Item Env:\PIXIV_DESKTOP_THEME -ErrorAction SilentlyContinue }

    $proc = Start-Process -FilePath (Resolve-Path $Exe).Path -PassThru
    try {
        # `MainWindowHandle` is not "the window the user sees": winit creates a hidden
        # "Thread Event Target" window before the real one, and polling only for a
        # non-zero handle therefore returns that one. Measured: the first version of
        # this script read an empty tree from class 'Winit Thread Event Target' while
        # the app was perfectly healthy. Wait for the titled window instead.
        for ($i = 0; $i -lt 60; $i++) {
            Start-Sleep -Milliseconds 250
            $proc.Refresh()
            if ($proc.MainWindowHandle -ne [IntPtr]::Zero -and $proc.MainWindowTitle -eq "Pixiv Desktop") { break }
        }
        if ($proc.MainWindowHandle -eq [IntPtr]::Zero -or $proc.MainWindowTitle -ne "Pixiv Desktop") {
            $script:skips += ("{0}: no titled window appeared in 15s" -f $Page)
            return $null
        }
        [void][Drive32]::SetForegroundWindow($proc.MainWindowHandle)
        Start-Sleep -Milliseconds 1800

        # Slint publishes its tree through accesskit, which answers the OS the first
        # time it is asked rather than at map time. A single FindAll right after the
        # window appears can therefore come back empty — measured, not assumed: the
        # first run of this script reported nodes=0 for all eight pages.
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($proc.MainWindowHandle)
        $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        for ($i = 0; $i -lt 15 -and $all.Count -eq 0; $i++) {
            Start-Sleep -Milliseconds 400
            $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        }
        if ($all.Count -eq 0) {
            $script:skips += ("{0}: no accessibility tree (root class '{1}', title '{2}')" -f $Page, $root.Current.ClassName, $proc.MainWindowTitle)
            return $null
        }

        $byType = @{}
        $focusable = 0
        $invokable = 0
        $named = 0
        $invokableNames = @()
        # G21: the set a keyboard user has to be able to *act on*, collected while the
        # tree is already in hand. Only Button, ListItem and Edit - the roles this shell
        # gives an action or a cursor; a focusable Text node is a label the tree made
        # reachable, not a control somebody has to go and find. This count is the
        # denominator of the assertion ("31 focusable, 24 visited"), and the rectangle is
        # deliberately not used for matching - see Compare-StopCoverage.
        $anchorRoles = @("Button", "ListItem", "Edit")
        $required = New-Object System.Collections.Generic.List[object]
        foreach ($el in $all) {
            $c = $el.Current
            $type = $c.ControlType.ProgrammaticName -replace "ControlType\.", ""
            if ($byType.ContainsKey($type)) { $byType[$type]++ } else { $byType[$type] = 1 }
            if ($c.IsKeyboardFocusable) {
                $focusable++
                if ($anchorRoles -contains $type) {
                    $rect = $c.BoundingRectangle
                    $required.Add([pscustomobject]@{
                            Name   = $c.Name
                            Type   = $type
                            X      = [int]$rect.X
                            Y      = [int]$rect.Y
                            Width  = [int]$rect.Width
                            Height = [int]$rect.Height
                        })
                }
            }
            if ($c.Name) { $named++ }
            $pattern = $null
            if ($el.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
                $invokable++
                if ($c.Name) { $invokableNames += $c.Name }
            }
        }

        $tab = @()
        $walk = $null
        if (-not $SkipTab) {
            [void](Focus-AppWindow -ProcessName "pixiv-desktop")
            Start-Sleep -Milliseconds 200
            # A Tab key goes to whatever window is in front, which is not
            # necessarily the one under test: a stray keystroke lands in the
            # terminal that started this script. Measure the foreground window
            # first and refuse to type into anything else.
            $foreground = [Drive32]::GetForegroundWindow()
            if ($foreground -ne $proc.MainWindowHandle) {
                # Cannot run here, for this page: a Tab walk typed into another window
                # measures that window. SKIP, named, instead of an exception.
                $script:skips += ("{0}: refused to synthesise Tab - foreground window {1} is not the app ({2})" -f $Page, $foreground, $proc.MainWindowHandle)
                return $null
            }
            # G5: walk until the identity a stop is read by repeats - Tab has come back
            # round to a stop the walk already saw - so every stop the page has is in the
            # list when the loop ends. The old fixed ten presses made the sample size and
            # the requirement the same number: the budget pinned 10 on every page while 14
            # to 44 things were focusable.
            #
            # G21: that identity is (Name, Type, rectangle) now. With (Name, Type) alone
            # the walk stopped at the second control sharing a name with an earlier one
            # and reported cycle=yes - true, and useless: bookmarks has two controls
            # called "收藏", and the stops after the second of them were never visited
            # (31 focusable, 24 walked). The engine lives in tools/walk-lib.ps1 so the
            # selftest can run this exact rule against the old one without a window.
            $walk = Measure-TabWalk -MaxSteps $MaxTabSteps -NextStop {
                Send-TabKey -Count 1
                $f = Get-FocusedInfo
                [pscustomobject]@{
                    Name   = $f.Name
                    Type   = $f.Type
                    X      = $f.X
                    Y      = $f.Y
                    Width  = $f.Width
                    Height = $f.Height
                }
            }
            $tab = @($walk.Stops | ForEach-Object { if ($_.Name) { $_.Name } else { "<$($_.Type)>" } })
        }

        # G21's second half: what the walk reached, against what the tree says is
        # focusable. $null when -SkipTab skipped the walk - the assertion block turns
        # that into a named failure, because "not measured" is not a pass.
        $coverage = if ($null -ne $walk) { Compare-StopCoverage -Required $required.ToArray() -Walked $walk.Stops.ToArray() } else { $null }

        return [pscustomobject]@{
            page           = $Page
            total          = $all.Count
            byType         = $byType
            focusable      = $focusable
            invokable      = $invokable
            named          = $named
            invokableNames = $invokableNames
            tab            = $tab
            # cycle=yes only when the walk came back round; "cap" and "no-focus" print
            # as themselves, because a truncated walk that reads as complete is how G21
            # stayed invisible for four rounds.
            tabCycle       = if ($null -ne $walk) { $walk.Cycle } else { $false }
            walkEnd        = if ($null -ne $walk) { $walk.End } else { "skipped" }
            distinctTab    = if ($null -ne $walk) { $walk.Distinct } else { 0 }
            required       = $required.ToArray()
            walked         = if ($null -ne $walk) { $walk.Stops.ToArray() } else { @() }
            coverage       = $coverage
        }
    }
    finally {
        Stop-App
    }
}

<#
  The measurement a live run would have produced, read from a file (G21/G25).

  Both assertions this script makes are about a walk, and a walk needs a window, a
  foreground app and an interactive desktop - none of which a runner has. So the file
  carries the raw measurement (the tree counts, the focusable stops, the stops that were
  walked) and every comparison is still made here, on the same code path a real page
  takes: a fixture cannot pre-compute a verdict, which is what keeps this from being a
  second implementation of the gate.

  What a replay cannot decide: whether a real page produces these numbers. That is the
  live run's job, and the run says which one it did.
#>
function Read-Replay {
    param([string]$Path)
    if (-not (Test-Path $Path)) { throw "replay file not found: $Path" }
    $out = @()
    foreach ($p in @(Get-Content $Path -Raw | ConvertFrom-Json)) {
        $byType = @{}
        foreach ($prop in $p.byType.PSObject.Properties) { $byType[$prop.Name] = [int]$prop.Value }
        $required = @($p.required | Where-Object { $null -ne $_ } | ForEach-Object {
                [pscustomobject]@{ Name = "$($_.name)"; Type = "$($_.type)"; X = [int]$_.x; Y = [int]$_.y; Width = [int]$_.width; Height = [int]$_.height }
            })
        $walked = @($p.walked | Where-Object { $null -ne $_ } | ForEach-Object {
                [pscustomobject]@{ Name = "$($_.name)"; Type = "$($_.type)"; X = [int]$_.x; Y = [int]$_.y; Width = [int]$_.width; Height = [int]$_.height }
            })
        $keys = @($walked | ForEach-Object { Get-StopKey -Name $_.Name -Type $_.Type -X $_.X -Y $_.Y -Width $_.Width -Height $_.Height } | Select-Object -Unique)
        # G58 (E4): a replay that does not say the walk cycled has not said it cycled.
        # This defaulted to "cycle", so every fixture written before walkEnd existed
        # claimed a complete walk - which is the same defect the assertion below fixes,
        # one layer down.
        $walkEnd = if ($p.walkEnd) { "$($p.walkEnd)" } else { "cap" }
        $out += [pscustomobject]@{
            page           = "$($p.page)"
            total          = [int]$p.total
            byType         = $byType
            focusable      = [int]$p.focusable
            invokable      = [int]$p.invokable
            named          = [int]$p.named
            invokableNames = @()
            tab            = @($walked | ForEach-Object { if ($_.Name) { $_.Name } else { "<$($_.Type)>" } })
            tabCycle       = ($walkEnd -eq "cycle")
            walkEnd        = $walkEnd
            distinctTab    = $keys.Count
            required       = $required
            walked         = $walked
            coverage       = (Compare-StopCoverage -Required $required -Walked $walked)
        }
    }
    return $out
}

$results = @()
if ($Replay) {
    $results = @(Read-Replay -Path $Replay)
    # A replay that holds no page is a broken fixture, not a host that cannot run it: a
    # FAIL that names the file, never the 77 a host condition earns.
    if ($results.Count -eq 0) {
        Write-Host ("FAIL {0} holds no page: a replay that measured nothing compared nothing" -f $Replay) -ForegroundColor Red
        exit 1
    }
    Write-Host ("replay: {0} (recorded measurement; no window was launched)" -f (Resolve-Path $Replay).Path)
}
else {
    foreach ($page in $Pages) {
        $r = Measure-Page -Page $page
        if ($null -eq $r) { continue }
        $results += $r
    }
}

foreach ($r in $results) {
    $types = ($r.byType.GetEnumerator() | Sort-Object Value -Descending | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join " "
    # cycle=yes means the walk came back round; anything else is printed as itself, so a
    # truncated walk can never read as a complete one (G21).
    $cycleLabel = switch ($r.walkEnd) { "cycle" { "yes" } "cap" { "cap" } "no-focus" { "lost" } default { "skip" } }
    $reachLabel = if ($null -ne $r.coverage) { "{0}/{1}" -f $r.coverage.Reached, $r.coverage.Required } else { "-" }
    $line = "  {0,-10} nodes={1,-4} invokable={2,-4} focusable={3,-3} tabStops={4,-3} cycle={5,-5} reached={6,-9} {7}" -f $r.page, $r.total, $r.invokable, $r.focusable, $r.distinctTab, $cycleLabel, $reachLabel, $types
    Write-Host $line
}
foreach ($skip in $skips) { Write-Host ("  skipped: " + $skip) -ForegroundColor DarkYellow }

# G18: a run that measured no page has nothing to assert. It used to throw instead,
# and a throw is a FAIL to tools/gates.ps1 whether the cause was a defect or a host
# with no desktop. 77 is the documented "cannot run here" code.
if ($results.Count -eq 0) {
    Write-Host ""
    Write-Host ("SKIP: no page produced a measurement ({0} of {1} skipped) - {2}" -f $skips.Count, $Pages.Count, ($skips -join "; ")) -ForegroundColor DarkYellow
    Write-Host "      Nothing was measured, so nothing about accessibility is being reported: exit 77, never a pass."
    exit 77
}

# The summary is the *live* measurement, so a replay does not overwrite it with fixture
# numbers: target/a11y/a11y-summary.json stays the record of the last run against a window.
if (-not $Replay) {
    $results | ConvertTo-Json -Depth 6 | Set-Content -Path (Join-Path $OutDir "a11y-summary.json") -Encoding utf8
}

# ---------------------------------------------------------------- budget
function Read-Budget {
    if (-not (Test-Path $Budget)) { return $null }
    return Get-Content $Budget -Raw | ConvertFrom-Json
}

<#
  The floors for one page, or $null when this budget has none for it.

  G4: the old version fell back to the document's default 1/2/3 for a page absent from
  it, so deleting one page's entry made that page green against a floor nobody had
  ever measured. A page with no entry is now a failure that names the page; the
  default block is no longer a floor that any page can inherit.

  The round-five version of this bug was one letter wide: `$budget = Read-Budget` and
  the `$Budget` path parameter are the same variable to PowerShell, so the document
  replaced the path it had just been read from and every comparison ran against
  nothing. The local is $b for that reason, and stays $b.
#>
function Get-Need {
    param($Page)
    $b = Read-Budget
    if ($null -eq $b) { return $null }
    if (-not $b.pages) { return $null }
    if (-not ($b.pages.PSObject.Properties.Name -contains $Page)) { return $null }
    $entry = $b.pages.$Page
    $need = [ordered]@{}
    foreach ($key in @("minInvokable", "minFocusable", "minTabStops")) {
        $value = $entry.$key
        if ($null -eq $value) { return $null }
        $need[$key] = [int]$value
    }
    $roles = @($entry.roles | Where-Object { $_ })
    if ($roles.Count -eq 0 -and $b.default) { $roles = @($b.default.roles | Where-Object { $_ }) }
    $need["roles"] = $roles
    return $need
}

if ($Update -and $Replay) {
    # A replay is a fixture, and a fixture must never become the floor: -Update writes
    # tools/a11y-budget.json, and the two together would pin the numbers a test invented.
    Write-Host ("FAIL -Update and -Replay together would re-pin {0} from a fixture; run -Update against the application" -f $Budget) -ForegroundColor Red
    exit 1
}

if ($Update) {
    # G15: -Update ratchets, it does not record. It used to write the very measurement
    # it exists to bound, so following the documented "run with -Update" on a degraded
    # build recorded the degradation as the floor. Every floor is now max(old,
    # measured): a re-pin can raise a budget and can never lower one. A page the old
    # document knows and this run did not measure keeps its entry untouched.
    $old = Read-Budget
    $oldPages = @{}
    if ($old -and $old.pages) {
        foreach ($p in $old.pages.PSObject.Properties) { $oldPages[$p.Name] = $p.Value }
    }
    $doc = [ordered]@{
        _comment = "Accessibility budget: what each page must expose to UI Automation after round five. Written by tools/verify-a11y.ps1 -Update, which ratchets: each floor is max(previous, measured), so a re-pin on a degraded build cannot lower one. invokable = elements with an InvokePattern, distinctTab = distinct stops the Tab walk reached before its (Name, Type, rectangle) identity repeated - G21, round nine: the pair alone ended the walk at the second control sharing a name and read as a cycle."
        default  = [ordered]@{ minInvokable = 1; minFocusable = 2; minTabStops = 3; roles = @("Button") }
        pages    = [ordered]@{}
    }
    Write-Host ""
    foreach ($r in $results) {
        $measured = [ordered]@{
            minInvokable = [int]$r.invokable
            minFocusable = [int]$r.focusable
            minTabStops  = [int]$r.distinctTab
        }
        $shown = @()
        foreach ($key in @("minInvokable", "minFocusable", "minTabStops")) {
            $was = $null
            if ($oldPages.ContainsKey($r.page)) { $was = $oldPages[$r.page].$key }
            if ($null -ne $was -and [int]$was -gt $measured[$key]) {
                $shown += ("{0} {1}->{2} KEPT" -f $key, [int]$was, [int]$was)
                $measured[$key] = [int]$was
            }
            elseif ($null -eq $was) {
                $shown += ("{0} none->{1}" -f $key, $measured[$key])
            }
            else {
                $shown += ("{0} {1}->{2}" -f $key, [int]$was, $measured[$key])
            }
        }
        $entry = [ordered]@{
            minInvokable = $measured.minInvokable
            minFocusable = $measured.minFocusable
            minTabStops  = $measured.minTabStops
        }
        if ($oldPages.ContainsKey($r.page) -and $oldPages[$r.page].roles) {
            $entry["roles"] = @($oldPages[$r.page].roles)
        }
        $doc.pages[$r.page] = $entry
        Write-Host ("  {0,-10} {1}" -f $r.page, ($shown -join "; ")) -ForegroundColor Yellow
    }
    foreach ($name in $oldPages.Keys) {
        if (-not $doc.pages.Contains($name)) {
            $doc.pages[$name] = $oldPages[$name]
            Write-Host ("  {0,-10} not measured this run: entry kept unchanged" -f $name) -ForegroundColor DarkYellow
        }
    }
    # G32: the budget is keyed by the accessibility page set, and this is where that is
    # made true instead of asserted. A page in the set that this run did not measure gets
    # an entry of zeros - which bounds nothing, and says so on the line - and the next
    # measured run ratchets it up (G15). A key the set does not contain is dropped,
    # because nothing measures it.
    $set = @(Get-PageSet -For budget)
    foreach ($page in $set) {
        if (-not $doc.pages.Contains($page)) {
            $doc.pages[$page] = [ordered]@{ minInvokable = 0; minFocusable = 0; minTabStops = 0 }
            Write-Host ("  {0,-10} not measured yet: entry seeded at 0, which bounds nothing until a measured run ratchets it" -f $page) -ForegroundColor DarkYellow
        }
    }
    foreach ($name in @($doc.pages.Keys)) {
        if ($set -notcontains $name) {
            $doc.pages.Remove($name)
            Write-Host ("  {0,-10} dropped: not in the accessibility page set (tools/page-set.ps1)" -f $name) -ForegroundColor DarkYellow
        }
    }
    ($doc | ConvertTo-Json -Depth 6) | Set-Content -Path $Budget -Encoding utf8
    Write-Host ""
    Write-Host "budget re-pinned, ratcheted up only: $Budget" -ForegroundColor Yellow
    exit 0
}

$budgetDoc = Read-Budget
if ($null -eq $budgetDoc) {
    Write-Host ""
    # G4: this used to print "no budget - run with -Update" and exit 0, after launching
    # the app eight times. Nothing was compared against any floor, so the run fails and
    # names the path it could not read.
    Write-Host ("FAIL no accessibility budget at '{0}': every page below was measured against no floor at all - run -Update once to pin today's numbers" -f $Budget) -ForegroundColor Red
    if ($Report) {
        Write-Host "not enforced (-Report)" -ForegroundColor Yellow
        exit 0
    }
    exit 1
}

$failures = @()

# G58 (E3): a skip is not a pass. Until round twelve only "nothing was measured at all"
# left by 77, so ten skipped pages beside one measured one printed the budget sentence
# and exited 0. The rule is the one tools/walk-lib.ps1 ships, so the selftest can drive
# it without a window; here it is applied to what this run actually measured.
# A replay is exempt by construction: the file *is* the page set it was recorded for,
# and the count in --Pages is the live selection, not a claim about the fixture.
if (-not $Replay) {
    $failures += @(Get-UnmeasuredFailure -Pages $Pages -Results $results -Skips $skips)
}

foreach ($r in $results) {
    # G58 (E4): walkEnd was printed and asserted by nobody, while
    # tools/verify-focus.ps1:458 fails on the same condition. A walk that ended at "cap"
    # did not come back round, so it proves the stops it visited were reachable and
    # nothing about the rest of the ring.
    $failures += @(Get-WalkEndFailure -Page $r.page -WalkEnd $r.walkEnd)
    $need = Get-Need -Page $r.page
    if ($null -eq $need) {
        # G4: a page absent from the budget is a failure that names the page, not a
        # silent pass against the document's default floors.
        $failures += ("{0}: this page has no entry in {1} - a page absent from the budget has no floor, so nothing about it was asserted; run -Update to pin it" -f $r.page, $Budget)
        continue
    }
    if ($r.invokable -lt $need.minInvokable) {
        $failures += ("{0}: invokable {1} < {2}" -f $r.page, $r.invokable, $need.minInvokable)
    }
    if ($r.focusable -lt $need.minFocusable) {
        $failures += ("{0}: focusable {1} < {2}" -f $r.page, $r.focusable, $need.minFocusable)
    }
    if (-not $SkipTab -and $r.distinctTab -lt $need.minTabStops) {
        $failures += ("{0}: distinct Tab stops {1} < {2}" -f $r.page, $r.distinctTab, $need.minTabStops)
    }
    # G21, second assertion: every focusable Button/ListItem/Edit has to have been
    # reached by the walk. A page holding a focusable control Tab cannot visit is a FAIL -
    # not a SKIP and not a 0 - because the control exists, the tree advertises it to
    # assistive technology, and a keyboard user cannot get to it. That is the class U7
    # (a TagChip with a role, a default action and no FocusScope) belongs to; this
    # assertion is what makes it measurable instead of reviewable.
    if ($null -eq $r.coverage) {
        $failures += ("{0}: the focusable-coverage assertion did not run (-SkipTab) - a page whose Tab walk was skipped did not pass the assertion, so it cannot pass the gate" -f $r.page)
    }
    elseif (-not $r.coverage.Ok) {
        $shown = @($r.coverage.Missing | Select-Object -First 4)
        foreach ($m in $shown) {
            $failures += ("{0}: focusable {1} was never reached by Tab ({2} of {3} focusable Button/ListItem/Edit stops visited)" -f $r.page, (Format-StopLabel -Stop $m), $r.coverage.Reached, $r.coverage.Required)
        }
        if ($r.coverage.Missing.Count -gt $shown.Count) {
            $rest = ($r.coverage.Missing | Select-Object -Skip 4 | ForEach-Object { "$($_.Type) '$($_.Name)'" }) -join "; "
            $failures += ("{0}: {1} further focusable stop(s) unreachable: {2}" -f $r.page, ($r.coverage.Missing.Count - $shown.Count), $rest)
        }
    }
    $roles = @($need.roles | Where-Object { $_ })
    if ($roles.Count -gt 0) {
        $hit = $false
        foreach ($role in $roles) { if ($r.byType.ContainsKey($role)) { $hit = $true } }
        if (-not $hit) {
            $failures += ("{0}: none of the roles [{1}] appears in the tree" -f $r.page, ($roles -join ", "))
        }
    }
}

# G32: a page whose entry is a seeded zero passes because nothing is asserted about it,
# not because it was measured. One line says which, so a green run cannot be read as
# "the novel pages were measured here" - that is -Update's job on this desktop.
$pending = @($results | Where-Object {
        $prop = $budgetDoc.pages.PSObject.Properties[$_.page]
        $null -ne $prop -and [int]$prop.Value.minInvokable -eq 0 -and [int]$prop.Value.minFocusable -eq 0 -and [int]$prop.Value.minTabStops -eq 0
    } | ForEach-Object { $_.page })
if ($pending.Count -gt 0) {
    Write-Host ("  pending floors, nothing asserted yet, pinned by the first measured -Update: " + ($pending -join ", ")) -ForegroundColor DarkYellow
}

Write-Host ""
if ($failures.Count -eq 0) {
    Write-Host ("{0} pages meet the accessibility budget" -f $results.Count) -ForegroundColor Green
    exit 0
}

$failures | ForEach-Object { Write-Host ("  FAIL " + $_) -ForegroundColor Red }
if ($Report) {
    Write-Host ("{0} failures, not enforced (-Report)" -f $failures.Count) -ForegroundColor Yellow
    exit 0
}
Write-Host ("{0} accessibility failures" -f $failures.Count) -ForegroundColor Red
exit 1
