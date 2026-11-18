# Layout verification matrix.
#
# For every page and window size: capture a ground-truth screenshot and assert the
# shell invariant that the "half-empty window" bug broke —
#
#     status_y + status_height == shell_height     (gap == 0)
#
# — using the app's own `PIXIV_DESKTOP_TRACE=1` numbers, which report the sizes the
# layout actually resolved to. A positive gap means unpainted canvas below the
# status bar, which is exactly the symptom users reported.
#
# Capture is delegated to tools/shot.ps1 so there is one implementation of
# "restore the window, resize it, copy the screen". That function also crops to the
# client area and writes a sidecar JSON with the scale factor, which is what makes
# the screenshots usable for pixel measurements afterwards.
#
# G57: the child's exit code and this run's own files decide a row. Round twelve
# measured both halves of the old defect: `& pwsh @args | Out-Null` threw the exit
# code away, and the row then read whatever `<page>-<size>.json` the *previous* run
# had left in the output directory. A run with -Exe target\debug\nope.exe therefore
# printed one OK row per page and exited 0, judging last round's trace. Both files of
# a row are now removed before its capture, and a child that fails is that row's
# CAPTURE verdict - the shape tools/verify-visual.ps1:282-323 already used.
#
# Usage:
#   pwsh -File tools/verify-layout.ps1                       # full matrix
#   pwsh -File tools/verify-layout.ps1 -Pages settings -Sizes 1024x768
#   pwsh -File tools/verify-layout.ps1 -Dark
param(
    # G32: empty means "ask tools/page-set.ps1 for the layout selection", which is resolved
    # below; an explicit -Pages, including an empty one, still wins.
    [string[]]$Pages = @(),
    [string[]]$Sizes = @("maximize", "1024x768", "800x600", "720x520"),
    [string]$Exe     = "target\debug\pixiv-desktop.exe",
    [string]$OutDir  = "docs/screenshots/verify",
    [switch]$Dark,
    # Rows the shell must account for. gap must be 0 within this tolerance.
    [double]$Tolerance = 1.0
)

$ErrorActionPreference = "Stop"
# G32: the page list and each page's launch recipe come from tools/page-set.ps1, so this
# matrix cannot walk a page set the verifiers and the budget do not share.
. (Join-Path $PSScriptRoot "page-set.ps1") | Out-Null
$env:PIXIV_DESKTOP_DEMO = "1"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# G32: the shared page set supplies every destination.
if (-not $PSBoundParameters.ContainsKey("Pages")) { $Pages = @(Get-PageSet -For layout) }
$Pages = @($Pages | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$Sizes = @($Sizes | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
Write-Host "pages: $($Pages -join ', ')"
Write-Host "sizes: $($Sizes -join ', ')"

$results = @()
foreach ($size in $Sizes) {
    foreach ($page in $Pages) {
        $suffix = if ($Dark) { "-dark" } else { "" }
        # G32: a name the page set does not know has no recipe, and shot.ps1 would capture
        # the home page under another page's name. That row is a failure, not a gap.
        if ((Get-PageLaunch -Page $page).Count -eq 0) {
            Write-Host ("  {0,-10} {1,-10} NO-PAGE" -f $page, $size) -ForegroundColor Red
            $results += [pscustomobject]@{ Page = $page; Size = $size; Gap = [double]::NaN; Verdict = "PAGE"; Line = "" }
            continue
        }
        # G32: shot.ps1 inherits this environment, so the deep-link keys are set or
        # cleared per page here rather than inside the capture script - one recipe table,
        # in tools/page-set.ps1. A page it can start on by name needs nothing extra.
        Set-PageLaunch -Page $page
        $out = Join-Path $OutDir ("{0}-{1}{2}.png" -f $page, $size, $suffix)
        $metaPath = [System.IO.Path]::ChangeExtension($out, ".json")
        # G57, first half: this row's two files are removed before its capture, so a
        # trace that is on disk afterwards was written by this run. The purge is per
        # row rather than per directory because $OutDir is shared with
        # tools/verify-icons.ps1 - one gate must not delete another gate's evidence.
        Remove-Item -Force $out, $metaPath -ErrorAction SilentlyContinue
        $mode = if ($size -eq "maximize") { "maximize" } else { "size" }
        $args = @("-File", (Join-Path $PSScriptRoot "shot.ps1"), "-Page", $page, "-Mode", $mode,
            "-Size", $size, "-Out", $out, "-Exe", $Exe, "-Trace")
        if ($Dark) { $args += "-Dark" }
        & pwsh @args | Out-Null
        # G57, second half: the child's verdict is the row's verdict. Without this the
        # capture failed, the sidecar from the previous run was still there, and the
        # matrix reported OK for a window that never opened.
        $shotExit = $LASTEXITCODE
        if ($shotExit -ne 0) {
            Write-Host ("  {0,-10} {1,-10} CAPTURE  shot.ps1 exited {2}: nothing was captured, so no trace was read" -f $page, $size, $shotExit) -ForegroundColor Red
            $results += [pscustomobject]@{ Page = $page; Size = $size; Gap = [double]::NaN; Verdict = "CAPTURE"; Line = "" }
            continue
        }

        if (-not (Test-Path $metaPath)) {
            Write-Host ("  {0,-10} {1,-10} NO-CAPTURE" -f $page, $size) -ForegroundColor Red
            $results += [pscustomobject]@{ Page = $page; Size = $size; Gap = [double]::NaN; Verdict = "CAPTURE"; Line = "" }
            continue
        }
        $meta = Get-Content $metaPath -Raw | ConvertFrom-Json
        $line = $meta.trace
        if (-not $line) {
            Write-Host ("  {0,-10} {1,-10} NO-TRACE" -f $page, $size) -ForegroundColor Red
            $results += [pscustomobject]@{ Page = $page; Size = $size; Gap = [double]::NaN; Verdict = "TRACE"; Line = "" }
            continue
        }
        $gap = [double]::NaN
        if ($line -match 'gap=(-?\d+)') { $gap = [double]$Matches[1] }
        $verdict = if ([math]::Abs($gap) -le $Tolerance) { "OK" } else { "GAP" }
        $color = if ($verdict -eq "OK") { "Green" } else { "Red" }
        $short = ($line -replace 'trace ', '')
        Write-Host ("  {0,-10} {1,-10} {2,-6} {3}" -f $page, $size, $verdict, $short) -ForegroundColor $color
        $results += [pscustomobject]@{ Page = $page; Size = $size; Gap = $gap; Verdict = $verdict; Line = $line }
    }
}

$bad = $results | Where-Object { $_.Verdict -ne "OK" }
Write-Host ""
Write-Host ("{0} checks, {1} not OK" -f $results.Count, $bad.Count) -ForegroundColor $(if ($bad.Count) { "Red" } else { "Green" })
if ($bad.Count) {
    $bad | Format-Table -AutoSize | Out-String | Write-Host
    exit 1
}
# G11: zero checks is not a pass. With no page or no size selected this script used
# to fall off the end at exit 0 printing "0 checks, 0 not OK" - measured at HEAD with
# "-Pages , -Sizes ,": a gate that was asked for nothing and reported success. 77 is
# the documented "cannot run on this host" code, which tools/gates.ps1 reports as
# SKIP rather than PASS.
if ($results.Count -eq 0) {
    Write-Host ("SKIP: nothing was checked - the selection resolved to {0} page(s) x {1} size(s)" -f $Pages.Count, $Sizes.Count) -ForegroundColor Yellow
    Write-Host "      Select at least one page and one size (-Pages home -Sizes 1024x768): a run with no check cannot pass." -ForegroundColor Yellow
    exit 77
}
