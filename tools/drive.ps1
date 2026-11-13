# Synthetic pointer and keyboard input, for the checks that need a real event.
#
# Why this file exists: docs/UI-NOTES.md §10 recorded that hover and keyboard
# interaction "cannot be synthesised on this host". That conclusion was wrong and
# had been wrong for rounds — tools/gui-lib.ps1 already drives the app with
# SetCursorPos + mouse_event, and the accessibility tree can be read while it
# happens. A pointer move, a click, a wheel notch and a keystroke are all
# available; what was missing was a place to put them.
#
# Dot-source it to get the functions, or call it with -Action for one step:
#   . tools/drive.ps1
#   pwsh -File tools/drive.ps1 -Action click -X 400 -Y 300
#   pwsh -File tools/drive.ps1 -Action tab -Count 5
#   pwsh -File tools/drive.ps1 -Action focus      # who has the keyboard now
param(
    [string]$Action = "",
    [int]$X = -1,
    [int]$Y = -1,
    [int]$Delta = -120,
    [string]$Name = "",
    [int]$Count = 1,
    [switch]$Double
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms

if (-not ('Drive32' -as [type])) {
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Drive32 {
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, int d, IntPtr e);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
}
'@
}
[void][Drive32]::SetProcessDPIAware()

function Move-Pointer {
    param([int]$X, [int]$Y)
    [void][Drive32]::SetCursorPos($X, $Y)
    Start-Sleep -Milliseconds 130
}

function Invoke-ClickAt {
    param([int]$X, [int]$Y, [switch]$Double)
    Move-Pointer -X $X -Y $Y
    $times = if ($Double) { 2 } else { 1 }
    for ($i = 0; $i -lt $times; $i++) {
        [void][Drive32]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero)   # left down
        [void][Drive32]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero)   # left up
        Start-Sleep -Milliseconds 90
    }
    Start-Sleep -Milliseconds 260
}

function Invoke-WheelAt {
    param([int]$X, [int]$Y, [int]$Delta)
    Move-Pointer -X $X -Y $Y
    [void][Drive32]::mouse_event(0x0800, 0, 0, $Delta, [IntPtr]::Zero)  # WHEEL
    Start-Sleep -Milliseconds 220
}

function Send-Keys {
    param([string]$Text)
    [System.Windows.Forms.SendKeys]::SendWait($Text)
    Start-Sleep -Milliseconds 160
}

function Send-TabKey {
    param([int]$Count = 1)
    if ($Count -lt 1) { $Count = 1 }
    Send-Keys ('{TAB}' * $Count)
}

function Find-AppWindow {
    param([string]$ProcessName = 'pixiv-desktop')
    $p = Get-Process $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $p) { return [IntPtr]::Zero }
    return $p.MainWindowHandle
}

function Focus-AppWindow {
    param([string]$ProcessName = 'pixiv-desktop')
    $h = Find-AppWindow -ProcessName $ProcessName
    if ($h -eq [IntPtr]::Zero) { throw "$ProcessName window not found" }
    [void][Drive32]::SetForegroundWindow($h)
    Start-Sleep -Milliseconds 250
    return $h
}

function Get-FocusedInfo {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes -ErrorAction SilentlyContinue
    $f = [System.Windows.Automation.AutomationElement]::FocusedElement
    if ($null -eq $f) { return [pscustomobject]@{ Name = ''; Type = '' } }
    return [pscustomobject]@{
        Name = $f.Current.Name
        Type = ($f.Current.ControlType.ProgrammaticName -replace 'ControlType\.', '')
    }
}

switch ($Action) {
    'hover' { Move-Pointer -X $X -Y $Y; "hovered $X,$Y" }
    'click' { Invoke-ClickAt -X $X -Y $Y -Double:$Double; "clicked $X,$Y" }
    'wheel' { Invoke-WheelAt -X $X -Y $Y -Delta $Delta; "wheeled $Delta at $X,$Y" }
    'key'   { Send-Keys $Name; "sent $Name" }
    'tab'   { Send-TabKey -Count $Count; "tabbed $Count" }
    'focus' { Get-FocusedInfo | ConvertTo-Json -Compress }
    default { }
}
