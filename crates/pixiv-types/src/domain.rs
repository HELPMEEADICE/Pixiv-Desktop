//! Normalised domain types — the vocabulary the UI and application layers use.
//!
//! Everything Pixiv-specific and irregular is resolved here exactly once:
//! string/number ids, nested tag groups, the three different "thumbnail" fields
//! across endpoints, and the int/uint narrowing the wire format forces.

use std::fmt;

use crate::Id;
use crate::wire;

pub type IllustId = Id;
pub type UserId = Id;

/// Illustration kind, from the wire `illustType` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IllustKind {
    Illustration,
    Manga,
    Ugoira,
}

impl IllustKind {
    /// `0` illustration, `1` manga, `2` ugoira.
    ///
    /// The ugoira mapping was confirmed live: illustrations returned by the
    /// `illustType == 2` filter are exactly the ones whose `ugoira_meta`
    /// endpoint answers.
    #[must_use]
    pub const fn from_wire(code: i32) -> Self {
        match code {
            1 => Self::Manga,
            2 => Self::Ugoira,
            _ => Self::Illustration,
        }
    }

    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Illustration => 0,
            Self::Manga => 1,
            Self::Ugoira => 2,
        }
    }

    /// Short label for badges in the UI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Illustration => "插画",
            Self::Manga => "漫画",
            Self::Ugoira => "动图",
        }
    }
}

/// A tag plus its optional machine translation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tag {
    pub name: String,
    pub translated: Option<String>,
}

impl Tag {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            translated: None,
        }
    }

    /// Text to render: the translation when present, otherwise the raw tag.
    #[must_use]
    pub fn display(&self) -> &str {
        self.translated.as_deref().unwrap_or(&self.name)
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display())
    }
}

/// An artist.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Author {
    pub id: UserId,
    pub name: String,
    /// Pixiv account handle without the `@`.
    pub account: String,
    pub avatar: Option<String>,
}

/// Image URLs for an illustration's cover.
///
/// Pixiv exposes five size tiers and different endpoints populate different
/// subsets, so every field may be empty and callers must go through the
/// selectors below rather than touching a field directly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IllustUrls {
    pub mini: String,
    pub thumb: String,
    pub small: String,
    pub regular: String,
    pub original: String,
}

impl IllustUrls {
    fn first_non_empty<const N: usize>(candidates: [&str; N]) -> &str {
        candidates
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or_default()
    }

    /// Best tier for a grid thumbnail: `regular` (master1200) is crisp at card
    /// widths while staying small enough to decode hundreds of them.
    #[must_use]
    pub fn grid(&self) -> &str {
        Self::first_non_empty([&self.regular, &self.small, &self.thumb, &self.mini])
    }

    /// Best tier for the detail viewer before the full-size page URLs load.
    #[must_use]
    pub fn viewer(&self) -> &str {
        Self::first_non_empty([&self.regular, &self.small, &self.original])
    }

    /// Largest known tier.
    #[must_use]
    pub fn largest(&self) -> &str {
        Self::first_non_empty([&self.original, &self.regular, &self.small, &self.thumb])
    }
}

impl From<wire::IllustUrlsWire> for IllustUrls {
    fn from(w: wire::IllustUrlsWire) -> Self {
        Self {
            mini: w.mini,
            thumb: w.thumb,
            small: w.small,
            regular: w.regular,
            original: w.original,
        }
    }
}

/// Bookmark visibility, which Pixiv calls `restrict`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BookmarkRestrict {
    #[default]
    Public,
    Private,
}

impl BookmarkRestrict {
    /// Value for the `rest` field of bookmark mutations.
    #[must_use]
    pub const fn as_param(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }

    /// Value for the `rest` query parameter of bookmark *listings*.
    #[must_use]
    pub const fn as_list_filter(self) -> &'static str {
        match self {
            Self::Public => "show",
            Self::Private => "hide",
        }
    }

    /// Value for the `restrict` field of bookmark *mutations*.
    ///
    /// The mutation endpoints take `0`/`1`, not the `public`/`private` words the
    /// listing endpoint uses — mixing the two is rejected by Pixiv with a `422`.
    #[must_use]
    pub const fn as_flag(self) -> u8 {
        match self {
            Self::Public => 0,
            Self::Private => 1,
        }
    }

    /// Inverse of [`Self::as_flag`].
    #[must_use]
    pub const fn from_flag(flag: u8) -> Self {
        if flag == 1 {
            Self::Private
        } else {
            Self::Public
        }
    }
}

/// Bookmark state as the server reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Bookmark {
    pub bookmarked: bool,
    pub restrict: BookmarkRestrict,
    pub count: u64,
}

/// What the server says about the signed-in account.
///
/// Obtained from `GET /ajax/user/self`, which is the only endpoint that both
/// proves the cookie is live *and* names the account behind it. A bare cookie
/// value is not evidence of a session: Pixiv answers `200` with
/// `"userData": null` when the cookie belongs to nobody, and `401` when it is
/// present but no longer valid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Account {
    pub id: UserId,
    /// Display name, e.g. `HELPMEEADICE`.
    pub name: String,
    /// Handle without the `@`, e.g. `helpmeeadice`.
    pub account: String,
    /// `170x170` avatar URL when the payload carried one.
    pub avatar: Option<String>,
    /// CSRF token that every state-changing request must echo back in
    /// `x-csrf-token`. Rotates with the session, so it is stored next to the
    /// cookie rather than hardcoded.
    pub csrf_token: Option<String>,
}

/// The viewer's own bookmark state for one illustration, as the server reports
/// it.
///
/// `id` is the *bookmark row* id, which is not the illustration id: deleting a
/// bookmark is addressed by this value, so a client that drops it cannot undo a
/// bookmark it just created.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BookmarkState {
    pub bookmarked: bool,
    pub restrict: BookmarkRestrict,
    pub id: Option<String>,
}

impl BookmarkState {
    /// State for "this illustration is not bookmarked".
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// State for "this illustration is bookmarked at `restrict`".
    #[must_use]
    pub fn bookmarked(restrict: BookmarkRestrict, id: Option<String>) -> Self {
        Self {
            bookmarked: true,
            restrict,
            id,
        }
    }
}

/// Aggregate engagement counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IllustStats {
    pub views: u64,
    pub bookmarks: u64,
    pub likes: u64,
    pub comments: u64,
}

/// A fully normalised illustration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Illust {
    pub id: IllustId,
    pub title: String,
    pub kind: IllustKind,
    pub page_count: u32,
    pub width: u32,
    pub height: u32,
    pub urls: IllustUrls,
    pub author: Author,
    pub tags: Vec<Tag>,
    pub description: String,
    pub create_date: Option<String>,
    pub bookmark: Bookmark,
    pub stats: IllustStats,
    /// `0` all-ages, `1` R-18, `2` R-18G.
    pub x_restrict: u8,
    /// Pixiv's sensitivity level (`sl`).
    pub sl: u8,
    pub is_masked: bool,
    pub is_bookmarkable: bool,
}

