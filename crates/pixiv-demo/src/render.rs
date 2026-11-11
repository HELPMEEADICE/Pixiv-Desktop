//! Procedural placeholder artwork.
//!
//! Nothing in this module reads the clock, the filesystem, the network or any
//! thread-local state: the byte stream is a pure function of `(width, height,
//! seed)`. That keeps image caches and layout snapshots stable across runs and
//! across machines, and it is why the repository can ship zero third-party
//! imagery.
//!
//! The picture is deliberately simple but not flat: a vertical gradient between
//! two seed-derived hues, a soft radial highlight, and a one-pixel frame so
//! neighbouring cards stay visually separated in the grid.

use image::{ExtendedColorType, ImageEncoder};

use crate::prng::Rng;

/// Defensive ceiling applied to either dimension before any allocation.
const MAX_DIM: u32 = 4096;

/// Deterministic 24-bit fraction in `0.0..1.0`.
const FRACTION_SCALE: f32 = 16_777_216.0;

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

fn paint(width: u32, height: u32, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed ^ 0xA5A5_5A5A_C3C3_3C3C);

    // Two related hues: a deep tone at the top fading into a lighter one at the
    // bottom, which reads as depth rather than as a flat block of colour.
    let hue = fraction(rng.next_u64()) * 360.0;
    let hue_shift = 24.0 + f32::from(u16::try_from(rng.range(0, 56)).unwrap_or(0));
    let top = hsl_to_rgb(hue, 0.58, 0.30);
    let bottom = hsl_to_rgb(hue + hue_shift, 0.52, 0.68);

    // Highlight centre wanders a little per seed but stays inside the frame.
    let centre_x = 0.34 + 0.32 * fraction(rng.next_u64());
    let centre_y = 0.28 + 0.26 * fraction(rng.next_u64());

    let span_x = f32::from(u16::try_from(width.saturating_sub(1)).unwrap_or(0)).max(1.0);
    let span_y = f32::from(u16::try_from(height.saturating_sub(1)).unwrap_or(0)).max(1.0);

    let pixel_len = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(3);
    let mut pixels = vec![0u8; pixel_len];

    for y in 0..height {
        let yf = y as f32 / span_y;
        // Smoothstep so the blend does not band at the ends.
        let t = yf * yf * (3.0 - 2.0 * yf);
        for x in 0..width {
            let xf = x as f32 / span_x;
            let blend = (t * 0.82 + xf * 0.18).clamp(0.0, 1.0);
            let mut rgb = [
                top[0] + (bottom[0] - top[0]) * blend,
                top[1] + (bottom[1] - top[1]) * blend,
                top[2] + (bottom[2] - top[2]) * blend,
            ];

            let dx = xf - centre_x;
            let dy = yf - centre_y;
            let glow = 1.0 - (dx * dx + dy * dy) * 4.2;
            if glow > 0.0 {
                let strength = glow * glow * 0.34;
                for channel in &mut rgb {
                    *channel += (1.0 - *channel) * strength;
                }
            }

            if x == 0 || y == 0 || x + 1 == width || y + 1 == height {
                for channel in &mut rgb {
                    *channel *= 0.55;
                }
            }

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

fn fraction(value: u64) -> f32 {
    let bits = u32::try_from(value >> 40).unwrap_or(0);
    bits as f32 / FRACTION_SCALE
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> [f32; 3] {
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
    use super::{MAX_DIM, clamp_dims, hsl_to_rgb};

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
}
