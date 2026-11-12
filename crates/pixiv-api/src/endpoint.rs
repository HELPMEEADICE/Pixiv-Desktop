//! Request construction: the exact URLs, query strings and form bodies Pixiv
//! expects.
//!
//! Everything here is pure and synchronous, and deliberately knows nothing about
//! `reqwest`, so the wire format can be asserted in tests without a socket. The
//! shapes encoded below were captured from the live site, including the details
//! that are easy to get wrong:
//!
//! * `ranking.php` lives at the site root, not under `ajax/`.
//! * `ajax/*` answers are wrapped in `{error, message, body}`, `ranking.php` is
//!   a bare document.
//! * the search endpoint repeats the search word twice: once in the path and
//!   once as the `word` query parameter.

use pixiv_types::domain::BookmarkRestrict;
use pixiv_types::{Id, RankingKind, SearchQuery};

/// Host serving the web front end and the `ajax` endpoints.
pub(crate) const WEB_HOST: &str = "https://www.pixiv.net";

/// Locale requested on every `ajax/*` call that accepts one.
pub(crate) const LANG: &str = "zh";

/// `limit` sent to the bookmarks endpoint, and therefore the divisor used to
/// derive `last_page` from `total`.
pub(crate) const BOOKMARK_PAGE_SIZE: u32 = 48;

/// `mode` sent to the artwork search endpoint. Pixiv requires it even when it
/// matches the default.
const SEARCH_MODE: &str = "all";

/// Bookmark listings are requested without a tag filter.
const BOOKMARK_TAG: &str = "";

/// `GET /ajax/user/self` — the signed-in account, or `userData: null`.
///
/// This is the only endpoint that both validates a cookie and names the account,
/// so it is what sign-in verification is built on. It answers `200` with
/// `"userData": null` when there is no session at all and `401` when the cookie
/// is present but dead — a distinction the UI needs to report honestly.
pub(crate) fn user_self() -> String {
    format!("{WEB_HOST}/ajax/user/self?lang={LANG}")
}

/// `GET /ajax/illust/{id}`.
pub(crate) fn illust_detail(id: Id) -> String {
    format!("{WEB_HOST}/ajax/illust/{}?lang={LANG}", id.get())
}

/// `GET /ajax/illust/{id}/pages`.
pub(crate) fn illust_pages(id: Id) -> String {
    format!("{WEB_HOST}/ajax/illust/{}/pages?lang={LANG}", id.get())
}

/// `GET /ajax/illust/{id}/ugoira_meta`.
pub(crate) fn ugoira_meta(id: Id) -> String {
    format!(
        "{WEB_HOST}/ajax/illust/{}/ugoira_meta?lang={LANG}",
        id.get()
    )
}

/// `GET /ajax/illust/{id}/bookmarkData` — the viewer's own bookmark row.
pub(crate) fn illust_bookmark_state(id: Id) -> String {
    format!(
        "{WEB_HOST}/ajax/illust/{}/bookmarkData?lang={LANG}",
        id.get()
    )
}

/// `GET /ajax/search/artworks/{word}`.
///
/// The word is percent-encoded once and used for both the path segment and the
/// `word` parameter, which is what keeps non-ASCII searches working.
pub(crate) fn search(query: &SearchQuery) -> String {
    let word = urlencoding::encode(&query.word);
    format!(
        "{WEB_HOST}/ajax/search/artworks/{word}?word={word}&order={order}&mode={SEARCH_MODE}&p={page}&s_mode={target}&type={kind}&lang={LANG}",
        order = query.sort.as_param(),
        page = query.page.max(1),
        target = query.target.as_param(),
        kind = query.kind.as_param(),
    )
}

/// `GET /ranking.php?mode={kind}&p={page}&format=json`.
pub(crate) fn ranking(kind: RankingKind, page: u32) -> String {
    format!(
        "{WEB_HOST}/ranking.php?mode={}&p={}&format=json",
        kind.mode(),
        page.max(1)
    )
}

