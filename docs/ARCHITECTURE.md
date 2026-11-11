# Pixiv Desktop — Architecture

A native Windows-first desktop client for Pixiv, written in Rust with Slint.
This document records the decisions behind the structure, not a tour of the files.

## 1. Shape of the system

```
pixiv-desktop (bin + Slint UI)          ← ui/, src/
├── pixiv-demo      demo catalogue + procedural artwork (offline)
├── pixiv-download  bounded-concurrency download queue, filename templates
├── pixiv-image     priority image cache: memory LRU + disk tier + single-flight
├── pixiv-store     SQLite: history, settings, download records
├── pixiv-api       HTTP transport: sessions, retry, rate limiting
└── pixiv-types     wire + domain contracts (no I/O, no UI)
```

Dependencies point one way. `pixiv-types` depends on nothing but `serde`; the UI
depends on everything; no library crate depends on the UI.

The reason for six crates rather than one module tree is testability and
ownership: the transport layer can be tested against captured fixtures without
starting a UI, and the image cache can be tested with generated pixels without a
network. A single monolithic crate would have forced every concern into every
test run.

## 2. Data contracts live in one place

`pixiv-types` is split into `wire` and `domain`:

- **`wire`** mirrors Pixiv's JSON *verbatim*, including its inconsistencies.
- **`domain`** is what the rest of the application uses.

Everything irregular is resolved once, at that boundary:

| Irregularity | Where it is resolved |
| --- | --- |
| ids are strings on `ajax/*`, numbers on `ranking.php` | `Id` accepts both |
| `illust_detail.tags` is one object; other payloads send arrays | `tag_groups` deserialiser |
| `ranking.php` sends `"illust_type":"0"` as a *string* | `wire::flex` coercers |
| `createDate` is a string in one payload, an object in another | `wire::flex::string` |
| `mime_type` is snake_case on `ugoira_meta`, camelCase elsewhere | explicit `rename` |
| three different names for the smallest thumbnail tier | `IllustUrls::grid()` |

Those mappings are pinned by tests in `crates/pixiv-types/tests/fixtures.rs`,
which parse **real captured responses** from `fixtures/`. When Pixiv changes a
payload, that test fails before the UI does.

## 3. Authentication

Pixiv issues no OAuth client credentials to third-party desktop clients. The
only path that yields real data without embedding someone else's `client_secret`
is to reuse the session the user already has in their own browser.

Flow:

1. The user signs in at `pixiv.net` in their **system browser** (the app opens it
   with `open`). The application never renders a login form and never sees a
   password.
2. The user supplies the resulting `PHPSESSID` cookie value. It is sealed with
   DPAPI (`CryptProtectData`, user-scoped) into `session.bin`, validated against
   an authenticated endpoint before it is trusted, and never written to SQLite or
   to a log line. On a copy of the file from another account or machine the DPAPI
   blob is undecryptable, which is the point.
3. Requests that need identity carry `Cookie: PHPSESSID=…`.

Consequences that are visible in the code:

- Unauthenticated features (detail, pages, search, ranking, artist pages) work
  with no session at all. The app is useful before signing in.
- Mutations (bookmark add/remove, follow) return `PixivError::Unauthorized`
  without a session. The UI shows a sign-in prompt. **A bookmark never flips
  locally on its own** — if the server did not confirm it, the state does not
  change. This is deliberate: fake local state is the one thing that would make
  the client untrustworthy.
- Signing out deletes the credentials file and rebuilds the HTTP client.

## 4. Images: the part that decides whether the app feels good

`pixiv-image` owns the path from URL to RGBA. The constraints it exists to
satisfy:

- **Tiered fetching.** Grid cards request the `regular` (master1200) tier; the
  viewer requests `original`. A 4000 px illustration is never decoded to fill a
  240 px card.
- **Single-flight.** Concurrent requests for one URL share a single fetch.
- **Priority.** `Visible` > `Near` > `Prefetch`, so scrolling prioritises what is
  on screen and abandons speculative work.
- **Bounded memory.** A byte budget with LRU eviction, not an unbounded map.
  Thumbnails are cheap, originals are not.
- **Disk tier.** Encoded bytes are cached on disk with its own budget, so
  revisiting a page does not re-download.

`i.pximg.net` rejects requests without `Referer: https://www.pixiv.net/`; the
referer is applied centrally in the transport layer so no call site can forget
it.

Slint is deliberately not a dependency of the cache: it yields `DecodedImage`
(raw RGBA8) and the app converts to `slint::Image` on the UI thread. That keeps
the cache testable headlessly.

## 5. Threading model

- The Slint event loop owns the main thread.
- One `tokio` multi-thread runtime is created at start-up and owned by the app.
- Every network call, decode, and database access happens off the UI thread.
- Results return to the UI through `slint::invoke_from_event_loop`, so Slint's
  models are only ever mutated on the main thread.
- Long-lived workers: the download queue. Everything else is request/response.

## 6. UI structure

