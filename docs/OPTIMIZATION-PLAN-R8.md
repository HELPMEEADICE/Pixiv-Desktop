
# Optimization plan — round eight

Written against master @ 0314718 (round seven, 92 commits), on the machine that produced every
capture in docs/screenshots/. This is a *plan*. docs/UI-NOTES.md stays the record of what has
been fixed; docs/OPTIMIZATION-PLAN-R7.md is left exactly as it was written, including the items
this round picks up.

Round seven was about gates that could not fail. Four of them still cannot, one of them cannot
even *run* at HEAD, and the gate that cannot run is the one an earlier round told the reader to
consult about DPI. That is where round eight starts, but it does not end there: the same audit
found ten UI defects no gate was ever pointed at, two "load more" paths that never reach the
append path round seven fixed, and a download that can publish a file two writers interleaved.

---

## 1. Baseline measured today, not remembered

| Measurement | Result | How |
| --- | --- | --- |
| cargo test --workspace --all-targets -- --list | **304** entries listed | run this session |
| tools/gates.ps1 -Ui -SkipBuild -SkipTest -Lint Off | **exit 1** — the suite is red at HEAD | run this session |
| tools/verify-dpi.ps1 -NumericOnly | throws at :78 "src/images.rs no longer rounds the decode target with div_ceil" | run this session |
| tools/verify-a11y.ps1 | 8 of 8 pages meet the budget; **tabStops=10 on every page** while focusable is 14–44 | run this session |
| home / discovery / ranking / search / bookmarks / history / downloads / settings | focusable **44 / 35 / 43 / 21 / 30 / 14 / 18 / 20**; nodes 98/83/114/47/74/32/44/65 | as above |
| tools/verify-visual.ps1 | 20 of 20 OK, over=0.0000 %, max=0 on every row | run this session |
| tools/verify-focus.ps1 | 10 stops per page, a ring on 8 of them, RING-DIM (a=0.4) on back/forward | run this session |
| tools/verify-layout.ps1 -Pages , -Sizes , | **"0 checks, 0 not OK", exit 0** | run this session |
| Tracked tree | 245 files, 24 806 815 B | git ls-files + sizes |
| PNG inside it | 22 685 773 B = **91.5 %** of tracked bytes | as above |
| docs/screenshots/baseline/focus-*.png | 34 files, 12 890 992 B = **52.0 %** of tracked bytes, read by nothing | verify-focus.ps1:50-51 |
| target/ | 20.01 GB, untracked | on disk |
| tools/*.ps1 tracked | 13; **3 of them are invoked by nothing** (verify-icons, verify-layout, selftest-gates) | git ls-files '*.ps1' |

The tree compiles and the pixel gate is clean. Nothing below exists because a test is red; it
exists because a gate *cannot* be red, because no gate was ever pointed at the defect, or
because two documents state opposite things and both are committed.

---

## 2. Findings

Severity is "how much of the product a user notices", not "how hard it is to fix". Every line
number is HEAD @ 0314718.

### 2.1 Gates that cannot fail, or cannot run

**G6 · P0 · The DPI gate cannot run at all, and that is why the suite is red.** verify-dpi.ps1:77
matches div_ceil)\s*\*\s*(\d+) against src/images.rs:60
(physical.div_ceil(TIER_STEP) * TIER_STEP), and :81 matches a rounded.clamp(n, n) form against
:91 (tier_up(physical).clamp(128, cap.max(128))). Neither substring exists any more, so the
script throws before measuring anything and gates.ps1 reports one FAIL for the whole suite.
Worse: the header of verify-dpi.ps1 is where round seven pointed the reader for "the ladder a
100 % host can run".

**G7 · P0 · A failed capture is invisible, so the pixel gate diffs yesterday's PNG.**
verify-visual.ps1:241 runs the capture with "| Out-Null" — the child's exit code is discarded —
and :242 then only asks whether the file exists. $currentDir is created at :224 and never
cleared, so when the capture fails the comparison runs against the previous run's PNG and
reports today's render as matching it. One run with -Exe target\debug\nope.exe prints twenty OK
rows and exits 0.

**G8 · P1 · The icon gate's own "not measured" path exits 0.** verify-icons.ps1:63 prints "Not
measured: icon geometry read from a truncated frame would prove nothing" and then exits 0.
gates.ps1:101 maps 0 to PASS, so the one gate that measures glyph ink reports a pass for a run
that measured nothing. The code for "this host cannot run it" is 77 and this script never uses
it.

**G9 · P1 · A partial skip is reported as a clean run.** verify-visual.ps1:335-337 exits 0
whenever some captures were judged and some were skipped, printing "N skipped (not judged)".
G1 closed the all-skipped case in round seven; one-judged-nineteen-skipped is still green,
which on a 100 % or 125 % host is the *normal* outcome.

**G10 · P1 · The diff budget admits 354 arbitrary pixels.** verify-visual.ps1:44
MaxDiffPercent = 0.02 against the tracked 1536x1152 captures is 354 pixels — a 12 px icon can
disappear inside it — and the message prints "within 0.02 %", which reads as a hundredth of a
percent.

**G11 · P1 · Zero checks is a pass.** verify-layout.ps1:73-76 counts failures, and with no size
selected there are none, so the script falls off the end at exit 0 printing "0 checks,
0 not OK". Measured this session. Same class as G1, fixed in a different file.

**G12 · P1 · A partial DPI ladder, or a numbers-only run, claims the ladder held.**
verify-dpi.ps1:254 requires both zero rows and some skips for the 77; :258 then exits 0 with
"N scales measured live" for any run where nothing *failed*, including -NumericOnly, which
measured zero scales.

**G13 · P1 · The numeric half of the DPI gate asserts PowerShell against itself.**
verify-dpi.ps1:94 recomputes the rounding with [math]::Ceiling and :106 compares the mirror to
the mirror. No Rust executes, so the only link between the gate and images::target_width is the
pair of regexes in G6 — which no longer match. The gate was measuring its own arithmetic and
reporting on the product.

**G4 · P1 (carried, still open) · The accessibility gate passes without its budget.**
verify-a11y.ps1:198-201 prints "no budget … run with -Update" and exits 0 after launching the
app eight times; :171-172 falls back to the default floors (1/2/3) for a page absent from the
JSON. Delete the file, or delete one page's entry, and that scope is green.

**G5 · P1 (carried, still open) · The Tab walk stops at its own sample size, and the sample is
pinned as the requirement.** verify-a11y.ps1:25 TabSteps = 10, :117 walks $i -lt $TabSteps,
:188 writes minTabStops = $r.distinctTab into tools/a11y-budget.json:15. Today every page
reports tabStops=10 while focusable is 14–44: the budget and the sample are the same number, so
the gate cannot see a stop it never walks.

**G14 · P2 · The scale guard fails open.** verify-visual.ps1:270-272 sets $baseScale = 0 when
the sidecar is missing or has no scale, and the skip test then reads "$baseScale -ne 0 && …" —
an unreadable baseline switches the guard off instead of skipping the comparison.

**G15 · P2 · -Update blesses whatever it measures.** verify-a11y.ps1:188-194 writes the budget
from the same measurement it is supposed to bound, so following the documented fix instructions
on a degraded build records the degradation as the floor.

**G16 · P2 · Re-baselining has no floor on pixels.** selftest-gates.ps1's only comparator case is
a 200x200 blob — 40 000 px, 113x the budget — so it proves the comparator is not blind, not
that the budget means anything. A 12x12 planted rectangle (144 px, 0.0081 %) is the case that
would.

**G17 · P2 · The gate named "one entry point for every gate" has three gates outside it.**
gates.ps1:245-260 calls four verifiers; verify-icons.ps1, verify-layout.ps1 and
selftest-gates.ps1 are called by nothing, while README.md:177,180 and UI-NOTES.md:903 describe
all three as gates. selftest-gates.ps1 is the negative experiment the acceptance rules rest on
and there is no way to run it from the list.

**G18 · P2 · The accessibility gate has no "cannot run here" path.** verify-a11y.ps1:69 throws
when no window appears; 77 appears in verify-focus, verify-visual and verify-dpi and nowhere in
this file. A gate that can only pass or crash is the mirror of a gate that cannot fail.

**G19 · P2 · No CI job runs any gate script.** .github/workflows/ci.yml runs fmt, actionlint,
check, clippy and test; README.md:239-245 says so. The measured gates — accessibility, pixels,
focus, icons, layout, the selftest — have no per-commit evidence anywhere, which is how G6 sat
broken without anyone noticing.

**G20 · P3 · Two capture harnesses do not pin mode or theme.** verify-icons.ps1:35 and
verify-layout.ps1:45 call shot.ps1 without forcing demo mode or a theme, while verify-a11y:49,55,
verify-focus:119,133 and verify-dpi:160,162 do. The same artefact name therefore holds two
different appearances depending on the host's OS theme.

### 2.2 UI/UX defects, measured

Eight carried from round seven, twelve new, each verified at HEAD by reading the declaration
that produces it.

**U5 · P1 (carried) · The detail page has no scroll view, and its vertical budget does not fit.**
detail.slint:534 is a VerticalLayout directly in the page and main.slint:744 clips the host. At
720x600 the column wants 44 (toolbar :551) + 1 (divider :604) + 289.3 (viewer :710, which is
min(root.height * 0.55, …)) + 160 (InfoPanel min-height, :730) + 68 (strip :764) = 562.3 in a
526 px box: 36 px, and the strip's hit area, are gone.

**U6 · P1 (carried) · The suggestion panel clips ten of seventeen rows and the arrow keys still
select the hidden ones.** main.slint:1141 is
height: min(392px, panel-body.preferred-height + 10px) with clip: true at :1149; rows are 34 px
(:51), the list is 12 recommendation tags plus up to 5 history rows, and suggest_move
(src/app/feed/search.rs:191) walks all of them.

**U7 · P1 (carried) · Twelve chips are twelve Tab stops before the first artwork.**
home.slint:228 builds one TagChip per tag and every chip carries its own
key_focus := FocusScope (components.slint:843); there is no roving focus, so reaching the grid
costs 26 presses (measured in round seven, §U3).

**U10 · P1 (carried) · The detail page's thumbnail strip puts its scrollbar on the thumbnails.**
detail.slint:764 is 68 px with padding: Theme.space-2 (:774) and 48 px thumbs (:779-780):
content 8–56, bar band 54–68, 2 px of overlap. Home and gallery reserve their band (§12d); this
one was never given one.

**U11 · P1 (carried) · A recommended artist never says whether you follow them.**
RecommendUserItem carries followed and premium (components.slint:1791-1792), UserCard draws
neither, callback toggle-follow() (:1824) has no producing control, and both call sites bind it
to an empty block (home.slint:384, gallery.slint:257). Rust fills both fields
(src/app/feed/mod.rs:627-628) and nothing reads them.

**U14 · P2 (carried) · The recent-search row breaks at a hard-coded four.**
gallery.slint:347,350: Math.ceil(root.recent-searches.length / 4) and Math.min(4, …). The break
is a constant, not a function of the available width.

**U17 · P1 · In dark mode the ranking badge's digits are 2.38:1 against their own fill.**
components.slint:1529 paints Theme.accent-fill for rank <= 3 and :1536 paints the digits with the
literal #ffffff. theme.slint:58 makes accent-fill #4cb0f5 in dark, and theme.slint:57 already
states the measurement — which is exactly why on-accent exists (dark ? #101010, 8.01:1).
uiaudit cannot see it because #ffffff sits in the artwork-overlay allowlist (src/uiaudit.rs:176)
and the pair is not in the table at :229-276.

**U18 · P1 · The detail page's "reload this page" button is dead.** In ZoomViewer the failure
block is declared at detail.slint:87 with its AppButton at :110-114, and the viewer's full-size
ta := TouchArea follows at :136. Slint hit-tests in reverse declaration order — the project
states this law itself at components.slint:386-388 and main.slint:1130 — so the press lands on
the zoom surface and the only retry affordance in the page cannot be clicked.

**U19 · P2 · Four controls in the detail toolbar sit 6–13 px above the row's centre line.** The
row is 44 px (detail.slint:550) but only the back button carries row-height: 44px (:559-562); the
fit, zoom and fullscreen IconButtons (32 px) and the page-count Badge (18 px) leave alignment at
its default, which in Slint places a fixed-size child at the padding edge
(i-slint-core-1.18.1/layout.rs:1373-1382). The InfoPanel's compact follow button (compact: true
gives 26 px in a 44 px cell, :342-349) is 9 px high for the same reason. main.slint:518,526,557
and downloads.slint:8-11 already document and obey the rule this toolbar breaks.

**U20 · P2 · UserCard's text column runs under its own preview image.** The 96 px preview is
placed at x: parent.width - 96px (components.slint:1862-1865) and the text column then takes the
full width with padding: Theme.space-3 (:1877-1878). At the default 210 px tile the column spans
12–198 and the preview 114–210, so 84 px of every name, handle and comment is painted over
artwork at half opacity.

**U21 · P2 · The demo-mode switch and the theme selector are not Tab stops at all.** Neither
ToggleSwitch (components.slint:567-574) nor SegmentedControl (:633-640) declares a FocusScope,
and Slint focuses nothing else. Both call sites also omit label: (settings.slint:335, :222), so
when UI Automation does reach them they announce as an unnamed switch and an unnamed radio group.
uiaudit asserts a role (:852) and orders a scope that already exists (:913); it has no rule for
"an interactive component with no scope".

**U22 · P2 · The only chrome path to your own profile is invisible to Tab and to UI Automation.**
main.slint:580-591 is a bare TouchArea with no FocusScope and no accessible-role; the Avatar
above it (:568-578) has no role either. The command bar's measured walk is back, forward, search,
theme, 首页 (UI-NOTES.md:537-538), so the account affordance — and, signed out, the sign-in flow
— is reachable only by mouse.

**U23 · P2 · The cancelled download badge is 4.32:1 in light mode.** downloads.slint:132-139 pairs
text-tertiary (#6e6e6e) with layer-sunken (#ececee) for state 4. 4.32:1 is under AA for the 11 px
label (dark measures 7.32:1, so this is light-only). src/uiaudit.rs:243-252 measures text-tertiary
against canvas, layer and layer-alt — layer-sunken is the one surface it omits.

**U24 · P2 · The "latest works" band renders nothing in the empty state.** home.slint has a body
for latest-state == 1 (:301) and == 0 (:312) and none for == 2 (empty) or == 3 (error, which only
reaches the header note). BAND_EMPTY is 2 (src/app/mod.rs:419), set in demo mode with a message
(src/app/feed/home.rs:411-412) and for an empty payload (:505-506). The artists band twelve lines
below handles the same state correctly (home.slint:390-392) — the pattern exists and this band
does not use it.

**U25 · P2 · A failed discovery-users request is silent.** gallery.slint:107 declares
recommend-users-message, the shell fills it (main.slint:812), and the page never reads it; the only
branch requires recommend-users-state == 0 and a non-empty list (:217), so an error makes the
whole artist band disappear with no text and no retry.

**U26 · P3 · The loading skeleton always draws four columns.** home.slint:257, gallery.slint:267,
profile.slint:293: for n in [0, 1, 2, 3]. At the 720 px minimum window the pane is already compact,
GridGeometry::for_width(618) resolves to three columns of 195.33 px (src/layout.rs:57-67,83), and
4 x 195.33 + 3 x 16 = 829 px sits in a 618 px box with horizontal-scrollbar-policy: always-off
(components.slint:1190) — a clip, not a scroll. The fourth skeleton is sliced off and unreachable.

**U27 · P3 · InfoBar counts its body's padding twice.** components.slint:928 is
height: body.preferred-height + Theme.space-2 * 2 while body already declares
padding-top/bottom: Theme.space-2 (:950-951). A layout's preferred-height includes its own padding
(i-slint-core-1.18.1/layout.rs:1394-1420), so every bar is 16 px taller than its content and the
slack is all under the last line.

**U28 · P3 · The active-download count disappears below 1000 px.** main.slint:746 draws the Badge
only if !root.compact and root.badge > 0, and rail-compact is rail-collapsed or narrow (:123) with
narrow = width < 1000 (src/app/feed/mod.rs:450-452). Nothing else in the shell reports how many
downloads are running.

### 2.3 Runtime: the append path round seven fixed, minus two call sites

Round seven made plan_feed_update able to extend instead of rebuild, and R2 closed at the generic
pager. Two callers were never converted.

**R10 · P1 · The profile's "load more" rebuilds every column and throws away the append path.**
src/app/profile.rs:339 clones the whole accumulated list and :345 calls render_feed(all_works,
false). append = false makes already = 0, so plan_feed_update returns Rebuild
(src/app/feed/mod.rs:114): new models for every row, a fresh distribute, five SharedString
allocations per item and full card re-creation — cost grows with what the artist's page already
showed.

**R11 · P1 · The home "for you" load-more does the same, with an extra full clone.**
src/app/feed/home.rs:602-603: "let works = app.home.works.clone(); app.render_feed(works, false);".
app.home.works already holds the merged list, so the clone buys nothing and false disables the
very path R2 unblocked.

**R12 · P1 · A RefCell borrow of App is held across a modal Win32 dialog.**
src/app/callbacks/downloads.rs:11-12 takes shared.borrow_mut() and then calls picker::pick_folder;
src/app/callbacks/settings.rs:144-145 repeats it. src/picker.rs:56 passes hwnd_owner: null, so the
main window stays enabled and clickable while the dialog pumps its own message loop; every other
callback borrows App (callbacks/settings.rs:13), and a click that lands during the loop is a
BorrowMutError panic. Latent on this host until someone runs the experiment in §7 — but a borrow
held across a modal loop is wrong whether or not it is observed today.

**R13 · P2 · Covers are pruned only when the feed is replaced.** src/app/feed/mod.rs:57-62 prunes
inside if !append, and prune_covers has exactly one call site, so an endlessly appended feed keeps
one decoded texture per work ever shown, outside the 192 MiB cache budget.

**R14 · P2 · The scroll table grows one entry per route ever visited and is scanned linearly on
every scroll event.** src/app/mod.rs:315-318 pushes a (route, offset) pair and finds it on each
call; callbacks/feed.rs:44-45 calls it once per scroll delta, and App::go:226-227 stores the route
being left, so one entry per artwork opened accumulates for the session.

**R15 · P2 · Every avatar arrival re-publishes whole bands.** src/app/feed/avatars.rs:163,171
calls publish_home and publish_discovery_users, each of which builds new VecModels
(feed/home.rs:648, feed/discovery.rs:253), so a home load with 40 faces re-creates every row 40
times. apply_cover (feed/mod.rs:417-421) already shows the right shape: patch the one row that
owns the URL.

**R16 · P2 · record_view's MAX(seq) scan and a recent-search query run on the UI thread.**
src/app/detail.rs:201 records a view on the frame thread, and src/app/settings.rs:171 re-reads the
recent-search words inside refresh_chrome, which every navigation calls. Both take the store mutex
and prepare a statement (crates/pixiv-store/src/lib.rs:246,289-290).

**R17 · P2 · DPAPI seal, file create, write and fsync run inside an on_ui closure.**
src/app/settings.rs:59 and :272 call session::save, which syncs (src/session.rs:118), on the thread
that paints.

**R18 · P2 · A failed artist-work request is rendered as a statement about the account.**
src/app/profile.rs:99 is "let ids = ids.unwrap_or_default();" — an error becomes an empty list, and
:260 then says "这个用户还没有公开作品。"

**R19 · P2 · A failed "load more" still advances the cursor.** src/app/profile.rs:337
state.loaded += requested runs on both branches, so after a failure that slice of the artist's
works can never be requested again and the page silently under-reports.

**R20 · P3 · Three small inconsistencies with one cause each.** src/app/profile.rs:23-24 increments
epoch twice; the tooltip reset lives only in App::go (src/app/mod.rs:239) so Back/Forward can leave
a stale bubble; and src/app/profile.rs:71-72 sets the profile band to error but leaves the works
grid on BAND_LOADING, which is a skeleton with no retry.

**Carried from round seven, unchanged at HEAD:** R1 (opening an artwork plans a URL for every page
and decodes three at a time at VIEWER_MAX_WIDTH = 1600 for thumbnails drawn at 48 px —
src/app/detail.rs:229-236,263,283, with detail_epoch bumped only on open at :34), R3/R4 (the disk
tier walks its tree inside ImageCache::new from src/app/lifecycle.rs:140 before the window exists,
and re-walks it on every put past the budget — crates/pixiv-image/src/disk.rs:53,102-104,139),
R9 (the download worker polls a 20 ms timer for its whole life —
crates/pixiv-download/src/worker.rs:27,355).

### 2.4 Crates: durability, cancellation, and one unbounded allocation

**C1 · P1 · Remote image bytes are decoded with no dimension or allocation ceiling.**
crates/pixiv-image/src/lib.rs:638 is image::load_from_memory(encoded) and :271-274 buffers the
whole body first. The API crate has a BodyBudget and the ugoira crate uses image::Limits with a
512 MiB budget; this path has neither, so a 12000x12000 response is allocated before anything can
refuse it.

**C2 · P1 · The download's .part name is a function of the destination, so two tasks can interleave
into one file.** worker.rs:455 builds part_path(&destination) and :501 renames it. A filename
template without {page} (src/app/lifecycle.rs:151-152) maps two pages to one name; both tasks open
the same .part, write concurrently and both rename, so the published file can be two images
interleaved.

**C3 · P2 · The download's atomic write never fsyncs.** worker.rs:483 flushes the handle (user space
to OS) and :501 renames. A crash between the two publishes a destination whose data was never
written, and :409 then treats that zero-length file as "already downloaded" for the life of the
queue.

**C4 · P2 · The disk tier's temp file is one fixed name per key.** disk.rs:98 is
path.with_extension("part"), while FlightKey includes the width (lib.rs:229), so two single-flight
cells for the same URL at different widths call put on the same key at the same time and one
rename can publish the other writer's half-written bytes.

**C5 · P2 · The disk tier never fsyncs either.** disk.rs:99-100: fs::write then fs::rename. The
order is right; nothing is flushed, so after a crash the name can exist with zero or torn bytes.
lib.rs:536-540 does recover on a decode failure, which is why this is P2 and not P1.

**C6 · P2 · A cancelled download keeps its semaphore permit until the body arrives.**
worker.rs:431 starts fetch and the cancel flag is only read at :445 afterwards, so a cancelled task
holds a hard permit for the whole retry budget (up to 3 x 20 s). Three cancelled tasks at
concurrency: 3 stall every later enqueue.

**C7 · P2 · POST retries have no idempotency, so a lost reply is replayed.**
crates/pixiv-api/src/lib.rs:1089 retries PixivError::Network (error.rs:70) for set_bookmark
(:773-807). A bookmark that reached Pixiv and lost its reply is sent again; addressed by bookmark
row id the replay answers "row gone", and the caller reports a failure for a mutation that
succeeded.

**C8 · P2 · enforce_budget stores a total that a concurrent put has already invalidated.**
disk.rs:152 self.bytes.store(total, Relaxed) while put/remove use fetch_add/fetch_sub. The counter
can drift below what is on disk and the tier stops trimming.

**C9 · P2 · MAX(seq) has no index.** crates/pixiv-store/src/lib.rs:266,290 scan the whole table for
the next sequence, and the only indexes are on viewed_at and created_at (:121-122).
search_history()'s ORDER BY seq DESC (:248) scans too.

**C10 · P3 · Four smaller defects with one consequence each.** worker.rs:198-199 never calls
dedupe_path (naming.rs:107 is exported at lib.rs:58 and called nowhere), so a name collision
becomes Skipped, indistinguishable from "already downloaded"; wire.rs:369 SearchItemWire.id is the
one id field with no serde(default) and a strict Id, so one id-less row fails the whole search page
while :626-627 loosened exactly that value for street payloads; api/lib.rs:382 reserves the declared
Content-Length before a chunk arrives (32 MiB reserved for a 1 KB body, per in-flight request);
ugoira/lib.rs:339 floors each GIF delay to a centisecond with no remainder carry, so the exported
animation runs uniformly fast.

### 2.5 Documentation that is false today

**D5 · P1 · README says the discovery endpoint is unavailable while the client implements it.**
README.md:73: "/ajax/discovery/artworks | **Not available** — no usable endpoint shape found".
docs/API-NOTES.md:159 records "Verified 200 for mode=all, safe and r18", and
crates/pixiv-api/src/endpoint.rs:254 builds it.

**D6 · P1 · README says bookmark and follow POSTs were never confirmed, while the network report
records them as PASS.** README.md:102-103 versus docs/NETWORK-TEST-REPORT.md:100 ("bookmark/follow
mutations succeed and mutate the real account").

**D7 · P1 · Four false statements in docs/ARCHITECTURE.md.** :18 says pixiv-types "depends on
nothing but serde" (its manifest has three dependencies, crates/pixiv-types/Cargo.toml:11-12); :169
and :180 name src/app.rs, which does not exist (it is a 20-file module tree); :193 describes an
Arc<dyn DataSource> that appears nowhere; :164 claims a 13 px body and a 4/8/12/16 scale where the
tokens hold 14 px and seven steps to 32 (ui/theme.slint:175,189-195).

**D8 · P2 · The test count is stale and counts fifteen no-op tests as passes.** README.md:230 says
"291 tests: 14 source rules, 15 live tests"; --list at HEAD prints **304** entries, and the live
suite returns on its first line when the cookie is absent (crates/pixiv-api/tests/live.rs:69-70) —
a return inside a counted test, not an ignore.

**D9 · P2 · Three documents disagree about the capture size that is the ground truth.**
tools/shot.ps1:22 defaults to 1140x886; README.md:28 says 1280x800 logical;
docs/ARCHITECTURE.md:152 says 1140. The tracked README captures are 1920x1200 (1280x800 at 150 %)
and the baselines 1536x1152 (1024x768 at 150 %), so the default in the script matches neither.

**D10 · P3 · Five stale citations.** docs/NETWORK-TEST-REPORT.md:96 cites /ajax/user/extra for
follower counts — the client never calls it and API-NOTES.md:204 says the count is not in that
payload; API-NOTES.md:5 claims every claim in a 30-row matrix is backed by a saved response (7
fixtures exist); ARCHITECTURE.md:207 links "README § Known limitations", which is called "Honest
limits"; UI-NOTES.md:518 still prints "grep -c accessible- -> 0" as current when the tree has 43;
docs/screenshots/readme/search-empty.png (61 090 B) is referenced by no document.

### 2.6 Repository hygiene

**D11 · P1 · 34 tracked captures are 52 % of all tracked bytes and nothing reads them.**
docs/screenshots/baseline/focus-*.png — 12 890 992 B. verify-focus.ps1:50-51 says plainly that the
capture it measures is the fresh one in target/focus/current, never the baseline;
verify-focus.ps1:249-250 copies into the baseline path behind a Test-Path, so a normal run
*writes* them and no code path ever compares them. .gitignore:16's negation
(!/docs/screenshots/baseline/) is what admits them.

**D12 · P2 · Six small dead ends in the manifests and the harness.**
Cargo.toml:37 enables reqwest's stream feature and no call site streams (bytes_stream appears
nowhere); :16 declares rust-version = "1.85" and CI installs stable only, so nothing exercises it;
domain.rs:307,966 are public functions referenced nowhere; gates.ps1:239 says "the two harnesses"
where four run; and no local cargo invocation passes --locked while all three CI cargo jobs do
(ci.yml:97,122,140), so a stale lock passes every local gate and fails all three CI jobs.

---

## 3. The style contract — unchanged, and it still binds

The visual target stays **70 % WinUI 3 / Fluent 2 + 30 % Pixiv web**, as declared at the top of
ui/theme.slint. No phase may:

* add a colour, radius, spacing or font literal outside ui/theme.slint;
* introduce a shadow, glow, gradient or blur where the theme has no token for it;
* lengthen a motion past Theme.motion (150 ms) or give one a bounce/spring curve;
* change a control metric: 32 px controls, 4 px control radius, 8 px card radius, 48 px top bar,
  232/52 px rail, 26 px status bar;
* touch the accent's division of labour (accent decorates, accent-fill carries ink, accent-ink is
  text on the accent wash, on-accent is ink *on* accent-fill);
* trade a measured number for a looked-at one. If a change moves a baseline, the baseline moves in
  the same commit with an explanation.

Every fix in §2.2 is a *repair inside that contract*, not a restyle: a retry button that can be
clicked is the same button in a different declaration order; a toolbar child on the centre line is
the same 32 px control with the row height the file already uses elsewhere; a badge that uses
on-accent is the token the theme added for exactly this pair; a switch that takes focus is the
WinUI control it already looks like. The two *new* patterns round eight introduces — roving focus
inside a chip strip (U7) and focus-follow scrolling (U3) — are the two WinUI patterns the shell
claims to follow and does not yet implement.

---

## 4. Phases, and the commit each one earns

### Phase 0 — the plan, and the false statements (Lead, this session)

Write this document; correct D5–D10 where the correction is a sentence; re-state the test count as
the number --list prints. Commit `docs(plan): round eight, and the gates that cannot fail or cannot run`.

### Phase 1 — four parallel tracks on disjoint boundaries

| Track | Boundary | Delivers | Commit |
| --- | --- | --- | --- |
| **A** tools-truth | tools/** | G6 (parse by constant name, fail 77 not throw), G7 (clear the capture dir, read the child's exit code), G8/G12/G18 (77 instead of 0 or a crash), G9 (a skip budget), G11 (zero checks gives 77), G10 (a pixel ceiling beside the percentage), G17 (all seven verifiers in one list), G4/G5 (fail on a missing budget or a missing page; walk until focus cycles), G14 (fail closed), G15 (ratchet up only), G16 (a 144 px planted rectangle), G19 (--locked) | `test(tools): nine gates that could not decide, and the experiment for each` |
| **B** ui-truth | ui/pages/detail.slint, ui/pages/downloads.slint, ui/components.slint, ui/pages/home.slint, ui/pages/gallery.slint | U17, U18, U19, U20, U21, U23, U24, U25, U26, U27, U10 | `fix(ui): ten declarations that drew the wrong thing, one of them dead` |
| **C** app-truth | src/app/** | R10, R11, R12, R13, R14, R15, R16, R17, R18, R19, R20 | `fix(app): two append paths that never ran, and a borrow held across a dialog` |
| **D** crates-truth | crates/** | C1, C2, C3, C4, C5, C6, C7, C8, C9 | `fix(crates): a temp file two writers can share, and a reply that was never synced` |

Disjoint because A is the harness, B is the Slint source, C is the app controllers, D is the
crates. No track edits another's files. src/uiaudit.rs is the Lead's: the rules U17, U21 and U23
need are the ones that would have caught them, and a rule written by the track being judged is not
a guard.

### Phase 2 — the UX work that needs a new seam (next wave)

1. **U3** focus-follow scrolling on a card focus, and a focus gate that walks past the chrome (G5's
   cycle detection is the prerequisite).
2. **U5** a scroll view on the detail page, with the vertical budget stated rather than discovered
   by clipping.
3. **U7** roving focus inside the chip strip: one Tab stop, arrows inside.
4. **U6** the suggestion panel inside a ScrollView, and suggest_move bounded by what is drawn.
5. **U13** the PIXIV_DESKTOP_FIXTURE=downloads seam, a baseline, and one assertion that a row
   actually renders.
6. **U14** layout::wrap_chips(widths, available, gap).
7. **U11** followed/premium on UserCard and a caller for toggle-follow.
8. **U12** the 8 px: measure the block's centre, bisect the three candidates, remove it or write the
   arithmetic that makes 8 correct.
9. **U22** a role and a scope on the account affordance.
10. **U28** the download count in compact mode.
11. **R1** detail paging: current page plus or minus one at viewer size, thumbnails at 48 px, epoch
    incremented on leave; **R3/R4/R9** an indexed disk tier and a wake-up instead of a poll.

### Phase 3 — record and re-verify

Full tools/gates.ps1 -Ui green *including* the DPI gate it has not run for a round, re-baseline where
a change legitimately moved a pixel with the reason named, git rm the 34 write-only captures (D11)
and teach .gitignore the rule, and docs/UI-NOTES.md §15 in the voice of rounds one to seven: root
cause, fix, guard, and what is still not verified.

---

## 5. Acceptance rules

1. **A gate change is accepted on a negative experiment**, not on a passing run, and the experiment
   must be re-runnable. Round eight's experiments are the ones it adds to tools/selftest-gates.ps1,
   including the 144 px rectangle of G16.
2. **Every defect class ends with a guard** — a unit test, a uiaudit rule or a tools gate — and the
   guard must fail on the defect as it is today. Where no guard is possible, the plan says so
   instead of implying one.
3. **A visible change ships Before and After captures** under the same scenario, viewport and theme.
4. **A number that moves is explained**: test counts, baseline pixels, decode targets, Tab stops,
   tracked bytes.
5. **No claim without a measurement.** Where a measurement is impossible on this host, the plan says
   so where the claim would have gone.

---

## 6. Risk register

| Risk | Mitigation |
| --- | --- |
| A's fixes change a gate's exit contract while B/C/D run gates | A runs its experiments against target/ copies, never docs/screenshots/; the green run is quoted before and after |
| Making G5 walk until focus cycles changes the reported stop count from 10 to 44+ | that *is* the fix; the budget is re-pinned from the measured count in the same commit, and the old 10 is recorded as the sample size it was |
| B moves a baseline | U10, U19, U20 and U27 legitimately move detail, home and downloads; each re-baseline names the page, the reason and the pixel count |
| C's append fixes change card order | acceptance asserts the existing column models are pointer-identical and only rows are added |
| D's fsync costs throughput | measured on the download path only; the disk tier keeps one fsync per write and the write is already off the UI thread |
| U18's reorder changes which element wins the hit test in the working states | the viewer's TouchArea stays below the toolbar controls, which is where it already is; only the two state overlays move |
| C10's dedupe change renames files a user already has | dedupe_path only ever appends " (1)"; nothing is deleted |

---

## 7. What is still not verified after this round

A real screen reader reading the tree §12a produced; the Linux and macOS input paths; a
pointer-driven drag gesture; whether SegmentedControl's one-way binding is destroyed by its own
first click; whether visible: false removes a child from a layout (both settle with one capture and
are named where they matter); whether wrap: word-wrap plus overflow: elide draws an ellipsis in
Slint 1.18; whether the app's window.scale_factor() matches the sidecar's scale for the 20
committed captures; and whether R12's borrow is live rather than latent, which needs one run with
the dialog open and a click on the rail.
