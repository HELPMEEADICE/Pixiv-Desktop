//! Construction and querying of the demo catalogue.
//!
//! Every value here is derived from [`SEED`] through [`crate::prng`]; there is no
//! clock, no entropy and no I/O, so two runs — on two machines — produce the
//! byte-identical catalogue. The shapes are chosen to exercise the UI: three
//! aspect ratios, three illustration kinds, a mix of page counts, and stats with
//! enough spread that sorting and ranking look plausible.

use std::cmp::Ordering;

use pixiv_types::domain::Tag;
use pixiv_types::{
    Author, Bookmark, BookmarkRestrict, Id, Illust, IllustKind, IllustStats, IllustUrls, PageMeta,
    PageUrls, RankingKind, RankingRow, SearchQuery, SearchResult, SortOrder, TypeFilter,
    UserProfile,
};

use crate::prng::{Rng, mix};
use crate::{DEMO_MARKER, DEMO_SCHEME, DemoCatalog};

/// Master seed. Changing it reseeds the whole catalogue.
pub(crate) const SEED: u64 = 0x5EED_1DEA_D3A0_2024;

/// Illustrations in the catalogue.
const ILLUST_COUNT: usize = 60;
/// Works shown on the front page.
const FEED_SIZE: usize = 24;
/// Rows per search page.
const SEARCH_PAGE_SIZE: usize = 24;
/// Rows per ranking page.
const RANKING_PAGE_SIZE: usize = 50;
/// Tags offered by the discovery page.
const TRENDING_COUNT: usize = 12;
/// First illustration id.
const ILLUST_ID_BASE: u64 = 700_000_000;
/// First artist id.
const USER_ID_BASE: u64 = 900_000;
/// Epoch day of 2024-01-01, the base of the pseudo publication dates.
const DAY_BASE: i64 = 19_723;
/// Largest dimension of a generated `original` link.
const ORIGINAL_DIM: u32 = 4096;
/// Salt keeping the feed order independent from the id order.
const FEED_SALT: u64 = 0x00C0_FFEE_0BAD_F00D;

const ARTISTS: [(&str, &str); 8] = [
    ("朝霧シオン", "asagiri_shion"),
    ("雪鳴ユキ", "yukinari_yuki"),
    ("星屑ミナ", "hoshikuzu_mina"),
    ("月島レイ", "tsukishima_rei"),
    ("紫陽花あや", "ajisai_aya"),
    ("空色ハル", "sorairo_haru"),
    ("灯籠カナタ", "tourou_kanata"),
    ("白鳥ノア", "shiratori_noa"),
];

/// 15 × 8 = 120 unique prefix/noun pairs, enough for the 60 works below.
const TITLE_PREFIXES: [&str; 15] = [
    "静寂の",
    "きらめく",
    "黄昏の",
    "はじける",
    "夢見る",
    "ざわめく",
    "やさしい",
    "あやかしの",
    "しずかな",
    "とろける",
    "ひそやかな",
    "あざやかな",
    "ささやく",
    "燃える",
    "冴えた",
];

const TITLE_NOUNS: [&str; 8] = [
    "星空",
    "サクラ",
    "少女",
    "午後の窓",
    "雨音",
    "ネオン街",
    "花と猫",
    "水平線",
];

/// Tag pool. Every walk step below is coprime with this length, which is what
/// guarantees a work never repeats one of its own tags.
const TAG_POOL: [(&str, Option<&str>); 24] = [
    ("オリジナル", Some("original")),
    ("女の子", Some("girl")),
    ("風景", Some("scenery")),
    ("空", Some("sky")),
    ("猫", Some("cat")),
    ("和服", Some("kimono")),
    ("夜景", Some("night view")),
    ("ポートレート", Some("portrait")),
    ("ファンタジー", Some("fantasy")),
    ("メカ", Some("mecha")),
    ("水彩", Some("watercolor")),
    ("厚塗り", Some("impasto")),
    ("背景", Some("background")),
    ("瞳", Some("eyes")),
    ("花", Some("flower")),
    ("雨", Some("rain")),
    ("海", Some("sea")),
    ("制服", Some("school uniform")),
    ("魔法", Some("magic")),
    ("ドラゴン", Some("dragon")),
    ("桜", Some("sakura")),
    ("青空", Some("blue sky")),
    ("夕焼け", Some("sunset")),
    ("モノクロ", Some("monochrome")),
];

/// Strides coprime with `TAG_POOL.len()` (24 = 2³·3).
const TAG_STEPS: [u32; 8] = [1, 5, 7, 11, 13, 17, 19, 23];

