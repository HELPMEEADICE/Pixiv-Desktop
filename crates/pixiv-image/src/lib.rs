//! Asynchronous, priority-aware image cache.
//!
//! # Scope
//!
//! Owns the full path from a URL to raw RGBA pixels: request coalescing,
//! priority ordering, bounded concurrency, in-memory LRU eviction, an on-disk
//! tier, and decode. It deliberately knows nothing about Slint — the app crate
//! converts [`DecodedImage`] into a `slint::Image`.
//!
//! # Why this exists as its own crate
//!
//! Scrolling a grid must never decode full-resolution originals or issue one
//! HTTP request per visible card. Requirements the implementation must satisfy:
//!
//! * **Tiered loading** — grids request the `regular` tier, the viewer requests
//!   original pages. Never decode a 4000px original to fill a 240px card.
//! * **Priority** — visible items jump ahead of prefetch work.
//! * **Coalescing** — two requests for the same URL share one network fetch.
//! * **Cancellation** — abandoning a page drops its queued work instead of
//!   letting it run to completion.
//! * **Bounded memory** — a byte budget with LRU eviction, not an unbounded map.
//! * **Disk budget** — a configurable cap with a prune path.
//!
//! # How the tiers fit together
//!
//! ```text
//! load(url, prio)
//!   ├─ memory  (parking_lot::Mutex over an intrusive LRU, no I/O, no alloc)
//!   │            hit -> return Arc<DecodedImage> immediately
//!   ├─ single-flight  (one OnceCell per (url, target width))
//!   │            the winner runs `fill`, everybody else awaits the same cell,
//!   │            so N concurrent requests cause exactly one fetch
//!   └─ fill
//!        ├─ disk   (encoded bytes, read on spawn_blocking)
//!        ├─ gate   (priority-ordered permit, size = config.concurrency)
//!        ├─ network (reqwest GET with the mandatory Pixiv Referer)
//!        └─ decode (image crate, on spawn_blocking)
//! ```
//!
//! The memory tier is an intrusive LRU over a slab: [`ImageCache::peek`] is the
//! hot path (once per visible card per frame) and costs one mutex acquisition,
//! one hash lookup, and four pointer writes — no allocation, ever.
//!
//! # Testability
//!
//! The network step sits behind [`BytesFetcher`], so a test can inject a
//! counting stub and prove coalescing without touching the network. Build that
//! cache with [`ImageCache::with_fetcher`]; [`ImageCache::new`] installs the real
//! HTTP fetcher.

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use pixiv_types::{PIXIV_REFERER, PixivError};
use tokio::sync::OnceCell;

mod disk;
mod gate;
mod lru;

use disk::DiskTier;
use gate::PriorityGate;
use lru::{MemoryTier, Variant};

/// `User-Agent` sent with every image request; mirrors the transport crate.
const USER_AGENT: &str = concat!(
    "PixivDesktop/",
    env!("CARGO_PKG_VERSION"),
    " (Rust; desktop client)"
);

/// Upper bound for one image download.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Decoded, tightly packed RGBA8 pixels.
#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Arc<[u8]>,
}

impl DecodedImage {
    /// Byte size of the pixel buffer only (no allocator overhead).
    #[must_use]
    pub fn byte_size(&self) -> usize {
        self.rgba.len()
    }
}

/// Scheduling hint. Higher priority work is dequeued first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// On screen right now.
    Visible = 0,
    /// Adjacent to the viewport; preloaded.
    Near = 1,
    /// Speculative.
    Prefetch = 2,
}

/// Cache limits.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// In-memory pixel budget. Eviction is LRU by last use.
    pub memory_budget_bytes: usize,
    /// On-disk budget for cached encoded bytes and decoded thumbnails.
    pub disk_budget_bytes: u64,
    /// Cache directory. `None` disables the disk tier entirely.
    pub disk_dir: Option<PathBuf>,
    /// Maximum simultaneous network fetches.
    pub concurrency: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            memory_budget_bytes: 192 * 1024 * 1024,
            disk_budget_bytes: 1024 * 1024 * 1024,
            disk_dir: None,
            concurrency: 6,
        }
    }
}

/// Observable cache counters, surfaced in the settings page.
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheStats {
    pub memory_bytes: usize,
    pub memory_entries: usize,
    pub disk_bytes: u64,
    pub memory_hits: u64,
    pub disk_hits: u64,
    pub network_loads: u64,
    pub in_flight: usize,
}

/// The replaceable network step: fetch the encoded bytes for a URL.
///
/// Implemented by the real HTTP client and by test stubs; keeping it behind a
/// trait is what makes coalescing and tier behaviour provable without a socket.
pub trait BytesFetcher: Send + Sync {
    /// Fetch `url`, returning the encoded image bytes (JPEG/PNG/WebP/GIF).
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Vec<u8>, PixivError>>;
}

