# Pixiv API notes (probed, not documented)

Pixiv publishes no API documentation for third-party clients. Everything below
was established by observing the live site on the date of development, and is
recorded so the next person does not have to rediscover it. **Every claim here
was verified with a request**, and the responses were saved under `fixtures/`.

## Authentication reality

- There is no public third-party OAuth registration. The official apps ship a
  `client_id`/`client_secret` pair that is not redistributable, so hardcoding one
  is not an option.
- The usable credential is the browser session cookie `PHPSESSID`.
- OAuth refresh-token flows (`oauth2/token` with `refresh_token`) exist but
  require those same client credentials, so they are out of reach for a client
  the user builds themselves.

## Every authenticated request carries five things

Taken from the web client's own axios wrapper (`_app-*.js`), then confirmed by
experiment — dropping either CSRF header makes every mutation answer
`422 不正なリクエストです。` no matter how correct the body is:

```
Cookie: PHPSESSID=<redacted>
x-csrf-token: <32 hex chars, from GET /ajax/user/self -> "token">
x-user-id:    <the signed-in uid>
x-pixiv-next: 1
Referer:      https://www.pixiv.net/
```

The token is session-scoped, so a stored copy can go stale; the client refreshes
it once and replays the request when it sees a `422`.

## Endpoint matrix

| Purpose | Request | Auth | Notes |
| --- | --- | --- | --- |
| **Session identity + CSRF** | `GET /ajax/user/self?lang=zh` | **yes** | **no envelope**; `userData`, `token`, `services` at top level |
| Illustration detail | `GET /ajax/illust/{id}?lang=zh` | no | `{error,message,body}` envelope |
| Page list / originals | `GET /ajax/illust/{id}/pages?lang=zh` | no | array body; `urls.original` |
| Ugoira metadata | `GET /ajax/illust/{id}/ugoira_meta?lang=zh` | no | 404 when not ugoira |
| Search | `GET /ajax/search/artworks/{kw}` | no | `body.illustManga.data[]` |
| Tag metadata | `GET /ajax/search/tags/{tag}` | no | translations, not autocomplete |
| Ranking | `GET /ranking.php?mode=…&p=…&format=json` | no | **not** an envelope |
| Artist work ids | `GET /ajax/user/{id}/profile/all` | no | `illusts` map of id→null |
| Artist profile | `GET /ajax/user/{id}?full=1` | no | large payload |
| Artist works (paged) | `GET /ajax/user/{id}/illusts` | **yes** | 400 without cookie |
| Bookmarks | `GET /ajax/user/{id}/illusts/bookmarks` | **yes** | `rest=show\|hide`; rows are in `body.works[]` |
| Bookmark state | `GET /ajax/illust/{id}/bookmarkData` | **yes** | `body.bookmarkData` = `null` or `{id, private}` |
| Add / re-tag bookmark | `POST /ajax/illusts/bookmarks/add` | **yes** | **JSON** `{illust_id, restrict, comment:"", tags:[]}`; answers `last_bookmark_id` |
| Remove bookmark | `POST /ajax/illusts/bookmarks/delete` | **yes** | form `bookmark_id=<row>` — *not* the illust id |
| Follow | `POST /bookmark_add.php` | **yes** | form `mode=add&type=user&user_id&tag&restrict&format=json`; success = `[]` |
| Unfollow | `POST /rpc_group_setting.php` | **yes** | form `mode=del&type=bookuser&id`; echoes `user_id` |
| Follow state | `GET /ajax/user/{id}?full=1` | no | `body.isFollowed` |
| **Home: for you** | `GET /ajax/street/for_you?lang=zh` | recommended **yes** | sections in `body.contents[]` |
| **Home: latest** | `GET /ajax/street/latest?lang=zh` | no | one carousel in `body.content` |
| **Home: tags** | `GET /ajax/street/recommend_tags?lang=zh` | recommended **yes** | `body.content.tags[]` |
| **Home: artists** | `GET /ajax/street/sub?lang=zh` | recommended **yes** | artists live in the section's `thumbnails[]` |
| **Discovery batch** | `GET /ajax/discovery/artworks?mode=all&limit=60&lang=zh` | yes | 60 ranked ids + 5 enriched |
| **Discovery grid** | `GET /ajax/illust/discovery?mode=all&lang=zh` | yes | 10 fully described works |
| **Discovery artists** | `GET /ajax/discovery/users?mode=all&lang=zh` | yes | 20 users with avatars |
| **Autocomplete** | `GET /rpc/cps.php?keyword=…&lang=zh` | optional | no envelope; translations + access counts |
| **Tag counts** | `GET /ajax/tags/suggest_by_word?word=…&lang=zh` | no | `body.candidates[]` with `illust_count` |
| User works batch | `GET /ajax/user/{id}/illusts?ids[]=…` | **yes** | map of id to work |
| User works by profile | `GET /ajax/user/{id}/profile/illusts?ids[]=…&work_category=illustManga` | no | same map shape |
| Follower count | `GET /ajax/user/{id}/followers?offset=0&limit=1&rest=show` | **yes, self only** | 403 for anybody else |

