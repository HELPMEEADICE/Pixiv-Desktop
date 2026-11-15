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
        // Tags are nested per-author; the flattening (first translation wins) is
        // the rule the novel detail document uses too, so it lives in one place.
        let tags = tags_from_groups(w.tags);

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

    /// Build from a street-feed section item (the real home page).
    ///
    /// This is the only row kind that carries the author's avatar, so it is the
    /// one that makes a real face possible on the home page. The cover tiers are
    /// keyed by pixel size here, not by tier name, which is why the generic
    /// mapping could never have worked for these payloads.
    #[must_use]
    pub fn from_street(w: wire::StreetThumbWire) -> Self {
        // The street payload sends tag objects with their own translations, so
        // the home cards can show localised tags without a second request.
        let tags = w
            .tags
            .into_iter()
            .filter(|tag| !tag.name.trim().is_empty())
            .map(|tag| {
                let translated = tag.translated_name.filter(|text| {
                    let text = text.trim();
                    !text.is_empty() && text != tag.name.trim()
                });
                Tag {
                    name: tag.name,
                    translated,
                }
            })
            .collect();
        let first = w.pages.first();
        let (width, height) = first.map_or((0, 0), |page| (page.width, page.height));
        let id = w.id.trim().parse::<u64>().map(Id).unwrap_or(Id(0));
        let mut out = Self::base(
            id,
            w.title,
            IllustKind::from_wire(match w.kind.as_str() {
                "manga" => 1,
                "ugoira" => 2,
                _ => 0,
            }),
            w.page_count,
            width,
            height,
            tags,
        );
        // "1200x1200_standard" is the master1200 tier, which is what a grid card
        // wants; the 540 and 360 crops are the fallbacks.
        let regular = first.map_or(String::new(), |p| {
            p.pick(&["1200x1200_standard", "1200x1200", "540x540"])
        });
        let small = first.map_or(String::new(), |p| p.pick(&["540x540", "360x360"]));
        let thumb = first.map_or(String::new(), |p| p.pick(&["360x360", "540x540"]));
        out.urls = IllustUrls {
            mini: thumb.clone(),
            thumb,
            small,
            regular,
            original: String::new(),
        };
        out.description = w.description;
        out.author = Author {
            id: w.user_id,
            name: w.user_name,
            account: w.pixiv_id,
            avatar: non_empty(w.profile_image_url),
        };
        out.create_date = non_empty(w.create_date);
        out.x_restrict = u8::try_from(w.x_restrict.max(0)).unwrap_or(0);
        out.sl = u8::try_from(w.sl.max(0)).unwrap_or(0);
        out.is_bookmarkable = w.bookmarkable;
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
///
/// Every field comes from /ajax/user/{id}?full=1 as the live payload actually
/// names it. The previous mapping read totalFollowUsers and totalMyPixivUsers,
/// neither of which the endpoint sends: the real keys are following (how many
/// accounts this user follows) and mypixivCount, so the profile card reported
/// zero followees for everybody.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserProfile {
    pub id: UserId,
    pub name: String,
    /// Handle without the @, from the payload's pixivId.
    pub account: String,
    /// 170x170 avatar; falls back to the 50x50 one.
    pub avatar: Option<String>,
    pub comment: String,
    /// How many accounts this user follows.
    pub followees: u64,
    /// How many accounts follow this user. Only the signed-in account may read
    /// its own follower list, so this is None for everybody else.
    pub followers: Option<u64>,
    /// Total illustrations, derived from the profile id map when available.
    pub illust_count: usize,
    /// Manga count, from the same id map.
    pub manga_count: usize,
    pub is_followed: bool,
    /// Profile cover image, when the user set one.
    pub background: Option<String>,
    pub is_premium: bool,
    /// Personal website, when the user published one.
    pub webpage: Option<String>,
}

impl UserProfile {
    #[must_use]
    pub fn from_wire(w: wire::UserDetailWire, illust_count: usize, manga_count: usize) -> Self {
        Self {
            id: w.user_id,
            name: w.name,
            account: w.pixiv_id,
            avatar: [w.image_big, w.image]
                .into_iter()
                .find(|url| !url.trim().is_empty()),
            comment: w.comment,
            followees: u64::try_from(w.following.max(0)).unwrap_or(0),
            followers: None,
            illust_count,
            manga_count,
            is_followed: w.is_followed,
            background: w
                .background
                .filter(|value| value.as_str().is_some_and(|url| !url.trim().is_empty()))
                .and_then(|value| value.as_str().map(str::to_string)),
            is_premium: w.premium,
            webpage: w.webpage.filter(|url| !url.trim().is_empty()),
        }
    }

    /// Attach the follower count, which only the signed-in account can read.
    #[must_use]
    pub fn with_followers(mut self, followers: Option<u64>) -> Self {
        self.followers = followers;
        self
    }
}

// ===========================================================================
// Home feed, discovery and search suggestions
// ===========================================================================

