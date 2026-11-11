//! Queue internals: shared state, task records, and the worker loop.
//!
//! The manager is a thin handle over [`Inner`], which is behind an `Arc` so the
//! worker can own everything it touches. All mutation happens under a
//! `parking_lot::Mutex`; no lock is ever held across an `await`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use pixiv_types::PixivError;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::naming::download_file_name;
use crate::{DownloadConfig, DownloadManager, DownloadRequest, DownloadTask, TaskId, TaskState};

/// Bytes per write; also the cancellation granularity, since the flag is
/// re-checked between chunks.
const CHUNK_BYTES: usize = 64 * 1024;

/// How long the idle loop sleeps before looking at the queue again.
const IDLE_POLL: Duration = Duration::from_millis(20);

/// One queued page download.
#[derive(Debug)]
pub(crate) struct Record {
    id: TaskId,
    request: DownloadRequest,
    state: TaskState,
    bytes_done: u64,
    bytes_total: Option<u64>,
    destination: Option<PathBuf>,
    cancel: Arc<AtomicBool>,
}

impl Record {
    fn snapshot(&self) -> DownloadTask {
        DownloadTask {
            id: self.id,
            request: self.request.clone(),
            state: self.state.clone(),
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            destination: self.destination.clone(),
        }
    }
}

/// Everything a clone of [`DownloadManager`] shares.
#[derive(Debug)]
pub(crate) struct Inner {
    config: RwLock<Arc<DownloadConfig>>,
    tasks: Mutex<Vec<Record>>,
    next_id: AtomicU64,
}

/// A task claimed by the worker: queued -> running, with its destination fixed.
struct Claim {
    id: TaskId,
    request: DownloadRequest,
    destination: PathBuf,
    cancel: Arc<AtomicBool>,
}

impl Inner {
    fn new(config: DownloadConfig) -> Self {
        Self {
            config: RwLock::new(Arc::new(config)),
            tasks: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
        }
    }

    fn config(&self) -> Arc<DownloadConfig> {
        Arc::clone(&self.config.read())
    }

    fn update_config(&self, edit: impl FnOnce(&mut DownloadConfig)) {
        let mut guard = self.config.write();
        let mut config = (**guard).clone();
        edit(&mut config);
        *guard = Arc::new(config);
    }

    fn enqueue(&self, requests: Vec<DownloadRequest>) -> Vec<TaskId> {
        let mut tasks = self.tasks.lock();
        let mut ids = Vec::with_capacity(requests.len());
        for request in requests {
            let id = self
                .next_id
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            tasks.push(Record {
                id,
                request,
                state: TaskState::Queued,
                bytes_done: 0,
                bytes_total: None,
                destination: None,
                cancel: Arc::new(AtomicBool::new(false)),
            });
            ids.push(id);
        }
        ids
    }

    fn tasks(&self) -> Vec<DownloadTask> {
        let tasks = self.tasks.lock();
        tasks.iter().rev().map(Record::snapshot).collect()
    }

    fn cancel(&self, id: TaskId) -> bool {
        let mut tasks = self.tasks.lock();
        let Some(record) = tasks.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        if record.state.is_terminal() {
            return false;
        }
        record.cancel.store(true, Ordering::SeqCst);
        if matches!(record.state, TaskState::Queued) {
            // Never started, so nothing has to unwind: mark it done right here
            // and the worker will simply never pick it up.
            record.state = TaskState::Cancelled;
        }
        true
    }

    fn retry(&self, id: TaskId) -> bool {
        let mut tasks = self.tasks.lock();
        let Some(record) = tasks.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        if !matches!(record.state, TaskState::Failed(_) | TaskState::Cancelled) {
            return false;
        }
        record.state = TaskState::Queued;
        record.bytes_done = 0;
        record.bytes_total = None;
        record.destination = None;
        record.cancel = Arc::new(AtomicBool::new(false));
        true
    }

    fn clear_finished(&self) {
        self.tasks.lock().retain(|r| !r.state.is_terminal());
    }

    fn has_queued(&self) -> bool {
        self.tasks
            .lock()
            .iter()
            .any(|r| matches!(r.state, TaskState::Queued))
    }

