//! What the user has selected on a grid page (round nine, F2).
//!
//! The model lives here from phase 0 so `App` can own one field without every
//! parallel track editing `src/app/mod.rs`. The feed track fills in the methods
//! and removes the `allow` below.

/// One grid page's selection.
///
/// `ids` keeps the work ids in **load order** (the order `self.items` holds
/// them), not in click order, because a Shift+click range is defined over what
/// the page shows. A reload clears it; an append does not.
#[derive(Default, Debug)]
pub(crate) struct Selection {
    /// Whether a plain click toggles instead of opening. Entering the mode is
    /// what Ctrl+click or Space does; leaving it clears the ids.
    pub active: bool,
    /// Selected works, in load order, without duplicates.
    pub ids: Vec<u64>,
    /// Where a Shift+click range starts, as a position in `ids`' source order.
    pub anchor: Option<usize>,
}

impl Selection {
    pub(crate) fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub(crate) fn contains(&self, id: u64) -> bool {
        self.ids.contains(&id)
    }

    /// Re-order `ids` into load order, dropping whatever the page no longer
    /// shows and any duplicate a caller could have introduced.
    ///
    /// One function owns the invariant `ids` is documented to hold, instead of
    /// every mutation re-implementing it: a Shift range taken before an append
    /// and one taken after must both read in the order the grid draws them.
    fn resort(&mut self, order: &[u64]) {
        let mut kept: Vec<u64> = Vec::with_capacity(self.ids.len());
        for id in order {
            if self.ids.contains(id) && !kept.contains(id) {
                kept.push(*id);
            }
        }
        self.ids = kept;
    }

    /// Toggle one work, keeping load order. Returns whether it is now selected.
    pub(crate) fn toggle(&mut self, id: u64, order: &[u64]) -> bool {
        if let Some(position) = self.ids.iter().position(|candidate| *candidate == id) {
            self.ids.remove(position);
            false
        } else {
            self.ids.push(id);
            self.resort(order);
            true
        }
    }

    /// Put the Shift anchor on the row that was just clicked.
    pub(crate) fn anchor_to(&mut self, id: u64, order: &[u64]) {
        self.anchor = order.iter().position(|candidate| *candidate == id);
    }

    /// Select the range between the anchor and `id`, in `order` - which is
    /// `self.items`, never the column the card happens to sit in.
    ///
    /// Returns how many works are selected afterwards. A missing anchor (the
    /// first Shift+click of a session) means "this row alone", which is what a
    /// Shift+click does in Explorer and in WinUI's ListView.
    pub(crate) fn extend_to(&mut self, id: u64, order: &[u64]) -> usize {
        let Some(target) = order.iter().position(|candidate| *candidate == id) else {
            return self.ids.len();
        };
        let start = self
            .anchor
            .unwrap_or(target)
            .min(order.len().saturating_sub(1));
        let (low, high) = if start <= target {
            (start, target)
        } else {
            (target, start)
        };
        self.ids = order[low..=high].to_vec();
        self.ids.dedup();
        self.ids.len()
    }

    /// Select everything the page has loaded.
    pub(crate) fn select_all(&mut self, order: &[u64]) -> usize {
        self.ids = order.to_vec();
        self.ids.dedup();
        self.ids.len()
    }

    /// Enter or leave the mode. Leaving clears, because a mode that kept a hidden
    /// selection would re-open with rows the user never chose again.
    pub(crate) fn set_mode(&mut self, active: bool) {
        self.active = active;
        if !active {
            self.ids.clear();
            self.anchor = None;
        }
    }

    /// A feed reload: the ids referred to a page that no longer exists.
    pub(crate) fn reset(&mut self) {
        self.set_mode(false);
    }
}

#[cfg(test)]
mod tests {
    use super::Selection;

    fn order(ids: &[u64]) -> Vec<u64> {
        ids.to_vec()
    }

