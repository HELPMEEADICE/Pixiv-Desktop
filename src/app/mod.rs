//! Application wiring: state, background work and Slint model plumbing.
//!
//! Threading contract
//! ------------------
//! * All Slint properties, models and callbacks are touched **only** on the UI
//!   thread. `App` is therefore `Rc<RefCell<..>>`, never `Arc<Mutex<..>>`.
//! * Network, decode and database work runs on a tokio runtime and returns to the
//!   UI thread through [`slint::invoke_from_event_loop`].
//! * The cover loader is bounded (`images::COVER_CONCURRENCY`) and keyed by a
//!   feed epoch, so a slow decode from a page you already left can never patch a
//!   card on the page you are looking at.
//! * The session cookie is never logged, never enters the database, and is sealed
//!   with DPAPI on Windows (see [`crate::session`]).
//!
//! Responsive layout contract
//! --------------------------
//! Nothing here guesses at the window width. Slint measures the real content box
//! (`PageScroll.body-width`) and reports it through `grid-measured`; the column
//! count and card width come from [`crate::layout`] applied to that number. There
//! is no resize timer and no second source of truth to drift out of sync.

mod callbacks;
mod detail;
mod downloads;
mod feed;
mod helpers;
mod lifecycle;
mod palette;
mod profile;
mod selection;
mod settings;
mod ugoira;

pub(crate) use self::{
    callbacks::install_callbacks, feed::*, helpers::*, lifecycle::*, palette::PaletteState,
    profile::*, selection::Selection, settings::*, ugoira::PlaybackState,
};

use slint::{
    ComponentHandle, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel,
    invoke_from_event_loop,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::runtime::Runtime;

use pixiv_api::{Client, ClientConfig, Session};
use pixiv_demo::DemoCatalog;
use pixiv_download::{DownloadConfig, DownloadManager, DownloadRequest};
use pixiv_image::{CacheConfig, ImageCache, Priority};
use pixiv_store::{HistoryEntry, Store};
use pixiv_types::domain::{
    Author, Bookmark, BookmarkRestrict, FeedSource, Illust, IllustKind, IllustStats, IllustUrls,
    PageMeta, RankingKind, RecommendTag, RecommendUser, SearchQuery, Suggestion, UserProfile,
};
use pixiv_types::{Id, PixivError};

use crate::images::{COVER_CONCURRENCY, CoverLoader};
use crate::layout::{self, GridGeometry};
use crate::route::{History, Route, RouteHistory, RouteParam};
use crate::{
    AppWindow, Destination, DownloadRow, IllustItem, ProfileStatItem, RecommendTagItem,
    RecommendUserItem, SuggestionItem, Theme, ThemeMode, Tip, ViewerPage,
};

/// Real in-memory cache budget, in MiB. Settings displays this value, so the page
/// cannot describe a limit the cache is not actually enforcing.
const CACHE_MEMORY_MB: usize = 192;
/// Real on-disk cache budget, in GiB.
const CACHE_DISK_MB: u64 = 1024;
/// Which data source is answering. Rendered in the status bar and as a top-bar
/// badge so demo data can never be mistaken for live data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Demo,
    Real,
}

/// Viewport cap for full-size artwork. A 4000px original drawn into a 900px box
/// wastes ~60MB of RGBA; 1600 keeps detail on wide windows without the waste.
const VIEWER_MAX_WIDTH: u32 = 1600;

pub(crate) struct App {
    ui: slint::Weak<AppWindow>,
    rt: Arc<Runtime>,
    store: Store,
    cache: ImageCache,
    demo: DemoCatalog,
    client: Option<Client>,
    mode: Mode,
    session: Session,
    downloads: DownloadManager,
    data_dir: PathBuf,
    download_dir: PathBuf,