/// A failure shared by every waiter of one single-flight cell.
///
/// [`PixivError`] is not `Clone`, so the shared cell carries this instead and
/// each caller rebuilds its own error.
#[derive(Debug)]
enum FetchError {
    Network(String),
    Http { status: u16, message: String },
    Decode(String),
    Io(String),
    Cancelled,
    Other(String),
}

impl FetchError {
    fn from_pixiv(error: &PixivError) -> Self {
        match error {
            PixivError::Network(message) => Self::Network(message.clone()),
            PixivError::Http { status, message } => Self::Http {
                status: *status,
                message: message.clone(),
            },
            PixivError::Decode(message) => Self::Decode(message.clone()),
            PixivError::Io(message) => Self::Io(message.clone()),
            PixivError::Cancelled => Self::Cancelled,
            other => Self::Other(other.to_string()),
        }
    }

    fn to_pixiv(&self) -> PixivError {
        match self {
            Self::Network(message) => PixivError::Network(message.clone()),
            Self::Http { status, message } => PixivError::http(*status, message.clone()),
            Self::Decode(message) => PixivError::Decode(message.clone()),
            Self::Io(message) => PixivError::Io(message.clone()),
            Self::Cancelled => PixivError::Cancelled,
            Self::Other(message) => PixivError::Other(message.clone()),
        }
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_pixiv())
    }
}

/// The single-flight outcome: cheap to clone, shared by all waiters.
type FlightResult = Result<Arc<DecodedImage>, Arc<FetchError>>;
type FlightCell = Arc<OnceCell<FlightResult>>;
/// `(url, target width)`; the width is part of the key because a card-sized and
/// a full-screen request for one URL are different results.
type FlightKey = (String, Option<u32>);
type Flights = Mutex<HashMap<FlightKey, FlightCell>>;

/// The real network step: `reqwest` with the headers `i.pximg.net` insists on.
#[derive(Debug)]
struct HttpFetcher {
    client: Option<reqwest::Client>,
}

impl HttpFetcher {
    fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| {
                tracing::warn!(%error, "image HTTP client could not be built");
            })
            .ok();
        Self { client }
    }
}

impl BytesFetcher for HttpFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Vec<u8>, PixivError>> {
        async move {
            let Some(client) = self.client.as_ref() else {
                return Err(PixivError::Network(
                    "image HTTP client unavailable".to_owned(),
                ));
            };
            let response = client
                .get(url)
                // Without this header i.pximg.net answers 403.
                .header(reqwest::header::REFERER, PIXIV_REFERER)
                .send()
                .await
                .map_err(|error| PixivError::Network(error.to_string()))?;
            let status = response.status();
            if !status.is_success() {
                return Err(PixivError::http(status.as_u16(), url.to_owned()));
            }
            let bytes = response
                .bytes()
                .await
                .map_err(|error| PixivError::Network(error.to_string()))?;
            Ok(bytes.to_vec())
        }
        .boxed()
    }
}

struct Inner {
    config: CacheConfig,
    memory: Mutex<MemoryTier>,
    flights: Flights,
    gate: Arc<PriorityGate>,
    fetcher: Arc<dyn BytesFetcher>,
    disk: Option<Arc<DiskTier>>,
    memory_hits: AtomicU64,
    disk_hits: AtomicU64,
    network_loads: AtomicU64,
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageCache")
            .field("config", &self.config)
            .field("memory_bytes", &self.memory.lock().bytes())
            .field("memory_entries", &self.memory.lock().len())
            .field("in_flight", &self.flights.lock().len())
            .field("disk_dir", &self.disk.as_ref().map(|disk| disk.dir()))
            .finish_non_exhaustive()
    }
}

/// The cache itself.
///
/// Cheap to clone: every clone shares one set of tiers, one flight table, and
/// one concurrency gate.
#[derive(Clone, Debug)]
pub struct ImageCache {
    inner: Arc<Inner>,
    _private: (),
}

impl ImageCache {
    /// Build a cache that downloads over HTTP.
    ///
    /// The disk tier is created only when [`CacheConfig::disk_dir`] is `Some`;
    /// an unusable directory degrades to memory-only with a warning.
    #[must_use]
    pub fn new(config: CacheConfig) -> Self {
        Self::with_fetcher(config, Arc::new(HttpFetcher::new()))
    }

