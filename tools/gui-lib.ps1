# Shared GUI driver for the Pixiv Desktop end-to-end checks.
#
# Slint exposes its scene through UI Automation, so every step here is a real
# click or keystroke against the actual window - no callback is invoked directly.
# The session cookie is only ever read from .local-secrets, which is gitignored.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
if (-not ('Gui' -as [type])) {
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Gui {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, IntPtr e);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("gdi32.dll")] public static extern bool BitBlt(IntPtr d, int x, int y, int w, int h, IntPtr s, int sx, int sy, uint rop);
}
'@
}
[void][Gui]::SetProcessDPIAware()

$script:Root = 'C:\oss\Project\Rust\Pixiv_Desktop'
$script:ShotDir = Join-Path $script:Root 'docs\screenshots\live'

function Get-AppWindow {
  $p = Get-Process pixiv-desktop -ErrorAction SilentlyContinue | Select-Object -First 1
  if (-not $p) { return $null }
  return $p.MainWindowHandle
}

function Get-UiRoot {
  $h = Get-AppWindow
  if ($h -eq 0 -or $null -eq $h) { throw 'pixiv-desktop window not found' }
  [void][Gui]::SetForegroundWindow($h)
  return [System.Windows.Automation.AutomationElement]::FromHandle($h)
}

function Get-Nodes {
  $root = Get-UiRoot
  return $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
}

function Get-Names {
  $out = @()
  foreach ($n in Get-Nodes) { if ($n.Current.Name) { $out += $n.Current.Name } }
  return $out
}

function Find-Node {
  param([string]$Name, [string]$ControlType = '')
  foreach ($n in Get-Nodes) {
    if ($n.Current.Name -eq $Name -and ($ControlType -eq '' -or $n.Current.ControlType.ProgrammaticName -eq $ControlType)) { return $n }
  }
  return $null
}

function Click-Node {
  param($Node)
  $r = $Node.Current.BoundingRectangle
  if ($r.Width -le 0 -or $r.Height -le 0) { throw 'node has no bounds' }
  $x = [int]($r.X + $r.Width / 2)
  $y = [int]($r.Y + $r.Height / 2)
  [void][Gui]::SetCursorPos($x, $y)
  Start-Sleep -Milliseconds 220
  [void][Gui]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero)
  [void][Gui]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 350
  return @($x, $y)
}

function Click-Name {
  param([string]$Name)
  $n = Find-Node -Name $Name
  if ($null -eq $n) { throw "no node named '$Name'" }
  return Click-Node $n
}

function Set-WindowSize {
  param([int]$W = 1400, [int]$H = 940)
  $h = Get-AppWindow
  [void][Gui]::ShowWindow($h, 9)
  [void][Gui]::SetWindowPos($h, [IntPtr]::Zero, 16, 16, $W, $H, 0x0040)
  [void][Gui]::SetForegroundWindow($h)
  Start-Sleep -Milliseconds 700
}

function Save-Shot {
  param([string]$Name)
  New-Item -ItemType Directory -Force -Path $script:ShotDir | Out-Null
  $h = Get-AppWindow
  $rect = New-Object Gui+RECT
  [void][Gui]::GetWindowRect($h, [ref]$rect)
  $w = $rect.Right - $rect.Left; $ht = $rect.Bottom - $rect.Top
  $bmp = New-Object System.Drawing.Bitmap($w, $ht)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $src = [Gui]::GetDC([IntPtr]::Zero)
  $dst = $g.GetHdc()
  [void][Gui]::BitBlt($dst, 0, 0, $w, $ht, $src, $rect.Left, $rect.Top, 0x00CC0020)
  $g.ReleaseHdc($dst); [void][Gui]::ReleaseDC([IntPtr]::Zero, $src)
  $path = Join-Path $script:ShotDir "$Name.png"
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
  return $path
}

function Wait-ForName {
  param([string]$Pattern, [int]$TimeoutSec = 30)
  $deadline = (Get-Date).AddSeconds($TimeoutSec)
  while ((Get-Date) -lt $deadline) {
    foreach ($n in Get-Names) { if ($n -match $Pattern) { return $n } }
    Start-Sleep -Milliseconds 500
  }
  return $null
}