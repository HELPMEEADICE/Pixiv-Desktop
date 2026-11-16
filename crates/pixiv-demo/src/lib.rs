//! Demo Mode data.
//!
//! # Why this crate is separate
//!
//! Demo Mode exists so the UI can be developed and demonstrated without a Pixiv
//! session. The one hard rule is that it must never be mistaken for real data:
//!
//! * Every demo illustration URL uses the `demo://` scheme, so any code path that
//!   reaches the network is visibly wrong.
//! * Titles carry a `DEMO_MARKER` prefix and the UI shows a persistent banner
//!   while Demo Mode is active.
//! * Nothing here parses or imitates a Pixiv JSON response.
//!
//! Artwork is generated procedurally at request time, so the repository ships no
//! third-party imagery and there is no copyright question.
//!
//! The novels arrive the same way: one eight-chapter series, one standalone work
//! and one `isLoginOnly` row whose body a session-less client may not read, with
//! the first chapter's body carrying every marker the novel grammar knows. Their
//! covers are `demo://` URLs like the illustrations', so the repository ships no
//! novel imagery either.
//!
//! # Determinism
//!
//! The catalogue is a pure function of a single integer seed (`catalog::SEED`)
//! and the placeholder renderer is a pure function of `(width, height, seed)`.
//! No clock, entropy or I/O is involved anywhere, so two runs — on two
//! machines — produce exactly the same data and the same PNG bytes.

mod catalog;
mod prng;
mod render;

use pixiv_types::{
    Id, Illust, Novel, NovelSeries, NovelSummary, PageMeta, RankingKind, RankingRow, SearchQuery,
    SearchResult, UserProfile,
};

/// Scheme marking every demo-owned URL.
pub const DEMO_SCHEME: &str = "demo://";

/// Prefix applied to demo titles so they can never be confused with real works.
pub const DEMO_MARKER: &str = "[DEMO] ";

/// True when `url` belongs to Demo Mode.
#[must_use]
pub fn is_demo_url(url: &str) -> bool {
    url.as_bytes()
        .get(..DEMO_SCHEME.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(DEMO_SCHEME.as_bytes()))
}

/// Render a deterministic placeholder illustration as PNG bytes.
///
/// Must be pure: the same `seed` always yields the same image, so caches and
/// layout snapshots stay stable across runs.
#[must_use]
pub fn render_placeholder(width: u32, height: u32, seed: u64) -> Vec<u8> {
    render::render(width, height, seed)
}

/// One novel in the demo catalogue.
///
/// The row and the document are kept side by side because the two are not the
/// same list: `isLoginOnly` says a row exists while its body may not be read, and
/// `NovelSummary` has no field to carry that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DemoNovel {
    /// What a list surface draws: search hits, a series' chapters, an artist's work.
    pub summary: NovelSummary,
    /// The document, body included. `None` when the work cannot be opened.
    pub detail: Option<Novel>,
    /// The wire flag `isLoginOnly`: the row exists, the body does not.
    pub is_login_only: bool,
}

/// The demo catalogue.
#[derive(Clone, Debug, Default)]
pub struct DemoCatalog {
    /// Every work, in a stable id order.
    pub(crate) illusts: Vec<Illust>,
    /// The artists, as the profile page wants them.
    pub(crate) artists: Vec<UserProfile>,
    /// Every novel, in a stable id order.
    pub(crate) novels: Vec<DemoNovel>,
    /// The one novel series.
    pub(crate) series: Vec<NovelSeries>,
    /// Tags for the discovery page, most used first.
    pub(crate) trending: Vec<String>,
}

impl DemoCatalog {
    /// Build a fixed, seeded catalogue.
    #[must_use]
    pub fn new() -> Self {
        catalog::build()
    }

