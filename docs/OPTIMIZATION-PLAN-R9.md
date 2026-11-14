# Optimization plan — round nine

Written against master @ 63e842d (round eight, 99 commits), on the machine that produced every
capture in docs/screenshots/. This is a *plan*. docs/UI-NOTES.md stays the record of what has been
fixed; docs/OPTIMIZATION-PLAN-R8.md is left exactly as it was written, including the phase-two list
this round finally picks up.

Round eight closed the gates that could not fail and the declarations that drew the wrong thing. It
also, in the same commit, left one line of test code that `clippy::useless_vec` rejects — so at HEAD
the lint gate fails before it lints anything, and the six measured UI gates never ran. That is where
round nine starts: one line, then the five features the shell has been saying it does not have, and
the three optimisations that only show up under load.

---

## 1. Baseline measured today, not remembered

Every row was produced in this session at 63e842d with a clean worktree.

| Measurement | Result | How |
| --- | --- | --- |
| `cargo test --workspace --all-targets -- --list` | **327** cases across 10 binaries | run this session |
| `cargo test --workspace` | every binary green | run this session |
| `cargo check` via clippy-driver, `RUSTFLAGS=-D warnings` | **FAIL** — `src/app/feed/mod.rs:848:23: error: useless use of \`vec!\`` | run this session |
| `tools/gates.ps1` | 4 gates, **1 failed**, the six UI gates never reached | run this session |
| `tools/gates.ps1 -Ui -SkipBuild -SkipTest -Lint Off` | 10 gates, 0 failed, **544.8 s** | run this session |
| `verify-a11y` | 8 of 8 pages meet the budget; **four pages walk fewer stops than they expose** | same run |
| `verify-visual` | 20 of 20 OK; settings light 0.0028 % / 145 px, dark 0.0025 % / 125 px of jitter | same run |
| `verify-focus` | 3 pages x **10** stops, 27 rings, 3 accent rules, 0 invisible; back/forward RING-DIM (a=0.4) | same run |
| `verify-dpi` | 4 of 4 scales measured live, 20 numeric rows, 1060 sweep points, 0 below draw size | same run |
| `verify-icons` | 11 glyph centres within 2.5 px; rail glyph column spread 1.5 px | same run |
| `verify-layout` | 24 checks, 0 not OK | same run |
| `selftest-gates` | 8 cases, 0 failed | same run |
| Tracked tree | **212** files, **12 100 122 B**; 27 PNGs are 9 810 799 B = **81.1 %** | `git ls-files` + sizes |

Accessibility, per page — focusable / distinct Tab stops:

| home | discovery | ranking | search | bookmarks | history | downloads | settings |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 45 / 51 | 36 / 38 | 44 / 71 | **22 / 21** | **31 / 24** | **15 / 14** | **19 / 18** | 21 / 26 |

Three facts read out of the code, not measured from a run, because they need a real account:

1. Opening a work plans a `regular` decode for **every page** of it (`src/app/detail.rs:249-283`), and
   the 48 px thumbnail strip reuses that same full-size bitmap (`detail.rs:302-303`). At 1200x1600x4
   that is ~7.7 MB a page: thirty pages is ~230 MB of RGBA for one screen of artwork.
2. Once the disk cache is over budget, **every put** walks the entire cache tree with a `metadata()`
   per file (`crates/pixiv-image/src/disk.rs:137-138 -> 211-212 -> 260-273`).
3. The download queue has no pause state (`crates/pixiv-download/src/lib.rs:78-86`) and its idle loop
   is a 20 ms poll (`worker.rs:27, 393`).

Already correct, and named here so nobody re-fixes them: search suggestions are debounced 280 ms with
a generation token that drops stale replies (`src/app/feed/search.rs:101-186`); the atomic write
fsyncs; the disk temp name is per-writer; `suggest_move` is bounded and wraps.

---

## 2. Findings

### 2.1 The gate that stopped every other gate

