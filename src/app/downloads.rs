use super::*;
use pixiv_download::{DownloadTask, TaskState};

/// Take the one queue snapshot a tick is allowed to take.
///
/// Returns `None` when the queue is empty **and** the model already shows
/// nothing: that is the idle case, and it must not write to the UI. `shown` is
/// the app-side cache of what the model holds (`download_rows.row_count()`), which
/// is what lets the emptiness test share the snapshot instead of cloning the
/// queue a second time to find out.
pub(crate) fn snapshot_downloads(
    source: impl FnOnce() -> Vec<pixiv_download::DownloadTask>,
    shown: usize,
) -> Option<Vec<pixiv_download::DownloadTask>> {
    let tasks = source();
    if tasks.is_empty() && shown == 0 {
        return None;
    }
    Some(tasks)
}

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
        // Round nine (G24): a capture can ask for one deterministic queue instead of
        // whatever this machine's history left behind. The fixture replaces the
        // *snapshot*, never the publish path, so the rows a capture photographs are
        // the rows `to_download_rows` builds (tools/fixture.ps1 is the other half of
        // this seam and names the state).
        if fixture_is_downloads() {
            let (tasks, throughput) = fixture_downloads();
            self.publish_downloads(tasks, throughput);
            return;
        }
        // One snapshot per call. The 500ms timer used to take one just to ask
        // "is the queue empty" and this function took a second one, and
        // `DownloadManager::tasks` deep-clones every row
        // (`crates/pixiv-download/src/worker.rs:278`) - so a queue with anything
        // in it paid that twice per half second (plan §2.3 R8).
        let Some(tasks) =
            snapshot_downloads(|| self.downloads.tasks(), self.download_rows.row_count())
        else {
            // Idle: no task and no row. One thing still has to be cleared by hand,
            // because nothing else will: a rate describes the three seconds that just
            // passed, so a queue that emptied would otherwise keep the last number it
            // printed on screen for the rest of the session. Slint drops a write of an
            // equal value, so repeating this on an idle tick paints nothing.
            let ui = self.ui();
            ui.set_throughput("".into());
            ui.set_queue_paused(false);
            return;
        };
        self.publish_downloads(tasks, self.downloads.throughput());
    }

    /// Apply one queue snapshot to the downloads page: the rows, the three counts the
    /// page already drew, and the four properties round nine added (§4:
    /// `queue-paused`, `queue-done`, `queue-total`, `throughput`).
    fn publish_downloads(&self, tasks: Vec<DownloadTask>, throughput_bytes_per_second: u64) {
        let view = DownloadsView::build(tasks, throughput_bytes_per_second);
        let ui = self.ui();
        let active = view
            .rows
            .iter()
            .filter(|row| row.state == 0 || row.state == 1)
            .count() as i32;
        let done = view.rows.iter().filter(|row| row.state == 2).count() as i32;
        let failed = view.rows.iter().filter(|row| row.state == 3).count() as i32;
        // Patch in place. The old code built a brand-new `VecModel` every 500ms,
        // which replaced every row element: the row under the pointer lost its
        // hover state five times a second, and an idle queue still did that work.
        sync_download_rows(&self.download_rows, view.rows);
        ui.set_downloads_active(active);
        ui.set_downloads_done(done);
        ui.set_downloads_failed(failed);
        ui.set_queue_paused(view.paused);
        ui.set_queue_done(view.done);
        ui.set_queue_total(view.total);
        ui.set_throughput(view.throughput.into());
    }
}

impl App {
    /// Hold one row (F5). `pause` reuses the in-flight cancel inside the manager, so a
    /// running task gives its concurrency permit back at once instead of holding it
    /// until its body would have landed.
    pub(crate) fn pause_download(&self, id: i32) -> bool {
        let changed = self.downloads.pause(id.max(0) as u64);
        self.push_downloads();
        changed
    }

    /// Send one held row back to the queue. HTTP Range resume is out of scope this
    /// round (plan §5 F5), so the body starts again from zero - the row keeps the page
    /// it is on, which is why a held row keeps its own progress on screen.
    pub(crate) fn resume_download(&self, id: i32) -> bool {
        let changed = self.downloads.resume(id.max(0) as u64);
        self.push_downloads();
        changed
    }

