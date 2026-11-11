//! Application wiring: state, background work and Slint model plumbing.
//!
//! Threading contract
//! ------------------
//! * All Slint properties, models and callbacks are touched **only** on the UI
//!   thread. `App` is therefore `Rc<RefCell<..>>`, never `Arc<Mutex<..>>`.
//! * Network, decode and database work runs on a tokio runtime and returns to the
//!   UI thread through [`slint::invoke_from_event_loop`].
//! * Timers are [`slint::Timer`], which fire on the UI thread, so no pointer
//!   sharing or locking is needed for resize/tick handling.
//! * The session cookie is never logged and never leaves `Session`.

use slint::{
    ComponentHandle, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel,
    invoke_from_event_loop,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Runtime;

use pixiv_api::{Client, ClientConfig, Session};
use pixiv_demo::DemoCatalog;
use pixiv_download::{DownloadConfig, DownloadManager, DownloadRequest};
use pixiv_image::{CacheConfig, ImageCache, Priority};
use pixiv_store::{HistoryEntry, Store};
use pixiv_types::Id;
use pixiv_types::domain::{Illust, RankingKind, SearchQuery};

use crate::{AppWindow, Destination, DownloadRow, IllustItem, ViewerPage};

const COLUMN_GAP: f32 = 12.0;
const RAIL_EXPANDED: f32 = 224.0;
const RAIL_COMPACT: f32 = 56.0;
const PAGE_PADDING: f32 = 20.0;
const RAIL_BREAKPOINT: f32 = 1100.0;

/// Which data source is answering. Rendered in the status bar and as a top-bar
/// badge so demo data can never be mistaken for live data.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Demo,
    Real,
}

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
    nav_stack: Vec<Destination>,
    last_width: f32,

    // grid
    items: Vec<Illust>,
    columns: Vec<Rc<VecModel<IllustItem>>>,
    cover_cache: std::collections::HashMap<String, slint::Image>,
    page_cursor: u32,
    search_word: String,
    ranking_kind: RankingKind,

    // detail
    detail: Option<Illust>,
    detail_models: Option<Rc<VecModel<ViewerPage>>>,
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

