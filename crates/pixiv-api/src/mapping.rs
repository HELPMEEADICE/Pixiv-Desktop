//! Response mapping, error classification and the retry policy.
//!
//! The functions here are pure: they take the bytes Pixiv returned and produce
//! domain values or a [`PixivError`], with no `reqwest` types involved. That is
//! what makes the whole mapping layer assertable against `fixtures/*.json`
//! without a socket, and it is the only place where the wire quirks
//! (`{error, message, body}` envelopes, plain-`ranking.php` documents, object
//! valued tag translations) are resolved.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use pixiv_types::domain::{
    Account, BookmarkRestrict, BookmarkState, DiscoveryBatch, Illust, LatestFeed, PageMeta,
    RankingRow, RecommendTag, RecommendUser, SearchResult, StreetFeed, Suggestion, UgoiraMeta,
    UserProfile, merge_suggestions,
};
use pixiv_types::wire::{
    BookmarkStateWire, CpsCandidateWire, CpsWire, DiscoveryArtworksWire, DiscoveryUserWire,
    DiscoveryUsersWire, FollowerListWire, IllustDetailWire, IllustDiscoveryWire, IllustPageWire,
    ProfileIllustsWire, RankingResponseWire, SearchBodyWire, SearchItemWire, SearchTagsWire,
    StreetForYouWire, StreetLatestWire, StreetRecommendTagsWire, StreetSubWire, StreetTagWire,
    SuggestByWordWire, SuggestCandidateWire, UgoiraMetaWire, UserBookmarksWire, UserDetailWire,
    UserProfileAllWire, UserSelfWire,
};
use pixiv_types::{Id, PixivError};

/// First backoff step.
const BACKOFF_BASE: Duration = Duration::from_millis(300);
/// Ceiling for the exponential backoff.
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Ceiling applied to a server supplied `Retry-After`, so a hostile or broken
/// header cannot stall the client for hours.
const RETRY_AFTER_MAX: Duration = Duration::from_secs(60);
/// How much of an unparsable error body is quoted back to the caller.
const SNIPPET_LIMIT: usize = 300;

// ---------------------------------------------------------------------------
// Retry policy
// ---------------------------------------------------------------------------

/// True when another attempt is both allowed and useful.
///
/// `attempt` is the zero-based index of the attempt that just failed, so
/// `max_retries` extra attempts means `attempt < max_retries`.
pub(crate) fn should_retry(attempt: u32, max_retries: u32, error: &PixivError) -> bool {
    attempt < max_retries && error.is_retryable()
}

/// Delay before the next attempt.
///
/// Exponential from [`BACKOFF_BASE`], capped at [`BACKOFF_MAX`]. A `Retry-After`
/// hint replaces the computed value because the server knows better, bounded by
/// [`RETRY_AFTER_MAX`].
pub(crate) fn backoff_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(hint) = retry_after {
        return hint.min(RETRY_AFTER_MAX);
    }
    let factor = 1u32 << attempt.min(16);
    BACKOFF_BASE.saturating_mul(factor).min(BACKOFF_MAX)
}

/// Parse a `Retry-After` header value in its delay-seconds form.
///
/// The HTTP-date form is not honoured: Pixiv only ever sends seconds, and
/// treating an unparsable date as "no hint" is safer than fabricating a wait.
pub(crate) fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

// ---------------------------------------------------------------------------
// Error classification
// ---------------------------------------------------------------------------

/// Recognise the `{"error": true, "message": "..."}` payload Pixiv uses for
/// both non-2xx rejections and 200 responses that failed logically.
///
/// Returns `None` when the body is not JSON, is not an object, or does not set
/// the flag.
fn api_message(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    if object.get("error").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let message = object
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    Some(if message.is_empty() {
        "Pixiv 接口错误".to_string()
    } else {
        message
    })
}

/// A short, printable excerpt of a body that is not structured JSON.
fn snippet(body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut out: String = trimmed.chars().take(SNIPPET_LIMIT).collect();
    if trimmed.chars().count() > SNIPPET_LIMIT {
        out.push('…');
    }
    Some(out)
}

/// Map a non-success HTTP status onto the error the UI will show.
///
/// Order matters and is fixed by the API contract: `404` is "gone", `401`/`403`
/// are "sign in", a flagged body carries Pixiv's own message, and everything
/// else is a plain transport failure. `429` stays retryable through
/// [`PixivError::is_retryable`] because it maps to `Http { status: 429 }`.
pub(crate) fn map_status_error(status: u16, body: &str) -> PixivError {
    let flagged = serde_json::from_str::<Value>(body)
        .ok()
        .as_ref()
        .and_then(api_message);
    match status {
        404 => PixivError::NotFound(
            flagged
                .or_else(|| snippet(body))
                .unwrap_or_else(|| "资源不存在".to_string()),
        ),
        401 | 403 => PixivError::Unauthorized,
        _ => match flagged {
            Some(message) => PixivError::Api(message),
            None => PixivError::http(status, snippet(body).unwrap_or_default()),
        },
    }
}

