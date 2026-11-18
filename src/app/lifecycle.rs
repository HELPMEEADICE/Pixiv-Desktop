//! Application lifecycle: startup, and a shutdown that actually finishes.
//!
//! # The defect this module closes
//!
//! Closing the window hid it and returned from `ui.run()`, and nothing else
//! happened. The process then tore itself down in whatever order the operating
//! system chose:
//!
//! * `thread_local APP` still held an `Rc<RefCell<App>>`, and `App` owns
//!   `Arc<Runtime>`. Tokio's multi-threaded runtime spawns worker threads that park
//!   on a condvar; dropping the last `Arc` signals them, but only when it is
//!   dropped, and a `thread_local` is finalised *during* thread teardown - after
//!   `main` has returned, in an order the C runtime picks.
//! * Every `slint::Timer` was leaked with `std::mem::forget`. A forgotten timer is
//!   never stopped, so the 500 ms download tick could fire while the window was
//!   already gone, and its closure holds another `Rc<RefCell<App>>` - one more
//!   strong reference to a runtime nobody was going to shut down deliberately.
//! * Nothing told the tokio tasks to stop. Downloads, image decodes, novel fetches
//!   and the history writer each held their own clones of runtime-owned state and
//!   simply kept going.
//!
//! The result was a process that could outlive its window: the ordering was left to
//! chance, and under real work (a download in flight, a large decode) the process
//! stayed behind with no window at all.
//!
//! # What replaces it
//!
//! One explicit sequence, run once, on the UI thread, before `main` returns:
//!
//! 1. **Stop accepting work.** `shutting_down` is set, so anything that would start
//!    a new background task returns immediately instead.
//! 2. **Cancel.** The shared `CancellationToken` is cancelled. Every spawned task
//!    selects on it, so a request in flight is dropped rather than awaited.
//! 3. **Stop the timers.** They are owned by `App` now, not forgotten, so they can be
//!    stopped and their callbacks cannot fire again.
//! 4. **Flush what is worth keeping.** The reading position is written, bounded by a
//!    deadline: a state save must never be the reason an exit hangs.
//! 5. **Drop the App and park the runtime.** The `thread_local` is cleared first, so
//!    it cannot hold the last strong reference while the runtime is being torn down,
//!    and `shutdown_timeout` gives the workers a bounded window to observe the
//!    cancellation instead of leaving the process to chance.
//!
//! The whole thing is bounded by [`SHUTDOWN_BUDGET`]. If a step overruns, the next one
//! still runs - a slow network call is not allowed to hold the window open.

use super::*;

/// How long the orderly half of shutdown may take, in total.
///
/// Sized from the work it covers: one SQLite write and one runtime park. Anything
/// longer means something is stuck, and the answer to that is to finish the exit
/// anyway rather than to wait.
pub(crate) const SHUTDOWN_BUDGET: Duration = Duration::from_secs(3);

/// How long the runtime gets to park its workers before the process leaves without
/// waiting for them.
const RUNTIME_PARK_BUDGET: Duration = Duration::from_millis(1500);

