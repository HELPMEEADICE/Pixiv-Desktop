//! Cover loading coordinator.
//!
//! The previous implementation spawned **one** task per grid render and awaited
//! every cover strictly in sequence inside it, so a 24-item page decoded one
//! JPEG at a time and the last card could only appear after all 23 before it had
//! finished. It also kept no record of what was already in flight, so a resize
//! or a "load more" re-requested the same URLs.
//!
//! This module is the bookkeeping half of the fix; the feed provides the bounded
//! concurrency (`buffer_unordered`) and the UI patching. Everything here is pure
//! so the de-duplication rules can be tested without a network, an event loop or
//! a window.
//!
//! # Tiers, and why a cover has to be re-requested
//!
//! A decode target is a *tier*: a multiple of [`TIER_STEP`] physical pixels, so
//! two nearby window widths share one decoded bitmap instead of missing each
//! other by a few pixels and decoding twice.
//!
//! Which tier a card needs depends on a width Slint measures **after** the first
//! frame, and the geometry it is measured from starts as a guess
//! (`GridGeometry::for_width(858.0)`), so every cover is requested for a width
//! nobody has measured yet. That is not fixable by waiting: a page with no grid,
//! or a window that never lays out, would then request nothing at all. It is
//! fixable after the fact, which is what [`needs_redecode`] states and what
//! [`CoverLoader::replan`] does the bookkeeping for - which ids the feed must
//! drop, and which in-flight slots must be freed so the new request is not
//! suppressed by the old one. Both are pure, so "one re-plan per settle" is a
//! unit test and not something to watch in a window.

use std::collections::{HashMap, HashSet};

/// How many cover decodes may be in flight at once. Six keeps a fast SSD busy
/// without starving the two cores that are actually rendering the window.
pub const COVER_CONCURRENCY: usize = 6;

/// Granularity of a decode target, in physical pixels.
///
/// 32px is the band inside which two card widths share one decoded bitmap: at
/// 150% that is ~21 logical px, finer than a drag step and far coarser than the
/// noise in a measured width.
pub const TIER_STEP: u32 = 32;

/// One cover worth fetching, and the tier it is being fetched for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverRequest {
    /// Illust id, used as the cache key for patching the right card later.
    pub id: String,
    pub url: String,
    /// Physical decode width this request was planned at.
    ///
    /// It travels with the request so the result is recorded at the tier it was
    /// actually decoded for, not at whatever the geometry happens to say when it
    /// lands - those differ on every resize that crosses a tier.
    pub target: u32,
}

/// Round a physical width up to the nearest decode tier.
pub fn tier_up(physical: u32) -> u32 {
    physical.div_ceil(TIER_STEP) * TIER_STEP
}

/// Decode width for a card, in physical pixels.
///
/// Decoding a 3000px original to draw a 200px card wastes ~12MB of RGBA per
/// image and pushes the memory cache into eviction during a normal scroll. The
/// target is rounded up to a 32px multiple ([`tier_up`]) so nearby window sizes
/// share one cache entry instead of missing each other by a few pixels.
pub fn target_width(card_width: f32, scale_factor: f32) -> u32 {
    target_width_up_to(card_width, scale_factor, CARD_MAX_WIDTH)
}

/// Ceiling for a card cover. A grid card is at most ~420 logical px wide, so a
/// 640px decode already covers a 150% display with room for the settle.
pub const CARD_MAX_WIDTH: u32 = 640;

/// Ceiling for the profile banner, which is drawn across the whole header row.
///
/// The card ceiling is the wrong limit for it: at 150% a full-width banner asks
/// for ~1 800 physical px and the card clamp handed back 640, so the image was
/// decoded at a third of its draw size and then stretched (plan §2.2 U8).
pub const BANNER_MAX_WIDTH: u32 = 2048;

/// [`target_width`] for a surface that is wider than a card.
///
/// Same rule - the [`tier_up`] rounding is what lets two nearby window widths
/// share one decoded bitmap - with a ceiling the caller names instead of the
/// card's.
pub fn target_width_up_to(width: f32, scale_factor: f32, cap: u32) -> u32 {
    let physical = (width.max(1.0) * scale_factor.max(1.0)).ceil() as u32;
    tier_up(physical).clamp(128, cap.max(128))
}

