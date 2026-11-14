# PIXIV_DESKTOP_FIXTURE: the value -> the state a capture has to photograph.
#
# G24, round nine. The variable was named in three plans and implemented nowhere:
# a search of the tree found it in docs/ only, no src/ and no tools/. That is why
# U13 (the downloads row's progress bar) was deferred three rounds and why G23 (the
# settings cache figure is a live number, so both of its captures are
# nondeterministic) cannot be fixed at all: there was no way to ask the app for a
# deterministic state. This file is the gate half of the seam - it says, for each
# fixture, which page to start on, what to do to reach the state, and which captures
# that state is supposed to produce. The app half (src/, phase F) reads the same
# variable and puts the shell in that state.
#
# Exit codes, and why they are not negotiable (docs/OPTIMIZATION-PLAN-R9 section 7):
#   0   the fixture is known, or no fixture was asked for - the mapping is printed
#   77  the fixture is NOT known. Not 0: an unknown value means the run cannot know
#       what to photograph, and a gate that reports "nothing to do" as success is the
#       failure mode this whole round is about. 77 is the code tools/gates.ps1 already
#       reads as SKIP ("this host cannot run this gate"), which is the honest verdict:
#       not a pass, not a failure of the code under test.
#
# Usage:
#   pwsh -File tools/fixture.ps1                     # resolve $env:PIXIV_DESKTOP_FIXTURE
#   pwsh -File tools/fixture.ps1 -Fixture downloads  # resolve one value
#   pwsh -File tools/fixture.ps1 -List               # every state, for a reader
#   $env:PIXIV_DESKTOP_FIXTURE = "nope"; pwsh -File tools/fixture.ps1; $LASTEXITCODE  # 77
#
# Dot-sourced, this file defines the functions and does nothing else: a capture gate
# that imports Get-FixtureState must not be killed by an unrelated ambient value. The
# gate that *does* want the exit-77 rule calls Get-FixtureStateOrExit.
param(
    [string]$Fixture,
    [switch]$List
)

<#
  One entry per fixture. The four are the four states round nine's features create
  and nothing had ever captured (plan section 7, G24):
    ugoira    F1  playback in the viewer (README limit 6: export only)
    selection F2  multi-select and the batch bar (no entry point at all)
    palette   F3  Ctrl+P command palette (no entry point at all)
    downloads F5  pause/throughput/ETA, and U13's progress width
  Setup names the shipped path that reaches the state, so the fixture is a fixture
  and not a second invention: every gesture named here is one the plan already
  describes. Env carries only knobs that exist today (src/ reads PIXIV_DESKTOP_DEMO,
  _START_PAGE, _START_ILLUST, _START_USER, _THEME, _SIZE, _TIP), so the app half never
  has to guess a variable name.
#>
function Get-FixtureTable {
    return [ordered]@{
        ugoira    = [pscustomobject]@{
            Name     = "ugoira"
            Page     = "detail"
            Env      = [ordered]@{
                PIXIV_DESKTOP_DEMO         = "1"
                PIXIV_DESKTOP_START_PAGE   = "home"
                PIXIV_DESKTOP_START_ILLUST = "700000003"
            }
            Setup    = "Space on the film strip (F1 toggle-playback), waited one frame interval"
            Captures = @("detail-ugoira-play")
            Asserts  = "the frame index advanced between two captures of the same work, so a frozen film cannot pass"
            Defect   = "F1"
        }
        selection = [pscustomobject]@{
            Name     = "selection"
            Page     = "home"
            Env      = [ordered]@{
                PIXIV_DESKTOP_DEMO       = "1"
                PIXIV_DESKTOP_START_PAGE = "home"
            }
            Setup    = "Ctrl+A on the grid (F2 select-all), or Ctrl+click two cards"
            Captures = @("home-selection")
            Asserts  = "the 22px check and the 2px accent ring are drawn on a selected tile, and the batch bar is present"
            Defect   = "F2"
        }
        palette   = [pscustomobject]@{
            Name     = "palette"
            Page     = "home"
            Env      = [ordered]@{
                PIXIV_DESKTOP_DEMO       = "1"
                PIXIV_DESKTOP_START_PAGE = "home"
            }
            Setup    = "Ctrl+P (F3 palette-open), then type one letter"
            Captures = @("palette-open")
            Asserts  = "36px rows on the layer surface, and the key-hint footer is not clipped"
            Defect   = "F3"
        }
        downloads = [pscustomobject]@{
            Name     = "downloads"
            Page     = "downloads"
            Env      = [ordered]@{
                PIXIV_DESKTOP_DEMO       = "1"
                PIXIV_DESKTOP_START_PAGE = "downloads"
            }
            Setup    = "a demo queue with one running task (F5), then toggle-pause-all for the second capture"
            Captures = @("downloads-queue", "downloads-queue-paused")
            Asserts  = "throughput and ETA are non-empty, and U13's progress bar keeps its frozen width while paused"
            Defect   = "F5/U13"
        }
    }
}

