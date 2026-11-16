# Optimization plan — round eleven

Written against master @ 2fa9aee (262 tracked files, 18.8 MiB, of which 40 PNGs are 83.7 %),
with a clean worktree. Every row below carries a `path:line`. The rows came from four read-only
audits run in one session — Slint front end, application layer, crate layer, and the verification
and documentation layer — and the P0 and four P1s were re-read against the source before this
document was written. Nothing in §1 was measured by launching the app; §7's gates are what
measure it. `7e949c6` (one test in `crates/pixiv-image/src/disk.rs`) landed after the audits and
changes none of these rows.

Round ten wrote twelve gates, five features and eight optimisations. What is in the tree today:

* **Four gates landed, two half, six absent.** G34 (query plans) and G37 (no gate inherits the
  verdict of the row before it) are in; G30 landed only its Tab-coverage half and G31 only its
  colour half; G26, G27, G28, G29, G32, G33, G35 and G36 are not in the tree.
  `src/uiaudit.rs` still holds 19 rules and `tools/selftest-gates.ps1` still has 16 cases.
* **The three novel pages are invisible at every layer.** `tools/verify-a11y.ps1:34`,
  `tools/verify-layout.ps1:22`, `tools/verify-visual.ps1:51` and `tools/a11y-budget.json` do not
  name them once; none of the 30 tracked baselines is a novel page; `ui/main.slint:192-207`
  already has 13 destinations. The data is not the blocker — `crates/pixiv-demo/src/catalog.rs`
  already carries novels. Only the wiring is missing.
* **One P0.** `Ctrl+P` opens the command palette and `palette-field` (`ui/main.slint:1705`) is
  never given `focus-input()`, where both search boxes at `:636` and `:680` are. Typing, Up/Down
  and Enter all land nowhere; the panel is pointer-only.
* **The same disease recurs.** `BookmarkMenu` (`ui/pages/detail.slint:1203-1222`) declares
  Up/Down/Esc/Enter while `take-focus` has no caller; the reader's settings panel
  (`ui/pages/novel-reader.slint:941-995`) takes no focus and sets no trap; the card's author link
  (`ui/components.slint:1877-1892`) is a bare `TouchArea`; README limit 10 has said since round
  nine that the keyboard reaches every control and the page does not follow it.
* **Five failures are swallowed.** A failed page-list request becomes "one page"
  (`src/app/detail.rs:106-114`); only the sign-in and novel paths say what an expired session
  means (`src/app/settings.rs:3` has the sentence, the other paths do not); a failed avatar stays
  in the loading state forever (`src/app/feed/avatars.rs:189-192`); a failed "load more" advances
  `page_cursor` anyway, so that page is gone for the session (`src/app/feed/home.rs:266`); and the
  image concurrency gate loses a slot whenever a queued waiter is cancelled
  (`crates/pixiv-image/src/gate.rs:72-92`) — at zero slots every later load waits for good.
* **One promise has no implementation.** `Store::insert_download` / `downloads` /
  `delete_download` / `stats` (`crates/pixiv-store/src/lib.rs:499/516/537/543`) have no caller in
  `src/`, while `crates/pixiv-download/src/lib.rs:13-14` says the list survives a restart.
* **The documentation still restates old numbers.** README says "diffs 20 forced-demo captures"
  (the tree has 15 states x 2 themes = 30), "24 checks: 8 pages x 3 sizes", and that novel captures
  come from `PIXIV_DESKTOP_NOVEL_FIXTURE` — which no `tools/*.ps1` sets.

Round eleven is therefore: **finish the input paths that are already declared and turn them into
rules, make the failures that already happen visible, and wire only the features whose data the
crates already carry.**

---

## 1 · Interface contract (frozen before anything is filled)

Each track owns a disjoint file set; the names below are the seams the tracks meet at. A track
that needs a change in a file it does not own reports it instead of editing it.