**P0 · HEAD cannot pass its own lint gate.** `src/app/feed/mod.rs:848` builds `vec![100.0_f32; 4]`
and then only asks it for `len()`. Under `-D warnings` `clippy::useless_vec` is an error, so
`cargo check --workspace --all-targets` dies on the test target, `gates.ps1` reports one FAIL, and
because the UI section runs after the cargo section, a11y/visual/focus/dpi/icons/layout were skipped
for the whole run. CI's `cargo clippy -D warnings` would have caught it; nothing local did, because
the lint step is the one that aborts the suite.

Fixed before this document was committed (`59582d3`, one line). The lesson is a gate hole, not a
style note: **a suite that stops at the first failing gate reports four gates instead of ten** —
G25 closes that on CI, where the failure must be visible per job.

### 2.2 UI/UX defects, measured

| ID | Symptom | Evidence at HEAD | Why no gate saw it |
| --- | --- | --- | --- |
| **U5** | The detail page has no vertical scroll view: in a short window the InfoPanel (`min-height: 160px`, `vertical-stretch: 2`) is squeezed and description, tags and stats are unreachable | `ui/pages/detail.slint:559` is a bare `VerticalLayout`; the only `ScrollView` in the file is the 68 px thumbnail strip at :810 | captures are 1280x800; the layout gate measures the shell invariant, not page content height |
| **U6** | The suggestion panel is `height: min(392px, ...)` with `clip: true`, so past nine rows the list is drawn clipped and cannot be scrolled | `ui/main.slint:1169-1214` | no capture, no assertion |
| **U7** | Tag chips cannot be reached by keyboard at all: `TagChip` declares a role and a default action but no `FocusScope` | `ui/components.slint:863-871`, versus `IllustCard` and `UserCard`, which both have one | the budget counts focusable elements, not *which* ones a Tab walk reaches |
| **U3** | Focus does not follow: Tab past the fold leaves the focused card off screen | `git grep scroll-into-view` has no hit in the tree | the pixel gate photographs a static frame; the focus gate reads rects, not the viewport |
| **U11** | `UserCard` never draws "followed" or "premium"; the fields exist and nothing reads them | `RecommendUserItem.followed/premium` at `components.slint:1866-1867` are the only hits in the file | no assertion |
| **U12** | The detail page's metadata block sits 8 px off its row (round eight bisected three candidates and did not land it) | round eight §2.2 U12 | needs a measurement, not a reading |
| **U28** | In compact rail mode the active-download badge disappears entirely, leaving only a tooltip | `components.slint:811` is `if !root.compact && root.badge > 0` | no assertion |
| **U29** | Switching pages in the viewer needs a mouse: the strip's thumbs are bare `TouchArea`s with no `FocusScope`, and no key handler steps pages | `ui/pages/detail.slint:846-849` | no assertion |
| **G21** | Four pages walk fewer stops than they expose (bookmarks 31 focusable -> 24 stops, search 22 -> 21, history 15 -> 14, downloads 19 -> 18) and the run still prints `cycle=yes`, because the walk's stop identity is a **(Name, Type)** pair. **Corrected while fixing it:** replaying the pair-only rule against a recorded walk reproduces every count exactly (51/38/71/21/24/14/18/26), so a duplicate name is *not* what ends these walks — the gate simply never compared "focusable" with "reached". What it cannot see, and what the new coverage assertion does see, is three controls per page that are focusable but never reached at all: a second same-named search `Edit` (1242x48, outside the ring), the account `Button` "未登录，打开设置以登录", and settings' `Edit` "粘贴 PHPSESSID 的值" | `tools/verify-a11y.ps1:161-164` for the identity rule; the coverage gap is measured by the new assertion, not by the old one | the gate had no notion of "reached" |
| **G22** | The focus-ring gate still samples ten stops per page: 30 of the 51-71 stops a page has, i.e. the ring is measured on roughly 8 % of them | `tools/verify-focus.ps1` walk | round eight's G5 fixed the a11y walk, not this one |
| **G23** | Both settings captures are nondeterministic because the cache figure is a live number; they pass only because the diff budget is a percentage | 0.0028 % / 145 px measured today | there is no fixture for that line |

### 2.3 Runtime: what a real account would feel

