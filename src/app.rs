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

use slint::{
    ComponentHandle, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel,
    invoke_from_event_loop,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
use crate::route::{History, Route, RouteHistory};
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
enum Mode {
    Demo,
    Real,
}

/// Viewport cap for full-size artwork. A 4000px original drawn into a 900px box
/// wastes ~60MB of RGBA; 1600 keeps detail on wide windows without the waste.
const VIEWER_MAX_WIDTH: u32 = 1600;

struct App {
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
    scroll: Vec<(Route, f32)>,

    // responsive geometry, fed by what Slint measured
    measured_width: f32,
    geometry: GridGeometry,

    // feed
    items: Vec<Illust>,
    columns: Vec<Rc<VecModel<IllustItem>>>,
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

    // detail
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
}

/// Below this content width the profile header stacks instead of squeezing the
/// name column between an 88px portrait and a 210px action block.
const PROFILE_WIDE_MIN: f32 = 720.0;

type Shared = Rc<RefCell<App>>;

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
fn history_illust(entry: &HistoryEntry) -> Illust {
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

fn describe_session_error(error: &PixivError) -> String {
    match error {
        PixivError::Unauthorized => {
            "Pixiv 不接受这个 PHPSESSID：它可能已经退出登录或过期，请到浏览器重新复制".to_string()
        }
        PixivError::Network(message) => format!("无法连接 Pixiv：{message}"),
        PixivError::Http { status, message } => {
            if message.trim().is_empty() {
                format!("Pixiv 返回 HTTP {status}")
            } else {
                format!("Pixiv 返回 HTTP {status}：{message}")
            }
        }
        PixivError::RateLimited(after) => {
            format!("请求过于频繁，请等待 {:.0} 秒后重试", after.as_secs_f32())
        }
        PixivError::Decode(message) => {
            format!("Pixiv 的响应格式与预期不符（可能是接口变化）：{message}")
        }
        other => other.to_string(),
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
/// offline should not log the user out.
fn verify_stored_session(app: &Shared) {
    let (client, stored, data_dir, rt) = {
        let app = app.borrow();
        (
            app.client.clone(),
            app.session.clone(),
            app.data_dir.clone(),
            app.rt.clone(),
        )
    };
    if !stored.is_authenticated() {
        return;
    }
    let Some(client) = client else {
        return;
    };

    rt.spawn(async move {
        let outcome = client.account().await;
        on_ui(move |shared| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            match outcome {
                Ok(account) => {
                    let refreshed = Session {
                        phpsessid: stored.phpsessid.clone(),
                        user_id: Some(account.id),
                        user_name: Some(account.name.clone()),
                        csrf_token: account.csrf_token.clone(),
                    };
                    client.set_session(refreshed.clone());
                    let _ = crate::session::save(&data_dir, &refreshed);
                    app.session = refreshed;
                    app.mode = Mode::Real;
                    app.account_avatar = account.avatar.clone();
                    {
                        let ui = app.ui();
                        ui.set_authenticated(true);
                        ui.set_demo_mode(false);
                        ui.set_session_user(account.name.clone().into());
                    }
                    app.refresh_chrome();
                    if let Some(url) = app.account_avatar.clone() {
                        app.spawn_avatars(vec![url]);
                    }
                }
                Err(PixivError::Unauthorized) => {
                    // The session is dead: forget it rather than pretending.
                    crate::session::clear(&data_dir);
                    app.session = Session::default();
                    app.mode = Mode::Demo;
                    app.feed_owner = None;
                    {
                        let ui = app.ui();
                        ui.set_authenticated(false);
                        ui.set_session_user("".into());
                        ui.set_demo_mode(true);
                    }
                    app.refresh_chrome();
                    app.notify(2, "保存的登录已失效，已切回演示模式；请重新登录");
                    drop(app);
                    App::load_page(shared, false);
                }
                Err(error) => {
                    // Offline is not the same as signed out.
                    app.notify(
                        2,
                        format!("暂未验证登录状态：{}", describe_session_error(&error)),
                    );
                }
            }
        });
    });
}

/// Run `f` with the app handle on the UI thread.
fn on_ui<F>(f: F)
where
    F: FnOnce(&Shared) + Send + 'static,
{
    let _ = invoke_from_event_loop(move || {
        APP.with(|slot| {
            let borrowed = slot.borrow();
            if let Some(shared) = borrowed.as_ref() {
                f(shared);
            }
        });
    });
}

/// Raw pixels of a decoded image. `Send`, unlike `slint::Image`, and shares the
/// cache's `Arc<[u8]>` instead of copying it.
struct RawImage {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
}

impl RawImage {
    fn to_slint(&self) -> slint::Image {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(self.width, self.height);
        let expected = (self.width as usize) * (self.height as usize) * 4;
        let bytes = buffer.make_mut_bytes();
        let len = expected.min(self.rgba.len());
        bytes[..len].copy_from_slice(&self.rgba[..len]);
        slint::Image::from_rgba8(buffer)
    }
}

pub fn run() -> Result<(), slint::PlatformError> {
    let rt =
        Arc::new(Runtime::new().map_err(|e| slint::PlatformError::Other(format!("tokio: {e}")))?);
    let ui = AppWindow::new()?;
    let app: Shared = Rc::new(RefCell::new(App::new(ui.as_weak(), rt)));
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));

    install_callbacks(&app);
    start_timers(&app);
    // The stored cookie is only a claim until Pixiv confirms it.
    verify_stored_session(&app);

    // The window is not mapped yet, so a size applied here would be clamped by
    // the window manager. A single-shot timer after the loop starts is the
    // reliable moment to size it (and it is also what makes screenshot captures
    // deterministic).
    if std::env::var("PIXIV_DESKTOP_SIZE").is_ok() {
        // Applied a few times because the scale factor is not known to Slint
        // until after the first frame is composed; the last application wins and
        // the earlier ones are what make the window map at roughly the right
        // place instead of starting tiny.
        for delay in [120u64, 600, 1500] {
            let shared = app.clone();
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(delay),
                move || apply_window_size(&shared),
            );
            std::mem::forget(timer);
        }
    }

    // Tooltips only appear on hover and this host has no input automation, so
    // the one thing the UI cannot show on its own is that the bubble is drawn
    // above the page at all. PIXIV_DESKTOP_TIP=1 raises one over the back button
    // so a capture can prove it; nothing else ever sets this.
    ui.set_debug_tip(std::env::var_os("PIXIV_DESKTOP_TIP").is_some());

    // Optional deep link for smoke-testing and visual regression captures:
    //   PIXIV_DESKTOP_START_PAGE=settings
    //   PIXIV_DESKTOP_START_ILLUST=700000003
    let start = std::env::var("PIXIV_DESKTOP_START_PAGE").ok();
    match start.as_deref() {
        Some("discovery") => App::navigate(&app, Destination::Discovery),
        Some("ranking") => App::navigate(&app, Destination::Ranking),
        Some("search") => App::navigate(&app, Destination::Search),
        Some("bookmarks") => App::navigate(&app, Destination::Bookmarks),
        Some("history") => App::navigate(&app, Destination::History),
        Some("downloads") => App::navigate(&app, Destination::Downloads),
        Some("settings") => App::navigate(&app, Destination::Settings),
        _ => App::navigate(&app, Destination::Home),
    }
    if let Ok(id) = std::env::var("PIXIV_DESKTOP_START_USER") {
        App::open_profile(&app, &id);
    }
    if let Ok(id) = std::env::var("PIXIV_DESKTOP_START_ILLUST") {
        App::open_detail(&app, &id);
    }

    {
        let a = app.borrow();
        a.start_download_worker();
    }
    // One honest line about the state the app is in, deferred until the event loop
    // is up so the auto-dismiss timer is actually scheduled. It also doubles as the
    // visual proof that the shell-level notification bar works.
    {
        let shared = app.clone();
        let hello = slint::Timer::default();
        hello.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(400),
            move || {
                let Ok(app) = shared.try_borrow() else {
                    return;
                };
                if !app.session.is_authenticated() {
                    app.notify(
                        2,
                        "未登录：可以浏览公开作品；收藏、关注与下载需要先在设置页完成登录",
                    );
                }
            },
        );
        std::mem::forget(hello);
    }

    ui.run()
}

/// Honour `PIXIV_DESKTOP_SIZE=1140x886` so screenshot captures are deterministic
/// instead of depending on the window manager's idea of a nice default.
///
/// The value is interpreted as *logical* pixels and converted with the live scale
/// factor: `set_size` with a logical size is resolved against the scale factor
/// known at that moment, which is still 1.0 before the window is mapped, so a
/// 1140x886 request came out as 1140x886 *physical* (a 760x590 layout at 150%
/// DPI). Converting explicitly keeps the captured layout at the documented size.
fn apply_window_size(app: &Shared) {
    let Ok(spec) = std::env::var("PIXIV_DESKTOP_SIZE") else {
        return;
    };
    let Some((w, h)) = spec.split_once('x') else {
        return;
    };
    let (Ok(w), Ok(h)) = (w.trim().parse::<f32>(), h.trim().parse::<f32>()) else {
        return;
    };
    if w < 320.0 || h < 240.0 {
        return;
    }
    let ui = app.borrow().ui();
    let window = ui.window();
    let scale = window.scale_factor().max(1.0);
    window.set_size(slint::PhysicalSize::new(
        (w * scale).round() as u32,
        (h * scale).round() as u32,
    ));
}

impl App {
    fn new(ui: slint::Weak<AppWindow>, rt: Arc<Runtime>) -> Self {
        let data_dir = data_dir();
        let _ = std::fs::create_dir_all(&data_dir);

        // A broken database must never stop the app from starting.
        let store = Store::open(&data_dir.join("pixiv-desktop.db"))
            .or_else(|_| Store::open_in_memory())
            .expect("in-memory store always opens");

        // Cache budgets, defined once. Settings renders these numbers, and before
        // this the page showed a second hard-coded 192/1024 that merely happened to
        // match; changing the real budget would have left the UI describing a limit
        // that was not in force.
        let cache = ImageCache::new(CacheConfig {
            memory_budget_bytes: CACHE_MEMORY_MB * 1024 * 1024,
            disk_budget_bytes: CACHE_DISK_MB * 1024 * 1024 * 1024,
            disk_dir: Some(data_dir.join("images")),
            concurrency: COVER_CONCURRENCY,
        });

        let download_dir = persisted(&store, "download_dir")
            .map(PathBuf::from)
            .unwrap_or_else(downloads_dir);
        let _ = std::fs::create_dir_all(&download_dir);
        let template = persisted(&store, "filename_template")
            .unwrap_or_else(|| "{illust_id}_p{page}_{title}".to_string());

        let downloads = DownloadManager::new(DownloadConfig {
            directory: download_dir.clone(),
            concurrency: 3,
            filename_template: template.clone(),
        });

        let session = crate::session::load(&data_dir);
        let (client, mode) = match Client::new(ClientConfig::default(), session.clone()) {
            Ok(client) if session.is_authenticated() => (Some(client), Mode::Real),
            Ok(client) => (Some(client), Mode::Demo),
            Err(_) => (None, Mode::Demo),
        };

        // One persisted value, three states. PIXIV_DESKTOP_THEME stays as an
        // explicit override for the capture harness (it must be able to force a
        // theme without touching the user's saved choice).
        let theme_mode = match std::env::var("PIXIV_DESKTOP_THEME").ok().as_deref() {
            Some("dark") => ThemeMode::Dark,
            Some("light") => ThemeMode::Light,
            _ => crate::theme::mode_from_setting(
                persisted(&store, crate::theme::SETTING_KEY).as_deref(),
            ),
        };

        if let Some(ui) = ui.upgrade() {
            crate::theme::apply(&ui, theme_mode);
            ui.set_demo_mode(mode == Mode::Demo);
            ui.set_authenticated(session.is_authenticated());
            ui.set_session_user(session.user_name.clone().unwrap_or_default().into());
            ui.set_download_directory(download_dir.display().to_string().into());
            ui.set_download_template(template.clone().into());
            ui.set_template_input(template.into());
            ui.set_app_version(env!("CARGO_PKG_VERSION").into());
            ui.set_cache_memory_mb(CACHE_MEMORY_MB as i32);
            ui.set_cache_disk_mb(CACHE_DISK_MB as i32);
            ui.set_ranking_filters(ModelRc::from(Rc::new(VecModel::from(
                RankingKind::ALL
                    .iter()
                    .map(|kind| slint::SharedString::from(kind.label()))
                    .collect::<Vec<_>>(),
            ))));
            ui.set_grid_action_label("前往设置".into());
        }

        Self {
            ui,
            rt,
            store,
            cache,
            demo: DemoCatalog::new(),
            client,
            mode,
            session,
            downloads,
            data_dir,
            download_dir,
            page: Destination::Home,
            route: Route::plain(Destination::Home),
            history: History::new(32),
            feed_owner: None,
            scroll: Vec::new(),
            measured_width: 0.0,
            geometry: GridGeometry::for_width(858.0),
            items: Vec::new(),
            columns: Vec::new(),
            index: HashMap::new(),
            covers: HashMap::new(),
            loader: CoverLoader::new(),
            epoch: 0,
            page_cursor: 1,
            reached_end: false,
            loading_more: false,
            search_word: String::new(),
            ranking_kind: RankingKind::Daily,
            home: HomeBands::loading(),
            discovery: DiscoveryBands::default(),
            suggest: SuggestState::default(),
            profile: None,
            avatars: HashMap::new(),
            avatar_loading: HashSet::new(),
            author_avatars: HashMap::new(),
            author_avatar_lookup: HashSet::new(),
            account_avatar: None,
            detail: None,
            detail_author_avatar_url: None,
            detail_pages: Vec::new(),
            detail_models: None,
            detail_epoch: 0,
            notice_timer: slint::Timer::default(),
            theme_mode,
            theme_timer: slint::Timer::default(),
        }
    }

    fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("window alive")
    }
    /// Keep `Theme.system-dark` in step with Windows while the user asked to
    /// follow it.
    ///
    /// Reads one registry DWORD every five seconds and *only* while the mode is
    /// `System`: picking Light or Dark stops the timer, so the poll costs nothing
    /// in the common case. Slint 1.18 exposes no colour-scheme notification and no
    /// raw window procedure, so this is the honest substitute — documented in
    /// `docs/UI-NOTES.md` rather than hidden.
    fn watch_system_theme(&self, mode: ThemeMode) {
        if mode != ThemeMode::System {
            self.theme_timer.stop();
            return;
        }
        let weak = self.ui.clone();
        self.theme_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(5),
            move || {
                if let Some(ui) = weak.upgrade() {
                    let dark = crate::theme::system_is_dark();
                    if ui.global::<Theme>().get_system_dark() != dark {
                        ui.global::<Theme>().set_system_dark(dark);
                    }
                }
            },
        );
    }

    // ------------------------------------------------------------- chrome

    fn refresh_chrome(&self) {
        let ui = self.ui();
        ui.set_page(self.page);
        // The top-bar avatar comes from the account payload, not from the name:
        // a letter in a circle is not an avatar.
        if let Some(url) = self.account_avatar.as_ref() {
            match self.avatars.get(url) {
                Some(image) => {
                    ui.set_account_avatar(image.clone());
                    ui.set_account_avatar_state(BAND_OK);
                }
                None => ui.set_account_avatar_state(BAND_LOADING),
            }
        } else {
            ui.set_account_avatar_state(BAND_EMPTY);
        }
        self.publish_suggestions();
        ui.set_can_go_back(self.history.can_go_back());
        ui.set_can_go_forward(self.history.can_go_forward());
        ui.set_demo_mode(self.mode == Mode::Demo);
        ui.set_authenticated(self.session.is_authenticated());
        ui.set_session_user(self.session.user_name.clone().unwrap_or_default().into());
        ui.set_status_text(self.status_line().into());
        ui.set_cache_summary(self.cache_summary().into());
        self.refresh_recent_searches();
    }

    /// Push the stored search history into the UI.
    ///
    /// The app has always written every submitted keyword to `search_history` and
    /// never read it back, so the table grew and the user could not reach any of it.
    /// The empty-search state renders these as chips that re-run the search.
    fn refresh_recent_searches(&self) {
        let words = self.store.search_history(8).unwrap_or_default();
        let labels: Vec<slint::SharedString> =
            words.into_iter().map(slint::SharedString::from).collect();
        self.ui()
            .set_recent_searches(ModelRc::from(Rc::new(VecModel::from(labels))));
    }

    fn cache_summary(&self) -> String {
        let stats = self.cache.stats();
        let lookups = stats.memory_hits + stats.disk_hits;
        let requests = lookups + stats.network_loads;
        // The denominator used to be `max(1)`, so an untouched cache read "命中 0/1"
        // - a ratio that describes nothing. With no requests yet, say so.
        let hit_rate = match lookups
            .checked_mul(100)
            .and_then(|scaled| (requests != 0).then(|| scaled / requests))
        {
            Some(percent) => format!("命中 {lookups}/{requests}（{percent}%）"),
            None => "尚未请求".to_string(),
        };
        format!(
            "内存 {} · 磁盘 {} · {}",
            human_bytes(stats.memory_bytes as u64),
            human_bytes(stats.disk_bytes),
            hit_rate
        )
    }

    /// The one page-specific fact the status bar shows.
    ///
    /// Connection state (demo / signed-in / read-only) is *not* repeated here:
    /// the shell renders it once from the properties it already holds. The old
    /// line carried the mode and the auth state too, which is how one screen ended
    /// up saying "演示模式" in three separate places.
    fn status_line(&self) -> String {
        match self.page {
            Destination::Home
            | Destination::Discovery
            | Destination::Ranking
            | Destination::Search
            | Destination::Bookmarks
            | Destination::History => format!("{} 项作品", self.items.len()),
            _ => String::new(),
        }
    }

    /// Show a shell-level notification. The shell renders it above the status bar
    /// so it is visible on every page — a bookmark result happens on the detail
    /// page, a cache result on the settings page, and a page-local bar meant the
    /// other one was silently invisible.
    ///
    /// Auto-dismisses after a few seconds, but only if the message has not been
    /// replaced in the meantime (compared by value, so a second identical notice
    /// still gets its own full timeout).
    fn notify(&self, kind: i32, message: impl Into<slint::SharedString>) {
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
            app.page_cursor = 1;
            app.loading_more = false;
            app.suggest_close();
            let ui = app.ui();
            // A tooltip belongs to the page it was raised on. Navigating can
            // destroy the hovered control (a cancelled row, a reloaded feed), and
            // nothing would ever clear the bubble afterwards: `changed` in Slint
            // only watches a bare property of the enclosing element, so the reset
            // cannot live in the UI.
            ui.global::<Tip>().set_visible(false);
            ui.set_page(route.page);
            ui.set_grid_loading_more(false);
            ui.set_ranking_filter(ranking_index(app.ranking_kind));
            ui.set_search_word(app.search_word.clone().into());
        }
        App::load_page(shared, false);
    }

    fn navigate(shared: &Shared, page: Destination) {
        App::go(shared, Route::plain(page));
    }

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

    /// Show a work in the detail page without touching the history stack. Used
    /// when history navigation already restored the route.
    fn show_detail(shared: &Shared, id: u64) {
        App::open_detail_route(shared, id, false);
    }

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
            app.page_cursor = 1;
            app.loading_more = false;
            app.suggest_close();
            let ui = app.ui();
            ui.set_page(target.page);
            ui.set_grid_loading_more(false);
            ui.set_search_word(app.search_word.clone().into());
        }
        // Restoring rather than resetting: Back must land where you left off.
        App::load_page(shared, true);
    }

    fn remember_scroll(&mut self, route: Route, offset: f32) {
        if let Some(slot) = self.scroll.iter_mut().find(|(r, _)| *r == route) {
            slot.1 = offset;
        } else {
            self.scroll.push((route, offset));
        }
    }

    fn stored_scroll(&self, route: Route) -> f32 {
        self.scroll
            .iter()
            .find(|(r, _)| *r == route)
            .map(|(_, offset)| *offset)
            .unwrap_or(0.0)
    }

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

    /// Fetch page 1 of whatever the current destination shows, then render.
    fn reload_feed(shared: &Shared) {
        let (page, mode, epoch) = {
            let mut app = shared.borrow_mut();
            app.epoch += 1;
            app.reached_end = false;
            app.page_cursor = 1;
            // Covers requested for the feed we are leaving no longer belong to
            // any visible page; let the new feed re-plan them.
            app.loader.reset();
            let epoch = app.epoch;
            let ui = app.ui();
            ui.set_grid_state(1);
            ui.set_grid_loading_more(false);
            ui.set_grid_count_label("".into());
            (app.page, app.mode, epoch)
        };

        // Reset the visible page's scroll offset: this is new content.
        {
            let mut app = shared.borrow_mut();
            let ui = app.ui();
            if page == Destination::Home {
                ui.set_home_scroll_y(0.0);
            } else {
                ui.set_grid_scroll_y(0.0);
            }
            let route = app.route;
            app.remember_scroll(route, 0.0);
        }

        // "Search" with no keyword is not a search. It used to reach the API with an
        // empty string (which answered with an unrelated default listing) and, in
        // demo mode, show the whole catalogue under a "搜索" heading. Both read as
        // "these are your results". Prompt for input instead, and offer the recent
        // searches that the app already records in SQLite.
        {
            let mut app = shared.borrow_mut();
            if page == Destination::Search && app.search_word.trim().is_empty() {
                app.feed_owner = Some(app.route);
                app.set_tags(Vec::new());
                let ui = app.ui();
                ui.set_grid_state(2);
                ui.set_grid_state_title("搜索作品".into());
                ui.set_grid_state_glyph("search".into());
                ui.set_grid_message("在上方搜索框输入关键词后按回车。".into());
                ui.set_grid_action_label("".into());
                ui.set_grid_count_label("".into());
                ui.set_grid_columns(ModelRc::default());
                app.columns.clear();
                app.index.clear();
                app.reached_end = true;
                app.refresh_chrome();
                return;
            }
        }

        // History never leaves this machine, so it is answered here rather than
        // in a spawned task. It was previously wired to a stub that returned an
        // empty vector, which is why the page stayed blank even though every
        // opened artwork was recorded in SQLite.
        if page == Destination::History {
            let mut app = shared.borrow_mut();
            let entries = app.store.view_history(60).unwrap_or_default();
            let items: Vec<Illust> = entries
                .iter()
                // Rows written by earlier builds in demo mode carry `demo://`
                // covers; they cannot be fetched, so they are dropped rather than
                // rendered as permanently broken cards.
                .filter(|entry| entry.thumb_url.starts_with("http"))
                .map(history_illust)
                .collect();
            app.feed_owner = Some(app.route);
            app.reached_end = true;
            app.set_tags(Vec::new());
            if items.is_empty() {
                let ui = app.ui();
                ui.set_grid_state(2);
                ui.set_grid_state_title("还没有浏览记录".into());
                ui.set_grid_state_glyph("history".into());
                ui.set_grid_message("打开任意作品后，它会按时间出现在这里。".into());
                ui.set_grid_action_label("".into());
                ui.set_grid_count_label("".into());
                ui.set_grid_columns(ModelRc::default());
                app.columns.clear();
                app.index.clear();
            } else {
                app.render_feed(items, false);
            }
            app.refresh_chrome();
            return;
        }

        match mode {
            Mode::Demo => {
                let app = shared.borrow();
                let items = app.demo_items(1);
                let tags = if matches!(page, Destination::Discovery | Destination::Home) {
                    app.demo.trending_tags()
                } else {
                    Vec::new()
                };
                drop(app);
                let mut app = shared.borrow_mut();
                app.feed_owner = Some(app.route);
                app.set_tags(tags);
                app.render_feed(items, false);
                app.refresh_chrome();
            }
            Mode::Real => {
                let app = shared.borrow();
                let client = app.client.clone();
                let rt = app.rt.clone();
                let word = app.search_word.clone();
                let kind = app.ranking_kind;
                let session = app.session.clone();
                drop(app);

                // The history page needs no network at all, so it is answered
                // before a task is spawned.
                rt.spawn(async move {
                    let result = match (client, page) {
                        (Some(client), _) => match page {
                            Destination::Ranking => client
                                .ranking(kind, 1)
                                .await
                                .map(|rows| rows.into_iter().map(|r| r.illust).collect()),
                            Destination::Search => client
                                .search(&SearchQuery::new(&word))
                                .await
                                .map(|r| r.items),
                            // Never fall back to a default id: user 0 does not
                            // exist, so the request would either 404 or answer
                            // somebody else's page.
                            Destination::Bookmarks => match session.user_id {
                                Some(user) => {
                                    client.bookmarks(user, false, 1).await.map(|r| r.items)
                                }
                                None => Err(PixivError::Unauthorized),
                            },
                            // History is local, so there is nothing to ask
                            // Pixiv. It used to return an empty vector, which made
                            // the page permanently blank even though every opened
                            // artwork was being written to SQLite.
                            Destination::History => Ok(Vec::new()),
                            _ => client
                                .ranking(RankingKind::Daily, 1)
                                .await
                                .map(|rows| rows.into_iter().map(|r| r.illust).collect::<Vec<_>>()),
                        },
                        (None, _) => Err(pixiv_types::PixivError::Api(
                            "没有可用的网络客户端".to_string(),
                        )),
                    };
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        if app.epoch != epoch || app.page != page {
                            // A newer navigation already owns the feed.
                            return;
                        }
                        match result {
                            Ok(items) => {
                                app.feed_owner = Some(app.route);
                                app.render_feed(items, false);
                            }
                            Err(error) => app.show_feed_error(&error.to_string()),
                        }
                        app.refresh_chrome();
                    });
                });
            }
        }
    }

    fn show_feed_error(&mut self, message: &str) {
        let ui = self.ui();
        ui.set_grid_state(3);
        ui.set_grid_state_title("载入失败".into());
        ui.set_grid_state_glyph("warning".into());
        ui.set_grid_message(message.into());
        ui.set_grid_action_label("重试".into());
        ui.set_grid_columns(ModelRc::default());
        self.columns.clear();
        self.index.clear();
    }

    fn set_tags(&self, tags: Vec<String>) {
        let labels: Vec<slint::SharedString> = tags.into_iter().map(Into::into).collect();
        self.ui()
            .set_grid_tags(ModelRc::from(Rc::new(VecModel::from(labels))));
    }

    fn demo_items(&self, page: u32) -> Vec<Illust> {
        match self.page {
            Destination::Ranking => self
                .demo
                .ranking(self.ranking_kind, page)
                .into_iter()
                .map(|row| row.illust)
                .collect(),
            Destination::Search => {
                let mut query = SearchQuery::new(&self.search_word);
                query.page = page;
                self.demo.search(&query).items
            }
            Destination::Bookmarks => self.demo.bookmarks().items,
            _ => {
                let all = self.demo.feed();
                if page <= 1 {
                    all
                } else {
                    // Demo catalogue is finite; page 2 is deliberately empty so
                    // "load more" terminates honestly.
                    Vec::new()
                }
            }
        }
    }

    // ------------------------------------------------------------- feed

    fn render_feed(&mut self, items: Vec<Illust>, append: bool) {
        let mut all = if append {
            std::mem::take(&mut self.items)
        } else {
            Vec::new()
        };
        if items.is_empty() && all.is_empty() {
            let ui = self.ui();
            ui.set_grid_state(2);
            // The headline names what came back empty; the message says what to do
            // about it. One generic sentence for every page read as filler.
            let (headline, glyph, hint) = match self.page {
                Destination::Search => {
                    ("没有匹配的作品", "search", "换个关键词，或用标签再试一次。")
                }
                Destination::Bookmarks => (
                    "收藏夹是空的",
                    "star",
                    "在作品详情页点击「收藏」后，会出现在这里。",
                ),
                Destination::History => (
                    "还没有浏览记录",
                    "history",
                    "本机看过的作品会记录在这里，仅保存在本地。",
                ),
                _ => ("这里还没有内容", "", "试试刷新，或换一个标签。"),
            };
            ui.set_grid_state_title(headline.into());
            ui.set_grid_state_glyph(glyph.into());
            ui.set_grid_message(hint.into());
            ui.set_grid_action_label("".into());
            ui.set_grid_count_label("".into());
            ui.set_grid_columns(ModelRc::default());
            self.columns.clear();
            self.index.clear();
            self.reached_end = true;
            return;
        }
        all.extend(items);
        self.items = all;
        self.rebuild_columns();
    }

    /// Recompute the column buckets from `self.items` for the current geometry.
    ///
    /// This is the expensive path (it rebuilds every row model), so it is only
    /// called when the *column count* changes or when the feed itself changes.
    /// A pure width change keeps the buckets and only updates `card-width`, which
    /// is one property assignment — that is what stops a window drag from
    /// rebuilding the grid on every frame.
    fn rebuild_columns(&mut self) {
        let geometry = self.geometry;
        let heights: Vec<f32> = self
            .items
            .iter()
            .map(|illust| {
                layout::card_height(
                    geometry.card_width,
                    layout::clamped_aspect(illust.width, illust.height),
                )
            })
            .collect();
        let assignment = layout::distribute(&heights, geometry.columns);
        let buckets = layout::bucket(&assignment, geometry.columns);

        let mut models = Vec::with_capacity(buckets.len());
        let mut index = HashMap::new();
        for (column, rows) in buckets.iter().enumerate() {
            let rows: Vec<IllustItem> = rows
                .iter()
                .enumerate()
                .map(|(row, item_index)| {
                    let illust = &self.items[*item_index];
                    let mut item = illust_to_item(illust, self.rank_of(illust));
                    let key = illust.id.to_string();
                    if let Some(image) = self.covers.get(&key) {
                        item.cover = image.clone();
                        item.state = 0;
                    }
                    index.insert(key, (column, row));
                    item
                })
                .collect();
            models.push(Rc::new(VecModel::from(rows)));
        }

        let outer: Vec<ModelRc<IllustItem>> =
            models.iter().map(|m| ModelRc::from(m.clone())).collect();

        let ui = self.ui();
        ui.set_grid_columns(ModelRc::from(Rc::new(VecModel::from(outer))));
        ui.set_grid_card_width(geometry.card_width);
        ui.set_grid_state(0);
        ui.set_grid_action_label("".into());
        ui.set_grid_count_label(format!("{} 件作品", self.items.len()).into());
        ui.set_grid_message("".into());
        ui.set_status_text(self.status_line().into());

        self.columns = models;
        self.index = index;
        self.spawn_covers();
        self.backfill_author_avatars();
    }

    fn rank_of(&self, illust: &Illust) -> i32 {
        if self.page != Destination::Ranking {
            return 0;
        }
        self.items
            .iter()
            .position(|candidate| candidate.id == illust.id)
            .map(|index| index as i32 + 1)
            .unwrap_or(0)
    }

    /// Start cover loads for anything not already cached or in flight.
    fn spawn_covers(&mut self) {
        let urls: Vec<(String, String)> = self
            .items
            .iter()
            .map(|illust| (illust.id.to_string(), illust.urls.grid().to_string()))
            .collect();
        let cached = &self.covers;
        let plan = self.loader.plan(urls, |id| cached.contains_key(id));
        if plan.is_empty() {
            return;
        }

        let epoch = self.epoch;
        let demo = self.mode == Mode::Demo;
        let cache = self.cache.clone();
        let rt = self.rt.clone();
        let scale = self.ui().window().scale_factor();
        let target = crate::images::target_width(self.geometry.card_width, scale);

        rt.spawn(async move {
            let cache_for = cache.clone();
            let mut stream = futures::stream::iter(plan)
                .map(move |request| {
                    let cache = cache_for.clone();
                    async move {
                        let raw = if demo {
                            demo_raw(
                                Id(request.id.parse::<u64>().unwrap_or(0)),
                                target,
                                target,
                                0,
                            )
                            .await
                        } else {
                            fetch_sized(&cache, &request.url, target).await
                        };
                        (request.id, raw)
                    }
                })
                .buffer_unordered(COVER_CONCURRENCY);

            while let Some((id, raw)) = stream.next().await {
                let ok = raw.is_some();
                on_ui(move |shared| {
                    let Ok(mut app) = shared.try_borrow_mut() else {
                        return;
                    };
                    app.loader.complete(&id, ok);
                    // A result for a feed the user already left must not touch
                    // the current grid.
                    if app.epoch != epoch {
                        return;
                    }
                    match raw {
                        Some(raw) => app.apply_cover(&id, &raw),
                        None => app.mark_cover_failed(&id),
                    }
                });
            }
        });
    }

    fn apply_cover(&mut self, id: &str, raw: &RawImage) {
        let image = raw.to_slint();
        self.covers.insert(id.to_string(), image.clone());
        let Some((column, row)) = self.index.get(id).copied() else {
            return;
        };
        let Some(model) = self.columns.get(column) else {
            return;
        };
        if let Some(mut item) = model.row_data(row) {
            item.cover = image;
            item.state = 0;
            model.set_row_data(row, item);
        }
    }

    fn mark_cover_failed(&mut self, id: &str) {
        let Some((column, row)) = self.index.get(id).copied() else {
            return;
        };
        let Some(model) = self.columns.get(column) else {
            return;
        };
        if let Some(mut item) = model.row_data(row) {
            item.state = 2;
            model.set_row_data(row, item);
        }
    }

    /// Slint reported a new window width. This is the only place the shell's
    /// narrow/compact decision is made, and it is event-driven: the previous
    /// implementation polled `Window::size()` every 250ms *and* reported the width
    /// from a Slint callback, so the two could disagree for a quarter of a second
    /// on every resize.
    fn on_window_width(&mut self, width: f32) {
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        const NARROW_BREAKPOINT: f32 = 1000.0;

        let narrow = width < NARROW_BREAKPOINT;
        let collapsed = self.ui().get_rail_collapsed();
        // Read the rail widths from the theme rather than restating them here: they
        // were duplicated as 240/52 while `theme.slint` said 232/52, so the content
        // box this function reported was 8px wider than the one the layout used.
        let ui = self.ui();
        let rail = {
            let theme = ui.global::<Theme>();
            if narrow || collapsed {
                theme.get_rail_width_compact()
            } else {
                theme.get_rail_width()
            }
        };
        let content = (width - rail).max(0.0);
        ui.set_narrow(narrow);
        ui.set_content_width(content);
    }

    /// Slint reported the content width it actually resolved. Recompute geometry.
    fn on_measured(&mut self, width: f32) {
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        if (width - self.measured_width).abs() < 0.5 {
            return;
        }
        self.measured_width = width;
        // The profile header is the one page that changes *shape* rather than just
        // density, so it needs the same measured number the grid is built from.
        self.ui().set_profile_wide(width >= PROFILE_WIDE_MIN);
        let geometry = GridGeometry::for_width(width);
        let columns_changed = geometry.columns != self.geometry.columns;
        let width_changed = (geometry.card_width - self.geometry.card_width).abs() >= 0.5;
        self.geometry = geometry;
        if !width_changed && !columns_changed {
            return;
        }
        if columns_changed && !self.items.is_empty() {
            self.rebuild_columns();
        } else {
            self.ui().set_grid_card_width(geometry.card_width);
        }
    }

    fn load_more(shared: &Shared) {
        match shared.borrow().page {
            Destination::Home => {
                App::load_home_more(shared);
                return;
            }
            Destination::Discovery => {
                App::load_discovery(shared, true);
                return;
            }
            Destination::UserProfile => {
                App::load_profile_more(shared);
                return;
            }
            _ => {}
        }
        let should_load = {
            let mut app = shared.borrow_mut();
            if app.loading_more || app.reached_end || app.page == Destination::Detail {
                return;
            }
            app.loading_more = true;
            app.page_cursor += 1;
            app.ui().set_grid_loading_more(true);
            true
        };
        if !should_load {
            return;
        }

        let (page, mode, cursor, epoch) = {
            let app = shared.borrow();
            (app.page, app.mode, app.page_cursor, app.epoch)
        };

        match mode {
            Mode::Demo => {
                let mut app = shared.borrow_mut();
                let next = app.demo_items(cursor);
                app.loading_more = false;
                let ui = app.ui();
                ui.set_grid_loading_more(false);
                if next.is_empty() {
                    app.reached_end = true;
                    return;
                }
                app.render_feed(next, true);
            }
            Mode::Real => {
                let app = shared.borrow();
                let client = app.client.clone();
                let rt = app.rt.clone();
                let word = app.search_word.clone();
                let kind = app.ranking_kind;
                let session = app.session.clone();
                drop(app);
                rt.spawn(async move {
                    let result = match client {
                        Some(client) => match page {
                            Destination::Ranking => client
                                .ranking(kind, cursor)
                                .await
                                .map(|rows| rows.into_iter().map(|r| r.illust).collect()),
                            Destination::Search => {
                                let mut query = SearchQuery::new(&word);
                                query.page = cursor;
                                client.search(&query).await.map(|r| r.items)
                            }
                            Destination::Bookmarks => match session.user_id {
                                Some(user) => {
                                    client.bookmarks(user, false, cursor).await.map(|r| r.items)
                                }
                                None => Err(PixivError::Unauthorized),
                            },
                            // A local list has no next page, and falling
                            // through to the daily ranking would have appended
                            // unrelated live works to the history.
                            Destination::History => Ok(Vec::new()),
                            _ => client
                                .ranking(RankingKind::Daily, cursor)
                                .await
                                .map(|rows| rows.into_iter().map(|r| r.illust).collect::<Vec<_>>()),
                        },
                        None => Ok(Vec::new()),
                    };
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        app.loading_more = false;
                        if app.epoch != epoch {
                            return;
                        }
                        let ui = app.ui();
                        ui.set_grid_loading_more(false);
                        match result {
                            Ok(items) if !items.is_empty() => app.render_feed(items, true),
                            Ok(_) => app.reached_end = true,
                            Err(error) => {
                                ui.set_status_text(format!("加载更多失败：{error}").into())
                            }
                        }
                    });
                });
            }
        }
    }

    // ------------------------------------------------------------- detail

    /// Open a work from anywhere in the UI.
    fn open_detail(shared: &Shared, id: &str) {
        let Ok(raw) = id.trim().parse::<u64>() else {
            return;
        };
        App::open_detail_route(shared, raw, true);
    }

    /// Load one work into the detail page.
    ///
    /// push_history is false when the route is already on the stack (Back and
    /// Forward restore a destination; they do not create a new one).
    fn open_detail_route(shared: &Shared, raw: u64, push_history: bool) {
        let illust_id = Id(raw);

        let (mode, epoch) = {
            let mut app = shared.borrow_mut();
            let current = app.route;
            let target = Route::illust(Destination::Detail, raw);
            if push_history {
                app.history.visit(current, target);
                let scroll = app.current_scroll();
                app.remember_scroll(current, scroll);
            }
            app.route = target;
            app.page = Destination::Detail;
            app.detail_epoch += 1;
            let epoch = app.detail_epoch;
            let ui = app.ui();
            ui.set_page(Destination::Detail);
            ui.set_detail_state(1);
            ui.set_detail_pages(ModelRc::default());
            ui.set_detail_page_index(0);
            ui.set_detail_zoom(1.0);
            ui.set_detail_pan_x(0.0);
            ui.set_detail_pan_y(0.0);
            ui.set_detail_fit_mode(true);
            ui.set_detail_busy(false);
            ui.set_detail_following(false);
            ui.set_detail_ugoira_hint("".into());
            ui.set_detail_tags(ModelRc::default());
            (app.mode, epoch)
        };

        match mode {
            Mode::Demo => {
                let app = shared.borrow();
                let found = app.demo.detail(illust_id);
                let pages = app.demo.pages(illust_id);
                drop(app);
                let Some(illust) = found else {
                    App::fail_detail(shared, "找不到该作品，或它对该账号不可见。");
                    return;
                };
                let mut app = shared.borrow_mut();
                app.install_detail(illust, pages, epoch);
            }
            Mode::Real => {
                let app = shared.borrow();
                let client = app.client.clone();
                let rt = app.rt.clone();
                drop(app);
                let Some(client) = client else {
                    App::fail_detail(shared, "没有可用的网络客户端。");
                    return;
                };
                rt.spawn(async move {
                    // Detail, pages and follow-state in one round trip batch: the
                    // page used to show a single page for every real artwork
                    // because the page list was never fetched.
                    let detail = client.illust_detail(illust_id).await;
                    let (illust, pages, following) = match detail {
                        Ok(illust) => {
                            let pages = client
                                .illust_pages(illust_id)
                                .await
                                .unwrap_or_else(|_| Vec::new());
                            let following = client
                                .user_profile(illust.author.id)
                                .await
                                .map(|profile| profile.is_followed)
                                .unwrap_or(false);
                            (illust, pages, following)
                        }
                        Err(error) => {
                            on_ui(move |shared| {
                                App::fail_detail(shared, &error.to_string());
                            });
                            return;
                        }
                    };
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        if app.detail_epoch != epoch {
                            return;
                        }
                        app.install_detail_with_follow(illust, pages, following, epoch);
                    });
                });
            }
        }
    }

    fn fail_detail(shared: &Shared, message: &str) {
        let app = shared.borrow();
        let ui = app.ui();
        ui.set_detail_state(3);
        ui.set_detail_message(message.into());
    }

    fn install_detail(&mut self, illust: Illust, pages: Vec<PageMeta>, epoch: u64) {
        let following = false;
        self.install_detail_with_follow(illust, pages, following, epoch);
    }

    fn install_detail_with_follow(
        &mut self,
        illust: Illust,
        pages: Vec<PageMeta>,
        following: bool,
        epoch: u64,
    ) {
        if self.detail_epoch != epoch {
            return;
        }

        {
            let ui = self.ui();
            ui.set_detail_title(illust.title.clone().into());
            ui.set_detail_author(illust.author.name.clone().into());
            ui.set_detail_author_id(illust.author.id.to_string().into());
            ui.set_detail_description(illust.description.clone().into());
            ui.set_detail_posted(illust.create_date.clone().unwrap_or_default().into());
            ui.set_detail_kind(illust.kind.code());
            ui.set_detail_page_count(illust.page_count as i32);
            ui.set_detail_views(illust.stats.views as i32);
            ui.set_detail_bookmarks(illust.bookmark.count as i32);
            ui.set_detail_likes(illust.stats.likes as i32);
            ui.set_detail_bookmarked(illust.bookmark.bookmarked);
            ui.set_detail_following(following);
            ui.set_detail_ugoira_hint(
                if illust.kind == pixiv_types::domain::IllustKind::Ugoira {
                    "可导出为 GIF 保存在下载目录；播放尚未内置，请在浏览器中查看原始动图。"
                } else {
                    ""
                }
                .into(),
            );
            let tags: Vec<slint::SharedString> =
                illust.tags.iter().map(|t| t.display().into()).collect();
            ui.set_detail_tags(ModelRc::from(Rc::new(VecModel::from(tags))));

            // The author's face, from the same map the cards use. A work payload
            // usually carries the avatar URL; when it does not, the id lookup the
            // cards already do is reused rather than asking for a second endpoint.
            let avatar_url = illust
                .author
                .avatar
                .clone()
                .or_else(|| self.author_avatars.get(&illust.author.id.get()).cloned());
            let avatar = avatar_url
                .as_deref()
                .and_then(|url| self.avatars.get(url).cloned());
            match &avatar {
                Some(image) => {
                    ui.set_detail_author_avatar(image.clone());
                    ui.set_detail_author_avatar_state(BAND_OK);
                }
                None => {
                    ui.set_detail_author_avatar(slint::Image::default());
                    ui.set_detail_author_avatar_state(if avatar_url.is_some() {
                        BAND_LOADING
                    } else {
                        BAND_EMPTY
                    });
                }
            }
            self.detail_author_avatar_url = avatar_url;
        }

        // Demo works must never enter the on-disk history: their cover URLs are
        // `demo://` pseudo-URLs that cannot be fetched again, so a later real-mode
        // History page would show cards that can never load. The two modes stay
        // separate, which is the whole point of the demo badge.
        if self.mode == Mode::Real {
            let _ = self.store.record_view(&HistoryEntry {
                illust_id: illust.id.to_string(),
                title: illust.title.clone(),
                author: illust.author.name.clone(),
                thumb_url: illust.urls.grid().to_string(),
                viewed_at: now_secs(),
            });
        }

        // Page skeletons, sized from the real page metadata when we have it.
        let count = (illust.page_count.max(1) as usize).max(pages.len());
        let skeletons: Vec<ViewerPage> = (0..count)
            .map(|index| {
                let meta = pages.get(index);
                ViewerPage {
                    source: slint::Image::default(),
                    thumb: slint::Image::default(),
                    width: meta.map_or(illust.width as i32, |p| p.width as i32),
                    height: meta.map_or(illust.height as i32, |p| p.height as i32),
                    loaded: false,
                    failed: false,
                }
            })
            .collect();
        let models = Rc::new(VecModel::from(skeletons));
        self.ui().set_detail_pages(ModelRc::from(models.clone()));
        self.ui().set_detail_state(0);

        let urls: Vec<String> = if pages.is_empty() {
            vec![illust.urls.largest().to_string()]
        } else {
            pages
                .iter()
                .map(|page| page.urls.regular.to_string())
                .collect()
        };

        let mode = self.mode;
        let cache = self.cache.clone();
        let rt = self.rt.clone();
        let seed = illust.id;
        let target = models.clone();

        rt.spawn(async move {
            let mut stream = futures::stream::iter(urls.into_iter().enumerate())
                .map(move |(index, url)| {
                    let cache = cache.clone();
                    async move {
                        let raw = if mode == Mode::Demo {
                            demo_raw(
                                Id(seed.get().wrapping_add(index as u64)),
                                900,
                                1200,
                                index as u64,
                            )
                            .await
                        } else {
                            fetch_sized_viewer(&cache, &url).await
                        };
                        (index, raw)
                    }
                })
                .buffer_unordered(3);

            while let Some((index, raw)) = stream.next().await {
                on_ui(move |shared| {
                    let Ok(app) = shared.try_borrow() else {
                        return;
                    };
                    let Some(model) = app.detail_models.clone() else {
                        return;
                    };
                    if app.detail_epoch != epoch {
                        return;
                    }
                    let Some(mut page) = model.row_data(index) else {
                        return;
                    };
                    match raw {
                        Some(raw) => {
                            let image = raw.to_slint();
                            page.source = image.clone();
                            page.thumb = image;
                            page.loaded = true;
                        }
                        None => page.failed = true,
                    }
                    model.set_row_data(index, page);
                });
            }
        });

        self.detail = Some(illust);
        self.detail_pages = pages;
        self.detail_models = Some(target);

        // Only now, with this page owning the work: the bounded id lookup reads
        // `self.detail`, so asking before the move resolves the *previous* page's
        // author, which is what the first attempt did.
        match self.detail_author_avatar_url.clone() {
            Some(url) => self.spawn_avatars(vec![url]),
            None => self.backfill_author_avatars(),
        }
        self.refresh_chrome();
    }

    /// Re-fetch the image of the page the viewer is showing.
    ///
    /// The failed state has had a 「重新载入」 button since the viewer was written,
    /// but it was wired to `request-toggle-zoom`: pressing it flipped between fit
    /// and 1:1 and never requested the image again. One page is retried, not the
    /// whole work - a 40-page illustration should not refetch 40 images to recover
    /// from one timeout.
    fn reload_detail_page(&self) {
        let Some(models) = self.detail_models.clone() else {
            return;
        };
        let Some(illust) = self.detail.clone() else {
            return;
        };
        let index = self.ui().get_detail_page_index().max(0) as usize;
        let Some(mut page) = models.row_data(index) else {
            return;
        };

        let Some(url) = retry_url(&self.detail_pages, illust.urls.largest(), index) else {
            // A work with no image at all - a novel, a deleted page, a page list
            // that came back empty - has nothing to retry. Saying so is the whole
            // difference between a button that is broken and one that has no work
            // to do.
            self.notify(
                2,
                "这件作品没有可重新载入的图片，可以点工具栏的「在浏览器中打开」",
            );
            return;
        };

        // Back to "loading" first: leaving `failed` set would keep the error screen
        // up while the retry is in flight, which is exactly the lie the old button
        // told.
        page.loaded = false;
        page.failed = false;
        models.set_row_data(index, page);

        let mode = self.mode;
        let cache = self.cache.clone();
        let rt = self.rt.clone();
        let epoch = self.detail_epoch;
        let seed = illust.id;
        rt.spawn(async move {
            let raw = if mode == Mode::Demo {
                demo_raw(
                    Id(seed.get().wrapping_add(index as u64)),
                    900,
                    1200,
                    index as u64,
                )
                .await
            } else {
                fetch_sized_viewer(&cache, &url).await
            };
            on_ui(move |shared| {
                let Ok(app) = shared.try_borrow() else {
                    return;
                };
                if app.detail_epoch != epoch {
                    return;
                }
                let Some(model) = app.detail_models.clone() else {
                    return;
                };
                let Some(mut page) = model.row_data(index) else {
                    return;
                };
                match raw {
                    Some(raw) => {
                        let image = raw.to_slint();
                        page.source = image.clone();
                        page.thumb = image;
                        page.loaded = true;
                        page.failed = false;
                    }
                    None => {
                        page.loaded = false;
                        page.failed = true;
                    }
                }
                model.set_row_data(index, page);
            });
        });
    }

    // ------------------------------------------------------------- workers

    fn start_download_worker(&self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let manager = self.downloads.clone();
        self.rt.spawn(async move {
            manager.run(client).await;
        });
    }

    fn push_downloads(&self) {
        let rows = to_download_rows(self.downloads.tasks());
        let ui = self.ui();
        let active = rows
            .iter()
            .filter(|row| row.state == 0 || row.state == 1)
            .count() as i32;
        let done = rows.iter().filter(|row| row.state == 2).count() as i32;
        let failed = rows.iter().filter(|row| row.state == 3).count() as i32;
        let model = Rc::new(VecModel::from(rows));
        ui.set_download_rows(ModelRc::from(model));
        ui.set_downloads_active(active);
        ui.set_downloads_done(done);
        ui.set_downloads_failed(failed);
    }

    fn toggle_bookmark(&mut self, id: &str) {
        let Ok(raw) = id.parse::<u64>() else {
            return;
        };
        let illust_id = Id(raw);

        match self.mode {
            Mode::Demo => {
                if let Some(item) = self.items.iter_mut().find(|i| i.id == illust_id) {
                    let next = !item.bookmark.bookmarked;
                    item.apply_bookmark(next, item.bookmark.restrict);
                }
                let bookmarked = self.bookmarked_state(illust_id);
                let ui = self.ui();
                if self.page == Destination::Detail {
                    ui.set_detail_bookmarked(bookmarked);
                }
                self.notify(2, "演示模式：收藏状态只在本机切换，不会提交到 Pixiv");
                self.resync_bookmarks();
            }
            Mode::Real => {
                if !self.session.is_authenticated() {
                    self.notify(2, "需要登录后才能收藏，请到设置页登录");
                    return;
                }
                let Some(client) = self.client.clone() else {
                    return;
                };
                // Decide the direction from what the server last told us, and
                // carry the current visibility so a private bookmark stays
                // private.
                let current = self
                    .detail
                    .as_ref()
                    .filter(|d| d.id == illust_id)
                    .map(|d| d.bookmark)
                    .unwrap_or_default();
                let next = !current.bookmarked;
                let restrict = if current.bookmarked {
                    current.restrict
                } else {
                    BookmarkRestrict::Public
                };
                let rt = self.rt.clone();
                rt.spawn(async move {
                    let result = client.set_bookmark(illust_id, next, restrict).await;
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        match result {
                            Ok(state) => {
                                // Apply the state the server reported, not the
                                // one we asked for: if Pixiv refused, this is the
                                // old value and the UI stays truthful.
                                let now_bookmarked = state.bookmarked;
                                app.apply_bookmark_state(illust_id, state);
                                app.notify(
                                    1,
                                    if now_bookmarked {
                                        "已加入收藏"
                                    } else {
                                        "已取消收藏"
                                    },
                                );
                            }
                            Err(error) => {
                                // Leave the UI on the previous state and say why.
                                app.notify(3, format!("收藏失败：{error}"));
                            }
                        }
                    });
                });
            }
        }
    }

    fn bookmarked_state(&self, id: Id) -> bool {
        self.items
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.bookmark.bookmarked)
            .or_else(|| {
                self.detail
                    .as_ref()
                    .filter(|d| d.id == id)
                    .map(|d| d.bookmark.bookmarked)
            })
            .unwrap_or(false)
    }

    /// Adopt the server's bookmark verdict everywhere the illustration appears.
    ///
    /// The feed card, the detail model and the metadata list each hold their own
    /// copy, and they used to drift: toggling on the detail page left the card in
    /// the grid showing the opposite state until the page was reloaded.
    fn apply_bookmark_state(&mut self, id: Id, state: pixiv_types::BookmarkState) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.apply_bookmark(state.bookmarked, state.restrict);
        }
        if let Some(detail) = self.detail.as_mut().filter(|d| d.id == id) {
            detail.apply_bookmark(state.bookmarked, state.restrict);
            let count = detail.bookmark.count as i32;
            let ui = self.ui();
            ui.set_detail_bookmarks(count);
            ui.set_detail_bookmarked(state.bookmarked);
        }
        self.resync_bookmarks();
    }

    /// Push the in-memory bookmark state back into the visible cards.
    fn resync_bookmarks(&mut self) {
        let map: HashMap<String, bool> = self
            .items
            .iter()
            .map(|i| (i.id.to_string(), i.bookmark.bookmarked))
            .collect();
        for column in &self.columns {
            for row in 0..column.row_count() {
                let Some(mut item) = column.row_data(row) else {
                    continue;
                };
                if let Some(flag) = map.get(item.id.as_str())
                    && item.bookmarked != *flag
                {
                    item.bookmarked = *flag;
                    column.set_row_data(row, item);
                }
            }
        }
    }

    fn enqueue_download(&mut self, all_pages: bool) {
        let Some(illust) = self.detail.clone() else {
            return;
        };
        if self.mode == Mode::Demo {
            self.notify(2, "演示模式不写入文件；登录并切换到真实会话后可用");
            return;
        }
        let pages = &self.detail_pages;
        let requests: Vec<DownloadRequest> = if pages.is_empty() {
            vec![DownloadRequest {
                illust_id: illust.id.to_string(),
                title: illust.title.clone(),
                author: illust.author.name.clone(),
                page_index: 0,
                page_count: illust.page_count.max(1),
                url: illust.urls.largest().to_string(),
                extension: "jpg".to_string(),
            }]
        } else {
            let selected: Vec<&PageMeta> = if all_pages {
                pages.iter().collect()
            } else {
                pages.iter().take(1).collect()
            };
            selected
                .into_iter()
                .map(|page| DownloadRequest {
                    illust_id: illust.id.to_string(),
                    title: illust.title.clone(),
                    author: illust.author.name.clone(),
                    page_index: page.index,
                    page_count: pages.len().max(1) as u32,
                    url: page.urls.original.clone(),
                    extension: extension_for(&page.urls.original),
                })
                .collect()
        };

        // Never queue the same page twice: the manager would happily download it
        // again, and the old UI had no guard at all.
        let existing: Vec<(String, u32)> = self
            .downloads
            .tasks()
            .into_iter()
            .filter(|task| !task.state.is_terminal())
            .map(|task| (task.request.illust_id.clone(), task.request.page_index))
            .collect();
        let fresh: Vec<DownloadRequest> = requests
            .into_iter()
            .filter(|request| {
                !existing
                    .iter()
                    .any(|(id, page)| *id == request.illust_id && *page == request.page_index)
            })
            .collect();

        if fresh.is_empty() {
            self.notify(2, "这些页面已经在下载队列中");
            return;
        }
        let count = fresh.len();
        self.downloads.enqueue(fresh);
        self.push_downloads();
        self.notify(1, format!("已加入 {count} 个下载任务"));
    }

    /// Fetch the ugoira archive, rebuild it and write a GIF next to the other
    /// downloads. The decoding/encoding already exists in `pixiv-ugoira`; this is
    /// the UI integration that was missing.
    fn export_ugoira(&mut self) {
        let Some(illust) = self.detail.clone() else {
            return;
        };
        let Some(client) = self.client.clone() else {
            self.notify(3, "没有可用的网络客户端");
            return;
        };
        if self.mode == Mode::Demo {
            self.notify(2, "演示模式没有动图资源");
            return;
        }
        self.ui().set_detail_busy(true);
        let rt = self.rt.clone();
        let dir = self.download_dir.clone();
        rt.spawn(async move {
            let outcome = async {
                let meta = client.ugoira_meta(illust.id).await?;
                let (bytes, _) = client.fetch_bytes(meta.best_zip()).await?;
                let film = pixiv_ugoira::UgoiraFilm::decode_zip(&bytes, &meta.frames)
                    .map_err(|e| pixiv_types::PixivError::Api(e.to_string()))?;
                let gif = film
                    .export_gif()
                    .map_err(|e| pixiv_types::PixivError::Api(e.to_string()))?;
                let path = dir.join(format!("{}_ugoira.gif", illust.id));
                if let Err(error) = std::fs::write(&path, gif) {
                    return Err(pixiv_types::PixivError::Api(error.to_string()));
                }
                Ok(path)
            }
            .await;
            on_ui(move |shared| {
                let Ok(app) = shared.try_borrow() else {
                    return;
                };
                app.ui().set_detail_busy(false);
                match outcome {
                    Ok(path) => app.notify(1, format!("已导出动图：{}", path.display())),
                    Err(error) => app.notify(3, format!("导出动图失败：{error}")),
                }
            });
        });
    }

    /// Toggle the follow state for the current author.
    ///
    /// The button's own label is not evidence of anything: it is a cached render.
    /// The server is asked first, then told to change its mind, then asked again
    /// to confirm — because `POST /ajax/user/follow` never existed, and the
    /// version that assumed success reported a follow that had not happened.
    fn toggle_follow(&mut self) {
        let Some(illust) = self.detail.clone() else {
            return;
        };
        if !self.session.is_authenticated() {
            self.notify(2, "需要登录后才能关注画师");
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let artist = illust.author.id;
        self.ui().set_detail_busy(true);
        let rt = self.rt.clone();
        rt.spawn(async move {
            let outcome = async {
                let current = client.follow_state(artist).await?;
                let target = !current;
                client.follow(artist, target).await?;
                let confirmed = client.follow_state(artist).await?;
                Ok::<_, PixivError>((target, confirmed))
            }
            .await;
            on_ui(move |shared| {
                let Ok(app) = shared.try_borrow() else {
                    return;
                };
                app.ui().set_detail_busy(false);
                match outcome {
                    Ok((target, confirmed)) => {
                        app.ui().set_detail_following(confirmed);
                        if confirmed == target {
                            app.notify(
                                1,
                                if confirmed {
                                    "已关注画师"
                                } else {
                                    "已取消关注"
                                },
                            );
                        } else {
                            // The server disagreed with the request: say so rather
                            // than claim the click worked.
                            app.notify(
                                3,
                                if confirmed {
                                    "关注状态未改变：服务器仍然显示已关注"
                                } else {
                                    "关注状态未改变：服务器仍然显示未关注"
                                },
                            );
                        }
                    }
                    Err(error) => app.notify(3, format!("关注失败：{error}")),
                }
            });
        });
    }

    /// Verify a pasted cookie against Pixiv and adopt the account it belongs to.
    ///
    /// The previous version called this "verified" after probing user id 0's
    /// bookmarks and then asked for user id 0's *profile* to find out who the
    /// user was, which of course failed — so the app announced a successful
    /// sign-in with no name, no id and `session` never updated. Everything
    /// downstream (the signed-in badge, the bookmark gate, the bookmarks page)
    /// then behaved as if nobody was signed in.
    ///
    /// Now the only evidence that counts is `GET /ajax/user/self`: it either
    /// names the account or it does not. Nothing is adopted optimistically, and
    /// demo mode stays on until the server has answered.
    fn apply_cookie(&mut self, cookie: String) {
        let session = Session {
            phpsessid: Some(cookie.clone()),
            user_id: None,
            user_name: None,
            csrf_token: None,
        };
        let Ok(client) = Client::new(ClientConfig::default(), session) else {
            self.notify(3, "无法创建网络客户端");
            return;
        };
        // Install the client now so long-lived workers share one connection
        // pool, but leave the mode alone until the server has answered.
        self.client = Some(client.clone());

        self.ui().set_busy(true);
        self.notify(0, "正在向 Pixiv 验证会话…");
        let rt = self.rt.clone();
        let data_dir = self.data_dir.clone();
        rt.spawn(async move {
            let outcome = client.account().await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.ui().set_busy(false);
                match outcome {
                    Ok(account) => {
                        let verified = Session {
                            phpsessid: Some(cookie),
                            user_id: Some(account.id),
                            user_name: Some(account.name.clone()),
                            csrf_token: account.csrf_token.clone(),
                        };
                        // The probe client and the stored session must agree, or
                        // later mutations would run without a CSRF token.
                        client.set_session(verified.clone());
                        let _ = crate::session::save(&data_dir, &verified);
                        app.session = verified;
                        app.client = Some(client);
                        app.mode = Mode::Real;
                        app.feed_owner = None;
                        app.account_avatar = account.avatar.clone();
                        {
                            let ui = app.ui();
                            ui.set_authenticated(true);
                            ui.set_demo_mode(false);
                            ui.set_session_user(account.name.clone().into());
                            ui.set_cookie_input("".into());
                        }
                        app.refresh_chrome();
                        if let Some(url) = app.account_avatar.clone() {
                            app.spawn_avatars(vec![url]);
                        }
                        let label = if account.account.trim().is_empty() {
                            format!("{}（UID {}）", account.name, account.id.get())
                        } else {
                            format!(
                                "{} @{}（UID {}）",
                                account.name,
                                account.account,
                                account.id.get()
                            )
                        };
                        app.notify(1, format!("已登录：{label}"));
                        drop(app);
                        App::load_page(shared, false);
                    }
                    Err(error) => {
                        app.session = Session::default();
                        app.mode = Mode::Demo;
                        app.feed_owner = None;
                        {
                            let ui = app.ui();
                            ui.set_authenticated(false);
                            ui.set_session_user("".into());
                            ui.set_demo_mode(true);
                        }
                        app.refresh_chrome();
                        app.notify(3, format!("登录失败：{}", describe_session_error(&error)));
                    }
                }
            });
        });
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