| Where | Declaration |
| --- | --- |
| `ui/theme.slint` | `suggest-row-height`, `suggest-row-pad`, `notice-line-height`, `menu-metric-*`, `progress-rule-height` |
| `ui/pages/gallery.slint` | `callback card-focused(string, length)` — the second argument is the card's content-space y; `in property <[string]> sort-labels: []`, `in-out property <int> sort-index: 0`, `in-out property <int> mode-index: 0`, `callback sort-chosen(int)`, `callback mode-chosen(int)`, `in property <[string]> related-tags: []`, `callback related-tag-chosen(string)` |
| `ui/novel-model.slint` | `NovelRow.progress: float` — `-1` means "no progress", and a row with `-1` draws no rule |
| `ui/pages/profile.slint` | the existing `stats: [ProfileStatItem]` carries the follower figure; `in property <string> mutual-label: ""` |
| `ui/pages/downloads.slint` | `struct LocalRow { id, title, author, path, when, state }`; `in property <[LocalRow]> local-rows: []`; `callback local-open(string)`, `local-reveal(string)`, `local-redownload(string)`, `local-forget(int)` |
| `crates/pixiv-store/src/novel.rs` | `recent_rows_with_progress()`; the progress index becomes `(series_id, updated_at DESC)` |
| `tools/page-set.ps1` | one exported page set that the four verifiers and the budget all read |

## 2 · Findings

### 2.1 Track A — input honesty

| ID | Evidence | Symptom | Sev |
| --- | --- | --- | --- |
| U20 | `ui/main.slint:1705`, `:685-692` | the palette opens and its field never takes focus: typing, Up/Down and Enter are dropped | P0 |
| U21 | `ui/pages/detail.slint:1203-1222`; `ui/components.slint:2413`, `:2463-2471` | the bookmark menu's keys never fire: `take-focus` has no caller and `restrict-open` is page-local, so the shell's Escape has nothing to close | P1 |
| U22 | `ui/pages/settings.slint:93-140` vs `:338-359` | the clear-history confirmation is drawn at the top of the scroll body while the button that raises it sits 600+ px lower | P1 |
| U23 | `ui/pages/novel-reader.slint:941-995` | opening the reader settings takes no focus, sets no trap, and Tab walks the page behind the panel | P1 |
| U24 | `ui/components.slint:1387`; `ui/pages/gallery.slint:177`; `ui/main.slint:1150`; `src/app/callbacks/feed.rs:43-49` | `content-y` is two-way bound, so the view writes a clamped value back over `remember_scroll` whenever the body shrinks | P1 |
| U-f | `ui/pages/gallery.slint:64`; `ui/main.slint:328`; README limit 10 | focus does not scroll the page: `gained-focus` exists, the content-space y does not | P1 |
| U25 | `ui/pages/detail.slint:651`, `:890-999`; `ui/main.slint:406-407` | `detail-state` defaults to 0 with an empty page list, so the first frame after navigation is blank canvas | P2 |
| U26 | `ui/pages/home.slint:360-369`, `:452-461` vs `:147-151` | both skeleton bands hard-code 5 and 4 cards — 714/888 px inside a 618 px body at the minimum window | P2 |
| U27 | `ui/components.slint:1877-1892` | the card's author link is a bare `TouchArea`: no role, no label, no keyboard path | P2 |
| U28 | `ui/pages/settings.slint:223-228`, `:331-335`; `ui/components.slint:709-721` | a one-way `current:` binding is then assigned imperatively on click, which kills it: after one click the theme group stops following the command bar | P2 |
| U30 | `ui/main.slint:1417-1471` | the notice bar is a fixed 44 px single elided line with no role and no timeout, and it covers the last 44 px of the page | P2 |
| U32 | `ui/theme.slint:44-45,48,81,146,157,210-211,233,161-162` | eleven tokens have no reader; `card-min/max-width` duplicate `src/layout.rs:16-17` with no test tying the two | P2 |
| U34 | `ui/pages/home.slint:51-57` vs `ui/components.slint:273-298` | a non-compact button in a 26 px row paints a 32 px pill, 3 px outside on each side | P2 |
| U35 | `ui/main.slint:159` vs `:1636-1638` | SuggestRow's 34/5 px pair is written twice (the row and the panel's scroll arithmetic), and 44/26/48/68/28 px furniture bypasses the theme | P2 |

### 2.2 Track B — failure honesty

