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

Each of these had been enforced by review alone and each had already been broken:

- `raw_glyphs_exist_only_inside_the_glyph_containers` — a bare `FluentGlyph` is
  legal only inside `IconPresenter`/`IconSlot`. Converting the remaining call
  sites removed four more copies of the centring arithmetic.
- `every_referenced_icon_name_is_generated` — an unknown glyph name renders
  *nothing* (no error, no empty box), so a typo silently deletes an icon.
- `pages_take_colours_from_the_theme` — only artwork overlays may be literal.
  This found three real leaks (the tooltip's inverted surface, the viewer
  backdrop, the floating control bar), now `Theme.tip-*` / `Theme.viewer-*`.

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
- An empty search keyword no longer returns an unrelated default listing.
- `clear-cache` walks the disk tier on the runtime; it used to freeze the window.

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

## 10. What is still not verified here

- **Pointer-driven interaction.** No input automation on this host: hover,
  press, drag, wheel and keyboard focus order are exercised by code path and unit
  test only. Pages are reached through `PIXIV_DESKTOP_START_PAGE` and
  `PIXIV_DESKTOP_START_ILLUST`.
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