impl Illust {
    /// Width divided by height, guarding against malformed payloads.
    #[must_use]
    pub fn aspect_ratio(&self) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 1.0;
        }
        self.width as f32 / self.height as f32
    }

    /// True when the illustration is a multi-page work.
    #[must_use]
    pub fn is_multi_page(&self) -> bool {
        self.page_count > 1
    }

    /// Apply the server's answer to a bookmark mutation.
    pub fn apply_bookmark(&mut self, bookmarked: bool, restrict: BookmarkRestrict) {
        if self.bookmark.bookmarked != bookmarked {
            if bookmarked {
                self.bookmark.count = self.bookmark.count.saturating_add(1);
            } else {
                self.bookmark.count = self.bookmark.count.saturating_sub(1);
            }
        }
        // Moving between public and private keeps the total unchanged.
        self.bookmark.bookmarked = bookmarked;
        self.bookmark.restrict = restrict;
        self.stats.bookmarks = self.bookmark.count;
    }

    fn base(
        id: IllustId,
        title: String,
        kind: IllustKind,
        page_count: i32,
        width: i32,
        height: i32,
        tags: Vec<Tag>,
    ) -> Self {
        Self {
            id,
            title,
            kind,
            page_count: u32::try_from(page_count.max(1)).unwrap_or(1),
            width: u32::try_from(width.max(0)).unwrap_or(0),
            height: u32::try_from(height.max(0)).unwrap_or(0),
            urls: IllustUrls::default(),
            author: Author::default(),
            tags,
            description: String::new(),
            create_date: None,
            bookmark: Bookmark::default(),
            stats: IllustStats::default(),
            x_restrict: 0,
            sl: 0,
            is_masked: false,
            is_bookmarkable: true,
        }
    }

    fn bookmark_from(data: Option<wire::BookmarkDataWire>, count: i64) -> Bookmark {
        Bookmark {
            bookmarked: data.is_some(),
            restrict: match data {
                Some(d) if d.is_private => BookmarkRestrict::Private,
                _ => BookmarkRestrict::Public,
            },
            count: u64::try_from(count.max(0)).unwrap_or(0),
        }
    }

    /// Build from `/ajax/illust/{id}`.
    #[must_use]
    pub fn from_detail(w: wire::IllustDetailWire) -> Self {
        // Tags are nested per-author; flatten while keeping the first
        // translation seen for each tag name.
        let mut tags: Vec<Tag> = Vec::new();
        for group in w.tags {
            for t in group.tags {
                if t.tag.is_empty() {
                    continue;
                }
                let translated = t
                    .translation
                    .as_ref()
                    .and_then(|m| m.get("zh").or_else(|| m.get("en")).cloned());
                match tags.iter_mut().find(|existing| existing.name == t.tag) {
                    Some(existing) => {
                        if existing.translated.is_none() {
                            existing.translated = translated;
                        }
                    }
                    None => tags.push(Tag {
                        name: t.tag,
                        translated,
                    }),
                }
            }
        }

        let mut out = Self::base(
            w.illust_id,
            w.illust_title.clone(),
            IllustKind::from_wire(w.illust_type),
            w.page_count,
            w.width,
            w.height,
            tags,
        );
        out.description = if w.description.is_empty() {
            w.illust_comment
        } else {
            w.description
        };
        out.urls = w.urls.into();
        out.author = Author {
            id: w.user_id,
            name: w.user_name,
            account: w.user_account,
            avatar: None,
        };
        out.create_date = non_empty(w.create_date);
        out.bookmark = Self::bookmark_from(w.bookmark_data, w.bookmark_count);
        out.stats = IllustStats {
            views: u64::try_from(w.view_count.max(0)).unwrap_or(0),
            bookmarks: u64::try_from(w.bookmark_count.max(0)).unwrap_or(0),
            likes: u64::try_from(w.like_count.max(0)).unwrap_or(0),
            comments: u64::try_from(w.comment_count.max(0)).unwrap_or(0),
        };
        out.x_restrict = u8::try_from(w.x_restrict.max(0)).unwrap_or(0);
        out.sl = u8::try_from(w.sl.max(0)).unwrap_or(0);
        out.is_masked = w.is_masked;
        out.is_bookmarkable = w.is_bookmarkable;
        out
    }

    /// Build from a `/ajax/search/artworks` row.
    #[must_use]
    pub fn from_search(w: wire::SearchItemWire) -> Self {
        let tags = w.tags.into_iter().map(Tag::new).collect();
        let mut out = Self::base(
            w.id,
            w.title,
            IllustKind::from_wire(w.illust_type),
            w.page_count,
            w.width,
            w.height,
            tags,
        );
        // Search only provides a square crop; reuse it for every small tier so
        // `grid()` has something to show before the detail call refines it.
        out.urls = IllustUrls {
            mini: w.url.clone(),
            thumb: w.url.clone(),
            small: w.url.clone(),
            regular: w.url,
            original: String::new(),
        };
        out.description = w.description;
        out.author = Author {
            id: w.user_id,
            name: w.user_name,
            account: String::new(),
            avatar: non_empty(w.profile_image_url),
        };
        out.create_date = non_empty(w.create_date);
        out.bookmark = Self::bookmark_from(w.bookmark_data, 0);
        out.x_restrict = u8::try_from(w.x_restrict.max(0)).unwrap_or(0);
        out.sl = u8::try_from(w.sl.max(0)).unwrap_or(0);
        out.is_masked = w.is_masked;
        out.is_bookmarkable = w.is_bookmarkable;
        out
    }

    /// Build from a `ranking.php` row.
    #[must_use]
    pub fn from_ranking(w: wire::RankingItemWire) -> Self {
        let tags = w.tags.into_iter().map(Tag::new).collect();
        // Every numeric field on this endpoint arrives as a string, so it is
        // parsed into i64 by the wire layer and narrowed here.
        let narrow = |v: i64| i32::try_from(v).unwrap_or(0);
        let mut out = Self::base(
            w.illust_id,
            w.title,
            IllustKind::from_wire(narrow(w.illust_type)),
            narrow(w.illust_page_count),
            narrow(w.width),
            narrow(w.height),
            tags,
        );
        // Ranking exposes one pre-cropped thumbnail and no page URLs.
        out.urls = IllustUrls {
            mini: w.url.clone(),
            thumb: w.url.clone(),
            small: w.url.clone(),
            regular: w.url,
            original: String::new(),
        };
        out.author = Author {
            id: w.user_id,
            name: w.user_name,
            account: String::new(),
            avatar: non_empty(w.profile_img),
        };
        out.bookmark = Bookmark {
            bookmarked: false,
            restrict: BookmarkRestrict::Public,
            count: u64::try_from(w.rating_count.max(0)).unwrap_or(0),
        };
        out.stats = IllustStats {
            views: u64::try_from(w.view_count.max(0)).unwrap_or(0),
            bookmarks: u64::try_from(w.rating_count.max(0)).unwrap_or(0),
            likes: 0,
            comments: 0,
        };
        out.is_masked = w.is_masked;
        out
    }
}

fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}

/// One page of a multi-page illustration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageUrls {
    pub thumb_mini: String,
    pub small: String,
    pub regular: String,
    pub original: String,
}

impl PageUrls {
    /// Prefer the original resolution; fall back down the tiers.
    #[must_use]
    pub fn largest(&self) -> &str {
        for candidate in [&self.original, &self.regular, &self.small, &self.thumb_mini] {
            if !candidate.is_empty() {
                return candidate;
            }
        }
        ""
    }
}

impl From<wire::PageUrlsWire> for PageUrls {
    fn from(w: wire::PageUrlsWire) -> Self {
        Self {
            thumb_mini: w.thumb_mini,
            small: w.small,
            regular: w.regular,
            original: w.original,
        }
    }
}

/// A resolved illustration page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMeta {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub urls: PageUrls,
}

impl PageMeta {
    #[must_use]
    pub fn from_wire(index: u32, w: wire::IllustPageWire) -> Self {
        Self {
            index,
            width: u32::try_from(w.width.max(0)).unwrap_or(0),
            height: u32::try_from(w.height.max(0)).unwrap_or(0),
            urls: w.urls.into(),
        }
    }
}

/// One ugoira frame descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UgoiraFrame {
    pub file: String,
    /// How long this frame is displayed.
    pub delay_ms: u32,
}

/// `/ajax/illust/{id}/ugoira_meta` normalised.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UgoiraMeta {
    pub zip_url: String,
    pub original_zip_url: String,
    pub mime_type: String,
    pub frames: Vec<UgoiraFrame>,
}

impl UgoiraMeta {
    #[must_use]
    pub fn total_duration_ms(&self) -> u64 {
        self.frames.iter().map(|f| u64::from(f.delay_ms)).sum()
    }

