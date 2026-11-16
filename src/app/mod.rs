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
mod novel;
mod palette;
mod profile;
mod selection;
mod settings;
mod ugoira;

pub(crate) use self::{
    callbacks::install_callbacks, feed::*, helpers::*, lifecycle::*, novel::NovelState,
    palette::PaletteState, profile::*, selection::Selection, settings::*, ugoira::PlaybackState,
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
    AppWindow, Destination, DownloadRow, IllustItem, LocalRow, NovelBlock, NovelChapter, NovelRow,
    NovelView, ProfileStatItem, ReaderView, RecommendTagItem, RecommendUserItem, SuggestionItem,
    Theme, ThemeMode, Tip, ViewerPage,
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
    /// One id buffer per render, refilled in place (B11).
    ///
    /// The prune and the plan both need every id as a string, and a render used to
    /// materialise them twice and drop both. Refilling this keeps the strings, so a
    /// page of covers that is already held allocates nothing at all.
    cover_ids: RefCell<Vec<String>>,
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
    /// The local-record band's model, created once and patched in place (B7). The
    /// band used to be a brand-new model per completed page: every row was a fresh
    /// element, so the row under the pointer lost its hover and the focused row lost
    /// focus, once per downloaded page.
    download_local_rows: Rc<VecModel<LocalRow>>,
    /// The records that model was built from, so a publish can compute what changed
    /// without reading the model back (B7).
    download_local_drawn: RefCell<Vec<downloads::LocalRecord>>,
    /// Where each queued page will land, keyed by task id, mirrored from the last
    /// queue snapshot (B12).
    ///
    /// The "open file" gesture used to find its row through `DownloadManager::tasks`,
    /// which deep-clones every queued task: two hundred pages cost two hundred
    /// `DownloadTask` clones to read one path. The snapshot the tick already takes is
    /// the only place that has the paths, so it is where the index comes from.
    task_paths: RefCell<HashMap<u64, PathBuf>>,
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
    playback: PlaybackState,

    // ---- novels (round ten) ----
    /// Search, the landing page and the reader, as one state.
    novel: NovelState,
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
        // Leaving the reader must not lose the place: the throttled write only
        // fires every 2s, so a quick Back would otherwise land before the last
        // write. Flushing here also covers a chapter change, which navigates.
        if self.page == Destination::NovelReader && next.page != Destination::NovelReader {
            self.flush_novel_state();
            self.novel.reader.settings_open = false;
        }
        // Round nine (F1, O1): a detail page owns a playback clock and a page
        // model full of decoded bitmaps. Leaving it must give both back now. The
        // tick's own route check stops the clock within one frame, but the
        // bitmaps would otherwise live until the next visit, which is exactly the
        // cost O1 exists to remove. The epoch bump is what makes a decode still in
        // flight land dead instead of painting into a page nobody is looking at.
        if self.detail.is_some() && next.page != Destination::Detail {
            self.release_playback();
            self.detail_epoch = self.detail_epoch.wrapping_add(1);
            self.detail_models = None;
            ui.set_detail_pages(ModelRc::default());
        }
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
            RouteParam::Novel(id) => (3, id),
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

        // Split so a refresh can put back an offset it captured *before* the reload:
        // reload_feed zeroes the stored entry, so reading it afterwards would
        // restore the top of the page rather than the row the user was reading.
        let put_scroll = |app: &App, offset: f32| {
            let ui = app.ui();
            if page == Destination::Home {
                ui.set_home_scroll_y(offset);
            } else {
                ui.set_grid_scroll_y(offset);
            }
        };
        let restore = |app: &App| put_scroll(app, app.stored_scroll(route));

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
            // The novel surfaces own their own loading, and none of them uses the
            // shared grid: a novel has no columns, no masonry and no bookmark pill.
            Destination::Novels => {
                if reuse_feed && restore_scroll {
                    let app = shared.borrow();
                    restore(&app);
                    app.refresh_chrome();
                } else {
                    App::load_novels_page(shared);
                    shared.borrow().refresh_chrome();
                }
                return;
            }
            Destination::Novel => {
                let mut app = shared.borrow_mut();
                let offset = app.stored_scroll(route);
                app.page_cursor = 1;
                app.suggest_close();
                drop(app);
                App::load_novel_page(shared);
                let app = shared.borrow();
                restore(&app);
                let _ = offset;
                app.refresh_chrome();
                return;
            }
            Destination::NovelReader => {
                App::load_reader_page(shared);
                shared.borrow().refresh_chrome();
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

        // The one page whose own content changes while the user is elsewhere (B6):
        // record_view queues the work just opened to the same local table, so the
        // history sheet a reader comes back to is one view stale. It is re-read - and
        // the scroll is put back, because this is a refresh and not a navigation.
        if destination_rereads(page) {
            let offset = shared.borrow().stored_scroll(route);
            App::reload_feed(shared);
            {
                let app = shared.borrow();
                put_scroll(&app, offset);
            }
            let mut app = shared.borrow_mut();
            app.remember_scroll(route, offset);
            app.refresh_chrome();
            return;
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
    use super::helpers::{destination_rereads, history_illust, retry_url};
    use super::settings::describe_session_error;
    // Track B (docs/OPTIMIZATION-PLAN-R11.md §2.2) · the failure-honesty tests.
    use super::detail::{FollowState, detail_note, read_detail, toggle_allowed, viewer_page_count};
    use super::feed::{append_feed_page, rank_in, rank_table};
    use crate::{Destination, DownloadRow};
    use pixiv_download::{DownloadRequest, DownloadTask, TaskState};
    use pixiv_types::domain::PageMeta;
    use slint::Model;
    use std::path::PathBuf;

    /// B6 - the history page reads its table again on the way back.
    ///
    /// /load_page/'s reuse shortcut restored the scroll and returned, so the work the
    /// reader had just opened - queued to the same table by record_view - was missing
    /// until the page was left and re-entered. The plant that makes this red is
    /// [destination_rereads] answering false, which is the rule it had.
    #[test]
    fn b6_history_reads_its_table_again_and_the_other_pages_do_not() {
        assert!(
            destination_rereads(Destination::History),
            "the local table changed while the page was not on screen"
        );
        for page in [
            Destination::Home,
            Destination::Search,
            Destination::Ranking,
            Destination::Bookmarks,
            Destination::Discovery,
            Destination::Novels,
        ] {
            assert!(
                !destination_rereads(page),
                "{page:?} keeps the feed it already has"
            );
        }
    }

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
        let order = ["a", "b", "c"].map(str::to_string);
        // Only the first `keep` entries of the feed survive, and an id the feed no
        // longer lists is dropped even when it would have fitted.
        assert_eq!(super::prune_covers(&mut covers, &order, 2), 2);
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
            // Round nine: the row's measured rate. Zero is "no sample yet";
            // `eta_seconds` returns None for it, and no mapping test reads it.
            bytes_per_second: 0,
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

    // =====================================================================
    // Track B - failure honesty (docs/OPTIMIZATION-PLAN-R11.md §2.2 and §6)
    // =====================================================================

    /// One feed row, the way a loader hands it over.
    fn feed_row(id: u64, title: &str) -> super::Illust {
        let mut illust = history_illust(&pixiv_store::HistoryEntry {
            illust_id: id.to_string(),
            title: title.to_string(),
            author: "author".into(),
            thumb_url: "https://i.pximg.net/cover.jpg".into(),
            viewed_at: 0,
        });
        illust.page_count = 1;
        illust
    }

    /// T1 - a failed page list must not turn a 30-page work into a one-page one,
    /// and a failed profile read must not be drawn as 「未关注」.
    ///
    /// Both reads used to be collapsed at the source: unwrap_or_else(|_| Vec::new())
    /// and .unwrap_or(false). The assertion that fails on the old code is the one
    /// about 「unknown」: there was no state a failure could travel in.
    #[test]
    fn t1_a_failed_side_read_stays_a_failure() {
        use pixiv_types::PixivError;
        let mut illust = feed_row(9_001, "三十页");
        illust.page_count = 30;

        let payload = read_detail(
            illust.clone(),
            Err(PixivError::Network("timeout".into())),
            Ok(true),
        );
        assert!(payload.pages_failed, "the page-list failure is kept");
        assert_eq!(
            viewer_page_count(payload.illust.page_count, payload.pages.len()),
            30,
            "a failed page list still draws the page count the payload reported"
        );
        assert_eq!(
            payload.following,
            FollowState::Following,
            "one read failing says nothing about the other"
        );

        let payload = read_detail(illust, Ok(Vec::new()), Err(PixivError::Unauthorized));
        assert!(!payload.pages_failed, "an empty page list is not a failure");
        assert_eq!(
            payload.following,
            FollowState::Unknown,
            "an unread profile is unknown, never 未关注"
        );
        assert_eq!(payload.following.known(), None);
        let note = detail_note(false, payload.following).unwrap_or_default();
        assert!(note.contains("关注状态未知"), "{note}");
        assert_eq!(
            detail_note(false, FollowState::NotFollowing),
            None,
            "a healthy read says nothing at all"
        );
    }

    /// T2 - one sentence for a dead cookie, on every page that can show one.
    #[test]
    fn t2_an_expired_cookie_asks_for_a_new_login_on_every_page() {
        use pixiv_types::PixivError;
        let session = describe_session_error(&PixivError::Unauthorized);
        assert!(session.contains("重新登录"), "{session}");
        assert!(session.contains("PHPSESSID"), "{session}");
        // The novel page reads the app-wide sentence now, not one of its own.
        assert_eq!(
            super::novel::novel_error_text(&PixivError::Unauthorized),
            session,
            "the novel page and the grids say the same thing"
        );
    }

    /// C6 ripple - a flagged non-auth 4xx keeps Pixiv's own sentence.
    #[test]
    fn c6_a_flagged_4xx_keeps_pixivs_own_sentence() {
        use pixiv_types::PixivError;
        let flagged = PixivError::Http {
            status: 400,
            message: "这本书里含有非公开的标签".into(),
        };
        assert_eq!(
            super::novel::novel_error_text(&flagged),
            "这本书里含有非公开的标签",
            "the Http arm strips the status prefix and nothing else"
        );
        let missing = PixivError::Http {
            status: 404,
            message: String::new(),
        };
        assert!(
            super::novel::novel_error_text(&missing).contains("这本小说不存在"),
            "a 404 still reads like a novel page"
        );
    }

    /// T3 - a page that repeats a row adds no card and no count.
    ///
    /// The second copy used to be appended, which also re-pointed the id's cell at
    /// the new row: the card that was already on screen kept its placeholder cover
    /// for good.
    #[test]
    fn t3_a_repeated_page_adds_no_card_and_no_count() {
        let mut feed = vec![feed_row(1, "一"), feed_row(2, "二")];
        let added = append_feed_page(&mut feed, vec![feed_row(2, "二·重复"), feed_row(3, "三")]);
        assert_eq!(added, 1, "only the new id is added");
        assert_eq!(feed.len(), 3, "the count does not double");
        assert_eq!(feed[1].title, "二", "the row that was there keeps its cell");
        assert_eq!(feed[2].title, "三");
    }

    /// T5 - one rank table per render says exactly what a scan per card said.
    #[test]
    fn t5_the_rank_table_says_what_a_scan_per_card_said() {
        let items: Vec<super::Illust> = (1..=40).map(|id| feed_row(id, "x")).collect();
        let table = rank_table(super::Destination::Ranking, &items);
        for (index, illust) in items.iter().enumerate() {
            let scanned = items
                .iter()
                .position(|candidate| candidate.id == illust.id)
                .map(|position| position as i32 + 1)
                .unwrap_or(0);
            assert_eq!(
                rank_in(&table, illust),
                scanned,
                "the table and the per-card scan must agree"
            );
            assert_eq!(rank_in(&table, illust), index as i32 + 1);
        }
        // A page that does not rank has no numbers at all, and says 0.
        let home = rank_table(super::Destination::Home, &items);
        assert!(home.is_empty(), "no rank on a page that does not rank");
        assert_eq!(rank_in(&home, &items[0]), 0);
    }

    // =====================================================================
    // T8 and T13
    // =====================================================================

    /// T8 - a window that is gone yields no handle.
    ///
    /// It used to be upgrade().expect("window alive"), and every landing of a
    /// background result went through it, so a result arriving after the window was
    /// dropped panicked the event loop. The panic is what this test names on the old
    /// tree; on this one it is a None that on_ui turns into one dropped result.
    #[test]
    fn t8_a_window_that_is_gone_yields_no_handle() {
        use super::helpers::live_window;
        use slint::ComponentHandle;
        let live = crate::AppWindow::new().expect("a component, before it is shown");
        let weak = live.as_weak();
        assert!(
            live_window(&weak).is_some(),
            "a window that exists is handed out"
        );
        drop(live);
        assert!(
            live_window(&weak).is_none(),
            "a window that is gone must be None, not a panic"
        );
    }

    /// T13 - the second click of a double click is dropped, not sent.
    ///
    /// Two POSTs computed from one stale state used to disagree: the second undid
    /// the first, and the page kept whichever reply landed last.
    #[test]
    fn t13_a_second_click_is_dropped_while_the_first_is_in_flight() {
        let mut busy = false;
        assert!(toggle_allowed(busy), "the first click goes through");
        busy = true;
        assert!(
            !toggle_allowed(busy),
            "a click that arrives while one is in flight is dropped"
        );
        busy = false;
        assert!(
            toggle_allowed(busy),
            "the slot is free once the reply lands"
        );
    }

    // =====================================================================
    // G41 - every failed load reaches a page state or a log
    // =====================================================================

    /// The shapes in which a Result stops travelling.
    ///
    /// unwrap_or_else(|_ never looks at the error, .ok() throws it away, and
    /// let _ = discards the answer of a call that may have one.
    fn g41_shape(line: &str) -> Option<&'static str> {
        if line.contains("unwrap_or_else(|_") {
            return Some("unwrap_or_else(|_");
        }
        if line.contains(".ok()") {
            return Some(".ok()");
        }
        if line.contains("let _ = ") {
            return Some("let _ =");
        }
        None
    }

    /// Every .rs file under a directory, depth first.
    fn g41_sources(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                g41_sources(&path, out);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                out.push(path);
            }
        }
    }

    /// G41's allowlist: (path fragment, line fragment, why a failure here needs
    /// neither a page state nor a log).
    ///
    /// Every entry is a judgement about one line, not about a file: a new
    /// unwrap_or_else(|_ anywhere still fails the scan until somebody writes down
    /// why it is fine. An entry that stops matching anything also fails the test, so
    /// the table cannot rot into a blanket permission.
    const G41_ALLOWED: &[(&str, &str, &str)] = &[
        (
            "app/",
            "store.push_search(",
            "persisting one search word: the result line is the search that just ran, and nothing on the page depends on the write",
        ),
        (
            "app/callbacks/",
            "open::that(",
            "opening the browser or a folder: the OS answer is the whole result and there is no page to put it on",
        ),
        (
            "app/callbacks/downloads.rs",
            "Command::new(\"explorer\")",
            "starting Explorer: the answer is the window it opens",
        ),
        (
            "app/callbacks/",
            "let _ = app",
            "persisting a preference that is already applied in memory and drawn on the page, so a failed write only affects the next launch",
        ),
        (
            "app/callbacks/settings.rs",
            "let _ = page",
            "an unused binding in the settings fixture arm, not a dropped Result",
        ),
        (
            "app/callbacks/detail.rs",
            "std::env::var(",
            "reading the capture-fixture switch: an absent variable is the normal case",
        ),
        (
            "app/callbacks/feed.rs",
            ".ok()",
            "a continuation of the usize::try_from(index) conversion above it: an out-of-range Slint index is not a failed load",
        ),
        (
            "app/palette.rs",
            ".ok()",
            "index and id conversions of what Slint handed over: a value that does not convert is not a failed load",
        ),
        (
            "app/feed/batch.rs",
            "get_setting(",
            "reading the persisted bookmark-visibility preference: absent means the default",
        ),
        (
            "app/feed/fixture.rs",
            "std::env::var(",
            "reading the capture-fixture switch: an absent variable is the normal case",
        ),
        (
            "app/feed/home.rs",
            "let _ = reached_end",
            "an unused binding kept beside its comment, not a dropped Result",
        ),
        (
            "app/helpers.rs",
            "let _ = invoke_from_event_loop(",
            "the only failure is 'no event loop', which is the window-is-gone case on_ui answers",
        ),
        (
            "app/helpers.rs",
            ".ok()?",
            "an Option-returning decode: None is the caller's miss, and the caller draws the card's own failure state",
        ),
        (
            "app/ugoira.rs",
            ".ok()?",
            "a demo-frame decode: None means the film is not ready, which the page already draws",
        ),
        (
            "app/lifecycle.rs",
            "std::env::var(",
            "reading a capture switch or the explicit theme override: an absent variable is the normal case",
        ),
        (
            "app/lifecycle.rs",
            "novel_setting(",
            "reading the reader's saved preferences: absent or unreadable means the defaults, and the sliders clamp what they read",
        ),
        (
            "app/mod.rs",
            "parse::<u64>()",
            "parsing a route id typed by the user: an Option, not a Result from a loader",
        ),
        (
            "app/mod.rs",
            "let _ = offset",
            "an unused binding in the novel route arm, not a dropped Result",
        ),
        (
            "app/novel.rs",
            "std::env::var(",
            "the novel fixture switch: an absent variable is the normal case",
        ),
        (
            "app/novel.rs",
            "std::fs::read_to_string(",
            "the fixture file: unreadable means there is no fixture, and the caller asks Pixiv instead",
        ),
        (
            "app/novel.rs",
            "serde_json::from_str(",
            "the fixture JSON: unparsable means there is no fixture, and the caller asks Pixiv instead",
        ),
        (
            "app/novel.rs",
            "serde_json::from_value(",
            "the fixture body: a shape that does not parse means there is no fixture, which the caller already prices in",
        ),
        (
            "app/novel.rs",
            "from_str_radix(",
            "parsing a hex colour pair: an unparsable byte keeps the caller's default colour",
        ),
        (
            "app/novel.rs",
            "record_novel_read(",
            "a write, not a load: one row in the reading history",
        ),
        (
            "app/novel.rs",
            "save_novel_progress(",
            "a write, not a load: the position is already live in memory and on the page",
        ),
        (
            "app/novel.rs",
            "set_novel_setting(",
            "a write, not a load: the preference is already live in memory",
        ),
        (
            "app/settings.rs",
            "let _ = app",
            "persisting the theme choice, which is already applied to the window and to its watcher",
        ),
    ];

    /// G41 - every failed load either reaches a page state or a log.
    ///
    /// The scan walks src/app/**/*.rs, which is this track's whole contract: a
    /// Result dropped in a loader is a failure the user reads as a fact. Test
    /// modules are not scanned - they are not loaders - and a comment is not code.
    #[test]
    fn g41_a_failed_load_reaches_a_page_state_or_a_log() {
        for (scope, needle, reason) in G41_ALLOWED {
            assert!(
                !reason.trim().is_empty(),
                "the allowlist entry for {scope} {needle} carries no reason"
            );
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files: Vec<PathBuf> = Vec::new();
        g41_sources(&root.join("src").join("app"), &mut files);
        assert!(
            files.len() > 20,
            "the scan found {} files, which cannot be the application layer",
            files.len()
        );

        let mut unlisted: Vec<String> = Vec::new();
        let mut used = vec![false; G41_ALLOWED.len()];
        for file in &files {
            let Ok(text) = std::fs::read_to_string(file) else {
                continue;
            };
            let relative = file.to_string_lossy().replace('\\', "/");
            let lines: Vec<&str> = text.lines().collect();
            // The tests module is the tail of a file, and the attribute alone is not
            // enough: downloads.rs gates a static with it halfway through its real
            // code, and a scan that stopped at the first attribute would hand out a
            // free pass over the 150 lines after it (G41).
            let tests_from = lines
                .iter()
                .enumerate()
                .find_map(|(index, line)| {
                    if !line.trim_start().starts_with("#[cfg(test)]") {
                        return None;
                    }
                    let next = lines.get(index + 1)?.trim();
                    next.starts_with("mod tests").then_some(index)
                })
                .unwrap_or(usize::MAX);
            for (index, line) in lines.iter().enumerate() {
                if index >= tests_from {
                    continue;
                }
                let trimmed = line.trim();
                if trimmed.starts_with("//") || trimmed.is_empty() {
                    continue;
                }
                let Some(shape) = g41_shape(line) else {
                    continue;
                };
                match G41_ALLOWED.iter().position(|(scope, needle, _)| {
                    relative.contains(scope) && line.contains(needle)
                }) {
                    Some(slot) => used[slot] = true,
                    None => {
                        unlisted.push(format!("{relative}:{}: {trimmed}  [{shape}]", index + 1))
                    }
                }
            }
        }
        assert!(
            unlisted.is_empty(),
            "these lines drop a Result without a page state and without a log:\n  {}\neither fix the line or name it in G41_ALLOWED with the reason it needs neither",
            unlisted.join("\n  ")
        );
        let stale: Vec<&str> = G41_ALLOWED
            .iter()
            .zip(used)
            .filter(|(_, matched)| !matched)
            .map(|((_, needle, _), _)| *needle)
            .collect();
        assert!(
            stale.is_empty(),
            "these allowlist entries match nothing any more, so they are permissions nobody uses: {stale:?}"
        );
    }
}