/// Validate the response to a mutation.
///
/// Pixiv answers a bookmark or follow request with a 2xx envelope whose `error`
/// flag is authoritative: `{"error": true}` means the mutation did not happen
/// even though the status line said 200. Success is therefore only reported when
/// the status is 2xx *and* the body does not deny it, and a 2xx body that cannot
/// be parsed at all is an error rather than an assumed success.
pub(crate) fn check_mutation(status: u16, body: &str) -> Result<(), PixivError> {
    if !(200..300).contains(&status) {
        return Err(map_status_error(status, body));
    }
    if body.trim().is_empty() {
        return Ok(());
    }
    let value = serde_json::from_str::<Value>(body)
        .map_err(|e| PixivError::Decode(format!("响应不是合法 JSON：{e}")))?;
    match api_message(&value) {
        Some(message) => Err(PixivError::Api(message)),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Envelope decoding
// ---------------------------------------------------------------------------

fn parse_json(raw: &str) -> Result<Value, PixivError> {
    serde_json::from_str(raw).map_err(|e| PixivError::Decode(format!("响应不是合法 JSON：{e}")))
}

/// Pull the `body` member out of an `ajax/*` envelope, rejecting flagged
/// payloads first so that a logical error never reaches a deserialiser that
/// would report it as a shape mismatch.
fn take_body(mut value: Value) -> Result<Value, PixivError> {
    if let Some(message) = api_message(&value) {
        return Err(PixivError::Api(message));
    }
    value
        .get_mut("body")
        .map(Value::take)
        .ok_or_else(|| PixivError::Decode("响应缺少 body 字段".to_string()))
}

/// Decode the payload of an `ajax/*` envelope.
fn decode_body<T: DeserializeOwned>(raw: &str) -> Result<T, PixivError> {
    let body = take_body(parse_json(raw)?)?;
    serde_json::from_value(body).map_err(|e| PixivError::Decode(e.to_string()))
}

/// Decode a document that is not wrapped in an envelope (`ranking.php`).
fn decode_plain<T: DeserializeOwned>(raw: &str) -> Result<T, PixivError> {
    let value = parse_json(raw)?;
    if let Some(message) = api_message(&value) {
        return Err(PixivError::Api(message));
    }
    serde_json::from_value(value).map_err(|e| PixivError::Decode(e.to_string()))
}

// ---------------------------------------------------------------------------
// Endpoint payloads
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Identity, bookmark state and mutations
// ---------------------------------------------------------------------------

/// `GET /ajax/user/self`.
///
/// Three answers mean three different things and the UI must not conflate them:
///
/// * `200` with a populated `userData` -> a live session, plus the account and the
///   CSRF token every mutation has to carry.
/// * `200` with `userData: null` -> no session at all (anonymous request).
/// * `401` -> a cookie was sent but Pixiv no longer honours it.
///
/// Only the first is a signed-in state; the other two are
/// [`PixivError::Unauthorized`], and the caller shows different wording for them.
pub(crate) fn parse_account(raw: &str) -> Result<Account, PixivError> {
    let wire = serde_json::from_str::<UserSelfWire>(raw)
        .map_err(|e| PixivError::Decode(format!("响应不是合法的用户信息：{e}")))?;
    let Some(data) = wire.user_data else {
        return Err(PixivError::Unauthorized);
    };
    if data.id.is_zero() {
        return Err(PixivError::Unauthorized);
    }
    // Prefer the large avatar, falling back to the small one, and never invent a
    // URL when Pixiv sent none.
    let avatar = [data.profile_img_big, data.profile_img]
        .into_iter()
        .find(|candidate| !candidate.trim().is_empty());
    Ok(Account {
        id: data.id,
        name: data.name,
        account: data.pixiv_id,
        avatar,
        csrf_token: Some(wire.token).filter(|token| !token.trim().is_empty()),
    })
}

/// `GET /ajax/illust/{id}/bookmarkData`.
pub(crate) fn parse_bookmark_state(raw: &str) -> Result<BookmarkState, PixivError> {
    let body = decode_body::<BookmarkStateWire>(raw)?;
    Ok(match body.bookmark_data {
        None => BookmarkState::none(),
        Some(data) => BookmarkState::bookmarked(
            BookmarkRestrict::from_flag(u8::from(data.is_private)),
            data.id.filter(|id| !id.trim().is_empty()),
        ),
    })
}

/// `GET /ajax/user/{id}?full=1`, reduced to the follow flag.
pub(crate) fn parse_follow_state(raw: &str) -> Result<bool, PixivError> {
    decode_body::<UserDetailWire>(raw).map(|wire| wire.is_followed)
}

/// Validate `POST /bookmark_add.php` (follow).
///
/// The script answers a JSON **array** and reports success by returning it empty;
/// a non-empty array carries the failure. This is not the `{error, message}`
/// envelope every `ajax` route uses, so it needs its own check — treating a
/// `200` as success would report a follow that never happened.
pub(crate) fn check_follow_add(status: u16, body: &str) -> Result<(), PixivError> {
    if !(200..300).contains(&status) {
        return Err(map_status_error(status, body));
    }
    let value: Value = serde_json::from_str(body.trim())
        .map_err(|e| PixivError::Decode(format!("关注响应不是合法 JSON：{e}")))?;
    match value {
        Value::Array(items) if items.is_empty() => Ok(()),
        Value::Array(items) => Err(PixivError::Api(format!(
            "关注被拒绝：{}",
            serde_json::to_string(&items).unwrap_or_default()
        ))),
        other => match api_message(&other) {
            Some(message) => Err(PixivError::Api(message)),
            None => Err(PixivError::Decode(
                "关注响应既不是空数组也不是错误对象".to_string(),
            )),
        },
    }
}

/// Validate `POST /rpc_group_setting.php` (unfollow).
///
/// Success is the script echoing back the user it acted on:
/// `{"user_id":"123","type":"bookuser"}`. Comparing the echo is what makes this
/// a verification rather than an assumption.
pub(crate) fn check_follow_remove(status: u16, body: &str, user: Id) -> Result<(), PixivError> {
    if !(200..300).contains(&status) {
        return Err(map_status_error(status, body));
    }
    let value: Value = serde_json::from_str(body.trim())
        .map_err(|e| PixivError::Decode(format!("取消关注响应不是合法 JSON：{e}")))?;
    if let Some(message) = api_message(&value) {
        return Err(PixivError::Api(message));
    }
    let echoed = value.get("user_id").and_then(|v| match v {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    });
    match echoed.as_deref() {
        Some(text) if text == user.get().to_string() => Ok(()),
        Some(text) => Err(PixivError::Decode(format!(
            "取消关注回执的用户不匹配：{text}"
        ))),
        None => Err(PixivError::Decode(
            "取消关注响应缺少 user_id 回执".to_string(),
        )),
    }
}

/// `/ajax/illust/{id}`.
pub(crate) fn parse_illust_detail(raw: &str) -> Result<Illust, PixivError> {
    decode_body::<IllustDetailWire>(raw).map(Illust::from_detail)
}

/// `/ajax/illust/{id}/pages`, indexed by position.
pub(crate) fn parse_illust_pages(raw: &str) -> Result<Vec<PageMeta>, PixivError> {
    let pages = decode_body::<Vec<IllustPageWire>>(raw)?;
    Ok(pages
        .into_iter()
        .enumerate()
        .map(|(index, wire)| PageMeta::from_wire(u32::try_from(index).unwrap_or(0), wire))
        .collect())
}

/// `/ajax/illust/{id}/ugoira_meta`.
pub(crate) fn parse_ugoira_meta(raw: &str) -> Result<UgoiraMeta, PixivError> {
    decode_body::<UgoiraMetaWire>(raw).map(UgoiraMeta::from)
}

/// `/ajax/search/artworks/{word}`.
pub(crate) fn parse_search(raw: &str) -> Result<SearchResult, PixivError> {
    decode_body::<SearchBodyWire>(raw).map(SearchResult::from_wire)
}

/// `/ranking.php?format=json`.
pub(crate) fn parse_ranking(raw: &str) -> Result<Vec<RankingRow>, PixivError> {
    decode_plain::<RankingResponseWire>(raw).map(ranking_rows)
}

/// Build ranking rows.
///
/// The row's `rank` comes from the item itself, never from the vector index:
/// a filtered or partial page would otherwise claim wrong positions.
pub(crate) fn ranking_rows(document: RankingResponseWire) -> Vec<RankingRow> {
    document
        .contents
        .into_iter()
        .map(|item| {
            let rank = u32::try_from(item.rank.max(0)).unwrap_or(0);
            RankingRow {
                rank,
                illust: Illust::from_ranking(item),
            }
        })
        .collect()
}

/// `/ajax/user/{id}/profile/all`.
/// /ajax/user/{id}/profile/all, as separate illustration and manga counts.
///
/// The two categories live in two dictionaries on the wire; merging them would
/// report a combined total the profile card does not promise.
pub(crate) fn parse_user_work_counts(raw: &str) -> Result<(usize, usize), PixivError> {
    decode_body::<UserProfileAllWire>(raw).map(|body| (body.illusts.len(), body.manga.len()))
}

pub(crate) fn parse_user_illust_ids(raw: &str) -> Result<Vec<Id>, PixivError> {
    decode_body::<UserProfileAllWire>(raw).map(|body| illust_ids(&body))
}

/// Union of the illustration and manga id maps, newest first.
///
/// The maps are `{"<id>": null}` dictionaries, so the keys *are* the payload.
/// Keys that do not parse as an id are skipped instead of failing the response.
pub(crate) fn illust_ids(wire: &UserProfileAllWire) -> Vec<Id> {
    let mut ids = BTreeSet::new();
    for key in wire.illusts.keys().chain(wire.manga.keys()) {
        if let Ok(value) = key.trim().parse::<u64>() {
            ids.insert(value);
        }
    }
    // BTreeSet is ascending; the UI wants newest (largest id) first.
    ids.into_iter().rev().map(Id).collect()
}

/// `/ajax/user/{id}?full=1`, combined with the id count from the profile map.
pub(crate) fn parse_user_profile(
    raw: &str,
    illust_count: usize,
    manga_count: usize,
) -> Result<UserProfile, PixivError> {
    decode_body::<UserDetailWire>(raw)
        .map(|wire| UserProfile::from_wire(wire, illust_count, manga_count))
}

/// `/ajax/user/{id}/illusts/bookmarks`.
pub(crate) fn parse_bookmarks(raw: &str, limit: u32) -> Result<SearchResult, PixivError> {
    decode_body::<UserBookmarksWire>(raw).map(|body| bookmark_result(body, limit))
}

/// Turn a bookmarks body into a page of results.
///
/// This endpoint reports only `total`, so the page count is derived from the
/// limit that was requested: `ceil(total / limit)`.
pub(crate) fn bookmark_result(wire: UserBookmarksWire, limit: u32) -> SearchResult {
    let total = u64::try_from(wire.total.max(0)).unwrap_or(0);
    let limit = u64::from(limit.max(1));
    SearchResult {
        items: wire.works.into_iter().map(Illust::from_search).collect(),
        total,
        last_page: u32::try_from(total.div_ceil(limit)).unwrap_or(u32::MAX),
    }
}

/// `/ajax/search/tags/{tag}`.
pub(crate) fn parse_tag_translations(raw: &str) -> Result<HashMap<String, String>, PixivError> {
    decode_body::<SearchTagsWire>(raw).map(|wire| tag_translations(&wire.tag_translation))
}

/// Collapse the tag-translation dictionary to one string per tag.
///
/// Values are objects, not strings, and the available keys vary by tag: Pixiv
/// sends `{"zh": ...}` for tags it has localised, `{"en": ...}` for others and
/// `{"romaji": ...}` for Japanese-only ones. Preference order is
/// `zh`, `en`, `romaji`, then whatever string value the object happens to hold.
pub(crate) fn tag_translations(map: &HashMap<String, Value>) -> HashMap<String, String> {
    map.iter()
        .filter_map(|(tag, value)| pick_translation(value).map(|text| (tag.clone(), text)))
        .collect()
}

fn pick_translation(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => non_empty(text),
        Value::Object(object) => {
            for key in ["zh", "en", "romaji"] {
                if let Some(found) = object.get(key).and_then(Value::as_str).and_then(non_empty) {
                    return Some(found);
                }
            }
            object.values().find_map(|v| v.as_str().and_then(non_empty))
        }
        _ => None,
    }
}

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// ---------------------------------------------------------------------------
// Home street feed, discovery and suggestions
// ---------------------------------------------------------------------------

/// /ajax/street/for_you.
pub(crate) fn parse_street_for_you(raw: &str) -> Result<StreetFeed, PixivError> {
    decode_body::<StreetForYouWire>(raw).map(street_feed)
}

/// Fold a street body into the three collections the home page renders.
///
/// Sections are dispatched by their own kind rather than by position: the
/// payload is ordered by Pixiv's ranking and that order changes, so indexing
/// into it would silently start reading the wrong section.
pub(crate) fn street_feed(w: StreetForYouWire) -> StreetFeed {
    let mut feed = StreetFeed::default();
    for section in w.contents {
        match section.kind.as_str() {
            "illust" | "manga" | "ugoira" => {
                feed.works.extend(works_from(section.thumbnails));
            }
            // A collection is a group of works; its first member is the
            // representative image, and the rest are already in the feed.
            "collection" => {
                feed.works
                    .extend(works_from(section.thumbnails).into_iter().take(1));
            }
            "recommend_tags" => feed
                .tags
                .extend(section.tags.into_iter().map(recommend_tag)),
            // The artists arrive in the thumbnails array, not in users: the key
            // is reused for whatever that section is about. Reading only users
            // is why the recommended-artist row came back empty, so both are
            // read and the payload decides which one is populated.
            "recommend_users" => {
                feed.artists.extend(
                    section
                        .users
                        .into_iter()
                        .map(RecommendUser::from)
                        .chain(users_from(section.thumbnails))
                        .filter(|user| !user.id.is_zero()),
                );
            }
            // Novels are not modelled; skipping them keeps the grid honest
            // instead of rendering a novel as a broken illustration.
            _ => {}
        }
    }
    feed
}

/// Decode a street thumbnails array as works, dropping entries that cannot be
/// drawn instead of failing the whole feed over one of them.
fn works_from(items: Vec<Value>) -> Vec<Illust> {
    items
        .into_iter()
        .filter_map(|item| serde_json::from_value::<pixiv_types::wire::StreetThumbWire>(item).ok())
        .filter(is_renderable)
        .map(Illust::from_street)
        .filter(|illust| !illust.id.is_zero())
        .collect()
}

/// Decode a street thumbnails array as recommended users.
fn users_from(items: Vec<Value>) -> Vec<RecommendUser> {
    items
        .into_iter()
        .filter_map(|item| serde_json::from_value::<DiscoveryUserWire>(item).ok())
        .map(RecommendUser::from)
        .filter(|user| !user.id.is_zero())
        .collect()
}

/// Three things disqualify a street item, and each one would otherwise render as
/// a permanently blank card: a novel (no illustration pages), a collection (its
/// synthetic id does not fit an illustration id), and an item whose pages carry
/// no URL at all.
fn is_renderable(thumb: &pixiv_types::wire::StreetThumbWire) -> bool {
    if thumb.kind == "novel" {
        return false;
    }
    if !thumb.id.trim().parse::<u64>().is_ok_and(|id| id > 0) {
        return false;
    }
    thumb
        .pages
        .iter()
        .any(|page| page.urls.values().any(|url| !url.trim().is_empty()))
}

fn recommend_tag(w: StreetTagWire) -> RecommendTag {
    let translated = w
        .translated_name
        .filter(|text| !text.trim().is_empty() && text.trim() != w.name);
    RecommendTag {
        name: w.name,
        translated,
        emphasized: w.is_emphasized,
    }
}

/// /ajax/street/latest.
pub(crate) fn parse_street_latest(raw: &str) -> Result<LatestFeed, PixivError> {
    let wire = decode_body::<StreetLatestWire>(raw)?;
    Ok(LatestFeed {
        title: if wire.content.title.trim().is_empty() {
            "最新作品".to_string()
        } else {
            wire.content.title
        },
        works: wire
            .content
            .thumbnails
            .into_iter()
            .filter(is_renderable)
            .map(Illust::from_street)
            .filter(|illust| !illust.id.is_zero())
            .collect(),
    })
}

/// /ajax/street/recommend_tags.
pub(crate) fn parse_street_recommend_tags(raw: &str) -> Result<Vec<RecommendTag>, PixivError> {
    decode_body::<StreetRecommendTagsWire>(raw).map(|wire| {
        wire.content
            .tags
            .into_iter()
            .map(recommend_tag)
            .filter(|tag| !tag.name.trim().is_empty())
            .collect()
    })
}

/// /ajax/street/sub -- recommended artists.
pub(crate) fn parse_street_sub(raw: &str) -> Result<Vec<RecommendUser>, PixivError> {
    let wire = decode_body::<StreetSubWire>(raw)?;
    Ok(wire
        .contents
        .into_iter()
        .filter(|section| section.kind == "recommend_users")
        .flat_map(|section| {
            section
                .users
                .into_iter()
                .map(RecommendUser::from)
                .chain(users_from(section.thumbnails))
        })
        .filter(|user| !user.id.is_zero())
        .collect())
}

/// /ajax/discovery/artworks.
///
/// Answers sixty recommended *ids* and enriches only the first handful, so the
/// batch carries both: the works that can be drawn immediately and the ids that
/// still need a detail call.
pub(crate) fn parse_discovery_artworks(raw: &str) -> Result<DiscoveryBatch, PixivError> {
    let wire = decode_body::<DiscoveryArtworksWire>(raw)?;
    let mut batch = DiscoveryBatch {
        works: wire
            .thumbnails
            .get("illust")
            .map(|rows| rows.iter().cloned().map(Illust::from_search).collect())
            .unwrap_or_default(),
        users: wire
            .users
            .into_iter()
            .map(RecommendUser::from)
            .filter(|user| !user.id.is_zero())
            .collect(),
        ..DiscoveryBatch::default()
    };
    for candidate in wire.recommended_illusts {
        if candidate.illust_id.is_zero() {
            continue;
        }
        if batch
            .works
            .iter()
            .any(|work| work.id == candidate.illust_id)
            || batch.pending.contains(&candidate.illust_id)
        {
            continue;
        }
        batch.pending.push(candidate.illust_id);
    }
    Ok(batch)
}

/// /ajax/illust/discovery.
pub(crate) fn parse_illust_discovery(raw: &str) -> Result<Vec<Illust>, PixivError> {
    decode_body::<IllustDiscoveryWire>(raw).map(|wire| {
        wire.illusts
            .into_iter()
            .map(Illust::from_search)
            .filter(|illust| !illust.id.is_zero())
            .collect()
    })
}

/// /ajax/discovery/users.
pub(crate) fn parse_discovery_users(raw: &str) -> Result<Vec<RecommendUser>, PixivError> {
    decode_body::<DiscoveryUsersWire>(raw).map(|wire| {
        wire.users
            .into_iter()
            .map(RecommendUser::from)
            .filter(|user| !user.id.is_zero())
            .collect()
    })
}

/// /ajax/user/{id}/illusts?ids[]=... -- newest first.
///
/// The body is a map, so its key order is not the requested order; sorting by id
/// descending reproduces Pixiv's own newest-first ordering.
pub(crate) fn parse_user_illusts_batch(raw: &str) -> Result<Vec<Illust>, PixivError> {
    let map = decode_body::<HashMap<String, SearchItemWire>>(raw)?;
    Ok(newest_first(map))
}

/// /ajax/user/{id}/profile/illusts?ids[]=...
pub(crate) fn parse_profile_illusts(raw: &str) -> Result<Vec<Illust>, PixivError> {
    let wire = decode_body::<ProfileIllustsWire>(raw)?;
    Ok(newest_first(wire.works))
}

fn newest_first(map: HashMap<String, SearchItemWire>) -> Vec<Illust> {
    let mut rows: Vec<Illust> = map
        .into_values()
        .map(Illust::from_search)
        .filter(|illust| !illust.id.is_zero())
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.id));
    rows
}

