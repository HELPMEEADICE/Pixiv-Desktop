//! Live integration tests against the real Pixiv service.
//!
//! # Why these are separate from the offline suite
//!
//! Everything else in this workspace is asserted against captured fixtures and
//! never opens a socket. These tests do the opposite: they talk to
//! `www.pixiv.net` and `i.pximg.net` with a real session and check that the
//! client's reading of the responses still matches reality. A fixture proves the
//! parser handles *one known* payload; only the live suite proves Pixiv still
//! serves it.
//!
//! # How to enable
//!
//! They are inert unless a session is supplied, so a plain `cargo test` can
//! never pick them up by accident:
//!
//! ```text
//! PIXIV_LIVE_PHPSESSID=<cookie value> cargo test -p pixiv-api --test live -- --nocapture
//! ```
//!
//! The credential is only ever read from the environment — never written to a
//! file, a log line or an assertion message. Failures print the *classification*
//! of the error, not the request.
//!
//! # Reversibility
//!
//! The write tests mutate a real account, so each one follows
//! read -> mutate -> re-read -> restore -> re-read, and restores the state it
//! found. They refuse to run their mutation half when the target is already in
//! the desired state, and they never remove a bookmark they did not create.

use pixiv_api::{Client, ClientConfig, Session};
use pixiv_types::{BookmarkRestrict, PixivError};