const PORTRAIT_WIDTHS: [u32; 3] = [720, 900, 1024];
const LANDSCAPE_HEIGHTS: [u32; 3] = [720, 900, 1024];
const SQUARE_SIDES: [u32; 3] = [800, 1000, 1200];

/// An artist before the catalogue counts its works.
struct ArtistSeed {
    id: Id,
    name: &'static str,
    account: &'static str,
}

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

pub(crate) fn build() -> DemoCatalog {
    let artists = artist_seeds();
    let mut rng = Rng::new(SEED);

    let mut illusts: Vec<Illust> = Vec::with_capacity(ILLUST_COUNT);
    for index in 0..ILLUST_COUNT {
        if let Some(artist) = artists.get(index % artists.len().max(1)) {
            illusts.push(build_illust(index, artist, &mut rng));
        }
    }

    let profiles: Vec<UserProfile> = artists
        .iter()
        .map(|artist| build_profile(artist, &illusts, &mut rng))
        .collect();

    let trending = trending_tags_of(&illusts);
    DemoCatalog {
        illusts,
        artists: profiles,
        trending,
    }
}

fn artist_seeds() -> Vec<ArtistSeed> {
    ARTISTS
        .iter()
        .enumerate()
        .map(|(index, (name, account))| ArtistSeed {
            id: Id(USER_ID_BASE + u64::try_from(index).unwrap_or(0)),
            name,
            account,
        })
        .collect()
}

fn build_illust(index: usize, artist: &ArtistSeed, rng: &mut Rng) -> Illust {
    let kind = kind_for(index);
    let (width, height) = dimensions_for(index, rng);
    let seed = seed_for(index);
    let bookmarks = u64::from(rng.range(40, 18_000));

    Illust {
        id: Id(ILLUST_ID_BASE + u64::try_from(index).unwrap_or(0)),
        title: title_for(index),
        kind,
        page_count: page_count_for(kind, index),
        width,
        height,
        urls: cover_urls(seed, width, height),
        author: Author {
            id: artist.id,
            name: artist.name.to_owned(),
            account: artist.account.to_owned(),
            avatar: Some(avatar_url(artist.id)),
        },
        tags: tags_for(index, rng),
        description: format!(
            "{DEMO_MARKER}デモモード用に生成されたダミー作品です。実在の作品・作者とは関係ありません。"
        ),
        create_date: Some(date_for(rng)),
        bookmark: Bookmark {
            bookmarked: is_saved(index),
            restrict: BookmarkRestrict::Public,
            count: bookmarks,
        },
        stats: IllustStats {
            // Views always exceed bookmarks, as they do on a real listing.
            views: bookmarks.saturating_mul(u64::from(rng.range(9, 41))),
            bookmarks,
            likes: u64::from(rng.range(20, 6_000)),
            comments: u64::from(rng.range(0, 420)),
        },
        x_restrict: 0,
        sl: 0,
        is_masked: false,
        is_bookmarkable: true,
    }
}

fn build_profile(artist: &ArtistSeed, illusts: &[Illust], rng: &mut Rng) -> UserProfile {
    UserProfile {
        id: artist.id,
        name: artist.name.to_owned(),
        account: artist.account.to_owned(),
        avatar: Some(avatar_url(artist.id)),
        comment: format!("{DEMO_MARKER}{} のデモ用プロフィールです。", artist.name),
        followees: u64::from(rng.range(20, 900)),
        // A demo profile has no server to ask, and the real follower count is
        // only readable for the signed-in account, so it stays unknown here too.
        followers: None,
        illust_count: illusts
            .iter()
            .filter(|illust| illust.author.id == artist.id)
            .count(),
        manga_count: illusts
            .iter()
            .filter(|illust| {
                illust.author.id == artist.id && illust.kind == pixiv_types::IllustKind::Manga
            })
            .count(),
        is_followed: false,
        background: None,
        is_premium: false,
        webpage: None,
    }
}

/// Three kinds in a fixed 7 : 2 : 1 rhythm, so the grid always has manga and
/// ugoira badges to render.
fn kind_for(index: usize) -> IllustKind {
    match index % 10 {
        7 | 8 => IllustKind::Manga,
        9 => IllustKind::Ugoira,
        _ => IllustKind::Illustration,
    }
}

fn page_count_for(kind: IllustKind, index: usize) -> u32 {
    match kind {
        IllustKind::Manga => 3 + u32::try_from(index % 6).unwrap_or(0),
        IllustKind::Illustration | IllustKind::Ugoira => 1,
    }
}

