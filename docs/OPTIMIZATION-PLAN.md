# Optimization plan — round six

Written 2026-11-14. This is a *plan*, not a record: `docs/UI-NOTES.md` is the record of
what was already fixed. Rounds one to five measured the shell, the layout, the theme,
the pointer and the accessible tree. This document says what is still wrong, in what
order to fix it, and how each fix is proved.

> **Status.** Phases 0 to 2 landed in this same session: F8, F1, F3, F9, F2 and the
> verification half of F7 are fixed and guarded, and `docs/UI-NOTES.md` § 13 is the
> record of how. Still open and unchanged by this round: F4 (populated download
> queue), F5 (search-history wrap), F6 (the 8 px residue), and the tab order, which
> turned out to be the more interesting defect (§ 13f). The remaining sections are
> kept as written, because a plan edited after the fact stops being evidence of what
> was planned.

Every finding below cites a line in the current tree. A finding without a citation is
an opinion and does not belong here.

---

## 1. Baseline

Measured on the working tree this document was written against (`master`, 75 commits).

| Gate | Result |
| --- | --- |
| `cargo test --workspace` | 269 passed, 0 failed (17 test targets) |
| `src/uiaudit.rs` source rules | 13 |
| `tools/verify-a11y.ps1` | asserts 10 distinct Tab stops and 12–34 invokable nodes on 8 pages |
| `tools/verify-visual.ps1` | 20 tracked baselines, per-pixel tolerance 6, 0.02 % of pixels may differ |
| `tools/gates.ps1` | fmt → test → lint (`clippy-driver`) → release build; `-Ui` adds the two measured UI gates |

The tree is green. Nothing in this plan exists because a gate is failing; it exists
because a gate does not exist for the thing that is wrong.

---

## 2. Findings

Severity is "how much of the product a user notices", not "how hard it is to fix".

### F1 — P0 · Covers are decoded below their draw size, and the cache can never correct it

The project already names the first half of this as round six's first item
(`docs/UI-NOTES.md` § 12i). The second half is worse and was not named.

| Step | Evidence |
| --- | --- |
| The grid geometry is seeded from a **guess**, not a measurement: `GridGeometry::for_width(858.0)` | `src/app.rs:624` |
| Cover decode width is derived from that geometry at request time | `src/app.rs:1424` |
| At 1024×768 @150 % that guess yields `target = 320`; the settled card box is 333 physical px | § 12i, re-derived: `target_width(202.5, 1.5) = 320` |
| `on_measured` updates the geometry but **never re-plans covers** | `src/app.rs:1529`–`1552` |
| `CoverLoader::plan` skips any id already in the app-side cover map, so a small cover is never re-requested | `src/images.rs:57`–`70`, `src/app.rs:1414` |
| The memory tier treats *anything smaller* as a hit, so a 320 px variant satisfies a later 352 px request forever | `crates/pixiv-image/src/lru.rs:110`–`120` |

So the cover is soft on the first frame **and stays soft for the life of the offscreen
cache entry**. Fixing only the request size (the documented fix) leaves the permanent
half in place.

**Fix shape.** Three parts, each testable without a window:

1. `crates/pixiv-image`: a public `ImageCache::pick(url, want)` that returns the best
   candidate **plus** a `needs_upgrade` flag. `MemoryTier::probe` for `Some(width)`
   prefers the *smallest entry ≥ width*; a shortfall is a fallback, not a hit, and is
   reported so the caller can schedule a better decode. Today a shortfall is silent.
2. `src/images.rs`: the re-request *policy* as pure functions — a tier boundary
   (`target_width` already rounds up to 32 px) and `needs_redecode(cached, target)`.
3. `src/app.rs`: do not plan covers before the first measurement has landed, and
   re-plan (debounced, at most once per settle) when `card_width` crosses a decode
   tier. `spawn_covers` gains the `needs_upgrade` path so a stale small entry is
   replaced rather than kept.