pub fn run() -> Result<(), slint::PlatformError> {
    let rt =
        Arc::new(Runtime::new().map_err(|e| slint::PlatformError::Other(format!("tokio: {e}")))?);
    let ui = AppWindow::new()?;
    // A launch that cannot open a single store still opens the window, on the
    // sentence that says why: a panic here is a window that vanishes with no
    // message on the screen at all (T8).
    let app: Shared = match App::new(ui.as_weak(), rt) {
        Ok(app) => Rc::new(RefCell::new(app)),
        Err(error) => {
            tracing::error!(%error, "no history store could be opened; the window reports it");
            let message =
                format!("无法打开本地数据库：{error}。请检查数据目录是否可写，然后重新启动。");
            // The home page is the destination this window starts on, and each of its
            // bands draws its own state: the reason has to be on all four, or the only
            // thing a user sees is a set of skeletons that never fill (T8).
            ui.set_home_feed_state(3);
            ui.set_home_feed_message(message.clone().into());
            ui.set_home_latest_state(3);
            ui.set_home_latest_message(message.clone().into());
            ui.set_home_tags_state(3);
            ui.set_home_tags_message(message.clone().into());
            ui.set_home_artists_state(3);
            ui.set_home_artists_message(message.clone().into());
            ui.set_grid_state(3);
            ui.set_grid_state_title("本地数据库无法打开".into());
            ui.set_grid_state_glyph("warning".into());
            ui.set_grid_message(message.clone().into());
            ui.set_status_text("本地数据库不可用：这台机器上的记录无法保存".into());
            return ui.run();
        }
    };
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
        app.borrow().retain_timer(hello);
    }

    // Closing the main window ends the application. There is no tray icon and no
    // background residency mode, so this is the documented semantic rather than an
    // accident of the default handler: the close is accepted, the window hides, and
    // the event loop returns, which is what reaches the shutdown sequence below. A
    // future tray feature has to change this line instead of silently changing what
    // the X button does.
    ui.window()
        .on_close_requested(|| slint::CloseRequestResponse::HideWindow);

    let result = ui.run();
    shutdown(&app);
    result
}

/// The one exit path.
///
/// Runs on the UI thread after the event loop has returned. The steps are ordered by
/// what they unblock: cancellation first (so nothing new starts), then the timers (so
/// nothing fires again), then the flush (the only part that loses data if skipped),
/// then the drop that ends the runtime.
pub(crate) fn shutdown(app: &Shared) {
    let started = std::time::Instant::now();
    tracing::info!("shutting down");

    // ---- 1 + 2: stop accepting work, and cancel what is running --------------
    // Taken in one borrow so a task cannot slip in between the flag and the token.
    {
        let mut app = match app.try_borrow_mut() {
            Ok(app) => app,
            // A callback is mid-flight and holds the borrow. Nothing can be
            // cancelled from here, but the teardown below still runs, and that is
            // what ends the runtime - so the exit still completes.
            Err(_) => {
                tracing::warn!("shutdown ran while the app was borrowed; skipping the flush");
                release(app, started);
                return;
            }
        };
        app.tasks.begin_shutdown();
        app.stop_timers();
        // The reading position is the one piece of state that is expensive to lose
        // and cheap to write, so it is flushed here, inside the budget.
        let deadline = SHUTDOWN_BUDGET.saturating_sub(started.elapsed());
        app.flush_on_exit(deadline);
    }

    release(app, started);
}

