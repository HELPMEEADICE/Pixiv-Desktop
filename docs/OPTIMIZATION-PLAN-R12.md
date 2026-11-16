# Optimization plan — round twelve

Written against `master @ b4fb7d1`, clean worktree, with `cargo test --workspace` measured first:
**388 passed, 0 failed** (`target/r12-baseline-test.log`). The rows below came from four read-only
audits (Slint front end, application layer, crate layer, and the gate and documentation layer) that
produced 48 findings; the nine P1 rows were re-read against the source before this document was
written and are marked "re-read" in the tables.

Round eleven landed: U20–U35, T1–T14, C1–C11, F1–F5 and G26–G44 of `OPTIMIZATION-PLAN-R11.md` are
in the tree or were explicitly dropped there, so none of them is repeated here.

**Round twelve is: give every seam a reader.** R11 froze six interfaces and wired half of them.
`SortOrder`/`SearchTarget` have a vocabulary and no control. `ai_type`, `is_masked`, `sl`,
`is_multi_page` and the ranking row's own `date` are parsed and read by nobody. The downloads page's
"completed (local record)" band has four buttons that a full-row `TouchArea` eats the clicks of. The
notice bar sits on top of the batch bar, hiding its buttons while letting their clicks through. Five
gates report PASS when they failed. This round wires those four classes, fixes them, and turns each
one into a rule that can fail.

---

## 0 · Acceptance

1. `cargo test --workspace` green; the new tests map one-to-one onto §9's gate table, and there are
   at least as many of them as there are gate rows.
2. `tools/gates.ps1 -Ui` ends on `0 failed · 0 skipped`. **A skipped gate is not a pass** — that is
   what E3 and E4 fix.
3. Every P0/P1 user-visible defect carries `target/r12/<ID>/before.png` and `after.png`: same
   scenario, same viewport, same theme, same scale factor. A state already in the pixel baseline
   uses the `-Twice` run plus the `-Update` diff as its evidence instead.
4. `tools/verify-visual.ps1 -Twice` proves reproducibility (0 px between the two captures of one
   state) before any baseline is rewritten. A round that re-baselines first and asks the question
   afterwards is rolled back.
5. One commit per item. Every commit compiles and passes its own tests, and no commit turns a gate
   red. Documentation drift (README limit 10 still says focus does not scroll the page, limit 12, the
   baseline count, the page count) is rewritten against code facts by track E.

## 1 · What the tree actually is, at the start of this round

* **R11's F2 shipped half, and its own commit says so.** `3f76aaf` changed only
  `crates/pixiv-types/src/domain.rs` (+117 lines of vocabulary); its message reads "The Rust half of
  that wiring is registered, not done". `ui/pages/gallery.slint:165-189` declares eleven properties
  and callbacks, `:311-362` draws two `SegmentedControl`s and the related-tag strip, and
  `sort_chosen`, `mode_chosen` and `related_tag` appear in neither `src/` nor `ui/main.slint`
  (re-read).