Search query parameters that matter:

```
word=<urlencoded>  order=date_d|date_a|popular_d  mode=all
p=<page>          s_mode=s_tag_full|s_tag|s_tc
type=all|illust_and_ugoira|illust|manga|ugoira   lang=zh
```

Ranking `mode` values: `daily`, `weekly`, `monthly`, `rookie`, `original`,
`male`, `female`.

## Payload quirks that will bite you

1. **Ids change type per endpoint.** `"illustId":"117661885"` on `ajax/*`,
   `"illust_id":117661885` on `ranking.php`. Handled by `pixiv_types::Id`.
2. **`illust_detail.tags` is a single object**, not an array:
   `{"authorId":"…","isLocked":false,"tags":[{"tag":"…","translation":{"en":"…"}}],"writable":false}`.
3. **`ranking.php` sends numbers as strings**: `"illust_type":"0"`,
   `"illust_page_count":"1"`, `"rank":"1"`.
4. **`mime_type` is snake_case** on `ugoira_meta` while everything around it is
   camelCase.
5. **`createDate` on search rows can be an object**, not a string.
6. **`tagTranslation` values are objects** (`{"zh":"…"}`), not strings.
7. Page URLs use `thumb_mini`; illustration URLs use `mini`. Different keys.
8. Auth-gated endpoints distinguish **two** failures and they are not the same
   thing: a request with **no cookie at all** answers `200` with
   `"userData": null` (on `/ajax/user/self`), while a cookie Pixiv no longer
   honours answers **`401`**. Some older routes still answer `400` with
   `{"error":true,"message":"不正确的请求。"}`. A client that only tests "the cookie
   string is non-empty" believes a dead session is alive.

## The User-Agent is not a free choice

Both `www.pixiv.net` and `i.pximg.net` sit behind a bot-protection edge. A
request identifying itself with a custom string

```text
User-Agent: PixivDesktop/0.1.0 (Rust; desktop client)
-> 403, body starts with "<!DOCTYPE html> <title>Just a moment...</title>"
```

is challenged before Pixiv sees it, with the same cookie and URL that answer
`200` for a browser User-Agent. The client therefore sends a stock desktop
Chrome UA plus `Accept` and `Accept-Language`. Two symptoms this produced
before it was found: the live suite failing with `Unauthorized` on a cookie
that worked by hand, and cards rendering as "加载失败" because the "image"
they downloaded was an HTML challenge page.

## Images

- Host: `i.pximg.net`. A request without `Referer: https://www.pixiv.net/` is
  refused with 403, so the header is mandatory on every image fetch.
- Size tiers in the path/prefix: `c/128x128`, `c/540x540_70`,
  `c/250x250_80_a2`, `_master1200` (`regular`), `img-original` (`original`).
- `regular` (master1200) is the sensible grid tier; `original` is what the viewer
  loads on demand.
- Ugoira archives live under `img-zip-ugoira/…_ugoira600x600.zip` and
  `…_ugoira1920x1080.zip`, and are plain ZIPs of still frames.

## The home page is not the ranking

Four endpoints answer, and each is a different thing. All of them need the
headers above; without them the answers are generic at best and HTTP 400 at
worst.

