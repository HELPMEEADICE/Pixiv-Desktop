# Optimization plan — round seven

Written against `master` @ e030a67` (round six, 84 commits), on the machine that produced
every capture in `docs/screenshots/`. This is a *plan*: docs/UI-NOTES.md` is the record of
what has been fixed. Round six's plan (`docs/OPTIMIZATION-PLAN.md`) is left exactly as it
was written — a plan edited after the fact stops being evidence of what was planned.

Round six ended believing its gates could decide what they claim. Five of them cannot, and
that is where this round starts.

---

## 1. Baseline measured today, not remembered

| Measurement | Result | How |
| --- | --- | --- |
| cargo test --workspace` | 291 listed, 291 passed (15 of them live tests that return early) | `cargo test -- --list` |
| tools/verify-visual.ps1` | 20 of 20 inside tolerance; `settings` 0.0055 %, largest max=145` | run this session |
| `tools/verify-a11y.ps1 -Report` | 8 of 8 pages meet the budget; tabStops=10` on every page while `focusable` is 14–44 | run this session |
| Tab walk, home`, 1024×768, real keys | **45 distinct stops**; first chip at 15, first card at 27 | probe, `target/probe/tab2.ps1` |
| Tracked PNG under docs/screenshots` | 81 files, 22 687 609 B = **91.8 %** of the tracked tree | `git ls-files` + sizes |
| focus-*.png` alone | 34 files, 12 890 992 B = **52.2 %** of tracked bytes, and nothing reads them | `tools/verify-focus.ps1:247` |
| src` + `ui` + crates` + `tools` + fixtures` | 1 590 992 B | as above |
| `.git` / target` | 34.4 MB / 19.6 GB (untracked) | on disk |

The tree is green. Nothing below exists because a gate is failing; it exists because a gate
*cannot* fail, or because no gate exists for the thing that is wrong.

---

## 2. Findings

Severity is "how much of the product a user notices", not "how hard it is to fix".

### 2.1 Five gates that cannot fail

Each was confirmed by reading the path *and* running the gate.

**G1 · P0 · A skipped pixel comparison is reported as a pass.** `verify-visual.ps1:174`
adds a SKIP` row when the baseline sidecar's `scale` differs from the live one, and :200`
builds the failure set as `Verdict -notin @("OK","BASE","SKIP")`. On a 100 % or 125 % host —
a DPI verify-dpi.ps1` tells the reader to try — all 20 comparisons are skipped, the script
prints *"20 captures within 0.02 % of the baseline"* and exits 0. `gates.ps1:93` already has
the right verdict for this (77 → SKIP`); `verify-visual` never returns it.