| ID | Evidence | Symptom | Sev |
| --- | --- | --- | --- |
| T1 | `src/app/detail.rs:106-114` | a failed page list is `unwrap_or_else(|_| Vec::new())`, so a 30-page work renders as one page; a failed profile read reports "not following" | P1 |
| T2 | `src/app/settings.rs:3` vs `src/app/feed/home.rs:182`, `src/app/detail.rs:119` | only sign-in and startup route through `describe_session_error`; every other surface shows the raw error while the novel page says 登录已失效 | P1 |
| T3 | `src/app/feed/mod.rs:59`, `:309`, `:439` | the append path does not de-duplicate while `index` holds one cell per id: an overlapping page yields two cards, the first keeps its placeholder, and the count double-counts | P1 |
| T5 | `src/app/feed/mod.rs:329-338` | `rank_of` scans `items` per card, so 500 rows cost about 125 000 comparisons per render | P1 |
| T7 | `src/app/feed/home.rs:266` | `page_cursor` is incremented before the request and never rolled back on failure | P1 |
| T8 | `src/app/helpers.rs:127-129` | `ui()` is `expect("window alive")`, so a result landing after the window is gone panics the event loop | P1 |
| T10 | `src/app/feed/avatars.rs:189-192`, `src/app/profile.rs:195-198` | a failed avatar drops its key and leaves the row in the loading state | P2 |
| T13 | `src/app/detail.rs:384-457` | the bookmark toggle has no in-flight guard: a double click sends two POSTs computed from one stale state | P2 |
| T14 | `src/app/feed/reconcile.rs:10` | a 20 Hz timer runs for the life of the window only to read `palette-open` and `selection-mode` | P2 |

### 2.3 Track C — crate layer

| ID | Evidence | Symptom | Sev |
| --- | --- | --- | --- |
| C1 | `crates/pixiv-image/src/gate.rs:72-92` | `release` sends `()` and returns; a waiter cancelled between send and poll discards the value, so one slot is lost per cancellation and at zero slots `acquire` waits for good | high |
| C3 | `crates/pixiv-ugoira/src/lib.rs:307-336`, `:59`, `:274` | `export_gif` accumulates the whole GIF in `out` while `scaled_to_width` clones the film, so a 60-frame 1080p export holds three copies | med |
| C4 | `crates/pixiv-download/src/naming.rs:107-130`; `worker.rs:946`, `:972-988` | two pages rendering to one path both pass `enqueue`; the later rename wins, one page is lost silently, and both rows report Completed | med |
| C5 | `crates/pixiv-api/src/mapping.rs:154` | 401 and 403 both become `Unauthorized`, so a self-only route tells a signed-in user to sign in | low |
| C6 | `mapping.rs:156-158` | a flagged 4xx becomes `Api(message)` and loses its status; the POST path carries it in `Failure`, GET has no equivalent | low |
| C7 | `crates/pixiv-novel/src/parse.rs:729-735` | `numeric_entity` accepts `&#0;`, putting a NUL into a caption that reaches a Slint text model | low |
| C8 | `parse.rs:647-652` | `attribute` searches the whole tag text for `href=` and can match inside another attribute's value | low |
| C11 | `crates/pixiv-image/src/disk.rs:461`, `:327`, `:244` | a killed process leaves `.part` files that `bytes()` and `enforce_budget` do not see, so the directory can exceed its budget | low |
| C10 | `crates/pixiv-demo/src/render.rs:251,293,303,313-315` | `f32` `sin`/`cos`/`powf` are libm-dependent, and the module claims two machines produce the same PNG bytes without naming the condition | docs |

## 3 · Style contract

The target stays 70 % WinUI 3 / Fluent 2 + 30 % Pixiv web; 32 px controls, 4 px control radius,
8 px card radius, 48 px top bar, 232/52 px rail, 26 px status bar, unchanged. New surfaces are
composed from `AppButton`, `IconButton`, `TextField`, `TagChip`, `Switch`, `SegmentedControl`,
`SettingsRow`, `IllustCard`, `StateView`, `InfoBar` and `MenuFlyout` — no 27th control. Colour,
spacing, control-size and type literals live in `ui/theme.slint` only; G31's literal rule widens
from "colours in page files" to "colours, spacing and control sizes in every file but the theme".

One clause is new and has the same standing: **focus is a first-class state**. Every surface a
pointer can open must be a surface the keyboard can enter, and closing it returns focus to the
control that opened it. U20, U21, U23 and U27 are all counter-examples.

Every regression-risk visual change carries a before/after pair under `target/r11/<ID>/`; a page
already in the pixel baseline uses the `-Update` diff as its evidence instead.

## 4 · Features