/// Rotates portrait, landscape and square so the responsive grid is exercised.
fn dimensions_for(index: usize, rng: &mut Rng) -> (u32, u32) {
    match index % 3 {
        // Portrait, 2:3.
        0 => {
            let width = pick_dimension(&PORTRAIT_WIDTHS, rng, 900);
            (width, width.saturating_mul(3) / 2)
        }
        // Landscape, 3:2.
        1 => {
            let height = pick_dimension(&LANDSCAPE_HEIGHTS, rng, 900);
            (height.saturating_mul(3) / 2, height)
        }
        // Square.
        _ => {
            let side = pick_dimension(&SQUARE_SIDES, rng, 1000);
            (side, side)
        }
    }
}

fn pick_dimension(table: &[u32], rng: &mut Rng, fallback: u32) -> u32 {
    let len = u32::try_from(table.len()).unwrap_or(0);
    let index = usize::try_from(rng.below(len)).unwrap_or(0);
    table.get(index).copied().unwrap_or(fallback)
}

fn title_for(index: usize) -> String {
    let prefix = TITLE_PREFIXES
        .get(index % TITLE_PREFIXES.len())
        .copied()
        .unwrap_or_default();
    let noun = TITLE_NOUNS
        .get((index / TITLE_PREFIXES.len()) % TITLE_NOUNS.len())
        .copied()
        .unwrap_or_default();
    format!("{DEMO_MARKER}{prefix}{noun}")
}

fn tags_for(index: usize, rng: &mut Rng) -> Vec<Tag> {
    let len = TAG_POOL.len();
    if len == 0 {
        return Vec::new();
    }
    let start = usize::try_from(rng.below(u32::try_from(len).unwrap_or(0))).unwrap_or(0);
    let step = usize::try_from(TAG_STEPS[index % TAG_STEPS.len()]).unwrap_or(1);
    let count = 3 + index % 4;

    (0..count)
        .filter_map(|offset| TAG_POOL.get((start + offset * step) % len))
        .map(|(name, translated)| Tag {
            name: (*name).to_owned(),
            translated: translated.map(str::to_owned),
        })
        .collect()
}

/// A deterministic pseudo publication date, `YYYY-MM-DD hh:mm:00`.
fn date_for(rng: &mut Rng) -> String {
    let day = DAY_BASE + i64::from(rng.range(0, 730));
    let (year, month, day_of_month) = civil_from_days(day);
    let hour = rng.range(0, 24);
    let minute = rng.range(0, 60);
    format!("{year:04}-{month:02}-{day_of_month:02} {hour:02}:{minute:02}:00")
}

/// Civil date for a Unix epoch day number (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day_of_month = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day_of_month).unwrap_or(1),
    )
}

/// Every sixth work from index 3 is pre-saved, which yields the ten bookmark
/// rows the bookmarks page shows in Demo Mode.
fn is_saved(index: usize) -> bool {
    index % 6 == 3
}

