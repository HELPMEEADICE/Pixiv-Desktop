# UI / UX refactor — defects, root causes, verification

This is the engineering record for the WinUI 3 / Fluent redesign. It lists what
was actually broken, why, what changed, and what could and could not be verified
in this environment. Screenshots live in `docs/screenshots/` (git-ignored).
## 1. The layout bugs, and their single root cause

The reported symptoms were: a large blank strip on the right of every page, a
vertical scrollbar *inside* the artwork area, a horizontal scrollbar along the
bottom of pages that should never scroll sideways, cards sliced off at the right
edge, and a settings page whose content ran past the window.

They were all one bug with two faces.

### 1a. `ScrollView` content sized itself from its children

A Slint `ScrollView` lays its child out and takes the child's *preferred* size as
the scrollable content size. The old page bodies were a `VerticalLayout` with no
width of its own, so the body inherited the widest child's minimum width:

- a `Text` with no `overflow` reports its full string width as a minimum;
- a `HorizontalLayout` of tag chips reports the sum of the chips.

Once any of those exceeded the viewport, `content-width > viewport-width`, so:

1. a horizontal scrollbar appeared at the bottom of the page;
2. the grid was laid out with `card-width` inside that wider content box, so the
   columns stopped short of the right edge — the blank strip;
3. the vertical scrollbar, drawn at `content-width`, appeared *inside* the
   artwork area instead of at the window edge.

**Fix.** `PageScroll` (in `ui/components.slint`) binds its body width to
`ScrollView.visible-width` — the viewport, which does not depend on the content —
so `content-width == viewport-width` by construction. Page scrollers also set
`horizontal-scrollbar-policy: always-off`; the only horizontal scrollers left are
the deliberate ones (tag strip, page strip, filter rows), and those use
incompressible text (`min-width: self.preferred-width`) so their content really is
wider than the viewport.

### 1b. Rust guessed the window width, twice

`column_count()` and `card_width()` read `App::last_width`, which was:

- initialised to a hard-coded `1280.0`;
- updated by a `width-changed` callback from Slint (logical px);
- updated *again* by a 250 ms `slint::Timer` polling `Window::size()` and dividing
  by the scale factor.

Two writers, one of them polled, and a wrong initial value. Before the first
callback the grid was computed for 1280 px inside a 1000 px window; after a
resize the two writers could disagree for a quarter of a second.

**Fix.** No polling at all. Slint measures the real content box and reports it
(`grid-measured` → `PageScroll.body-width`); `src/layout.rs` turns it into
geometry. The pane collapse comes from the `width-changed` event and is pushed
into Slint as `narrow`. There is exactly one source of truth, and it is the width
the layout actually resolved.

### 1c. `i % columns` is not masonry

Cards were bucketed with `buckets[i % cols].push(item)`. With variable-height
cards that produces columns of wildly different lengths, and the raggedness grows
with every page appended.

**Fix.** `layout::distribute()` puts each card in the currently shortest column,
preserving feed order within a column. Unit tests assert that the spread beats
round-robin and that a single tall card does not drag the page out of balance.

### 1d. Settings rows had a fixed control column

`SettingsRow` reserved a hard-coded `168px` for the control, so at narrow widths
the label + hint column was squeezed and the long Chinese hints clipped. The
control column is now capped at `min(control-width, max(76px, 46% of card))`, and
hints wrap. The card itself lives in a `PageScroll`, so it can never be wider than
the viewport.

## 2. Interaction defects that were fixed

| Defect | Cause | Fix |
| --- | --- | --- |
| Forward button always went Home | handler navigated to `Destination::Home` | real three-stack history (`src/route.rs`), button disabled when empty |
| "关注动态" nav item opened Search | it *was* `Destination::Search`, and two items shared one active state | item removed; each destination has its own selection state |
| Search icon in the rail never highlighted | no `selected` binding on that item | every nav item binds `selected` to its own destination |
| 收藏 flipped locally in real mode | the demo path ran regardless of mode | real mode only changes state after the server confirms; failure keeps the old state and shows the error |
| 关注 button did nothing | no Rust handler was installed for `detail-follow` | `Client::follow()` wired, follow state read from `user_profile()` |
| 更改保存目录 opened the current folder | there was no picker | `src/picker.rs` (`SHBrowseForFolderW`, resizable dialog) |
| 文件名模板 was dead | no handler, and the manager kept its start-up config | `DownloadManager::set_template()` + persisted to SQLite |
| 下载全部 fetched one image | page list was never fetched in real mode | `illust_pages()` is fetched with the detail; downloads use per-page original URLs, de-duplicated against the queue |
| Image pan did nothing | `drag_to()` ignored its arguments | pan deltas clamped against the image bounds in `src/zoom.rs` |
| Wheel zoom drifted from the cursor | maths used the *window* width and a hard-coded `view_h = 520.0` | viewer forwards its own width/height; anchor maths in `zoom.rs`, unit tested |
| Cover images loaded one at a time | one task awaited every cover in sequence, with no in-flight tracking | `images.rs` de-duplicates and `buffer_unordered(6)` streams them; decode width = card width × DPI |
| Stale results repainted the current page | no generation guard | feed epoch + detail epoch; results from an older epoch are dropped |
| Duplicate download tasks | no de-duplication | `(illust_id, page_index)` checked against the live queue |
| Session cookie in plaintext | `session` file was `phpsessid=…` | DPAPI-sealed `session.bin`, validated before trust, `0600` fallback elsewhere |
| Keyboard shortcuts were advertised but absent | tooltips claimed Alt+←/Ctrl+F/F11 | one ancestor `FocusScope` handles them |
| Binding-loop warnings | layout info derived from the window width | window width is pushed in as `narrow`; the shell is not derived from its own size |

## 3. The half-empty window (round two)

The symptom the user kept hitting was different from §1 and worse: the window
maximised, the top ~450 logical pixels were painted (navigation, one row of
cards, the status bar stranded mid-window) and everything below was blank canvas.

Round one had attributed this to the VM's software renderer ("paints only the top
~460 px at 150% DPI"). That was wrong, and the error was in the measuring tool,
not in the app. `PrintWindow` renders the application's *own* surface, which can
legitimately be a different size than the window; a capture built on it cannot
distinguish "the app painted a small surface" from "the app painted a small
area". Two habits made the artifact disappear:

* capture the screen (`BitBlt` of the window rect), not the app's surface;
* `ShowWindow(SW_RESTORE)` before resizing - a resize delivered to a window that
  has never been restored is dropped by the backend, so the app never sees it.

With a truthful capture the bug reproduced every time, and the trace showed it
was not a rendering problem at all:

```
phys=2560x1506 scale=1.5 logical=1707x1004 shell=1004 body=379 status_y=427
```

The shell was the full 1004px tall; the body row was 379px and the status bar sat
at 427. `body-row.max-height` also reported 379: the row was **clamped**, not
mis-measured.

**Root cause.** Slint propagates layout constraints up from content. The body row
sat in the shell's `VerticalLayout` with `vertical-stretch: 1`, but it contains
the navigation column, whose preferred height is the sum of its fixed rows
(~379px). That becomes the row's implicit *max*-height, so stretch had nothing to
give: the row stayed at its content height, the status bar was placed directly
under it, and the remaining 552px of the shell were never claimed by anything.

**Fix.** The row's height is derived from the window rather than negotiated:

```slint
height: root.height - Theme.topbar-height - Theme.statusbar-height;
```

Exact at every size, monotone in the window height, and no leftover space for a
stretch rule to lose. `vertical-stretch` cannot be rescued with a percentage
either - Slint only accepts percentages for `width`, `height` and `preferred-*`,
not `max-height` - which is why the clamp cannot be expressed away.

An isolated reproduction lives in the probe used to find this: a
`FocusScope`-hosted column with seven fixed rows produced `body_h = 292` (the sum
of the rows) where the same tree with plain rectangles produced `body_h = 932`.

## 4. Defects found by screenshotting every page afterwards

| Defect | Cause | Fix |
| --- | --- | --- |
| Settings labels floated ~180px in from the card edge | `alignment: center` on the row's `HorizontalLayout` makes the row pack children at their preferred widths, dropping the label column's stretch | no `alignment` on that row |
| Settings labels still indented | `alignment: center` on the label column's `VerticalLayout` (cross-axis) shrank each `Text` to its own width | `alignment: start` |
| The "saved to" card was 280px tall for two lines | the page body is stretched to the viewport, and the card was the only flexible child left to absorb the leftover | `SettingsCard` pins its height to its own content |
| Empty-state icon sat at the left edge while its text was centred | a layout's `alignment` does not centre a fixed-width child | paired stretch spacers in `StateView` and in the detail viewer's states |
| `换一批` was a 600px white slab | `AppButton` had no `width`, so it was a stretch candidate in every layout | `width: max(floor, content.preferred-width)` |
| Tag strip grew a scrollbar and hid labels | a 60px-high horizontal `ScrollView` | explicit two-chip rows; `GridLayout` was tried and lays everything on one row |
| Home hero was the largest block of colour in the app | full-width `accent-subtle` panel | plain header |
| Demo covers were colour swatches | two-stop gradient | five composited motifs (§6) |

## 5. Demo artwork

`pixiv-demo` composes one of five motifs per seed - sunset, bloom, poster, night,
drift - each with a harmonised three-hue palette, a vignette and film grain. Still
one analytic function of `(width, height, seed)`: no textures, no bundled
imagery, so the licensing position is unchanged and an illustration always renders
identically. The three motifs that read badly at thumbnail size were iterated on a
rendered contact sheet (petals that ringed the core, a halftone that swallowed the
disc, a stray thin circle).

## 6. Verification

### Automated

```
cargo fmt --all -- --check
cargo test --workspace            # 205 tests
RUSTC_WORKSPACE_WRAPPER=C:\msys64\ucrt64\bin\clippy-driver.exe \
  cargo check --workspace --all-targets
cargo build --release
```

