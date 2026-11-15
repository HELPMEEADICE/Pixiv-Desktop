//! Verbatim `serde` mirrors of the JSON served by `pixiv.net`.
//!
//! Every struct in this module was written against fixtures captured from the
//! live site (see `fixtures/`). Nothing here is inferred from documentation,
//! because Pixiv publishes none: field presence is discovered, not specified.
//!
//! Rules that keep this layer honest:
//!
//! * Anything that may be absent is `Option<T>` or `#[serde(default)]`.
//! * Fields the client does not need are simply not declared — `serde` ignores
//!   unknown keys by default, so an endpoint adding a field never breaks us.
//! * No endpoint is modelled here unless it has been observed to answer.

use std::collections::HashMap;

use serde::Deserialize;
use serde::de::{self, Deserializer};

use crate::Id;

/// Deserialisers for fields whose JSON type is not stable.
///
/// These exist because Pixiv genuinely does not keep a consistent type per
/// field across endpoints: `ranking.php` sends `"illust_type":"0"` while
/// `ajax/*` sends `"illustType":0`, and `createDate` is a string in one payload
/// and an object in another. Rather than let one changed field take down a whole
/// response, the client coerces and moves on.
pub mod flex {
    use super::{Deserializer, de};
    use serde::Deserialize;

    fn value<'de, D: Deserializer<'de>>(d: D) -> Result<serde_json::Value, D::Error> {
        serde_json::Value::deserialize(d).map_err(de::Error::custom)
    }

    /// Accept `0`, `"0"`, or anything numeric-ish; fall back to 0.
    pub fn u32<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
        let v = value(d)?;
        Ok(match v {
            serde_json::Value::Number(n) => n.as_u64().unwrap_or(0) as u32,
            serde_json::Value::String(s) => s.trim().parse::<u32>().unwrap_or(0),
            _ => 0,
        })
    }

    /// Accept `0`, `"0"`, or anything numeric-ish; fall back to 0.
    pub fn i64<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
        let v = value(d)?;
        Ok(match v {
            serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
            serde_json::Value::String(s) => s.trim().parse::<i64>().unwrap_or(0),
            _ => 0,
        })
    }

    /// Accept an object, and treat an empty array as the same empty map.
    ///
    /// /ajax/user/{id}/profile/all answers {"illusts":{},"manga":{}} for a
    /// prolific artist but [] for a user with no works in a category, so a plain
    /// HashMap rejects half the accounts with "invalid type: sequence".
    pub fn map<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<std::collections::HashMap<String, Option<serde_json::Value>>, D::Error> {
        let v = value(d)?;
        match v {
            serde_json::Value::Object(map) => Ok(map
                .into_iter()
                .map(|(key, value)| (key, Some(value)))
                .collect()),
            serde_json::Value::Array(items) if items.is_empty() => {
                Ok(std::collections::HashMap::new())
            }
            serde_json::Value::Null => Ok(std::collections::HashMap::new()),
            other => Err(de::Error::custom(format!(
                "expected a map or an empty array, got {other}"
            ))),
        }
    }

    /// Accept a string; yield `""` for objects, numbers, null and arrays.
    pub fn string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        let v = value(d)?;
        Ok(match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            _ => String::new(),
        })
    }
}

/// `illust_detail` sends `tags` as one object; other payloads send an array.
/// Normalise both into a `Vec`.
fn tag_groups<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<TagGroupWire>, D::Error> {
    let v = serde_json::Value::deserialize(d).map_err(de::Error::custom)?;
    match v {
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::Array(_) => {
            serde_json::from_value::<Vec<TagGroupWire>>(v).map_err(de::Error::custom)
        }
        serde_json::Value::Object(_) => serde_json::from_value::<TagGroupWire>(v)
            .map(|one| vec![one])
            .map_err(de::Error::custom),
        other => Err(de::Error::custom(format!(
            "unexpected tags payload: {other}"
        ))),
    }
}

/// The `{"error": bool, "message": String, "body": T}` envelope used by every
/// `ajax/*` endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct Envelope<T> {
    #[serde(default)]
    pub error: bool,
    #[serde(default)]
    pub message: String,
    pub body: T,
}

/// Illustration image URLs. `i.pximg.net` refuses requests without a Referer.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IllustUrlsWire {
    #[serde(default)]
    pub mini: String,
    #[serde(default)]
    pub thumb: String,
    #[serde(default)]
    pub small: String,
    #[serde(default)]
    pub regular: String,
    #[serde(default)]
    pub original: String,
}