**O1 · P1 · One screen of artwork decodes every page of the work.** `install_detail` maps *all* page
URLs at the `regular` tier through `buffer_unordered(3)`, then hands each decoded bitmap to both the
viewer row and the 48 px thumbnail (`detail.rs:249-283`, `:302-303`). A thirty-page work therefore
holds ~230 MB of RGBA to show one page and thirty postage stamps, and the strip re-asks the cache for
a 48 px image it already has at 1200 px.

**O2 · P1 · The disk cache rescans the tree on every write once it is over budget.** `put` calls
`enforce_budget` whenever the counter exceeds the budget (`disk.rs:137-138`), and `enforce_budget`
starts with a recursive `walk` that `metadata()`s every file (`disk.rs:211-212`, `:260-273`). With
5000 cached entries and six concurrent covers that is ~30 000 stat calls per batch, on the blocking
pool, forever after the cache fills.

**O3 · P2 · The idle download worker wakes 50 times a second.** `IDLE_POLL = 20 ms`
(`worker.rs:27`, `:393`) with nothing to do.

### 2.4 The features the shell says it does not have

1. **Ugoira play in the viewer.** README limit 6: export to GIF only. `UgoiraFilm` already carries
   per-frame RGBA and delays (`crates/pixiv-ugoira/src/lib.rs:82-95`), so playback is a publishing
   problem, not a decoding one.
2. **No multi-select.** Bulk-downloading thirty works means opening thirty detail pages.
3. **No entry point for an id or a link.** The only way in is to paste an id into search.
4. **Bookmarks are public-only from the app.** `toggle_bookmark` adds with
   `BookmarkRestrict::Public` (`src/app/detail.rs:455-460`); a private bookmark can be kept but not
   created.
5. **The queue cannot pause and says nothing about throughput.** Cancel is the only control.

### 2.5 Two smaller gate holes worth closing in the same round

* **G24 · `PIXIV_DESKTOP_FIXTURE` exists only in plans.** `git grep` finds it in three `docs/` files
  and nowhere in `src/` or `tools/`, which is why U13 was deferred three rounds and why G23 cannot be
  fixed: there is no way to ask the app for a deterministic state.
* **G25 · CI runs no gate script.** `.github/workflows/ci.yml` runs fmt, actionlint, check, clippy and
  test. `selftest-gates.ps1` — the negative experiment every gate change is accepted on — runs
  nowhere but this machine.

---

## 3. The style contract — unchanged, and it still binds

The visual target stays **70 % WinUI 3 / Fluent 2 + 30 % Pixiv web**, as declared at the top of
ui/theme.slint. No phase may:

* add a colour, radius, spacing or font literal outside ui/theme.slint;
* introduce a shadow, glow, gradient or blur the theme has no token for (the command palette uses
  `layer` + `border-strong` + `shadow-2`; it does not fake acrylic);
* lengthen a motion past `Theme.motion`, or give one a bounce;
* change a control metric: 32 px controls, 4 px control radius, 8 px card radius, 48 px top bar,
  232/52 px rail, 26 px status bar;
* touch the accent's division of labour (accent decorates, accent-fill carries ink, accent-ink is text
  on the accent wash, on-accent is ink *on* accent-fill);
* trade a measured number for a looked-at one.

Two patterns are new and both must be traceable to the idiom the shell already claims: the command
palette is Windows Terminal's (layer + shadow + 36 px rows + a key hint footer), and the bookmark
control is Fluent's SplitButton + MenuFlyout (32 px rows, 8 px radius, Up/Down/Escape). Multi-select
uses WinUI's list-item selection: a 22 px check in the tile's top-left plus a 2 px accent ring —
not a badge of our own invention.

---

## 4. Interface contract — frozen in phase 0, before anything is filled

Parallel safety rests on this table. Phase 0 lands both halves (declaration with defaults, no-op
handler) so the tree compiles at every commit; afterwards a track may only fill its own files.