```text
GET /ajax/street/for_you        -> body.contents[]  sections, personalised
GET /ajax/street/latest         -> body.content     one carousel, not personalised
GET /ajax/street/recommend_tags -> body.content.tags[]   name + translatedName
GET /ajax/street/sub            -> body.contents[]  recommend_users + contests
```

Quirks that cost real time:

1. **A street work is not a search row.** Cover tiers are keyed by pixel size —
   `pages[0].urls["1200x1200_standard"]`, `["540x540"]`, `["360x360"]` — not by
   the `mini/thumb/small/regular/original` names.
2. **Street tags are objects**, `{name, translatedName, isEmphasized}`, not the
   flat strings search sends. They carry their own translations.
3. **`recommend_users` puts the users in `thumbnails[]`**, not in `users`.
   Reading `users` yields an empty recommended-artist row and no error.
4. **A `collection` id does not fit a u64** (`74493503404124799791`), so an id is
   parsed leniently and the card is dropped rather than failing the feed.
5. `for_you` is not paginated. A second call returns a *different* sample of the
   same engine, so "load more" is another call plus de-duplication.

## Discovery was never 400

The previous notes recorded HTTP 400 for `/ajax/discovery/artworks`. The route
is fine: it needs the `x-pixiv-next`, `x-user-id` and `Referer` headers above
and a `mode` parameter. Verified 200 for `mode=all`, `safe` and `r18`.

```text
body.recommendedIllusts[]  60 ranked ids + recommendMethods (why it was picked)
body.thumbnails.illust[]   only the first 5, fully described
body.users[]               usually empty on this route
body.tagTranslation{}      tag -> {en, zh, romaji, ...}
```

The batch describes five works and names sixty, so a grid built from it alone
shows five cards. `/ajax/illust/discovery` answers the other half: ten works,
fully described, different on every call. A discovery screen asks for both and
hydrates the remaining ids through `/ajax/illust/{id}` in a bounded batch.

## Search suggestion is a real endpoint

```text
/rpc/cps.php?keyword=…&lang=zh          no envelope: {"candidates":[…]}
/ajax/tags/suggest_by_word?word=…&lang=zh  {error,message,body{candidates}}
```

`cps.php` is what the site's own search box requests. Each candidate carries
`tag_name`, `access_count` (string), `type` (`prefix`/`tag_translation`) and,
for Japanese tags, `tag_translation`. `suggest_by_word` adds `illust_count`.

Neither is complete on its own: `cps.php` has no work counts, and
`suggest_by_word` is prefix-only. Joining them on the tag name gives the
official ordering *with* counts. A Latin keyword such as `landscape` returns
three rows (`Landscape`, `landscape painting`→風景画, `landscape`→景観), which is
exactly what the web box shows.

`/ajax/search/suggestion` also answers, but its `recommendTags` list is the same
45-46 tags for every keyword: it is a recommendation list, not a completion
feed, and its payload is ~270KB per keystroke. Not used.

## User pages

```text
GET /ajax/user/{id}?full=1     flat body: userId, name, pixivId, image, imageBig,
                               comment, background, isFollowed, following, premium, webpage
GET /ajax/user/{id}/profile/all {illusts:{id:null}, manga:{id:null}, novels:[], …}
```

1. The handle is **`pixivId`**, not `account`.
2. The followee count is **`following`**. There is no `totalFollowUsers`.
3. **The follower count is not in this payload at all.** The only source is
   `/ajax/user/{id}/followers`, which answers 403 for anybody but the signed-in
   account. So the client shows a real figure for your own profile and a dash for
   everyone else, instead of printing a zero it cannot support.
4. `illusts`/`manga` are maps for a prolific artist but `[]` for a user with no
   works in that category, so a plain `HashMap` rejects half the accounts with
   "invalid type: sequence".

## Still not modelled (deliberately)

- Related illustrations and comments — the paths under `/ajax/illust/{id}/…`
  returned 404 for every variant tried.
- Novel chapter bodies — not probed; novels are skipped in every feed rather
  than rendered as broken illustration cards.

Guessing a payload shape and shipping it would produce a client that fails
silently in the user's hands; leaving these out and saying so is the honest
option.
