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
    tx: oneshot::Sender<()>,
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
        if rx.await.is_err() {
            // The gate is gone; nothing to release.
            return GatePermit { gate: None };
        }
        GatePermit {
            gate: Some(Arc::clone(self)),
        }
    }

    fn release(&self) {
        let mut state = self.state.lock();
        loop {
            let Some(pos) = state
                .waiters
                .iter()
                .enumerate()
                .min_by_key(|(_, waiter)| (waiter.priority, waiter.seq))
                .map(|(pos, _)| pos)
            else {
                state.available += 1;
                return;
            };
            let waiter = state.waiters.swap_remove(pos);
            // A caller that was cancelled drops its receiver; pass the slot on
            // rather than losing it.
            if waiter.tx.send(()).is_ok() {
                return;
            }
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
    use std::sync::Arc;
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
