//! Priority-ordered concurrency gate.
//!
//! `tokio::sync::Semaphore` hands permits out first-in-first-out, which is the
//! wrong order for a grid: a prefetch queued a moment earlier must not delay the
//! card that just scrolled into view. This gate keeps one queue of waiters and
//! hands a released slot straight to the highest-priority waiter, tie-broken by
//! arrival order so equal priorities stay fair.

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::oneshot;

use crate::Priority;

struct Waiter {
    priority: Priority,
    seq: u64,
    tx: oneshot::Sender<GatePermit>,
}

struct State {
    available: usize,
    next_seq: u64,
    waiters: Vec<Waiter>,
}

/// A counting gate where the release order follows [`Priority`].
pub(crate) struct PriorityGate {
    state: Mutex<State>,
}

impl PriorityGate {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        let capacity = capacity.max(1);
        Arc::new(Self {
            state: Mutex::new(State {
                available: capacity,
                next_seq: 0,
                waiters: Vec::new(),
            }),
        })
    }

    /// Take a slot, waiting in line behind higher-priority work.
    pub(crate) async fn acquire(self: &Arc<Self>, priority: Priority) -> GatePermit {
        // The guard must not live across the await, so the queueing happens in
        // its own scope and only the receiver escapes it.
        let rx = {
            let mut state = self.state.lock();
            if state.available > 0 {
                state.available -= 1;
                return GatePermit {
                    gate: Some(Arc::clone(self)),
                };
            }
            let seq = state.next_seq;
            state.next_seq += 1;
            let (tx, rx) = oneshot::channel();
            state.waiters.push(Waiter { priority, seq, tx });
            rx
        };
        match rx.await {
            // The released slot arrived as a value: hold exactly that permit.
            Ok(permit) => permit,
            // The gate is gone; nothing to release.
            Err(_) => GatePermit { gate: None },
        }
    }

    /// Hand this slot to the highest-priority waiter, or back to the pool.
    ///
    /// The permit itself travels through the channel. `()` in it was the defect:
    /// a waiter cancelled between the hand-over and its poll drops the receiver,
    /// the value vanished with it, and that slot was gone for the life of the
    /// gate - one per cancellation until `acquire` waited forever. A `GatePermit`
    /// instead comes back in the `Err` arm and puts the slot wherever it belongs,
    /// here or in the next waiter's queue entry.
    ///
    /// The state lock is *not* held across the send: a waiter cancelled while
    /// queued hands the permit straight back, and dropping it re-enters this
    /// function, which behind a lock the sender still held would wait on itself
    /// forever.
    fn release(self: &Arc<Self>) {
        // Queue bookkeeping under the lock, the hand-over outside it.
        let waiter = {
            let mut state = self.state.lock();
            let Some(pos) = state
                .waiters
                .iter()
                .enumerate()
                .min_by_key(|(_, waiter)| (waiter.priority, waiter.seq))
                .map(|(pos, _)| pos)
            else {
                // Nobody is queued, so the slot returns to the pool.
                state.available += 1;
                return;
            };
            state.waiters.swap_remove(pos)
        };
        // The lock is gone here on purpose - see the doc comment above.
        if let Err(returned) = waiter.tx.send(GatePermit { gate: Some(Arc::clone(self)) }) {
            // Dropping it *is* the re-entry: the slot goes to the next waiter, or
            // back to the pool. Nothing is lost either way.
            drop(returned);
        }
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.state.lock().waiters.len()
    }
}

/// Held for as long as the expensive part of a load runs.
pub(crate) struct GatePermit {
    gate: Option<Arc<PriorityGate>>,
}