/// Drop the last owners and park the runtime, in the one order that terminates.
///
/// Split out of the sequence above so the borrowed-app edge case cannot skip it: a
/// missed flush costs one reading position, a missed teardown costs a process.
fn release(app: &Shared, started: std::time::Instant) {
    // ---- the reference chain, named ----------------------------------------
    //
    // At this point `Rc::strong_count(app)` is the number of *Slint callbacks* that
    // captured a clone: every `ui.on_*(move |..| { let shared = app.clone(); .. })`
    // in `src/app/callbacks/*` and in the modules that register their own. Measured
    // on this tree, that is 114. They live inside the `AppWindow`, and the `AppWindow`
    // lives until the component is dropped - which happened *after* `ui.run()`
    // returned, so the runtime was still owned by callbacks nobody could reach, and
    // the process's exit depended on the order the component finally unwound in.
    //
    // Clearing the handlers is what breaks the chain. Slint keeps them in the
    // component's own property table, so resetting each one drops its closure - and
    // with it the `Rc` - while the component is still alive and the UI thread is
    // still ours. After that the App is reachable exactly once, from this function,
    // and its drop is what parks the runtime.
    detach_callbacks(app);

    // Order matters. The thread-local is cleared *first* so the slot cannot hold the
    // last strong reference while the runtime is being torn down. What remains is the
    // App itself, whose drop ends the runtime.
    APP.with(|slot| slot.borrow_mut().take());

    // The task handle is taken out first, while the App is still definite, so the
    // park below cannot be skipped by the App's own drop order.
    let tasks = match app.try_borrow() {
        Ok(app) => app.tasks.clone(),
        Err(_) => {
            tracing::warn!("the app stayed borrowed at exit; the runtime was not parked");
            return;
        }
    };

    // Drop every remaining owner of the App, then park the runtime.
    //
    // `Rc::try_unwrap` is attempted but not required: callbacks registered on pages
    // that are constructed lazily can still hold clones, and a missed unwrap must not
    // cost the park. Whichever way it goes, the App goes out of scope here.
    match Rc::try_unwrap(app.clone()) {
        Ok(cell) => drop(cell.into_inner()),
        Err(still_shared) => {
            tracing::warn!(
                refs = Rc::strong_count(&still_shared),
                "the app was still shared at exit; parking the runtime anyway"
            );
            drop(still_shared);
        }
    }

    // Parked explicitly instead of being left to process teardown: shutdown_timeout
    // gives the workers a bounded window to observe the cancellation and then abandons
    // whatever is left, which is what keeps a stuck socket from holding the process
    // open. This works regardless of how many `Tasks` clones a task still holds, which
    // is the case that made the first version of this function silently do nothing.
    let budget = RUNTIME_PARK_BUDGET.min(SHUTDOWN_BUDGET.saturating_sub(started.elapsed()));
    // The result is reported rather than assumed: a park that did not happen means
    // the runtime left the app while a task still held it, and that is the difference
    // between a clean exit and a process that lingers.
    if !tasks.park(budget) {
        tracing::warn!("the runtime was already parked or is still owned by a task");
    }
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        "exit complete"
    );
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

/// Open the history database: the file first, memory second.
///
/// The `expect` this replaces was the round's second non-test panic (T8), and a
/// panic on the UI thread is a window that disappears without a word. A disk that
/// cannot be opened is not fatal - the session runs, it just will not be
/// remembered - so it comes back as the sentence the shell shows; a store that
/// cannot even be allocated is returned, and the caller opens the window on its
/// own error state instead of throwing the process away.
pub(crate) fn open_history_store(
    path: &std::path::Path,
) -> Result<(Store, Option<String>), pixiv_store::StoreError> {
    match Store::open(path) {
        Ok(store) => Ok((store, None)),
        Err(error) => {
            tracing::warn!(
                %error,
                "the history database could not be opened; this session runs from memory"
            );
            let notice = format!(
                "本地历史数据库无法打开（{error}）：本次使用内存记录，浏览历史与设置不会保存"
            );
            Store::open_in_memory().map(|store| (store, Some(notice)))
        }
    }
}

impl App {
    /// Write the state that has to survive the exit, on a deadline.
    ///
    /// Two things only: the reader's position and preferences (the reading page is the
    /// one screen whose value is entirely in where you left it) and nothing else. The
    /// history queue is deliberately *not* flushed here - its consumer is a task on
    /// the runtime, and the runtime is parked immediately afterwards, so draining it
    /// here would either block on a dead worker or race it. A history row is a
    /// convenience; a lost reading position is a lost session.
    ///
    /// `deadline` is what is left of [`SHUTDOWN_BUDGET`]. The writes are small, so in
    /// practice it is never approached; it exists so that a locked database file
    /// cannot turn "close the window" into "wait".
    pub(crate) fn flush_on_exit(&mut self, deadline: Duration) {
        if deadline.is_zero() {
            tracing::warn!("shutdown budget spent; the reading position was not written");
            return;
        }
        let started = std::time::Instant::now();
        self.flush_novel_state_blocking(deadline);
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "exit state flushed"
        );
    }

    /// Hold a timer for as long as the app lives, instead of forgetting it.
    ///
    /// `std::mem::forget` was how the startup timers stayed alive, and it is also why
    /// they could never be stopped: a forgotten `slint::Timer` has no handle, so
    /// nothing can reach it and its callback keeps a strong reference to the App for
    /// the whole life of the process.
    pub(crate) fn retain_timer(&self, timer: slint::Timer) {
        self.timers.borrow_mut().push(timer);
    }

    /// Stop every timer this app started, and drop them.
    pub(crate) fn stop_timers(&self) {
        for timer in self.timers.borrow_mut().drain(..) {
            timer.stop();
        }
    }
}

