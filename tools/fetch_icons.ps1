# Fetches Microsoft Fluent UI System Icons (MIT) and converts them into themeable
# Slint `Path` components. Path geometry is extracted from the SVG `d` attributes so
# icons inherit the active theme colour instead of carrying a baked-in fill.
#
# Usage:
#   pwsh -File tools/fetch_icons.ps1            # fetch, cache the SVGs, rewrite the Slint
#   pwsh -File tools/fetch_icons.ps1 -Check     # compare only: no network, no writes
#
# E10: -Check is what makes this script a gate instead of a one-off. ui/icons.slint is
# checked in and every glyph in the app comes from it, but until round twelve the only
# script that can regenerate it was referenced by nothing, so a hand edit could drift from
# the geometry the tree ships and no run would say so. -Check derives the file from the
# tracked assets/icons/raw/*.svg - the same conversion, no Invoke-WebRequest - and
# compares the result with what is on disk: exit 0 when they match, exit 1 naming the
# first line that differs. tools/gates.ps1 runs it, so a drift fails the suite.
param(
    [string]$OutSlint = "ui/icons.slint",
    [string]$RawDir   = "assets/icons/raw",
    [switch]$Check
)

$ErrorActionPreference = "Stop"
$ProgressPreference    = "SilentlyContinue"

# logical name -> Fluent asset folder name
$icons = [ordered]@{
    "home"          = "Home"
    "discover"      = "CompassNorthwest"
    "ranking"       = "Trophy"
    "follow"        = "Heart"
    "search"        = "Search"
    "history"       = "History"
    "downloads"     = "ArrowDownload"
    "artist"        = "Person"
    "settings"      = "Settings"
    "notification"  = "Alert"
    "light"         = "WeatherSunny"
    "dark"          = "WeatherMoon"
    "back"          = "ArrowLeft"
    "forward"       = "ArrowRight"
    "refresh"       = "ArrowClockwise"
    "chevron-down"  = "ChevronDown"
    "chevron-left"  = "ChevronLeft"
    "chevron-right" = "ChevronRight"
    "close"         = "Dismiss"
    "minimise"      = "Subtract"
    "maximise"      = "Square"
    "restore"       = "SquareMultiple"
    "more"          = "MoreHorizontal"
    "bookmark"      = "Bookmark"
    "star"          = "Star"
    "open"          = "Open"
    "copy"          = "Copy"
    "filter"        = "Filter"
    "calendar"      = "Calendar"
    "book"          = "Book"
    "font"          = "TextFont"
    "check"         = "Checkmark"
    "info"          = "Info"
    "warning"       = "Warning"
    "delete"        = "Delete"
    "pin"           = "Pin"
    "image-off"     = "ImageOff"
    "folder"        = "FolderOpen"
    "play"          = "Play"
    "pause"         = "Pause"
    "zoom-in"       = "ZoomIn"
    "zoom-out"      = "ZoomOut"
    "fit"           = "ArrowMinimize"
    "fullscreen"    = "FullScreenMaximize"
    "actual-size"   = "ArrowExpand"
    "novel"         = "BookOpen"
    "tag"           = "Tag"
    "clock"         = "Clock"
    "grid"          = "Grid"
    "comment"       = "Comment"
    "share"         = "Share"
    "document"      = "Document"
    "arrow-up"      = "ArrowUp"
    "dismiss"       = "DismissCircle"
    "eye"           = "Eye"
    "sparkle"       = "Sparkle"
    "shield"        = "Shield"
    "key"           = "Key"
}

$base = "https://raw.githubusercontent.com/microsoft/fluentui-system-icons/main/assets"
# -Check writes nothing, so it does not create the cache directory either: a missing
# directory is a fact about the tree, not something to fix from inside a comparison.
if (-not $Check) { New-Item -ItemType Directory -Force -Path $RawDir | Out-Null }

function Get-IconSvg {
    param([string]$Folder, [string]$Snake, [int]$Size)
    # Upstream folders are space separated ("Arrow Left"), files are snake_case.
    $spaced = ($Folder -creplace '([a-z0-9])([A-Z])', '$1 $2')
    foreach ($dir in @($spaced, $Folder)) {
        $u = "$base/" + ($dir -replace ' ', '%20') + "/SVG/ic_fluent_${Snake}_${Size}_regular.svg"
        try {
            $r = Invoke-WebRequest -Uri $u -TimeoutSec 25 -UseBasicParsing
            if ($r.StatusCode -eq 200) { return $r.Content }
        } catch { }
    }
    return $null
}

