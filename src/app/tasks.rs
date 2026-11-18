//! The boundary between the application and its background runtime.
//!
//! Every task the app starts goes through [`Tasks::spawn`]. That is the whole point:
//! shutdown is a property of the *boundary*, not a rule every call site has to
//! remember. Two facts live here - whether the app has begun shutting down, and the
//! [`CancellationToken`] that in-flight work selects on - and they are set together so
//! a task cannot be started between them.
//!
//! Before this existed, every module held its own `Arc<Runtime>` clone and called
//! `rt.spawn(...)` directly. Nothing could stop those tasks: closing the window
//! dropped the visible UI and left downloads, decodes and fetches running against a
//! runtime that was only wound down by process teardown. See `src/app/lifecycle.rs`
//! for the exit sequence that now drives this handle.
//!
//! # Why the runtime sits behind a lock
//!
//! Parking a tokio runtime needs *ownership* of it (`shutdown_timeout(self, ..)`),
//! and this handle is cloned by every task that wants to start another one. The
//! first version tried `Arc::try_unwrap` and therefore silently failed whenever any
//! task still held a clone - which, at exit, is exactly when the runtime most needs
//! to be parked. The runtime now lives in `Mutex<Option<..>>` so [`Tasks::park`] can
//! take it out no matter how many handles exist, and a second park is a no-op.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

/// The app's handle on its runtime, plus the state that gates it.
///
/// Cheap to clone - it is one `Arc` and a token handle - so the call sites keep the
/// shape they already had (`let rt = app.tasks.clone();`).
#[derive(Clone)]
pub(crate) struct Tasks {
    inner: Arc<Inner>,
}

struct Inner {
    /// `None` once [`Tasks::park`] has taken the runtime to shut it down.
    rt: Mutex<Option<Arc<Runtime>>>,
    /// Cancelled once, at the start of shutdown. Tasks select on it.
    cancel: CancellationToken,
    /// Set before `cancel`, and read by `spawn`. A task started before the flag is
    /// already selecting on the token; a task started after is refused outright.
    closing: AtomicBool,
}

impl Tasks {
    pub(crate) fn new(rt: Arc<Runtime>) -> Self {
        Self {
            inner: Arc::new(Inner {
                rt: Mutex::new(Some(rt)),
                cancel: CancellationToken::new(),
                closing: AtomicBool::new(false),
            }),
        }
    }

    /// Borrow the runtime, if it has not been parked yet.
    fn with_rt<R>(&self, f: impl FnOnce(&Arc<Runtime>) -> R) -> Option<R> {
        let guard = match self.inner.rt.lock() {
            Ok(guard) => guard,
            // Poisoned: a previous holder panicked. Nothing can be started anyway,
            // and the caller's Option is how it learns that.
            Err(_) => {
                tracing::warn!("the runtime lock was poisoned; no background work can start");
                return None;
            }
        };
        guard.as_ref().map(f)
    }

    /// Start a background task, unless the app is closing.
    ///
    /// Returns `false` when the work was refused, so a caller can leave its own state
    /// consistent instead of showing a spinner that will never resolve.
    pub(crate) fn spawn<F>(&self, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.is_closing() {
            return false;
        }
        // The task is wrapped so that a future spawned *before* shutdown also stops
        // when the token fires: awaiting it races cancellation, and losing that race
        // means the task body is dropped rather than run to completion.
        //
        // `biased` matters. Without it `select!` polls the arms in a random order, so
        // a task whose next step is already ready can still be polled once after the
        // token fired - harmless for a download, wrong for anything that writes state
        // during teardown. With the cancellation arm first, a cancelled app never
        // makes progress on new work.
        let cancel = self.inner.cancel.clone();
        let spawned = self.with_rt(|rt| {
            rt.spawn(async move {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => {}
                    () = future => {}
                }
            });
        });
        spawned.is_some()
    }

    /// Run a blocking closure on the runtime's blocking pool.
    ///
    /// Kept separate from `spawn` because the callers need the `JoinHandle` - they
    /// await the value - and because refusing a `spawn_blocking` mid-encode would
    /// leave a half-written file. Shutdown does not cancel these; it bounds them with
    /// the runtime's own `shutdown_timeout`.
    pub(crate) fn spawn_blocking<F, R>(&self, f: F) -> Option<tokio::task::JoinHandle<R>>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.with_rt(|rt| rt.spawn_blocking(f))
    }

    /// Run a future to completion on this runtime.
    ///
    /// Used by the exit flush, which has to *wait* for its write: the runtime is
    /// parked immediately afterwards, so a merely-spawned write would usually lose
    /// that race. This runs on the UI thread, never inside the runtime, which is what
    /// `block_on` requires.
    pub(crate) fn block_on<F: Future>(&self, future: F) -> Option<F::Output> {
        self.with_rt(|rt| rt.block_on(future))
    }

    /// Whether the app has begun shutting down.
    pub(crate) fn is_closing(&self) -> bool {
        self.inner.closing.load(Ordering::Acquire)
    }

    /// Refuse new work and cancel everything already running.
    ///
    /// The flag is set first: a task spawned a microsecond later is refused, one
    /// spawned a microsecond earlier is already selecting on the token.
    pub(crate) fn begin_shutdown(&self) {
        self.inner.closing.store(true, Ordering::Release);
        self.inner.cancel.cancel();
    }

    /// Take the runtime and park it, giving the workers a bounded window to observe
    /// the cancellation.
    ///
    /// This is the step that actually ends the process. It succeeds no matter how many
    /// `Tasks` handles still exist - the runtime is taken out of the mutex rather than
    /// unwrapped out of the `Arc` - and calling it twice is a no-op, so a second exit
    /// path cannot double-park.
    ///
    /// Returns `true` when this call parked the runtime.
    pub(crate) fn park(&self, budget: std::time::Duration) -> bool {
        let taken = match self.inner.rt.lock() {
            Ok(mut slot) => slot.take(),
            // A poisoned lock means a panic while the runtime was being taken. The
            // process is leaving either way, so there is nothing left to report to.
            Err(_) => None,
        };
        let Some(rt) = taken else {
            return false;
        };
        match Arc::try_unwrap(rt) {
            Ok(rt) => {
                rt.shutdown_timeout(budget);
                true
            }
            Err(still_shared) => {
                // A task kept its own `Arc<Runtime>`. Nothing more can be done from
                // here; dropping the last one parks the workers when it goes.
                tracing::warn!(
                    refs = Arc::strong_count(&still_shared),
                    "the runtime left the app while a task still held it; it parks when that task drops"
                );
                false
            }
        }
    }
}