| Where | What |
| --- | --- |
| `IllustItem` | `selected: bool`, `selectable: bool` |
| `IllustCard` | `in-out selected`, `callback clicked-with(bool ctrl, bool shift)` |
| grid page roots (home/gallery/profile) | `in-out selection-mode`, `in selected-count`, callbacks `select-all()`, `clear-selection()`, `batch-download()`, `batch-bookmark()` |
| `main.slint` | `struct PaletteRow { label, detail, shortcut, kind }`; `in-out palette-open`, `in palette-rows`, `in-out palette-active`, callbacks `palette-query(string)`, `palette-choose(int)`, `palette-close()` |
| `detail.slint` | `in film-ready`, `in film-frame`, `in film-index`, `in film-count`, `callback toggle-playback()`, `callback page-step(int)` |
| `detail.slint` | `in bookmark-restrict` (0 public / 1 private), `callback pick-restrict(int)` |
| `downloads.slint` | `in queue-paused`, `in queue-done`, `in queue-total`, `in throughput`, callbacks `pause-task(int)`, `resume-task(int)`, `toggle-pause-all()` |
| `src/app/mod.rs` | fields `selection`, `palette`, `playback`; shared `publish_*` helpers only |
| `crates/pixiv-ugoira` | `total_duration_ms()`, `frame_index_at(ms)`, `scaled_to_width(w)` |
| `crates/pixiv-download` | `TaskState::Paused`, `pause/resume/pause_all/resume_all`, `eta_seconds()`, `throughput()` |
| `crates/pixiv-image` | no public change; `DiskTier` gains an index |
| `src/app/palette.rs` (new) | `parse_target(&str)`, `palette_open/close/query/move/choose` |
| `src/app/ugoira.rs` (new) | `start_playback/stop_playback/toggle_playback/playback_tick` |

---

## 5. Features

### F1 · Ugoira playback in the viewer

Frames are already decoded with their delays. Add three pure functions to `pixiv-ugoira`
(`total_duration_ms`, `frame_index_at` by cumulative delay, `scaled_to_width` by area average), a
64 MB publish budget that thins frames by stride when exceeded and says so in the UI, and a 16 ms
`slint::Timer` that computes the frame from a monotonic clock rather than accumulating ticks.
Playback stops on page change, route change and window close. The toolbar gains play/pause and a
"frame 12/24" caption; `ugoira-hint` stops saying playback is missing. Demo mode gains a deterministic
24-frame film so the state can be captured offline.

Failure modes: 404, `FrameMismatch`, over budget — each keeps the still and explains itself in an
InfoBar. Tests: boundary table for `frame_index_at` (zero, exact boundary, wrap, single frame),
`scaled_to_width` dimensions and byte count, the thinning map, and that two demo films are byte-identical.

### F2 · Multi-select and batch actions

`Selection { active, ids, anchor }` keeps ids in load order so it survives append; a reload clears it.
Plain click still opens; in selection mode a plain click toggles; Ctrl+click toggles and enters the
mode; Shift+click selects the range from the anchor in `items` order (not column order); Ctrl+A
selects what is loaded; Escape clears and exits. Whether Windows delivers `Ctrl`/`Shift` inside
`TouchArea.pointer-event` is settled by one experiment in phase 1; if it does not, the fallback is
Fluent's explicit selection-mode button plus plain clicks, and the document says so instead of
pretending modifiers work.

Batch download fetches `illust_pages` per work (bounded by three) and enqueues through the existing
manager, dedupes with the existing skip rule, caps at fifty works, and reports one summary line.
Batch bookmark is sequential `set_bookmark` with the round's chosen restrict and never flips local
state without the server's verdict.

### F3 · Command palette (`Ctrl+P`) and direct jump

One entry point for three jobs: go to a page, run an action, open an id or a link. The parser accepts
`https://www.pixiv.net/artworks/1234`, `/en/artworks/1234`, `pixiv.net/artworks/1234`, `id=1234`,
`1234` (work) and `/users/5678`, `users/5678` (artist); anything else becomes "search this". Rows
come from eight pages, five actions and five recent views; ranking is prefix, then subsequence, then
fuzzy, capped at eight rows. `Ctrl+K` keeps focusing search. Escape closes and returns focus to the
search field.

Tests: a twelve-case parse table (trailing slash, query string, case, doubled slash), ranking order,
and empty-query ordering.

### F4 · Bookmark visibility

