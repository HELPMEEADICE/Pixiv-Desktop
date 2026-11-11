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

/// `POST /ajax/illusts/bookmarks/add` or `.../delete`.
pub(crate) fn bookmark_mutation(add: bool) -> String {
    format!(
        "{WEB_HOST}/ajax/illusts/bookmarks/{}",
        if add { "add" } else { "delete" }
    )
}

/// `POST /ajax/user/follow` or `/ajax/user/unfollow`.
pub(crate) fn follow_mutation(follow: bool) -> String {
    format!(
        "{WEB_HOST}/ajax/user/{}",
        if follow { "follow" } else { "unfollow" }
    )
}

/// `GET /ajax/search/tags/{tag}`.
pub(crate) fn search_tags(tag: &str) -> String {
    format!(
        "{WEB_HOST}/ajax/search/tags/{}?lang={LANG}",
        urlencoding::encode(tag)
    )
}

/// Field pairs for a bookmark mutation.
///
/// The `add` flag only exists on the `add` endpoint; `delete` is identified by
/// its path alone. `rest` is required by both, because Pixiv stores the table
/// the bookmark belongs to.
pub(crate) fn bookmark_form(illust: Id, add: bool, private: bool) -> Vec<(&'static str, String)> {
    let restrict = if private {
        BookmarkRestrict::Private
    } else {
        BookmarkRestrict::Public
    };
    let mut fields = vec![("illust_id", illust.get().to_string())];
    if add {
        fields.push(("add", "1".to_string()));
    }
    fields.push(("rest", restrict.as_param().to_string()));
    fields
}

/// Field pairs for a follow mutation.
pub(crate) fn follow_form(user: Id) -> Vec<(&'static str, String)> {
    vec![("user_id", user.get().to_string())]
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
    fn mutation_paths_follow_the_add_delete_pairing() {
        assert_eq!(
            bookmark_mutation(true),
            "https://www.pixiv.net/ajax/illusts/bookmarks/add"
        );
        assert_eq!(
            bookmark_mutation(false),
            "https://www.pixiv.net/ajax/illusts/bookmarks/delete"
        );
        assert_eq!(
            follow_mutation(true),
            "https://www.pixiv.net/ajax/user/follow"
        );
        assert_eq!(
            follow_mutation(false),
            "https://www.pixiv.net/ajax/user/unfollow"
        );
    }

    #[test]
    fn bookmark_form_matches_the_captured_field_list() {
        assert_eq!(
            form_body(&bookmark_form(Id(99), true, false)),
            "illust_id=99&add=1&rest=public"
        );
        // Deleting carries no `add` field at all.
        assert_eq!(
            form_body(&bookmark_form(Id(99), false, true)),
            "illust_id=99&rest=private"
        );
    }

    #[test]
    fn follow_form_sends_the_user_id() {
        assert_eq!(form_body(&follow_form(Id(123))), "user_id=123");
    }

    #[test]
    fn form_body_preserves_order_and_encodes_values() {
        let fields = vec![("a", "1 2".to_string()), ("b", "x&y".to_string())];
        assert_eq!(form_body(&fields), "a=1%202&b=x%26y");
        assert_eq!(form_body(&[]), "");
    }
}