    fn take_next_queued(&self) -> Option<Claim> {
        let config = self.config();
        let mut tasks = self.tasks.lock();
        let record = tasks
            .iter_mut()
            .find(|r| matches!(r.state, TaskState::Queued))?;
        let name = download_file_name(&config.filename_template, &record.request);
        let destination = config.directory.join(name);
        record.state = TaskState::Running;
        record.destination = Some(destination.clone());
        Some(Claim {
            id: record.id,
            request: record.request.clone(),
            destination,
            cancel: Arc::clone(&record.cancel),
        })
    }

    fn set_progress(&self, id: TaskId, done: u64, total: Option<u64>) {
        if let Some(record) = self.tasks.lock().iter_mut().find(|r| r.id == id) {
            record.bytes_done = done;
            record.bytes_total = total;
        }
    }

    fn finish(&self, id: TaskId, state: TaskState, done: u64, total: Option<u64>) {
        if let Some(record) = self.tasks.lock().iter_mut().find(|r| r.id == id) {
            record.state = state;
            record.bytes_done = done;
            record.bytes_total = total;
        }
    }
}

impl DownloadManager {
    #[must_use]
    pub fn new(config: DownloadConfig) -> Self {
        Self {
            inner: Arc::new(Inner::new(config)),
        }
    }

    /// Shared handle for the UI to observe.
    #[must_use]
    pub fn config(&self) -> Arc<DownloadConfig> {
        self.inner.config()
    }

    pub fn set_directory(&self, dir: PathBuf) {
        self.inner.update_config(|config| config.directory = dir);
    }

    pub fn set_template(&self, template: String) {
        self.inner
            .update_config(|config| config.filename_template = template);
    }

    /// Queue one request per page. Returns the new task ids.
    pub fn enqueue(&self, requests: Vec<DownloadRequest>) -> Vec<TaskId> {
        self.inner.enqueue(requests)
    }

    /// Snapshot of every task, newest first.
    #[must_use]
    pub fn tasks(&self) -> Vec<DownloadTask> {
        self.inner.tasks()
    }

    pub fn cancel(&self, id: TaskId) -> bool {
        self.inner.cancel(id)
    }

    /// Re-queue a failed or cancelled task.
    pub fn retry(&self, id: TaskId) -> bool {
        self.inner.retry(id)
    }

    /// Drop terminal tasks from the list.
    pub fn clear_finished(&self) {
        self.inner.clear_finished();
    }

    /// Run the worker loop until the manager is dropped.
    ///
    /// Intended to be spawned once by the app on a tokio task. The client is
    /// only used through [`pixiv_api::Client::fetch_bytes`], so downloads
    /// inherit its `Referer`, session and rate limiting.
    pub async fn run(&self, client: pixiv_api::Client) {
        let fetch = move |url: String| {
            let client = client.clone();
            async move { client.fetch_bytes(&url).await }
        };
        self.run_with(fetch).await;
    }

    /// Worker loop over an arbitrary body fetcher; [`Self::run`] is this plus
    /// the real client, and the tests drive it with a local fake.
    pub(crate) async fn run_with<F, Fut>(&self, fetch: F)
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(Vec<u8>, Option<String>), PixivError>> + Send + 'static,
    {
        run_loop(Arc::downgrade(&self.inner), Arc::new(fetch)).await;
    }
}

async fn run_loop<F, Fut>(weak: Weak<Inner>, fetch: Arc<F>)
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(Vec<u8>, Option<String>), PixivError>> + Send + 'static,
{
    let mut inflight: JoinSet<()> = JoinSet::new();
    let Some(first) = weak.upgrade() else {
        return;
    };
    // The cap is a hard bound: a permit must be held for the whole task.
    let permits = first.config().concurrency.max(1);
    let semaphore = Arc::new(Semaphore::new(permits));
    drop(first);

    loop {
        while let Some(joined) = inflight.try_join_next() {
            if let Err(err) = joined {
                tracing::warn!(%err, "download worker task did not finish cleanly");
            }
        }

        // Never hold a strong reference across the iteration boundary, so
        // `weak.strong_count()` really means "a caller still owns the manager".
        let Some(inner) = weak.upgrade() else {
            if inflight.is_empty() {
                return;
            }
            inflight.join_next().await;
            continue;
        };

        if !inner.has_queued() {
            let idle = inflight.is_empty();
            drop(inner);
            if idle {
                tokio::time::sleep(IDLE_POLL).await;
            } else {
                inflight.join_next().await;
            }
            continue;
        }

        let Ok(permit) = Arc::clone(&semaphore).acquire_owned().await else {
            return;
        };
        let Some(claim) = inner.take_next_queued() else {
            // Another cloner won the race; the permit is dropped with this arm.
            continue;
        };
        let task_inner = Arc::clone(&inner);
        let task_fetch = Arc::clone(&fetch);
        drop(inner);
        inflight.spawn(async move {
            let _permit = permit;
            process(task_inner, claim, task_fetch).await;
        });
    }
}