/// Why a section exists, so the UI can label it honestly.
///
/// The home page used to render the daily ranking under the heading
/// "为你推荐"; that is a lie a badge cannot fix. Every feed now carries the
/// source it actually came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeedSource {
    /// /ajax/street/for_you -- personalised by Pixiv for the signed-in account.
    #[default]
    ForYou,
    /// /ajax/street/latest -- the newest works, not personalised.
    Latest,
    /// /ajax/illust/discovery -- the recommendation engine's own batch feed.
    Discovery,
    /// /ajax/discovery/artworks -- personalised, hydrated from ids.
    DiscoveryPersonalised,
    /// /ranking.php -- the official ranking, kept separate on purpose.
    Ranking,
}

impl FeedSource {
    /// Short label for a section header.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ForYou => "为你推荐",
            Self::Latest => "最新作品",
            Self::Discovery => "发现",
            Self::DiscoveryPersonalised => "为你推荐的作品",
            Self::Ranking => "排行榜",
        }
    }

    /// One line explaining where the works came from.
    #[must_use]
    pub const fn note(self) -> &'static str {
        match self {
            Self::ForYou => "Pixiv 按你的浏览与收藏生成",
            Self::Latest => "Pixiv 全站最新投稿，与推荐无关",
            Self::Discovery => "Pixiv 推荐引擎返回的批次，每次刷新都不同",
            Self::DiscoveryPersonalised => "Pixiv 推荐引擎按你的收藏生成",
            Self::Ranking => "Pixiv 官方榜单",
        }
    }
}

/// A tag Pixiv recommends to this account.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecommendTag {
    pub name: String,
    /// Localised name, when Pixiv has one.
    pub translated: Option<String>,
    /// Highlighted by Pixiv (usually because it is trending right now).
    pub emphasized: bool,
}

impl RecommendTag {
    /// Text to render.
    #[must_use]
    pub fn display(&self) -> &str {
        self.translated.as_deref().unwrap_or(&self.name)
    }
}

/// A user Pixiv recommends, with enough detail to render a card.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecommendUser {
    pub id: UserId,
    pub name: String,
    pub account: String,
    pub avatar: Option<String>,
    /// Profile cover image, when the payload carried one.
    pub background: Option<String>,
    pub comment: String,
    pub is_followed: bool,
    pub is_premium: bool,
    /// A few recent works, used as the card preview.
    pub previews: Vec<Illust>,
}

impl From<wire::DiscoveryUserWire> for RecommendUser {
    fn from(w: wire::DiscoveryUserWire) -> Self {
        // Two payload families fill in different names for the same identity,
        // so each field takes whichever one arrived.
        let name = [w.name, w.user_name]
            .into_iter()
            .find(|s| !s.trim().is_empty())
            .unwrap_or_default();
        let comment = [w.comment, w.user_comment]
            .into_iter()
            .find(|s| !s.trim().is_empty())
            .unwrap_or_default();
        let avatar = [w.image_big, w.image, w.profile_image_url]
            .into_iter()
            .find(|s| !s.trim().is_empty());
        let background = [w.profile_background_image_url]
            .into_iter()
            .find(|s| !s.trim().is_empty())
            .or_else(|| {
                w.background
                    .as_ref()
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            });
        Self {
            id: w.user_id,
            name,
            account: w.pixiv_id,
            avatar,
            background,
            comment,
            is_followed: w.is_followed,
            is_premium: w.premium,
            previews: w
                .thumbnails
                .into_iter()
                .map(Illust::from_street)
                .chain(w.illusts.into_iter().map(Illust::from_search))
                .collect(),
        }
    }
}

/// The home page, as Pixiv answered for it.
///
/// One struct per source, because each section loads independently: a failing
/// recommendation endpoint must not blank the newest-works row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreetFeed {
    /// Works from /ajax/street/for_you, in payload order.
    pub works: Vec<Illust>,
    /// Tags from /ajax/street/recommend_tags.
    pub tags: Vec<RecommendTag>,
    /// Artists from /ajax/street/sub.
    pub artists: Vec<RecommendUser>,
}

/// The newest-works row from /ajax/street/latest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LatestFeed {
    pub title: String,
    pub works: Vec<Illust>,
}

/// One batch of recommendations from the discovery engine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryBatch {
    /// Works with complete metadata, ready to render.
    pub works: Vec<Illust>,
    /// Ids the engine recommended but did not describe, in ranked order.
    pub pending: Vec<IllustId>,
    /// Artists that came with the batch.
    pub users: Vec<RecommendUser>,
    /// Tags the engine used, for the "because you like X" line.
    pub tags: Vec<RecommendTag>,
}

/// Where a suggestion came from. The distinction is shown in the UI: local
/// history is never presented as a Pixiv suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SuggestionSource {
    /// /rpc/cps.php -- the document the official search box requests.
    #[default]
    Pixiv,
    /// A local search this user performed before.
    LocalHistory,
}

