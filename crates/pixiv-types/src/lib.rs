//! Domain and wire data contracts for the Pixiv Desktop client.
//!
//! This crate is deliberately free of HTTP, UI and storage concerns so that the
//! transport layer ([`pixiv_api`]), the UI and the persistence layer all speak
//! exactly the same vocabulary.
//!
//! Two layers live here:
//!
//! * [`wire`] — `serde` structs mirroring the JSON returned by `pixiv.net`
//!   verbatim, quirks included (ids arrive as strings on `ajax/*` endpoints but
//!   as numbers on `ranking.php`; illustration tags are nested one level deeper
//!   than search tags).
//! * [`domain`] — the normalised types the rest of the application uses.
//!
//! Keeping the quirks in one place is what stops them leaking into UI code.

pub mod domain;
pub mod error;
pub mod id;
pub mod wire;

pub use domain::{
    Account, Author, Bookmark, BookmarkRestrict, BookmarkState, DiscoveryBatch, FeedSource, Illust,
    IllustKind, IllustStats, IllustUrls, LatestFeed, PageMeta, PageUrls, RankingKind, RankingRow,
    RecommendTag, RecommendUser, SearchQuery, SearchResult, SearchTarget, SortOrder, StreetFeed,
    Suggestion, SuggestionSource, TypeFilter, UgoiraFrame, UgoiraMeta, UserProfile, append_unique,
    history_suggestions, merge_suggestions,
};
pub use error::PixivError;
pub use id::Id;

/// The `Referer` that `i.pximg.net` requires. Requests without it are refused
/// with a 403, so no image request may ever omit it.
pub const PIXIV_REFERER: &str = "https://www.pixiv.net/";

/// Host serving illustrations. Requires [`PIXIV_REFERER`].
pub const IMAGE_HOST: &str = "i.pximg.net";

/// The User-Agent every pixiv.net and i.pximg.net request carries.
///
/// A stock desktop browser string on purpose. Both hosts sit behind the same
/// bot-protection edge: a "PixivDesktop/..." UA is answered with a 403
/// Cloudflare interstitial before Pixiv sees the request, which shows up as
/// blank cards and a "session rejected" error on a perfectly good cookie.
/// Verified: one URL, one cookie, 200 with a browser UA and 403 without.
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