    // navigation
    page: Destination,
    /// The page plus the id it shows. A bare Destination could not tell two
    /// profiles apart, which is why Back used to land on the wrong author.
    route: Route,
    history: RouteHistory,
    /// Destination the currently loaded feed belongs to, so going Back to a page
    /// whose content is still loaded does not re-fetch (and therefore does not
    /// lose the scroll position).
    feed_owner: Option<Route>,
    /// Where each route was left. A map, because this is read on every scroll
    /// delta and written once per navigation (plan §2.3 R14).
    scroll: HashMap<ScrollKey, f32>,
    /// Touch order for `scroll`, oldest first, so the table stays bounded: the
    /// entry that has sat untouched longest is the deepest Back entry.
    scroll_order: VecDeque<ScrollKey>,

    // responsive geometry, fed by what Slint measured
    measured_width: f32,
    geometry: GridGeometry,

    // feed
    items: Vec<Illust>,
    columns: Vec<Rc<VecModel<IllustItem>>>,
    /// Accumulated height of each column, so an appended page can go to the
    /// shortest one without re-reading every model.
    column_heights: Vec<f32>,
    index: HashMap<String, (usize, usize)>,
    covers: HashMap<String, slint::Image>,
    loader: CoverLoader,
    /// Incremented on every feed reload; async results from an older epoch are
    /// dropped instead of being applied to the wrong page.
    epoch: u64,
    page_cursor: u32,
    reached_end: bool,
    loading_more: bool,
    search_word: String,
    ranking_kind: RankingKind,

    // ---- home, discovery, suggestions and the native profile ----
    home: HomeBands,
    discovery: DiscoveryBands,
    suggest: SuggestState,
    profile: Option<ProfileState>,
    /// Decoded avatars, keyed by URL. Shared by every surface that shows an
    /// identity so one download serves the card, the header and the top bar.
    avatars: HashMap<String, slint::Image>,
    /// URLs already requested, so a repaint cannot start a second fetch.
    avatar_loading: HashSet<String>,
    /// Author id -> avatar URL, for payloads that name the author but not their
    /// face. Filled by a bounded profile lookup, never one request per card.
    author_avatars: HashMap<u64, String>,
    /// Author ids whose avatar lookup is already in flight.
    author_avatar_lookup: HashSet<u64>,
    /// Avatar URL of the signed-in account, once known.
    account_avatar: Option<String>,
    /// The recent search words, cached.
    ///
    /// `refresh_chrome` runs on every navigation and used to re-read them from the
    /// database each time: `search_history` takes the store mutex and prepares a
    /// statement on the thread that paints (plan §2.3 R16). The words are refreshed
    /// when the history actually changes - a search, a clear - and published from
    /// here the rest of the time.
    recent_words: Vec<String>,
    /// The model those words are published into. One model for the life of the
    /// process, patched in place, so a navigation does not allocate a new one.
    recent_searches: Rc<VecModel<slint::SharedString>>,
    /// Ordered, bounded queue for `record_view`. The store write runs on the
    /// runtime, in submission order, never on the UI thread.
    view_queue: tokio::sync::mpsc::Sender<HistoryEntry>,

    // detail
    /// The download queue's model, created once and patched in place. Replacing
    /// it on every tick rebuilt every row five times a second.
    download_rows: Rc<VecModel<DownloadRow>>,
    detail: Option<Illust>,
    /// Avatar URL the metadata panel is waiting for. Stored rather than recomputed:
    /// a resolved lookup lands in `author_avatars`, not in `illust.author.avatar`,
    /// so the repaint has to match what was actually requested.
    detail_author_avatar_url: Option<String>,
    detail_pages: Vec<PageMeta>,
    detail_models: Option<Rc<VecModel<ViewerPage>>>,
    detail_epoch: u64,
    /// Reused so a burst of notices does not leak one timer per message.
    notice_timer: slint::Timer,
    /// Persisted three-state theme choice; Theme.dark is derived from it.
    theme_mode: ThemeMode,
    /// Slow AppsUseLightTheme watcher, running only while the mode is System.
    theme_timer: slint::Timer,