async fn process<F, Fut>(inner: Arc<Inner>, claim: Claim, fetch: Arc<F>)
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(Vec<u8>, Option<String>), PixivError>> + Send + 'static,
{
    let Claim {
        id,
        request,
        destination,
        cancel,
    } = claim;
    let cancelled = || cancel.load(Ordering::SeqCst);
    let log = LogCtx {
        id,
        illust: request.illust_id.clone(),
        page: request.page_index,
    };

    if cancelled() {
        inner.finish(id, TaskState::Cancelled, 0, None);
        return;
    }

    if destination.exists() {
        tracing::debug!(task = log.id, illust = %log.illust, page = log.page, "destination exists, skipping");
        inner.finish(id, TaskState::Skipped, 0, None);
        return;
    }

    let Some(parent) = destination.parent().map(Path::to_path_buf) else {
        inner.finish(id, TaskState::Failed("目标路径无效".to_string()), 0, None);
        return;
    };
    if let Err(err) = tokio::fs::create_dir_all(&parent).await {
        inner.finish(
            id,
            TaskState::Failed(format!("无法创建目录：{}", short_io(&err))),
            0,
            None,
        );
        return;
    }

    // `fetch_bytes` returns the whole body, so there is no Content-Length to
    // report while the request is in flight; the size becomes known on arrival.
    let body = match fetch(request.url.clone()).await {
        Ok((bytes, _content_type)) => bytes,
        Err(PixivError::Cancelled) => {
            inner.finish(id, TaskState::Cancelled, 0, None);
            return;
        }
        Err(err) => {
            let reason = describe(&err);
            tracing::warn!(task = log.id, illust = %log.illust, page = log.page, %reason, "download failed");
            inner.finish(id, TaskState::Failed(reason), 0, None);
            return;
        }
    };

    if cancelled() {
        inner.finish(id, TaskState::Cancelled, 0, None);
        return;
    }

    let total = body.len() as u64;
    inner.set_progress(id, 0, Some(total));

    // Write beside the destination, then rename: a cancelled or interrupted
    // download can never leave a truncated file that looks complete.
    let part = part_path(&destination);
    let mut file = match tokio::fs::File::create(&part).await {
        Ok(file) => file,
        Err(err) => {
            inner.finish(
                id,
                TaskState::Failed(format!("无法创建临时文件：{}", short_io(&err))),
                0,
                Some(total),
            );
            return;
        }
    };

    let mut written = 0u64;
    let mut interrupted: Option<String> = None;
    for chunk in body.chunks(CHUNK_BYTES) {
        if cancelled() {
            break;
        }
        if let Err(err) = file.write_all(chunk).await {
            interrupted = Some(format!("写入失败：{}", short_io(&err)));
            break;
        }
        written = written.saturating_add(chunk.len() as u64);
        inner.set_progress(id, written, Some(total));
    }
    if interrupted.is_none() && !cancelled() {
        if let Err(err) = file.flush().await {
            interrupted = Some(format!("写入失败：{}", short_io(&err)));
        }
    }
    drop(file);

    if let Some(reason) = interrupted {
        remove_part(&part).await;
        inner.finish(id, TaskState::Failed(reason), 0, Some(total));
        return;
    }
    if cancelled() {
        remove_part(&part).await;
        tracing::debug!(task = log.id, illust = %log.illust, page = log.page, "download cancelled");
        inner.finish(id, TaskState::Cancelled, 0, None);
        return;
    }

    match tokio::fs::rename(&part, &destination).await {
        Ok(()) => {
            tracing::debug!(task = log.id, illust = %log.illust, page = log.page, bytes = written, "download complete");
            inner.finish(id, TaskState::Completed, written, Some(total));
        }
        Err(err) => {
            remove_part(&part).await;
            inner.finish(
                id,
                TaskState::Failed(format!("重命名失败：{}", short_io(&err))),
                0,
                Some(total),
            );
        }
    }
}