# The state one value names, or $null. The single place a fixture value is looked up,
# so "known" cannot mean two different things in two gates.
function Get-FixtureState {
    param([string]$Name)
    if (-not $Name) { return $null }
    $table = Get-FixtureTable
    $key = $Name.Trim().ToLowerInvariant()
    if (-not $table.Contains($key)) { return $null }
    return $table[$key]
}

<#
  The state, or exit 77. This is what a capture gate calls after reading the ambient
  value: an unknown fixture is a host condition it cannot work around, so it says so
  once and stops, rather than photographing the normal page list and calling that a
  fixture run.
#>
function Get-FixtureStateOrExit {
    param([string]$Name = $env:PIXIV_DESKTOP_FIXTURE)
    if (-not $Name) { return $null }
    $state = Get-FixtureState -Name $Name
    if ($null -eq $state) {
        Write-Host ("SKIP: PIXIV_DESKTOP_FIXTURE='{0}' is not a fixture this repository knows ({1}); exit 77 - an unknown fixture is not ""no fixture""" -f $Name, ((Get-FixtureTable).Keys -join ", ")) -ForegroundColor DarkYellow
        exit 77
    }
    return $state
}

function Write-FixtureState {
    param($State)
    Write-Host ("{0}  page={1}  captures={2}" -f $State.Name, $State.Page, ($State.Captures -join ", "))
    Write-Host ("    setup:   {0}" -f $State.Setup)
    Write-Host ("    asserts: {0}" -f $State.Asserts)
    Write-Host ("    defect:  {0}" -f $State.Defect)
    foreach ($k in $State.Env.Keys) { Write-Host ("    env:     {0}={1}" -f $k, $State.Env[$k]) }
}

# The command line, and only from the command line: dot-sourcing defines the
# functions above and returns here without touching the exit code.
if ($MyInvocation.InvocationName -ne '.') {
    $table = Get-FixtureTable
    $requested = if ($PSBoundParameters.ContainsKey('Fixture')) { $Fixture } else { $env:PIXIV_DESKTOP_FIXTURE }

    if ($List) {
        Write-Host ("{0} fixtures: {1}" -f $table.Count, ($table.Keys -join ", "))
        foreach ($state in $table.Values) { Write-FixtureState -State $state }
        exit 0
    }

    if (-not $requested) {
        # Unset is not unknown: it means "capture the normal page list", which is
        # every run before this round and stays a success.
        Write-Host ("no PIXIV_DESKTOP_FIXTURE set: the capture gates run their normal page list. Known fixtures: {0}" -f ($table.Keys -join ", "))
        exit 0
    }

    $state = Get-FixtureState -Name $requested
    if ($null -eq $state) {
        Write-Host ("SKIP: PIXIV_DESKTOP_FIXTURE='{0}' is not a fixture this repository knows ({1}); exit 77 - an unknown fixture is not ""no fixture"", so it is neither a pass nor a failure of the code under test" -f $requested, ($table.Keys -join ", ")) -ForegroundColor DarkYellow
        exit 77
    }
    Write-FixtureState -State $state
    exit 0
}