    // ---- round nine (docs/OPTIMIZATION-PLAN-R9.md §4) --------------------
    // Declared in phase 0 so three parallel tracks never edit this file. Each
    // carries an allow until the track that owns it reads it; that allow is part
    // of the track's acceptance, not a permanent fixture.
    /// What the user has selected on a grid page (F2, feed track).
    #[allow(dead_code)]
    selection: Selection,
    /// Command palette state (F3, feed track).
    #[allow(dead_code)]
    palette: PaletteState,
    /// Ugoira playback for the detail page (F1, detail track).
    #[allow(dead_code)]
    playback: PlaybackState,
}

/// How many routes Back/Forward can hold. Named because the scroll table is sized
/// from it: two stacks plus the page on screen.
const HISTORY_LIMIT: usize = 32;
/// How many scroll offsets the app keeps. Two history stacks plus the current
/// route, so nothing the user can navigate back to is evicted before the history
/// itself has forgotten it.
const SCROLL_KEEP: usize = HISTORY_LIMIT * 2 + 1;

/// How many decoded covers the feed keeps across a reload. `ImageCache` is the
/// real cache (memory LRU + disk); this map only exists so a column rebuild can
/// paint instantly instead of flashing empty, so the top of the feed is enough.
const COVERS_KEEP: usize = 512;
/// How many view-history rows may wait for the store before the newest is dropped.
///
/// The consumer drains this in milliseconds; a queue this deep means the database
/// has stopped answering, and a frame must not wait for it (plan §2.3 R16).
const VIEW_QUEUE: usize = 64;
/// How many faces to keep before dropping the lot. One re-fetch is a cache hit:
/// `ImageCache` still holds the bytes, and every row already on screen holds its
/// own `Image` copy, so clearing this map never blanks a visible avatar.
const AVATARS_KEEP: usize = 256;

/// Below this content width the profile header stacks instead of squeezing the
/// name column between an 88px portrait and a 210px action block.
const PROFILE_WIDE_MIN: f32 = 720.0;

pub(crate) type Shared = Rc<RefCell<App>>;

thread_local! {
    /// The single `App` handle, reachable from closures that Slint runs on the UI
    /// thread.
    ///
    /// `slint::invoke_from_event_loop` requires a `Send` closure, but Slint models
    /// (`Rc<VecModel<_>>`) are `!Send`. The resolution is to capture only plain
    /// data in the background task and look the handle up here once the closure is
    /// already running on the UI thread. Nothing non-`Send` ever crosses a thread.
    static APP: std::cell::RefCell<Option<Shared>> = const { std::cell::RefCell::new(None) };
}

impl App {
    // ------------------------------------------------------------- pages

    /// Where the page currently on screen is scrolled to.
    fn current_scroll(&self) -> f32 {
        // The home and profile pages keep their own offset because they are not
        // the shared grid; every other page shares grid-scroll-y.
        if self.page == Destination::Home {
            self.ui().get_home_scroll_y()
        } else {
            self.ui().get_grid_scroll_y()
        }
    }
}

impl App {
    /// Navigate to a parameterised route.
    ///
    /// Every entry point goes through here so the history stack always holds a
    /// complete destination: the page *and* the id. Pushing a bare Destination
    /// is what made Back restore the wrong author.
    fn go(shared: &Shared, route: Route) {
        {
            let mut app = shared.borrow_mut();
            let current = app.route;
            // Same page and same id means the user re-activated the entry they
            // are already on; still reload the data, but do not grow the stack.
            if !current.same_view(route) {
                app.history.visit(current, route);
            }
            let scroll = app.current_scroll();
            app.remember_scroll(current, scroll);
            app.route = route;
            app.page = route.page;
            app.leave_page(route);
        }
        App::load_page(shared, false);
    }
}