/// One row of the search-suggestion panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Suggestion {
    /// Text inserted into the search box when chosen.
    pub tag: String,
    /// Localised form, when Pixiv sent one.
    pub translated: Option<String>,
    /// Works carrying this tag, when the endpoint reported a count.
    pub illust_count: Option<u64>,
    /// Relative popularity (access count), used only for ordering.
    pub popularity: Option<u64>,
    pub source: SuggestionSource,
}

impl Suggestion {
    /// Text to render as the primary line.
    #[must_use]
    pub fn display(&self) -> &str {
        self.translated.as_deref().unwrap_or(&self.tag)
    }
}

/// Merge the two suggestion endpoints into one ordered list.
///
/// /rpc/cps.php is the list the site's own search box shows and it is the only
/// source of translations, but it reports no work counts. /ajax/tags/suggest_by_word
/// reports counts but is prefix-only. Consulting both and joining on the tag name
/// gives the official ordering with real counts attached, instead of picking one
/// and losing a visible piece of information.
#[must_use]
pub fn merge_suggestions(
    cps: Vec<wire::CpsCandidateWire>,
    counts: Vec<wire::SuggestCandidateWire>,
    limit: usize,
) -> Vec<Suggestion> {
    fn number(text: &str) -> Option<u64> {
        text.trim().parse::<u64>().ok()
    }

    let mut out: Vec<Suggestion> = Vec::new();
    for candidate in cps {
        let tag = candidate.tag_name.trim();
        if tag.is_empty() {
            continue;
        }
        if out.iter().any(|existing| existing.tag == tag) {
            continue;
        }
        out.push(Suggestion {
            tag: tag.to_string(),
            translated: candidate
                .tag_translation
                .filter(|text| !text.trim().is_empty() && text.trim() != tag),
            illust_count: None,
            popularity: number(&candidate.access_count),
            source: SuggestionSource::Pixiv,
        });
    }
    for candidate in counts {
        let tag = candidate.tag_name.trim();
        if tag.is_empty() {
            continue;
        }
        let count = u64::try_from(candidate.illust_count.max(0)).unwrap_or(0);
        match out.iter_mut().find(|existing| existing.tag == tag) {
            Some(existing) => existing.illust_count = Some(count),
            None => out.push(Suggestion {
                tag: tag.to_string(),
                translated: None,
                illust_count: Some(count),
                popularity: None,
                source: SuggestionSource::Pixiv,
            }),
        }
    }
    out.truncate(limit);
    out
}

/// Local search history rendered as suggestion rows, clearly marked as local.
#[must_use]
pub fn history_suggestions(words: &[String], limit: usize) -> Vec<Suggestion> {
    words
        .iter()
        .filter(|word| !word.trim().is_empty())
        .take(limit)
        .map(|word| Suggestion {
            tag: word.clone(),
            translated: None,
            illust_count: None,
            popularity: None,
            source: SuggestionSource::LocalHistory,
        })
        .collect()
}

/// Append a batch of works, dropping ids already present.
///
/// The discovery engine re-recommends what it has already shown, so appending
/// blindly grows the list with duplicates and never reaches an end condition.
/// Returns how many rows were actually added.
pub fn append_unique(into: &mut Vec<Illust>, incoming: impl IntoIterator<Item = Illust>) -> usize {
    let mut added = 0;
    for illust in incoming {
        if illust.id.is_zero() || into.iter().any(|existing| existing.id == illust.id) {
            continue;
        }
        into.push(illust);
        added += 1;
    }
    added
}

// ---------------------------------------------------------------------------
// Novels
// ---------------------------------------------------------------------------
//
// A novel is a different content type, not a variant of an illustration: it has a
// body, no image pages, a position inside an optional series, and its own
// counters. It is modelled in full so the reader, the search grid and the series
// page all speak one vocabulary instead of counting fields per endpoint.
//
// The invariants every type in this section obeys:
//
// * a missing string is "", a missing number is 0;
// * a negative number is clamped to 0, and a number too large for the field
//   saturates rather than wrapping or zeroing;
// * a row whose id does not parse is dropped by the mapping layer, never kept as
//   id 0 -- an id-0 novel is indistinguishable from "nothing here".

/// An illustration embedded in a novel body, from "textEmbeddedImages".
///
/// The body references these by the id in "[uploadedimage:<id>]", so the id is
/// kept as the string Pixiv sent rather than narrowed to a number.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NovelImage {
    pub id: String,
    /// Reading-width tier (480mw, falling back inward then outward).
    pub small: String,
    /// Full-width tier (1200x1200, falling back to the original).
    pub large: String,
}

impl NovelImage {
    fn from_wire(key: String, w: wire::NovelImageWire) -> Self {
        // The map key and the inner id are the same value on every capture; the
        // key is the fallback so an image without its own id is still addressable.
        let id = if w.novel_image_id.trim().is_empty() {
            key
        } else {
            w.novel_image_id
        };
        Self {
            id,
            small: w.urls.small().to_string(),
            large: w.urls.large().to_string(),
        }
    }
}

