//! Ugoira playback state (round nine, F1).
//!
//! The film is decoded once, at the size it is drawn, thinned to a publish
//! budget and converted to `slint::Image`; from then on the frame on screen is a
//! function of a monotonic clock, never of a tick counter.
//!
//! **Why the clock and not an accumulated tick.** A repeated 16 ms timer is a
//! request, not a promise: Windows delivers it late under load (a compositor
//! hitch, a decode on the same thread, another window on top), and a counter that
//! adds one frame per tick would run the animation in slow motion - silently,
//! forever, and only on slow machines. `Instant` cannot drift: the visible index
//! is `frame_index_at(now - started + offset)`, so a tick that arrives 60 ms late
//! simply draws the frame the film is actually on. It also makes pause/resume
//! exact (the offset is the wall-clock position, not a frame count), and it makes
//! the whole thing testable without an event loop: the boundary table below is a
//! pure function of elapsed milliseconds.
//!
//! The timer lives in this state because it is the only thing that must stop with
//! it: releasing the film stops the timer, and the tick self-terminates when the
//! page it belongs to is no longer on screen.

use super::*;
use pixiv_ugoira::UgoiraFilm;
use std::time::Instant;

/// Playback tick, in milliseconds. One compositor frame at 60 Hz; the index is
/// derived from the clock, so a skipped tick costs nothing.
pub(crate) const TICK_MS: u64 = 16;

/// While a film is loaded but paused the same timer keeps running slowly, only to
/// notice that the page has been left: a paused film still holds every frame's
/// bitmap, and a route change has to release them.
const PRESENCE_MS: u64 = 400;

/// Total bytes of decoded frames that may be published at once.
///
/// A 1920 px film with 200 frames is ~1.6 GB of RGBA. Publishing that is not a
/// slow animation, it is an out-of-memory crash, so frames are thinned by stride
/// until the published set fits and the InfoBar says how many are left.
pub(crate) const PUBLISH_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

/// The demo film's shape: 24 frames, 80 ms each, so two captures of the same
/// command show the same frame at the same moment.
pub(crate) const DEMO_FRAMES: usize = 24;
/// Delay of every demo frame.
pub(crate) const DEMO_DELAY_MS: u32 = 80;
/// Width the demo film is generated at. Small on purpose: it is synthetic, it is
/// only ever seen in a fixture capture, and it keeps all 24 frames inside the
/// publish budget so the capture cannot show a thinning banner.
pub(crate) const DEMO_FILM_WIDTH: u32 = 640;

/// What the detail page is playing, if anything.
///
/// `slint::Image` is `!Send`, and that is why this lives in `App` (one per UI
/// thread) rather than in the image cache: frames are publishing state, not
/// cache entries.
pub(crate) struct PlaybackState {
    /// Whether the clock is running. The button's own glyph is not the source of
    /// truth; this is.
    pub playing: bool,
    /// The frames that are drawn, in order, `stride` apart in the timeline.
    pub frames: Vec<slint::Image>,
    /// The film's *timing*. After `publish` the frames carry no pixels - the
    /// delays are what `frame_index_at` needs, and keeping the RGBA as well would
    /// double the memory the budget just bounded.
    pub film: Option<UgoiraFilm>,
    /// Index into the *timeline* (not into `frames`), so the caption counts the
    /// film's own frames even when half of them were thinned away.
    pub index: usize,
    /// Bumped when the page or the route changes, so a decode or a tick that
    /// lands late cannot paint into a viewer that has moved on.
    pub epoch: u64,
    /// When the current run of the clock started. `None` while paused.
    pub started: Option<Instant>,
    /// Wall-clock position the current run started from, in milliseconds.
    pub offset_ms: u64,
    /// How many timeline frames each published frame spans.
    pub stride: usize,
    /// The 16 ms tick. Stopped when nothing is playing.
    pub timer: slint::Timer,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            playing: false,
            frames: Vec::new(),
            film: None,
            index: 0,
            epoch: 0,
            started: None,
            offset_ms: 0,
            stride: 1,
            timer: slint::Timer::default(),
        }
    }
}

