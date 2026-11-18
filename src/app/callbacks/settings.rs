use super::*;

pub(crate) fn install(app: &Shared) {
    let ui = app.borrow().ui();
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
            // The sweep walks the whole disk tier. It used to run right here, on the
            // thread that paints the window - UI-NOTES 8a claimed it had been moved
            // to the runtime and the code never was. `spawn_blocking` because it is
            // a blocking filesystem walk, not async work.
            let (cache, rt) = {
                let app = shared.borrow();
                app.ui().set_busy(true);
                app.notify(0, "正在清理磁盘缓存…");
                (app.cache.clone(), app.tasks.clone())
            };
            rt.spawn(async move {
                let sweep = cache.clone();
                let reclaimed =
                    tokio::task::spawn_blocking(move || sweep.clear_disk().unwrap_or(0))
                        .await
                        .unwrap_or(0);
                cache.prune_memory();
                on_ui(move |shared| {
                    let Ok(app) = shared.try_borrow() else {
                        return;
                    };
                    app.ui().set_busy(false);
                    app.refresh_chrome();
                    app.notify(1, format!("已清理磁盘缓存 {}", human_bytes(reclaimed)));
                });
            });
        });
    }

    {
        let shared = app.clone();
        ui.on_clear_history(move || {
            let (views, searches) = {
                let mut app = shared.borrow_mut();
                let views = app.store.clear_view_history();
                let searches = app.store.clear_search_history();
                // The chips on the empty-search state read this table; leaving them
                // on screen after "已清除" would be a lie. This is a history change,
                // so the cache is refreshed here rather than on the next navigation.
                app.reload_recent_searches();
                (views, searches)
            };
            let app = shared.borrow();
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
            // Same rule as the downloads page: the modal dialog pumps the message
            // loop with the main window still enabled, so the borrow starts after
            // it returns (plan §2.3 R12). The window itself is held across the loop,
            // which is all the old code was buying with a borrow.
            let _window = shared.borrow().ui.upgrade();
            let Some(dir) = crate::picker::pick_folder("选择下载保存目录") else {
                return;
            };
            let created = std::fs::create_dir_all(&dir);
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            if let Err(error) = created {
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
}