/// Must a cover decoded at tier `decoded` be requested again for `target`?
///
/// One comparison, and deliberately one-sided: **only upward**. A bitmap at or
/// above the target is drawn down and is never re-requested, so shrinking a
/// window costs no decode and the widest tier a card has ever needed stays in the
/// cache. Anything below the target is refused even when the shortfall is a
/// single tier - that is exactly the case that produced the soft covers in
/// `docs/UI-NOTES.md` section 12i, where a 320px decode served a box whose tier
/// was 352 and was never replaced.
pub fn needs_redecode(decoded: u32, target: u32) -> bool {
    decoded < target
}

/// What a settle found: the covers the feed has to request again at a new tier.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Replan {
    /// Ids whose drawn cover was decoded below the settled tier, sorted. The
    /// caller drops them from its own map so the next [`CoverLoader::plan`]
    /// re-requests them in feed order.
    pub stale: Vec<String>,
    /// How many requests were in flight at a tier below the settled one. Their
    /// bookkeeping is cleared, because it would otherwise suppress the new
    /// request; their answer still arrives and is refused by
    /// [`CoverLoader::is_superseded`].
    pub restarted: usize,
}

impl Replan {
    /// True when the settle asks for nothing - the width stayed inside one tier.
    pub fn is_empty(&self) -> bool {
        self.stale.is_empty() && self.restarted == 0
    }
}

#[derive(Default)]
pub struct CoverLoader {
    /// id -> the tier the request in flight was planned at.
    inflight: HashMap<String, u32>,
    /// id -> the tier the cover the feed currently *holds* was decoded at. Kept
    /// separately from `inflight` because the two disagree the moment a re-plan
    /// frees a slot: the card still holds the old pixels until the new decode
    /// lands.
    tiers: HashMap<String, u32>,
    failed: usize,
}

