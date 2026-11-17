//! Zoom / pan arithmetic for the artwork viewer.
//!
//! This module owns every transform the viewer applies, so the Slint page can stay
//! a pure painter and the maths can be tested without a window.
//!
//! Two defects this file exists to keep fixed:
//!
//! * **Wheel direction.** Slint forwards the winit delta unchanged and winit maps
//!   wheel-up to a *positive* delta-y (winit-0.30.13/
//!   src/platform_impl/windows/event_loop.rs:1727-1744: WM_MOUSEWHEEL's positive
//!   wParam >> 16 becomes LineDelta(0.0, +value), which
//!   i-slint-backend-winit-1.18.1/winitwindowadapter.rs:1472 multiplies by 60).
//!   Scrolling up must zoom in, so the delta has to *increase* the multiplier.
//!   [\`wheel_zoom\`] used to multiply by \`1.0 - delta * 0.0015\`, i.e. wheel-up
//!   shrank the picture, and its unit test asserted that inverted behaviour.
//!
//! * **Viewport geometry.** Pan was clamped against \`ui.window().size()\`, the whole
//!   window, while the image is drawn inside a viewer box that is several hundred
//!   pixels narrower and shorter (navigation rail, toolbar, info panel, thumbnail
//!   strip). Dragging was therefore free to travel far past the edges of the
//!   surface that clips it. Every function here takes the viewer's own
//!   [\`Viewport\`], and the Slint page reports it with each gesture.
//!
//! The screen space they model is
//!
//!     screen = viewport_centre + pan + content * scale
//!
//! where \`scale\` is the multiplier actually applied to the source pixels.

/// Lower and upper bounds on the zoom multiplier. 0.2 keeps a huge scan readable
/// on a small window; 8.0 is past the point where a single pixel is a screen
/// centimetre.
pub const ZOOM_MIN: f32 = 0.2;
pub const ZOOM_MAX: f32 = 8.0;

/// One wheel notch, in the units Slint reports.
///
/// A line-based notch arrives as one winit line (LineDelta(0, +-1)), which
/// \`i-slint-backend-winit\` scales by 60, so a detent is 60.
pub const WHEEL_NOTCH: f32 = 60.0;

/// Multiplier applied per wheel notch. \`1.12^+-1\` is the step WinUI's media
/// surfaces use; it is finer than a zoom button press (1.25) because a wheel
/// detent is a smaller gesture.
pub const WHEEL_STEP: f32 = 1.12;

/// How far one raw wheel event may move the multiplier, in notches.
///
/// A precision touchpad sends pixel deltas tens of units long and can deliver them
/// at 100+ Hz. Applying a per-notch step to every frame multiplies to a runaway
/// zoom, so the exponent is damped: a small delta (the usual trackpad event) keeps
/// its full proportional effect, while an outsized one is compressed. Big motions
/// still reach the bounds - the damping is on the rate, not on the reach.
pub const WHEEL_MAX_STEP: f32 = 4.0;

/// Viewer box, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