/// Log fields. Deliberately excludes the URL: it can carry session-scoped
/// query parameters, and the justification and page identify a task well enough.
struct LogCtx {
    id: TaskId,
    illust: String,
    page: u32,
}

fn part_path(destination: &Path) -> PathBuf {
    let mut name = destination
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    name.push_str(".part");
    destination.with_file_name(name)
}

async fn remove_part(part: &Path) {
    if let Err(err) = tokio::fs::remove_file(part).await {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(%err, "could not remove partial download");
        }
    }
}

/// Short, human-readable reason stored in [`TaskState::Failed`]. Never echoes a
/// network payload, and truncates server text so one bad response cannot fill
/// the UI.
fn describe(err: &PixivError) -> String {
    match err {
        PixivError::Network(_) => "网络请求失败；请检查网络后重试".to_string(),
        PixivError::Http { status, message } => {
            format!("服务器返回 {status}：{}", truncate(message, 80))
        }
        PixivError::Unauthorized => "登录状态已失效，请重新登录".to_string(),
        PixivError::Api(message) => format!("接口错误：{}", truncate(message, 80)),
        PixivError::RateLimited(_) => "请求过于频繁，请稍后再试".to_string(),
        PixivError::Decode(_) => "响应无法解析".to_string(),
        PixivError::NotFound(_) => "资源不存在或当前账号不可见".to_string(),
        PixivError::Io(_) => "读取响应失败".to_string(),
        PixivError::Cancelled => "已取消".to_string(),
        PixivError::Other(message) => truncate(message, 80),
    }
}

