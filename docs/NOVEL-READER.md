# Novel reader

The reader is a native page, not a web view. This file records what the Pixiv novel
API actually answers, how the body markup is turned into rows, and what the reader
persists.

## 1. The API, as probed

| Purpose | Request | Notes |
| --- | --- | --- |
| Novel detail **and body** | `GET /ajax/novel/{id}?lang=zh` | one document: metadata **and** `content`, the raw markup of the whole body |
| Series metadata | `GET /ajax/novel/series/{id}?lang=zh` | `total`, `publishedTotalCharacterCount`, `firstNovelId`, `latestNovelId` |
| Series chapters | `GET /ajax/novel/series_content/{id}?lang=zh` | rows in `body.thumbnails.novel[]` |
| Search | `GET /ajax/search/novels/{word}?word=…&order=date_d&mode=all&p=…&s_mode=s_tag_full&lang=zh` | rows use `textCount` and `url`, not `characterCount`/`coverUrl` |
| A user's novels | `GET /ajax/user/{id}/profile/all` -> `body.novels`, then `GET /ajax/user/{id}/novels?ids[]=…` | the id map is `{id: null}` or `[]` |

Measured facts that changed the design:

1. **`/ajax/novel/{id}/text` answers 404. The body is in the detail
   document's `content` field. Probed again after the first 404 (no cookie, browser
   UA, `Referer`) — the route is gone, so the client does not use it, and opening a
   novel is one request instead of two.
2. `/ajax/novel/series/{id}/content` answers 404; the chapter list lives at
   `/ajax/novel/series_content/{id}`.
3. `characterCount` is the length of the **markup**, not of the text a reader sees
   (4 025 vs 3 833 rendered characters in the capture used here). Progress therefore
   uses the parser's own character count.
4. The detail document carries `seriesNavData` with `prev`/`next` (id, order, title,
   `available`). A neighbour with `available: false` is treated as absent: linking it
   would open a page that cannot be read.
5. Embedded illustrations are **not** in the body: the body holds
   `[uploadedimage:26247899]` and `textEmbeddedImages` maps that id to its URL tiers.
6. `isLoginOnly`, `xRestrict` and `isUnlisted` exist; a work that is not readable
   answers an error envelope, which surfaces as "这本小说打不开" with the API message.

Novel **bookmarks** are read (`bookmarkData`) but not written: the mutation route
was never verified against a live account, and a client that guesses a write is
worse than one that says it has not verified it.

## 2. The body markup, and what the parser does with it

`crates/pixiv-novel` is pure: no network, no disk, no UI types. `parse` is total — every
input produces a `Document`, and nothing is ever dropped.

| Marker | Result |
| --- | --- |
| `[newpage]` | `BlockKind::PageBreak`, drawn as a "— 分页 —" divider |
| `[chapter:title]` | `BlockKind::Heading` (a line-level marker, not a tree) |
| `[[rb:base > reading]]` | `Span::Ruby`; rendered as "base（reading）" or just "base", per the ruby setting |
| `[pixivimage:1234-2]` | `Span::WorkRef`, a clickable row that opens the artwork |
| `[[jumpuri:label > url]]` | `Span::Link`, a clickable row |
| `[[jump:1234]]` | `Span::Link` with a `jump:` href |
| `[uploadedimage:26247899]` | `BlockKind::Image`, decoded from `textEmbeddedImages`; a marker with no entry keeps a readable `[图片 id]` |
| `***` / `---` | `BlockKind::Separator` |
| anything else in brackets | **stays literal text**: a marker this client does not know is a marker it must not delete |

Ruby cannot be drawn as real furigana above a base run: Slint has no inline spans, and
laying two texts per run breaks wrapping. The reading is therefore inlined in
parentheses (the CJK convention) or hidden, and that choice is a setting.

The document model is flat (`Vec<Block>`) because progress is stored as
`(block_index, char_offset)` and has to mean the same thing forever, and because a
flat list maps 1:1 onto a Slint row model.

## 3. The reader page

`ui/pages/novel-reader.slint` + `src/app/novel.rs`.

* **One model, one writer.** The page draws `ReaderView`, built by Rust from
  `ReaderPrefs`. The settings panel edits *ratios* (0..1) and calls back; Rust
  clamps through `pixiv_novel::prefs`, persists, and republishes. The live text behind
  the panel is the preview, so a preview can never disagree with the body.
* **Typography** is `font-size`, `font-weight`, `font-italic`, `letter-spacing` and
  `line-height-factor` (Slint's multiplier of the font's natural line height) on one
  `Text` per block, inside a column of width `min(measure, viewport - 2·margin)`
  centred in the view. Widening the window widens the margins, not the line.
* **First-line indent** is applied by Rust as U+3000 characters: Slint has no
  `text-indent`, and a spacer element cannot wrap with the paragraph.
* **Position** is a scroll ratio, not a pixel offset: it survives a font-size change.
  `PIXIV_DESKTOP_READER_SCROLL=0.42` exercises the restore path without input.
* **Keys**: PageUp/PageDown step one viewport (minus an overlap line, or exactly one
  viewport in paged mode), Home/End jump, `a` opens the settings, Escape closes the
  settings, then the hidden toolbar, then goes back.
* **Immersive**: the toolbar hides; a progress pill stays in the corner.

## 4. What is persisted

| Table / key | Contents |
| --- | --- |
| `novel_progress` | per novel: series id, block index, character offset, ratio, finished, updated_at |
| `novel_history` | per novel: title, author, cover, series id, read_at — the "最近阅读" list |
| `settings`, key `novel.reader.prefs` | the whole `ReaderPrefs` as JSON |

Writes are throttled: progress at most every 2 s (plus a forced flush when the page is
left), preferences at most every 400 ms, both on the tokio runtime. Migration step 3
is additive; `upgrading_from_version_two_keeps_the_existing_rows` proves an old database
keeps its history.

## 5. Verified, and not

Verified here:

* `cargo test --workspace` — 148 (app) + 88 + 39 + 25 + 16 + the crate suites, all green,
  including 21 parser tests and 12 store tests.
* Live: `cargo test -p pixiv-api --test live live_novel` with a real cookie fetched
  novel 29647105 (title, body, two embedded images, series 15745812 with 8 chapters).
* Offline: `fixtures/novel_detail.json` (a trimmed live capture) drives the whole
  reader through `PIXIV_DESKTOP_NOVEL_FIXTURE`, which is how the captures in this
  document were taken. It is a test mode and the status bar says so.

Not verified / not implemented:

* Novel bookmark **writes** (state is read only).
* True page-by-page pagination: Slint cannot reflow a column into pages, so "分页"
  steps by viewport. Said plainly in the settings panel ("整屏翻页").
* `cargo clippy`: unavailable in this toolchain (linker), as recorded in the README.
