# The Tab walk's shared arithmetic: what counts as "the same stop", and whether a
# walk visited everything the page says is focusable.
#
# Why this file exists (round nine, G21 and G22). Two gates press real Tab keys:
# tools/verify-a11y.ps1 counts the stops a page has, tools/verify-focus.ps1 measures
# the ring each one draws. Both need the same answer to "have I been here already",
# and until round nine that answer was wrong in the same way for both.
#
# G21: a stop was identified by its (Name, Type) pair alone, so the *second* control
# called "收藏" ended the walk. The run then printed cycle=yes - the walk really had
# come back round to a stop it had seen - while the page held 31 focusable elements
# and the walk had visited 24. bookmarks 31/24, search 22/21, history 15/14,
# downloads 19/18: every shortfall was a duplicate name, not a shorter page. The
# identity is now (Name, Type, rectangle): two identically-named buttons are two
# stops, because they are two places on the screen.
#
# The functions are pure - no UI Automation, no window, no pixels - so
# tools/selftest-gates.ps1 drives them with a synthetic sequence and shows both
# halves of the defect: the old identity stops early, the new one does not, and the
# coverage check names the control the old rule never reached. A walk that could
# only be re-run with a window in front of it would be a walk whose regression
# nobody could reproduce on a runner.
#
# -PairOnly reproduces the pre-G21 identity. The gates never pass it; it exists so
# the negative experiment can run the old rule through the shipped engine instead of
# a copy of it, which is how tools/selftest-gates.ps1 proves the fix is doing work.

<#
  The identity of one Tab stop.

  With geometry: "Name|Type|X,Y,WxH" - two buttons that share a name are distinct
  stops (G21). Without -X: "Name|Type", the pre-G21 rule, kept only for the negative
  experiment.

  The rectangle is rounded to whole pixels by the cast: UI Automation reports
  fractional screen coordinates at 150 % scale, and a stop must not become "new"
  because its rect moved by a third of a pixel between two reads of a static page.
#>
function Get-StopKey {
    param(
        [string]$Name,
        [string]$Type,
        $X,
        $Y,
        [int]$Width = 0,
        [int]$Height = 0
    )
    $head = "{0}|{1}" -f $Name, $Type
    if (-not $PSBoundParameters.ContainsKey('X')) { return $head }
    return ("{0}|{1},{2},{3}x{4}" -f $head, [int]$X, [int]$Y, $Width, $Height)
}

<#
  Walk the keyboard until a stop repeats, and say how the walk ended.

  -NextStop is a scriptblock that presses Tab once and returns one stop:
  @{ Name; Type; X; Y; Width; Height }, or $null when nothing is focused. Injecting
  the reader is what makes the engine testable: the gates pass a UI Automation read,
  the selftest passes a fixed sequence.

  End is one of:
    cycle     a stop repeated - the walk has come back round, so every stop the page
              can reach is in the list. This is the only end that means "complete".
    cap       MaxSteps presses without a repeat: the sample is still a sample.
    no-focus  MaxMisses presses in a row focused nothing: the walk left the app's
              focus ring (a modal, a window change), so the list is truncated.
