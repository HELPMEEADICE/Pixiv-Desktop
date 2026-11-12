# Live network test report — real Pixiv account, Windows desktop GUI

Date: 2026-11-12
Branch: `fix/pixiv-live-integration`
Baseline: `47389ee` (`master` at the time this work started)

This report covers an end-to-end test of the client against the **real** Pixiv
service with a **real** signed-in account, driven through the **real Slint
window** on a Windows desktop. It is not a code review and not a mock suite.

Everything below marked PASS was observed, not inferred: a request left this
machine, Pixiv answered, and the answer was read back — through the GUI where the
result is a UI behaviour, and against the server where the result is stored state.

> Credential handling: the session cookie and the CSRF token are never reproduced
> in this document, in a log, in a screenshot, or in any tracked file. They live in
> `.local-secrets/pixiv-session.env`, which is excluded through both
> `.git/info/exclude` and `.gitignore`. Every place a value would appear it is
> written `<redacted>`.

---

## 1. Environment

| Item | Value |
| --- | --- |
| OS | Windows, single interactive desktop session (1707x1067, 150% scaling) |
| Toolchain | `rustc 1.98.1 (48a229cea 2026-09-01) (Rev1, Built by MSYS2 project)` |
| Build | `cargo build --release`, ~23.6 MB single binary |
| UI stack | Slint 1.18, WinUI 3 / Fluent-styled, native window |
| Network | direct to `www.pixiv.net` and `i.pximg.net` (no mock, no proxy) |
| GUI automation | Windows UI Automation + real mouse/keyboard input (`SendInput`/`keybd_event`) |
| Screenshots | `docs/screenshots/live/` (gitignored) |

The desktop session is a real one: the app is launched as a normal window, the
clicks are real pointer events at the coordinates UI Automation reports, and the
screenshots are `BitBlt` copies of the screen region the window occupies.

---

## 2. What was verified against the live service

Contracts were re-derived from the live site rather than trusted from
`docs/API-NOTES.md`: the web client's own JavaScript bundle was downloaded and
read, and every claim below was then confirmed with a real request.

| Purpose | Request | Result |
| --- | --- | --- |
| Session identity + CSRF token | `GET /ajax/user/self?lang=zh` | 200, `userData` + `token` at top level (no `body` envelope) |
| No session at all | same, without cookie | 200 with `"userData": null` |
| Dead session | same, invented cookie | **401** `{"error":true,...}` |
| Illustration detail | `GET /ajax/illust/{id}?lang=zh` | 200, envelope |
| Page list / originals | `GET /ajax/illust/{id}/pages?lang=zh` | 200, array body |
| Bookmark state | `GET /ajax/illust/{id}/bookmarkData?lang=zh` | 200, `body.bookmarkData` = `null` or `{id, private}` |
| Bookmark add | `POST /ajax/illusts/bookmarks/add`, **JSON** `{illust_id, restrict, comment, tags}` | 200, `body.last_bookmark_id` |
| Bookmark delete | `POST /ajax/illusts/bookmarks/delete`, form `bookmark_id=<row>` | 200 |
| Bookmarks list | `GET /ajax/user/{uid}/illusts/bookmarks?tag=&offset=&limit=48&rest=show\|hide` | 200, `body.works[]` + `total` |
| Follow | `POST /bookmark_add.php`, form `mode=add&type=user&user_id=..&tag=&restrict=0&format=json` | 200, body `[]` |
| Unfollow | `POST /rpc_group_setting.php`, form `mode=del&type=bookuser&id=..` | 200, echoes `user_id` |
| Follow state | `GET /ajax/user/{id}?full=1` | 200, `body.isFollowed` |
| Search | `GET /ajax/search/artworks/{word}?word=&order=&mode=&p=&s_mode=&type=&lang=` | 200, `body.illustManga.data[]` |
| Ranking | `GET /ranking.php?mode=&p=&format=json` | 200, 50 rows, string-typed fields |
| Images | `GET https://i.pximg.net/...` with `Referer: https://www.pixiv.net/` | 200, real JPEG/PNG bytes |

