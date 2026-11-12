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
# Usage:
#   pwsh -File tools/gates.ps1                 # fmt, test, lint, release build
#   pwsh -File tools/gates.ps1 -SkipBuild      # the fast loop
#   pwsh -File tools/gates.ps1 -Lint Clippy    # force the toolchain clippy
param(
    [ValidateSet("Auto", "Driver", "Clippy", "Off")]
    [string]$Lint = "Auto",
    [switch]$SkipBuild,
    [switch]$SkipTest,
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
    $script:results += [pscustomobject]@{
        Gate    = $Name
        Seconds = [math]::Round($watch.Elapsed.TotalSeconds, 1)
        Verdict = if ($code -eq 0) { "PASS" } else { "FAIL" }
    }
    if ($code -ne 0) { $script:failed = $true }
    return $code
}

# ---- 1. formatting: cheap, and the only gate that is fixed mechanically ----
Invoke-Gate "cargo fmt --check" { cargo fmt --all -- --check 2>&1 | Out-Host } | Out-Null

# ---- 2. the test suite. It never touches the network: the live suite in
# crates/pixiv-api/tests/live.rs is inert unless PIXIV_LIVE_PHPSESSID is set. ----
if (-not $failed -and -not $SkipTest) {
    Invoke-Gate "cargo test --workspace" {
        cargo test --workspace 2>&1 |
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
        $env:RUSTC_WORKSPACE_WRAPPER = $driver
        # A separate target dir is what makes this step lint instead of
        # re-print "Finished" over cached artifacts.
        $env:CARGO_TARGET_DIR = "target\lint"
        try {
            Invoke-Gate "cargo check (clippy-driver)" {
                cargo check --workspace --all-targets --message-format short 2>&1 |
                    Select-String -Pattern "^(warning|error)" |
                    ForEach-Object { $_.Line } | Out-Host
            } | Out-Null
        } finally {
            $env:RUSTC_WORKSPACE_WRAPPER = $previous
            $env:CARGO_TARGET_DIR = $previousTarget
        }
    }
    else {
        Invoke-Gate "cargo clippy -D warnings" {
            cargo clippy --workspace --all-targets --message-format short -- -D warnings 2>&1 |
                Select-String -Pattern "^(warning|error)" |
                ForEach-Object { $_.Line } | Out-Host
        } | Out-Null
    }
}

# ---- 4. the shipped artefact. Last, because it is by far the slowest. ----
if (-not $failed -and -not $SkipBuild) {
    Invoke-Gate "cargo build --release" {
        cargo build --release 2>&1 | Select-Object -Last 3 | Out-Host
    } | Out-Null
}

Write-Host ""
$results | Format-Table -AutoSize | Out-String -Width 120 | Write-Host
$total = ($results | Measure-Object -Property Seconds -Sum).Sum
$bad = @($results | Where-Object { $_.Verdict -ne "PASS" })
$colour = if ($bad.Count) { "Red" } else { "Green" }
Write-Host ("{0} gates, {1} failed, {2:N1}s total" -f $results.Count, $bad.Count, $total) -ForegroundColor $colour
if ($bad.Count) { exit 1 }
