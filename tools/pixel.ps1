# Pixel-level geometry probes for the UI regression harness.
#
# Why this exists: a Slint declaration saying `width: 20px; height: 20px` does not
# tell you where the renderer actually put the ink. Icon alignment has to be
# measured from pixels, so this module reads a capture and reports ink runs and
# bounding boxes.
#
# Performance note: PowerShell function calls cost tens of microseconds, so a
# per-pixel helper turns a 30k-pixel strip into a ten-second operation. The hot
# paths below therefore walk the locked bitmap byte array directly, with no
# function call inside the loops.

Add-Type -AssemblyName System.Drawing

function Get-PixelMap {
    param([Parameter(Mandatory)][string]$Path)
    $img = [System.Drawing.Bitmap]::FromFile((Resolve-Path $Path))
    $rect = New-Object System.Drawing.Rectangle 0, 0, $img.Width, $img.Height
    $data = $img.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $bytes = New-Object byte[] ($data.Stride * $img.Height)
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
    $map = [pscustomobject]@{
        Width  = $img.Width
        Height = $img.Height
        Stride = $data.Stride
        Bytes  = $bytes
    }
    $img.UnlockBits($data)
    $img.Dispose()
    return $map
}

<#
  Which rows of a strip contain "ink"?

  "Ink" = a pixel whose |dR|+|dG|+|dB| distance from that row's reference pixel
  exceeds -Tolerance. The reference is taken just outside the strip (X - Margin)
  on the same row, so a strip cut through a glyph column still compares against
  the flat surface it sits on. Returns a bool[] indexed by row offset.
#>
function Get-RowInkFlags {
    param(
        $Map,
        [Parameter(Mandatory)][int]$X,
        [Parameter(Mandatory)][int]$W,
        [Parameter(Mandatory)][int]$Y,
        [Parameter(Mandatory)][int]$H,
        [int]$Tolerance = 60,
        [int]$RefOffset = -4
    )
    $bytes = $Map.Bytes
    $stride = $Map.Stride
    $flags = New-Object bool[] $H
    for ($row = 0; $row -lt $H; $row++) {
        $base = ($Y + $row) * $stride
        $refX = $X + $RefOffset
        if ($refX -lt 0) { $refX = $X + $W }   # fall back to the right edge
        $ro = $base + $refX * 4
        $rb = $bytes[$ro]; $rg = $bytes[$ro + 1]; $rr = $bytes[$ro + 2]
        $hit = $false
        for ($col = 0; $col -lt $W; $col++) {
            $o = $base + ($X + $col) * 4
            $db = [math]::Abs($bytes[$o] - $rb)
            if ($db -gt $Tolerance) { $hit = $true; break }
            $dg = [math]::Abs($bytes[$o + 1] - $rg)
            $db += $dg
            if ($db -gt $Tolerance) { $hit = $true; break }
            $dr = [math]::Abs($bytes[$o + 2] - $rr)
            if (($db + $dr) -gt $Tolerance) { $hit = $true; break }
        }
        $flags[$row] = $hit
    }
    return $flags
}

<# Group contiguous ink rows into runs, expressed in absolute image coordinates. #>
function Get-InkRuns {
    param(
        $Map,
        [Parameter(Mandatory)][int]$X,
        [Parameter(Mandatory)][int]$W,
        [Parameter(Mandatory)][int]$Y,
        [Parameter(Mandatory)][int]$H,
        [int]$Tolerance = 60,
        [int]$Gap = 1
    )
    $flags = Get-RowInkFlags $Map $X $W $Y $H $Tolerance
    $runs = New-Object System.Collections.Generic.List[object]
    $inRun = $false; $start = 0; $last = 0
    for ($i = 0; $i -lt $H; $i++) {
        if ($flags[$i]) {
            if (-not $inRun) { $inRun = $true; $start = $i }
            $last = $i
        } elseif ($inRun -and ($i - $last) -gt $Gap) {
            $runs.Add([pscustomobject]@{
                    Top = $Y + $start; Bottom = $Y + $last
                    Center = $Y + $start + ($last - $start + 1) / 2.0
                })
            $inRun = $false
        }
    }
    if ($inRun) {
        $runs.Add([pscustomobject]@{
                Top = $Y + $start; Bottom = $Y + $last
                Center = $Y + $start + ($last - $start + 1) / 2.0
            })
    }
    return $runs
}