fn short_io(err: &std::io::Error) -> String {
    truncate(&err.to_string(), 80)
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn request(page_index: u32) -> DownloadRequest {
        DownloadRequest {
            illust_id: "42".to_string(),
            title: "テスト".to_string(),
            author: "artist".to_string(),
            page_index,
            page_count: 3,
            url: format!("https://i.pximg.net/img-original/{page_index}.jpg"),
            extension: "jpg".to_string(),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::AtomicU64 as Seq;
        static SEQ: Seq = Seq::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "pixiv-download-worker-{}-{tag}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn config(directory: PathBuf, concurrency: usize) -> DownloadConfig {
        DownloadConfig {
            directory,
            concurrency,
            filename_template: "{illust_id}_p{page}".to_string(),
        }
    }

    /// Fetch that records how many requests are in flight at once.
    async fn tracked_fetch(
        live: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        delay: Duration,
        body: Vec<u8>,
    ) -> Result<(Vec<u8>, Option<String>), PixivError> {
        let now = live.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(delay).await;
        live.fetch_sub(1, Ordering::SeqCst);
        Ok((body, Some("image/jpeg".to_string())))
    }

    async fn wait_terminal(manager: &DownloadManager) -> Vec<DownloadTask> {
        for _ in 0..600 {
            let tasks = manager.tasks();
            if !tasks.is_empty() && tasks.iter().all(|t| t.state.is_terminal()) {
                return tasks;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        manager.tasks()
    }

    #[test]
    fn enqueue_assigns_ids_and_lists_newest_first() {
        let dir = temp_dir("enqueue");
        let manager = DownloadManager::new(config(dir.clone(), 3));
        let ids = manager.enqueue(vec![request(0), request(1), request(2)]);
        assert_eq!(ids, vec![1, 2, 3]);

        let tasks = manager.tasks();
        assert_eq!(tasks.len(), 3);
        assert_eq!(
            tasks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
        assert!(tasks.iter().all(|t| t.state == TaskState::Queued));
        assert!(tasks.iter().all(|t| t.bytes_done == 0));
        assert!(tasks.iter().all(|t| t.bytes_total.is_none()));
        assert!(tasks.iter().all(|t| t.percent().is_none()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_and_retry_move_through_the_state_machine() {
        let dir = temp_dir("cancel");
        let manager = DownloadManager::new(config(dir.clone(), 3));
        let ids = manager.enqueue(vec![request(0), request(1)]);

        assert!(manager.cancel(ids[0]));
        let cancelled = manager
            .tasks()
            .into_iter()
            .find(|t| t.id == ids[0])
            .unwrap();
        assert_eq!(cancelled.state, TaskState::Cancelled);
        // Cancelling twice, or cancelling an unknown id, is not an error but a no.
        assert!(!manager.cancel(ids[0]));
        assert!(!manager.cancel(9999));

        assert!(manager.retry(ids[0]));
        let queued = manager
            .tasks()
            .into_iter()
            .find(|t| t.id == ids[0])
            .unwrap();
        assert_eq!(queued.state, TaskState::Queued);
        // A queued task cannot be retried.
        assert!(!manager.retry(ids[0]));

        manager.clear_finished();
        assert_eq!(manager.tasks().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_finished_drops_only_terminal_tasks() {
        let dir = temp_dir("clear");
        let manager = DownloadManager::new(config(dir.clone(), 3));
        let ids = manager.enqueue(vec![request(0), request(1), request(2)]);
        assert!(manager.cancel(ids[0]));
        // ids[1] and ids[2] stay queued.
        manager.clear_finished();
        let remaining: Vec<TaskId> = manager.tasks().iter().map(|t| t.id).collect();
        assert_eq!(remaining, vec![ids[2], ids[1]]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_directory_and_set_template_replace_the_config() {
        let dir = temp_dir("config");
        let manager = DownloadManager::new(config(dir.clone(), 3));
        let other = dir.join("nested");
        manager.set_directory(other.clone());
        manager.set_template("{author}_{page}".to_string());
        let config = manager.config();
        assert_eq!(config.directory, other);
        assert_eq!(config.filename_template, "{author}_{page}");
        assert_eq!(config.concurrency, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrency_cap_is_enforced_not_advisory() {
        let dir = temp_dir("cap");
        let manager = DownloadManager::new(config(dir.clone(), 1));
        let ids = manager.enqueue((0..4).map(request).collect());
        assert_eq!(ids.len(), 4);

        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let fetch = {
            let live = Arc::clone(&live);
            let peak = Arc::clone(&peak);
            move |_url: String| {
                let live = Arc::clone(&live);
                let peak = Arc::clone(&peak);
                async move { tracked_fetch(live, peak, Duration::from_millis(40), vec![7u8; 32]).await }
            }
        };

        let runner = manager.clone();
        let handle = tokio::spawn(async move { runner.run_with(fetch).await });
        let tasks = wait_terminal(&manager).await;
        handle.abort();

        assert!(
            tasks.iter().all(|t| t.state == TaskState::Completed),
            "{tasks:?}"
        );
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "only one request may be in flight"
        );
        assert_eq!(live.load(Ordering::SeqCst), 0);
        for task in &tasks {
            let destination = task.destination.clone().expect("destination recorded");
            assert!(destination.exists());
            assert_eq!(std::fs::read(&destination).unwrap().len(), 32);
            assert_eq!(task.bytes_done, 32);
            assert_eq!(task.bytes_total, Some(32));
            assert_eq!(task.percent(), Some(100));
        }
        // No partial files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_cap_holds_for_a_larger_batch() {
        let dir = temp_dir("cap2");
        let manager = DownloadManager::new(config(dir.clone(), 2));
        manager.enqueue((0..6).map(request).collect());

        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let fetch = {
            let live = Arc::clone(&live);
            let peak = Arc::clone(&peak);
            move |_url: String| {
                let live = Arc::clone(&live);
                let peak = Arc::clone(&peak);
                async move { tracked_fetch(live, peak, Duration::from_millis(30), vec![1u8; 8]).await }
            }
        };

        let runner = manager.clone();
        let handle = tokio::spawn(async move { runner.run_with(fetch).await });
        let tasks = wait_terminal(&manager).await;
        handle.abort();

        assert_eq!(tasks.len(), 6);
        assert!(tasks.iter().all(|t| t.state == TaskState::Completed));
        assert!(peak.load(Ordering::SeqCst) <= 2, "cap is 2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_existing_destination_is_skipped_and_never_overwritten() {
        let dir = temp_dir("skip");
        let manager = DownloadManager::new(config(dir.clone(), 2));
        let ids = manager.enqueue(vec![request(0)]);

        let existing = dir.join("42_p1.jpg");
        std::fs::write(&existing, b"original").expect("seed file");

        let calls = Arc::new(AtomicUsize::new(0));
        let fetch = {
            let calls = Arc::clone(&calls);
            move |_url: String| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, PixivError>((vec![9u8; 16], None::<String>))
                }
            }
        };

        let runner = manager.clone();
        let handle = tokio::spawn(async move { runner.run_with(fetch).await });
        let tasks = wait_terminal(&manager).await;
        handle.abort();

        assert_eq!(tasks[0].id, ids[0]);
        assert_eq!(tasks[0].state, TaskState::Skipped);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no request for a skipped task"
        );
        assert_eq!(std::fs::read(&existing).unwrap(), b"original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_request_becomes_failed_and_can_be_retried() {
        let dir = temp_dir("fail");
        let manager = DownloadManager::new(config(dir.clone(), 1));
        let ids = manager.enqueue(vec![request(0)]);

        let fetch = move |_url: String| async move {
            Err::<(Vec<u8>, Option<String>), PixivError>(PixivError::Network("reset".to_string()))
        };
        let runner = manager.clone();
        let handle = tokio::spawn(async move { runner.run_with(fetch).await });
        let tasks = wait_terminal(&manager).await;
        handle.abort();

        match &tasks[0].state {
            TaskState::Failed(message) => assert!(!message.is_empty()),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(manager.retry(ids[0]));

        let fetch =
            move |_url: String| async move { Ok::<_, PixivError>((vec![3u8; 4], None::<String>)) };
        let runner = manager.clone();
        let handle = tokio::spawn(async move { runner.run_with(fetch).await });
        let tasks = wait_terminal(&manager).await;
        handle.abort();
        assert_eq!(tasks[0].state, TaskState::Completed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_a_running_task_leaves_no_partial_file() {
        let dir = temp_dir("cancel-run");
        let manager = DownloadManager::new(config(dir.clone(), 1));
        let ids = manager.enqueue(vec![request(0)]);

        let fetch = move |_url: String| async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            Ok::<_, PixivError>((vec![5u8; 4096], None::<String>))
        };
        let runner = manager.clone();
        let handle = tokio::spawn(async move { runner.run_with(fetch).await });

        // Wait until the worker has claimed the task, then cancel it in flight.
        for _ in 0..100 {
            if manager
                .tasks()
                .first()
                .is_some_and(|t| t.state == TaskState::Running)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(manager.cancel(ids[0]));
        let tasks = wait_terminal(&manager).await;
        handle.abort();

        assert_eq!(tasks[0].state, TaskState::Cancelled);
        assert_eq!(tasks[0].bytes_done, 0);
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(entries.is_empty(), "no file may survive cancellation");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_a_queued_task_never_starts_the_request() {
        let dir = temp_dir("cancel-queued");
        let manager = DownloadManager::new(config(dir.clone(), 1));
        let ids = manager.enqueue(vec![request(0), request(1)]);

        let calls = Arc::new(AtomicUsize::new(0));
        let fetch = {
            let calls = Arc::clone(&calls);
            move |_url: String| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    Ok::<_, PixivError>((vec![1u8; 8], None::<String>))
                }
            }
        };
        let runner = manager.clone();
        let handle = tokio::spawn(async move { runner.run_with(fetch).await });

        // The second task is still queued behind the first; cancel it there.
        assert!(manager.cancel(ids[1]));
        let tasks = wait_terminal(&manager).await;
        handle.abort();

        let second = tasks.iter().find(|t| t.id == ids[1]).unwrap();
        assert_eq!(second.state, TaskState::Cancelled);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the cancelled task never fetched"
        );
        assert!(!dir.join("42_p2.jpg").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_message_is_short_and_never_panics_on_long_text() {
        let long = PixivError::Api("x".repeat(5000));
        let message = describe(&long);
        assert!(message.chars().count() <= 90, "{}", message.chars().count());
        assert_eq!(describe(&PixivError::Cancelled), "已取消");
        assert!(describe(&PixivError::NotFound("gone".to_string())).contains("不存在"));
        assert!(
            describe(&PixivError::Http {
                status: 403,
                message: "forbidden".to_string()
            })
            .contains("403")
        );
    }

    #[test]
    fn part_path_sits_next_to_the_destination() {
        let destination = PathBuf::from("dir").join("42_p1.jpg");
        assert_eq!(
            part_path(&destination),
            PathBuf::from("dir").join("42_p1.jpg.part")
        );
    }
}