    /// Build a cache around a custom [`BytesFetcher`].
    ///
    /// Used by the app for a shared HTTP client and by tests for a stub.
    #[must_use]
    pub fn with_fetcher(config: CacheConfig, fetcher: Arc<dyn BytesFetcher>) -> Self {
        let disk = config
            .disk_dir
            .clone()
            .and_then(|dir| DiskTier::open(dir, config.disk_budget_bytes).map(Arc::new));
        let gate = PriorityGate::new(config.concurrency);
        let memory = MemoryTier::new(config.memory_budget_bytes);
        Self {
            inner: Arc::new(Inner {
                config,
                memory: Mutex::new(memory),
                flights: Mutex::new(HashMap::new()),
                gate,
                fetcher,
                disk,
                memory_hits: AtomicU64::new(0),
                disk_hits: AtomicU64::new(0),
                network_loads: AtomicU64::new(0),
            }),
            _private: (),
        }
    }

    /// Synchronous memory-only probe, for use while building a grid frame.
    ///
    /// Never touches the disk, the network, or an allocator: this is the call a
    /// grid makes for every visible card. It does refresh the entry's recency so
    /// what is on screen is what survives eviction, and it does not count as a
    /// hit in [`CacheStats`] (that would drown the counters in frame rate).
    #[must_use]
    pub fn peek(&self, url: &str) -> Option<Arc<DecodedImage>> {
        self.inner.memory.lock().get_any(url, None)
    }

    /// Load an image: memory, then disk, then network. Coalesced per URL.
    pub async fn load(
        &self,
        url: &str,
        priority: Priority,
    ) -> Result<Arc<DecodedImage>, PixivError> {
        self.load_sized(url, priority, None).await
    }

    /// Load an image capped to `target_width` display pixels.
    ///
    /// This is the method grids use: passing the card width means the cache
    /// stores (and later hands back) pixels sized for the card instead of a
    /// 4000px original, which is what keeps a 200-card scroll inside the byte
    /// budget. `None` and `Some(0)` both mean "no cap" and behave exactly like
    /// [`ImageCache::load`].
    ///
    /// The width is part of the coalescing key, so a card-sized request and a
    /// full-screen viewer request for one URL are two fetches, never one result
    /// silently serving the other. A cache hit needs pixels no wider than the
    /// request; anything wider is refetched and downscaled.
    pub async fn load_sized(
        &self,
        url: &str,
        priority: Priority,
        target_width: Option<u32>,
    ) -> Result<Arc<DecodedImage>, PixivError> {
        let wanted = target_width.filter(|width| *width > 0);
        if let Some(hit) = self.memory_hit(url, wanted) {
            return Ok(hit);
        }
        let key: FlightKey = (url.to_owned(), wanted);
        let flight = self.flight(&key);
        let outcome = flight
            .get_or_init(|| self.fill(&key, priority))
            .await
            .clone();
        self.retire(&key, &flight);
        outcome.map_err(|error| error.to_pixiv())
    }

    /// Drop cached state for a URL (used when a request is cancelled).
    ///
    /// Memory entries and any in-progress single-flight cell go; the disk entry
    /// stays, because re-reading it is cheaper than re-downloading.
    pub fn forget(&self, url: &str) {
        self.inner.memory.lock().remove(url);
        self.inner.flights.lock().retain(|key, _| key.0 != url);
    }

    #[must_use]
    pub fn stats(&self) -> CacheStats {
        let (memory_bytes, memory_entries) = {
            let memory = self.inner.memory.lock();
            (memory.bytes(), memory.len())
        };
        CacheStats {
            memory_bytes,
            memory_entries,
            disk_bytes: self.inner.disk.as_ref().map_or(0, |disk| disk.bytes()),
            memory_hits: self.inner.memory_hits.load(Ordering::Relaxed),
            disk_hits: self.inner.disk_hits.load(Ordering::Relaxed),
            network_loads: self.inner.network_loads.load(Ordering::Relaxed),
            in_flight: self.inner.flights.lock().len(),
        }
    }

    /// Evict memory until the budget is respected.
    pub fn prune_memory(&self) {
        self.inner.memory.lock().prune();
    }

    /// Delete the disk tier and return the bytes reclaimed.
    ///
    /// Returns `Ok(0)` when the tier is disabled or was already empty. This is a
    /// maintenance call, not a hot path, so the filesystem walk runs inline.
    pub fn clear_disk(&self) -> Result<u64, PixivError> {
        match self.inner.disk.as_ref() {
            None => Ok(0),
            Some(disk) => disk
                .clear()
                .map_err(|error| PixivError::Io(error.to_string())),
        }
    }

    /// Memory probe that bumps the hit counter; used on the `load` paths only.
    fn memory_hit(&self, url: &str, wanted: Option<u32>) -> Option<Arc<DecodedImage>> {
        let hit = self.inner.memory.lock().get(url, wanted);
        if hit.is_some() {
            self.inner.memory_hits.fetch_add(1, Ordering::Relaxed);
        }
        hit
    }