impl PlaybackState {
    /// Drop the film and stop the clock. The UI properties are the caller's job:
    /// this is the part that must also work when the window is already gone.
    pub fn reset(&mut self) {
        self.timer.stop();
        self.playing = false;
        self.frames.clear();
        self.film = None;
        self.index = 0;
        self.started = None;
        self.offset_ms = 0;
        self.stride = 1;
        // A late decode or a late tick compares against this, so releasing the
        // film also invalidates everything that was in flight for it.
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Wall-clock position of the film, in milliseconds.
    pub fn elapsed_ms(&self, now: Instant) -> u64 {
        let running = self.started.map_or(Duration::ZERO, |started| {
            now.saturating_duration_since(started)
        });
        let running = u64::try_from(running.as_millis()).unwrap_or(u64::MAX);
        self.offset_ms.saturating_add(running)
    }

    /// Whether a film is loaded and drawable.
    pub fn ready(&self) -> bool {
        !self.frames.is_empty() && self.film.is_some()
    }
}

// ---------------------------------------------------------------- budget

/// Bytes one decoded frame occupies in RGBA8.
pub(crate) fn frame_bytes(width: u32, height: u32) -> u64 {
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(4)
}

/// How many timeline frames one published frame spans.
///
/// The smallest stride whose published set fits `PUBLISH_BUDGET_BYTES`; `1`
/// when the whole film already fits. A film that cannot fit even with one frame
/// per slot returns `count` (and the caller publishes what it can draw, which is
/// the most it can honestly do).
pub(crate) fn stride_for(count: usize, bytes_per_frame: u64) -> usize {
    if count == 0 || bytes_per_frame == 0 {
        return 1;
    }
    let frames = u64::try_from(count).unwrap_or(u64::MAX);
    if bytes_per_frame.saturating_mul(frames) <= PUBLISH_BUDGET_BYTES {
        return 1;
    }
    for stride in 2..=count {
        let published = u64::try_from(count.div_ceil(stride)).unwrap_or(u64::MAX);
        if bytes_per_frame.saturating_mul(published) <= PUBLISH_BUDGET_BYTES {
            return stride;
        }
    }
    count
}

/// The timeline indices that are published for `stride`: 0, stride, 2 * stride, ...
pub(crate) fn published_indices(count: usize, stride: usize) -> Vec<usize> {
    if count == 0 {
        return Vec::new();
    }
    (0..count).step_by(stride.max(1)).collect()
}

/// Which published frame draws timeline frame `original`.
///
/// Rounded *down*: a thinned film shows the last frame it kept, not one from the
/// future, so a stride of three never draws a frame before it is due.
pub(crate) fn slot_for(original: usize, stride: usize, published: usize) -> usize {
    if published == 0 {
        return 0;
    }
    (original / stride.max(1)).min(published - 1)
}

// ---------------------------------------------------------------- demo film

/// The size the demo film is generated at, for a work of `width x height`.
pub(crate) fn demo_film_size(width: u32, height: u32) -> (u32, u32) {
    let width = if width == 0 { 3 } else { width };
    let height = if height == 0 { 4 } else { height };
    let film_width = DEMO_FILM_WIDTH.clamp(64, 1400);
    let film_height = u64::from(film_width)
        .saturating_mul(u64::from(height))
        .saturating_div(u64::from(width))
        .clamp(64, 1400);
    (film_width, u32::try_from(film_height).unwrap_or(1400))
}

/// A deterministic 24-frame film, generated from `id`.
///
/// Demo mode has no archive to download, so playback would have nothing to draw.
/// Rendering one costs a few milliseconds off the UI thread and makes the fixture
/// repeatable: the same id yields the same 24 frames, and the same seed yields the
/// same pixels in every build.
pub(crate) fn demo_film(id: Id, width: u32, height: u32) -> Option<UgoiraFilm> {
    let (film_width, film_height) = demo_film_size(width, height);
    demo_film_at(id, film_width, film_height)
}

/// [`demo_film`] at exactly this resolution.
pub(crate) fn demo_film_at(id: Id, width: u32, height: u32) -> Option<UgoiraFilm> {
    let film_width = width.clamp(64, 1400);
    let film_height = height.clamp(64, 1400);
    let mut frames = Vec::with_capacity(DEMO_FRAMES);
    for index in 0..DEMO_FRAMES {
        let seed = id
            .get()
            .wrapping_add(u64::try_from(index).unwrap_or(0).wrapping_mul(7919));
        let png = pixiv_demo::render_placeholder(film_width, film_height, seed);
        let decoded = image::load_from_memory(&png).ok()?.into_rgba8();
        if decoded.width() != film_width || decoded.height() != film_height {
            return None;
        }
        frames.push(pixiv_ugoira::FilmFrame {
            file: format!("demo-{index:02}.png"),
            delay_ms: DEMO_DELAY_MS,
            rgba: decoded.into_raw(),
        });
    }
    Some(UgoiraFilm {
        width: film_width,
        height: film_height,
        frames,
    })
}

// ---------------------------------------------------------------- the clock

/// Width the film should be decoded at: the window's width, on the same tier grid
/// and behind the same cap the still viewer uses.
///
/// `scaled_to_width` never upscales, so a small film is left alone; the cap is
/// what keeps a 4K window from decoding 24 frames at 4K.
pub(crate) fn film_width_for(ui: &AppWindow) -> u32 {
    let window = ui.window();
    let physical = window.size().width;
    let scale = window.scale_factor().max(1.0);
    let logical = physical as f32 / scale;
    crate::images::target_width_up_to(logical, scale, VIEWER_MAX_WIDTH)
}

/// The tick body. Runs on the UI thread, from the timer Slint owns.
///
/// It looks the app up in the same thread-local the rest of the app uses
/// (`APP`): capturing `Shared` in the closure would make `App` own a closure that
/// owns `App`, and the window would never be freed.
pub(crate) fn playback_tick(epoch: u64) {
    APP.with(|slot| {
        let Ok(handle) = slot.try_borrow() else {
            return;
        };
        let Some(shared) = handle.as_ref() else {
            return;
        };
        let Ok(mut app) = shared.try_borrow_mut() else {
            return;
        };
        app.advance_playback(epoch);
    });
}

impl App {
    /// Stop the clock and drop the frames, leaving the still in the viewer.
    ///
    /// Everything that ends a film's life goes through here - another work, a
    /// route change, a failed load - so the three exits cannot drift.
    pub(crate) fn release_playback(&mut self) {
        self.playback.reset();
        if let Some(ui) = self.ui.upgrade() {
            ui.set_film_playing(false);
            ui.set_film_ready(false);
            ui.set_film_frame(slint::Image::default());
            ui.set_film_index(0);
            ui.set_film_count(0);
        }
    }