# Extract every path `d` and rect->path so the geometry can be replayed in Slint.
function ConvertTo-Commands {
    param([string]$Svg)
    $cmds = New-Object System.Collections.Generic.List[string]
    foreach ($m in [regex]::Matches($Svg, 'd="([^"]+)"')) { $cmds.Add($m.Groups[1].Value) }
    # A few Fluent icons are drawn as <rect>; replay those as rectangle paths.
    foreach ($m in [regex]::Matches($Svg, '<rect[^>]*>')) {
        $tag = $m.Value
        $get = { param($n, $d) $mm = [regex]::Match($tag, "$n=`"([0-9.]+)`""); if ($mm.Success) { [double]$mm.Groups[1].Value } else { $d } }
        $x = & $get 'x' 0; $y = & $get 'y' 0; $w = & $get 'width' 0; $h = & $get 'height' 0
        if ($w -gt 0 -and $h -gt 0) {
            $cmds.Add("M$x $y L$($x+$w) $y L$($x+$w) $($y+$h) L$x $($y+$h) Z")
        }
    }
    return ($cmds -join " ")
}

$sb = New-Object System.Text.StringBuilder
[void]$sb.AppendLine("// @generated by tools/fetch_icons.ps1 -- do not edit by hand.")
[void]$sb.AppendLine("// Source: Microsoft Fluent UI System Icons (https://github.com/microsoft/fluentui-system-icons)")
[void]$sb.AppendLine("// License: MIT. Only the path geometry is embedded; colour comes from the theme.")
[void]$sb.AppendLine("")
[void]$sb.AppendLine("export component FluentIcon inherits Path {")
[void]$sb.AppendLine("    in property <brush> tint: #000000;")
[void]$sb.AppendLine("    viewbox-width: 24;")
[void]$sb.AppendLine("    viewbox-height: 24;")
[void]$sb.AppendLine("    width: 18px;")
[void]$sb.AppendLine("    height: 18px;")
[void]$sb.AppendLine("    fill: root.tint;")
[void]$sb.AppendLine("    clip: false;")
[void]$sb.AppendLine("}")
[void]$sb.AppendLine("")

$entries = New-Object System.Collections.Generic.List[object]
$ok = 0; $failed = @(); $uncached = @()
foreach ($kv in $icons.GetEnumerator()) {
    $name = $kv.Key; $folder = $kv.Value
    $snake = ($folder -creplace '([a-z0-9])([A-Z])', '$1_$2').ToLower()
    $cache = Join-Path $RawDir "$snake.svg"
    if ($Check) {
        # The tracked SVG is the input, so the comparison needs no network and answers
        # the question a gate can act on: is the emitted file still what this conversion
        # produces from the assets in the tree?
        if (-not (Test-Path $cache)) { $uncached += "$name ($cache)"; continue }
        $svg = Get-Content $cache -Raw
    }
    else {
        $svg = Get-IconSvg -Folder $folder -Snake $snake -Size 24
        if (-not $svg) { $failed += "$name ($folder/$snake)"; continue }
        Set-Content -Path $cache -Value $svg -Encoding utf8
    }

    $cmdText = ConvertTo-Commands -Svg $svg
    if ([string]::IsNullOrWhiteSpace($cmdText)) { $failed += "$name (no geometry)"; continue }

    $pascal = ($name -split '[-_]') | Where-Object { $_ -ne '' } | ForEach-Object { $_.Substring(0,1).ToUpper() + $_.Substring(1) }
    $comp = "Icon" + ($pascal -join '')
    $entries.Add([pscustomobject]@{ Name = $name; Comp = $comp; Cmd = $cmdText })
    $ok++
}

# 1) one component per icon, for direct use
foreach ($e in $entries) {
    [void]$sb.AppendLine("export component $($e.Comp) inherits FluentIcon {")
    [void]$sb.AppendLine("    commands: `"$($e.Cmd)`";")
    [void]$sb.AppendLine("}")
    [void]$sb.AppendLine("")
}

# 2) a path-constant global, so icons can be selected by name at runtime
[void]$sb.AppendLine("// Path geometry as string constants; used by FluentGlyph below.")
[void]$sb.AppendLine("export global IconPaths {")
foreach ($e in $entries) { [void]$sb.AppendLine("    out property <string> $($e.Name): `"$($e.Cmd)`";") }
[void]$sb.AppendLine("}")
[void]$sb.AppendLine("")

# 3) a single name-dispatched glyph, so buttons/nav can pick an icon by string
[void]$sb.AppendLine("// One reusable glyph: pick any icon by name. Unknown names render nothing.")
[void]$sb.AppendLine("export component FluentGlyph inherits FluentIcon {")
[void]$sb.AppendLine("    in property <string> name: `"`";")
$chain = ($entries | ForEach-Object { "name == `"$($_.Name)`" ? IconPaths.$($_.Name)" }) -join " : `n        "
[void]$sb.AppendLine("    commands: $chain : `"`";")
[void]$sb.AppendLine("}")

if ($failed.Count -gt 0) { Write-Host "MISSING: $($failed -join ', ')" -ForegroundColor Yellow }

if ($Check) {
    # Compare, never write. Both sides are read as text, split on a bare LF, and their
    # trailing blank lines dropped: how many newlines the *writer* appended is not part
    # of the derived geometry, and the tracked file may be checked out with CRLF.
    function Get-Line {
        param([string]$Text)
        $crlf = [string][char]13 + [char]10
        $lf = [string][char]10
        $lines = @(($Text -replace $crlf, $lf) -split $lf)
        $end = $lines.Count
        while ($end -gt 0 -and $lines[$end - 1] -eq "") { $end-- }
        if ($end -eq 0) { return @() }
        return @($lines[0..($end - 1)])
    }
    if ($uncached.Count -gt 0) {
        Write-Host ("FAIL no cached SVG for {0} icon(s): {1}" -f $uncached.Count, ($uncached -join ", ")) -ForegroundColor Red
        Write-Host "      -Check reads $RawDir instead of the network, so an icon without a tracked SVG cannot be compared" -ForegroundColor Red
        exit 1
    }
    if ($ok -eq 0) {
        Write-Host "FAIL no icon produced any geometry, so there is nothing to compare" -ForegroundColor Red
        exit 1
    }
    if (-not (Test-Path $OutSlint)) {
        Write-Host ("FAIL {0} does not exist: run without -Check to write it" -f $OutSlint) -ForegroundColor Red
        exit 1
    }
    $mine = @(Get-Line -Text ($sb.ToString()))
    $theirs = @(Get-Line -Text (Get-Content $OutSlint -Raw))
    $at = -1
    $same = ($mine.Count -eq $theirs.Count)
    if ($same) {
        for ($i = 0; $i -lt $mine.Count; $i++) {
            if ($mine[$i] -cne $theirs[$i]) { $same = $false; $at = $i; break }
        }
    }
    else {
        $at = [math]::Min($mine.Count, $theirs.Count)
    }
    if ($same) {
        Write-Host ("icons match: {0} icons derived from {1}, {2} lines, no difference" -f $ok, $RawDir, $mine.Count) -ForegroundColor Green
        exit 0
    }
    Write-Host ("FAIL {0} is not what this script derives from {1} - first difference at line {2}" -f $OutSlint, $RawDir, ($at + 1)) -ForegroundColor Red
    if ($at -lt $theirs.Count) { Write-Host ("      on disk: {0}" -f $theirs[$at].Trim()) -ForegroundColor Red }
    if ($at -lt $mine.Count) { Write-Host ("      derived: {0}" -f $mine[$at].Trim()) -ForegroundColor Red }
    if ($mine.Count -ne $theirs.Count) {
        Write-Host ("      {0} line(s) on disk, {1} derived" -f $theirs.Count, $mine.Count) -ForegroundColor Red
    }
    Write-Host "      run without -Check to rewrite it, and read the diff before committing" -ForegroundColor Red
    exit 1
}

Set-Content -Path $OutSlint -Value $sb.ToString() -Encoding utf8
Write-Host "icons written: $ok -> $OutSlint"