/// Per-page image URLs. Note `thumb_mini`, whereas [`IllustUrlsWire`] uses
/// `mini`; the two endpoints genuinely differ.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PageUrlsWire {
    #[serde(default)]
    pub thumb_mini: String,
    #[serde(default)]
    pub small: String,
    #[serde(default)]
    pub regular: String,
    #[serde(default)]
    pub original: String,
}

/// A single tag attached to an illustration.
#[derive(Debug, Clone, Deserialize)]
pub struct IllustTagWire {
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub locked: Option<bool>,
    #[serde(default)]
    pub deletable: Option<bool>,
    #[serde(default)]
    pub user_id: Option<Id>,
    /// Locale translations, e.g. `{"en": "光·遇"}`.
    #[serde(default)]
    pub translation: Option<HashMap<String, String>>,
    #[serde(default)]
    pub user_name: Option<String>,
}

/// Illustration tags arrive wrapped: one entry per author that added tags.
///
/// `{"authorId":"104439010","isLocked":false,"tags":[{...}],"writable":false}`
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagGroupWire {
    #[serde(default)]
    pub author_id: Option<Id>,
    #[serde(default)]
    pub is_locked: Option<bool>,
    #[serde(default)]
    pub tags: Vec<IllustTagWire>,
    #[serde(default)]
    pub writable: Option<bool>,
}

/// The viewer's own bookmark state for an illustration.
#[derive(Debug, Clone, Deserialize)]
pub struct BookmarkDataWire {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, rename = "private")]
    pub is_private: bool,
}

/// `GET /ajax/user/self` document.
///
/// Unlike every other endpoint in this module this one is **not** wrapped in an
/// `{error, message, body}` envelope: `userData`, `token` and `services` sit at
/// the top level. `user_data` is `null` for an anonymous request, which is how
/// the client tells "no session" apart from "expired session" (that one is a
/// `401`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSelfWire {
    #[serde(default)]
    pub user_data: Option<UserSelfDataWire>,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub lang: String,
    #[serde(default)]
    pub services: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSelfDataWire {
    #[serde(default)]
    pub id: Id,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pixiv_id: String,
    #[serde(default)]
    pub profile_img: String,
    #[serde(default)]
    pub profile_img_big: String,
    #[serde(default)]
    pub premium: bool,
}

/// `GET /ajax/illust/{id}/bookmarkData` body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkStateWire {
    #[serde(default)]
    pub id: Id,
    #[serde(default)]
    pub is_bookmarkable: bool,
    #[serde(default)]
    pub bookmark_data: Option<BookmarkDataWire>,
}

/// `/ajax/illust/{id}` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IllustDetailWire {
    #[serde(default)]
    pub illust_id: Id,
    #[serde(default)]
    pub illust_title: String,
    #[serde(default)]
    pub illust_comment: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "illustType")]
    pub illust_type: i32,
    #[serde(default)]
    pub create_date: String,
    #[serde(default)]
    pub upload_date: String,
    #[serde(default)]
    pub x_restrict: i32,
    #[serde(default)]
    pub sl: i32,
    #[serde(default, rename = "aiType")]
    pub ai_type: i32,
    #[serde(default)]
    pub urls: IllustUrlsWire,
    #[serde(default, deserialize_with = "tag_groups")]
    pub tags: Vec<TagGroupWire>,
    #[serde(default)]
    pub user_id: Id,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub user_account: String,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(default)]
    pub page_count: i32,
    #[serde(default)]
    pub bookmark_count: i64,
    #[serde(default)]
    pub like_count: i64,
    #[serde(default)]
    pub comment_count: i64,
    #[serde(default)]
    pub view_count: i64,
    #[serde(default)]
    pub is_bookmarkable: bool,
    #[serde(default)]
    pub bookmark_data: Option<BookmarkDataWire>,
    #[serde(default)]
    pub is_masked: bool,
    #[serde(default)]
    pub alt: String,
}

/// `/ajax/illust/{id}/pages` body element.
#[derive(Debug, Clone, Deserialize)]
pub struct IllustPageWire {
    #[serde(default)]
    pub urls: PageUrlsWire,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
}

