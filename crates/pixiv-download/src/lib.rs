//! Download queue.
//!
//! # Scope
//!
//! Taking *requests* (one per page of an illustration) and turning them into
//! files on disk, with bounded concurrency, progress reporting, cancellation,
//! retry, filename templating and duplicate detection.
//!
//! # Requirements
//!
//! * Concurrency is capped by config, defaulting to 3 — respectful of Pixiv and
//!   of the user's bandwidth.
//! * Every download keeps provenance: the artist and illustration id are written
//!   into the filename template and recorded in SQLite by the app layer.
//! * Re-queuing an already present file must be detected and reported rather than
//!   silently overwriting.
//! * Two requests never render to one path. A template without `{page}` sends every
//!   page of an artwork to one file, and a queue that lets both through publishes
//!   one page, loses the other with nothing said, and reports both `Completed` - so
//!   the second request is refused when it is queued.
//! * Cancellation releases the in-flight request promptly.
//!
//! # Transport
//!
//! This crate never builds HTTP itself. [`DownloadManager::run`] takes a
//! `pixiv_api::Client` and downloads through
//! [`pixiv_api::Client::fetch_bytes`], so every image request silently inherits
//! the mandatory `Referer: https://www.pixiv.net/` header, the session cookie
//! and the client's rate limiting.
//!
//! # Copyright posture
//!
//! This downloads works the user can already view in their own account, for
//! personal offline use only. It is a personal archive tool, not a scraper:
//! there are deliberately no bulk helpers here — no "download every page of
//! every artist", no pagination crawlers, no queueing straight from an artist
//! id. Callers hand in explicit [`DownloadRequest`]s for illustrations the user
//! already opened, and the UI states that limit next to the button.
//!
//! # State machine
//!
//! ```text
//! Queued ──▶ Running ──▶ Completed
//!    │           ├────▶ Failed(msg)  ──retry──▶ Queued
//!    │           ├────▶ Cancelled    ──retry──▶ Queued
//!    │           └────▶ Skipped        (destination already exists)
//!    ├──cancel──▶ Cancelled
//!    ├──refuse──▶ Failed(msg)          (another task renders the same file)
//!    └──pause───▶ Paused   ──resume──▶ Queued
//! Running ──pause──▶ Paused ──resume──▶ Queued
//! ```
//!
//! `Paused` is not terminal: the row keeps its place, its partial bytes are
//! discarded, and `resume` starts the body again from zero. HTTP Range resume is
//! deliberately not implemented — the destination is only published once the whole
//! body has arrived, so restarting is safe and costs one request, where a partial
//! body on disk would be a second code path to get wrong.
//!
//! Files are written to `<name>.part` and renamed into place only once the whole
//! body has been flushed, so an interrupted download can never leave a
//! truncated image that looks complete.

use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;

mod naming;
mod worker;

pub use naming::{dedupe_path, render_filename, sanitize_component};

/// Download failures.
#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("下载失败：{0}")]
    Network(String),
    #[error("写入失败：{0}")]
    Io(String),
    #[error("任务不存在：{0}")]
    UnknownTask(u64),
    #[error("已取消")]
    Cancelled,
}

/// Stable task handle.
pub type TaskId = u64;

/// Lifecycle of one page download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Queued,
    Running,
    /// Held by the user: not eligible for a claim, and not terminal.
    ///
    /// Pausing a *running* task cancels its request rather than waiting for the
    /// body, so the concurrency permit goes back to the pool while the row sits
    /// here — a paused queue must let the tasks that are not paused run.
    Paused,
    Completed,
    Failed(String),
    Cancelled,
    /// The destination file already existed, so nothing was written.
    Skipped,
}

impl TaskState {
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed(_) | Self::Cancelled | Self::Skipped
        )
    }
}

/// What to download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    pub illust_id: String,
    pub title: String,
    pub author: String,
    /// Zero-based page index.
    pub page_index: u32,
    pub page_count: u32,
    pub url: String,
    /// File extension without the dot, e.g. `jpg`.
    pub extension: String,
}

/// Observable progress for one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadTask {
    pub id: TaskId,
    pub request: DownloadRequest,
    pub state: TaskState,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    /// Bytes per second over the last three seconds of this task's own writes.
    ///
    /// Zero before the first chunk lands and after a resume, because the attempt
    /// starts again from zero: a rate that survived a pause would describe bytes
    /// that were already thrown away.
    pub bytes_per_second: u64,
    pub destination: Option<PathBuf>,
}

impl DownloadTask {
    /// Completion percentage in `0..=100`, or `None` when the size is unknown.
    #[must_use]
    pub fn percent(&self) -> Option<u32> {
        let total = self.bytes_total?;
        if total == 0 {
            return None;
        }
        Some(((self.bytes_done.min(total) * 100) / total) as u32)
    }

    /// Seconds left at the current rate, or `None` when that cannot be known.
    ///
    /// `None` rather than a guess in three cases, because a row showing `0 s` reads
    /// as "about to finish": the size is unknown (the body has not arrived, so the
    /// row would claim it is done), no rate has been measured inside the window
    /// (nothing written yet, or a row that is queued or paused), or the row is not
    /// running at all - a terminal row keeps its bytes but has no countdown to
    /// honour. `Some(0)` therefore means exactly "running, and the last byte is in".
    #[must_use]
    pub fn eta_seconds(&self) -> Option<u32> {
        if self.state != TaskState::Running || self.bytes_per_second == 0 {
            return None;
        }
        let total = self.bytes_total?;
        let remaining = total.saturating_sub(self.bytes_done);
        if remaining == 0 {
            return Some(0);
        }
        // Rounded up: a row that has 1.2 s left must read "2 s", because "1 s" is a
        // promise it will miss, and the countdown never shows 0 while bytes remain.
        let seconds =
            remaining / self.bytes_per_second + u64::from(remaining % self.bytes_per_second != 0);
        Some(u32::try_from(seconds).unwrap_or(u32::MAX))
    }
}

/// Queue configuration.
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    pub directory: PathBuf,
    pub concurrency: usize,
    /// Template using `{illust_id}`, `{title}`, `{author}`, `{page}`, `{page_count}`.
    pub filename_template: String,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("."),
            concurrency: 3,
            filename_template: "{illust_id}_p{page}_{title}".to_string(),
        }
    }
}

/// The queue.
///
/// Cheap to clone: every clone shares the same task list, so the UI can keep one
/// while the app hands another to the worker.
#[derive(Clone, Debug)]
pub struct DownloadManager {
    pub(crate) inner: Arc<worker::Inner>,
}
