//! In-memory pixel tier: a byte-budgeted LRU keyed by URL.
//!
//! The grid probes this once per visible card per frame, so the hot path must
//! not allocate. Entries live in a slab and recency is an intrusive doubly
//! linked list over slab indices: a hit is one hash lookup plus four pointer
//! writes, an eviction is one list splice, and nothing ever grows per touch.
//!
//! Each URL owns up to two slots — the uncapped pixels from [`crate::ImageCache::load`]
//! and one downscaled variant from [`crate::ImageCache::load_sized`] — so a grid
//! card and a full-screen viewer sharing a URL do not evict each other.
//!
//! Two selection rules live here, and they answer different questions.
//! `MemoryTier::get` is the *load* path: the caller is about to store a result,
//! so pixels already no wider than the request are a usable hit, and the entry
//! it returns is never upscaled. `MemoryTier::probe_best` is the *draw* path:
//! it asks whether what is cached is big enough for the box it is about to fill,
//! and reports a shortfall instead of hiding it behind a clean hit.

use std::collections::HashMap;
use std::sync::Arc;

use crate::DecodedImage;

/// Which flavour of a URL an entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variant {
    /// Inserted by `load`: pixels keep the source resolution.
    Full,
    /// Inserted by `load_sized`: pixels were capped at a target display width.
    Sized,
}

struct Slot {
    key: Arc<str>,
    variant: Variant,
    /// Width the pixels were capped at; `u32::MAX` for [`Variant::Full`].
    target_width: u32,
    image: Arc<DecodedImage>,
    prev: Option<usize>,
    next: Option<usize>,
}

#[derive(Default)]
struct Bucket {
    full: Option<usize>,
    sized: Option<usize>,
}

/// Byte-budgeted LRU over decoded pixels.
pub(crate) struct MemoryTier {
    budget: usize,
    bytes: usize,
    count: usize,
    slots: Vec<Option<Slot>>,
    free: Vec<usize>,
    index: HashMap<Arc<str>, Bucket>,
    /// Most recently used.
    head: Option<usize>,
    /// Least recently used.
    tail: Option<usize>,
}