/// The four independent bands of the home page.
struct HomeBands {
    works: Vec<Illust>,
    latest: Vec<Illust>,
    latest_title: String,
    tags: Vec<RecommendTag>,
    artists: Vec<RecommendUser>,
    works_state: i32,
    latest_state: i32,
    tags_state: i32,
    artists_state: i32,
    works_message: String,
    latest_message: String,
    tags_message: String,
    artists_message: String,
    /// Extra "load more" batches appended to the for-you grid.
    batches: u32,
    empty_batches: u32,
    exhausted: bool,
}

impl HomeBands {
    fn loading() -> Self {
        Self {
            works: Vec::new(),
            latest: Vec::new(),
            latest_title: String::new(),
            tags: Vec::new(),
            artists: Vec::new(),
            works_state: BAND_LOADING,
            latest_state: BAND_LOADING,
            tags_state: BAND_LOADING,
            artists_state: BAND_LOADING,
            works_message: String::new(),
            latest_message: String::new(),
            tags_message: String::new(),
            artists_message: String::new(),
            batches: 0,
            empty_batches: 0,
            exhausted: false,
        }
    }
}

struct DiscoveryBands {
    users: Vec<RecommendUser>,
    users_state: i32,
    users_message: String,
    /// Every id this page has already rendered or tried, so "load more" cannot
    /// append the same work twice.
    seen: HashSet<u64>,
    batches: u32,
    empty_batches: u32,
    exhausted: bool,
}