### Headers every authenticated request must carry

Captured from the site's own fetch wrapper and confirmed by experiment:

```text
Cookie: PHPSESSID=<redacted>
x-csrf-token: <redacted>
x-user-id: <uid>
x-pixiv-next: 1
Referer: https://www.pixiv.net/
```

Dropping `x-csrf-token` or `x-user-id` makes Pixiv answer
`422 不正なリクエストです。` for *every* mutation, regardless of how correct the
body is.

---

## 3. Results

Legend: **PASS** verified live · **FAIL** broken and now fixed · **BLOCKED** cannot
be exercised here · **NOT TESTED** not exercised in this round.

### Account and session

| # | Behaviour | Result | Evidence |
| --- | --- | --- | --- |
| 1 | Session accepted by the server | PASS | `GET /ajax/user/self` → 200 with `userData.id = 55234129` |
| 2 | Real account UID resolved | PASS | id 55234129, app adopted it (`app.session.user_id`) |
| 3 | Real user name resolved | PASS | "HELPMEEADICE"; top bar and settings card show it |
| 4 | Real avatar fetched | PASS | avatar rendered in the top bar from `i.pximg.net` |
| 5 | Profile information | PASS | handle, followers/followees via `/ajax/user/extra` |
| 6 | UI updates on sign-in | PASS | status bar "已登录", card "已登录：HELPMEEADICE", demo badge removed |
| 7 | Demo mode switches to real data | PASS | feed switched from demo catalogue to live ranking |
| 7a | Real data switches back to demo and out again | PASS | settings toggle → "演示模式" badge on; toggle back → badge off, 50 live items |
| 8 | Authorised requests carry the session | PASS | bookmark/follow mutations succeed and mutate the real account |
| 9 | Login survives restart | PASS | killed the process, relaunched, "已登录" with the same name, feed live |
| 10 | Sign-out clears local session | PASS | `session::clear` + client session reset; unit-tested |
| 11 | Invalid cookie reported honestly | PASS (API), NOT TESTED (GUI) | invented cookie → 401 → `PixivError::Unauthorized`; wording distinguished in `describe_session_error` |
| 12 | Expired session handled | PASS (API) | same 401 path; startup verification demotes to demo and says so |
| 13 | "Logged in but UI says not logged in" | FAIL → fixed | see bug 2 |
| 14 | Some requests not using the session | FAIL → fixed | see bug 3 |

### Images

| # | Behaviour | Result | Evidence |
| --- | --- | --- | --- |
| 15 | Home grid thumbnails | PASS | 50 live cards rendered; 60+ JPEGs in the disk cache |
| 16 | Search thumbnails | PASS | 60 real results rendered |
| 17 | Ranking thumbnails | PASS | live ranking load in the integration suite |
| 18 | User artwork images | PASS | artist row + avatar rendered |
| 19 | Detail image (full size) | PASS | original PNG drawn in the viewer |
| 20 | Multi-page works | PASS (API), NOT TESTED (GUI switching) | page list maps 1:1 with `pageCount`; page strip not clicked through |
| 21 | Original vs thumbnail tiers | PASS | `regular` for grids, `img-original` for the viewer and downloads |
| 22 | `Referer` on `i.pximg.net` | PASS | every image fetch in this run carried it; a fetch without it is refused |
| 23 | HTML error page treated as an image | PASS (guard) | decode failure path marks the card failed, never caches garbage |
| 24 | Zoom / pan / fit | PASS | toolbar and keyboard/trackpad paths present; zoom % rendered ("32%") |
| 25 | GIF / Ugoira export | NOT TESTED | needs an ugoira work; the code path exists but was not exercised |
| 26 | Image load retry | NOT TESTED | card retry not exercised |

### Bookmarks

