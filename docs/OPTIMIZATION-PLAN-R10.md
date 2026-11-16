# Optimization plan — round ten

Written against master @ 1813e8b (261 tracked files, 18.65 MB), on the machine that produced
every capture in docs/screenshots/. This is a *plan*. docs/UI-NOTES.md stays the record of what
has been fixed; docs/OPTIMIZATION-PLAN-R9.md is left exactly as it was written, including the
phase-two list this round finally picks up.

Round nine did what it said. All five features shipped, all three optimisations landed, and each
one now carries a counting test instead of a claim (§2.4). Then a round arrived that wrote no plan
at all: it added a crate (`pixiv-novel`), a markup parser (1 084 lines), a native reader with
nineteen typography settings, and three pages. Nothing looked at them. That is not a suspicion,
it is a measurement —

* `git grep -l novel -- tools/` returns **nothing**: six UI verifiers, zero references.
* `tools/a11y-budget.json` has **eight** page keys; `novels`, `novel` and `novel-reader` are not
  among them, and `tools/verify-a11y.ps1:34` iterates a hard-coded list of the same eight pages,
  so the new pages are not merely unmeasured — they are not visited.
* `tools/verify-layout.ps1:22` carries the same eight page names; the 24-check layout matrix has
  never seen a novel page at any of its three sizes.