`cargo clippy` itself cannot start in this MSYS2 + MSVC environment (it
re-executes an MSVC toolchain and dies with `linker link.exe not found` before
reaching project code - reproducible on dependency-only crates). The
`clippy-driver` wrapper above is the documented equivalent gate; it emits real
lints (it found `approx_constant` in the new artwork code, which was fixed).

### Layout matrix

`tools/verify-layout.ps1` launches the app once per page per size, restores the
window, resizes it, and asserts the shell invariant
`status_y + status_height == shell_height` from the app's own
`PIXIV_DESKTOP_TRACE=1` numbers. `gap=0` means the status bar's bottom edge is the
shell's bottom edge, i.e. nothing below it is unpainted.

All 24 cells pass (8 pages x {maximised 1707x1004, 1024x768, 800x600}).

### Visual

`tools/shot.ps1` takes ground-truth screen captures. Inspected at 800x600,
1024x768, 1280x800, 1920x1200 and maximised, in light and dark:

* no blank strip at any size; the status bar is on the window edge everywhere;
* masonry reflows 3 / 4 / 5 / 7 columns with correct card widths and no clipping;
* settings rows align to the card edge with controls on the right;
* downloads page hugs its card and centres its empty state;
* detail page: viewer centred (measured 144px left gutter vs 143px right), tags
  wrap, toolbar reachable;
* dark mode restyles canvas, chrome, cards, chips, fields and scrollbars.

## 7. Round three: the icons were never where they looked

The reported symptom was "图标错位" — nav glyphs not lining up with their
labels, button glyphs off-centre. The declarations all looked correct, which is
why it survived two rounds of review.

### 7a. Two facts a Slint declaration cannot show

Both were measured from real captures, not inferred:

1. **`Path` places itself at `(0,0)`.** `FluentIcon` gives it a size but no
   position, so a 16px glyph inside a 32px `IconButton` rendered in the button's
   top-left corner — 8px right and 8px down of where it belonged. Every
   icon-only button in the app had this.
2. **A fixed-height child of a `HorizontalLayout` is aligned to the *start* of
   the cross axis, not centred.** Measured on the navigation rail: every glyph's
   ink centre sat **15.5 physical px** (10.3 logical px at 150% DPI) above the
   centre line of its row, while the `Text` siblings — which fill the row and
   centre their own baseline — were within 1px.

Both are invisible in review and neither is a Slint bug: they are the documented
behaviour of element default position and layout cross-axis alignment.

**Fix.** Two containers, and nothing else places a glyph:

- `IconPresenter` — centres a glyph inside a box the caller owns (used where the
  box is a pill, a tile or a fixed square);
- `IconSlot` — claims the row's full height (`height: 100%`) and centres inside
  that, which is the only way to get a centred fixed-size child out of a Slint
  horizontal layout.

`IconButton` and `AppButton` also gained `row-height`: a 32px control inside a
48px command bar can now claim the bar and centre its own pill instead of being
top-aligned by the row.

### 7b. What the fix had to preserve

Selection must not move anything. The rail's selection indicator, glyph, label and
counter are now all positioned from the item's own height, so hover, press and
select change paint only. The glyph column is a fixed inset, so labels share one
vertical rule across all nine rows.

### 7c. Hit-testing bugs found along the way

Slint hit-tests in reverse declaration order, which produced three real defects:

| Symptom | Cause |
| --- | --- |
| Clicking the bookmark heart opened the artwork | `IllustCard`'s open-on-click `TouchArea` was declared *last*, so it covered the pill |
| The search field's clear button did nothing | a full-size `TouchArea` declared after the layout covered the button |
| Top-bar tooltips never appeared | they were drawn above a 48px bar at y = 0, i.e. outside the window |

### 7d. Verification: pixel geometry, not declarations

`tools/verify-icons.ps1` captures the real window and finds ink runs —
contiguous rows of pixels that differ from the surface behind them — inside a
narrow column. Each run is one glyph or one text label, so comparing run centres
compares what the user sees.

| Size | Theme | Result |
| --- | --- | --- |
| 720x520 | light / dark | 8 glyph rows, all within 2.5px of the rail centre |
| 800x600 | light / dark | same (compact pane) |
| 1024x768 | light / dark | 9 rows, all within 1.0 logical px of their label |
| 1280x800 | light / dark | same |
| 1440x900 | light / dark | same |
| 1920x1080 | — | **not measured**: needs 2880x1620 physical px and the screen is 2560x1600, so the capture is truncated. The harness reports this and exits 0 rather than measuring a cut-off frame. |

`tools/pixel.ps1` does the reading. It walks the locked bitmap directly: the
first version called a PowerShell helper per pixel and took minutes for one strip.

### 7e. Three rules became tests

`src/uiaudit.rs` turns six design rules into tests that read the `.slint` sources.
Each of them had been enforced by review alone, and each had already been broken at
least once in this codebase:

| Test | What it prevents |
| --- | --- |
| `raw_glyphs_exist_only_inside_the_glyph_containers` | a bare `FluentGlyph` outside `IconPresenter`/`IconSlot` — the exact shape of the original bug. Converting the remaining call sites removed four more copies of the centring arithmetic. |
| `every_referenced_icon_name_is_generated` | an unknown glyph name, which renders *nothing*: no error, no empty box, just a control whose icon vanished. |
| `pages_take_colours_from_the_theme` | literal colours outside `theme.slint`. Found three real leaks — the tooltip's inverted surface, the viewer backdrop and the floating control bar — now `Theme.tip-*` / `Theme.viewer-*`. |
| `theme_text_pairs_clear_wcag_aa` | a palette edit that makes text unreadable (see § 9). |
| `accent_is_decoration_only` | the brand blue being reused as a fill or as text, which is what broke button contrast (see § 9). |
| `key_handlers_compare_character_codes_not_key_names` | `e.text == "LeftArrow"`, which compiles and can never match (see § 8b). |

### 7f. A second consequence: the shortcuts never fired

`e.text` in Slint carries a *character code*, not a key name: the left arrow is
U+F702, the right arrow U+F703, F11 U+F70E, Escape U+001B. The handler compared
against `"LeftArrow"`, `"F11"` and `"Escape"` — a one-character string against a
longer one, which compiles, reads correctly, and never matches.

So three features this README advertised had never worked: **Alt+←/→** (page
history), **F11** (fullscreen) and **Escape** (leave the detail page). Ctrl+F,
Ctrl+R and Ctrl+1..7 were unaffected because they compare single printable
characters.

## 8. Theme: three states, not two

The old model stored a boolean `theme` plus a `follow_system_theme` flag that was
persisted, never read, and could describe a state the window was not in. Two
consequences: "follow the system" did nothing, and `system_is_dark()` had its
predicate inverted, so a light desktop painted the whole app dark.

- `ThemeMode` is `System` / `Light` / `Dark`; `Theme.dark` is **derived** from it
  and `Theme.system-dark`, so no page can hold a contradictory state.
- Windows exposes the preference as one registry DWORD (`AppsUseLightTheme` under
  `...\Themes\Personalize`), read through a hand-rolled `RegQueryValueExW` — the
  crate pulls in no Windows bindings, matching how `session.rs` binds DPAPI.
- Slint 1.18 has no colour-scheme notification and no reachable window procedure,
  so a 5s watcher runs **only** while the mode is `System` and stops itself when
  the user picks a fixed theme. Documented limitation, not a hidden poll.
- Settings uses a segmented control; the top-bar button is a genuine two-state
  quick switch that pins Light or Dark.

### 8a. Duplicated state, removed

The status bar said "演示模式（本地数据）" twice on one line, the top bar said it a
third time, and the History page was permanently empty because it asked the
*network* for history that lives in SQLite.

- The shell owns connection state: one dot plus one label, from properties it
  already has. Pages contribute one fact through `status-text`.
- The top-bar DEMO chip is gone; the hero explains demo data once.
- The pane collapse toggle hides itself when the host has auto-compacted the
  rail, where it could not change anything.
- `History` reads `Store::view_history` and maps rows to cards. Unknown fields
  stay empty rather than being invented; an unknown aspect ratio renders square.
- An empty search keyword no longer returns an unrelated default listing (it used
  to reach the API with `""` and render the reply as "your results").
- `clear-cache` walks the disk tier on the runtime; it used to freeze the window
  for the duration of the sweep. (This line was written before the code caught up:
  the handler still called `clear_disk()` inline until round four - see § 11.)
- The search history was write-only: every keyword went into SQLite and nothing
  read it back. The empty-search state now offers the eight most recent terms as
  chips, refreshed on submit and on "clear history".
- Every feed page reused one sentence, the page title and a picture-unavailable
  glyph for its empty state. `GalleryPage` gained `state-title`/`state-glyph`, and
  each destination now says what is empty and what to do about it.

## 9. Contrast: one blue, three jobs

The theme had a single accent used as a decoration colour, a filled-control
background and a text colour. That cannot satisfy all three:

| Pair | Ratio | Requirement |
| --- | --- | --- |
| `#ffffff` on `#0096fa` (primary button, light) | 3.11:1 | 4.5:1 |
| `#ffffff` on `#4cb0f5` (primary button, dark) | 2.38:1 | 4.5:1 |
| `#0096fa` on `#e6f4ff` (selected nav label) | 2.78:1 | 4.5:1 |
| `#8a8a8a` on `#f7f8f9` (hint text) | 3.25:1 | 4.5:1 |

**Fix: separate roles.** `accent` keeps the exact Pixiv blue and is decoration
only. `accent-fill` (light `#0071c2`, 5.07:1 with white) backgrounds anything
carrying text; dark mode keeps the light blue and flips the ink to `#101010`
(8.01:1), as WinUI's own accent buttons do. `accent-ink` is the legible variant
for the accent wash (4.74:1 light, 6.83:1 dark). `text-tertiary` light went to
`#6e6e6e` (4.80:1 on `layer-alt`).