The visual target is *70 % Pixiv web layout + 30 % WinUI 3 desktop idiom*.

- `ui/theme.slint` — design tokens (colour, spacing, radius, type scale, control
  sizes, card-width bounds) and the light/dark palette in one switchable global.
  No page hard-codes a colour or a spacing value.
- `ui/icons.slint` — **generated** from Microsoft Fluent UI System Icons (MIT).
  Only path geometry is embedded, so icons take their colour from the theme and
  stay crisp at any DPI. No emoji, no hand-drawn substitutes, no mixed icon sets.
- `ui/components.slint` — Fluent 2 primitives: buttons, icon buttons, text field,
  toggle, nav item, chips, badges, `InfoBar`, `ProgressBar`, four-state
  `StateView`, skeleton, `IllustCard`, `SettingsCard`/`SettingsRow`, and
  `PageScroll`.
- `ui/pages/*.slint` — one file per page; `gallery.slint` holds the single
  masonry renderer (`ArtworkGrid`) shared by Discovery / Ranking / Search /
  Bookmarks / History, `home.slint` adds the hero + trending-tag band.
- `ui/main.slint` — the shell: command bar, NavigationView pane, page router,
  status bar, keyboard shortcuts.

### 6.1 The responsive contract (why the old layout was broken)

Every layout defect this rewrite fixes came from the same two mistakes:

1. **The content decided its own width.** A `ScrollView` whose child sizes itself
   from its children's *preferred* width inherits the widest child's minimum. One
   long unwrapped `Text`, or one `HorizontalLayout` of chips, was enough to make
   the content wider than the viewport. The result was the pair of symptoms the
   screenshots showed: a horizontal scrollbar along the bottom of the page, and a
   grid laid out at `card-width` inside an over-wide content box, leaving a large
   blank strip on the right.
2. **Rust guessed the window width.** `column_count()`/`card_width()` were
   computed from a `last_width` field fed by *both* a Slint `width-changed`
   callback and a 250 ms `Window::size()` poll, initialised to a hard-coded 1280.
   The two sources could disagree for a quarter of a second on every resize, and
   the initial value was wrong before the first callback.

The contract now:

- `PageScroll` binds its body to `ScrollView.visible-width` (the viewport, which
  does not depend on the content) and reports `body-width` — the content box after
  padding — through a callback. Horizontal scrolling is `always-off` on page
  scrollers by construction, not by luck.
- `src/layout.rs` turns that measured number into a column count, a card width
  and a height-balanced masonry assignment. The grid is unit tested at
  640/800/1024/1140/1280/1920/2560 px of content box.
- The column count only rebuilds the row models when it *changes*. A pure width
  change updates one property (`grid-card-width`) and lets the cards re-derive
  their own height, so dragging a window edge does not rebuild the feed.
- The navigation pane collapses from `rail-compact`, which Rust sets from the
  `width-changed` event. Nothing polls, and nothing derives layout from its own
  width (that is a binding loop Slint reports as such).

Density follows Pixiv, not a touch UI: 4/8/12/16 spacing scale, 13 px body text,
8 px card radius, 1 px borders.

### 6.2 Rust-side controllers

`src/app.rs` is the only file that touches Slint, and it delegates the parts that
can be tested without a window:

| Module | Owns |
| --- | --- |
| `src/layout.rs` | column count, card width, masonry distribution |
| `src/zoom.rs` | fit scale, cursor-anchored wheel zoom, pan clamping |
| `src/images.rs` | cover de-duplication, decode target width |
| `src/route.rs` | back/forward history |
| `src/session.rs` | DPAPI-sealed session file |
| `src/picker.rs` | Win32 folder chooser |
| `src/app.rs` | state, background work, model plumbing, epoch guards |


## 7. Demo Mode

Demo Mode exists so the UI can be exercised with no session and no network.
It is kept honest by construction:

- Every demo URL uses the `demo://` scheme, so a network path reaching demo data
  is an obvious bug rather than a silent lie.
- Every title carries a `[DEMO]` marker and the shell shows a persistent banner.
- Artwork is generated procedurally at runtime — no third-party imagery is
  bundled, so there is no licensing question.
- Demo Mode is a separate `Arc<dyn DataSource>` implementation; it never parses
  or imitates a Pixiv response.

## 8. Error handling and limits

- One error enum, `PixivError`, with an explicit `is_retryable()` policy: network
  faults and 5xx retry, `Unauthorized`/`NotFound` do not.
- Retries use exponential backoff and honour `Retry-After`.
- A minimum interval between outbound requests keeps the client polite.
- Every page has four states: loading (skeleton), content, empty, error. The
  error state offers retry rather than a dead end.

## 9. Known limits

Recorded honestly rather than papered over — see `README.md` § Known limitations
for the user-facing version. In short: bookmark/follow POST shapes are
implemented from observed request formats but could not be confirmed against a
live authenticated session; comment, novel-chapter and `discovery` endpoints did
not expose a usable shape and are not modelled; ugoira export writes GIF only.