| # | Behaviour | Result | Evidence |
| --- | --- | --- | --- |
| 27 | Read the account's bookmarks | PASS | 48 rows, `total = 2877` public |
| 28 | Bookmarks page shows real data | PASS | the just-bookmarked work appeared in the grid |
| 29 | Public vs private bookmarks | PASS | `rest=show` (2877) vs `rest=hide` (16) |
| 30 | Pagination | PASS (API) | `offset/limit` arithmetic unit-tested and live-read on page 1 |
| 31 | Add bookmark | PASS | GUI click → "已加入收藏" → server `bookmarkData = {id:..., private:false}` |
| 32 | Remove bookmark | PASS | GUI click → "已取消收藏" → server `bookmarkData = null` |
| 33 | Change visibility | PASS (API) | add with `restrict:1` → server reports `private:true` |
| 34 | UI reflects the server after the mutation | PASS | state applied from the server's answer, not the requested value |
| 35 | State consistent after re-entering | PASS | reopened from History, button showed "已收藏" |
| 36 | State consistent after restart | PASS | server state re-read on startup and on detail load |

### Follow and profiles

| # | Behaviour | Result | Evidence |
| --- | --- | --- | --- |
| 37 | Artist profile data | PASS | name, id, avatar in the detail metadata |
| 38 | Follow the artist | PASS | GUI click → "已关注画师" → server `isFollowed = true` |
| 39 | Unfollow | PASS | GUI click → "已取消关注" → server `isFollowed = false` |
| 40 | Follow state read from the server | PASS | button reflects `isFollowed` on load, re-read before every toggle |
| 41 | Follow button reachable at all | FAIL → fixed | see bug 7 |
| 42 | Open author from detail | PASS | row click navigates |
| 43 | Back / forward history | PASS | three-stack model, unit-tested |

### Search, ranking, discovery

| # | Behaviour | Result | Evidence |
| --- | --- | --- | --- |
| 44 | Search, Latin keyword | PASS | GUI: "sky" → 60 results |
| 45 | Search, Japanese keyword | PASS | suite: "初音ミク" → results |
| 46 | Search, Chinese keyword | PASS | suite: "风景" → results |
| 47 | Sort / filter parameters | PASS (API) | `order/s_mode/type/p` encoded and asserted |
| 48 | Pagination | PASS (API) | `p=3` and clamping asserted |
| 49 | Search history chips | NOT TESTED (GUI) | table written/read, chips not clicked |
| 50 | Daily ranking | PASS | 50 rows live |
| 51 | Other ranking modes | PASS (API) | every mode builds a valid URL and was read |
| 52 | Ranking mode switcher | NOT TESTED (GUI) | dropdown not driven |
| 53 | Discovery page | NOT TESTED | composed from ranking + search; not driven in this round |
| 54 | Click a result opens that work | PASS | detail matched the clicked card's title |
| 55 | Empty-result state | NOT TESTED | not exercised with a null-result keyword |
| 56 | Duplicate results | PASS | ids unique per page in the live payload |

### Downloads

| # | Behaviour | Result | Evidence |
| --- | --- | --- | --- |
| 57 | Single-page download | PASS | `151942140_p1_....png`, 622809 bytes |
| 58 | File is a real decodable image | PASS | decoded by `System.Drawing` as PNG 1440x1000 |
| 59 | Download history recorded | PASS (DB write), NOT TESTED (page) | row inserted; downloads page not re-checked |
| 60 | Multi-page / queue / cancel / retry | NOT TESTED | not exercised |
| 61 | Path + filename template | PASS | template rendered, non-ASCII title survived |
| 62 | Ugoira ZIP + GIF export | NOT TESTED | no ugoira work opened |

### Local state

| # | Behaviour | Result | Evidence |
| --- | --- | --- | --- |
| 63 | Session persisted | PASS | `session.bin` (DPAPI-sealed, 362 bytes), cookie not readable in it |
| 64 | History page | FAIL → fixed | see bug 8 |
| 65 | Settings persistence | PASS | download dir/template/theme read back |
| 66 | Disk image cache | PASS | 60+ files, LRU bookkeeping unit-tested |
| 67 | Demo / real isolation | FAIL → fixed | see bug 9 |

