//! Procedural placeholder artwork.
//!
//! Nothing in this module reads the clock, the filesystem, the network or any
//! thread-local state: the byte stream is a pure function of `(width, height,
//! seed)`. That keeps image caches and layout snapshots stable across runs and
//! across machines, and it is why the repository can ship zero third-party
//! imagery.
//!
//! Demo Mode used to paint a two-stop gradient with a soft blob in it. At card
//! size that reads as a colour swatch, not as artwork, so the grid looked like a
//! palette instead of a Pixiv feed. The renderer now composes one of five motifs
//! — a sunset landscape, a bloom, a graphic poster, a night sky, or an abstract
//! mesh — each built from seeded shapes with a harmonised palette, a vignette and
//! film grain. Everything is still analytic per pixel: no textures, no
//! dependencies, no bundled images, and the whole frame is a pure function of the
//! seed, so the same illustration always looks the same.

use image::{ExtendedColorType, ImageEncoder};

use crate::prng::Rng;

/// Defensive ceiling applied to either dimension before any allocation.
const MAX_DIM: u32 = 4096;

/// Deterministic 24-bit fraction in `0.0..1.0`.
const FRACTION_SCALE: f32 = 16_777_216.0;

/// Which composition a seed produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Motif {
    Sunset,
    Bloom,
    Poster,
    Night,
    Drift,
}

impl Motif {
    fn from_index(index: u32) -> Self {
        match index % 5 {
            0 => Motif::Sunset,
            1 => Motif::Bloom,
            2 => Motif::Poster,
            3 => Motif::Night,
            _ => Motif::Drift,
        }
    }
}

/// An sRGB triple in `0.0..=1.0`.
type Rgb = [f32; 3];

pub(crate) fn render(width: u32, height: u32, seed: u64) -> Vec<u8> {
    let (w, h) = clamp_dims(width, height);
    let pixels = paint(w, h, seed);
    let encoded = encode_png(w, h, &pixels);
    if encoded.is_empty() {
        // Writing into a `Vec<u8>` cannot realistically fail; if it ever did, a
        // single pixel is still a valid PNG, which keeps the caller panic-free.
        return encode_png(1, 1, &[120, 132, 156]);
    }
    encoded
}

fn clamp_dims(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (1, 1);
    }
    (width.min(MAX_DIM), height.min(MAX_DIM))
}

fn encode_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let encoded = image::codecs::png::PngEncoder::new(&mut out).write_image(
        pixels,
        width,
        height,
        ExtendedColorType::Rgb8,
    );
    if encoded.is_ok() { out } else { Vec::new() }
}

// ---------------------------------------------------------------- composition

/// Everything the per-pixel shader needs, resolved once per image.
struct Scene {
    motif: Motif,

    /// Dominant background colours (top, bottom).
    back: (Rgb, Rgb),
    /// Primary subject colour.
    subject: Rgb,
    /// Secondary/accent colour.
    accent: Rgb,
    /// Silhouette colour for foreground layers.
    ink: Rgb,
    /// Horizon height in `0.0..1.0` (motifs that have one).
    horizon: f32,
    /// Focal point.
    focus: (f32, f32),
    /// Focal radius in normalised units.
    focus_r: f32,
    /// Shape phases/frequencies for the procedural outlines.
    waves: [(f32, f32); 3],
    /// Star/dot cell density.
    density: f32,
    /// Per-image grain key.
    grain: u64,
}