/// `/ajax/illust/{id}/ugoira_meta` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UgoiraMetaWire {
    /// Downscaled ZIP (`ugoira600x600.zip`).
    #[serde(default)]
    pub src: String,
    /// Original-resolution ZIP. This is the one to download for export.
    #[serde(default)]
    pub original_src: String,
    /// Read from `"mime_type"`: this endpoint alone uses snake_case.
    #[serde(default, rename = "mime_type", alias = "mimeType")]
    pub mime_type: String,
    #[serde(default)]
    pub frames: Vec<UgoiraFrameWire>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UgoiraFrameWire {
    #[serde(default)]
    pub file: String,
    /// Frame duration in milliseconds.
    #[serde(default)]
    pub delay: u32,
}

/// `/ajax/search/artworks/{keyword}` body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchBodyWire {
    #[serde(default)]
    pub illust_manga: IllustMangaWire,
    #[serde(default)]
    pub popular: Option<serde_json::Value>,
    #[serde(default)]
    pub related_tags: Vec<String>,
    /// `{"鏡音リン": {"zh": "鏡音鈴"}}` — values are objects, not strings.
    #[serde(default)]
    pub tag_translation: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IllustMangaWire {
    #[serde(default)]
    pub data: Vec<SearchItemWire>,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub last_page: i64,
}

/// A search-result row.
///
/// Search rows carry a different subset than the detail endpoint: tags are a flat
/// `Vec<String>` and only a square thumbnail `url` is provided.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchItemWire {
    pub id: Id,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "illustType")]
    pub illust_type: i32,
    #[serde(default)]
    pub x_restrict: i32,
    #[serde(default)]
    pub sl: i32,
    /// Square thumbnail (`.../c/250x250_80_a2/...`).
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub user_id: Id,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(default)]
    pub page_count: i32,
    #[serde(default)]
    pub bookmark_data: Option<BookmarkDataWire>,
    #[serde(default)]
    pub is_bookmarkable: bool,
    /// Sometimes a string, sometimes a `{"Date": ...}` object.
    #[serde(default, deserialize_with = "flex::string")]
    pub create_date: String,
    #[serde(default)]
    pub is_masked: bool,
    #[serde(default)]
    pub profile_image_url: String,
}

/// `ranking.php?format=json` document.
///
/// This endpoint is *not* under `ajax/`, sends ids as numbers, and uses
/// snake_case keys.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RankingResponseWire {
    #[serde(default)]
    pub contents: Vec<RankingItemWire>,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub page: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RankingItemWire {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub url: String,
    /// `ranking.php` sends these as strings (`"0"`), unlike `ajax/*`.
    #[serde(default, deserialize_with = "flex::i64")]
    pub illust_type: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub illust_page_count: i64,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub profile_img: String,
    #[serde(default)]
    pub illust_id: Id,
    #[serde(default, deserialize_with = "flex::i64")]
    pub width: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub height: i64,
    #[serde(default)]
    pub user_id: Id,
    #[serde(default, deserialize_with = "flex::i64")]
    pub rank: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub rating_count: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub view_count: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub illust_upload_timestamp: i64,
    #[serde(default)]
    pub is_masked: bool,
}

/// `/ajax/user/{id}/profile/all` body.
///
/// `illusts` is a map of id -> null; it is the complete id list and is the only
/// way to enumerate an artist's works without authentication.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfileAllWire {
    #[serde(default, deserialize_with = "flex::map")]
    pub illusts: HashMap<String, Option<serde_json::Value>>,
    #[serde(default, deserialize_with = "flex::map")]
    pub manga: HashMap<String, Option<serde_json::Value>>,
    /// Novel ids, with the same quirk the other two maps have: an object for a
    /// user who has novels and an empty *array* for one who has none.
    #[serde(default, deserialize_with = "flex::map")]
    pub novels: HashMap<String, Option<serde_json::Value>>,
    #[serde(default)]
    pub pickup: Vec<serde_json::Value>,
}

/// `/ajax/user/{id}` body.
///
/// Marked experimental: the endpoint answers unauthenticated but its payload is
/// large and partially unknown, so only the fields the UI needs are modelled.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserDetailWire {
    #[serde(default)]
    pub user_id: Id,
    #[serde(default)]
    pub name: String,
    /// Handle without the @. This key is pixivId on the live endpoint; reading
    /// "account" instead silently yields an empty handle for every user.
    #[serde(default)]
    pub pixiv_id: String,
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub image_big: String,
    #[serde(default)]
    pub comment: String,
    /// Profile cover image. null when the user did not set one.
    #[serde(default)]
    pub background: Option<serde_json::Value>,
    /// How many accounts this user follows. The wire key is "following"; the
    /// follower count is not sent by this endpoint at all.
    #[serde(default)]
    pub following: i64,
    #[serde(default)]
    pub mypixiv_count: i64,
    #[serde(default)]
    pub is_followed: bool,
    #[serde(default)]
    pub is_mypixiv: bool,
    #[serde(default)]
    pub is_blocking: bool,
    #[serde(default)]
    pub followed_back: bool,
    #[serde(default)]
    pub premium: bool,
    #[serde(default)]
    pub webpage: Option<String>,
}