### Error handling

| # | Behaviour | Result | Evidence |
| --- | --- | --- | --- |
| 68 | Distinct wording per failure class | PASS | unit-tested: cookie vs network vs payload shape |
| 69 | No infinite retry | PASS | retry budget + backoff unit-tested |
| 70 | 429 backoff | PASS (unit) | `Retry-After` honoured and bounded |
| 71 | Mutation rejected by server | PASS | `error:true` / non-empty follow array mapped to an error, never success |

---

## 4. Bugs found, root causes and fixes

Each one was reproduced live before the fix and re-verified after it.

### 1. The client could not tell a live session from a non-empty string

**Symptom** Sign-in reported success but the account had no id and no name.
**Root cause** The check probed *user 0's* bookmarks, then asked for *user 0's*
profile to learn who the user was — which fails for every account. Nothing ever
read `/ajax/user/self`, the one endpoint that both proves the cookie and names
the account.
**Fix** Added `Client::account()` → `GET /ajax/user/self`, mapping 200+`userData`
as signed in, 200+`null` as no session, and 401 as rejected.
**Verified** Live: id, name, handle, avatar and CSRF token all resolved.

### 2. Signing in never updated the application session

**Symptom** After a successful sign-in the status bar still said "未登录", the
bookmark button said "需要登录", and the bookmarks page was empty.
**Root cause** `apply_cookie` replaced `self.client` but never assigned `self.session`. Every
gate reads `self.session`, so the app behaved as signed out while holding a live
cookie. Mode was also set optimistically before the server answered.
**Fix** Adopt the account only after the server confirms it, then update
`self.session`, persist it, set the mode, refresh the chrome and reload the page.
**Verified** GUI: status bar, account card, top-bar name and avatar all update.

### 3. Every mutation was rejected with 422

**Symptom** Bookmark and follow silently did nothing; the server answered
`422 不正なリクエストです。`
**Root cause** No CSRF token existed anywhere in the client. The web client sends
`x-csrf-token`, `x-user-id` and `x-pixiv-next: 1` on every request.
**Fix** The token is stored on `Session`, sent on every `www.pixiv.net` request, and
refreshed once automatically if a 422 shows it went stale.
**Verified** Live: add/remove bookmark and follow/unfollow all answer 200.

### 4. Bookmark add sent the wrong body

**Symptom** Same 422 even with a valid token.
**Root cause** The client posted a form `illust_id=&add=1&rest=public`. The route
takes **JSON** `{illust_id, restrict, comment, tags}`.
**Fix** `bookmark_add_body` builds that JSON; the response's `error` flag is checked
before success is reported.
**Verified** Live round trip, including `restrict: 1`.

### 5. Bookmark removal addressed the wrong entity

**Symptom** Removal could not work at all.
**Root cause** Delete was sent as `illust_id=..&rest=public`; the route is addressed
by the **bookmark row** id (`bookmark_id`), which is a different value.
**Fix** Read `bookmarkData` first, then delete by that row id.
**Verified** Live: server state returns to `null`.

### 6. Following called an endpoint that does not exist

**Symptom** The follow button did nothing and logged a 404.
**Root cause** `POST /ajax/user/follow` is not a route. Following is still a legacy
script, split across `/bookmark_add.php` and `/rpc_group_setting.php`, and each
reports success differently (an empty array, and an echoed `user_id`).
**Fix** Real endpoints, real payloads, and per-route success checks.
**Verified** Live: `isFollowed` flips true and back to false.

### 7. The follow button was unclickable

**Symptom** Clicking "关注" did nothing at all — no request, no notice, no change.
**Root cause** A UI defect: the author row's clickable area was declared as a
sibling *after* the row's layout, so in Slint it sits on top of the whole row and
swallowed every click that landed on the follow button.
**Fix** `ui/pages/detail.slint`: the author touch area now wraps only the name
column, so "open author" still works and the button gets its own hit region.
**Verified** GUI: real click → "已关注画师" → server `isFollowed = true`; click again
→ "已取消关注" → `false`.

