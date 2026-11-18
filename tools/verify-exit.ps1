# Process-residue gate: closing the window must end the process.
#
# The defect this exists for: the app could leave a live process behind after its
# window was closed. The root cause was not one stuck call - it was that nothing
# owned the teardown. `thread_local APP` held an `Rc<RefCell<App>>`, `App` owns the
# tokio runtime, every `slint::Timer` was leaked with `std::mem::forget` (and its
# closure holds another `Rc`), and no background task had a cancellation path. The
# ordering was left to process teardown, so whether the process ended cleanly
# depended on how much work was in flight.
#
# ---
#
# A note on the harness, because the first version of this gate was wrong and the
# wrongness was instructive. It closed the window with `Process.MainWindowHandle`,
# and the three chromium-and-winit-era caveats all applied at once:
#
#   * the handle it reports is *the* main window only if the app has one; a winit
#     app also owns a hidden 'Winit Thread Event Target' window, and .NET reported
#     `hwnd=0` for the process at exactly the moment the app was busy;
#   * it is cached per `Process` object, so a stale object keeps reporting a handle
#     that no longer exists;
#   * `CloseMainWindow()` posts WM_CLOSE to that handle and returns, so a run whose
#     handle was the event-target window closed nothing and reported a 'residue'
#     that was really an app which had never been asked to close.
#
# So the window is found by enumerating the process's top-level windows and taking
# the visible one whose class is not winit's event target - the window a user clicks
# the X on. That also means the gate fails loudly if no such window exists, instead
# of silently passing on a process it never addressed.
#
# Usage:
#   pwsh -File tools/verify-exit.ps1
#   pwsh -File tools/verify-exit.ps1 -Exe target\release\pixiv-desktop.exe
param(
    [string]$Exe = "target\debug\pixiv-desktop.exe",
    [int]$ExitTimeoutSec = 12
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exePath = if ([System.IO.Path]::IsPathRooted($Exe)) { $Exe } else { Join-Path $root $Exe }
if (-not (Test-Path $exePath)) { throw "no such executable: $exePath" }

if (-not ('ExitGate.Win' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
namespace ExitGate {
  public class Win {
    public delegate bool EnumProc(IntPtr h, IntPtr p);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);

    public static List<IntPtr> TopWindows(uint pid) {
      var list = new List<IntPtr>();
      EnumWindows((h, p) => { uint q; GetWindowThreadProcessId(h, out q); if (q == pid) list.Add(h); return true; }, IntPtr.Zero);
      return list;
    }
    public static string ClassOf(IntPtr h) {
      var c = new StringBuilder(256); GetClassName(h, c, 256); return c.ToString();
    }
    public static string TitleOf(IntPtr h) {
      var t = new StringBuilder(256); GetWindowText(h, t, 256); return t.ToString();
    }
    // The window a user closes: visible, titled, and not one of winit's plumbing
    // windows ("Winit Thread Event Target") or the console host's pseudo window.
    public static IntPtr AppWindow(uint pid) {
      foreach (var h in TopWindows(pid)) {
        if (!IsWindowVisible(h)) continue;
        var cls = ClassOf(h);
        var title = TitleOf(h);
        if (cls == "Winit Thread Event Target") continue;
        if (cls == "PseudoConsoleWindow") continue;
        if (cls == "IME" || cls == "MSCTFIME UI") continue;
        if (title.Length == 0) continue;
        return h;
      }
      return IntPtr.Zero;
    }
  }
}
'@
}

$failures = @()
function Fail([string]$m) { $script:failures += $m; Write-Host ("  FAIL  " + $m) -ForegroundColor Red }
function Pass([string]$m) { Write-Host ("  ok    " + $m) -ForegroundColor Green }
function Info([string]$m) { Write-Host ("  ..    " + $m) }

function Reset-App {
    Get-Process pixiv-desktop -ErrorAction SilentlyContinue | ForEach-Object {
        try { $_.Kill(); $_.WaitForExit(3000) | Out-Null } catch { }
    }
    Start-Sleep -Milliseconds 600
}

<#
  Close the real window and classify what is left.
  'gone' and 'unreapable' pass; 'alive' is a residue; 'nowindow' is a harness fault
  that must not be counted as a pass.
#>
function Close-And-Classify($proc, [string]$label) {
    $hwnd = [ExitGate.Win]::AppWindow([uint32]$proc.Id)
    if ($hwnd -eq [IntPtr]::Zero) {
        Fail "$label : no visible application window to close"
        return 'nowindow'
    }
    [void][ExitGate.Win]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)  # WM_CLOSE
    $deadline = (Get-Date).AddSeconds($ExitTimeoutSec)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 200
        $live = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
        if ($null -eq $live) { return 'gone' }
        if ($live.HasExited) { return 'gone' }
        if (-not $live.Threads -or $live.Threads.Count -le 0) { return 'gone' }
    }
    $live = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
    if ($null -eq $live) { return 'gone' }
    if (-not $live.Threads -or $live.Threads.Count -le 0) {
        Info "$label : pid $($proc.Id) already terminated (0 threads)"
        return 'unreapable'
    }
    $ws = [math]::Round($live.WorkingSet64 / 1MB, 1)
    Fail "$label : process $($proc.Id) survived window close ($($live.Threads.Count) threads, $ws MB)"
    return 'alive'
}

function Start-App([hashtable]$Env, [string]$label) {
    foreach ($k in $Env.Keys) { Set-Item -Path ("Env:" + $k) -Value $Env[$k] }
    $p = Start-Process -FilePath $exePath -PassThru
    $deadline = (Get-Date).AddSeconds(40)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 300
        $p.Refresh()
        if ($p.HasExited) { throw "$label : the app exited during startup" }
        if ([ExitGate.Win]::AppWindow([uint32]$p.Id) -ne [IntPtr]::Zero) { return $p }
    }
    throw "$label : no application window after 40s"
}

