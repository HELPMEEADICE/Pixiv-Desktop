//! Construction and querying of the demo catalogue.
//!
//! Every value here is derived from [`SEED`] through [`crate::prng`]; there is no
//! clock, no entropy and no I/O, so two runs — on two machines — produce the
//! byte-identical catalogue. The shapes are chosen to exercise the UI: three
//! aspect ratios, three illustration kinds, a mix of page counts, and stats with
//! enough spread that sorting and ranking look plausible. The novels are the
//! second half: one eight-chapter series whose first body carries every marker the
//! novel grammar knows, one standalone work, and one `isLoginOnly` row — enough
//! data that the reader can be photographed, and no more.

use std::cmp::Ordering;

use pixiv_types::domain::Tag;
use pixiv_types::{
    Author, Bookmark, BookmarkRestrict, Id, Illust, IllustKind, IllustStats, IllustUrls, Novel,
    NovelImage, NovelSeries, NovelSeriesNav, NovelSummary, PageMeta, PageUrls, RankingKind,
    RankingRow, SearchQuery, SearchResult, SortOrder, TypeFilter, UserProfile,
};

use crate::prng::{Rng, mix};
use crate::{DEMO_MARKER, DEMO_SCHEME, DemoCatalog, DemoNovel};

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

/// First novel id. Chapters occupy `base + 1 ..= base + 8`, so a chapter's order
/// is readable straight off its id.
pub(crate) const NOVEL_ID_BASE: u64 = 810_000_000;
/// The one series in the catalogue.
pub(crate) const NOVEL_SERIES_ID: u64 = 810_000_500;
/// Side of a generated novel cover. Pixiv serves novel covers square, and the
/// cover is drawn by the same procedural renderer the illustrations use, so the
/// fixture ships no image file.
pub(crate) const NOVEL_COVER_SIDE: u32 = 480;
/// Offset of the standalone work, past the chapter block.
const STANDALONE_ID_OFFSET: u64 = 100;
/// Offset of the row a session-less client may not open.
const LOGIN_ONLY_ID_OFFSET: u64 = 101;
/// The uploaded image every chapter's body refers to, resolved through
/// `Novel::images` the way `textEmbeddedImages` resolves it over the wire.
const UPLOADED_IMAGE_ID: &str = "26247899";
/// Artists who publish the demo novels, as indices into [`ARTISTS`].
const SERIES_AUTHOR: usize = 0;
const STANDALONE_AUTHOR: usize = 3;
const LOGGED_OUT_AUTHOR: usize = 6;

const CHAPTER_TITLES: [&str; 8] = [
    "第一章　硝子の海",
    "第二章　灯りをともす",
    "第三章　雨のあとで",
    "第四章　古い写真",
    "第五章　白い鳥",
    "第六章　遠い祭り",
    "第七章　冬の手紙",
    "終章　水平線",
];