/// `/ajax/search/tags/{tag}` body.
///
/// This endpoint returns tag *metadata* and translations. It is not an
/// autocomplete feed, so the search box offers history and previous queries
/// rather than pretending to have server-side suggestions.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTagsWire {
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub word: String,
    /// Values are objects such as `{"romaji":"hatsu"}`, not plain strings.
    #[serde(default)]
    pub tag_translation: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub my_favorite_tags: Vec<String>,
}

/// `/ajax/user/{id}/illusts` body (requires a session).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserIllustsWire {
    #[serde(default)]
    pub illusts: HashMap<String, Option<serde_json::Value>>,
}

/// `/ajax/user/{id}/illusts/bookmarks` body (requires a session).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserBookmarksWire {
    #[serde(default)]
    pub works: Vec<SearchItemWire>,
    #[serde(default)]
    pub total: i64,
}

// ===========================================================================
// Street feed (/ajax/street/*) -- the real home page
// ===========================================================================
//
// The street endpoints do not share the shape of search/detail rows, and that
// mismatch is why the home page used to fall back to the daily ranking:
//
// * the cover tiers live in pages[].urls under *pixel-dimension* keys
//   ("1200x1200_standard", "540x540", "360x360") rather than in the named
//   mini/thumb/small/regular/original fields;
// * the author's avatar travels as profileImageUrl and the handle as pixivId,
//   so nothing in the generic illust wire could carry them;
// * a feed is a list of *sections*, not a list of works.

/// One page of a street-feed work. Keys are pixel sizes, not tier names.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreetPageWire {
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(default)]
    pub urls: HashMap<String, String>,
}

impl StreetPageWire {
    /// Best URL for the given preference order.
    #[must_use]
    pub fn pick(&self, order: &[&str]) -> String {
        for key in order {
            if let Some(url) = self.urls.get(*key)
                && !url.trim().is_empty()
            {
                return url.clone();
            }
        }
        // Unknown tier set: take the longest value, which is the largest variant
        // in practice, rather than rendering a broken card.
        self.urls
            .values()
            .max_by_key(|url| url.len())
            .cloned()
            .unwrap_or_default()
    }
}

/// A work inside a street-feed section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreetThumbWire {
    /// "illust" / "manga" / "ugoira" / "novel".
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub page_count: i32,
    #[serde(default)]
    pub pages: Vec<StreetPageWire>,
    /// The author's avatar. Present on every street payload, which is what makes
    /// a real avatar possible on the home and discovery pages.
    #[serde(default)]
    pub profile_image_url: String,
    /// Kept as text: a "collection" section carries a synthetic id that does
    /// not fit in a u64 (74493503404124799791), and a strict Id would reject the
    /// whole feed over one card the grid cannot render anyway.
    #[serde(default, deserialize_with = "flex::string")]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Tag *objects* here, not the flat strings the search routes send, and each
    /// one carries its own translation.
    #[serde(default)]
    pub tags: Vec<StreetTagWire>,
    #[serde(default)]
    pub restrict: i32,
    #[serde(default)]
    pub x_restrict: i32,
    #[serde(default)]
    pub user_id: Id,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub pixiv_id: String,
    #[serde(default, deserialize_with = "flex::string")]
    pub create_date: String,
    #[serde(default)]
    pub ai_type: i32,
    #[serde(default)]
    pub sl: i32,
    #[serde(default)]
    pub is_sensitive_for_masking: bool,
    #[serde(default)]
    pub bookmarkable: bool,
}

/// One section of /ajax/street/for_you or /ajax/street/sub.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreetContentWire {
    /// "illust", "manga", "collection", "novel", "recommend_users", "contests".
    #[serde(default)]
    pub kind: String,
    /// The type of this array depends on the section kind, which is why it stays
    /// raw here: an illust section carries works, and a recommend_users section
    /// carries *users* in this very same key. Decoding it eagerly would silently
    /// drop the recommended artists.
    #[serde(default)]
    pub thumbnails: Vec<serde_json::Value>,
    /// Present on recommend_users sections.
    #[serde(default)]
    pub users: Vec<DiscoveryUserWire>,
    #[serde(default)]
    pub tags: Vec<StreetTagWire>,
}