impl Drop for GatePermit {
    fn drop(&mut self) {
        if let Some(gate) = self.gate.take() {
            gate.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PriorityGate;
    use crate::Priority;
    use parking_lot::Mutex;
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    async fn wait_for(condition: impl Fn() -> bool) {
        for _ in 0..600 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("gate never reached the expected state");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn visible_is_granted_before_near_and_prefetch() {
        let gate = PriorityGate::new(1);
        let held = gate.acquire(Priority::Visible).await;
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

        let mut tasks = Vec::new();
        for (priority, label) in [
            (Priority::Prefetch, "prefetch"),
            (Priority::Near, "near"),
            (Priority::Visible, "visible"),
        ] {
            let gate = Arc::clone(&gate);
            let order = Arc::clone(&order);
            tasks.push(tokio::spawn(async move {
                let permit = gate.acquire(priority).await;
                order.lock().push(label);
                drop(permit);
            }));
        }

        let probe = Arc::clone(&gate);
        wait_for(move || probe.waiting() == 3).await;
        drop(held);
        for task in tasks {
            task.await.expect("waiter task");
        }
        assert_eq!(*order.lock(), vec!["visible", "near", "prefetch"]);
    }

    /// Poll a future once, with a waker that wakes nobody.
    ///
    /// No runtime drives it afterwards, so the single poll is deterministic: the
    /// caller is queued by it and never woken, which is the shape of one that has
    /// already been cancelled.
    fn poll_once<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
        let mut cx = Context::from_waker(Waker::noop());
        std::pin::Pin::new(future).poll(&mut cx)
    }

    /// G43: a cancelled waiter returns its permit.
    ///
    /// The waiter is queued by exactly one poll and then dropped without a second
    /// one, so the release below lands between the hand-over and the poll - the
    /// window the old `send(())` lost the slot in. At capacity 1 a lost slot is
    /// the whole gate: the last `acquire` would wait for a release that can never
    /// come. The timeout is a hang detector, not an assertion about a duration.
    #[tokio::test]
    async fn a_cancelled_waiter_returns_its_permit() {
        let gate = PriorityGate::new(1);
        let held = gate.acquire(Priority::Visible).await;

        let mut queued = Box::pin(gate.acquire(Priority::Near));
        assert!(
            poll_once(&mut queued).is_pending(),
            "the second caller must queue behind the only permit"
        );
        assert_eq!(gate.waiting(), 1, "the caller is queued, not running");

        // Release hands the permit to that queued caller; the caller then goes
        // away without ever receiving it.
        drop(held);
        assert_eq!(gate.waiting(), 0, "the hand-over left the queue");
        drop(queued);

        // The slot came back through `GatePermit::drop`, so this resolves.
        let again = tokio::time::timeout(Duration::from_secs(5), gate.acquire(Priority::Visible))
            .await
            .expect("a cancelled waiter returns its permit");
        assert_eq!(gate.waiting(), 0, "nobody is left queued");
        drop(again);
    }

    /// A waiter cancelled *while queued* hands the permit straight back out of
    /// `send`, and that drop re-enters `release`.
    ///
    /// This is the shape that proves the lock discipline: with the state mutex
    /// held across the send, the re-entry blocks on a lock the same thread is
    /// holding and the gate is wedged for good. The timeout is a hang detector.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_waiter_cancelled_while_queued_does_not_wedge_the_gate() {
        let gate = PriorityGate::new(1);
        let held = gate.acquire(Priority::Visible).await;

        let mut queued = Box::pin(gate.acquire(Priority::Near));
        assert!(poll_once(&mut queued).is_pending());
        // Cancelled before the hand-over: the queue entry is still there, its
        // receiver is gone.
        drop(queued);

        // `release` finds the dead waiter and the slot has to come back out.
        tokio::time::timeout(Duration::from_secs(5), async { drop(held) })
            .await
            .expect("a dead waiter must not wedge the release");
        assert_eq!(gate.waiting(), 0, "the dead entry left the queue");
        let again = tokio::time::timeout(Duration::from_secs(5), gate.acquire(Priority::Visible))
            .await
            .expect("the slot survived a waiter cancelled while queued");
        drop(again);
    }

    #[tokio::test]
    async fn permits_are_returned_to_the_pool() {
        let gate = PriorityGate::new(2);
        let first = gate.acquire(Priority::Visible).await;
        let second = gate.acquire(Priority::Visible).await;
        drop(first);
        let third = gate.acquire(Priority::Prefetch).await;
        drop(second);
        drop(third);
        // Two more acquisitions must succeed immediately, not deadlock.
        let a = gate.acquire(Priority::Near).await;
        let b = gate.acquire(Priority::Near).await;
        assert_eq!(gate.waiting(), 0);
        drop(a);
        drop(b);
    }
}
