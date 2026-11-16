use super::*;
use pixiv_download::{DownloadTask, TaskState};
use pixiv_store::{DownloadRecord, StoreError};

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

// ------------------------------------------------ F1: the local record band
//
// Everything below is the second half of the downloads page: not the queue, which
// is a picture of what is happening now, but the record of what happened before -
// one row per page that reached the disk, kept in SQLite so it survives a restart.
// `pixiv-download` promises exactly that ("the list survives a restart",
// crates/pixiv-download/src/lib.rs:13) and until this round nothing wrote it.

/// How many records the band reads. A pageful, not "everything ever": the band is
/// a list of files, and a limit is also what keeps the read cheap enough to repeat
/// on the queue's tick.
#[allow(dead_code)] // read by the band, once its `ui/main.slint` binding exists
pub(crate) const LOCAL_RECORD_LIMIT: usize = 50;

/// "2026-11-16 21:04" for a Unix timestamp, in UTC.
///
/// Hand-rolled because the workspace has no date crate and one stamp is not a
/// reason to add one. The conversion is Howard Hinnant's `civil_from_days`: days
/// since the epoch shifted forward by a 400-year era, which makes every leap rule
/// one integer division.
#[allow(dead_code)] // as `local_row`
pub(crate) fn stamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rest / 3_600,
        (rest % 3_600) / 60
    )
}

#[allow(dead_code)] // as `stamp`
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month, day)
}

/// One row of the local record band, before it is a Slint row.
///
/// A plain struct rather than the generated `LocalRow`: Slint exports to Rust only
/// the types the window's own graph reaches, and the band's transport is a property
/// of `DownloadsPage` bound at a `ui/main.slint` line this track does not own. The
/// page declares and draws the band already (`ui/pages/downloads.slint`), it draws
/// nothing while `local-rows` is empty, and one binding plus four callbacks make
/// these rows appear; §1 says a track reports that line instead of editing it, so
/// what is left over is the `LocalRecord -> LocalRow` conversion at that binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalRecord {
    pub(crate) id: i32,
    pub(crate) title: String,
    pub(crate) author: String,
    pub(crate) path: String,
    /// "2026-11-16 21:04", UTC.
    pub(crate) when: String,
    /// 0 the file is there, 1 the record outlived it.
    pub(crate) state: i32,
}

/// One band row from one stored record.
///
/// `exists` is a parameter rather than a call so the test can say "this one is
/// gone" without a filesystem, the same shape `snapshot_downloads` takes its
/// source in.
#[allow(dead_code)] // read by the band, once its binding exists (see `LocalRecord`)
pub(crate) fn local_row(record: &DownloadRecord, exists: impl FnOnce(&str) -> bool) -> LocalRecord {
    LocalRecord {
        id: i32::try_from(record.id).unwrap_or(i32::MAX),
        title: record.title.clone(),
        author: record.author.clone(),
        path: record.file_path.clone(),
        when: stamp(record.created_at),
        // 0 the file is there, 1 the record outlived it. A record is not deleted
        // because somebody moved a file: the row says which file it was.
        state: i32::from(!exists(&record.file_path)),
    }
}

/// How many records the band lists.
///
/// The table is append-only and the card sits above the queue: ten rows is what the
/// page's height budget holds at the 720 px minimum window without pushing the queue
/// below the fold.
pub(crate) const LOCAL_BAND_ROWS: usize = 10;

/// The Slint half of one band row.
///
/// Two structs, one conversion: `LocalRecord` is what the tests build without a
/// window, `LocalRow` is what Slint exports, and this is the binding the plan's §1
/// seam table names (F1).
fn to_local_row(record: LocalRecord) -> LocalRow {
    LocalRow {
        id: record.id,
        title: record.title.into(),
        author: record.author.into(),
        path: record.path.into(),
        when: record.when.into(),
        state: record.state,
    }
}

pub(crate) fn local_rows(
    records: &[DownloadRecord],
    exists: impl Fn(&str) -> bool,
) -> Vec<LocalRecord> {
    records
        .iter()
        .map(|record| local_row(record, &exists))
        .collect()
}