impl Viewport {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width: if width.is_finite() {
                width.max(0.0)
            } else {
                0.0
            },
            height: if height.is_finite() {
                height.max(0.0)
            } else {
                0.0
            },
        }
    }

    /// Whether the box is usable for geometry at all.
    pub fn is_usable(self) -> bool {
        self.width > 0.0 && self.height > 0.0
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
    if image_width <= 0.0
        || image_height <= 0.0
        || !image_width.is_finite()
        || !image_height.is_finite()
        || !viewport.is_usable()
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
/// Positive \`wheel_delta\` is "wheel scrolled up" in Slint's coordinate system
/// (see the module docs), and scrolling up zooms **in**.
pub fn wheel_zoom(current: f32, wheel_delta: f32) -> f32 {
    let current = if current.is_finite() { current } else { 1.0 };
    if !wheel_delta.is_finite() || wheel_delta == 0.0 {
        return current.clamp(ZOOM_MIN, ZOOM_MAX);
    }
    let notches = damp(wheel_delta / WHEEL_NOTCH, WHEEL_MAX_STEP);
    // Work in log space and clamp there: a huge delta would otherwise overflow to
    // infinity and report "no change" instead of the bound it clearly asked for.
    let stepped = current.clamp(ZOOM_MIN, ZOOM_MAX).ln() + notches * WHEEL_STEP.ln();
    stepped
        .exp()
        .clamp(ZOOM_MIN, ZOOM_MAX)
        .max(ZOOM_MIN)
        .min(ZOOM_MAX)
}

/// Pan offset that keeps the image point under \`cursor\` pinned while the scale
/// changes by \`factor\`.
///
/// Since \`screen = centre + pan + content * scale\`, pinning the content point
/// under the cursor gives \`pan' = pan + (cursor - centre - pan) * (1 - factor)\`.
pub fn anchored_pan(pan: Pan, cursor: (f32, f32), viewport: Viewport, factor: f32) -> Pan {
    if !factor.is_finite() || factor <= 0.0 || !viewport.is_usable() {
        return pan;
    }
    let (cx, cy) = viewport.centre();
    let cursor = (
        if cursor.0.is_finite() { cursor.0 } else { cx },
        if cursor.1.is_finite() { cursor.1 } else { cy },
    );
    Pan {
        x: pan.x + (cursor.0 - cx - pan.x) * (1.0 - factor),
        y: pan.y + (cursor.1 - cy - pan.y) * (1.0 - factor),
    }
}

/// Clamp the pan so the image can never be dragged completely out of view.
///
/// \`slack\` is how much of the image already extends past each edge: when the
/// image is smaller than the viewport the slack is zero and the image stays
/// centred, which is what stops a half-page scan from being lost in the void.
pub fn clamp_pan(pan: Pan, viewport: Viewport, image: (f32, f32), scale: f32) -> Pan {
    if !viewport.is_usable() {
        return Pan::default();
    }
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let drawn_w = image.0.max(0.0) * scale;
    let drawn_h = image.1.max(0.0) * scale;

    let slack_x = ((drawn_w - viewport.width) / 2.0).max(0.0);
    let slack_y = ((drawn_h - viewport.height) / 2.0).max(0.0);
    Pan {
        x: clamp_finite(pan.x, slack_x),
        y: clamp_finite(pan.y, slack_y),
    }
}

/// Soften an outsized wheel delta without capping it.
///
/// Below `soft` the value passes through unchanged, so an ordinary detent (and the
/// small deltas a precision touchpad produces) keep their exact proportional
/// response. Above it, the excess is square-rooted, which compresses a single huge
/// event - the shape a trackpad's inertia produces - while still letting a
/// deliberate scroll reach the zoom bounds.
fn damp(notches: f32, soft: f32) -> f32 {
    if !notches.is_finite() {
        return 0.0;
    }
    let magnitude = notches.abs();
    let damped = if magnitude <= soft {
        magnitude
    } else {
        soft + (magnitude - soft).sqrt()
    };
    notches.signum() * damped
}

fn clamp_finite(value: f32, slack: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-slack, slack)
    } else {
        0.0
    }
}

/// The canonical state of each viewing mode.
///
/// The two modes do not share a meaning for the multiplier: in fit mode it rides on
/// top of the fit scale, so 1.0 means "fitted"; in actual-size mode it *is* the
/// scale, so 1.0 means 1:1. Every control that switches modes therefore has to set
/// both halves, and this is the one place that says what "the other mode" is.
///
/// The readout follows the drawn scale, so it reads the fit percentage while fitted
/// and exactly 100% at actual size - which is what the button promises.
pub fn toggled_mode(fit_mode: bool) -> (bool, f32) {
    (!fit_mode, 1.0)
}