**G2 · P0 · The comparator cannot see the defect class rounds four and five were about.**
PixDiff.Compare` counts a pixel as equal when any neighbour within `Tolerance` matches, so
a whole-image 1 px shift scores over = 0` — and §12d was a defect of exactly one pixel of
clearance. `MaxDiffPercent = 0.02` (:28`) also allows 157 arbitrary pixels at 1024×768,
which is a 12 px icon disappearing.

**G3 · P0 · The lint gate runs a command that cannot fail on a lint.** `gates.ps1:190` runs
cargo check` through `RUSTC_WORKSPACE_WRAPPER` with no -D warnings`; only the Clippy branch
at `:202` has it, and this checkout cannot use that branch. Both branches then filter with
^(warning|error)` while `--message-format short` emits path:line:col: warning: …`, so
per-item lints are dropped twice over.

**G4 · P0 · The accessibility gate passes when its budget is missing or incomplete.**
`verify-a11y.ps1:197` exit 0`s after printing "no budget — run with -Update", and `:171`
falls back to the default` floors (1/2/3) for a page absent from the JSON. Delete the budget
and the gate is green; delete one page's entry and that page is green.

**G5 · P0 · The Tab measurement is truncated by its own sample size.** `minTabStops` is
pinned to 10 on all eight pages and TabSteps` defaults to 10, so the budget and the sample
are the same number: the walk stops exactly where the pin is. Today's walk reaches **45**
stops on `home` against focusable=44`. That is how §13f came to state, as fact, something
that is false (U3).

### 2.2 UI/UX defects, measured

**U1 · P0 · The home "latest works" strip clips away the title and author of every card.**
The strip is `height: 150px + Theme.card-caption-height` with clip: true`
(`ui/pages/home.slint:300`–302`), i.e. 194 px; `IllustCard` is
column-width × clamped-aspect + caption` (`ui/components.slint:1396`–1398`), and at
140 px with the 1.4–1.6 aspects Pixiv serves that is 240–268 px. The caption therefore starts
*below* the box: title, author and avatar are all cut, while the row above (the grid) shows
them. The skeletons declare `height: 150px` (home.slint:292`–`295`), which is what the
intent was; IllustCard` has no way to express it. No capture shows this, because the strip
sits below the fold at 1024×768.

**U2 · P0 · In dark mode, hovering a card's bookmark makes the glyph disappear.** Hover sets
`background: Theme.layer` (ui/components.slint:1329`); the unmarked glyph is the literal
`#2b2b2b` (:1340`); and `Theme.layer` *is* #2b2b2b` in dark (`ui/theme.slint:70`). One
declaration pair, contrast 1.0:1, no rendering required to know. uiaudit` cannot see it
because both literals sit in the artwork-overlay allowlist (`src/uiaudit.rs:176`).

**U3 · P1 · Tab reaches the cards and the page never scrolls to them.** Measured on home`
with real keystrokes: stops 1–13 are the command bar and the rail, 15–26 the twelve chips, 27
the first card — and from 27 on every reported card rectangle is clipped to the viewport's
bottom edge (physical y = 1499 at 1024×768 @150 %: `1260+239`, 1173+326`, `1043+456` all end
on the same row). The ScrollView never moves, so a keyboard user tabs into artwork they cannot
see; verify-focus.ps1` never notices because it walks the same ten chrome stops.
*Fix shape:* scroll the page on card focus — the geometry already exists in `src/layout.rs` —
and let the focus gate walk past the chrome, where it fails today.

**U4 · P1 · Collapsing the rail does not re-lay-out the detail page.** ui/main.slint:874`
feeds `DetailPage.available-width` from root.content-width`, an `in property` pushed by Rust
only from the width-changed callback (main.slint:390`, `src/app/feed/mod.rs:407`). At 1024
px with the rail collapsed the page really is 972 px wide, but content-width` still says 792,
so `wide` (detail.slint:518`) stays false and the detail page keeps its stacked layout —
180 px of unused room — until the window is resized. The grid is unaffected because it uses a
measured width, which is exactly the shape §1b paid for: a second, pushed copy of a width
Slint can derive itself.

**U5 · P1 · At 720×600 the detail page's page-strip is cut in half; at 720×520 it is
unreachable.** Page height 600 − 48 − 26 = 526; the strip needs 44 (toolbar) + 1 (divider) +
289.3 (viewer, `detail.slint:710`–714`) + 160 (`InfoPanel` min-height`, `:730`) + 68
(:764`) = 562.3. `main.slint:747` clips the overflow, so the strip's top lands at y = 494.3
and 31.7 px of it — with its hit area — is gone. At 520 the whole strip falls below the fold
and the detail page is the one page with no scroll view.

**U6 · P1 · The suggestion panel is clipped at 392 px and the arrow keys still select the
hidden rows.** 12 recommendation tags plus up to 5 history rows = 17 × 34px`
(`main.slint:51`) = 578, plus padding and the note ≈ 608, against height: min(392px, …)`
with `clip: true` (main.slint:1137`, `:1145`). Ten rows are visible; suggest_move`
(`src/app/feed/search.rs:191`) advances active` through all seventeen, so Enter searches a
tag the user never saw.

**U7 · P1 · Twelve chips are twelve Tab stops before the first artwork.** Stops 15–26
(U3's walk). The WinUI chip strip is one stop with arrow-key navigation inside; here reaching
the grid costs 26 presses.

**U8 · P1 · The profile banner is decoded at 640 px and drawn 2–3× wider.**
`ui/pages/profile.slint:106` draws it across the row; src/app/profile.rs:160` asks for it with
`images::target_width(1200.0, scale)`, whose clamp(128, 640)` (`src/images.rs:71`) is the
*card* ceiling. At 150 % that is 640 px stretched over ~1 800: the "decoded below its draw
size" class §13a closed, still live at a second call site.

**U9 · P2 · Text inside a field sits 2 px above the control's centre line.** TextInput` and
its placeholder are `y: 0px`, height: root.height - 4px` (`components.slint:469`, :497`,
`:499`) — centre 14 in a 32 px control — while both icon groups centre with
(root.height - self.height) / 2` (`:463`, :512`) — centre 16.

**U10 · P2 · The detail page's thumbnail strip has its scrollbar on top of the thumbnails.**
Box 68 px (`detail.slint:764`), padding: space-2` and 48 px thumbs (`:774`) put the content
at y 8–56; the horizontal bar occupies the bottom 14 px, 54–68. The home and gallery strips
both leave 4 px (§12d); this one overlaps by 2.

**U11 · P2 · A recommended artist never shows whether you follow them.** UserCard` reads
`name/account/comment/avatar/preview/avatar-state` and nothing else
(components.slint:1791`), so the `followed` and premium` fields Rust fills
(`src/app/feed/mod.rs:627`–628`) are never drawn, and `toggle-follow` has no caller —
home.slint:366` and `gallery.slint:257` both bind it to an empty block.

**U12 · P2 (carried) · The state block is 8 logical px off centre**; §12c's three measurements
still agree on 8. Accepted is not fixed.

**U13 · P2 (carried) · The download page has never been seen populated.** The six-state
mapping is unit tested; no capture contains a row.

**U14 · P2 (carried) · The recent-search row breaks at a hard-coded four chips**, verified only
at 720/800 px with the demo vocabulary.

**U15 · P2 · Hover is measurable and unmeasured.** tools/drive.ps1:47` exposes
`Move-Pointer`, and its own header records that the "cannot be synthesised" conclusion has
been wrong for rounds; §10/§13f still list hover as unverifiable.

**U16 · P3 (to verify) · The folder picker has no owner window** (src/picker.rs:56`,
`hwnd_owner: null`), so it can open behind the main window and leaves it clickable.

### 2.3 Runtime: work that repeats, never cancels, or blocks the frame

**R1 · P0 · Opening an artwork decodes every page at 1600 px and never cancels.**
src/app/detail.rs:229` plans a URL for all pages; `:263` decodes three at a time at
VIEWER_MAX_WIDTH = 1600` (`src/app/mod.rs:83`) for thumbnails drawn at 48 px
(ui/pages/detail.slint:778`). A 40-page work is ~326 MB of resident pixels, the 192 MB memory
tier is flushed of covers, and because `detail_epoch` is only incremented on open
(detail.rs:34`), leaving the page cancels nothing.

**R2 · P1 · "Load more" rebuilds the whole grid, every time.** `src/app/feed/mod.rs:14`
std::mem::take`s `self.items`, and :51` then reads `self.items.len()` — 0 by construction.
already > 0` at `:66` is never true, append_columns` is dead code, and every append runs
`rebuild_columns()`: new models for every row, a fresh distribute`, five `SharedString`
allocations per item and a full card re-creation in Slint. Cost grows with what the user has
already scrolled through.

**R3 · P1 · Every start walks the whole image cache on the UI thread.**
crates/pixiv-image/src/disk.rs:53` sums the tree by recursive `read_dir` inside
DiskTier::open`, from `ImageCache::new` (src/app/lifecycle.rs:140`) before the window
exists — thousands of `metadata` calls against a 1 GiB ceiling.

**R4 · P1 · Once the disk cache is full, every write re-walks it.** disk.rs:103` checks the
budget on each `put` and enforce_budget` (`:138`) re-lists and sorts the whole tree; put`
trims to exactly the budget, so the next `put` always exceeds it. O(files) per cover, six
covers in flight.

**R5 · P2 · The download progress bar cannot show network progress.**
crates/pixiv-download/src/worker.rs:431` fetches the whole body before `set_progress` is first
called with a total (:451`), so the fraction measures the local write and the bar jumps from
indeterminate to 100 %.

**R6 · P2 · History is written on the UI thread with an unindexed table scan.**
`src/app/detail.rs:201` calls record_view`, whose `SELECT COALESCE(MAX(seq),0)+1`
(crates/pixiv-store/src/lib.rs:290`) has no index and no pruning policy.

**R7 · P2 · The avatar map is emptied wholesale, banner included.**
`src/app/feed/avatars.rs:66` clears at 256 entries; the profile banner lives in the same map
(src/app/profile.rs:172`) and nothing re-requests it.

**R8 · P2 · The download queue is deep-cloned twice per 500 ms tick.** `lifecycle.rs:329`
calls tasks()` only to test for emptiness, then `push_downloads` clones again
(src/app/downloads.rs:19`); `tasks()` clones every row (worker.rs:278`).

**R9 · P3 · The download worker polls a 20 ms timer for its whole life** (`worker.rs:27`,
:355`), including with an empty queue.

### 2.4 Documentation that is false today

* `README.md:133` — "there is no accessible-*` attribute anywhere in `ui/`": there are **43**.
* README.md:235` — "`ci.yml` runs the same gates … and a release build": ci.yml` runs fmt,
  actionlint, check, clippy and test; the release build is `build.yml`, and no tools/*.ps1`
  gate runs in CI at all.
* `docs/UI-NOTES.md:826` — "Tab does not reach a card … this is the next round's first item":
  it does, at stop 27 (U3). G5 is why nobody knew.
* 15 live tests count as passed while returning on their first line
  (crates/pixiv-api/tests/live.rs:64`): "291 tests" is 276 plus 15 skips.

---

## 3. The style contract — unchanged, and it still binds

The visual target stays **70 % WinUI 3 / Fluent 2 + 30 % Pixiv web**, as declared at the top of
`ui/theme.slint`. No phase may:

* add a colour, radius, spacing or font literal outside ui/theme.slint`;
* introduce a shadow, glow, gradient or blur where the theme has no token for it;
* lengthen a motion past `Theme.motion` (150 ms) or give one a bounce/spring curve;
* change a control metric: 32 px controls, 4 px control radius, 8 px card radius, 48 px top
  bar, 232/52 px rail, 26 px status bar;
* touch the accent's division of labour (accent` decorates, `accent-fill` carries ink,
  accent-ink` is text on the accent wash);
* trade a measured number for a looked-at one. If a change moves a baseline, the baseline
  moves in the same commit with an explanation.

Fixes in this round are conventions, not restyling: a caption that is drawn is the same card
with its text visible; a hover state that does not erase its glyph is the same pill; roving
focus inside a chip strip is the WinUI pattern. Acceptance for each is the pixel gate: only
the pages a fix legitimately touches may move.

The contract's "no shadow, no gradient" clause is narrower than the tree: `components.slint:147`
(drop-shadow-blur`), `main.slint:993`, main.slint:1142`, `detail.slint:174` and one
@linear-gradient` in `profile.slint:119` exist, and theme.slint:116`–`119` opens the door for
them deliberately. That divergence is older than this round; it is recorded here so the next
person does not "discover" it as a regression. Either the clause or the five sites move, in a
round that decides which.

---

## 4. Phases, and the commit each one earns

### Phase 0 — the plan, and the documentation that is false (Lead, this session)

Rewrite the four false statements (D1–D4) and correct §13f with the measurement that
disproves its first item. Commit docs(plan): round seven, and the five gates that cannot
fail`.

### Phase 1 — four parallel tracks on disjoint boundaries (this session)

| Track | Boundary | Delivers | Commit |
| --- | --- | --- | --- |
| **A** `tools-truth` | tools/verify-visual.ps1`, `tools/gates.ps1` | G1 (nothing compared → exit 77, never green), G2 (a SHIFT` verdict when a shifted comparison is *strictly* better), G3 (`-D warnings` through RUSTFLAGS`, restored in `finally`; no lossy filter), plus a re-runnable negative experiment per gate | test(tools): three gates that can fail, and the run that proves it` |
| **B** `api-truth` | crates/pixiv-api/**`, `crates/pixiv-types/src/error.rs` | one profile_bundle` instead of two GETs of the same 94 KB document; a response-size ceiling checked before buffering; `429`/503` reaching the variant that already has a user-facing message, with `Retry-After` honoured | fix(api): one fetch for one document, and a body that cannot grow unbounded` |
| **C** `app-runtime` | src/app/feed/mod.rs`, `feed/avatars.rs`, profile.rs`, `lifecycle.rs`, downloads.rs`, `src/images.rs` | R2's dead fast path (one moved statement), U8's banner decode cap, R7's newest-N eviction, R8's dirty version instead of two clones | fix(app): the append path that never ran, and three costs that ran every tick` |
| **D** `ui-truth` | ui/pages/home.slint`, `ui/components.slint`, ui/pages/detail.slint`, `ui/main.slint` | U1 (a caption band the card can actually fit), U2 (hover that does not erase its glyph), U4 (drop the pushed width copy), U9 (field text on the centre line) | fix(ui): four declarations that disagreed with what they draw` |

Disjoint because A is the harness, B is the network crate, C is the app controllers, D is the
UI source. No track edits another's files; the one-line Rust follow-up that U4 leaves behind
lives in `src/app/feed/mod.rs`, which C owns, so the Lead lands it after both reports.

### Phase 2 — what is left, in this order (next wave)

[Added after Phase 1 landed: § 2.1's G4 and G5 were found by this round's audit and then
left unscheduled by the first draft of this list. They are item 0, ahead of everything
else, because G5 is the reason one finding in this plan was false. The amendment is
recorded rather than folded in silently.]

0. **G4/G5** the accessibility gate: a missing budget must fail instead of `exit 0`; a page
   absent from the budget must fail instead of falling back to the 1/2/3 floors; and the Tab
   walk must stop when focus cycles, not at a fixed 10 that is then pinned as the budget.
1. **U3** focus-follow scrolling + a focus gate that walks past the chrome.
2. **R1** detail paging: current page ±1 at viewer size, thumbnails at 48 px, epoch
   incremented on leave.
3. **U5/U10** the detail page's vertical budget and the strip's bar band.
4. **U6** the suggestion panel inside a ScrollView.
5. **U7** roving focus in the chip strip → one Tab stop.
6. **U13** the PIXIV_DESKTOP_FIXTURE=downloads` seam, a baseline, and a progress-width
   assertion.
7. **U12** the 8 px: measure the block's centre, bisect the three candidates, remove it or
   write the arithmetic that makes 8 correct.
8. **U14** `layout::wrap_chips(widths, available, gap)`.
9. **U11** followed/premium on UserCard`, and a caller for `toggle-follow`.
10. **U15** a hover gate on drive.ps1`'s `Move-Pointer`, exit 77 without a desktop.
11. **R3/R4** index the disk tier; **R5/R6/R9** streaming progress, an indexed history cursor
    off the UI thread, Notify` instead of the 20 ms poll.

### Phase 3 — record and re-verify

Full `tools/gates.ps1 -Ui`, re-baseline where a change legitimately moved a pixel, prune the
write-only focus captures with the script that wrote them, and docs/UI-NOTES.md` § 14 in the
same voice as rounds one to six: root cause, fix, guard, and what is still not verified.

---

## 5. Acceptance rules

1. **A gate change is accepted on a negative experiment**, not on a passing run, and the
   experiment must be re-runnable.
2. **Every defect class ends with a guard** — a unit test, a `uiaudit` rule or a tools` gate —
   and the guard must fail on the defect as it is today.
3. **A visible change ships Before and After captures** under the same scenario, viewport and
   theme.
4. **A number that moves is explained**: test counts, baseline pixels, decode targets, Tab
   stops, tracked bytes.
5. **No claim without a measurement.** Where a measurement is impossible on this host, the
   plan says so where the claim would have gone.

---

## 6. Risk register

| Risk | Mitigation |
| --- | --- |
| A changes a gate's exit contract while the other tracks run gates | the negative experiment runs against a copy in `target/`, never against docs/screenshots`; the green run is quoted before and after |
| B's `profile_bundle` leaves the old methods uncalled | the call site is Phase 2's first line, owned by the Lead |
| C's append fix changes card order | acceptance asserts the existing column models are pointer-identical and only rows are added |
| D moves a baseline | U1 and U9 legitimately move home` and `search`; each re-baseline names the page, the reason and the pixel count |
| A SHIFT` verdict makes the pixel gate flaky | it fires only when the shifted comparison is strictly better, and no baseline changes as a result |

---

## 7. What is still not verified after this round

A real screen reader reading the tree §12a produced; the Linux and macOS input paths; a
pointer-driven drag gesture; `U16`'s owner window until the drive harness can assert
IsWindowEnabled`; and the four upstream gates' behaviour on a 100 % host, which this host
cannot be turned into without a logon change.