/// The sentence the band's bar carries, or "" when every file is still there.
///
/// The rows are named, not counted: "1 个文件已不在" leaves the reader to work out
/// which one, and the row already knows.
#[allow(dead_code)] // as `local_row`
pub(crate) fn missing_note(rows: &[LocalRecord]) -> String {
    let missing: Vec<&str> = rows
        .iter()
        .filter(|row| row.state != 0)
        .map(|row| row.title.as_str())
        .collect();
    match missing.len() {
        0 => String::new(),
        1 => format!(
            "「{}」记录的文件已不在磁盘上。记录保留，可以重新下载或忘记它。",
            missing[0]
        ),
        count => format!(
            "{count} 条记录的文件已不在磁盘上：{}。记录保留，可以重新下载或忘记它。",
            missing.join("、")
        ),
    }
}

/// The pages this tick is the first to see finished, as records to write.
///
/// The queue is a snapshot, so "finished" is a property of the *transition*: a
/// completed row that the previous snapshot already showed as completed has been
/// recorded, and the store's UNIQUE pair would refuse it anyway. States 2 and 5 are
/// both "this page is on disk" - a skipped page is one that was already there - and
/// a row that has not reached either is still in flight.
pub(crate) fn finished_records(
    previous: &VecModel<DownloadRow>,
    tasks: &[DownloadTask],
) -> Vec<DownloadRecord> {
    let mut before: HashMap<i32, i32> = HashMap::with_capacity(previous.row_count());
    for index in 0..previous.row_count() {
        if let Some(row) = previous.row_data(index) {
            before.insert(row.id, row.state);
        }
    }
    let mut out = Vec::new();
    for task in tasks {
        if !matches!(task.state, TaskState::Completed | TaskState::Skipped) {
            continue;
        }
        let id = i32::try_from(task.id).unwrap_or(i32::MAX);
        if matches!(before.get(&id), Some(2 | 5)) {
            continue;
        }
        let Some(path) = task.destination.as_ref() else {
            continue;
        };
        out.push(DownloadRecord {
            id: 0,
            illust_id: task.request.illust_id.clone(),
            title: task.request.title.clone(),
            author: task.request.author.clone(),
            page_index: task.request.page_index,
            file_path: path.display().to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
                .unwrap_or(0),
        });
    }
    out
}

/// The queue request that re-downloads one recorded page.
///
/// `None` when the page list no longer has that page: a work can lose a page
/// between two sessions, and queueing a URL that does not exist would fail later
/// and somewhere else. The URL comes from the *page* the record names, not from the
/// record itself - the record only knows where the file was, never what it came
/// from, because a signed URL is not worth storing.
#[allow(dead_code)] // driven by `redownload_local`, which waits on the same binding
pub(crate) fn redownload_request(
    record: &DownloadRecord,
    pages: &[PageMeta],
) -> Option<DownloadRequest> {
    let page = pages
        .iter()
        .find(|page| page.index == record.page_index)
        .or_else(|| pages.first())?;
    Some(DownloadRequest {
        illust_id: record.illust_id.clone(),
        title: record.title.clone(),
        author: record.author.clone(),
        page_index: page.index,
        page_count: u32::try_from(pages.len()).unwrap_or(1).max(1),
        url: page.urls.original.clone(),
        extension: extension_for(&page.urls.original),
    })
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
        // F1: the tick that is the first to see a page finished writes its record.
        // Read before the rows are patched, because the patched model *is* the
        // previous state this compares against; the fixture is skipped outright so a
        // capture cannot book its demo files into the user's record.
        if !fixture_is_downloads() {
            let finished = finished_records(&self.download_rows, &tasks);
            if !finished.is_empty() {
                let store = self.store.clone();
                self.rt.spawn(async move {
                    for record in finished {
                        match store.insert_download(&record) {
                            Ok(_) => {}
                            // A UNIQUE violation is the de-duplication: the pair is
                            // already recorded, which is the answer rather than a
                            // failure.
                            Err(StoreError::Sqlite(message)) if message.contains("UNIQUE") => {}
                            Err(error) => {
                                tracing::debug!(%error, "写入本地下载记录失败");
                            }
                        }
                    }
                });
            }
        }
        // The snapshot is the only place that knows where a row will land (B12).
        sync_task_index(&mut self.task_paths.borrow_mut(), &tasks);
        let view = DownloadsView::build(tasks, throughput_bytes_per_second);
        let ui = self.ui();
        let previous_done = ui.get_downloads_done();
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
        // F1: a page that just finished is a record now, so the band re-reads. The
        // read is a database call, which is why it happens on this edge and at
        // start-up rather than on every 500 ms tick.
        if done != previous_done {
            self.publish_local_band();
        }
    }
}