impl App {
    pub(crate) fn new(
        ui: slint::Weak<AppWindow>,
        rt: Arc<Runtime>,
    ) -> Result<Self, pixiv_store::StoreError> {
        let tasks = Tasks::new(rt);
        let data_dir = data_dir();
        if let Err(error) = std::fs::create_dir_all(&data_dir) {
            // Not fatal - the store open below reports the same directory - but a
            // permission problem that only shows up as a missing history is a mystery
            // (G41).
            tracing::warn!(%error, dir = %data_dir.display(), "data directory could not be created");
        }

        // A broken database must never stop the app from starting, and it must not
        // start silently either: the file is tried first, memory second, and the
        // failure with no fallback is *returned* rather than unwrapped (T8).
        let (store, store_notice) = open_history_store(&data_dir.join("pixiv-desktop.db"))?;

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
        if let Err(error) = std::fs::create_dir_all(&download_dir) {
            // Each download reports its own failure; this line is what explains why
            // they all fail (G41).
            tracing::warn!(%error, dir = %download_dir.display(), "download directory could not be created");
        }
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
        // The local-record band gets the same treatment (B7): one model, patched by
        // the publish that knows which record changed.
        let download_local_rows = Rc::new(VecModel::<LocalRow>::default());
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
            tasks.spawn(async move {
                while let Some(entry) = view_rx.recv().await {
                    // One history row fewer is the only consequence, which is exactly
                    // why it has to be said somewhere (G41).
                    if let Err(error) = store.record_view(&entry) {
                        tracing::debug!(%error, "history row could not be written");
                    }
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
            ui.set_download_local_rows(ModelRc::from(download_local_rows.clone()));
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

        let app = Self {
            ui,
            tasks: tasks.clone(),
            timers: RefCell::new(Vec::new()),
            store,
            cache,
            demo: DemoCatalog::new(),
            client,
            mode,
            session,
            downloads,
            download_rows,
            download_local_rows,
            download_local_drawn: RefCell::new(Vec::new()),
            task_paths: RefCell::new(HashMap::new()),
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
            cover_ids: RefCell::new(Vec::new()),
            epoch: 0,
            page_cursor: 1,
            reached_end: false,
            loading_more: false,
            search_word: String::new(),
            ranking_kind: RankingKind::Daily,
            controls: FeedControls::default(),
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
            viewer_viewport: (0.0, 0.0),
            detail: None,
            detail_author_avatar_url: None,
            detail_pages: Vec::new(),
            detail_models: None,
            detail_epoch: 0,
            detail_masked: false,
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
        };
        // Said once, on the shell's own notice bar: a session that cannot remember
        // anything must not look like one that can (T8).
        if let Some(notice) = store_notice {
            app.notify(2, notice);
        }
        Ok(app)
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
        // Owned, not forgotten: see the module docs. The trace timer is only armed
        // under PIXIV_DESKTOP_TRACE, but it is the same lifetime question.
        app.borrow().retain_timer(trace);
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
            // A tick that lands between "shutdown began" and "timers stopped" must
            // do nothing: it would take the borrow the shutdown sequence is waiting
            // for.
            if app.tasks.is_closing() {
                return;
            }
            // One `tasks()` snapshot per tick, taken inside `push_downloads`:
            // the emptiness test used to cost a second deep clone of the whole
            // queue (plan §2.3 R8).
            app.push_downloads();
        },
    );
    // The 500 ms tick is the timer whose callback most needs to stop at exit: it
    // takes the borrow and reads the download manager, and it was the one kept alive
    // by `mem::forget` with no handle to stop it.
    app.borrow().retain_timer(tick);
}
