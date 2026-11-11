//! HTTP transport for `pixiv.net`.
//!
//! # Scope
//!
//! This crate owns *transport only*: building URLs, attaching the session
//! cookie, the `i.pximg.net` Referer, retry/backoff, rate limiting, and mapping
//! wire JSON into [`pixiv_types::domain`] types. It never touches the UI, disk
//! caches or SQLite.
//!
//! # Authentication model
//!
//! The client authenticates with the *user's own browser session*. There is no
//! OAuth application to register: Pixiv does not issue client credentials to
//! third-party desktop clients, and embedding another application's
//! `client_secret` is not an option. What works — and what this crate
//! implements — is replaying the `PHPSESSID` cookie the user already obtained by
//! signing in to `pixiv.net` in their own browser.
//!
//! Consequences that shape the code:
//!
//! * Unauthenticated calls are legal and useful (detail, pages, search,
//!   ranking, artist profiles). They must keep working with no cookie set.
//! * Mutations (bookmark add/remove, follow) require the cookie. When it is
//!   missing we return [`PixivError::Unauthorized`] and the UI shows a sign-in
//!   prompt. We never fake a successful mutation locally.
//! * The cookie is a secret. It must never be logged, and never placed in an
//!   error message.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use pixiv_types::{
    Id, Illust, PIXIV_REFERER, PageMeta, PixivError, RankingKind, RankingRow, SearchQuery,
    SearchResult, UgoiraMeta, UserProfile,
};

mod endpoint;
mod mapping;

/// Credentials for the authenticated subset of the API.
#[derive(Clone, Default)]
pub struct Session {
    /// Value of the `PHPSESSID` cookie. Written with `secrecy`-style discipline:
    /// the `Debug` impl must redact it.
    pub phpsessid: Option<String>,
    /// The signed-in user's id, needed for `/ajax/user/{id}/...` calls.
    pub user_id: Option<Id>,
    /// Display name, purely for the UI.
    pub user_name: Option<String>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("phpsessid", &self.phpsessid.as_ref().map(|_| "<redacted>"))
            .field("user_id", &self.user_id)
            .field("user_name", &self.user_name)
            .finish()
    }
}

impl Session {
    /// True when a cookie is present and mutations may be attempted.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.phpsessid.as_deref().is_some_and(|v| !v.is_empty())
    }
}

/// Tuning knobs for the transport.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Per-request timeout.
    pub timeout: Duration,
    /// Extra attempts after the first, for retryable failures only.
    pub max_retries: u32,
    /// Minimum spacing between outbound requests, to stay polite to Pixiv.
    pub min_interval: Duration,
    /// Sent as `User-Agent`.
    pub user_agent: String,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(20),
            max_retries: 2,
            min_interval: Duration::from_millis(250),
            user_agent: concat!(
                "PixivDesktop/",
                env!("CARGO_PKG_VERSION"),
                " (Rust; desktop client)"
            )
            .to_string(),
        }
    }
}

/// The Pixiv HTTP client.
///
/// Cheap to clone: cloning shares one connection pool.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

/// Everything a clone shares. The session lives behind a lock so that signing in
/// takes effect for requests already in flight elsewhere, and so that clones
/// cannot drift apart.
struct Inner {
    http: reqwest::Client,
    session: RwLock<Session>,
    limiter: RateLimiter,
    config: ClientConfig,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the session: `Session`'s own `Debug` redacts, and this
        // impl additionally keeps the cookie out of the field list entirely.
        f.debug_struct("Client")
            .field("authenticated", &self.session().is_authenticated())
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

/// A single failed attempt: the classified error plus the server's own hint
/// about how long to wait, when it bothered to send one.
struct Failure {
    error: PixivError,
    retry_after: Option<Duration>,
}

impl Failure {
    fn new(error: PixivError) -> Self {
        Self {
            error,
            retry_after: None,
        }
    }

    fn with_hint(error: PixivError, retry_after: Option<Duration>) -> Self {
        Self { error, retry_after }
    }
}

/// Spacing gate shared by every clone of a [`Client`].
///
/// It hands out the earliest instant each caller may start at, so concurrent
/// callers queue behind one another instead of all sleeping the same interval
/// and then firing together.
#[derive(Debug)]
struct RateLimiter {
    min_interval: Duration,
    next_slot: Mutex<Option<Instant>>,
}

impl RateLimiter {
    fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            next_slot: Mutex::new(None),
        }
    }

    /// Claim the next free slot, returning when it may be used.
    fn reserve(&self) -> Instant {
        let mut slot = self.next_slot.lock();
        let now = Instant::now();
        let start = match *slot {
            Some(at) if at > now => at,
            _ => now,
        };
        *slot = Some(start.checked_add(self.min_interval).unwrap_or(start));
        start
    }

    /// Wait until this caller's slot is due.
    async fn wait(&self) {
        if self.min_interval.is_zero() {
            return;
        }
        let start = self.reserve();
        let now = Instant::now();
        if start > now {
            tokio::time::sleep(start - now).await;
        }
    }
}

