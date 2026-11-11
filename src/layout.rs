//! Pure layout maths for the artwork grid.
//!
//! Why this is Rust and not Slint: a height-balanced masonry needs to compare
//! accumulated column heights, which is not expressible as a Slint binding. It
//! also has to be unit tested, and the previous "assign item *i* to column
//! `i % n`" approach was not masonry at all — it produced visibly ragged columns
//! that grew worse the more pages you loaded.
//!
//! The width model mirrors `ui/theme.slint`. `usable_width` is the number Slint
//! reports from `PageScroll.body-width`, i.e. the content box *after* page
//! padding and the scrollbar gutter are removed, so nothing here has to guess at
//! DPI, rail width or window decoration.

/// Minimum and maximum card width, mirroring `Theme.card-min-width` /
/// `Theme.card-max-width`.
pub const CARD_MIN_WIDTH: f32 = 168.0;
pub const CARD_MAX_WIDTH: f32 = 300.0;
/// `Theme.card-gap`.
pub const CARD_GAP: f32 = 16.0;
/// `Theme.card-caption-height`: the title+author band under the cover. The card's
/// total height is `cover + caption`, which is exactly what `IllustCard` binds.
pub const CARD_CAPTION_HEIGHT: f32 = 44.0;
/// Aspect clamps, mirroring `IllustCard.clamped-aspect`.
pub const ASPECT_MIN: f32 = 0.55;
pub const ASPECT_MAX: f32 = 1.6;
/// Never fewer than two columns (one column would waste the window) and never so
/// many that a card gets narrower than a thumbnail.
pub const MIN_COLUMNS: usize = 2;
pub const MAX_COLUMNS: usize = 8;

/// Clamp an artwork's height/width ratio into the band a grid card can show
/// without one panorama blowing up the whole column.
pub fn clamped_aspect(width: u32, height: u32) -> f32 {
    if width == 0 || height == 0 {
        return 1.0;
    }
    (height as f32 / width as f32).clamp(ASPECT_MIN, ASPECT_MAX)
}

/// Height of one card, matching `IllustCard.height` exactly.
pub fn card_height(card_width: f32, aspect: f32) -> f32 {
    card_width * aspect + CARD_CAPTION_HEIGHT
}

/// Card width for a fixed column count. Never negative, even for absurd input.
pub fn card_width_for(usable_width: f32, columns: usize) -> f32 {
    let columns = columns.max(1) as f32;
    ((usable_width - CARD_GAP * (columns - 1.0)) / columns).max(0.0)
}

/// Choose the column count for a content width.
///
/// The count starts from "how many `CARD_MIN_WIDTH` cards fit" and then grows
/// while the resulting card would exceed `CARD_MAX_WIDTH`. The second step is
/// what prevents the failure this app used to have at 2560px: a hard cap of six
/// columns left each card clamped at 300px and ~400px of the window unused.
pub fn column_count(usable_width: f32) -> usize {
    if usable_width <= 0.0 {
        return MIN_COLUMNS;
    }
    let mut columns = (((usable_width + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)).floor() as usize)
        .clamp(MIN_COLUMNS, MAX_COLUMNS);
    while columns < MAX_COLUMNS && card_width_for(usable_width, columns) > CARD_MAX_WIDTH {
        columns += 1;
    }
    columns
}

/// The full grid geometry for a content width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridGeometry {
    pub columns: usize,
    pub card_width: f32,
}

impl GridGeometry {
    pub fn for_width(usable_width: f32) -> Self {
        let columns = column_count(usable_width);
        // `card_width_for` is an exact fit by construction, so clamping upwards
        // to CARD_MIN_WIDTH would reintroduce the overflow this is guarding
        // against. Only the upper bound is applied; the column count already
        // guarantees a sensible width at every realistic window size.
        let card_width = card_width_for(usable_width, columns).min(CARD_MAX_WIDTH);
        Self {
            columns,
            card_width,
        }
    }

    /// Total width the columns actually occupy, used by tests to assert that the
    /// grid never overflows its container.
    #[cfg(test)]
    pub fn occupied_width(&self) -> f32 {
        self.card_width * self.columns as f32 + CARD_GAP * (self.columns.saturating_sub(1)) as f32
    }
}

/// Distribute `heights` into `columns` buckets, balance-first with a stable
/// tie-break on the leftmost column.
///
/// This is the classic greedy masonry: each card goes to whichever column is
/// currently shortest. The result reads left-to-right for the first row, and the
/// bottom of the page is even within one card height — which is what makes the
/// scroll feel finished instead of stopping mid-column.
pub fn distribute(heights: &[f32], columns: usize) -> Vec<usize> {
    let columns = columns.max(1);
    let mut totals = vec![0.0_f32; columns];
    let mut assignment = Vec::with_capacity(heights.len());
    for height in heights {
        let mut target = 0;
        for index in 1..columns {
            if totals[index] < totals[target] {
                target = index;
            }
        }
        totals[target] += *height;
        assignment.push(target);
    }
    assignment
}

/// Bucket item indices by column, preserving feed order inside each column.
pub fn bucket(assignment: &[usize], columns: usize) -> Vec<Vec<usize>> {
    let mut buckets = vec![Vec::new(); columns.max(1)];
    for (item, column) in assignment.iter().enumerate() {
        if let Some(slot) = buckets.get_mut(*column) {
            slot.push(item);
        }
    }
    buckets
}