**Acceptance.** A unit test proves a 320 px entry is *not* reported as satisfying a
352 px request, and that the caller is told to upgrade. A traced run at 1024×768
@150 % shows `target ≥ drawn width` for every cover after the first settle.
Evidence: the trace line, before and after.

---

### F2 — P1 · `src/app.rs` is 5 173 lines and `install_callbacks` is 782 of them

`src/app.rs:4042`. The file holds the state machine, six page controllers, the
callback table, four timers and twenty free functions. The project deliberately left
this for after the layout fixes (§ 12i) so a bisect on behaviour stayed possible; the
layout is now green and measured, so the reason has expired.

**Fix shape.** `src/app.rs` becomes `src/app/mod.rs` plus
`src/app/lifecycle.rs` (`run`, `App::new`, window sizing),
`src/app/feed.rs`, `src/app/detail.rs`, `src/app/profile.rs`,
`src/app/downloads.rs`, `src/app/settings.rs`, `src/app/callbacks/mod.rs` and one
module per callback family. **No behaviour change.** The only permitted semantic edits
are `use` lists and visibility (`pub(super)` / `pub(crate)`).

**Acceptance.** `cargo test --workspace` reports the same 269 passed / 0 failed;
`git diff --stat` shows movement, not rewriting; no function body changes in the diff
other than the `mod`/`use` scaffolding. Largest single file under `src/` drops below
~1 200 lines.

---

### F3 — P1 · The motion tokens are declared and almost entirely unused

`ui/theme.slint:194`–`195` declare `motion-fast: 90ms` and `motion: 150ms`. There is
exactly **one** `animate` in the whole UI (`ui/components.slint:548`, the nav selection
indicator), and `Theme.motion` is referenced nowhere at all.

Fluent Design 2 — the declared visual target — is defined as much by its motion as by
its surfaces: a rail that snaps between 232 px and 52 px, a page that appears with no
transition, a cover that pops into a card, and a flyout that blinks into existence all
read as "unfinished" next to the same app with the WinUI durations applied.

**Fix shape.** Token-driven only, short and non-playful (90/150 ms, `ease-out`), and
applied to a bounded set: the compact-rail width, the page host on route change, a
card cover's opacity when its decode lands, the `InfoBar` enter, and the focus ring.
Add a `uiaudit` rule that a declared motion token must be referenced, so the token
table cannot drift from the components again.

**Constraint.** A pixel baseline must not become flaky. Every animated property has to
reach its rest value without a capture seeing an intermediate frame — otherwise
`tools/verify-visual.ps1` becomes a random gate, which § 11b already paid for once.

---

### F4 — P1 · The download page has never been seen populated

§ 12i: "demo mode writes no files, so no baseline shows real rows." The six-state
mapping is unit tested, but the page a user actually looks at has never been captured.
A row is the densest thing in the app (thumbnail, name, progress bar, two buttons,
state glyph) and is the most likely place for a clipped label or a 1 px misalignment.

**Fix shape.** A deterministic fixture seam — `PIXIV_DESKTOP_FIXTURE=downloads` — that
injects a fixed six-state queue into the row model **without touching the filesystem**,
in the same spirit as `PIXIV_DESKTOP_DEMO=1`. Then a tracked baseline and an assertion
that the progress bar's filled width matches the row's fraction.

---

### F5 — P2 · The recent-search row breaks at a hard-coded four chips

§ 12e: "Slint cannot measure text to break a line at real chip widths, so the line
length is fixed rather than measured." Verified safe at 720 and 800 logical px with the
demo vocabulary; not verified with eight long CJK tags, and not verified at 100 % or
200 % DPI.

**Fix shape.** Pack the lines in Rust. `src/layout.rs` already owns every other
measured decision in the grid; a pure `wrap_chips(widths, available, gap) -> Vec<usize>`
is the same kind of function and is testable at any DPI.

