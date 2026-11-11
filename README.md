# Pixiv Desktop

A native desktop client for [Pixiv](https://www.pixiv.net/), built with **Rust + Slint**.
No Electron, no CEF, no Tauri, no WebView — the UI is compiled Slint drawing directly
through a GPU/software renderer.

Visual target: **70 % Pixiv web layout and information architecture + 30 % Windows 11
WinUI 3 / Fluent Design** desktop idiom. Pixiv's blue accent, white canvas, dense
illustration-first grid and compact type scale survive; Fluent contributes layered
surfaces, 1 px borders, 6–8 px radii, a real light/dark pairing and a NavigationView-style rail.

![Home](docs/screenshots/03-home-grid.png)

---

## 1. What works

| Area | Status |
| --- | --- |
| Native window, Fluent shell, collapsible nav rail, responsive reflow | **Implemented** |
| Light / dark theme, switchable at runtime, persisted | **Implemented** |
| Illustration grid with true masonry (per-card aspect ratio, no cropping) | **Implemented** |
| Demo Mode with procedurally generated artwork | **Implemented** |
| Real Pixiv data: illustration detail, page list, search, ranking, artist work ids, tag metadata | **Implemented** (verified against the live API) |
| Zoomable image viewer: wheel zoom anchored at cursor, drag pan, double-click toggle, fit / 1:1, fullscreen | **Implemented** |
| Image pipeline: priority queue, single-flight, memory LRU + disk tier, generated fallbacks | **Implemented** |
| Local SQLite: search history, view history, settings, download records | **Implemented** |
| Download queue: bounded concurrency, filename templates, cancel / retry, duplicate detection, atomic writes | **Implemented** |
| Ugoira decode + timing + GIF export | **Implemented** (library + tests; see limits) |
| Session login via system browser + `PHPSESSID` | **Implemented** |
| Bookmark / follow mutations | **Implemented, unverified against a live session** |
| Novel reader | **Placeholder** (structured page, opens the official page) |
| Comments, "related works", `/ajax/discovery/artworks` | **Not available** — no usable endpoint shape found |

### Honest limits

These are real, not hedging. They are listed because a client that quietly guesses is
worse than one that says what it does not know.

1. **Pixiv has no public third-party OAuth registration.** There is no client
   credential a self-built desktop client can legitimately hold. The working credential
   is therefore the session cookie from the user's own browser. This app never renders a
   login form and never sees a password.
2. **`cargo clippy` cannot run in this build environment.** MSYS2's `cargo-clippy`
   re-executes an MSVC rustup toolchain and dies with `linker link.exe not found` before
   reaching any project code; this reproduces on dependency-only crates. The equivalent
   gate that *does* run, and was used for every crate here, is:
   ```
   $env:RUSTC_WORKSPACE_WRAPPER="C:\msys64\ucrt64\bin\clippy-driver.exe"
   cargo check --workspace --all-targets
   ```
   This was verified to emit genuine clippy lints (a probe crate produced
   `clippy::bool_comparison` and `clippy::unnecessary_first_then_check`), not a plain
   `rustc` check.
3. **Bookmark and follow POSTs are implemented from observed request shapes but could
   not be confirmed end-to-end**, because no credential was available to verify a success
   response. They never fake success: an error surfaces as an error, and a bookmark can
   only change state after the server confirms it.
4. **`/ajax/discovery/artworks`, related works and comments returned no usable shape**
   (400/404 for every parameter combination tried). Discovery is therefore composed from
   ranking + search + tag metadata rather than guessed. See `docs/API-NOTES.md`.
5. **No native folder picker.** Choosing the download directory opens the current folder
   in Explorer; the directory itself comes from the OS Pictures folder. A proper picker
   needs an extra dialog crate, which was out of scope here.
6. **The ugoira player is not wired to the detail page UI yet.** The codec, frame timing
   and GIF export are implemented and tested in `crates/pixiv-ugoira`; the UI currently
   shows ugoira works as still images.
7. Ugoira export writes **animated GIF only** (WebP animation encoding is not in the
   `image` crate).
8. **On a desktop narrower than the window's `min-width`, the right edge of the UI is
   clipped.** This was measured, not guessed: Slint lays out at its `min-width` /
   `preferred-width` while the window manager draws into a smaller client area, so the
   chrome past that width is cut off. `preferred-width` is 1000 px and `min-width` 380 px,
   so this only appears on very small / heavily scaled displays — the development VM
   reports a ~333 logical-px desktop, where it is visible. The fix belongs in a
   `GetClientRect` round-trip that pushes the real size back into Slint; it is not
   implemented yet, and the screenshots in `docs/screenshots/` were taken on that VM, so
   the right-hand controls are cropped in them.

---

## 2. Build and run

Requires a Rust toolchain (developed on 1.98.1) and, on Windows, a C toolchain for
`rusqlite`'s bundled SQLite (UCRT64 gcc works).

```powershell
cargo build --release
cargo run --release
```

The debug binary also runs and is what the screenshots above were taken with.

### Verification gates

```powershell
cargo fmt --all -- --check
cargo test --workspace                       # 159 tests
cargo build --release

# lint (see limit 2 for why not `cargo clippy`)
$env:RUSTC_WORKSPACE_WRAPPER="C:\msys64\ucrt64\bin\clippy-driver.exe"
cargo check --workspace --all-targets
```

---

## 3. Signing in

Pixiv issues no OAuth client credentials to third-party desktop clients, so this client
reuses the session you already have:

1. Click **在浏览器中登录** in Settings — it opens `pixiv.net` in your **system browser**.
2. Sign in there as usual.
3. Open DevTools → Application → Cookies → `https://www.pixiv.net` → copy the value of
   `PHPSESSID`.
4. Paste it into the session field and press **保存会话**.

The app validates the cookie against a real authenticated endpoint before storing it, and
writes it to a dedicated file in the app data directory (mode `0600` on Unix). It is
never written to SQLite and never logged. **退出登录** deletes the file and reverts to
Demo Mode.

Nothing about the app is required to sign in: detail pages, pages, search, ranking and
artist work lists all work unauthenticated.

---

## 4. Demo Mode

Demo Mode exists so the UI is fully explorable with no account and no network.

* Every demo URL uses the `demo://` scheme, so any code path reaching the network with
  demo data is an obvious bug.
* Every demo title carries a `[DEMO]` prefix, the top bar shows a **DEMO 演示数据** badge
  and the status bar reads **演示模式（本地数据）**.
* Artwork is generated procedurally at runtime from a seeded PRNG — no third-party imagery
  is bundled, so there is no licensing question.
* Demo Mode is a separate data source; it never parses or imitates a Pixiv response.

---

## 5. Architecture

```
pixiv-desktop      UI (Slint) + application wiring
├── pixiv-demo     demo catalogue + procedural artwork
├── pixiv-download download queue, filename templates
├── pixiv-image    priority cache: memory LRU + disk tier + single-flight
├── pixiv-store    SQLite persistence
├── pixiv-api      HTTP transport: session, retry, rate limiting
└── pixiv-types    wire + domain contracts (no I/O, no UI)
```

Dependencies point one way; `pixiv-types` depends on nothing but `serde`.

**Why a data-contract crate.** Pixiv's JSON is inconsistent in ways that are only
discoverable by observing it: ids are strings on `ajax/*` but numbers on `ranking.php`;
`illust_detail.tags` is a single object while other payloads send arrays; `ranking.php`
sends `"illust_type":"0"` as a string; `mime_type` is snake_case on one endpoint only;
`createDate` is sometimes an object. All of that is normalised exactly once, in
`pixiv-types`, and pinned by tests that parse **real captured responses** in `fixtures/`.
When Pixiv changes a payload, those tests fail before the UI does.

**Threading.** The Slint event loop owns the main thread. One tokio runtime handles
network, decode and database work. Slint models are `!Send`, so background tasks return
only plain bytes and the UI thread builds `slint::Image` values — no pointer sharing, no
locks around models.

**Why native window decorations.** Slint can draw a custom title bar with
`no-frame: true`, but dragging, Snap Layouts, per-monitor DPI and accessibility then all
have to be rebuilt on `Window::set_position`, which is jittery and loses Win11 snapping.
The native frame is kept and Fluent chrome is drawn inside it.

**Icons.** Generated by `tools/fetch_icons.ps1` from
[Microsoft Fluent UI System Icons](https://github.com/microsoft/fluentui-system-icons)
(MIT). Only the path geometry is embedded, emitted as Slint `Path` elements, so icons
inherit theme colour and stay crisp at any DPI. No emoji, no hand-drawn substitutes.

Full detail: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) ·
API observations: [`docs/API-NOTES.md`](docs/API-NOTES.md)

---

## 6. Keyboard shortcuts

| Key | Action |
| --- | --- |
| `Ctrl+F` | Focus search |
| `Alt+←` / `Alt+→` | Back / forward through page history |
| wheel | Zoom the viewer, anchored at the cursor |
| double-click | Toggle zoom at the cursor |
| `F` | Fit to window |
| `1` | Actual size |
| `F11` | Fullscreen |

---

## 7. Legal

This is an unofficial client and is not affiliated with Pixiv Inc. It reads data the
signed-in user can already view in their own account, for personal browsing and offline
use. It deliberately contains **no bulk-scraping facilities** (no artist-wide crawlers, no
pagination bots) and no access-control bypass. Respect the rights of the artists whose
work you view.