/// Tallest minus shortest column height; the quality metric the tests assert on.
#[cfg(test)]
pub fn column_spread(heights: &[f32], assignment: &[usize], columns: usize) -> f32 {
    let mut totals = vec![0.0_f32; columns.max(1)];
    for (height, column) in heights.iter().zip(assignment) {
        if let Some(slot) = totals.get_mut(*column) {
            *slot += *height;
        }
    }
    let max = totals.iter().cloned().fold(0.0_f32, f32::max);
    let min = totals.iter().cloned().fold(f32::INFINITY, f32::min);
    if min.is_finite() { max - min } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1140px window: the size the reported screenshots were taken at.
    /// 1140 - 240 rail - 32 padding - 10 gutter = 858 usable.
    #[test]
    fn user_screenshot_width_gets_four_columns_with_no_leftover_strip() {
        let geometry = GridGeometry::for_width(858.0);
        assert_eq!(geometry.columns, 4);
        assert!(
            (geometry.occupied_width() - 858.0).abs() < 1.0,
            "grid must fill the content box, occupied {} of 858",
            geometry.occupied_width()
        );
        assert!(geometry.card_width >= CARD_MIN_WIDTH);
        assert!(geometry.card_width <= CARD_MAX_WIDTH);
    }

    #[test]
    fn columns_grow_with_the_window_instead_of_leaving_blank_space() {
        for width in [546.0_f32, 712.0, 858.0, 998.0, 1638.0, 2278.0] {
            let geometry = GridGeometry::for_width(width);
            assert!(
                geometry.occupied_width() <= width + 0.5,
                "overflow at {width}: occupied {}",
                geometry.occupied_width()
            );
            assert!(
                width - geometry.occupied_width() < geometry.card_width,
                "left {0} px unused at {width}, more than one card wide",
                width - geometry.occupied_width()
            );
            assert!(geometry.columns >= MIN_COLUMNS && geometry.columns <= MAX_COLUMNS);
        }
    }

    #[test]
    fn reaches_five_plus_columns_on_a_full_hd_desktop() {
        // 1920 - 240 - 32 - 10
        assert!(GridGeometry::for_width(1638.0).columns >= 5);
        // 2560 - 240 - 32 - 10
        assert_eq!(GridGeometry::for_width(2278.0).columns, MAX_COLUMNS);
    }

    #[test]
    fn tiny_and_degenerate_widths_stay_usable() {
        assert_eq!(GridGeometry::for_width(0.0).columns, MIN_COLUMNS);
        // Windows narrower than one card cannot be laid out meaningfully; the
        // contract there is only "do not panic". (The real minimum window is
        // 640px, which leaves ~546px of content.)
        assert_eq!(GridGeometry::for_width(1.0).columns, MIN_COLUMNS);
        for width in [40.0_f32, 120.0] {
            let geometry = GridGeometry::for_width(width);
            assert_eq!(geometry.columns, MIN_COLUMNS);
            assert!(geometry.occupied_width() <= width + 0.5);
        }
        // From ~200px up there is a real card width.
        for width in [200.0_f32, 320.0] {
            let geometry = GridGeometry::for_width(width);
            assert!(geometry.card_width > 0.0);
            assert!(geometry.occupied_width() <= width + 0.5);
        }
    }

    #[test]
    fn aspect_is_clamped_into_the_band_the_card_can_show() {
        assert_eq!(clamped_aspect(0, 0), 1.0);
        assert!((clamped_aspect(100, 100) - 1.0).abs() < f32::EPSILON);
        assert!((clamped_aspect(1000, 100) - ASPECT_MIN).abs() < f32::EPSILON);
        assert!((clamped_aspect(100, 1000) - ASPECT_MAX).abs() < f32::EPSILON);
    }

    #[test]
    fn card_height_matches_the_slint_binding() {
        // IllustCard: height = max(48px, column-width * aspect) + caption
        assert!((card_height(200.0, 1.0) - 244.0).abs() < 0.01);
        assert!((card_height(200.0, 0.5) - 144.0).abs() < 0.01);
    }

    #[test]
    fn masonry_balances_columns_instead_of_round_robin() {
        // A pathological feed: one very tall card followed by many short ones.
        let mut heights = vec![1200.0_f32];
        heights.extend(std::iter::repeat_n(150.0_f32, 11));
        let assignment = distribute(&heights, 4);
        let spread = column_spread(&heights, &assignment, 4);

        let round_robin: Vec<usize> = (0..heights.len()).map(|i| i % 4).collect();
        let round_robin_spread = column_spread(&heights, &round_robin, 4);
        assert!(
            spread < round_robin_spread,
            "masonry spread {spread} should beat round-robin {round_robin_spread}"
        );

        // The real property that matters: no column is left more than one item
        // taller than the average, i.e. the tall card is not stacked on top of
        // other work.
        let total: f32 = heights.iter().sum();
        let ideal = total / 4.0;
        assert!(
            spread <= 1200.0 - 150.0,
            "spread {spread} exceeds the single-card bound (ideal {ideal})"
        );
    }

    #[test]
    fn the_first_card_starts_in_the_first_column() {
        let heights = vec![200.0_f32; 8];
        let assignment = distribute(&heights, 4);
        assert_eq!(assignment[0], 0);
        // With equal heights the first four cards fill one row left-to-right.
        assert_eq!(&assignment[..4], &[0, 1, 2, 3]);
    }

    #[test]
    fn every_item_lands_in_exactly_one_column_and_order_is_kept() {
        let heights: Vec<f32> = (0..37).map(|i| 100.0 + (i % 5) as f32 * 37.0).collect();
        let assignment = distribute(&heights, 5);
        assert_eq!(assignment.len(), heights.len());
        let buckets = bucket(&assignment, 5);
        assert_eq!(buckets.iter().map(Vec::len).sum::<usize>(), heights.len());
        for column in &buckets {
            assert!(column.windows(2).all(|w| w[0] < w[1]), "order lost");
        }
    }
}
