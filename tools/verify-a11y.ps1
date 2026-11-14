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
# Usage:
#   pwsh -File tools/verify-a11y.ps1 -Report        # measure, never fail
#   pwsh -File tools/verify-a11y.ps1                # measure and assert
#   pwsh -File tools/verify-a11y.ps1 -Update        # re-pin the budget to today
#   pwsh -File tools/verify-a11y.ps1 -Pages home,settings -Dark
param(
    [string[]]$Pages = @("home", "discovery", "ranking", "search", "bookmarks", "history", "downloads", "settings"),
    [string]$Size = "1024x768",
    [string]$Exe = "target\debug\pixiv-desktop.exe",
    [string]$Budget = "tools\a11y-budget.json",
    [string]$OutDir = "target\a11y",
    # G5: the walk used to be exactly this many presses, and -Update then pinned that
    # same number as the requirement - the budget and the sample were one number, so
    # the gate could not see a stop it never walked. The walk now runs until the
    # (Name, Type) pair it reads repeats (that is the cycle); this is only the safety
    # cap for a page whose focus never comes back round. -TabSteps is kept as an alias
    # so a documented invocation still binds.
    [Alias("TabSteps")]
    [int]$MaxTabSteps = 200,
    [switch]$Dark,
    [switch]$SkipTab,
    [switch]$Report,
    [switch]$Update
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)
. (Join-Path $PSScriptRoot "drive.ps1") | Out-Null
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

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

    if (-not (Test-Path $Exe)) {
        $script:skips += ("{0}: no binary at {1}" -f $Page, $Exe)
        return $null
    }
    Stop-App
    $env:PIXIV_DESKTOP_DEMO = "1"
    $env:PIXIV_DESKTOP_START_PAGE = $Page
    $env:PIXIV_DESKTOP_SIZE = $Size
    Remove-Item Env:\PIXIV_DESKTOP_START_ILLUST -ErrorAction SilentlyContinue
    Remove-Item Env:\PIXIV_DESKTOP_START_USER -ErrorAction SilentlyContinue
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
        foreach ($el in $all) {
            $c = $el.Current
            $type = $c.ControlType.ProgrammaticName -replace "ControlType\.", ""
            if ($byType.ContainsKey($type)) { $byType[$type]++ } else { $byType[$type] = 1 }
            if ($c.IsKeyboardFocusable) { $focusable++ }
            if ($c.Name) { $named++ }
            $pattern = $null
            if ($el.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
                $invokable++
                if ($c.Name) { $invokableNames += $c.Name }
            }
        }

        $tab = @()
        $pairs = New-Object System.Collections.Generic.List[string]
        $cycle = $false
        $misses = 0
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
            # G5: walk until the (Name, Type) pair repeats. That repeat *is* the cycle -
            # Tab has come back round to a stop the walk already saw - so every distinct
            # stop the page has is in the list when the loop ends. The old fixed ten
            # presses made the sample size and the requirement the same number: the
            # budget pinned 10 on every page while 14 to 44 things were focusable.
            for ($i = 0; $i -lt $MaxTabSteps; $i++) {
                Send-TabKey -Count 1
                $f = Get-FocusedInfo
                if (-not $f.Name -and -not $f.Type) {
                    # Nothing is focused: not a stop, and no evidence either way. A few
                    # of these in a row mean the walk has left the app's focus ring.
                    $misses++
                    if ($misses -ge 5) { break }
                    continue
                }
                $misses = 0
                $pair = "{0}|{1}" -f $f.Name, $f.Type
                if ($pairs.Contains($pair)) { $cycle = $true; break }
                $pairs.Add($pair)
                if ($f.Name) { $tab += $f.Name } else { $tab += "<$($f.Type)>" }
            }
        }

        return [pscustomobject]@{
            page           = $Page
            total          = $all.Count
            byType         = $byType
            focusable      = $focusable
            invokable      = $invokable
            named          = $named
            invokableNames = $invokableNames
            tab            = $tab
            tabCycle       = $cycle
            distinctTab    = $pairs.Count
        }
    }
    finally {
        Stop-App
    }
}

$results = @()
foreach ($page in $Pages) {
    $r = Measure-Page -Page $page
    if ($null -eq $r) { continue }
    $results += $r
    $types = ($r.byType.GetEnumerator() | Sort-Object Value -Descending | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join " "
    $cycleLabel = if ($r.tabCycle) { "yes" } else { "cap" }
    $line = "  {0,-10} nodes={1,-4} invokable={2,-4} focusable={3,-3} tabStops={4,-3} cycle={5,-4} {6}" -f $r.page, $r.total, $r.invokable, $r.focusable, $r.distinctTab, $cycleLabel, $types
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

$results | ConvertTo-Json -Depth 6 | Set-Content -Path (Join-Path $OutDir "a11y-summary.json") -Encoding utf8

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
        _comment = "Accessibility budget: what each page must expose to UI Automation after round five. Written by tools/verify-a11y.ps1 -Update, which ratchets: each floor is max(previous, measured), so a re-pin on a degraded build cannot lower one. invokable = elements with an InvokePattern, distinctTab = distinct stops the Tab walk reached before its (Name, Type) pair repeated."
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
foreach ($r in $results) {
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
    $roles = @($need.roles | Where-Object { $_ })
    if ($roles.Count -gt 0) {
        $hit = $false
        foreach ($role in $roles) { if ($r.byType.ContainsKey($role)) { $hit = $true } }
        if (-not $hit) {
            $failures += ("{0}: none of the roles [{1}] appears in the tree" -f $r.page, ($roles -join ", "))
        }
    }
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
