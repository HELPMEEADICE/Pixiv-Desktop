use super::*;

/// One sentence a user can act on, per failure class.
///
/// The three cases that used to collapse into "网络错误" are genuinely different:
/// a rejected cookie needs a new cookie, an unreachable host needs a network
/// check, and a changed payload needs a bug report. Saying which one it is is the
/// difference between a fixable problem and a shrug.
/// Turn one stored history row into a feed card.
///
/// The history table only kept the id, title, author and cover URL, so every
/// other field stays at its default instead of being fabricated — asking the
/// network to enrich a local list would make the page fail whenever it is offline.
pub(crate) fn history_illust(entry: &HistoryEntry) -> Illust {
    Illust {
        id: entry.illust_id.parse::<u64>().map(Id).unwrap_or(Id(0)),
        title: entry.title.clone(),
        kind: IllustKind::Illustration,
        page_count: 1,
        width: 0,
        height: 0,
        urls: IllustUrls {
            regular: entry.thumb_url.clone(),
            ..IllustUrls::default()
        },
        author: Author {
            id: Id(0),
            name: entry.author.clone(),
            account: String::new(),
            avatar: None,
        },
        tags: Vec::new(),
        description: String::new(),
        create_date: None,
        bookmark: Bookmark::default(),
        stats: IllustStats::default(),
        x_restrict: 0,
        sl: 0,
        is_masked: false,
        is_bookmarkable: true,
    }
}

/// Re-validate the session loaded from disk before trusting it.
///
/// A stored cookie is a claim, not a fact: it may have been revoked in the
/// browser months ago. This asks the server once at startup, quietly refreshes
/// the account id and CSRF token when it is still good, and demotes the app to
/// demo mode with an explanation when it is not.
///
/// A *network* failure is deliberately not treated as a dead session — being
/// True when the capture harness asked for the demo data source.
pub(crate) fn forced_demo() -> bool {
    std::env::var_os("PIXIV_DESKTOP_DEMO").is_some()
}

/// The window, when the UI thread still owns one.
///
/// `App::ui()` used to be `upgrade().expect("window alive")`, and every landing of
/// a background result went through that accessor, so a decode or a network answer
/// that arrived after the window was dropped panicked the event loop (T8). The
/// accessor stays the short form for code that provably runs with a window on
/// screen; the one place a result can land *without* one - `on_ui` - asks this
/// first and drops the single result instead.
pub(crate) fn live_window(ui: &slint::Weak<AppWindow>) -> Option<AppWindow> {
    ui.upgrade()
}

/// Run `f` with the app handle on the UI thread.
///
/// A result that lands after the window is gone is dropped, with one debug line:
/// every closure registered here ends up setting Slint properties, and a window
/// that no longer exists has none to set.
pub(crate) fn on_ui<F>(f: F)
where
    F: FnOnce(&Shared) + Send + 'static,
{
    let _ = invoke_from_event_loop(move || {
        APP.with(|slot| {
            let borrowed = slot.borrow();
            let Some(shared) = borrowed.as_ref() else {
                return;
            };
            // A borrow already held here means the UI thread is mid-mutation; that
            // was never a dead window, so the old behaviour stands.
            let alive = match shared.try_borrow() {
                Ok(app) => app.ui_alive(),
                Err(_) => true,
            };
            if alive {
                f(shared);
            } else {
                tracing::debug!("dropping a result that landed after the window closed");
            }
        });
    });
}

/// Raw pixels of a decoded image. `Send`, unlike `slint::Image`, and shares the
/// cache's `Arc<[u8]>` instead of copying it.
pub(crate) struct RawImage {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
}

/// Draw RGBA bytes, deriving the height from the data instead of trusting it.
///
/// The buffer is the only source of truth for how many pixels exist. A declared
/// size that disagrees with it used to be copied with `min(expected, len)`, which
/// silently drew garbage: a buffer one row short left the last row black, and one
/// row long sheared every row after the first. Neither is a picture and neither
/// reported anything, so the size is clamped to whole rows and the clamp warns.
///
/// Both draw paths go through here - a freshly decoded cover (`RawImage`) and one
/// served from the cache (`feed::cached_texture`) - because two copies of an
/// invariant drift, and this one was already written twice in one round.
pub(crate) fn rgba_to_slint(width: u32, height: u32, rgba: &[u8]) -> slint::Image {
    let width = width.max(1);
    let mut height = height.max(1);
    let stride = width as usize * 4;
    let available = rgba.len() / stride;
    if (height as usize) > available {
        tracing::warn!(
            width,
            height,
            bytes = rgba.len(),
            "image is shorter than its declared size; drawing whole rows only"
        );
        height = available.max(1) as u32;
    }
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let expected = (width as usize) * (height as usize) * 4;
    let bytes = buffer.make_mut_bytes();
    let len = expected.min(rgba.len());
    bytes[..len].copy_from_slice(&rgba[..len]);
    slint::Image::from_rgba8(buffer)
}

