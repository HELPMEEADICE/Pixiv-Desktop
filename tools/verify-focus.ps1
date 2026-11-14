# Focus-visibility gate: after you Tab to a stop, is there anything to see?
#
# Why this exists. docs/UI-NOTES.md 12a fixed the worst defect this repository
# has had - nothing in the window was reachable without a mouse - by giving
# twelve components a FocusScope and the same 2px Theme.focus-ring AppButton
# already had. Round five then proved the ring by reading components.slint and by
# counting Tab stops; nothing ever looked at the ring on screen. Sections 10 and
# 12i still say "hover and keyboard are not verified here ... the harness can
# trace a window but cannot type into it", which stopped being true the day
# tools/drive.ps1 landed. This script presses real Tab keys and measures, in
# pixels, what each stop draws once it has focus.
#
# What it can and cannot decide (in the terms of UI-NOTES 12h):
#   * it CAN decide that a reached stop is visible: the focused element is read
#     from UI Automation, its rectangle is used as the region, and the region is
#     searched for the colour ui/theme.slint declares for focus. The colour is
#     parsed out of theme.slint on every run, so a theme edit that forgets to
#     draw the ring fails here instead of in review.
#   * a disabled stop is dimmed by design, not invisible: components.slint:264
#     (AppButton) and :321 (IconButton) apply opacity to the whole body, ring
#     included. The gate therefore measures a disabled stop's ring on the blend
#     that opacity produces over the region's dominant colour (a 2px stroke is
#     never the majority of the box it outlines), and prints the alpha it found -
#     measured here as 0.4 on the back/forward buttons, 512 pixels, which is
#     exactly the IconButton opacity.
#   * it CANNOT decide that the ring *reads* well, that the Tab *order* is
#     sensible (that is verify-a11y.ps1 territory: name, role, action), that an
#     eleventh stop is visible, or that a ring drawn outside its element
#     rectangle was seen at all.
#   * measured limit, not a design choice: the first ten stops on all three pages
#     are the command bar and the navigation rail (back, forward, search, theme,
#     then the eight rail entries). The walk proves the ring on the chrome. It
#     has not yet reached a card, and says so instead of implying otherwise.
#   * one declared exception: components.slint:424 and :436 give the search field
#     a focused border plus a 2px accent bottom rule instead of the ring, so an
#     Edit stop passes on Theme.accent-fill and the table prints which of the two
#     colours was found. A stop showing neither is a stop you can Tab to and
#     cannot see - exit 1.
#
# Exit codes:
#   0  measured, and every reached stop shows a focus indication
#   1  a Tab stop was reached and shows no focus indication (the 12a defect)
#   77 this host cannot run it: no binary, no window, no UI Automation tree, or
#      the app is not the foreground window. Reported as SKIP by gates.ps1 and
#      never as PASS - a fabricated PASS would be worse than no gate.
#
# Baselines: each focused state is captured to
# docs/screenshots/baseline/focus-<page>-<n>.png. Additive only: an existing
# baseline is never overwritten, so a committed capture keeps dating the run
# that first produced it. The capture measured is always the fresh one in
# target/focus/current, never the baseline.
#
# Usage:
#   pwsh -File tools/verify-focus.ps1                        # home, detail, settings
#   pwsh -File tools/verify-focus.ps1 -Pages home,settings
#   pwsh -File tools/verify-focus.ps1 -Stops 6 -Dark         # dark ring #7ac0f0
#   pwsh -File tools/verify-focus.ps1 -Report                # measure, never fail
param(
    [string[]]$Pages = @("home", "detail", "settings"),
    [string]$Size = "1024x768",
    [string]$Exe = "target\debug\pixiv-desktop.exe",
    [int]$Stops = 10,
    [int]$Tolerance = 10,
    [int]$MinPixels = 6,
    [string]$BaselineDir = "docs\screenshots\baseline",
    [string]$OutDir = "target\focus",
    [switch]$Dark,
    [switch]$Report
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)
. (Join-Path $PSScriptRoot "gui-lib.ps1")
. (Join-Path $PSScriptRoot "drive.ps1")
. (Join-Path $PSScriptRoot "pixel.ps1")
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms, System.Drawing