| ID | Data that already exists | Surface | Acceptance |
| --- | --- | --- | --- |
| F1 | `Store::insert_download/downloads/delete_download/stats` (`pixiv-store/src/lib.rs:499/516/537/543`, no caller in `src/`) | the downloads page grows a "已完成（本地记录）" band: title, source work, time, and open / reveal / re-download / forget per row; the settings page prints `Store::stats` | a store round trip including the UNIQUE pair and the limit; no band when the table is empty; re-download returns the row to the queue in `Pending` |
| F2 | `SearchQuery.sort`/`.target` (`pixiv-types/src/domain.rs:738/767`, serialised into the URL, never assigned in `src/`; `SortOrder::label()` already returns the Chinese labels), `SearchBodyWire.related_tags`/`.tag_translation` (`wire.rs:345/348`) | a segmented control on the search surface (最新/最早/热门) with a match mode (标签精确/标签部分/标题与说明), plus a related-tag chip row that searches on click | the URL carries `order`/`s_mode`; changing either resets the cursor and reloads page 0; an empty related-tag list draws no row |
| F3 | `Client::profile_bundle` (`pixiv-api/src/lib.rs:612`), `user_follower_count` (`:809`) | the profile page stops fetching `/profile/all` twice (`src/app/profile.rs:83`); the follower figure rides in `stats` (signed-in account only, `—` otherwise); `is_mypixiv`/`followed_back` drive `mutual-label` | a mock transport sees one `/profile/all`; another artist's page shows no follower figure; the mutual label appears only when the field says so |
| F4 | `crates/pixiv-store/src/novel.rs:69` stores `(block_index, char_offset)`; `series_progress()` (`:119`) is called only by its own test | a 3 px progress rule and a percentage under the recent list, and "继续阅读 第 4 章" on the chapter band | the row's percentage equals `progress_percent(block, char)`; no progress draws no rule rather than 0 %; the figure clamps into 0..=100 |
| F5 | `RankingResponseWire.date`/`.mode`/`.page` (`wire.rs:417-421`), dropped by `mapping.rs:371-383` | the ranking header names the board's date instead of implying "today" | the mapping passes `date` through; an empty date draws no separator |

## 5 · Optimisations

Each row is a measurable symptom from §2, not an impression:

1. **T5** — one `id → rank` map built per render pass instead of a linear scan per card.
2. **T3** — the append path goes through `append_unique`, which also removes the duplicate decodes.
3. **T14** — 20 Hz becomes 100–150 ms, and the timer is not armed while no surface can own the
   palette or the selection.
4. **C1** — the gate hands the permit itself to the waiter, so cancellation returns it through
   `Drop`; a counter would paper over the same bug.
5. **C3** — `export_gif` bounds `out` and fails with `UgoiraError::Encode` at the ceiling;
   `scaled_to_width` stops cloning the film.
6. **C4** — the second request for one rendered path is refused at `enqueue`.
7. **C9** (with F4) — the progress index becomes `(series_id, updated_at DESC)` with an
   `EXPLAIN QUERY PLAN` assertion in the shape `novel.rs:290` already uses.
8. **C11** — `DiskTier::open` sweeps `.part` files that do not belong to the current pid.

## 6 · Gates this round owes

Ten rows round ten wrote and did not land; four are new. **A gate lands with its fix in the same
commit; no commit adds a red rule.**

| ID | Rule | Today | Negative experiment |
| --- | --- | --- | --- |
| G26 | a page-level clipped body is reachable through a `ScrollView` | absent | replace the detail body's `ScrollView` with `clip: true` |
| G27 | every fixed-size child of a row declares its cross-axis alignment (widening `uiaudit.rs:1514` from `Text` to all children) | narrow form only | delete one 28 px button's `vertical-alignment` |
| G28+G39 | a declaration has a reader: a `property` has a read site and a `callback` has an invoker | absent | plant an unread `property <string>` in `home.slint` |
| G29 | a state overlay is declared after the hit surface it must receive clicks over | absent | move the detail failure block before the viewer `TouchArea` |
| G30 | the role rule's sample widens to every hit surface with a `clicked` handler, and the Tab walk asserts coverage, not only a count | half | plant a bare `TouchArea` in `components.slint` |
| G31 | label coverage, and the literal rule widens to every file but `theme.slint` including spacing and control sizes | half | drop a `label:` from a `Switch` |
| G32 | `page-set`: the destination enum, the four `$Pages` lists and the budget's keys agree | absent | delete `settings` from `verify-a11y.ps1:34` only |
| G33 | the reader is in the pixel baseline: six novel page-states x two themes | absent | change one glyph in the reader header |
| G35 | no gate-printed number is restated in prose | absent | put `327 tests: 17 uiaudit rules` back into README |
| G36 | `-Twice`: one run captures every state twice and requires 0 px between the two | absent | today's `suggestion` state, whose cursor arrives on a 400 ms timer, is the experiment |
| G41 | every failed load either reaches a page state or a log (scan `unwrap_or_else(|_`, `.ok()`, `let _ =`; the allowlist carries a reason) | new | re-plant `unwrap_or_else(|_| Vec::new())` after T1 is fixed |
| G42 | two requests never render to one path | new | two templates without `{page}` |
| G43 | a cancelled waiter returns its permit | new | capacity 1: hold, queue, release, drop the waiter before it polls, then `acquire` again |
| G44 | the page cursor only advances on success | new | fail the first load-more and assert the second request still asks for page 2 |