The bookmark control becomes a SplitButton: main action plus a 24 px chevron opening a MenuFlyout of
two or three items depending on state (public / private / remove). `set_bookmark(id, add, restrict)`
already accepts a re-tag; the state always comes from the reply, and a private bookmark draws a lock
glyph. Menu state machine and `restrict -> glyph` are tested; the demo catalogue gains a private
bookmark so the state can be captured.

### F5 · Queue control: pause, resume, throughput, ETA

`TaskState::Paused`; `pause(id)` reuses the existing `Cancel` so a running task gives its permit back
instead of holding it until the body arrives; `take_next_queued` skips paused rows; `resume(id)`
returns it to `Queued`. **HTTP Range resume is out of scope this round** and the document says so: a
resumed task restarts its body, which is safe because the destination is only published after the
whole body arrives. Throughput is a three-second sliding window over completed bytes;
`eta_seconds` is `None` when unknown.

The downloads page header gains total progress, throughput, ETA and pause/resume-all; each row gains
a pause/resume button and its own speed line, using the `on-accent` pair so U23's contrast defect is
not repeated.

---

## 6. Optimisations

**O1 · Decode what is shown.** Plan 48 px thumbnails for the strip (grid tier) and viewer-size decodes
only for the current page and its two neighbours; replan on `select-page`; drop stale results on
`detail_epoch`; release the bitmaps when the route changes. Accepted by a counting fake cache: thirty
pages produce three large decodes and thirty small ones, each exactly once.

**O2 · Index the disk tier.** Build the index once in `DiskTier::open`, maintain it on put and remove,
and let `enforce_budget` delete from the index instead of walking. A read that fails (file removed
behind our back) drops the entry and is treated as a miss. Accepted by asserting zero `read_dir` calls
on the hot path and an index that equals a fresh scan after a simulated restart.

**O3 · Wake instead of poll.** Replace `IDLE_POLL` with a `Notify` signalled by enqueue, cancel and
resume. Accepted by counting wakeups (zero in 200 ms idle) and a start latency of at most 5 ms.

---

## 7. Gates this round owes

| ID | Gate | Negative experiment |
| --- | --- | --- |
| G21 | the Tab walk identifies a stop by `(Name, Type, rect)` and asserts every focusable Button/ListItem/Edit was visited; `cycle=` also reports `lost`/`skip`, and `-SkipTab` is a FAIL rather than a pass | a page fixture with an unreachable control: 17 failures across eight pages on the tree as it stands, all three classes named above; and a recorded walk replayed through the old rule to prove the identity change alone moves no count |
| G22 | at least one page walks every stop; the rest keep ten; the run reports its own duration | a planted stop with no ring fails that page |
| G23 | the settings cache figure comes from a fixture, so both baselines can be exact | change one digit -> red |
| G24 | `PIXIV_DESKTOP_FIXTURE` is implemented for `ugoira/selection/palette/downloads`, and an unknown value exits **77** | an unknown value -> 77 |
| G25 | CI runs the gate suite in its own job | a planted lint fails that job |

---

## 8. Phases, and the commit each one earns

**Phase 0 — Lead.** The lint (done, `59582d3`), this document, then the interface of §4 on both sides
with defaults and no-op handlers, so every later commit compiles alone.
Commit: `docs(plan): round nine, and the five features the shell still lacks` then
`refactor(app): the round-nine seams, declared before they are filled`.

**Phase 1 — three tracks on disjoint boundaries.**

| Track | Boundary | Delivers | Commit |
| --- | --- | --- | --- |
| **A crates** | `crates/**` | F1's three functions, F5's pause/throughput, O2's index, O3's wake-up, each with tests | `feat(crates): a film with a budget, a queue that pauses, an index that stops rescanning` |
| **B tools** | `tools/**`, `.github/workflows/ci.yml` | G21, G22, G23, G24, G25, and selftest cases for each | `test(tools): a walk that visits every stop, fixtures for four states, a CI job that runs them` |
| **C ui** | `ui/**` | U5, U6, U7, U11, U28, U29 and all F1-F5 declarations behind the §4 names | `feat(ui): five surfaces the shell lacked, and six declarations that drew the wrong thing` |

**Phase 2 — three controller tracks, after phase 1 lands.**