$Pages = @($Pages | ForEach-Object { $_ -split "," } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$currentDir = Join-Path $OutDir "current"
New-Item -ItemType Directory -Force -Path $currentDir | Out-Null
if (-not (Test-Path $BaselineDir)) { New-Item -ItemType Directory -Force -Path $BaselineDir | Out-Null }

# The ring and the accent rule are read from the source, not remembered: a gate
# that hard-codes #0a84d8 keeps passing after the theme stops using it.
function Read-ThemeColor {
    param([string]$Name, [switch]$Dark)
    $line = Select-String -Path "ui\theme.slint" -Pattern ("out property <color> " + $Name + ":") | Select-Object -First 1
    if ($null -eq $line) { throw ("ui/theme.slint declares no colour named ""{0}""" -f $Name) }
    if ($line.Line -notmatch 'dark \? #([0-9a-fA-F]{6})\s*:\s*#([0-9a-fA-F]{6})') {
        throw ("{0} is not a dark/light pair: {1}" -f $Name, $line.Line.Trim())
    }
    $hex = if ($Dark) { $Matches[1] } else { $Matches[2] }
    return [pscustomobject]@{
        Name = $Name
        Hex  = ("#" + $hex)
        R    = [Convert]::ToInt32($hex.Substring(0, 2), 16)
        G    = [Convert]::ToInt32($hex.Substring(2, 2), 16)
        B    = [Convert]::ToInt32($hex.Substring(4, 2), 16)
    }
}

# The capture itself is gui-lib.ps1 Save-WindowShot: a window-rect copy, so a UI
# Automation rectangle (screen coordinates) maps onto the bitmap by one
# subtraction. shot.ps1 cannot be reused - it launches its own instance of the
# app, and this walk must measure the window it is typing into.

function Stop-App {
    Get-Process pixiv-desktop -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400
}

function Invoke-FocusWalk {
    param([string]$Page, $Ring, $Accent)

    if (-not (Test-Path $Exe)) {
        return [pscustomobject]@{ Page = $Page; Skip = ("no binary at " + $Exe); Stops = @() }
    }
    Stop-App
    $env:PIXIV_DESKTOP_DEMO = "1"
    $env:PIXIV_DESKTOP_SIZE = $Size
    if ($Page -eq "detail") {
        $env:PIXIV_DESKTOP_START_PAGE = "home"
        $env:PIXIV_DESKTOP_START_ILLUST = "700000003"
    }
    else {
        $env:PIXIV_DESKTOP_START_PAGE = $Page
        Remove-Item Env:\PIXIV_DESKTOP_START_ILLUST -ErrorAction SilentlyContinue
    }
    Remove-Item Env:\PIXIV_DESKTOP_START_USER -ErrorAction SilentlyContinue
    Remove-Item Env:\PIXIV_DESKTOP_TIP -ErrorAction SilentlyContinue
    # Pinned, not inherited: the ring colour asserted below is the one for the
    # theme this run forces, so the OS choice must not be able to change it.
    $env:PIXIV_DESKTOP_THEME = if ($Dark) { "dark" } else { "light" }

    $proc = Start-Process -FilePath (Resolve-Path $Exe).Path -PassThru
    try {
        # winit creates a hidden "Thread Event Target" window first, so wait for
        # the titled one (measured in verify-a11y.ps1, same trap).
        for ($i = 0; $i -lt 60; $i++) {
            Start-Sleep -Milliseconds 250
            $proc.Refresh()
            if ($proc.MainWindowHandle -ne [IntPtr]::Zero -and $proc.MainWindowTitle -eq "Pixiv Desktop") { break }
        }
        if ($proc.MainWindowHandle -eq [IntPtr]::Zero) {
            return [pscustomobject]@{ Page = $Page; Skip = "no window after 15s"; Stops = @() }
        }
        $handle = $proc.MainWindowHandle
        [void][Gui]::SetForegroundWindow($handle)
        Start-Sleep -Milliseconds 1800

        # accesskit answers the first query rather than at map time, so a single
        # FindAll right after the window appears can be empty.
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        for ($i = 0; $i -lt 15 -and $all.Count -eq 0; $i++) {
            Start-Sleep -Milliseconds 400
            $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        }
        if ($all.Count -eq 0) {
            return [pscustomobject]@{ Page = $Page; Skip = "no UI Automation tree"; Stops = @() }
        }

        # A Tab goes to the foreground window, which is not necessarily the app.
        # Refuse to type into anything else rather than measure the terminal.
        [void](Focus-AppWindow -ProcessName "pixiv-desktop")
        if ([Drive32]::GetForegroundWindow() -ne $handle) {
            return [pscustomobject]@{ Page = $Page; Skip = "the app is not the foreground window"; Stops = @() }
        }

        $rows = @()
        for ($i = 1; $i -le $Stops; $i++) {
            Send-TabKey -Count 1
            Start-Sleep -Milliseconds 200
            $el = [System.Windows.Automation.AutomationElement]::FocusedElement
            $name = ""
            $type = ""
            $rect = $null
            $enabled = $false
            if ($null -ne $el) {
                $name = $el.Current.Name
                $type = ($el.Current.ControlType.ProgrammaticName -replace "ControlType\.", "")
                $enabled = $el.Current.IsEnabled
                $r = $el.Current.BoundingRectangle
                if ($r.Width -gt 0 -and $r.Height -gt 0) { $rect = $r }
            }

            # UI-NOTES 12h: the pointer is part of the capture. A pointer resting
            # over a control paints a hover scrim and would sit inside the region
            # being measured, so park it in a corner the window is not using.
            Move-Pointer -X 4 -Y 4
            Start-Sleep -Milliseconds 150
            # The theme goes into the name only for a dark run: the default capture
            # keeps the plain focus-<page>-<n> name, and a dark run cannot silently
            # collide with a light baseline and be "kept" as if it were the same.
            $stem = if ($Dark) { "focus-{0}-dark-{1}" -f $Page, $i } else { "focus-{0}-{1}" -f $Page, $i }
            $png = Join-Path $currentDir ($stem + ".png")
            $win = Save-WindowShot -Path $png -Handle $handle

            $ringPx = 0
            $accentPx = 0
            $alpha = 0.0
            $surface = $null
            if ($null -ne $rect) {
                $map = Get-PixelMap -Path $png
                # 2px of margin: the ring is a 2px stroke drawn inside its
                # element, and a rounding error of one pixel at 150% would
                # otherwise clip the outermost row of it.
                $lx = [int]($rect.X - $win.Left) - 2
                $ly = [int]($rect.Y - $win.Top) - 2
                $lw = [int]$rect.Width + 4
                $lh = [int]$rect.Height + 4
                $ringBox = Get-ColorBox -Map $map -R $Ring.R -G $Ring.G -B $Ring.B -X $lx -Y $ly -W $lw -H $lh -Tolerance $Tolerance
                $accentBox = Get-ColorBox -Map $map -R $Accent.R -G $Accent.G -B $Accent.B -X $lx -Y $ly -W $lw -H $lh -Tolerance $Tolerance
                if ($null -ne $ringBox) { $ringPx = $ringBox.Count; $alpha = 1.0 }
                if ($null -ne $accentBox) { $accentPx = $accentBox.Count }

                # A disabled component dims its whole body - components.slint:264
                # for AppButton, :321 for IconButton - so its ring is drawn at that
                # opacity over the surface the element sits on, and a search for
                # the full-strength colour finds nothing. Take that surface as the
                # dominant colour of the region (a 2px stroke is never the majority
                # of the box it outlines) and search the blends the declaration
                # allows, so "drawn but dimmed" is measured instead of waived.
                if ($ringPx -lt $MinPixels -and -not $enabled) {
                    $surface = Get-DominantColor -Map $map -X $lx -Y $ly -W $lw -H $lh
                    if ($null -ne $surface) {
                        foreach ($a in @(0.75, 0.6, 0.5, 0.4, 0.3)) {
                            $br = [int]($a * $Ring.R + (1 - $a) * $surface.R)
                            $bg = [int]($a * $Ring.G + (1 - $a) * $surface.G)
                            $bb = [int]($a * $Ring.B + (1 - $a) * $surface.B)
                            $box = Get-ColorBox -Map $map -R $br -G $bg -B $bb -X $lx -Y $ly -W $lw -H $lh -Tolerance ($Tolerance + 4)
                            if ($null -ne $box -and $box.Count -ge ($MinPixels * 2)) {
                                $ringPx = $box.Count
                                $alpha = $a
                                break
                            }
                        }
                    }
                }
            }

            $verdict = "NONE"
            if ($ringPx -ge $MinPixels -and $alpha -eq 1.0) { $verdict = "RING" }
            elseif ($ringPx -ge ($MinPixels * 2) -and $alpha -gt 0) { $verdict = "RING-DIM" }
            elseif ($accentPx -ge $MinPixels -and $type -eq "Edit") { $verdict = "ACCENT" }

            $baseline = Join-Path $BaselineDir ($stem + ".png")
            $kept = $false
            if (-not (Test-Path $baseline)) {
                Copy-Item $png $baseline
                $kept = $true
            }

            $rows += [pscustomobject]@{
                Stop     = $i
                Name     = $name
                Type     = $type
                X        = if ($null -ne $rect) { [int]$rect.X } else { 0 }
                Y        = if ($null -ne $rect) { [int]$rect.Y } else { 0 }
                W        = if ($null -ne $rect) { [int]$rect.Width } else { 0 }
                H        = if ($null -ne $rect) { [int]$rect.Height } else { 0 }
                Enabled  = $enabled
                Surface  = if ($null -ne $surface) { ("rgb({0},{1},{2})" -f $surface.R, $surface.G, $surface.B) } else { "" }
                Ring     = $ringPx
                Alpha    = $alpha
                Accent   = $accentPx
                Verdict  = $verdict
                Capture  = $png
                Baseline = if ($kept -or (Test-Path $baseline)) { $baseline } else { "" }
            }
        }
        return [pscustomobject]@{ Page = $Page; Skip = $null; Stops = $rows }
    }
    finally {
        Stop-App
    }
}

if (-not [Environment]::UserInteractive) {
    Write-Host "SKIP: no interactive desktop in this session" -ForegroundColor DarkYellow
    exit 77
}

$ring = Read-ThemeColor -Name "focus-ring" -Dark:$Dark
$accent = Read-ThemeColor -Name "accent-fill" -Dark:$Dark
$themeName = if ($Dark) { "dark" } else { "light" }
Write-Host ("theme {0}: focus-ring {1}, text field rule {2}" -f $themeName, $ring.Hex, $accent.Hex)
Write-Host ("pages: {0}  stops per page: {1}  region tolerance: {2}/channel  min pixels: {3}" -f ($Pages -join ", "), $Stops, $Tolerance, $MinPixels)

$sessions = @()
$skips = @()
foreach ($page in $Pages) {
    $walk = Invoke-FocusWalk -Page $page -Ring $ring -Accent $accent
    if ($walk.Skip) {
        $skips += ("{0}: {1}" -f $walk.Page, $walk.Skip)
        Write-Host ("  {0,-9} SKIP {1}" -f $walk.Page, $walk.Skip) -ForegroundColor DarkYellow
        continue
    }
    $sessions += $walk
    Write-Host ""
    Write-Host ("  {0} - {1} stops" -f $walk.Page, $walk.Stops.Count)
    $walk.Stops | ForEach-Object {
        $colour = switch ($_.Verdict) { "RING" { "Green" } "RING-DIM" { "Green" } "ACCENT" { "Green" } default { "Red" } }
        $short = if ($_.Name.Length -gt 40) { $_.Name.Substring(0, 38) } else { $_.Name }
        Write-Host ("    {0,2}  {1,-9} {2,-40} rect={3,4}x{4,-4} at {5,5},{6,-5} ring={7,5} a={8,-4} accent={9,5}  {10}" -f
            $_.Stop, $_.Type, $short, $_.W, $_.H, $_.X, $_.Y, $_.Ring, $_.Alpha, $_.Accent, $_.Verdict) -ForegroundColor $colour
        if ($_.Verdict -eq "RING-DIM") { Write-Host ("        dimmed ring measured against surface " + $_.Surface) -ForegroundColor DarkGray }
    }
}

$allStops = @($sessions | ForEach-Object { $_.Stops } | Where-Object { $null -ne $_ })
$missing = @($allStops | Where-Object { $_.Verdict -eq "NONE" })
$ringStops = @($allStops | Where-Object { $_.Verdict -eq "RING" -or $_.Verdict -eq "RING-DIM" })
$accentStops = @($allStops | Where-Object { $_.Verdict -eq "ACCENT" })

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$summary = [ordered]@{
    capturedAt = (Get-Date).ToString("o")
    theme      = $themeName
    ring       = $ring.Hex
    accent     = $accent.Hex
    pages      = $sessions.Count
    stops      = $allStops.Count
    withRing   = $ringStops.Count
    withAccent = $accentStops.Count
    missing    = $missing.Count
    skipped    = $skips
    detail     = $allStops
}
($summary | ConvertTo-Json -Depth 6) | Set-Content -Path (Join-Path $OutDir "focus-summary.json") -Encoding utf8

Write-Host ""
if ($allStops.Count -eq 0) {
    $reason = if ($skips.Count) { $skips -join "; " } else { "no page produced a walk" }
    Write-Host ("SKIP: {0}" -f $reason) -ForegroundColor DarkYellow
    exit 77
}
Write-Host ("{0} Tab stops on {1} pages: {2} with the focus ring, {3} with the text-field accent rule, {4} invisible" -f `
        $allStops.Count, $sessions.Count, $ringStops.Count, $accentStops.Count, $missing.Count) -ForegroundColor $(if ($missing.Count) { "Red" } else { "Green" })
if ($skips.Count) { Write-Host ("  skipped pages: " + ($skips -join "; ")) -ForegroundColor DarkYellow }
if ($missing.Count -eq 0) { exit 0 }
if ($Report) {
    Write-Host ("{0} invisible stops, not enforced (-Report)" -f $missing.Count) -ForegroundColor Yellow
    exit 0
}
exit 1