/// A personalised tag inside a recommend_tags section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreetTagWire {
    #[serde(default)]
    pub name: String,
    /// Localised name. null for tags Pixiv has no translation for.
    #[serde(default)]
    pub translated_name: Option<String>,
    #[serde(default)]
    pub is_emphasized: bool,
}

/// /ajax/street/for_you body.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreetForYouWire {
    #[serde(default)]
    pub contents: Vec<StreetContentWire>,
}

/// /ajax/street/latest body: a single titled carousel section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreetLatestWire {
    #[serde(default)]
    pub content: StreetLatestSectionWire,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreetLatestSectionWire {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub see_all_url: String,
    #[serde(default)]
    pub thumbnails: Vec<StreetThumbWire>,
}

/// /ajax/street/recommend_tags body.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreetRecommendTagsWire {
    #[serde(default)]
    pub content: StreetRecommendTagsSectionWire,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreetRecommendTagsSectionWire {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<StreetTagWire>,
}

/// /ajax/street/sub body -- the "further for you" row, which is where the
/// recommended artists live.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreetSubWire {
    #[serde(default)]
    pub contents: Vec<StreetContentWire>,
}

// ===========================================================================
// Discovery
// ===========================================================================

/// /ajax/discovery/artworks body.
///
/// The endpoint answers 60 *ids* plus a small pre-enriched map, so the client
/// has to decide how much detail to hydrate. It is not paginated: each call
/// returns a fresh batch of recommendations.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryArtworksWire {
    #[serde(default)]
    pub recommended_illusts: Vec<RecommendedIllustWire>,
    /// { "illust": [ ... ] } -- works Pixiv already enriched for the first
    /// screen. Anything else has to be fetched by id.
    #[serde(default)]
    pub thumbnails: HashMap<String, Vec<SearchItemWire>>,
    #[serde(default)]
    pub users: Vec<DiscoveryUserWire>,
    #[serde(default)]
    pub tag_translation: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecommendedIllustWire {
    #[serde(default)]
    pub illust_id: Id,
    #[serde(default)]
    pub recommend_methods: Vec<String>,
    #[serde(default)]
    pub recommend_score: f64,
}

/// /ajax/illust/discovery body.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IllustDiscoveryWire {
    #[serde(default)]
    pub illusts: Vec<SearchItemWire>,
}

/// /ajax/discovery/users and the recommend_users street section.
///
/// Both shapes are folded into one wire type because they carry the same
/// identity under different field names: the discovery route sends name/image
/// and the street section sends userName/profileImageUrl.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryUserWire {
    #[serde(default)]
    pub user_id: Id,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pixiv_id: String,
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub image_big: String,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub is_followed: bool,
    #[serde(default)]
    pub is_blocking: bool,
    #[serde(default)]
    pub is_mypixiv: bool,
    #[serde(default)]
    pub premium: bool,
    #[serde(default)]
    pub partial: i32,
    #[serde(default)]
    pub followed_back: bool,
    #[serde(default)]
    pub background: Option<serde_json::Value>,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub user_comment: String,
    #[serde(default)]
    pub profile_image_url: String,
    #[serde(default)]
    pub profile_background_image_url: String,
    #[serde(default)]
    pub thumbnails: Vec<StreetThumbWire>,
    #[serde(default)]
    pub is_self: bool,
    #[serde(default)]
    pub illusts: Vec<SearchItemWire>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DiscoveryUsersWire {
    #[serde(default)]
    pub users: Vec<DiscoveryUserWire>,
}

// ===========================================================================
// Search suggestions
// ===========================================================================

/// /ajax/tags/suggest_by_word body -- prefix-matched tags with work counts.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestByWordWire {
    #[serde(default)]
    pub candidates: Vec<SuggestCandidateWire>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SuggestCandidateWire {
    #[serde(default)]
    pub tag_name: String,
    #[serde(default)]
    pub illust_count: i64,
    #[serde(default)]
    pub total_count: i64,
    /// front_matching, tag_translation, ...
    #[serde(default)]
    pub suggest_type: String,
}