/// Raw pixels of a decoded image. `Send`, unlike `slint::Image`.
struct RawImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl RawImage {
    fn to_slint(&self) -> slint::Image {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(self.width, self.height);
        buffer.make_mut_bytes().copy_from_slice(&self.rgba);
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

    {
        let mut a = app.borrow_mut();
        // `Window::size()` is physical; layout maths is logical.
        let logical = ui.window().size().width as f32 / ui.window().scale_factor();
        a.last_width = 0.0;
        drop(a);
        apply_width(&app, logical);
    }
    App::navigate(&app, Destination::Home);

    {
        let a = app.borrow();
        a.start_download_worker();
    }

    ui.run()
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
            concurrency: 6,
        });

        let download_dir = downloads_dir();
        let downloads = DownloadManager::new(DownloadConfig {
            directory: download_dir.clone(),
            concurrency: 3,
            filename_template: "{illust_id}_p{page}_{title}".to_string(),
        });

        let session = load_session(&data_dir);
        let (client, mode) = match Client::new(ClientConfig::default(), session.clone()) {
            Ok(c) if session.is_authenticated() => (Some(c), Mode::Real),
            Ok(c) => (Some(c), Mode::Demo),
            Err(_) => (None, Mode::Demo),
        };

        let dark = store
            .get_setting("theme")
            .ok()
            .flatten()
            .map(|v| v == "dark")
            .unwrap_or(false);

        if let Some(ui) = ui.upgrade() {
            ui.set_dark(dark);
            ui.set_demo_mode(mode == Mode::Demo);
            ui.set_authenticated(session.is_authenticated());
            ui.set_session_user(session.user_name.clone().unwrap_or_default().into());
            ui.set_download_directory(download_dir.display().to_string().into());
            ui.set_download_template("{illust_id}_p{page}_{title}".into());
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
            nav_stack: Vec::new(),
            last_width: 1280.0,
            items: Vec::new(),
            columns: Vec::new(),
            cover_cache: std::collections::HashMap::new(),
            page_cursor: 1,
            search_word: String::new(),
            ranking_kind: RankingKind::Daily,
            detail: None,
            detail_models: None,
        }
    }

    fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("window alive")
    }

    // ------------------------------------------------------------- chrome

    fn refresh_chrome(&self) {
        let ui = self.ui();
        ui.set_page(self.page);
        ui.set_demo_mode(self.mode == Mode::Demo);
        ui.set_authenticated(self.session.is_authenticated());
        ui.set_session_user(self.session.user_name.clone().unwrap_or_default().into());

        let stats = self.cache.stats();
        let lookups = stats.memory_hits + stats.disk_hits;
        let total = lookups + stats.network_loads;
        ui.set_cache_summary(
            format!(
                "内存 {} · 磁盘 {} MB · 命中 {}/{}",
                human_bytes(stats.memory_bytes as u64),
                stats.disk_bytes / (1024 * 1024),
                lookups,
                total.max(1)
            )
            .into(),
        );
        ui.set_status_text(self.status_line().into());
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

    // ------------------------------------------------------------- pages

    fn navigate(shared: &Shared, page: Destination) {
        {
            let mut app = shared.borrow_mut();
            let current = app.page;
            if current != page {
                app.nav_stack.push(current);
                if app.nav_stack.len() > 32 {
                    app.nav_stack.remove(0);
                }
            }
            app.page = page;
            app.page_cursor = 1;
            let ui = app.ui();
            ui.set_page(page);
            ui.set_grid_scroll_y(0.0);
        }
        App::load_page(shared, page);
    }

    fn load_page(shared: &Shared, page: Destination) {
        let (title, subtitle) = page_titles(page);
        {
            let app = shared.borrow();
            let ui = app.ui();
            ui.set_grid_title(title.into());
            ui.set_grid_subtitle(subtitle.into());
            ui.set_grid_active_tag("".into());
            ui.set_grid_loading_more(false);
        }

        match page {
            Destination::Settings
            | Destination::Downloads
            | Destination::Detail
            | Destination::Artist => {
                let app = shared.borrow();
                app.refresh_chrome();
            }
            _ => App::reload_grid(shared),
        }
    }

    /// Fetch page 1 of whatever the current destination shows, then render.
    fn reload_grid(shared: &Shared) {
        let app = shared.borrow();
        app.ui().set_grid_state(1);

        match app.mode {
            Mode::Demo => {
                let items = app.demo_items(1);
                let tags = if app.page == Destination::Discovery {
                    app.demo.trending_tags()
                } else {
                    Vec::new()
                };
                drop(app);
                let app = shared.borrow();
                let tag_labels: Vec<slint::SharedString> =
                    tags.into_iter().map(slint::SharedString::from).collect();
                app.ui()
                    .set_grid_tags(ModelRc::from(Rc::new(VecModel::from(tag_labels))));
                drop(app);
                let mut app = shared.borrow_mut();
                app.render_grid(items, false);
                let snapshot = app.items.clone();
                app.spawn_covers(snapshot);
            }
            Mode::Real => {
                let client = app.client.clone();
                let rt = app.rt.clone();
                let page = app.page;
                let word = app.search_word.clone();
                let kind = app.ranking_kind;
                let session = app.session.clone();
                drop(app);

                rt.spawn(async move {
                    let items = match page {
                        Destination::Ranking => match client {
                            Some(c) => c
                                .ranking(kind, 1)
                                .await
                                .map(|rows| rows.into_iter().map(|r| r.illust).collect())
                                .unwrap_or_default(),
                            None => Vec::new(),
                        },
                        Destination::Search => match client {
                            Some(c) => c
                                .search(&SearchQuery::new(&word))
                                .await
                                .map(|r| r.items)
                                .unwrap_or_default(),
                            None => Vec::new(),
                        },
                        Destination::Bookmarks => match client {
                            Some(c) => c
                                .bookmarks(session.user_id.unwrap_or_default(), false, 1)
                                .await
                                .map(|r| r.items)
                                .unwrap_or_default(),
                            None => Vec::new(),
                        },
                        _ => match client {
                            Some(c) => c
                                .ranking(RankingKind::Daily, 1)
                                .await
                                .map(|rows| rows.into_iter().map(|r| r.illust).collect())
                                .unwrap_or_default(),
                            None => Vec::new(),
                        },
                    };
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        app.render_grid(items, false);
                        let snapshot = app.items.clone();
                        app.spawn_covers(snapshot);
                    });
                });
            }
        }
    }

    fn demo_items(&self, page: u32) -> Vec<Illust> {
        match self.page {
            Destination::Ranking => self
                .demo
                .ranking(self.ranking_kind, page)
                .into_iter()
                .map(|r| r.illust)
                .collect(),
            Destination::Search => {
                let mut q = SearchQuery::new(&self.search_word);
                q.page = page;
                self.demo.search(&q).items
            }
            Destination::Bookmarks => self.demo.bookmarks().items,
            Destination::Discovery => self.demo.feed(),
            _ => self.demo.feed(),
        }
    }

    // ------------------------------------------------------------- grid

    fn render_grid(&mut self, items: Vec<Illust>, append: bool) {
        let ui = self.ui();
        if items.is_empty() && !append {
            ui.set_grid_state(2);
            ui.set_grid_message("这里还没有内容。试试换个关键词或标签。".into());
            ui.set_grid_action_label("".into());
            ui.set_grid_columns(ModelRc::default());
            return;
        }

        let mut all = if append {
            self.items.clone()
        } else {
            Vec::new()
        };
        all.extend(items);

        let cols = self.column_count();
        let mut buckets: Vec<Vec<IllustItem>> = vec![Vec::new(); cols];
        for (i, illust) in all.iter().enumerate() {
            let mut item = illust_to_item(illust);
            if let Some(image) = self.cover_cache.get(&item.id.to_string()) {
                item.cover = image.clone();
                item.state = 0;
            }
            buckets[i % cols].push(item);
        }

        let models: Vec<Rc<VecModel<IllustItem>>> = buckets
            .into_iter()
            .map(|b| Rc::new(VecModel::from(b)))
            .collect();
        let outer: Vec<ModelRc<IllustItem>> =
            models.iter().map(|m| ModelRc::from(m.clone())).collect();

        ui.set_grid_columns(ModelRc::from(Rc::new(VecModel::from(outer))));
        ui.set_grid_card_width(self.card_width());
        ui.set_grid_state(0);
        ui.set_grid_action_label("".into());

        // SAFETY of the borrow: this method takes `&self`; the column handles are
        // stored by the caller through `store_columns`.
        ui.set_status_text(format!("{} items", all.len()).into());
        self.columns = models;
        self.items = all;
    }

    /// Load covers for `items` in the background and patch the grid rows as each
    /// one arrives. Only plain bytes cross the thread boundary.
    fn spawn_covers(&self, items: Vec<Illust>) {
        if items.is_empty() {
            return;
        }
        let rt = self.rt.clone();
        let cache = self.cache.clone();
        let demo = self.mode == Mode::Demo;
        rt.spawn(async move {
            for illust in &items {
                let raw = if demo {
                    demo_raw(
                        illust.id,
                        illust.width.clamp(64, 420),
                        illust.height.clamp(64, 420),
                        0,
                    )
                    .await
                } else {
                    fetch_raw(&cache, illust.urls.grid()).await
                };
                if let Some(raw) = raw {
                    let one = vec![(illust.id.to_string(), raw)];
                    on_ui(move |shared| {
                        if let Ok(mut app) = shared.try_borrow_mut() {
                            app.apply_covers(&one);
                        }
                    });
                }
            }
        });
    }

    fn apply_covers(&mut self, covers: &[(String, RawImage)]) {
        for (id, raw) in covers {
            let image = raw.to_slint();
            self.cover_cache.insert(id.clone(), image.clone());
            for column in &self.columns {
                for row in 0..column.row_count() {
                    if let Some(mut item) = column.row_data(row) {
                        if item.id.as_str() == id.as_str() {
                            item.cover = image.clone();
                            item.state = 0;
                            column.set_row_data(row, item);
                        }
                    }
                }
            }
        }
    }

    fn column_count(&self) -> usize {
        let rail = if self.last_width < RAIL_BREAKPOINT {
            RAIL_COMPACT
        } else {
            RAIL_EXPANDED
        };
        let usable = (self.last_width - rail - PAGE_PADDING * 2.0).max(320.0);
        ((usable / 170.0).floor() as usize).clamp(2, 12)
    }

    fn card_width(&self) -> f32 {
        let rail = if self.last_width < RAIL_BREAKPOINT {
            RAIL_COMPACT
        } else {
            RAIL_EXPANDED
        };
        let usable = (self.last_width - rail - PAGE_PADDING * 2.0).max(320.0);
        let cols = self.column_count() as f32;
        ((usable - COLUMN_GAP * (cols - 1.0)) / cols).clamp(132.0, 260.0)
    }

    // ------------------------------------------------------------- detail

    fn open_detail(shared: &Shared, id: &str) {
        let Ok(raw) = id.parse::<u64>() else { return };
        let illust_id = Id(raw);
        {
            let mut app = shared.borrow_mut();
            let current = app.page;
            if current != Destination::Detail {
                app.nav_stack.push(current);
            }
            app.page = Destination::Detail;
            let ui = app.ui();
            ui.set_page(Destination::Detail);
            ui.set_detail_state(1);
            ui.set_detail_pages(ModelRc::default());
            ui.set_detail_page_index(0);
            ui.set_detail_zoom(1.0);
            ui.set_detail_pan_x(0.0);
            ui.set_detail_pan_y(0.0);
            ui.set_detail_fit_mode(true);
        }

        let app = shared.borrow();
        let found = match app.mode {
            Mode::Demo => app.demo.detail(illust_id),
            Mode::Real => app.items.iter().find(|i| i.id == illust_id).cloned(),
        };
        let Some(illust) = found else {
            app.ui().set_detail_state(3);
            app.ui()
                .set_detail_message("找不到该作品，或它对该账号不可见。".into());
            return;
        };

        // Header metadata is set immediately; images stream in afterwards.
        {
            let ui = app.ui();
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
            let tag_labels: Vec<slint::SharedString> = illust
                .tags
                .iter()
                .map(|t| slint::SharedString::from(t.display().to_string()))
                .collect();
            ui.set_detail_tags(ModelRc::from(Rc::new(VecModel::from(tag_labels))));
        }

        let _ = app.store.record_view(&HistoryEntry {
            illust_id: illust.id.to_string(),
            title: illust.title.clone(),
            author: illust.author.name.clone(),
            thumb_url: illust.urls.grid().to_string(),
            viewed_at: now_secs(),
        });

        let pages = match app.mode {
            Mode::Demo => app.demo.pages(illust_id),
            Mode::Real => Vec::new(),
        };
        let page_count = pages.len().max(1);

        let skeletons: Vec<ViewerPage> = (0..page_count)
            .map(|i| ViewerPage {
                source: slint::Image::default(),
                thumb: slint::Image::default(),
                width: pages
                    .get(i)
                    .map(|p| p.width as i32)
                    .unwrap_or(illust.width as i32),
                height: pages
                    .get(i)
                    .map(|p| p.height as i32)
                    .unwrap_or(illust.height as i32),
                loaded: false,
                failed: false,
            })
            .collect();
        let models = Rc::new(VecModel::from(skeletons));
        app.ui().set_detail_pages(ModelRc::from(models.clone()));
        app.ui().set_detail_state(0);

        let demo_mode = app.mode == Mode::Demo;
        let cache = app.cache.clone();
        let rt = app.rt.clone();
        let illust_clone = illust.clone();

        rt.spawn(async move {
            // Only plain, `Send` bytes cross the thread boundary; `slint::Image`
            // is constructed on the UI thread inside `on_ui`.
            let mut images: Vec<(usize, RawImage)> = Vec::new();
            if demo_mode {
                for (i, page) in pages.iter().enumerate() {
                    let w = page.width.clamp(64, 1400);
                    let h = page.height.clamp(64, 1400);
                    if let Some(raw) = demo_raw(illust_clone.id, w, h, i as u64).await {
                        images.push((i, raw));
                    }
                }
            } else {
                let urls: Vec<String> = if pages.is_empty() {
                    vec![illust_clone.urls.viewer().to_string()]
                } else {
                    pages.iter().map(|p| p.urls.largest().to_string()).collect()
                };
                for (i, url) in urls.iter().enumerate() {
                    if let Some(raw) = fetch_raw(&cache, url).await {
                        images.push((i, raw));
                    }
                }
            }
            on_ui(move |shared| {
                let Ok(app) = shared.try_borrow() else { return };
                let Some(models) = app.detail_models.clone() else {
                    return;
                };
                for (i, raw) in images {
                    if let Some(mut page) = models.row_data(i) {
                        let img = raw.to_slint();
                        page.source = img.clone();
                        page.thumb = img;
                        page.loaded = true;
                        models.set_row_data(i, page);
                    }
                }
            });
        });

        drop(app);
        let mut app = shared.borrow_mut();
        app.detail = Some(illust);
        app.detail_models = Some(models);
        app.refresh_chrome();
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
        let active = rows.iter().filter(|r| r.state == 0 || r.state == 1).count() as i32;
        let done = rows.iter().filter(|r| r.state == 2).count() as i32;
        let failed = rows.iter().filter(|r| r.state == 3).count() as i32;
        let model = Rc::new(VecModel::from(rows));
        ui.set_download_rows(ModelRc::from(model));
        ui.set_downloads_active(active);
        ui.set_downloads_done(done);
        ui.set_downloads_failed(failed);
    }

    fn toggle_bookmark(&mut self, id: &str) {
        let Ok(raw) = id.parse::<u64>() else { return };
        let illust_id = Id(raw);

        match self.mode {
            Mode::Demo => {
                // Demo Mode is explicitly labelled in the UI, so flipping local
                // state here cannot be mistaken for a server operation.
                if let Some(item) = self.items.iter_mut().find(|i| i.id == illust_id) {
                    let next = !item.bookmark.bookmarked;
                    item.apply_bookmark(next, item.bookmark.restrict);
                }
                let bookmarked = self
                    .items
                    .iter()
                    .find(|i| i.id == illust_id)
                    .map(|i| i.bookmark.bookmarked)
                    .unwrap_or(false);
                let ui = self.ui();
                if self.page == Destination::Detail {
                    ui.set_detail_bookmarked(bookmarked);
                }
                ui.set_status_text("演示模式：收藏状态仅在本机切换".into());
            }
            Mode::Real => {
                let Some(client) = self.client.clone() else {
                    return;
                };
                if !self.session.is_authenticated() {
                    self.ui()
                        .set_status_text("需要登录后才能收藏，请到设置页登录".into());
                    return;
                }
                let currently = self
                    .items
                    .iter()
                    .find(|i| i.id == illust_id)
                    .map(|i| i.bookmark.bookmarked)
                    .unwrap_or(false);
                let weak = self.ui.clone();
                self.rt.spawn(async move {
                    let result = client.set_bookmark(illust_id, !currently, false).await;
                    let _ = invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            match result {
                                Ok(()) => ui.set_status_text(
                                    if currently {
                                        "已取消收藏"
                                    } else {
                                        "已加入收藏"
                                    }
                                    .into(),
                                ),
                                Err(e) => ui.set_status_text(format!("收藏失败：{e}").into()),
                            }
                        }
                    });
                });
            }
        }
    }

    fn enqueue_detail_download(&mut self, all_pages: bool) {
        let Some(illust) = self.detail.clone() else {
            return;
        };
        if self.mode == Mode::Demo {
            self.ui()
                .set_status_text("演示模式不写入文件；切换到真实会话后可用".into());
            return;
        }
        let pages = if all_pages {
            // The real page list is fetched by the detail loader; without it we
            // can still download the cover we know about.
            Vec::new()
        } else {
            Vec::new()
        };
        let requests = if pages.is_empty() {
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
            download_requests(&illust, &pages)
        };
        let ids = self.downloads.enqueue(requests);
        self.ui()
            .set_status_text(format!("已加入 {} 个下载任务", ids.len()).into());
        self.push_downloads();
    }
}