    /// Publish a decoded film: thin it to the budget, convert it, show frame 0.
    pub(crate) fn publish_film(&mut self, mut film: UgoiraFilm) {
        let count = film.frames.len();
        if count == 0 {
            self.notify(3, "动图里没有任何帧，保持静帧显示");
            return;
        }
        let bytes = frame_bytes(film.width, film.height);
        let stride = stride_for(count, bytes);
        let keep = published_indices(count, stride);
        let mut frames = Vec::with_capacity(keep.len());
        for index in keep {
            // Dropped as it is converted: the published set and the decoded film
            // never both hold their full RGBA, so the peak stays near the budget
            // the stride was chosen for.
            if let Some(frame) = film.frames.get_mut(index) {
                frames.push(rgba_to_slint(film.width, film.height, &frame.rgba));
                frame.rgba = Vec::new();
            }
        }
        // Timing only from here on: `frame_index_at` needs the delays, nothing
        // needs the pixels a second time.
        for frame in &mut film.frames {
            frame.rgba = Vec::new();
        }
        let published = frames.len();
        let Some(first) = frames.first().cloned() else {
            self.notify(3, "动图里没有任何可播放的帧，保持静帧显示");
            return;
        };

        self.playback.frames = frames;
        self.playback.film = Some(film);
        self.playback.stride = stride;
        self.playback.index = 0;
        self.playback.offset_ms = 0;
        self.playback.started = None;
        self.playback.playing = false;

        {
            let ui = self.ui();
            ui.set_film_count(i32::try_from(count).unwrap_or(i32::MAX));
            ui.set_film_index(0);
            ui.set_film_frame(first);
            ui.set_film_ready(true);
            ui.set_film_playing(false);
        }
        self.set_ugoira_hint_for_playback();
        if stride > 1 {
            self.notify(
                2,
                format!(
                    "动图共 {count} 帧，超过 {} MB 的播放预算，已按 {published} 帧抽稀播放",
                    PUBLISH_BUDGET_BYTES / (1024 * 1024)
                ),
            );
        }
    }

    /// Start (or resume) the clock.
    pub(crate) fn start_playback(&mut self) {
        if !self.playback.ready() || self.playback.playing {
            return;
        }
        self.playback.playing = true;
        self.playback.started = Some(Instant::now());
        self.arm_playback(TICK_MS);
        self.ui().set_film_playing(true);
    }

    /// Stop the clock but keep the frame on screen, frozen where it was.
    pub(crate) fn pause_playback(&mut self) {
        if self.playback.playing {
            let now = Instant::now();
            self.playback.offset_ms = self.playback.elapsed_ms(now);
            self.playback.started = None;
            self.playback.playing = false;
        }
        // The slow tick is not playback: it is how a paused film notices that the
        // page it belongs to has been left, so its bitmaps can be released.
        if self.playback.ready() {
            self.arm_playback(PRESENCE_MS);
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.set_film_playing(false);
        }
    }