    /// Get or create the single-flight cell for `key`.
    fn flight(&self, key: &FlightKey) -> FlightCell {
        let mut flights = self.inner.flights.lock();
        Arc::clone(flights.entry(key.clone()).or_default())
    }

    /// Forget a completed cell so later loads go back to the memory tier.
    fn retire(&self, key: &FlightKey, cell: &FlightCell) {
        let mut flights = self.inner.flights.lock();
        if flights
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, cell))
        {
            flights.remove(key);
        }
    }

    /// The body of a single flight: disk, then network, then store.
    async fn fill(&self, key: &FlightKey, priority: Priority) -> FlightResult {
        let (url, wanted) = (key.0.as_str(), key.1);

        // A sibling flight (same URL, other width) may have landed already.
        if let Some(hit) = self.memory_hit(url, wanted) {
            return Ok(hit);
        }

        if let Some(encoded) = self.disk_read(url).await {
            match decode_owned(encoded, wanted).await {
                Ok(image) => {
                    self.inner.disk_hits.fetch_add(1, Ordering::Relaxed);
                    self.store(url, wanted, &image);
                    return Ok(image);
                }
                Err(error) => {
                    // Corrupt entry: drop it and fall through to the network.
                    tracing::warn!(url, error = %error, "cached image bytes are undecodable");
                    self.disk_forget(url).await;
                }
            }
        }

        // Priority is decided here: the permit is what the gate hands out in
        // Visible -> Near -> Prefetch order.
        let _permit = self.inner.gate.acquire(priority).await;
        if let Some(hit) = self.memory_hit(url, wanted) {
            return Ok(hit);
        }

        let encoded: Arc<[u8]> = match self.inner.fetcher.fetch(url).await {
            Ok(bytes) => bytes.into(),
            Err(error) => {
                tracing::debug!(url, %error, "image fetch failed");
                return Err(Arc::new(FetchError::from_pixiv(&error)));
            }
        };
        self.inner.network_loads.fetch_add(1, Ordering::Relaxed);

        let image = decode_owned(Arc::clone(&encoded), wanted).await?;
        self.store(url, wanted, &image);
        self.disk_put(url, encoded).await;
        Ok(image)
    }

    /// Put decoded pixels in the memory tier, evicting to fit.
    fn store(&self, url: &str, wanted: Option<u32>, image: &Arc<DecodedImage>) {
        let (variant, target_width) = match wanted {
            None => (Variant::Full, u32::MAX),
            Some(width) => (Variant::Sized, width),
        };
        self.inner
            .memory
            .lock()
            .insert(url, variant, target_width, Arc::clone(image));
    }

    async fn disk_read(&self, url: &str) -> Option<Arc<[u8]>> {
        let disk = Arc::clone(self.inner.disk.as_ref()?);
        let key = cache_key(url);
        tokio::task::spawn_blocking(move || disk.get(&key))
            .await
            .ok()
            .flatten()
            .map(Arc::from)
    }

    async fn disk_put(&self, url: &str, encoded: Arc<[u8]>) {
        let Some(disk) = self.inner.disk.as_ref().map(Arc::clone) else {
            return;
        };
        let key = cache_key(url);
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(error) = disk.put(&key, &encoded) {
                tracing::warn!(%error, "could not write cached image");
            }
        })
        .await;
    }

    async fn disk_forget(&self, url: &str) {
        let Some(disk) = self.inner.disk.as_ref().map(Arc::clone) else {
            return;
        };
        let key = cache_key(url);
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(error) = disk.remove(&key) {
                tracing::warn!(%error, "could not drop cached image");
            }
        })
        .await;
    }

    /// How many loads are queued for a worker permit. Test-only.
    #[cfg(test)]
    fn gate_waiting(&self) -> usize {
        self.inner.gate.waiting()
    }
}

/// Decode encoded bytes off the async path and apply the width cap.
async fn decode_owned(
    encoded: Arc<[u8]>,
    wanted: Option<u32>,
) -> Result<Arc<DecodedImage>, Arc<FetchError>> {
    match tokio::task::spawn_blocking(move || decode_bytes(&encoded, wanted)).await {
        Ok(Ok(image)) => Ok(Arc::new(image)),
        Ok(Err(error)) => Err(Arc::new(error)),
        Err(error) => Err(Arc::new(FetchError::Other(format!(
            "decode task failed: {error}"
        )))),
    }
}

/// Blocking half of the decode step.
fn decode_bytes(encoded: &[u8], wanted: Option<u32>) -> Result<DecodedImage, FetchError> {
    let decoded =
        image::load_from_memory(encoded).map_err(|error| FetchError::Decode(error.to_string()))?;
    let rgba = decoded.to_rgba8();
    let image = DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: Arc::from(rgba.into_raw()),
    };
    Ok(match wanted {
        Some(width) => fit_to_width(&image, width),
        None => image,
    })
}

