//! Cover loading coordinator.
//!
//! The previous implementation spawned **one** task per grid render and awaited
//! every cover strictly in sequence inside it, so a 24-item page decoded one
//! JPEG at a time and the last card could only appear after all 23 before it had
//! finished. It also kept no record of what was already in flight, so a resize
//! or a "load more" re-requested the same URLs.
//!
//! This module is the bookkeeping half of the fix; `app.rs` provides the bounded
//! concurrency (`buffer_unordered`) and the UI patching. Everything here is pure
//! so the de-duplication rules can be tested without a network, an event loop or
//! a window.

use std::collections::HashSet;

/// How many cover decodes may be in flight at once. Six keeps a fast SSD busy
/// without starving the two cores that are actually rendering the window.
pub const COVER_CONCURRENCY: usize = 6;

/// One cover worth fetching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverRequest {
    /// Illust id, used as the cache key for patching the right card later.
    pub id: String,
    pub url: String,
}

/// Decode width for a card, in physical pixels.
///
/// Decoding a 3000px original to draw a 200px card wastes ~12MB of RGBA per
/// image and pushes the memory cache into eviction during a normal scroll. The
/// target is rounded up to a 32px multiple so nearby window sizes share one
/// cache entry instead of missing each other by a few pixels.
pub fn target_width(card_width: f32, scale_factor: f32) -> u32 {
    let physical = (card_width.max(1.0) * scale_factor.max(1.0)).ceil() as u32;
    let rounded = physical.div_ceil(32) * 32;
    rounded.clamp(128, 640)
}

#[derive(Default)]
pub struct CoverLoader {
    inflight: HashSet<String>,
    failed: usize,
}

impl CoverLoader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide which covers to start now.
    ///
    /// `items` is (id, url) in reading order — callers pass the feed order, so
    /// the first cards on screen are the first requests issued.
    /// `cached` reports whether a cover is already in the memory cache.
    pub fn plan<I>(&mut self, items: I, cached: impl Fn(&str) -> bool) -> Vec<CoverRequest>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut requests = Vec::new();
        for (id, url) in items {
            if url.is_empty() || cached(&id) || self.inflight.contains(&id) {
                continue;
            }
            self.inflight.insert(id.clone());
            requests.push(CoverRequest { id, url });
        }
        requests
    }

    /// Record that a request finished (either way) so it can be retried later.
    pub fn complete(&mut self, id: &str, ok: bool) {
        self.inflight.remove(id);
        if !ok {
            self.failed += 1;
        }
    }

    /// Drop all in-flight bookkeeping, e.g. when the user navigates away and the
    /// results no longer belong to any visible page.
    pub fn reset(&mut self) {
        self.inflight.clear();
    }

    #[cfg(test)]
    pub fn inflight(&self) -> usize {
        self.inflight.len()
    }

    #[cfg(test)]
    pub fn failed(&self) -> usize {
        self.failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(id: u64) -> (String, String) {
        (id.to_string(), format!("https://example.invalid/{id}.jpg"))
    }

    #[test]
    fn duplicate_ids_in_one_plan_are_requested_once() {
        let mut loader = CoverLoader::new();
        let requests = loader.plan(vec![pair(1), pair(1), pair(2)], |_| false);
        assert_eq!(requests.len(), 2);
        assert_eq!(loader.inflight(), 2);
    }

    #[test]
    fn a_second_plan_does_not_re_request_what_is_already_in_flight() {
        let mut loader = CoverLoader::new();
        let first = loader.plan(vec![pair(1), pair(2)], |_| false);
        assert_eq!(first.len(), 2);
        let second = loader.plan(vec![pair(1), pair(2), pair(3)], |_| false);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].id, "3");
    }

    #[test]
    fn cached_covers_and_empty_urls_are_skipped() {
        let mut loader = CoverLoader::new();
        let requests = loader.plan(
            vec![pair(1), pair(2), ("3".to_string(), String::new())],
            |id| id == "1",
        );
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, "2");
    }

    #[test]
    fn completing_frees_the_slot_for_a_retry() {
        let mut loader = CoverLoader::new();
        loader.plan(vec![pair(1)], |_| false);
        loader.complete("1", false);
        assert_eq!(loader.inflight(), 0);
        assert_eq!(loader.failed(), 1);
        // A failed card may be retried on the next render.
        assert_eq!(loader.plan(vec![pair(1)], |_| false).len(), 1);
    }

    #[test]
    fn order_is_preserved_so_the_first_screen_loads_first() {
        let mut loader = CoverLoader::new();
        let items: Vec<_> = (1..=5).map(pair).collect();
        let requests = loader.plan(items, |_| false);
        assert_eq!(
            requests.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
            vec!["1", "2", "3", "4", "5"]
        );
    }

    #[test]
    fn reset_clears_in_flight_after_navigation() {
        let mut loader = CoverLoader::new();
        loader.plan(vec![pair(1), pair(2)], |_| false);
        loader.reset();
        assert_eq!(loader.inflight(), 0);
        assert_eq!(loader.plan(vec![pair(1)], |_| false).len(), 1);
    }

    #[test]
    fn target_width_is_the_card_size_not_the_original() {
        assert_eq!(target_width(200.0, 1.0), 224);
        assert_eq!(target_width(200.0, 1.5), 320);
        // Never absurdly small or large.
        assert_eq!(target_width(10.0, 1.0), 128);
        assert_eq!(target_width(4000.0, 2.0), 640);
    }
}
