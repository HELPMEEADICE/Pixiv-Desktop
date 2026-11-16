use super::*;

pub(crate) fn install(app: &Shared) {
    let ui = app.borrow().ui();
    // F1: the band is read once here rather than on the first tick, because the
    // records outlive the session and a reader who opens Downloads before
    // downloading anything must still see what is already on disk.
    app.borrow().publish_local_band();
    {
        let shared = app.clone();
        ui.on_local_open(move |path| {
            if let Ok(app) = shared.try_borrow() {
                app.open_local(path.as_str());
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_local_reveal(move |path| {
            if let Ok(app) = shared.try_borrow() {
                app.reveal_local(path.as_str());
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_local_redownload(move |path| {
            if let Ok(app) = shared.try_borrow() {
                app.redownload_local(path.as_str());
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_local_forget(move |id| {
            if let Ok(app) = shared.try_borrow() {
                app.forget_local(id);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_download_choose_directory(move || {
            // `pick_folder` is a modal Win32 dialog that pumps its own message loop
            // while the main window stays enabled, so no borrow of `App` may be alive
            // across it: a click that lands during the loop would be a
            // BorrowMutError panic. Take nothing, ask, then borrow and apply
            // (plan §2.3 R12).
            //
            // The window handle is held for the duration of the loop, which is what
            // the old code was doing with a live `App` borrow; holding the handle is
            // not holding the borrow.
            let _window = shared.borrow().ui.upgrade();
            let Some(dir) = crate::picker::pick_folder("选择下载保存目录") else {
                return;
            };
            let created = std::fs::create_dir_all(&dir).is_ok();
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            if !created {
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

    // ---- round nine (F5): pause, resume, and the header's one button ----
    // Every one of these forwards to the manager and then republishes, so the row and
    // the header are redrawn from the queue's own answer instead of from what the
    // click hoped would happen: `pause` returns false for a terminal row, and a
    // resumed task is `Queued` again only because the manager says so.
    {
        let shared = app.clone();
        ui.on_pause_task(move |id| {
            let app = shared.borrow();
            app.pause_download(id);
        });
    }

    {
        let shared = app.clone();
        ui.on_resume_task(move |id| {
            let app = shared.borrow();
            app.resume_download(id);
        });
    }

    {
        let shared = app.clone();
        ui.on_toggle_pause_all(move || {
            let app = shared.borrow();
            app.toggle_pause_all();
        });
    }
}