impl RawImage {
    pub(crate) fn to_slint(&self) -> slint::Image {
        // The decoded path can assert what the cached path cannot: this buffer
        // was produced by the decoder for exactly this size.
        debug_assert_eq!(
            self.rgba.len(),
            (self.width as usize) * (self.height as usize) * 4,
            "cover buffer must be exactly width*height*4 bytes"
        );
        rgba_to_slint(self.width, self.height, &self.rgba)
    }
}

impl App {
    /// True while the window this app posts to still exists (T8).
    pub(crate) fn ui_alive(&self) -> bool {
        live_window(&self.ui).is_some()
    }

    /// The window.
    ///
    /// Sound for every caller that runs with a window on screen - a Slint callback,
    /// a repaint, a timer that already checked - and for the landing of a result,
    /// because `on_ui` refuses to run a closure once the window is gone.
    pub(crate) fn ui(&self) -> AppWindow {
        live_window(&self.ui).expect("window alive")
    }
}

impl App {
    /// Show a shell-level notification. The shell renders it above the status bar
    /// so it is visible on every page — a bookmark result happens on the detail
    /// page, a cache result on the settings page, and a page-local bar meant the
    /// other one was silently invisible.
    ///
    /// Auto-dismisses after a few seconds, but only if the message has not been
    /// replaced in the meantime (compared by value, so a second identical notice
    /// still gets its own full timeout).
    pub(crate) fn notify(&self, kind: i32, message: impl Into<slint::SharedString>) {
        let message: slint::SharedString = message.into();
        let ui = self.ui();
        ui.set_notice(message.clone());
        ui.set_notice_kind(kind);

        let weak = self.ui.clone();
        self.notice_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(4500),
            move || {
                if let Some(ui) = weak.upgrade()
                    && ui.get_notice() == message
                {
                    ui.set_notice("".into());
                }
            },
        );
    }
}

/// Current page's intrinsic size, needed to clamp panning.
pub(crate) fn current_image_size(ui: &AppWindow, app: &App) -> (f32, f32) {
    let index = ui.get_detail_page_index().max(0) as usize;
    if let Some(models) = &app.detail_models
        && let Some(page) = models.row_data(index)
    {
        return (page.width.max(1) as f32, page.height.max(1) as f32);
    }
    (1.0, 1.0)
}

// ---------------------------------------------------------------- helpers

/// Which image URL a reload of `index` should fetch.
///
/// A work with a page list has one real URL per page; a work without one has
/// exactly one image, at the largest size the detail payload offered. Anything
/// else is an index the page list does not have, and refusing is the honest
/// answer - guessing a URL would download the wrong picture.
pub(crate) fn retry_url(pages: &[PageMeta], largest: &str, index: usize) -> Option<String> {
    match pages.get(index) {
        Some(meta) => Some(meta.urls.regular.to_string()),
        None if index == 0 => Some(largest.to_string()),
        None => None,
    }
}

/// Keep the covers the top of `order` still references; returns how many went.
///
/// `covers` holds one decoded texture per work ever shown and nothing removed an
/// entry, so an hour of browsing kept every cover alive for the rest of the
/// session. `keep` bounds it even for an endless feed: the top of the feed is what
/// a column rebuild can paint without waiting.
pub(crate) fn prune_covers(
    covers: &mut HashMap<String, slint::Image>,
    order: impl Iterator<Item = String>,
    keep: usize,
) -> usize {
    let live: HashSet<String> = order.take(keep).collect();
    let before = covers.len();
    covers.retain(|id, _| live.contains(id));
    before - covers.len()
}

/// The column that should take the next card: the shortest one, ties to the left.
///
/// `layout::distribute` makes the same choice when it lays out a whole feed;
/// appending needs the rule on its own, against the heights already accumulated.
/// (Passing a one-element slice to `distribute` looks equivalent and is not: every
/// total starts at zero, so the tie always resolves to column 0.)
pub(crate) fn shortest_column(heights: &[f32]) -> usize {
    heights
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

pub(crate) fn ranking_index(kind: RankingKind) -> i32 {
    RankingKind::ALL
        .iter()
        .position(|candidate| *candidate == kind)
        .map(|index| index as i32)
        .unwrap_or(0)
}

pub(crate) fn persisted(store: &Store, key: &str) -> Option<String> {
    match store.get_setting(key) {
        Ok(value) => value,
        Err(error) => {
            // Absent and unreadable mean the same default, but only one of them is
            // worth a line (G41).
            tracing::debug!(%error, key, "stored setting could not be read; using the default");
            None
        }
    }
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MB", bytes / (1024 * 1024))
    } else {
        format!("{} KB", bytes / 1024)
    }
}