/// The session cookie, or `None` when the live suite is not enabled.
fn session_cookie() -> Option<String> {
    std::env::var("PIXIV_LIVE_PHPSESSID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Build a client for the live service, or return `None` to skip.
fn live_client() -> Option<Client> {
    let cookie = session_cookie()?;
    let config = ClientConfig {
        // The public service is not a fixture: give it room, but never retry
        // forever.
        timeout: std::time::Duration::from_secs(30),
        max_retries: 2,
        min_interval: std::time::Duration::from_millis(350),
        ..ClientConfig::default()
    };
    let session = Session {
        phpsessid: Some(cookie),
        user_id: None,
        user_name: None,
        csrf_token: None,
    };
    Some(Client::new(config, session).expect("HTTP client builds"))
}

/// Skip with a visible reason rather than passing silently.
macro_rules! client_or_skip {
    () => {
        match live_client() {
            Some(client) => client,
            None => {
                eprintln!(
                    "skip: PIXIV_LIVE_PHPSESSID is not set, live suite disabled"
                );
                return;
            }
        }
    };
}

/// Print a failure without ever echoing the credential.
fn describe(error: &PixivError) -> String {
    match error {
        PixivError::Unauthorized => "Unauthorized (session rejected)".to_string(),
        PixivError::Http { status, message } => format!("Http {status}: {message}"),
        PixivError::Api(message) => format!("Api: {message}"),
        PixivError::NotFound(message) => format!("NotFound: {message}"),
        PixivError::Network(message) => format!("Network: {message}"),
        PixivError::Decode(message) => format!("Decode: {message}"),
        PixivError::RateLimited(after) => format!("RateLimited: {after:?}"),
        other => format!("{other}"),
    }
}

#[tokio::test]
async fn a_bare_cookie_is_not_a_session() {
    // Gated like every other test here: it needs one unauthenticated request to
    // prove the server rejects an invented cookie, and a plain `cargo test` must
    // not open a socket.
    let _ = client_or_skip!();
    let client = Client::new(
        ClientConfig::default(),
        Session {
            phpsessid: Some("00000000_not_a_real_session".to_string()),
            ..Session::default()
        },
    )
    .expect("client builds");
    assert!(client.session().is_authenticated());
    // ...but the session is not authenticated by the *server*. A client that only
    // checked "the cookie string is non-empty" would call this signed in.
    let outcome = client.account().await;
    assert!(
        matches!(outcome, Err(PixivError::Unauthorized)),
        "an invented cookie must be rejected as Unauthorized, got {:?}",
        outcome.as_ref().err().map(describe)
    );
}

#[tokio::test]
async fn live_session_resolves_to_a_real_account() {
    let client = client_or_skip!();
    let account = client
        .account()
        .await
        .expect("the supplied session must resolve to an account");
    eprintln!(
        "account: id={} name={:?} handle={:?} avatar={} token={}",
        account.id.get(),
        account.name,
        account.account,
        account.avatar.is_some(),
        account.csrf_token.is_some()
    );
    assert!(!account.id.is_zero(), "a signed-in account has a real id");
    assert!(!account.name.trim().is_empty(), "the account has a name");
    assert!(
        account.csrf_token.is_some(),
        "mutations are impossible without the CSRF token"
    );
    // The client must have adopted the identity it just learned.
    assert_eq!(client.session().user_id, Some(account.id));
    assert_eq!(client.session().user_name.as_deref(), Some(account.name.as_str()));
}

#[tokio::test]
async fn live_detail_pages_and_images_map_through() {
    let client = client_or_skip!();
    // Taken from the daily ranking so the id is always a currently public work.
    let rows = client
        .ranking(pixiv_types::RankingKind::Daily, 1)
        .await
        .expect("ranking loads");
    assert_eq!(rows.len(), 50, "ranking.php serves 50 rows per page");
    let candidate = rows[0].illust.id;

    let detail = client.illust_detail(candidate).await.expect("detail loads");
    assert_eq!(detail.id, candidate);
    assert!(!detail.title.is_empty());
    assert!(detail.urls.original.contains("i.pximg.net"));
    assert!(!detail.author.name.is_empty());

    let pages = client.illust_pages(candidate).await.expect("pages load");
    assert_eq!(pages.len() as u32, detail.page_count);
    assert!(pages[0].urls.original.contains("img-original"));

    // The image host refuses requests without the Referer, so a successful fetch
    // here is also a test of that header.
    let (bytes, content_type) = client
        .fetch_bytes(detail.urls.regular.as_str())
        .await
        .expect("image fetch must succeed with the Referer header");
    assert!(bytes.len() > 4096, "an image, not an error page");
    let is_jpeg = bytes.starts_with(&[0xFF, 0xD8, 0xFF]);
    let is_png = bytes.starts_with(&[0x89, b'P', b'N', b'G']);
    assert!(
        is_jpeg || is_png,
        "body must be decoded image bytes, got content-type {content_type:?} and magic {:02X?}",
        &bytes[..bytes.len().min(8)]
    );
}

#[tokio::test]
async fn live_search_returns_real_results_in_every_script() {
    let client = client_or_skip!();
    for word in ["初音ミク", "sky", "风景"] {
        let result = client
            .search(&pixiv_types::SearchQuery::new(word))
            .await
            .unwrap_or_else(|error| panic!("search {word:?} failed: {}", describe(&error)));
        assert!(
            !result.items.is_empty(),
            "search {word:?} returned no items"
        );
        assert!(result.total > 0);
        assert!(result.last_page >= 1);
        let first = &result.items[0];
        assert!(!first.id.is_zero());
        assert!(first.urls.grid().contains("i.pximg.net"));
        eprintln!(
            "{word:?}: {} items, total {}, last_page {}",
            result.items.len(),
            result.total,
            result.last_page
        );
    }
}

#[tokio::test]
async fn live_bookmark_listing_reads_the_signed_in_account() {
    let client = client_or_skip!();
    let account = client.account().await.expect("account");
    let public = client
        .bookmarks(account.id, false, 1)
        .await
        .expect("public bookmarks load");
    eprintln!(
        "public bookmarks: {} items, total {}",
        public.items.len(),
        public.total
    );
    for item in &public.items {
        assert!(!item.id.is_zero(), "every bookmark row carries an id");
    }
    // Private bookmarks are a different table and may legitimately be empty.
    let private = client
        .bookmarks(account.id, true, 1)
        .await
        .expect("private bookmarks load");
    eprintln!("private bookmarks: total {}", private.total);
}

#[tokio::test]
async fn live_bookmark_round_trip_is_verified_and_restored() {
    let client = client_or_skip!();
    client.account().await.expect("account");

    let rows = client
        .ranking(pixiv_types::RankingKind::Daily, 1)
        .await
        .expect("ranking loads");

    // Pick a work that is *not* bookmarked, so adding it is a reversible change
    // and removing it restores exactly the state we found.
    let mut target = None;
    for row in rows.iter().take(20) {
        let state = client
            .bookmark_state(row.illust.id)
            .await
            .expect("bookmark state loads");
        if !state.bookmarked {
            target = Some(row.illust.id);
            break;
        }
    }
    let Some(illust) = target else {
        eprintln!("skip: every candidate is already bookmarked; not touching the account");
        return;
    };

    let before = client.bookmark_state(illust).await.expect("state");
    assert!(!before.bookmarked);

    // add
    let after_add = client
        .set_bookmark(illust, true, BookmarkRestrict::Public)
        .await
        .unwrap_or_else(|error| panic!("bookmark add failed: {}", describe(&error)));
    assert!(
        after_add.bookmarked,
        "the server must report the bookmark after adding it"
    );
    assert_eq!(after_add.restrict, BookmarkRestrict::Public);
    eprintln!("added bookmark on illust {} (row {:?})", illust.get(), after_add.id);

    // move to private: the same route, with restrict=1
    let after_private = client
        .set_bookmark(illust, true, BookmarkRestrict::Private)
        .await
        .expect("bookmark restrict change");
    assert!(
        after_private.bookmarked && after_private.restrict == BookmarkRestrict::Private,
        "restrict change must be observable on the server"
    );

    // remove, and prove the account is back where it started
    let after_remove = client
        .set_bookmark(illust, false, BookmarkRestrict::Public)
        .await
        .expect("bookmark removal");
    assert!(
        !after_remove.bookmarked,
        "the server must report the bookmark gone"
    );
    let final_state = client.bookmark_state(illust).await.expect("state");
    assert_eq!(final_state, before, "account state must be restored");
}

#[tokio::test]
async fn live_follow_round_trip_is_verified_and_restored() {
    let client = client_or_skip!();
    client.account().await.expect("account");

    let rows = client
        .ranking(pixiv_types::RankingKind::Daily, 1)
        .await
        .expect("ranking loads");
    let artist = rows[0].illust.author.id;
    assert!(!artist.is_zero());

    let before = client.follow_state(artist).await.expect("follow state");
    if before {
        eprintln!("skip: already following {artist:?}; not touching the account");
        return;
    }

    client
        .follow(artist, true)
        .await
        .unwrap_or_else(|error| panic!("follow failed: {}", describe(&error)));
    assert!(
        client.follow_state(artist).await.expect("state"),
        "the follow must be visible on the server"
    );
    eprintln!("followed artist {}", artist.get());

    client
        .follow(artist, false)
        .await
        .unwrap_or_else(|error| panic!("unfollow failed: {}", describe(&error)));
    assert!(
        !client.follow_state(artist).await.expect("state"),
        "the unfollow must be visible on the server"
    );
}
