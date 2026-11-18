# One page set: the destinations the shell has, and how each one is opened.
#
# G32 (docs/OPTIMIZATION-PLAN-R11.md 6). Five files carried five page lists and they
# had already drifted. tools/verify-a11y.ps1:34 and tools/verify-layout.ps1:22 named
# the same eight destinations, tools/verify-focus.ps1:75 named three,
# tools/verify-visual.ps1:51 named fifteen capture states, and
# tools/a11y-budget.json keyed its floors by the same eight. Not one of them named the
# three novel pages that ui/main.slint has carried since round ten, so every gate that
# claimed to walk "every page" walked three destinations fewer than the shell has, and
# nothing in the tree could see it: a list that only agrees with itself always agrees.
#
# The authority is ui/main.slint's `Destination` enum (thirteen members). This file
# states it once. A consumer asks for the selection it owns - `Get-PageSet -For a11y` -
# instead of writing a list down, and `Compare-PageSet` re-reads all five consumers from
# their own files and names any pair that disagrees. tools/selftest-gates.ps1 case
# `page-set` is what runs that comparison, live and against a planted copy.
#
# Dot-sourced, like tools/drive.ps1 and tools/fixture.ps1: nothing here has a param
# block, nothing writes to the environment unless asked, and nothing launches the app.
#
# What this file can and cannot decide. It can decide that the five selections are the
# ones declared below, that every name they use is a destination the shell has or a
# capture state declared here, and that the budget is keyed by the accessibility set. It
# cannot decide that a page *renders*: that is the gates' job.

# $PSScriptRoot is the directory of the file being dot-sourced, so this works from
# every consumer without a Set-Location of its own.
$script:PageSetRoot = if ($PSScriptRoot) { $PSScriptRoot } else { Split-Path -Parent $MyInvocation.MyCommand.Path }

# ui/main.slint:192-210, in the order the enum declares them.
$script:PageSet = @(
    "home"
    "discovery"
    "ranking"
    "search"
    "bookmarks"
    "history"
    "detail"
    "downloads"
    "settings"
    "user-profile"
    "novels"
    "novel"
    "novel-reader"
)

# The nine capture states. They are not destinations: each one is a destination plus a
# forced state, reached through PIXIV_DESKTOP_START_ILLUST or PIXIV_DESKTOP_FIXTURE, and
# tools/verify-visual.ps1 is the only consumer that photographs them. They are declared
# here so that a name a consumer uses is either a destination or this list - a typo in a
# page list used to be indistinguishable from a page.
#
# Round twelve added the last three: each one photographs a feature that only exists in a
# state the demo catalogue cannot reach on its own (a masked work, an AI-flagged card,
# novel result rows), so the pixels are the evidence a test cannot give.
$script:PageState = @(
    "detail-error"
    "detail-ugoira"
    "palette"
    "selection"
    "suggestion"
    "downloads-queue"
    "detail-masked"
    "ranking-ai"
    "novels-results"
)

<#
  How each page is opened, as the environment the shell reads
  (src/app/lifecycle.rs:42-74):

    * PIXIV_DESKTOP_START_PAGE names the pages the shell can start on by name;
    * PIXIV_DESKTOP_START_ILLUST opens a work on the detail page;
    * PIXIV_DESKTOP_START_USER opens the artist page;
    * PIXIV_DESKTOP_START_NOVEL opens one novel, and PIXIV_DESKTOP_START_READER=1 goes
      straight into the reader.

  The ids are the demo catalogue's, not a live account's: illustration ids start at
  700_000_001, demo user ids are 900_000 + the artist's index
  (crates/pixiv-demo/src/catalog.rs:40, :253) and novel chapters occupy
  NOVEL_ID_BASE + 1 ..= + 8 with NOVEL_ID_BASE = 810_000_000
  (crates/pixiv-demo/src/catalog.rs:127). So a capture in forced demo mode needs no
  session and no network, which is what makes it repeatable.

  This knowledge used to live in three scripts and one capture table: verify-a11y
  hand-set START_PAGE and deleted START_ILLUST/START_USER, verify-focus special-cased
  "detail", and verify-visual kept a fixturePages table. One table now, and
  Set-PageLaunch applies exactly one page's recipe - clearing the keys the page does not
  use - so a deep link cannot leak from the page before it.