/// /rpc/cps.php -- the document the site's own search box requests.
///
/// Not enveloped: the candidates array is the whole document.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CpsWire {
    #[serde(default)]
    pub candidates: Vec<CpsCandidateWire>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CpsCandidateWire {
    #[serde(default)]
    pub tag_name: String,
    /// Sent as a string by the live endpoint.
    #[serde(default, deserialize_with = "flex::string")]
    pub access_count: String,
    /// prefix or tag_translation.
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub tag_translation: Option<String>,
}

// ===========================================================================
// User works in bulk
// ===========================================================================

/// /ajax/user/{id}/illusts?ids[]=... -- a map of id to work, in the endpoint's
/// own order. Requires a session.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserIllustsBatchWire(pub HashMap<String, SearchItemWire>);

/// /ajax/user/{id}/profile/illusts?ids[]=... body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileIllustsWire {
    /// Map of id to work. A plain object, not an array.
    #[serde(default)]
    pub works: HashMap<String, SearchItemWire>,
}
/// /ajax/user/{id}/followers body -- only readable for the signed-in account.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FollowerListWire {
    #[serde(default)]
    pub users: Vec<DiscoveryUserWire>,
    /// Sent as a string by the live endpoint.
    #[serde(default, deserialize_with = "flex::string")]
    pub total: String,
}

// ===========================================================================
// Novels (/ajax/novel/*, /ajax/search/novels/*, /ajax/user/{id}/novels)
// ===========================================================================
//
// A novel is a second content type and almost none of its vocabulary matches the
// illustration payloads above:
//
// * the *body* of a novel travels inside the detail document as raw markup
//   ("content"). "/ajax/novel/{id}/text" is gone -- it answers 404 -- so there is
//   nothing left to fetch separately, and one document stays one request.
// * the character count is "characterCount" on the detail but "textCount" on
//   every row, and the detail document has no "textLength" field at all.
// * the cover is "coverUrl" on the detail and "url" on a row.
// * embedded images are a map keyed by image id under "textEmbeddedImages", with
//   pixel-dimension keys instead of the named mini/thumb/small tiers.
// * the chapter list is "/ajax/novel/series_content/{id}";
//   "/ajax/novel/series/{id}/content" answers 404.

/// URL tiers of a novel image or of a series cover.
///
/// The keys are pixel dimensions, and the set is wider than the illustration
/// tiers: "240mw" / "480mw" are the reading-width crops, "1200x1200" is the
/// master cover, "128x128" is the square crop, and "original" is whatever the
/// author uploaded.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NovelImageUrlsWire {
    #[serde(default, rename = "240mw")]
    pub mw240: String,
    #[serde(default, rename = "480mw")]
    pub mw480: String,
    #[serde(default, rename = "1200x1200")]
    pub master1200: String,
    #[serde(default, rename = "128x128")]
    pub square: String,
    #[serde(default)]
    pub original: String,
}

impl NovelImageUrlsWire {
    /// Reading-width tier for an illustration inside the body.
    ///
    /// "480mw" is what the web reader draws at. The fallbacks walk inward first
    /// ("240mw", then the square crop) and outward second ("1200x1200", then the
    /// original): a slightly smaller crop of the right picture is a better card
    /// than a multi-megabyte original of it.
    #[must_use]
    pub fn small(&self) -> &str {
        Self::first(&[
            self.mw480.as_str(),
            self.mw240.as_str(),
            self.square.as_str(),
            self.master1200.as_str(),
            self.original.as_str(),
        ])
    }

    /// Full-width tier for the reader and its lightbox.
    ///
    /// "1200x1200" is the master the site itself renders; "original" is the
    /// largest but is often an unoptimised PNG, so it is the second choice.
    #[must_use]
    pub fn large(&self) -> &str {
        Self::first(&[
            self.master1200.as_str(),
            self.original.as_str(),
            self.mw480.as_str(),
            self.mw240.as_str(),
            self.square.as_str(),
        ])
    }

    /// Tier for a series or novel cover on a card: the master, then the original.
    #[must_use]
    pub fn cover(&self) -> &str {
        Self::first(&[
            self.master1200.as_str(),
            self.original.as_str(),
            self.mw480.as_str(),
            self.mw240.as_str(),
            self.square.as_str(),
        ])
    }

    fn first<'a>(candidates: &[&'a str]) -> &'a str {
        candidates
            .iter()
            .copied()
            .find(|url| !url.trim().is_empty())
            .unwrap_or_default()
    }
}