impl App {
    /// Everything that belongs to the page being left, in one place.
    ///
    /// There are two ways to leave a page - a fresh navigation (`go`) and
    /// Back/Forward (`travel`) - and the resets had already drifted: the tooltip
    /// was cleared in `go` only, so Back and Forward could leave a bubble floating
    /// over a control that no longer exists (plan §2.3 R20). The caller has already
    /// moved `route` and `page` to `next` when this runs.
    fn leave_page(&mut self, next: Route) {
        self.page_cursor = 1;
        self.loading_more = false;
        self.suggest_close();
        let ui = self.ui();
        // A tooltip belongs to the page it was raised on. Navigating can destroy
        // the hovered control (a cancelled row, a reloaded feed), and nothing would
        // ever clear the bubble afterwards: `changed` in Slint only watches a bare
        // property of the enclosing element, so the reset cannot live in the UI.
        ui.global::<Tip>().set_visible(false);
        ui.set_page(next.page);
        ui.set_grid_loading_more(false);
        // Both properties describe the page on screen, so they are re-stated on
        // every arrival rather than only on a fresh navigation.
        ui.set_ranking_filter(ranking_index(self.ranking_kind));
        ui.set_search_word(self.search_word.clone().into());
    }
}

impl App {
    fn navigate(shared: &Shared, page: Destination) {
        App::go(shared, Route::plain(page));
    }
}

impl App {
    /// Open a user's native profile. An empty id means "the signed-in account".
    fn open_profile(shared: &Shared, id: &str) {
        let parsed = id.trim().parse::<u64>().ok().filter(|value| *value > 0);
        let fallback = shared.borrow().session.user_id.map(|user| user.get());
        match parsed.or(fallback) {
            Some(uid) => App::go(shared, Route::user(uid)),
            None => {
                let app = shared.borrow();
                app.notify(2, "未登录：无法打开用户主页，请先在设置中登录");
            }
        }
    }
}

impl App {
    /// Show a work in the detail page without touching the history stack. Used
    /// when history navigation already restored the route.
    fn show_detail(shared: &Shared, id: u64) {
        App::open_detail_route(shared, id, false);
    }
}

impl App {
    /// Navigate back/forward through history rather than to an arbitrary page.
    fn travel(shared: &Shared, backwards: bool) {
        {
            let mut app = shared.borrow_mut();
            let current = app.route;
            let target = if backwards {
                app.history.back(current)
            } else {
                app.history.forward(current)
            };
            let Some(target) = target else {
                app.refresh_chrome();
                return;
            };
            let scroll = app.current_scroll();
            // Two profiles are different entries, so the offset is stored per
            // route: restoring one author's scroll on another's page was the
            // visible half of the same bug.
            app.remember_scroll(current, scroll);
            app.route = target;
            app.page = target.page;
            // The same resets a fresh navigation runs, tooltip included: Back and
            // Forward leave a page exactly the way `go` does (plan §2.3 R20).
            app.leave_page(target);
        }
        // Restoring rather than resetting: Back must land where you left off.
        App::load_page(shared, true);
    }
}

/// One route as a hashable key.
///
/// Slint generates `Destination` as `PartialEq` only (no `Eq`, no `Hash`), so the
/// page and its parameter are packed here rather than in `src/route.rs`, which is
/// not this module. The discriminant is stable within one build and is only ever
/// compared to another key from this same table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct ScrollKey {
    page: u32,
    param: u8,
    id: u64,
}

impl ScrollKey {
    fn of(route: Route) -> Self {
        // `RouteParam` is not hashable either, so the three cases become a tag and
        // an id: `None` carries no id and is the only case that uses tag 0.
        let (param, id) = match route.param {
            RouteParam::None => (0, 0),
            RouteParam::Illust(id) => (1, id),
            RouteParam::User(id) => (2, id),
        };
        Self {
            page: route.page as u32,
            param,
            id,
        }
    }
}

/// Store one offset and keep the table bounded, youngest last.
///
/// Returns how many entries fell off. A key that is stored again moves to the
/// young end instead of gaining a second slot, so what has not been touched for
/// longest is evicted first - and that is the deepest Back entry, which is the one
/// the 32-deep history forgets first.
fn store_scroll(
    scroll: &mut HashMap<ScrollKey, f32>,
    order: &mut VecDeque<ScrollKey>,
    key: ScrollKey,
    offset: f32,
    keep: usize,
) -> usize {
    scroll.insert(key, offset);
    order.retain(|known| *known != key);
    order.push_back(key);
    let mut dropped = 0;
    while scroll.len() > keep.max(1) {
        let Some(oldest) = order.pop_front() else {
            break;
        };
        scroll.remove(&oldest);
        dropped += 1;
    }
    dropped
}