function Clear-Env {
    'PIXIV_DESKTOP_DEMO','PIXIV_DESKTOP_SIZE','PIXIV_DESKTOP_START_PAGE',
    'PIXIV_DESKTOP_START_ILLUST','PIXIV_DESKTOP_START_USER','PIXIV_DESKTOP_START_NOVEL',
    'PIXIV_DESKTOP_START_READER','PIXIV_DESKTOP_FIXTURE','PIXIV_DESKTOP_TRACE',
    'PIXIV_DESKTOP_EXIT_TRACE' | ForEach-Object { Remove-Item -Path ("Env:" + $_) -ErrorAction SilentlyContinue }
}

$env:RUST_LOG = 'warn'
$scenarios = 0
try {
    # 1: a plain close from the home page.
    Reset-App; Clear-Env
    $p = Start-App @{ 'PIXIV_DESKTOP_DEMO' = '1'; 'PIXIV_DESKTOP_SIZE' = '1200x820' } 'plain close'
    Start-Sleep -Seconds 2
    $r = Close-And-Classify $p 'plain close'
    $scenarios++
    if ($r -eq 'gone' -or $r -eq 'unreapable') { Pass 'closing the home page ends the process' }

    # 2: close while the artwork viewer is decoding.
    Reset-App; Clear-Env
    $p = Start-App @{ 'PIXIV_DESKTOP_DEMO'='1'; 'PIXIV_DESKTOP_SIZE'='1200x820';
                      'PIXIV_DESKTOP_START_ILLUST'='700000003' } 'decode in flight'
    $r = Close-And-Classify $p 'decode in flight'
    $scenarios++
    if ($r -eq 'gone' -or $r -eq 'unreapable') { Pass 'closing during an image decode ends the process' }

    # 3: close immediately after the window appears (the startup-timer race).
    Reset-App; Clear-Env
    $p = Start-App @{ 'PIXIV_DESKTOP_DEMO'='1'; 'PIXIV_DESKTOP_SIZE'='1200x820' } 'rapid close'
    $r = Close-And-Classify $p 'rapid close'
    $scenarios++
    if ($r -eq 'gone' -or $r -eq 'unreapable') { Pass 'closing immediately after startup ends the process' }

    # 4: three start/close cycles; nothing may accumulate.
    Reset-App; Clear-Env
    $before = $failures.Count
    for ($i = 1; $i -le 3; $i++) {
        $p = Start-App @{ 'PIXIV_DESKTOP_DEMO'='1'; 'PIXIV_DESKTOP_SIZE'='1100x760' } "cycle $i"
        $r = Close-And-Classify $p "cycle $i"
        # The exit is asynchronous past the point where the pid stops resolving: the
        # process object disappears first and the handle table drains a moment later.
        # The window below is the one this gate cares about, so wait for it rather
        # than sampling once and calling a still-draining process a residue.
        $settle = (Get-Date).AddSeconds(8)
        while ((Get-Date) -lt $settle) {
            $left = @(Get-Process pixiv-desktop -ErrorAction SilentlyContinue |
                Where-Object { $_.Threads -and $_.Threads.Count -gt 0 })
            if ($left.Count -eq 0) { break }
            Start-Sleep -Milliseconds 250
        }
        $left = @(Get-Process pixiv-desktop -ErrorAction SilentlyContinue |
            Where-Object { $_.Threads -and $_.Threads.Count -gt 0 })
        if ($left.Count -gt 0) {
            $detail = ($left | ForEach-Object {
                "pid=$($_.Id) threads=$($_.Threads.Count) start=$($_.StartTime.ToString('HH:mm:ss'))"
            }) -join '; '
            Fail "cycle $i : $($left.Count) process(es) left behind ($detail)"
        }
    }
    $scenarios++
    if ($failures.Count -eq $before) { Pass 'three start/close cycles leave nothing behind' }

    # 5: a repeated close request must not hang or resurrect anything.
    Reset-App; Clear-Env
    $p = Start-App @{ 'PIXIV_DESKTOP_DEMO'='1'; 'PIXIV_DESKTOP_SIZE'='1000x700' } 'double close'
    $hwnd = [ExitGate.Win]::AppWindow([uint32]$p.Id)
    if ($hwnd -ne [IntPtr]::Zero) {
        [void][ExitGate.Win]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 150
        if ([ExitGate.Win]::AppWindow([uint32]$p.Id) -ne [IntPtr]::Zero) {
            [void][ExitGate.Win]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
        }
    }
    $deadline = (Get-Date).AddSeconds($ExitTimeoutSec)
    $gone = $false
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 200
        if ($null -eq (Get-Process -Id $p.Id -ErrorAction SilentlyContinue)) { $gone = $true; break }
    }
    $scenarios++
    if ($gone) { Pass 'a repeated close request still ends the process' }
    else { Fail 'double close : the process survived two close requests' }
}
catch {
    Fail ("exception: " + $_.Exception.Message)
}
finally {
    Reset-App
    Clear-Env
}

Write-Host ""
if ($failures.Count -eq 0) {
    Write-Host ("exit: PASS (" + $scenarios + " scenarios)") -ForegroundColor Green
    exit 0
}
Write-Host ("exit: FAIL (" + $failures.Count + ")") -ForegroundColor Red
foreach ($f in $failures) { Write-Host ("  - " + $f) }
exit 1