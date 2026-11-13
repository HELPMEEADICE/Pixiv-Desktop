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
    [int]$TabSteps = 10,
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

function Measure-Page {
    param([string]$Page)

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
        if ($proc.MainWindowHandle -eq [IntPtr]::Zero) { throw "no window for page '$Page'" }
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
            throw ("no accessibility tree on page '{0}': root name '{1}' class '{2}' title '{3}'" -f $Page, $root.Current.Name, $root.Current.ClassName, $proc.MainWindowTitle)
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
        if (-not $SkipTab) {
            [void][Drive32]::SetForegroundWindow($proc.MainWindowHandle)
            Start-Sleep -Milliseconds 200
            # A Tab key goes to whatever window is in front, which is not
            # necessarily the one under test: a stray keystroke lands in the
            # terminal that started this script. Measure the foreground window
            # first and refuse to type into anything else.
            $foreground = [Drive32]::GetForegroundWindow()
            if ($foreground -ne $proc.MainWindowHandle) {
                throw ("refusing to synthesise Tab: foreground window {0} is not the app ({1})" -f $foreground, $proc.MainWindowHandle)
            }
            for ($i = 0; $i -lt $TabSteps; $i++) {
                Send-TabKey -Count 1
                $f = Get-FocusedInfo
                if ($f.Name) { $tab += $f.Name } elseif ($f.Type) { $tab += "<$($f.Type)>" } else { $tab += "<none>" }
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
            distinctTab    = @($tab | Where-Object { $_ -ne "<none>" -and $_ -ne "" } | Select-Object -Unique).Count
        }
    }
    finally {
        Stop-App
    }
}

$results = @()
foreach ($page in $Pages) {
    $r = Measure-Page -Page $page
    $results += $r
    $types = ($r.byType.GetEnumerator() | Sort-Object Value -Descending | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join " "
    $line = "  {0,-10} nodes={1,-4} invokable={2,-4} focusable={3,-3} tabStops={4,-3} {5}" -f $r.page, $r.total, $r.invokable, $r.focusable, $r.distinctTab, $types
    Write-Host $line
}

$results | ConvertTo-Json -Depth 6 | Set-Content -Path (Join-Path $OutDir "a11y-summary.json") -Encoding utf8

# ---------------------------------------------------------------- budget
function Read-Budget {
    if (-not (Test-Path $Budget)) { return $null }
    return Get-Content $Budget -Raw | ConvertFrom-Json
}

function Get-Need {
    param($Page)
    $b = Read-Budget
    if ($null -eq $b) { return $null }
    $need = [ordered]@{}
    foreach ($key in @("minInvokable", "minFocusable", "minTabStops", "roles")) {
        $value = $null
        if ($b.pages -and ($b.pages.PSObject.Properties.Name -contains $Page)) { $value = $b.pages.$Page.$key }
        if ($null -eq $value) { $value = $b.default.$key }
        $need[$key] = $value
    }
    return $need
}

if ($Update) {
    $doc = [ordered]@{
        _comment = "Accessibility budget: what each page must expose to UI Automation after round five. Written by tools/verify-a11y.ps1 -Update; the numbers are measured, not chosen. invokable = elements with an InvokePattern, distinctTab = distinct stops a real Tab walk reached."
        default  = [ordered]@{ minInvokable = 1; minFocusable = 2; minTabStops = 3; roles = @("Button") }
        pages    = [ordered]@{}
    }
    foreach ($r in $results) {
        $doc.pages[$r.page] = [ordered]@{
            minInvokable = $r.invokable
            minFocusable = $r.focusable
            minTabStops  = $r.distinctTab
        }
    }
    ($doc | ConvertTo-Json -Depth 6) | Set-Content -Path $Budget -Encoding utf8
    Write-Host ""
    Write-Host "budget re-pinned to today's measurement: $Budget" -ForegroundColor Yellow
    exit 0
}

$budget = Read-Budget
if ($null -eq $budget) {
    Write-Host ""
    Write-Host "no budget at $Budget - run with -Update once to pin today's numbers" -ForegroundColor Yellow
    exit 0
}

$failures = @()
foreach ($r in $results) {
    $need = Get-Need -Page $r.page
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