/// Conservative pre-decode downscale for a target display width.
///
/// Grids call this with the card width so a 3000px cover is never decoded at
/// full size. Returns the pixels unchanged when no downscale is needed.
#[must_use]
pub fn fit_to_width(image: &DecodedImage, target_width: u32) -> DecodedImage {
    let keep = || DecodedImage {
        width: image.width,
        height: image.height,
        // Arc clone: the caller pays nothing for a no-op downscale.
        rgba: Arc::clone(&image.rgba),
    };
    if target_width == 0 || image.width == 0 || image.height == 0 || image.width <= target_width {
        return keep();
    }
    let Some(buffer) = image::RgbaImage::from_raw(image.width, image.height, image.rgba.to_vec())
    else {
        return keep();
    };
    let scale = f64::from(target_width) / f64::from(image.width);
    let target_height = ((f64::from(image.height) * scale).round() as u32).max(1);
    let resized = image::imageops::resize(
        &buffer,
        target_width,
        target_height,
        image::imageops::FilterType::Triangle,
    );
    DecodedImage {
        width: resized.width(),
        height: resized.height(),
        rgba: Arc::from(resized.into_raw()),
    }
}

/// Stable, collision-resistant cache key for a URL.
#[must_use]
pub fn cache_key(url: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(url.as_bytes());
    let mut key = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(key, "{byte:02x}");
    }
    key
}

#[cfg(test)]
mod tests {
    use super::{
        BytesFetcher, CacheConfig, DecodedImage, ImageCache, PixivError, Priority, cache_key,
        fit_to_width,
    };
    use futures::FutureExt;
    use futures::future::BoxFuture;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

    const URL_A: &str = "https://i.pximg.net/img-master/img/a.jpg";
    const URL_B: &str = "https://i.pximg.net/img-master/img/b.jpg";
    const URL_C: &str = "https://i.pximg.net/img-master/img/c.jpg";
    const URL_D: &str = "https://i.pximg.net/img-master/img/d.jpg";

    /// Encodes a solid-ish frame so generated fixtures differ byte-wise.
    fn png_bytes(width: u32, height: u32, seed: u8) -> Vec<u8> {
        let mut buffer = image::RgbaImage::new(width, height);
        for (x, y, pixel) in buffer.enumerate_pixels_mut() {
            *pixel = image::Rgba([seed, (x % 251) as u8, (y % 251) as u8, 255]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buffer)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode fixture png");
        out.into_inner()
    }

    /// A network step that never touches the network.
    #[derive(Default)]
    struct StubFetcher {
        calls: AtomicUsize,
        urls: Mutex<Vec<String>>,
        bodies: Mutex<HashMap<String, Vec<u8>>>,
        hold: Mutex<Option<(String, Arc<Notify>)>>,
        delay: Mutex<Duration>,
    }

    impl StubFetcher {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn with_body(self: &Arc<Self>, url: &str, body: Vec<u8>) -> Arc<Self> {
            self.bodies.lock().insert(url.to_owned(), body);
            Arc::clone(self)
        }

        fn hold_on(self: &Arc<Self>, url: &str, notify: Arc<Notify>) {
            *self.hold.lock() = Some((url.to_owned(), notify));
        }

        fn set_delay(self: &Arc<Self>, delay: Duration) {
            *self.delay.lock() = delay;
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn order(&self) -> Vec<String> {
            self.urls.lock().clone()
        }
    }

    impl BytesFetcher for StubFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Vec<u8>, PixivError>> {
            async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.urls.lock().push(url.to_owned());
                let hold = { self.hold.lock().clone() };
                let delay = { *self.delay.lock() };
                if let Some((target, notify)) = hold {
                    if target == url {
                        notify.notified().await;
                    }
                }
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                self.bodies
                    .lock()
                    .get(url)
                    .cloned()
                    .ok_or_else(|| PixivError::NotFound(url.to_owned()))
            }
            .boxed()
        }
    }

    /// Coerce a concrete stub into the trait object `with_fetcher` wants.
    fn fetcher_handle(fetcher: &Arc<StubFetcher>) -> Arc<dyn BytesFetcher> {
        let clone: Arc<StubFetcher> = Arc::clone(fetcher);
        clone
    }