    /// The play/pause callback.
    ///
    /// The UI button flips `film-playing` optimistically before it calls here, so
    /// the request is read from the property and never inferred by inverting our
    /// own flag: two clicks in one frame must not leave the two disagreeing.
    pub(crate) fn toggle_playback(&mut self) {
        let want = self.ui().get_film_playing();
        if want {
            self.start_playback();
        } else {
            self.pause_playback();
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.set_film_playing(self.playback.playing);
        }
    }

    /// One tick: work out which frame the clock says, draw it if it changed.
    pub(crate) fn advance_playback(&mut self, epoch: u64) {
        if self.playback.epoch != epoch {
            self.playback.timer.stop();
            return;
        }
        if self.ui.upgrade().is_none() {
            // The window is gone; the film goes with it.
            self.playback.reset();
            return;
        }
        // Leaving the page (or the route) releases the frames. This is the one
        // exit no other in-boundary code can see, and the tick is already running,
        // so it is where the check belongs.
        if self.route.page != Destination::Detail || self.page != Destination::Detail {
            self.release_playback();
            return;
        }
        if !self.playback.playing {
            return;
        }
        let Some(film) = self.playback.film.as_ref() else {
            return;
        };
        let elapsed = self.playback.elapsed_ms(Instant::now());
        let original = film.frame_index_at(elapsed);
        if original == self.playback.index {
            return;
        }
        let slot = slot_for(original, self.playback.stride, self.playback.frames.len());
        let Some(image) = self.playback.frames.get(slot).cloned() else {
            return;
        };
        self.playback.index = original;
        let ui = self.ui();
        ui.set_film_frame(image);
        ui.set_film_index(i32::try_from(original).unwrap_or(i32::MAX));
    }

    /// Arm the single timer this state owns. Starting it again replaces the old
    /// callback, which is how the interval changes between playback and presence.
    fn arm_playback(&mut self, interval_ms: u64) {
        let epoch = self.playback.epoch;
        self.playback.timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(interval_ms),
            move || playback_tick(epoch),
        );
    }