    /// The header's single button, in both directions.
    ///
    /// The direction is read from the queue rather than from the property the button
    /// was last drawn with: a tick between the click and the read can land either side
    /// of a row finishing, and a button that trusted its own label would pause a queue
    /// the user had just resumed.
    pub(crate) fn toggle_pause_all(&self) {
        if queue_is_paused(&self.downloads.tasks()) {
            self.downloads.resume_all();
        } else {
            self.downloads.pause_all();
        }
        self.push_downloads();
    }
}

/// Everything the downloads page draws, from one queue snapshot.
///
/// Pure on purpose: the live queue, the fixture and the tests all take this one path,
/// so the numbers a capture asserts are the numbers the app computes rather than a
/// second set written next to them.
pub(crate) struct DownloadsView {
    pub(crate) rows: Vec<DownloadRow>,
    /// Every row that can be held is held, and there is at least one such row.
    pub(crate) paused: bool,
    /// Pages of the works in this queue that are on disk.
    pub(crate) done: i32,
    /// Pages those works are made of.
    pub(crate) total: i32,
    /// `human_rate` of the queue, empty while nothing has been measured.
    pub(crate) throughput: String,
}

impl DownloadsView {
    pub(crate) fn build(tasks: Vec<DownloadTask>, throughput_bytes_per_second: u64) -> Self {
        let paused = queue_is_paused(&tasks);
        let (done, total) = queue_totals(&tasks);
        let throughput = human_rate(throughput_bytes_per_second);
        Self {
            rows: to_download_rows(tasks),
            paused,
            done,
            total,
            throughput,
        }
    }
}

/// Is the *queue* held?
///
/// ui/pages/downloads.slint reads this one flag twice: the header's button labels
/// itself "全部继续" while it is true and "全部暂停" while it is false, and the
/// "已暂停" chip is drawn from it. "Any row is paused" would print that chip over a
/// queue that is still downloading, so the honest reading is **every row that can be
/// held is held** - plus the requirement that there is one, or an empty or
/// all-finished queue would claim to be paused.
pub(crate) fn queue_is_paused(tasks: &[DownloadTask]) -> bool {
    let mut held = 0;
    let mut runnable = 0;
    for task in tasks {
        match task.state {
            TaskState::Paused => held += 1,
            TaskState::Queued | TaskState::Running => runnable += 1,
            _ => {}
        }
    }
    held > 0 && runnable == 0
}

/// Pages finished, and pages the queue is responsible for.
///
/// Counted in **pages**, not in rows. `enqueue_download(all_pages)` turns one work of
/// twelve pages into twelve page tasks, and a row count would print "1 / 4" for a
/// queue that is a quarter of one artwork - a number whose meaning would change with
/// which of the two download buttons the user pressed. A work is identified by its
/// illust id, which `DownloadRequest` carries and `DownloadRow` does not, so this takes
/// the tasks rather than the rows it is published beside.
///
/// `done` is the highest finished page index of a work plus one: the manager claims a
/// work's pages oldest-first, so "pages 1..=n of this work are on disk" is what the
/// counter can honestly claim, and a work nobody has finished a page of prints 0.
pub(crate) fn queue_totals(tasks: &[DownloadTask]) -> (i32, i32) {
    let mut works: HashMap<&str, (u32, u32)> = HashMap::new();
    for task in tasks {
        let entry = works
            .entry(task.request.illust_id.as_str())
            .or_insert((1, 0));
        entry.0 = entry.0.max(task.request.page_count.max(1));
        if matches!(task.state, TaskState::Completed | TaskState::Skipped) {
            entry.1 = entry.1.max(task.request.page_index + 1);
        }
    }
    let total: u32 = works.values().map(|(pages, _)| *pages).sum();
    let done: u32 = works.values().map(|(_, finished)| *finished).sum();
    (
        i32::try_from(done).unwrap_or(i32::MAX),
        i32::try_from(total).unwrap_or(i32::MAX),
    )
}

