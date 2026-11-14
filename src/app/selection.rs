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
#[allow(dead_code)]
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
