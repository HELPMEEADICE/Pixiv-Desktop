# One entry point for every gate this repository claims to pass.
#
# The README listed five commands, one of which needs an environment variable
# that only makes sense in this MSYS2 checkout, and nothing ran them together.
# A gate that lives in prose drifts: the test count in the README said 232 while
# the tree ran 256. This script is the list, so the list cannot rot.
#
# Why the lint step has two modes: cargo clippy cannot start in this MSYS2 +
# MSVC environment (it re-executes an MSVC toolchain and dies with "linker
# link.exe not found" before reaching project code, reproducible on
# dependency-only crates). The documented equivalent is RUSTC_WORKSPACE_WRAPPER
# pointing at clippy-driver, which does emit real lints. On a normal MSVC
# toolchain - the CI runner - plain cargo clippy is correct, so the script picks
# whichever applies instead of hard-coding this machine.
#
# What makes the lint step honest: RUSTC_WORKSPACE_WRAPPER is NOT part of cargo
# fingerprints, so pointing it at clippy-driver on an already-built tree prints
# nothing and checks nothing - measured here: "Finished in 0.36s", zero lints. The
# step therefore uses its own target directory (target/lint), so it always has
# artifacts to produce and therefore always actually lints. A deliberate
# bool_comparison probe proved the wrapper reports real lints.
#
# What lets it fail: RUSTFLAGS='-D warnings', set inside the same try/finally that
# carries the wrapper and the target directory. Without it the step could only ever
# report, never reject: a lint printed a warning line and cargo still exited 0. The
# second half is the log: --message-format short emits "src/x.rs:12:5: warning: ...",
# so a filter of ^(warning|error) matched the summary line only and dropped every
# per-item lint. The step now keeps the whole log and shows the first 200 lines,
# which is bounded without being lossy.
#
# Why a path gate runs before anything is compiled: F8. A directory literally
# named %SystemDrive% sat in this repository root, holding seven files under
# %SystemDrive%\ProgramData\Microsoft\Windows\Caches\ - a path some tool wrote
# from an unexpanded variable, and nothing noticed for six rounds. .gitignore
# learned the rule only after a human read "?? %SystemDrive%/" in git status,
# which is one round too late: a path that looks like a variable and is not one
# has never been a legitimate artefact. The gate is narrow on purpose - one
# question, does any path in the worktree contain a '%'. It can decide that the
# class is absent right now. It cannot decide whether a path is tracked, who
# wrote it, or whether a '%' is legitimate somewhere unsuspected: '%' is legal
# in a Windows filename, so this is a hygiene rule for *this* tree, not a claim
# about anyone else's.
#
# Usage:
#   pwsh -File tools/gates.ps1                 # paths, fmt, test, lint, selftest, release build
#   pwsh -File tools/gates.ps1 -SkipBuild      # the fast loop
#   pwsh -File tools/gates.ps1 -Lint Clippy    # force the toolchain clippy
#   pwsh -File tools/gates.ps1 -Ui             # also run the six measured UI gates
#
# G17: this list was incomplete. It called four of the seven verifiers, so
# tools/verify-icons.ps1, tools/verify-layout.ps1 and tools/selftest-gates.ps1 - the
# last of which is the negative experiment the whole acceptance argument rests on -
# were invoked by nothing at all while README.md and UI-NOTES.md described all three
# as gates. The UI section now runs a11y, visual, focus, dpi, icons and layout; the
# selftest runs in the non-UI flow, because it needs no desktop.
#
# G19: the three cargo steps pass --locked, so a stale Cargo.lock fails here instead
# of passing locally and failing on a CI runner that resolves the graph.
#
# -Ui is opt-in because it needs an interactive desktop: it launches the app,
# reads the UI Automation tree, presses real Tab keys and captures the screen. On
# a CI runner without a session it cannot work, and a gate that fails for a reason
# unrelated to the change is worse than a gate that is not run there.
#
# What the two later UI gates can and cannot decide, in the same terms as the
# others (docs/UI-NOTES.md 12h):
#   * tools/verify-focus.ps1 - measures that a Tab stop is *visible*. It presses
#     real Tab keys and, for every stop on three pages, reads the stop's UI
#     Automation rectangle and counts pixels of the colour ui/theme.slint declares
#     for focus inside it. It can decide that a reached stop draws the 2px
#     Theme.focus-ring that components.slint declares - the thing UI-NOTES 12a
#     fixed and nothing guarded. Measured limit, not a design choice: the ten
#     stops it walks are the command bar and the rail on every page, so it proves
#     the chrome and does not yet reach a card. It cannot decide that a ring reads
#     well or that the Tab *order* is sensible.
#   * tools/verify-dpi.ps1 - the scale ladder. It can decide that the app starts
#     at 100/125/150/200 %, that the shell invariant holds at each, and that the
#     cover decode target never falls below the physical card size at that scale.
#     It cannot judge rendering quality: the pixels above 150 % still land on the
#     same 2560 physical pixels of this display.
#
# Exit code 77 from a gate means "this host cannot run this gate" - no desktop, no
# window, UI Automation unavailable - and is reported as SKIP, neither PASS nor
# FAIL, so the summary stays honest in both directions.
param(
    [ValidateSet("Auto", "Driver", "Clippy", "Off")]
    [string]$Lint = "Auto",
    [switch]$SkipBuild,
    [switch]$SkipTest,
    [switch]$Ui,
    [switch]$Quiet
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

$driver = "C:\msys64\ucrt64\bin\clippy-driver.exe"
$results = @()
$failed = $false

function Invoke-Gate {
    param([string]$Name, [scriptblock]$Body)
    if (-not $Quiet) { Write-Host ("== {0}" -f $Name) -ForegroundColor Cyan }
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    & $Body
    $code = if ($null -ne $LASTEXITCODE) { $LASTEXITCODE } else { 0 }
    $watch.Stop()
    # 77 is the documented "cannot run here" code (see the header). It is a
    # third verdict rather than a pass so that a host which silently stops
    # running a gate cannot look like a host that passed it.
    $verdict = if ($code -eq 0) { "PASS" } elseif ($code -eq 77) { "SKIP" } else { "FAIL" }
    $script:results += [pscustomobject]@{
        Gate    = $Name
        Seconds = [math]::Round($watch.Elapsed.TotalSeconds, 1)
        Verdict = $verdict
    }
    if ($verdict -eq "FAIL") { $script:failed = $true }
    return $code
}

<#
  Walk the worktree and report every path whose name contains a literal percent sign.

  The walk prunes two trees and says why: "target" is build output this repository
  generates itself (a dependency fixture may legitimately carry a percent sign), and
  ".git" is not part of the worktree a commit would add.

  A match stops the descent: the offender reported is then the top of the tree that
  was written wrong, not the forty files inside it.
#>
function Find-PercentPaths {
    param([string]$Root, [string[]]$Prune = @("target", ".git"))
    $hits = New-Object System.Collections.Generic.List[string]
    $dirs = 0
    $files = 0
    $queue = New-Object System.Collections.Generic.Queue[string]
    $queue.Enqueue($Root)
    while ($queue.Count -gt 0) {
        $dir = $queue.Dequeue()
        try {
            $entries = [System.IO.Directory]::EnumerateFileSystemEntries($dir)
        }
        catch {
            continue   # an unreadable directory cannot hide a path this tree owns
        }
        foreach ($entry in $entries) {
            $name = [System.IO.Path]::GetFileName($entry)
            if ($name.Contains('%')) { $hits.Add($entry); continue }
            if ([System.IO.Directory]::Exists($entry)) {
                if ($Prune -contains $name) { continue }
                $dirs++
                $queue.Enqueue($entry)
            }
            else {
                $files++
            }
        }
    }
    return [pscustomobject]@{ Hits = $hits; Dirs = $dirs; Files = $files }
}

# ---- 0. worktree hygiene: no path was written from an unexpanded variable.
# First, because it is one directory walk and this is the kind of mistake that
# should stop a run before a compiler is started at all. ----
Invoke-Gate "worktree paths contain no literal percent sign (F8)" {
    $scan = Find-PercentPaths -Root (Get-Location).Path
    if ($scan.Hits.Count -eq 0) {
        Write-Host ("  {0} directories, {1} files: no percent sign in any path" -f $scan.Dirs, $scan.Files)
        $global:LASTEXITCODE = 0
    }
    else {
        $rootPrefix = (Get-Location).Path + [System.IO.Path]::DirectorySeparatorChar
        $scan.Hits | Select-Object -First 20 | ForEach-Object {
            Write-Host ("  FAIL {0}" -f $_.Replace($rootPrefix, "")) -ForegroundColor Red
        }
        Write-Host ("  {0} path(s) read as an unexpanded variable; rename or remove them" -f $scan.Hits.Count) -ForegroundColor Red
        $global:LASTEXITCODE = 1
    }
} | Out-Null

# ---- 1. formatting: cheap, and the only gate that is fixed mechanically ----
Invoke-Gate "cargo fmt --check" { cargo fmt --all -- --check 2>&1 | Out-Host } | Out-Null

# ---- 2. the test suite. It never touches the network: the live suite in
# crates/pixiv-api/tests/live.rs is inert unless PIXIV_LIVE_PHPSESSID is set. ----
if (-not $failed -and -not $SkipTest) {
    Invoke-Gate "cargo test --workspace" {
        # --locked (G19): a stale Cargo.lock must fail here as it would in CI, not
        # pass locally because cargo silently re-resolved the graph.
        cargo test --workspace --locked 2>&1 |
            Select-String -Pattern "test result|^error|panicked" |
            ForEach-Object { $_.Line } | Out-Host
    } | Out-Null
}

# ---- 3. lint ----
if (-not $failed -and $Lint -ne "Off") {
    $mode = $Lint
    if ($mode -eq "Auto") {
        $mode = if (Test-Path $driver) { "Driver" } else { "Clippy" }
    }
    if ($mode -eq "Driver") {
        $previous = $env:RUSTC_WORKSPACE_WRAPPER
        $previousTarget = $env:CARGO_TARGET_DIR
        $previousFlags = $env:RUSTFLAGS
        $env:RUSTC_WORKSPACE_WRAPPER = $driver
        # A separate target dir is what makes this step lint instead of
        # re-print "Finished" over cached artifacts.
        $env:CARGO_TARGET_DIR = "target\lint"
        # Promoting warnings to errors is what lets this step fail. RUSTFLAGS is
        # restored with the other two: a -D warnings left behind in this shell would
        # turn the release build below - and any later build - into a denial on any
        # warning, so the restore is in the same finally, not after it.
        $env:RUSTFLAGS = "-D warnings"
        try {
            Invoke-Gate "cargo check (clippy-driver)" {
                # No pattern filter: see the header. The log is bounded by line count
                # instead, and the exit code is put back by hand because the last
                # statement in this block is the one the gate measures.
                $log = @(cargo check --workspace --all-targets --locked --message-format short 2>&1 | ForEach-Object { "$_" })
                $code = if ($null -ne $LASTEXITCODE) { $LASTEXITCODE } else { 0 }
                $log | Select-Object -First 200 | Out-Host
                if ($log.Count -gt 200) { Write-Host ("  ... {0} more line(s) suppressed" -f ($log.Count - 200)) }
                $linted = @($log | Where-Object { $_ -match ": (warning|error)" }).Count
                Write-Host ("  {0} log line(s), {1} per-item warning/error line(s)" -f $log.Count, $linted)
                $global:LASTEXITCODE = $code
            } | Out-Null
        }
        finally {
            $env:RUSTC_WORKSPACE_WRAPPER = $previous
            $env:CARGO_TARGET_DIR = $previousTarget
            $env:RUSTFLAGS = $previousFlags
        }
    }
    else {
        Invoke-Gate "cargo clippy -D warnings" {
            # Same rule as the Driver branch: -D warnings on the command line, and the
            # whole log bounded by line count rather than by a filter that only ever
            # matched the summary.
            $log = @(cargo clippy --workspace --all-targets --locked --message-format short -- -D warnings 2>&1 | ForEach-Object { "$_" })
            $code = if ($null -ne $LASTEXITCODE) { $LASTEXITCODE } else { 0 }
            $log | Select-Object -First 200 | Out-Host
            if ($log.Count -gt 200) { Write-Host ("  ... {0} more line(s) suppressed" -f ($log.Count - 200)) }
            $global:LASTEXITCODE = $code
        } | Out-Null
    }
}

# ---- 4. the gates' own negative experiments. No desktop needed: selftest-gates diffs
# tracked baselines with -SkipCapture and reads this file statically. Here rather than
# with the UI gates because it must run on a host with no session - it is the proof
# that the other gates can fail, and a proof that only runs where they run is no proof
# (G17: it was invoked by nothing while two documents described it as a gate). ----
if (-not $failed) {
    Invoke-Gate "tools/selftest-gates.ps1" {
        pwsh -File tools/selftest-gates.ps1 2>&1 | Select-Object -Last 30 | Out-Host
    } | Out-Null
}

# ---- 5. the measured UI gates. Opt-in: see the header. ----
if (-not $failed -and $Ui) {
    # The two harnesses drive target/debug, which the release gate above does not
    # produce, so build it here rather than assuming it is current.
    Invoke-Gate "cargo build (debug, for the capture harness)" {
        cargo build 2>&1 | Select-Object -Last 2 | Out-Host
    } | Out-Null
    if (-not $failed) {
        Invoke-Gate "tools/verify-a11y.ps1" {
            pwsh -File tools/verify-a11y.ps1 2>&1 | Out-Host
        } | Out-Null
    }
    if (-not $failed) {
        Invoke-Gate "tools/verify-visual.ps1" {
            pwsh -File tools/verify-visual.ps1 2>&1 | Select-Object -Last 30 | Out-Host
        } | Out-Null
    }
    if (-not $failed) {
        Invoke-Gate "tools/verify-focus.ps1" {
            pwsh -File tools/verify-focus.ps1 2>&1 | Select-Object -Last 40 | Out-Host
        } | Out-Null
    }
    if (-not $failed) {
        Invoke-Gate "tools/verify-dpi.ps1" {
            pwsh -File tools/verify-dpi.ps1 2>&1 | Select-Object -Last 40 | Out-Host
        } | Out-Null
    }
    # G17: verify-icons and verify-layout were described as gates by README.md and
    # UI-NOTES.md and invoked by nothing, so the one measurement that reads glyph ink
    # and the 24-check layout matrix had not run since the round that wrote them.
    if (-not $failed) {
        Invoke-Gate "tools/verify-icons.ps1" {
            pwsh -File tools/verify-icons.ps1 2>&1 | Select-Object -Last 25 | Out-Host
        } | Out-Null
    }
    if (-not $failed) {
        Invoke-Gate "tools/verify-layout.ps1" {
            pwsh -File tools/verify-layout.ps1 2>&1 | Select-Object -Last 35 | Out-Host
        } | Out-Null
    }
}

# ---- 6. the shipped artefact. Last, because it is by far the slowest. ----
if (-not $failed -and -not $SkipBuild) {
    Invoke-Gate "cargo build --release" {
        cargo build --release 2>&1 | Select-Object -Last 3 | Out-Host
    } | Out-Null
}

Write-Host ""
$results | Format-Table -AutoSize | Out-String -Width 120 | Write-Host
$total = ($results | Measure-Object -Property Seconds -Sum).Sum
# A SKIP is not a failure and must not read as a pass either: 77 says this host
# cannot run the gate, so both counts are printed.
$bad = @($results | Where-Object { $_.Verdict -eq "FAIL" })
$skipped = @($results | Where-Object { $_.Verdict -eq "SKIP" })
$colour = if ($bad.Count) { "Red" } else { "Green" }
Write-Host ("{0} gates, {1} failed, {2} skipped (exit 77 = cannot run on this host), {3:N1}s total" -f $results.Count, $bad.Count, $skipped.Count, $total) -ForegroundColor $colour
if ($bad.Count) { exit 1 }