impl Default for DiscoveryBands {
    fn default() -> Self {
        Self {
            users: Vec::new(),
            users_state: BAND_LOADING,
            users_message: String::new(),
            seen: HashSet::new(),
            batches: 0,
            empty_batches: 0,
            exhausted: false,
        }
    }
}

/// Search-suggestion panel state.
struct SuggestState {
    /// Bumped on every edit and every close. A result whose generation no longer
    /// matches is dropped, which is what stops a slow reply for "lan" from
    /// overwriting the list for "landscape".
    generation: u64,
    query: String,
    visible: bool,
    rows: Vec<Suggestion>,
    active: i32,
    note: String,
    timer: slint::Timer,
    /// Focus-state rows: the server's own recommended tags, plus local history.
    base: Vec<Suggestion>,
}

impl Default for SuggestState {
    fn default() -> Self {
        Self {
            generation: 0,
            query: String::new(),
            visible: false,
            rows: Vec::new(),
            active: -1,
            note: String::new(),
            timer: slint::Timer::default(),
            base: Vec::new(),
        }
    }
}

/// A user page that is currently open.
struct ProfileState {
    user: Id,
    profile: UserProfile,
    /// Every work id, newest first.
    ids: Vec<Id>,
    /// How many ids have been requested so far.
    loaded: usize,
    works: Vec<Illust>,
}

impl App {
    // ------------------------------------------------------------- home