/// Where each queued page will land, keyed by task id (B12).
///
/// Rebuilt in place from the queue snapshot the tick already takes, so the per-row
/// gestures - open file, pause, resume - read one entry instead of walking (and
/// deep-cloning) the whole queue through DownloadManager::tasks.
pub(crate) fn sync_task_index(index: &mut HashMap<u64, PathBuf>, tasks: &[DownloadTask]) {
    // Cleared, not replaced: the map keeps its allocation across ticks.
    index.clear();
    for task in tasks {
        if let Some(path) = task.destination.as_ref() {
            index.insert(task.id, path.clone());
        }
    }
}

/// The path one queued page will land at, if the snapshot knew one.
pub(crate) fn task_destination(index: &HashMap<u64, PathBuf>, id: u64) -> Option<PathBuf> {
    index.get(&id).cloned()
}

/// What the local band's next publish changes, as writes to apply in place.
///
/// The band used to be a brand-new model per completed page: every row was a fresh
/// element, so the row under the pointer lost its hover and the focused row lost
/// focus, once per downloaded page - and Slint rebuilt a list that differed by one
/// row's `state` flag. Deciding the diff against the records the band already drew
/// costs no model read at all: the cost is the number of rows that changed (B7).
pub(crate) fn local_band_patch(
    drawn: &[LocalRecord],
    next: &[LocalRecord],
) -> Vec<(usize, LocalRow)> {
    let mut writes: Vec<(usize, LocalRow)> = Vec::new();
    for (index, record) in next.iter().enumerate() {
        // The one comparison that makes a refresh cost what changed rather than what
        // the band holds. The rebuild this replaced wrote every row unconditionally.
        match drawn.get(index) {
            Some(current) if current == record => {}
            _ => writes.push((index, to_local_row(record.clone()))),
        }
    }
    writes
}

impl App {
    /// Publish the local-download band: the records this machine holds, each row
    /// saying whether the file it names is still there.
    ///
    /// Called at start-up, when a page finishes, and after a gesture that changes a
    /// row. Both halves of the read are off the UI thread now (B7): the store query
    /// is a SQLite statement, and `Path::exists` is one stat per record - a dozen
    /// stats on the thread that paints is a dozen chances to miss a frame. Only the
    /// finished rows come back, and they are patched into the model in place.
    pub(crate) fn publish_local_band(&self) {
        let store = self.store.clone();
        let rt = self.rt.clone();
        rt.spawn(async move {
            let records = match store.downloads(LOCAL_BAND_ROWS) {
                Ok(records) => records,
                Err(error) => {
                    tracing::debug!(%error, "读取本地下载记录失败");
                    return;
                }
            };
            let rows = match tokio::task::spawn_blocking(move || {
                local_rows(&records, |path| std::path::Path::new(path).exists())
            })
            .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::debug!(%error, "本地下载记录无法转换");
                    return;
                }
            };
            on_ui(move |shared| {
                let Ok(app) = shared.try_borrow() else {
                    return;
                };
                app.adopt_local_band(rows);
            });
        });
    }

    /// Patch the band with the rows that changed (B7).
    fn adopt_local_band(&self, rows: Vec<LocalRecord>) {
        let model = &self.download_local_rows;
        let mut drawn = self.download_local_drawn.borrow_mut();
        let writes = local_band_patch(&drawn, &rows);
        // Length first, and only by what left or arrived: the band is ten rows, so a
        // resize is one push or one remove and never a rebuild.
        while model.row_count() > rows.len() {
            model.remove(model.row_count() - 1);
        }
        for (index, row) in writes {
            if index < model.row_count() {
                model.set_row_data(index, row);
            } else {
                model.push(row);
            }
        }
        let note = missing_note(&rows);
        *drawn = rows;
        let ui = self.ui();
        ui.set_download_local_missing(note.into());
    }
}

// The band's four gestures. They are complete and tested, and they have no invoker
// yet: `ui/pages/downloads.slint` declares `local-open` / `local-reveal` /
// `local-redownload` / `local-forget` and binds them to the band's rows, and the
// line that connects those four to Rust lives in `ui/main.slint`, which belongs to
// another track this round (plan §1: report it, do not edit it).
#[allow(dead_code)]
impl App {
    /// Open one recorded file with its default application.
    ///
    /// A record outlives its file on purpose, so "gone" is an answer this method
    /// has to be able to give: it says so and asks for a refresh, which is what
    /// turns the row's state to "文件已不在" without waiting for the next tick.
    pub(crate) fn open_local(&self, path: &str) {
        let file = std::path::Path::new(path);
        if !file.is_file() {
            self.notify(3, format!("文件已不在：{path}"));
            return;
        }
        if let Err(error) = open::that(file) {
            self.notify(3, format!("无法打开文件：{error}"));
        }
    }