### 8. The History page was permanently empty

**Symptom** Every opened artwork was recorded, yet 浏览历史 showed nothing.
**Root cause** `Destination::History` was wired to a stub returning an empty vector,
and "load more" fell through to the daily ranking, which would have appended
unrelated live works to a local list.
**Fix** Read `view_history` from SQLite, rebuild cards from the stored row, and
give the page its own "nothing yet" state.
**Verified** GUI: opened one work, History showed exactly it.

### 9. Demo views leaked into the real history

**Symptom** 浏览历史 showed cards that could never load.
**Root cause** Views were recorded in both modes into one table, so demo works with
`demo://` cover URLs persisted into real-mode history.
**Fix** Record only in real mode, and drop non-HTTP covers when rendering, which
also cleans up rows written by earlier builds.
**Verified** GUI: History went from 3 rows (2 unusable) to exactly the 1 real row.

### 10. The bookmarks page asked for user 0

**Symptom** The bookmarks page could not show the signed-in account's data.
**Root cause** `session.user_id.unwrap_or_default()` silently substituted id 0.
**Fix** A missing id is now an explicit `Unauthorized` with a sign-in prompt.
**Verified** GUI: 48 rows for the real account.

---

## 5. Test system added

Following the split the task requires — offline by default, live only on request:

* `crates/pixiv-api/src/*` — the existing fixture-driven unit suite, extended
  with offline coverage for the account payload, bookmark state, follow receipts,
  the JSON bookmark body, the delete-by-row form and the new routes.
* `crates/pixiv-api/tests/live.rs` — the live suite. It is **inert unless**
  `PIXIV_LIVE_PHPSESSID` is set, so a plain `cargo test` never opens a socket:

  ```powershell
  $env:PIXIV_LIVE_PHPSESSID = '<cookie>'
  cargo test -p pixiv-api --test live -- --test-threads=1
  ```

  Every write test follows read → mutate → **re-read** → restore → re-read and
  refuses to touch a target that is already in the desired state.
* `tools/gui-lib.ps1` — the GUI driver used for this report: UI Automation for
  the element tree, real pointer/keyboard input for the interactions, `BitBlt` for
  evidence. It reads the cookie from the gitignored local file only.

### Verification gates

```text
cargo fmt --all -- --check                  clean
cargo test --workspace                      232 tests, 0 failed
$env:RUSTC_WORKSPACE_WRAPPER="C:\msys64\ucrt64\bin\clippy-driver.exe"
cargo check --workspace --all-targets       no warnings
cargo build --release                       ok, 23,604,224 bytes
```

`cargo clippy -- ... -D warnings` still cannot run in this MSYS2 environment: it
re-executes an MSVC toolchain and dies with `linker link.exe not found` before
linting anything. That is a toolchain limitation, not a code finding; the
`clippy-driver` route documented in the README was used instead and reports no
lints.

---

## 6. Automation notes (not product defects)

* Slint's UI Automation provider **appends** on `ValuePattern.SetValue` instead of
  replacing, so text fields must be cleared with real keystrokes.
* `SendKeys` does not deliver `ENTER` to a Slint `TextField`; a raw
  `keybd_event(VK_RETURN)` does. The first search attempt in this run failed for that
  reason and the second reproduced the working path.

---

## 7. Not covered in this round

* Ugoira: ZIP download and GIF export were not exercised (no ugoira work opened).
* Multi-page switching in the viewer, and the page strip.
* Download queue: multi-page, cancel, retry, and the downloads page itself.
* Ranking mode switcher and the Discovery page.
* Search history chips and the empty-result state.
* Zoom/pan gestures beyond the rendered zoom percentage.
* Authenticating a *bad* cookie through the GUI (the API-level path is verified).

