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
    Account, BookmarkRestrict, BookmarkState, DiscoveryBatch, Id, Illust, LatestFeed,
    PIXIV_REFERER, PageMeta, PixivError, RankingKind, RankingRow, RecommendTag, RecommendUser,
    SearchQuery, SearchResult, StreetFeed, Suggestion, UgoiraMeta, UserProfile,
};

mod endpoint;
mod mapping;

/// Credentials for the authenticated subset of the API.
#[derive(Clone, Default)]
pub struct Session {
    /// Value of the `PHPSESSID` cookie. Written with `secrecy`-style discipline:
    /// the `Debug` impl must redact it.
    pub phpsessid: Option<String>,
    /// The signed-in user's id, needed for `/ajax/user/{id}/...` calls and
    /// echoed as `x-user-id` on every request the web client makes.
    pub user_id: Option<Id>,
    /// Display name, purely for the UI.
    pub user_name: Option<String>,
    /// CSRF token from `/ajax/user/self`. Every state-changing request must
    /// echo it in `x-csrf-token` or Pixiv answers `422 不正なリクエストです。`.
    ///
    /// It is refreshed on demand rather than trusted forever: it is session
    /// scoped, so a stale copy would turn every mutation into a 422.
    pub csrf_token: Option<String>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("phpsessid", &self.phpsessid.as_ref().map(|_| "<redacted>"))
            .field("user_id", &self.user_id)
            .field("user_name", &self.user_name)
            .field(
                "csrf_token",
                &self.csrf_token.as_ref().map(|_| "<redacted>"),
            )
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
    /// Hard ceiling on one buffered response body, in bytes.
    ///
    /// Checked against a declared `Content-Length` before a byte of body is read
    /// and against the running total on every chunk, because `Response::bytes()`
    /// buffers the whole document before the caller can look at the status: a
    /// Cloudflare interstitial or an endless `chunked` reply would otherwise be
    /// held in memory in full. 32 MiB is roughly 60 times the largest JSON
    /// envelope this client asks for, and small enough that a hostile answer
    /// cannot become the process's memory problem.
    pub max_body_bytes: u64,
    /// Ceiling for an image or archive body.
    ///
    /// A separate, larger number because these are legitimately bigger documents
    /// than a JSON envelope: an original-resolution page or an ugoira archive
    /// runs to tens of megabytes, and refusing those would break the viewer for
    /// the sake of a limit aimed at text.
    pub max_image_bytes: u64,
}

/// 32 MiB: see [`ClientConfig::max_body_bytes`].
pub const DEFAULT_MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;
/// 64 MiB: see [`ClientConfig::max_image_bytes`].
pub const DEFAULT_MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(20),
            max_retries: 2,
            min_interval: Duration::from_millis(250),
            user_agent: DEFAULT_USER_AGENT.to_string(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
        }
    }
}

/// The User-Agent every request carries.
///
/// This is deliberately a stock desktop Chrome string. pixiv.net sits behind a
/// bot-protection edge that answers a non-browser User-Agent with a 403
/// Cloudflare interstitial ("Just a moment...") before Pixiv itself sees the
/// request -- verified: the same cookie and URL return 200 with a browser UA and
/// 403 with a "PixivDesktop/..." one. The client identifies itself in the UI,
/// not by sabotaging its own requests.
pub const DEFAULT_USER_AGENT: &str = pixiv_types::BROWSER_USER_AGENT;

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
    /// A scripted stand-in for the network, armed only by this crate's tests.
    ///
    /// `reqwest` offers no seam a test can substitute, and "one profile is one
    /// request per document" has to be asserted rather than asserted-in-a-comment.
    /// When this is `Some`, every attempt answers from the table instead of
    /// opening a socket; production leaves it `None` and the field does not exist.
    #[cfg(test)]
    fake: Mutex<Option<FakeWire>>,
}

/// A table of canned replies, standing in for `www.pixiv.net`.
#[cfg(test)]
#[derive(Debug, Default)]
struct FakeWire {
    /// `(url prefix, status, content type, body)`, matched in order.
    replies: Vec<(String, u16, Option<String>, String)>,
    /// Every URL this client asked for, in the order it asked.
    seen: Vec<String>,
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
    /// HTTP status of the attempt, when the failure came from one.
    ///
    /// Kept because [`PixivError`] deliberately flattens a flagged `4xx` into
    /// [`PixivError::Api`], and the one place that needs the raw code is CSRF
    /// recovery: Pixiv answers a stale token with `422`, which is worth exactly
    /// one retry after refreshing the token.
    status: Option<u16>,
}

impl Failure {
    fn new(error: PixivError) -> Self {
        Self {
            error,
            retry_after: None,
            status: None,
        }
    }

    fn with_hint(error: PixivError, retry_after: Option<Duration>) -> Self {
        Self {
            error,
            retry_after,
            status: None,
        }
    }

    fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// True when Pixiv refused the request's CSRF token.
    fn is_csrf_rejection(&self) -> bool {
        self.status == Some(422)
    }
}

/// A failed mutation, and whether Pixiv may have applied it anyway.
///
/// A `POST` is not `GET`: replaying one is a second mutation, so the caller has to
/// know whether the state it is about to describe could have moved. That is what
/// [`Client::set_bookmark`] re-reads before it reports a failure.
struct MutationFailure {
    error: PixivError,
    /// `true` when the request went out more than once, or when no reply was ever
    /// seen. Both mean the server state is unknown.
    possibly_applied: bool,
}

