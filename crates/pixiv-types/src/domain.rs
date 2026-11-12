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