    fn memory_only(budget: usize, concurrency: usize) -> CacheConfig {
        CacheConfig {
            memory_budget_bytes: budget,
            disk_budget_bytes: 0,
            disk_dir: None,
            concurrency,
        }
    }

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "pixiv-image-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ))
    }

    async fn wait_until(mut condition: impl FnMut() -> bool) {
        for _ in 0..600 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("condition never became true");
    }

    #[test]
    fn cache_key_is_stable_and_distinct() {
        let first = cache_key(URL_A);
        assert_eq!(first, cache_key(URL_A));
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, cache_key(URL_B));
        // A one-character edit must not collide.
        assert_ne!(
            cache_key("https://a.test/x.jpg"),
            cache_key("https://a.test/y.jpg")
        );
        assert_eq!(cache_key(""), cache_key(""));
        assert_ne!(cache_key(""), first);
    }

    #[test]
    fn fit_to_width_downscales_only_when_wider() {
        let source = DecodedImage {
            width: 400,
            height: 200,
            rgba: vec![9u8; 400 * 200 * 4].into(),
        };
        // Wider than the target: downscaled, aspect ratio preserved.
        let small = fit_to_width(&source, 100);
        assert_eq!(small.width, 100);
        assert_eq!(small.height, 50);
        assert_eq!(small.byte_size(), 100 * 50 * 4);
        // Already narrow enough: the pixel buffer is shared, not copied.
        let untouched = fit_to_width(&source, 400);
        assert_eq!(untouched.width, 400);
        assert_eq!(untouched.byte_size(), source.byte_size());
        assert!(Arc::ptr_eq(&untouched.rgba, &source.rgba));
        let wider = fit_to_width(&source, 4096);
        assert_eq!(wider.width, 400);
        assert!(Arc::ptr_eq(&wider.rgba, &source.rgba));
    }

    #[test]
    fn fit_to_width_guards_zero() {
        let source = DecodedImage {
            width: 32,
            height: 16,
            rgba: vec![1u8; 32 * 16 * 4].into(),
        };
        let guarded = fit_to_width(&source, 0);
        assert_eq!(guarded.width, 32);
        assert_eq!(guarded.height, 16);
        assert!(Arc::ptr_eq(&guarded.rgba, &source.rgba));
    }

    #[tokio::test]
    async fn lru_eviction_respects_the_memory_budget() {
        // Every fixture decodes to 64 * 64 * 4 = 16384 bytes.
        let body = png_bytes(64, 64, 1);
        let budget = 40_000;
        let fetcher = StubFetcher::new()
            .with_body(URL_A, body.clone())
            .with_body(URL_B, body.clone())
            .with_body(URL_C, body.clone());
        let cache = ImageCache::with_fetcher(memory_only(budget, 2), fetcher);

        for url in [URL_A, URL_B, URL_C] {
            cache.load(url, Priority::Visible).await.expect("load");
        }

        let stats = cache.stats();
        assert!(
            stats.memory_bytes <= budget,
            "memory_bytes {} exceeded budget {budget}",
            stats.memory_bytes
        );
        assert_eq!(stats.memory_bytes, 2 * 64 * 64 * 4);
        assert_eq!(stats.memory_entries, 2);
        // Oldest evicted, newest survives.
        assert!(cache.peek(URL_A).is_none(), "oldest entry should be gone");
        assert!(cache.peek(URL_B).is_some());
        assert!(cache.peek(URL_C).is_some());
    }

    #[tokio::test]
    async fn prune_memory_leaves_a_cache_inside_its_budget_alone() {
        let body = png_bytes(32, 32, 2);
        let fetcher = StubFetcher::new().with_body(URL_A, body.clone());
        let cache = ImageCache::with_fetcher(memory_only(1 << 20, 1), fetcher);
        cache.load(URL_A, Priority::Visible).await.expect("load");
        let before = cache.stats();
        cache.prune_memory();
        let after = cache.stats();
        assert_eq!(before.memory_bytes, after.memory_bytes);
        assert_eq!(after.memory_entries, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_loads_share_one_fetch() {
        let body = png_bytes(48, 48, 3);
        let fetcher = StubFetcher::new().with_body(URL_A, body);
        fetcher.set_delay(Duration::from_millis(80));
        let cache = ImageCache::with_fetcher(memory_only(1 << 22, 4), fetcher_handle(&fetcher));

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            tasks.push(tokio::spawn(async move {
                cache.load(URL_A, Priority::Visible).await
            }));
        }
        let results = futures::future::join_all(tasks).await;

        assert_eq!(fetcher.calls(), 1, "single-flight must collapse the herd");
        let mut shared: Option<Arc<DecodedImage>> = None;
        for result in results {
            let image = result.expect("join").expect("load");
            match &shared {
                None => shared = Some(image),
                Some(first) => assert!(
                    Arc::ptr_eq(first, &image),
                    "every waiter must get the same decoded allocation"
                ),
            }
        }
        let stats = cache.stats();
        assert_eq!(stats.network_loads, 1);
        assert_eq!(stats.in_flight, 0);
        // A later call is a pure memory hit.
        cache
            .load(URL_A, Priority::Visible)
            .await
            .expect("hot load");
        assert_eq!(cache.stats().network_loads, 1);
        assert_eq!(cache.stats().memory_hits, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn priority_drains_visible_before_near_before_prefetch() {
        let body = png_bytes(16, 16, 4);
        let fetcher = StubFetcher::new()
            .with_body(URL_A, body.clone())
            .with_body(URL_B, body.clone())
            .with_body(URL_C, body.clone())
            .with_body(URL_D, body.clone());
        let hold = Arc::new(Notify::new());
        fetcher.hold_on(URL_A, Arc::clone(&hold));
        // One worker: everything after A has to queue behind the gate.
        let cache = ImageCache::with_fetcher(memory_only(1 << 22, 1), fetcher_handle(&fetcher));

        let first = {
            let cache = cache.clone();
            tokio::spawn(async move { cache.load(URL_A, Priority::Visible).await })
        };
        let probe = Arc::clone(&fetcher);
        wait_until(move || probe.calls() == 1).await;

        let mut queued = Vec::new();
        for (url, priority) in [
            (URL_B, Priority::Prefetch),
            (URL_C, Priority::Visible),
            (URL_D, Priority::Near),
        ] {
            let cache = cache.clone();
            queued.push(tokio::spawn(async move { cache.load(url, priority).await }));
        }
        let gate_probe = cache.clone();
        wait_until(move || gate_probe.gate_waiting() == 3).await;

        hold.notify_one();
        first.await.expect("join").expect("load a");
        for task in queued {
            task.await.expect("join").expect("queued load");
        }

        let requested: Vec<&str> = fetcher
            .order()
            .iter()
            .map(|url| {
                if url.ends_with("a.jpg") {
                    "a"
                } else if url.ends_with("b.jpg") {
                    "b"
                } else if url.ends_with("c.jpg") {
                    "c"
                } else {
                    "d"
                }
            })
            .collect();
        assert_eq!(requested, vec!["a", "c", "d", "b"]);
    }

    #[tokio::test]
    async fn disk_tier_round_trips_and_serves_a_second_cache() {
        let dir = scratch_dir("roundtrip");
        let body = png_bytes(40, 24, 5);
        let config = CacheConfig {
            memory_budget_bytes: 1 << 22,
            disk_budget_bytes: 1 << 20,
            disk_dir: Some(dir.clone()),
            concurrency: 2,
        };

        let warm = StubFetcher::new().with_body(URL_A, body.clone());
        let first = ImageCache::with_fetcher(config.clone(), fetcher_handle(&warm));
        let image = first.load(URL_A, Priority::Visible).await.expect("load");
        assert_eq!((image.width, image.height), (40, 24));
        assert_eq!(first.stats().network_loads, 1);
        assert!(first.stats().disk_bytes >= body.len() as u64);

        // A cold cache over the same directory must answer from disk only.
        let cold_fetcher = StubFetcher::new();
        let second = ImageCache::with_fetcher(config, fetcher_handle(&cold_fetcher));
        assert!(second.stats().disk_bytes >= body.len() as u64);
        let cached = second
            .load(URL_A, Priority::Visible)
            .await
            .expect("disk load");
        assert_eq!((cached.width, cached.height), (40, 24));
        assert_eq!(second.stats().disk_hits, 1);
        assert_eq!(second.stats().network_loads, 0);
        assert_eq!(
            cold_fetcher.calls(),
            0,
            "disk tier must not hit the network"
        );

        let reclaimed = second.clear_disk().expect("clear disk");
        assert_eq!(reclaimed, body.len() as u64);
        assert_eq!(second.stats().disk_bytes, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn disk_tier_degrades_to_memory_only() {
        let body = png_bytes(8, 8, 6);
        // `None`: no disk tier at all.
        let disabled = StubFetcher::new().with_body(URL_A, body.clone());
        let cache = ImageCache::with_fetcher(memory_only(1 << 20, 1), fetcher_handle(&disabled));
        cache.load(URL_A, Priority::Visible).await.expect("load");
        assert_eq!(cache.stats().disk_bytes, 0);
        assert_eq!(cache.clear_disk().expect("clear"), 0);

        // An unwritable directory: a regular file blocks the path.
        let scratch = scratch_dir("unwritable");
        std::fs::create_dir_all(&scratch).expect("scratch");
        let blocker = scratch.join("file");
        std::fs::write(&blocker, b"x").expect("blocker");
        let degraded = StubFetcher::new().with_body(URL_B, body);
        let cache = ImageCache::with_fetcher(
            CacheConfig {
                memory_budget_bytes: 1 << 20,
                disk_budget_bytes: 1 << 20,
                disk_dir: Some(blocker.join("cache")),
                concurrency: 1,
            },
            degraded,
        );
        cache.load(URL_B, Priority::Visible).await.expect("load");
        assert_eq!(cache.stats().disk_bytes, 0);
        assert_eq!(cache.clear_disk().expect("clear"), 0);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[tokio::test]
    async fn corrupt_bytes_surface_as_decode_errors() {
        let fetcher = StubFetcher::new().with_body(URL_A, b"definitely not an image".to_vec());
        let cache = ImageCache::with_fetcher(memory_only(1 << 20, 1), fetcher_handle(&fetcher));
        let error = cache
            .load(URL_A, Priority::Visible)
            .await
            .expect_err("must fail");
        assert!(
            matches!(error, PixivError::Decode(_)),
            "expected Decode, got {error:?}"
        );
        assert!(cache.peek(URL_A).is_none());
        // A later attempt is allowed to retry, because nothing is cached.
        assert!(cache.load(URL_A, Priority::Visible).await.is_err());
        assert_eq!(fetcher.calls(), 2);
    }

    #[tokio::test]
    async fn forget_drops_memory_and_forces_a_refetch() {
        let body = png_bytes(12, 12, 7);
        let fetcher = StubFetcher::new().with_body(URL_A, body);
        let cache = ImageCache::with_fetcher(memory_only(1 << 20, 1), fetcher_handle(&fetcher));
        cache.load(URL_A, Priority::Visible).await.expect("load");
        assert!(cache.peek(URL_A).is_some());
        cache.forget(URL_A);
        assert!(cache.peek(URL_A).is_none());
        assert_eq!(cache.stats().memory_entries, 0);
        cache.load(URL_A, Priority::Visible).await.expect("reload");
        assert_eq!(fetcher.calls(), 2);
        assert_eq!(cache.stats().network_loads, 2);
    }

    #[tokio::test]
    async fn load_sized_stores_card_sized_pixels() {
        let body = png_bytes(200, 100, 8);
        let fetcher = StubFetcher::new().with_body(URL_A, body);
        let cache = ImageCache::with_fetcher(memory_only(1 << 22, 1), fetcher_handle(&fetcher));

        let small = cache
            .load_sized(URL_A, Priority::Visible, Some(50))
            .await
            .expect("sized load");
        assert_eq!((small.width, small.height), (50, 25));
        assert_eq!(cache.stats().memory_bytes, 50 * 25 * 4);

        // Second sized request for the same width is a memory hit.
        let again = cache
            .load_sized(URL_A, Priority::Visible, Some(50))
            .await
            .expect("hot sized load");
        assert!(Arc::ptr_eq(&small, &again));
        assert_eq!(fetcher.calls(), 1);

        // A full-resolution request is a different flight and different pixels.
        let full = cache
            .load(URL_A, Priority::Visible)
            .await
            .expect("full load");
        assert_eq!((full.width, full.height), (200, 100));
        assert_eq!(fetcher.calls(), 2);
        // Both flavours coexist in the budget.
        assert_eq!(cache.stats().memory_entries, 2);
        // `Some(0)` means "no cap" and matches `load`.
        assert!(
            cache
                .load_sized(URL_A, Priority::Visible, Some(0))
                .await
                .is_ok()
        );
        assert_eq!(fetcher.calls(), 2);
    }

    #[tokio::test]
    async fn stats_track_every_counter() {
        let body = png_bytes(20, 20, 9);
        let fetcher = StubFetcher::new().with_body(URL_A, body);
        let cache = ImageCache::with_fetcher(memory_only(1 << 20, 2), fetcher);
        assert_eq!(cache.stats().network_loads, 0);

        cache
            .load(URL_A, Priority::Visible)
            .await
            .expect("cold load");
        let cold = cache.stats();
        assert_eq!(cold.network_loads, 1);
        assert_eq!(cold.memory_hits, 0);
        assert_eq!(cold.disk_hits, 0);
        assert_eq!(cold.memory_entries, 1);
        assert_eq!(cold.in_flight, 0);
        assert_eq!(cold.disk_bytes, 0);

        cache
            .load(URL_A, Priority::Visible)
            .await
            .expect("hot load");
        let hot = cache.stats();
        assert_eq!(hot.memory_hits, 1);
        assert_eq!(hot.network_loads, 1);
    }

    #[test]
    fn new_builds_a_working_memory_only_cache() {
        let cache = ImageCache::new(CacheConfig::default());
        assert!(cache.peek(URL_A).is_none());
        let stats = cache.stats();
        assert_eq!(stats.memory_bytes, 0);
        assert_eq!(stats.memory_entries, 0);
        assert_eq!(stats.disk_bytes, 0);
    }
}
