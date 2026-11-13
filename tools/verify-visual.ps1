# Pixel-regression gate: capture every page in both themes and diff it against a
# tracked baseline.
#
# The repository already measures icon ink and the shell invariant, but the rest of
# the UI was only ever "reviewed" — a human looked at a screenshot and said it
# looked fine. That is how a torn cover band and a left-pinned error block survived
# four rounds. This turns "looks fine" into a number.
#
# Baselines are captured in forced demo mode (PIXIV_DESKTOP_DEMO=1) so they are
# deterministic and contain no personal data: procedural artwork, no session, no
# timer-driven notice. A capture on a different DPI reports *skipped*, not failed,
# because a 150% baseline cannot judge a 100% render.
#
# Usage:
#   pwsh -File tools/verify-visual.ps1                  # capture, diff, assert
#   pwsh -File tools/verify-visual.ps1 -Update          # re-baseline after a fix
#   pwsh -File tools/verify-visual.ps1 -Pages home,settings -Themes light
#   pwsh -File tools/verify-visual.ps1 -SkipCapture     # diff what is on disk
param(
    [string[]]$Pages = @("home", "discovery", "ranking", "search", "bookmarks", "history", "downloads", "settings"),
    [string[]]$Themes = @("light", "dark"),
    [string]$Size = "1024x768",
    [int]$Tolerance = 6,
    [double]$MaxDiffPercent = 0.02,
    [string]$Exe = "target\debug\pixiv-desktop.exe",
    [string]$BaselineDir = "docs/screenshots/baseline",
    [string]$OutDir = "target/visual",
    [switch]$Update,
    [switch]$SkipCapture
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)
Add-Type -AssemblyName System.Drawing