/// A byte rate, as the page prints it ("3.2 MB/s"): empty while nothing is known.
///
/// `human_bytes` (app/helpers.rs:233) is reused for everything under a megabyte; from
/// one megabyte up the rate keeps a decimal, because a three-second sliding window
/// almost never lands on a whole number and "3 MB/s" for 3.9 MB/s is a 30 % lie in the
/// one number a user watches while downloading. Zero is the empty string rather than
/// "0 B/s": the page hides the line instead of claiming a stalled queue over a window
/// that simply has no samples yet (the same argument the page makes above
/// `throughput`).
pub(crate) fn human_rate(bytes_per_second: u64) -> String {
    if bytes_per_second == 0 {
        return String::new();
    }
    if bytes_per_second >= 1024 * 1024 {
        return format!("{:.1} MB/s", bytes_per_second as f64 / (1024.0 * 1024.0));
    }
    if bytes_per_second < 1024 {
        return format!("{bytes_per_second} B/s");
    }
    format!("{}/s", human_bytes(bytes_per_second))
}

/// The row's one caption line: "第 2 / 4 页 · 3.2 MB/s · 剩 12 秒".
///
/// **Deliberate reuse.** `DownloadRow` is declared in ui/pages/downloads.slint, which
/// this round froze, so the rate and the countdown ride in `subtitle` instead of
/// gaining two fields the row would then have to lay out: the row already draws
/// `author + " · " + subtitle` as one elided line, which is the shape a speed line
/// wants anyway. The page number is the floor - a queued, finished or held row has no
/// rate, and `DownloadTask::eta_seconds` is `None` exactly when a countdown could not
/// be honoured, so nothing here guesses one.
pub(crate) fn row_subtitle(
    page_index: u32,
    page_count: u32,
    bytes_per_second: u64,
    eta_seconds: Option<u32>,
) -> String {
    let mut line = format!("第 {} / {} 页", page_index + 1, page_count.max(1));
    let rate = human_rate(bytes_per_second);
    if rate.is_empty() {
        return line;
    }
    line.push_str(" · ");
    line.push_str(&rate);
    if let Some(seconds) = eta_seconds {
        line.push_str(&format!(" · 剩 {seconds} 秒"));
    }
    line
}

// ---------------------------------------------------------- fixtures (G24)

/// Whether the ambient fixture is this page's.
///
/// There is one parser for `PIXIV_DESKTOP_FIXTURE` and it lives in
/// `feed::fixture`: the variable names one state, so a second reader would be a
/// second answer waiting to disagree. This file only asks whether the state is
/// the queue one.
fn fixture_is_downloads() -> bool {
    matches!(
        super::feed::fixture::current(),
        super::feed::fixture::Fixture::Downloads
    )
}

/// 3.2 MiB/s - the rate the fixture prints - in bytes.
///
/// In bytes rather than as the string "3.2 MB/s", so the header's throughput and the
/// row's countdown are computed from one number: two literals would be free to
/// disagree the day either formatter changes.
pub(crate) const FIXTURE_RATE: u64 = 3_355_443;

/// The four rows `PIXIV_DESKTOP_FIXTURE=downloads` puts on screen, and the queue's
/// throughput.
///
/// One work of twelve pages, four of them in the queue (pages 3 to 6): page 3
/// finished, page 4 failed, page 5 held, page 6 in flight at 3.2 MB/s with twelve
/// seconds left. The header therefore prints "已完成 3 / 12" over four rows - the two
/// numbers are different units (`queue_totals`), and this is the state that proves the
/// page can say both without lying about either.
pub(crate) fn fixture_downloads() -> (Vec<DownloadTask>, u64) {
    const PAGE_BYTES: u64 = 4_000_000;
    // Exactly twelve seconds of work left at FIXTURE_RATE and 40 % done, so the row's
    // countdown states the state rather than a rounding of it.
    let in_flight_total = 20 * FIXTURE_RATE;
    let in_flight_done = 8 * FIXTURE_RATE;
    let tasks = vec![
        fixture_task(
            901,
            2,
            TaskState::Completed,
            PAGE_BYTES,
            Some(PAGE_BYTES),
            0,
        ),
        fixture_task(
            902,
            3,
            TaskState::Failed("连接在 3.1 MB 处被重置".to_string()),
            3_100_000,
            Some(PAGE_BYTES),
            0,
        ),
        fixture_task(903, 4, TaskState::Paused, 1_000_000, Some(PAGE_BYTES), 0),
        fixture_task(
            904,
            5,
            TaskState::Running,
            in_flight_done,
            Some(in_flight_total),
            FIXTURE_RATE,
        ),
    ];
    (tasks, FIXTURE_RATE)
}

