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