    /// Load all four home bands.
    ///
    /// The daily ranking is not consulted here at all: it is a different page
    /// with a different meaning. Using it as a fallback is what made the home
    /// page indistinguishable from 排行榜.
    fn load_home(shared: &Shared) {
        let (mode, epoch, client, rt, route) = {
            let mut app = shared.borrow_mut();
            app.epoch += 1;
            app.reached_end = false;
            app.page_cursor = 1;
            app.loading_more = false;
            app.loader.reset();
            app.home = HomeBands::loading();
            app.feed_owner = None;
            app.items.clear();
            app.columns.clear();
            app.index.clear();
            let epoch = app.epoch;
            {
                let ui = app.ui();
                ui.set_home_scroll_y(0.0);
                ui.set_grid_scroll_y(0.0);
                ui.set_grid_state(BAND_LOADING);
                ui.set_grid_columns(ModelRc::default());
                ui.set_grid_loading_more(false);
                ui.set_grid_count_label("".into());
            }
            let route = app.route;
            app.remember_scroll(route, 0.0);
            app.publish_home();
            (
                app.mode,
                epoch,
                app.client.clone(),
                app.rt.clone(),
                app.route,
            )
        };

        if mode == Mode::Demo {
            let app = shared.borrow();
            let items = app.demo_items(1);
            let tags: Vec<RecommendTag> = app
                .demo
                .trending_tags()
                .into_iter()
                .map(|name| RecommendTag {
                    translated: None,
                    name,
                    emphasized: false,
                })
                .collect();
            drop(app);
            let mut app = shared.borrow_mut();
            app.home.works_state = BAND_OK;
            app.home.tags_state = if tags.is_empty() { BAND_EMPTY } else { BAND_OK };
            app.home.tags = tags;
            app.home.latest_state = BAND_EMPTY;
            app.home.latest_message = "演示模式不提供真实最新作品".to_string();
            app.home.artists_state = BAND_EMPTY;
            app.home.artists_message = "演示模式不提供推荐画师".to_string();
            app.feed_owner = Some(route);
            app.render_feed(items, false);
            app.publish_home();
            app.refresh_chrome();
            return;
        }

        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.fail_home("没有可用的网络客户端");
            app.refresh_chrome();
            return;
        };

        rt.spawn(async move {
            let (a, b, c, d) = (
                client.clone(),
                client.clone(),
                client.clone(),
                client.clone(),
            );
            let (works, tags, artists, latest) = tokio::join!(
                a.home_feed(),
                b.recommend_tags(),
                c.recommend_users(),
                d.latest_works()
            );
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                if app.epoch != epoch || app.page != Destination::Home {
                    return;
                }
                let mut avatar_urls: Vec<String> = Vec::new();

                match works {
                    Ok(feed) => {
                        app.home.works = feed.works.clone();
                        app.home.works_state = if feed.works.is_empty() {
                            BAND_EMPTY
                        } else {
                            BAND_OK
                        };
                        avatar_urls
                            .extend(feed.works.iter().filter_map(|w| w.author.avatar.clone()));
                    }
                    Err(error) => {
                        let message = error.to_string();
                        app.home.works_state = BAND_ERROR;
                        app.home.works_message = message.clone();
                        app.show_feed_error(&message);
                    }
                }

                match tags {
                    Ok(tags) => {
                        app.home.tags = tags;
                        app.home.tags_state = if app.home.tags.is_empty() {
                            BAND_EMPTY
                        } else {
                            BAND_OK
                        };
                    }
                    Err(error) => {
                        app.home.tags_state = BAND_ERROR;
                        app.home.tags_message = error.to_string();
                    }
                }

                match artists {
                    Ok(artists) => {
                        app.home.artists = artists;
                        app.home.artists_state = if app.home.artists.is_empty() {
                            BAND_EMPTY
                        } else {
                            BAND_OK
                        };
                        avatar_urls.extend(App::user_image_urls(&app.home.artists));
                    }
                    Err(error) => {
                        app.home.artists_state = BAND_ERROR;
                        app.home.artists_message = error.to_string();
                    }
                }

                match latest {
                    Ok(latest) => {
                        app.home.latest_title = latest.title.clone();
                        app.home.latest = latest.works.clone();
                        app.home.latest_state = if latest.works.is_empty() {
                            BAND_EMPTY
                        } else {
                            BAND_OK
                        };
                        avatar_urls
                            .extend(latest.works.iter().filter_map(|w| w.author.avatar.clone()));
                    }
                    Err(error) => {
                        app.home.latest_state = BAND_ERROR;
                        app.home.latest_message = error.to_string();
                    }
                }

                // Refresh the suggestion panel's focus-state rows from the tags
                // that just arrived: these are the server's own picks.
                app.suggest.base = app
                    .home
                    .tags
                    .iter()
                    .map(|tag| Suggestion {
                        tag: tag.name.clone(),
                        translated: tag.translated.clone(),
                        illust_count: None,
                        popularity: None,
                        source: pixiv_types::domain::SuggestionSource::Pixiv,
                    })
                    .collect();

                app.feed_owner = Some(app.route);
                let works = app.home.works.clone();
                app.render_feed(works, false);
                app.publish_home();
                app.spawn_avatars(avatar_urls);
                app.refresh_chrome();
            });
        });
    }

    /// Append another for-you batch. The street feed is not paginated, so a
    /// second call is simply a second sample of the same recommendation engine.
    fn load_home_more(shared: &Shared) {
        let (client, rt, epoch, reached_end) = {
            let mut app = shared.borrow_mut();
            if app.loading_more || app.reached_end || app.home.exhausted {
                return;
            }
            app.loading_more = true;
            app.ui().set_grid_loading_more(true);
            (
                app.client.clone(),
                app.rt.clone(),
                app.epoch,
                app.reached_end,
            )
        };
        let _ = reached_end;
        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.loading_more = false;
            app.ui().set_grid_loading_more(false);
            app.reached_end = true;
            return;
        };
        rt.spawn(async move {
            let feed = client.home_feed().await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.loading_more = false;
                app.ui().set_grid_loading_more(false);
                if app.epoch != epoch || app.page != Destination::Home {
                    return;
                }
                match feed {
                    Ok(feed) => {
                        let urls: Vec<String> = feed
                            .works
                            .iter()
                            .filter_map(|w| w.author.avatar.clone())
                            .collect();
                        let added =
                            pixiv_types::domain::append_unique(&mut app.home.works, feed.works);
                        app.home.batches += 1;
                        if added == 0 {
                            app.home.empty_batches += 1;
                        } else {
                            app.home.empty_batches = 0;
                        }
                        if app.home.empty_batches >= 2 || app.home.batches >= 8 {
                            app.home.exhausted = true;
                            app.reached_end = true;
                        }
                        if added > 0 {
                            let works = app.home.works.clone();
                            app.render_feed(works, false);
                        }
                        app.spawn_avatars(urls);
                        app.publish_home();
                    }
                    Err(error) => {
                        app.ui()
                            .set_status_text(format!("加载更多失败：{error}").into());
                        app.reached_end = true;
                    }
                }
            });
        });
    }

    fn fail_home(&mut self, message: &str) {
        self.show_feed_error(message);
        self.home.works_state = BAND_ERROR;
        self.home.works_message = message.to_string();
        self.home.tags_state = BAND_ERROR;
        self.home.tags_message = message.to_string();
        self.home.artists_state = BAND_ERROR;
        self.home.artists_message = message.to_string();
        self.home.latest_state = BAND_ERROR;
        self.home.latest_message = message.to_string();
        self.publish_home();
    }

    /// Push the home bands into the window.
    fn publish_home(&self) {
        let ui = self.ui();
        let tags: Vec<RecommendTagItem> = self
            .home
            .tags
            .iter()
            .map(|tag| RecommendTagItem {
                name: tag.name.clone().into(),
                label: tag.display().to_string().into(),
                emphasized: tag.emphasized,
            })
            .collect();
        ui.set_home_tags(ModelRc::from(Rc::new(VecModel::from(tags))));

        let artists: Vec<RecommendUserItem> = self
            .home
            .artists
            .iter()
            .map(|user| self.user_item(user))
            .collect();
        ui.set_home_artists(ModelRc::from(Rc::new(VecModel::from(artists))));

        let latest: Vec<IllustItem> = self
            .home
            .latest
            .iter()
            .map(|illust| self.illust_item_avatar(illust, 0))
            .collect();
        ui.set_home_latest(ModelRc::from(Rc::new(VecModel::from(latest))));

        ui.set_home_latest_title(if self.home.latest_title.trim().is_empty() {
            "最新作品".into()
        } else {
            self.home.latest_title.clone().into()
        });
        ui.set_home_feed_state(self.home.works_state);
        ui.set_home_latest_state(self.home.latest_state);
        ui.set_home_tags_state(self.home.tags_state);
        ui.set_home_artists_state(self.home.artists_state);
        ui.set_home_feed_message(self.home.works_message.clone().into());
        ui.set_home_latest_message(self.home.latest_message.clone().into());
        ui.set_home_tags_message(self.home.tags_message.clone().into());
        ui.set_home_artists_message(self.home.artists_message.clone().into());

        let note = match self.mode {
            Mode::Demo => {
                "演示模式：以下内容来自本地演示目录，标记为 [DEMO]，不会访问 Pixiv".to_string()
            }
            Mode::Real if !self.session.is_authenticated() => {
                "未登录：Pixiv 返回的是公共推荐，不是按你生成的".to_string()
            }
            Mode::Real => format!(
                "{} · 推荐来自 /ajax/street/for_you",
                FeedSource::ForYou.note()
            ),
        };
        ui.set_home_source_note(note.into());
    }

    // ------------------------------------------------------------- avatars

    /// Take one URL to a decoded image, so a card, a header and the top bar can
    /// share one download.
    fn start_avatar(&mut self, url: &str) -> bool {
        if url.trim().is_empty()
            || self.avatars.contains_key(url)
            || self.avatar_loading.contains(url)
        {
            return false;
        }
        self.avatar_loading.insert(url.to_string());
        true
    }

    fn spawn_avatars(&mut self, urls: Vec<String>) {
        let mut plan: Vec<String> = Vec::new();
        for url in urls {
            if self.start_avatar(&url) {
                plan.push(url);
            }
        }
        if plan.is_empty() {
            return;
        }
        let cache = self.cache.clone();
        let rt = self.rt.clone();
        let demo = self.mode == Mode::Demo;
        let scale = self.ui().window().scale_factor().max(1.0);
        // A 170px source is the largest Pixiv sends for an avatar; decoding at
        // the size actually drawn keeps 40 avatars off the memory budget.
        let target = crate::images::target_width(96.0, scale);
        rt.spawn(async move {
            let mut stream = futures::stream::iter(plan)
                .map(move |url| {
                    let cache = cache.clone();
                    async move {
                        let raw = if demo {
                            None
                        } else {
                            fetch_sized(&cache, &url, target).await
                        };
                        (url, raw)
                    }
                })
                .buffer_unordered(4);
            while let Some((url, raw)) = stream.next().await {
                on_ui(move |shared| {
                    let Ok(mut app) = shared.try_borrow_mut() else {
                        return;
                    };
                    app.avatar_loading.remove(&url);
                    let Some(raw) = raw else {
                        return;
                    };
                    let image = raw.to_slint();
                    app.avatars.insert(url.clone(), image);
                    app.repaint_avatars(&url);
                });
            }
        });
    }

    /// Patch every surface that shows the avatar that just arrived.
    fn repaint_avatars(&mut self, url: &str) {
        if self.account_avatar.as_deref() == Some(url)
            && let Some(image) = self.avatars.get(url).cloned()
        {
            let ui = self.ui();
            ui.set_account_avatar(image);
            ui.set_account_avatar_state(BAND_OK);
        }

        if self
            .home
            .artists
            .iter()
            .any(|user| user.avatar.as_deref() == Some(url))
        {
            self.publish_home();
        }
        if self
            .discovery
            .users
            .iter()
            .any(|user| user.avatar.as_deref() == Some(url))
        {
            self.publish_discovery_users();
        }
        if self
            .profile
            .as_ref()
            .and_then(|state| state.profile.avatar.clone())
            .as_deref()
            == Some(url)
        {
            self.publish_profile();
        }

        if self.detail_author_avatar_url.as_deref() == Some(url)
            && let Some(image) = self.avatars.get(url).cloned()
        {
            let ui = self.ui();
            ui.set_detail_author_avatar(image);
            ui.set_detail_author_avatar_state(BAND_OK);
        }

        // Cards: match on the URL the row was built with, so only the rows that
        // wanted this face are patched.
        let Some(image) = self.avatars.get(url).cloned() else {
            return;
        };
        for column in &self.columns {
            for row in 0..column.row_count() {
                let Some(mut item) = column.row_data(row) else {
                    continue;
                };
                if item.author_avatar_url.as_str() != url {
                    continue;
                }
                item.author_avatar = image.clone();
                item.author_avatar_state = BAND_OK;
                column.set_row_data(row, item);
            }
        }
    }

    /// Resolve the faces of authors whose payload carried no avatar URL.
    ///
    /// Ranking and search rows do name a profile image, but a page assembled
    /// from several sources can contain authors that did not. Rather than one
    /// profile request per card (an N+1 storm), at most four distinct unknown
    /// authors are resolved per render, the result is cached by user id, and an
    /// id that is already in flight is never requested twice.
    fn backfill_author_avatars(&mut self) {
        if self.mode != Mode::Real {
            return;
        }
        let mut wanted: Vec<u64> = Vec::new();
        // The open detail page is the one the user is looking at, so its author
        // gets first claim on the four lookups this render is allowed.
        if let Some(detail) = &self.detail
            && detail.author.id.get() != 0
            && detail.author.avatar.is_none()
            && !self.author_avatars.contains_key(&detail.author.id.get())
            && !self.author_avatar_lookup.contains(&detail.author.id.get())
        {
            wanted.push(detail.author.id.get());
        }
        for illust in &self.items {
            let author = illust.author.id.get();
            if author == 0 || illust.author.avatar.is_some() {
                continue;
            }
            if self.author_avatars.contains_key(&author)
                || self.author_avatar_lookup.contains(&author)
                || wanted.contains(&author)
            {
                continue;
            }
            wanted.push(author);
            if wanted.len() == 4 {
                break;
            }
        }
        if wanted.is_empty() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        for author in &wanted {
            self.author_avatar_lookup.insert(*author);
        }
        let rt = self.rt.clone();
        rt.spawn(async move {
            for author in wanted {
                let found = client
                    .user_profile(Id(author))
                    .await
                    .ok()
                    .and_then(|profile| profile.avatar);
                on_ui(move |shared| {
                    let Ok(mut app) = shared.try_borrow_mut() else {
                        return;
                    };
                    app.author_avatar_lookup.remove(&author);
                    let Some(url) = found else {
                        return;
                    };
                    app.author_avatars.insert(author, url.clone());
                    app.apply_author_avatar(author, &url);
                    app.adopt_detail_author_avatar(author, &url);
                    app.spawn_avatars(vec![url]);
                });
            }
        });
    }

    /// Point every card of one author at the avatar that was just resolved.
    /// Point the open detail panel at an avatar that was just resolved by id.
    fn adopt_detail_author_avatar(&mut self, author: u64, url: &str) {
        let Some(detail) = &self.detail else {
            return;
        };
        if detail.author.id.get() != author {
            return;
        }
        self.detail_author_avatar_url = Some(url.to_string());
        let ui = self.ui();
        match self.avatars.get(url).cloned() {
            Some(image) => {
                ui.set_detail_author_avatar(image);
                ui.set_detail_author_avatar_state(BAND_OK);
            }
            None => ui.set_detail_author_avatar_state(BAND_LOADING),
        }
    }

    fn apply_author_avatar(&mut self, author: u64, url: &str) {
        let key = author.to_string();
        for column in &self.columns {
            for row in 0..column.row_count() {
                let Some(mut item) = column.row_data(row) else {
                    continue;
                };
                if item.author_id.as_str() != key || !item.author_avatar_url.is_empty() {
                    continue;
                }
                item.author_avatar_url = url.into();
                item.author_avatar_state = BAND_LOADING;
                column.set_row_data(row, item);
            }
        }
    }

    /// One card, with whatever avatar is already decoded.
    fn illust_item_avatar(&self, illust: &Illust, rank: i32) -> IllustItem {
        let mut item = illust_to_item(illust, rank);
        if let Some(url) = illust.author.avatar.as_ref()
            && let Some(image) = self.avatars.get(url)
        {
            item.author_avatar = image.clone();
            item.author_avatar_state = BAND_OK;
        }
        item
    }

    /// Every image one recommended-user card needs: their avatar and the cover
    /// of the work used as a preview. Without this the artist row is drawn from
    /// placeholders, which is exactly what "no avatar" looked like.
    fn user_image_urls(users: &[RecommendUser]) -> Vec<String> {
        let mut urls = Vec::new();
        for user in users {
            urls.extend(user.avatar.clone());
            if let Some(preview) = user.previews.first() {
                let cover = preview.urls.grid();
                if !cover.is_empty() {
                    urls.push(cover.to_string());
                }
            }
        }
        urls
    }

    fn user_item(&self, user: &RecommendUser) -> RecommendUserItem {
        let avatar_url = user.avatar.clone().unwrap_or_default();
        let preview = user
            .previews
            .first()
            .and_then(|illust| self.avatars.get(illust.urls.grid()))
            .cloned()
            .unwrap_or_default();
        RecommendUserItem {
            id: user.id.to_string().into(),
            name: user.name.clone().into(),
            account: user.account.clone().into(),
            comment: user.comment.clone().into(),
            avatar: self.avatars.get(&avatar_url).cloned().unwrap_or_default(),
            avatar_url: avatar_url.clone().into(),
            preview,
            followed: user.is_followed,
            premium: user.is_premium,
            avatar_state: if avatar_url.is_empty() {
                BAND_EMPTY
            } else if self.avatars.contains_key(&avatar_url) {
                BAND_OK
            } else {
                BAND_LOADING
            },
        }
    }
}