    /// Prefer the full-resolution archive, falling back to the 600px one.
    #[must_use]
    pub fn best_zip(&self) -> &str {
        if self.original_zip_url.is_empty() {
            &self.zip_url
        } else {
            &self.original_zip_url
        }
    }
}

impl From<wire::UgoiraMetaWire> for UgoiraMeta {
    fn from(w: wire::UgoiraMetaWire) -> Self {
        Self {
            zip_url: w.src,
            original_zip_url: w.original_src,
            mime_type: w.mime_type,
            frames: w
                .frames
                .into_iter()
                .map(|f| UgoiraFrame {
                    file: f.file,
                    delay_ms: f.delay,
                })
                .collect(),
        }
    }
}

/// Ranking boards exposed by `ranking.php?mode=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RankingKind {
    Daily,
    Weekly,
    Monthly,
    Rookie,
    Original,
    Male,
    Female,
}

impl RankingKind {
    pub const ALL: [Self; 7] = [
        Self::Daily,
        Self::Weekly,
        Self::Monthly,
        Self::Rookie,
        Self::Original,
        Self::Male,
        Self::Female,
    ];

    /// The `mode` query value.
    #[must_use]
    pub const fn mode(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Rookie => "rookie",
            Self::Original => "original",
            Self::Male => "male",
            Self::Female => "female",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Daily => "日榜",
            Self::Weekly => "周榜",
            Self::Monthly => "月榜",
            Self::Rookie => "新人榜",
            Self::Original => "原创榜",
            Self::Male => "男性向",
            Self::Female => "女性向",
        }
    }

    #[must_use]
    pub fn from_mode(mode: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.mode() == mode)
    }
}

/// A ranked row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankingRow {
    pub rank: u32,
    pub illust: Illust,
}

/// Search ordering, using Pixiv's own `order` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    DateDesc,
    DateAsc,
    PopularDesc,
}

impl SortOrder {
    #[must_use]
    pub const fn as_param(self) -> &'static str {
        match self {
            Self::DateDesc => "date_d",
            Self::DateAsc => "date_a",
            Self::PopularDesc => "popular_d",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DateDesc => "最新",
            Self::DateAsc => "最早",
            Self::PopularDesc => "热门",
        }
    }
}

/// Which fields the search matches, i.e. Pixiv's `s_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchTarget {
    /// Full tag match (`s_tag_full`).
    #[default]
    TagFull,
    /// Partial tag match (`s_tag`).
    TagPartial,
    /// Title, caption and tag text (`s_tc`).
    TitleCaption,
}

impl SearchTarget {
    #[must_use]
    pub const fn as_param(self) -> &'static str {
        match self {
            Self::TagFull => "s_tag_full",
            Self::TagPartial => "s_tag",
            Self::TitleCaption => "s_tc",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::TagFull => "标签精确",
            Self::TagPartial => "标签部分",
            Self::TitleCaption => "标题/简介",
        }
    }
}

/// Work-type filter, i.e. Pixiv's `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TypeFilter {
    #[default]
    All,
    Illustration,
    Manga,
    Ugoira,
}

impl TypeFilter {
    #[must_use]
    pub const fn as_param(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Illustration => "illust",
            Self::Manga => "manga",
            Self::Ugoira => "ugoira",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Illustration => "插画",
            Self::Manga => "漫画",
            Self::Ugoira => "动图",
        }
    }
}

/// A search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub word: String,
    pub sort: SortOrder,
    pub target: SearchTarget,
    pub kind: TypeFilter,
    /// 1-based page number.
    pub page: u32,
}

impl SearchQuery {
    #[must_use]
    pub fn new(word: impl Into<String>) -> Self {
        Self {
            word: word.into(),
            sort: SortOrder::default(),
            target: SearchTarget::default(),
            kind: TypeFilter::default(),
            page: 1,
        }
    }

    #[must_use]
    pub fn with_page(mut self, page: u32) -> Self {
        self.page = page.max(1);
        self
    }
}

/// A page of search results.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchResult {
    pub items: Vec<Illust>,
    pub total: u64,
    /// Last available page; `0` when the server did not report one.
    pub last_page: u32,
}

