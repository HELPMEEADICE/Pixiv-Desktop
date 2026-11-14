//! Ugoira playback state (round nine, F1).
//!
//! A film is decoded once at the size it is drawn and published as Slint images;
//! the timer that advances `index` is owned by the app, not by this struct, so
//! the state is plain data and can be tested without an event loop.

/// What the detail page is playing, if anything.
///
/// `slint::Image` is `!Send`, and that is why this lives in `App` (one per UI
/// thread) rather than in the image cache: frames are publishing state, not
/// cache entries.
#[allow(dead_code)]
#[derive(Default)]
pub(crate) struct PlaybackState {
    pub playing: bool,
    /// Frames at the drawn size, in order. Empty when nothing is loaded.
    pub frames: Vec<slint::Image>,
    /// Per-frame delay in milliseconds; same length as `frames`.
    pub delays: Vec<u32>,
    /// Index into `frames`, always `< frames.len()` when `frames` is non-empty.
    pub index: usize,
    /// Bumped when the page or the route changes, so a decode that lands late
    /// cannot paint into a viewer that has moved on.
    pub epoch: u64,
}
