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

## Images

- Host: `i.pximg.net`. A request without `Referer: https://www.pixiv.net/` is
  refused with 403, so the header is mandatory on every image fetch.
- Size tiers in the path/prefix: `c/128x128`, `c/540x540_70`,
  `c/250x250_80_a2`, `_master1200` (`regular`), `img-original` (`original`).
- `regular` (master1200) is the sensible grid tier; `original` is what the viewer
  loads on demand.
- Ugoira archives live under `img-zip-ugoira/…_ugoira600x600.zip` and
  `…_ugoira1920x1080.zip`, and are plain ZIPs of still frames.

## Not modelled (deliberately)

- `GET /ajax/discovery/artworks` — every parameter combination tried returned
  HTTP 400. The Discovery page is therefore composed from ranking + search +
  tag metadata instead of guessing.
- Related illustrations and comments — the paths under `/ajax/illust/{id}/…`
  returned 404 for every variant tried, so they are not modelled.
- Novel chapter bodies — not probed; the novel reader is presented as a
  structured placeholder that opens the official page.

Guessing a payload shape and shipping it would produce a client that fails
silently in the user's hands; leaving these out and saying so is the honest
option.