/// `GET /ajax/user/{id}/profile/all`.
pub(crate) fn user_profile_all(user: Id) -> String {
    format!(
        "{WEB_HOST}/ajax/user/{}/profile/all?lang={LANG}",
        user.get()
    )
}

/// `GET /ajax/user/{id}?full=1`.
pub(crate) fn user_detail(user: Id) -> String {
    format!("{WEB_HOST}/ajax/user/{}?full=1&lang={LANG}", user.get())
}

/// `GET /ajax/user/{id}/illusts/bookmarks`.
pub(crate) fn user_bookmarks(user: Id, private_only: bool, page: u32) -> String {
    let page = page.max(1);
    let offset = u64::from(page - 1) * u64::from(BOOKMARK_PAGE_SIZE);
    let rest = if private_only {
        BookmarkRestrict::Private.as_list_filter()
    } else {
        BookmarkRestrict::Public.as_list_filter()
    };
    format!(
        "{WEB_HOST}/ajax/user/{}/illusts/bookmarks?tag={BOOKMARK_TAG}&offset={offset}&limit={BOOKMARK_PAGE_SIZE}&rest={rest}",
        user.get()
    )
}

/// `POST /ajax/illusts/bookmarks/add`.
pub(crate) fn bookmark_add() -> String {
    format!("{WEB_HOST}/ajax/illusts/bookmarks/add")
}

/// `POST /ajax/illusts/bookmarks/delete`.
///
/// Addressed by the *bookmark row* id, not the illustration id, so the id from
/// `bookmarkData` (or from the add response) has to be carried along.
pub(crate) fn bookmark_delete() -> String {
    format!("{WEB_HOST}/ajax/illusts/bookmarks/delete")
}

/// `POST /bookmark_add.php` — how the web client toggles a *user* follow.
///
/// Following an artist is not an `ajax` route: the modern web front end still
/// posts to the legacy script. `POST /ajax/user/follow` answers `404`, which is
/// why the follow button did nothing.
pub(crate) fn follow_add() -> String {
    format!("{WEB_HOST}/bookmark_add.php")
}

/// `POST /rpc_group_setting.php` — unfollow, also a legacy script.
pub(crate) fn follow_remove() -> String {
    format!("{WEB_HOST}/rpc_group_setting.php")
}

/// `GET /ajax/search/tags/{tag}`.
pub(crate) fn search_tags(tag: &str) -> String {
    format!(
        "{WEB_HOST}/ajax/search/tags/{}?lang={LANG}",
        urlencoding::encode(tag)
    )
}

/// JSON body for `POST /ajax/illusts/bookmarks/add`.
///
/// The endpoint takes a **JSON** object, not a form: `restrict` (`0` public,
/// `1` private) replaces the old `add`/`rest` pair. Sending
/// `illust_id=..&add=1&rest=public` as a form is rejected with
/// `422 不正なリクエストです。` — that was why no bookmark ever succeeded.
pub(crate) fn bookmark_add_body(illust: Id, restrict: BookmarkRestrict) -> String {
    // Built by hand so the field order and types are fixed and testable; the id
    // is a string on the wire even though it is numeric.
    format!(
        "{{\"illust_id\":\"{}\",\"restrict\":{},\"comment\":\"\",\"tags\":[]}}",
        illust.get(),
        restrict.as_flag()
    )
}

/// Field pairs for `POST /ajax/illusts/bookmarks/delete`.
pub(crate) fn bookmark_delete_form(bookmark_id: &str) -> Vec<(&'static str, String)> {
    vec![("bookmark_id", bookmark_id.to_string())]
}

/// Field pairs for `POST /bookmark_add.php` (follow a user).
pub(crate) fn follow_add_form(user: Id, restrict: BookmarkRestrict) -> Vec<(&'static str, String)> {
    vec![
        ("mode", "add".to_string()),
        ("type", "user".to_string()),
        ("user_id", user.get().to_string()),
        ("tag", String::new()),
        ("restrict", restrict.as_flag().to_string()),
        ("format", "json".to_string()),
    ]
}

