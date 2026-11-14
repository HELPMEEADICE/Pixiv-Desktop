# What one Tab stop draws, decided in one place (round nine, G22).
#
# tools/verify-focus.ps1 reads pixels for every stop it reaches and then classifies
# the result as RING, RING-DIM, ACCENT or NONE. G22 is about the *walk* - ten stops
# on a page that has fifty - but the classification is what has to go red when the
# walk finally reaches a stop with nothing drawn on it, and until round nine that
# classification lived inline in a 200-line capture loop where no test could reach
# it. A gate whose failure branch cannot be re-run on a runner is a gate accepted on
# a screenshot of its source.
#
# So the two decisions - "what did this stop draw" and "which stops drew nothing" -
# are functions here, the gate calls them, and tools/selftest-gates.ps1 plants a stop
# with ring=0, accent=0 and requires that it comes back as one invisible stop.
# What this file does NOT decide: whether the ring reads well, whether the order is
# sensible, or anything at all about a stop outside its own rectangle. That is
# stated in the gate's header and stays true.

<#
  The verdict for one stop, from the two pixel counts the gate measured.

  RING      the full-strength Theme.focus-ring is inside the rectangle
  RING-DIM  a disabled stop's ring, found at the opacity components.slint applies to
            the whole body (measured: 0.4 on IconButton, 512 px)
  ACCENT    a text field drew the 2px accent rule instead of the ring (the two
            declared exceptions, components.slint:424 and :436)
  NONE      reached by Tab and invisible: exactly the UI-NOTES 12a defect

  The thresholds are the gate's own -MinPixels, not a second copy of it: a stop
  needs 6 pixels of ring to count, and 12 to count as a dimmed one, because a dimmed
  ring is measured through a blend and blends find stray pixels.
#>
function Get-FocusVerdict {
    param(
        [int]$RingPixels = 0,
        [double]$Alpha = 0.0,
        [int]$AccentPixels = 0,
        [string]$Type = "",
        [int]$MinPixels = 6
    )
    if ($RingPixels -ge $MinPixels -and $Alpha -eq 1.0) { return "RING" }
    if ($RingPixels -ge ($MinPixels * 2) -and $Alpha -gt 0) { return "RING-DIM" }
    if ($AccentPixels -ge $MinPixels -and $Type -eq "Edit") { return "ACCENT" }
    return "NONE"
}

# The stops that drew nothing. A page with one of these is a FAIL, never a partial
# pass: "44 of 45 stops have a ring" is the sentence 12a would have shipped with.
function Get-InvisibleStops {
    param([object[]]$Stops = @())
    return @($Stops | Where-Object { $null -ne $_ -and $_.Verdict -eq "NONE" })
}

<#
  How many stops this page walks.

  G22: "at least one page walks every stop, the rest keep ten". A page in -FullPages
  is walked until its focus ring closes (Measure-TabWalk in tools/walk-lib.ps1), so
  the sample is the whole page; every other page keeps -Stops, because a full walk
  costs a capture and a pixel map per stop and the ring on the chrome is the same
  three pages deep.

  The old behaviour is the same function with -FullPages @(): home then walks ten
  too, which is what the selftest case plants.
#>
function Get-WalkLimit {
    param(
        [string]$Page,
        [string[]]$FullPages = @(),
        [int]$Stops = 10,
        [int]$MaxStops = 200
    )
    if (@($FullPages) -contains $Page) { return $MaxStops }
    return $Stops
}