| Track | Boundary | Delivers | Commit |
| --- | --- | --- | --- |
| **D detail** | `src/app/detail.rs`, `src/app/ugoira.rs`, `src/app/callbacks/detail.rs` | F1 playback, F4 visibility, U29 paging, O1 paging plan | `feat(app): playback, a private bookmark, and a viewer that pages` |
| **E feed** | `src/app/feed/**`, `src/app/palette.rs`, `src/app/callbacks/feed.rs`, `src/app/callbacks/search.rs` | F2 selection and batch, F3 palette and jump | `feat(app): selection, batch actions and the command palette` |
| **F downloads** | `src/app/downloads.rs`, `src/app/settings.rs`, `src/app/callbacks/downloads.rs`, `src/app/callbacks/settings.rs` | F5 wiring, throughput publishing, fixture injection | `feat(app): a queue that can stop, and says how fast it is going` |

The Lead keeps `src/app/mod.rs`, `src/route.rs`, `src/images.rs`, `src/uiaudit.rs`, `src/main.rs`,
`src/theme.rs`, `ui/theme.slint`, `docs/**` and `README.md`. Tracks D-F add their own `impl App`
blocks in their own files, which is how `home.rs` and `profile.rs` already work, so no track needs to
edit `mod.rs`.

**Phase 3 — record and re-verify.** Captures for the four new states, re-baseline where a change
legitimately moved a pixel with the reason named, `tools/gates.ps1 -Ui` green, docs/UI-NOTES.md §16 in
the voice of rounds one to eight, README's limits and shortcut tables updated.
Commits: `test(visual): four new states, and why each baseline moved`, then
`docs(ui-notes): round nine, and what each new guard decides`.

---

## 9. Acceptance rules

1. **A gate change is accepted on a negative experiment**, not on a passing run, and the experiment is
   re-runnable from `tools/selftest-gates.ps1`.
2. **Every defect class ends with a guard** — a unit test, a `src/uiaudit.rs` rule or a tools gate — and
   the guard must fail on the defect as it is today. Where no guard is possible, the plan says so.
3. **A visible change ships Before and After** under the same fixture, viewport and theme.
4. **A number that moves is explained**: test count (327), baseline pixels, decode targets, Tab stops,
   tracked bytes (212 files / 12 100 122 B).
5. **No claim without a measurement.**
6. **A feature is not done without all four**: a unit test, an accessibility assertion, a keyboard path,
   and a capture.
7. **A degradation is written down**: modifiers that do not arrive, Range resume that is not
   implemented, IME behaviour that is not verified — in the document, never as "supported".

## 10. Risk register

| Risk | Mitigation |
| --- | --- |
| A 16 ms playback tick costs CPU | frame time derived from `delay_ms`; stops on page/route change; demo capped at 24 frames |
| Film memory | 64 MB publish budget, stride thinning, current page only |
| `Ctrl`/`Shift` never arrive in `PointerEvent` | one experiment first; documented fallback is an explicit selection-mode button |
| Three tracks editing one struct | §4 frozen in phase 0; C owns `ui/**`, D-F own their own Rust files |
| The a11y walk now visits every stop | budget ratchets up only, re-pinned in the same commit with the measured numbers |
| Detail baselines move because thumbnails sharpen | allowed, one reason per page, no widening of the pixel ceiling |
| Pause leaks or deadlocks a permit | reuses `ClaimGuard` and the biased select; a test asserts another task starts while one is paused |
| The disk index disagrees with the tree | a failed read drops the entry and is a miss; a restart-and-compare test |
| A CI gate job makes PRs slow | runs `-SkipUi -SkipBuild` after one build step; measured duration goes in README |

## 11. What is still not verified after this round

A real screen reader reading the tree § 12a produced; the Linux and macOS input paths and their
modifiers; whether Pixiv's real `ugoira_meta` frame count matches its archive (needs a signed-in
session — the demo film proves the pipeline only); anything about Range resume, which this round
deliberately does not implement; the command palette under an IME composition; whether
`SegmentedControl`'s one-way binding survives its own first click; whether `visible: false` removes a
child from a layout (`TaskRow`'s two buttons still depend on it); and the settings capture's cache
figure until G23 lands.