/// One entry of "textEmbeddedImages".
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelImageWire {
    #[serde(default)]
    pub novel_image_id: String,
    /// Sensitivity level. Sent as a *string* here ("2"), unlike the numeric "sl"
    /// on illustration payloads.
    #[serde(default, deserialize_with = "flex::string")]
    pub sl: String,
    #[serde(default)]
    pub urls: NovelImageUrlsWire,
}

/// Decode "textEmbeddedImages": a map keyed by image id, "[]" when empty.
///
/// The same quirk the profile id maps have: a novel without illustrations is
/// answered with an empty *array* where an object belongs, and a plain
/// HashMap would reject the whole document over it.
fn novel_image_map<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<HashMap<String, NovelImageWire>, D::Error> {
    let value = serde_json::Value::deserialize(d).map_err(de::Error::custom)?;
    Ok(match value {
        serde_json::Value::Object(map) => map
            .into_iter()
            // One malformed image must not blank the body around it.
            .filter_map(|(key, image)| {
                serde_json::from_value::<NovelImageWire>(image)
                    .ok()
                    .map(|image| (key, image))
            })
            .collect(),
        _ => HashMap::new(),
    })
}

/// One neighbour of a series chapter.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelSeriesLinkWire {
    #[serde(default)]
    pub id: Id,
    #[serde(default, deserialize_with = "flex::i64")]
    pub order: i64,
    #[serde(default)]
    pub title: String,
    /// False when the chapter is private or deleted. Kept as an Option so a
    /// payload that omits the key still counts as available: only an explicit
    /// false is a refusal.
    #[serde(default)]
    pub available: Option<bool>,
}

/// "seriesNavData": which series this chapter belongs to, plus its neighbours.
///
/// null for a standalone novel, which is why the detail struct keeps it as an
/// Option and the domain type keeps an Option in turn.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelSeriesNavWire {
    #[serde(default)]
    pub series_type: String,
    #[serde(default)]
    pub series_id: Id,
    #[serde(default)]
    pub title: String,
    /// 1-based position of this chapter inside the series.
    #[serde(default, deserialize_with = "flex::i64")]
    pub order: i64,
    #[serde(default)]
    pub prev: Option<NovelSeriesLinkWire>,
    #[serde(default)]
    pub next: Option<NovelSeriesLinkWire>,
    #[serde(default)]
    pub is_concluded: bool,
}

/// "/ajax/novel/{id}" body -- the whole novel, body included.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelDetailWire {
    #[serde(default)]
    pub id: Id,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub user_id: Id,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub cover_url: String,
    /// The HTML-ish blurb: "<br />" line breaks and "<a href>" links.
    #[serde(default)]
    pub description: String,
    /// The novel itself, as Pixiv's own markup ("[[rb:.. > ..]]",
    /// "[uploadedimage:..]", "[[jumpuri:..]]"). Never rewritten in this layer.
    #[serde(default)]
    pub content: String,
    /// Nested exactly like illustration tags: one group per author.
    #[serde(default, deserialize_with = "tag_groups")]
    pub tags: Vec<TagGroupWire>,
    /// The viewer's own bookmark row; null when not bookmarked.
    #[serde(default)]
    pub bookmark_data: Option<BookmarkDataWire>,
    #[serde(default, deserialize_with = "flex::i64")]
    pub view_count: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub bookmark_count: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub like_count: i64,
    /// The character count. There is no "textLength" in this document.
    #[serde(default, deserialize_with = "flex::i64")]
    pub character_count: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub word_count: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub reading_time: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub page_count: i64,
    #[serde(default)]
    pub create_date: String,
    #[serde(default)]
    pub upload_date: String,
    #[serde(default)]
    pub is_original: bool,
    #[serde(default, deserialize_with = "flex::i64")]
    pub x_restrict: i64,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub series_nav_data: Option<NovelSeriesNavWire>,
    #[serde(default, deserialize_with = "novel_image_map")]
    pub text_embedded_images: HashMap<String, NovelImageWire>,
}