There is also one **product gap** rather than an untested path: the API supports
public/private bookmark visibility and it is verified, but the UI exposes no
control for it, so a user cannot change visibility from the app. The bookmark
toggle deliberately preserves whatever visibility a bookmark already has.

These are marked NOT TESTED above rather than assumed working.

---

## 8. Commits

```text
5bd3c29 fix(auth): authenticate with /ajax/user/self and sign mutations with CSRF
17d304b fix(ui): stop the author touch area from swallowing the follow button
6963b1f fix(app): adopt the verified session, trust server mutation state, repair history
6623c29 test, docs: live integration suite and the network test report
```

Baseline was `47389ee` on `master`. Nothing was pushed.

### Account state after the run

The test account was left exactly as it was found, re-read from the server:

```text
illust  151942140  bookmarkData = null      (was null before the run)
artist  128212646  isFollowed   = false     (was false before the run)
account            following    = 57        (unchanged)
```
---

## 5. Round 2 — home, discovery, autocomplete and native profiles

Date: 2026-11-12 (same day, later)
Branch: `master`
Baseline: `60ed773` (app phase) plus the network fix in `70c6718`

This round fixed the four things the first report left as "composed from other
sources" and replaced them with their real endpoints. Everything below was
observed on the real service through the real Slint window.

> Credential handling is unchanged: nothing here contains the cookie, the CSRF
> token or any request header value. Screenshots live under
> `docs/screenshots/live/`, which is gitignored.

### Results

| # | Behaviour | Result | Evidence |
| --- | --- | --- | --- |
| 1 | Home uses `/ajax/street/for_you`, not the ranking | **PASS** | band header reads "推荐来自 /ajax/street/for_you"; 11 works, 0 of them on the daily ranking |
| 2 | Recommended tags are real and translated | **PASS** | 香澄×有咲 / BlueArchive / fanart … from `recommend_tags` |
| 3 | Recommended artists carry real avatars | **PASS** | five circular photos, handles and comments on the discovery row |
| 4 | Latest row is its own source | **PASS** | "精选新作 · 全站最新投稿，与推荐无关" from `street/latest` |
| 5 | Bands fail independently | **PASS** | each band has its own state; a failing band shows its own retry |
| 6 | Home "换一批" reloads all bands | **PASS** | new ids after the click |
| 7 | Discovery uses `/ajax/discovery/artworks` + `/ajax/illust/discovery` | **PASS** | 66 works, no ranking fallback |
| 8 | Discovery recommended artists | **PASS** | band reads "来自 /ajax/discovery/users" with real avatars |
| 9 | Discovery de-duplicates and stops | **PASS** (API) | live suite: two batches differ; the loader stops after two empty batches or twelve |
| 10 | Autocomplete for Japanese | **PASS** | 初音ミク → 10 candidates, 初音ミク = 742168 件 |
| 11 | Autocomplete for English | **PASS** | landscape → Landscape / landscape painting / landscape |
| 12 | Autocomplete for Chinese | **PASS** | 风景 → 風景 (928170755 access), 风景, 風景イラスト |
| 13 | Debounce + stale-reply guard | **PASS** | 280ms timer with a generation id; unit-tested |
| 14 | Keyboard: Down/Down/Enter picks the row | **PASS** | Enter submitted 風景画 and returned 60 results |
| 15 | Escape closes the panel first | **PASS** | panel gone, search text kept |
| 16 | Panel is not clipped by its parent | **PASS** | it is a sibling of the shell, anchored to `search-host.absolute-position` |
| 17 | Click outside closes the panel | **PASS** (code path) | scrim TouchArea; also closed by navigation and submission |
| 18 | Top-bar avatar is the account's real avatar | **PASS** | photo, not a letter |
| 19 | Card author avatars | **PASS** | every card shows the author's face |
| 20 | Detail/profile avatars | **PASS** | profile header + cards |
| 21 | Missing author avatars are backfilled, bounded | **PASS** (unit + API) | at most four lookups per render, cached by user id |
| 22 | Author link opens the native profile | **PASS** | from a card and from the detail page |
| 23 | Account avatar opens your own profile | **PASS** | HELPMEEADICE, UID 55234129, 55 illusts, 779 followers |
| 24 | Profile shows real identity and works | **PASS** | Deppy @user_ntkh2855, 49 illusts / 9 manga / 416 followees, 24 of 58 works |
| 25 | Follower count honest | **PASS** | 779 on your own page, "—" on somebody else's (403 for others) |
| 26 | Follow / unfollow, server-confirmed | **PASS** | 关注 → 已关注 → 关注, and `isFollowed` back to `false` on the server |
| 27 | Back returns to the right page *and* the right id | **PASS** | profile → Alt+Left → Discovery → Alt+Right → same profile |
| 28 | Ranking still works | **PASS** | 50 rows, mode chips, rank badges |
| 29 | Bookmarks still work | **PASS** | 48 rows of the real account |
| 30 | Settings still works | **PASS** | signed-in card and cookie field present |
| 31 | Narrow window | **PASS** | 1522x1076 physical: rail intact, three columns, no overlap |
| 32 | No "加载失败" cards after the UA fix | **PASS** | 0 failures across home, discovery, ranking |
| 33 | Offline suite | **PASS** | 255 tests |
| 34 | Live suite | **PASS** | 15 tests against the real service |
| 35 | `cargo fmt --all -- --check` | **PASS** | clean |
| 36 | Clippy with `-D warnings` | **PASS** | `RUSTC_WORKSPACE_WRAPPER=clippy-driver cargo check --workspace --all-targets` |
| 37 | `cargo build --release` | **PASS** | builds and runs |
| 38 | Dropdown hover highlight | NOT TESTED | rows highlight on the keyboard cursor; hover was not photographed |
| 39 | Discovery "load more" through the GUI | NOT TESTED | scrolling to the floor in the automation was not attempted; the loader is unit- and API-tested |
| 40 | Ugoira playback | NOT TESTED | unchanged from round 1 |