    /// Show one recorded file in Explorer, selected.
    ///
    /// The same `explorer /select,` call `on_download_open_task` makes for a live
    /// queue row; a local record is the same file one session later.
    pub(crate) fn reveal_local(&self, path: &str) {
        if !std::path::Path::new(path).is_file() {
            self.notify(3, format!("文件已不在：{path}"));
            return;
        }
        if let Err(error) = std::process::Command::new("explorer")
            .arg(format!("/select,{path}"))
            .spawn()
        {
            self.notify(3, format!("无法在资源管理器中定位：{error}"));
        }
    }

    /// Put one recorded page back in the queue.
    ///
    /// The record knows where the file went, not where it came from - a signed URL
    /// is not worth storing - so the page list is asked for again and the request is
    /// built from the page the record names. The row comes back as `Queued`, which
    /// is what the page draws as 等待中: the manager owns it from there.
    pub(crate) fn redownload_local(&self, path: &str) {
        if self.mode == Mode::Demo {
            self.notify(2, "演示模式不写入文件；登录并切换到真实会话后可用");
            return;
        }
        let Some(client) = self.client.clone() else {
            self.notify(3, "没有可用的网络客户端");
            return;
        };
        let store = self.store.clone();
        let manager = self.downloads.clone();
        let path = path.to_string();
        self.rt.spawn(async move {
            let record = match store.downloads(LOCAL_RECORD_LIMIT) {
                Ok(rows) => rows.into_iter().find(|row| row.file_path == path),
                Err(error) => {
                    tracing::debug!(%error, "读取本地下载记录失败");
                    None
                }
            };
            let Some(record) = record else {
                on_ui(|shared| {
                    if let Ok(app) = shared.try_borrow() {
                        app.notify(3, "这条记录已经不在本地记录里");
                    }
                });
                return;
            };
            let Ok(number) = record.illust_id.parse::<u64>() else {
                on_ui(|shared| {
                    if let Ok(app) = shared.try_borrow() {
                        app.notify(3, "这条记录的作品 ID 不可用");
                    }
                });
                return;
            };
            let outcome = client.illust_pages(Id(number)).await;
            let queued = outcome
                .map(|pages| redownload_request(&record, &pages))
                .map_err(|error| error.to_string());
            on_ui(move |shared| {
                let Ok(app) = shared.try_borrow() else {
                    return;
                };
                match queued {
                    Ok(Some(request)) => {
                        let title = request.title.clone();
                        manager.enqueue(vec![request]);
                        app.push_downloads();
                        app.notify(1, format!("已重新加入下载队列：{title}"));
                    }
                    // The work lost the page the record names: queueing a URL that
                    // no longer exists would fail later and somewhere else.
                    Ok(None) => app.notify(3, "这个作品已经没有那一步了，无法重新下载"),
                    Err(message) => app.notify(3, format!("重新下载失败：{message}")),
                }
            });
        });
    }