impl SearchResult {
    #[must_use]
    pub fn from_wire(w: wire::SearchBodyWire) -> Self {
        Self {
            items: w
                .illust_manga
                .data
                .into_iter()
                .map(Illust::from_search)
                .collect(),
            total: u64::try_from(w.illust_manga.total.max(0)).unwrap_or(0),
            last_page: u32::try_from(w.illust_manga.last_page.max(0)).unwrap_or(0),
        }
    }
}

/// An artist profile.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserProfile {
    pub id: UserId,
    pub name: String,
    pub account: String,
    pub avatar: Option<String>,
    pub comment: String,
    pub followees: u64,
    pub followers: u64,
    /// Total illustrations, derived from the profile id map when available.
    pub illust_count: usize,
    pub is_followed: bool,
}

impl UserProfile {
    #[must_use]
    pub fn from_wire(w: wire::UserDetailWire, illust_count: usize) -> Self {
        Self {
            id: w.user_id,
            name: w.name,
            account: w.account,
            avatar: Some(if w.image_big.is_empty() {
                w.image
            } else {
                w.image_big
            }),
            comment: w.comment,
            followees: u64::try_from(w.total_follow_users.max(0)).unwrap_or(0),
            followers: u64::try_from(w.total_my_pixiv_users.max(0)).unwrap_or(0),
            illust_count,
            is_followed: w.is_followed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_display_prefers_translation() {
        let tag = Tag {
            name: "空".into(),
            translated: Some("sky".into()),
        };
        assert_eq!(tag.display(), "sky");
        assert_eq!(Tag::new("空").display(), "空");
    }

    #[test]
    fn url_selectors_skip_empty_tiers() {
        let urls = IllustUrls {
            mini: "m".into(),
            thumb: String::new(),
            small: "s".into(),
            regular: String::new(),
            original: "o".into(),
        };
        assert_eq!(urls.grid(), "s");
        assert_eq!(urls.viewer(), "s");
        assert_eq!(urls.largest(), "o");
    }

    #[test]
    fn aspect_ratio_guards_against_zero_height() {
        let mut i = Illust::base(
            Id(1),
            String::new(),
            IllustKind::Illustration,
            1,
            100,
            0,
            vec![],
        );
        assert!((i.aspect_ratio() - 1.0).abs() < f32::EPSILON);
        i.height = 100;
        assert!((i.aspect_ratio() - 1.0).abs() < f32::EPSILON);
        i.width = 200;
        assert!((i.aspect_ratio() - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn bookmark_toggle_keeps_count_consistent() {
        let mut i = Illust::base(
            Id(1),
            String::new(),
            IllustKind::Illustration,
            1,
            10,
            10,
            vec![],
        );
        i.bookmark.count = 5;
        i.apply_bookmark(true, BookmarkRestrict::Public);
        assert_eq!(i.bookmark.count, 6);
        // Public -> private must not change the total.
        i.apply_bookmark(true, BookmarkRestrict::Private);
        assert_eq!(i.bookmark.count, 6);
        assert_eq!(i.bookmark.restrict, BookmarkRestrict::Private);
        i.apply_bookmark(false, BookmarkRestrict::Public);
        assert_eq!(i.bookmark.count, 5);
        assert!(!i.bookmark.bookmarked);
    }

    #[test]
    fn illust_kind_maps_wire_codes() {
        assert_eq!(IllustKind::from_wire(0), IllustKind::Illustration);
        assert_eq!(IllustKind::from_wire(1), IllustKind::Manga);
        assert_eq!(IllustKind::from_wire(2), IllustKind::Ugoira);
        assert_eq!(IllustKind::Ugoira.code(), 2);
    }

    #[test]
    fn ranking_kind_round_trips_mode() {
        for kind in RankingKind::ALL {
            assert_eq!(RankingKind::from_mode(kind.mode()), Some(kind));
        }
        assert_eq!(RankingKind::from_mode("nope"), None);
    }
}