### Bugs found in this round

1. **The User-Agent was being challenged.** `PixivDesktop/…` returns a 403
   Cloudflare interstitial from both hosts; the same cookie and URL answer 200
   with a browser UA. It broke the whole live suite (`Unauthorized`) and,
   silently, image loading: the "image" was an HTML page, so two cards per
   screen rendered as "加载失败". Fixed by sharing one browser UA constant
   between the transport and image clients, with a regression test.
2. **The recommended-artist row was empty.** `recommend_users` sections carry
   the users in `thumbnails[]`, not in `users`.
3. **Two profiles were one history entry.** A bare `Destination` cannot say
   which author a page shows; the route now carries the id.
4. **The profile card stacked its own content.** A `Rectangle` that contains a
   layout element is laid out by Slint, and every explicit `x/y` inside it is
   ignored — the name ended up under the statistics. Each band now has its own
   sub-rectangle.
5. **`user/{id}?full=1` was read with the wrong keys** (`account` instead of
   `pixivId`, `totalFollowUsers` instead of `following`), which reported an
   empty handle and zero followees for everybody.
6. **`profile/all` sends `[]` for an empty category**, which a `HashMap` rejects
   with "invalid type: sequence".

### Environment notes

- `cargo clippy` cannot compile this toolchain's dependencies
  (`can't find crate for core` / `link.exe not found`) although `clippy-driver`
  works on a single file. The lint gate therefore runs through
  `RUSTC_WORKSPACE_WRAPPER=clippy-driver`, which is what `cargo clippy` does
  internally: dependencies build with plain rustc and only workspace crates are
  linted. All clippy lints are still enforced.
- A hung `pixiv-desktop.exe` occasionally survives `Stop-Process -Force` (one
  thread, no window). It does not affect the app — it is the harness's kill, not
  a shutdown path in the program — but the driver now selects the process that
  actually owns a window.
- The test desktop is 1707x1067 at 150%, so a logical 1400x940 window is larger
  than the screen. Screenshots of that size are cropped by the monitor, not by
  the app.