/// Where a chapter sits inside its series ("seriesNavData").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NovelSeriesNav {
    pub id: Id,
    pub title: String,
    /// 1-based position of this chapter inside the series.
    pub order: u32,
    /// Previous chapter, when it exists and the reader may open it.
    pub prev: Option<Id>,
    /// Next chapter, under the same rule.
    pub next: Option<Id>,
    /// Neighbour titles, so the reader footer can read "上一章 ...".
    pub prev_title: String,
    pub next_title: String,
    /// True when the author marked the series finished.
    pub concluded: bool,
}

impl NovelSeriesNav {
    /// Build from "seriesNavData", or None when this work is standalone.
    ///
    /// An explicit null and an object carrying a zero series id mean the same
    /// thing: not in a series. A neighbour whose "available" flag is false (a
    /// private or deleted chapter) is treated as absent, because this type has no
    /// field to carry the flag and offering the link would open an error page.
    #[must_use]
    fn from_wire(w: wire::NovelSeriesNavWire) -> Option<Self> {
        if w.series_id.is_zero() {
            return None;
        }
        let (prev, prev_title) = neighbour(w.prev);
        let (next, next_title) = neighbour(w.next);
        Some(Self {
            id: w.series_id,
            title: w.title,
            order: count32(w.order),
            prev,
            next,
            prev_title,
            next_title,
            concluded: w.is_concluded,
        })
    }
}

/// One neighbour: the id when it may be opened, plus its title either way.
fn neighbour(w: Option<wire::NovelSeriesLinkWire>) -> (Option<Id>, String) {
    match w {
        Some(link) if link.available != Some(false) && !link.id.is_zero() => {
            (Some(link.id), link.title)
        }
        _ => (None, String::new()),
    }
}

/// A complete novel, body included.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Novel {
    pub id: Id,
    pub title: String,
    pub author: Author,
    /// Cover URL at the master tier; absent when the work has no cover.
    pub cover: Option<String>,
    /// The description reduced to plain text by clean_caption.
    pub caption: String,
    pub tags: Vec<Tag>,
    /// The body, exactly as Pixiv sent it.
    ///
    /// Kept raw on purpose: ruby ("[[rb:base > reading]]"), embedded images
    /// ("[uploadedimage:<id>]") and jump links ("[[jumpuri:..]]") are rendered by
    /// the reader, and rewriting them here would destroy information the reader
    /// could not recover.
    pub body: String,
    /// Embedded images, sorted by id.
    ///
    /// The wire map has no order of its own and the body is the only authority on
    /// where an image appears; sorting at least makes this vector deterministic.
    pub images: Vec<NovelImage>,
    pub character_count: u32,
    pub word_count: u32,
    pub reading_time: u32,
    pub page_count: u32,
    pub views: u32,
    pub bookmarks: u32,
    pub likes: u32,
    /// "YYYY-MM-DD" when the timestamp parsed, otherwise the raw string.
    pub upload_date: String,
    pub is_original: bool,
    /// 0 all-ages, 1 R-18, 2 R-18G.
    pub x_restrict: u8,
    pub language: String,
    pub series: Option<NovelSeriesNav>,
    pub bookmarked: bool,
}

impl Novel {
    /// Build from "/ajax/novel/{id}" -- the whole document, body included.
    ///
    /// The detail payload carries no author handle and no avatar (userName and
    /// userId only), so those stay empty until a profile call fills them in;
    /// inventing them here would mean guessing.
    #[must_use]
    pub fn from_detail(w: wire::NovelDetailWire) -> Self {
        let mut images: Vec<NovelImage> = w
            .text_embedded_images
            .into_iter()
            .map(|(key, image)| NovelImage::from_wire(key, image))
            .filter(|image| !image.id.trim().is_empty())
            .collect();
        images.sort_by_key(|image| image.id.parse::<u64>().unwrap_or(u64::MAX));
        Self {
            id: w.id,
            title: w.title,
            author: Author {
                id: w.user_id,
                name: w.user_name,
                account: String::new(),
                avatar: None,
            },
            cover: non_empty(w.cover_url),
            caption: clean_caption(&w.description),
            tags: tags_from_groups(w.tags),
            body: w.content,
            images,
            character_count: count32(w.character_count),
            word_count: count32(w.word_count),
            reading_time: count32(w.reading_time),
            page_count: count32(w.page_count),
            views: count32(w.view_count),
            bookmarks: count32(w.bookmark_count),
            likes: count32(w.like_count),
            upload_date: date_part(&w.create_date, &w.upload_date),
            is_original: w.is_original,
            x_restrict: restrict8(w.x_restrict),
            language: w.language,
            series: w.series_nav_data.and_then(NovelSeriesNav::from_wire),
            bookmarked: w.bookmark_data.is_some(),
        }
    }
}