    /// The metadata hint once playback exists: it used to say the app could not
    /// play a film at all, which is no longer true.
    pub(crate) fn set_ugoira_hint_for_playback(&self) {
        if self
            .detail
            .as_ref()
            .is_some_and(|d| d.kind == IllustKind::Ugoira)
        {
            self.ui()
                .set_detail_ugoira_hint("可直接播放；也能导出为 GIF 保存在下载目录。".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixiv_ugoira::FilmFrame;

    fn film_of(delays: &[u32]) -> UgoiraFilm {
        UgoiraFilm {
            width: 2,
            height: 2,
            frames: delays
                .iter()
                .enumerate()
                .map(|(index, delay)| FilmFrame {
                    file: format!("f{index}.png"),
                    delay_ms: *delay,
                    rgba: vec![0; 2 * 2 * 4],
                })
                .collect(),
        }
    }

    /// The boundary table the whole playback rests on: which frame is on screen
    /// at which millisecond, including the exact boundaries, the last frame and
    /// the wrap. The timer only ever asks this question, so this table *is* the
    /// animation.
    #[test]
    fn the_frame_index_follows_the_metadata_timeline() {
        // 80 + 120 + 80 + 320 = 600 ms per pass.
        let film = film_of(&[80, 120, 80, 320]);
        assert_eq!(film.total_duration_ms(), 600);
        let table = [
            (0, 0),
            (1, 0),
            (79, 0),
            (80, 1),
            (199, 1),
            (200, 2),
            (279, 2),
            (280, 3),
            (599, 3),
            (600, 0),
            (679, 0),
            (680, 1),
            (1_259, 0),
            (u64::MAX, film.frame_index_at(u64::MAX)),
        ];
        for (elapsed, expected) in table {
            assert_eq!(film.frame_index_at(elapsed), expected, "at {elapsed} ms");
        }

        // A single frame, and a film of zero-length frames, both stay put.
        let one = film_of(&[80]);
        assert_eq!(one.frame_index_at(0), 0);
        assert_eq!(one.frame_index_at(9_999), 0);
        let instant = film_of(&[0, 0]);
        assert_eq!(instant.total_duration_ms(), 0);
        assert_eq!(instant.frame_index_at(500), 0);
    }

    /// The thinning map: every published index is on the stride grid, the set
    /// fits the budget, and a slot never points past the frames published.
    #[test]
    fn a_film_over_the_budget_is_thinned_by_stride() {
        let frame = 4 * 1024 * 1024; // 4 MiB at RGBA8
        // Exactly at the budget: nothing is dropped.
        assert_eq!(stride_for(16, frame), 1);
        // 24 frames of 4 MiB is 96 MiB, so half of them go.
        let stride = stride_for(24, frame);
        assert_eq!(stride, 2);
        let published = published_indices(24, stride);
        assert_eq!(published.len(), 12);
        assert_eq!(published.first(), Some(&0));
        assert_eq!(published.last(), Some(&22));
        assert!(
            u64::try_from(published.len()).unwrap_or(0) * frame <= PUBLISH_BUDGET_BYTES,
            "the published set fits"
        );

        // A monster film keeps at least one frame.
        let huge = stride_for(200, 400 * 1024 * 1024);
        assert_eq!(published_indices(200, huge).len(), 1);

        // Degenerate inputs never divide by zero.
        assert_eq!(stride_for(0, frame), 1);
        assert_eq!(stride_for(10, 0), 1);
        assert!(published_indices(0, 3).is_empty());
        assert_eq!(slot_for(5, 3, 0), 0);
    }

    /// The slot a timeline frame maps to: rounded down, clamped, and never past
    /// the frames that were actually published.
    #[test]
    fn a_thinned_frame_draws_the_last_kept_frame_not_the_next_one() {
        let published = published_indices(24, 2).len();
        assert_eq!(published, 12);
        let table = [
            (0, 0),
            (1, 0),
            (2, 1),
            (3, 1),
            (22, 11),
            (23, 11),
            // Beyond anything published still lands on the last frame.
            (99, 11),
        ];
        for (original, expected) in table {
            assert_eq!(
                slot_for(original, 2, published),
                expected,
                "frame {original}"
            );
        }
    }

    #[test]
    fn the_demo_film_is_twenty_four_deterministic_frames_of_eighty_milliseconds() {
        let first = demo_film_at(Id(700_000_009), 64, 64).expect("demo film renders");
        assert_eq!(first.frames.len(), DEMO_FRAMES);
        assert_eq!(first.total_duration_ms(), 1_920);
        assert!(
            first
                .frames
                .iter()
                .all(|frame| frame.delay_ms == DEMO_DELAY_MS)
        );
        assert!(
            first
                .frames
                .iter()
                .all(|frame| frame.rgba.len() == 64 * 64 * 4)
        );
        // Frame content is derived from the id, so two builds of the same work
        // are the same picture - which is what makes the capture repeatable.
        let second = demo_film_at(Id(700_000_009), 64, 64).expect("demo film renders");
        assert_eq!(first.frames[0].rgba, second.frames[0].rgba);
        assert_eq!(first.frames[23].rgba, second.frames[23].rgba);
        assert_ne!(first.frames[0].rgba, first.frames[1].rgba, "frames differ");
        // And the clock walks it 80 ms at a time.
        assert_eq!(first.frame_index_at(79), 0);
        assert_eq!(first.frame_index_at(80), 1);
        assert_eq!(first.frame_index_at(1_919), 23);
        assert_eq!(first.frame_index_at(1_920), 0);
    }

    #[test]
    fn the_state_starts_paused_with_no_frames_and_releases_everything() {
        let mut state = PlaybackState::default();
        assert!(!state.ready());
        assert_eq!(state.elapsed_ms(Instant::now()), 0);
        let epoch = state.epoch;

        state.frames.push(slint::Image::default());
        state.film = Some(film_of(&[80]));
        state.playing = true;
        state.offset_ms = 480;
        assert!(state.ready());
        assert_eq!(state.elapsed_ms(Instant::now()), 480);

        state.reset();
        assert!(!state.ready());
        assert!(!state.playing);
        assert!(state.frames.is_empty());
        assert!(state.film.is_none());
        assert_eq!(state.offset_ms, 0);
        assert_eq!(state.stride, 1);
        // A late tick compares against this, so it must move.
        assert_ne!(state.epoch, epoch);
    }

    #[test]
    fn a_demo_film_keeps_the_aspect_of_the_work_it_stands_in_for() {
        let (width, height) = demo_film_size(900, 1200);
        assert_eq!(width, DEMO_FILM_WIDTH);
        assert_eq!(height, 853, "3:4 kept, 640 * 1200 / 900");
        // Degenerate dimensions fall back instead of dividing by zero.
        assert_eq!(demo_film_size(0, 0), (DEMO_FILM_WIDTH, 853));
        assert_eq!(demo_film_size(1200, 900), (DEMO_FILM_WIDTH, 480));
    }
}