pub(crate) fn illust_to_item(illust: &Illust, rank: i32) -> IllustItem {
    IllustItem {
        id: illust.id.to_string().into(),
        title: illust.title.clone().into(),
        author: illust.author.name.clone().into(),
        author_id: illust.author.id.to_string().into(),
        cover: slint::Image::default(),
        kind: illust.kind.code(),
        page_count: illust.page_count as i32,
        bookmarked: illust.bookmark.bookmarked,
        rank,
        width: illust.width as i32,
        height: illust.height as i32,
        state: 1,
        author_avatar: slint::Image::default(),
        author_avatar_url: illust.author.avatar.clone().unwrap_or_default().into(),
        // 1 while the payload named an avatar URL and it has not arrived yet; 2
        // when there is no URL at all, which is the only case where drawing the
        // initial is honest.
        author_avatar_state: if illust.author.avatar.is_some() { 1 } else { 2 },
        // Round nine (F2). A freshly built card is never selected: selection is
        // a property of the page, applied by the feed track when it enters the
        // mode, never something a card is born with.
        selected: false,
        selectable: false,
    }
}

// ------------------------------------------------------- landing (B1/B2/B4/B5)

/// Which surface a reply belongs to.
///
/// Four of this round's rows are the same sentence with a different key: a reply
/// computed from one surface's state is written onto whatever surface happens to be
/// on screen when it lands. The illustration page keys its surface by
/// `detail_epoch` (bumped by every open), the user page by the account it shows.
/// The reply carries the key it *asked* with; the page passes the key it is
/// *showing*; a pair that does not match is dropped whole.
///
/// The second half of the same invariant is the busy slot
/// (`detail.rs`'s [`toggle_allowed`]): a surface takes one reply at a time, so two
/// gestures cannot both start a request whose replies then race. Together they are
/// the two halves of "the reply that lands belongs to the page that asked".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReplySurface {
    /// The illustration page, keyed by the epoch that opened it.
    Detail(u64),
    /// The user page, keyed by the account id it is showing.
    Profile(u64),
}

/// Does the reply `asked` still belong to the surface `showing`?
///
/// A key never repeats: `detail_epoch` only grows and a profile page's key is the
/// account itself, so equality is the whole test.
pub(crate) fn reply_lands(showing: ReplySurface, asked: ReplySurface) -> bool {
    showing == asked
}

pub(crate) fn extension_for(url: &str) -> String {
    let lower = url.to_ascii_lowercase();
    for candidate in ["png", "webp", "gif", "jpeg", "jpg"] {
        if lower.contains(&format!(".{candidate}")) {
            return candidate.to_string();
        }
    }
    "jpg".to_string()
}

pub(crate) async fn fetch_sized(cache: &ImageCache, url: &str, target: u32) -> Option<RawImage> {
    if url.is_empty() {
        return None;
    }
    let decoded = cache
        .load_sized(url, Priority::Visible, Some(target))
        .await
        .ok()?;
    Some(RawImage {
        width: decoded.width,
        height: decoded.height,
        rgba: Arc::clone(&decoded.rgba),
    })
}

pub(crate) async fn fetch_sized_viewer(cache: &ImageCache, url: &str) -> Option<RawImage> {
    if url.is_empty() {
        return None;
    }
    let decoded = cache
        .load_sized(url, Priority::Visible, Some(VIEWER_MAX_WIDTH))
        .await
        .ok()?;
    Some(RawImage {
        width: decoded.width,
        height: decoded.height,
        rgba: Arc::clone(&decoded.rgba),
    })
}

/// Decode a procedurally generated demo cover off the UI thread.
pub(crate) async fn demo_raw(seed_id: Id, width: u32, height: u32, page: u64) -> Option<RawImage> {
    let seed = seed_id.get().wrapping_add(page.wrapping_mul(7919));
    let png = tokio::task::spawn_blocking(move || {
        pixiv_demo::render_placeholder(width.clamp(64, 1400), height.clamp(64, 1400), seed)
    })
    .await
    .ok()?;
    let decoded = image::load_from_memory(&png).ok()?.to_rgba8();
    let (w, h) = decoded.dimensions();
    Some(RawImage {
        width: w,
        height: h,
        rgba: Arc::from(decoded.into_raw()),
    })
}

pub(crate) fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("dev", "pixivdesktop", "PixivDesktop")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn downloads_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.picture_dir().map(|path| path.join("PixivDesktop")))
        .unwrap_or_else(|| PathBuf::from("PixivDesktop"))
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
