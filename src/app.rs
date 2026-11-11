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
use std::collections::HashMap;
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
use pixiv_types::Id;
use pixiv_types::domain::{Illust, PageMeta, RankingKind, SearchQuery};

use crate::images::{COVER_CONCURRENCY, CoverLoader};
use crate::layout::{self, GridGeometry};
use crate::route::History;
use crate::{AppWindow, Destination, DownloadRow, IllustItem, ViewerPage};

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
    history: History<Destination>,
    /// Destination the currently loaded feed belongs to, so going Back to a page
    /// whose content is still loaded does not re-fetch (and therefore does not
    /// lose the scroll position).
    feed_owner: Option<Destination>,
    scroll: Vec<(Destination, f32)>,

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

    // detail
    detail: Option<Illust>,
    detail_pages: Vec<PageMeta>,
    detail_models: Option<Rc<VecModel<ViewerPage>>>,
    detail_epoch: u64,
}

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
    if let Ok(id) = std::env::var("PIXIV_DESKTOP_START_ILLUST") {
        App::open_detail(&app, &id);
    }

    {
        let a = app.borrow();
        a.start_download_worker();
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

        let cache = ImageCache::new(CacheConfig {
            memory_budget_bytes: 192 * 1024 * 1024,
            disk_budget_bytes: 1024 * 1024 * 1024,
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

        let dark = std::env::var("PIXIV_DESKTOP_THEME")
            .ok()
            .map(|theme| theme == "dark")
            .unwrap_or_else(|| {
                persisted(&store, "theme")
                    .map(|value| value == "dark")
                    .unwrap_or(false)
            });
        let follow_system = persisted(&store, "follow_system_theme")
            .map(|value| value != "false")
            .unwrap_or(true);

        if let Some(ui) = ui.upgrade() {
            ui.set_dark(dark);
            ui.set_follow_system_theme(follow_system);
            ui.set_demo_mode(mode == Mode::Demo);
            ui.set_authenticated(session.is_authenticated());
            ui.set_session_user(session.user_name.clone().unwrap_or_default().into());
            ui.set_download_directory(download_dir.display().to_string().into());
            ui.set_download_template(template.clone().into());
            ui.set_template_input(template.into());
            ui.set_app_version(env!("CARGO_PKG_VERSION").into());
            ui.set_cache_memory_mb(192);
            ui.set_cache_disk_mb(1024);
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
            detail: None,
            detail_pages: Vec::new(),
            detail_models: None,
            detail_epoch: 0,
        }
    }

    fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("window alive")
    }

    // ------------------------------------------------------------- chrome

    fn refresh_chrome(&self) {
        let ui = self.ui();
        ui.set_page(self.page);
        ui.set_can_go_back(self.history.can_go_back());
        ui.set_can_go_forward(self.history.can_go_forward());
        ui.set_demo_mode(self.mode == Mode::Demo);
        ui.set_authenticated(self.session.is_authenticated());
        ui.set_session_user(self.session.user_name.clone().unwrap_or_default().into());
        ui.set_status_text(self.status_line().into());
        ui.set_cache_summary(self.cache_summary().into());
    }

    fn cache_summary(&self) -> String {
        let stats = self.cache.stats();
        let lookups = stats.memory_hits + stats.disk_hits;
        let total = lookups + stats.network_loads;
        format!(
            "内存 {} · 磁盘 {} MB · 命中 {}/{}",
            human_bytes(stats.memory_bytes as u64),
            stats.disk_bytes / (1024 * 1024),
            lookups,
            total.max(1)
        )
    }

    fn status_line(&self) -> String {
        let mode = if self.mode == Mode::Demo {
            "演示模式（本地数据）"
        } else {
            "真实 Pixiv 会话"
        };
        let auth = if self.session.is_authenticated() {
            "已登录"
        } else {
            "未登录 · 只读"
        };
        format!("{mode} · {auth} · {} 项", self.items.len())
    }

    fn notify(&self, kind: i32, message: impl Into<slint::SharedString>) {
        let ui = self.ui();
        ui.set_notice(message.into());
        ui.set_notice_kind(kind);
    }

    // ------------------------------------------------------------- pages

    fn navigate(shared: &Shared, page: Destination) {
        {
            let mut app = shared.borrow_mut();
            let current = app.page;
            app.history.visit(current, page);
            // Remember where the page we are leaving was scrolled to.
            let scroll = if current == Destination::Home {
                app.ui().get_home_scroll_y()
            } else {
                app.ui().get_grid_scroll_y()
            };
            app.remember_scroll(current, scroll);
            app.page = page;
            app.page_cursor = 1;
            app.loading_more = false;
            app.ui().set_page(page);
            app.ui().set_grid_loading_more(false);
            app.ui().set_ranking_filter(ranking_index(app.ranking_kind));
            app.ui().set_search_word(app.search_word.clone().into());
        }
        App::load_page(shared, false);
    }

    /// Navigate back/forward through history rather than to an arbitrary page.
    fn travel(shared: &Shared, backwards: bool) {
        let target = {
            let mut app = shared.borrow_mut();
            let current = app.page;
            let target = if backwards {
                app.history.back(current)
            } else {
                app.history.forward(current)
            };
            let Some(target) = target else {
                app.refresh_chrome();
                return;
            };
            let scroll = if current == Destination::Home {
                app.ui().get_home_scroll_y()
            } else {
                app.ui().get_grid_scroll_y()
            };
            app.remember_scroll(current, scroll);
            app.page = target;
            app.page_cursor = 1;
            app.loading_more = false;
            app.ui().set_page(target);
            app.ui().set_grid_loading_more(false);
            app.ui().set_search_word(app.search_word.clone().into());
            target
        };
        // Restoring rather than resetting: Back must land where you left off.
        let _ = target;
        App::load_page(shared, true);
    }

    fn remember_scroll(&mut self, page: Destination, offset: f32) {
        if let Some(slot) = self.scroll.iter_mut().find(|(p, _)| *p == page) {
            slot.1 = offset;
        } else {
            self.scroll.push((page, offset));
        }
    }

    fn stored_scroll(&self, page: Destination) -> f32 {
        self.scroll
            .iter()
            .find(|(p, _)| *p == page)
            .map(|(_, offset)| *offset)
            .unwrap_or(0.0)
    }

    fn load_page(shared: &Shared, restore_scroll: bool) {
        let (page, reuse_feed) = {
            let app = shared.borrow();
            (app.page, app.feed_owner == Some(app.page))
        };

        let is_feed_page = matches!(
            page,
            Destination::Home
                | Destination::Discovery
                | Destination::Ranking
                | Destination::Search
                | Destination::Bookmarks
                | Destination::History
        );

        if !is_feed_page {
            let app = shared.borrow();
            app.refresh_chrome();
            return;
        }

        if reuse_feed && restore_scroll {
            let app = shared.borrow();
            let offset = app.stored_scroll(page);
            let ui = app.ui();
            if page == Destination::Home {
                ui.set_home_scroll_y(offset);
            } else {
                ui.set_grid_scroll_y(offset);
            }
            drop(app);
            let app = shared.borrow();
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
            app.remember_scroll(page, 0.0);
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
                app.feed_owner = Some(page);
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
                            Destination::Bookmarks => client
                                .bookmarks(session.user_id.unwrap_or_default(), false, 1)
                                .await
                                .map(|r| r.items),
                            Destination::History => {
                                Ok(App::history_items_dummy(1).into_iter().collect())
                            }
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
                                app.feed_owner = Some(page);
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

    /// History is read from SQLite, not from Pixiv, so it needs no network.
    fn history_items_dummy(_page: u32) -> Vec<Illust> {
        Vec::new()
    }

    fn show_feed_error(&mut self, message: &str) {
        let ui = self.ui();
        ui.set_grid_state(3);
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
            ui.set_grid_message("这里还没有内容。试试换个关键词或标签。".into());
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
        const RAIL_EXPANDED: f32 = 240.0;
        const RAIL_COMPACT: f32 = 52.0;

        let narrow = width < NARROW_BREAKPOINT;
        let collapsed = self.ui().get_rail_collapsed();
        let rail = if narrow || collapsed {
            RAIL_COMPACT
        } else {
            RAIL_EXPANDED
        };
        let content = (width - rail).max(0.0);
        let ui = self.ui();
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
                            Destination::Bookmarks => client
                                .bookmarks(session.user_id.unwrap_or_default(), false, cursor)
                                .await
                                .map(|r| r.items),
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

    fn open_detail(shared: &Shared, id: &str) {
        let Ok(raw) = id.parse::<u64>() else {
            return;
        };
        let illust_id = Id(raw);

        let (mode, epoch) = {
            let mut app = shared.borrow_mut();
            let current = app.page;
            app.history.visit(current, Destination::Detail);
            if current == Destination::Home {
                let scroll = app.ui().get_home_scroll_y();
                app.remember_scroll(current, scroll);
            } else if current != Destination::Detail {
                let scroll = app.ui().get_grid_scroll_y();
                app.remember_scroll(current, scroll);
            }
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
        }

        let _ = self.store.record_view(&HistoryEntry {
            illust_id: illust.id.to_string(),
            title: illust.title.clone(),
            author: illust.author.name.clone(),
            thumb_url: illust.urls.grid().to_string(),
            viewed_at: now_secs(),
        });

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
        self.refresh_chrome();
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
                let currently = self.bookmarked_state(illust_id);
                let next = !currently;
                let weak = self.ui.clone();
                let rt = self.rt.clone();
                rt.spawn(async move {
                    let result = client.set_bookmark(illust_id, next, false).await;
                    let _ = invoke_from_event_loop(move || {
                        let Some(ui) = weak.upgrade() else {
                            return;
                        };
                        match result {
                            Ok(()) => {
                                ui.set_detail_bookmarked(next);
                                ui.set_notice(
                                    if next {
                                        "已加入收藏"
                                    } else {
                                        "已取消收藏"
                                    }
                                    .into(),
                                );
                                ui.set_notice_kind(1);
                            }
                            Err(error) => {
                                // No false success: the UI keeps the previous
                                // state and says what happened.
                                ui.set_detail_bookmarked(currently);
                                ui.set_notice(format!("收藏失败：{error}").into());
                                ui.set_notice_kind(3);
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
        let next = !self.ui().get_detail_following();
        self.ui().set_detail_busy(true);
        let rt = self.rt.clone();
        rt.spawn(async move {
            let result = client.follow(illust.author.id, next).await;
            on_ui(move |shared| {
                let Ok(app) = shared.try_borrow() else {
                    return;
                };
                app.ui().set_detail_busy(false);
                match result {
                    Ok(()) => {
                        app.ui().set_detail_following(next);
                        app.notify(
                            1,
                            if next {
                                "已关注画师"
                            } else {
                                "已取消关注"
                            },
                        );
                    }
                    Err(error) => app.notify(3, format!("关注失败：{error}")),
                }
            });
        });
    }

    fn apply_cookie(&mut self, cookie: String) {
        let session = Session {
            phpsessid: Some(cookie.clone()),
            user_id: None,
            user_name: None,
        };
        let Ok(client) = Client::new(ClientConfig::default(), session) else {
            self.notify(3, "无法创建网络客户端");
            return;
        };

        self.ui().set_busy(true);
        self.notify(0, "正在验证会话…");
        let rt = self.rt.clone();
        let weak = self.ui.clone();
        let data_dir = self.data_dir.clone();
        let probe_client = client.clone();
        rt.spawn(async move {
            // Exercising an authenticated endpoint is the only honest way to
            // know a pasted cookie works; Pixiv answers Unauthorized otherwise.
            let probe = probe_client.bookmarks(Id(0), false, 1).await;
            let accepted = !matches!(
                probe,
                Err(pixiv_types::PixivError::Unauthorized) | Err(pixiv_types::PixivError::Api(_))
            );
            let message = match &probe {
                Err(pixiv_types::PixivError::Unauthorized) => "会话无效或已过期".to_string(),
                Err(pixiv_types::PixivError::Api(error)) => format!("会话无效：{error}"),
                Err(error) => format!("会话验证失败：{error}"),
                Ok(_) => "会话已验证".to_string(),
            };
            let (user_id, user_name) = if accepted {
                match probe_client.user_profile(Id(0)).await {
                    Ok(profile) => (Some(profile.id), Some(profile.name)),
                    Err(_) => (None, None),
                }
            } else {
                (None, None)
            };
            let verified = Session {
                phpsessid: accepted.then_some(cookie),
                user_id,
                user_name,
            };
            let _ = invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                ui.set_busy(false);
                if accepted {
                    let _ = crate::session::save(&data_dir, &verified);
                    ui.set_authenticated(true);
                    ui.set_demo_mode(false);
                    ui.set_session_user(verified.user_name.clone().unwrap_or_default().into());
                    ui.set_cookie_input("".into());
                    ui.set_notice("会话已验证并保存".into());
                    ui.set_notice_kind(1);
                } else {
                    ui.set_notice(message.into());
                    ui.set_notice_kind(3);
                }
            });
        });

        self.client = Some(client);
        self.mode = Mode::Real;
        self.refresh_chrome();
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
        ui.on_shell_shortcut(move |action| {
            if action.as_str() == "escape" {
                let page = shared.borrow().page;
                if page == Destination::Detail {
                    App::travel(&shared, true);
                }
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_search_submitted(move |word| {
            let word = word.trim().to_string();
            if word.is_empty() {
                let app = shared.borrow();
                app.notify(2, "请输入关键词");
                return;
            }
            {
                let mut app = shared.borrow_mut();
                app.search_word = word.clone();
                app.feed_owner = None;
                let _ = app.store.push_search(&word);
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
            // Honest behaviour: the account page lives on pixiv.net, there is no
            // user feed screen in this build.
            let app = shared.borrow();
            let _ = open::that(format!("https://www.pixiv.net/users/{}", id));
            app.notify(0, "已在浏览器中打开画师主页");
        });
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
            let page = app.page;
            app.remember_scroll(page, offset);
        });
    }
    {
        let shared = app.clone();
        ui.on_grid_retry(move || {
            let page = shared.borrow().page;
            App::load_page(&shared, false);
            let _ = page;
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
                let app = shared.borrow();
                let _ = open::that(format!("https://www.pixiv.net/users/{id}"));
                app.notify(0, "已在浏览器中打开画师主页");
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
    {
        let shared = app.clone();
        ui.on_toggle_theme(move |dark| {
            let app = shared.borrow();
            app.ui().set_dark(dark);
            // A manual choice means "do not follow the system any more".
            app.ui().set_follow_system_theme(false);
            let _ = app
                .store
                .set_setting("theme", if dark { "dark" } else { "light" });
            let _ = app.store.set_setting("follow_system_theme", "false");
        });
    }
    {
        let shared = app.clone();
        ui.on_toggle_follow_system(move |follow| {
            let app = shared.borrow();
            app.ui().set_follow_system_theme(follow);
            let _ = app
                .store
                .set_setting("follow_system_theme", if follow { "true" } else { "false" });
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
                eprintln!(
                    "trace size={:?} physical scale={} shell_h={} measured_body={} columns={} card={:.1}",
                    window.size(),
                    window.scale_factor(),
                    app.ui().get_shell_height(),
                    app.measured_width,
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

fn ranking_index(kind: RankingKind) -> i32 {
    RankingKind::ALL
        .iter()
        .position(|candidate| *candidate == kind)
        .map(|index| index as i32)
        .unwrap_or(0)
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
