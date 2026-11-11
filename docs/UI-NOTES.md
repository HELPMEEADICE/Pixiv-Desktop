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

## 3. Verification

### Automated

```
cargo fmt --all -- --check      # clean
cargo test --workspace          # 195 tests pass
cargo check --workspace --all-targets   # clean, no Slint binding-loop warnings
RUSTC_WORKSPACE_WRAPPER=C:\msys64\ucrt64\bin\clippy-driver.exe \
  cargo check --workspace --all-targets # clean
cargo build --release           # succeeds
```

`cargo clippy` itself cannot start in this MSYS2 + MSVC environment (it
re-executes an MSVC toolchain and dies with `linker link.exe not found` before
reaching project code — reproducible on dependency-only crates). The
`clippy-driver` wrapper above is the documented equivalent gate and was verified
to emit real lints, not a plain `rustc` check.

New tests in this change:

- `src/layout.rs` — column count and card width at 640/800/1024/1140/1280/1920/2560,
  "no blank strip wider than one card", masonry balance vs round-robin, aspect
  clamping, per-column order preservation.
- `src/zoom.rs` — fit scale, wheel bounds, cursor anchor invariance (the anchored
  point stays under the cursor to 1e-3 px), pan clamping.
- `src/images.rs` — request de-duplication, in-flight suppression, retry after
  failure, reading order, decode target width.
- `src/route.rs` — back/forward correctness, forward invalidation, bounded stack.
- `src/session.rs` — round-trip, malformed input, DPAPI seal/unseal, and a check
  that the cookie string does not appear in the file on disk.

### Visual

`tools/capture.ps1` now sets the DPI awareness of the capture process before
grabbing (without it a 1710×1329 window at 150 % DPI comes back as a 1140×886
bitmap showing only the top-left two thirds), resizes the *client* area to an
exact logical size with `SetWindowPos`, and uses `PrintWindow` with
`PW_RENDERFULLCONTENT`.

Captured and inspected at 640×480: home, discovery, ranking, search, bookmarks,
history, downloads, settings, detail. Confirmed in those captures:

- content fills the client width; no blank strip, no horizontal scrollbar;
- 3 columns at 640 px of window (compact rail), cards unclipped, aspect correct;
- badges sit inside the cover, captions directly under the image;
- settings cards fit, descriptions wrap, no clipped control;
- headers are left-aligned with the item count on the right;
- tag strip keeps full chip labels and scrolls horizontally by design.

### Not verified here

**Full-height painting above ~480 px of client height.** `Window::size()`,
`scale_factor()` and the Slint-side shell height all report the correct values
(traced: 1140×886 window → `shell_h = 886` → body 898 px → 4 columns × 212.5 =
898, no leftover), and the same binary paints the whole shell at 640×480, but at
larger sizes this VM's software renderer paints only the top ~460 px of the client
area and leaves the rest blank. Forcing `SLINT_BACKEND=winit-software` reproduces
it; `winit-femtovg` cannot start here (no OpenGL). It is a rendering artefact of
this environment, not a layout result — but it means the *wide* detail layout
(viewer + info side by side) and the >900 px card counts were verified
numerically, not visually.

**Interactions requiring real input** (click, hover, wheel, drag) were verified by
code path and unit test, not by driving the window: this environment has no
input-automation path, and every page was exercised through the
`PIXIV_DESKTOP_START_PAGE` deep link instead.

**DPI ladder.** Only 150 % was available (the host display). The layout maths is
scale-independent (Slint reports logical px), and `images::target_width()` is
unit tested at 1.0/1.5/2.0, but 100 %, 125 % and 200 % were not exercised on a
real display.