/// A novel as it appears in a list: search results, a series page, a profile.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NovelSummary {
    pub id: Id,
    pub title: String,
    pub author: Author,
    pub cover: Option<String>,
    pub tags: Vec<Tag>,
    pub caption: String,
    /// Characters, from the row's "textCount".
    pub character_count: u32,
    pub reading_time: u32,
    /// Absent for a standalone work.
    pub series_id: Option<Id>,
    pub series_title: String,
    /// This chapter's position inside its series; 0 when standalone.
    pub series_order: u32,
    pub upload_date: String,
    pub bookmarked: bool,
    pub x_restrict: u8,
}

impl NovelSummary {
    /// Build from any novel row: a search hit, a series chapter, a user's work.
    #[must_use]
    pub fn from_row(w: wire::NovelItemWire) -> Self {
        Self {
            id: w.id,
            title: w.title,
            author: Author {
                id: w.user_id,
                name: w.user_name,
                account: w.pixiv_id,
                avatar: non_empty(w.profile_image_url),
            },
            cover: non_empty(w.url),
            tags: clean_tags(w.tags),
            caption: clean_caption(&w.description),
            character_count: count32(w.text_count),
            reading_time: count32(w.reading_time),
            series_id: w.series_id.filter(|id| !id.is_zero()),
            series_title: w.series_title,
            series_order: count32(w.series_content_order),
            upload_date: date_part(&w.create_date, ""),
            bookmarked: w.bookmark_data.is_some(),
            x_restrict: restrict8(w.x_restrict),
        }
    }
}

/// One page of novel search results.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NovelSearch {
    pub novels: Vec<NovelSummary>,
    pub total: u32,
    pub last_page: u32,
}

impl NovelSearch {
    /// True when the page carried no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.novels.is_empty()
    }
}

/// A novel series ("/ajax/novel/series/{id}").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NovelSeries {
    pub id: Id,
    pub title: String,
    /// The series blurb, reduced to plain text.
    pub caption: String,
    pub author: Author,
    pub cover: Option<String>,
    pub tags: Vec<Tag>,
    /// Published chapters.
    pub total: u32,
    pub total_characters: u32,
    pub total_reading_time: u32,
    pub first_novel: Option<Id>,
    pub latest_novel: Option<Id>,
    pub concluded: bool,
}

impl NovelSeries {
    /// Build from "/ajax/novel/series/{id}".
    #[must_use]
    pub fn from_wire(w: wire::NovelSeriesWire) -> Self {
        // "publishedContentCount" is the count the chapter list is built from;
        // "total" mirrors it but is the one the header renders, so it is the
        // fallback rather than the other way round.
        let total = if w.published_content_count > 0 {
            w.published_content_count
        } else {
            w.total
        };
        Self {
            id: w.id,
            title: w.title,
            caption: clean_caption(&w.caption),
            author: Author {
                id: w.user_id,
                name: w.user_name,
                account: w.pixiv_id,
                avatar: non_empty(w.profile_image_url),
            },
            cover: w
                .cover
                .map(|cover| cover.urls.cover().to_string())
                .filter(|url| !url.trim().is_empty()),
            tags: clean_tags(w.tags),
            total: count32(total),
            total_characters: count32(w.published_total_character_count),
            total_reading_time: count32(w.published_reading_time),
            first_novel: Some(w.first_novel_id).filter(|id| !id.is_zero()),
            latest_novel: Some(w.latest_novel_id).filter(|id| !id.is_zero()),
            concluded: w.is_concluded,
        }
    }
}

/// One chapter in a series listing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeriesChapter {
    pub id: Id,
    pub title: String,
    /// 1-based position inside the series.
    pub order: u32,
    pub cover: Option<String>,
    pub character_count: u32,
    pub reading_time: u32,
}

impl SeriesChapter {
    /// Build from a "/ajax/novel/series_content/{id}" row.
    #[must_use]
    pub fn from_row(w: wire::NovelItemWire) -> Self {
        Self {
            id: w.id,
            title: w.title,
            order: count32(w.series_content_order),
            cover: non_empty(w.url),
            character_count: count32(w.text_count),
            reading_time: count32(w.reading_time),
        }
    }
}