impl MemoryTier {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            budget,
            bytes: 0,
            count: 0,
            slots: Vec::new(),
            free: Vec::new(),
            index: HashMap::new(),
            head: None,
            tail: None,
        }
    }

    /// Current decoded pixel bytes held.
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Number of live entries.
    pub(crate) fn len(&self) -> usize {
        self.count
    }

    /// Probe and mark as used, strict about resolution.
    ///
    /// `want == None` only accepts the uncapped entry, because `load` promises
    /// full-resolution pixels; `want == Some(width)` only accepts pixels that
    /// are already at most `width` wide (or were produced for exactly that
    /// width). A miss means the caller must fetch rather than upscale.
    pub(crate) fn get(&mut self, url: &str, want: Option<u32>) -> Option<Arc<DecodedImage>> {
        self.probe(url, want, false)
    }

    /// Like [`MemoryTier::get`], but a `None` probe also accepts a downscaled
    /// entry — what `peek` needs while a grid frame is being built.
    pub(crate) fn get_any(&mut self, url: &str, want: Option<u32>) -> Option<Arc<DecodedImage>> {
        self.probe(url, want, true)
    }

    fn probe(&mut self, url: &str, want: Option<u32>, lenient: bool) -> Option<Arc<DecodedImage>> {
        let idx = {
            let bucket = self.index.get(url)?;
            match want {
                None if lenient => bucket.full.or(bucket.sized),
                None => bucket.full,
                Some(width) => Self::pick(bucket, &self.slots, width),
            }
        }?;
        let image = Arc::clone(&self.slots.get(idx)?.as_ref()?.image);
        self.touch(idx);
        Some(image)
    }

    fn pick(bucket: &Bucket, slots: &[Option<Slot>], width: u32) -> Option<usize> {
        let fits = |idx: usize| -> bool {
            slots
                .get(idx)
                .and_then(Option::as_ref)
                .is_some_and(|slot| slot.image.width <= width || slot.target_width == width)
        };
        bucket
            .sized
            .filter(|idx| fits(*idx))
            .or_else(|| bucket.full.filter(|idx| fits(*idx)))
    }

    /// Probe for the best entry a `width`-wide box deserves, reporting a shortfall.
    ///
    /// Unlike `get`, a shortfall is a *fallback*, not a hit: the pixels come back
    /// with `needs_upgrade` set, so the caller can schedule a decode at the right
    /// size instead of magnifying a thumbnail for the life of the entry. Like
    /// `get` it refreshes recency, and like `pick` it compares at most two slots,
    /// so it never allocates.
    pub(crate) fn probe_best(
        &mut self,
        url: &str,
        width: u32,
    ) -> Option<(Arc<DecodedImage>, bool)> {
        let (idx, needs_upgrade) = {
            let bucket = self.index.get(url)?;
            Self::best_fit(bucket, &self.slots, width)?
        };
        let image = Arc::clone(&self.slots.get(idx)?.as_ref()?.image);
        self.touch(idx);
        Some((image, needs_upgrade))
    }

    /// The draw-path selection rule: which entry serves a `width`-wide box, and
    /// is it actually big enough?
    ///
    /// Three outcomes, in order:
    ///
    /// 1. The *smallest* entry at least `width` wide. A clean hit: it is already
    ///    big enough, so it is neither upscaled nor upgraded.
    /// 2. Otherwise the largest entry, but only when it reaches three quarters of
    ///    `width`. A 320 px entry in a 352 px box is 91% of the box, which the eye
    ///    forgives for the frame a decode takes — but the caller is told, because
    ///    the entry will never grow on its own.
    /// 3. Otherwise nothing. A 280 px entry in a 400 px box is 70%, and a briefly
    ///    empty card beats a blurry one: the caller should decode at 400.
    fn best_fit(bucket: &Bucket, slots: &[Option<Slot>], width: u32) -> Option<(usize, bool)> {
        // At most two candidates per URL, so this is a pair of comparisons and
        // never touches the allocator.
        let mut reaching: Option<(usize, u32)> = None;
        let mut largest: Option<(usize, u32)> = None;
        let mut source_is_small = false;
        for idx in [bucket.sized, bucket.full].into_iter().flatten() {
            let Some(slot) = slots.get(idx).and_then(Option::as_ref) else {
                continue;
            };
            let natural = slot.image.width;
            if natural >= width {
                if reaching.is_none_or(|(_, best)| natural < best) {
                    reaching = Some((idx, natural));
                }
            } else {
                if slot.variant == Variant::Full {
                    // Uncapped pixels narrower than the request: the artwork
                    // itself is that small, so no decode can reach `width`.
                    source_is_small = true;
                }
                if largest.is_none_or(|(_, best)| natural > best) {
                    largest = Some((idx, natural));
                }
            }
        }

        if let Some((idx, _)) = reaching {
            return Some((idx, false));
        }
        let (idx, natural) = largest?;
        // Cross-multiplied rather than divided: no float on the hot path, and the
        // boundary lands exactly on three quarters instead of a rounded 0.749.
        if u64::from(natural) * 4 < u64::from(width) * 3 {
            return None;
        }
        // `source_is_small` outranks the variant. A Full entry of 300 px means a
        // 352 px decode would come back at 300 as well; only a Sized entry left
        // short of its target has a wider source to go and get.
        let upgrade = !source_is_small
            && slots
                .get(idx)
                .and_then(Option::as_ref)
                .is_some_and(|slot| slot.variant == Variant::Sized && slot.target_width < width);
        Some((idx, upgrade))
    }

    /// Insert (or replace) one flavour of `url` and enforce the budget.
    ///
    /// Returns `false` when the image alone exceeds the whole budget, in which
    /// case it is handed to the caller but never cached.
    pub(crate) fn insert(
        &mut self,
        url: &str,
        variant: Variant,
        target_width: u32,
        image: Arc<DecodedImage>,
    ) -> bool {
        let bytes = image.byte_size();
        if bytes > self.budget {
            return false;
        }
        let existing = self.index.get(url).and_then(|bucket| match variant {
            Variant::Full => bucket.full,
            Variant::Sized => bucket.sized,
        });
        if let Some(old) = existing {
            self.remove_slot(old);
        }
        let key: Arc<str> = Arc::from(url);
        let slot = Slot {
            key: Arc::clone(&key),
            variant,
            target_width,
            image,
            prev: None,
            next: None,
        };
        let idx = match self.free.pop() {
            Some(idx) => {
                self.slots[idx] = Some(slot);
                idx
            }
            None => {
                self.slots.push(Some(slot));
                self.slots.len() - 1
            }
        };
        {
            let bucket = self.index.entry(key).or_default();
            match variant {
                Variant::Full => bucket.full = Some(idx),
                Variant::Sized => bucket.sized = Some(idx),
            }
        }
        self.push_front(idx);
        self.count += 1;
        self.bytes += bytes;
        self.prune();
        true
    }

    /// Drop every entry for `url`, returning how many were removed.
    pub(crate) fn remove(&mut self, url: &str) -> usize {
        let Some(bucket) = self.index.get(url) else {
            return 0;
        };
        let targets: Vec<usize> = [bucket.full, bucket.sized].into_iter().flatten().collect();
        for idx in &targets {
            self.remove_slot(*idx);
        }
        targets.len()
    }

    /// Evict least-recently-used entries until the budget holds.
    pub(crate) fn prune(&mut self) {
        while self.bytes > self.budget {
            let Some(victim) = self.tail else { break };
            self.remove_slot(victim);
        }
    }

    #[cfg(test)]
    pub(crate) fn force_budget(&mut self, budget: usize) {
        self.budget = budget;
    }

    fn remove_slot(&mut self, idx: usize) {
        if self.slots.get(idx).and_then(Option::as_ref).is_none() {
            return;
        }
        self.unlink(idx);
        let Some(slot) = self.slots.get_mut(idx).and_then(Option::take) else {
            return;
        };
        self.bytes = self.bytes.saturating_sub(slot.image.byte_size());
        self.count = self.count.saturating_sub(1);
        self.free.push(idx);
        let mut empty = false;
        if let Some(bucket) = self.index.get_mut(slot.key.as_ref()) {
            let target = match slot.variant {
                Variant::Full => &mut bucket.full,
                Variant::Sized => &mut bucket.sized,
            };
            if *target == Some(idx) {
                *target = None;
            }
            empty = bucket.full.is_none() && bucket.sized.is_none();
        }
        if empty {
            self.index.remove(slot.key.as_ref());
        }
    }

    fn push_front(&mut self, idx: usize) {
        let old_head = self.head;
        if let Some(slot) = self.slots[idx].as_mut() {
            slot.prev = None;
            slot.next = old_head;
        }
        if let Some(head) = old_head {
            if let Some(slot) = self.slots[head].as_mut() {
                slot.prev = Some(idx);
            }
        }
        self.head = Some(idx);
        if self.tail.is_none() {
            self.tail = Some(idx);
        }
    }

    fn unlink(&mut self, idx: usize) {
        let Some((prev, next)) = self
            .slots
            .get(idx)
            .and_then(Option::as_ref)
            .map(|slot| (slot.prev, slot.next))
        else {
            return;
        };
        match prev {
            Some(prev) => {
                if let Some(slot) = self.slots[prev].as_mut() {
                    slot.next = next;
                }
            }
            None => self.head = next,
        }
        match next {
            Some(next) => {
                if let Some(slot) = self.slots[next].as_mut() {
                    slot.prev = prev;
                }
            }
            None => self.tail = prev,
        }
    }

    fn touch(&mut self, idx: usize) {
        if self.head == Some(idx) {
            return;
        }
        self.unlink(idx);
        self.push_front(idx);
    }
}