    /// A range taken before an append and finished after it must cover the
    /// appended rows: the anchor is a position in `self.items`, and "load more"
    /// extends that list rather than replacing it.
    #[test]
    fn a_range_survives_an_append() {
        let mut selection = Selection::default();
        selection.set_mode(true);
        let first_page = order(&[10, 11, 12]);
        selection.toggle(11, &first_page);
        selection.anchor_to(11, &first_page);
        assert_eq!(selection.extend_to(12, &first_page), 2);
        assert_eq!(selection.ids, vec![11, 12]);

        // The second page arrives at the tail of the same list.
        let both = order(&[10, 11, 12, 13, 14]);
        assert_eq!(selection.extend_to(14, &both), 4);
        assert_eq!(selection.ids, vec![11, 12, 13, 14]);
    }

    /// Clicking a row moves the anchor, so the next Shift+click starts there
    /// instead of at whatever was clicked first.
    #[test]
    fn a_toggle_moves_the_anchor() {
        let mut selection = Selection::default();
        selection.set_mode(true);
        let page = order(&[10, 11, 12, 13]);
        selection.toggle(13, &page);
        selection.anchor_to(13, &page);
        assert_eq!(selection.extend_to(11, &page), 3);
        assert_eq!(selection.ids, vec![11, 12, 13]);

        // A plain click on 10 drops it (it was not selected) and re-anchors.
        selection.toggle(10, &page);
        selection.anchor_to(10, &page);
        assert_eq!(selection.ids, vec![10, 11, 12, 13]);
        selection.toggle(13, &page);
        assert_eq!(selection.ids, vec![10, 11, 12]);
        selection.anchor_to(10, &page);
        // From the new anchor the range is one row, not the old four.
        assert_eq!(selection.extend_to(10, &page), 1);
        assert_eq!(selection.ids, vec![10]);
    }

    /// A reload clears the mode, the ids and the anchor: the ids named a page
    /// that is gone.
    #[test]
    fn a_reload_clears_everything() {
        let mut selection = Selection::default();
        selection.set_mode(true);
        let page = order(&[10, 11, 12]);
        selection.select_all(&page);
        selection.anchor_to(11, &page);
        assert_eq!(selection.ids.len(), 3);

        selection.reset();
        assert!(!selection.active);
        assert!(selection.ids.is_empty() && selection.is_empty());
        assert_eq!(selection.anchor, None);
    }

    /// `ids` is documented as load-ordered and duplicate-free, and every
    /// mutation has to keep both halves of that.
    #[test]
    fn ids_are_unique_and_in_load_order() {
        let mut selection = Selection::default();
        selection.set_mode(true);
        let page = order(&[10, 11, 12]);

        // Clicked in reverse, stored in load order.
        selection.toggle(12, &page);
        selection.toggle(10, &page);
        assert_eq!(selection.ids, vec![10, 12]);

        // The same work twice is one entry, and the second click removes it.
        selection.toggle(12, &page);
        selection.toggle(12, &page);
        assert_eq!(selection.ids, vec![10, 12]);
        assert_eq!(selection.ids.iter().filter(|id| **id == 12).count(), 1);

        // Select-all takes the page as it is, and a repeated id in the order
        // cannot become two entries.
        assert_eq!(selection.select_all(&order(&[5, 5, 6])), 2);
        assert_eq!(selection.ids, vec![5, 6]);

        // An id the page no longer shows cannot stay selected: the range is
        // rebuilt from the page, so 5 and 6 fall away with it.
        selection.anchor_to(11, &page);
        assert_eq!(selection.extend_to(12, &page), 2);
        assert_eq!(selection.ids, vec![11, 12]);
    }

    /// Leaving the mode drops the selection instead of keeping a hidden one.
    #[test]
    fn leaving_the_mode_clears_the_ids() {
        let mut selection = Selection::default();
        selection.set_mode(true);
        selection.select_all(&order(&[1, 2, 3]));
        assert_eq!(selection.ids.len(), 3);
        selection.set_mode(false);
        assert!(selection.ids.is_empty());
        assert_eq!(selection.anchor, None);
    }
}
