use super::*;

impl App {
    // ------------------------------------------------------------- workers

    pub(crate) fn start_download_worker(&self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let manager = self.downloads.clone();
        self.rt.spawn(async move {
            manager.run(client).await;
        });
    }
}

impl App {
    pub(crate) fn push_downloads(&self) {
        let rows = to_download_rows(self.downloads.tasks());
        let ui = self.ui();
        let active = rows
            .iter()
            .filter(|row| row.state == 0 || row.state == 1)
            .count() as i32;
        let done = rows.iter().filter(|row| row.state == 2).count() as i32;
        let failed = rows.iter().filter(|row| row.state == 3).count() as i32;
        // Patch in place. The old code built a brand-new `VecModel` every 500ms,
        // which replaced every row element: the row under the pointer lost its
        // hover state five times a second, and an idle queue still did that work.
        sync_download_rows(&self.download_rows, rows);
        ui.set_downloads_active(active);
        ui.set_downloads_done(done);
        ui.set_downloads_failed(failed);
    }
}

impl App {
    pub(crate) fn enqueue_download(&mut self, all_pages: bool) {
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
}

impl App {
    /// Fetch the ugoira archive, rebuild it and write a GIF next to the other
    /// downloads. The decoding/encoding already exists in `pixiv-ugoira`; this is
    /// the UI integration that was missing.
    pub(crate) fn export_ugoira(&mut self) {
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
}

/// Patch `model` so it holds `next`, touching only the rows that differ.
///
/// Returns how many rows were written, which is what the tests assert on and what
/// used to be "all of them, five times a second". Fields are compared one by one
/// rather than through `PartialEq` on a generated Slint struct, so adding a field
/// to `DownloadRow` cannot silently make every row compare equal.
pub(crate) fn sync_download_rows(model: &VecModel<DownloadRow>, next: Vec<DownloadRow>) -> usize {
    let mut touched = 0;
    while model.row_count() > next.len() {
        model.remove(model.row_count() - 1);
        touched += 1;
    }
    for (index, row) in next.into_iter().enumerate() {
        match model.row_data(index) {
            Some(current) if same_download_row(&current, &row) => {}
            Some(_) => {
                model.set_row_data(index, row);
                touched += 1;
            }
            None => {
                model.push(row);
                touched += 1;
            }
        }
    }
    touched
}

pub(crate) fn same_download_row(a: &DownloadRow, b: &DownloadRow) -> bool {
    a.id == b.id
        && a.state == b.state
        && a.progress == b.progress
        && a.title == b.title
        && a.author == b.author
        && a.subtitle == b.subtitle
        && a.message == b.message
        && a.path == b.path
}

pub(crate) fn to_download_rows(tasks: Vec<pixiv_download::DownloadTask>) -> Vec<DownloadRow> {
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
