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
    #[serde(default)]
    pub illusts: HashMap<String, Option<serde_json::Value>>,
    #[serde(default)]
    pub manga: HashMap<String, Option<serde_json::Value>>,
    #[serde(default)]
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
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub image_big: String,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub total_follow_users: i64,
    #[serde(default)]
    pub total_my_pixiv_users: i64,
    #[serde(default)]
    pub is_followed: bool,
    #[serde(default)]
    pub premium: bool,
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