if (-not ('PixDiff' -as [type])) {
# The C# helper needs the *loaded* drawing assembly, not the "System.Drawing"
# name: on PowerShell 7 that name resolves to a forwarding facade and the
# compiler reports CS1069 for Bitmap. Measured, not guessed.
Add-Type -AssemblyName System.Drawing
# ...and on .NET 10 the facade splits again: Bitmap lives in
# System.Private.Windows.GdiPlus, so the reference list is built from the
# assemblies that are actually loaded rather than from guesswork.
try { [void][System.Reflection.Assembly]::Load("System.Private.Windows.GdiPlus") } catch { }
$refs = [AppDomain]::CurrentDomain.GetAssemblies() | Where-Object { -not $_.IsDynamic -and $_.Location } | ForEach-Object { $_.Location }
Add-Type -ReferencedAssemblies $refs @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;

public class PixDiff
{
    // "over|max|mean" on success, "size ...|0|0" when the two captures disagree in
    // shape. A pixel counts as different only if no neighbour within one pixel is
    // within tolerance, which absorbs the sub-pixel text antialiasing that changes
    // with no layout change at all.
    public static string Compare(string pathA, string pathB, int tolerance)
    {
        using (var ia = new Bitmap(pathA))
        using (var ib = new Bitmap(pathB))
        {
            if (ia.Width != ib.Width || ia.Height != ib.Height)
                return string.Format("size {0}x{1} vs {2}x{3}|0|0", ia.Width, ia.Height, ib.Width, ib.Height);

            var rect = new Rectangle(0, 0, ia.Width, ia.Height);
            var da = ia.LockBits(rect, ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
            var db = ib.LockBits(rect, ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
            int stride = da.Stride;
            var ba = new byte[stride * ia.Height];
            var bb = new byte[stride * ia.Height];
            Marshal.Copy(da.Scan0, ba, 0, ba.Length);
            Marshal.Copy(db.Scan0, bb, 0, bb.Length);
            ia.UnlockBits(da);
            ib.UnlockBits(db);

            long over = 0;
            int max = 0;
            double sum = 0;
            for (int y = 0; y < ia.Height; y++)
            {
                int row = y * stride;
                for (int x = 0; x < ia.Width; x++)
                {
                    int i = row + x * 4;
                    int dr = Math.Abs(ba[i + 2] - bb[i + 2]);
                    int dg = Math.Abs(ba[i + 1] - bb[i + 1]);
                    int db0 = Math.Abs(ba[i] - bb[i]);
                    int d = Math.Max(dr, Math.Max(dg, db0));
                    sum += d;
                    if (d > max) max = d;
                    if (d > tolerance && !NeighbourWithin(ba, bb, stride, ia.Width, ia.Height, x, y, tolerance)) over++;
                }
            }
            double total = (double)ia.Width * ia.Height;
            return string.Format("{0:F4}|{1}|{2:F3}", over * 100.0 / total, max, sum / total);
        }
    }

    static bool NeighbourWithin(byte[] a, byte[] b, int stride, int w, int h, int x, int y, int tol)
    {
        int j = y * stride + x * 4;
        for (int ny = Math.Max(0, y - 1); ny <= Math.Min(h - 1, y + 1); ny++)
        {
            for (int nx = Math.Max(0, x - 1); nx <= Math.Min(w - 1, x + 1); nx++)
            {
                int i = ny * stride + nx * 4;
                int d = Math.Max(Math.Abs(a[j] - b[i]), Math.Max(Math.Abs(a[j + 1] - b[i + 1]), Math.Abs(a[j + 2] - b[i + 2])));
                if (d <= tol) return true;
            }
        }
        return false;
    }
}
'@
}

$Pages = @($Pages | ForEach-Object { $_ -split "," } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$Themes = @($Themes | ForEach-Object { $_ -split "," } | ForEach-Object { $_.Trim() } | Where-Object { $_ })

$currentDir = Join-Path $OutDir "current"
New-Item -ItemType Directory -Force -Path $currentDir | Out-Null
if (-not (Test-Path $BaselineDir)) { New-Item -ItemType Directory -Force -Path $BaselineDir | Out-Null }

$env:PIXIV_DESKTOP_DEMO = "1"

$results = @()
foreach ($theme in $Themes) {
    foreach ($page in $Pages) {
        $name = "{0}-{1}" -f $page, $theme
        $png = Join-Path $currentDir "$name.png"
        $shotArgs = @("-File", (Join-Path $PSScriptRoot "shot.ps1"), "-Page", $page, "-Mode", "size", "-Size", $Size, "-Out", $png, "-Exe", $Exe)
        if ($theme -eq "dark") { $shotArgs += "-Dark" }

        if (-not $SkipCapture) {
            & pwsh @shotArgs | Out-Null
            if (-not (Test-Path $png)) {
                $results += [pscustomobject]@{ Page = $page; Theme = $theme; Over = [double]::NaN; Max = 0; Mean = 0; Verdict = "CAPTURE"; Detail = "no capture" }
                continue
            }
        }

        $shotMeta = [System.IO.Path]::ChangeExtension($png, ".json")
        $scale = 0
        if (Test-Path $shotMeta) { $scale = (Get-Content $shotMeta -Raw | ConvertFrom-Json).scale }

        $basePng = Join-Path $BaselineDir "$name.png"
        $baseMeta = Join-Path $BaselineDir "$name.json"

        if ($Update) {
            Copy-Item $png $basePng -Force
            $bitmap = [System.Drawing.Image]::FromFile($png)
            $meta = [ordered]@{ page = $page; theme = $theme; size = $Size; scale = $scale; width = $bitmap.Width; height = $bitmap.Height }
            $bitmap.Dispose()
            ($meta | ConvertTo-Json -Compress) | Set-Content -Path $baseMeta -Encoding utf8
            $results += [pscustomobject]@{ Page = $page; Theme = $theme; Over = 0; Max = 0; Mean = 0; Verdict = "BASE"; Detail = "baseline written" }
            continue
        }

        if (-not (Test-Path $basePng)) {
            $results += [pscustomobject]@{ Page = $page; Theme = $theme; Over = [double]::NaN; Max = 0; Mean = 0; Verdict = "NOBASE"; Detail = "run -Update once" }
            continue
        }

        $baseScale = 0
        if (Test-Path $baseMeta) { $baseScale = (Get-Content $baseMeta -Raw | ConvertFrom-Json).scale }
        if ($baseScale -ne 0 -and $scale -ne 0 -and [math]::Abs($baseScale - $scale) -gt 0.01) {
            $results += [pscustomobject]@{ Page = $page; Theme = $theme; Over = [double]::NaN; Max = 0; Mean = 0; Verdict = "SKIP"; Detail = ("scale {0} vs {1}" -f $scale, $baseScale) }
            continue
        }

        $line = [PixDiff]::Compare($basePng, $png, $Tolerance)
        $parts = $line -split "\|"
        $over = 0.0
        if (-not [double]::TryParse($parts[0], [ref]$over)) {
            $results += [pscustomobject]@{ Page = $page; Theme = $theme; Over = [double]::NaN; Max = 0; Mean = 0; Verdict = "SHAPE"; Detail = $parts[0] }
            continue
        }
        $verdict = if ($over -le $MaxDiffPercent) { "OK" } else { "DIFF" }
        $results += [pscustomobject]@{ Page = $page; Theme = $theme; Over = $over; Max = [int]$parts[1]; Mean = [double]::Parse($parts[2]); Verdict = $verdict; Detail = "" }
    }
}

Write-Host ""
foreach ($r in $results) {
    $colour = switch ($r.Verdict) { "OK" { "Green" } "BASE" { "Yellow" } "SKIP" { "DarkGray" } default { "Red" } }
    $line = "  {0,-10} {1,-6} {2,-6} over={3,8:N4}%  max={4,4}  mean={5,6:N2}  {6}" -f $r.Page, $r.Theme, $r.Verdict, $r.Over, $r.Max, $r.Mean, $r.Detail
    Write-Host $line -ForegroundColor $colour
}

$results | ConvertTo-Json -Depth 4 | Set-Content -Path (Join-Path $OutDir "visual-summary.json") -Encoding utf8

$bad = @($results | Where-Object { $_.Verdict -notin @("OK", "BASE", "SKIP") })
Write-Host ""
if ($bad.Count -eq 0) {
    Write-Host ("{0} captures within {1}% of the baseline" -f $results.Count, $MaxDiffPercent) -ForegroundColor Green
    exit 0
}
Write-Host ("{0} of {1} captures differ from the baseline" -f $bad.Count, $results.Count) -ForegroundColor Red
exit 1
