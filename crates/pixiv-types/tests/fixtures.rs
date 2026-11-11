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