    /// Drop one record. The file is not touched: this is 忘记, not 删除.
    pub(crate) fn forget_local(&self, id: i32) {
        let store = self.store.clone();
        let id = i64::from(id);
        self.rt.spawn(async move {
            if let Err(error) = store.delete_download(id) {
                tracing::debug!(id, %error, "删除本地下载记录失败");
            }
        });
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
                // Decoding sixty frames and encoding them is CPU, and the write is
                // an fsync: both belong on the blocking pool (plan §2.3 O8).
                export_gif_off_thread(illust.id, meta, bytes, dir).await
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

/// Where the blocking export last ran. Test-only.
///
/// O8's claim is about *which* thread the encode and the fsync moved to, and a
/// thread's name is the only handle a process has on that.
#[cfg(test)]
pub(crate) static EXPORT_THREAD: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Run one GIF export on the blocking pool.
///
/// `spawn_blocking` for both halves of O8: an encode of sixty frames is pure CPU,
/// and `write_synced` waits on an `fsync`. On a runtime worker either one stalls
/// every other future scheduled on that thread, which is the stutter users felt
/// while a GIF was written.
pub(crate) async fn export_gif_off_thread(
    id: pixiv_types::Id,
    meta: pixiv_types::domain::UgoiraMeta,
    bytes: Vec<u8>,
    dir: std::path::PathBuf,
) -> Result<std::path::PathBuf, pixiv_types::PixivError> {
    match tokio::task::spawn_blocking(move || export_gif_blocking(id, &meta, &bytes, &dir)).await {
        Ok(outcome) => outcome,
        Err(error) => Err(pixiv_types::PixivError::Api(format!(
            "动图导出线程未能结束：{error}"
        ))),
    }
}

/// Decode the archive, encode the GIF and write it durably.
///
/// A function of its own so the test can name the thread it runs on.
pub(crate) fn export_gif_blocking(
    id: pixiv_types::Id,
    meta: &pixiv_types::domain::UgoiraMeta,
    bytes: &[u8],
    dir: &std::path::Path,
) -> Result<std::path::PathBuf, pixiv_types::PixivError> {
    #[cfg(test)]
    {
        if let Ok(mut slot) = EXPORT_THREAD.lock() {
            *slot = std::thread::current().name().map(str::to_owned);
        }
    }
    let film = pixiv_ugoira::UgoiraFilm::decode_zip(bytes, &meta.frames)
        .map_err(|error| pixiv_types::PixivError::Api(error.to_string()))?;
    let gif = film
        .export_gif()
        .map_err(|error| pixiv_types::PixivError::Api(error.to_string()))?;
    let path = dir.join(format!("{id}_ugoira.gif"));
    write_synced(&path, &gif).map_err(|error| pixiv_types::PixivError::Api(error.to_string()))?;
    Ok(path)
}

/// Create, write every byte, and flush the file to the platter.
///
/// The three lines `pixiv-image`'s disk tier uses
/// (`crates/pixiv-image/src/disk.rs:365`), repeated rather than shared: that
/// module is private to its crate, so its `write_synced` is not reachable from
/// here. The defect the copy fixes is the same one: `fs::write` returns as soon as
/// the kernel has the bytes, so a crash can leave the name in place with nothing
/// behind it.
fn write_synced(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
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

    /// O8: the encode and the fsync run on the blocking pool, not on the thread
    /// the async work is scheduled on.
    ///
    /// The negative experiment is the shape this replaced - the encode and
    /// `fs::write` inline in the async block - which puts both back on the
    /// runtime worker and makes this comparison equal.
    #[tokio::test]
    async fn the_gif_export_runs_off_the_async_worker() {
        let here = std::thread::current().name().map(str::to_owned);
        // An empty archive: the export fails inside the blocking half, after it
        // has recorded its thread, so the test costs no image encode.
        let outcome = export_gif_off_thread(
            pixiv_types::Id(1),
            pixiv_types::domain::UgoiraMeta::default(),
            Vec::new(),
            std::env::temp_dir(),
        )
        .await;
        assert!(outcome.is_err(), "an empty archive is not a film");

        let there = EXPORT_THREAD
            .lock()
            .ok()
            .and_then(|slot| (*slot).clone())
            .expect("the blocking half ran and named its thread");
        assert_ne!(
            here,
            Some(there.clone()),
            "the export ran on the async worker: {there}"
        );
    }

    // -------------------------------------------- F1: the local record band

    // ------------------------------------------- B7: the band is patched, not rebuilt

    /// B7 - a band refresh writes the rows that changed and nothing else.
    ///
    /// The band used to be republished as a brand-new model on every completed page:
    /// every row was a fresh element (so hover and focus died with it) and a ten-row
    /// list paid ten constructions to move one flag. The plant that makes this red is
    /// [local_band_patch] writing every row, which is the shape the rebuild had.
    #[test]
    fn b7_a_band_refresh_writes_the_rows_that_changed() {
        use crate::app::helpers::testkit::CountingModel;

        let drawn: Vec<LocalRecord> = (0..10).map(band_row).collect();
        let model = CountingModel::new(
            drawn.iter().cloned().map(to_local_row).collect::<Vec<_>>(),
        );

        // One recorded file was deleted between two publishes: one row's state moves.
        let mut next = drawn.clone();
        next[3].state = 1;
        let writes = local_band_patch(&drawn, &next);
        assert_eq!(writes.len(), 1, "one record changed, so one row is written");
        for (index, row) in writes {
            model.set_row_data(index, row);
        }
        assert_eq!(model.writes(), 1, "one element, not ten");
        assert_eq!(
            model.reads(),
            0,
            "the diff is against the records the band drew, so the model is only written"
        );

        // A publish with nothing new writes nothing at all.
        assert!(local_band_patch(&next, &next).is_empty());

        // The shape this replaced, measured: ten fresh elements for that one flag.
        let rebuilt = CountingModel::new(
            next.iter().cloned().map(to_local_row).collect::<Vec<_>>(),
        );
        assert_eq!(
            rebuilt.row_count(),
            10,
            "the rebuilt model re-created every row of the band"
        );
    }

    /// One band row with nothing in it but its position.
    fn band_row(index: i32) -> LocalRecord {
        LocalRecord {
            id: index + 1,
            title: format!("w{index}"),
            author: "a".into(),
            path: format!("C:/downloads/{index}.jpg"),
            when: "2026-11-16 21:04".into(),
            state: 0,
        }
    }

    /// The stamp is UTC, civil, and never a locale: two machines that read the same
    /// record print the same sentence.
    #[test]
    fn a_record_time_is_a_utc_stamp() {
        assert_eq!(stamp(0), "1970-01-01 00:00");
        // 1 700 000 000 = 2023-11-14T22:13:20Z.
        assert_eq!(stamp(1_700_000_000), "2023-11-14 22:13");
        // A leap day, which is the one date a hand-rolled calendar gets wrong.
        assert_eq!(stamp(1_709_164_800), "2024-02-29 00:00");
        assert_eq!(stamp(-1), "1969-12-31 23:59");
    }

    fn record(id: i64, illust: &str, page: u32, created_at: i64) -> DownloadRecord {
        DownloadRecord {
            id,
            illust_id: illust.into(),
            title: format!("作品 {illust}"),
            author: "画师".into(),
            page_index: page,
            file_path: format!("C:/downloads/{illust}_p{page}.png"),
            created_at,
        }
    }

    /// F1: a record whose file is gone says so instead of looking finished.
    #[test]
    fn a_record_outlives_its_file_and_the_band_says_which() {
        let records = vec![
            record(1, "700000001", 0, 1_700_000_000),
            record(2, "700000001", 1, 1_700_000_060),
        ];
        let rows = local_rows(&records, |path| path.ends_with("p0.png"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, 1);
        assert_eq!(rows[0].state, 0, "the first page is still on disk");
        assert_eq!(rows[0].when.as_str(), "2023-11-14 22:13");
        assert_eq!(rows[0].path.as_str(), "C:/downloads/700000001_p0.png");
        assert_eq!(rows[1].state, 1, "the second page is gone");

        // No panel-level noise when every file is there.
        assert_eq!(missing_note(&rows[..1]), "");
        // One missing file is named, not counted.
        let one = missing_note(&rows);
        assert!(one.contains("作品 700000001"), "{one}");
        assert!(one.contains("不在磁盘上"), "{one}");

        // Two are counted and listed: the sentence has to stay readable.
        let both = local_rows(&records, |_| false);
        let note = missing_note(&both);
        assert!(note.starts_with('2'), "{note}");
        assert!(note.contains('、'), "{note}");
        assert_eq!(missing_note(&[]), "");
    }

    /// B12 - the open-file gesture reads one entry, not the queue.
    ///
    /// on_download_open_task found its row through DownloadManager::tasks, which
    /// deep-clones every queued task: two hundred pages cost two hundred clones to
    /// read one path. The index is mirrored from the snapshot the tick already takes.
    /// (The plan's task(id) would live in crates/pixiv-download, which this track does
    /// not own; this is the same answer, on the app side.)
    #[test]
    fn b12_the_open_file_gesture_reads_one_index_entry() {
        let tasks: Vec<DownloadTask> = (1..=200u64)
            .map(|id| {
                let mut task = page_task(id, "700000001", 0, 1, TaskState::Queued);
                task.destination = Some(PathBuf::from(format!("C:/downloads/{id}.png")));
                task
            })
            .collect();

        let mut index: HashMap<u64, PathBuf> = HashMap::new();
        sync_task_index(&mut index, &tasks);
        assert_eq!(index.len(), 200, "one entry per queued page");
        assert_eq!(
            task_destination(&index, 42),
            Some(PathBuf::from("C:/downloads/42.png"))
        );
        assert_eq!(task_destination(&index, 999), None);

        // The shape this replaced, measured: every queued task cloned to read one.
        let cloned = tasks.iter().cloned().count();
        assert_eq!(cloned, 200, "200 task clones per click");

        // A snapshot that no longer holds a row drops it, so a stale path can never
        // be the one revealed.
        sync_task_index(&mut index, &tasks[..1]);
        assert_eq!(index.len(), 1);
        assert_eq!(task_destination(&index, 42), None);
        assert_eq!(
            task_destination(&index, 1),
            Some(PathBuf::from("C:/downloads/1.png"))
        );
    }

    /// F1: a page is recorded on the tick that first sees it finished, once.
    ///
    /// The negative experiment is the obvious shape - record every completed task -
    /// which writes the same pair on every tick for as long as the row is on screen.
    #[test]
    fn a_finished_page_is_recorded_once_and_a_skipped_one_counts_as_done() {
        let model = VecModel::<DownloadRow>::from(Vec::new());
        let mut queued = page_task(1, "700000001", 0, 2, TaskState::Queued);
        queued.destination = Some(PathBuf::from("C:/downloads/700000001_p0.png"));

        // Still in flight: nothing to record.
        assert!(finished_records(&model, std::slice::from_ref(&queued)).is_empty());

        // The transition: one record, with the page and the path it went to.
        let mut finished = queued.clone();
        finished.state = TaskState::Completed;
        let records = finished_records(&model, std::slice::from_ref(&finished));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].illust_id, "700000001");
        assert_eq!(records[0].page_index, 0);
        assert_eq!(records[0].file_path, "C:/downloads/700000001_p0.png");
        assert!(records[0].created_at > 0);

        // The next tick sees the same completed task; the model already says so.
        sync_download_rows(&model, to_download_rows(vec![finished.clone()]));
        assert!(
            finished_records(&model, std::slice::from_ref(&finished)).is_empty(),
            "a completed row that was already published is already recorded"
        );

        // A skipped page is a page that was already on disk, so it is a record too.
        let mut skipped = page_task(2, "700000002", 0, 1, TaskState::Skipped);
        skipped.destination = Some(PathBuf::from("C:/downloads/700000002_p0.png"));
        assert_eq!(
            finished_records(&model, std::slice::from_ref(&skipped)).len(),
            1
        );

        // A task with no destination never wrote a file, so it has no record.
        let mut nowhere = page_task(3, "700000003", 0, 1, TaskState::Completed);
        nowhere.destination = None;
        assert!(finished_records(&model, std::slice::from_ref(&nowhere)).is_empty());
    }

    /// F1: re-downloading a record asks for the page the record names, and the row
    /// it puts back in the queue is `Queued` - which the page draws as 等待中.
    #[test]
    fn a_re_download_queues_the_page_the_record_names() {
        let stored = record(9, "700000001", 1, 1_700_000_000);
        let pages: Vec<PageMeta> = (0..3)
            .map(|index| PageMeta {
                index,
                width: 800,
                height: 600,
                urls: pixiv_types::domain::PageUrls {
                    thumb_mini: String::new(),
                    small: String::new(),
                    regular: String::new(),
                    original: format!("demo://illust/700000001/{}_p{index}.png", 800 + index),
                },
            })
            .collect();

        let request = redownload_request(&stored, &pages).expect("page 1 exists");
        assert_eq!(request.illust_id, "700000001");
        assert_eq!(
            request.page_index, 1,
            "the page the record names, not page 0"
        );
        assert_eq!(request.page_count, 3);
        assert_eq!(request.url, "demo://illust/700000001/801_p1.png");
        assert_eq!(request.extension, "png");

        // And the queue really answers `Queued` for it.
        let manager = DownloadManager::new(DownloadConfig::default());
        manager.enqueue(vec![request]);
        let tasks = manager.tasks();
        assert_eq!(tasks.len(), 1);
        assert!(matches!(tasks[0].state, TaskState::Queued));

        // A work that lost the page falls back to the first one rather than
        // queueing a URL that does not exist; an empty page list records nothing.
        let gone = record(9, "700000001", 7, 1_700_000_000);
        assert_eq!(
            redownload_request(&gone, &pages).map(|r| r.page_index),
            Some(0)
        );
        assert!(redownload_request(&stored, &[]).is_none());
    }

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
