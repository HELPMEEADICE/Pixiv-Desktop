use super::*;

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
        Some("novels") => App::navigate(&app, Destination::Novels),
        _ => App::navigate(&app, Destination::Home),
    }
    if let Ok(id) = std::env::var("PIXIV_DESKTOP_START_USER") {
        App::open_profile(&app, &id);
    }
    if let Ok(id) = std::env::var("PIXIV_DESKTOP_START_ILLUST") {
        App::open_detail(&app, &id);
    }
    // PIXIV_DESKTOP_START_NOVEL=<id> opens the novel page; adding
    // PIXIV_DESKTOP_START_READER=1 goes straight into the reader, which is what the
    // reader capture and the reader gate use.
    if let Ok(id) = std::env::var("PIXIV_DESKTOP_START_NOVEL") {
        if std::env::var_os("PIXIV_DESKTOP_START_READER").is_some() {
            if let Ok(raw) = id.trim().parse::<u64>() {
                App::open_reader_route(&app, raw);
            }
        } else {
            App::open_novel_from(&app, &id);
        }
    }

    {
        let a = app.borrow();
        a.start_download_worker();
    }
    // One honest line about the state the app is in, deferred until the event loop
    // is up so the auto-dismiss timer is actually scheduled. It also doubles as the
    // visual proof that the shell-level notification bar works.
    // Skipped in forced-demo captures: a notice that fades on a timer is the one
    // thing on the shell whose presence depends on when the screenshot is taken.
    if !forced_demo() {
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
pub(crate) fn apply_window_size(app: &Shared) {
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
    pub(crate) fn new(ui: slint::Weak<AppWindow>, rt: Arc<Runtime>) -> Self {
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

        // One row model for the life of the process: `push_downloads` patches it
        // rather than swapping it.
        let download_rows = Rc::new(VecModel::<DownloadRow>::default());
        // Same rule for the recent-search chips: one model, patched when the
        // history changes, published on every navigation without a store read.
        let recent_searches = Rc::new(VecModel::<slint::SharedString>::default());
        // One store read at startup fills the cache `refresh_chrome` publishes from;
        // after this the words are re-read only when the history changes.
        let recent_words = store.search_history(8).unwrap_or_default();
        recent_searches.set_vec(
            recent_words
                .iter()
                .cloned()
                .map(slint::SharedString::from)
                .collect::<Vec<_>>(),
        );

        // `record_view` takes the store mutex and scans MAX(seq); on the frame
        // thread that was a visible cost per opened work. One bounded queue, one
        // consumer, so the rows are still written in the order they were seen.
        let (view_queue, mut view_rx) = tokio::sync::mpsc::channel::<HistoryEntry>(VIEW_QUEUE);
        {
            let store = store.clone();
            rt.spawn(async move {
                while let Some(entry) = view_rx.recv().await {
                    let _ = store.record_view(&entry);
                }
            });
        }

        // `PIXIV_DESKTOP_DEMO=1` forces the demo data source, for the capture
        // harness. Two properties are needed and both fall out of ignoring the
        // stored session: a screenshot baseline must never contain the signed-in
        // user's own feed, and it must not change between two runs on one tree.
        // Everything below already has a demo path, so nothing else has to know.
        let session = if forced_demo() {
            Session::default()
        } else {
            crate::session::load(&data_dir)
        };
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
            ui.set_download_rows(ModelRc::from(download_rows.clone()));
            ui.set_recent_searches(ModelRc::from(recent_searches.clone()));
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

        // Read before the literal: the store is moved into the struct below, and the
        // reader's preferences are one small row that has to be read once anyway.
        let reader_prefs_json = store.novel_setting("novel.reader.prefs").ok().flatten();

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
            download_rows,
            data_dir,
            download_dir,
            page: Destination::Home,
            route: Route::plain(Destination::Home),
            history: History::new(HISTORY_LIMIT),
            feed_owner: None,
            scroll: HashMap::new(),
            scroll_order: VecDeque::new(),
            measured_width: 0.0,
            geometry: GridGeometry::for_width(858.0),
            items: Vec::new(),
            columns: Vec::new(),
            column_heights: Vec::new(),
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
            recent_words,
            recent_searches,
            view_queue,
            detail: None,
            detail_author_avatar_url: None,
            detail_pages: Vec::new(),
            detail_models: None,
            detail_epoch: 0,
            notice_timer: slint::Timer::default(),
            theme_mode,
            theme_timer: slint::Timer::default(),
            selection: Selection::default(),
            palette: PaletteState::default(),
            playback: PlaybackState::default(),
            // The reader's preferences are read once, here, and clamped on the way
            // in: a hand-edited or half-written JSON blob can only produce values
            // the sliders can express.
            novel: {
                let mut state = NovelState::new();
                if let Some(json) = reader_prefs_json {
                    state.prefs = pixiv_novel::prefs::ReaderPrefs::from_json(&json);
                }
                state
            },
        }
    }
}

// ---------------------------------------------------------------- timers

pub(crate) fn start_timers(app: &Shared) {
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
            // One `tasks()` snapshot per tick, taken inside `push_downloads`:
            // the emptiness test used to cost a second deep clone of the whole
            // queue (plan §2.3 R8).
            app.push_downloads();
        },
    );
    std::mem::forget(tick);
}