/// Flatten Pixiv's per-author tag groups into one list, first translation wins.
///
/// Both the illustration and the novel detail documents nest tags one level
/// deeper than the list payloads; the flattening happens here so no caller has to
/// know about it.
fn tags_from_groups(groups: Vec<wire::TagGroupWire>) -> Vec<Tag> {
    let mut tags: Vec<Tag> = Vec::new();
    for group in groups {
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
    tags
}

/// Flat tag names into Tag values, dropping blanks.
fn clean_tags(names: Vec<String>) -> Vec<Tag> {
    names
        .into_iter()
        .filter(|name| !name.trim().is_empty())
        .map(Tag::new)
        .collect()
}

/// Narrow a wire counter to the u32 the UI renders, saturating.
///
/// Saturating rather than wrapping or zeroing: a count that does not fit is still
/// "a very large number", and reporting 0 would quietly say "no views".
fn count32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

/// "xRestrict" is 0 all-ages, 1 R-18, 2 R-18G; anything else clamps.
fn restrict8(value: i64) -> u8 {
    u8::try_from(value.clamp(0, 2)).unwrap_or(0)
}

/// The date half of a Pixiv timestamp, or the raw text when it does not parse.
///
/// Kept as a string rather than a date type: the value is displayed and never
/// compared, and the same field arrives as "2026-11-15T15:52:25+00:00", as
/// "2026-01-02 03:04:05" and occasionally as an object. Anything unrecognised is
/// passed through verbatim so a new format shows up as text instead of vanishing.
fn date_part(primary: &str, fallback: &str) -> String {
    let raw = if primary.trim().is_empty() {
        fallback
    } else {
        primary
    };
    let raw = raw.trim();
    let bytes = raw.as_bytes();
    let dated = bytes.len() >= 10
        && bytes.iter().take(10).enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    if dated {
        raw[..10].to_string()
    } else {
        raw.to_string()
    }
}

/// Reduce Pixiv's HTML-ish blurb markup to plain text.
///
/// A novel description is not HTML in the sense a browser parses: it is author
/// text with "<br />" line breaks and "<a href>" links sprinkled in. Rendering it
/// raw put the tags on screen; stripping every tag destroyed the link targets. So
/// both halves are handled explicitly:
///
/// * any spelling of a break ("<br>", "<br/>", "<br />") becomes a newline --
///   Pixiv writes "<br />" but the markup is not validated;
/// * an anchor becomes "Y (X)" for "<a href="X">Y</a>", keeping the URL the tag
///   would otherwise hide -- a caption's links are often the only source a work
///   cites;
/// * a tag without content ("<strong>", "<span style=..>") is dropped, keeping
///   the text it wraps;
/// * "&amp;", "&lt;", "&gt;", "&quot;", "&#39;", "&nbsp;" and numeric escapes are
///   decoded last, so an escaped "&lt;br&gt;" stays visible text instead of
///   turning into a line break;
/// * "&nbsp;" decodes to a normal space: it is a visual separator in Pixiv
///   captions, and a non-breaking space in a wrap-sensitive label would refuse to
///   break there.
#[must_use]
pub fn clean_caption(html: &str) -> String {
    decode_entities(&strip_markup(html)).trim().to_string()
}

/// Drop tags and rewrite links/breaks, without touching entities yet.
fn strip_markup(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        if let Some(len) = break_tag_len(tail) {
            out.push('\n');
            rest = &tail[len..];
            continue;
        }
        if let Some((len, href, inner)) = anchor_at(tail) {
            let text = strip_markup(inner);
            out.push_str(&text);
            if !repeats_the_target(&text, &href) {
                out.push_str(" (");
                out.push_str(href.trim());
                out.push(')');
            }
            rest = &tail[len..];
            continue;
        }
        match tail.find('>') {
            // An unterminated "<" is text, not markup: keep the remainder.
            Some(end) => rest = &tail[end + 1..],
            None => {
                out.push_str(tail);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Length of a break tag at the head of tail, if there is one.
fn break_tag_len(tail: &str) -> Option<usize> {
    if !tail.get(..3)?.eq_ignore_ascii_case("<br") {
        return None;
    }
    let end = tail.find('>')?;
    // "<br />" is the longest spelling Pixiv writes; anything longer is not a
    // break tag.
    if end > 6 {
        return None;
    }
    if tail[3..end].chars().all(|c| c == '/' || c.is_whitespace()) {
        Some(end + 1)
    } else {
        None
    }
}

/// Parse an anchor at the head of tail into (bytes consumed, href, inner text).
fn anchor_at(tail: &str) -> Option<(usize, String, &str)> {
    if !tail.get(..2)?.eq_ignore_ascii_case("<a") {
        return None;
    }
    let open_end = tail.find('>')?;
    let tag = &tail[..open_end];
    // "<abbr>" and "<area>" also start with "<a"; a real anchor is followed by a
    // space, a slash or the closing bracket.
    let after_name = tag.get(2..)?;
    if !after_name.is_empty() && !after_name.starts_with([' ', '/', '>']) {
        return None;
    }
    let after = tail.get(open_end + 1..)?;
    let close = after.to_ascii_lowercase().find("</a>")?;
    Some((open_end + 1 + close + 4, href_of(tag), &after[..close]))
}

/// True when appending the target would only repeat what the text already says.
///
/// Pixiv writes two kinds of link. A citation like "<a href="..">novel/1</a>"
/// needs its target printed, because the text alone does not say where it goes.
/// The other kind is a bare URL wrapped in "/jump.php?<percent-encoded target>",
/// where the visible text *is* the destination: appending the wrapper prints the
/// same address twice, once as an unreadable blob.
fn repeats_the_target(text: &str, href: &str) -> bool {
    let text = text.trim();
    let href = href.trim();
    if text.is_empty() || href.is_empty() || href == text {
        return true;
    }
    // The text is already a URL: the reader can see where this points.
    if text.starts_with("http://") || text.starts_with("https://") {
        return true;
    }
    // Pixiv's outbound wrapper, whose payload is percent-encoded noise.
    href.starts_with("/jump.php")
}

/// The "href" value of an opening tag, quoted or unquoted.
fn href_of(tag: &str) -> String {
    let lower = tag.to_ascii_lowercase();
    let Some(at) = lower.find("href") else {
        return String::new();
    };
    let rest = tag[at + 4..].trim_start();
    let Some(rest) = rest.strip_prefix('=') else {
        return String::new();
    };
    let rest = rest.trim_start();
    let mut chars = rest.chars();
    match chars.next() {
        Some(quote @ ('"' | '\'')) => {
            let body = &rest[1..];
            body.find(quote)
                .map(|end| body[..end].to_string())
                .unwrap_or_default()
        }
        Some(_) => rest
            .split(|c: char| c.is_whitespace() || c == '>')
            .next()
            .unwrap_or_default()
            .to_string(),
        None => String::new(),
    }
}

/// Decode the entities Pixiv's blurbs actually use.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        match entity_at(tail) {
            Some((len, replacement)) => {
                out.push_str(&replacement);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// One entity at the head of tail: (bytes consumed, replacement).
fn entity_at(tail: &str) -> Option<(usize, String)> {
    const NAMED: [(&str, &str); 6] = [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
    ];
    for (pattern, replacement) in NAMED {
        if tail.starts_with(pattern) {
            return Some((pattern.len(), (*replacement).to_string()));
        }
    }
    // Numeric forms: &#39; and &#x27;.
    let body = tail.strip_prefix("&#")?;
    let semi = body.find(';')?;
    if semi > 8 {
        return None;
    }
    let digits = &body[..semi];
    let code = match digits.strip_prefix('x').or_else(|| digits.strip_prefix('X')) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    Some((2 + semi + 1, char::from_u32(code)?.to_string()))
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
    // -- novels -------------------------------------------------------------

    #[test]
    fn caption_breaks_links_and_entities_become_plain_text() {
        // Every spelling of a break Pixiv writes.
        assert_eq!(clean_caption("a<br />b"), "a\nb");
        assert_eq!(clean_caption("a<br>b"), "a\nb");
        assert_eq!(clean_caption("a<br/>b"), "a\nb");
        assert_eq!(
            clean_caption(r#"<a href="https://x.test/p">link</a>"#),
            "link (https://x.test/p)"
        );
        assert_eq!(clean_caption("&amp;&lt;&gt;&quot;&#39;"), "&<>\"'");
        // A non-breaking space becomes a normal one, and the result is trimmed.
        assert_eq!(clean_caption("a&nbsp;b"), "a b");
        assert_eq!(clean_caption("<strong>bold</strong>"), "bold");
        // Entities decode last, so an escaped tag stays visible text.
        assert_eq!(clean_caption("a &lt;br&gt; b"), "a <br> b");
        // An unterminated "<" is text, not markup.
        assert_eq!(clean_caption("1 < 2"), "1 < 2");
        assert_eq!(clean_caption(""), "");
    }

    #[test]
    fn caption_keeps_a_link_target_that_nested_markup_wrapped() {
        let html = r#"[[jumpuri:x &gt;<strong><a href="https://www.pixiv.net/novel/show.php?id=27815393">novel/27815393</a></strong>]]"#;
        assert_eq!(
            clean_caption(html),
            "[[jumpuri:x >novel/27815393 (https://www.pixiv.net/novel/show.php?id=27815393)]]"
        );
    }

    #[test]
    fn a_link_whose_text_is_the_target_is_not_printed_twice() {
        let html = r#"x <a href="/jump.php?https%3A%2F%2Fexample.test%2Fp" target="_blank">https://example.test/p</a> y"#;
        assert_eq!(clean_caption(html), "x https://example.test/p y");
        // A citation keeps its target, because the text does not say where it goes.
        assert_eq!(
            clean_caption(r#"<a href="/novel/show.php?id=1">第一章</a>"#),
            "第一章 (/novel/show.php?id=1)"
        );
    }

    #[test]
    fn an_empty_detail_document_yields_a_zeroed_novel() {
        let novel = Novel::from_detail(crate::wire::NovelDetailWire::default());
        assert!(novel.id.is_zero());
        assert!(novel.body.is_empty());
        assert!(novel.caption.is_empty());
        assert_eq!(novel.character_count, 0);
        assert_eq!(novel.x_restrict, 0);
        assert!(novel.cover.is_none());
        assert!(novel.series.is_none());
        assert!(!novel.bookmarked);
        assert!(novel.images.is_empty());
        assert!(novel.tags.is_empty());
    }

    #[test]
    fn detail_numbers_clamp_and_the_date_keeps_only_the_day() {
        let wire = crate::wire::NovelDetailWire {
            character_count: 4025,
            view_count: -3,
            x_restrict: 9,
            create_date: "2026-11-15T15:52:25+00:00".into(),
            ..crate::wire::NovelDetailWire::default()
        };
        let novel = Novel::from_detail(wire);
        assert_eq!(novel.character_count, 4025);
        // A negative counter is zero, never a wrapped 18446744073709551613.
        assert_eq!(novel.views, 0);
        // Out-of-range sensitivity clamps to the highest level.
        assert_eq!(novel.x_restrict, 2);
        assert_eq!(novel.upload_date, "2026-11-15");
    }

    #[test]
    fn an_unparseable_date_is_passed_through_verbatim() {
        let wire = crate::wire::NovelDetailWire {
            upload_date: "yesterday".into(),
            ..crate::wire::NovelDetailWire::default()
        };
        assert_eq!(Novel::from_detail(wire).upload_date, "yesterday");
    }

    #[test]
    fn series_navigation_links_the_neighbours_that_may_be_opened() {
        let nav = crate::wire::NovelSeriesNavWire {
            series_id: Id(15_745_812),
            title: "華国異伝".into(),
            order: 8,
            prev: Some(crate::wire::NovelSeriesLinkWire {
                id: Id(29_425_155),
                title: "二".into(),
                available: Some(true),
                ..crate::wire::NovelSeriesLinkWire::default()
            }),
            next: Some(crate::wire::NovelSeriesLinkWire {
                id: Id(30_000_000),
                title: "四".into(),
                available: Some(false),
                ..crate::wire::NovelSeriesLinkWire::default()
            }),
            ..crate::wire::NovelSeriesNavWire::default()
        };
        let nav = NovelSeriesNav::from_wire(nav).expect("in a series");
        assert_eq!(nav.id, Id(15_745_812));
        assert_eq!(nav.title, "華国異伝");
        assert_eq!(nav.order, 8);
        assert_eq!(nav.prev, Some(Id(29_425_155)));
        assert_eq!(nav.prev_title, "二");
        // A private or deleted neighbour is absent, not a link to an error page.
        assert_eq!(nav.next, None);
        assert_eq!(nav.next_title, "");
        assert!(!nav.concluded);
    }

    #[test]
    fn a_zero_series_id_is_not_a_series() {
        assert!(NovelSeriesNav::from_wire(crate::wire::NovelSeriesNavWire::default()).is_none());
    }

    #[test]
    fn image_tiers_prefer_the_reading_width_and_the_master() {
        let urls = crate::wire::NovelImageUrlsWire {
            mw240: "240.jpg".into(),
            mw480: "480.jpg".into(),
            master1200: "1200.jpg".into(),
            square: "128.jpg".into(),
            original: "orig.png".into(),
        };
        assert_eq!(urls.small(), "480.jpg");
        assert_eq!(urls.large(), "1200.jpg");
        assert_eq!(urls.cover(), "1200.jpg");
        // Falling inward when the reading tier is missing...
        let inward = crate::wire::NovelImageUrlsWire {
            mw480: String::new(),
            ..urls.clone()
        };
        assert_eq!(inward.small(), "240.jpg");
        // ...and outward when only the master tier is left.
        let outward = crate::wire::NovelImageUrlsWire {
            mw480: String::new(),
            mw240: String::new(),
            square: String::new(),
            ..urls.clone()
        };
        assert_eq!(outward.small(), "1200.jpg");
        assert_eq!(outward.large(), "1200.jpg");
        assert_eq!(crate::wire::NovelImageUrlsWire::default().small(), "");
    }

    #[test]
    fn a_row_uses_text_count_and_url_not_the_detail_field_names() {
        let row = crate::wire::NovelItemWire {
            id: Id(9),
            title: "t".into(),
            url: "cover.jpg".into(),
            text_count: 120,
            reading_time: 5,
            series_id: Some(Id(7)),
            series_content_order: 3,
            tags: vec!["a".into(), "  ".into()],
            ..crate::wire::NovelItemWire::default()
        };
        let summary = NovelSummary::from_row(row);
        assert_eq!(summary.character_count, 120);
        assert_eq!(summary.reading_time, 5);
        assert_eq!(summary.cover.as_deref(), Some("cover.jpg"));
        assert_eq!(summary.series_id, Some(Id(7)));
        assert_eq!(summary.series_order, 3);
        assert_eq!(summary.tags.len(), 1, "a blank tag name is not a tag");
    }

    #[test]
    fn a_standalone_row_has_no_series_and_a_zero_series_id_is_dropped() {
        let standalone = crate::wire::NovelItemWire {
            id: Id(9),
            series_id: Some(Id(0)),
            ..crate::wire::NovelItemWire::default()
        };
        let summary = NovelSummary::from_row(standalone);
        assert_eq!(summary.series_id, None);
        assert_eq!(summary.series_order, 0);
        assert_eq!(summary.upload_date, "");
    }
}