fn fixture_task(
    id: u64,
    page_index: u32,
    state: TaskState,
    bytes_done: u64,
    bytes_total: Option<u64>,
    bytes_per_second: u64,
) -> DownloadTask {
    DownloadTask {
        id,
        request: DownloadRequest {
            illust_id: "700000001".into(),
            title: "[DEMO] 夜明けの街 · 12 页".into(),
            author: "演示作者".into(),
            page_index,
            page_count: 12,
            url: format!("demo://illust/700000001/{}/regular.png", 800 + page_index),
            extension: "png".into(),
        },
        state,
        bytes_done,
        bytes_total,
        bytes_per_second,
        destination: Some(PathBuf::from(format!(
            "pixiv-demo/700000001_p{}.png",
            page_index + 1
        ))),
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
                // Round nine (F5). `6` is the next number in the contract with
                // ui/pages/downloads.slint: a paused row keeps its progress and
                // offers "继续" instead of "取消".
                pixiv_download::TaskState::Paused => (6, String::new()),
            };
            DownloadRow {
                id: task.id as i32,
                title: task.request.title.clone().into(),
                author: task.request.author.clone().into(),
                // The rate and the countdown ride in `subtitle`: `DownloadRow` is
                // declared in ui/pages/downloads.slint and this round froze that file.
                subtitle: row_subtitle(
                    task.request.page_index,
                    task.request.page_count,
                    task.bytes_per_second,
                    task.eta_seconds(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str) -> DownloadRequest {
        DownloadRequest {
            illust_id: id.into(),
            title: "t".into(),
            author: "a".into(),
            page_index: 0,
            page_count: 1,
            url: format!("demo://illust/{id}/800x600/regular.png"),
            extension: "png".into(),
        }
    }

    /// A 500ms tick must read the queue once. It used to read it twice -
    /// `lifecycle.rs` took a snapshot only to test for emptiness, and
    /// `push_downloads` took another to build the rows, each one a deep clone of
    /// every task (plan §2.3 R8).
    #[test]
    fn a_tick_takes_one_queue_snapshot_not_two() {
        let manager = DownloadManager::new(DownloadConfig::default());
        manager.enqueue(vec![request("700000001")]);

        let mut snapshots = 0;
        let taken = snapshot_downloads(
            || {
                snapshots += 1;
                manager.tasks()
            },
            0,
        );
        assert_eq!(
            snapshots, 1,
            "one snapshot serves both the test and the push"
        );
        assert_eq!(taken.map(|tasks| tasks.len()), Some(1));

        // Idle queue, nothing on screen: one snapshot and no UI write at all.
        let idle = DownloadManager::new(DownloadConfig::default());
        let mut idle_snapshots = 0;
        let nothing = snapshot_downloads(
            || {
                idle_snapshots += 1;
                idle.tasks()
            },
            0,
        );
        assert!(nothing.is_none(), "an idle tick pushes nothing");
        assert_eq!(idle_snapshots, 1);

        // An empty queue with rows still on screen is not idle: the rows have to
        // be dropped, and that still costs exactly one snapshot.
        let mut stale_snapshots = 0;
        let stale = snapshot_downloads(
            || {
                stale_snapshots += 1;
                idle.tasks()
            },
            3,
        );
        assert_eq!(stale.map(|tasks| tasks.len()), Some(0));
        assert_eq!(stale_snapshots, 1);
    }

    /// One page of one work, which is what `enqueue_download(all_pages)` builds.
    fn page_task(
        id: u64,
        illust: &str,
        page_index: u32,
        page_count: u32,
        state: TaskState,
    ) -> DownloadTask {
        DownloadTask {
            id,
            request: DownloadRequest {
                illust_id: illust.into(),
                title: "t".into(),
                author: "a".into(),
                page_index,
                page_count,
                url: "demo://illust/1/800x600/regular.png".into(),
                extension: "png".into(),
            },
            state,
            bytes_done: 0,
            bytes_total: Some(4_000_000),
            bytes_per_second: 0,
            destination: None,
        }
    }

    /// F5: holding a row is state 6 on screen, and the whole-queue flag flips only
    /// when there is nothing left to run.
    #[test]
    fn a_held_row_is_state_six_and_holds_the_queue_only_when_nothing_is_queued() {
        let manager = DownloadManager::new(DownloadConfig::default());
        let ids = manager.enqueue(vec![request("700000001"), request("700000002")]);
        assert!(
            !queue_is_paused(&manager.tasks()),
            "two queued rows are not a held queue"
        );

        assert!(manager.pause(ids[0]), "a queued row can be held");
        let rows = to_download_rows(manager.tasks());
        let held = rows
            .iter()
            .find(|row| row.id == ids[0] as i32)
            .expect("a held row is still in the model");
        assert_eq!(
            held.state, 6,
            "6 is the number ui/pages/downloads.slint reads for TaskState::Paused"
        );
        assert_eq!(
            held.subtitle.as_str(),
            "第 1 / 1 页",
            "a held row has no rate and no countdown"
        );
        assert!(
            !queue_is_paused(&manager.tasks()),
            "one held row beside a queued one is not a held queue"
        );

        // Pause-all takes the rest; now the header is told the whole queue is held.
        assert_eq!(manager.pause_all(), 1, "only the queued row changes");
        assert!(queue_is_paused(&manager.tasks()));
        assert!(
            to_download_rows(manager.tasks())
                .iter()
                .all(|row| row.state == 6)
        );

        // Resume sends it back to the queue, which is state 0 on screen.
        assert!(manager.resume(ids[0]));
        let rows = to_download_rows(manager.tasks());
        assert_eq!(
            rows.iter()
                .find(|row| row.id == ids[0] as i32)
                .map(|row| row.state),
            Some(0)
        );
        assert!(!queue_is_paused(&manager.tasks()));
        assert_eq!(
            manager.resume_all(),
            1,
            "the row pause-all held is still held, and resume-all is the only thing that moves it"
        );
        assert_eq!(manager.resume_all(), 0, "and now nothing is held");
    }

    /// The three captions the row can print, and the floor it falls back to.
    #[test]
    fn the_row_caption_carries_the_rate_and_the_countdown_when_there_is_one() {
        assert_eq!(row_subtitle(1, 4, 0, None), "第 2 / 4 页");
        assert_eq!(
            row_subtitle(1, 4, FIXTURE_RATE, None),
            "第 2 / 4 页 · 3.2 MB/s"
        );
        assert_eq!(
            row_subtitle(1, 4, FIXTURE_RATE, Some(12)),
            "第 2 / 4 页 · 3.2 MB/s · 剩 12 秒"
        );
        // A zero page count is one page, not a division by zero and not "第 1 / 0 页".
        assert_eq!(row_subtitle(0, 0, 0, None), "第 1 / 1 页");
        // `Some(0)` is the library's "running, last byte in", not "unknown".
        assert_eq!(
            row_subtitle(0, 1, FIXTURE_RATE, Some(0)),
            "第 1 / 1 页 · 3.2 MB/s · 剩 0 秒"
        );
    }

    /// The throughput property: empty at zero, and never a unit-less number.
    #[test]
    fn a_zero_rate_is_empty_and_every_other_rate_carries_its_unit() {
        assert_eq!(human_rate(0), "", "no samples is not 0 B/s");
        assert_eq!(human_rate(FIXTURE_RATE), "3.2 MB/s");
        assert_eq!(human_rate(1024 * 1024), "1.0 MB/s");
        assert_eq!(human_rate(4096), "4 KB/s", "under a megabyte: human_bytes");
        assert_eq!(
            human_rate(512),
            "512 B/s",
            "and not \"0 KB/s\" while bytes flow"
        );
        assert!(human_rate(FIXTURE_RATE).ends_with("/s"));

        let idle = DownloadsView::build(Vec::new(), 0);
        assert_eq!(idle.throughput, "", "an idle queue publishes no rate");
        assert_eq!((idle.done, idle.total), (0, 0), "and no phantom progress");
        assert!(!idle.paused);
        assert!(idle.rows.is_empty());
    }

    /// The counter is pages of the work, not rows: four rows of a twelve-page work
    /// whose third page is on disk read "已完成 3 / 12".
    #[test]
    fn the_queue_counter_is_pages_of_the_work_not_rows() {
        let (fixture, _) = fixture_downloads();
        assert_eq!(fixture.len(), 4);
        assert_eq!(queue_totals(&fixture), (3, 12));

        let fresh: Vec<DownloadTask> = (0..12)
            .map(|page| page_task(page as u64, "9", page, 12, TaskState::Queued))
            .collect();
        assert_eq!(queue_totals(&fresh), (0, 12), "nothing finished yet");

        let mut finished = fresh.clone();
        for task in finished.iter_mut().take(4) {
            task.state = TaskState::Completed;
        }
        assert_eq!(queue_totals(&finished), (4, 12), "pages 1..=4 are on disk");

        let mut skipped = fresh.clone();
        skipped[0].state = TaskState::Skipped;
        assert_eq!(
            queue_totals(&skipped),
            (1, 12),
            "Skipped means the file was already there, which is a finished page"
        );

        let mut two_works = fresh.clone();
        two_works.extend(
            (0..5).map(|page| page_task(100 + page as u64, "8", page, 5, TaskState::Queued)),
        );
        assert_eq!(queue_totals(&two_works), (0, 17), "two works add up");

        assert_eq!(
            queue_totals(&[page_task(1, "7", 0, 0, TaskState::Queued)]),
            (0, 1),
            "a zero page count is one page"
        );
    }

    /// The fixture is the state tools/fixture.ps1 promises: four rows - one finished,
    /// one failed, one held, one in flight - and the header the capture asserts.
    #[test]
    fn the_downloads_fixture_is_the_state_the_capture_asserts() {
        let (tasks, rate) = fixture_downloads();
        let view = DownloadsView::build(tasks, rate);
        assert_eq!(view.rows.len(), 4);
        assert_eq!((view.done, view.total), (3, 12));
        assert_eq!(view.throughput, "3.2 MB/s");
        assert!(
            !view.paused,
            "one held row beside a running one does not hold the queue"
        );
        assert_eq!(
            view.rows.iter().map(|row| row.state).collect::<Vec<_>>(),
            vec![2, 3, 6, 1]
        );
        assert_eq!(view.rows[1].message.as_str(), "连接在 3.1 MB 处被重置");
        assert_eq!(
            view.rows[2].subtitle.as_str(),
            "第 5 / 12 页",
            "a held row keeps its page and prints no rate"
        );
        assert_eq!(
            view.rows[2].progress, 25,
            "a held row keeps the progress it froze at (U13)"
        );
        assert_eq!(
            view.rows[3].subtitle.as_str(),
            "第 6 / 12 页 · 3.2 MB/s · 剩 12 秒"
        );
        assert_eq!(view.rows[3].progress, 40);
        // Two publishes of the same state are the same pixels.
        let (again, again_rate) = fixture_downloads();
        let second = DownloadsView::build(again, again_rate);
        assert_eq!(second.throughput, view.throughput);
        assert_eq!(
            second
                .rows
                .iter()
                .map(|row| row.subtitle.clone())
                .collect::<Vec<_>>(),
            view.rows
                .iter()
                .map(|row| row.subtitle.clone())
                .collect::<Vec<_>>()
        );
    }

    /// G24: this page answers exactly one fixture value, and that answer comes
    /// from the one parser (feed::fixture) rather than from a second copy of the
    /// table - two readers of one variable are two answers waiting to disagree.
    #[test]
    fn the_fixture_variable_names_one_state_and_leaves_the_queue_alone() {
        use super::super::feed::fixture::{Fixture, fixture_from};
        assert_eq!(fixture_from(None), Fixture::None);
        assert_eq!(fixture_from(Some("")), Fixture::None);
        assert_eq!(fixture_from(Some("downloads")), Fixture::Downloads);
        assert_eq!(fixture_from(Some("  Downloads ")), Fixture::Downloads);
        assert_eq!(
            fixture_from(Some("ugoira")),
            Fixture::Ugoira,
            "another track's fixture parses, and this page still does not claim it"
        );
        assert_eq!(fixture_from(Some("no-such-fixture")), Fixture::Unknown);
    }
}