#>
function Measure-TabWalk {
    param(
        [Parameter(Mandatory = $true)][scriptblock]$NextStop,
        [int]$MaxSteps = 200,
        [int]$MaxMisses = 5,
        [switch]$PairOnly
    )

    $stops = New-Object System.Collections.Generic.List[object]
    $keys = New-Object System.Collections.Generic.List[string]
    $cycle = $false
    $misses = 0
    $end = "cap"

    for ($i = 0; $i -lt $MaxSteps; $i++) {
        $s = & $NextStop
        if ($null -eq $s -or ((-not $s.Name) -and (-not $s.Type))) {
            # Nothing is focused: not a stop, and no evidence either way. A few of
            # these in a row mean the walk has left the app's focus ring.
            $misses++
            if ($misses -ge $MaxMisses) { $end = "no-focus"; break }
            continue
        }
        $misses = 0
        if ($PairOnly) {
            $key = Get-StopKey -Name $s.Name -Type $s.Type
        }
        else {
            $key = Get-StopKey -Name $s.Name -Type $s.Type -X $s.X -Y $s.Y -Width $s.Width -Height $s.Height
        }
        if ($keys.Contains($key)) { $cycle = $true; $end = "cycle"; break }
        $keys.Add($key)
        $stops.Add([pscustomobject]@{
                Index  = $stops.Count + 1
                Name   = $s.Name
                Type   = $s.Type
                X      = [int]$s.X
                Y      = [int]$s.Y
                Width  = [int]$s.Width
                Height = [int]$s.Height
                Key    = $key
            })
    }

    return [pscustomobject]@{
        Stops    = $stops
        Keys     = $keys
        Distinct = $keys.Count
        Cycle    = $cycle
        End      = $end
        Misses   = $misses
    }
}

<#
  Which of -Required the walk never reached.

  G21's assertion half: every focusable Button/ListItem/Edit on the page must have
  been visited, or the page is a FAIL. The match is on (Name, Type) *with
  multiplicity*, not on the rectangle: the walk's job is to prove the control is
  reachable, and a control that scrolls into view or sits one pixel off between two
  reads is still reached. The rectangle is what keeps the walk from ending early;
  this check is what makes "ended early" visible.

  Each unmatched instance is reported by name, type and rectangle, so the message
  names the control an operator has to go and look at.
#>
function Compare-StopCoverage {
    param(
        [object[]]$Required = @(),
        [object[]]$Walked = @()
    )

    $requiredStops = @($Required | Where-Object { $null -ne $_ })
    $walkedStops = @($Walked | Where-Object { $null -ne $_ })

    # Two passes, in this order, and the order is the whole design:
    #
    # 1. Exact identity - (Name, Type, rectangle). A walked stop that matches a required
    #    stop's rectangle *is* that control, which is what lets the failure name the right
    #    one when two controls share a name (measured: with the pair-only pass first, the
    #    stop reported was the one the walk had just visited).
    # 2. Then, for what is left, the pair alone. That pass exists because a rectangle can
    #    legitimately move between the tree read and the Tab walk - a card that scrolls
    #    into view when it takes focus does exactly that, and U3 is the defect that makes
    #    it happen - and a control that is reached must not be called unreachable because
    #    it moved. It cannot hide a duplicate: pass 1 has already spent one visit per
    #    visited rectangle, so two buttons sharing a name still need two visits.
    $exact = @{}
    foreach ($w in $walkedStops) {
        $k = Get-StopKey -Name $w.Name -Type $w.Type -X $w.X -Y $w.Y -Width $w.Width -Height $w.Height
        if ($exact.ContainsKey($k)) { $exact[$k]++ } else { $exact[$k] = 1 }
    }
    $byPair = @{}
    foreach ($w in $walkedStops) {
        $k = Get-StopKey -Name $w.Name -Type $w.Type
        if ($byPair.ContainsKey($k)) { $byPair[$k]++ } else { $byPair[$k] = 1 }
    }

    $left = New-Object System.Collections.Generic.List[object]
    foreach ($r in $requiredStops) {
        $k = Get-StopKey -Name $r.Name -Type $r.Type -X $r.X -Y $r.Y -Width $r.Width -Height $r.Height
        $pair = Get-StopKey -Name $r.Name -Type $r.Type
        if ($exact.ContainsKey($k) -and $exact[$k] -gt 0) {
            $exact[$k]--
            if ($byPair.ContainsKey($pair) -and $byPair[$pair] -gt 0) { $byPair[$pair]-- }
        }
        else {
            $left.Add($r)
        }
    }

    $missing = New-Object System.Collections.Generic.List[object]
    foreach ($r in $left) {
        $pair = Get-StopKey -Name $r.Name -Type $r.Type
        if ($byPair.ContainsKey($pair) -and $byPair[$pair] -gt 0) {
            $byPair[$pair]--
            continue
        }
        $missing.Add([pscustomobject]@{
                Name   = $r.Name
                Type   = $r.Type
                X      = [int]$r.X
                Y      = [int]$r.Y
                Width  = [int]$r.Width
                Height = [int]$r.Height
                Label  = (Format-StopLabel -Stop $r)
            })
    }

    return [pscustomobject]@{
        Required = $requiredStops.Count
        Walked   = $walkedStops.Count
        Reached  = $requiredStops.Count - $missing.Count
        Missing  = $missing
        Ok       = ($missing.Count -eq 0)
    }
}