    /// Illustration count in the catalogue.
    #[must_use]
    pub fn len(&self) -> usize {
        self.illusts.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Front page feed.
    #[must_use]
    pub fn feed(&self) -> Vec<Illust> {
        catalog::feed(self)
    }

    /// Popular tags for the discovery page.
    #[must_use]
    pub fn trending_tags(&self) -> Vec<String> {
        catalog::trending_tags(self)
    }

    /// Featured artists for the discovery page.
    #[must_use]
    pub fn featured_artists(&self) -> Vec<UserProfile> {
        catalog::featured_artists(self)
    }

    /// Filtered, paged search over the catalogue.
    #[must_use]
    pub fn search(&self, query: &SearchQuery) -> SearchResult {
        catalog::search(self, query)
    }

    /// Ranking board.
    #[must_use]
    pub fn ranking(&self, kind: RankingKind, page: u32) -> Vec<RankingRow> {
        catalog::ranking(self, kind, page)
    }

    #[must_use]
    pub fn detail(&self, id: Id) -> Option<Illust> {
        catalog::detail(self, id)
    }

    /// Demo pages, whose URLs are `demo://` and therefore served locally.
    #[must_use]
    pub fn pages(&self, id: Id) -> Vec<PageMeta> {
        catalog::pages(self, id)
    }

    #[must_use]
    pub fn profile(&self, user: Id) -> Option<UserProfile> {
        catalog::profile(self, user)
    }

    /// Works by one artist.
    #[must_use]
    pub fn works_by(&self, user: Id) -> Vec<Illust> {
        catalog::works_by(self, user)
    }

    /// Saved works, used by the bookmarks page in Demo Mode.
    #[must_use]
    pub fn bookmarks(&self) -> SearchResult {
        catalog::bookmarks(self)
    }

    /// Every novel row, in a stable id order: the eight chapters of the series,
    /// the standalone work, then the row a session-less client may not open.
    #[must_use]
    pub fn novels(&self) -> Vec<DemoNovel> {
        catalog::novels(self)
    }

    /// One novel with its body.
    ///
    /// `None` means the id is not in the catalogue **or** the work is
    /// `isLoginOnly`: a row that cannot be read has no document to hand out.
    #[must_use]
    pub fn novel(&self, id: Id) -> Option<Novel> {
        catalog::novel(self, id)
    }

    /// Series metadata, for the reader's chapter panel.
    #[must_use]
    pub fn novel_series(&self, id: Id) -> Option<NovelSeries> {
        catalog::novel_series(self, id)
    }

    /// One series' chapters, in reading order.
    #[must_use]
    pub fn novel_chapters(&self, series: Id) -> Vec<DemoNovel> {
        catalog::novel_chapters(self, series)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use pixiv_novel::{BlockKind, ImageInfo, Span};
    use pixiv_types::{
        Id, Illust, IllustKind, NovelSummary, RankingKind, SearchQuery, SearchTarget, SortOrder,
        TypeFilter,
    };

    use super::{
        DEMO_MARKER, DEMO_SCHEME, DemoCatalog, DemoNovel, is_demo_url, render_placeholder,
    };
    use crate::catalog::{NOVEL_COVER_SIDE, NOVEL_ID_BASE, NOVEL_SERIES_ID};

    fn catalogue() -> DemoCatalog {
        DemoCatalog::new()
    }

    /// The whole catalogue, straight from the (crate-private) storage.
    fn all(catalog: &DemoCatalog) -> Vec<Illust> {
        catalog.illusts.clone()
    }

    fn query(word: &str) -> SearchQuery {
        SearchQuery::new(word)
    }

    fn ids(items: &[Illust]) -> Vec<Id> {
        items.iter().map(|illust| illust.id).collect()
    }

    #[test]
    fn catalogue_has_sixty_works_with_the_expected_kind_mix() {
        let catalog = catalogue();
        assert_eq!(catalog.len(), 60);
        assert!(!catalog.is_empty());

        let (mut illustration, mut manga, mut ugoira) = (0_usize, 0_usize, 0_usize);
        for illust in all(&catalog) {
            match illust.kind {
                IllustKind::Illustration => {
                    illustration += 1;
                    assert_eq!(illust.page_count, 1, "{} has pages", illust.id);
                }
                IllustKind::Manga => {
                    manga += 1;
                    assert!(
                        (3..=8).contains(&illust.page_count),
                        "{} has {} pages",
                        illust.id,
                        illust.page_count
                    );
                }
                IllustKind::Ugoira => {
                    ugoira += 1;
                    assert_eq!(illust.page_count, 1, "{} has pages", illust.id);
                }
            }
            assert!(
                (3..=6).contains(&illust.tags.len()),
                "{} has {} tags",
                illust.id,
                illust.tags.len()
            );
            assert!(illust.width > 0 && illust.height > 0);
            assert!(illust.stats.views > illust.stats.bookmarks);
            assert_eq!(illust.bookmark.count, illust.stats.bookmarks);
            assert_eq!(illust.x_restrict, 0);
            assert!(illust.is_bookmarkable);
        }

        assert_eq!((illustration, manga, ugoira), (42, 12, 6));
    }

    #[test]
    fn catalogue_exercises_portrait_landscape_and_square() {
        let catalog = catalogue();
        let (mut portrait, mut landscape, mut square) = (0_usize, 0_usize, 0_usize);
        for illust in all(&catalog) {
            let ratio = illust.aspect_ratio();
            if ratio < 0.75 {
                portrait += 1;
                assert!(illust.height > illust.width);
            } else if ratio > 1.25 {
                landscape += 1;
                assert!(illust.width > illust.height);
            } else {
                square += 1;
                assert_eq!(illust.width, illust.height);
            }
        }
        assert_eq!((portrait, landscape, square), (20, 20, 20));
    }

    #[test]
    fn every_title_is_marked_and_every_url_is_demo() {
        let catalog = catalogue();
        let illusts = all(&catalog);
        assert_eq!(illusts.len(), 60);

        for illust in &illusts {
            assert!(
                illust.title.starts_with(DEMO_MARKER),
                "unmarked title {:?}",
                illust.title
            );
            let urls = [
                &illust.urls.mini,
                &illust.urls.thumb,
                &illust.urls.small,
                &illust.urls.regular,
                &illust.urls.original,
            ];
            for url in urls {
                assert!(url.starts_with(DEMO_SCHEME), "url {url} leaked");
                assert!(is_demo_url(url));
            }
            if let Some(avatar) = &illust.author.avatar {
                assert!(is_demo_url(avatar), "avatar {avatar} leaked");
            }

            let pages = catalog.pages(illust.id);
            assert_eq!(
                u32::try_from(pages.len()).unwrap_or(0),
                illust.page_count,
                "{} page mismatch",
                illust.id
            );
            for page in &pages {
                let urls = [
                    &page.urls.thumb_mini,
                    &page.urls.small,
                    &page.urls.regular,
                    &page.urls.original,
                ];
                for url in urls {
                    assert!(is_demo_url(url), "page url {url} leaked");
                }
            }
        }

        for artist in catalog.featured_artists() {
            assert!(!artist.name.starts_with(DEMO_MARKER));
            assert!(artist.comment.starts_with(DEMO_MARKER));
            if let Some(avatar) = &artist.avatar {
                assert!(is_demo_url(avatar));
            }
        }

        for row in catalog.ranking(RankingKind::Daily, 1) {
            assert!(row.illust.title.starts_with(DEMO_MARKER));
            assert!(is_demo_url(row.illust.urls.grid()));
        }

        for illust in catalog.feed() {
            assert!(illust.title.starts_with(DEMO_MARKER));
        }
        for illust in catalog.bookmarks().items {
            assert!(illust.title.starts_with(DEMO_MARKER));
        }
    }

    #[test]
    fn catalogue_is_deterministic_across_builds() {
        let first = catalogue();
        let second = catalogue();
        assert_eq!(first.illusts, second.illusts);
        assert_eq!(first.artists, second.artists);
        assert_eq!(first.trending, second.trending);
        assert_eq!(first.feed(), second.feed());
        assert_eq!(first.trending_tags(), second.trending_tags());
        assert_eq!(
            first.ranking(RankingKind::Monthly, 1),
            second.ranking(RankingKind::Monthly, 1)
        );
        assert_eq!(first.bookmarks(), second.bookmarks());
    }

    #[test]
    fn titles_are_distinct() {
        let catalog = catalogue();
        let titles: BTreeSet<String> = all(&catalog).into_iter().map(|i| i.title).collect();
        assert_eq!(titles.len(), 60);
    }

    #[test]
    fn search_with_an_empty_word_returns_the_paged_catalogue() {
        let catalog = catalogue();
        let page_one = catalog.search(&query(""));
        assert_eq!(page_one.total, 60);
        assert_eq!(page_one.items.len(), 24);
        assert_eq!(page_one.last_page, 3);

        let page_two = catalog.search(&query("").with_page(2));
        assert_eq!(page_two.items.len(), 24);
        assert_eq!(page_two.last_page, 3);
        assert_ne!(ids(&page_one.items), ids(&page_two.items));

        let page_three = catalog.search(&query("").with_page(3));
        assert_eq!(page_three.items.len(), 12);

        let page_four = catalog.search(&query("").with_page(4));
        assert!(page_four.items.is_empty());
        assert_eq!(page_four.total, 60);

        // A page number of 0 is treated as page 1 rather than panicking.
        let mut zero = query("");
        zero.page = 0;
        zero.kind = TypeFilter::All;
        zero.sort = SortOrder::DateDesc;
        zero.target = SearchTarget::TagFull;
        assert_eq!(ids(&catalog.search(&zero).items), ids(&page_one.items));
    }

    #[test]
    fn search_filters_by_kind_word_and_tag() {
        let catalog = catalogue();

        let mut manga = query("");
        manga.kind = TypeFilter::Manga;
        let result = catalog.search(&manga);
        assert_eq!(result.total, 12);
        assert_eq!(result.last_page, 1);
        assert!(
            result
                .items
                .iter()
                .all(|illust| illust.kind == IllustKind::Manga)
        );

        let mut ugoira = query("").with_page(1);
        ugoira.kind = TypeFilter::Ugoira;
        let result = catalog.search(&ugoira);
        assert_eq!(result.total, 6);
        assert!(
            result
                .items
                .iter()
                .all(|illust| illust.kind == IllustKind::Ugoira)
        );

        let mut illustration = query("");
        illustration.kind = TypeFilter::Illustration;
        let result = catalog.search(&illustration);
        assert_eq!(result.total, 42);
        assert_eq!(result.last_page, 2);

        // Title match.
        let title_hits = catalog.search(&query("星"));
        assert!(title_hits.total >= 15, "total {}", title_hits.total);
        for illust in &title_hits.items {
            assert!(
                illust.title.contains('星')
                    || illust.tags.iter().any(|tag| tag.name.contains('星')
                        || tag.translated.as_deref().is_some_and(|t| t.contains('星'))),
                "{} did not match",
                illust.title
            );
        }

        // Tag match, taken from the catalogue itself so it cannot go stale.
        let helper = catalog.search(&query(""));
        let tag = helper
            .items
            .first()
            .and_then(|illust| illust.tags.first())
            .map(|tag| tag.name.clone())
            .expect("a tag");
        let tag_hits = catalog.search(&query(&tag));
        assert!(tag_hits.total > 0);
        assert!(tag_hits.total < 60, "a single tag matched everything");
        assert!(
            tag_hits
                .items
                .iter()
                .all(|illust| illust.tags.iter().any(|candidate| candidate.name == tag))
        );

        // Case-insensitive substring over the translated tag text.
        let sky = catalog.search(&query("SKY"));
        assert!(sky.total > 0);
        assert!(sky.items.iter().all(|illust| illust.tags.iter().any(|tag| {
            tag.translated
                .as_deref()
                .is_some_and(|t| t.to_lowercase().contains("sky"))
        })));

        // No match at all.
        let empty = catalog.search(&query("そんな作品はありません"));
        assert_eq!(empty.total, 0);
        assert_eq!(empty.last_page, 0);
        assert!(empty.items.is_empty());
    }

    #[test]
    fn search_sorts_are_applied_and_paged() {
        let catalog = catalogue();

        let mut newest = query("");
        newest.sort = SortOrder::DateDesc;
        let newest = catalog.search(&newest);

        let mut oldest = query("");
        oldest.sort = SortOrder::DateAsc;
        let oldest = catalog.search(&oldest);

        let mut popular = query("");
        popular.sort = SortOrder::PopularDesc;
        let popular = catalog.search(&popular);

        assert_eq!(newest.total, oldest.total);
        assert_ne!(ids(&newest.items), ids(&oldest.items));

        let dates = |items: &[Illust]| -> Vec<String> {
            items
                .iter()
                .map(|illust| illust.create_date.clone().unwrap_or_default())
                .collect()
        };
        let desc_dates = dates(&newest.items);
        let asc_dates = dates(&oldest.items);
        assert!(desc_dates.windows(2).all(|w| w[0] >= w[1]));
        assert!(asc_dates.windows(2).all(|w| w[0] <= w[1]));

        // Popular ordering is a total order on the bookmark count.
        assert!(
            popular
                .items
                .windows(2)
                .all(|w| w[0].stats.bookmarks >= w[1].stats.bookmarks)
        );
        let all_bookmarks: Vec<u64> = all(&catalog)
            .iter()
            .map(|illust| illust.stats.bookmarks)
            .collect();
        if let Some(max) = all_bookmarks.iter().max() {
            assert_eq!(popular.items[0].stats.bookmarks, *max);
        }

        // A second page of a sorted query continues the same order.
        let mut newest_page_two = query("").with_page(2);
        newest_page_two.sort = SortOrder::DateDesc;
        let newest_page_two = catalog.search(&newest_page_two);
        assert_eq!(newest_page_two.items.len(), 24);
        let last_of_page_one = newest
            .items
            .last()
            .map(|illust| illust.create_date.clone().unwrap_or_default());
        let first_of_page_two = newest_page_two
            .items
            .first()
            .map(|illust| illust.create_date.clone().unwrap_or_default());
        assert!(first_of_page_two <= last_of_page_one);
    }

    #[test]
    fn ranking_numbers_rows_absolutely_and_differs_per_kind() {
        let catalog = catalogue();

        let first_page = catalog.ranking(RankingKind::Daily, 1);
        assert_eq!(first_page.len(), 50);
        assert_eq!(first_page[0].rank, 1);
        assert_eq!(first_page[49].rank, 50);

        let second_page = catalog.ranking(RankingKind::Daily, 2);
        assert_eq!(second_page.len(), 10);
        assert_eq!(second_page[0].rank, 51);
        assert_eq!(second_page[9].rank, 60);

        assert!(catalog.ranking(RankingKind::Daily, 3).is_empty());
        // A page number of 0 is treated as page 1.
        assert_eq!(catalog.ranking(RankingKind::Weekly, 0).len(), 50);

        let ranks: Vec<u32> = first_page.iter().map(|row| row.rank).collect();
        let expected: Vec<u32> = (1..=50).collect();
        assert_eq!(ranks, expected);

        let orders: Vec<Vec<Id>> = RankingKind::ALL
            .into_iter()
            .map(|kind| {
                ids(&catalog
                    .ranking(kind, 1)
                    .iter()
                    .map(|row| row.illust.clone())
                    .collect::<Vec<_>>())
            })
            .collect();
        let distinct: BTreeSet<Vec<Id>> = orders.iter().cloned().collect();
        assert!(
            distinct.len() >= 6,
            "only {} of 7 boards produced a distinct order",
            distinct.len()
        );
        assert_ne!(orders[0], orders[1], "daily and weekly agreed");
        assert_ne!(
            catalog.ranking(RankingKind::Daily, 1)[0].illust.id,
            catalog.ranking(RankingKind::Rookie, 1)[0].illust.id
        );
    }

    #[test]
    fn unknown_ids_report_absent_entries() {
        let catalog = catalogue();
        for id in [Id(0), Id(1), Id(u64::MAX), Id(123_456_789)] {
            assert!(catalog.detail(id).is_none(), "detail({id}) was Some");
            assert!(catalog.profile(id).is_none(), "profile({id}) was Some");
            assert!(catalog.pages(id).is_empty(), "pages({id}) was not empty");
            assert!(
                catalog.works_by(id).is_empty(),
                "works_by({id}) was not empty"
            );
        }
    }

    #[test]
    fn known_ids_round_trip_through_the_lookups() {
        let catalog = catalogue();
        for illust in all(&catalog) {
            let detail = catalog.detail(illust.id).expect("catalogue entry resolves");
            assert_eq!(detail, illust);

            let pages = catalog.pages(illust.id);
            assert_eq!(u32::try_from(pages.len()).unwrap_or(0), illust.page_count);
            assert_eq!(pages[0].index, 0);
            assert_eq!(pages[0].width, illust.width);
            assert_eq!(pages[0].height, illust.height);
            let page_urls: BTreeSet<&str> = pages
                .iter()
                .map(|page| page.urls.regular.as_str())
                .collect();
            assert_eq!(page_urls.len(), pages.len(), "page urls collide");

            let profile = catalog
                .profile(illust.author.id)
                .expect("artist has a profile");
            assert_eq!(profile.id, illust.author.id);
            assert!(profile.illust_count > 0);
            assert_eq!(
                profile.illust_count,
                catalog.works_by(illust.author.id).len(),
                "profile count disagrees with the works list"
            );
        }
    }

    #[test]
    fn discovery_surfaces_are_populated() {
        let catalog = catalogue();

        let feed = catalog.feed();
        assert_eq!(feed.len(), 24);
        assert!(
            feed.iter()
                .all(|illust| catalog.detail(illust.id).is_some())
        );
        let feed_ids: BTreeSet<Id> = feed.iter().map(|illust| illust.id).collect();
        assert_eq!(feed_ids.len(), 24);

        let tags = catalog.trending_tags();
        assert!(
            (10..=14).contains(&tags.len()),
            "trending returned {} tags",
            tags.len()
        );
        let unique: BTreeSet<&String> = tags.iter().collect();
        assert_eq!(unique.len(), tags.len(), "trending tags repeat");

        let artists = catalog.featured_artists();
        assert_eq!(artists.len(), 8);
        let artist_ids: BTreeSet<Id> = artists.iter().map(|artist| artist.id).collect();
        assert_eq!(artist_ids.len(), 8);
        assert_eq!(
            artists
                .iter()
                .map(|artist| artist.illust_count)
                .sum::<usize>(),
            60
        );

        let saved = catalog.bookmarks();
        assert_eq!(saved.total, 10);
        assert_eq!(saved.items.len(), 10);
        assert_eq!(saved.last_page, 1);
        assert!(saved.items.iter().all(|illust| illust.bookmark.bookmarked));
        assert!(ids(&saved.items).iter().all(|id| {
            catalog
                .detail(*id)
                .is_some_and(|illust| illust.bookmark.bookmarked)
        }));
    }

    #[test]
    fn is_demo_url_classifies_urls() {
        assert!(is_demo_url("demo://illust/1/800x600/regular.png"));
        assert!(is_demo_url("demo://"));
        assert!(is_demo_url("DEMO://illust/1"));
        assert!(!is_demo_url(""));
        assert!(!is_demo_url("dem"));
        assert!(!is_demo_url("demo:/illust/1"));
        assert!(!is_demo_url("https://i.pximg.net/img-original/img/x.png"));
        assert!(!is_demo_url("file:///demo/x.png"));
        assert!(!is_demo_url(" not-a-url"));
    }

    #[test]
    fn placeholder_png_is_deterministic_and_decodable() {
        let first = render_placeholder(64, 48, 7);
        let second = render_placeholder(64, 48, 7);
        assert_eq!(first, second, "same seed must yield identical bytes");
        assert_ne!(first, render_placeholder(64, 48, 8));

        assert_eq!(&first[..8], b"\x89PNG\r\n\x1a\n");

        let decoded = image::load_from_memory(&first).expect("png decodes");
        assert_eq!((decoded.width(), decoded.height()), (64, 48));
    }

    #[test]
    fn placeholder_is_not_a_flat_block() {
        let bytes = render_placeholder(96, 96, 4_242);
        let image = image::load_from_memory(&bytes)
            .expect("png decodes")
            .to_rgb8();
        let top = image.get_pixel(48, 1);
        let bottom = image.get_pixel(48, 94);
        assert_ne!(top, bottom, "gradient is flat");

        let colours: BTreeSet<(u8, u8, u8)> = image
            .pixels()
            .map(|pixel| (pixel[0], pixel[1], pixel[2]))
            .collect();
        assert!(colours.len() > 8, "only {} distinct colours", colours.len());
    }

    #[test]
    fn placeholder_survives_degenerate_and_oversized_dimensions() {
        for (width, height) in [(0, 0), (0, 32), (32, 0)] {
            let bytes = render_placeholder(width, height, 1);
            assert!(!bytes.is_empty());
            let decoded = image::load_from_memory(&bytes).expect("png decodes");
            assert_eq!((decoded.width(), decoded.height()), (1, 1));
        }

        // Oversized requests are clamped instead of allocating gigabytes.
        let wide = render_placeholder(999_999, 8, 3);
        let decoded = image::load_from_memory(&wide).expect("png decodes");
        assert_eq!((decoded.width(), decoded.height()), (4096, 8));

        let tall = render_placeholder(8, 999_999, 3);
        let decoded = image::load_from_memory(&tall).expect("png decodes");
        assert_eq!((decoded.width(), decoded.height()), (8, 4096));

        // A 1x1 request is still a valid image.
        let single = render_placeholder(1, 1, 0);
        let decoded = image::load_from_memory(&single).expect("png decodes");
        assert_eq!((decoded.width(), decoded.height()), (1, 1));
    }
    /// Every chapter of the demo series, in reading order.
    fn series_rows(catalog: &DemoCatalog) -> Vec<NovelSummary> {
        catalog
            .novel_chapters(Id(NOVEL_SERIES_ID))
            .into_iter()
            .map(|row| row.summary)
            .collect()
    }

    #[test]
    fn the_series_has_eight_chapters_in_reading_order() {
        let catalog = catalogue();
        let series = catalog
            .novel_series(Id(NOVEL_SERIES_ID))
            .expect("the series exists");
        let rows = series_rows(&catalog);

        assert_eq!(rows.len(), 8, "the series has {} chapters", rows.len());
        assert_eq!(
            series.total, 8,
            "the header disagrees with the chapter list"
        );
        assert_eq!(series.first_novel, rows.first().map(|row| row.id));
        assert_eq!(series.latest_novel, rows.last().map(|row| row.id));

        // Order is 1..=8 with no gap and no repeat.
        let orders: Vec<u32> = rows.iter().map(|row| row.series_order).collect();
        assert_eq!(
            orders,
            (1..=8).collect::<Vec<u32>>(),
            "orders are not 1..=8"
        );

        let titles: BTreeSet<&String> = rows.iter().map(|row| &row.title).collect();
        assert_eq!(titles.len(), 8, "two chapters share a title");

        for (index, row) in rows.iter().enumerate() {
            let order = u32::try_from(index + 1).unwrap_or(0);
            assert_eq!(row.id, Id(NOVEL_ID_BASE + u64::from(order)));
            assert_eq!(row.series_id, Some(Id(NOVEL_SERIES_ID)));
            assert_eq!(row.series_title, series.title);
            assert_eq!(row.series_order, order);
            assert!(
                row.title.starts_with(DEMO_MARKER),
                "unmarked {:?}",
                row.title
            );
            assert!(row.character_count > 0, "chapter {order} has no text");
        }

        // The reader nav chain agrees with the list at both ends and in the middle.
        let nav_of = |order: u64| {
            catalog
                .novel(Id(NOVEL_ID_BASE + order))
                .expect("a chapter document")
                .series
                .expect("a chapter is in the series")
        };
        let first = nav_of(1);
        assert_eq!(first.order, 1);
        assert!(first.prev.is_none(), "chapter one has a previous chapter");
        assert_eq!(first.next, Some(Id(NOVEL_ID_BASE + 2)));
        assert!(first.prev_title.is_empty());

        let middle = nav_of(4);
        assert_eq!(middle.prev, Some(Id(NOVEL_ID_BASE + 3)));
        assert_eq!(middle.next, Some(Id(NOVEL_ID_BASE + 5)));
        assert!(middle.prev_title.starts_with(DEMO_MARKER));
        assert!(middle.next_title.starts_with(DEMO_MARKER));

        let last = nav_of(8);
        assert_eq!(last.order, 8);
        assert!(last.next.is_none(), "the last chapter has a next chapter");
        assert_eq!(last.prev, Some(Id(NOVEL_ID_BASE + 7)));
        assert!(last.next_title.is_empty());
    }

    #[test]
    fn the_first_chapter_exercises_every_block_and_span_kind() {
        let catalog = catalogue();
        let chapter = catalog.novel(Id(NOVEL_ID_BASE + 1)).expect("chapter one");
        let images: BTreeMap<String, ImageInfo> = chapter
            .images
            .iter()
            .map(|image| {
                (
                    image.id.clone(),
                    ImageInfo {
                        small: image.small.clone(),
                        large: image.large.clone(),
                    },
                )
            })
            .collect();
        let document = pixiv_novel::parse(&chapter.body, &images);

        for kind in [
            BlockKind::Paragraph,
            BlockKind::Heading,
            BlockKind::Separator,
            BlockKind::PageBreak,
            BlockKind::Image,
        ] {
            assert!(
                document.blocks.iter().any(|block| block.kind == kind),
                "the fixture carries no {kind:?} block"
            );
        }

        let mut ruby = 0_usize;
        let mut work_ref: Option<String> = None;
        let mut hrefs: Vec<String> = Vec::new();
        for span in document.blocks.iter().flat_map(|block| &block.spans) {
            match span {
                Span::Ruby { base, reading } => {
                    assert!(!base.is_empty() && !reading.is_empty(), "an empty ruby run");
                    ruby += 1;
                }
                Span::WorkRef { id, .. } => work_ref = Some(id.clone()),
                Span::Link { href, .. } => hrefs.push(href.clone()),
                Span::Text(_) => {}
            }
        }
        assert_eq!(ruby, 1, "expected exactly one ruby run");
        assert!(
            hrefs.iter().any(|href| href.starts_with("https://")),
            "no external link: {hrefs:?}"
        );
        assert!(
            hrefs.iter().any(|href| href.starts_with("jump:")),
            "no site jump: {hrefs:?}"
        );

        // The image marker resolved through the map: an unresolved one would draw
        // a placeholder instead of the embedded illustration.
        let image_block = document
            .blocks
            .iter()
            .find(|block| block.kind == BlockKind::Image)
            .expect("an image block");
        assert!(
            !image_block.missing,
            "the image map did not resolve the body"
        );
        assert_eq!(document.images.len(), 1, "expected one embedded image");

        // A marker the grammar does not know stays literal: deleting it would lose
        // content the author wrote.
        assert!(
            document.plain.contains("[fontsize:18]"),
            "an unknown marker was deleted: {:?}",
            document.plain
        );
        assert!(
            document.blocks.iter().any(|block| block.spans.iter().any(
                |span| matches!(span, Span::Text(text) if text.contains("[fontsize:18]この行"))
            )),
            "the unknown marker did not stay inside a text span"
        );

        // The ids the body names exist in this same catalogue, so the reader link
        // and the jump open something instead of an empty page.
        let work = work_ref.expect("a work reference");
        let work_id: u64 = work.parse().expect("a numeric work id");
        assert!(
            catalog.detail(Id(work_id)).is_some(),
            "work {work} is not in the catalogue"
        );
        let jump = hrefs
            .iter()
            .find_map(|href| href.strip_prefix("jump:"))
            .expect("a jump href");
        let jump_id: u64 = jump.parse().expect("a numeric jump id");
        assert!(
            catalog.detail(Id(jump_id)).is_some(),
            "jump {jump} is not in the catalogue"
        );

        // The API sends the length of the markup, not of the text a reader sees,
        // so the fixture counts what it ships.
        assert_eq!(
            usize::try_from(chapter.character_count).unwrap_or(0),
            chapter.body.chars().count()
        );
    }

    #[test]
    fn the_standalone_work_and_the_login_only_row_are_distinguishable() {
        let catalog = catalogue();
        let rows = catalog.novels();
        assert_eq!(rows.len(), 10, "8 chapters + 1 standalone + 1 locked");

        let standalone: Vec<&DemoNovel> = rows
            .iter()
            .filter(|row| row.summary.series_id.is_none())
            .collect();
        assert_eq!(
            standalone.len(),
            2,
            "one standalone work and one locked row"
        );
        let open = standalone
            .iter()
            .find(|row| !row.is_login_only)
            .expect("one readable standalone work");
        assert!(open.detail.is_some(), "the standalone work has no body");
        assert!(!open.is_login_only);

        let locked: Vec<&DemoNovel> = rows.iter().filter(|row| row.is_login_only).collect();
        assert_eq!(locked.len(), 1, "expected one isLoginOnly row");
        let locked = locked[0];
        assert!(
            locked.detail.is_none(),
            "a login-only row handed out a body"
        );
        assert!(
            catalog.novel(locked.summary.id).is_none(),
            "the login-only row resolved to a document"
        );
        // The row is still drawable: a list has to be able to show it.
        assert!(locked.summary.cover.is_some());
        assert!(locked.summary.title.starts_with(DEMO_MARKER));

        // Every row is labelled and every URL is a demo URL, so the catalogue can
        // neither be mistaken for real data nor reach the network.
        for row in &rows {
            assert!(
                row.summary.title.starts_with(DEMO_MARKER),
                "unmarked {:?}",
                row.summary.title
            );
            let cover = row.summary.cover.as_deref().expect("every row has a cover");
            assert!(is_demo_url(cover), "cover {cover} leaked");
            assert!(row.summary.tags.len() >= 2, "a novel with no tags");
            if let Some(detail) = &row.detail {
                assert_eq!(detail.id, row.summary.id);
                assert!(detail.title.starts_with(DEMO_MARKER));
                assert!(detail.caption.starts_with(DEMO_MARKER));
                assert!(detail.author.avatar.as_deref().is_some_and(is_demo_url));
                for image in &detail.images {
                    assert!(is_demo_url(&image.small) && is_demo_url(&image.large));
                }
            }
        }
    }

    #[test]
    fn the_novels_are_a_pure_function_of_the_build() {
        let first = catalogue();
        let second = catalogue();
        assert_eq!(first.novels(), second.novels(), "two builds disagree");
        assert_eq!(
            first.novel_series(Id(NOVEL_SERIES_ID)),
            second.novel_series(Id(NOVEL_SERIES_ID))
        );
        for row in first.novels() {
            let id = row.summary.id;
            assert_eq!(first.novel(id), second.novel(id), "novel {id} is unstable");
        }

        // The cover is drawn, not shipped: the id seeds the existing procedural
        // renderer, and the same id always produces the same bytes at the size the
        // cover URL declares.
        for row in first.novels() {
            let id = row.summary.id;
            let cover = row.summary.cover.as_deref().expect("a cover");
            assert!(
                cover.contains(&id.to_string()),
                "cover {cover} does not name {id}"
            );
            let bytes = render_placeholder(NOVEL_COVER_SIDE, NOVEL_COVER_SIDE, id.get());
            assert_eq!(
                bytes,
                render_placeholder(NOVEL_COVER_SIDE, NOVEL_COVER_SIDE, id.get()),
                "cover bytes for {id} are not stable"
            );
            let decoded = image::load_from_memory(&bytes).expect("png decodes");
            assert_eq!(
                (decoded.width(), decoded.height()),
                (NOVEL_COVER_SIDE, NOVEL_COVER_SIDE)
            );
        }
    }
}