/// Chapter one's body, and the reason this fixture exists: it carries every
/// marker the grammar knows, in one document.
///
/// * `[chapter:...]` on its own line — a heading (a section of this chapter, not
///   the chapter title itself).
/// * `***` on its own line — a separator.
/// * `[[rb:硝子 > がらす]]` — ruby, base and reading.
/// * `[[jumpuri:label > url]]` — a link. The URL is under the reserved `.invalid`
///   TLD, so a demo build that ever tried to fetch it would fail immediately
///   instead of reaching a third party.
/// * `[[jump:700000001]]` — an in-site jump, kept as a `jump:` href.
/// * `[pixivimage:700000018-2]` — page 2 of a work in this very catalogue, so the
///   reader's artwork link resolves.
/// * `[uploadedimage:26247899]` — an embedded image, resolved through
///   [`UPLOADED_IMAGE_ID`]; on its own line it becomes an image block.
/// * `[newpage]` — a page break.
/// * `[fontsize:18]` — a marker this client does not implement. It must survive as
///   literal text; deleting it would lose content the author wrote.
const SERIES_CHAPTER_ONE: &str = "\
[chapter:一節　朝の海]
静かな朝だった。窓の外では、[[rb:硝子 > がらす]]のように透けた海がまだ眠っている。
***
その日、ぼくは[[jumpuri:古い日記 > https://example.invalid/demo/diary]]を開いた。
[[jump:700000001]]の頁に、少女の写真が挟まっていた。
[pixivimage:700000018-2]を、どこかで見た気がした。
[uploadedimage:26247899]
[fontsize:18]この行の頭には、この端末が解釈しない指定が付いている。
[newpage]
[chapter:二節　港の灯り]
港の灯りが、ひとつずつ消えていく。ぼくは名前を書かないまま、頁を閉じた。";

/// One body per chapter. Chapter one is the coverage document above; the others
/// exist so the series panel has eight real chapters to list, and so a reader
/// that opens chapter five does not open chapter one's text again.
const SERIES_CHAPTER_BODIES: [&str; 8] = [
    SERIES_CHAPTER_ONE,
    "灯りをともすと、窓にぼくの顔が映った。\n\n海はもう起きていた。",
    "雨のあとで、砂浜には貝がらが増えている。\n\nひとつ拾って、耳に当てた。",
    "古い写真の裏には、読めない字で日付だけが書かれていた。",
    "白い鳥が一羽、桟橋の杭にとまって、しばらく動かなかった。",
    "遠い祭りの音が、風に乗ってここまで届く夜がある。",
    "冬の手紙は、まだ封を切っていない。\n\n切るのが惜しかった。",
    "水平線が、思っていたより近かった。\n\nここが終わりではない。",
];

const STANDALONE_TITLE: &str = "短編集　雨と猫";
const STANDALONE_BODY: &str = "\
雨の音で目が覚めた。[[rb:雨音 > あまおと]]は、窓を叩くたびに少しずつ違う。
猫が一匹、濡れたまま、軒下に座っている。";

/// A row whose body a session-less client may not read (`isLoginOnly`).
const LOGIN_ONLY_TITLE: &str = "会員限定　書きかけの頁";

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

    let (novels, series) = build_novels();
    let trending = trending_tags_of(&illusts);
    DemoCatalog {
        illusts,
        artists: profiles,
        novels,
        series,
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
// Novels
// ---------------------------------------------------------------------------

/// Build the novels: one eight-chapter series, one standalone work, and one row
/// a session-less client may not open.
///
/// Nothing here draws on [`SEED`]: the bodies are literals and every derived
/// number comes from the work id itself, so the novel half of the catalogue is
/// deterministic for a reason a reader can check by eye.
fn build_novels() -> (Vec<DemoNovel>, Vec<NovelSeries>) {
    let series_id = Id(NOVEL_SERIES_ID);
    let series_title = format!("{DEMO_MARKER}硝子の海");
    let author = author_at(SERIES_AUTHOR);
    let chapters = SERIES_CHAPTER_BODIES.len();

    let mut novels: Vec<DemoNovel> = Vec::with_capacity(chapters + 2);
    for order in 1..=chapters {
        let id = Id(NOVEL_ID_BASE + u64::try_from(order).unwrap_or(0));
        let previous =
            (order > 1).then(|| Id(NOVEL_ID_BASE + u64::try_from(order - 1).unwrap_or(0)));
        let following =
            (order < chapters).then(|| Id(NOVEL_ID_BASE + u64::try_from(order + 1).unwrap_or(0)));
        let nav = NovelSeriesNav {
            id: series_id,
            title: series_title.clone(),
            order: u32::try_from(order).unwrap_or(0),
            prev: previous,
            next: following,
            prev_title: if order > 1 {
                chapter_title_of(order - 1)
            } else {
                String::new()
            },
            next_title: if order < chapters {
                chapter_title_of(order + 1)
            } else {
                String::new()
            },
            concluded: true,
        };
        let body = SERIES_CHAPTER_BODIES
            .get(order - 1)
            .copied()
            .unwrap_or_default();
        let day = DAY_BASE + i64::try_from(order - 1).unwrap_or(0) * 3;
        let novel = build_novel(id, &chapter_title_of(order), &author, body, Some(nav), day);
        novels.push(readable(novel));
    }

    let series = NovelSeries {
        id: series_id,
        title: series_title,
        caption: format!(
            "{DEMO_MARKER}デモモード用の連載です。全{chapters}章、実在の作品・作者とは関係ありません。"
        ),
        author: author.clone(),
        cover: Some(novel_cover_url(series_id)),
        tags: novel_tags(),
        total: u32::try_from(chapters).unwrap_or(0),
        total_characters: novels.iter().map(|row| row.summary.character_count).sum(),
        total_reading_time: novels.iter().map(|row| row.summary.reading_time).sum(),
        first_novel: Some(Id(NOVEL_ID_BASE + 1)),
        latest_novel: Some(Id(NOVEL_ID_BASE + u64::try_from(chapters).unwrap_or(0))),
        concluded: true,
    };

    let standalone = build_novel(
        Id(NOVEL_ID_BASE + STANDALONE_ID_OFFSET),
        STANDALONE_TITLE,
        &author_at(STANDALONE_AUTHOR),
        STANDALONE_BODY,
        None,
        DAY_BASE + 40,
    );
    novels.push(readable(standalone));

    // The isLoginOnly row: the row is real, the document is not. NOVEL-READER
    // section 1 point 6 records that an unreadable work answers an error
    // envelope, so there is no body to hand out and the template below never
    // leaves this function.
    let locked = build_novel(
        Id(NOVEL_ID_BASE + LOGIN_ONLY_ID_OFFSET),
        LOGIN_ONLY_TITLE,
        &author_at(LOGGED_OUT_AUTHOR),
        "",
        None,
        DAY_BASE + 41,
    );
    novels.push(DemoNovel {
        summary: summary_of(&locked),
        detail: None,
        is_login_only: true,
    });

    (novels, vec![series])
}

fn build_novel(
    id: Id,
    title: &str,
    author: &Author,
    body: &str,
    series: Option<NovelSeriesNav>,
    day: i64,
) -> Novel {
    let characters = u32::try_from(body.chars().count()).unwrap_or(u32::MAX);
    // Counters ride on the same id-derived value the cover uses, so a build
    // cannot produce two disagreeing totals.
    let noise = u32::try_from(seed_for_id(id) % 1_000).unwrap_or(0);
    Novel {
        id,
        title: format!("{DEMO_MARKER}{title}"),
        author: author.clone(),
        cover: Some(novel_cover_url(id)),
        caption: format!(
            "{DEMO_MARKER}デモモード用の本文です。実在の作品・作者とは関係ありません。"
        ),
        tags: novel_tags(),
        body: body.to_owned(),
        images: novel_images(),
        // The API characterCount is the length of the markup, not of the text a
        // reader sees (NOVEL-READER section 1 point 3), so the demo counts the
        // body it ships.
        character_count: characters,
        word_count: characters / 2,
        reading_time: (characters / 400).max(1),
        page_count: new_page_count(body),
        views: 600 + noise.saturating_mul(5),
        bookmarks: 20 + noise % 300,
        likes: 5 + noise % 90,
        upload_date: novel_date(day),
        is_original: true,
        x_restrict: 0,
        language: "ja".to_owned(),
        series,
        bookmarked: false,
    }
}

fn readable(novel: Novel) -> DemoNovel {
    DemoNovel {
        summary: summary_of(&novel),
        detail: Some(novel),
        is_login_only: false,
    }
}

/// The row shape of a document: what every list surface draws.
fn summary_of(novel: &Novel) -> NovelSummary {
    let series = novel.series.as_ref();
    NovelSummary {
        id: novel.id,
        title: novel.title.clone(),
        author: novel.author.clone(),
        cover: novel.cover.clone(),
        tags: novel.tags.clone(),
        caption: novel.caption.clone(),
        // The document does not carry a genre code, so the row derives it from the
        // one field Pixiv also derives it from: an original work is オリジナル.
        genre: if novel.is_original { "1" } else { "2" }.to_owned(),
        language: novel.language.clone(),
        bookmarks: novel.bookmarks,
        character_count: novel.character_count,
        reading_time: novel.reading_time,
        series_id: series.map(|nav| nav.id),
        series_title: series.map(|nav| nav.title.clone()).unwrap_or_default(),
        series_order: series.map(|nav| nav.order).unwrap_or(0),
        upload_date: novel.upload_date.clone(),
        bookmarked: novel.bookmarked,
        x_restrict: novel.x_restrict,
    }
}

fn author_at(index: usize) -> Author {
    let id = Id(USER_ID_BASE + u64::try_from(index).unwrap_or(0));
    let (name, account) = ARTISTS.get(index).copied().unwrap_or_default();
    Author {
        id,
        name: name.to_owned(),
        account: account.to_owned(),
        avatar: Some(avatar_url(id)),
    }
}

fn chapter_title_of(order: usize) -> String {
    let title = CHAPTER_TITLES
        .get(order.saturating_sub(1))
        .copied()
        .unwrap_or_default();
    format!("{DEMO_MARKER}{title}")
}

/// A cover URL in the same demo:// grammar as the illustrations: nothing
/// fetches it, and the bytes come from [`crate::render_placeholder`] seeded by
/// the id.
fn novel_cover_url(id: Id) -> String {
    let side = NOVEL_COVER_SIDE;
    format!("{DEMO_SCHEME}novel/{id}/{side}x{side}/cover.png")
}

/// The embedded image, resolved the way textEmbeddedImages resolves it.
fn novel_images() -> Vec<NovelImage> {
    vec![NovelImage {
        id: UPLOADED_IMAGE_ID.to_owned(),
        small: format!("{DEMO_SCHEME}novel/image/{UPLOADED_IMAGE_ID}/480x480/small.png"),
        large: format!("{DEMO_SCHEME}novel/image/{UPLOADED_IMAGE_ID}/1200x1200/large.png"),
    }]
}

/// Tags taken from the illustration pool, so the demo never invents vocabulary
/// the rest of the catalogue does not also use.
fn novel_tags() -> Vec<Tag> {
    // オリジナル, 風景, 海.
    [0_usize, 2, 16]
        .into_iter()
        .filter_map(|index| TAG_POOL.get(index))
        .map(|(name, translated)| Tag {
            name: (*name).to_owned(),
            translated: translated.map(str::to_owned),
        })
        .collect()
}

/// Pages in a body: every explicit [newpage] marker starts one.
fn new_page_count(body: &str) -> u32 {
    u32::try_from(body.matches("[newpage]").count() + 1).unwrap_or(1)
}

/// A publication date as a novel row carries it: YYYY-MM-DD.
fn novel_date(day: i64) -> String {
    let (year, month, day_of_month) = civil_from_days(day);
    format!("{year:04}-{month:02}-{day_of_month:02}")
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
/// Every novel row, in a stable id order: the series chapters, then the
/// standalone work, then the row a session-less client may not open.
pub(crate) fn novels(catalog: &DemoCatalog) -> Vec<DemoNovel> {
    catalog.novels.clone()
}

/// One novel with its body. `isLoginOnly` rows resolve to `None`: the row exists,
/// the document does not.
pub(crate) fn novel(catalog: &DemoCatalog, id: Id) -> Option<Novel> {
    catalog
        .novels
        .iter()
        .find(|row| row.summary.id == id)
        .and_then(|row| row.detail.clone())
}

pub(crate) fn novel_series(catalog: &DemoCatalog, id: Id) -> Option<NovelSeries> {
    catalog
        .series
        .iter()
        .find(|series| series.id == id)
        .cloned()
}

/// One series chapter list, in reading order.
pub(crate) fn novel_chapters(catalog: &DemoCatalog, series: Id) -> Vec<DemoNovel> {
    let mut rows: Vec<DemoNovel> = catalog
        .novels
        .iter()
        .filter(|row| row.summary.series_id == Some(series))
        .cloned()
        .collect();
    rows.sort_by_key(|row| row.summary.series_order);
    rows
}