---

### F6 — P2 · The error/empty block is 8 px off centre, and that is a real number

§ 12c records it honestly: three independent measurements agree on 8 logical px and the
round accepted it. Accepted is not fixed.

**Fix shape.** Find it. `StateView`'s stretch-spacer pair, the scrollbar gutter and the
toolbar's trailing inset are the three candidates; measure which one contributes 8 and
remove it, or document the arithmetic that makes 8 correct.

---

### F7 — P2 · The DPI ladder above and below 150 % has never run

§ 10 and § 12i. `images::target_width` is unit tested at 1.0/1.5/2.0 and Slint reports
logical px, but only 150 % was ever seen on a real display — and F1 is a *resolution*
bug, so a 100 % run is the cheapest independent check that the fix is right.

**Fix shape.** `tools/drive.ps1` gains a per-process DPI override; the visual gate gains
a second baseline set at 100 %. If this host cannot set the DPI, the plan records the
exact blocker and the 100 % case is covered by the `target_width` / `wrap_chips` unit
tests instead — no claim without a measurement.

---

### F8 — P2 · A stray `%SystemDrive%/` tree sits in the repo root, untracked and unignored

`git status` reports `?? %SystemDrive%/`; it holds seven files under
`%SystemDrive%\\ProgramData\\Microsoft\\Windows\\Caches\\`. `.gitignore` has no rule for it, so a
`git add -A` commits a directory named after an unexpanded environment variable.

**Fix shape.** Delete it, ignore the class of path, and make `tools/gates.ps1` assert
that the worktree has no untracked entry at the repository root outside an allowlist.
A hygiene rule is cheap and this is the second time a shell-expanded path was written
literally.

---

### F9 — P3 · The artist strip repeats three magic lengths

`210px` appears at `ui/pages/home.slint:343`, `ui/pages/home.slint:364` and
`ui/pages/gallery.slint:255`; `132px` at `home.slint:341`/`:344` and
`gallery.slint:241`. `ui/theme.slint` exists to hold exactly this
("No page hard-codes a colour, radius, spacing or control size") and the strip is the
one place that broke the rule.

---

## 3. The style contract — what this plan must not change

The visual target stays **70 % WinUI 3 / Fluent 2 + 30 % Pixiv web**, as declared at the
top of `ui/theme.slint`. Concretely, no phase may:

* add a colour, radius, spacing or font literal outside `ui/theme.slint`;
* introduce a shadow, glow, gradient or blur where the theme has no token for it
  (Slint has no blur; elevation is two flat steps on purpose);
* lengthen a motion past `Theme.motion` (150 ms) or give one a bounce/spring curve;
* change a control metric: 32 px controls, 4 px control radius, 8 px card radius,
  48 px top bar, 232/52 px rail, 26 px status bar;
* touch the accent's division of labour (`accent` decorates, `accent-fill` carries ink,
  `accent-ink` is text on the accent wash) — § 9 exists because that split was got
  wrong once;
* trade a measured number for a looked-at one. If a change moves a baseline, the
  baseline moves with an explanation, not a re-run.

---

## 4. Phases

Ordered by (user-visible impact ÷ risk). Each phase lands its own commit.

### Phase 0 — hygiene and this document (Lead)

Delete the stray `%SystemDrive%/` tree, ignore the class, commit the plan.

### Phase 1 — four parallel tracks on disjoint files

| Track | Boundary | Delivers |
| --- | --- | --- |
| **A** | `crates/pixiv-image/**` | `ImageCache::pick` best-fit + upgrade signal; a stale small variant can no longer masquerade as a hit. Unit tests. Unblocks the second half of F1. |
| **B** | `ui/**`, `src/uiaudit.rs` | F3 (motion, token-driven), F9 (strip tokens), one new source rule. |
| **C** | `tools/**` | F8's gate, a focus/hover capture harness (§ 10 has said for five rounds that hover cannot be synthesised; the Tab walk already presses real keys, so focus can be), and the DPI path for F7. |
| **D** | `src/app.rs`, `src/main.rs` | F2: the module split, behaviour-preserving. |

