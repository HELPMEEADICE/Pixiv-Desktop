//! Zoom / pan arithmetic for the artwork viewer.
//!
//! This used to live inline in `app.rs` and read the *window* width plus a
//! hard-coded `view_h = 520.0`, so the anchor point drifted away from the cursor
//! on every window size and panning did nothing at all. Slint now forwards the
//! viewer's own width/height with each pointer event and this module owns the
//! maths, where it can be tested without a window.

/// Lower and upper bounds on the zoom multiplier. 0.2 keeps a huge scan readable
/// on a small window; 8.0 is past the point where a single pixel is a screen
/// centimetre.
pub const ZOOM_MIN: f32 = 0.2;
pub const ZOOM_MAX: f32 = 8.0;

/// Viewer box, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

impl Viewport {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width: width.max(0.0),
            height: height.max(0.0),
        }
    }

    fn centre(self) -> (f32, f32) {
        (self.width / 2.0, self.height / 2.0)
    }
}

/// Pan offset in logical pixels, applied on top of the centred image.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pan {
    pub x: f32,
    pub y: f32,
}

/// Scale that makes an image fit entirely inside the viewport ("contain").
/// Returns 1.0 for degenerate input so callers never divide by zero.
pub fn fit_scale(viewport: Viewport, image_width: f32, image_height: f32) -> f32 {
    if image_width <= 0.0 || image_height <= 0.0 || viewport.width <= 0.0 || viewport.height <= 0.0
    {
        return 1.0;
    }
    (viewport.width / image_width).min(viewport.height / image_height)
}

/// Effective scale actually used to draw the image.
pub fn effective_scale(viewport: Viewport, image: (f32, f32), fit_mode: bool, zoom: f32) -> f32 {
    let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
    if fit_mode {
        fit_scale(viewport, image.0, image.1) * zoom
    } else {
        zoom
    }
}