<#
  G58 (E3): a page this run asked for and did not measure is a failure, not a gap.

  Round twelve measured the hole: tools/verify-a11y.ps1 exited 77 only when *nothing*
  was measured, so a run in which ten pages were skipped and one was measured printed
  the budget sentence and exited 0. Ten pages had no accessibility floor asserted
  against them and the gate called it a pass.

  The rule is stated as a difference rather than as a count, because the count is what
  the gate has to report: every requested page either has a row in -Results or is named
  with the reason it does not. -Skips carries the reasons Measure-Page already records,
  so the failure names the host condition instead of just the page.
#>
function Get-UnmeasuredFailure {
    param(
        [string[]]$Pages = @(),
        [object[]]$Results = @(),
        [string[]]$Skips = @()
    )
    $failures = New-Object System.Collections.Generic.List[string]
    $measured = @($Results | Where-Object { $null -ne $_ } | ForEach-Object { "$($_.page)" })
    foreach ($page in $Pages) {
        if ($measured -contains $page) { continue }
        $reason = @($Skips | Where-Object { $_ -like ($page + ":*") })
        $why = if ($reason.Count) { $reason[0] } else { "no measurement and no recorded reason" }
        $failures.Add(("{0}: in the requested page set and not measured ({1}) - a page that was skipped did not pass, so {2} of {3} pages met the budget at most" -f $page, $why, $measured.Count, $Pages.Count))
    }
    return $failures.ToArray()
}

<#
  G58 (E4): a walk that did not come back round has not proved anything about the ring.

  walkEnd is printed by tools/verify-a11y.ps1 and was asserted by nobody, while
  tools/verify-focus.ps1 fails on the same condition (:458) - two gates reading one fact
  and only one of them acting on it. A walk that ends at "cap" visited MaxSteps stops and
  never returned to its first, so "reached == required" says the controls it happened to
  visit were reachable and nothing about the order it stopped in.

  "cycle" is the only accepted value. The message says what ended the walk, because
  "cap" (a budget) and "no-focus" (focus left the ring) are different defects.
#>
function Get-WalkEndFailure {
    param([string]$Page, [string]$WalkEnd)
    if ($WalkEnd -eq "cycle") { return @() }
    $what = switch ($WalkEnd) {
        "cap" { "the walk hit its step cap without returning to its first stop, so the ring after that point was never measured" }
        "no-focus" { "focus left the page's ring (nothing focusable was found several times in a row)" }
        "skipped" { "the Tab walk was skipped, so no cycle was measured at all" }
        default { "the walk ended without reporting a cycle" }
    }
    return @(("{0}: the Tab walk ended at '{1}', not 'cycle' - {2}" -f $Page, $WalkEnd, $what))
}

# One stop, in the words a failure message needs.
function Format-StopLabel {
    param($Stop)
    $type = if ($Stop.Type) { $Stop.Type } else { "?" }
    $name = if ($Stop.Name) { "'" + $Stop.Name + "'" } else { "<unnamed>" }
    return ("{0} {1} at {2},{3} {4}x{5}" -f $type, $name, [int]$Stop.X, [int]$Stop.Y, [int]$Stop.Width, [int]$Stop.Height)
}
