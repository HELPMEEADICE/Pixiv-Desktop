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
    Id, Illust, PageMeta, RankingKind, RankingRow, SearchQuery, SearchResult, UserProfile,
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

/// The demo catalogue.
#[derive(Clone, Debug, Default)]
pub struct DemoCatalog {
    /// Every work, in a stable id order.
    pub(crate) illusts: Vec<Illust>,
    /// The artists, as the profile page wants them.
    pub(crate) artists: Vec<UserProfile>,
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
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use pixiv_types::{
        Id, Illust, IllustKind, RankingKind, SearchQuery, SearchTarget, SortOrder, TypeFilter,
    };

    use super::{DEMO_MARKER, DEMO_SCHEME, DemoCatalog, is_demo_url, render_placeholder};

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
}
