# Wheel-zoom direction, measured through the real input path.
#
# The defect this gate exists for: scrolling up shrank the picture. It could not be
# caught by a unit test alone, because the bug was a *sign convention* agreed on by
# two places that were never compared - Slint forwards winit's delta unchanged
# (wheel up = +60), and src/zoom.rs multiplied by (1 - delta*0.0015), so +60 meant
# "zoom out". src/zoom.rs's test asserted the same inverted convention, so the unit
# suite was green while the application was wrong.
#
# So this gate never touches arithmetic. It moves the pointer over the artwork,
# sends a real WM_MOUSEWHEEL through tools/drive.ps1, and reads the zoom percentage
# back out of the accessibility tree - the same number a user sees. Nothing here can
# pass unless the whole chain agrees: winit -> i-slint-backend-winit -> Slint's
# scroll-event -> the Rust callback -> the property -> the label.
#
# Usage:
#   pwsh -File tools/verify-zoom.ps1
#   pwsh -File tools/verify-zoom.ps1 -Exe target\release\pixiv-desktop.exe
param(
    [string]$Exe = "target\debug\pixiv-desktop.exe",
    # A wheel notch. Windows defines WHEEL_DELTA as +120 for "away from the user",
    # which every desktop application calls "scroll up".
    [int]$Notch = 120,
    [int]$LaunchTimeoutSec = 40
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot "gui-lib.ps1")
. (Join-Path $PSScriptRoot "drive.ps1")

$exePath = if ([System.IO.Path]::IsPathRooted($Exe)) { $Exe } else { Join-Path $root $Exe }
if (-not (Test-Path $exePath)) { throw "no such executable: $exePath" }

# Forced demo mode: the artwork detail page is reachable without a session, and the
# capture does not depend on whether this machine holds a live cookie.
$env:PIXIV_DESKTOP_DEMO = '1'
$env:PIXIV_DESKTOP_SIZE = '1280x860'
$env:PIXIV_DESKTOP_START_ILLUST = '700000003'
$env:RUST_LOG = 'warn'
Remove-Item Env:PIXIV_DESKTOP_FIXTURE -ErrorAction SilentlyContinue

$proc = $null
$failures = @()
function Fail([string]$Message) {
    $script:failures += $Message
    Write-Host ("  FAIL  " + $Message) -ForegroundColor Red
}
function Pass([string]$Message) {
    Write-Host ("  ok    " + $Message) -ForegroundColor Green
}

<#
  Read the zoom percentage the page displays. It is the tooltip-free Label inside the
  viewer's control pill, so the only nodes that end in a percent sign are the pill's
  readout and the "actual size (100%)" button tip; the readout is told apart by not
  being a button.