/// Field pairs for `POST /rpc_group_setting.php` (unfollow a user).
pub(crate) fn follow_remove_form(user: Id) -> Vec<(&'static str, String)> {
    vec![
        ("mode", "del".to_string()),
        ("type", "bookuser".to_string()),
        ("id", user.get().to_string()),
    ]
}

/// Render `application/x-www-form-urlencoded` content from field pairs.
///
/// Built by hand rather than through a serialiser so the byte layout is fixed
/// and testable: field order is preserved and both keys and values are
/// percent-encoded.
pub(crate) fn form_body(fields: &[(&str, String)]) -> String {
    fields
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixiv_types::domain::{SearchTarget, SortOrder, TypeFilter};

    #[test]
    fn detail_urls_target_the_ajax_endpoints() {
        assert_eq!(
            illust_detail(Id(117_661_885)),
            "https://www.pixiv.net/ajax/illust/117661885?lang=zh"
        );
        assert_eq!(
            illust_pages(Id(42)),
            "https://www.pixiv.net/ajax/illust/42/pages?lang=zh"
        );
        assert_eq!(
            ugoira_meta(Id(151_939_293)),
            "https://www.pixiv.net/ajax/illust/151939293/ugoira_meta?lang=zh"
        );
    }

    #[test]
    fn search_encodes_the_word_and_carries_every_filter() {
        let query = SearchQuery::new("初音 ミク");
        let url = search(&query);

        // The raw word must never leak into the URL.
        assert!(!url.contains('初'), "word was not encoded: {url}");
        assert!(!url.contains(' '), "space was not encoded: {url}");
        assert!(
            url.contains("artworks/%E5%88%9D%E9%9F%B3%20%E3%83%9F%E3%82%AF?"),
            "path segment missing encoded word: {url}"
        );
        assert!(url.contains("word=%E5%88%9D%E9%9F%B3%20%E3%83%9F%E3%82%AF"));
        assert!(url.contains("order=date_d"));
        assert!(url.contains("mode=all"));
        assert!(url.contains("p=1"));
        assert!(url.contains("s_mode=s_tag_full"));
        assert!(url.contains("type=all"));
        assert!(url.contains("lang=zh"));
    }

    #[test]
    fn search_carries_the_selected_sort_target_and_type() {
        let query = SearchQuery::new("sky").with_page(3);
        let query = SearchQuery {
            sort: SortOrder::PopularDesc,
            target: SearchTarget::TitleCaption,
            kind: TypeFilter::Ugoira,
            ..query
        };
        let url = search(&query);
        assert!(url.contains("p=3"), "{url}");
        assert!(url.contains("order=popular_d"), "{url}");
        assert!(url.contains("s_mode=s_tc"), "{url}");
        assert!(url.contains("type=ugoira"), "{url}");
        // A page of zero would be rejected by Pixiv; it is clamped.
        assert!(search(&query.clone().with_page(0)).contains("p=1"));
    }

    #[test]
    fn ranking_url_is_not_under_ajax_and_asks_for_json() {
        let url = ranking(RankingKind::Weekly, 2);
        assert_eq!(
            url,
            "https://www.pixiv.net/ranking.php?mode=weekly&p=2&format=json"
        );
        assert!(!url.contains("/ajax/"));
        for kind in RankingKind::ALL {
            let url = ranking(kind, 1);
            assert!(url.contains(&format!("mode={}", kind.mode())), "{url}");
            assert!(url.contains("format=json"), "{url}");
        }
    }

    #[test]
    fn bookmarks_url_paginates_with_the_declared_limit() {
        let first = user_bookmarks(Id(7), false, 1);
        assert_eq!(
            first,
            "https://www.pixiv.net/ajax/user/7/illusts/bookmarks?tag=&offset=0&limit=48&rest=show"
        );
        let second = user_bookmarks(Id(7), true, 2);
        assert!(second.contains("offset=48"), "{second}");
        assert!(second.contains("limit=48"), "{second}");
        assert!(second.contains("rest=hide"), "{second}");
        // Page zero is normalised to the first page instead of a negative offset.
        assert!(user_bookmarks(Id(7), false, 0).contains("offset=0"));
    }

    #[test]
    fn user_urls_carry_the_full_profile_flag() {
        assert_eq!(
            user_profile_all(Id(11)),
            "https://www.pixiv.net/ajax/user/11/profile/all?lang=zh"
        );
        assert_eq!(
            user_detail(Id(11)),
            "https://www.pixiv.net/ajax/user/11?full=1&lang=zh"
        );
    }

    #[test]
    fn tag_search_encodes_the_tag() {
        let url = search_tags("初音ミク/ボカロ");
        assert!(url.starts_with("https://www.pixiv.net/ajax/search/tags/"));
        assert!(url.contains("%E5%88%9D%E9%9F%B3%E3%83%9F%E3%82%AF"));
        assert!(url.contains("%2F"));
        assert!(url.contains("lang=zh"));
        // The path segment must stay a single segment: no unescaped separator.
        let segment = url
            .split("/ajax/search/tags/")
            .nth(1)
            .and_then(|rest| rest.split('?').next())
            .expect("tag path present");
        assert!(!segment.contains('/'), "unescaped slash in tag path: {url}");
    }

    #[test]
    fn bookmark_mutations_use_the_ajax_routes_that_answer() {
        assert_eq!(
            bookmark_add(),
            "https://www.pixiv.net/ajax/illusts/bookmarks/add"
        );
        assert_eq!(
            bookmark_delete(),
            "https://www.pixiv.net/ajax/illusts/bookmarks/delete"
        );
    }

    #[test]
    fn identity_and_bookmark_state_target_their_ajax_routes() {
        assert_eq!(user_self(), "https://www.pixiv.net/ajax/user/self?lang=zh");
        assert_eq!(
            illust_bookmark_state(Id(42)),
            "https://www.pixiv.net/ajax/illust/42/bookmarkData?lang=zh"
        );
    }

    /// Following is a legacy script, not an `ajax` route. `POST /ajax/user/follow`
    /// answers `404`, which is what made the follow button inert.
    #[test]
    fn follow_mutations_target_the_legacy_scripts() {
        assert_eq!(follow_add(), "https://www.pixiv.net/bookmark_add.php");
        assert_eq!(
            follow_remove(),
            "https://www.pixiv.net/rpc_group_setting.php"
        );
    }

    #[test]
    fn bookmark_add_body_is_the_json_the_live_endpoint_accepts() {
        assert_eq!(
            bookmark_add_body(Id(99), BookmarkRestrict::Public),
            r#"{"illust_id":"99","restrict":0,"comment":"","tags":[]}"#
        );
        assert_eq!(
            bookmark_add_body(Id(99), BookmarkRestrict::Private),
            r#"{"illust_id":"99","restrict":1,"comment":"","tags":[]}"#
        );
        // The body must be valid JSON, not a form string.
        let parsed: serde_json::Value =
            serde_json::from_str(&bookmark_add_body(Id(7), BookmarkRestrict::Private))
                .expect("body is JSON");
        assert_eq!(parsed["illust_id"], "7");
        assert_eq!(parsed["restrict"], 1);
        assert_eq!(parsed["comment"], "");
        assert!(parsed["tags"].is_array());
    }

    #[test]
    fn bookmark_delete_is_addressed_by_the_bookmark_row() {
        assert_eq!(
            form_body(&bookmark_delete_form("39714494245")),
            "bookmark_id=39714494245"
        );
    }

    #[test]
    fn follow_forms_match_the_verified_script_payloads() {
        assert_eq!(
            form_body(&follow_add_form(Id(123), BookmarkRestrict::Public)),
            "mode=add&type=user&user_id=123&tag=&restrict=0&format=json"
        );
        assert_eq!(
            form_body(&follow_remove_form(Id(123))),
            "mode=del&type=bookuser&id=123"
        );
    }

    #[test]
    fn form_body_preserves_order_and_encodes_values() {
        let fields = vec![("a", "1 2".to_string()), ("b", "x&y".to_string())];
        assert_eq!(form_body(&fields), "a=1%202&b=x%26y");
        assert_eq!(form_body(&[]), "");
    }
}