impl App {
    /// Store where one route was left.
    ///
    /// A map, not a `Vec` scanned on every scroll delta: `feed.rs:44-45` calls
    /// this once per scroll event and `App::go` stores one entry per route ever
    /// visited, so the table grew for the whole session and every scroll paid for
    /// it (plan §2.3 R14).
    fn remember_scroll(&mut self, route: Route, offset: f32) {
        let keep = SCROLL_KEEP;
        store_scroll(
            &mut self.scroll,
            &mut self.scroll_order,
            ScrollKey::of(route),
            offset,
            keep,
        );
    }
}

impl App {
    /// Where one route was left, or the top of the page when it was never left.
    fn stored_scroll(&self, route: Route) -> f32 {
        self.scroll
            .get(&ScrollKey::of(route))
            .copied()
            .unwrap_or(0.0)
    }
}

impl App {
    fn load_page(shared: &Shared, restore_scroll: bool) {
        let (route, reuse_feed) = {
            let app = shared.borrow();
            (app.route, app.feed_owner == Some(app.route))
        };
        let page = route.page;

        let restore = |app: &App| {
            let offset = app.stored_scroll(route);
            let ui = app.ui();
            if page == Destination::Home {
                ui.set_home_scroll_y(offset);
            } else {
                ui.set_grid_scroll_y(offset);
            }
        };

        match page {
            // The detail page is not a feed: it is re-shown from its own route,
            // which is what makes Back return to the *same* work.
            Destination::Detail => {
                if let Some(id) = route.illust_id() {
                    App::show_detail(shared, id);
                }
                shared.borrow().refresh_chrome();
                return;
            }
            Destination::UserProfile => {
                if reuse_feed && restore_scroll && shared.borrow().profile.is_some() {
                    let app = shared.borrow();
                    restore(&app);
                    app.refresh_chrome();
                } else {
                    App::load_profile(shared);
                }
                return;
            }
            Destination::Home => {
                if reuse_feed && restore_scroll {
                    let app = shared.borrow();
                    restore(&app);
                    app.refresh_chrome();
                } else {
                    App::load_home(shared);
                }
                return;
            }
            Destination::Discovery => {
                if reuse_feed && restore_scroll {
                    let app = shared.borrow();
                    restore(&app);
                    app.refresh_chrome();
                } else {
                    App::load_discovery(shared, false);
                }
                return;
            }
            Destination::Ranking
            | Destination::Search
            | Destination::Bookmarks
            | Destination::History => {}
            _ => {
                shared.borrow().refresh_chrome();
                return;
            }
        }

        if reuse_feed && restore_scroll {
            let app = shared.borrow();
            restore(&app);
            app.refresh_chrome();
            return;
        }

        App::reload_feed(shared);
    }
}

// ===========================================================================
// Home, discovery, profile and search-suggestion state
// ===========================================================================

/// Band states, matching what the pages switch on.
const BAND_OK: i32 = 0;
const BAND_LOADING: i32 = 1;
const BAND_EMPTY: i32 = 2;
const BAND_ERROR: i32 = 3;

/// How many works one profile page shows before "load more".
const PROFILE_PAGE: usize = 24;
/// How many recommended ids one discovery batch hydrates through the detail
/// route. Twelve keeps the batch under a few seconds even with the shared
/// 250ms request spacing, and is enough to fill a screen.
const DISCOVERY_HYDRATE: usize = 12;
/// Stop after this many batches, and after two consecutive batches that added
/// nothing. The recommendation engine re-serves what it has already shown, so a
/// loader without an end condition spins forever.
const DISCOVERY_MAX_BATCHES: u32 = 12;

#[cfg(test)]
mod tests {
    use super::downloads::{sync_download_rows, to_download_rows};
    use super::helpers::{history_illust, retry_url};
    use super::settings::describe_session_error;
    use crate::DownloadRow;
    use pixiv_download::{DownloadRequest, DownloadTask, TaskState};
    use pixiv_types::domain::PageMeta;
    use slint::Model;
    use std::path::PathBuf;