Disjoint because A is a crate, B is the UI source, C is the harness, D is the one Rust
file nothing else writes.

### Phase 2 — Lead: the covers (F1), and the seams the split moved

Wire A's `pick`/upgrade path into the (now split) feed controller, add F5's
`wrap_chips`, and take F4's fixture seam. This is the phase that changes what a user
sees, so it is serialised after A and D rather than beside them.

### Phase 3 — Lead: re-verify and record

Full gate run (`tools/gates.ps1 -Ui`), baseline re-capture where Phase 1 legitimately
moved a pixel, and a `docs/UI-NOTES.md` § 13 that records round six in the same voice
as rounds one to five: root cause, fix, guard, and what still is not verified.

---

## 5. Acceptance rules

1. **A phase is accepted on evidence, not on a report.** Tests run, gates run, and for
   any visible change a **Before and After capture under the same scenario, viewport
   and theme**.
2. **Every defect class named above ends with a guard**, not with a fix alone: a test,
   a `uiaudit` rule or a `tools/` gate. Three of the thirteen existing rules were
   written after the bug they catch; that is the pattern.
3. **A number that moves is explained.** Test counts, baseline pixels, decode targets
   and Tab stops are all quoted with their before and after.
4. **No claim without a measurement.** § 10 has been honest about hover and DPI for
   five rounds; a round that closes one of those has to show the measurement, and a
   round that cannot has to say so in the same place.

---

## 6. Risk register

| Risk | Mitigation |
| --- | --- |
| Four tracks share one `target/`, so `cargo` serialises on the package lock | Accepted. Cargo waits rather than fails; a build that appears to hang on the lock is waiting, not stuck. |
| Only one of the four tracks needs the desktop (C's captures), the others only need `cargo` | C owns every app launch; the rest are `cargo`-only, so a window cannot be contended. |
| F2's split conflicts with anything else editing `src/app.rs` | Nothing else does. Phase 2 is explicitly sequenced after D. |
| Motion makes the pixel gate flaky | B's brief requires every animated property to reach its rest value before a capture; a moved baseline is reported, not silently re-baselined. |
| F1's fix re-decodes covers on every resize step | Debounced to a decode-tier crossing (32 px), not to a width delta, and at most one re-plan per settle. |
| A track produces churn instead of value | A track that finds no real defect reports `dropped` with evidence rather than inventing work. |

---

## 7. Assumptions

Recorded rather than asked, because each is reversible and none changes the phase order.

1. The visual target stays as declared; no phase proposes a redesign, and no page gains
   a new surface type.
2. Commits are authored by the Lead only, one per accepted track, so the history reads
   as five decisions rather than forty intermediate states.
3. "Fix the UI bugs" means the *cited* defects above plus anything a track proves with
   a measurement. It does not mean a sweep of every page for taste.
4. The download fixture seam (F4) writes no file and reads no store, in the same
   contract § 12g established for `PIXIV_DESKTOP_DEMO=1`.

---

## 8. Commit plan

| # | Message | Phase |
| --- | --- | --- |
| 1 | `chore(repo): remove a tree named after an unexpanded variable` | 0 |
| 2 | `docs(plan): round six, and the two halves of the cover defect` | 0 |
| 3 | `feat(image): a cache hit that says whether it is big enough` | 1A |
| 4 | `feat(ui): motion from the tokens, not from a single animate` | 1B |
| 5 | `test(tools): a gate for the worktree, and a focus capture` | 1C |
| 6 | `refactor(app): one file per page, and the same behaviour` | 1D |
| 7 | `fix(feed): decode a cover at the width it is drawn` | 2 |
| 8 | `docs(ui-notes): round six, and what each new guard decides` | 3 |
