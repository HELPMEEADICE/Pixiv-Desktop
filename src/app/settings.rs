use super::*;

pub(crate) fn describe_session_error(error: &PixivError) -> String {
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

/// offline should not log the user out.
pub(crate) fn verify_stored_session(app: &Shared) {
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
        // The Session is built and sealed *here*, on the runtime: DPAPI, a file
        // create, a write and an fsync used to run inside the UI closure, on the
        // thread that paints (plan §2.3 R17). The closure only adopts what came back.
        let outcome = client.account().await.map(|account| {
            let refreshed = Session {
                phpsessid: stored.phpsessid.clone(),
                user_id: Some(account.id),
                user_name: Some(account.name.clone()),
                csrf_token: account.csrf_token.clone(),
            };
            let _ = crate::session::save(&data_dir, &refreshed);
            (account, refreshed)
        });
        on_ui(move |shared| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            match outcome {
                Ok((account, refreshed)) => {
                    client.set_session(refreshed.clone());
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

impl App {
    /// Keep `Theme.system-dark` in step with Windows while the user asked to
    /// follow it.
    ///
    /// Reads one registry DWORD every five seconds and *only* while the mode is
    /// `System`: picking Light or Dark stops the timer, so the poll costs nothing
    /// in the common case. Slint 1.18 exposes no colour-scheme notification and no
    /// raw window procedure, so this is the honest substitute — documented in
    /// `docs/UI-NOTES.md` rather than hidden.
    pub(crate) fn watch_system_theme(&self, mode: ThemeMode) {
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
}

impl App {
    // ------------------------------------------------------------- chrome

    pub(crate) fn refresh_chrome(&self) {
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
        // Published from the cache, not re-read: `refresh_chrome` runs on every
        // navigation and `search_history` takes the store mutex and prepares a
        // statement (plan §2.3 R16).
        self.publish_recent_searches();
    }
}

impl App {
    /// Re-read the stored search history, then publish it.
    ///
    /// The app has always written every submitted keyword to `search_history` and
    /// never read it back, so the table grew and the user could not reach any of it.
    /// The empty-search state renders these as chips that re-run the search.
    ///
    /// Called when the history actually changes - a submitted or picked search, a
    /// cleared history - never on a plain navigation.
    pub(crate) fn reload_recent_searches(&mut self) {
        self.recent_words = self.store.search_history(8).unwrap_or_default();
        self.publish_recent_searches();
    }
}

impl App {
    /// Push the cached words into the UI. No store access, so it is cheap enough
    /// for every `refresh_chrome`.
    pub(crate) fn publish_recent_searches(&self) {
        let labels: Vec<slint::SharedString> = self
            .recent_words
            .iter()
            .cloned()
            .map(slint::SharedString::from)
            .collect();
        // The same model every time: Slint keeps the items it is not told to drop.
        self.recent_searches.set_vec(labels);
        self.ui()
            .set_recent_searches(ModelRc::from(self.recent_searches.clone()));
    }
}

impl App {
    /// The settings page's "缓存占用" line.
    ///
    /// In demo mode it states the *budget* and takes no measurement at all, which is
    /// what makes the settings capture deterministic (G23). Two facts justify it:
    /// demo images never enter the cache - `demo_raw` renders them procedurally
    /// (app/helpers.rs:313) instead of asking `ImageCache` for them - so 0 B is the
    /// true figure rather than a convenient one; and a live figure describes this
    /// machine's leftovers from earlier real sessions, which is why the two settings
    /// baselines could only pass on a percentage budget (0.0028 % / 145 px of jitter,
    /// measured in docs/OPTIMIZATION-PLAN-R9.md §2.2). Real mode keeps the
    /// measurement, the disk figure and the hit rate.
    pub(crate) fn cache_summary(&self) -> String {
        if self.mode == Mode::Demo {
            return demo_cache_summary();
        }
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
}

/// The demo-mode cache line: the configured budgets, and nothing measured.
///
/// `CACHE_MEMORY_MB` / `CACHE_DISK_MB` are the budgets `App::new` hands to
/// `ImageCache`, so this cannot state a limit that is not in force - and the one
/// test below pins the exact string, because it is the input to a pixel baseline.
pub(crate) fn demo_cache_summary() -> String {
    format!(
        "已用 0 B / 上限 {} MB · 磁盘 0 B / 上限 {:.1} GB",
        CACHE_MEMORY_MB,
        CACHE_DISK_MB as f64 / 1024.0
    )
}

impl App {
    /// The one page-specific fact the status bar shows.
    ///
    /// Connection state (demo / signed-in / read-only) is *not* repeated here:
    /// the shell renders it once from the properties it already holds. The old
    /// line carried the mode and the auth state too, which is how one screen ended
    /// up saying "演示模式" in three separate places.
    pub(crate) fn status_line(&self) -> String {
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
}

impl App {
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
    pub(crate) fn apply_cookie(&mut self, cookie: String) {
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
            // Built and sealed on the runtime: the DPAPI seal, the file create, the
            // write and the fsync were all running inside the UI closure below, on the
            // thread that paints (plan §2.3 R17). The closure receives the finished
            // Session and does nothing but adopt it.
            let outcome = client.account().await.map(|account| {
                let verified = Session {
                    phpsessid: Some(cookie),
                    user_id: Some(account.id),
                    user_name: Some(account.name.clone()),
                    csrf_token: account.csrf_token.clone(),
                };
                let _ = crate::session::save(&data_dir, &verified);
                (account, verified)
            });
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.ui().set_busy(false);
                match outcome {
                    Ok((account, verified)) => {
                        // The probe client and the stored session must agree, or
                        // later mutations would run without a CSRF token.
                        client.set_session(verified.clone());
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

/// Apply the theme, persist the choice and re-arm (or stop) the system watcher.
///
/// One function so the three entry points — start-up, the top-bar quick switch and
/// the Settings segmented control — cannot disagree about what a mode change means.
pub(crate) fn set_theme_mode(app: &App, mode: ThemeMode) {
    crate::theme::apply(&app.ui(), mode);
    let _ = app
        .store
        .set_setting(crate::theme::SETTING_KEY, crate::theme::setting_value(mode));
    app.watch_system_theme(mode);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G23: two captures of this page are byte-identical only if this line is.
    ///
    /// It used to be "内存 96 MB · 磁盘 812 MB · 命中 41/57（71%）" - three numbers that
    /// move with the machine's cache history, which is why both settings baselines
    /// passed on a percentage budget instead of an exact one. The literal is pinned on
    /// purpose: if a budget or the wording moves, the baseline moves with it, and this
    /// is the test that says so before the capture does.
    #[test]
    fn the_demo_cache_line_states_the_budget_and_nothing_measured() {
        assert_eq!(
            demo_cache_summary(),
            "已用 0 B / 上限 192 MB · 磁盘 0 B / 上限 1.0 GB"
        );
    }

    /// No hit rate, no megabytes measured, nothing that a run can move: the same
    /// string twice, and no placeholder left behind.
    #[test]
    fn the_demo_cache_line_is_the_same_string_twice() {
        assert_eq!(demo_cache_summary(), demo_cache_summary());
        assert!(
            !demo_cache_summary().contains("命中"),
            "a rate is a measurement"
        );
        assert!(demo_cache_summary().contains("上限 192 MB"));
        assert!(demo_cache_summary().contains("上限 1.0 GB"));
    }
}