#>
$script:PageLaunch = @{
    "home"            = @{ StartPage = "home" }
    "discovery"       = @{ StartPage = "discovery" }
    "ranking"         = @{ StartPage = "ranking" }
    "search"          = @{ StartPage = "search" }
    "bookmarks"       = @{ StartPage = "bookmarks" }
    "history"         = @{ StartPage = "history" }
    "downloads"       = @{ StartPage = "downloads" }
    "settings"        = @{ StartPage = "settings" }
    "novels"          = @{ StartPage = "novels" }
    "detail"          = @{ StartPage = "home"; Illust = "700000003" }
    "detail-error"    = @{ StartPage = "home"; Illust = "999999999" }
    "detail-ugoira"   = @{ StartPage = "home"; Fixture = "ugoira" }
    "palette"         = @{ StartPage = "home"; Fixture = "palette" }
    "selection"       = @{ StartPage = "home"; Fixture = "selection" }
    "suggestion"      = @{ StartPage = "home"; Fixture = "suggestion" }
    "downloads-queue" = @{ StartPage = "downloads"; Fixture = "downloads" }
    "user-profile"    = @{ StartPage = "home"; User = "900000" }
    "novel"           = @{ StartPage = "home"; Novel = "810000001" }
    "novel-reader"    = @{ StartPage = "home"; Novel = "810000001"; Reader = $true }
    "detail-masked"   = @{ StartPage = "home"; Illust = "700000003"; Fixture = "sensitive" }
    "ranking-ai"      = @{ StartPage = "ranking"; Fixture = "ai" }
    "novels-results"  = @{ StartPage = "novels"; Fixture = "novels" }
}

# The focus gate samples three destinations. Every destination has a demo launch recipe.
$script:PageFocus = @("home", "detail", "settings")

function Get-PageSet {
    param(
        [ValidateSet("all", "a11y", "layout", "focus", "visual", "budget")]
        [string]$For = "all"
    )
    switch ($For) {
        "focus" { return @($script:PageSet | Where-Object { $script:PageFocus -contains $_ }) }
        "visual" { return @($script:PageSet + $script:PageState) }
        default { return @($script:PageSet) }
    }
}

<#
  One page's launch recipe, as a hashtable of environment values.

  Empty for a name that is neither a destination nor a capture state, so a caller can
  tell a typo from a page instead of launching the shell and measuring the home page.
#>
function Get-PageLaunch {
    param([string]$Page)
    if (-not $script:PageLaunch.ContainsKey($Page)) { return @{} }
    return $script:PageLaunch[$Page]
}

<#
  Apply one page's recipe to the current process, so a child capture inherits it.

  Every key the shell reads is written or removed on every call. That is the whole point:
  the old scripts each cleared the keys they remembered, so a run that captured "detail"
  and then "settings" left START_ILLUST set for settings. PIXIV_DESKTOP_FIXTURE is not
  touched here - Set-PageFixture owns it - so a caller that does not want a fixture state
  cannot get one by accident.