fn network_error(error: &reqwest::Error) -> PixivError {
    if error.is_timeout() {
        PixivError::Network("请求超时".to_string())
    } else {
        // `reqwest`'s own message names the failure kind; it never contains
        // request headers, so the cookie cannot leak through here.
        PixivError::Network(error.to_string())
    }
}

/// Read the `Retry-After` hint from a response, if any.
fn retry_hint(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(mapping::parse_retry_after)
}

impl Client {
    /// Build a client. Returns an error only if the TLS backend cannot start.
    pub fn new(config: ClientConfig, session: Session) -> Result<Self, PixivError> {
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(config.user_agent.clone())
            .gzip(true)
            .build()
            .map_err(|e| PixivError::Network(format!("无法初始化 HTTP 客户端：{e}")))?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                session: RwLock::new(session),
                limiter: RateLimiter::new(config.min_interval),
                config,
            }),
        })
    }

    /// Replace the session (used after sign-in and sign-out).
    pub fn set_session(&self, session: Session) {
        *self.inner.session.write() = session;
    }

    /// Current session.
    #[must_use]
    pub fn session(&self) -> Session {
        self.inner.session.read().clone()
    }

    /// `GET /ajax/illust/{id}` — works without a session.
    pub async fn illust_detail(&self, id: Id) -> Result<Illust, PixivError> {
        let raw = self.get_text(&endpoint::illust_detail(id)).await?;
        mapping::parse_illust_detail(&raw)
    }

    /// `GET /ajax/illust/{id}/pages` — every page at full resolution.
    pub async fn illust_pages(&self, id: Id) -> Result<Vec<PageMeta>, PixivError> {
        let raw = self.get_text(&endpoint::illust_pages(id)).await?;
        mapping::parse_illust_pages(&raw)
    }

    /// `GET /ajax/illust/{id}/ugoira_meta`. Only valid for [`IllustKind::Ugoira`].
    pub async fn ugoira_meta(&self, id: Id) -> Result<UgoiraMeta, PixivError> {
        let raw = self.get_text(&endpoint::ugoira_meta(id)).await?;
        mapping::parse_ugoira_meta(&raw)
    }

    /// `GET /ajax/search/artworks/{word}`.
    pub async fn search(&self, query: &SearchQuery) -> Result<SearchResult, PixivError> {
        let raw = self.get_text(&endpoint::search(query)).await?;
        mapping::parse_search(&raw)
    }

    /// `GET /ranking.php?mode={kind}&format=json`.
    pub async fn ranking(
        &self,
        kind: RankingKind,
        page: u32,
    ) -> Result<Vec<RankingRow>, PixivError> {
        let raw = self.get_text(&endpoint::ranking(kind, page)).await?;
        mapping::parse_ranking(&raw)
    }

    /// `GET /ajax/user/{id}/profile/all` — enumerate an artist's work ids.
    pub async fn user_illust_ids(&self, user: Id) -> Result<Vec<Id>, PixivError> {
        let raw = self.get_text(&endpoint::user_profile_all(user)).await?;
        mapping::parse_user_illust_ids(&raw)
    }

    /// `GET /ajax/user/{id}` combined with the profile id map.
    pub async fn user_profile(&self, user: Id) -> Result<UserProfile, PixivError> {
        // The detail payload cannot count works, and the id map cannot name the
        // artist, so both requests are needed to build one profile.
        let ids = self.user_illust_ids(user).await?;
        let raw = self.get_text(&endpoint::user_detail(user)).await?;
        mapping::parse_user_profile(&raw, ids.len())
    }

    /// `GET /ajax/user/{id}/illusts/bookmarks` — requires a session.
    pub async fn bookmarks(
        &self,
        user: Id,
        private_only: bool,
        page: u32,
    ) -> Result<SearchResult, PixivError> {
        self.require_session()?;
        let raw = self
            .get_text(&endpoint::user_bookmarks(user, private_only, page))
            .await?;
        mapping::parse_bookmarks(&raw, endpoint::BOOKMARK_PAGE_SIZE)
    }

    /// `POST /ajax/illusts/bookmarks/add|delete` — requires a session.
    ///
    /// Must return [`PixivError::Unauthorized`] without a cookie, and must
    /// surface server rejection as an error rather than assuming success.
    pub async fn set_bookmark(
        &self,
        illust: Id,
        add: bool,
        private: bool,
    ) -> Result<(), PixivError> {
        self.require_session()?;
        let fields = endpoint::bookmark_form(illust, add, private);
        self.post_form(&endpoint::bookmark_mutation(add), &fields)
            .await
    }

    /// `POST /ajax/user/follow|unfollow` — requires a session.
    pub async fn follow(&self, user: Id, follow: bool) -> Result<(), PixivError> {
        self.require_session()?;
        let fields = endpoint::follow_form(user);
        self.post_form(&endpoint::follow_mutation(follow), &fields)
            .await
    }

    /// `GET /ajax/search/tags/{tag}` — translated tag metadata.
    pub async fn tag_translations(&self, tag: &str) -> Result<HashMap<String, String>, PixivError> {
        let raw = self.get_text(&endpoint::search_tags(tag)).await?;
        mapping::parse_tag_translations(&raw)
    }

    /// Fetch raw bytes (images, ugoira archives).
    ///
    /// Must send `Referer: https://www.pixiv.net/`; without it `i.pximg.net`
    /// answers 403. Returns the body plus its `Content-Type` when present.
    pub async fn fetch_bytes(&self, url: &str) -> Result<(Vec<u8>, Option<String>), PixivError> {
        let max_retries = self.inner.config.max_retries;
        let mut attempt = 0;
        loop {
            let failure = match self.attempt_bytes(url).await {
                Ok(body) => return Ok(body),
                Err(failure) => failure,
            };
            if !mapping::should_retry(attempt, max_retries, &failure.error) {
                return Err(failure.error);
            }
            tracing::debug!(
                attempt,
                url = %url,
                error = %failure.error,
                "重试 Pixiv 二进制请求"
            );
            tokio::time::sleep(mapping::backoff_delay(attempt, failure.retry_after)).await;
            attempt += 1;
        }
    }

    // -- transport --------------------------------------------------------

    /// Refuse to send anything when the endpoint needs credentials we do not
    /// have. This happens before a socket is opened: retrying a request that
    /// cannot be authorised only wastes the user's quota.
    fn require_session(&self) -> Result<(), PixivError> {
        if self.inner.session.read().is_authenticated() {
            Ok(())
        } else {
            Err(PixivError::Unauthorized)
        }
    }

    /// The cookie header to send with a request to `url`, if any.
    ///
    /// `PHPSESSID` belongs to `www.pixiv.net` and is scoped to it: image traffic
    /// goes to `i.pximg.net`, which is a different domain and needs only the
    /// Referer. Keeping the cookie off that host is why this decision is a
    /// function rather than an unconditional header.
    ///
    /// The value is written straight into the header and never into a log line
    /// or an error string.
    fn cookie_for(&self, url: &str) -> Option<String> {
        if !url.starts_with(endpoint::WEB_HOST) {
            return None;
        }
        let session = self.inner.session.read();
        match session.phpsessid.as_deref() {
            Some(cookie) if !cookie.trim().is_empty() => Some(format!("PHPSESSID={cookie}")),
            _ => None,
        }
    }

    /// Attach the session cookie when the target is the web host and we have one.
    fn authorize(&self, url: &str, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.cookie_for(url) {
            Some(cookie) => builder.header(reqwest::header::COOKIE, cookie),
            None => builder,
        }
    }

    async fn attempt_text(&self, url: &str) -> Result<String, Failure> {
        self.inner.limiter.wait().await;
        let response = self
            .authorize(url, self.inner.http.get(url))
            .send()
            .await
            .map_err(|e| Failure::new(network_error(&e)))?;

        let status = response.status().as_u16();
        let hint = if status == 429 {
            retry_hint(&response)
        } else {
            None
        };
        // `Response::text` needs the `charset` feature, which this crate does
        // not enable, so the body is decoded here instead.
        let bytes = response
            .bytes()
            .await
            .map_err(|e| Failure::with_hint(network_error(&e), hint))?;
        let body = String::from_utf8_lossy(&bytes).into_owned();

        if (200..300).contains(&status) {
            Ok(body)
        } else {
            Err(Failure::with_hint(
                mapping::map_status_error(status, &body),
                hint,
            ))
        }
    }

    async fn attempt_bytes(&self, url: &str) -> Result<(Vec<u8>, Option<String>), Failure> {
        self.inner.limiter.wait().await;
        let response = self
            .authorize(url, self.inner.http.get(url))
            // `i.pximg.net` answers 403 for any request without this.
            .header(reqwest::header::REFERER, PIXIV_REFERER)
            .send()
            .await
            .map_err(|e| Failure::new(network_error(&e)))?;

        let status = response.status().as_u16();
        let hint = if status == 429 {
            retry_hint(&response)
        } else {
            None
        };
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response
            .bytes()
            .await
            .map_err(|e| Failure::with_hint(network_error(&e), hint))?;

        if (200..300).contains(&status) {
            Ok((bytes.to_vec(), content_type))
        } else {
            let body = String::from_utf8_lossy(&bytes);
            Err(Failure::with_hint(
                mapping::map_status_error(status, &body),
                hint,
            ))
        }
    }

    async fn attempt_form(&self, url: &str, body: &str) -> Result<(), Failure> {
        self.inner.limiter.wait().await;
        let response = self
            .authorize(
                url,
                self.inner
                    .http
                    .post(url)
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(body.to_string()),
            )
            .send()
            .await
            .map_err(|e| Failure::new(network_error(&e)))?;

        let status = response.status().as_u16();
        let hint = if status == 429 {
            retry_hint(&response)
        } else {
            None
        };
        let bytes = response
            .bytes()
            .await
            .map_err(|e| Failure::with_hint(network_error(&e), hint))?;
        let text = String::from_utf8_lossy(&bytes);

        mapping::check_mutation(status, &text).map_err(|error| Failure::with_hint(error, hint))
    }

    /// `GET` returning the response body, retrying retryable failures.
    async fn get_text(&self, url: &str) -> Result<String, PixivError> {
        let max_retries = self.inner.config.max_retries;
        let mut attempt = 0;
        loop {
            let failure = match self.attempt_text(url).await {
                Ok(text) => return Ok(text),
                Err(failure) => failure,
            };
            if !mapping::should_retry(attempt, max_retries, &failure.error) {
                return Err(failure.error);
            }
            tracing::debug!(
                attempt,
                url = %url,
                error = %failure.error,
                "重试 Pixiv 请求"
            );
            tokio::time::sleep(mapping::backoff_delay(attempt, failure.retry_after)).await;
            attempt += 1;
        }
    }

    /// `POST` a form body, retrying retryable failures.
    ///
    /// Retries are limited to transport-level failures (network error, `429`,
    /// `5xx`); a `{"error": true}` answer is final and is reported as such.
    async fn post_form(&self, url: &str, fields: &[(&str, String)]) -> Result<(), PixivError> {
        let body = endpoint::form_body(fields);
        let max_retries = self.inner.config.max_retries;
        let mut attempt = 0;
        loop {
            let failure = match self.attempt_form(url, &body).await {
                Ok(()) => return Ok(()),
                Err(failure) => failure,
            };
            if !mapping::should_retry(attempt, max_retries, &failure.error) {
                return Err(failure.error);
            }
            tracing::debug!(
                attempt,
                url = %url,
                error = %failure.error,
                "重试 Pixiv 写操作"
            );
            tokio::time::sleep(mapping::backoff_delay(attempt, failure.retry_after)).await;
            attempt += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client that is built for real — `reqwest` needs no network to start —
    /// but that is never asked to speak to anything.
    fn offline_client() -> Client {
        Client::new(ClientConfig::default(), Session::default())
            .expect("the TLS backend must start without network access")
    }

    #[test]
    fn session_debug_redacts_the_cookie() {
        let session = Session {
            phpsessid: Some("SECRET-COOKIE-VALUE".to_string()),
            user_id: Some(Id(7)),
            user_name: Some("me".to_string()),
        };
        let rendered = format!("{session:?}");
        assert!(!rendered.contains("SECRET"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("me"), "{rendered}");
        assert!(session.is_authenticated());
        assert!(!Session::default().is_authenticated());
        assert!(
            !Session {
                phpsessid: Some(String::new()),
                ..Session::default()
            }
            .is_authenticated()
        );
    }

    #[test]
    fn client_debug_never_prints_the_cookie() {
        let client = Client::new(
            ClientConfig::default(),
            Session {
                phpsessid: Some("SECRET-COOKIE-VALUE".to_string()),
                ..Session::default()
            },
        )
        .expect("client builds");
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("SECRET"), "{rendered}");
        assert!(rendered.contains("authenticated"));
        assert!(client.session().is_authenticated());
    }

    #[test]
    fn set_session_replaces_credentials() {
        let client = offline_client();
        assert!(!client.session().is_authenticated());

        client.set_session(Session {
            phpsessid: Some("abc".to_string()),
            user_id: Some(Id(11)),
            user_name: None,
        });
        assert!(client.session().is_authenticated());
        assert_eq!(client.session().user_id, Some(Id(11)));

        client.set_session(Session::default());
        assert!(!client.session().is_authenticated());
        assert_eq!(client.session().user_id, None);
    }

    #[test]
    fn clones_share_one_session() {
        let client = offline_client();
        let clone = client.clone();
        clone.set_session(Session {
            phpsessid: Some("shared".to_string()),
            ..Session::default()
        });
        assert!(client.session().is_authenticated());
    }

    #[test]
    fn rate_limiter_spaces_consecutive_slots() {
        let limiter = RateLimiter::new(Duration::from_millis(250));
        let first = limiter.reserve();
        let second = limiter.reserve();
        let third = limiter.reserve();
        assert_eq!(second.duration_since(first), Duration::from_millis(250));
        assert_eq!(third.duration_since(second), Duration::from_millis(250));

        // A zero interval means "no spacing", not "rewind the clock".
        let free = RateLimiter::new(Duration::ZERO);
        let a = free.reserve();
        let b = free.reserve();
        assert!(b >= a);
    }

    #[test]
    fn rate_limiter_is_shared_across_clones() {
        let client = offline_client();
        let clone = client.clone();
        let first = client.inner.limiter.reserve();
        let second = clone.inner.limiter.reserve();
        assert_eq!(
            second.duration_since(first),
            ClientConfig::default().min_interval
        );
    }

    // -- credential gating -------------------------------------------------

    #[tokio::test]
    async fn mutations_without_a_session_are_unauthorized() {
        let client = offline_client();
        // Each of these must fail locally: no request is sent, so the test stays
        // independent of the network and of the user's real bookmarks.
        assert!(matches!(
            client.bookmarks(Id(1), false, 1).await,
            Err(PixivError::Unauthorized)
        ));
        assert!(matches!(
            client.bookmarks(Id(1), true, 2).await,
            Err(PixivError::Unauthorized)
        ));
        assert!(matches!(
            client.set_bookmark(Id(1), true, false).await,
            Err(PixivError::Unauthorized)
        ));
        assert!(matches!(
            client.set_bookmark(Id(1), false, true).await,
            Err(PixivError::Unauthorized)
        ));
        assert!(matches!(
            client.follow(Id(1), true).await,
            Err(PixivError::Unauthorized)
        ));
        assert!(matches!(
            client.follow(Id(1), false).await,
            Err(PixivError::Unauthorized)
        ));
    }

    #[test]
    fn the_session_cookie_only_goes_to_the_web_host() {
        let client = Client::new(
            ClientConfig::default(),
            Session {
                phpsessid: Some("abc123".to_string()),
                ..Session::default()
            },
        )
        .expect("client builds");
        assert_eq!(
            client.cookie_for("https://www.pixiv.net/ajax/illust/1?lang=zh"),
            Some("PHPSESSID=abc123".to_string())
        );
        // Images live on another domain and need the Referer, not the cookie.
        assert_eq!(
            client.cookie_for("https://i.pximg.net/img-original/img/x_p0.jpg"),
            None
        );
        assert_eq!(client.cookie_for("https://example.invalid/x"), None);

        let anonymous = offline_client();
        assert_eq!(
            anonymous.cookie_for("https://www.pixiv.net/ajax/illust/1"),
            None
        );
    }

    #[tokio::test]
    async fn a_blank_cookie_is_not_authentication() {
        let client = Client::new(
            ClientConfig::default(),
            Session {
                phpsessid: Some(String::new()),
                ..Session::default()
            },
        )
        .expect("client builds");
        assert!(matches!(
            client.set_bookmark(Id(1), true, false).await,
            Err(PixivError::Unauthorized)
        ));
        assert!(matches!(
            client.bookmarks(Id(1), false, 1).await,
            Err(PixivError::Unauthorized)
        ));
    }

    #[test]
    fn failures_carry_an_optional_server_hint() {
        let plain = Failure::new(PixivError::Unauthorized);
        assert!(plain.retry_after.is_none());
        let hinted = Failure::with_hint(
            PixivError::http(429, "slow down"),
            Some(Duration::from_secs(3)),
        );
        assert_eq!(hinted.retry_after, Some(Duration::from_secs(3)));
        assert!(hinted.error.is_retryable());
    }
}