<# Ink bounding box inside a region, background sampled from just outside it. #>
function Get-InkBox {
    param(
        $Map,
        [Parameter(Mandatory)][int]$X,
        [Parameter(Mandatory)][int]$Y,
        [Parameter(Mandatory)][int]$W,
        [Parameter(Mandatory)][int]$H,
        [int]$Tolerance = 60,
        [int]$RefX = -1,
        [int]$RefY = -1
    )
    $bytes = $Map.Bytes; $stride = $Map.Stride
    if ($RefX -lt 0) { $RefX = $X - 4 }
    if ($RefX -lt 0) { $RefX = $X + $W }
    if ($RefY -lt 0) { $RefY = $Y }
    $ro = $RefY * $stride + $RefX * 4
    $rb = $bytes[$ro]; $rg = $bytes[$ro + 1]; $rr = $bytes[$ro + 2]

    $minX = [int]::MaxValue; $minY = [int]::MaxValue; $maxX = -1; $maxY = -1; $ink = 0
    for ($row = 0; $row -lt $H; $row++) {
        $base = ($Y + $row) * $stride
        for ($col = 0; $col -lt $W; $col++) {
            $o = $base + ($X + $col) * 4
            $d = [math]::Abs($bytes[$o] - $rb) + [math]::Abs($bytes[$o + 1] - $rg) + [math]::Abs($bytes[$o + 2] - $rr)
            if ($d -le $Tolerance) { continue }
            $ink++
            if ($col -lt $minX) { $minX = $col }
            if ($col -gt $maxX) { $maxX = $col }
            if ($row -lt $minY) { $minY = $row }
            if ($row -gt $maxY) { $maxY = $row }
        }
    }
    if ($ink -eq 0) { return $null }
    return [pscustomobject]@{
        X       = $X + $minX
        Y       = $Y + $minY
        W       = $maxX - $minX + 1
        H       = $maxY - $minY + 1
        Ink     = $ink
        CenterX = $X + $minX + ($maxX - $minX + 1) / 2.0
        CenterY = $Y + $minY + ($maxY - $minY + 1) / 2.0
    }
}


<#
  Count pixels within -Tolerance of an absolute colour inside a region, and
  report how many there are and where they are.

  Get-InkBox cannot answer this: it compares every pixel against a reference
  pixel sampled outside the region, which finds "something was drawn here", not
  "this specific colour was drawn here". A focus ring has to be matched by its
  own value, because the surface under it changes with the theme.
#>
function Get-ColorBox {
    param(
        $Map,
        [Parameter(Mandatory)][int]$R,
        [Parameter(Mandatory)][int]$G,
        [Parameter(Mandatory)][int]$B,
        [Parameter(Mandatory)][int]$X, [Parameter(Mandatory)][int]$Y,
        [Parameter(Mandatory)][int]$W, [Parameter(Mandatory)][int]$H,
        [int]$Tolerance = 10
    )
    $bytes = $Map.Bytes
    $stride = $Map.Stride
    $x0 = [math]::Max(0, $X)
    $y0 = [math]::Max(0, $Y)
    $x1 = [math]::Min($Map.Width - 1, $X + $W - 1)
    $y1 = [math]::Min($Map.Height - 1, $Y + $H - 1)
    if ($x1 -lt $x0 -or $y1 -lt $y0) { return $null }
    $count = 0
    $minX = [int]::MaxValue; $minY = [int]::MaxValue; $maxX = -1; $maxY = -1
    for ($row = $y0; $row -le $y1; $row++) {
        $base = $row * $stride
        for ($col = $x0; $col -le $x1; $col++) {
            $o = $base + $col * 4
            if ([math]::Abs($bytes[$o + 2] - $R) -gt $Tolerance) { continue }
            if ([math]::Abs($bytes[$o + 1] - $G) -gt $Tolerance) { continue }
            if ([math]::Abs($bytes[$o] - $B) -gt $Tolerance) { continue }
            $count++
            if ($col -lt $minX) { $minX = $col }
            if ($col -gt $maxX) { $maxX = $col }
            if ($row -lt $minY) { $minY = $row }
            if ($row -gt $maxY) { $maxY = $row }
        }
    }
    if ($count -eq 0) { return $null }
    return [pscustomobject]@{
        Count = $count
        X     = $minX
        Y     = $minY
        W     = $maxX - $minX + 1
        H     = $maxY - $minY + 1
    }
}

<#
  The most frequent colour in a region.

  A dimmed stroke has to be matched as a blend of its own colour with whatever is
  under it, and "whatever is under it" cannot be sampled from one guessed pixel:
  a rounded corner, a 1px rule or a neighbour's border can be exactly there. The
  mode of the region is the surface, because a 2px stroke is never the majority
  of the box it outlines.