#>
function Set-PageLaunch {
    # -Clear sweeps every key without applying a recipe. tools/shot.ps1 uses it for a page
    # name the page set does not know, so even that call cannot inherit a deep link from
    # whatever ran before it (G59/E11).
    param([string]$Page, [switch]$Clear)
    $recipe = if ($Clear) { @{} } else { Get-PageLaunch -Page $Page }
    if ($recipe.ContainsKey("StartPage")) { $env:PIXIV_DESKTOP_START_PAGE = $recipe.StartPage }
    else { Remove-Item Env:\PIXIV_DESKTOP_START_PAGE -ErrorAction SilentlyContinue }
    foreach ($pair in @(
            @("Illust", "PIXIV_DESKTOP_START_ILLUST"),
            @("User", "PIXIV_DESKTOP_START_USER"),
            @("Novel", "PIXIV_DESKTOP_START_NOVEL"))) {
        if ($recipe.ContainsKey($pair[0])) { Set-Item -Path ("Env:\" + $pair[1]) -Value $recipe[$pair[0]] }
        else { Remove-Item ("Env:\" + $pair[1]) -ErrorAction SilentlyContinue }
    }
    if ($recipe.ContainsKey("Reader")) { Set-Item -Path "Env:\PIXIV_DESKTOP_START_READER" -Value "1" }
    else { Remove-Item Env:\PIXIV_DESKTOP_START_READER -ErrorAction SilentlyContinue }
}

<#
  Apply one page's fixture state, or clear it. Split from Set-PageLaunch because only the
  capture gate photographs states: the accessibility and layout gates measure the page as
  it opens, and an ambient PIXIV_DESKTOP_FIXTURE must not silently become part of that
  measurement.
#>
function Set-PageFixture {
    param([string]$Page)
    $recipe = Get-PageLaunch -Page $Page
    if ($recipe.ContainsKey("Fixture")) { $env:PIXIV_DESKTOP_FIXTURE = $recipe.Fixture }
    else { Remove-Item Env:\PIXIV_DESKTOP_FIXTURE -ErrorAction SilentlyContinue }
}

<#
  Which file owns which selection. The five sets G32 is about.
#>
function Get-PageConsumer {
    return [ordered]@{
        a11y   = "tools/verify-a11y.ps1"
        layout = "tools/verify-layout.ps1"
        focus  = "tools/verify-focus.ps1"
        visual = "tools/verify-visual.ps1"
        budget = "tools/a11y-budget.json"
    }
}

<#
  The page list one consumer file actually carries.

  Two shapes are read: the derivation (`Get-PageSet -For <name>`, which is what every
  consumer was changed to write) and a literal `$Pages = @("...")`. Nothing else counts as
  a list - a file that reads neither has no page list, and "no list" is a difference, not
  an empty selection.

  The literal case is narrowed twice, and both narrowings were found by the selftest
  rather than reasoned about. It ignores a parenthesised expression (no nested brackets)
  and it requires at least one quoted name, because `[string[]]$Pages = @()` in the param
  block matches the naive pattern first and reported "a list of 0 pages" - which is true
  of the param default and false of the file. Of the remaining candidates the *last* one
  wins: the param block is above the body, and a file that assigns $Pages twice has its
  answer in the later line.

  Returns Source = derived | literal | none, the line the answer came from, and Pages.
#>
function Get-PageListFromText {
    param([string]$Name, [string]$Text)
    $derived = [regex]::Match($Text, ("Get-PageSet\s+-For\s+" + [regex]::Escape($Name) + "\b"))
    if ($derived.Success) {
        $line = ($Text.Substring(0, $derived.Index) -split "\r?\n").Count
        return [pscustomobject]@{ Source = "derived"; Line = $line; Pages = @(Get-PageSet -For $Name) }
    }
    $literal = $null
    foreach ($candidate in [regex]::Matches($Text, '\$Pages\s*=\s*@\(([^()]*)\)')) {
        if ($candidate.Groups[1].Value -match '["'']') { $literal = $candidate }
    }
    if ($null -ne $literal) {
        $line = ($Text.Substring(0, $literal.Index) -split "\r?\n").Count
        $pages = @([regex]::Matches($literal.Groups[1].Value, '["'']([^"'']+)["'']') | ForEach-Object { $_.Groups[1].Value })
        return [pscustomobject]@{ Source = "literal"; Line = $line; Pages = $pages }
    }
    return [pscustomobject]@{ Source = "none"; Line = 0; Pages = @() }
}

<#
  G56: the page set agrees with itself.

  Two questions, and neither is answered by any single list agreeing with itself.

  1. No page the capture set photographs may sit in the unmeasured list. A page with a
     tracked baseline and no accessibility floor is exactly the inconsistency round
     twelve found: detail has been photographed since round seven, sat in the old
     PageUncaptured, and had no floor and no gap check. This is the half that fails if
     somebody re-merges the two exclusion lists.
  2. The three measurement selections are the same pages. A page measured for layout and
     not for accessibility is a page whose floors are missing for a reason nobody
     wrote down.

  The overrides exist for the negative experiment: tools/selftest-gates.ps1 hands the
  pre-E6 shape in (-Unmeasured @("detail")) and this function - the
  shipped one, not a copy - has to name detail.
#>
function Test-PageSetCompleteness {
    param(
        [string[]]$Unmeasured = $null,
        [string[]]$Visual     = $null,
        [string[]]$A11y       = $null,
        [string[]]$Layout     = $null
    )
    if ($null -eq $Unmeasured) { $Unmeasured = @() }
    if ($null -eq $Visual) { $Visual = @(Get-PageSet -For visual) }
    if ($null -eq $A11y) { $A11y = @(Get-PageSet -For a11y) }
    if ($null -eq $Layout) { $Layout = @(Get-PageSet -For layout) }
    $problems = New-Object System.Collections.Generic.List[string]
    foreach ($page in $Visual) {
        if ($Unmeasured -contains $page) {
            $problems.Add(("'{0}' is photographed by the visual set and excluded from every measurement set ({1}): a page with a baseline and no floor is the inconsistency G56 is about" -f $page, ($Unmeasured -join ", ")))
        }
    }
    foreach ($page in $A11y) {
        if ($Layout -notcontains $page) {
            $problems.Add(("'{0}' is measured for accessibility and not for layout: the two selections have to be one list" -f $page))
        }
    }
    foreach ($page in $Layout) {
        if ($A11y -notcontains $page) {
            $problems.Add(("'{0}' is measured for layout and not for accessibility: the two selections have to be one list" -f $page))
        }
    }
    return $problems.ToArray()
}

<#
  Compare all five sets and return one object per disagreement - nothing when they agree.

  Each object carries Pair ("page-set\a11y", "a11y\budget"), the file, the line and a
  message that names the pair and the page, so the operator reads which list drifted and
  where rather than a count. For a consumer whose file stopped reading this one the pair
  is page-set versus that consumer; for a page the budget does not key, it is a11y
  versus budget.

  -Text overrides a file's content, keyed by consumer name. That is how
  tools/selftest-gates.ps1 plants a defect - the parser and every comparison below are
  the shipped ones, and only the bytes are substituted.
#>
function Compare-PageSet {
    param(
        [string]$Root = (Split-Path -Parent $script:PageSetRoot),
        [hashtable]$Text = @{}
    )
    $consumers = Get-PageConsumer
    $known = @($script:PageSet) + @($script:PageState)
    $diffs = New-Object System.Collections.Generic.List[object]
    $budgetKeys = @()
    # What each verifier's file actually carries, so the budget is compared with the
    # accessibility list that is written down rather than with the one this file
    # prescribes. A file that drifted and a budget that agrees with the drift is still
    # one disagreement, but the pair that names it is a11y\budget.
    $effective = @{}

    foreach ($name in $consumers.Keys) {
        $file = $consumers[$name]
        $body = if ($Text.ContainsKey($name)) { [string]$Text[$name] } else { Get-Content (Join-Path $Root $file) -Raw }
        if ($null -eq $body) { $body = "" }

        if ($name -eq "budget") {
            $entry = $null
            try { $entry = $body | ConvertFrom-Json } catch { $entry = $null }
            if ($null -eq $entry -or -not $entry.pages) {
                $diffs.Add([pscustomobject]@{
                        Pair    = "a11y\budget"
                        File    = $file
                        Line    = 0
                        Message = ("a11y\budget: {0} holds no pages object, so no floor is keyed by the accessibility page set" -f $file)
                    })
                continue
            }
            $budgetKeys = @($entry.pages.PSObject.Properties.Name)
            continue
        }

        $list = Get-PageListFromText -Name $name -Text $body
        $effective[$name] = @($list.Pages)
        if ($list.Source -ne "derived") {
            $shown = if ($list.Source -eq "literal") {
                ("carries its own list of {0} page(s)" -f $list.Pages.Count)
            }
            else {
                "carries no page list at all"
            }
            $where = if ($list.Line -gt 0) { ":" + $list.Line } else { "" }
            $diffs.Add([pscustomobject]@{
                    Pair    = ("page-set\" + $name)
                    File    = $file
                    Line    = $list.Line
                    Message = ("page-set\{0}: {1}{2} {3} instead of Get-PageSet -For {0}, so it can disagree with the other four sets" -f $name, $file, $where, $shown)
                })
        }
        foreach ($page in $list.Pages) {
            if ($known -notcontains $page) {
                $diffs.Add([pscustomobject]@{
                        Pair    = ("{0}\page-set" -f $name)
                        File    = $file
                        Line    = $list.Line
                        Message = ("{0}\page-set: {1} names '{2}', which is neither a destination in ui/main.slint's enum nor a capture state in tools/page-set.ps1" -f $name, $file, $page)
                    })
            }
        }
    }

    # The budget is data, not a script: it agrees when its keys are the accessibility set,
    # page for page. A page with no entry has no floor (G4), so a missing key is a
    # difference even though the gate would also fail on it at run time - this one names
    # the file and the page before the app is launched.
    if ($budgetKeys.Count -gt 0) {
        # The accessibility list as verify-a11y.ps1 carries it. When that file reads the
        # page set this is the prescription; when it carries its own list, this is what
        # the budget has to be keyed by for the two to be about the same pages.
        $want = if ($effective.ContainsKey("a11y") -and @($effective["a11y"]).Count -gt 0) { @($effective["a11y"]) } else { @(Get-PageSet -For budget) }
        foreach ($page in $want) {
            if ($budgetKeys -notcontains $page) {
                $diffs.Add([pscustomobject]@{
                        Pair    = "a11y\budget"
                        File    = $consumers["budget"]
                        Line    = 0
                        Message = ("a11y\budget: '{0}' is in the accessibility page set but has no entry in {1}, so no floor bounds it" -f $page, $consumers["budget"])
                    })
            }
        }
        foreach ($key in $budgetKeys) {
            if ($want -notcontains $key) {
                $diffs.Add([pscustomobject]@{
                        Pair    = "a11y\budget"
                        File    = $consumers["budget"]
                        Line    = 0
                        Message = ("a11y\budget: '{0}' has an entry in {1} and is not in the accessibility page set, so nothing measures it" -f $key, $consumers["budget"])
                    })
            }
        }
    }

    return $diffs.ToArray()
}