fn paint(width: u32, height: u32, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed ^ 0xA5A5_5A5A_C3C3_3C3C);
    let scene = scene(&mut rng, seed);

    let span_x = f32::from(u16::try_from(width.saturating_sub(1)).unwrap_or(0)).max(1.0);
    let span_y = f32::from(u16::try_from(height.saturating_sub(1)).unwrap_or(0)).max(1.0);
    // Grain is scaled by the short edge so it stays visible on a thumbnail and
    // does not turn into noise on a full-size image.
    let grain_scale = 1.0 / (width.min(height) as f32).max(64.0);

    let pixel_len = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(3);
    let mut pixels = vec![0u8; pixel_len];

    for y in 0..height {
        let yf = y as f32 / span_y;
        for x in 0..width {
            let xf = x as f32 / span_x;
            let mut rgb = shade(&scene, xf, yf);
            rgb = vignette(rgb, xf, yf);
            rgb = grain(rgb, xf, yf, grain_scale, scene.grain);
            let offset = ((y as usize) * (width as usize) + (x as usize)) * 3;
            if let Some(slot) = pixels.get_mut(offset..offset + 3) {
                for (dst, src) in slot.iter_mut().zip(rgb.iter()) {
                    *dst = (src.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        }
    }

    pixels
}

fn scene(rng: &mut Rng, seed: u64) -> Scene {
    let motif = Motif::from_index(rng.below(5));
    let hue = fraction(rng.next_u64()) * 360.0;
    // Analogous partner 18-46 degrees away, and a deliberate accent in the
    // complementary half of the wheel so no composition is monochrome.
    let hue_b = hue + 18.0 + f32::from(u16::try_from(rng.below(29)).unwrap_or(0));
    let hue_accent = hue + 150.0 + f32::from(u16::try_from(rng.below(70)).unwrap_or(0));

    let back = match motif {
        Motif::Sunset => (
            hsl_to_rgb(hue + 210.0, 0.44, 0.16),
            hsl_to_rgb(hue + 20.0, 0.72, 0.62),
        ),
        Motif::Bloom => (
            hsl_to_rgb(hue_b, 0.34, 0.92),
            hsl_to_rgb(hue + 12.0, 0.42, 0.74),
        ),
        Motif::Poster => (
            hsl_to_rgb(hue + 200.0, 0.52, 0.22),
            hsl_to_rgb(hue + 200.0, 0.52, 0.30),
        ),
        Motif::Night => (
            hsl_to_rgb(hue + 230.0, 0.48, 0.10),
            hsl_to_rgb(hue + 250.0, 0.38, 0.22),
        ),
        Motif::Drift => (hsl_to_rgb(hue, 0.46, 0.24), hsl_to_rgb(hue_b, 0.36, 0.62)),
    };

    let subject = match motif {
        Motif::Sunset => hsl_to_rgb(hue + 34.0, 0.88, 0.74),
        Motif::Bloom => hsl_to_rgb(hue + 330.0, 0.62, 0.68),
        Motif::Poster => hsl_to_rgb(hue_accent, 0.74, 0.56),
        Motif::Night => hsl_to_rgb(hue + 52.0, 0.22, 0.92),
        Motif::Drift => hsl_to_rgb(hue + 196.0, 0.62, 0.58),
    };
    let accent = hsl_to_rgb(hue_accent, 0.66, 0.60);
    let ink = hsl_to_rgb(hue + 232.0, 0.32, 0.11);

    let horizon = 0.52 + 0.20 * fraction(rng.next_u64());
    let focus = (
        0.28 + 0.44 * fraction(rng.next_u64()),
        0.24 + 0.40 * fraction(rng.next_u64()),
    );
    let focus_r = 0.07 + 0.07 * fraction(rng.next_u64());

    let mut waves = [(0.0f32, 0.0f32); 3];
    for slot in &mut waves {
        let freq = 0.7 + 2.0 * fraction(rng.next_u64());
        let phase = std::f32::consts::TAU * fraction(rng.next_u64());
        *slot = (freq, phase);
    }

    let density = 0.2 + 0.4 * fraction(rng.next_u64());

    Scene {
        motif,
        back,
        subject,
        accent,
        ink,
        horizon,
        focus,
        focus_r,
        waves,
        density,
        grain: seed ^ 0x5DEE_CE66_D0C9_1B7F,
    }
}

fn shade(scene: &Scene, xf: f32, yf: f32) -> Rgb {
    match scene.motif {
        Motif::Sunset => sunset(scene, xf, yf),
        Motif::Bloom => bloom(scene, xf, yf),
        Motif::Poster => poster(scene, xf, yf),
        Motif::Night => night(scene, xf, yf),
        Motif::Drift => drift(scene, xf, yf),
    }
}

/// Layered hills under a low sun, with a warm haze band on the horizon.
fn sunset(scene: &Scene, xf: f32, yf: f32) -> Rgb {
    let h = scene.horizon;
    // Sky: deep at the top, warm at the horizon, plus a hot band right on it.
    let mut color = if yf < h {
        let t = smoothstep((yf / h).clamp(0.0, 1.0));
        mix(scene.back.0, scene.back.1, t)
    } else {
        let t = smoothstep(((yf - h) / (1.0 - h).max(1e-3)).clamp(0.0, 1.0));
        mix(darken(scene.back.1, 0.45), scene.ink, t)
    };

    if yf < h {
        let glow_band = 1.0 - (((yf - h * 0.94) / (h * 0.16)).abs()).clamp(0.0, 1.0);
        color = screen(color, scale(scene.subject, glow_band * 0.55));

        // Sun disc with a soft limb.
        let (sx, sy) = scene.focus;
        let sun_y = sy.min(h - 0.06);
        let disc = soft_disc(xf, yf, sx, sun_y, scene.focus_r, 0.06);
        color = screen(color, scale(scene.subject, disc * 0.95));
        let halo = soft_disc(xf, yf, sx, sun_y, scene.focus_r * 2.6, 0.55);
        color = screen(color, scale(scene.subject, halo * 0.18));

        // Two thin clouds: horizontal strokes that fade at the edges.
        for (index, (freq, phase)) in scene.waves.iter().take(2).enumerate() {
            let band_y = sun_y - 0.12 - 0.09 * index as f32;
            let wobble = 0.012 * ((xf * freq * 6.0 + phase).sin());
            let thickness = 0.022 + 0.01 * index as f32;
            let d = ((yf - band_y - wobble).abs() / thickness).clamp(0.0, 1.0);
            let inside = 1.0 - smoothstep(d);
            let ends = 1.0 - smoothstep(((xf - 0.5).abs() * 1.6).clamp(0.0, 1.0));
            color = mix(color, scale(scene.subject, 1.08), inside * ends * 0.30);
        }
    }

    // Three silhouettes, nearest darkest.
    for (index, (freq, phase)) in scene.waves.iter().enumerate() {
        let depth = index as f32;
        let base = h + 0.035 * depth;
        let amp = 0.055 - 0.012 * depth;
        let ridge = base
            + amp
                * ((xf * std::f32::consts::PI * freq + phase).sin() * 0.7
                    + (xf * std::f32::consts::PI * freq * 2.3 + phase * 1.7).sin() * 0.3);
        if yf > ridge {
            let layer = mix(
                darken(scene.ink, 1.0 - 0.10 * (2.0 - depth)),
                scene.back.1,
                0.06 * depth,
            );
            color = layer;
            // A faint rim of light where the sun catches the ridge.
            let rim = 1.0 - ((yf - ridge) / 0.012).clamp(0.0, 1.0);
            color = screen(
                color,
                scale(scene.subject, rim * 0.22 * (1.0 - depth * 0.3)),
            );
        }
    }

    color
}

/// Overlapping translucent petals around a bright core.
fn bloom(scene: &Scene, xf: f32, yf: f32) -> Rgb {
    // Radial wash so the petals sit on a lit background, with a soft shaded pool
    // behind the flower: without it pale petals on a pale ground have no edge.
    let dc = distance(xf, yf, scene.focus.0, scene.focus.1);
    let wash = (1.0 - (dc / 1.05).clamp(0.0, 1.0)).powf(1.6);
    let mut color = mix(scene.back.1, scene.back.0, wash);
    let pool = 1.0 - smoothstep((dc / (scene.focus_r * 4.2)).clamp(0.0, 1.0));
    color = mix(color, darken(scene.accent, 0.55), pool * 0.22);

    let petals = 6 + (scene.density * 8.0) as u32;
    for petal in 0..petals {
        let angle = std::f32::consts::TAU * (petal as f32 / petals as f32)
            + scene.waves[0].1
            + 0.35 * ((petal as f32) * 1.7).sin();
        let (sin, cos) = angle.sin_cos();
        let dx = xf - scene.focus.0;
        let dy = yf - scene.focus.1;
        // Petal frame: `u` runs along the petal, `v` across it. The centre sits
        // half a semi-axis from the flower core so the petals actually overlap it
        // instead of ringing it with a gap.
        let rx = scene.focus_r * (1.9 + 0.55 * ((petal as f32) * 1.7).sin());
        let ry = scene.focus_r * 1.25;
        let u = dx * cos + dy * sin - rx * 0.5;
        let v = -dx * sin + dy * cos;
        let norm = (u / rx).powi(2) + (v / ry).powi(2);
        if norm <= 1.0 {
            let body = 1.0 - smoothstep((norm.sqrt() - 0.82) / 0.18);
            let shade_f = 0.78 + 0.36 * (0.5 + 0.5 * (u / rx).clamp(-1.0, 1.0));
            let petal_color = mix(scene.subject, scene.accent, (petal % 3) as f32 * 0.20);
            color = mix(color, scale(petal_color, shade_f), body * 0.88);
        }
    }

    // Core: a warm disc plus a couple of stamen dots.
    let core = soft_disc(
        xf,
        yf,
        scene.focus.0,
        scene.focus.1,
        scene.focus_r * 0.42,
        0.02,
    );
    color = mix(color, mix(scene.accent, [1.0, 1.0, 1.0], 0.35), core * 0.92);
    for dot in 0..5 {
        let a = std::f32::consts::TAU * (dot as f32 / 5.0) + 0.6;
        let r = scene.focus_r * 0.22;
        let px = scene.focus.0 + a.cos() * r;
        let py = scene.focus.1 + a.sin() * r;
        let d = soft_disc(xf, yf, px, py, scene.focus_r * 0.045, 0.01);
        color = mix(color, scene.ink, d * 0.45);
    }

    color
}

/// Flat duotone poster: a diagonal split, a disc, and a halftone corner.
fn poster(scene: &Scene, xf: f32, yf: f32) -> Rgb {
    // A rotated diagonal decides which half of the sheet a pixel belongs to.
    let tilt = 0.35 + 0.4 * scene.density;
    let across = (xf * tilt + yf * (1.0 - tilt * 0.35)).clamp(0.0, 1.0);
    let mut color = if across < 0.5 {
        scene.back.0
    } else {
        mix(scene.back.0, scene.back.1, (across - 0.5) * 2.0)
    };

    // Halftone field in one corner only: a printed-poster cue that reads even at
    // 180px wide and does not swallow the main shape.
    let corner = (1.0 - ((xf * 1.35 + yf * 0.75) / 0.95).clamp(0.0, 1.0)).powf(1.5);
    if corner > 0.02 {
        let cells = 14.0 + 12.0 * scene.density;
        let gx = xf * cells;
        let gy = yf * cells;
        let fx = gx - gx.floor();
        let fy = gy - gy.floor();
        let d = ((fx - 0.5).powi(2) + (fy - 0.5).powi(2)).sqrt();
        if d < 0.10 + 0.24 * corner {
            color = mix(color, scene.ink, 0.50 * corner);
        }
    }

    // Solid accent disc with a thin ring, off-centre so it does not read as a
    // bullseye.
    let disc = soft_disc(xf, yf, scene.focus.0, scene.focus.1, 0.27, 0.008);
    color = mix(color, scene.subject, disc * 0.95);
    let ring = (soft_disc(xf, yf, scene.focus.0, scene.focus.1, 0.33, 0.006)
        - soft_disc(xf, yf, scene.focus.0, scene.focus.1, 0.305, 0.006))
    .max(0.0);
    color = mix(color, scene.accent, ring * 0.80);

    // A thin horizontal rule across the lower third, the other half of the
    // geometric language.
    if (yf - 0.80).abs() < 0.018 {
        color = mix(color, scene.accent, 0.65);
    }

    color
}

/// Night sky: a moon, scattered stars and a dark ridge.
fn night(scene: &Scene, xf: f32, yf: f32) -> Rgb {
    let t = smoothstep(yf.clamp(0.0, 1.0));
    let mut color = mix(scene.back.0, scene.back.1, t);

    // Aurora-ish bands: broad, low-contrast, additive.
    for (index, (freq, phase)) in scene.waves.iter().take(2).enumerate() {
        let center = 0.28 + 0.14 * index as f32;
        let wobble = 0.05 * ((xf * freq * 4.0 + phase).sin());
        let d = ((yf - center - wobble).abs() / 0.16).clamp(0.0, 1.0);
        let band = (1.0 - smoothstep(d)).powi(2);
        color = screen(color, scale(scene.accent, band * 0.10));
    }

    // Stars: one candidate per lattice cell, so the density is uniform and the
    // positions are stable for a given seed.
    let cells = 26.0 + 26.0 * scene.density;
    let gx = xf * cells;
    let gy = yf * cells;
    let cell_x = gx.floor();
    let cell_y = gy.floor();
    let key = hash2(cell_x as i64, cell_y as i64, scene.grain);
    if key > 0.86 {
        let jitter_x = hash2(cell_x as i64 + 7, cell_y as i64, scene.grain ^ 0x9E37);
        let jitter_y = hash2(cell_x as i64, cell_y as i64 + 13, scene.grain ^ 0x85EB);
        let px = (cell_x + jitter_x) / cells;
        let py = (cell_y + jitter_y) / cells;
        let brightness = (key - 0.86) / 0.14;
        let d = distance(xf, yf, px, py);
        let star = (1.0 - smoothstep((d / 0.006).clamp(0.0, 1.0))) * brightness;
        color = screen(color, scale([1.0, 0.97, 0.90], star));
    }

    // Moon.
    let moon = soft_disc(xf, yf, scene.focus.0, scene.focus.1 * 0.8, 0.11, 0.014);
    color = mix(color, scene.subject, moon * 0.95);
    let halo = soft_disc(xf, yf, scene.focus.0, scene.focus.1 * 0.8, 0.26, 0.7);
    color = screen(color, scale(scene.subject, halo * 0.12));

    // Ridge line.
    let (freq, phase) = scene.waves[2];
    let ridge = 0.74
        + 0.05 * ((xf * std::f32::consts::PI * freq + phase).sin())
        + 0.02 * ((xf * std::f32::consts::PI * freq * 3.1 + phase * 0.6).sin());
    if yf > ridge {
        let depth = ((yf - ridge) / (1.0 - ridge).max(1e-3)).clamp(0.0, 1.0);
        color = mix(
            darken(scene.ink, 1.0),
            darken(scene.back.0, 0.55),
            depth * 0.4,
        );
    }

    color
}

/// Abstract mesh: four harmonised light sources over a gradient, plus grain.
fn drift(scene: &Scene, xf: f32, yf: f32) -> Rgb {
    let t = (xf * 0.35 + yf * 0.65).clamp(0.0, 1.0);
    let mut color = mix(scene.back.0, scene.back.1, smoothstep(t));

    let (freq, phase) = scene.waves[0];
    let blobs = [
        (
            scene.focus.0,
            scene.focus.1,
            0.34,
            scene.subject,
            0.62 + 0.1 * phase.sin(),
        ),
        (
            1.0 - scene.focus.0 * 0.8,
            0.18 + 0.4 * scene.waves[1].1.sin().abs(),
            0.30,
            scene.accent,
            0.44,
        ),
        (
            0.18 + 0.3 * (phase * 1.3).cos().abs(),
            0.78,
            0.36,
            scene.back.1,
            0.55,
        ),
        (
            0.72 + 0.12 * (phase * 0.7).cos(),
            0.62 + 0.1 * freq.sin(),
            0.22,
            scene.subject,
            0.30,
        ),
    ];

    for (cx, cy, r, tint, strength) in blobs {
        let d = distance(xf, yf, cx, cy);
        let falloff = (1.0 - smoothstep((d / r).clamp(0.0, 1.0))).powf(1.7);
        color = mix(color, tint, falloff * strength);
    }

    // Two soft bands give the mesh a direction instead of pure blur.
    for (index, (freq, phase)) in scene.waves.iter().take(2).enumerate() {
        let angle = 0.4 + 0.5 * index as f32;
        let across = xf * angle.cos() + yf * angle.sin();
        let wave = (across * (3.0 + freq) + phase).sin() * 0.5 + 0.5;
        let band = wave.powf(5.0);
        color = screen(color, scale(scene.accent, band * 0.09));
    }

    // A single hard-edged band across the lower third stops the mesh from reading
    // as a blurred blob. A thin ring was tried first and looked like a stray
    // artifact at card size.
    let band_edge = 0.62 + 0.06 * scene.waves[2].1.cos();
    if yf > band_edge {
        color = mix(color, scale(scene.accent, 0.55), 0.35);
    }

    // A dark vignette-corner wedge for contrast.
    if yf > 0.72 + 0.10 * (xf * std::f32::consts::PI * scene.waves[1].0 + scene.waves[1].1).sin() {
        color = mix(color, darken(scene.ink, 0.9), 0.45);
    }

    color
}

// ---------------------------------------------------------------- helpers

fn fraction(value: u64) -> f32 {
    let bits = u32::try_from(value >> 40).unwrap_or(0);
    bits as f32 / FRACTION_SCALE
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn scale(color: Rgb, k: f32) -> Rgb {
    [color[0] * k, color[1] * k, color[2] * k]
}

fn screen(a: Rgb, b: Rgb) -> Rgb {
    [
        1.0 - (1.0 - a[0]) * (1.0 - b[0]),
        1.0 - (1.0 - a[1]) * (1.0 - b[1]),
        1.0 - (1.0 - a[2]) * (1.0 - b[2]),
    ]
}

fn darken(color: Rgb, factor: f32) -> Rgb {
    scale(color, factor.clamp(0.0, 1.0))
}

fn distance(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let dx = ax - bx;
    let dy = ay - by;
    (dx * dx + dy * dy).sqrt()
}

/// 1 inside the radius, fading to 0 across `feather`.
fn soft_disc(xf: f32, yf: f32, cx: f32, cy: f32, radius: f32, feather: f32) -> f32 {
    let d = distance(xf, yf, cx, cy);
    if d >= radius + feather {
        return 0.0;
    }
    if d <= radius {
        return 1.0;
    }
    1.0 - smoothstep(((d - radius) / feather.max(1e-4)).clamp(0.0, 1.0))
}

fn vignette(color: Rgb, xf: f32, yf: f32) -> Rgb {
    let dx = xf - 0.5;
    let dy = yf - 0.5;
    let r = (dx * dx + dy * dy) * 2.0;
    let k = 1.0 - 0.22 * r.clamp(0.0, 1.0);
    scale(color, k)
}

/// Deterministic per-pixel grain. Coarse (cells of several pixels) so it reads as
/// film texture rather than television snow at thumbnail sizes.
fn grain(color: Rgb, xf: f32, yf: f32, scale_factor: f32, key: u64) -> Rgb {
    let cell = 1.0 / scale_factor.max(1e-6) * 0.02;
    let gx = (xf / cell.max(1e-6)).floor() as i64;
    let gy = (yf / cell.max(1e-6)).floor() as i64;
    let noise = hash2(gx, gy, key) - 0.5;
    let amount = 0.022;
    [
        color[0] + noise * amount,
        color[1] + noise * amount,
        color[2] + noise * amount,
    ]
}

/// Stable `0.0..1.0` hash of a 2-D integer cell plus a key.
fn hash2(x: i64, y: i64, key: u64) -> f32 {
    let mut v = key
        ^ (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    v ^= v >> 33;
    v = v.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    v ^= v >> 33;
    v = v.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    v ^= v >> 33;
    (v >> 40) as f32 / FRACTION_SCALE
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> Rgb {
    let hue = hue.rem_euclid(360.0);
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue / 60.0;
    let second = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = if sector < 1.0 {
        (chroma, second, 0.0)
    } else if sector < 2.0 {
        (second, chroma, 0.0)
    } else if sector < 3.0 {
        (0.0, chroma, second)
    } else if sector < 4.0 {
        (0.0, second, chroma)
    } else if sector < 5.0 {
        (second, 0.0, chroma)
    } else {
        (chroma, 0.0, second)
    };
    let base = lightness - chroma / 2.0;
    [r + base, g + base, b + base]
}

#[cfg(test)]
mod tests {
    use super::{MAX_DIM, Motif, clamp_dims, hash2, hsl_to_rgb, render};

    #[test]
    fn dims_are_clamped_and_never_zero() {
        assert_eq!(clamp_dims(0, 0), (1, 1));
        assert_eq!(clamp_dims(0, 400), (1, 1));
        assert_eq!(clamp_dims(400, 0), (1, 1));
        assert_eq!(clamp_dims(64, 48), (64, 48));
        assert_eq!(clamp_dims(99_999, 8), (MAX_DIM, 8));
        assert_eq!(clamp_dims(8, 99_999), (8, MAX_DIM));
    }

    #[test]
    fn hsl_stays_in_the_unit_range() {
        for step in 0..72 {
            let rgb = hsl_to_rgb(step as f32 * 5.0, 0.5, 0.5);
            for channel in rgb {
                assert!((0.0..=1.0).contains(&channel), "channel {channel}");
            }
        }
        // Negative and out-of-range hues wrap instead of producing garbage.
        assert_eq!(hsl_to_rgb(-60.0, 0.5, 0.5), hsl_to_rgb(300.0, 0.5, 0.5));
        assert_eq!(hsl_to_rgb(420.0, 0.5, 0.5), hsl_to_rgb(60.0, 0.5, 0.5));
    }

    #[test]
    fn rendering_is_a_pure_function_of_the_seed() {
        let a = render(96, 128, 42);
        let b = render(96, 128, 42);
        assert_eq!(a, b, "the same seed must produce byte-identical artwork");
        let c = render(96, 128, 43);
        assert_ne!(a, c, "different seeds must produce different artwork");
    }

    #[test]
    fn rendered_png_decodes_to_the_requested_size() {
        for (w, h) in [(1, 1), (3, 240), (240, 3), (180, 180), (512, 320)] {
            let png = render(w, h, 7);
            let decoded = image::load_from_memory(&png).expect("valid png");
            assert_eq!((decoded.width(), decoded.height()), (w, h));
        }
    }

    #[test]
    fn every_motif_paints_real_variation() {
        // Some seeds are flat-looking by construction (a poster can be mostly one
        // field), so this asserts that across the motif range every image carries
        // a usable spread of colour instead of a single sampled value.
        for seed in 0..25u64 {
            let png = render(80, 100, seed);
            let img = image::load_from_memory(&png).expect("valid png").to_rgb8();
            let mut min = [255u8; 3];
            let mut max = [0u8; 3];
            for pixel in img.pixels() {
                for channel in 0..3 {
                    min[channel] = min[channel].min(pixel[channel]);
                    max[channel] = max[channel].max(pixel[channel]);
                }
            }
            let spread =
                (max[0] - min[0]) as u32 + (max[1] - min[1]) as u32 + (max[2] - min[2]) as u32;
            assert!(
                spread > 24,
                "seed {seed} produced a near-flat image (spread {spread})"
            );
        }
    }

    #[test]
    fn motif_selection_covers_every_variant() {
        let mut seen = Vec::new();
        for index in 0..5 {
            let motif = Motif::from_index(index);
            assert!(!seen.contains(&motif), "motif {motif:?} repeat at {index}");
            seen.push(motif);
        }
        // Wraps rather than panicking on large indices.
        assert_eq!(Motif::from_index(5), Motif::from_index(0));
    }

    #[test]
    fn hash_is_stable_and_in_range() {
        for x in -40..40 {
            for y in -40..40 {
                let a = hash2(x, y, 99);
                let b = hash2(x, y, 99);
                assert_eq!(a, b);
                assert!((0.0..=1.0).contains(&a), "hash out of range: {a}");
            }
        }
        assert_ne!(hash2(1, 2, 99), hash2(2, 1, 99));
    }
}
