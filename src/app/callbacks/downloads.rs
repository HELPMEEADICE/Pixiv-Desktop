use super::*;

pub(crate) fn install(app: &Shared) {
    let ui = app.borrow().ui();
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