#>
function Get-ZoomPercent {
    $rootElement = Get-UiRoot
    foreach ($n in $rootElement.FindAll([System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition)) {
        $name = $n.Current.Name
        if ($name -match '^\s*(\d+)%\s*$') {
            return [int]$Matches[1]
        }
    }
    return $null
}

function Get-ViewerCentre {
    # The artwork surface spans the page between the toolbar and the thumbnail strip,
    # and between the navigation rail and the information panel. Anchoring on the UIA
    # rectangle of the "适应窗口" button is not enough - a wheel over the pill would
    # still work, but it would not prove the *artwork* responds. The centre of the
    # window minus the chrome is the honest target, and it is where a user scrolls.
    $h = Get-AppWindow
    $rect = New-Object Gui+RECT
    [void][Gui]::GetWindowRect($h, [ref]$rect)
    $scale = 1.0
    $meta = $null
    # Left rail (~200) and right info panel (~360 at this width) are outside the
    # viewer; the toolbar (44) and strip (68) are outside it vertically.
    $x = [int]($rect.Left + ($rect.Right - $rect.Left) * 0.45)
    $y = [int]($rect.Top + ($rect.Bottom - $rect.Top) * 0.42)
    return @($x, $y)
}

try {
    Get-Process pixiv-desktop -ErrorAction SilentlyContinue | ForEach-Object {
        try { $_.Kill(); $_.WaitForExit(2000) | Out-Null } catch { }
    }
    Start-Sleep -Milliseconds 400

    Write-Host "launching $exePath"
    $proc = Start-Process -FilePath $exePath -PassThru

    $deadline = (Get-Date).AddSeconds($LaunchTimeoutSec)
    $percent = $null
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 700
        try { $percent = Get-ZoomPercent } catch { $percent = $null }
        if ($null -ne $percent) { break }
    }
    if ($null -eq $percent) {
        # No window, or a window with no viewer: either way nothing below can run.
        Fail "the detail page never reported a zoom percentage"
        throw "cannot continue"
    }
    [void](Focus-AppWindow)
    Start-Sleep -Milliseconds 500

    $before = Get-ZoomPercent
    Write-Host "baseline zoom: $before%"

    $point = Get-ViewerCentre
    Write-Host ("pointer target: " + $point[0] + "," + $point[1])

    # ---- wheel UP must zoom IN ----
    Invoke-WheelAt -X $point[0] -Y $point[1] -Delta $Notch
    Invoke-WheelAt -X $point[0] -Y $point[1] -Delta $Notch
    Start-Sleep -Milliseconds 400
    $afterUp = Get-ZoomPercent
    if ($null -eq $afterUp) {
        Fail "no zoom readout after wheel up"
    } elseif ($afterUp -le $before) {
        Fail "wheel up must zoom IN: $before% -> $afterUp%"
    } else {
        Pass "wheel up zooms in: $before% -> $afterUp%"
    }

    # ---- wheel DOWN must zoom OUT, back through the start point ----
    for ($i = 0; $i -lt 4; $i++) {
        Invoke-WheelAt -X $point[0] -Y $point[1] -Delta (-$Notch)
    }
    Start-Sleep -Milliseconds 400
    $afterDown = Get-ZoomPercent
    if ($null -eq $afterDown) {
        Fail "no zoom readout after wheel down"
    } elseif ($afterDown -ge $afterUp) {
        Fail "wheel down must zoom OUT: $afterUp% -> $afterDown%"
    } else {
        Pass "wheel down zooms out: $afterUp% -> $afterDown%"
    }

    # Two notches in and four out must land below the start, not above it. This is
    # what catches a fix that only reversed the label instead of the transform.
    if ($null -ne $afterDown -and $afterDown -gt $before) {
        Fail "a net downward scroll must end below the start: $before% -> $afterDown%"
    } elseif ($null -ne $afterDown) {
        Pass "a net downward scroll ends below the start ($before% -> $afterDown%)"
    }

    # ---- the button and the wheel must agree ----
    # "Actual size" is 1:1, i.e. exactly 100%, in both fit and actual-size mode.
    $actual = Find-Node -Name '实际尺寸 (100%)'
    if ($null -eq $actual) {
        Fail "the viewer has no actual-size control"
    } else {
        [void](Click-Node $actual)
        Start-Sleep -Milliseconds 500
        $atActual = Get-ZoomPercent
        if ($null -eq $atActual) {
            Fail "no zoom readout after pressing actual size"
        } elseif ([Math]::Abs($atActual - 100) -gt 1) {
            Fail "actual size must draw at 100%, read $atActual%"
        } else {
            Pass "actual size draws at 100% (read $atActual%)"
        }
    }

    # ---- the wheel keeps working from actual-size mode ----
    Invoke-WheelAt -X $point[0] -Y $point[1] -Delta $Notch
    Start-Sleep -Milliseconds 400
    $afterActualUp = Get-ZoomPercent
    if ($null -ne $afterActualUp -and $null -ne $atActual -and $afterActualUp -gt $atActual) {
        Pass "wheel up still zooms in from actual size: $atActual% -> $afterActualUp%"
    } else {
        Fail "wheel up from actual size: $atActual% -> $afterActualUp%"
    }
}
catch {
    if ($_.Exception.Message -ne "cannot continue") {
        Fail ("exception: " + $_.Exception.Message)
    }
}
finally {
    if ($proc -and -not $proc.HasExited) {
        try { [void]$proc.CloseMainWindow() } catch { }
        if (-not $proc.WaitForExit(5000)) {
            try { $proc.Kill() } catch { }
        }
    }
}

Write-Host ""
if ($failures.Count -eq 0) {
    Write-Host "zoom: PASS" -ForegroundColor Green
    exit 0
}
Write-Host ("zoom: FAIL (" + $failures.Count + ")") -ForegroundColor Red
foreach ($f in $failures) { Write-Host ("  - " + $f) }
exit 1