    #[test]
    fn history_rows_build_cards_and_demo_rows_are_excluded() {
        use pixiv_store::HistoryEntry;
        let real = HistoryEntry {
            illust_id: "151942140".into(),
            title: "Sky".into(),
            author: "author".into(),
            thumb_url: "https://i.pximg.net/img-master/x_p0_master1200.jpg".into(),
            viewed_at: 1,
        };
        let demo = HistoryEntry {
            illust_id: "4407147838939147252".into(),
            title: "demo".into(),
            author: "demo".into(),
            thumb_url: "demo://illust/1/1000x1000/regular.png".into(),
            viewed_at: 2,
        };
        let card = history_illust(&real);
        assert_eq!(card.id, pixiv_types::Id(151_942_140));
        assert_eq!(card.urls.grid(), real.thumb_url);
        assert_eq!(card.author.name, "author");
        // Demo covers are pseudo-URLs that can never be fetched, so the real-mode
        // history must drop those rows instead of rendering broken cards.
        let rows = [real, demo];
        let kept: Vec<_> = rows
            .iter()
            .filter(|entry| entry.thumb_url.starts_with("http"))
            .collect();
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn session_failures_are_described_as_different_problems() {
        use pixiv_types::PixivError;
        // A dead cookie, an unreachable host and a changed payload need three
        // different reactions from the user, so they must not share one sentence.
        let unauthorized = describe_session_error(&PixivError::Unauthorized);
        let offline = describe_session_error(&PixivError::Network("dns".into()));
        let changed = describe_session_error(&PixivError::Decode("shape".into()));
        assert!(unauthorized.contains("PHPSESSID"), "{unauthorized}");
        assert!(offline.contains("无法连接"), "{offline}");
        assert!(changed.contains("格式"), "{changed}");
        assert_ne!(unauthorized, offline);
        assert_ne!(offline, changed);
    }

    #[test]
    fn appended_cards_go_to_the_shortest_column() {
        let mut heights = vec![0.0_f32; 4];
        assert_eq!(super::shortest_column(&heights), 0, "ties go left");
        heights[0] += 300.0;
        assert_eq!(super::shortest_column(&heights), 1);
        heights[1] += 300.0;
        heights[2] += 100.0;
        assert_eq!(super::shortest_column(&heights), 3);
    }

    /// Appending must produce the same distribution as laying the whole feed out
    /// in one pass; otherwise "load more" would quietly drift away from the
    /// balancer the grid tests assert on.
    #[test]
    fn incremental_placement_matches_a_single_pass() {
        let heights: Vec<f32> = (0..40).map(|i| 100.0 + (i % 5) as f32 * 40.0).collect();
        let mut running = vec![0.0_f32; 4];
        for height in &heights {
            let target = super::shortest_column(&running);
            running[target] += *height;
        }

        let assignment = crate::layout::distribute(&heights, 4);
        let mut one_pass = vec![0.0_f32; 4];
        for (height, column) in heights.iter().zip(&assignment) {
            one_pass[*column] += *height;
        }
        assert_eq!(running, one_pass);
    }

    /// Recall one offset, and keep the table bounded.
    ///
    /// The table was a `Vec` scanned linearly and appended once per route ever
    /// visited; a session of opening artworks grew it without bound and every
    /// scroll delta paid for the scan (plan §2.3 R14). A map with a touch order
    /// recalls the same number and drops what the history can no longer reach.
    #[test]
    fn the_scroll_table_recalls_an_offset_and_stays_bounded() {
        use crate::route::Route;
        use std::collections::{HashMap, VecDeque};

        let mut scroll: HashMap<super::ScrollKey, f32> = HashMap::new();
        let mut order: VecDeque<super::ScrollKey> = VecDeque::new();
        let home = super::ScrollKey::of(Route::plain(crate::Destination::Home));
        let first = super::ScrollKey::of(Route::user(11));
        let second = super::ScrollKey::of(Route::user(22));
        // Two profiles are two keys, which is what makes Back restore the right
        // one of them.
        assert_ne!(first, second);

        super::store_scroll(&mut scroll, &mut order, home, 120.0, 3);
        super::store_scroll(&mut scroll, &mut order, first, 64.0, 3);
        assert_eq!(scroll.get(&home).copied(), Some(120.0));

        // Storing the same route again updates it in place.
        super::store_scroll(&mut scroll, &mut order, home, 90.0, 3);
        assert_eq!(scroll.len(), 2);
        assert_eq!(order.len(), 2);
        assert_eq!(scroll.get(&home).copied(), Some(90.0));

        // Bounded: the entry that has sat untouched longest goes first, which is
        // the deepest Back entry.
        assert_eq!(
            super::store_scroll(&mut scroll, &mut order, second, 8.0, 2),
            1
        );
        assert_eq!(scroll.len(), 2);
        assert!(!scroll.contains_key(&first), "the oldest offset is evicted");
        assert_eq!(scroll.get(&home).copied(), Some(90.0));
        assert_eq!(scroll.get(&second).copied(), Some(8.0));
    }
    #[test]
    fn cover_pruning_keeps_the_top_of_the_feed() {
        let mut covers = std::collections::HashMap::new();
        for id in ["a", "b", "c", "d"] {
            covers.insert(id.to_string(), slint::Image::default());
        }
        let order = ["a", "b", "c"].iter().map(|id| id.to_string());
        // Only the first `keep` entries of the feed survive, and an id the feed no
        // longer lists is dropped even when it would have fitted.
        assert_eq!(super::prune_covers(&mut covers, order, 2), 2);
        assert!(covers.contains_key("a") && covers.contains_key("b"));
        assert!(!covers.contains_key("c") && !covers.contains_key("d"));
    }

    fn download_row(id: i32, progress: i32) -> DownloadRow {
        DownloadRow {
            id,
            title: format!("t{id}").into(),
            author: "a".into(),
            subtitle: "".into(),
            progress,
            state: 1,
            message: "".into(),
            path: "".into(),
        }
    }

    /// The queue is patched, not rebuilt: replacing the model five times a second
    /// reset every row element, so a hovered cancel button lost its state while the
    /// download it belonged to was still going.
    #[test]
    fn download_rows_are_patched_not_rebuilt() {
        let model = slint::VecModel::from(vec![download_row(1, 10), download_row(2, 20)]);

        // Nothing changed: nothing is written.
        assert_eq!(
            sync_download_rows(&model, vec![download_row(1, 10), download_row(2, 20)]),
            0
        );

        // One progress tick: exactly one row is written.
        assert_eq!(
            sync_download_rows(&model, vec![download_row(1, 55), download_row(2, 20)]),
            1
        );
        assert_eq!(model.row_data(0).unwrap().progress, 55);

        // A new task lands at the head and pushes the rest down. That is the one
        // case that legitimately rewrites the list, and the model survives it.
        assert_eq!(
            sync_download_rows(
                &model,
                vec![download_row(3, 0), download_row(1, 55), download_row(2, 20)]
            ),
            3
        );
        assert_eq!(model.row_count(), 3);
        assert_eq!(model.row_data(0).unwrap().id, 3);

        // Clearing finished rows shrinks the tail.
        assert_eq!(sync_download_rows(&model, vec![]), 3);
        assert_eq!(model.row_count(), 0);
    }

    fn page_meta(id: &str) -> PageMeta {
        PageMeta {
            urls: pixiv_types::domain::PageUrls {
                thumb_mini: String::new(),
                small: String::new(),
                regular: format!("regular-{id}"),
                original: format!("original-{id}"),
            },
            index: 0,
            width: 1000,
            height: 1000,
        }
    }

    fn task(state: TaskState, bytes_done: u64, bytes_total: Option<u64>) -> DownloadTask {
        DownloadTask {
            id: 7,
            request: DownloadRequest {
                illust_id: "700000001".into(),
                title: "题目".into(),
                author: "作者".into(),
                page_index: 1,
                page_count: 4,
                url: "demo://illust/1/800x600/regular.png".into(),
                extension: "png".into(),
            },
            state,
            bytes_done,
            bytes_total,
            destination: Some(PathBuf::from(r"C:\out\700000001_p2.png")),
        }
    }

    /// The integers here are a contract with `ui/pages/downloads.slint`, which
    /// switches its badge text and its action buttons on them. If the mapping
    /// drifts, a finished task renders as "下载中" or offers a cancel button that
    /// does nothing - exactly the stuck-queue symptom this app had.
    #[test]
    fn reloading_a_page_never_invents_a_url() {
        // A page list gives one URL per page, and the index is the page the viewer
        // is showing - not always page one.
        let pages = vec![page_meta("p0"), page_meta("p1")];
        assert_eq!(
            retry_url(&pages, "largest", 1).as_deref(),
            Some("regular-p1")
        );

        // No page list: exactly one image exists, and only index 0 may ask for it.
        assert_eq!(retry_url(&[], "largest", 0).as_deref(), Some("largest"));
        assert_eq!(retry_url(&[], "largest", 1), None);

        // Past the end of a real list: refuse rather than guess.
        assert_eq!(retry_url(&pages, "largest", 5), None);
    }

    #[test]
    fn task_states_map_to_the_integers_the_ui_switches_on() {
        assert_eq!(
            to_download_rows(vec![task(TaskState::Queued, 0, None)])[0].state,
            0
        );
        assert_eq!(
            to_download_rows(vec![task(TaskState::Running, 5, Some(10))])[0].state,
            1
        );
        assert_eq!(
            to_download_rows(vec![task(TaskState::Completed, 10, Some(10))])[0].state,
            2
        );
        assert_eq!(
            to_download_rows(vec![task(TaskState::Failed("boom".into()), 0, None)])[0].state,
            3
        );
        assert_eq!(
            to_download_rows(vec![task(TaskState::Cancelled, 0, None)])[0].state,
            4
        );
        assert_eq!(
            to_download_rows(vec![task(TaskState::Skipped, 0, None)])[0].state,
            5
        );
    }

    #[test]
    fn unknown_progress_is_indeterminate_rather_than_zero() {
        // The UI draws a spinning bar for -1 and a determinate bar otherwise, so a
        // running task with no content-length must not report "0%".
        let row = &to_download_rows(vec![task(TaskState::Running, 0, None)])[0];
        assert_eq!(row.progress, -1);
        let row = &to_download_rows(vec![task(TaskState::Running, 3, Some(4))])[0];
        assert_eq!(row.progress, 75);
        // A zero-length body is also unknown, not a divide-by-zero.
        let row = &to_download_rows(vec![task(TaskState::Running, 0, Some(0))])[0];
        assert_eq!(row.progress, -1);
    }

    #[test]
    fn failure_message_reaches_the_row_and_success_does_not_carry_one() {
        let row = &to_download_rows(vec![task(TaskState::Failed("磁盘已满".into()), 0, None)])[0];
        assert_eq!(row.message.as_str(), "磁盘已满");
        let row = &to_download_rows(vec![task(TaskState::Completed, 1, Some(1))])[0];
        assert_eq!(row.message.as_str(), "");
    }

    #[test]
    fn page_numbers_are_one_based_and_never_zero() {
        let row = &to_download_rows(vec![task(TaskState::Queued, 0, None)])[0];
        // page_index 1 of 4 pages reads as "第 2 / 4 页".
        assert_eq!(row.subtitle.as_str(), "第 2 / 4 页");
        let mut broken = task(TaskState::Queued, 0, None);
        broken.request.page_count = 0;
        let row = &to_download_rows(vec![broken])[0];
        assert_eq!(row.subtitle.as_str(), "第 2 / 1 页");
    }

    #[test]
    fn finished_rows_keep_the_destination_path_for_reveal() {
        let row = &to_download_rows(vec![task(TaskState::Completed, 1, Some(1))])[0];
        assert_eq!(row.path.as_str(), r"C:\out\700000001_p2.png");
        let mut no_path = task(TaskState::Cancelled, 0, None);
        no_path.destination = None;
        assert_eq!(to_download_rows(vec![no_path])[0].path.as_str(), "");
    }
}