`theme_text_pairs_clear_wcag_aa` parses the palette out of `theme.slint`, computes
WCAG 2.1 relative luminance for the pairs the components actually compose, and
fails under 4.5:1 (text) or 3:1 (non-text). `accent_is_decoration_only` enforces
the absence of `Theme.accent` from every UI file, so the roles cannot merge again.


### 9a. Every translucent colour had its alpha in the wrong byte

Slint parses an 8-digit colour literal as `#RRGGBBAA` — the alpha is *trailing*
(`i-slint-common::color_parsing`, whose own test asserts `#01234567 -> 0x67012345`).
The theme was written as `#AARRGGBB`, so alpha and red were swapped on every
translucent token:

| Token | Written | Rendered | Intended |
| --- | --- | --- | --- |
| `hover` (light) | `#0a000000` | alpha 0 — invisible | 4% black |
| `hover` (dark) | `#14ffffff` | alpha 255 bright cyan | 8% white |
| `pressed` | `#14000000` | alpha 0 — invisible | 8% black |
| `shadow-1` / `shadow-2` | `#4d000000` | alpha 0 — no shadow | 30% / 45% black |
| `scrim` | `#b3050505` | alpha 5 | 70% black |
| rank / type badge | `#a6000000` | alpha 0 — invisible | 65% black |
| bookmark pill | `#d9ffffff` | opaque pale cyan | 85% white |
| `viewer-chrome` | `#dd1c1c20` | alpha 32 | 87% near-black |

Two consequences were visible in captures once you knew where to look: the detail
page's zoom bar was a washed-out pale band instead of a dark pill, and the ranking
grid showed bare `#4` text with no scrim behind it. Every hover and press state in
the app was either invisible or the wrong colour.

Fixed by putting every token in `#RRGGBBAA` order with its intended colour and
alpha preserved. `translucent_colours_use_the_rrggbbaa_order` rejects any 8-digit
literal whose alpha byte is 0 — the shape this mistake always takes.

## 10. What is still not verified here

- **Pointer and keyboard interaction.** No input automation on this host: hover,
  press, drag, wheel and keyboard focus order are exercised by code path and unit
  test only. Pages are reached through `PIXIV_DESKTOP_START_PAGE` and
  `PIXIV_DESKTOP_START_ILLUST`. In particular the key-code fix (§ 7f) is verified
  by reading Slint's own key table and by a test on the source, **not** by pressing
  F11: the harness can trace a window but cannot type into it.
- **The DPI ladder above 150%.** 100/125/200% is not selectable on this host.
  Layout maths is scale-independent (Slint reports logical px) and
  `images::target_width()` is unit tested at 1.0/1.5/2.0, but only 150% was seen
  on a real display. 1920x1080 at 150% needs a 2880px-wide screen and this one is
  2560, so the largest size in the matrix could not be captured.
- **Bookmark / follow against a live session.** No credential was available, so
  those paths are implemented from observed request shapes and are covered by
  failure-path tests only. They never fake success.
- **Download rows with real tasks.** The page renders from the live queue and the
  state mapping is unit tested for all six states, but no screenshot shows a
  populated queue because demo mode refuses to write files by design.


## 11. Round four: what was still broken after three passes

Round three ended with a claim that the layout was measured rather than reviewed.
It was, and it still missed twelve things - because a rule can only be measured if
somebody measured it, and every defect below was invisible to the checks that
existed at the time. Each one now has a guard.

### 11a. Three of the four tooltips could not be displayed

| Site | Why it never appeared |
| --- | --- |
| Compact navigation rail | the label tip was a child at `x: item.width + 8px`, inside a column with `clip: true` and 52px wide |
| Command bar (back / forward / theme / account) | `TipPlacement.Below` put it at y = 54px, under the page, which is declared *after* the bar and paints an opaque `Theme.canvas` background |