/// Apply a raw wheel delta to the zoom multiplier.
///
/// A wheel notch is roughly 120 device units, and `delta / 1000` is the same
/// response curve the previous implementation used, so the feel is unchanged —
/// only the anchoring was wrong.
pub fn wheel_zoom(current: f32, wheel_delta: f32) -> f32 {
    (current * (1.0 - wheel_delta * 0.0015)).clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Pan offset that keeps the image point under `cursor` pinned while the scale
/// changes by `factor`.
///
/// `screen = centre + pan + content * scale`, so pinning the content point under
/// the cursor gives `pan' = pan + (cursor - centre - pan) * (1 - factor)`.
pub fn anchored_pan(pan: Pan, cursor: (f32, f32), viewport: Viewport, factor: f32) -> Pan {
    if !factor.is_finite() || factor <= 0.0 {
        return pan;
    }
    let (cx, cy) = viewport.centre();
    Pan {
        x: pan.x + (cursor.0 - cx - pan.x) * (1.0 - factor),
        y: pan.y + (cursor.1 - cy - pan.y) * (1.0 - factor),
    }
}

/// Clamp the pan so the image can never be dragged completely out of view.
///
/// `slack` is how much of the image already extends past each edge: when the
/// image is smaller than the viewport the slack is zero and the image stays
/// centred, which is what stops a half-page scan from being lost in the void.
pub fn clamp_pan(pan: Pan, viewport: Viewport, image: (f32, f32), scale: f32) -> Pan {
    let drawn_w = image.0 * scale;
    let drawn_h = image.1 * scale;
    let slack_x = ((drawn_w - viewport.width) / 2.0).max(0.0);
    let slack_y = ((drawn_h - viewport.height) / 2.0).max(0.0);
    Pan {
        x: pan.x.clamp(-slack_x, slack_x),
        y: pan.y.clamp(-slack_y, slack_y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: Viewport = Viewport {
        width: 800.0,
        height: 600.0,
    };

    #[test]
    fn fit_scale_contains_the_whole_image() {
        // Landscape 1600x800 in an 800x600 box is limited by width.
        assert!((fit_scale(VIEW, 1600.0, 800.0) - 0.5).abs() < 1e-6);
        // Portrait 800x1600 is limited by height.
        assert!((fit_scale(VIEW, 800.0, 1600.0) - 0.375).abs() < 1e-6);
        // Never larger than 1:1? It may be, for tiny images; that is fine.
        assert!(fit_scale(VIEW, 100.0, 100.0) > 1.0);
    }

    #[test]
    fn fit_scale_is_total_for_degenerate_input() {
        assert_eq!(fit_scale(VIEW, 0.0, 100.0), 1.0);
        assert_eq!(fit_scale(VIEW, 100.0, 0.0), 1.0);
        assert_eq!(fit_scale(Viewport::new(0.0, 0.0), 100.0, 100.0), 1.0);
    }

    #[test]
    fn wheel_zoom_is_bounded_and_directional() {
        assert!(wheel_zoom(1.0, -120.0) > 1.0, "wheel up zooms in");
        assert!(wheel_zoom(1.0, 120.0) < 1.0, "wheel down zooms out");
        assert_eq!(wheel_zoom(1.0, 1e9), ZOOM_MIN);
        assert_eq!(wheel_zoom(1.0, -1e9), ZOOM_MAX);
    }

    #[test]
    fn anchored_zoom_keeps_the_point_under_the_cursor_still() {
        let image = (1600.0, 1200.0);
        let start_zoom = 1.0;
        let scale = effective_scale(VIEW, image, false, start_zoom);
        let pan = Pan { x: 40.0, y: -25.0 };
        let cursor = (620.0, 180.0);

        // Which image pixel is under the cursor before the zoom?
        let (cx, cy) = VIEW.centre();
        let content_x = (cursor.0 - cx - pan.x) / scale;
        let content_y = (cursor.1 - cy - pan.y) / scale;

        let factor = 1.7;
        let moved = anchored_pan(pan, cursor, VIEW, factor);
        let new_scale = scale * factor;

        // ... and after. It must be the same pixel.
        let check_x = cx + moved.x + content_x * new_scale;
        let check_y = cy + moved.y + content_y * new_scale;
        assert!((check_x - cursor.0).abs() < 1e-3, "x drifted to {check_x}");
        assert!((check_y - cursor.1).abs() < 1e-3, "y drifted to {check_y}");
    }

    #[test]
    fn zooming_about_the_centre_scales_the_existing_pan() {
        // The centre of the viewport is the fixed point, so whatever content point
        // was centred stays centred. In screen coordinates the offset has to
        // grow with the scale, hence pan' = pan * factor — not pan' = pan.
        let pan = Pan { x: 12.0, y: -34.0 };
        let factor = 2.0;
        let moved = anchored_pan(pan, VIEW.centre(), VIEW, factor);
        assert!((moved.x - pan.x * factor).abs() < 1e-4, "x {}", moved.x);
        assert!((moved.y - pan.y * factor).abs() < 1e-4, "y {}", moved.y);
    }

    #[test]
    fn pan_is_clamped_to_the_image_bounds() {
        let image = (1600.0, 1200.0);
        // At 1:1 the image is twice as wide and tall as the viewport, so the
        // pan may move by at most the overhang.
        let clamped = clamp_pan(
            Pan {
                x: 9_999.0,
                y: -9_999.0,
            },
            VIEW,
            image,
            1.0,
        );
        assert_eq!(clamped.x, 400.0);
        assert_eq!(clamped.y, -300.0);

        // When the image fits, there is nothing to pan.
        let fitted = clamp_pan(Pan { x: 50.0, y: 50.0 }, VIEW, image, 0.1);
        assert_eq!(fitted, Pan::default());
    }

    #[test]
    fn effective_scale_matches_fit_times_zoom() {
        let image = (1600.0, 800.0);
        let fit = effective_scale(VIEW, image, true, 1.0);
        assert!((fit - 0.5).abs() < 1e-6);
        assert!((effective_scale(VIEW, image, true, 2.0) - 1.0).abs() < 1e-6);
        assert!((effective_scale(VIEW, image, false, 1.0) - 1.0).abs() < 1e-6);
        // Zoom is clamped before use.
        assert_eq!(effective_scale(VIEW, image, false, 999.0), ZOOM_MAX);
    }
}