* `docs/screenshots/baseline` holds 30 tracked captures over 15 page-states (settled in `4fd113f`,
  and `git log 4fd113f..HEAD -- ui/` is **empty**, so the baselines still describe today's UI).
  None of the 15 is a novel page.
* The forced-demo capture path cannot even reach them: `git grep -c novel -- crates/pixiv-demo/src`
  is **0**. The demo catalogue has no novels, so `PIXIV_DESKTOP_DEMO=1` has no data to draw.
* What exists instead: `src/app/novel.rs:219` (`fixture_novel`, behind `PIXIV_DESKTOP_NOVEL_FIXTURE`,
  which no gate sets) and three hand-taken PNGs under `docs/screenshots/readme/` (README:31-35).

Round ten is therefore not "more features". It is: give the surface that shipped without a plan the
same guards every other surface already has, fix the twelve defects those guards would have caught,
and only then add the features the guards now make safe.

---

## 1. Baseline measured today, not remembered

Every row was produced in this session at 1813e8b with a clean worktree.

| Measurement | Result | How |
| --- | --- | --- |
| `cargo test --workspace` | **469** cases, every binary green | run this session |
| `cargo fmt --all -- --check` | PASS, 0.80 s | `tools/gates.ps1` |
| clippy-driver, `RUSTFLAGS=-D warnings`, local 1.98.1 | PASS, 0 lints, 148.40 s | same run |
| clippy-driver through **1.99.1**, the version CI pins | PASS, 360 log lines, 0 per-item lints, 171.00 s | same run |
| `tools/selftest-gates.ps1` | **15** negative experiments, 0 failed (round nine left 8) | same run |
| `tools/gates.ps1 -SkipBuild` | 6 gates, 0 failed, 0 skipped, **342.1 s** | run this session |
| `tools/gates.ps1 -Ui` | 14 rows (13 when the second toolchain is absent) | `tools/gates.ps1` |
| `src/uiaudit.rs` design rules | **19** `#[test]` (README:280 still says 17) | `Select-String` |
| Accessibility budget pages | **8**; novel pages absent | `tools/a11y-budget.json` |
| Visual baselines | 30 captures / 15 page-states; `ui/` untouched since 4fd113f | `git log` |
| Tracked tree | **261** files, **18.65 MB**; 40 PNGs are **15.71 MB = 84.2 %** | `git ls-files` + sizes |

Three facts read out of the code, not measured from a run, because they need a live account or a
reader's own library:

1. **One unknown opcode wipes every reading preference and persists the wipe** —
   `src/app/novel.rs:1344` is `_ => prefs = ReaderPrefs::default()`, and the two lines after it
   (`:1346` `self.novel.prefs = prefs.clamped()`, `:1347` `self.save_prefs(false)`) commit that
   default to SQLite. Nineteen settings, no test, no logging.
2. **The reader re-renders its whole body to change a colour.** `rerender_reader` (`:1356-1361`)
   calls `rows_from` and then `blocks.set_vec(rows)` — a full model reset — and it is reached from
   *every* edit, including the purely chromatic opcodes 0/1/2/17/18 (`:1348`).
3. **The body is parsed on the UI thread.** `parse::parse` + `rows_from` run inside the `on_ui`
   closure (`:1135 -> :1148 -> :1162-1163`); a 200 k-character novel pays that in the paint
   callback, and the reader is the one page whose content size is unbounded.

---

## 2. Findings

### 2.1 The round that shipped three pages and no gate

§0's measurements, in one sentence: the novel surface is the only part of this application where a
regression is invisible until a human looks at it. The reader is also the largest new surface since
the shell itself — 894 lines of Slint plus 1 746 lines of application code — so the exposure is not
hypothetical.

Two smaller consequences worth naming now, because each one is a separate commit:

* `PIXIV_DESKTOP_FIXTURE` understands five states (`ugoira`, `palette`, `selection`, `suggestion`,
  `downloads` — `tools/verify-visual.ps1:256-261`) and none of them is a novel state. The reader
  cannot be photographed deterministically at all, which is why its evidence is hand-taken.
* The demo catalogue has no novels, so the fix is not in the capture script. Data comes first.

### 2.2 Twelve UI defects a gate could have caught

Each row is provable from the declarations; none of them is visible in a capture of a page's
resting state, which is exactly why they survived a round.

| ID | Symptom | Evidence at HEAD | Why no gate saw it |
| --- | --- | --- | --- |
| **U1** | Novel search results cannot be reached by keyboard and are nameless in the UIA tree: `NovelRow`'s only hit surface is a bare `TouchArea`, and the page declares no role, no label and no `FocusScope` at all | `ui/pages/novels.slint:101-104`, callback at `:27` | `every_interactive_component_declares_a_role` samples *components that declare a role*; a bare `TouchArea` is not in the sample. The Tab walk only counts stops, and the page is absent from the budget |
| **U2** | The novel page's author link is clickable, not focusable, and anonymous: two `TouchArea`s, zero roles, zero labels on the whole page | `ui/pages/novel.slint:168-171` | same sampling hole |
| **U3** | The detail page's zoom/pan surface is pointer-only: `scroll-event` and `pointer-event`, no role, no key handler — 4 `TouchArea`s in the file, 1 with a role | `ui/pages/detail.slint:83-95`, `:1149`, `:1198` | the role rule and the Tab walk both miss "interactive surface with pointer events and nothing else" |
| **U4** | Suggestion rows are keyboard-selectable (the panel's cursor is driven by `suggest-key`) but the row itself is a bare `TouchArea` with no role, so a screen reader reads text and no action | `ui/main.slint:151-154` | self-drawn overlay, not a page, so no budget and no capture |
| **U5** | Same defect in the command palette's rows — the one control the palette exists for | `ui/main.slint:122-125` | same |
| **U6** | The novel page's tag strip is 26 px tall with `clip: true` and an inner `Flickable` whose scrollbars are `always-off`: chips past the width cannot be wheel-scrolled or reached from the keyboard, and the drag gesture competes with the page's own vertical scroll | `ui/pages/novel.slint:208-231` | R9 §12d fixed the scrollbar *overlapping* the chips; whether the chips are reachable was never asked |
| **U7** | There is no in-page way back from a novel: `novel-detail-back` is declared, has a live Rust handler, and is called by nothing | declared `ui/main.slint:546`, handler `src/app/callbacks/novel.rs:105` | the "declared vs reached" gate counts declarations, so zero callers is not a failure |
| **U8** | `open-card-author` is declared, wired to `App::open_profile`, and called by nothing — the documented "open the artist from the card without a second lookup" path is dead; two pages use `open-author(it.author-id)` instead | declared `ui/main.slint:458`, handler `src/app/callbacks/profile.rs:16`, live path `ui/pages/gallery.slint:61`, `ui/pages/home.slint:395` | same |
| **U9** | The style contract says no colour literal may live outside `ui/theme.slint`; seven still do, three of them with no token and no in-place reason | `ui/components.slint:1750`, `:1788`, `:1797`, default tint `ui/icons.slint:6` vs `ui/theme.slint:75` (`scrim` only) | §3 of R9 is prose. The tests that exist are contrast and byte-order — neither forbids a literal |
| **U10** | The profile header is a fixed 208/254 px box with `clip: true`; a long display name or an extra stat row is silently cut, and there is no scroll inside the card | `ui/pages/profile.slint:130-140` (reason comment `:131-134`) | `verify-visual` only ever photographs the fixture's name length |
| **U11** | The tag strip's height is the magic number 26 twice (container and `content-height`), and it is also the status-bar metric — if a `TagChip` ever gets taller, the strip crops it | `ui/pages/novel.slint:209`, `:221`; `TagChip` at `ui/components.slint:959` | duplicated literal, no assertion relating the two heights |
| **U12** | The reader's prev/next chapter captions are declared and never read: `ReaderView.prev-label` / `next-label` have zero consumers, so the chapter you are about to open is unnamed | `ui/novel-model.slint:91-92` (whole-repo grep: declaration only) | no rule asks whether a declared property has a reader |

Rows U1-U5 share one root cause and one fix: **a hit surface that is a bare `TouchArea` is invisible
to both the accessibility gate and the role rule**. Widening the rule's sample (§7 G30) turns the
five fixes into one guarded class, and it is the cheapest structural win in this round.

### 2.3 A preference wipe, one unknown opcode away

| ID | Symptom | Evidence | Acceptance |
| --- | --- | --- | --- |
| **O4** | An unknown reader-pref opcode resets all nineteen settings **and saves the default over the user's row** | `src/app/novel.rs:1344`, `:1346-1347` | calling `apply_reader_edit(op = 99, value)` leaves every `ReaderPrefs` field equal to what it was and makes **zero** `set_novel_setting` calls; out-of-range op is a no-op |
| **O5** | Any preference edit rebuilds the whole body model; changing the theme colour rebuilds text that did not change | `src/app/novel.rs:1348`, `:1356-1361` | on an 800-block document: one font-size change -> `rows_from` == 1 and `set_vec` == 1; one theme change -> `rows_from` == 0 and no block reset; ten consecutive drags -> `rows_from` <= 1 (debounced) |
| **O6** | `parse::parse` and `rows_from` run inside the `on_ui` closure, i.e. on the paint thread | `src/app/novel.rs:1135 -> :1148 -> :1162-1163` | a 200 k-character body keeps the `on_ui` body under 16 ms, and the parse runs on a thread whose name contains `blocking` |
| **O7** | Blocking SQLite calls sit inside `rt.spawn` async blocks (``Arc<Mutex<Connection>>`` behind them) | `src/app/novel.rs:1124`, `:1238-1240`; `crates/pixiv-store/src/lib.rs:254` | a static check finds zero `async move { .. store.` call sites; a parallel timing task jitters < 1 ms while a store write runs |
| **O8** | GIF export encodes and writes on a runtime worker, with no `sync_all` | `src/app/downloads.rs:449`, `:455-459` | export runs under `spawn_blocking`, the write goes through the existing `write_synced` (`crates/pixiv-image/src/disk.rs:365`), and a 60-frame export does not move a parallel timer's p99 by more than 1 ms |
| **O9** | Every store query re-prepares its statement | `crates/pixiv-store/src/lib.rs:297`, `:314`, `:338`, `:366` | one "append a history row then read it back" path goes from 2 prepares to 0 (counted), p50 < 200 us |
| **O10** | Retry is *per attempt* 20 s, three attempts, no overall deadline — the worker's own comment says worst case 3 x 20 s | `crates/pixiv-api/src/lib.rs:91`, `:124`, `:1003`; `crates/pixiv-download/src/worker.rs:95-96` | a mock transport that always times out returns from one client call in <= 25 s, and the sleeps still sum to at most `BACKOFF_CAP` |
| **O11** | `DiskTier::put` holds the index lock across create/write/**fsync**/rename, so six concurrent covers serialise behind one fsync each | `crates/pixiv-image/src/disk.rs:190-196`, `:365-369` | with an injected 20 ms `write_synced`, six concurrent puts finish in <= 2 x 20 ms instead of 6 x 20 ms, and `the_byte_counter_never_drifts_from_the_tree` (`:539`) stays green |

Already correct, and named here so nobody re-fixes them: R9's three optimisations all landed —
the detail page decodes per the plan in `install_detail` (`src/app/detail.rs:267`, `:649-702`),
`DiskTier` keeps an index and never walks on the hot path (`crates/pixiv-image/src/disk.rs:43`,
`:294-329`, with `scans()` and restart-equivalence tests at `:582-609`, `:614-651`), and the
download worker parks on a `Notify` (`crates/pixiv-download/src/worker.rs:101`, `:724-775`, with
a `wakeups` counter as the measurement). Search suggestions are debounced with a generation token;
the store runs WAL + `busy_timeout` (`crates/pixiv-store/src/lib.rs:228-231`); migrations run in a
transaction (`:206`); decode and disk IO are already on the blocking pool
(`crates/pixiv-image/src/lib.rs:690`, `:702`, `:715`, `:735`).

### 2.4 Documentation has drifted from the tree in four measured places

| Claim | Where | Reality today |
| --- | --- | --- |
| "pixel regression: **20** captures (8 pages x light/dark, plus the detail page in both its content and its error state)" | README:227 | **30** captures over 15 page-states: three detail states plus five fixture states (`tools/verify-visual.ps1:51`, `:256-261`) |
| "...and the **four** measured UI gates as well" | README:272 | **six** verifiers run under `-Ui` (`tools/gates.ps1:313-346`) |
| "**327** tests: **17** uiaudit rules, 15 live tests ... a green run is 312 passes and 15 no-ops" | README:280 | **469** cases; uiaudit holds **19** `#[test]` rules (two of them landed in the novel round) |
| "it does **not** run any `tools/*.ps1` gate" | README:292-295 | CI has a `gates` job running `pwsh -File tools/gates.ps1 -SkipTest -SkipBuild`, and the summary job `needs` it (`.github/workflows/ci.yml:146-182`) |

Also stale, and cheap: `docs/UI-NOTES.md` has two `## 14` sections (`:860` and `:1305`), and the
focus captures README:243 points at `docs/screenshots/baseline/focus-*` while the gate writes
`target/focus` (`tools/gates.ps1:328`); 30 untracked `focus-*.png` are still sitting in the
baseline directory (60 PNGs on disk, 30 tracked).

### 2.5 What rounds eight and nine left open

UI-NOTES §14h listed seven open items; two are now closed (the accessibility gate fails without its
budget; the Tab walk no longer pins its own sample as the requirement). §15g listed seven; **none**
are closed, and three of them are registered for a third time — a screen reader reading the tree,
the Linux/macOS input paths, and pointer-drag gestures. This plan stops registering them as
"unverified" and gives the two that are testable here an owner (§7 G26, G30, G31 for the tree;
§9 names the two that stay out of scope because this machine cannot decide them).

---

## 3. The style contract — unchanged, and it still binds

The visual target stays **70 % WinUI 3 / Fluent 2 + 30 % Pixiv web**, as declared at the top of
`ui/theme.slint`. No phase may:

* add a colour, radius, spacing or font literal outside `ui/theme.slint` — and that sentence becomes
  a test this round (G31), because U9 is what happens when it stays prose;
* introduce a shadow, glow, gradient or blur the theme has no token for;
* change a control metric: 32 px controls, 4 px control radius, 8 px card radius, 48 px top bar,
  232/52 px rail, 26 px status bar;
* touch the accent's division of labour (accent decorates, accent-fill carries ink, accent-ink is
  text on the accent wash, on-accent is ink *on* accent-fill);
* trade a measured number for a looked-at one.

One clause is new, and it is the reason §2.1 hurts: **the reader is not a separate design system.**
Its measure, indent, ruby convention and type ramp are settings over the same type layer
(`crates/pixiv-novel/src/prefs.rs` clamps to the same bounds `uiaudit` already checks in
`reader_bounds_match_the_preference_model`), and any new reader control must reuse
`IconButton` / `TextField` / `TagChip` rather than invent a 26 px one (U11).

---

## 4. Interface contract — frozen in phase 0, before anything is filled

Parallel safety rests on this table. Phase 0 lands every declaration with a default and a no-op
handler, so the tree compiles at every commit; afterwards a track may only fill its own files.

| Where | What |
| --- | --- |
| `src/uiaudit.rs` | G26-G31 helpers: `page_level_clip`, `row_children`, `declared_properties`, `hit_surfaces`, `labelled_roles` |
| `ui/novel-model.slint` | `ReaderChapter { id, order, title, readable }`; `in property <[ReaderChapter]> chapters: []`; `in property <bool> toc-open: false`; `callback toc-toggle()`, `callback toc-choose(int)`; `NovelRow.progress: float` |
| `ui/main.slint` | `struct PaletteRow` gains `kind: int` (0 page / 1 action / 2 recent / 3 novel); `callback novel-detail-back()` gets its call site |
| `crates/pixiv-store/src/novel.rs` | `recent_rows_with_progress()`; `series_progress` becomes reachable from `src/` |
| `crates/pixiv-store/src/lib.rs` | `Store::prepare_cache` (private); no public shape change |
| `crates/pixiv-demo/src/catalog.rs` | `novel(s)` rows with a deterministic 8-chapter series and a body that exercises every block kind |
| `src/app/novel.rs` | `apply_reader_edit` split into `restyle` / `reflow`; `reader_chapter_rows()`; `parse off-thread` |
| `src/app/palette.rs` | `Target::Novel(u64)`; `novel/show.php?id=`, `/novel/1234`, `novels/1234` in the parser table |
| `tools/verify-visual.ps1` | six novel page-states in `$fixturePages`; the page list becomes one constant read by a11y, layout and visual |

---

## 5. Features

### F1 · The reader's table of contents, and the chapter you are about to open

`novel-reader.slint` has a prev/next pair whose captions are declared and never drawn (U12), and
nothing that names the series' other chapters. Build the panel from `series_content` data the
client already requests, on the existing 36 px floating pill: a list of `ReaderChapter` rows, the
current one marked, `available: false` rows drawn disabled (NOVEL-READER.md records that a
neighbour with `available: false` must be treated as absent, so a disabled row is the honest
rendering), `toc-toggle()` from the pill and from `t`, Escape closes and returns focus to the pill.
`prev-label`/`next-label` finally get their reader, which is also the negative experiment for G28.

Tests: the label map (`available` -> disabled, order -> caption), the panel's key machine
(open/close/Up/Down/Enter/Escape), and that opening a novel whose series has one chapter draws no
panel. Demo: the 8-chapter series from §4.

### F2 · "Continue reading 42 %" in the recent list

`crates/pixiv-store/src/novel.rs:69` already stores `(block_index, char_offset)`; `NovelRow`
(`ui/novel-model.slint:9-20`) has no field for it, so the list says only a title. Add the column
read (a join, not a new table), a percent in `NovelRow`, and a 3 px progress rule under the row in
the accent token. Acceptance: a store test that a row's percent equals
`progress_percent(block, char)` for a known document, and a home-page-shaped test that a novel with
no progress row draws no rule (not a 0 % one).

### F3 · Series-level continue, from the novel page

`series_progress()` exists (`crates/pixiv-store/src/novel.rs:119`) and is called by nothing but its
own test. The novel page's chapter band uses it to mark the chapter the reader was last in and to
draw "继续阅读 第 4 章" instead of "共 8 章". Acceptance: a store test over two chapters in one
series and one chapter in another (the existing test's shape), plus a page test that the band names
the chapter, not the count.

### F4 · The artist's novels, on the artist page

`crates/pixiv-api/src/lib.rs:762` (`user_novel_rows`) and `endpoint.rs:383` already fetch a user's
novels; `src/app/profile.rs` and `ui/pages/profile.slint` contain no reference to novels at all.
Add a second tab to the profile page using the same grid contract as the illustrations tab, with
the existing `NovelRow` card. Acceptance: a fixture test over the two-call endpoint shape
(`profile/all` -> `novels?ids[]=`) and a UI test that an artist with no novels shows the empty band
rather than an empty grid.

### F5 · The palette learns novels, and one of the dead callbacks dies

`parse_target` (`src/app/palette.rs:78-88`) knows `artworks` and `users`; `novel/show.php?id=1234`
falls through to "search this". Add `Target::Novel` with the three URL shapes the parser already
handles for artworks, and a `kind` on the row so the palette can label what it found. In the same
commit, U8's `open-card-author` is deleted with its handler: the codebase has a live path for that
action and a second, dead one is not documentation.

### F6 · The demo catalogue gets novels, so the reader can be photographed

Without this, F1-F3 cannot be verified by any gate and U1/U6/U10 cannot be baselined. Add to
`crates/pixiv-demo/src/catalog.rs`: one 8-chapter series whose body exercises every block kind
(`PageBreak`, `Heading`, ruby, `WorkRef`, `Link`, `Image`, `Separator`, an unknown marker that must
stay literal), one standalone novel, and one `isLoginOnly` row. Then add six page-states to
`$fixturePages` and capture them in both themes (12 new baselines).

### What this round is not doing, and why

* **Novel bookmark writes.** `src/app/callbacks/novel.rs:82-89` is a button that says "暂未开放",
  and it should stop being a button. But NOVEL-READER.md is explicit: the mutation route was never
  verified, and "a client that guesses a write is worse than one that says it has not verified it".
  This round makes the control honest (a disabled state with the reason in an InfoBar) and files the
  route as a verified-by-capture prerequisite. Faking it is the one thing the architecture forbids.
* **Ugoira WebP/MP4 export.** Needs an animation encoder the `image` crate does not have
  (README limit 7). A new dependency is its own decision, not a rider on this round.
* **HTTP Range resume.** `crates/pixiv-download/src/worker.rs:382` says out of scope, and O11's
  lock change touches the same write path — two risky changes in one file in one round is how a
  download queue regresses quietly.

---

## 6. Optimisations

The eight rows in §2.3 are the whole optimisation chapter, and each one already carries its
acceptance condition. Ordered the way they should be built:

1. **O4** (preference wipe) — a correctness bug first, by itself, with its test.
2. **O5** (restyle vs reflow) — the split the reader needs before F1 adds a panel to the same file.
3. **O6** (parse off-thread) — the unbounded-content fix; depends on O5's split to be small.
4. **O7** (blocking store calls) and **O9** (statement cache) — same crate, one commit each.
5. **O8** (GIF export) — three lines plus a test, and it removes a stutter users can feel.
6. **O10** (deadline) — the only change here with a network-visible effect.
7. **O11** (put lock) — last, because it re-opens the window O2 closed and its test is the O2 test.

---

## 7. Gates this round owes

| ID | Gate | Negative experiment |
| --- | --- | --- |
| **G26** | `clipped_bodies_are_reachable_through_a_scroll_view` — a page-level container with a clipped body and no `ScrollView` fails | replace the detail body's `ScrollView` with a `clip: true` rectangle -> fails and names the property |
| **G27** | `fixed_size_children_of_a_row_state_their_cross_axis_alignment` — every fixed-size child of a `HorizontalLayout` (not only `Text`) declares its cross-axis alignment | delete one 28 px button's `vertical-alignment` -> new rule fails, the existing `Text`-only rule still passes |
| **G28** | `every_declared_property_has_a_reader` — a declared `property` with no read site outside its declaration fails | plant an unread `property <string>` in `home.slint` -> names file and line; U12 makes it red on the tree as it stands |
| **G29** | `state_overlays_are_declared_after_their_hit_surface` — an overlay declared before the hit surface it must receive clicks over fails | move the detail failure block back before the viewer `TouchArea` -> fails, naming `IllustDetail` |
| **G30** | the role rule's sample widens from "components that declare a role" to "every hit surface with a `clicked` handler", and the Tab walk asserts *coverage*, not only a count | U1-U5 make it red on today's tree (five pages); a planted bare `TouchArea` in `components.slint` fails it after the fixes |
| **G31** | `every_interactive_component_passes_a_label`, plus a literal-colour rule extended from "page files" to "every file but `theme.slint`" (U9) | drop a `label:` argument from a `Switch` -> fails; re-plant `#ffffff` in `components.slint` -> fails |
| **G32** | `selftest-gates` gains a `page-set` case: the route enum, the three `$Pages` lists and the budget's keys must agree | delete one page from any of the four -> the case fails and prints the difference (this is the gate that would have caught §2.1 on the day) |
| **G33** | the reader is in the pixel baseline: 12 new captures (6 novel page-states x 2 themes), exact, from the demo catalogue | change one glyph in the reader's header -> red on the novel baseline only |
| **G34** | `EXPLAIN QUERY PLAN` on the three hot queries, asserting no `SCAN` of a large table | drop the composite index on `downloads(state, created_at)` -> the gate fails |
| **G35** | the numbers in README are recomputed, not remembered: test count, uiaudit rule count, capture count, gate count | change one of the four numbers in README -> the case fails and prints the measured value |

---

## 8. Phases, and the commit each one earns

**Phase 0 — Lead.** This document, then the §4 declarations with defaults and no-op handlers, so
every later commit compiles alone.
`docs(plan): round ten, and the round that shipped without one`
`refactor(app): the round-ten seams, declared before they are filled`

**Phase 1 — four tracks on disjoint boundaries.** No track edits another track's files; the
interface in §4 is what makes that true.

| Track | Boundary | Delivers |
| --- | --- | --- |
| **A · guards** | `src/uiaudit.rs`, `tools/*.ps1`, `ui/main.slint`, `ui/components.slint`, `ui/pages/{home,gallery,detail,downloads,settings}.slint` | G26-G31, G35; U3-U5, U9 |
| **B · the novel surface** | `ui/pages/{novels,novel,novel-reader}.slint`, `ui/novel-model.slint`, `src/app/novel.rs`, `crates/pixiv-store/src/novel.rs` | U1, U2, U6, U11, U12; F1-F3; O4-O7 |
| **C · demo and captures** | `crates/pixiv-demo/`, `docs/screenshots/baseline/` | F6, G33's data half |
| **D · transport and storage** | `crates/pixiv-api/`, `crates/pixiv-download/`, `crates/pixiv-image/disk.rs`, `crates/pixiv-store/src/lib.rs`, `src/app/{profile,palette,downloads}.rs`, `ui/pages/profile.slint` | U7, U8, U10; F4, F5; O8-O11; G34 |

Track commits:
`fix(novel): an unknown opcode must not reset the reader`
`perf(novel): restyle without rebuilding the body, and parse it off the paint thread`
`feat(novel): a table of contents, the chapter you are about to open, and 42 % behind the title`
`feat(profile): the artist's novels, and one dead callback fewer`
`feat(palette): novels are a target, not a search term`
`test(ui): five hit surfaces that were invisible to the accessibility gate`
`test(uiaudit): four laws this repository wrote down and never enforced`
`feat(demo): a series you can photograph, and the reader in the pixel baseline`
`perf(api): one deadline per call, not three deadlines per attempt`
`docs(readme): four numbers the tree stopped agreeing with in round nine`

**Phase 2 — Lead, and the only step that needs the desktop.** Re-baseline (12 novel captures),
then `tools/gates.ps1 -Ui` end to end. The round is accepted when that run reports 0 failed,
0 skipped, and `selftest-gates` reports 20+ cases.

---

## 9. What this round will still not prove

* **A real screen reader reading the tree.** Registered in round eight, round nine and round ten.
  It needs assistive technology this machine does not run; G30 makes the tree *complete*, which is
  necessary and not sufficient, and the plan says so instead of implying otherwise.
* **Linux and macOS input paths.** `build.yml` ships binaries for both; no gate has ever launched
  one here.
* **Hover.** Still unmeasured (§14h), and `tools/drive.ps1` can now synthesise a pointer move, so
  this is a gap in will rather than in capability — it stays out because this round already owes
  ten gates.
* **The settings capture's live cache figure** (UI-NOTES §15e): the fixture seam exists for five
  states and not for that number. A twelfth gate for one label is a poor trade; the budget absorbs
  it, and the plan records the trade rather than pretending the capture is deterministic.