/// /ajax/user/{id}/followers?limit=1 -- the total only.
pub(crate) fn parse_follower_total(raw: &str) -> Result<u64, PixivError> {
    decode_body::<FollowerListWire>(raw).map(|wire| wire.total.trim().parse::<u64>().unwrap_or(0))
}

/// /rpc/cps.php.
pub(crate) fn parse_completion(raw: &str) -> Result<Vec<CpsCandidateWire>, PixivError> {
    decode_plain::<CpsWire>(raw).map(|wire| wire.candidates)
}

/// /ajax/tags/suggest_by_word.
pub(crate) fn parse_tag_suggestions(raw: &str) -> Result<Vec<SuggestCandidateWire>, PixivError> {
    decode_body::<SuggestByWordWire>(raw).map(|wire| wire.candidates)
}

/// Merge both suggestion endpoints. Either side may be missing without losing
/// the other, which is why they are combined here rather than in the UI.
pub(crate) fn suggestions(
    completion: Option<Vec<CpsCandidateWire>>,
    tags: Option<Vec<SuggestCandidateWire>>,
    limit: usize,
) -> Vec<Suggestion> {
    merge_suggestions(
        completion.unwrap_or_default(),
        tags.unwrap_or_default(),
        limit,
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use pixiv_types::domain::{IllustKind, Tag};

    /// Read a captured response. The crate's tests never touch the network; the
    /// fixtures are the stand-in for the server.
    fn fixture(name: &str) -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
    }

    fn translation(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    // -- street feed, discovery and suggestions -----------------------------

    /// A street section item, shaped exactly like /ajax/street/for_you.
    fn street_thumb(id: &str) -> String {
        format!(
            r#"{{"type":"illust","pageCount":2,"pages":[{{"width":1200,"height":900,
            "urls":{{"1200x1200_standard":"https://i.pximg.net/p{id}_master1200.jpg",
            "540x540":"https://i.pximg.net/p{id}_540.jpg",
            "360x360":"https://i.pximg.net/p{id}_360.jpg"}}}}],
            "profileImageUrl":"https://i.pximg.net/u{id}_170.jpg",
            "id":"{id}","title":"t{id}","description":"d",
            "tags":[{{"name":"a","translatedName":"A","isEmphasized":false}}],
            "xRestrict":0,"userId":"42","userName":"artist","pixivId":"handle",
            "createDate":"2026-01-02 03:04:05","bookmarkable":true}}"#
        )
    }

    #[test]
    fn street_feed_keeps_the_author_avatar_and_orders_the_sections() {
        let raw = format!(
            r#"{{"error":false,"message":"","body":{{"contents":[
                {{"kind":"illust","thumbnails":[{}]}},
                {{"kind":"recommend_users","thumbnails":[],"users":[
                    {{"userId":"7","userName":"rec","profileImageUrl":"https://i.pximg.net/a.jpg"}}]}},
                {{"kind":"novel","thumbnails":[{{"id":"99","title":"n"}}]}}
            ]}}}}"#,
            street_thumb("11")
        );
        let feed = parse_street_for_you(&raw).expect("street feed parses");
        assert_eq!(feed.works.len(), 1, "novels must not become illustrations");
        let work = &feed.works[0];
        assert_eq!(work.id, Id(11));
        assert_eq!(work.page_count, 2);
        assert_eq!(work.width, 1200);
        assert_eq!(work.height, 900);
        // The 1200 tier is the grid tier, not the 360 crop.
        assert_eq!(work.urls.regular, "https://i.pximg.net/p11_master1200.jpg");
        assert_eq!(work.urls.small, "https://i.pximg.net/p11_540.jpg");
        // The avatar is the whole point of modelling this payload separately.
        assert_eq!(
            work.author.avatar.as_deref(),
            Some("https://i.pximg.net/u11_170.jpg")
        );
        assert_eq!(work.author.account, "handle");
        // Street tags arrive as objects with their own translation.
        assert_eq!(work.tags[0].display(), "A");
        assert_eq!(feed.artists.len(), 1);
        assert_eq!(feed.artists[0].name, "rec");
        assert_eq!(
            feed.artists[0].avatar.as_deref(),
            Some("https://i.pximg.net/a.jpg")
        );
    }

    #[test]
    fn street_feed_reports_recommended_tags_with_translations() {
        let raw = r#"{"error":false,"message":"","body":{"contents":[
            {"kind":"recommend_tags","thumbnails":[],"tags":[
                {"name":"風景","translatedName":"风景","isEmphasized":true},
                {"name":"fanart","translatedName":null}]}]}}"#;
        let feed = parse_street_for_you(raw).expect("tags parse");
        assert_eq!(feed.tags.len(), 2);
        assert_eq!(feed.tags[0].name, "風景");
        assert_eq!(feed.tags[0].display(), "风景");
        assert!(feed.tags[0].emphasized);
        // A null translation must not become the string "null".
        assert_eq!(feed.tags[1].display(), "fanart");
    }

    #[test]
    fn street_latest_keeps_its_own_title() {
        let raw = format!(
            r#"{{"error":false,"message":"","body":{{"content":
                {{"kind":"carousel","title":"精选新作","seeAllUrl":"/x","thumbnails":[{}]}}}}}}"#,
            street_thumb("5")
        );
        let latest = parse_street_latest(&raw).expect("latest parses");
        assert_eq!(latest.title, "精选新作");
        assert_eq!(latest.works.len(), 1);
        assert_eq!(latest.works[0].id, Id(5));
    }

    #[test]
    fn discovery_artworks_separates_ready_works_from_pending_ids() {
        let raw = r#"{"error":false,"message":"","body":{
                "recommendedIllusts":[
                    {"illustId":"5","recommendMethods":["m"],"recommendScore":1.0},
                    {"illustId":"6","recommendMethods":["m"],"recommendScore":1.0},
                    {"illustId":"5","recommendMethods":["m"],"recommendScore":1.0}],
                "thumbnails":{"illust":[
                    {"id":"5","title":"ready","url":"https://i.pximg.net/5.jpg",
                     "userId":"42","userName":"a","width":10,"height":10,"pageCount":1}]},
                "users":[],
                "tagTranslation":{}}}"#;
        let batch = parse_discovery_artworks(raw).expect("batch parses");
        assert_eq!(batch.works.len(), 1, "the pre-enriched work is renderable");
        assert_eq!(batch.works[0].title, "ready");
        // Id 5 was already described; only 6 still needs a detail call, and the
        // duplicate 5 must not appear twice.
        assert_eq!(batch.pending, vec![Id(6)]);
    }

    #[test]
    fn discovery_users_fold_both_payload_families_into_one_shape() {
        let raw = r#"{"error":false,"message":"","body":{"users":[
            {"userId":"1","name":"A","pixivId":"a","image":"https://i.pximg.net/a50.jpg",
             "imageBig":"https://i.pximg.net/a170.jpg","comment":"hi","isFollowed":true},
            {"userId":"2","userName":"B","profileImageUrl":"https://i.pximg.net/b.jpg",
             "userComment":"yo"}]}}"#;
        let users = parse_discovery_users(raw).expect("users parse");
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].name, "A");
        assert_eq!(
            users[0].avatar.as_deref(),
            Some("https://i.pximg.net/a170.jpg")
        );
        assert!(users[0].is_followed);
        // The street-style names must not be dropped on the floor.
        assert_eq!(users[1].name, "B");
        assert_eq!(
            users[1].avatar.as_deref(),
            Some("https://i.pximg.net/b.jpg")
        );
        assert_eq!(users[1].comment, "yo");
    }

    #[test]
    fn suggestions_join_the_official_order_with_real_counts() {
        let completion = parse_completion(
            r#"{"candidates":[
                {"tag_name":"初音ミク","access_count":"2405392582","type":"prefix"},
                {"tag_name":"風景","access_count":"928170755","tag_translation":"风景","type":"tag_translation"}]}"#,
        )
        .expect("completion parses");
        let counts = parse_tag_suggestions(
            r#"{"error":false,"message":"","body":{"candidates":[
                {"tag_name":"初音ミク","illust_count":742154,"total_count":742154,
                 "suggest_type":"front_matching"}]}}"#,
        )
        .expect("counts parse");
        let merged = merge_suggestions(completion, counts, 8);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].tag, "初音ミク");
        assert_eq!(merged[0].illust_count, Some(742_154));
        assert_eq!(merged[0].popularity, Some(2_405_392_582));
        // The translation is what the user should read, and it is not repeated
        // as its own row.
        assert_eq!(merged[1].display(), "风景");
        assert_eq!(merged[1].illust_count, None);
        for row in &merged {
            assert_eq!(row.source, pixiv_types::domain::SuggestionSource::Pixiv);
        }
    }

    #[test]
    fn suggestions_survive_one_endpoint_failing() {
        // Only the count endpoint answered: the list is still usable.
        let merged = merge_suggestions(
            Vec::new(),
            vec![SuggestCandidateWire {
                tag_name: "sky".to_string(),
                illust_count: 12,
                total_count: 12,
                suggest_type: "front_matching".to_string(),
            }],
            8,
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].tag, "sky");
        assert_eq!(merged[0].illust_count, Some(12));
    }

    #[test]
    fn local_history_is_marked_as_local() {
        let rows =
            pixiv_types::domain::history_suggestions(&["sky".to_string(), "  ".to_string()], 4);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].source,
            pixiv_types::domain::SuggestionSource::LocalHistory
        );
    }

    #[test]
    fn batch_work_maps_come_back_newest_first() {
        let raw = r#"{"error":false,"message":"","body":{
            "11":{"id":"11","title":"a","url":"https://i.pximg.net/a.jpg","pageCount":1},
            "13":{"id":"13","title":"c","url":"https://i.pximg.net/c.jpg","pageCount":1},
            "12":{"id":"12","title":"b","url":"https://i.pximg.net/b.jpg","pageCount":1}}}"#;
        let works = parse_user_illusts_batch(raw).expect("batch parses");
        assert_eq!(
            works.iter().map(|w| w.id).collect::<Vec<_>>(),
            vec![Id(13), Id(12), Id(11)]
        );
    }

    #[test]
    fn empty_discovery_batch_is_an_empty_result_not_an_error() {
        let raw = r#"{"error":false,"message":"","body":{"recommendedIllusts":[],
            "thumbnails":{},"users":[],"tagTranslation":{}}}"#;
        let batch = parse_discovery_artworks(raw).expect("empty batch parses");
        assert!(batch.works.is_empty());
        assert!(batch.pending.is_empty());
    }

    #[test]
    fn a_flagged_street_body_is_reported_as_an_error() {
        let raw = r#"{"error":true,"message":"不正确的请求。","body":[]}"#;
        assert!(matches!(parse_street_for_you(raw), Err(PixivError::Api(_))));
    }

    // -- fixtures ----------------------------------------------------------

    #[test]
    fn illust_detail_fixture_maps_through() {
        let illust = parse_illust_detail(&fixture("illust_detail.json")).expect("detail parses");
        assert_eq!(illust.id, Id(117_661_885));
        assert!(!illust.title.is_empty());
        assert_eq!(illust.kind, IllustKind::Illustration);
        assert_eq!(illust.page_count, 1);
        assert_eq!(illust.width, 2048);
        // Nested per-author tag groups must be flattened into one list.
        assert!(illust.tags.iter().any(|t: &Tag| !t.name.is_empty()));
        assert!(illust.urls.regular.contains("i.pximg.net"));
        assert!(illust.urls.original.ends_with(".jpg"));
        assert!(!illust.author.name.is_empty());
    }

    #[test]
    fn illust_pages_fixture_maps_through() {
        let pages = parse_illust_pages(&fixture("illust_pages.json")).expect("pages parse");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].index, 0);
        assert_eq!(pages[0].width, 2048);
        assert!(pages[0].urls.original.contains("img-original"));
        assert_eq!(pages[0].urls.largest(), pages[0].urls.original);
    }

    #[test]
    fn ugoira_meta_fixture_maps_through() {
        let meta = parse_ugoira_meta(&fixture("ugoira_meta.json")).expect("ugoira parses");
        assert_eq!(meta.frames.len(), 12);
        assert_eq!(meta.frames[0].file, "000000.jpg");
        assert_eq!(meta.frames[0].delay_ms, 200);
        assert!(meta.zip_url.contains("img-zip-ugoira"));
        assert!(meta.best_zip().contains("ugoira1920x1080"));
        assert!(meta.mime_type.contains("jpeg"));
    }

    #[test]
    fn search_fixture_maps_through() {
        let result = parse_search(&fixture("search_artworks.json")).expect("search parses");
        assert!(!result.items.is_empty());
        assert!(result.total > 0);
        assert!(result.last_page >= 1);
        let first = &result.items[0];
        assert!(!first.id.is_zero());
        assert!(!first.tags.is_empty());
        assert!(first.urls.grid().contains("i.pximg.net"));
    }

    #[test]
    fn ranking_fixture_maps_through() {
        let rows = parse_ranking(&fixture("ranking_daily.json")).expect("ranking parses");
        assert_eq!(rows.len(), 50);
        assert_eq!(rows[0].rank, 1);
        assert!(rows.iter().all(|row| !row.illust.id.is_zero()));
        assert!(rows.iter().all(|row| !row.illust.title.is_empty()));
    }

    #[test]
    fn user_profile_all_fixture_yields_descending_ids() {
        let ids = parse_user_illust_ids(&fixture("user_profile_all.json")).expect("profile parses");
        assert!(!ids.is_empty());
        assert!(ids.iter().all(|id| !id.is_zero()));
        assert!(
            ids.windows(2).all(|pair| pair[1] < pair[0]),
            "ids must be newest first and unique"
        );
    }

    #[test]
    fn tag_search_fixture_falls_back_to_romaji() {
        let translations =
            parse_tag_translations(&fixture("search_tags.json")).expect("tags parse");
        assert_eq!(translations.get("初").map(String::as_str), Some("hatsu"));
    }

    // -- mapping rules -----------------------------------------------------

    #[test]
    fn ranking_rank_comes_from_the_item_not_the_index() {
        let raw = r#"{"contents":[{"rank":"3","illust_id":11,"title":"third"},
                                  {"rank":"1","illust_id":22,"title":"first"}],
                      "mode":"daily","date":"2026年11月09日","page":1}"#;
        let rows = parse_ranking(raw).expect("synthetic ranking parses");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rank, 3);
        assert_eq!(rows[0].illust.id, Id(11));
        assert_eq!(rows[1].rank, 1);
        assert_eq!(rows[1].illust.id, Id(22));
    }

    #[test]
    fn user_ids_union_illusts_and_manga_newest_first() {
        let raw = r#"{"error":false,"message":"","body":{
            "illusts":{"100":null,"200":null,"not-an-id":null},
            "manga":{"200":null,"300":null},
            "novels":{"400":null},"pickup":[]}}"#;
        let ids = parse_user_illust_ids(raw).expect("union parses");
        assert_eq!(ids, vec![Id(300), Id(200), Id(100)]);
    }

    #[test]
    fn bookmark_pages_are_computed_with_ceiling_division() {
        let work = |id: &str| format!(r#"{{"id":"{id}","title":"t","illustType":0}}"#);
        let body = |total: i64| {
            format!(
                r#"{{"error":false,"message":"","body":{{"works":[{},{}],"total":{total}}}}}"#,
                work("1"),
                work("2")
            )
        };
        let three = parse_bookmarks(&body(97), 48).expect("bookmarks parse");
        assert_eq!(three.items.len(), 2);
        assert_eq!(three.total, 97);
        assert_eq!(three.last_page, 3);
        assert_eq!(parse_bookmarks(&body(48), 48).unwrap().last_page, 1);
        assert_eq!(parse_bookmarks(&body(49), 48).unwrap().last_page, 2);
        assert_eq!(parse_bookmarks(&body(0), 48).unwrap().last_page, 0);
        // A zero limit must not divide by zero.
        assert_eq!(parse_bookmarks(&body(0), 0).unwrap().last_page, 0);
        assert_eq!(
            parse_bookmarks(&body(97), 0).unwrap().last_page,
            u32::try_from(97).unwrap()
        );
        assert_eq!(crate::endpoint::BOOKMARK_PAGE_SIZE, 48);
    }

    #[test]
    fn bookmark_items_are_search_rows() {
        let raw = r#"{"error":false,"message":"","body":{"works":[
            {"id":"42","title":"hello","illustType":2,"url":"https://i.pximg.net/x.jpg",
             "userId":"9","userName":"a","pageCount":3,"tags":["t"],"isBookmarkable":true}],
            "total":1}}"#;
        let page = parse_bookmarks(raw, 48).expect("bookmark page parses");
        let illust = &page.items[0];
        assert_eq!(illust.id, Id(42));
        assert_eq!(illust.title, "hello");
        assert_eq!(illust.kind, IllustKind::Ugoira);
        assert_eq!(illust.page_count, 3);
        assert_eq!(illust.author.id, Id(9));
        assert_eq!(illust.tags.len(), 1);
    }

    #[test]
    fn user_profile_combines_detail_with_the_id_count() {
        let raw = r#"{"error":false,"message":"","body":{"userId":"77","name":"artist",
            "image":"https://i.pximg.net/a.png","imageBig":"https://i.pximg.net/b.png",
            "comment":"hi","pixivId":"acc","following":12,"mypixivCount":34,
            "isFollowed":true,"premium":false}}"#;
        let profile = parse_user_profile(raw, 5, 2).expect("profile parses");
        assert_eq!(profile.id, Id(77));
        assert_eq!(profile.name, "artist");
        assert_eq!(profile.account, "acc");
        assert_eq!(profile.illust_count, 5);
        assert_eq!(profile.manga_count, 2);
        assert_eq!(profile.followees, 12);
        // The follower count is not in this payload at all; deriving it from
        // mypixivCount would report a different number about a different thing.
        assert_eq!(profile.followers, None);
        assert!(profile.is_followed);
        assert_eq!(profile.avatar.as_deref(), Some("https://i.pximg.net/b.png"));
    }

    #[test]
    fn tag_translations_prefer_the_documented_key_order() {
        let map = translation(&[
            (
                "初",
                serde_json::json!({"zh":"第一","en":"first","romaji":"hatsu"}),
            ),
            ("sky", serde_json::json!({"en":"sky","romaji":"sora"})),
            ("空", serde_json::json!({"romaji":"sora"})),
            ("plain", serde_json::json!({"kana":"かな"})),
            ("scalar", serde_json::json!("direct")),
            ("empty", serde_json::json!({"zh":"   "})),
            ("num", serde_json::json!({"zh":1,"en":2})),
            ("null", Value::Null),
        ]);
        let out = tag_translations(&map);
        assert_eq!(out.get("初").map(String::as_str), Some("第一"));
        assert_eq!(out.get("sky").map(String::as_str), Some("sky"));
        assert_eq!(out.get("空").map(String::as_str), Some("sora"));
        assert_eq!(out.get("plain").map(String::as_str), Some("かな"));
        assert_eq!(out.get("scalar").map(String::as_str), Some("direct"));
        // Blank translations and non-string values are dropped, never guessed.
        assert!(!out.contains_key("empty"));
        assert!(!out.contains_key("num"));
        assert!(!out.contains_key("null"));
    }

    // -- identity, bookmark state, follow receipts -------------------------

    #[test]
    fn a_live_user_self_payload_yields_the_account_and_csrf_token() {
        // Shape captured from GET /ajax/user/self: it is *not* wrapped in the
        // usual {error, message, body} envelope.
        let raw = r#"{"userData":{"id":"55234129","pixivId":"helpmeeadice",
            "name":"HELPMEEADICE","profileImg":"https://i.pximg.net/s_50.jpg",
            "profileImgBig":"https://i.pximg.net/b_170.jpg","premium":false},
            "lang":"zh","token":"0123456789abcdef0123456789abcdef",
            "services":{"booth":"https://api.booth.pm"}}"#;
        let account = parse_account(raw).expect("account parses");
        assert_eq!(account.id, Id(55_234_129));
        assert_eq!(account.name, "HELPMEEADICE");
        assert_eq!(account.account, "helpmeeadice");
        assert_eq!(
            account.avatar.as_deref(),
            Some("https://i.pximg.net/b_170.jpg"),
            "the large avatar wins when both are present"
        );
        assert_eq!(
            account.csrf_token.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn user_data_null_is_not_a_session() {
        // An anonymous request answers 200 with userData: null. Reporting that as
        // "signed in" is exactly the bug this guards against.
        let raw = r#"{"userData":null,"lang":"zh",
            "token":"915b3c3b62fef576ed322a0ed557ba54","services":{}}"#;
        assert!(matches!(parse_account(raw), Err(PixivError::Unauthorized)));
        // An id of zero is equally unusable even if the object is present.
        let zero = r#"{"userData":{"id":"0","name":"nobody"},"token":"t"}"#;
        assert!(matches!(parse_account(zero), Err(PixivError::Unauthorized)));
    }

    #[test]
    fn bookmark_state_reports_presence_and_restrict() {
        let absent = r#"{"error":false,"message":"","body":
            {"id":151887140,"isBookmarkable":true,"bookmarkData":null}}"#;
        let state = parse_bookmark_state(absent).expect("state parses");
        assert!(!state.bookmarked);
        assert_eq!(state.id, None);

        let present = r#"{"error":false,"message":"","body":
            {"id":151887140,"isBookmarkable":true,
             "bookmarkData":{"id":"39714494245","private":true}}}"#;
        let state = parse_bookmark_state(present).expect("state parses");
        assert!(state.bookmarked);
        assert_eq!(state.restrict, BookmarkRestrict::Private);
        assert_eq!(state.id.as_deref(), Some("39714494245"));
    }

    #[test]
    fn follow_receipts_are_checked_not_assumed() {
        // bookmark_add.php answers an empty array on success.
        assert!(check_follow_add(200, "[]").is_ok());
        assert!(matches!(
            check_follow_add(200, r#"[{"error":"nope"}]"#),
            Err(PixivError::Api(_))
        ));
        assert!(matches!(
            check_follow_add(200, "not json"),
            Err(PixivError::Decode(_))
        ));
        assert!(matches!(
            check_follow_add(404, r#"{"error":true,"message":"gone"}"#),
            Err(PixivError::NotFound(_))
        ));

        // rpc_group_setting.php echoes the user it acted on.
        assert!(
            check_follow_remove(
                200,
                r#"{"user_id":"743604","type":"bookuser"}"#,
                Id(743_604)
            )
            .is_ok()
        );
        assert!(matches!(
            check_follow_remove(200, r#"{"user_id":"999","type":"bookuser"}"#, Id(743_604)),
            Err(PixivError::Decode(_))
        ));
        assert!(matches!(
            check_follow_remove(200, "{}", Id(743_604)),
            Err(PixivError::Decode(_))
        ));
        assert!(matches!(
            check_follow_remove(200, r#"{"error":true,"message":"no"}"#, Id(743_604)),
            Err(PixivError::Api(_))
        ));
    }

    #[test]
    fn follow_state_reads_the_flag_from_the_user_payload() {
        let following = r#"{"error":false,"message":"","body":
            {"userId":"743604","name":"a","isFollowed":true}}"#;
        assert!(parse_follow_state(following).expect("parses"));
        let not = r#"{"error":false,"message":"","body":
            {"userId":"743604","name":"a","isFollowed":false}}"#;
        assert!(!parse_follow_state(not).expect("parses"));
    }

    // -- envelope / error handling ----------------------------------------

    #[test]
    fn a_flagged_200_body_becomes_an_api_error() {
        let raw = r#"{"error":true,"message":"不正确的请求。","body":[]}"#;
        match parse_illust_pages(raw) {
            Err(PixivError::Api(message)) => assert_eq!(message, "不正确的请求。"),
            other => panic!("expected Api error, got {other:?}"),
        }
        assert!(matches!(
            parse_illust_detail(raw),
            Err(PixivError::Api(message)) if message == "不正确的请求。"
        ));
        assert!(matches!(
            parse_ranking(r#"{"error":true,"message":"nope"}"#),
            Err(PixivError::Api(_))
        ));
    }

    #[test]
    fn malformed_payloads_are_decode_errors_not_panics() {
        assert!(matches!(
            parse_illust_detail("not json at all"),
            Err(PixivError::Decode(_))
        ));
        assert!(matches!(
            parse_illust_detail(r#"{"error":false,"message":""}"#),
            Err(PixivError::Decode(_))
        ));
        // Body present but of the wrong shape.
        assert!(matches!(
            parse_illust_detail(r#"{"error":false,"message":"","body":[1,2,3]}"#),
            Err(PixivError::Decode(_))
        ));
        assert!(matches!(
            parse_ranking(r#"{"contents":"nope"}"#),
            Err(PixivError::Decode(_))
        ));
    }

    #[test]
    fn status_mapping_follows_the_documented_policy() {
        assert!(matches!(
            map_status_error(404, "{}"),
            PixivError::NotFound(_)
        ));
        assert!(matches!(
            map_status_error(401, ""),
            PixivError::Unauthorized
        ));
        assert!(matches!(
            map_status_error(403, ""),
            PixivError::Unauthorized
        ));
        // A 400 carrying Pixiv's own message is reported as an API error.
        match map_status_error(400, r#"{"error":true,"message":"不正确的请求。"}"#) {
            PixivError::Api(message) => assert_eq!(message, "不正确的请求。"),
            other => panic!("expected Api error, got {other:?}"),
        }
        // Anything else stays a transport error, with the body quoted.
        match map_status_error(502, "<html>bad gateway</html>") {
            PixivError::Http { status, message } => {
                assert_eq!(status, 502);
                assert!(message.contains("bad gateway"));
            }
            other => panic!("expected Http error, got {other:?}"),
        }
        // 429 must remain retryable so the backoff loop can honour Retry-After.
        let limited = map_status_error(429, "");
        assert!(matches!(limited, PixivError::Http { status: 429, .. }));
        assert!(limited.is_retryable());
        // Long bodies are truncated instead of being copied verbatim.
        let huge = "x".repeat(SNIPPET_LIMIT * 2);
        match map_status_error(500, &huge) {
            PixivError::Http { message, .. } => {
                assert_eq!(message.chars().count(), SNIPPET_LIMIT + 1);
            }
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[test]
    fn mutations_are_only_successful_when_the_body_agrees() {
        assert!(check_mutation(200, r#"{"error":false,"message":"","body":{}}"#).is_ok());
        assert!(check_mutation(200, "").is_ok());
        assert!(check_mutation(204, "   ").is_ok());
        match check_mutation(200, r#"{"error":true,"message":"bookmark failed"}"#) {
            Err(PixivError::Api(message)) => assert_eq!(message, "bookmark failed"),
            other => panic!("flagged 200 must not be success, got {other:?}"),
        }
        assert!(matches!(
            check_mutation(200, "<html>gate</html>"),
            Err(PixivError::Decode(_))
        ));
        assert!(matches!(
            check_mutation(404, ""),
            Err(PixivError::NotFound(_))
        ));
        assert!(matches!(
            check_mutation(403, ""),
            Err(PixivError::Unauthorized)
        ));
    }

    // -- retry policy ------------------------------------------------------

    #[test]
    fn backoff_starts_at_300ms_and_caps_at_5s() {
        assert_eq!(backoff_delay(0, None), Duration::from_millis(300));
        assert_eq!(backoff_delay(1, None), Duration::from_millis(600));
        assert_eq!(backoff_delay(2, None), Duration::from_millis(1200));
        assert_eq!(backoff_delay(3, None), Duration::from_millis(2400));
        assert_eq!(backoff_delay(4, None), Duration::from_millis(4800));
        assert_eq!(backoff_delay(5, None), Duration::from_millis(5000));
        assert_eq!(backoff_delay(60, None), Duration::from_millis(5000));
    }

    #[test]
    fn retry_after_wins_over_the_computed_backoff() {
        let hint = parse_retry_after("7").expect("seconds parse");
        assert_eq!(hint, Duration::from_secs(7));
        assert_eq!(backoff_delay(0, Some(hint)), Duration::from_secs(7));
        // Even a hint below the computed backoff is honoured: the server decides.
        assert_eq!(
            backoff_delay(4, Some(Duration::from_millis(1))),
            Duration::from_millis(1)
        );
        // Absurd hints are bounded so the client cannot be stalled indefinitely.
        assert_eq!(
            backoff_delay(0, Some(Duration::from_secs(86_400))),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn retry_after_parsing_rejects_garbage_and_dates() {
        assert_eq!(parse_retry_after(" 120 "), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("soon"), None);
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-5"), None);
    }

    #[test]
    fn retries_stop_at_the_budget_and_skip_permanent_failures() {
        let retryable = PixivError::Network("reset".into());
        assert!(should_retry(0, 2, &retryable));
        assert!(should_retry(1, 2, &retryable));
        assert!(!should_retry(2, 2, &retryable));
        assert!(!should_retry(0, 0, &retryable));

        assert!(!should_retry(0, 2, &PixivError::Unauthorized));
        assert!(!should_retry(0, 2, &PixivError::NotFound("x".into())));
        assert!(!should_retry(0, 2, &PixivError::Api("x".into())));
        assert!(should_retry(0, 2, &PixivError::http(503, "down")));
        assert!(should_retry(0, 2, &PixivError::http(429, "slow")));
        assert!(!should_retry(0, 2, &PixivError::http(400, "bad")));
    }
}
