//! Contract tests against JSON captured from the live site.
//!
//! These are the tests that stop the client from silently drifting out of sync
//! with Pixiv: they parse real responses through the same structs production
//! code uses, and assert on the individual quirks that are easy to regress.

use std::path::PathBuf;

use pixiv_types::domain::*;
use pixiv_types::wire::*;
use pixiv_types::{Id, IllustKind};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn illust_detail_matches_live_payload() {
    let raw = fixture("illust_detail.json");
    let env: Envelope<IllustDetailWire> = serde_json::from_str(&raw).expect("detail envelope");
    assert!(!env.error, "fixture has no error flag set");

    let illust = Illust::from_detail(env.body);

    assert_eq!(illust.id, Id(117_661_885));
    assert_eq!(illust.title, "あの大空へ");
    assert_eq!(illust.kind, IllustKind::Illustration);
    assert_eq!(illust.page_count, 1);
    assert_eq!(illust.width, 2048);
    assert!(illust.height > 0);
    assert_eq!(illust.author.name, "はるりぃ*｡+");
    assert!(!illust.author.account.is_empty());
    // Nested tag groups must have been flattened.
    assert!(
        illust.tags.iter().any(|t| t.name.contains("sky")),
        "expected flattened tags, got {:?}",
        illust.tags.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
    // `urls.regular` is the master1200 tier and must be present.
    assert!(illust.urls.regular.contains("i.pximg.net"));
    assert!(illust.urls.original.ends_with(".jpg"));
    assert_eq!(illust.urls.grid(), illust.urls.regular);
    // No session cookie means no bookmark state.
    assert!(!illust.bookmark.bookmarked);
    assert!(illust.is_bookmarkable);
}

#[test]
fn illust_pages_carry_original_resolution_urls() {
    let raw = fixture("illust_pages.json");
    let env: Envelope<Vec<IllustPageWire>> = serde_json::from_str(&raw).expect("pages envelope");
    assert_eq!(env.body.len(), 1);

    let page = PageMeta::from_wire(0, env.body.into_iter().next().unwrap());
    assert_eq!(page.index, 0);
    assert_eq!(page.width, 2048);
    assert!(page.urls.thumb_mini.contains("square1200"));
    assert!(
        page.urls.original.contains("img-original"),
        "original tier lives under img-original, got {}",
        page.urls.original
    );
    assert_eq!(page.urls.largest(), page.urls.original);
}

#[test]
fn ugoira_meta_exposes_frames_and_original_zip() {
    let raw = fixture("ugoira_meta.json");
    let env: Envelope<UgoiraMetaWire> = serde_json::from_str(&raw).expect("ugoira envelope");
    let meta: UgoiraMeta = env.body.into();

    assert!(!meta.frames.is_empty(), "ugoira must declare frames");
    assert!(meta.zip_url.contains("img-zip-ugoira"));
    assert!(meta.original_zip_url.contains("img-zip-ugoira"));
    assert_eq!(meta.best_zip(), meta.original_zip_url);
    assert_eq!(meta.frames[0].file, "000000.jpg");
    assert_eq!(meta.frames[0].delay_ms, 200);
    assert!(meta.total_duration_ms() >= 200);
    assert!(meta.mime_type.contains("jpeg"), "got {}", meta.mime_type);
}

#[test]
fn ranking_rows_build_without_page_urls() {
    let raw = fixture("ranking_daily.json");
    let doc: RankingResponseWire = serde_json::from_str(&raw).expect("ranking document");
    assert_eq!(doc.contents.len(), 50, "ranking page holds 50 entries");

    let row = &doc.contents[0];
    // ranking.php sends ids as numbers, ajax/* sends strings: both must parse.
    assert!(!row.illust_id.is_zero());
    assert_eq!(row.rank, 1);

    let illust = Illust::from_ranking(doc.contents[0].clone());
    assert!(!illust.title.is_empty());
    assert!(!illust.author.name.is_empty());
    // Ranking only provides a pre-cropped thumbnail.
    assert!(!illust.urls.grid().is_empty());
    assert!(illust.urls.original.is_empty());
    assert_eq!(illust.urls.largest(), illust.urls.regular);
}

#[test]
fn search_result_flattens_string_tags() {
    let raw = fixture("search_artworks.json");
    let env: Envelope<SearchBodyWire> = serde_json::from_str(&raw).expect("search envelope");
    assert!(!env.error);

    let result = SearchResult::from_wire(env.body);
    assert!(!result.items.is_empty());
    assert!(result.total > 0);
    assert!(result.last_page >= 1);

    let first = &result.items[0];
    assert!(!first.id.is_zero());
    // Search rows have flat string tags, unlike the detail endpoint.
    assert!(!first.tags.is_empty());
    assert!(first.tags.iter().all(|t| !t.name.is_empty()));
    assert!(first.urls.grid().contains("i.pximg.net"));
}

#[test]
fn user_profile_all_lists_illust_ids() {
    let raw = fixture("user_profile_all.json");
    let env: Envelope<UserProfileAllWire> = serde_json::from_str(&raw).expect("profile envelope");
    assert!(!env.body.illusts.is_empty());
    // Every key must be a parseable id.
    for key in env.body.illusts.keys() {
        assert!(key.parse::<u64>().is_ok(), "bad id key {key}");
    }
    assert!(env.body.illusts.keys().count() > 0);
    // `bookmarkCount` is deliberately not modelled: the field is a number in
    // some payloads and an object in others, and the authoritative count comes
    // from the user detail endpoint.
    assert!(env.body.pickup.len() <= 4);
}

#[test]
fn search_tags_payload_parses() {
    let raw = fixture("search_tags.json");
    let env: Envelope<SearchTagsWire> = serde_json::from_str(&raw).expect("tags envelope");
    assert!(!env.body.tag.is_empty());
}

#[test]
fn envelope_with_error_flag_parses() {
    // Shape of the auth-gated rejection: `{"error":true,"message":"...","body":[]}`.
    let raw = r#"{"error":true,"message":"不正确的请求。","body":[]}"#;
    let env: Envelope<Vec<IllustPageWire>> = serde_json::from_str(raw).expect("error envelope");
    assert!(env.error);
    assert_eq!(env.message, "不正确的请求。");
    assert!(env.body.is_empty());
}

/// G54: user-visible text carries no markup.
///
/// The fixture is half of this gate: `description` has the shape the live
/// `/ajax/illust/{id}` sends - a `<br />` between paragraphs and an `<a href>`
/// around the work it cites - so the two assertions that matter cannot pass by
/// accident on a caption with no markup in it. The other half is C1: before it,
/// `from_detail` copied the string verbatim and both `contains` lines below were
/// red; a detail page then drew `<br />` and `<a href="...">` at the user.
#[test]
fn a_detail_caption_reaches_the_domain_without_markup() {
    let raw = fixture("illust_detail_html.json");
    let env: Envelope<IllustDetailWire> = serde_json::from_str(&raw).expect("detail envelope");
    assert!(
        env.body.description.contains("<br"),
        "the fixture must carry a break, or this gate cannot see the defect"
    );
    assert!(
        env.body.description.contains("<a "),
        "and a link, for the same reason"
    );

    let illust = Illust::from_detail(env.body);
    assert!(
        !illust.description.contains("<br"),
        "markup reached the domain: {}",
        illust.description
    );
    assert!(
        !illust.description.contains("<a "),
        "markup reached the domain: {}",
        illust.description
    );
    // Cleaned the way the novel path already cleans its caption: the break is a
    // newline, and a cited work keeps its target.
    assert!(
        illust.description.contains('\n'),
        "the break is a line break: {}",
        illust.description
    );
    assert!(
        illust
            .description
            .contains("https://www.pixiv.net/novel/show.php?id=27815393"),
        "the link target survives: {}",
        illust.description
    );
    assert!(illust.description.starts_with("新作を投稿しました。"));
}

/// The detail document has two caption fields, and the fallback is the one that
/// reaches the page when `description` is empty - so it is cleaned too.
#[test]
fn a_comment_only_caption_is_cleaned_too() {
    let raw = fixture("illust_detail_html.json");
    let mut env: Envelope<IllustDetailWire> = serde_json::from_str(&raw).expect("detail envelope");
    assert!(
        env.body.illust_comment.contains("<br"),
        "the fixture carries it"
    );
    env.body.description = String::new();

    let illust = Illust::from_detail(env.body);
    assert!(
        !illust.description.contains("<br"),
        "{}",
        illust.description
    );
    assert!(
        !illust.description.contains("<a "),
        "{}",
        illust.description
    );
    assert!(illust.description.contains('\n'));
}

/// A search row's `description` is the same HTML-ish text, so the same rule
/// applies to the other constructor that copies it.
#[test]
fn a_search_caption_is_cleaned_the_same_way() {
    let row: SearchItemWire = serde_json::from_str(
        r#"{"id":"151939482","description":"一行目<br />二行目<br /><a href=\"https://x.test/p\">出典</a>","tags":["t"]}"#,
    )
    .expect("search row");
    assert!(row.description.contains("<br"));

    let illust = Illust::from_search(row);
    assert_eq!(
        illust.description,
        "一行目\n二行目\n出典 (https://x.test/p)"
    );
}
/// C4: a ranking row carries the date it was ranked, and the ranking was the one
/// surface whose cards had no date at all.
///
/// The row's `date` is the board's own stamp for that work ("2026年11月09日 00:00"),
/// not the `illust_upload_timestamp`; `from_ranking` read neither before this.
#[test]
fn a_ranking_row_carries_the_date_it_was_ranked() {
    let raw = fixture("ranking_daily.json");
    let doc: RankingResponseWire = serde_json::from_str(&raw).expect("ranking document");
    let row = doc.contents.first().cloned().expect("a first row");
    let stamped = row.date.trim().to_string();
    assert!(!stamped.is_empty(), "the fixture carries a row date");

    let illust = Illust::from_ranking(row);
    assert_eq!(illust.create_date.as_deref(), Some(stamped.as_str()));
}

/// A row with no date is `None`, not an empty string: the card draws no date
/// rather than an empty slot.
#[test]
fn a_ranking_row_without_a_date_has_none() {
    let raw = fixture("ranking_daily.json");
    let doc: RankingResponseWire = serde_json::from_str(&raw).expect("ranking document");
    let mut row = doc.contents.into_iter().next().expect("a first row");
    row.date = "   ".to_string();
    assert_eq!(Illust::from_ranking(row).create_date, None);
}
/// C5: a novel row carries its genre, language and bookmark count, so the two
/// lists of one search surface have the same information density.
///
/// The row shape is `/ajax/search/novels/*`'s: `genre` is a *string* code, the
/// count is `bookmarkCount` (the detail document's `bookmarkCount` is the same
/// number under the same name, which is why the row does not rename it).
#[test]
fn a_novel_row_keeps_its_genre_language_and_bookmarks() {
    let row: NovelItemWire = serde_json::from_str(
        r#"{"id":"27815393","title":"華国異伝","genre":"2","language":"ja","bookmarkCount":412,"textCount":120}"#,
    )
    .expect("novel row");
    let summary = NovelSummary::from_row(row);
    assert_eq!(summary.genre, "2");
    assert_eq!(summary.language, "ja");
    assert_eq!(summary.bookmarks, 412);
}

/// The two absence rules, because a missing value must not become a zero the
/// list then prints: no genre is the empty string, no count is 0.
#[test]
fn a_novel_row_without_genre_or_count_reports_neither() {
    let row: NovelItemWire = serde_json::from_str(r#"{"id":"1","title":"t"}"#).expect("bare row");
    let summary = NovelSummary::from_row(row);
    assert_eq!(summary.genre, "");
    assert_eq!(summary.language, "");
    assert_eq!(summary.bookmarks, 0);
}
/// C2: the AI flag reaches the domain from every route that carries it.
///
/// The wire decoded `aiType` on the detail document and nowhere else, so the one
/// field that tells a reader which works are generated stopped at the wire.
/// Every row of `fixtures/search_artworks.json` is `"aiType":1`.
#[test]
fn an_ai_flag_reaches_the_domain_from_every_route() {
    let raw = fixture("search_artworks.json");
    let env: Envelope<SearchBodyWire> = serde_json::from_str(&raw).expect("search envelope");
    let result = SearchResult::from_wire(env.body);
    let first = &result.items[0];
    assert_eq!(first.ai_type, 1, "search rows carry aiType");
    // Pixiv's alt text is "#tag title - author", and every captured row has one.
    assert!(first.alt.starts_with('#'), "alt: {}", first.alt);

    let raw = fixture("illust_detail_html.json");
    let env: Envelope<IllustDetailWire> = serde_json::from_str(&raw).expect("detail envelope");
    let illust = Illust::from_detail(env.body);
    assert_eq!(illust.ai_type, 2, "the capture is AI-assisted");
    assert!(illust.alt.contains("灯火"), "alt: {}", illust.alt);

    // The home feed spells the same field `aiType` too, and carries no alt text.
    let row: StreetThumbWire =
        serde_json::from_str(r#"{"id":"151999999","title":"t","aiType":1,"sl":2}"#)
            .expect("street row");
    let card = Illust::from_street(row);
    assert_eq!(card.ai_type, 1);
    assert_eq!(card.sl, 2);
    assert!(card.alt.is_empty(), "no alt text on this route");
}

/// C3: the mask flag is filled on the route that has one, under the name that
/// route gives it.
///
/// The street payload calls it `isSensitiveForMasking` and `from_street` never
/// read it, so a work Pixiv marks sensitive was drawn unmasked on the home page.
#[test]
fn a_street_card_keeps_the_mask_flag() {
    let masked: StreetThumbWire =
        serde_json::from_str(r#"{"id":"1","title":"t","isSensitiveForMasking":true,"sl":2}"#)
            .expect("street row");
    let card = Illust::from_street(masked);
    assert!(card.is_masked);
    assert_eq!(card.sl, 2);

    // Absent means not masked; the flag is not inferred from the sensitivity level.
    let plain: StreetThumbWire =
        serde_json::from_str(r#"{"id":"2","title":"t"}"#).expect("street row");
    let card = Illust::from_street(plain);
    assert!(!card.is_masked);
    assert_eq!(card.sl, 0);
}
/// A street card's caption is the same HTML-ish text as a detail caption, so it
/// is reduced the same way (C1's rule, applied to the third constructor).
///
/// The negative experiment is `out.description = w.description` in `from_street`:
/// the two `contains` assertions then read the raw tags, which is what a home card
/// would have drawn.
#[test]
fn a_street_caption_is_cleaned_too() {
    let raw = r#"{"id":"151999999","title":"t","description":"一行目<br />二行目<br /><a href=\"https://www.pixiv.net/novel/show.php?id=27815393\">出典</a>"}"#;
    let row: StreetThumbWire = serde_json::from_str(raw).expect("street row");
    // The payload is half of this gate: a caption with no markup cannot fail it.
    assert!(
        row.description.contains("<br"),
        "the row must carry a break"
    );
    assert!(row.description.contains("<a "), "and a link");

    let card = Illust::from_street(row);
    assert!(
        !card.description.contains("<br"),
        "markup reached the domain: {}",
        card.description
    );
    assert!(
        !card.description.contains("<a "),
        "markup reached the domain: {}",
        card.description
    );
    assert_eq!(
        card.description,
        "一行目\n二行目\n出典 (https://www.pixiv.net/novel/show.php?id=27815393)"
    );
    // A card with no caption stays empty instead of gaining a stray space.
    let silent: StreetThumbWire =
        serde_json::from_str(r#"{"id":"2","title":"t"}"#).expect("street row");
    assert_eq!(Illust::from_street(silent).description, "");
}