/// A novel row: the shape shared by search results, series chapters and
/// "/ajax/user/{id}/novels".
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelItemWire {
    #[serde(default)]
    pub id: Id,
    #[serde(default)]
    pub title: String,
    /// Genre code, sent as a string ("1"). Kept as text rather than decoded to a
    /// number that a future code would silently break.
    #[serde(default, deserialize_with = "flex::string")]
    pub genre: String,
    #[serde(default, deserialize_with = "flex::i64")]
    pub x_restrict: i64,
    /// The cover. A row calls this "url"; only the detail document uses
    /// "coverUrl", so the two must not be confused.
    #[serde(default)]
    pub url: String,
    /// Flat tag names, unlike the detail document's per-author groups.
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub user_id: Id,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub pixiv_id: String,
    #[serde(default)]
    pub profile_image_url: String,
    /// The character count on a row ("textCount"), not "characterCount".
    #[serde(default, deserialize_with = "flex::i64")]
    pub text_count: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub word_count: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub reading_time: i64,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub bookmark_data: Option<BookmarkDataWire>,
    #[serde(default, deserialize_with = "flex::i64")]
    pub bookmark_count: i64,
    #[serde(default)]
    pub is_original: bool,
    /// Sometimes a string, sometimes an object, as on every other payload
    /// carrying a date.
    #[serde(default, deserialize_with = "flex::string")]
    pub create_date: String,
    /// null for a standalone work, the series id for a chapter.
    #[serde(default)]
    pub series_id: Option<Id>,
    #[serde(default)]
    pub series_title: String,
    /// This chapter's position inside its series.
    #[serde(default, deserialize_with = "flex::i64")]
    pub series_content_order: i64,
    #[serde(default)]
    pub language: String,
}

/// Novel rows, in either of the two shapes Pixiv serves them in.
///
/// "/ajax/search/novels/*" and "/ajax/novel/series_content/*" answer a list
/// ("novel.data", "thumbnails.novel"), while "/ajax/user/{id}/novels" answers a
/// map keyed by id whose *values* are the same rows. One type covers both so the
/// mapping layer never has to know which route answered.
///
/// A row that does not decode is dropped instead of failing the document: one
/// broken entry must not blank a whole page, and the rule for this domain is
/// that an id which does not fit is droppable.
#[derive(Debug, Clone, Default)]
pub struct NovelRowsWire(pub Vec<NovelItemWire>);

impl<'de> Deserialize<'de> for NovelRowsWire {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d).map_err(de::Error::custom)?;
        let rows = match value {
            serde_json::Value::Array(items) => items,
            serde_json::Value::Object(map) => map.into_values().collect(),
            // "no rows" arrives as [], {}, or null depending on the route.
            _ => Vec::new(),
        };
        Ok(Self(
            rows.into_iter()
                .filter_map(|row| serde_json::from_value::<NovelItemWire>(row).ok())
                .collect(),
        ))
    }
}

/// "/ajax/search/novels/{word}" body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelSearchBodyWire {
    #[serde(default)]
    pub novel: NovelSearchPageWire,
}

/// The result half of a novel search.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelSearchPageWire {
    #[serde(default)]
    pub data: NovelRowsWire,
    #[serde(default, deserialize_with = "flex::i64")]
    pub total: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub last_page: i64,
}

/// "/ajax/novel/series/{id}" body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelSeriesWire {
    #[serde(default)]
    pub id: Id,
    #[serde(default)]
    pub title: String,
    /// HTML-ish, like a novel description.
    #[serde(default)]
    pub caption: String,
    #[serde(default)]
    pub user_id: Id,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub pixiv_id: String,
    #[serde(default)]
    pub profile_image_url: String,
    /// Flat tag names here, unlike the detail document's nested groups.
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub cover: Option<NovelCoverWire>,
    #[serde(default, deserialize_with = "flex::i64")]
    pub published_content_count: i64,
    /// Mirrors "publishedContentCount"; kept because the page header is drawn
    /// from this one and the two have been seen to disagree.
    #[serde(default, deserialize_with = "flex::i64")]
    pub total: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub published_total_character_count: i64,
    #[serde(default, deserialize_with = "flex::i64")]
    pub published_reading_time: i64,
    #[serde(default)]
    pub first_novel_id: Id,
    #[serde(default)]
    pub latest_novel_id: Id,
    #[serde(default)]
    pub is_concluded: bool,
    #[serde(default, deserialize_with = "flex::i64")]
    pub x_restrict: i64,
}

/// A series cover: {"urls": {<pixel tiers>}}.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NovelCoverWire {
    #[serde(default)]
    pub urls: NovelImageUrlsWire,
}

/// "/ajax/novel/series_content/{id}" body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelSeriesContentWire {
    #[serde(default)]
    pub thumbnails: NovelThumbnailsWire,
}

/// The thumbnails block: one row list per content type, of which only "novel" is
/// ever populated for this route.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelThumbnailsWire {
    #[serde(default)]
    pub novel: NovelRowsWire,
}