#>
function Get-DominantColor {
    param(
        $Map,
        [Parameter(Mandatory)][int]$X, [Parameter(Mandatory)][int]$Y,
        [Parameter(Mandatory)][int]$W, [Parameter(Mandatory)][int]$H,
        [int]$Sample = 1
    )
    # The step and the bitmap's row stride must not be the same name: PowerShell
    # variables are case-insensitive, so "$Stride = $Map.Stride" silently replaced
    # the sampling step with a row stride of 6232 and made the histogram nonsense.
    $bytes = $Map.Bytes
    $rowStride = $Map.Stride
    $x0 = [math]::Max(0, $X)
    $y0 = [math]::Max(0, $Y)
    $x1 = [math]::Min($Map.Width - 1, $X + $W - 1)
    $y1 = [math]::Min($Map.Height - 1, $Y + $H - 1)
    if ($x1 -lt $x0 -or $y1 -lt $y0) { return $null }
    $hist = @{}
    for ($row = $y0; $row -le $y1; $row += $Sample) {
        $base = $row * $rowStride
        for ($col = $x0; $col -le $x1; $col += $Sample) {
            $o = $base + $col * 4
            $key = ([int]$bytes[$o + 2] -shl 16) -bor ([int]$bytes[$o + 1] -shl 8) -bor [int]$bytes[$o]
            if ($hist.ContainsKey($key)) { $hist[$key]++ } else { $hist[$key] = 1 }
        }
    }
    $best = $null
    $bestCount = -1
    foreach ($entry in $hist.GetEnumerator()) {
        if ($entry.Value -gt $bestCount) { $bestCount = $entry.Value; $best = $entry.Key }
    }
    if ($null -eq $best) { return $null }
    return [pscustomobject]@{
        R     = ($best -shr 16) -band 0xFF
        G     = ($best -shr 8) -band 0xFF
        B     = $best -band 0xFF
        Count = $bestCount
    }
}

<# Colour at a point, for diagnostics. #>
function Get-Pixel {
    param($Map, [int]$X, [int]$Y)
    $o = $Y * $Map.Stride + $X * 4
    if ($o -lt 0 -or ($o + 3) -ge $Map.Bytes.Length) { return $null }
    return [pscustomobject]@{ B = $Map.Bytes[$o]; G = $Map.Bytes[$o + 1]; R = $Map.Bytes[$o + 2] }
}

<#
  Recover the client-area origin inside a window-rect capture.

  Only needed for captures taken with `shot.ps1 -WholeWindow`; the default capture
  is already cropped to the client area and its sidecar records that.
#>
function Get-ClientOrigin {
    param($Map, [int]$RefX = -1, [int]$RefY = -1)
    if ($RefX -lt 0) { $RefX = [int]($Map.Width / 2) }
    if ($RefY -lt 0) { $RefY = [int]($Map.Height / 2) }
    $ref = Get-Pixel $Map $RefX $RefY
    $distance = {
        param($p)
        if ($null -eq $p) { return 999 }
        return [math]::Abs($p.R - $ref.R) + [math]::Abs($p.G - $ref.G) + [math]::Abs($p.B - $ref.B)
    }
    $left = 0
    for ($x = 0; $x -lt $Map.Width; $x++) {
        if ((& $distance (Get-Pixel $Map $x $RefY)) -le 6) { $left = $x; break }
    }
    $top = 0
    for ($y = 0; $y -lt $Map.Height; $y++) {
        if ((& $distance (Get-Pixel $Map $RefX $y)) -le 6) { $top = $y; break }
    }
    return @{ Left = $left; Top = $top }
}

function Crop-Scale {
    param(
        [Parameter(Mandatory)][string]$Source,
        [Parameter(Mandatory)][string]$Destination,
        [Parameter(Mandatory)][int]$X, [Parameter(Mandatory)][int]$Y,
        [Parameter(Mandatory)][int]$W, [Parameter(Mandatory)][int]$H,
        [double]$Scale = 1.0
    )
    $img = [System.Drawing.Image]::FromFile((Resolve-Path $Source))
    $bmp = New-Object System.Drawing.Bitmap ([int]($W * $Scale)), ([int]($H * $Scale))
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
    $dest = New-Object System.Drawing.Rectangle 0, 0, $bmp.Width, $bmp.Height
    $src = New-Object System.Drawing.Rectangle $X, $Y, $W, $H
    $g.DrawImage($img, $dest, $src, [System.Drawing.GraphicsUnit]::Pixel)
    $g.Dispose(); $img.Dispose()
    $dir = Split-Path -Parent $Destination
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $bmp.Save($Destination, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}