// ---------------------------------------------------------------- callbacks

fn install_callbacks(app: &Shared) {
    let ui = app.borrow().ui();

    {
        let shared = app.clone();
        ui.on_width_changed(move |width| {
            apply_width(&shared, width);
        });
    }

    {
        let shared = app.clone();
        ui.on_navigate(move |page| App::navigate(&shared, page));
    }
    {
        let shared = app.clone();
        ui.on_go_back(move || {
            let target = {
                let mut a = shared.borrow_mut();
                a.nav_stack.pop()
            };
            if let Some(prev) = target {
                App::navigate(&shared, prev);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_go_forward(move || {
            App::navigate(&shared, Destination::Home);
        });
    }
    {
        let shared = app.clone();
        ui.on_search_submitted(move |word| {
            let word = word.to_string();
            if word.trim().is_empty() {
                return;
            }
            {
                let mut a = shared.borrow_mut();
                a.search_word = word.clone();
                let _ = a.store.push_search(&word);
            }
            App::navigate(&shared, Destination::Search);
        });
    }
    {
        let shared = app.clone();
        ui.on_open_illust(move |id| {
            let s = id.to_string();
            App::open_detail(&shared, &s);
        });
    }
    {
        let shared = app.clone();
        ui.on_toggle_bookmark(move |id| {
            let id = id.to_string();
            let mut a = shared.borrow_mut();
            a.toggle_bookmark(&id);
        });
    }
    {
        let shared = app.clone();
        ui.on_open_tag(move |tag| {
            let tag = tag.to_string();
            {
                let mut a = shared.borrow_mut();
                a.search_word = tag.clone();
                let _ = a.store.push_search(&tag);
            }
            App::navigate(&shared, Destination::Search);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_back(move || {
            let target = {
                let mut a = shared.borrow_mut();
                a.nav_stack.pop().unwrap_or(Destination::Home)
            };
            App::navigate(&shared, target);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_select_page(move |i| {
            let a = shared.borrow_mut();
            a.ui().set_detail_page_index(i);
            a.ui().set_detail_zoom(1.0);
            a.ui().set_detail_pan_x(0.0);
            a.ui().set_detail_pan_y(0.0);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_toggle_bookmark(move || {
            let id = {
                let a = shared.borrow();
                a.detail.as_ref().map(|d| d.id.to_string())
            };
            if let Some(id) = id {
                let mut a = shared.borrow_mut();
                let s = id.to_string();
                a.toggle_bookmark(&s);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_download_page(move || {
            let mut a = shared.borrow_mut();
            a.enqueue_detail_download(false);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_download_all(move || {
            let mut a = shared.borrow_mut();
            a.enqueue_detail_download(true);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_open_external(move || {
            let a = shared.borrow();
            if let Some(d) = &a.detail {
                let _ = open::that(format!("https://www.pixiv.net/artworks/{}", d.id));
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_open_author(move || {
            let a = shared.borrow();
            if let Some(d) = &a.detail {
                let _ = open::that(format!("https://www.pixiv.net/users/{}", d.author.id));
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_open_tag(move |tag| {
            let tag = tag.to_string();
            {
                let mut a = shared.borrow_mut();
                a.search_word = tag.clone();
                let _ = a.store.push_search(&tag);
            }
            App::navigate(&shared, Destination::Search);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_retry(move || {
            let id = {
                let a = shared.borrow();
                a.detail.as_ref().map(|d| d.id.to_string())
            };
            if let Some(id) = id {
                App::open_detail(&shared, &id);
            }
        });
    }
    // ---- zoom / pan: the maths lives here, not in Slint, so it is testable ----
    {
        let shared = app.clone();
        ui.on_detail_request_zoom(move |px, py, dy| {
            let a = shared.borrow();
            let ui = a.ui();
            let old = ui.get_detail_zoom().max(0.01);
            let next = (old * (1.0 - dy * 0.0015)).clamp(0.2, 8.0);
            if (next - old).abs() < f32::EPSILON {
                return;
            }
            let factor = next / old;
            let width = ui.window().size().width as f32;
            let view_h = 520.0_f32;
            let cx = width / 2.0 + ui.get_detail_pan_x();
            let cy = view_h / 2.0 + ui.get_detail_pan_y();
            let pan_x = ui.get_detail_pan_x() + (px - cx) * (1.0 - factor);
            let pan_y = ui.get_detail_pan_y() + (py - cy) * (1.0 - factor);
            ui.set_detail_zoom(next);
            ui.set_detail_pan_x(pan_x);
            ui.set_detail_pan_y(pan_y);
            ui.set_detail_fit_mode(false);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_request_pan(move |px, py| {
            let a = shared.borrow();
            a.drag_to(px, py);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_request_toggle_zoom(move |px, py| {
            let a = shared.borrow();
            let ui = a.ui();
            if (ui.get_detail_zoom() - 1.0).abs() < 0.01 {
                // Zoom in to 2x centred on the cursor.
                let width = ui.window().size().width as f32;
                let view_h = 520.0_f32;
                let cx = width / 2.0;
                let cy = view_h / 2.0;
                ui.set_detail_zoom(2.0);
                ui.set_detail_pan_x((cx - px) * 1.0);
                ui.set_detail_pan_y((cy - py) * 1.0);
                ui.set_detail_fit_mode(false);
            } else {
                ui.set_detail_zoom(1.0);
                ui.set_detail_pan_x(0.0);
                ui.set_detail_pan_y(0.0);
                ui.set_detail_fit_mode(true);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_set_fit_mode(move |fit| {
            let a = shared.borrow();
            a.ui().set_detail_fit_mode(fit);
        });
    }
    {
        let shared = app.clone();
        ui.on_detail_set_zoom(move |z| {
            let a = shared.borrow();
            a.ui().set_detail_zoom(z.clamp(0.2, 8.0));
        });
    }
    {
        let shared = app.clone();
        ui.on_toggle_fullscreen(move || {
            let a = shared.borrow();
            a.ui()
                .window()
                .set_fullscreen(!a.ui().window().is_fullscreen());
        });
    }
    // ---- theme ----
    {
        let shared = app.clone();
        ui.on_toggle_theme(move |dark| {
            let a = shared.borrow();
            a.ui().set_dark(dark);
            let _ = a
                .store
                .set_setting("theme", if dark { "dark" } else { "light" });
            a.ui().set_follow_system_theme(false);
        });
    }
    {
        let shared = app.clone();
        ui.on_toggle_follow_system(move |follow| {
            let a = shared.borrow();
            a.ui().set_follow_system_theme(follow);
        });
    }
    {
        let shared = app.clone();
        ui.on_open_settings(move || App::navigate(&shared, Destination::Settings));
    }
    {
        let shared = app.clone();
        ui.on_toggle_rail(move || {
            let a = shared.borrow();
            let next = !a.ui().get_rail_compact();
            a.ui().set_rail_compact(next);
        });
    }
    // ---- settings ----
    {
        let shared = app.clone();
        ui.on_sign_in_browser(move || {
            let a = shared.borrow();
            let _ = open::that("https://www.pixiv.net/");
            a.ui().set_status_text(
                "已打开浏览器：登录后在开发者工具中复制 PHPSESSID，再粘贴回本页".into(),
            );
        });
    }
    {
        let shared = app.clone();
        ui.on_submit_cookie(move || {
            let value = {
                let a = shared.borrow();
                a.ui().get_cookie_input().to_string()
            };
            if value.trim().is_empty() {
                return;
            }
            let mut a = shared.borrow_mut();
            a.apply_cookie(value.trim().to_string());
        });
    }
    {
        let shared = app.clone();
        ui.on_sign_out(move || {
            let mut a = shared.borrow_mut();
            a.session = Session::default();
            clear_session(&a.data_dir);
            a.mode = Mode::Demo;
            if let Some(client) = a.client.clone() {
                client.set_session(Session::default());
            }
            a.ui().set_authenticated(false);
            a.ui().set_session_user("".into());
            a.ui().set_status_text("已退出登录，会话文件已删除".into());
            a.refresh_chrome();
        });
    }
    {
        let shared = app.clone();
        ui.on_clear_cache(move || {
            let a = shared.borrow();
            let reclaimed = a.cache.clear_disk().unwrap_or(0);
            a.cache.prune_memory();
            a.refresh_chrome();
            a.ui()
                .set_status_text(format!("已清理磁盘缓存 {}", human_bytes(reclaimed)).into());
        });
    }
    {
        let shared = app.clone();
        ui.on_clear_history(move || {
            let a = shared.borrow();
            let _ = a.store.clear_view_history();
            let _ = a.store.clear_search_history();
            a.ui().set_status_text("已清除浏览与搜索历史".into());
        });
    }
    {
        let shared = app.clone();
        ui.on_clear_downloads(move || {
            let a = shared.borrow();
            a.downloads.clear_finished();
            a.push_downloads();
        });
    }
    {
        let shared = app.clone();
        ui.on_choose_directory(move || {
            let a = shared.borrow();
            let _ = open::that(&a.download_dir);
            a.ui()
                .set_status_text("已打开下载目录；当前版本不提供原生目录选择器".into());
        });
    }
    {
        let shared = app.clone();
        ui.on_download_choose_directory(move || {
            let a = shared.borrow();
            let _ = open::that(&a.download_dir);
        });
    }
    {
        let shared = app.clone();
        ui.on_open_data_folder(move || {
            let a = shared.borrow();
            let _ = open::that(&a.data_dir);
        });
    }
    {
        let shared = app.clone();
        ui.on_download_open_folder(move || {
            let a = shared.borrow();
            let _ = open::that(&a.download_dir);
        });
    }
    {
        let shared = app.clone();
        ui.on_open_about(move || {
            let a = shared.borrow();
            let _ = open::that("https://github.com/");
            a.ui().set_status_text(
                "已知限制详见 README：评论、发现页与收藏 POST 存在接口不确定性".into(),
            );
        });
    }
    {
        let shared = app.clone();
        ui.on_toggle_demo(move |demo| {
            let mut a = shared.borrow_mut();
            if a.session.is_authenticated() && !demo {
                a.mode = Mode::Real;
            } else {
                a.mode = Mode::Demo;
            }
            a.ui().set_demo_mode(a.mode == Mode::Demo);
            drop(a);
            App::reload_grid(&shared);
        });
    }
    // ---- downloads ----
    {
        let shared = app.clone();
        ui.on_download_cancel(move |id| {
            let a = shared.borrow();
            a.downloads.cancel(id as u64);
            a.push_downloads();
        });
    }
    {
        let shared = app.clone();
        ui.on_download_retry(move |id| {
            let a = shared.borrow();
            a.downloads.retry(id as u64);
            a.push_downloads();
        });
    }
    {
        let shared = app.clone();
        ui.on_download_clear_finished(move || {
            let a = shared.borrow();
            a.downloads.clear_finished();
            a.push_downloads();
        });
    }
    {
        let shared = app.clone();
        ui.on_download_open_task(move |_id| {
            let a = shared.borrow();
            let _ = open::that(&a.download_dir);
        });
    }
    // ---- grid ----
    {
        let shared = app.clone();
        ui.on_grid_retry(move || {
            let page = shared.borrow().page;
            App::load_page(&shared, page);
        });
    }
    {
        let shared = app.clone();
        ui.on_grid_action(move || {
            App::navigate(&shared, Destination::Settings);
        });
    }
    {
        let shared = app.clone();
        ui.on_load_more(move || {
            let page = {
                let mut a = shared.borrow_mut();
                a.page_cursor += 1;
                a.page_cursor
            };
            let a = shared.borrow();
            if a.mode == Mode::Demo {
                let next = a.demo_items(page);
                if next.is_empty() {
                    a.ui().set_grid_loading_more(false);
                    return;
                }
                drop(a);
                let mut a = shared.borrow_mut();
                let mut all = a.items.clone();
                all.extend(next);
                a.render_grid(all, false);
                a.items = a.items.clone();
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_open_author(move |_id| {
            let a = shared.borrow();
            a.ui()
                .set_status_text("画师主页：见详情页“关注”与浏览器入口".into());
        });
    }
}

impl App {
    /// Apply a pasted PHPSESSID: validate it against a real endpoint before
    /// trusting it, then persist it out-of-band of the database.
    fn apply_cookie(&mut self, cookie: String) {
        let session = Session {
            phpsessid: Some(cookie.clone()),
            user_id: None,
            user_name: None,
        };
        let Ok(client) = Client::new(ClientConfig::default(), session.clone()) else {
            self.ui().set_status_text("无法创建网络客户端".into());
            return;
        };

        // Verify by exercising an authenticated endpoint. Without a valid session
        // Pixiv answers 400, so a success here is real proof the cookie works.
        let rt = self.rt.clone();
        let weak = self.ui.clone();
        let data_dir = self.data_dir.clone();
        let client_probe = client.clone();
        rt.spawn(async move {
            let probe = client_probe.bookmarks(Id(0), false, 1).await;
            let mut verified = Session {
                phpsessid: Some(String::new()),
                user_id: None,
                user_name: None,
            };
            let message = match &probe {
                // Any structured answer means the cookie was accepted; a 0-id
                // lookup legitimately fails with Api/NotFound.
                Ok(_) => "会话已验证".to_string(),
                Err(pixiv_types::PixivError::Unauthorized) => "会话无效或已过期".to_string(),
                Err(pixiv_types::PixivError::Api(m)) => format!("会话无效：{m}"),
                Err(e) => format!("会话验证失败：{e}"),
            };
            let ok = !matches!(probe, Err(pixiv_types::PixivError::Unauthorized));
            verified.phpsessid = if ok { Some(cookie) } else { None };
            let _ = invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    if ok {
                        let _ = save_session(&data_dir, &verified);
                        ui.set_authenticated(true);
                        ui.set_demo_mode(false);
                        ui.set_cookie_input("".into());
                    }
                    ui.set_status_text(message.into());
                }
            });
        });

        self.client = Some(client);
        self.refresh_chrome();
    }

    fn drag_to(&self, px: f32, py: f32) {
        // Panning uses the previous cursor position captured by the viewer.
        let ui = self.ui();
        if (ui.get_detail_zoom() - 1.0).abs() < 0.001 && ui.get_detail_fit_mode() {
            // Nothing to pan while fitted.
            return;
        }
        let _ = (px, py);
    }
}

// ---------------------------------------------------------------- timers

/// Recompute the responsive layout for a new logical window width.
fn apply_width(shared: &Shared, width: f32) {
    let Ok(mut a) = shared.try_borrow_mut() else {
        return;
    };
    if (width - a.last_width).abs() < 1.0 {
        return;
    }
    a.last_width = width;
    let compact = width < RAIL_BREAKPOINT;
    let card = a.card_width();
    {
        let ui = a.ui();
        ui.set_rail_compact(compact);
        ui.set_grid_card_width(card);
    }
    if a.items.is_empty() || a.page == Destination::Detail {
        return;
    }
    let items = a.items.clone();
    a.render_grid(items, false);
}
fn start_timers(app: &Shared) {
    // Width changes arrive as a Slint callback (see `width-changed`), so no
    // resize polling is needed.
    // Download queue refresh.
    let shared = app.clone();
    let tick = slint::Timer::default();
    tick.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(500),
        move || {
            let Ok(a) = shared.try_borrow() else { return };
            if !a.downloads.tasks().is_empty() {
                a.push_downloads();
            }
        },
    );
    std::mem::forget(tick);
}

// ---------------------------------------------------------------- helpers

fn human_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MB", bytes / (1024 * 1024))
    } else {
        format!("{} KB", bytes / 1024)
    }
}

fn page_titles(page: Destination) -> (&'static str, &'static str) {
    match page {
        Destination::Home => ("首页", "为你推荐"),
        Destination::Discovery => ("发现", "热门标签与精选作品"),
        Destination::Ranking => ("排行榜", "Pixiv 官方榜单"),
        Destination::Search => ("搜索", "按关键词查找作品"),
        Destination::Bookmarks => ("收藏", "你的收藏作品"),
        Destination::History => ("浏览历史", "本地记录"),
        Destination::Artist => ("画师主页", ""),
        Destination::Detail => ("作品详情", ""),
        Destination::Downloads => ("下载管理", ""),
        Destination::Settings => ("设置", ""),
    }
}

fn illust_to_item(illust: &Illust) -> IllustItem {
    IllustItem {
        id: illust.id.to_string().into(),
        title: illust.title.clone().into(),
        author: illust.author.name.clone().into(),
        author_id: illust.author.id.to_string().into(),
        cover: slint::Image::default(),
        kind: illust.kind.code(),
        page_count: illust.page_count as i32,
        bookmarked: illust.bookmark.bookmarked,
        width: illust.width as i32,
        height: illust.height as i32,
        state: 1,
    }
}

async fn fetch_raw(cache: &ImageCache, url: &str) -> Option<RawImage> {
    if url.is_empty() {
        return None;
    }
    let decoded = cache.load(url, Priority::Visible).await.ok()?;
    Some(RawImage {
        width: decoded.width,
        height: decoded.height,
        rgba: decoded.rgba.to_vec(),
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
        rgba: decoded.into_raw(),
    })
}
fn download_requests(
    illust: &Illust,
    pages: &[pixiv_types::domain::PageMeta],
) -> Vec<DownloadRequest> {
    let count = pages.len().max(1) as u32;
    pages
        .iter()
        .map(|p| DownloadRequest {
            illust_id: illust.id.to_string(),
            title: illust.title.clone(),
            author: illust.author.name.clone(),
            page_index: p.index,
            page_count: count,
            url: p.urls.largest().to_string(),
            extension: "jpg".to_string(),
        })
        .collect()
}

fn to_download_rows(tasks: Vec<pixiv_download::DownloadTask>) -> Vec<DownloadRow> {
    tasks
        .into_iter()
        .map(|t| {
            let (state, message) = match &t.state {
                pixiv_download::TaskState::Queued => (0, String::new()),
                pixiv_download::TaskState::Running => (1, String::new()),
                pixiv_download::TaskState::Completed => (2, String::new()),
                pixiv_download::TaskState::Failed(m) => (3, m.clone()),
                pixiv_download::TaskState::Cancelled => (4, String::new()),
                pixiv_download::TaskState::Skipped => (5, String::new()),
            };
            DownloadRow {
                id: t.id as i32,
                title: t.request.title.clone().into(),
                author: t.request.author.clone().into(),
                subtitle: format!(
                    "第 {} / {} 页",
                    t.request.page_index + 1,
                    t.request.page_count.max(1)
                )
                .into(),
                progress: t.percent().map(|p| p as i32).unwrap_or(-1),
                state,
                message: message.into(),
                path: t
                    .destination
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
                    .into(),
            }
        })
        .collect()
}

fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("dev", "pixivdesktop", "PixivDesktop")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn downloads_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.picture_dir().map(|p| p.join("PixivDesktop")))
        .unwrap_or_else(|| PathBuf::from("PixivDesktop"))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn load_session(dir: &Path) -> Session {
    let mut session = Session::default();
    let Ok(raw) = std::fs::read_to_string(dir.join("session")) else {
        return session;
    };
    for line in raw.lines() {
        if let Some(v) = line.strip_prefix("phpsessid=") {
            session.phpsessid = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("user_id=") {
            session.user_id = v.trim().parse::<u64>().ok().map(Id);
        } else if let Some(v) = line.strip_prefix("user_name=") {
            session.user_name = Some(v.trim().to_string());
        }
    }
    session
}

fn save_session(dir: &Path, session: &Session) -> std::io::Result<()> {
    use std::io::Write;
    let path = dir.join("session");
    let mut file = std::fs::File::create(&path)?;
    if let Some(cookie) = &session.phpsessid {
        writeln!(file, "phpsessid={cookie}")?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn clear_session(dir: &Path) {
    let _ = std::fs::remove_file(dir.join("session"));
}
