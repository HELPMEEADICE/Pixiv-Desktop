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
2. The user supplies the resulting `PHPSESSID` cookie value, which is stored in a
   dedicated credentials file with `0600`-style handling — not in SQLite, not in
   logs.
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

- `ui/theme.slint` — design tokens (colour, spacing, radius, type scale) and the
  light/dark palette in one switchable global.
- `ui/icons.slint` — **generated** from Microsoft Fluent UI System Icons (MIT).
  Only path geometry is embedded, so icons take their colour from the theme and
  stay crisp at any DPI. No emoji, no hand-drawn substitutes, no mixed icon sets.
- `ui/components.slint` — Fluent-flavoured primitives: buttons, icon buttons,
  search field, nav rail item, chips, skeleton, empty/error states, tooltips.
- `ui/pages/*.slint` — one file per page.
- `ui/main.slint` — the shell: title bar, navigation rail, top bar, page router.

Density follows Pixiv, not a touch UI: 4/8/12/16 spacing scale, 13 px body text,
8 px card radius, 1 px borders.

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