impl CoverLoader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide which covers to start now.
    ///
    /// `items` is (id, url) in reading order - callers pass the feed order, so
    /// the first cards on screen are the first requests issued.
    /// `target` is the tier every request in this plan is decoded for; it is
    /// stamped onto each [`CoverRequest`] and recorded on completion.
    /// `cached` reports whether a cover is already in the app-side map.
    pub fn plan<I>(
        &mut self,
        items: I,
        target: u32,
        cached: impl Fn(&str) -> bool,
    ) -> Vec<CoverRequest>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut requests = Vec::new();
        for (id, url) in items {
            if url.is_empty() || cached(&id) || self.inflight.contains_key(&id) {
                continue;
            }
            self.inflight.insert(id.clone(), target);
            requests.push(CoverRequest { id, url, target });
        }
        requests
    }

    /// Record that the request planned at `target` finished, either way.
    ///
    /// Two things keep the record honest. The tier is remembered only for a decode
    /// that actually landed — a failure must not claim pixels the card does not
    /// have, or the next settle would consider the card served and leave it soft.
    /// And the slot is cleared only when this is still the tier it holds: a
    /// smaller request that lost the race to a re-plan must neither free the newer
    /// slot nor claim its tier.
    pub fn complete(&mut self, id: &str, target: u32, ok: bool) {
        if self.inflight.get(id).copied() == Some(target) {
            self.inflight.remove(id);
            if ok {
                let tier = self.tiers.entry(id.to_string()).or_insert(target);
                *tier = (*tier).max(target);
            }
        }
        if !ok {
            self.failed += 1;
        }
    }

    /// Record that a cover is on screen at `tier` without this loader having
    /// requested it - the draw path took the pixels straight out of
    /// `ImageCache` after a re-plan dropped the app-side entry.
    ///
    /// Takes the maximum, so adopting a narrow entry can never lower what is
    /// already known about the id.
    pub fn adopt(&mut self, id: &str, tier: u32) {
        let slot = self.tiers.entry(id.to_string()).or_insert(tier);
        *slot = (*slot).max(tier);
    }

    /// Re-plan after the measured width settled on a new tier.
    ///
    /// `held` is every id the feed still has a drawn cover for. The work is
    /// exactly what a stale cover needs, plus one sweep that is free here:
    ///
    /// * a held cover decoded below `target` is reported in [`Replan::stale`]
    ///   and its tier record is dropped, so the caller can drop the pixels and
    ///   request them again;
    /// * a request in flight at a tier below `target` is reported in
    ///   [`Replan::restarted`] and its slot is cleared - otherwise `plan` would
    ///   skip the id and the card would keep the small decode forever;
    /// * tier records for ids that are neither held nor in flight are collected,
    ///   because this is the one path that already walks both sets.
    pub fn replan<'a, I>(&mut self, held: I, target: u32) -> Replan
    where
        I: Iterator<Item = &'a str>,
    {
        let held: HashSet<&str> = held.collect();
        let mut stale: Vec<String> = held
            .iter()
            .filter(|id| {
                self.tiers
                    .get(**id)
                    .is_some_and(|decoded| needs_redecode(*decoded, target))
            })
            .map(|id| (*id).to_string())
            .collect();
        // `held` is a set, so the walk above is unordered. Sorted here so the
        // return value is deterministic for tests; the caller re-requests in feed
        // order, so this order never reaches the screen.
        stale.sort();
        for id in &stale {
            self.tiers.remove(id);
        }

        let restarted = self.restart_planned_below(target);

        let inflight = &self.inflight;
        self.tiers
            .retain(|id, _| held.contains(id.as_str()) || inflight.contains_key(id));

        Replan { stale, restarted }
    }

    /// Free every in-flight slot planned below `target`; returns how many.
    fn restart_planned_below(&mut self, target: u32) -> usize {
        let below: Vec<String> = self
            .inflight
            .iter()
            .filter(|(_, planned)| needs_redecode(**planned, target))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &below {
            self.inflight.remove(id);
            self.tiers.remove(id);
        }
        below.len()
    }

    /// Is a decode landing at `target` a step *back* for `id`?
    ///
    /// A re-plan frees in-flight slots, so two decodes of one cover can be in the
    /// air at different tiers, and the older, smaller one can finish last. It
    /// must not repaint a card that already holds the wider bitmap.
    pub fn is_superseded(&self, id: &str, target: u32) -> bool {
        self.tiers.get(id).is_some_and(|held| *held > target)
    }

    /// Drop all in-flight bookkeeping, e.g. when the user navigates away and the
    /// results no longer belong to any visible page.
    ///
    /// The tier records survive: they describe what the feed still *holds*, and
    /// the app-side cover map outlives a navigation.
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

    #[cfg(test)]
    pub fn tier_of(&self, id: &str) -> Option<u32> {
        self.tiers.get(id).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tier the tests plan at when the exact number does not matter.
    const TIER: u32 = 320;

    /// The profile banner is not a card and must not be decoded at the card
    /// ceiling. At 150% it is drawn ~1 800 physical px wide; `target_width`
    /// clamps to 640, which is why the header looked soft (plan §2.2 U8).
    #[test]
    fn a_banner_is_not_decoded_at_the_card_ceiling() {
        assert_eq!(target_width(1200.0, 1.5), 640, "the card ceiling stays");
        assert!(
            target_width_up_to(1200.0, 1.5, 2048) >= (1200.0_f32 * 1.5) as u32,
            "a full-width banner must be decoded at its draw size, not a card's"
        );
        // Still on the 32px tier grid, so two nearby window widths share a decode.
        assert_eq!(target_width_up_to(1200.0, 1.5, 2048) % TIER_STEP, 0);
        // The cap is the caller's, and it is a real ceiling.
        assert_eq!(target_width_up_to(4000.0, 2.0, 2048), 2048);
        // A card-tier cap reproduces the old answer exactly.
        assert_eq!(target_width_up_to(1200.0, 1.5, CARD_MAX_WIDTH), 640);
    }

    fn pair(id: u64) -> (String, String) {
        (id.to_string(), format!("https://example.invalid/{id}.jpg"))
    }

    #[test]
    fn duplicate_ids_in_one_plan_are_requested_once() {
        let mut loader = CoverLoader::new();
        let requests = loader.plan(vec![pair(1), pair(1), pair(2)], TIER, |_| false);
        assert_eq!(requests.len(), 2);
        assert_eq!(loader.inflight(), 2);
    }

    #[test]
    fn a_second_plan_does_not_re_request_what_is_already_in_flight() {
        let mut loader = CoverLoader::new();
        let first = loader.plan(vec![pair(1), pair(2)], TIER, |_| false);
        assert_eq!(first.len(), 2);
        let second = loader.plan(vec![pair(1), pair(2), pair(3)], TIER, |_| false);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].id, "3");
    }

    #[test]
    fn cached_covers_and_empty_urls_are_skipped() {
        let mut loader = CoverLoader::new();
        let requests = loader.plan(
            vec![pair(1), pair(2), ("3".to_string(), String::new())],
            TIER,
            |id| id == "1",
        );
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, "2");
    }

    #[test]
    fn completing_frees_the_slot_for_a_retry() {
        let mut loader = CoverLoader::new();
        loader.plan(vec![pair(1)], TIER, |_| false);
        loader.complete("1", TIER, false);
        assert_eq!(loader.inflight(), 0);
        assert_eq!(loader.failed(), 1);
        // A failed card may be retried on the next render.
        assert_eq!(loader.plan(vec![pair(1)], TIER, |_| false).len(), 1);
    }

    #[test]
    fn order_is_preserved_so_the_first_screen_loads_first() {
        let mut loader = CoverLoader::new();
        let items: Vec<_> = (1..=5).map(pair).collect();
        let requests = loader.plan(items, TIER, |_| false);
        assert_eq!(
            requests.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
            vec!["1", "2", "3", "4", "5"]
        );
    }

    #[test]
    fn reset_clears_in_flight_after_navigation() {
        let mut loader = CoverLoader::new();
        loader.plan(vec![pair(1), pair(2)], TIER, |_| false);
        loader.reset();
        assert_eq!(loader.inflight(), 0);
        assert_eq!(loader.plan(vec![pair(1)], TIER, |_| false).len(), 1);
    }

    #[test]
    fn target_width_is_the_card_size_not_the_original() {
        assert_eq!(target_width(200.0, 1.0), 224);
        assert_eq!(target_width(200.0, 1.5), 320);
        // Never absurdly small or large.
        assert_eq!(target_width(10.0, 1.0), 128);
        assert_eq!(target_width(4000.0, 2.0), 640);
    }

    #[test]
    fn tier_up_rounds_to_the_step_and_never_down() {
        assert_eq!(tier_up(32), 32);
        assert_eq!(tier_up(33), 64);
        assert_eq!(tier_up(320), 320);
        assert_eq!(tier_up(321), 352);
        for physical in 1..=1024 {
            let tier = tier_up(physical);
            assert!(tier >= physical);
            assert!(tier - physical < TIER_STEP);
            assert_eq!(tier % TIER_STEP, 0);
        }
    }

    // ------------------------------------------------- target / re-decode

    #[test]
    fn needs_redecode_at_the_tier_boundary() {
        // Same tier: nothing to do.
        assert!(!needs_redecode(320, 320));
        // The box got smaller, or stayed inside the same tier: the wider bitmap
        // is drawn down, so nothing is re-requested.
        assert!(!needs_redecode(384, 320));
        assert!(!needs_redecode(352, 320));
        // One tier up - the section-12i case: a 320px decode in a 352px box.
        assert!(needs_redecode(320, 352));
        // Two tiers up, and absurd distances.
        assert!(needs_redecode(320, 384));
        assert!(needs_redecode(128, 640));
    }

    #[test]
    fn a_request_carries_the_tier_it_was_planned_for_through_plan() {
        let mut loader = CoverLoader::new();
        let requests = loader.plan(vec![pair(1), pair(2)], 352, |_| false);
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.target == 352));
        // The tier is recorded when the decode lands, not when it is planned.
        assert_eq!(loader.tier_of("1"), None);
        loader.complete("1", 352, true);
        assert_eq!(loader.tier_of("1"), Some(352));
        assert_eq!(loader.tier_of("2"), None);
    }

    #[test]
    fn a_failed_decode_claims_no_tier() {
        let mut loader = CoverLoader::new();
        loader.plan(vec![pair(1)], 352, |_| false);
        loader.complete("1", 352, false);
        assert_eq!(loader.tier_of("1"), None);
        assert!(!loader.is_superseded("1", 128));
    }

    #[test]
    fn adopting_cache_pixels_never_lowers_what_is_known() {
        let mut loader = CoverLoader::new();
        loader.adopt("1", 384);
        loader.adopt("1", 320);
        assert_eq!(loader.tier_of("1"), Some(384));
        // A 320 decode landing now would be a step back for this card.
        assert!(loader.is_superseded("1", 320));
    }

    // ------------------------------------------------------------- replan

    /// The whole settle path, end to end: covers decoded at one tier, an
    /// over-grown box, one re-plan, and a silent second settle.
    #[test]
    fn replan_drops_covers_decoded_below_the_new_tier_once() {
        let mut loader = CoverLoader::new();
        loader.plan(vec![pair(1), pair(2)], 320, |_| false);
        loader.complete("1", 320, true);
        loader.complete("2", 320, true);

        let held = ["1", "2"];
        let replan = loader.replan(held.iter().copied(), 352);
        assert_eq!(replan.stale, vec!["1".to_string(), "2".to_string()]);
        assert_eq!(replan.restarted, 0);
        assert!(!replan.is_empty());

        // The feed drops those two and requests again at the settled tier.
        let requests = loader.plan(vec![pair(1), pair(2)], 352, |_| false);
        assert_eq!(requests.len(), 2);
        loader.complete("1", 352, true);
        loader.complete("2", 352, true);

        // One re-plan per settle: the next settle at the same tier is silent.
        assert!(loader.replan(held.iter().copied(), 352).is_empty());
    }

    #[test]
    fn replan_is_silent_when_the_width_stays_inside_one_tier() {
        let mut loader = CoverLoader::new();
        loader.plan(vec![pair(1)], 320, |_| false);
        loader.complete("1", 320, true);
        // The measured width moved, the tier did not: nothing to re-request.
        assert!(loader.replan(["1"].into_iter(), 320).is_empty());
        // And a narrower box never re-decodes.
        assert!(loader.replan(["1"].into_iter(), 288).is_empty());
    }

    #[test]
    fn replan_restarts_a_request_planned_below_the_new_tier() {
        let mut loader = CoverLoader::new();
        // In flight at 320, nothing drawn yet - the state of the first screen
        // when the measurement lands mid-decode.
        loader.plan(vec![pair(1)], 320, |_| false);
        let replan = loader.replan(std::iter::empty(), 384);
        assert_eq!(replan.restarted, 1);
        assert!(replan.stale.is_empty());
        assert!(!replan.is_empty());
        // The slot is free, so the new plan really does re-request at 384.
        let requests = loader.plan(vec![pair(1)], 384, |_| false);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].target, 384);
    }

    #[test]
    fn replan_keeps_a_request_that_is_already_at_or_above_the_new_tier() {
        let mut loader = CoverLoader::new();
        loader.plan(vec![pair(1)], 384, |_| false);
        // The box shrank into a lower tier: the in-flight 384 decode is enough.
        assert!(loader.replan(std::iter::empty(), 320).is_empty());
        assert_eq!(loader.inflight(), 1);
    }

    /// The race a re-plan deliberately creates: two decodes of one cover in the
    /// air at different tiers, and the smaller one finishing last.
    #[test]
    fn a_superseded_landing_does_not_clear_the_newer_slot() {
        let mut loader = CoverLoader::new();
        loader.plan(vec![pair(1)], 320, |_| false);
        assert_eq!(loader.replan(["1"].into_iter(), 384).restarted, 1);
        loader.plan(vec![pair(1)], 384, |_| false);

        // The 320 answer arrives after the re-plan: it must not free the 384 slot,
        // claim its tier, or be accepted by the draw path.
        loader.complete("1", 320, true);
        assert_eq!(loader.inflight(), 1);
        assert_eq!(loader.tier_of("1"), None);
        assert!(!loader.is_superseded("1", 320));

        // The 384 answer is the one that counts.
        loader.complete("1", 384, true);
        assert_eq!(loader.inflight(), 0);
        assert_eq!(loader.tier_of("1"), Some(384));
        assert!(loader.is_superseded("1", 320));
    }

    #[test]
    fn replan_collects_tiers_the_feed_no_longer_holds() {
        let mut loader = CoverLoader::new();
        loader.plan(vec![pair(1), pair(2)], 320, |_| false);
        loader.complete("1", 320, true);
        loader.complete("2", 320, true);
        // The feed pruned id 2 (a reload keeps only the top of the list).
        loader.replan(["1"].into_iter(), 320);
        assert_eq!(loader.tier_of("1"), Some(320));
        assert_eq!(loader.tier_of("2"), None);
    }

    // ------------------------------------------- the cache contract we rely on

    /// `request_covers` passes `refresh = true` for every upgrade, and this is the
    /// fact that makes that necessary: asking the cache for a *wider* decode is
    /// answered with the narrower entry it already holds ("no wider than the
    /// request" is a hit), so without a `forget` first the re-request would return
    /// the same soft pixels and record the new tier as if it had worked.
    ///
    /// The one test in this module that runs the real `ImageCache` - over a stub
    /// fetcher, so there is still no socket and no event loop beyond the runtime it
    /// needs to await.
    #[test]
    fn a_wider_decode_through_the_cache_requires_a_forget_first() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use futures::future::BoxFuture;
        use pixiv_image::{BytesFetcher, CacheConfig, ImageCache, Priority};
        use pixiv_types::PixivError;

        struct Stub {
            calls: Arc<AtomicUsize>,
            png: Arc<Vec<u8>>,
        }

        impl BytesFetcher for Stub {
            fn fetch<'a>(&'a self, _url: &'a str) -> BoxFuture<'a, Result<Vec<u8>, PixivError>> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                let png = Arc::clone(&self.png);
                Box::pin(async move { Ok((*png).clone()) })
            }
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = ImageCache::with_fetcher(
            CacheConfig::default(),
            Arc::new(Stub {
                calls: Arc::clone(&calls),
                // Comfortably wider than either tier, so both decodes are real.
                png: Arc::new(pixiv_demo::render_placeholder(600, 400, 7)),
            }),
        );
        let url = "stub://cover";

        let first = rt
            .block_on(cache.load_sized(url, Priority::Visible, Some(320)))
            .expect("decode");
        assert_eq!(first.width, 320);
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        // A wider request is answered by the narrower entry, and nothing is fetched.
        let wider = rt
            .block_on(cache.load_sized(url, Priority::Visible, Some(352)))
            .expect("decode");
        assert_eq!(wider.width, 320, "the cache serves the smaller entry");
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        // After a forget the decode really happens at the new tier.
        cache.forget(url);
        let refreshed = rt
            .block_on(cache.load_sized(url, Priority::Visible, Some(352)))
            .expect("decode");
        assert_eq!(refreshed.width, 352);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    // -------------------------------------------------- the acceptance table

    /// After a settle the decode target covers the box the card is drawn in, at
    /// every width and every scale: at least what is drawn, never below it. That
    /// is the property the re-plan exists to preserve.
    #[test]
    fn the_settled_target_never_undershoots_the_drawn_box() {
        for body in [320.0_f32, 518.0, 620.0, 698.0, 742.0, 900.0, 1254.0, 1600.0] {
            for scale in [1.0_f32, 1.25, 1.5, 2.0] {
                let card = crate::layout::GridGeometry::for_width(body).card_width;
                let drawn = (card * scale).ceil() as u32;
                let target = target_width(card, scale);
                assert!(
                    target >= drawn,
                    "body {body} at {scale}: target {target} is below the drawn box {drawn}"
                );
            }
        }
    }

    /// The decode-target-versus-drawn-card table from the round-five report, as
    /// arithmetic rather than prose so it cannot rot.
    ///
    /// `body` is what Slint reports as `PageScroll.body-width`: the page area
    /// (`window - rail`, with `on_window_width`'s 1000px breakpoint picking
    /// the rail) minus the scrollbar gutter and the page's own horizontal
    /// padding. Those three numbers are `ui/theme.slint`'s `rail-width` /
    /// `rail-width-compact` / `scrollbar-gutter` and `page-padding`; if they
    /// change, this test is the alarm that the table in the report is stale.
    #[test]
    fn decode_target_versus_drawn_card_width() {
        const RAIL: f32 = 232.0;
        const RAIL_COMPACT: f32 = 52.0;
        const GUTTER: f32 = 10.0;
        const PAGE_PADDING: f32 = 20.0 * 2.0;
        // The seed every feed starts from before Slint measures anything.
        const GUESS: f32 = 858.0;

        let body = |window: f32| {
            let rail = if window < 1000.0 { RAIL_COMPACT } else { RAIL };
            window - rail - GUTTER - PAGE_PADDING
        };
        let near = |a: f32, b: f32| (a - b).abs() < 0.01;

        // The seed's target is the same at every window size: it is a guess.
        let seed_card = crate::layout::GridGeometry::for_width(GUESS).card_width;
        assert!(near(seed_card, 202.5));
        let seed = target_width(seed_card, 1.5);
        assert_eq!(seed, 320);

        // 1024x768 at 150%: the rail is expanded (1024 is above the breakpoint).
        let geometry = crate::layout::GridGeometry::for_width(body(1024.0));
        assert_eq!(geometry.columns, 4);
        assert!(near(body(1024.0), 742.0));
        assert!(near(geometry.card_width, 173.5));
        let drawn = (geometry.card_width * 1.5).ceil() as u32;
        assert_eq!(drawn, 261);
        assert_eq!(target_width(geometry.card_width, 1.5), 288);
        // The guess over-decoded here by one tier - one of the two directions the
        // guess is wrong in, and the reason "it looks soft" was never reported at
        // every window size.
        assert!(seed > target_width(geometry.card_width, 1.5));

        // 1536x1152 at 150%.
        let geometry = crate::layout::GridGeometry::for_width(body(1536.0));
        assert_eq!(geometry.columns, 6);
        assert!(near(body(1536.0), 1254.0));
        assert!(near(geometry.card_width, 195.666_67));
        let drawn = (geometry.card_width * 1.5).ceil() as u32;
        assert_eq!(drawn, 294);
        assert_eq!(target_width(geometry.card_width, 1.5), 320);
        // Unchanged by the fix at this size: the guess happens to land on the
        // tier the settled width wants.
        assert_eq!(seed, target_width(geometry.card_width, 1.5));

        // 800x768 at 150%: rail compact, three columns. This row reproduces the
        // numbers in section 12i exactly - a 320px decode, a card of 222 logical px
        // drawn in a 333px box, magnified 1.04x - which is how the report's
        // "1024" and its "222 logical / 333 physical" pair are reconciled: only a
        // window at this width produces that card.
        let geometry = crate::layout::GridGeometry::for_width(body(800.0));
        assert_eq!(geometry.columns, 3);
        assert!(near(body(800.0), 698.0));
        assert!(near(geometry.card_width, 222.0));
        let drawn = (geometry.card_width * 1.5).ceil() as u32;
        assert_eq!(drawn, 333);
        assert_eq!(target_width(geometry.card_width, 1.5), 352);
        assert!(seed < drawn, "the seed magnifies this card");

        // 620x768 at 150%: the rail is compact, and two columns make the card
        // *wider* than the guess assumed. This is the size where the seed
        // under-decodes and the cover is magnified.
        let geometry = crate::layout::GridGeometry::for_width(body(620.0));
        assert_eq!(geometry.columns, 2);
        assert!(near(body(620.0), 518.0));
        assert!(near(geometry.card_width, 251.0));
        let drawn = (geometry.card_width * 1.5).ceil() as u32;
        assert_eq!(drawn, 377);
        assert_eq!(target_width(geometry.card_width, 1.5), 384);
        assert!(seed < drawn, "the guess under-decodes the drawn box here");
    }
}