impl App {
    // ------------------------------------------------------------- discovery

    /// Load one discovery batch.
    ///
    /// The discovery engine is not paginated: every call returns a fresh ranked
    /// batch, so "load more" is a new request plus de-duplication against
    /// everything already shown, and the loader stops on its own.
    fn load_discovery(shared: &Shared, append: bool) {
        let (mode, epoch, client, rt, seen) = {
            let mut app = shared.borrow_mut();
            if append {
                if app.loading_more || app.reached_end || app.discovery.exhausted {
                    return;
                }
                app.loading_more = true;
                app.ui().set_grid_loading_more(true);
            } else {
                app.epoch += 1;
                app.reached_end = false;
                app.page_cursor = 1;
                app.loader.reset();
                app.discovery = DiscoveryBands::default();
                app.feed_owner = None;
                app.items.clear();
                app.columns.clear();
                app.index.clear();
                {
                    let ui = app.ui();
                    ui.set_grid_scroll_y(0.0);
                    ui.set_grid_state(BAND_LOADING);
                    ui.set_grid_columns(ModelRc::default());
                    ui.set_grid_loading_more(false);
                    ui.set_grid_count_label("".into());
                }
                app.publish_discovery_users();
            }
            (
                app.mode,
                app.epoch,
                app.client.clone(),
                app.rt.clone(),
                app.discovery.seen.clone(),
            )
        };

        if mode == Mode::Demo {
            let mut app = shared.borrow_mut();
            app.loading_more = false;
            app.ui().set_grid_loading_more(false);
            app.discovery.users_state = BAND_EMPTY;
            app.discovery.users_message = "演示模式不提供推荐画师".to_string();
            app.discovery.exhausted = true;
            let cursor = if append { app.page_cursor + 1 } else { 1 };
            app.page_cursor = cursor;
            let items = app.demo_items(cursor);
            if items.is_empty() {
                app.reached_end = true;
            } else {
                app.feed_owner = Some(app.route);
                app.render_feed(items, append);
            }
            app.publish_discovery_users();
            app.refresh_chrome();
            return;
        }

        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.loading_more = false;
            app.ui().set_grid_loading_more(false);
            app.show_feed_error("没有可用的网络客户端");
            app.refresh_chrome();
            return;
        };

        rt.spawn(async move {
            // The batch route names sixty recommendations but describes only a
            // handful, and the discovery route describes ten in full. Both are
            // the same engine, so asking for both fills a screen immediately and
            // still leaves ids to hydrate.
            let (quick_client, pool_client, user_client) =
                (client.clone(), client.clone(), client.clone());
            let (quick, pool, users) = tokio::join!(
                quick_client.illust_discovery("all"),
                pool_client.discovery_artworks("all", 60),
                user_client.discovery_users("all")
            );

            let mut works: Vec<Illust> = Vec::new();
            let mut pending: Vec<Id> = Vec::new();
            let mut quick_error: Option<String> = None;
            match quick {
                Ok(rows) => works.extend(rows),
                Err(error) => quick_error = Some(error.to_string()),
            }
            if let Ok(batch) = &pool {
                works.extend(batch.works.iter().cloned());
                pending.extend(batch.pending.iter().copied());
            }

            // Hydrate the next few recommended ids, skipping anything this page
            // has already shown.
            let mut targets: Vec<Id> = Vec::new();
            for id in &pending {
                if targets.len() >= DISCOVERY_HYDRATE || seen.contains(&id.get()) {
                    continue;
                }
                targets.push(*id);
            }
            let mut hydrated: Vec<Illust> = Vec::new();
            if !targets.is_empty() {
                let detail_client = client.clone();
                let mut stream = futures::stream::iter(targets)
                    .map(move |id| {
                        let client = detail_client.clone();
                        async move { client.illust_detail(id).await.ok() }
                    })
                    .buffer_unordered(4);
                while let Some(found) = stream.next().await {
                    if let Some(illust) = found {
                        hydrated.push(illust);
                    }
                }
            }
            works.extend(hydrated);

            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.loading_more = false;
                app.ui().set_grid_loading_more(false);
                if app.epoch != epoch || app.page != Destination::Discovery {
                    return;
                }

                if let Some(message) = quick_error
                    && app.items.is_empty()
                    && works.is_empty()
                {
                    app.show_feed_error(&message);
                    app.discovery.exhausted = true;
                    app.refresh_chrome();
                    return;
                }

                let mut urls: Vec<String> = works
                    .iter()
                    .filter_map(|illust| illust.author.avatar.clone())
                    .collect();

                if let Ok(users) = users {
                    app.discovery.users = users;
                    app.discovery.users_state = if app.discovery.users.is_empty() {
                        BAND_EMPTY
                    } else {
                        BAND_OK
                    };
                    // The artists' own faces and previews are not in the work
                    // batch, so they are requested here explicitly.
                    urls.extend(App::user_image_urls(&app.discovery.users));
                } else if app.discovery.users.is_empty() {
                    app.discovery.users_state = BAND_ERROR;
                    app.discovery.users_message = "推荐画师载入失败".to_string();
                }

                // Record every id this batch touched so the next one cannot
                // re-add it, including the ones that failed to hydrate.
                for illust in &works {
                    app.discovery.seen.insert(illust.id.get());
                }
                for id in &pending {
                    app.discovery.seen.insert(id.get());
                }
                let added = pixiv_types::domain::append_unique(
                    &mut app.items,
                    works.into_iter().filter(|illust| !illust.id.is_zero()),
                );

                app.discovery.batches += 1;
                if added == 0 {
                    app.discovery.empty_batches += 1;
                } else {
                    app.discovery.empty_batches = 0;
                }
                if app.discovery.empty_batches >= 2
                    || app.discovery.batches >= DISCOVERY_MAX_BATCHES
                {
                    app.discovery.exhausted = true;
                    app.reached_end = true;
                }

                app.feed_owner = Some(app.route);
                if added == 0 && app.items.is_empty() {
                    app.ui().set_grid_state(BAND_EMPTY);
                    app.ui().set_grid_state_title("没有更多推荐".into());
                    app.ui().set_grid_state_glyph("discover".into());
                    app.ui()
                        .set_grid_message("Pixiv 的推荐引擎这次没有返回新作品，稍后再试。".into());
                    app.ui().set_grid_action_label("".into());
                    app.ui().set_grid_columns(ModelRc::default());
                    app.columns.clear();
                    app.index.clear();
                } else {
                    app.render_feed(Vec::new(), true);
                }
                app.publish_discovery_users();
                app.spawn_avatars(urls);
                app.refresh_chrome();
            });
        });
    }

    fn publish_discovery_users(&self) {
        let ui = self.ui();
        let users: Vec<RecommendUserItem> = self
            .discovery
            .users
            .iter()
            .map(|user| self.user_item(user))
            .collect();
        ui.set_discovery_users(ModelRc::from(Rc::new(VecModel::from(users))));
        ui.set_discovery_users_state(self.discovery.users_state);
        ui.set_discovery_users_message(self.discovery.users_message.clone().into());
    }

    // ------------------------------------------------------------- profile

    /// Load the user page named by the current route.
    fn load_profile(shared: &Shared) {
        let (route, target, epoch, client, rt) = {
            let mut app = shared.borrow_mut();
            let route = app.route;
            let target = route.user_id().map(Id).or(app.session.user_id);
            app.epoch += 1;
            app.epoch += 1;
            let epoch = app.epoch;
            app.profile = None;
            app.feed_owner = None;
            app.items.clear();
            app.columns.clear();
            app.index.clear();
            app.loading_more = false;
            app.reached_end = false;
            {
                let ui = app.ui();
                ui.set_grid_scroll_y(0.0);
                ui.set_grid_state(BAND_LOADING);
                ui.set_grid_columns(ModelRc::default());
                ui.set_grid_loading_more(false);
                ui.set_grid_count_label("".into());
                ui.set_profile_state(BAND_LOADING);
                ui.set_profile_message("".into());
                ui.set_profile_count_label("".into());
                ui.set_profile_followed(false);
                ui.set_profile_avatar(slint::Image::default());
                ui.set_profile_avatar_state(BAND_LOADING);
                ui.set_profile_background(slint::Image::default());
                ui.set_profile_stats(ModelRc::default());
                ui.set_profile_name("".into());
                ui.set_profile_account("".into());
                ui.set_profile_uid(target.map(|id| id.to_string()).unwrap_or_default().into());
                ui.set_profile_comment("".into());
                ui.set_profile_webpage("".into());
                ui.set_profile_premium(false);
                ui.set_profile_self(false);
            }
            app.remember_scroll(route, 0.0);
            app.refresh_chrome();
            (route, target, epoch, app.client.clone(), app.rt.clone())
        };

        let Some(target) = target else {
            let app = shared.borrow_mut();
            app.ui().set_profile_state(BAND_ERROR);
            app.ui()
                .set_profile_message("没有可显示的用户：链接里没有用户 ID，也没有登录账号".into());
            return;
        };

        let Some(client) = client else {
            let app = shared.borrow_mut();
            app.ui().set_profile_state(BAND_ERROR);
            app.ui().set_profile_message("没有可用的网络客户端".into());
            return;
        };

        rt.spawn(async move {
            let (profile, ids) =
                tokio::join!(client.user_profile(target), client.user_illust_ids(target));
            let profile = match profile {
                Ok(profile) => profile,
                Err(error) => {
                    let message = error.to_string();
                    on_ui(move |shared| {
                        let Ok(app) = shared.try_borrow_mut() else {
                            return;
                        };
                        if app.epoch != epoch || app.route != route {
                            return;
                        }
                        app.ui().set_profile_state(BAND_ERROR);
                        app.ui().set_profile_message(message.clone().into());
                        app.ui().set_grid_state(BAND_ERROR);
                        app.ui().set_grid_state_title("无法载入用户主页".into());
                        app.ui().set_grid_message(message.into());
                    });
                    return;
                }
            };
            let ids = ids.unwrap_or_default();
            let first: Vec<Id> = ids.iter().take(PROFILE_PAGE).copied().collect();
            let works = client.user_works(target, &first).await.unwrap_or_default();

            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                if app.epoch != epoch || app.route != route {
                    return;
                }
                let mut avatar_urls: Vec<String> = Vec::new();
                if let Some(url) = profile.avatar.clone() {
                    avatar_urls.push(url);
                }
                avatar_urls.extend(
                    works
                        .iter()
                        .filter_map(|illust| illust.author.avatar.clone()),
                );
                let state = ProfileState {
                    user: target,
                    profile,
                    ids,
                    loaded: first.len(),
                    works: works.clone(),
                };
                app.profile = Some(state);
                app.feed_owner = Some(route);
                app.publish_profile();
                app.render_feed(works, false);
                app.publish_profile();
                app.spawn_avatars(avatar_urls);
                app.spawn_profile_background();
                app.refresh_chrome();
            });
        });
    }

    fn spawn_profile_background(&mut self) {
        let Some(url) = self
            .profile
            .as_ref()
            .and_then(|state| state.profile.background.clone())
        else {
            return;
        };
        if self.avatars.contains_key(&url) {
            if let Some(image) = self.avatars.get(&url).cloned() {
                self.ui().set_profile_background(image);
            }
            return;
        }
        if !self.start_avatar(&url) {
            return;
        }
        let cache = self.cache.clone();
        let rt = self.rt.clone();
        let scale = self.ui().window().scale_factor().max(1.0);
        let target = crate::images::target_width(1200.0, scale);
        rt.spawn(async move {
            let raw = fetch_sized(&cache, &url, target).await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.avatar_loading.remove(&url);
                let Some(raw) = raw else {
                    return;
                };
                let image = raw.to_slint();
                app.avatars.insert(url.clone(), image.clone());
                app.ui().set_profile_background(image);
            });
        });
    }

    fn publish_profile(&self) {
        let ui = self.ui();
        let Some(state) = self.profile.as_ref() else {
            return;
        };
        let profile = &state.profile;
        ui.set_profile_name(profile.name.clone().into());
        ui.set_profile_account(profile.account.clone().into());
        ui.set_profile_uid(profile.id.to_string().into());
        ui.set_profile_comment(profile.comment.clone().into());
        ui.set_profile_webpage(profile.webpage.clone().unwrap_or_default().into());
        ui.set_profile_premium(profile.is_premium);
        ui.set_profile_followed(profile.is_followed);
        ui.set_profile_self(self.session.user_id == Some(profile.id));

        let avatar_url = profile.avatar.clone().unwrap_or_default();
        let (image, avatar_state) = match self.avatars.get(&avatar_url) {
            Some(image) => (image.clone(), BAND_OK),
            None if avatar_url.is_empty() => (slint::Image::default(), BAND_EMPTY),
            None => (slint::Image::default(), BAND_LOADING),
        };
        ui.set_profile_avatar(image);
        ui.set_profile_avatar_state(avatar_state);

        // Figures the server actually sent. A count it did not send is shown as
        // a dash rather than as a zero, because zero is a claim.
        let mut stats: Vec<ProfileStatItem> = vec![
            ProfileStatItem {
                label: "插画".into(),
                value: profile.illust_count.to_string().into(),
            },
            ProfileStatItem {
                label: "漫画".into(),
                value: profile.manga_count.to_string().into(),
            },
            ProfileStatItem {
                label: "关注".into(),
                value: profile.followees.to_string().into(),
            },
            ProfileStatItem {
                label: "粉丝".into(),
                value: profile
                    .followers
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "—".to_string())
                    .into(),
            },
        ];
        if !profile
            .webpage
            .clone()
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            stats.push(ProfileStatItem {
                label: "主页".into(),
                value: profile.webpage.clone().unwrap_or_default().into(),
            });
        }
        ui.set_profile_stats(ModelRc::from(Rc::new(VecModel::from(stats))));

        let total = state.ids.len();
        ui.set_profile_count_label(if total == 0 {
            "".into()
        } else {
            format!("已显示 {} / {} 件作品", state.works.len().min(total), total).into()
        });
        ui.set_profile_state(if state.works.is_empty() {
            self.profile_empty_state()
        } else {
            BAND_OK
        });
        if state.works.is_empty() {
            ui.set_profile_message(
                if total == 0 {
                    "这个用户还没有公开作品。"
                } else {
                    "作品载入失败，点击重试。"
                }
                .into(),
            );
        }
    }

    fn profile_empty_state(&self) -> i32 {
        match self.profile.as_ref() {
            Some(state) if state.ids.is_empty() => BAND_EMPTY,
            Some(_) => BAND_ERROR,
            None => BAND_LOADING,
        }
    }

    /// Append the next slice of the artist's works.
    fn load_profile_more(shared: &Shared) {
        let (target, next, epoch, client, rt) = {
            let mut app = shared.borrow_mut();
            if app.loading_more || app.reached_end {
                return;
            }
            let (target, next) = {
                let Some(state) = app.profile.as_ref() else {
                    return;
                };
                if state.loaded >= state.ids.len() {
                    app.reached_end = true;
                    return;
                }
                let next: Vec<Id> = state
                    .ids
                    .iter()
                    .skip(state.loaded)
                    .take(PROFILE_PAGE)
                    .copied()
                    .collect();
                (state.user, next)
            };
            app.loading_more = true;
            app.ui().set_grid_loading_more(true);
            (target, next, app.epoch, app.client.clone(), app.rt.clone())
        };
        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.loading_more = false;
            app.ui().set_grid_loading_more(false);
            app.reached_end = true;
            return;
        };
        rt.spawn(async move {
            let works = client.user_works(target, &next).await.unwrap_or_default();
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.loading_more = false;
                app.ui().set_grid_loading_more(false);
                if app.epoch != epoch || app.page != Destination::UserProfile {
                    return;
                }
                let urls: Vec<String> = works
                    .iter()
                    .filter_map(|illust| illust.author.avatar.clone())
                    .collect();
                let requested = next.len();
                let (exhausted, all_works) = {
                    let Some(state) = app.profile.as_mut() else {
                        return;
                    };
                    state.loaded += requested;
                    pixiv_types::domain::append_unique(&mut state.works, works);
                    (state.loaded >= state.ids.len(), state.works.clone())
                };
                if exhausted {
                    app.reached_end = true;
                }
                app.publish_profile();
                app.render_feed(all_works, false);
                app.spawn_avatars(urls);
            });
        });
    }

    /// Follow or unfollow the user on the open profile page.
    fn toggle_profile_follow(&mut self) {
        let Some(state) = self.profile.as_ref() else {
            return;
        };
        let target = state.user;
        if !self.session.is_authenticated() {
            self.notify(2, "需要登录后才能关注");
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.ui().set_profile_busy(true);
        let rt = self.rt.clone();
        rt.spawn(async move {
            let outcome = async {
                let current = client.follow_state(target).await?;
                let wanted = !current;
                client.follow(target, wanted).await?;
                let confirmed = client.follow_state(target).await?;
                Ok::<_, pixiv_types::PixivError>((wanted, confirmed))
            }
            .await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.ui().set_profile_busy(false);
                match outcome {
                    Ok((wanted, confirmed)) => {
                        if let Some(state) = app.profile.as_mut() {
                            state.profile.is_followed = confirmed;
                        }
                        app.publish_profile();
                        if confirmed == wanted {
                            app.notify(
                                1,
                                if confirmed {
                                    "已关注"
                                } else {
                                    "已取消关注"
                                },
                            );
                        } else {
                            app.notify(3, "关注状态未改变：服务器报告的状态与请求不一致");
                        }
                    }
                    Err(error) => app.notify(3, format!("关注失败：{error}")),
                }
            });
        });
    }

    // ------------------------------------------------------------- suggestions

    fn publish_suggestions(&self) {
        let ui = self.ui();
        let rows: Vec<SuggestionItem> = self
            .suggest
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| SuggestionItem {
                tag: row.tag.clone().into(),
                label: row.display().to_string().into(),
                count: row
                    .illust_count
                    .map(|count| format!("{count} 件"))
                    .unwrap_or_default()
                    .into(),
                local: matches!(
                    row.source,
                    pixiv_types::domain::SuggestionSource::LocalHistory
                ),
                active: index as i32 == self.suggest.active,
            })
            .collect();
        ui.set_suggestions(ModelRc::from(Rc::new(VecModel::from(rows))));
        ui.set_suggest_visible(self.suggest.visible);
        ui.set_suggest_note(self.suggest.note.clone().into());
    }

    fn suggest_close(&mut self) {
        self.suggest.generation += 1;
        self.suggest.timer.stop();
        self.suggest.visible = false;
        self.suggest.active = -1;
        self.suggest.rows.clear();
        self.suggest.note.clear();
        self.publish_suggestions();
    }

    /// Rows for an empty box: the server's own recommended tags, then this
    /// machine's search history, which is labelled as local.
    fn suggest_show_base(&mut self) {
        let mut rows = self.suggest.base.clone();
        let history = self.store.search_history(5).unwrap_or_default();
        let local = pixiv_types::domain::history_suggestions(&history, 5);
        let had_history = !local.is_empty();
        rows.extend(local);
        self.suggest.rows = rows;
        self.suggest.active = -1;
        self.suggest.visible = !self.suggest.rows.is_empty();
        self.suggest.note = if self.suggest.base.is_empty() && had_history {
            "只有本机搜索记录，Pixiv 没有返回推荐标签".to_string()
        } else if had_history {
            "上方为 Pixiv 推荐标签，下方为本机搜索记录".to_string()
        } else {
            String::new()
        };
        self.publish_suggestions();
    }

    /// The user typed. Debounce, then ask Pixiv.
    fn suggest_query(&mut self, shared: &Shared, text: &str) {
        self.suggest.generation += 1;
        self.suggest.query = text.trim().to_string();
        self.suggest.timer.stop();
        if self.suggest.query.is_empty() {
            self.suggest_show_base();
            return;
        }

        // Show the panel immediately with an explicit loading note: an empty
        // dropdown while typing reads as "no results".
        self.suggest.visible = true;
        self.suggest.active = -1;
        self.suggest.note = "正在向 Pixiv 请求联想…".to_string();
        self.suggest.rows.clear();
        self.publish_suggestions();

        let generation = self.suggest.generation;
        let shared = shared.clone();
        // 280ms: long enough that a normal typing run issues one request, short
        // enough that the list feels live.
        self.suggest.timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(280),
            move || {
                let (word, client, rt) = {
                    let app = shared.borrow();
                    (
                        app.suggest.query.clone(),
                        app.client.clone(),
                        app.rt.clone(),
                    )
                };
                if word.is_empty() {
                    return;
                }
                let Some(client) = client else {
                    let mut app = shared.borrow_mut();
                    app.suggest.note = "未登录：联想需要网络客户端".to_string();
                    app.publish_suggestions();
                    return;
                };
                rt.spawn(async move {
                    let rows = client.suggestions(&word, 10).await;
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        // A reply for a query the user has already moved past
                        // must not overwrite the newer list.
                        if app.suggest.generation != generation {
                            return;
                        }
                        match rows {
                            Ok(rows) if !rows.is_empty() => {
                                app.suggest.rows = rows;
                                app.suggest.active = -1;
                                app.suggest.visible = true;
                                app.suggest.note = String::new();
                            }
                            Ok(_) => {
                                app.suggest.rows.clear();
                                app.suggest.active = -1;
                                app.suggest.visible = true;
                                app.suggest.note =
                                    "Pixiv 没有返回匹配标签，按回车可直接搜索这个词".to_string();
                            }
                            Err(error) => {
                                // Local history is offered as a clearly separate
                                // aid, never dressed up as a Pixiv suggestion.
                                let history = app.store.search_history(5).unwrap_or_default();
                                app.suggest.rows =
                                    pixiv_types::domain::history_suggestions(&history, 5);
                                app.suggest.active = -1;
                                app.suggest.visible = true;
                                app.suggest.note =
                                    format!("联想服务不可用（{error}），以上为本机搜索记录");
                            }
                        }
                        app.publish_suggestions();
                    });
                });
            },
        );
    }

    /// Up / Down inside the search field.
    fn suggest_move(&mut self, key: &str) {
        if !self.suggest.visible || self.suggest.rows.is_empty() {
            return;
        }
        let count = self.suggest.rows.len() as i32;
        let down = key == "\u{F701}";
        let next = if down {
            if self.suggest.active + 1 >= count {
                0
            } else {
                self.suggest.active + 1
            }
        } else if self.suggest.active <= 0 {
            count - 1
        } else {
            self.suggest.active - 1
        };
        self.suggest.active = next;
        self.publish_suggestions();
    }

    /// The chosen row, or the typed text when no row is highlighted.
    fn suggest_resolve(&self, typed: &str) -> String {
        if self.suggest.visible
            && self.suggest.active >= 0
            && let Some(row) = self.suggest.rows.get(self.suggest.active as usize)
        {
            return row.tag.clone();
        }
        typed.to_string()
    }
}

