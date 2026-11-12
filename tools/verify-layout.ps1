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
# Usage:
#   pwsh -File tools/verify-layout.ps1                       # full matrix
#   pwsh -File tools/verify-layout.ps1 -Pages settings -Sizes 1024x768
#   pwsh -File tools/verify-layout.ps1 -Dark
param(
    [string[]]$Pages = @("home", "discovery", "ranking", "search", "bookmarks", "history", "downloads", "settings"),
    [string[]]$Sizes = @("maximize", "1024x768", "800x600"),
    [string]$Exe     = "target\debug\pixiv-desktop.exe",
    [string]$OutDir  = "docs/screenshots/verify",
    [switch]$Dark,
    # Rows the shell must account for. gap must be 0 within this tolerance.
    [double]$Tolerance = 1.0
)

$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$Pages = @($Pages | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$Sizes = @($Sizes | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
Write-Host "pages: $($Pages -join ', ')"
Write-Host "sizes: $($Sizes -join ', ')"

$results = @()
foreach ($size in $Sizes) {
    foreach ($page in $Pages) {
        $suffix = if ($Dark) { "-dark" } else { "" }
        $out = Join-Path $OutDir ("{0}-{1}{2}.png" -f $page, $size, $suffix)
        $mode = if ($size -eq "maximize") { "maximize" } else { "size" }
        $args = @("-File", (Join-Path $PSScriptRoot "shot.ps1"), "-Page", $page, "-Mode", $mode,
            "-Size", $size, "-Out", $out, "-Exe", $Exe, "-Trace")
        if ($Dark) { $args += "-Dark" }
        & pwsh @args | Out-Null

        $metaPath = [System.IO.Path]::ChangeExtension($out, ".json")
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