/// The transform the viewer should draw.
///
/// One struct, because scale and pan have to agree: a pan clamped against a stale
/// scale is a picture that jumps the moment the window is resized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub scale: f32,
    pub pan: Pan,
}

/// Resolve the drawn scale and a legal pan for the current state.
pub fn resolve(
    viewport: Viewport,
    image: (f32, f32),
    fit_mode: bool,
    zoom: f32,
    pan: Pan,
) -> Transform {
    let scale = effective_scale(viewport, image, fit_mode, zoom);
    Transform {
        scale,
        pan: clamp_pan(pan, viewport, image, scale),
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
        // It may exceed 1:1 for tiny images; that is fine.
        assert!(fit_scale(VIEW, 100.0, 100.0) > 1.0);
    }

    #[test]
    fn fit_scale_is_total_for_degenerate_input() {
        assert_eq!(fit_scale(VIEW, 0.0, 100.0), 1.0);
        assert_eq!(fit_scale(VIEW, 100.0, 0.0), 1.0);
        assert_eq!(fit_scale(Viewport::new(0.0, 0.0), 100.0, 100.0), 1.0);
        assert_eq!(fit_scale(VIEW, f32::NAN, 100.0), 1.0);
    }

    /// The regression this module was reopened for.
    ///
    /// The deltas asserted here are the ones the backend produces: a wheel detent
    /// arrives as LineDelta(0, +-1) and i-slint-backend-winit multiplies it by 60
    /// (winitwindowadapter.rs:1472), so Slint hands the app delta-y = +60 for
    /// "wheel up" on Windows.
    #[test]
    fn wheel_up_zooms_in_and_wheel_down_zooms_out() {
        assert!(
            wheel_zoom(1.0, 60.0) > 1.0,
            "wheel up (+60, the backend's detent) must zoom in"
        );
        assert!(
            wheel_zoom(1.0, -60.0) < 1.0,
            "wheel down (-60) must zoom out"
        );
        // Three notches in, three notches out returns to where it started.
        let mut zoom = 1.0;
        for _ in 0..3 {
            zoom = wheel_zoom(zoom, 60.0);
        }
        assert!(zoom > 1.0);
        for _ in 0..3 {
            zoom = wheel_zoom(zoom, -60.0);
        }
        assert!((zoom - 1.0).abs() < 1e-4, "symmetric, got {zoom}");
    }

    #[test]
    fn wheel_zoom_is_bounded() {
        assert_eq!(wheel_zoom(1.0, 1e9), ZOOM_MAX);
        assert_eq!(wheel_zoom(1.0, -1e9), ZOOM_MIN);
        assert_eq!(wheel_zoom(ZOOM_MAX, 60.0), ZOOM_MAX);
        assert_eq!(wheel_zoom(ZOOM_MIN, -60.0), ZOOM_MIN);
        // Garbage in, unchanged out - never NaN, never zero (a zero multiplier is
        // an invisible picture and cannot be scrolled back out of).
        assert_eq!(wheel_zoom(1.5, f32::NAN), 1.5);
        assert_eq!(wheel_zoom(1.5, 0.0), 1.5);
        assert!(wheel_zoom(1.5, f32::INFINITY).is_finite());
    }

    /// A touchpad reports pixel deltas continuously, and an inertial flick can
    /// arrive as one very large event. That must be damped relative to the raw
    /// per-notch curve - but it must still move, and it must stay symmetric.
    #[test]
    fn a_touchpad_flood_is_damped_not_multiplied_out() {
        let huge = 4000.0;
        let raw = WHEEL_STEP.powf(huge / WHEEL_NOTCH);
        let jumped = wheel_zoom(1.0, huge);
        let one_notch_up = wheel_zoom(1.0, 60.0);

        assert!(
            jumped < raw / 10.0,
            "one event moved {jumped}, undamped it would have been {raw}"
        );
        assert!(jumped >= one_notch_up, "a big up-delta still zooms in");
        assert!(jumped < ZOOM_MAX, "one event must not span the whole range");
        // Symmetric: same magnitude downward is the reciprocal.
        let down = wheel_zoom(1.0, -huge);
        assert!((down - 1.0 / jumped).abs() < 1e-4, "down {down}");
    }

    /// A normal detent and a small trackpad delta are untouched by the damping -
    /// the curve only compresses the outsized events.
    #[test]
    fn ordinary_deltas_keep_their_exact_curve() {
        for delta in [6.0f32, 30.0, 60.0, 120.0] {
            let expected = WHEEL_STEP.powf(delta / WHEEL_NOTCH);
            let got = wheel_zoom(1.0, delta);
            assert!(
                (got - expected).abs() < 1e-5,
                "delta {delta}: got {got}, expected {expected}"
            );
        }
    }

    /// A stream of small touchpad deltas slows down but still converges, so it can
    /// never pin the zoom at a bound while the fingers are still moving.
    #[test]
    fn small_touchpad_deltas_accumulate_smoothly() {
        let mut zoom = 1.0;
        for _ in 0..20 {
            zoom = wheel_zoom(zoom, 6.0);
        }
        assert!(zoom > 1.0 && zoom < ZOOM_MAX, "got {zoom}");
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

    /// The anchor has to hold for the real gesture too, not just for a hand-picked
    /// factor: wheel in at a corner, wheel back out, and the point under the cursor
    /// is where it started.
    #[test]
    fn a_wheel_in_and_out_round_trip_returns_the_same_pixel() {
        let image = (1200.0, 1600.0);
        let viewport = Viewport::new(900.0, 520.0);
        let cursor = (120.0, 80.0);

        let zoom0 = 1.0;
        let scale0 = effective_scale(viewport, image, true, zoom0);
        let pan0 = Pan { x: 0.0, y: 0.0 };
        let (cx, cy) = viewport.centre();
        let content_x = (cursor.0 - cx - pan0.x) / scale0;
        let content_y = (cursor.1 - cy - pan0.y) / scale0;

        let zoom1 = wheel_zoom(zoom0, 60.0);
        let pan1 = clamp_pan(
            anchored_pan(pan0, cursor, viewport, zoom1 / zoom0),
            viewport,
            image,
            effective_scale(viewport, image, true, zoom1),
        );

        let zoom2 = wheel_zoom(zoom1, -60.0);
        let scale2 = effective_scale(viewport, image, true, zoom2);
        let pan2 = clamp_pan(
            anchored_pan(pan1, cursor, viewport, zoom2 / zoom1),
            viewport,
            image,
            scale2,
        );

        assert!((zoom2 - zoom0).abs() < 1e-4, "zoom came back to {zoom2}");
        let check_x = cx + pan2.x + content_x * scale2;
        let check_y = cy + pan2.y + content_y * scale2;
        assert!((check_x - cursor.0).abs() < 0.5, "x came back to {check_x}");
        assert!((check_y - cursor.1).abs() < 0.5, "y came back to {check_y}");
    }

    #[test]
    fn zooming_about_the_centre_scales_the_existing_pan() {
        // The centre of the viewport is the fixed point, so whatever content point
        // was centred stays centred. In screen coordinates the offset has to grow
        // with the scale, hence pan' = pan * factor.
        let pan = Pan { x: 12.0, y: -34.0 };
        let factor = 2.0;
        let moved = anchored_pan(pan, VIEW.centre(), VIEW, factor);
        assert!((moved.x - pan.x * factor).abs() < 1e-4, "x {}", moved.x);
        assert!((moved.y - pan.y * factor).abs() < 1e-4, "y {}", moved.y);
    }

    #[test]
    fn pan_is_clamped_to_the_image_bounds() {
        let image = (1600.0, 1200.0);
        // At 1:1 the image is twice as wide and tall as the viewport, so the pan
        // may move by at most the overhang.
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

    /// The defect the old code hid behind ui.window().size().
    ///
    /// The viewer is a box inside the page, usually much smaller than the window. A
    /// pan that is legal for the window is out of bounds for the viewer, which is
    /// what let a dragged picture be lost behind the info panel and the toolbars.
    #[test]
    fn the_pan_limit_follows_the_viewer_not_the_window() {
        let image = (2000.0, 2000.0);
        let window = Viewport::new(1400.0, 900.0);
        let viewer = Viewport::new(820.0, 560.0);
        let scale = 1.0;

        let drag = Pan { x: 900.0, y: 900.0 };
        let in_window = clamp_pan(drag, window, image, scale);
        let in_viewer = clamp_pan(drag, viewer, image, scale);

        assert_eq!(in_window.x, 300.0);
        assert_eq!(in_viewer.x, 590.0);
        assert!(
            in_viewer.x > in_window.x,
            "a smaller viewport allows more travel, and stops earlier than the window"
        );
        // Both agree the image edge lands exactly on the viewport edge.
        let centre = viewer.width / 2.0;
        let edge = centre + in_viewer.x - image.0 * scale / 2.0;
        assert!((edge - 0.0).abs() < 1e-3, "left edge at {edge}");
    }

    #[test]
    fn a_degenerate_viewport_never_loses_the_picture() {
        let pan = Pan { x: 10.0, y: 10.0 };
        assert_eq!(
            clamp_pan(pan, Viewport::new(0.0, 0.0), (100.0, 100.0), 1.0),
            Pan::default()
        );
        assert_eq!(
            anchored_pan(pan, (5.0, 5.0), Viewport::new(0.0, 0.0), 2.0),
            pan
        );
        // A NaN travelling through a property chain must not produce a NaN pan.
        let clamped = clamp_pan(
            Pan {
                x: f32::NAN,
                y: 3.0,
            },
            VIEW,
            (1600.0, 1200.0),
            1.0,
        );
        assert!(clamped.x.is_finite());
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

    /// Switching modes has to land on each mode's canonical state.
    ///
    /// This is the bug the "actual size" button shipped with: it set the multiplier
    /// to 1.0 but left fit mode on, so 1.0 meant "fitted" and the button redrew the
    /// picture at 25% instead of 100%.
    #[test]
    fn switching_modes_lands_on_each_modes_canonical_state() {
        let image = (1600.0, 800.0);

        // Fit -> actual size draws at exactly 1:1.
        let (fit, zoom) = toggled_mode(true);
        assert!(!fit);
        assert_eq!(zoom, 1.0);
        assert!((effective_scale(VIEW, image, fit, zoom) - 1.0).abs() < 1e-6);

        // Actual size -> fit is fitted, i.e. the fit scale, not 1:1.
        let (fit, zoom) = toggled_mode(false);
        assert!(fit);
        assert_eq!(zoom, 1.0);
        assert!((effective_scale(VIEW, image, fit, zoom) - 0.5).abs() < 1e-6);

        // It is an involution: two toggles return the original mode.
        assert_eq!(toggled_mode(toggled_mode(true).0).0, true);
        assert_eq!(toggled_mode(toggled_mode(false).0).0, false);
    }

    #[test]
    fn resolve_reports_a_legal_pan_for_the_scale_it_draws() {
        let image = (3000.0, 2000.0);
        let out = resolve(
            VIEW,
            image,
            true,
            4.0,
            Pan {
                x: 5_000.0,
                y: 5_000.0,
            },
        );
        let fit = fit_scale(VIEW, image.0, image.1);
        assert!((out.scale - fit * 4.0).abs() < 1e-5);
        assert!(out.pan.x <= (image.0 * out.scale - VIEW.width) / 2.0 + 1e-3);
        assert!(out.pan.y <= (image.1 * out.scale - VIEW.height) / 2.0 + 1e-3);
    }
}