// ---------------------------------------------------------------- callbacks

fn install_callbacks(app: &Shared) {
    let ui = app.borrow().ui();

    {
        let shared = app.clone();
        ui.on_width_changed(move |width| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.on_window_width(width);
        });
    }
    {
        let shared = app.clone();
        ui.on_navigate(move |page| App::navigate(&shared, page));
    }
    {
        let shared = app.clone();
        ui.on_go_back(move || App::travel(&shared, true));
    }
    {
        let shared = app.clone();
        ui.on_go_forward(move || App::travel(&shared, false));
    }
    {
        let shared = app.clone();
        ui.on_shell_shortcut(move |action| match action.as_str() {
            "escape" => {
                let (page, suggesting) = {
                    let app = shared.borrow();
                    (app.page, app.suggest.visible)
                };
                if suggesting {
                    shared.borrow_mut().suggest_close();
                } else if page == Destination::Detail {
                    App::travel(&shared, true);
                }
            }
            // An explicit browser hand-off is still available, but it is never
            // the default for an author link.
            "open-external" => {
                let target = {
                    let app = shared.borrow();
                    app.profile.as_ref().map(|state| state.user.get())
                };
                if let Some(uid) = target {
                    let _ = open::that(format!("https://www.pixiv.net/users/{uid}"));
                }
            }
            _ => {}
        });
    }
    {
        let shared = app.clone();
        ui.on_search_submitted(move |word| {
            // The highlighted suggestion wins over the raw text: that is what
            // pressing Enter on a dropdown row means everywhere else.
            let word = {
                let app = shared.borrow();
                app.suggest_resolve(word.as_str()).trim().to_string()
            };
            if word.is_empty() {
                let app = shared.borrow();
                app.notify(2, "请输入关键词");
                return;
            }
            {
                let mut app = shared.borrow_mut();
                app.suggest_close();
                app.search_word = word.clone();
                app.feed_owner = None;
                let _ = app.store.push_search(&word);
                // The list the empty state renders has to include what was just
                // searched for; `refresh_chrome` runs again on navigate but the
                // store write and the read are ordered here so the chip appears
                // without a second navigation.
                app.refresh_recent_searches();
                app.ui().set_search_text(word.clone().into());
            }
            App::navigate(&shared, Destination::Search);
        });
    }
    {
        let shared = app.clone();
        ui.on_open_illust(move |id| App::open_detail(&shared, id.as_str()));
    }
    {
        let shared = app.clone();
        ui.on_open_author(move |id| {
            // The author page is native now: no browser hand-off.
            App::open_profile(&shared, id.as_str());
        });
    }
    {
        let shared = app.clone();
        // A card's author link knows the user id, so it can go straight there.
        ui.on_open_card_author(move |id| App::open_profile(&shared, id.as_str()));
    }
    {
        let shared = app.clone();
        ui.on_open_profile(move |id| App::open_profile(&shared, id.as_str()));
    }
    {
        let shared = app.clone();
        ui.on_search_edited(move |text| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.suggest_query(&shared, text.as_str());
        });
    }
    {
        let shared = app.clone();
        ui.on_suggest_key(move |key| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.suggest_move(key.as_str());
        });
    }
    {
        let shared = app.clone();
        ui.on_suggest_chosen(move |tag| {
            let tag = tag.to_string();
            {
                let mut app = shared.borrow_mut();
                app.suggest_close();
                app.search_word = tag.clone();
                app.feed_owner = None;
                let _ = app.store.push_search(&tag);
                app.refresh_recent_searches();
                app.ui().set_search_text(tag.clone().into());
            }
            App::navigate(&shared, Destination::Search);
        });
    }
    {
        let shared = app.clone();
        ui.on_suggest_dismissed(move || {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.suggest_close();
        });
    }
    {
        let shared = app.clone();
        ui.on_profile_back(move || App::travel(&shared, true));
    }
    {
        let shared = app.clone();
        ui.on_profile_refresh(move || App::load_profile(&shared));
    }
    {
        let shared = app.clone();
        ui.on_profile_retry(move || App::load_profile(&shared));
    }
    {
        let shared = app.clone();
        ui.on_profile_follow(move || {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.toggle_profile_follow();
        });
    }
    {
        let shared = app.clone();
        ui.on_profile_load_more(move || App::load_profile_more(&shared));
    }
    {
        let shared = app.clone();
        ui.on_toggle_bookmark(move |id| {
            let id = id.to_string();
            let mut app = shared.borrow_mut();
            app.toggle_bookmark(&id);
        });
    }
    {
        let shared = app.clone();
        ui.on_open_tag(move |tag| {
            let tag = tag.to_string();
            {
                let mut app = shared.borrow_mut();
                app.search_word = tag.clone();
                app.feed_owner = None;
                let _ = app.store.push_search(&tag);
            }
            App::navigate(&shared, Destination::Search);
        });
    }
    {
        let shared = app.clone();
        ui.on_grid_measured(move |width| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.on_measured(width);
        });
    }
    {
        let shared = app.clone();
        ui.on_grid_scrolled(move |offset| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            let route = app.route;
            app.remember_scroll(route, offset);
        });
    }
    {
        let shared = app.clone();
        ui.on_grid_retry(move || {
            let page = shared.borrow().page;
            match page {
                Destination::Home => App::load_home(&shared),
                Destination::Discovery => App::load_discovery(&shared, false),
                Destination::UserProfile => App::load_profile(&shared),
                Destination::Detail => {
                    let id = shared.borrow().detail.as_ref().map(|d| d.id.get());
                    if let Some(id) = id {
                        App::open_detail_route(&shared, id, false);
                    }
                }
                _ => App::load_page(&shared, false),
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_grid_action(move || App::navigate(&shared, Destination::Settings));
    }
    {
        let shared = app.clone();
        ui.on_load_more(move || App::load_more(&shared));
    }
    {
        let shared = app.clone();
        ui.on_select_ranking_filter(move |index| {
            {
                let mut app = shared.borrow_mut();
                let kinds = RankingKind::ALL;
                let Some(kind) = usize::try_from(index)
                    .ok()
                    .and_then(|i| kinds.get(i).copied())
                else {
                    return;
                };
                if kind == app.ranking_kind && app.page == Destination::Ranking {
                    return;
                }
                app.ranking_kind = kind;
                app.feed_owner = None;
                app.ui().set_ranking_filter(index);
            }
            if shared.borrow().page != Destination::Ranking {
                App::navigate(&shared, Destination::Ranking);
            } else {
                App::reload_feed(&shared);
            }
        });
    }

    // ---- detail ----
    {
        let shared = app.clone();
        ui.on_detail_back(move || App::travel(&shared, true));
    }
    {
        let shared = app.clone();
        ui.on_detail_toggle_bookmark(move || {
            let id = {
                let app = shared.borrow();
                app.detail.as_ref().map(|d| d.id.to_string())
            };
            if let Some(id) = id {
                let mut app = shared.borrow_mut();
                app.toggle_bookmark(&id);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_open_author(move || {
            let id = {
                let app = shared.borrow();
                app.detail.as_ref().map(|d| d.author.id.to_string())
            };
            if let Some(id) = id {
                App::open_profile(&shared, &id);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_open_tag(move |tag| {
            let tag = tag.to_string();
            {
                let mut app = shared.borrow_mut();
                app.search_word = tag.clone();
                app.feed_owner = None;
                let _ = app.store.push_search(&tag);
            }
            App::navigate(&shared, Destination::Search);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_download_page(move || {
            let mut app = shared.borrow_mut();
            app.enqueue_download(false);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_download_all(move || {
            let mut app = shared.borrow_mut();
            app.enqueue_download(true);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_export_ugoira(move || {
            let mut app = shared.borrow_mut();
            app.export_ugoira();
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_follow(move || {
            let mut app = shared.borrow_mut();
            app.toggle_follow();
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_open_external(move || {
            let app = shared.borrow();
            if let Some(detail) = &app.detail {
                let _ = open::that(format!("https://www.pixiv.net/artworks/{}", detail.id));
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_retry(move || {
            let id = {
                let app = shared.borrow();
                app.detail.as_ref().map(|d| d.id.to_string())
            };
            if let Some(id) = id {
                App::open_detail(&shared, &id);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_select_page(move |index| {
            let app = shared.borrow();
            let ui = app.ui();
            ui.set_detail_page_index(index);
            ui.set_detail_zoom(1.0);
            ui.set_detail_pan_x(0.0);
            ui.set_detail_pan_y(0.0);
            ui.set_detail_fit_mode(true);
        });
    }
    // ---- viewer transform: all of it in `zoom`, all of it tested ----
    {
        let shared = app.clone();
        ui.on_detail_request_zoom(move |cursor_x, cursor_y, delta_y, view_w, view_h| {
            let app = shared.borrow();
            let ui = app.ui();
            let viewport = crate::zoom::Viewport::new(view_w, view_h);
            let current = ui.get_detail_zoom().max(crate::zoom::ZOOM_MIN);
            let next = crate::zoom::wheel_zoom(current, delta_y);
            if (next - current).abs() < f32::EPSILON {
                return;
            }
            let pan = crate::zoom::Pan {
                x: ui.get_detail_pan_x(),
                y: ui.get_detail_pan_y(),
            };
            let moved =
                crate::zoom::anchored_pan(pan, (cursor_x, cursor_y), viewport, next / current);
            let (image_w, image_h) = current_image_size(&ui, &app);
            let scale = crate::zoom::effective_scale(
                viewport,
                (image_w, image_h),
                ui.get_detail_fit_mode(),
                next,
            );
            let clamped = crate::zoom::clamp_pan(moved, viewport, (image_w, image_h), scale);
            ui.set_detail_zoom(next);
            ui.set_detail_pan_x(clamped.x);
            ui.set_detail_pan_y(clamped.y);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_request_pan(move |delta_x, delta_y| {
            let app = shared.borrow();
            let ui = app.ui();
            let pan = crate::zoom::Pan {
                x: ui.get_detail_pan_x() + delta_x,
                y: ui.get_detail_pan_y() + delta_y,
            };
            let viewport = crate::zoom::Viewport::new(
                ui.window().size().width as f32,
                ui.window().size().height as f32,
            );
            let (image_w, image_h) = current_image_size(&ui, &app);
            let scale = ui.get_detail_zoom();
            let clamped = crate::zoom::clamp_pan(pan, viewport, (image_w, image_h), scale);
            ui.set_detail_pan_x(clamped.x);
            ui.set_detail_pan_y(clamped.y);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_request_toggle_zoom(move |_cursor_x, _cursor_y| {
            let app = shared.borrow();
            let ui = app.ui();
            let fitting = ui.get_detail_fit_mode() && (ui.get_detail_zoom() - 1.0).abs() < 0.01;
            ui.set_detail_fit_mode(!fitting);
            ui.set_detail_zoom(1.0);
            ui.set_detail_pan_x(0.0);
            ui.set_detail_pan_y(0.0);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_reload_image(move || {
            let app = shared.borrow();
            app.reload_detail_page();
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_set_fit_mode(move |fit| {
            let app = shared.borrow();
            app.ui().set_detail_fit_mode(fit);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_set_zoom(move |zoom| {
            let app = shared.borrow();
            app.ui()
                .set_detail_zoom(zoom.clamp(crate::zoom::ZOOM_MIN, crate::zoom::ZOOM_MAX));
        });
    }
    {
        let shared = app.clone();
        ui.on_toggle_fullscreen(move || {
            let app = shared.borrow();
            let window = app.ui();
            let window = window.window();
            window.set_fullscreen(!window.is_fullscreen());
        });
    }
    // ---- theme ----
    // Two entry points, one implementation. The top-bar button is a two-state
    // quick switch (a single button can only honestly mean "give me the other
    // one"); the three-state choice lives in Settings, where "follow the system"
    // is a real option rather than a dead toggle.
    {
        let shared = app.clone();
        ui.on_toggle_theme(move || {
            let app = shared.borrow();
            let next = if app.ui().get_dark() {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            };
            set_theme_mode(&app, next);
        });
    }
    {
        let shared = app.clone();
        ui.on_select_theme_mode(move |mode| {
            let app = shared.borrow();
            set_theme_mode(&app, mode);
        });
    }
    // ---- settings ----
    {
        let shared = app.clone();
        ui.on_sign_in_browser(move || {
            let app = shared.borrow();
            let _ = open::that("https://www.pixiv.net/");
            app.notify(
                0,
                "浏览器已打开：登录后按 F12 → Application → Cookies 复制 PHPSESSID，再粘贴回来",
            );
        });
    }
    {
        let shared = app.clone();
        ui.on_submit_cookie(move || {
            let value = {
                let app = shared.borrow();
                app.ui().get_cookie_input().to_string()
            };
            let value = value.trim().to_string();
            if value.is_empty() {
                let app = shared.borrow();
                app.notify(2, "请先粘贴 PHPSESSID 的值");
                return;
            }
            let mut app = shared.borrow_mut();
            app.apply_cookie(value);
        });
    }
    {
        let shared = app.clone();
        ui.on_sign_out(move || {
            let mut app = shared.borrow_mut();
            app.session = Session::default();
            crate::session::clear(&app.data_dir);
            app.mode = Mode::Demo;
            if let Some(client) = app.client.clone() {
                client.set_session(Session::default());
            }
            app.feed_owner = None;
            let ui = app.ui();
            ui.set_authenticated(false);
            ui.set_session_user("".into());
            ui.set_demo_mode(true);
            app.notify(1, "已退出登录，本机会话文件已删除");
            app.refresh_chrome();
        });
    }
    {
        let shared = app.clone();
        ui.on_clear_cache(move || {
            let app = shared.borrow();
            let reclaimed = app.cache.clear_disk().unwrap_or(0);
            app.cache.prune_memory();
            app.refresh_chrome();
            app.notify(1, format!("已清理磁盘缓存 {}", human_bytes(reclaimed)));
        });
    }
    {
        let shared = app.clone();
        ui.on_clear_history(move || {
            let app = shared.borrow();
            let views = app.store.clear_view_history();
            let searches = app.store.clear_search_history();
            // The chips on the empty-search state read this table; leaving them on
            // screen after "已清除" would be a lie.
            app.refresh_recent_searches();
            if views.is_ok() && searches.is_ok() {
                app.notify(1, "已清除浏览历史与搜索历史");
            } else {
                app.notify(3, "清除历史失败，请查看日志");
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_clear_downloads(move || {
            let app = shared.borrow();
            app.downloads.clear_finished();
            app.push_downloads();
            app.notify(1, "已清除下载记录，磁盘上的文件保留");
        });
    }
    {
        let shared = app.clone();
        ui.on_choose_directory(move || {
            let mut app = shared.borrow_mut();
            let Some(dir) = crate::picker::pick_folder("选择下载保存目录") else {
                return;
            };
            if let Err(error) = std::fs::create_dir_all(&dir) {
                app.notify(3, format!("无法使用该目录：{error}"));
                return;
            }
            app.download_dir = dir.clone();
            app.downloads.set_directory(dir.clone());
            let _ = app
                .store
                .set_setting("download_dir", &dir.display().to_string());
            app.ui()
                .set_download_directory(dir.display().to_string().into());
            app.notify(1, format!("下载目录已改为 {}", dir.display()));
        });
    }
    {
        let shared = app.clone();
        ui.on_download_choose_directory(move || {
            if let Some(ui) = shared.borrow().ui.upgrade().as_ref() {
                let _ = ui; // keep the handle alive across the modal dialog
            }
            let mut app = shared.borrow_mut();
            let Some(dir) = crate::picker::pick_folder("选择下载保存目录") else {
                return;
            };
            if std::fs::create_dir_all(&dir).is_err() {
                app.notify(3, "无法使用该目录");
                return;
            }
            app.download_dir = dir.clone();
            app.downloads.set_directory(dir.clone());
            let _ = app
                .store
                .set_setting("download_dir", &dir.display().to_string());
            app.ui()
                .set_download_directory(dir.display().to_string().into());
            app.notify(1, format!("下载目录已改为 {}", dir.display()));
        });
    }
    {
        let shared = app.clone();
        ui.on_apply_template(move |template| {
            let template = template.trim().to_string();
            if template.is_empty() {
                let app = shared.borrow();
                app.notify(2, "模板不能为空");
                return;
            }
            if template.contains('/') || template.contains('\\') {
                let app = shared.borrow();
                app.notify(3, "模板不能包含路径分隔符");
                return;
            }
            let app = shared.borrow();
            app.downloads.set_template(template.clone());
            let _ = app.store.set_setting("filename_template", &template);
            app.ui().set_download_template(template.clone().into());
            app.notify(1, "文件名模板已应用，从下一个任务开始生效");
        });
    }
    {
        let shared = app.clone();
        ui.on_toggle_demo(move |demo| {
            let mut app = shared.borrow_mut();
            if demo {
                app.mode = Mode::Demo;
            } else if app.session.is_authenticated() {
                app.mode = Mode::Real;
            } else {
                app.notify(2, "未登录时只能使用演示模式，请先完成登录");
                app.ui().set_demo_mode(true);
                return;
            }
            app.feed_owner = None;
            app.ui().set_demo_mode(app.mode == Mode::Demo);
            let page = app.page;
            drop(app);
            App::load_page(&shared, false);
            let _ = page;
        });
    }
    {
        let shared = app.clone();
        ui.on_open_data_folder(move || {
            let app = shared.borrow();
            let _ = open::that(&app.data_dir);
        });
    }
    {
        ui.on_open_project(move || {
            let _ = open::that("https://github.com/HELPMEEADICE/Pixiv-Desktop");
        });
    }
    {
        let shared = app.clone();
        ui.on_dismiss_notice(move || {
            let app = shared.borrow();
            app.ui().set_notice("".into());
        });
    }
    // ---- downloads ----
    {
        let shared = app.clone();
        ui.on_download_cancel(move |id| {
            let app = shared.borrow();
            app.downloads.cancel(id.max(0) as u64);
            app.push_downloads();
        });
    }
    {
        let shared = app.clone();
        ui.on_download_retry(move |id| {
            let app = shared.borrow();
            app.downloads.retry(id.max(0) as u64);
            app.push_downloads();
        });
    }
    {
        let shared = app.clone();
        ui.on_download_retry_all(move || {
            let app = shared.borrow();
            let failed: Vec<u64> = app
                .downloads
                .tasks()
                .into_iter()
                .filter(|task| matches!(task.state, pixiv_download::TaskState::Failed(_)))
                .map(|task| task.id)
                .collect();
            for id in failed {
                app.downloads.retry(id);
            }
            app.push_downloads();
        });
    }
    {
        let shared = app.clone();
        ui.on_download_clear_finished(move || {
            let app = shared.borrow();
            app.downloads.clear_finished();
            app.push_downloads();
        });
    }
    {
        let shared = app.clone();
        ui.on_download_open_folder(move || {
            let app = shared.borrow();
            let _ = open::that(&app.download_dir);
        });
    }
    {
        let shared = app.clone();
        ui.on_download_open_task(move |id| {
            let app = shared.borrow();
            let task = app
                .downloads
                .tasks()
                .into_iter()
                .find(|task| task.id == id.max(0) as u64);
            // Reveal the actual file when it exists; otherwise fall back to the
            // directory rather than opening nothing at all.
            match task.and_then(|task| task.destination) {
                Some(path) if path.exists() => {
                    let _ = std::process::Command::new("explorer")
                        .arg(format!("/select,{}", path.display()))
                        .spawn();
                }
                _ => {
                    let _ = open::that(&app.download_dir);
                }
            }
        });
    }
}

/// Current page's intrinsic size, needed to clamp panning.
fn current_image_size(ui: &AppWindow, app: &App) -> (f32, f32) {
    let index = ui.get_detail_page_index().max(0) as usize;
    if let Some(models) = &app.detail_models
        && let Some(page) = models.row_data(index)
    {
        return (page.width.max(1) as f32, page.height.max(1) as f32);
    }
    (1.0, 1.0)
}

// ---------------------------------------------------------------- timers

fn start_timers(app: &Shared) {
    // Arm the system-theme watcher according to the persisted choice. It stops
    // itself immediately when the mode is not System.
    {
        let mode = app.borrow().theme_mode;
        app.borrow().watch_system_theme(mode);
    }
    // Layout diagnostics for high-DPI verification: `PIXIV_DESKTOP_TRACE=1` logs
    // the numbers the layout actually resolved to, once a second. This is how the
    // 100/125/150/200% scaling matrix is checked without a debugger.
    if std::env::var("PIXIV_DESKTOP_TRACE").is_ok() {
        let shared = app.clone();
        let trace = slint::Timer::default();
        trace.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(1000),
            move || {
                let Ok(app) = shared.try_borrow() else {
                    return;
                };
                let window = app.ui();
                let window = window.window();
                window.request_redraw();
                let ui = app.ui();
                let phys = window.size();
                let scale = window.scale_factor();
                // `gap` is the invariant the shell must hold: the status bar's
                // bottom edge has to land exactly on the shell's bottom edge. A
                // positive gap is the blank strip the layout bug used to produce.
                let shell = ui.get_shell_height();
                let bottom = ui.get_status_y() + ui.get_status_height();
                eprintln!(
                    "trace phys={}x{} scale={} logical={:.0}x{:.0} shell={:.0} body={:.0} status_y={:.0} gap={:.0} cols={} card={:.1}",
                    phys.width,
                    phys.height,
                    scale,
                    phys.width as f32 / scale,
                    phys.height as f32 / scale,
                    shell,
                    ui.get_body_height(),
                    ui.get_status_y(),
                    shell - bottom,
                    app.geometry.columns,
                    app.geometry.card_width
                );
            },
        );
        std::mem::forget(trace);
    }

    // The download queue is the only thing that needs polling: it is written by
    // a worker thread and read by a `VecModel`, and 500ms is far below the
    // threshold at which a progress bar looks stuck.
    let shared = app.clone();
    let tick = slint::Timer::default();
    tick.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(500),
        move || {
            let Ok(app) = shared.try_borrow() else {
                return;
            };
            if !app.downloads.tasks().is_empty() {
                app.push_downloads();
            }
        },
    );
    std::mem::forget(tick);
}

// ---------------------------------------------------------------- helpers

/// Which image URL a reload of `index` should fetch.
///
/// A work with a page list has one real URL per page; a work without one has
/// exactly one image, at the largest size the detail payload offered. Anything
/// else is an index the page list does not have, and refusing is the honest
/// answer - guessing a URL would download the wrong picture.
fn retry_url(pages: &[PageMeta], largest: &str, index: usize) -> Option<String> {
    match pages.get(index) {
        Some(meta) => Some(meta.urls.regular.to_string()),
        None if index == 0 => Some(largest.to_string()),
        None => None,
    }
}

fn ranking_index(kind: RankingKind) -> i32 {
    RankingKind::ALL
        .iter()
        .position(|candidate| *candidate == kind)
        .map(|index| index as i32)
        .unwrap_or(0)
}

/// Apply the theme, persist the choice and re-arm (or stop) the system watcher.
///
/// One function so the three entry points — start-up, the top-bar quick switch and
/// the Settings segmented control — cannot disagree about what a mode change means.
fn set_theme_mode(app: &App, mode: ThemeMode) {
    crate::theme::apply(&app.ui(), mode);
    let _ = app
        .store
        .set_setting(crate::theme::SETTING_KEY, crate::theme::setting_value(mode));
    app.watch_system_theme(mode);
}
fn persisted(store: &Store, key: &str) -> Option<String> {
    store.get_setting(key).ok().flatten()
}

fn human_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MB", bytes / (1024 * 1024))
    } else {
        format!("{} KB", bytes / 1024)
    }
}

fn illust_to_item(illust: &Illust, rank: i32) -> IllustItem {
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
    }
}

fn extension_for(url: &str) -> String {
    let lower = url.to_ascii_lowercase();
    for candidate in ["png", "webp", "gif", "jpeg", "jpg"] {
        if lower.contains(&format!(".{candidate}")) {
            return candidate.to_string();
        }
    }
    "jpg".to_string()
}

async fn fetch_sized(cache: &ImageCache, url: &str, target: u32) -> Option<RawImage> {
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

async fn fetch_sized_viewer(cache: &ImageCache, url: &str) -> Option<RawImage> {
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
async fn demo_raw(seed_id: Id, width: u32, height: u32, page: u64) -> Option<RawImage> {
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

fn to_download_rows(tasks: Vec<pixiv_download::DownloadTask>) -> Vec<DownloadRow> {
    tasks
        .into_iter()
        .map(|task| {
            let (state, message) = match &task.state {
                pixiv_download::TaskState::Queued => (0, String::new()),
                pixiv_download::TaskState::Running => (1, String::new()),
                pixiv_download::TaskState::Completed => (2, String::new()),
                pixiv_download::TaskState::Failed(message) => (3, message.clone()),
                pixiv_download::TaskState::Cancelled => (4, String::new()),
                pixiv_download::TaskState::Skipped => (5, String::new()),
            };
            DownloadRow {
                id: task.id as i32,
                title: task.request.title.clone().into(),
                author: task.request.author.clone().into(),
                subtitle: format!(
                    "第 {} / {} 页",
                    task.request.page_index + 1,
                    task.request.page_count.max(1)
                )
                .into(),
                progress: task.percent().map(|percent| percent as i32).unwrap_or(-1),
                state,
                message: message.into(),
                path: task
                    .destination
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
                    .into(),
            }
        })
        .collect()
}

fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("dev", "pixivdesktop", "PixivDesktop")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn downloads_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.picture_dir().map(|path| path.join("PixivDesktop")))
        .unwrap_or_else(|| PathBuf::from("PixivDesktop"))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{describe_session_error, history_illust, retry_url, to_download_rows};
    use pixiv_download::{DownloadRequest, DownloadTask, TaskState};
    use pixiv_types::domain::PageMeta;
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