`tools/selftest-gates.ps1` grows from 16 cases to 20+, one negative experiment per new gate.

## 7 · Phases, commits and ownership

Phase 0 is this document. Each track then owns a disjoint file set and commits one item at a time,
each commit compiling on its own.

| Track | Owns | Delivers |
| --- | --- | --- |
| A · input | `ui/{main,components,theme}.slint`, `ui/pages/{settings,novel-reader,gallery,home}.slint`, `src/uiaudit.rs`, `src/app/callbacks/feed.rs` | U20–U28, U30, U32, U34, U35, U-f; G26–G31 |
| B · failures | `src/app/{mod,detail,settings,helpers,lifecycle,route,images,zoom}.rs`, `src/app/feed/{mod,home,discovery,avatars,reconcile,selection,batch,fixture}.rs`, `src/app/callbacks/{detail,settings,novel,downloads,profile,search,shell,mod}.rs` | T1, T2, T3, T5, T7, T8, T10, T13, T14; G41, G44 |
| C · crates | `crates/{pixiv-image,pixiv-download,pixiv-ugoira,pixiv-novel,pixiv-demo}/**`, `crates/pixiv-api/**` | C1, C3–C8, C10, C11; G42, G43 |
| D · features | `crates/pixiv-store/**`, `crates/pixiv-types/src/{domain,wire}.rs`, `src/app/{downloads,profile,palette,novel}.rs`, `src/app/feed/search.rs`, `ui/pages/{downloads,profile,novel,novels}.slint` | F1–F5, C9 |
| E · tools and docs | `tools/**`, `.github/workflows/**`, `docs/**`, `README.md` | G32, G35, G36; one page set; README and UI-NOTES drift |

Wave one runs A, B and E; wave two runs C and D once A has released `ui/pages/gallery.slint`.
Phase 2 is the author's alone and needs this desktop, in this order: `verify-visual -Twice`
(reproducibility before `-Update`), `page-set`, the new baselines, then `tools/gates.ps1 -Ui`.
The round is accepted when that run reports every row it owns with 0 failed and 0 skipped.

## 8 · What this round is not doing

* The card context menu — it adds a whole new overlay to the class of gates this round is fixing.
* Six SQLite calls still on the UI thread (T6): they want one `spawn_blocking` seam and the same
  callbacks T1/T2 touch.
* T9, T11, T12 (the full-grid avatar clone, two unbounded maps, the palette model rebuilt per
  keystroke) — after T5 and T3 have measurable numbers.
* C2, byte-accurate download progress: it shares the write path C4 touches, and two risky changes
  in one file in one round is how a queue regresses.
* The novel table of contents (round ten's F1): another overlay that needs a focus trap.
* U29 (downloads rows to a `ListView`), U33 (the decode cap under DPI), C10's integer renderer.
* Unchanged: a real screen reader, Linux and macOS input paths, hover measurement, HTTP Range
  resume.

Drop order if the round slips: F5, then F3, then C11, then U25/U30.

## 9 · Assumptions

1. The keyboard and focus class is the most visible defect on the tree today, so it precedes the
   features.
2. No new dependency.
3. The UI gates run only on this machine's interactive desktop; CI keeps omitting `-Ui`.
4. Whether the 30 baselines are green today is not assumed: Phase 2 measures it with `-Twice`
   first and re-baselines only afterwards.
5. `docs/` stays English, as rounds seven through ten did.