impl MutationFailure {
    /// A rejection that proves the mutation did not happen: Pixiv answered, and
    /// the answer was `{"error": true}` or a status that cannot have applied.
    fn definite(error: PixivError) -> Self {
        Self {
            error,
            possibly_applied: false,
        }
    }
}

/// May a failed `POST` be sent again?
///
/// Only when the server itself asked for it: a `429` or `503` that also carried a
/// `Retry-After`. A bare transport error is never replayed. A bookmark that
/// reached Pixiv and lost its reply is exactly the case where a second attempt
/// changes the answer — the delete route is addressed by bookmark row id, so the
/// replay answers "row gone" and the caller reported a failure for a mutation
/// that had in fact succeeded.
fn should_retry_mutation(attempt: u32, max_retries: u32, failure: &Failure) -> bool {
    attempt < max_retries
        && matches!(failure.status, Some(429) | Some(503))
        && failure.retry_after.is_some()
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

/// Read the `Retry-After` hint a failed response carries, if any.
///
/// Honoured for every failed status, not only for `429`: a `503` carrying
/// `Retry-After` means the same thing, and reading the header only for `429`
/// made the client retry into a service that had just said when to come back.
/// A 2xx answer has no wait to honour, so it never yields a hint.
fn retry_hint_for(status: u16, headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    if (200..300).contains(&status) {
        return None;
    }
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(mapping::parse_retry_after)
}

/// The arithmetic behind the response-size ceiling.
///
/// Split out from the socket so the decision is a unit test rather than a claim:
/// [`BodyBudget::check_declared`] runs before the first body byte is read, and
/// [`BodyBudget::take`] runs on every chunk, so a document that lies about its
/// length (or sends none) still stops at the ceiling instead of after it.
#[derive(Debug)]
struct BodyBudget {
    limit: u64,
    taken: u64,
}

impl BodyBudget {
    fn new(limit: u64) -> Self {
        Self { limit, taken: 0 }
    }

    /// Refuse a declared `Content-Length` above the ceiling, before reading.
    ///
    /// Nothing has been buffered at this point, which is the whole point: the
    /// check that used to be impossible until `bytes()` had already paid for the
    /// document.
    fn check_declared(&self, declared: Option<u64>) -> Result<(), PixivError> {
        match declared {
            Some(len) if len > self.limit => Err(self.over()),
            _ => Ok(()),
        }
    }

    /// Count one chunk, failing the moment the running total crosses the ceiling.
    ///
    /// This is not a `Content-Length` check repeated: `gzip` decoding means the
    /// bytes that arrive are not the bytes that were declared, so the only
    /// trustworthy number is the count of what has actually been handed over.
    fn take(&mut self, len: usize) -> Result<(), PixivError> {
        let len = u64::try_from(len).unwrap_or(u64::MAX);
        let taken = self.taken.saturating_add(len);
        if taken > self.limit {
            return Err(self.over());
        }
        self.taken = taken;
        Ok(())
    }

    /// The error a refused body produces.
    ///
    /// Not [`PixivError::Network`]: that is retryable, and re-requesting a
    /// document that is oversized by construction just spends the user's quota to
    /// arrive at the same answer.
    fn over(&self) -> PixivError {
        PixivError::Other(format!("响应体超过 {} 字节上限，已停止读取", self.limit))
    }
}

/// The `Content-Length` a response declared, when it declared a usable one.
fn declared_length(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// Buffer one response body under `limit` bytes.
///
/// The declaration is checked first and every chunk after it, so an oversized
/// answer costs one header parse when it is honest about its size and one chunk
/// when it is not.
async fn read_body(
    mut response: reqwest::Response,
    limit: u64,
    hint: Option<Duration>,
) -> Result<Vec<u8>, Failure> {
    let declared = declared_length(response.headers());
    let mut budget = BodyBudget::new(limit);
    budget
        .check_declared(declared)
        .map_err(|error| Failure::with_hint(error, hint))?;
    // Reserve only what was declared and admitted: a declared length is the one
    // trustworthy hint about allocation, and it is already bounded by `limit`.
    let capacity = usize::try_from(declared.unwrap_or(0).min(limit)).unwrap_or(0);
    let mut body = Vec::with_capacity(capacity);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Failure::with_hint(network_error(&e), hint))?
    {
        budget
            .take(chunk.len())
            .map_err(|error| Failure::with_hint(error, hint))?;
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Turn a buffered text response into the caller's answer.
fn finish_text(status: u16, body: String, hint: Option<Duration>) -> Result<String, Failure> {
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(Failure::with_hint(
            mapping::map_status_error_with_hint(status, &body, hint),
            hint,
        ))
    }
}

/// Turn a buffered binary response into the caller's answer.
fn finish_bytes(
    status: u16,
    body: Vec<u8>,
    content_type: Option<String>,
    hint: Option<Duration>,
) -> Result<(Vec<u8>, Option<String>), Failure> {
    if (200..300).contains(&status) {
        Ok((body, content_type))
    } else {
        let text = String::from_utf8_lossy(&body);
        Err(Failure::with_hint(
            mapping::map_status_error_with_hint(status, &text, hint),
            hint,
        ))
    }
}

/// Turn a buffered `POST` answer into the caller's answer.
///
/// The raw status is kept on the failure because a `422` is the one code that
/// means "your CSRF token went stale" rather than "the request was wrong".
fn finish_post(status: u16, text: String, hint: Option<Duration>) -> Result<String, Failure> {
    if (200..300).contains(&status) {
        Ok(text)
    } else {
        Err(Failure::with_hint(
            mapping::map_status_error_with_hint(status, &text, hint),
            hint,
        )
        .with_status(status))
    }
}

impl Client {
    /// Build a client. Returns an error only if the TLS backend cannot start.
    pub fn new(config: ClientConfig, session: Session) -> Result<Self, PixivError> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json, text/plain, */*"),
        );
        headers.insert(
            reqwest::header::ACCEPT_LANGUAGE,
            reqwest::header::HeaderValue::from_static("zh-CN,zh;q=0.9,ja;q=0.8,en;q=0.7"),
        );
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(config.user_agent.clone())
            .default_headers(headers)
            .gzip(true)
            .build()
            .map_err(|e| PixivError::Network(format!("无法初始化 HTTP 客户端：{e}")))?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                session: RwLock::new(session),
                limiter: RateLimiter::new(config.min_interval),
                config,
                #[cfg(test)]
                fake: Mutex::new(None),
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
    ///
    /// One document, one request: the id map is the whole answer.
    pub async fn user_illust_ids(&self, user: Id) -> Result<Vec<Id>, PixivError> {
        let raw = self.get_text(&endpoint::user_profile_all(user)).await?;
        mapping::parse_user_illust_ids(&raw)
    }

    /// One artist, one request per document: the profile and its work ids.
    ///
    /// A profile page needs two Pixiv documents — `/ajax/user/{id}/profile/all` for
    /// the work counts *and* the id list, and `/ajax/user/{id}` for the name,
    /// avatar and comment. That is two requests, and it used to be four: the
    /// caller ran `user_profile` and `user_illust_ids` side by side, and each of
    /// them downloaded the same 94 KB id map, so one artist cost two fetches of
    /// one document, serialised through the global rate limiter.
    ///
    /// Here the id map is fetched once and decoded once
    /// ([`mapping::parse_profile_all`]), and both halves of the answer are derived
    /// from it. The follower count is a third request and only for the signed-in
    /// account, which is the only one Pixiv lets a session read.
    ///
    /// The two fetches still run concurrently: they are independent, and doing
    /// them in sequence doubled the time the profile card spent on a spinner.
    pub async fn profile_bundle(&self, user: Id) -> Result<(UserProfile, Vec<Id>), PixivError> {
        let profile_url = endpoint::user_profile_all(user);
        let detail_url = endpoint::user_detail(user);
        let (all, raw) = tokio::join!(self.get_text(&profile_url), self.get_text(&detail_url));
        // The two counts come from the id map, which keeps illustrations and
        // manga in separate dictionaries: a union would report one merged
        // total, which is not what the profile card promises.
        let (illust_count, manga_count, ids) = mapping::parse_profile_all(&all?)?;
        let mut profile = mapping::parse_user_profile(&raw?, illust_count, manga_count)?;
        if self.session().user_id == Some(user) {
            profile.followers = self.user_follower_count(user).await.unwrap_or(None);
        }
        Ok((profile, ids))
    }

    /// `GET /ajax/user/{id}` combined with the profile id map, discarding the ids.
    ///
    /// Kept because a caller that only wants the card should not have to destructure
    /// a tuple; it is exactly [`Client::profile_bundle`] with half the answer
    /// dropped, so it costs the same two requests and never re-fetches the map.
    pub async fn user_profile(&self, user: Id) -> Result<UserProfile, PixivError> {
        self.profile_bundle(user).await.map(|(profile, _)| profile)
    }

    /// GET /ajax/street/for_you -- the personalised home feed.
    ///
    /// Sending the session is what makes the answer personalised; the endpoint
    /// still answers without one, so the caller decides whether a signed-out
    /// result may be presented as a recommendation.
    pub async fn home_feed(&self) -> Result<StreetFeed, PixivError> {
        let raw = self.get_text(&endpoint::street_for_you()).await?;
        mapping::parse_street_for_you(&raw)
    }

    /// GET /ajax/street/latest -- the newest works row.
    pub async fn latest_works(&self) -> Result<LatestFeed, PixivError> {
        let raw = self.get_text(&endpoint::street_latest()).await?;
        mapping::parse_street_latest(&raw)
    }

    /// GET /ajax/street/recommend_tags.
    pub async fn recommend_tags(&self) -> Result<Vec<RecommendTag>, PixivError> {
        let raw = self.get_text(&endpoint::street_recommend_tags()).await?;
        mapping::parse_street_recommend_tags(&raw)
    }

    /// GET /ajax/street/sub -- recommended artists.
    pub async fn recommend_users(&self) -> Result<Vec<RecommendUser>, PixivError> {
        let raw = self.get_text(&endpoint::street_sub()).await?;
        mapping::parse_street_sub(&raw)
    }

    /// GET /ajax/discovery/artworks -- a batch of personalised recommendations.
    ///
    /// mode is all, safe or r18. The endpoint is not paginated: each call returns
    /// a fresh ranked batch, and the client is responsible for de-duplicating
    /// across calls.
    pub async fn discovery_artworks(
        &self,
        mode: &str,
        limit: u32,
    ) -> Result<DiscoveryBatch, PixivError> {
        let raw = self
            .get_text(&endpoint::discovery_artworks(mode, limit))
            .await?;
        mapping::parse_discovery_artworks(&raw)
    }

    /// GET /ajax/illust/discovery -- ten fully described recommendations.
    pub async fn illust_discovery(&self, mode: &str) -> Result<Vec<Illust>, PixivError> {
        let raw = self.get_text(&endpoint::illust_discovery(mode)).await?;
        mapping::parse_illust_discovery(&raw)
    }

    /// GET /ajax/discovery/users -- recommended artists.
    pub async fn discovery_users(&self, mode: &str) -> Result<Vec<RecommendUser>, PixivError> {
        let raw = self.get_text(&endpoint::discovery_users(mode)).await?;
        mapping::parse_discovery_users(&raw)
    }

    /// Search suggestions, joined from the two endpoints the site itself uses.
    ///
    /// /rpc/cps.php supplies the official ordering and the translations;
    /// /ajax/tags/suggest_by_word supplies the work counts. Losing either is not
    /// fatal: whichever answered still produces a usable list.
    pub async fn suggestions(
        &self,
        word: &str,
        limit: usize,
    ) -> Result<Vec<Suggestion>, PixivError> {
        let word = word.trim();
        if word.is_empty() {
            return Ok(Vec::new());
        }
        let completion_url = endpoint::search_completion(word);
        let tag_url = endpoint::tag_suggestion(word);
        let (completion, tags) =
            tokio::join!(self.get_text(&completion_url), self.get_text(&tag_url));
        let completion = completion
            .ok()
            .and_then(|raw| mapping::parse_completion(&raw).ok());
        let tags = tags
            .ok()
            .and_then(|raw| mapping::parse_tag_suggestions(&raw).ok());
        if completion.is_none() && tags.is_none() {
            return Err(PixivError::Network("联想服务暂时不可用".to_string()));
        }
        Ok(mapping::suggestions(completion, tags, limit))
    }

    /// GET /ajax/user/{id}/illusts?ids[]=... -- batch details.
    ///
    /// Falls back to the profile route when the session-gated one refuses, so a
    /// signed-out client can still open a profile.
    pub async fn user_works(&self, user: Id, ids: &[Id]) -> Result<Vec<Illust>, PixivError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if self.session().is_authenticated()
            && let Ok(raw) = self
                .get_text(&endpoint::user_illusts_batch(user, ids))
                .await
            && let Ok(works) = mapping::parse_user_illusts_batch(&raw)
            && !works.is_empty()
        {
            return Ok(works);
        }
        let raw = self
            .get_text(&endpoint::user_profile_illusts(
                user,
                ids,
                "illustManga",
                false,
            ))
            .await?;
        mapping::parse_profile_illusts(&raw)
    }

    /// Follower count of the signed-in account, which is the only one readable.
    pub async fn user_follower_count(&self, user: Id) -> Result<Option<u64>, PixivError> {
        if self.session().user_id != Some(user) {
            return Ok(None);
        }
        let raw = self
            .get_text(&endpoint::user_followers(user, 0, 1, true))
            .await?;
        mapping::parse_follower_total(&raw).map(Some)
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

    /// `GET /ajax/user/self` — the account behind this session.
    ///
    /// The only honest sign-in check: it proves the cookie is live *and* returns
    /// the id and display name, which a bare "the cookie is non-empty" test
    /// cannot. It also yields the CSRF token, so every caller that needs to
    /// mutate something calls this first.
    pub async fn account(&self) -> Result<Account, PixivError> {
        let raw = self.get_text(&endpoint::user_self()).await?;
        let account = mapping::parse_account(&raw)?;
        {
            let mut session = self.inner.session.write();
            session.user_id = Some(account.id);
            session.user_name = Some(account.name.clone());
            if account.csrf_token.is_some() {
                session.csrf_token = account.csrf_token.clone();
            }
        }
        Ok(account)
    }

    /// Re-read the account without surfacing the result, used to recover a stale
    /// CSRF token.
    async fn refresh_identity(&self) -> Result<(), PixivError> {
        self.account().await.map(|_| ())
    }

    /// Make sure a CSRF token is available before a mutation.
    async fn ensure_csrf(&self) -> Result<(), PixivError> {
        self.require_session()?;
        if self
            .inner
            .session
            .read()
            .csrf_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty())
        {
            return Ok(());
        }
        self.refresh_identity().await
    }

    /// `GET /ajax/illust/{id}/bookmarkData` — the viewer's own bookmark row.
    pub async fn bookmark_state(&self, illust: Id) -> Result<BookmarkState, PixivError> {
        let raw = self
            .get_text(&endpoint::illust_bookmark_state(illust))
            .await?;
        mapping::parse_bookmark_state(&raw)
    }

    /// `GET /ajax/user/{id}?full=1` — is this artist followed?
    pub async fn follow_state(&self, user: Id) -> Result<bool, PixivError> {
        let raw = self.get_text(&endpoint::user_detail(user)).await?;
        mapping::parse_follow_state(&raw)
    }

    /// Add or remove a bookmark — requires a session.
    ///
    /// The two directions are not symmetric and both details are easy to get
    /// wrong:
    ///
    /// * **add** takes a JSON body `{illust_id, restrict, comment, tags}`;
    ///   a form body is rejected with `422`.
    /// * **delete** is addressed by the *bookmark row* id via `bookmark_id`, not
    ///   by the illustration id — so the row id has to be read first.
    ///
    /// Returns the state the server reports *after* the mutation, not an
    /// assumption: a `200` on the POST is not proof the bookmark moved.
    pub async fn set_bookmark(
        &self,
        illust: Id,
        add: bool,
        restrict: BookmarkRestrict,
    ) -> Result<BookmarkState, PixivError> {
        self.require_session()?;
        // Both directions are wrapped so the one failure that needs a decision —
        // "the POST may have been applied and we never saw its reply" — can make
        // it, instead of reporting a failure for a mutation that succeeded.
        let mutation = if add {
            self.ensure_csrf().await?;
            let body = endpoint::bookmark_add_body(illust, restrict);
            match self.post_json(&endpoint::bookmark_add(), body).await {
                // A 2xx is not success on its own: these routes answer a 200
                // envelope whose `error` flag is the authoritative verdict.
                Ok(reply) => {
                    mapping::check_mutation(200, &reply).map_err(MutationFailure::definite)
                }
                Err(failure) => Err(failure),
            }
        } else {
            let current = self.bookmark_state(illust).await?;
            if !current.bookmarked {
                // Already absent: removing it again is a no-op, not an error.
                return Ok(current);
            }
            let Some(row) = current.id.clone() else {
                return Err(PixivError::Api(
                    "服务端报告已收藏但没有返回收藏条目 id，无法安全取消".to_string(),
                ));
            };
            self.ensure_csrf().await?;
            let fields = endpoint::bookmark_delete_form(&row);
            match self.post_form(&endpoint::bookmark_delete(), &fields).await {
                Ok(reply) => {
                    mapping::check_mutation(200, &reply).map_err(MutationFailure::definite)
                }
                Err(failure) => Err(failure),
            }
        };
        if let Err(failure) = mutation {
            if failure.possibly_applied {
                // The request was retried, or its reply was never seen. Read the
                // server instead of guessing: if the state already says what the
                // caller asked for, this is a success whose reply was lost.
                if let Ok(state) = self.bookmark_state(illust).await {
                    if state.bookmarked == add {
                        tracing::debug!(
                            illust = illust.0,
                            add,
                            "写操作报了错，但服务端状态已是目标状态；按成功处理"
                        );
                        return Ok(state);
                    }
                }
            }
            return Err(failure.error);
        }
        // Confirm against the server instead of trusting the POST.
        self.bookmark_state(illust).await
    }

    /// Follow or unfollow an artist — requires a session.
    ///
    /// Both directions live on legacy scripts, and each reports success its own
    /// way: `bookmark_add.php` answers an empty JSON array, and
    /// `rpc_group_setting.php` echoes the user it acted on.
    pub async fn follow(&self, user: Id, follow: bool) -> Result<(), PixivError> {
        self.require_session()?;
        self.ensure_csrf().await?;
        let mutation = if follow {
            let fields = endpoint::follow_add_form(user, BookmarkRestrict::Public);
            match self.post_form(&endpoint::follow_add(), &fields).await {
                Ok(body) => {
                    mapping::check_follow_add(200, &body).map_err(MutationFailure::definite)
                }
                Err(failure) => Err(failure),
            }
        } else {
            let fields = endpoint::follow_remove_form(user);
            match self.post_form(&endpoint::follow_remove(), &fields).await {
                Ok(body) => mapping::check_follow_remove(200, &body, user)
                    .map_err(MutationFailure::definite),
                Err(failure) => Err(failure),
            }
        };
        if let Err(failure) = mutation {
            if failure.possibly_applied {
                // Same rule as the bookmark routes: a replay or a lost reply is
                // not a verdict, so read the server before reporting one.
                if let Ok(current) = self.follow_state(user).await {
                    if current == follow {
                        tracing::debug!(
                            user = user.0,
                            follow,
                            "关注写操作按失败处理，但服务端状态已是目标状态"
                        );
                        return Ok(());
                    }
                }
            }
            return Err(failure.error);
        }
        Ok(())
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

    /// Attach everything the live site attaches to a request.
    ///
    /// Captured from the web client's own `fetch` wrapper, which sends this set on
    /// *every* call to `www.pixiv.net`:
    ///
    /// * `Cookie: PHPSESSID=...`
    /// * `x-csrf-token` — required by every state-changing route
    /// * `x-user-id` — required alongside it
    /// * `x-pixiv-next: 1` — marks the request as coming from the new front end
    /// * `Referer: https://www.pixiv.net/`
    ///
    /// Omitting the middle two is what made `POST /ajax/illusts/bookmarks/add`
    /// answer `422 不正なリクエストです。` no matter how correct the body was.
    ///
    /// The cookie is still scoped to the web host only: image traffic goes to
    /// `i.pximg.net`, which needs the Referer and must not receive the session.
    fn authorize(&self, url: &str, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let Some(cookie) = self.cookie_for(url) else {
            return builder;
        };
        let session = self.inner.session.read();
        let mut builder = builder
            .header(reqwest::header::COOKIE, cookie)
            .header(reqwest::header::REFERER, PIXIV_REFERER)
            .header("x-pixiv-next", "1");
        if let Some(token) = session
            .csrf_token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
        {
            builder = builder.header("x-csrf-token", token);
        }
        if let Some(user) = session.user_id {
            builder = builder.header("x-user-id", user.get().to_string());
        }
        builder
    }

    async fn attempt_text(&self, url: &str) -> Result<String, Failure> {
        #[cfg(test)]
        {
            if let Some((status, _, body)) = self.scripted(url) {
                return finish_text(status, body, None);
            }
        }
        self.inner.limiter.wait().await;
        let response = self
            .authorize(url, self.inner.http.get(url))
            .send()
            .await
            .map_err(|e| Failure::new(network_error(&e)))?;

        let status = response.status().as_u16();
        let hint = retry_hint_for(status, response.headers());
        // `Response::text` needs the `charset` feature, which this crate does
        // not enable, and `Response::bytes` would buffer the whole document
        // before anything could judge it: the body is read under the ceiling and
        // decoded here instead.
        let bytes = read_body(response, self.inner.config.max_body_bytes, hint).await?;
        finish_text(status, String::from_utf8_lossy(&bytes).into_owned(), hint)
    }

    async fn attempt_bytes(&self, url: &str) -> Result<(Vec<u8>, Option<String>), Failure> {
        #[cfg(test)]
        {
            if let Some((status, content_type, body)) = self.scripted(url) {
                return finish_bytes(status, body.into_bytes(), content_type, None);
            }
        }
        self.inner.limiter.wait().await;
        let response = self
            .authorize(url, self.inner.http.get(url))
            // `i.pximg.net` answers 403 for any request without this.
            .header(reqwest::header::REFERER, PIXIV_REFERER)
            .send()
            .await
            .map_err(|e| Failure::new(network_error(&e)))?;

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let hint = retry_hint_for(status, response.headers());
        // The image path is allowed a bigger ceiling than the text path, because
        // an original page or an ugoira archive is legitimately large.
        let bytes = read_body(response, self.inner.config.max_image_bytes, hint).await?;
        finish_bytes(status, bytes, content_type, hint)
    }

    /// One `POST` attempt, handing back the raw status and body.
    ///
    /// The body is returned rather than pre-checked because the mutation routes do
    /// not share one success envelope: the `ajax` routes answer
    /// `{error, message, body}`, `bookmark_add.php` answers a JSON array, and
    /// `rpc_group_setting.php` answers an object echoing the user. Each caller
    /// validates with the check that matches its route.
    async fn attempt_post(
        &self,
        url: &str,
        body: String,
        content_type: &'static str,
    ) -> Result<String, Failure> {
        #[cfg(test)]
        {
            if let Some((status, _, body)) = self.scripted(url) {
                return finish_post(status, body, None);
            }
        }
        self.inner.limiter.wait().await;
        let response = self
            .authorize(
                url,
                self.inner
                    .http
                    .post(url)
                    .header(reqwest::header::CONTENT_TYPE, content_type)
                    .body(body),
            )
            .send()
            .await
            .map_err(|e| Failure::new(network_error(&e)))?;

        let status = response.status().as_u16();
        let hint = retry_hint_for(status, response.headers());
        let bytes = read_body(response, self.inner.config.max_body_bytes, hint).await?;
        finish_post(status, String::from_utf8_lossy(&bytes).into_owned(), hint)
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

    /// `POST` a form body.
    ///
    /// Retries follow [`should_retry_mutation`] — a `429`/`503` that named a wait —
    /// plus one CSRF recovery: a `422` means the token went stale, and refreshing
    /// it is the fix rather than a second identical request. A `{"error": true}`
    /// answer is final and is reported as such. Every failure says whether the
    /// mutation might have landed, because a `POST` cannot be replayed by
    /// assumption.
    async fn post_form(
        &self,
        url: &str,
        fields: &[(&str, String)],
    ) -> Result<String, MutationFailure> {
        self.post_with_retry(
            url,
            endpoint::form_body(fields),
            "application/x-www-form-urlencoded",
        )
        .await
    }

    /// `POST` a JSON body (the bookmark routes take JSON, not a form).
    async fn post_json(&self, url: &str, body: String) -> Result<String, MutationFailure> {
        self.post_with_retry(url, body, "application/json").await
    }

    /// The shared `POST` retry loop.
    ///
    /// `sent` counts the requests that actually left this process, which is what
    /// decides whether a failure may have been applied: one attempt whose reply was
    /// never seen is as unknown as two attempts, and both are re-read by the caller.
    async fn post_with_retry(
        &self,
        url: &str,
        body: String,
        content_type: &'static str,
    ) -> Result<String, MutationFailure> {
        let max_retries = self.inner.config.max_retries;
        let mut attempt = 0;
        let mut sent = 0_u32;
        let mut refreshed = false;
        loop {
            sent = sent.saturating_add(1);
            let failure = match self.attempt_post(url, body.clone(), content_type).await {
                Ok(text) => return Ok(text),
                Err(failure) => failure,
            };
            // One attempt that produced no reply at all is a mutation of unknown
            // state; two attempts prove the request reached the server at least once.
            let possibly_applied = sent > 1 || failure.status.is_none();
            if !refreshed && failure.is_csrf_rejection() {
                // The session token rotated out from under us. Refresh once and
                // replay: retrying with the same token would just fail again.
                tracing::debug!(url = %url, "CSRF 校验失败，刷新令牌后重试");
                self.refresh_identity()
                    .await
                    .map_err(|error| MutationFailure {
                        error,
                        possibly_applied,
                    })?;
                refreshed = true;
                continue;
            }
            if !should_retry_mutation(attempt, max_retries, &failure) {
                return Err(MutationFailure {
                    error: failure.error,
                    possibly_applied,
                });
            }
            tracing::debug!(
                attempt,
                url = %url,
                error = %failure.error,
                "服务端要求稍后重试写操作"
            );
            tokio::time::sleep(mapping::backoff_delay(attempt, failure.retry_after)).await;
            attempt += 1;
        }
    }
}

#[cfg(test)]
impl Client {
    /// Answer one request from the armed table instead of the network.
    ///
    /// `None` means "no table", in which case the caller proceeds to `reqwest` as
    /// it always does. A table that is armed but has no reply for `url` is a test
    /// bug, and panicking here is the only way to keep that bug from turning into
    /// a live request (the whole point of the fake is that the suite never opens a
    /// socket).
    fn scripted(&self, url: &str) -> Option<(u16, Option<String>, String)> {
        let mut fake = self.inner.fake.lock();
        let fake = fake.as_mut()?;
        fake.seen.push(url.to_string());
        let reply = fake
            .replies
            .iter()
            .find(|(prefix, _, _, _)| url.starts_with(prefix.as_str()))
            .unwrap_or_else(|| panic!("no scripted reply for {url}"));
        Some((reply.1, reply.2.clone(), reply.3.clone()))
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
            csrf_token: Some("SECRET-CSRF-VALUE".to_string()),
        };
        let rendered = format!("{session:?}");
        assert!(!rendered.contains("SECRET"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("me"), "{rendered}");
        assert!(!rendered.contains("CSRF-VALUE"), "{rendered}");
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
            csrf_token: None,
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
            client
                .set_bookmark(Id(1), true, BookmarkRestrict::Public)
                .await,
            Err(PixivError::Unauthorized)
        ));
        assert!(matches!(
            client
                .set_bookmark(Id(1), false, BookmarkRestrict::Private)
                .await,
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
            client
                .set_bookmark(Id(1), true, BookmarkRestrict::Public)
                .await,
            Err(PixivError::Unauthorized)
        ));
        assert!(matches!(
            client.bookmarks(Id(1), false, 1).await,
            Err(PixivError::Unauthorized)
        ));
    }

    // -- mutation retries --------------------------------------------------

    /// A `POST` is replayed only when the server named a wait.
    #[test]
    fn a_mutation_is_replayed_only_for_a_429_or_503_that_names_a_wait() {
        let wait = Some(Duration::from_secs(2));
        let hinted_429 =
            Failure::with_hint(PixivError::http(429, "slow down"), wait).with_status(429);
        let hinted_503 = Failure::with_hint(PixivError::http(503, "later"), wait).with_status(503);
        let bare_429 =
            Failure::with_hint(PixivError::http(429, "slow down"), None).with_status(429);
        let bare_500 = Failure::with_hint(PixivError::http(500, "boom"), wait).with_status(500);
        let transport = Failure::new(PixivError::Network("connection reset".to_string()));

        assert!(should_retry_mutation(0, 2, &hinted_429));
        assert!(should_retry_mutation(0, 2, &hinted_503));
        assert!(
            !should_retry_mutation(0, 2, &bare_429),
            "a 429 that said nothing is not a scheduled replay"
        );
        assert!(!should_retry_mutation(0, 2, &bare_500));
        assert!(
            !should_retry_mutation(0, 2, &transport),
            "a lost reply must not be replayed: the mutation may have landed"
        );
        // The attempt budget still binds.
        assert!(!should_retry_mutation(2, 2, &hinted_503));
    }

    /// And the loop obeys it: a bare `429` is sent once, not three times.
    #[tokio::test]
    async fn a_bare_429_is_not_replayed() {
        let url = "https://www.pixiv.net/ajax/illust/1/bookmarkData";
        let client = scripted_client(&[(url, 429, "{}")]);
        let failure = client
            .post_form(url, &[("illust_id", "1".to_string())])
            .await
            .expect_err("a 429 fails the mutation");
        assert_eq!(asked(&client).len(), 1, "one attempt, not max_retries + 1");
        assert!(
            !failure.possibly_applied,
            "the server answered 429 before running anything: nothing to re-read"
        );
    }

    /// A refusal that proves nothing happened is reported as definite.
    #[tokio::test]
    async fn a_logical_rejection_is_a_definite_failure() {
        let url = "https://www.pixiv.net/ajax/illust/1/bookmarkData";
        let client = scripted_client(&[(url, 400, r#"{"error":true,"message":"bad"}"#)]);
        let failure = client
            .post_form(url, &[("illust_id", "1".to_string())])
            .await
            .expect_err("a flagged 400 fails the mutation");
        assert!(!failure.possibly_applied);
        assert_eq!(asked(&client).len(), 1);
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

    // -- one document, one request -----------------------------------------

    /// `/ajax/user/{id}/profile/all`, shaped like the live document: two
    /// illustrations and one manga, which is the pair of counts the card shows.
    const PROFILE_ALL: &str = r#"{"error":false,"message":"","body":{
        "illusts":{"905":null,"904":null},"manga":{"906":null},
        "novels":[],"pickup":[]}}"#;
    /// `/ajax/user/{id}`, reduced to the fields the card reads.
    const USER_DETAIL: &str = r#"{"error":false,"message":"","body":{
        "userId":"74135333","name":"artist","pixivId":"handle","image":"",
        "imageBig":"https://i.pximg.net/a.jpg","comment":"hi","following":12,
        "isFollowed":false,"premium":false}}"#;

    /// A client whose transport answers from a table instead of a socket.
    fn scripted_client(replies: &[(&str, u16, &str)]) -> Client {
        let client = Client::new(
            ClientConfig {
                min_interval: Duration::ZERO,
                ..ClientConfig::default()
            },
            Session::default(),
        )
        .expect("client builds");
        *client.inner.fake.lock() = Some(FakeWire {
            replies: replies
                .iter()
                .map(|(url, status, body)| ((*url).to_string(), *status, None, (*body).to_string()))
                .collect(),
            seen: Vec::new(),
        });
        client
    }

    /// Every URL the scripted client was asked for, in order.
    fn asked(client: &Client) -> Vec<String> {
        client
            .inner
            .fake
            .lock()
            .as_ref()
            .expect("the fake stays armed")
            .seen
            .clone()
    }

    #[tokio::test]
    async fn one_profile_bundle_is_two_requests_and_both_answers() {
        let user = Id(74_135_333);
        let profile_url = endpoint::user_profile_all(user);
        let detail_url = endpoint::user_detail(user);
        let client = scripted_client(&[
            (profile_url.as_str(), 200, PROFILE_ALL),
            (detail_url.as_str(), 200, USER_DETAIL),
        ]);

        let (profile, ids) = client
            .profile_bundle(user)
            .await
            .expect("the bundle is built");

        // The whole point: two documents, two requests. It used to be four,
        // because the id map was downloaded once per caller.
        let urls = asked(&client);
        assert_eq!(urls.len(), 2, "one request per document, got {urls:?}");
        assert_eq!(
            urls.iter()
                .filter(|url| url.contains("/profile/all"))
                .count(),
            1
        );
        assert_eq!(urls.iter().filter(|url| url.contains("?full=1")).count(), 1);

        // Both halves of the answer come from the single profile_all document.
        assert_eq!(profile.illust_count, 2);
        assert_eq!(profile.manga_count, 1);
        assert_eq!(ids, vec![Id(906), Id(905), Id(904)]);
        // The card itself is filled from the detail document, still once.
        assert_eq!(profile.id, user);
        assert_eq!(profile.name, "artist");
        assert_eq!(profile.followees, 12);
        assert!(
            profile.followers.is_none(),
            "only the signed-in account may read its own followers"
        );
    }

    #[tokio::test]
    async fn the_two_old_entry_points_cost_what_they_did_and_no_more() {
        let user = Id(74_135_333);
        let profile_url = endpoint::user_profile_all(user);
        let detail_url = endpoint::user_detail(user);

        // `user_profile` is the bundle with half the answer dropped: same two.
        let client = scripted_client(&[
            (profile_url.as_str(), 200, PROFILE_ALL),
            (detail_url.as_str(), 200, USER_DETAIL),
        ]);
        let profile = client.user_profile(user).await.expect("profile parses");
        assert_eq!(asked(&client).len(), 2);
        assert_eq!(profile.illust_count, 2);

        // `user_illust_ids` needs the id map alone: one request, as before.
        let client = scripted_client(&[(profile_url.as_str(), 200, PROFILE_ALL)]);
        let ids = client.user_illust_ids(user).await.expect("ids parse");
        assert_eq!(asked(&client).len(), 1);
        assert_eq!(ids, vec![Id(906), Id(905), Id(904)]);
    }

    // -- the response ceiling ----------------------------------------------

    #[test]
    fn the_ceiling_refuses_an_oversized_declaration_before_reading() {
        let budget = BodyBudget::new(1024 * 1024);
        // Ten megabytes declared against a one megabyte ceiling: refused on the
        // header, so no chunk is ever requested. That is what "before buffering"
        // has to mean — the old code paid for the whole document first.
        let refused = budget.check_declared(Some(10 * 1024 * 1024));
        assert!(refused.is_err(), "an oversized declaration must be refused");
        assert!(
            !refused.unwrap_err().is_retryable(),
            "asking again would arrive at the same oversized answer"
        );
        // The ceiling is inclusive; one byte over is not.
        assert!(budget.check_declared(Some(1024 * 1024)).is_ok());
        assert!(budget.check_declared(Some(1024 * 1024 + 1)).is_err());
        // No declaration proves nothing, which is why the running check exists.
        assert!(budget.check_declared(None).is_ok());
    }

    #[test]
    fn the_ceiling_stops_a_body_that_grows_past_it_mid_stream() {
        // A chunked answer that declares nothing: half a megabyte at a time under
        // a one megabyte ceiling must stop on the third chunk, not after the whole
        // document has been buffered.
        let mut budget = BodyBudget::new(1024 * 1024);
        assert!(budget.take(512 * 1024).is_ok());
        assert!(
            budget.take(512 * 1024).is_ok(),
            "exactly at the ceiling is allowed"
        );
        assert_eq!(budget.taken, 1024 * 1024);
        assert!(
            budget.take(1).is_err(),
            "the ceiling is a limit, not a suggestion"
        );
        // A refused chunk is not counted: the budget never claims what it refused.
        assert_eq!(budget.taken, 1024 * 1024);
    }

    #[test]
    fn a_retry_after_on_a_503_sets_the_wait() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("7"));
        // This header used to be read only for 429, so a 503 that said "come back
        // in seven seconds" was retried after 300 ms.
        let hint = retry_hint_for(503, &headers);
        assert_eq!(hint, Some(Duration::from_secs(7)));
        assert_eq!(mapping::backoff_delay(0, hint), Duration::from_secs(7));
        // The status is still reported as 503; only the wait moves.
        assert!(matches!(
            mapping::map_status_error_with_hint(503, "", hint),
            PixivError::Http { status: 503, .. }
        ));
        // A 429 with the same header carries the wait inside the error, which is
        // where the friendly sentence reads it from.
        match mapping::map_status_error_with_hint(429, "", retry_hint_for(429, &headers)) {
            PixivError::RateLimited(after) => assert_eq!(after, Duration::from_secs(7)),
            other => panic!("expected RateLimited, got {other:?}"),
        }
        // Success takes no hint, and an HTTP-date is not a hint this client honours.
        assert_eq!(retry_hint_for(200, &headers), None);
        let mut date = HeaderMap::new();
        date.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_hint_for(503, &date), None);
    }
}