* **README and UI-NOTES are behind the code.** README:167-179 says the keyboard reaches every control
  and the page does not follow it; `src/app/callbacks/feed.rs:133-177` implements exactly that
  (R11's U-f). `docs/UI-NOTES.md:1274-1277` still carries the same sentence, and UI-NOTES stops at
  §17 (round ten): round eleven has no numbered section at all.
* **Gates report PASS when they failed.** `tools/verify-layout.ps1:67` and
  `tools/verify-icons.ps1:38` throw away `shot.ps1`'s exit code (`& pwsh @args | Out-Null`) and
  neither clears its own output directory, so last round's `<page>-<size>.json` is read as this
  round's trace. `tools/verify-a11y.ps1:346` exits 77 only when nothing at all was measured: ten
  pages skipped and one measured passes with exit 0 (re-read).
* **`detail` is shut out of the accessibility and layout sets by a false premise.**
  `tools/page-set.ps1:113` says "The two destinations no capture set opens", yet
  `Get-PageSet -For visual` (`:167`) contains `detail`, and the tracked baselines carry
  `detail-light`, `detail-dark` and the ugoira pair (re-read): the page is photographed and
  baselined, and has no accessibility floor and no gap check.
* **The captions are markup, and one fixture already proves it.** `fixtures/illust_detail.json`'s
  `description` carries `<br />` nine times (JSON-escaped as `\u003Cbr \/\u003E`, which is why a
  plain-text grep misses it) and no `<a href>`; the real `/ajax/illust/{id}` payload carries both.
  `crates/pixiv-types/src/domain.rs:383-387` copies it verbatim and `:1745`'s `clean_caption` is
  called on the novel path only, so the demo-and-fixture capture path has always had this defect in
  front of it whenever a caption reached a `Text`. Track C adds a second fixture with a link as well,
  because one of the two tag kinds was never exercised (corrected during the round by track C).

## 2 · Interface contract (frozen before anything is filled)

| Where | Declaration |
| --- | --- |
| `crates/pixiv-types/src/domain.rs` | `Illust { ai_type: u8, alt: String, is_masked: bool, sl: u8 }`, filled by `from_detail`/`from_search`/`from_street`; `NovelSummary { genre, language, bookmarks }`; `Illust::from_ranking` sets `create_date` from the row's `date` |
| `crates/pixiv-types/src/wire.rs` | `SearchItemWire.ai_type` (not declared at all today); `NovelItemWire.genre/language/bookmark_count` keep their names |
| `ui/theme.slint` | `notice-gap`, `batch-bar-height` (48px), `card-check-inset`; `reader-font-min/max` and `reader-measure-min/max` become *read*, they are not new |
| `ui/components.slint` | `IllustCard` keys grow to `Enter/Space` (open), `a` (author), `b` (bookmark); `BookmarkPill` therefore needs *no* new Tab stop (round eight's exception keeps its reason) |
| `ui/pages/gallery.slint` | the names R11 froze — `sort-labels/sort-index/mode-labels/mode-index/sort-chosen/mode-chosen/related-tags/related-tag-chosen` — are filled, never renamed |
| `src/app/feed` | one `FeedControls { sort: SortOrder, target: SearchTarget }`; changing either resets the cursor and reloads page 0 |
| `src/app/detail.rs` | `fail_detail(epoch)`; `toggle_follow` and `pick_restrict` share the `detail-busy` slot |

## 3 · Style contract

The target stays 70 % WinUI 3 / Fluent 2 + 30 % Pixiv web; 32 px controls, 4 px control radius,
8 px card radius, 48 px top bar, 232/52 px rail, 26 px status bar, unchanged. New surfaces are
composed from the existing 26 shared controls; colour, spacing, control size and type literals live
in `ui/theme.slint` only.

One clause is new and has the same standing: **a surface a pointer can open must be a surface the
keyboard can enter, Escape can close, and closing returns focus to whatever opened it.** Round
twelve turns that from a convention into rules G47, G49 and G53.

## 4 · Track A — hit testing, focus and state (`ui/**`)

| ID | Evidence | Symptom | Sev |
| --- | --- | --- | --- |
| A1 | `ui/pages/downloads.slint:147`; the four `IconButton`s at `:115-144`; `ta.has-hover` read at `:69` | `ta := TouchArea {}` is declared *after* the buttons. Slint hit-tests in reverse declaration order and a `TouchArea` grabs the mouse on `MouseEvent::Pressed` whether or not it has a handler, so the local-record row's open / reveal / re-download / forget controls are dead to the pointer (the keyboard still reaches them) | P1 |
| A2 | `ui/pages/novel-reader.slint:125-131` vs the correct form at `ui/pages/detail.slint:96-102` | `moved =>` has no `pressed` guard: sweeping the pointer down the reader settings panel rewrites font size, line height, letter spacing, paragraph spacing, indent, margins and measure — seven persisted settings, without a click | P1 |
| A3 | `ui/main.slint:1442-1446` (y = H−44−16) vs `gallery.slint:627-631`, `home.slint:527-530`, `profile.slint:428-431` (y = H−48−12) | the notice bar `[H−60,H−16]` and the batch bar `[H−60,H−12]` overlap by 44 px; the notice takes no input except its close button, so the notice a batch download raises covers select-all / download / bookmark / clear / done and every click aimed at them lands on the invisible batch bar | P1 |
| A4 | `ui/components.slint:1775-1799` vs `:1830-1847` | the rank badge and the selection check share `x: Theme.space-2; y: Theme.space-2`, and the check is declared later, so selection mode hides `#1…#N` on the ranking page | P2 |
| A5 | `ui/components.slint:1528-1540,1568`; keys at `:1663-1679` | `BookmarkPill` declares `accessible-action-default` and owns no `FocusScope`; the card's keys are Enter/Space and `a` only, so no grid can be bookmarked from the keyboard | P2 |
| A6 | `ui/theme.slint:213-217` vs `ui/pages/novel-reader.slint:287,329` | the four `reader-*` bounds are read nowhere; the sliders compute `Math.round(14 + …)` and `Math.round(420 + …)` instead, so a bound has two sources and the model test cannot see the UI one | P2 |
| A7 | `ui/components.slint:1200-1209` | the indeterminate branch is a static 30 % block: no `animate`, no offset, so every running download looks stuck | P2 |
| A9 | `ui/pages/detail.slint:1254-1289`, `ui/main.slint:716-729`, `src/app/callbacks/shell.rs:57-63` | the bookmark-visibility menu can be closed only by its scrim or by its own focus scope; once focus is on the top bar, Escape reaches the shell and means "go back", which loses zoom and scroll and leaves `restrict-open` true so the menu re-opens on the way back | P2 |
| A10 | `ui/pages/novels.slint:250-258` vs `:309-355` | the 120 px loading block (`state == 1`) and the result cards (`rows.length > 0`) are not exclusive: every novel search pushes the old rows down 120 px and pulls them back | P3 |
| A11 | `ui/pages/home.slint:160,391` vs `:418-419` | the skeleton is 130×150 and the real card 140×(150+44): the latest strip jumps 10 px across and 44 px down when it resolves | P3 |
| A12 | `ui/pages/gallery.slint:271-283` | a 32 px button is wrapped in a `height: 0px` Rectangle and pushed out with `y: -Theme.space-1`, so its hit area overflows the header row by 8 px down and 4 px up; clicking the right end of the subtitle clicks 多选 | P3 |

Fixes. A1 declares `ta` before the row content (hover still comes from `ta.has-hover`; a capture has
to show the icon-area hover trade is acceptable). A2 adds `if (track.pressed)`. A3 subtracts
`(root.selection-mode ? Theme.batch-bar-height + Theme.space-2 : 0px)` from the notice's `y`. A4
anchors the check bottom-left (bottom-right is `BookmarkPill`, top-right the type badge). A5 gives
`IllustCard` a `b` key rather than `BookmarkPill` a Tab stop. A6 interpolates from `Theme.reader-*`.
A7 adds an `animate x` driven by `Theme.motion`. A9 puts `restrict-open` in the shell's escape
table, ordered overlay → selection → back. A10 makes the loading branch exclusive. A11 shares one
width property and adds the 44 px caption band to the skeleton. A12 deletes the zero-height wrapper
in favour of `compact` + `Theme.control-height-sm`.

## 5 · Track B — where a result lands, and what a render costs (`src/app/**`)

| ID | Evidence | Symptom | Sev |
| --- | --- | --- | --- |
| B1 | `src/app/detail.rs:113-121` vs `fail_detail` at `:165` | the success path compares `detail_epoch` first; the failure path writes `detail-state = 3` with no epoch at all, so a slow failed load of A replaces the already-loaded B with A's error | P1 |
| B2 | `src/app/profile.rs:518-528,263` | the follow reply is applied to whatever `profile` is current (no `state.user == target` check, no busy guard) and `publish_profile` then rewrites `profile-followed`, so A's result flips B's button; `profile-busy` is written and never read | P1 |
| B3 | `src/app/feed/batch.rs:307-329` + `src/app/detail.rs:643,649-668` | every batch result calls `resync_bookmarks`, which rebuilds a `HashMap` over all `items` and clones every row of every column: 50 works over 500 rows is ≈25 000 row clones and 50 000 `String`s on the UI thread in one reply | P1 |
| B4 | `src/app/detail.rs:701-708` | `toggle_follow`'s `on_ui` compares neither `detail_epoch` nor the route (the animation path at `:1064` guards both), so A's follow reply rewrites B's button | P2 |
| B5 | `src/app/detail.rs:1085-1148` | `pick_restrict` bypasses `toggle_allowed` and `detail-busy` entirely, so the menu and the star can both be in flight and the later reply wins invisibly | P2 |
| B6 | `src/app/mod.rs:583-588`, `src/app/feed/home.rs:87-88` | the History destination sets `reached_end = true` and `reuse_feed` restores scroll without re-reading the store, so the work just viewed is missing from the history list until the page is left and re-entered | P2 |
| B7 | `src/app/downloads.rs:337-339,353-368` | a change in the done count synchronously runs `store.downloads()`, `Path::exists()` per record and `Rc::new(VecModel::from(..))`: dozens of `stat` calls on the UI thread and a full rebuild that drops every row's hover and focus, once per completed page | P2 |
| B8 | `src/app/novel.rs:928-935,975` | `next_page` has no in-flight guard and no rollback, and the landing path replaces `rows` with the page that arrived while `summaries` accumulates: page 1 disappears, two clicks race | P2 |
| B9 | `src/app/novel.rs:1719-1747` | every embedded image that arrives walks `0..blocks.row_count()` cloning each row and linearly searching `images`: 800 blocks is 800 clones plus 800 comparisons per image | P2 |
| B10 | `src/app/feed/selection.rs:46-51,60-77` | every click publishes the whole selection: `row_data` per row, `id.parse::<u64>()` per row, `contains` per row; selecting one card in 500 rows is 500 clones, and ctrl+A plus one more click is ≈250 000 comparisons | P2 |
| B11 | `src/app/feed/mod.rs:77-78,377-385` | every `render_feed` builds a 512-entry `HashSet` in `prune_covers` and a `Vec<(String,String)>` in `spawn_covers` before `loader.plan` skips all of it: ≈1 500 `String`s per batch, dropped immediately | P2 |
| B12 | `src/app/callbacks/downloads.rs:132-138,99-109` | an "open file" click finds its row through `tasks()`, which deep-clones the whole queue: 200 pages cost 200 `DownloadTask` clones to read one | P3 |

Fixes. B1 `fail_detail(epoch)` compares first. B2 checks `state.user` and `profile_busy`. B3 calls
`resync_bookmarks` once at the end of the batch, or patches by `index`. B4 shares one guard for
success and failure. B5 reuses `detail-busy`. B6 keeps the scroll and re-reads the store. B7 patches
in place and moves `exists()` into `rt.spawn`. B8 derives rows from `summaries` behind a
`loading_more` slot. B9 builds a `url → row` index. B10 writes the two rows that changed and uses a
`HashSet`. B11 reuses one id slice for both prune and plan. B12 adds `task(id)`.

## 6 · Track C — contracts and resource bounds (`crates/**`)

| ID | Evidence | Symptom | Sev |
| --- | --- | --- | --- |
| C1 | `crates/pixiv-types/src/domain.rs:383-387`, `:1745`; `ui/pages/detail.slint:494-495` | `from_detail` copies `description`/`illustComment` verbatim into `Illust.description`; the module's own `clean_caption` is called on the novel path only, and the detail page binds the string straight into a `Text`. Every work whose caption has a line break shows `<br />`, and a linked one shows `<a href="…">` | **P1** |
| C2 | `crates/pixiv-types/src/wire.rs:263-264,649-650` → `domain.rs:273-293,369-408` | `ai_type` reaches the wire and not `Illust`; `SearchItemWire` has no `ai_type` at all, though every row of `fixtures/search_artworks.json` carries `"aiType":1` | P2 |
| C3 | `crates/pixiv-types/src/domain.rs:404-405`; no reader in `src/` | `is_masked` and `sl` are transported and read by nobody: a work Pixiv marks sensitive is shown unmasked, and the detail page hands out the `original` URL | P2 |
| C4 | `crates/pixiv-types/src/domain.rs:426-458` | a ranking row carries its own `date` (`fixtures/ranking_daily.json`: `"2026年11月09日 00:00"`) and `illust_upload_timestamp`; `from_ranking` reads neither and never sets `create_date` (`from_search` sets it at `:439`), so the ranking is the one surface with no dates on any card | P3 |
| C5 | `crates/pixiv-types/src/wire.rs:1153-1202` → `domain.rs:1517-1539` | `NovelSummary` drops `genre`, `language` and `bookmark_count` while the illustration row keeps its counterpart, so the two lists of one search surface have different information density | P3 |
| C6 | `crates/pixiv-api/src/lib.rs:446-454`, `crates/pixiv-image/src/lib.rs:363-369` | the body reader reserves `with_capacity(min(declared, limit))` up front: the limit bounds the total and not the reservation, so a declared 64 MiB is 64 MiB allocated before the first byte arrives, times six image permits | P2 |
| C7 | `crates/pixiv-store/src/lib.rs:100-122,429-433,473-477`, `novel.rs:149-160,261` | the four history tables have a "clear everything" and no retention clause and no cap constant: view, search and reading history grow without bound and `stats()` grows with them | P2 |
| C8 | `crates/pixiv-novel/src/parse.rs:768-779` | entity decoding rejects U+0000 only; the rest of the C0 range and U+202A..U+202E pass through, so one `&#x202e;` reverses the rest of a caption and `&#x1b;` reaches a Slint text model | P3 |

Fixes. C1 routes `from_detail` and `from_search` through `clean_caption` **and adds a fixture that
carries markup** (`fixtures/illust_detail_html.json`); without the fixture the gate can never see the
defect. C2–C5 fill the fields §2 froze. C6 clamps the reservation to 1 MiB and grows in chunks. C7
adds a 500-row retention clause and `DELETE … WHERE seq <= MAX(seq)-?1`. C8 filters `is_control()`
and the bidi overrides as well.

## 7 · Track D — features whose data already exists

| ID | Data that already exists | Surface | Acceptance |
| --- | --- | --- | --- |
| F1 | `domain.rs:772-878`'s `SortOrder`/`SearchTarget` (`ALL`, `labels()`, `as_param()`; `endpoint.rs:78-81` has serialised `order=`/`s_mode=` since round five); `SearchBodyWire.related_tags` | two `SegmentedControl`s in the search/bookmarks header — 最新/最早/热门 and 标签精确/标签部分/标题与说明 — plus a related-tag chip row | the URL carries `order`/`s_mode`; changing either resets the cursor and reloads page 0; an empty related-tag list draws no row; ten assertions, including "every index a control can select has a parameter the URL can carry" |
| F2 | C3's `is_masked`/`sl` | the detail page masks first (the same 显示 affordance the web has), with a sensitivity badge | a masked page does not request the original; 显示 is what does; the capture matrix gains a masked state |
| F3 | `is_multi_page` (`domain.rs:307`, called by tests only today); C4's row `date` | the card's 共 N 张 badge uses the predicate; the ranking card shows its upload date, and an empty date draws no separator | every row of the 50-row fixture has a `create_date`; `is_multi_page` gains a non-test caller |
| F4 | C2's `ai_type` | an AI 生成 badge on the card and the detail page; an 排除 AI switch on the search surface that affects the next request only | the fixture's `aiType:1` work carries the badge; with the switch on the first row has none |
| F5 | C5's three fields | the novel result row shows its language and bookmark count, at the illustration row's density | the row data carries `language` and `bookmarks`; a missing value draws — rather than 0 |

## 8 · Track E — gates and documentation (`tools/**`, `docs/**`, `README.md`)

| ID | Evidence | Symptom | Sev |
| --- | --- | --- | --- |
| E1 | `tools/verify-layout.ps1:37,67,70-73` | the child's exit code is discarded and the output directory is never cleared, so a failed capture is judged on the previous run's sidecar | P1 |
| E2 | `tools/verify-icons.ps1:38,41` | the same two mistakes: the glyph gate measures last round's PNG and reports OK | P1 |
| E3 | `tools/verify-a11y.ps1:346,480` | only "nothing was measured" exits 77; ten skips and one pass exits 0 and prints the budget sentence | P1 |
| E4 | `tools/verify-a11y.ps1:336,497-542`, `walk-lib.ps1:88` | `walkEnd` is printed and never asserted, while `verify-focus.ps1:458` fails on the same condition: two gates disagree about one fact | P1 |
| E5 | `tools/gates.ps1:318,353` vs its own header at `:57` | four of six cargo invocations carry `--locked`; `cargo build` and `--release` do not | P2 |
| E6 | `tools/page-set.ps1:109-119` | `detail` sits in `PageUncaptured` on a premise the `visual` set and the tracked baselines contradict | P2 |
| E7 | `README.md:259` vs `tools/verify-focus.ps1:91`, `gates.ps1:332` | the documentation sends a reader to `docs/screenshots/baseline/focus-*`, which nothing writes; the real output is `target/focus/baseline` and the `.gitignore` line is dead | P2 |
| E8 | `tools/selftest-gates.ps1:541-553`, `README.md:264` | the prose-number rule knows captures/checks/tests/rules/pages and not points, while README says 1060 points and `verify-dpi.ps1:177` prints the number every run | P2 |
| E9 | `tools/selftest-gates.ps1:571` | the rule reads README only; `docs/` carries sixty restatements of gate output with no check | P3 |
| E10 | `README.md:415`, `tools/fetch_icons.ps1` | the only script that can rebuild the icon geometry is referenced by nothing | P3 |
| E11 | `tools/shot.ps1:56` vs `tools/page-set.ps1:190-194` | only `START_ILLUST` is cleared, so a leftover `START_NOVEL` makes `-Page home` photograph the reader | P2 |
| E12 | `tools/verify-dpi.ps1:306` | `-NumericOnly` always exits 77: the one half that needs no desktop, and the only half CI could run, can never be green | P2 |

Fixes. E1/E2 clear their output directory first and then check `$LASTEXITCODE` (the shape
`verify-visual.ps1:282` already uses). E3 asserts `results.Count -eq Pages.Count` and fails the
difference. E4 asserts `walkEnd -eq "cycle"` and defaults a replay without the field to `"cap"`.
E5 adds `--locked` and stops restating the count in the header. E6 splits `PageUncaptured` into
`PageUnbaselined` and `PageUnmeasured` so `detail` joins the accessibility and layout sets. E7 moves
the documentation to `target/focus/baseline`. E8 adds `points?/sizes?` to the prose rule. E9 extends
it to `ARCHITECTURE.md` and `NOVEL-READER.md` (the R7–R11 archive stays exempt). E10 gives
`fetch_icons.ps1` a `-Check` mode that compares without writing, and runs it from `gates.ps1`. E11
routes `shot.ps1` through `page-set.ps1`'s full key sweep. E12 exits 0 for `-NumericOnly` with no
failure. Also: UI-NOTES gains a back-filled round eleven and a round twelve. The 88 zero-reference
`tools/_*` scratch scripts went too, but as a working-tree cleanup rather than a commit: `.gitignore:25`
(`tools/_*`) means git has never held one of them (`git log --all -- tools/_probe1.ps1` is empty), so
there is no deletion a commit could describe. They were proved unreferenced by a word-boundary sweep
over 362 text files (88 candidates, 88 zero-reference, 0 kept, with a positive control that hits and a
negative control that does not), then moved to `target/r12/removed-probes/` — reversible, and out of
the way of `tools/`'s eighteen tracked scripts. Restore with
`Move-Item target\r12\removed-probes\* tools\`.

## 9 · Gates this round owes (a gate lands with its fix in the same commit)

| ID | Rule | Today | Negative experiment |
| --- | --- | --- | --- |
| G45 | a `TouchArea` declared after an interactive sibling is a hit-test defect | absent | move `downloads.slint`'s `ta` back to the end of the row |
| G46 | a `moved =>` handler that assigns geometry or a value must guard on `pressed` | absent | delete `detail.slint:97`'s `if (ta.pressed && …)` — both sites go red |
| G47 | the notice bar's and the batch bar's `y` ranges must not intersect in `selection-mode` | absent | drop A3's subtraction |
| G48 | two absolute overlays in one component must not share an `(x,y)` anchor expression | absent | move the selection check back to the top-left |
| G49 | a component declaring `accessible-action-default` owns a `FocusScope` or carries a reasoned exemption (`TextField`, `MenuRow`, `PaletteRowView`, `SuggestRow`, `BookmarkPill`) | partial | delete `ListenRow`'s `FocusScope` |
| G50 | every `out property` of `Theme` has a reader under `ui/**` (the motion-token rule, widened to length tokens) | absent | put `14 + …` back into `novel-reader.slint` and the rule names `reader-font-min` |
| G51 | an indeterminate progress bar must animate from `Theme.motion` or loop | absent | delete A7's `animate` |
| G52 | every `in property`/`callback` of a page component is bound in `main.slint` or listed as exempt | absent | today `GalleryPage` names eleven (red before F1, green after) |
| G53 | an `on_ui` closure in `detail.rs` that writes `set_detail_*` also names `detail_epoch` (`profile.rs`/`novel.rs` in the same shape) | absent | delete B1's comparison and the rule names `fail_detail` |
| G54 | user-visible text carries no markup: the `description` of `fixtures/illust_detail_html.json` survives `clean_caption` without `<br` or `<a ` | absent, and red by construction until the fixture lands | restore `w.description` under C1 |
| G55 | batch and selection publishes are O(changed), not O(rows): a counting `VecModel` bounds `set_row_data` | absent | restore B3/B10's full write |
| G56 | the page set agrees with itself: `(Get-PageSet -For visual) ∩ PageUncaptured` is empty and all five consumers match (widening G32) | half | delete `settings` from `verify-a11y.ps1` only |
| G57 | no gate discards a child's exit code (`& pwsh … | Out-Null` must be followed by `$LASTEXITCODE`) | absent | run both verifiers with `-Exe nope.exe`: exit 1 and CAPTURE |
| G58 | a skip is not a pass: `results.Count == Pages.Count` and `walkEnd == "cycle"` | absent | `-Pages home,settings` with only home measurable: exit 0 today, non-zero after |
| G59 | `shot.ps1`'s `PIXIV_DESKTOP_*` key set equals `page-set.ps1`'s | absent | set `START_NOVEL` and capture `-Page home` |
| G60 | the `-NumericOnly` DPI gate can be green and can be red (raise `get_TargetWidth`'s clamp by one) | always 77 | the same experiment |

`tools/selftest-gates.ps1` grows one case per new gate, and each gate goes red before it goes green
inside one commit.

## 10 · Execution: goal, subagents, commits

**Phase 0 (Lead).** `create_goal` with §0 as its objective; this document; `todo_write` with one item
per track and its run count.

**Waves.** Wave one runs A, B and E in parallel on disjoint files. Wave two runs C, then D against the
contract §2 froze. Phase 3 is the Lead's alone: `-Twice`, `page-set`, new baselines, `verify-a11y
-Update`, `gates.ps1 -Ui`.

| Track | Owns | Delivers |
| --- | --- | --- |
| A · input | `ui/{main,components,theme}.slint`, `ui/pages/{downloads,novel-reader,gallery,home,profile,novels,detail}.slint`, `src/uiaudit.rs` | A1–A12; G45–G52 in the same commits |
| B · landing | `src/app/**`, `src/{layout,images,zoom}.rs` | B1–B12; one red-then-green test per P1 row, one counted-model assertion per ⚡ row |
| C · crates | `crates/pixiv-{types,api,store,novel,image}/src/**`, `fixtures/**` | C1–C8; G54 with its fixture |
| D · features | `src/app/{feed,downloads,novel,detail,palette}.rs`, `src/app/callbacks/{search,feed,detail}.rs`, `ui/pages/{gallery,detail,novels,home}.slint` | F1–F5; G52 turning green |
| E · gates | `tools/**`, `.github/workflows/**`, `README.md`, `docs/UI-NOTES.md` | E1–E12; G45–G60's negative experiments |

Each subagent inherits this session's route (no model allowlist is recorded, so an explicit pair is
refused), works only inside its boundary, gets one first attempt and at most two re-dispatches per
item, and reports a status with evidence. The Lead edits nothing inside an active boundary and
accepts only after reading the diff and running the tests.

**Commits.** One item per commit, each compiling and passing its own tests. The house style is
`type(scope): sentence` — `fix(ui): the row that ate its own buttons`, `fix(app): the reply that
lands on the wrong page`, `fix(crates): a description is markup, not prose`,
`feat(search): the sort control the vocabulary has been waiting for`,
`perf(app): one patch per change, not one per row`,
`test(gates): five gates that can fail, and the plants that prove it`,
`test(visual): the states this round moved, re-baselined after -Twice`.

**Phase 5.** `gates.ps1 -Ui` ends on `0 failed · 0 skipped`, UI-NOTES is written, the goal is
completed, and the delivery summary lists the risks that were noticed and where they were routed.

## 11 · Not this round

* **HTTP Range resume** (README limit 12): it shares the write path C6 touches and the pause/resume
  state of the queue; two risky changes in one file in one round is how a queue regresses.
* **The novel table of contents** and **the card context menu**: the same class as A3 and A9, so the
  overlay rules (G47, G49, G53) are made able to fail first and the surfaces come next round.
* **A real screen reader**, the Linux and macOS input paths, WebP animation export and a
  pointer-driven drag gesture: not measurable here, so they stay in UI-NOTES § "still not verified".
* **T6, all of it** (SQLite on the UI thread): B7 moves the one path it measured into `rt.spawn` and
  the rest waits for B7's numbers.
* **Deleting `tools/_*.ps1`**: one commit in track E, and only after a `Select-String` proof that
  nothing references them.

## 12 · Assumptions

1. The keyboard and hit-testing class outranks the features: A1, A2 and A3 are the only defects this
   round where a click produces a different action than the one aimed at.
2. The features read fields the crates already carry (C2–C5). No new endpoint, no new dependency.
3. The UI gates run on this machine's interactive desktop only; CI keeps omitting `-Ui`, and E12 is
   the one half CI could own.
4. Whether the thirty tracked baselines are green today is not assumed: `-Twice` measures
   reproducibility first and `-Update` comes after.
5. `docs/` stays English, as rounds seven through eleven did.
6. If a track slips, the drop order is F4, then F5, then C8, then A12; A1, A2, A3, B1, C1 and E1–E4
   do not drop.