Round three had already found the *same* class of bug once ("drawn above a 48px
bar at y = 0, i.e. outside the window") and fixed the direction without fixing the
layering. A tooltip is the only element that must be drawn outside the control that
owns it, and a child can do neither of the two things that requires.

**Fix.** `ui/tip.slint` defines a `Tip` global (text, anchor, side, visible) and
`AppWindow` owns the only `HoverTip`, declared after every page and before the
suggestion panel. Controls publish; the window draws. Placement is derived from the
anchor and clamped to the window, so no call site positions itself.
`tooltips_are_rendered_by_one_overlay` fails the build if a page re-parents one.

Because hover cannot be synthesised here, the bubble is captured through
`PIXIV_DESKTOP_TIP=1`, which anchors the same overlay to the back button: the debug
path and the real one share every line except where the anchor comes from.

### 11b. The rest, in the order they were found

| Defect | Root cause | Guard |
| --- | --- | --- |
| Four "loading more" footers drew their bar at the left edge with the label centred | `VerticalLayout { alignment: center }` orders children along the main axis only; a fixed-width child is placed at the start of the cross axis | `LoadingRow` + `indeterminate_loading_uses_the_shared_row` |
| The viewer's 「重新载入」 button changed the zoom instead of reloading | it called `request-toggle-zoom`, the only callback the failed state had | a real `reload-image` callback; `retry_url` is a pure function with a test |
| The detail page showed a fake author avatar (a blue disc with 画) | the panel predated `Avatar` and was never migrated | the real image through the same map; a resolved lookup also repoints the panel |
| `Avatar` rendered a whole CJK display name as two clipped glyphs | it centred the *word* and asked `elide` to fit it into 32px | a one-character-wide clipped box (Slint 1.18 has `character-count` but no `substr`) |
| A pending settings confirmation survived leaving the page | page instances stay alive; only `visible` changes | `changed visible` clears it |
| The profile header had no narrow mode | a 208px card, absolute children, a hard-coded 250px action reserve | two sets of offsets, switched from the *measured* width pushed in by Rust |
| `Ctrl+R` re-fetched a grid on pages that have none | one handler for seven pages | per-page refresh; the shortcut table now says what the key means |
| `clear-cache` froze the window | the disk walk ran inline, on the paint thread | `spawn_blocking`, notice first (this section also corrects § 8a) |
| Every download tick replaced the whole row model | a fresh `VecModel` every 500ms | one model, patched in place, with a test on the number of rows written |
| `covers` and `avatars` grew for the life of the process | nothing ever removed an entry | collected at feed replacement / capped |
| "Load more" re-balanced the entire feed | `rebuild_columns` ran on append | append into the shortest column; two tests pin the rule |
| A download claim could strand a task in `Running` | the guard lived inside the spawned future, so a future dropped before its first poll never created it | the guard travels with the claim |

That last one is why the suite could not be trusted for a whole round:
`a_failed_request_becomes_failed_and_can_be_retried` failed about one run in three,
and a gate that fails at random is worse than no gate at all. The cause was in the
product, not in the test.

### 11c. Rules added

| Rule | What it prevents |
| --- | --- |
| `sources_contain_no_stray_control_bytes` | the raw ESC that sat inside a comment in `gallery.slint` for three rounds |
| `tooltips_are_rendered_by_one_overlay` | a tooltip re-parented into a clip |
| `indeterminate_loading_uses_the_shared_row` | a fifth hand-rolled centred row |

`src/uiaudit.rs` now enforces ten rules. Three of them were written *after* the bug
they catch, which is the pattern this file has settled into: find it on screen,
prove it with pixels, then make it impossible.

### 11d. Still not verified here

- **Hover and keyboard.** The tooltip layer is proven by capture and by a debug
  anchor, not by moving a pointer over the control. Nothing in the app is reachable
  by Tab except `AppButton`; that is the next round, and the README now says so
  instead of implying otherwise.
- **A populated download queue.** Its state mapping is unit tested and the model is
  now patched rather than replaced, but no capture shows real rows: demo mode
  refuses to write files.
- **「Load more」 on a long feed.** The placement rule is tested against the
  balancer; the scrolling that triggers it still cannot be synthesised.

---

## 12. Round five: what the pointer and the screen reader see

Rounds one to four measured geometry. This round measured two things that never
appeared in a screenshot — whether a control exists as a *control*, and whether
the pixels a capture shows are the pixels the code intends — and it produced one
correction to itself.

### 12a. Nothing in the window was reachable without a mouse

Two independent measurements, both from the running app:

| Probe | Result |
| --- | --- |
| UI Automation tree (8 pages, 1024x768) | 64 nodes: **47 Text, 16 Image, 1 Edit**; `keyboardFocusable = 1` (the search box); elements answering `InvokePattern`: **0** |
| `grep -c accessible-` over `ui/` | **0**, measured before round five — 43 today |

So the app was a picture of a window: a screen reader read a wall of text with
nothing to operate, and Tab reached the search field and then cycled.

**Fix.** Semantics are declared *inside* the components, so the 50+ call sites did
not move: `accessible-role` (a constant, which Slint requires), `accessible-label`
taken from the text the control already draws (`IconButton` falls back to its tip,
then its glyph name), `accessible-action-default` forwarding `clicked`, plus
`accessible-checked` on the switch and the bookmark pill. Cards became
`list-item` nodes named "title · author" — one operable item instead of a dozen
unordered Text nodes. Keyboard: `FocusScope` + the 2px `Theme.focus-ring` ring that
`AppButton` already had, on `IconButton`, `NavItem`, `TagChip` and both card types;
Up/Down walks the rail and wraps. An entry cannot know its neighbours (it is one
component instantiated eight times), so it reports the delta to the window, which
is where the order is declared. Slint 1.18 has no modulo operator and no locals in
a function body, hence the two-comparison wrap.

**After**, per page: invokable 16–34, focusable 18–44, **10 distinct Tab stops** (round
five's sample size; round eight §15 measured 14–71 once the walk stopped at a cycle),
and the tree carries `Button` / `ListItem` / `CheckBox` / `Group`. Tab out of the
command bar goes back → forward → search → theme → 首页 → 发现 → 排行榜 → 搜索 →
收藏 → 浏览历史.

`tools/verify-a11y.ps1` is the gate; `tools/a11y-budget.json` is what it asserts,
and the numbers in it were written by `-Update` from a measurement, not chosen.

### 12b. The accessibility pass made a card need two clicks

Found in review by clicking, not by reading: a single click on a card focused it
and did nothing; the second click opened the work.

Slint hit-tests in **reverse declaration order**. `IllustCard` and `UserCard` had
their `FocusScope` declared last, so it was the topmost input element and swallowed
the first press to take focus. Every other primitive already declared the scope
*before* its `TouchArea`, which is why only the two cards regressed.

Both scopes moved above their touch area; the ring stays last so it still paints
over the cover. `uiaudit` gained `focus_scopes_sit_under_the_touch_area`, and it was
proved by putting the order back: it names the component and both line numbers.

### 12c. A page was as wide as its widest child

The shell's page host said only `horizontal-stretch: 1`, while every page binds
`width: parent.width` — a cycle that Slint resolves by handing the host the
*content's preferred width*. A page with wide content looked correct by luck.

Measured on the detail page's error state (1024x768, logical px): the page area is
872 wide, the state block 452 (its widest child is `StateView`'s 420px description
plus padding), the block's centre **170 left of the page centre**, and the toolbar's
last button **348 short of the right edge**.

The host now states `root.width - rail`. After: toolbar right edge 8 from the page
edge and the state block centred within 8. `StateView` is also centred by a pair of
stretch spacers rather than stretched, because its description starts at the
beginning of its box and a stretched block centres the heading over a left-shifted
body.

The same class showed up on the history page once the host was honest — its empty
state had been left-pinned too, and it is the reason the history baseline moved
35% in §12f.

**Residue, recorded rather than rounded away:** the block's centre is within ~8
logical px of the page centre, not 0. Three independent measurements (UIA rects,
pixel scan, and a re-measure after dropping a per-page width anchor) agree on 8,
so it is a real offset, small enough to accept and honest enough to write down.

### 12d. Two horizontal scrollbars sat on the chips they scrolled

Both tag strips put a 26px chip row and the scrollbar a `ScrollView` draws into one
fixed height. On the 720px window: chips ended at y=208, the bar's thumb started at
y=209 — one pixel of clearance, so the bar sat on the chip baseline. `home` 34 → 44
and `gallery` 40 → 48 give the bar its own band (thumb now starts 11.3 below the
chips). The `as-needed` policy stays, because the bar is the only way to reach the
chips past the viewport: verified by scrolling, the last chip moves 46px into view.

### 12e. Eight recent searches did not fit on one line

`search_history` returns at most eight, and the row was one centred
`HorizontalLayout` with no wrap, so it overflowed *both* edges: at 720 the last
chip's right edge was 22.7 past the window and 46.7 past the content box. Four per
line over two lines, still centred: the widest line ends 136 inside the right edge
at 720 and 176 at 800. Slint cannot measure text to break a line at real chip
widths, so the line length is fixed rather than measured.

### 12f. The torn cover was in the artwork, not in the renderer

Round five opened with what looked like the most serious defect of the set: a hard
vertical band inside cover cards, the same artwork at two scales, byte-identical
across two runs, at 800 and at 1024. Four hypotheses were tested and three were
killed by measurement:

| Hypothesis | Result |
| --- | --- |
| Stale frame / missing repaint | `request_redraw` every second and no trace produced the same PNG hash — **excluded** |
| Bitmap length disagreed with its declared size | all 50 covers exactly `w*h*4` — **excluded** (the hole was closed anyway, see below) |
| Decode width vs draw box mismatch | real (see §12i) but 1.05–1.08× magnification cannot put two scales side by side — **excluded** |
| `ImageFit.cover` cropping | recomputing cover from the source and diffing per column showed nothing anomalous — **excluded** |

The band is **inside the master JPEG** that Pixiv serves: `_p0_master1200.jpg` and
the `c/480x960` variant both carry it, a fresh download is byte-identical to the
cached copy, and two independent decoders agree. It is a 50% halftone overlay, i.e.
picture content. Our render is faithful: against a cover recomputation of the
source the mean |difference| over the card is 13.9 of 255, and the band's own
columns are the *closest* (7.7–10.5).

**The lesson is the useful part.** A pixel rule cannot tell a rendering defect from
a picture: the luma-step detector found **0** hits on that band (the overlay barely
changes luma) and a texture-step detector found it but also fired on eleven
legitimate bands in the same capture. What settled it was comparing our output
*against the source*, which is now the documented method for any future "the app
draws it wrong" claim.

Two things did come out of the investigation and shipped:

* `RawImage::to_slint` no longer trusts its declared size. A buffer one row short
  left the last row unpainted, one row long sheared every row after the first, and
  `min(expected, len)` reported neither. The height is now derived from the data
  when they disagree, with a warning.
* `fit_to_width_leaves_no_vertical_step` round-trips a synthetic gradient through
  JPEG and `fit_to_width` and fails if any column steps away from its neighbour for
  most of the height — the real decode path, not a hand-built buffer.

### 12g. `PIXIV_DESKTOP_DEMO=1` is the reason a baseline can be committed

The pixel gate captures in forced demo mode so a baseline is deterministic and
holds no personal data. That was not true for one page: history is answered from
the local SQLite view history rather than from a data source, so the first
baseline carried nine real works with their titles and authors. A forced-demo run
now returns an empty history; the store is not read, not cleared.

### 12h. What the new gates can and cannot decide

* `tools/verify-visual.ps1` — 20 captures against tracked baselines, per-pixel
  tolerance 6 with a 1px antialias neighbourhood, 0.02% of pixels allowed to
  differ. A capture at a different scale reports **skipped**, not failed, because a
  150% baseline cannot judge a 100% render. It also cannot see hover: the pointer
  is part of the capture, and a pointer resting over the grid produced a
  `#00000012` scrim on one card — 18/255 on that card and nothing elsewhere, which
  is exactly the shape of a false positive. `shot.ps1` now parks the pointer at
  (4,4) before copying the frame.
* `tools/verify-a11y.ps1` — counts semantics, not wording: it can prove a control
  has a role, a name and an action, and that Tab reaches ten distinct stops; it
  cannot judge whether a label *reads* well. Its Tab walk refuses to synthesise a
  keystroke unless the app is the foreground window, so a stray Tab cannot land in
  the terminal that started it.
* `src/uiaudit.rs` — 13 source rules, three added here
  (`every_interactive_component_declares_a_role`,
  `focus_scopes_sit_under_the_touch_area`, and a scanner self-check that a rule
  cannot run past the closing brace of the component it is checking).

### 12i. Still not verified here, and the next round's first item

* **Covers are decoded smaller than they are drawn.** Traced at 1024: covers are
  requested at `target = 320` (from a pre-layout card width of ~202) and drawn in a
  333px box at 150% — every cover is magnified 1.05–1.32× and looks soft. The fix
  needs `ImageCache::pick` (in `pixiv-image`) and a re-request policy when the
  column width settles, so it is a round of its own rather than a line.
* **The 5125-line `src/app.rs`** (`install_callbacks` alone is 782 lines) is still
  one file. Moving it is mechanical but wide, and it was deliberately left for
  after the fixes above so a bisect on behaviour stays possible.
* DPI 100/125/200% on a real display; a real screen reader (NVDA/Narrator) reading
  the tree this round produced; Linux and macOS input paths for `tools/drive.ps1`.
* A populated download queue in a capture: the state mapping is unit tested and the
  rows are patched, but demo mode writes no files, so no baseline shows real rows.

---

## 13. Round six: the two halves of the cover defect

Round five ended by naming two items for the next round: the cover decode width and
the 5 173-line `src/app.rs`. It named only half of the first one, and the half it
missed was the worse half.

### 13a. A cover was soft for one frame and stale for the life of the process

Three faults had to line up, and they did.

| Fault | Where | What it did |
| --- | --- | --- |
| the decode width came from a **guess** | `self.geometry` was seeded from `GridGeometry::for_width(858.0)`, and `spawn_covers` read `self.geometry.card_width` | covers were decoded for a card width no window had |
| the geometry fix never reached the images | `on_measured` recomputed the geometry and never re-planned a cover | a cover decoded at the guessed tier stayed at that tier |
| the cache called an undersized entry a **hit** | `load_sized`: "A cache hit needs pixels no wider than the request" | so even a re-request would have been answered with the same soft pixels |

The third is the one that would have voided the fix. `ImageCache::forget` has to be
called before every upgrade, and `a_wider_decode_through_the_cache_requires_a_forget_first`
proves it against the real cache with a stub fetcher: 320 → request 352 → still 320,
one fetch; `forget` → 352, two fetches.

The seed was wrong in **both** directions, which is why softness was reported at some
window sizes and not others:

| window @150% | card (logical) | drawn (physical) | decode before | decode after | before | after |
| --- | --- | --- | --- | --- | --- | --- |
| 620x768 | 251.0 | 377 | 320 | **384** | magnified 1.18x | sharp |
| 800x768 | 222.0 | 333 | 320 | **352** | magnified 1.04x | sharp |
| 1024x768 | 173.5 | 261 | 320 | **288** | over-decoded 1.23x | 1.10x, sharp |
| 1536x1152 | 195.7 | 294 | 320 | 320 | 1.09x | 1.09x |

**This corrects § 12i.** Its numbers only reproduce at an 800-wide window: a 222
logical card is what puts a cover in a 333 physical box, and 1024 is not that width.
The label was wrong; the measurement under it was right.

**Fix.** `ImageCache::pick(url, want)` returns the pixels *and* a `needs_upgrade`
flag - the smallest entry at least `want` wide is a clean hit, otherwise the largest
entry that still reaches three quarters comes back flagged, otherwise `None` so the
caller decodes at the right size. `src/images.rs` holds the policy as pure functions
(`tier_up`, `needs_redecode`, `Replan`); `CoverLoader` records the tier each drawn
cover was decoded at, separately from the in-flight slots, because a re-plan frees a
slot while the card still holds the old pixels; `on_measured` re-plans once per
*settle* - a thread-local `slint::Timer` restarted by every measurement, 180 ms, where
the restart *is* the debounce - and only when that settle crossed a tier; and both
draw paths consult `pick`, so a cover that exists only in the cache is drawn *and*
scheduled for upgrade.

Nothing gates the first frame on a measurement. Requesting immediately at the current
tier and upgrading on a crossing is strictly better than waiting, and it has no stall
case - a page that never lays out would otherwise show nothing forever.

Guards: `the_settled_target_never_undershoots_the_drawn_box` (8 body widths x 4
scales, asserting `target_width >= ceil(card * scale)`),
`decode_target_versus_drawn_card_width` (the table above, as a test), and the
`forget` test named above.

### 13b. Four pages moved, and the split is the evidence

Re-baselining is where a round like this lies to itself, so the report is in shapes
rather than in reassurance. After the fix `tools/verify-visual.ps1` reported **8 of
20 captures differ**: `home`, `discovery`, `ranking` and `bookmarks`, in both themes,
0.145-0.253 % of pixels over tolerance. `search`, `history`, `downloads`, `settings`,
`detail` and `detail-error` were **byte-identical**.

That split is the argument. The six that did not move are the pages with no cover
grid; the four that did are the pages whose pixels *are* covers. No chrome, caption
or rail pixel is in the diff.

The cause is a property of the fixture, not of the renderer: a demo cover is
generated *at the decode target* (`demo_raw(id, target, target, 0)`), so correcting
the target re-renders the artwork. The direction is the point - 1024 goes from a 320
decode to a 288 one, still above the 261 box so still sharp, with 19 % fewer pixels
per cover; 800 goes from a 320 decode under a 333 box to a 352 one, which is the
actual defect being fixed. Baselines were written by `-Update` from that measurement.

### 13c. Two motion tokens, one `animate`, twelve now

`Theme.motion-fast` and `Theme.motion` were declared in round one and `Theme.motion`
was referenced nowhere; the whole UI had one `animate`. Five sites use them now -
`ui/main.slint:642` (rail width), `ui/main.slint:745` (page host),
`ui/components.slint:1442` and `:1838` (card covers), `:938` (InfoBar) - plus the
focus ring on six primitives. Every duration is a token, every curve is `ease-out`,
none is longer than 150 ms.

**The safety argument is a number that did not move.** Before any re-baseline, 18 of
20 captures were byte-identical and the other two (`settings`, which carries a live
cache figure) differed by 0.0038 %, inside a 0.02 % tolerance. That is only possible
if every animated property had reached its rest value. A page host whose entrance
timer never fired would have produced a blank baseline, not a subtle diff - which is
why the risk was allowed in the first place.

Guard: `every_declared_motion_token_is_referenced_and_no_animate_uses_a_literal_duration`,
proved in both directions by deliberate break - a literal `90ms` at `main.slint:745`
and an unused `motion-unused-probe` token each failed with the file and the line named.
The reference check is not a `contains`: `Theme.motion` is a prefix of
`Theme.motion-fast`, so a dead `motion` token would have passed on its sibling's
strength.

### 13d. The file that no longer exists

`src/app.rs` - 5 173 lines, `install_callbacks` 782 - is now 20 files under
`src/app/`, the largest 664 lines, with `install_callbacks` a seven-line sequence of
family calls. The largest file under `src/` overall is now `src/uiaudit.rs` at 1 085.

Pure movement is a claim, so it was measured twice: the multiset of `fn` names in the
old file and in the new tree is identical except for seven new `install` entry points,
and the per-target test counts are unchanged (291 workspace-wide, +15 from this
round).

### 13e. Gates for two things five rounds said could not be measured

§ 10 and § 12i both said keyboard and DPI were out of reach. Both statements were
already partly stale, and neither had ever been tested to the end.

* `tools/verify-focus.ps1` presses real Tab keys across three pages, reads each
  focused element's UI Automation rect and counts pixels of the focus colour parsed
  live out of `ui/theme.slint`. 30 stops: 27 show the ring, 3 are `Edit` fields that
  pass on the field's `accent-fill` rule instead, 0 invisible. It captures each stop
  additively. The failure path is real, not asserted: `-MinPixels 100000` makes every
  stop `NONE` and exits 1.
* `tools/verify-dpi.ps1` settles the DPI question. Slint reads `SLINT_SCALE_FACTOR` at
  window creation (`i-slint-backend-winit-1.18.1/winitwindowadapter.rs:602`) and then
  ignores the platform's own scale change, so the ladder *can* run here: four scales
  measured live with `gap=0`, plus a numeric half that sweeps 1 060 points across
  168-300 logical px at 1/1.25/1.5/2x and fails any row whose decode target is below
  the drawn size. 0 rows.
* the worktree gate runs first in `tools/gates.ps1` and fails on any path containing
  `%`, the class a directory literally named `%SystemDrive%` came from. Proved by
  creating one (`FAIL %X%`, exit 1) and removing it (pass).

`tools/gates.ps1` now maps exit 77 to SKIP, because a gate that cannot run on this
host is not a gate that failed, and prints both counts in the footer.

**What the new gates cannot decide.** The focus walk reaches the same ten stops on
every page: the command bar and the rail. `verify-a11y.ps1` reports `focusable` of
14-44, so stops exist below the chrome and Tab never reaches them. The gate proves
the ring, not the tab order. And the DPI table proves the *arithmetic* of
`target_width` against the settled geometry - it cannot see the ordering bug in § 13a,
because that one was about *when* the request happened, not how big it was.

### 13f. What is still not verified here

* ~~**Tab does not reach a card.**~~ **Wrong, and measured wrong in round seven.**
  Ten distinct stops on all eight pages against `focusable` counts up to 44 was true
  only because `verify-a11y.ps1` walks `-TabSteps 10` and the budget pins
  `minTabStops` to the same 10: the sample and the assertion were the same number.
  A walk of 45 steps on `home` reaches **45** distinct stops — 13 chrome, 12 tag
  chips, and the first `IllustCard` at **27**. So the semantic was reachable all
  along and § 12b was right. What round seven found in its place is worse for a
  keyboard user: the page never scrolls to the focused card, so from stop 27 on the
  reported rectangle is clipped to the viewport's bottom edge (physical y = 1499 at
  1024×768 @150 %) and nothing visible has focus. Recorded in
  `docs/OPTIMIZATION-PLAN-R7.md` U3/G5; the fix and its gate are that round's
  Phase 2.
* **A populated download queue.** Unchanged from § 12i: the state mapping is tested,
  the rows are patched in place, and no capture shows real rows.
* **Hover.** Unchanged from § 10. `verify-focus.ps1` moves focus with real keys; a
  pointer still cannot be synthesised, so a hover tint is still unmeasured.
* **A real screen reader** reading the tree § 12a produced.
* **Linux and macOS input paths.**
* Resident cost: the 34 focus captures are 12.3 MB of tracked PNG. Worth trimming to
  one representative stop per page before this directory doubles again.

### 13g. One duplication found in review and closed

`RawImage::to_slint` and the new `feed::cached_texture` had grown the same
length-versus-height clamp with the same warning, because `RawImage`'s fields are
private to `helpers.rs` and the draw path has to be synchronous. Two copies of an
invariant drift, and this particular invariant exists because a short buffer used to
shear every row (§ 12f). Both now call `helpers::rgba_to_slint`; the `debug_assert`
stays on the decoded path only, where the decoder guarantees it. All 20 captures were
byte-identical afterwards, which is the whole output a refactor like that should have.

---

## 14. Round seven: five gates that could not fail

Rounds one to six were about what the app draws. Round seven opened with the
measurements themselves, because round six closed on a claim that was false: § 13f's
first item. Every finding below was confirmed by reading the path and running it.

### 14a. A comparison that never happened was reported as a pass

`tools/verify-visual.ps1` adds a `SKIP` row when the baseline sidecar's `scale` differs
from the live one, and built its failure set with `Verdict -notin @("OK","BASE","SKIP")`
(:200 before this round). So on a 100 % or 125 % host - a DPI `verify-dpi.ps1` tells the
reader to try - all twenty comparisons were skipped and the script printed *"20 captures
within 0.02 % of the baseline"* and exited 0.

Now a run in which nothing was compared exits **77** (`gates.ps1` already reads that as
SKIP) and says how many it skipped; a partly-skipped run exits 0 only with the skipped
count printed. `-Update` still exits 0, because it compares nothing by design.

### 14b. The comparator could not see the defect class rounds four and five were about

`PixDiff.Compare` counts a pixel as equal when any neighbour within `Tolerance` matches,
which absorbs sub-pixel antialiasing - and also means a capture displaced wholesale by one
pixel scores `over = 0`. § 12d was a defect of exactly one pixel of clearance.

A second question is now asked of every capture that already matched: a *strict* counter
(`CompareShift`, no neighbour absorption) compares the capture against the baseline moved
by one pixel in each of the four directions, and if a move recovers more than the gate's
own budget in pixels the verdict is **SHIFT** and the gate fails. Measured: a one-pixel
displacement explains **77 584** pixels; the budget at 1536×1152 is 354, so it cannot be
confused with antialiasing noise (0-225 px per capture on this host).

### 14c. The lint gate could not fail on a lint

`gates.ps1` ran `cargo check` through `RUSTC_WORKSPACE_WRAPPER` with no `-D warnings` -
`cargo check` exits 0 on a warning - and both lint branches then filtered output with
`^(warning|error)` while `--message-format short` emits `path:line:col: warning: …`, so
per-item lints were dropped twice over. The step now sets `RUSTFLAGS='-D warnings'`
(restored in the same `finally` as the wrapper and the target dir) and no longer filters.

Proved in both directions with a real probe: `let unused_probe = 1;` in `src/app/mod.rs`
produced `src\app\mod.rs:201:13: error: unused variable: unused_probe`, the gate reported
`FAIL`, exit 1; with the line removed the gate passed with the file byte-identical
(sha256 65976320F652BB7D1A5C87FF6443C8C259E49C1E0EC41D99006C1AB64D869546).

`tools/selftest-gates.ps1` is the re-runnable half: it ran five cases over a read-only copy
of one tracked baseline - `match` (positive control), `shift` (a one-pixel move must be
SHIFT), `erase` (200×200 of pixels must be DIFF), `skip` (nothing compared must be exit 77),
and a static `lint` case that reads `gates.ps1` for the declaration and the filter. Removing
the three fixes and re-running the same file flips three of the five cases to FAIL, which is
what makes it a guard rather than a rerun. Round eight §15 added three cases (`blob`, the
144-pixel rectangle a percentage budget cannot see; `capture`, a failed capture; `noscale`,
a sidecar with no scale) and wired the file into `tools/gates.ps1`, which had never run it.

### 14d. What those gates were hiding

* **The 最新作品 strip clipped every card's title and author.** `home.slint` sized the band
  at `150px + card-caption-height` (194px) with `clip: true`, while `IllustCard` is
  `column-width × clamped-aspect + caption` - 196-224px of cover alone at 140px, before the
  caption. Fixed by letting the card state its cover (`cover-height-override`, default 0px so
  the masonry grid is untouched) and having the strip state the same 150px the skeleton
  beside it always claimed: 150 + 44 + 10 (bar band) = 204px.
* **Dark-mode hover erased the bookmark glyph.** The pill's hover surface was `Theme.layer`,
  which *is* `#2b2b2b` in dark, and the unmarked glyph is the fixed `#2b2b2b`: 1.00:1. Fixed
  by keeping the surface fixed and moving hover to the border. Measured on screen, same crop,
  same pointer path: ink-coloured pixels of a 58×58 crop around the pill were **1950 idle →
  3205 hover** before and **1950 idle → 1998 hover** after. That pair needed a real pointer
  (`tools/drive.ps1` `Move-Pointer`), which is why six rounds never saw it.
* **The marked state was the next one in the queue.** With the surface fixed, `accent-fill`
  (#4cb0f5 in dark) measures 2.38:1 on that pill, so the marked glyph was unreadable before
  anyone hovered it. A fixed `Theme.pill-ink-marked` (#0071c2, 5.07:1 in both themes) is now
  what the marked state paints, and `bookmark_pill_keeps_its_glyph_visible` reads the `tint:`
  expression - themed or literal - so both inks are checked against the surface in both
  themes. It used to filter to literals, which is exactly how `Theme.accent-fill` escaped.
* **Tab does reach a card, and the page does not follow it.** 45 stops on `home`: 13 chrome,
  12 chips, first card at 27. From 27 on, every reported card rectangle is clipped to the
  viewport's bottom edge, so focus sits on artwork that is not on screen. § 13f's first item
  is corrected in place; the fix is round seven's next wave.

### 14e. Two runtime costs that were not about the UI at all

* **"Load more" rebuilt every column, every time.** `render_feed` took the previous page out
  of `self.items` with `std::mem::take` and *then* read `self.items.len()`, so the count was
  0 by construction, `append_columns` was dead code and every append ran `rebuild_columns`.
  The count and the take are now one operation (`take_existing`), the choice is a pure
  function (`plan_feed_update`) and placement is shared (`place_in_columns`), so the
  incremental path is the one that runs. Guard: a test asserts the pre-existing column
  models are `Rc::ptr_eq` after a second page arrives.
* **The profile banner was decoded at the card ceiling.** `target_width` clamps to 640px,
  which is right for a grid card and wrong for a banner drawn across the row: `1200 × 1.5`
  became 640, stretched over ~1800 physical px. `target_width_up_to` keeps the 32px tier
  rounding and takes the cap from the caller (2048 for the banner): the same request now
  decodes 1824. The cover path is unchanged, which the test pins by asserting
  `target_width(1200.0, 1.5)` is still 640.

Two smaller ones landed with them: the avatar map evicts oldest-first instead of calling
`clear()` at 256 entries (and the banner, which lives in the same map, can no longer be
evicted by avatar pressure), and the download tick takes one queue snapshot instead of two
deep clones per 500ms.

### 14f. The re-baseline, and why it is all twenty

Before the re-baseline the gate reported **20 of 20 differ**, `over = 0.0922-0.0971 %`,
`max = 118-139`. The cause is one line: the field text moved 2px onto its control's centre
line (§ 14d), and the search field lives in the command bar that every page carries - so a
two-pixel shift inside one control moves a small number of pixels on all twenty captures.
That is the argument, and it is a number, not a reassurance: identical page-specific noise
across sixteen pages that share nothing but the bar.

After `-Update` the same gate reports 20 of 20 `OK` at `over = 0.0000 %`, and the twenty
baselines are in the commit that changed the pixels.

One page's fix is *not* visible in any capture: the 最新作品 strip renders only when
`latest-state == 0`, and forced demo mode sets it to `BAND_EMPTY` (`src/app/feed/home.rs:411`),
so the strip has never been photographed - which is also why its clipped captions survived
six rounds. Its proof is arithmetic (204px box against a 194px card before, 194 ≤ 204 after)
and the skeleton beside it; § 14h keeps the capture gap on the record.

### 14g. What the new guards cannot decide

`selftest-gates.ps1` proves the comparator, not the application: a shift it reports is a
displacement of a capture, and it says nothing about *what* moved. `SHIFT` asks four one-
pixel questions; a two-pixel move is caught by the ordinary comparison instead, at a bigger
`over`. And the two accessibility-gate holes this round found are **not** closed - see § 14h.

### 14h. What is still not verified here

* **The accessibility gate still passes without its budget** (`verify-a11y.ps1:197` exits 0
  when the file is missing) and still falls back to the 1/2/3 floors for a page that is absent
  from it, and `minTabStops` is still pinned to the same 10 steps the walk takes - the reason
  § 13f was wrong. Scheduled as round seven's next item, not done in this one.
* **Focus does not scroll.** Measured, unfixed (§ 14d). The focus gate still walks the ten
  chrome stops because that is all it has ever walked.
* **The download page has never been seen populated**, the recent-search row still breaks at a
  hard-coded four chips, and the state block is still 8px off centre.
* **Hover has no gate yet**, although it is now measurable: § 14d's pill pair was taken with
  `tools/drive.ps1` by hand and would make a cheap gate.
* A real screen reader reading the tree § 12a produced; the Linux and macOS input paths.

---

## 15. Round eight: gates that could not fail, or could not run

Round seven closed its plan believing five gates could not decide. Four were still open at the
start of round eight, one of them could not *run* at all, and `tools/gates.ps1 -Ui` exited 1 on
a tree that had no red test. That gate - the DPI ladder - is the one an earlier round told the
reader to consult about a 100 % host.

### 15a. Eleven gates, and the entry point that ran four of them

The DPI gate did not measure the app: it measured a PowerShell re-implementation of
`images::target_width`, and the only link to the Rust was two regexes over `src/images.rs`. Those
regexes stopped matching (`div_ceil(TIER_STEP) * TIER_STEP`, then `tier_up(physical).clamp(...)`),
so `verify-dpi.ps1` threw at line 78 before measuring anything. It now reads the two constants
*by name* - `Read-ImagesConstant` - the way the file already read layout constants, and exits 77
when a piece cannot be parsed instead of throwing. A gate that cannot be built is a gate that
cannot run, which is a third verdict, not a crash.

That was the visible half. The rest:

* A **failed capture was invisible** - `shot.ps1`'s exit code was discarded and `target/visual/
  current` was never cleared, so the gate diffed the previous run's PNG and reported *today's*
  render as matching it. `-Exe target\debug\nope.exe` printed twenty `OK` rows and exited 0.
* A **partial skip was a clean run**: 1 judged + 19 skipped exited 0, which on a 100 % or 125 %
  host is the normal outcome.
* The **diff budget** admitted 354 pixels at 1536x1152 and the message said "0.02 %". There is
  now an absolute pixel ceiling beside the percentage, and `selftest-gates.ps1` carries the case
  the percentage cannot see: a 12x12 rectangle, 144 px, 0.0057 % - `under the 0.02 % budget but
  over the 96-pixel ceiling`.
* **Zero checks was a pass** (`verify-layout.ps1` printed "0 checks, 0 not OK" and exited 0), and
  a **partial DPI ladder** claimed the ladder held. Both are 77 now.
* The **accessibility gate passed without its budget** and fell back to the 1/2/3 floors for a
  page absent from it. Both fail now, naming the page.
* The **Tab walk stopped at its own sample size** and then pinned that sample as the requirement.
  It now walks until the (Name, Type) pair repeats and writes the count it measured:
  home 10→51, discovery 10→38, ranking 10→71, search 10→21, bookmarks 10→24, history 10→14,
  downloads 10→18, settings 10→23. `-Update` only ratchets *up*, so following the fix
  instructions on a degraded build can no longer record the degradation as the floor.
* Three verifiers - `verify-icons`, `verify-layout`, `selftest-gates` - were invoked by nothing
  while the README described all three as gates, and the file named "one entry point for every
  gate" ran four of seven. It runs twelve rows now, `--locked` matches CI's three cargo jobs, and
  the negative experiment for the comparator's pixel ceiling was re-run against a copy of the old
  comparator: the 144 px blob passes it (`exit=0 … over= 0.0057%`) and fails the new one.

`tools/gates.ps1 -Ui` at the end of this round: **12 gates, 0 failed, 0 skipped, 680.9 s, exit 0**
- and it includes the DPI gate, which had not run for a round.

### 15b. Twelve UI defects, one of them a button that could not be clicked

Every one of these was provable from the declarations; none was visible in a capture of the
page's resting state.

* **The detail page's 「重新载入」 was dead.** The failure block was declared *before* the viewer's
  full-size `TouchArea`, and Slint hit-tests in reverse declaration order - the law this file
  states three times elsewhere. The block now sits after the hit surface, before the floating
  controls, and only the two state overlays moved.
* **The ranking badge's digits were 2.38:1 in dark mode**: `#ffffff` on `accent-fill`. The theme
  states the number and the pair; the ink is `on-accent` now (8.01:1), and the literal survives
  only on the `#000000a6` overlay it must not follow.
* **Four controls in the 44 px detail toolbar sat 6-13 px above its centre line**, and five 28 px
  buttons sat 4 px above the centre of the 36 px floating pill: a fixed-size child of a layout is
  aligned to the start of the cross axis, which is the law `downloads.slint` already documents.
  `row-height` claims the row height and the control centres itself.
* **`UserCard`'s text ran under its own preview** - 84 px of every name, handle and comment
  painted on artwork at half opacity. The column reserves it now.
* **The demo switch and the theme selector were not Tab stops at all**: neither declared a
  `FocusScope`, and Slint focuses nothing else. They also announced as an unnamed switch and an
  unnamed radio group because neither call site passed `label:`. Both take focus, both are named,
  and the ring is opacity so the resting pixels do not move.
* **The account affordance in the command bar** - the only path to your own page, and, signed
  out, to the sign-in flow - was a bare `TouchArea` with no role and no scope. One `open-account()`
  function now serves the pointer and Tab+Space/Enter, so the two cannot disagree.
* **The cancelled download badge was 4.32:1** (`text-tertiary` on `layer-sunken`); it is
  `text-secondary` (5.58:1) and the pair is in the WCAG table, which had measured `text-tertiary`
  against `canvas`, `layer` and `layer-alt` only.
* **The 最新作品 band drew nothing in its empty state**, `recommend-users-message` was plumbed and
  never read (a failed artist request made the whole band vanish), the skeleton always drew four
  columns at a 3-column window (`4 x 195.33 + 3 x 16 = 829 px` into 618 px with the horizontal
  scrollbar off), `InfoBar` counted its body's padding twice, and the thumbnail strip put its
  scrollbar on 2 px of the thumbnails.

### 15c. Three guards, and the one exception

`uiaudit` gained two rules and the WCAG table gained one pair. Both rules were proved against the
defect they describe: planting `color: #ffffff` back into the badge made
`text_drawn_on_the_accent_fill_uses_the_ink_token` fail and name `components.slint:1459:
IllustCard`; `every_interactive_component_can_take_focus` failed on its first run and named
`BookmarkPill`, which is the one interactive component still without a scope.

That one is an exception with a reason rather than a hole: a Tab stop per card would make a
40-item grid 80 stops *before* roving focus exists, and the same action already has a keyboard
path on the detail page. The exception is written inside the rule, next to the item in phase 2
that removes the need for it.

The accent rule began as "any mention of `accent-fill` plus any `Text`", which flagged five
components that ink nothing on it (`TextField` sets it as a focus border, `NavItem` as a selection
bar). Narrowed twice - to a `background:` fill, then to a *literal* ink on that fill - it is the
rule that can fail on the defect and only on the defect.

### 15d. `record_view`, the append path, and a temp file two writers could share

Round seven made `plan_feed_update` able to extend instead of rebuild, and closed it at the
generic pager. Two callers were never converted: the profile's "load more" cloned the whole
accumulated list and passed `append = false`, and the home feed did the same with an extra clone.
Both reach the extend branch now. The same audit found a `RefCell` borrow of `App` held across
the modal folder picker - and `picker.rs` passes a null owner, so the main window stays clickable
while the dialog pumps messages - covers that were pruned only when the feed was replaced, a
scroll table that scanned linearly and grew one entry per route ever visited, avatar arrivals
that rebuilt whole bands, a DPAPI seal plus fsync run on the painting thread, and two failure
paths that reported a network error as "this artist has no public works".

In the crates: the download's `.part` name was a function of the destination alone, so two pages
rendering one filename wrote the same temp file and both renamed it - the published file could be
two images interleaved. It is per task now, the write fsyncs before the rename, a zero-length
destination is a miss instead of "already downloaded", the disk tier's temp is per writer, a
cancelled task releases its semaphore permit immediately (`select!` on the fetch), POST retries
only replay a 429/503 that names a wait, `enforce_budget` subtracts what it deleted instead of
storing a total, `seq` has a covering index, and remote image bytes are decoded under
`image::Limits` (32 MiB body, 16384 px edge, 256 MiB of pixels) instead of unbounded.

### 15e. The re-baseline, and what it named

Seven of twenty captures moved. Each is explained, and each explanation is a fix, not a wobble:

| Capture | Pixels | Why |
| --- | --- | --- |
| settings (light) | 242 319 (13.69 %) | `InfoBar` lost its second padding, so every card below it moved up 16 px |
| settings (dark) | 226 774 (12.82 %) | the same |
| detail (light) | 3 304 (0.19 %) | toolbar row heights, the strip's own band, the pill's five buttons |
| detail (dark) | 3 435 (0.19 %) | the same |
| detail-error (light) | 1 272 (0.07 %) | the same two rows, seen through the error state |
| detail-error (dark) | 1 283 (0.07 %) | the same |
| ranking (dark) | 501 (0.03 %) | the badge's ink, `#ffffff` → `on-accent` (light is unchanged: `on-accent` *is* white there) |

One thing the comparison found that is not a fix: **the settings capture contains a live number**.
`缓存占用 · 磁盘 403 MB` in the old baseline reads `432 MB` now, because the disk tier grew while
the round ran. A tracked baseline that depends on the local cache is a baseline that will drift
on a host doing nothing. Recorded here rather than papered over; the fix is a fixture seam of the
same shape as §2.2's `U13`, and it is scheduled with it.

### 15f. What the new guards cannot decide

* The Tab walk stops when a `(Name, Type)` pair repeats, so two distinct stops with one name end
  the walk early: the count is a **lower bound** on real stops, not an element census. It is also
  why four pages report *more* stops than UI Automation calls focusable (Slint `FocusScope`s are
  not marked `IsKeyboardFocusable`) and four report *fewer* (UIA marks nodes Tab never lands on).
  Both directions are measured; neither is a bug in the gate.
* The accent-ink rule sees a *literal*. A token ink on an accent-filled surface can still be the
  wrong token, and no static rule here can decide that.
* `gates.ps1` still short-circuits at the first FAIL, so a run with one failure prints a partial
  table. Splitting the jobs is why the first failure names itself; printing every row regardless
  is a separate change.
* The 最新作品 band's empty and error states, and the discovery artist band's error state, have no
  capture - the pixel gate only sees the states the demo catalogue reaches.
* `verify-focus.ps1` still writes its captures and never compares them; they are untracked now
  (34 files, 12 890 992 B - **52.0 %** of every tracked byte in this repository), so a normal run
  no longer dirties the worktree, but nothing reads them either.
* `verify-icons.ps1` still throws when `shot.ps1` writes no sidecar; only its "not measured" path
  became 77.

### 15g. What is still not verified here

A real screen reader reading the tree § 12a produced; the Linux and macOS input paths; a
pointer-driven drag gesture; whether `SegmentedControl`'s one-way binding survives its own first
click; whether `visible: false` removes a child from a layout (two `TaskRow` buttons depend on
it); whether R12's borrow is live or only latent, which needs one run with the folder dialog open
and a click on the rail; and the settings capture's cache figure, until it has a fixture.

---

## 16. Round nine: five surfaces, eighteen declarations, and a gate that counted the wrong thing

### 16a. The gate could not tell "exists" from "reached" — and my own hypothesis was wrong

The plan for this round claimed the accessibility walk ended early because its stop
identity was a `(Name, Type)` pair: two controls sharing a name would repeat a pair and
read as a cycle. Replaying the pair-only rule against a recorded walk reproduced every
count exactly — 51, 38, 71, 21, 24, 14, 18, 26 — so the hypothesis was false and the
plan was corrected (`3b908a2`).

What the walk *could* not see was that three controls per page were focusable and never
reached at all. `TextField` published its role and label from the outer frame while the
focusable node was the `TextInput` inside it, so the tree carried two Edits with the same
name and the second could never be Tabbed to. The account affordance published its role
on the `TouchArea` while the focusable node was the `FocusScope` beside it. The settings
session field had the same shape. In total: seventeen failures across eight pages, none
of which any earlier gate could name, because no gate compared what the tree says is
focusable with what the walk reached.

The guard is now `Compare-StopCoverage` in `tools/walk-lib.ps1`: identity is
`(Name, Type, rectangle)`, then a second pass on the pair alone so a card that scrolls
when it takes focus is not called unreachable for moving — and the pair pass cannot hide
a duplicate, because the exact pass has already spent one visit per rectangle.

### 16b. U5: the detail page's vertical budget, written down

At a 600 px window the shell spends 48 (top bar) + 26 (status bar), leaving 526. The
page's furniture is 44 (toolbar) + 1 (rule) + 68 (thumbnail strip) = 113, so the body
gets 413. The narrow layout draws a viewer of `min(526 × 0.55, aspect cap)` = 289 px and
a metadata column whose intrinsic height is about 600: 889 px of content in a 413 px box.
That is the peephole: description, tags and the stats row were unreachable, and the
column below them was empty canvas.

Only the body scrolls now; the toolbar and the strip stay. `body-height` is exposed so
the wide layout can size the viewer from the space actually left (1260 × 800: 613, viewer
589 — pixel-identical to before), and the narrow layout uses a shared `InfoBody` card
instead of an `InfoPanel` that scrolls inside a page that scrolls. Two Slint limits made
this the shape it is: a ternary cannot mix a length and `100%`, and `min-height:
natural ? page.body-height : 160px` is a binding loop (`content-height → body.height →
page.body-height → min-height`).

At 1024×768 the page takes the narrow layout (`available-width = 792 < 900`), which is why
that baseline moves by 22 %: the metadata is a card now and the page grows an overlay
scrollbar. The poster area is 0 % different and the wide layout is pixel-identical — that
is the evidence that the shift is U5 and not a gutter.

### 16c. U6: the sign of a scroll offset, and why the bug had never been visible

The panel scrolled its content *down*: `became-active` wrote a positive `content-y`, so
the ninth row was pushed 213 px below the panel instead of being brought into view. In
this shell the negative direction is downward — `PageScroll.scrolled => root.scrolled(
root.scroll-y)` stores a negative offset. It is a negative offset now, clamped on both
sides: the lower bound is the content's own overflow (so a short list never leaves a
negative residue) and the upper bound is zero. The arithmetic is 34 px per row, 5 px of
body padding, 64 px of lookahead.

The reason this could not be seen for eight rounds is worth more than the fix: the
suggestion model was rebuilt on every publish, so the row elements were new each time and
the panel's `changed is-active` handler never fired. Patching the model in place (the
same shape `download_rows` already uses) is what makes an active row observable at all.

### 16d. A 32 px control that moved every page below it

The select-mode command went into a header row whose height came from a 20 px title. A
32 px button made the row 32 px, and every pixel of the masonry below it moved down ten
logical px: the visual gate reported discovery 20.87 % over, rows 107..1112, every row
about 1113 px wide — a page-shaped diff, not a control-shaped one. It is a 26 px compact
pill now, sharing one cell with the count label in a zero-height cell so the row height
is the title's again. Re-measured: 0.08 % (1378 px) on discovery, 1415 on ranking, 1342 on
bookmarks, and all of it inside rows 106..127 — the header row itself.

### 16e. What the round built

Five surfaces the shell said it did not have: playback in the ugoira viewer (decoded once
at the drawn width, 64 MB budget, frames derived from a monotonic clock); multi-select
with batch download and batch bookmark on every grid page; the `Ctrl+P` palette with a
parser for artwork and user URLs; bookmark visibility with a split button and re-tagging;
and a download queue that pauses, resumes and reports throughput and an ETA per row.

One trade-off, taken deliberately: roving focus inside a chip row is not expressible in
Slint 1.18. A `FocusScope` with `enabled: false` still reports `IsKeyboardFocusable`, so
the walk saw seven chips it could not reach. Every chip is a real stop now — more Tab
presses, but reachable, and the reason is written on `TagChip` rather than discovered
again next round.

### 16f. The pixel gate has five states it never had

U6 is the argument for this: a panel nothing photographed was wrong for eight rounds.
`PIXIV_DESKTOP_FIXTURE` — named in three plans and implemented in none — now puts the
shell in a named state (ugoira, selection, palette, suggestion, downloads), `tools/
fixture.ps1` maps a value to a page and the captures it owes, and an unknown value exits
77 rather than passing. `verify-visual.ps1` captures those states as ordinary pages, so
the round's baselines are 30, ten of them new.

### 16g. What the new guards can and cannot decide

The coverage assertion decides reachability; it cannot decide whether a control is worth
reaching, and it counts `Button`, `ListItem` and `Edit` — eight to ten `CheckBox` nodes per
page are focusable and still not walked, which is recorded here rather than asserted. The
budget is a ratchet, and one floor was lowered by hand once: settings `minFocusable`
20 → 19, because the duplicate Edit node is gone and a floor above what the tree can
produce is unsatisfiable rather than a guard.

### 16h. What is still not verified here

Focus-follow scrolling is not wired: the seam exists (`IllustCard.gained-focus` →
`card-focused(id)` → the window), but the offset needs the card's content-space y, which
only the page can compute, and a shared component for that is next round's first item —
README limit 10 still says focus does not scroll. Whether Windows delivers `Ctrl`/
`Shift` inside `TouchArea.pointer-event` was never measured; the page's own toggle is
why the feature does not depend on the answer. A real screen reader reading this tree;
an IME composing inside the palette; batch download and batch bookmark against a live
account (only the cap, the de-duplication and the summary are tested); and Range resume,
which this round deliberately does not implement.

---

## 17. Round ten — typography as a system, and four Slint geometry traps

### 17a. The typography layer

Every text step now has a name: `TextRole` (Caption 11 · Small 12 · Body 14 · Subhead 14 ·
Title 16 · Page 20 · Display 24) and `Emphasis` (Regular/Medium/Semibold/Bold).
`components.slint` exposes exactly two text elements:

* `Label` — one line, elided, natural line height, `vertical-alignment: center`.
  It is the only supported way to put text in a row, because a `Text` handed a height
  puts its first line at the *top* of that box; that is the "label sits 3-6 px above the
  number beside it" class of defect, and it cannot be fixed by adding an offset.
* `BodyText` — wrapped, `vertical-alignment: top`, and the only place a
  `line-height-factor` is applied (`Theme.line-body` = 1.5, `line-small` = 1.35).

73 call sites were converted mechanically from `Text { font-size: Theme.font-…; }` to the
two primitives. A single-line label keeps the natural line height on purpose: a
non-default `line-height-factor` moves a `Text` onto Slint's complex-text path, and the
feed draws ~200 single-line labels for every paragraph it draws.

Two gates hold the layer:

* `text_in_a_horizontal_row_states_its_cross_axis_alignment` — a `Text` that is a direct
  child of a `HorizontalLayout` must say where its line goes (`vertical-alignment` or
  `y`). Text in a `VerticalLayout` is exempt: there the vertical axis is the layout's
  main axis.
* `reader_bounds_match_the_preference_model` — the reader's slider bounds on the Slint
  side and the `MIN_/MAX_` constants that actually clamp the stored value are compared
  across the two languages, because the compiler cannot see both.

Writing that second gate found a bug in itself worth recording: reading "the first
number on the declaration line" of `pub const MIN_FONT_SIZE: f32 = 14.0;` returns 32,
from the `f32`. It reads the last number now.

### 17b. Four Slint geometry traps this round paid for

All four were found by measuring, and all four look correct in review:

1. **A conditional element at the top level of a non-layout component collapses.**
   `if cond : Rectangle { width: 100%; height: 44px; … }` as a direct child of the page
   root paints nothing at all — not even its children — because the conditional wrapper
   is sized from its single child and the child's `100%` closes the loop at zero. Inside a
   layout the same construct is fine, which is why every page's `if PageScroll` works and
   the reader's toolbar did not. Fixed with a plain element and `visible: cond`, which is
   what `main.slint` does for pages.
2. **A child of a plain `Rectangle` is not stretched.** `width: 100%` is only meaningful
   against a parent whose own size is known; the reader's scroll wrapper and its
   `ScrollView` both said 100% and both came out ~378 px tall inside a 764 px page.
   Explicit `width: root.width; height: root.height` is the idiom that works.
3. `ScrollView.content-height` is an *input* to the flickable and reads 0 until something
   sets it. The reader's scroll range is `column.height - sv.visible-height`, and its
   progress is a binding on that, not a value written from the `scrolled` handler — which
   is why the readout stayed at 0% after a restore until the user scrolled.
4. A restore must converge on the *ratio*, not on the offset: the column keeps growing
   for a few frames, so an offset that "sticks" on the first tick lands at the wrong
   fraction (measured: 18% after asking for 55%).

### 17c. What the reader shares with the rest of the app

Same `Theme` colours, same `Label`/`BodyText` primitives, same `PageScroll` idiom for the
novels list, same `StateView` for loading/error, same `InfoBar` for a failed chapter list,
same segmented control and toggle switches in the settings panel. The reading surface
is the one place with its own palette (`ReaderTheme`), because a sepia page is not "the
app in light mode".

### 17d. What is still not verified here

The typography layer is held by two gates that read declarations: a `Text` in a row says
where its line goes, and the reader's slider bounds agree with the constants that clamp the
stored value. Neither photographs a converted label, and the reader is in no pixel baseline
at all - `tools/verify-visual.ps1` captures fifteen states and none of them is a reader
state, which is G33 of `docs/OPTIMIZATION-PLAN-R11.md`. All four geometry traps were
measured at 150 % on this host; the reader's scroll range is
`column.height - sv.visible-height` in physical pixels, and the ladder above 150 % has
been on this record as unmeasured since §10. The reader's keys are confirmed by pressing
them, not measured by a gate: `PageDown`, `PageUp`, `Home`, `End` and a bare `A`
are handled by the shell's shortcut scope while `Destination.NovelReader` is on screen
(`ui/main.slint:723-735`), and `tools/verify-focus.ps1` walks home, detail and settings,
so no gate has ever measured a stop on the reader. The three novel destinations have no
measured accessibility floor yet either: they join `tools/verify-a11y.ps1`'s list with the
one page set (`tools/page-set.ps1`, G32) in round eleven, and the floors are pinned by
that round's first measured run.

---

## Appendix: round-one limits (kept for the record)

### Not verified in round one

**Pointer-driven interaction** (click, hover, wheel, drag, keyboard focus order)
was exercised by code path and unit test only: this environment has no input
automation. Pages were reached through `PIXIV_DESKTOP_START_PAGE` and
`PIXIV_DESKTOP_START_ILLUST`.

**The DPI ladder.** Only 150% is available on this host. Layout maths is
scale-independent (Slint reports logical px) and the image pipeline's
`target_width()` is unit tested at 1.0/1.5/2.0, but 100%, 125% and 200% were not
exercised on a real display.

**Bookmark / follow against a live session** - unchanged from round one: no
credential was available.