fn seed_for(index: usize) -> u64 {
    mix(SEED
        ^ u64::try_from(index)
            .unwrap_or(0)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

fn seed_for_id(id: Id) -> u64 {
    mix(id.get().wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

fn avatar_url(user: Id) -> String {
    format!("{DEMO_SCHEME}user/{user}/avatar.png")
}

fn cover_urls(seed: u64, width: u32, height: u32) -> IllustUrls {
    IllustUrls {
        mini: tier_url(seed, width, height, 180, "mini"),
        thumb: tier_url(seed, width, height, 360, "thumb"),
        small: tier_url(seed, width, height, 540, "small"),
        regular: tier_url(seed, width, height, 1_200, "regular"),
        original: full_url(seed, width, height),
    }
}

fn tier_url(seed: u64, width: u32, height: u32, long_edge: u32, tier: &str) -> String {
    let (w, h) = fit(width, height, long_edge);
    format!("{DEMO_SCHEME}illust/{seed}/{w}x{h}/{tier}.png")
}

fn full_url(seed: u64, width: u32, height: u32) -> String {
    let w = width.saturating_mul(2).clamp(1, ORIGINAL_DIM);
    let h = height.saturating_mul(2).clamp(1, ORIGINAL_DIM);
    format!("{DEMO_SCHEME}illust/{seed}/{w}x{h}/original.png")
}

/// Scale `width × height` so its long edge matches `long_edge`, keeping the
/// aspect ratio. Images already smaller than the tier are left alone.
fn fit(width: u32, height: u32, long_edge: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (long_edge, long_edge);
    }
    let long = width.max(height);
    if long <= long_edge {
        return (width, height);
    }
    let scale = |value: u32| -> u32 {
        u32::try_from(u64::from(value) * u64::from(long_edge) / u64::from(long))
            .unwrap_or(1)
            .max(1)
    };
    (scale(width), scale(height))
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

pub(crate) fn feed(catalog: &DemoCatalog) -> Vec<Illust> {
    let mut items: Vec<&Illust> = catalog.illusts.iter().collect();
    // A per-work mixed key gives a stable "curated" order that mixes artists,
    // kinds and aspect ratios instead of showing one artist's block.
    items.sort_by_key(|illust| (mix(illust.id.get() ^ FEED_SALT), illust.id.get()));
    items.into_iter().take(FEED_SIZE).cloned().collect()
}

pub(crate) fn trending_tags(catalog: &DemoCatalog) -> Vec<String> {
    catalog.trending.clone()
}

fn trending_tags_of(illusts: &[Illust]) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for illust in illusts {
        for tag in &illust.tags {
            match counts.iter_mut().find(|(name, _)| *name == tag.name) {
                Some((_, count)) => *count += 1,
                None => counts.push((tag.name.clone(), 1)),
            }
        }
    }
    // Most used first, then alphabetically so ties never depend on hash order.
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
        .into_iter()
        .take(TRENDING_COUNT)
        .map(|(name, _)| name)
        .collect()
}

pub(crate) fn featured_artists(catalog: &DemoCatalog) -> Vec<UserProfile> {
    catalog.artists.clone()
}

pub(crate) fn search(catalog: &DemoCatalog, query: &SearchQuery) -> SearchResult {
    let needle = query.word.trim().to_lowercase();
    let mut items: Vec<Illust> = catalog
        .illusts
        .iter()
        .filter(|illust| kind_matches(illust.kind, query.kind) && matches_word(illust, &needle))
        .cloned()
        .collect();
    sort_items(&mut items, query.sort);
    paginate(items, query.page)
}

fn kind_matches(kind: IllustKind, filter: TypeFilter) -> bool {
    match filter {
        TypeFilter::All => true,
        TypeFilter::Illustration => kind == IllustKind::Illustration,
        TypeFilter::Manga => kind == IllustKind::Manga,
        TypeFilter::Ugoira => kind == IllustKind::Ugoira,
    }
}

/// Case-insensitive substring match over the title or any tag (raw or
/// translated). An empty needle matches everything.
fn matches_word(illust: &Illust, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if illust.title.to_lowercase().contains(needle) {
        return true;
    }
    illust.tags.iter().any(|tag| {
        tag.name.to_lowercase().contains(needle)
            || tag
                .translated
                .as_deref()
                .is_some_and(|translated| translated.to_lowercase().contains(needle))
    })
}

fn date_key(illust: &Illust) -> &str {
    illust.create_date.as_deref().unwrap_or_default()
}

fn sort_items(items: &mut [Illust], sort: SortOrder) {
    match sort {
        SortOrder::DateDesc => {
            items.sort_by(|a, b| date_key(b).cmp(date_key(a)).then_with(|| a.id.cmp(&b.id)))
        }
        SortOrder::DateAsc => {
            items.sort_by(|a, b| date_key(a).cmp(date_key(b)).then_with(|| a.id.cmp(&b.id)))
        }
        SortOrder::PopularDesc => items.sort_by(|a, b| {
            b.stats
                .bookmarks
                .cmp(&a.stats.bookmarks)
                .then_with(|| a.id.cmp(&b.id))
        }),
    }
}

fn paginate(items: Vec<Illust>, page: u32) -> SearchResult {
    let total = u64::try_from(items.len()).unwrap_or(0);
    let last_page = last_page_for(items.len(), SEARCH_PAGE_SIZE);
    let start = usize::try_from(page.max(1) - 1)
        .unwrap_or(usize::MAX / SEARCH_PAGE_SIZE)
        .saturating_mul(SEARCH_PAGE_SIZE);
    let end = start.saturating_add(SEARCH_PAGE_SIZE).min(items.len());
    SearchResult {
        items: items
            .get(start..end)
            .map(<[Illust]>::to_vec)
            .unwrap_or_default(),
        total,
        last_page,
    }
}

fn last_page_for(total: usize, page_size: usize) -> u32 {
    if total == 0 || page_size == 0 {
        return 0;
    }
    u32::try_from(total.div_ceil(page_size)).unwrap_or(u32::MAX)
}

pub(crate) fn ranking(catalog: &DemoCatalog, kind: RankingKind, page: u32) -> Vec<RankingRow> {
    let mut items: Vec<&Illust> = catalog.illusts.iter().collect();
    items.sort_by(|a, b| compare_rank(kind, a, b));

    let start = usize::try_from(page.max(1) - 1)
        .unwrap_or(usize::MAX / RANKING_PAGE_SIZE)
        .saturating_mul(RANKING_PAGE_SIZE);
    let end = start.saturating_add(RANKING_PAGE_SIZE);

    items
        .iter()
        .enumerate()
        .filter(|(offset, _)| *offset >= start && *offset < end)
        .map(|(offset, illust)| RankingRow {
            // Ranks are absolute, not page-relative: page 2 starts at 51.
            rank: u32::try_from(offset + 1).unwrap_or(u32::MAX),
            illust: (*illust).clone(),
        })
        .collect()
}

/// Each board sorts on a different signal, so switching boards visibly changes
/// the list. The id tie-breaker makes the order total, hence reproducible.
fn compare_rank(kind: RankingKind, a: &Illust, b: &Illust) -> Ordering {
    let primary = match kind {
        RankingKind::Daily => b.stats.bookmarks.cmp(&a.stats.bookmarks),
        RankingKind::Weekly => b.stats.views.cmp(&a.stats.views),
        RankingKind::Monthly => heat(b).cmp(&heat(a)),
        RankingKind::Rookie => date_key(b).cmp(date_key(a)),
        RankingKind::Original => b.stats.likes.cmp(&a.stats.likes),
        RankingKind::Male => b.stats.comments.cmp(&a.stats.comments),
        RankingKind::Female => mood(b).cmp(&mood(a)),
    };
    primary.then_with(|| a.id.cmp(&b.id))
}

fn heat(illust: &Illust) -> u64 {
    illust.stats.views / 2 + illust.stats.bookmarks.saturating_mul(3)
}

fn mood(illust: &Illust) -> u64 {
    illust
        .stats
        .likes
        .saturating_mul(2)
        .saturating_add(illust.stats.bookmarks)
}

pub(crate) fn detail(catalog: &DemoCatalog, id: Id) -> Option<Illust> {
    catalog
        .illusts
        .iter()
        .find(|illust| illust.id == id)
        .cloned()
}

pub(crate) fn pages(catalog: &DemoCatalog, id: Id) -> Vec<PageMeta> {
    let Some(illust) = catalog.illusts.iter().find(|illust| illust.id == id) else {
        return Vec::new();
    };
    let seed = seed_for_id(illust.id);
    let count = illust.page_count.max(1);
    (0..count)
        .map(|index| PageMeta {
            index,
            width: illust.width,
            height: illust.height,
            urls: page_urls(seed, index, illust.width, illust.height),
        })
        .collect()
}

fn page_urls(seed: u64, index: u32, width: u32, height: u32) -> PageUrls {
    let (thumb_w, thumb_h) = fit(width, height, 180);
    let (small_w, small_h) = fit(width, height, 540);
    let (regular_w, regular_h) = fit(width, height, 1_200);
    let original_w = width.saturating_mul(2).clamp(1, ORIGINAL_DIM);
    let original_h = height.saturating_mul(2).clamp(1, ORIGINAL_DIM);
    PageUrls {
        thumb_mini: page_image(seed, index, thumb_w, thumb_h, "thumb"),
        small: page_image(seed, index, small_w, small_h, "small"),
        regular: page_image(seed, index, regular_w, regular_h, "regular"),
        original: page_image(seed, index, original_w, original_h, "original"),
    }
}

fn page_image(seed: u64, index: u32, width: u32, height: u32, tier: &str) -> String {
    format!("{DEMO_SCHEME}illust/{seed}/{width}x{height}/page-{index}-{tier}.png")
}

pub(crate) fn profile(catalog: &DemoCatalog, user: Id) -> Option<UserProfile> {
    catalog
        .artists
        .iter()
        .find(|artist| artist.id == user)
        .cloned()
}

pub(crate) fn works_by(catalog: &DemoCatalog, user: Id) -> Vec<Illust> {
    let mut items: Vec<Illust> = catalog
        .illusts
        .iter()
        .filter(|illust| illust.author.id == user)
        .cloned()
        .collect();
    sort_items(&mut items, SortOrder::DateDesc);
    items
}

pub(crate) fn bookmarks(catalog: &DemoCatalog) -> SearchResult {
    let mut items: Vec<Illust> = catalog
        .illusts
        .iter()
        .filter(|illust| illust.bookmark.bookmarked)
        .cloned()
        .collect();
    sort_items(&mut items, SortOrder::DateDesc);
    SearchResult {
        total: u64::try_from(items.len()).unwrap_or(0),
        last_page: last_page_for(items.len(), SEARCH_PAGE_SIZE),
        items,
    }
}