#[cfg(test)]
mod tests {
    use super::{MemoryTier, Variant};
    use crate::DecodedImage;
    use std::sync::Arc;

    fn image(width: u32, height: u32) -> Arc<DecodedImage> {
        Arc::new(DecodedImage {
            width,
            height,
            rgba: vec![0u8; (width * height * 4) as usize].into(),
        })
    }

    #[test]
    fn eviction_takes_the_least_recently_used_entry() {
        let mut tier = MemoryTier::new(3 * 400);
        assert!(tier.insert("a", Variant::Full, u32::MAX, image(10, 10)));
        assert!(tier.insert("b", Variant::Full, u32::MAX, image(10, 10)));
        assert!(tier.insert("c", Variant::Full, u32::MAX, image(10, 10)));
        // Touch "a" so "b" becomes the victim.
        assert!(tier.get("a", None).is_some());
        assert!(tier.insert("d", Variant::Full, u32::MAX, image(10, 10)));
        assert_eq!(tier.bytes(), 3 * 400);
        assert!(tier.get("b", None).is_none());
        assert!(tier.get("a", None).is_some());
        assert_eq!(tier.len(), 3);
    }

    #[test]
    fn shrinking_the_budget_lets_prune_catch_up() {
        let mut tier = MemoryTier::new(1000);
        assert!(tier.insert("a", Variant::Full, u32::MAX, image(10, 10)));
        assert!(tier.insert("b", Variant::Full, u32::MAX, image(10, 10)));
        assert_eq!(tier.bytes(), 800);
        tier.force_budget(400);
        tier.prune();
        assert_eq!(tier.bytes(), 400);
        assert!(tier.get("a", None).is_none());
        assert!(tier.get("b", None).is_some());
    }

    #[test]
    fn an_image_larger_than_the_budget_is_never_cached() {
        let mut tier = MemoryTier::new(100);
        assert!(!tier.insert("a", Variant::Full, u32::MAX, image(10, 10)));
        assert_eq!(tier.len(), 0);
        assert_eq!(tier.bytes(), 0);
    }

    #[test]
    fn both_flavours_of_one_url_coexist() {
        let mut tier = MemoryTier::new(100_000);
        assert!(tier.insert("a", Variant::Full, u32::MAX, image(20, 20)));
        assert_eq!(tier.get("a", None).map(|i| i.width), Some(20));
        assert_eq!(tier.get_any("a", None).map(|i| i.width), Some(20));
        // 20px pixels cannot fill an 8px card without a fresh downscale.
        assert!(tier.get("a", Some(8)).is_none());

        assert!(tier.insert("a", Variant::Sized, 8, image(8, 8)));
        assert_eq!(tier.len(), 2);
        assert_eq!(tier.get("a", Some(8)).map(|i| i.width), Some(8));
        // A wider target accepts the smaller entry; never the other way round.
        assert_eq!(tier.get("a", Some(64)).map(|i| i.width), Some(8));
        assert_eq!(
            tier.get("a", None).map(|i| i.width),
            Some(20),
            "an uncapped probe must never be served card pixels"
        );
        assert_eq!(tier.remove("a"), 2);
        assert_eq!(tier.len(), 0);
        assert!(tier.get_any("a", None).is_none());
    }
}
