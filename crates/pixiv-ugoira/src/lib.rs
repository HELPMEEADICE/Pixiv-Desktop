//! Ugoira decoding and export.
//!
//! Ugoira is a ZIP of JPEG/PNG frames plus a per-frame delay list supplied by the
//! `ugoira_meta` endpoint — it is not a video container. This crate turns that
//! pair into frames the UI can blit, and can re-encode them as an animated GIF.
//!
//! Frames are decoded lazily in the UI; this crate exposes both the timing model
//! ([`UgoiraFilm::frame_index_at`]) and full decoding, plus
//! [`UgoiraFilm::scaled_to_width`] for a film published at the size it is drawn.
//!
//! # Memory profile
//!
//! [`UgoiraFilm`] owns every decoded frame, so decoding is all-or-nothing: the
//! resident cost of one film is `width * height * 4 * frames` bytes and it stays
//! alive until the film is dropped. Frames are inflated and decoded one at a
//! time — nothing ever holds a staging copy of the whole archive — so the peak is
//! that same film footprint plus one entry. Exporting to GIF temporarily costs
//! one extra frame copy because the GIF encoder takes ownership of the buffer it
//! converts; that copy is released as soon as the frame is encoded.
//!
//! # Hostile input
//!
//! A ugoira archive is remote data: every byte in it, including the central
//! directory's declared sizes, is attacker-controlled. [`UgoiraFilm::decode_zip`]
//! contains no `unwrap`, `expect` or indexing that can panic, bounds each read by
//! the entry's declared size, and refuses archives whose decoded footprint would
//! exceed [`MAX_DECODED_BYTES`] — see that constant for where the ceiling is
//! enforced.

use std::io::{Cursor, Read};
use std::time::Duration;

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, ImageReader, Limits, RgbaImage};
use pixiv_types::UgoiraFrame;
use thiserror::Error;
use zip::ZipArchive;
use zip::result::ZipError;

/// Ceiling on the decoded pixel bytes one [`UgoiraFilm`] may hold: 512 MiB.
///
/// The ceiling exists because the archive's frame list is remote data: a hostile
/// ZIP can declare a handful of frames whose inflated RGBA8 buffers add up to
/// gigabytes, and decoding them all would abort the process instead of failing
/// the load. 512 MiB is ~60 frames at the `ugoira1920x1080` tier (~8.3 MiB per
/// frame) and ~660 frames at the 600x600 tier, both far above the tens of frames
/// Pixiv actually serves, so a legitimate film never comes close.
///
/// It is enforced at three points, cheapest first:
///
/// 1. before decompressing anything, each entry's declared uncompressed size is
///    charged against the remaining budget — a zip bomb is rejected without ever
///    being inflated;
/// 2. once the first frame reveals the real resolution, the whole-film footprint
///    (`frames * width * height * 4`) is checked, so a small archive that
///    decompresses to huge images is refused after one frame instead of N;
/// 3. the `image` decoder itself is given the remaining budget as its `max_alloc`
///    limit, so an image header claiming absurd dimensions fails to allocate.
pub const MAX_DECODED_BYTES: u64 = 512 * 1024 * 1024;

/// Largest accepted frame edge, in pixels.
///
/// `ugoira1920x1080` tops out at 1920; 16384 leaves room for future tiers while
/// staying inside GIF's `u16` frame dimensions and well under the byte ceiling
/// for a single frame.
const MAX_FRAME_SIDE: u32 = 16_384;

/// Decode/encode failures.
#[derive(Debug, Error)]
pub enum UgoiraError {
    #[error("动图压缩包损坏：{0}")]
    Zip(String),
    #[error("帧解码失败：{0}")]
    Decode(String),
    #[error("导出失败：{0}")]
    Encode(String),
    #[error("帧数量不匹配：压缩包 {in_zip} 帧，元数据 {in_meta} 帧")]
    FrameMismatch { in_zip: usize, in_meta: usize },
}

/// One decoded frame.
#[derive(Debug, Clone)]
pub struct FilmFrame {
    pub file: String,
    pub delay_ms: u32,
    /// RGBA8, `width * height * 4`.
    pub rgba: Vec<u8>,
}

/// A fully decoded ugoira.
#[derive(Debug, Clone)]
pub struct UgoiraFilm {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<FilmFrame>,
}

impl UgoiraFilm {
    /// Decode a ugoira ZIP against its frame timing metadata.
    ///
    /// Frames must be emitted in the order given by `frames` (the ZIP entry
    /// order is not guaranteed to match) and must all share one resolution.
    ///
    /// # Errors
    ///
    /// * [`UgoiraError::FrameMismatch`] — the archive holds fewer entries than
    ///   the metadata declares.
    /// * [`UgoiraError::Zip`] — the archive itself is unreadable, an entry named
    ///   by the metadata is absent (the message names the file), or the decoded
    ///   footprint would exceed [`MAX_DECODED_BYTES`].
    /// * [`UgoiraError::Decode`] — an entry is not a decodable image, or a frame's
    ///   resolution differs from the first frame's.
    pub fn decode_zip(zip_bytes: &[u8], frames: &[UgoiraFrame]) -> Result<Self, UgoiraError> {
        if frames.is_empty() {
            return Err(UgoiraError::Zip("元数据未声明任何帧".to_owned()));
        }

        let mut archive = ZipArchive::new(Cursor::new(zip_bytes))
            .map_err(|e| UgoiraError::Zip(format!("无法打开压缩包：{e}")))?;
        // Every metadata frame needs an entry; a short archive can only be damage.
        if archive.len() < frames.len() {
            return Err(UgoiraError::FrameMismatch {
                in_zip: archive.len(),
                in_meta: frames.len(),
            });
        }
        if archive.len() > frames.len() {
            // Not fatal: entries are located by name, so extra members are simply
            // never opened. Worth a log line when diagnosing a weird archive.
            tracing::debug!(
                entries = archive.len(),
                frames = frames.len(),
                "ugoira 压缩包条目多于元数据帧数，按文件名取帧"
            );
        }

        // Frame count is known up front, so the whole-film footprint can be
        // judged as soon as the first frame's resolution is known.
        let frame_count = u64::try_from(frames.len()).unwrap_or(u64::MAX);
        let mut resolution: Option<(u32, u32)> = None;
        let mut budget = MAX_DECODED_BYTES;
        let mut decoded: Vec<FilmFrame> = Vec::with_capacity(frames.len());

        for meta in frames {
            // Entries are looked up by name, never by position: the archive's
            // order is not the playback order.
            let entry = archive.by_name(&meta.file).map_err(|e| match e {
                ZipError::FileNotFound => {
                    UgoiraError::Zip(format!("压缩包缺少帧文件 {}", meta.file))
                }
                other => UgoiraError::Zip(format!("读取帧 {} 失败：{other}", meta.file)),
            })?;

            let declared = entry.size();
            if declared > budget {
                return Err(over_ceiling(declared, &meta.file));
            }
            let entry_bytes = read_entry(entry, declared)?;

            let mut reader = ImageReader::new(Cursor::new(entry_bytes.as_slice()))
                .with_guessed_format()
                .map_err(|e| UgoiraError::Decode(format!("帧 {} 无法识别格式：{e}", meta.file)))?;
            reader.limits(decode_limits(budget));
            let image = reader
                .decode()
                .map_err(|e| UgoiraError::Decode(format!("帧 {} 解码失败：{e}", meta.file)))?;

            let rgba = image.into_rgba8();
            let (width, height) = (rgba.width(), rgba.height());
            match resolution {
                None => {
                    if !footprint_fits(width, height, frame_count) {
                        return Err(over_ceiling(
                            u64::from(width)
                                .saturating_mul(u64::from(height))
                                .saturating_mul(4)
                                .saturating_mul(frame_count),
                            &meta.file,
                        ));
                    }
                    resolution = Some((width, height));
                }
                Some(first) if first != (width, height) => {
                    return Err(UgoiraError::Decode(format!(
                        "帧 {} 分辨率 {width}x{height} 与首帧 {}x{} 不一致",
                        meta.file, first.0, first.1
                    )));
                }
                Some(_) => {}
            }

            let pixel_bytes = u64::try_from(rgba.as_raw().len()).unwrap_or(u64::MAX);
            budget = budget.saturating_sub(pixel_bytes);
            decoded.push(FilmFrame {
                file: meta.file.clone(),
                // Timing is metadata, never derived from the archive.
                delay_ms: meta.delay_ms,
                rgba: rgba.into_raw(),
            });
        }

        // `frames` is non-empty and every iteration sets `resolution`.
        let (width, height) = resolution.unwrap_or((0, 0));
        Ok(Self {
            width,
            height,
            frames: decoded,
        })
    }

    /// Milliseconds one pass of the film takes.
    ///
    /// Clamped at `u32` because the value drives a UI clock and a caption, and one
    /// frame's delay is already `u32`: the only films that saturate are the ones a
    /// hostile metadata list declares (49 days of playback per loop). The exact sum
    /// is still available where the wrap arithmetic needs it — see
    /// [`Self::timeline_ms`].
    #[must_use]
    pub fn total_duration_ms(&self) -> u32 {
        u32::try_from(self.timeline_ms()).unwrap_or(u32::MAX)
    }

    /// Which frame is showing at `elapsed_ms`, looping.
    #[must_use]
    pub fn frame_index_at(&self, elapsed_ms: u64) -> usize {
        let total = self.timeline_ms();
        // A degenerate film (no frames, or every delay zero) has no timeline to
        // index into; the first frame is the only defensible answer.
        if total == 0 || self.frames.is_empty() {
            return 0;
        }
        let offset = elapsed_ms % total;
        let mut window_end = 0_u64;
        for (index, frame) in self.frames.iter().enumerate() {
            window_end += u64::from(frame.delay_ms);
            if offset < window_end {
                return index;
            }
        }
        // Unreachable: the windows sum to `total` and `offset < total`. Kept as a
        // total fallback rather than an assertion so bad data cannot panic.
        self.frames.len() - 1
    }

    /// Elapsed time at which `index` starts.
    #[must_use]
    pub fn frame_start_ms(&self, index: usize) -> u64 {
        let index = index.min(self.frames.len().saturating_sub(1));
        self.frames
            .iter()
            .take(index)
            .map(|f| u64::from(f.delay_ms))
            .sum()
    }

    /// The exact loop length in milliseconds.
    ///
    /// `u64` and private: a frame's delay is `u32`, so a metadata list of
    /// `u32::MAX` delays sums past `u32::MAX`, and the modulo in
    /// [`Self::frame_index_at`] must not be clamped with it — a film whose total
    /// saturates would report frame 0 for an elapsed time inside a later loop.
    fn timeline_ms(&self) -> u64 {
        self.frames.iter().map(|f| u64::from(f.delay_ms)).sum()
    }

    /// A copy of this film scaled to `width` pixels wide, keeping the aspect ratio.
    ///
    /// `0`, the current width and anything wider return the film unchanged: a
    /// viewer asking for a size it already holds must not pay for a second copy of
    /// every frame, and an upscale would invent pixels the archive never carried.
    /// Everything else is an area average — see [`area_average`] for why that
    /// filter and not a sample.
    #[must_use]
    pub fn scaled_to_width(&self, width: u32) -> Self {
        if width == 0 || width >= self.width {
            return self.clone();
        }
        let height = scaled_height(self.width, self.height, width);
        let frames = self
            .frames
            .iter()
            .map(|frame| FilmFrame {
                file: frame.file.clone(),
                // Timing is metadata; a smaller frame has the same delay.
                delay_ms: frame.delay_ms,
                rgba: area_average(&frame.rgba, self.width, self.height, width, height),
            })
            .collect();
        Self {
            width,
            height,
            frames,
        }
    }

    /// Re-encode as an animated GIF, honouring the original per-frame delays.
    ///
    /// GIF delays are counted in centiseconds (10 ms), so each delay is rounded
    /// down to that grid with a floor of one tick: a 3 ms frame becomes 10 ms
    /// rather than 0, because a zero delay makes viewers show the frame
    /// instantaneously. The loop is infinite, matching ugoira playback.
    ///
    /// # Errors
    ///
    /// [`UgoiraError::Encode`] when the film is empty or a frame's buffer does
    /// not match `width * height * 4`, or the GIF encoder rejects the frame.
    pub fn export_gif(&self) -> Result<Vec<u8>, UgoiraError> {
        if self.frames.is_empty() {
            return Err(UgoiraError::Encode("没有任何帧可导出".to_owned()));
        }

        let mut out = Vec::new();
        {
            // Scoped so the encoder drops (and writes the GIF trailer) before the
            // buffer is handed back.
            let mut encoder = GifEncoder::new(&mut out);
            encoder
                .set_repeat(Repeat::Infinite)
                .map_err(|e| UgoiraError::Encode(e.to_string()))?;
            for frame in &self.frames {
                // The encoder needs an owned buffer, so this is the one place the
                // film is copied; the copy is dropped as soon as it is encoded.
                let buffer = RgbaImage::from_raw(self.width, self.height, frame.rgba.clone())
                    .ok_or_else(|| {
                        UgoiraError::Encode(format!(
                            "帧 {} 的像素长度与 {}x{} 不符",
                            frame.file, self.width, self.height
                        ))
                    })?;
                encoder
                    .encode_frame(Frame::from_parts(buffer, 0, 0, gif_delay(frame.delay_ms)))
                    .map_err(|e| UgoiraError::Encode(format!("帧 {} 编码失败：{e}", frame.file)))?;
            }
        }
        Ok(out)
    }
}

/// Read one ZIP entry without trusting its declared size.
///
/// `take(declared + 1)` bounds the inflated output to what the pre-flight budget
/// check already agreed to, so a lying header cannot expand past it, and the
/// buffer grows with the bytes actually produced instead of the declared count.
fn read_entry(mut entry: impl Read, declared: u64) -> Result<Vec<u8>, UgoiraError> {
    let mut bytes = Vec::new();
    (&mut entry)
        .take(declared.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| UgoiraError::Zip(format!("解压帧数据失败：{e}")))?;
    Ok(bytes)
}

/// Decoder limits for one frame: the strict side caps plus the remaining budget.
///
/// Starts from [`Limits::no_limits`] and re-adds exactly the limits this crate
/// wants, so a change to `image`'s defaults can neither loosen nor tighten them.
fn decode_limits(budget: u64) -> Limits {
    let mut limits = Limits::no_limits();
    limits.max_image_width = Some(MAX_FRAME_SIDE);
    limits.max_image_height = Some(MAX_FRAME_SIDE);
    limits.max_alloc = Some(budget);
    limits
}

/// Would `frame_count` frames of `width x height` RGBA8 fit under the ceiling?
///
/// Saturating throughout: a hostile header can report `u32::MAX` on both sides,
/// and the answer must be a refusal, not an overflow panic.
fn footprint_fits(width: u32, height: u32, frame_count: u64) -> bool {
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(4)
        .saturating_mul(frame_count)
        <= MAX_DECODED_BYTES
}

/// The refusal for an archive whose decoded footprint exceeds the ceiling.
fn over_ceiling(bytes: u64, file: &str) -> UgoiraError {
    UgoiraError::Zip(format!(
        "帧 {file} 解码后需 {bytes} 字节，超过 {} MiB 上限",
        MAX_DECODED_BYTES / (1024 * 1024)
    ))
}

/// The height that keeps `src_width`:`src_height` at `width`, rounded to nearest.
///
/// Rounded rather than truncated then floored at one: a 31-frame strip scaled to a
/// width whose exact height is 12.7 must not collapse to 12 and squash every frame
/// by a hair, and a very wide frame must not become zero rows tall.
fn scaled_height(src_width: u32, src_height: u32, width: u32) -> u32 {
    if src_width == 0 {
        return src_height.max(1);
    }
    let scaled = u64::from(src_height) * u64::from(width);
    let rounded = (scaled + u64::from(src_width) / 2) / u64::from(src_width);
    u32::try_from(rounded).unwrap_or(u32::MAX).max(1)
}

/// Area-average downscale of one RGBA8 frame.
///
/// Every destination pixel is the mean of the source pixels it covers, each
/// weighted by the fraction of it that falls inside the destination pixel's box.
/// Point sampling throws that weight away — at 3 columns into 2 it keeps one column
/// and drops the other — and bilinear `resize` point-samples at this ratio too, so
/// a 1920 px frame thinned to a viewer's 900 px would lose half its detail to a
/// grid that does not line up with it.
///
/// Weights are integers in units of `1/width` horizontally and `1/height`
/// vertically, so the overlap of two boxes is an exact product and no rounding
/// happens until the final division, which rounds half up: truncating would darken
/// every averaged pixel by up to one level, which reads as a grey cast on a flat
/// field.
fn area_average(src: &[u8], src_width: u32, src_height: u32, width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0_u8; width as usize * height as usize * 4];
    // A frame whose buffer is shorter than its declared geometry has nothing to
    // average; `decode_zip` never produces one, and a film built by hand gets
    // transparent black instead of a panic.
    if src.len() / 4 < src_width as usize * src_height as usize {
        return out;
    }
    let (sw, sh) = (u64::from(src_width), u64::from(src_height));
    let (dw, dh) = (u64::from(width), u64::from(height));
    for dy in 0..dh {
        let top = dy * sh;
        let bottom = top + sh;
        let sy_first = top / dh;
        let sy_last = bottom.div_ceil(dh);
        for dx in 0..dw {
            let left = dx * sw;
            let right = left + sw;
            let sx_first = left / dw;
            let sx_last = right.div_ceil(dw);
            let mut acc = [0_u64; 4];
            let mut weight = 0_u64;
            for sy in sy_first..sy_last {
                let wy = (sy * dh + dh).min(bottom) - (sy * dh).max(top);
                let row = sy * sw;
                for sx in sx_first..sx_last {
                    let wx = (sx * dw + dw).min(right) - (sx * dw).max(left);
                    let w = wx * wy;
                    let offset = usize::try_from((row + sx) * 4).ok();
                    let Some(pixel) = offset.and_then(|offset| src.get(offset..offset + 4)) else {
                        continue;
                    };
                    for (sum, value) in acc.iter_mut().zip(pixel) {
                        *sum += u64::from(*value) * w;
                    }
                    weight += w;
                }
            }
            if weight == 0 {
                continue;
            }
            let offset = ((dy * dw + dx) * 4) as usize;
            for (slot, sum) in out[offset..offset + 4].iter_mut().zip(acc) {
                *slot = u8::try_from((sum + weight / 2) / weight).unwrap_or(u8::MAX);
            }
        }
    }
    out
}

/// GIF timing: centiseconds, never zero.
fn gif_delay(delay_ms: u32) -> Delay {
    let rounded = (delay_ms / 10 * 10).max(10);
    Delay::from_saturating_duration(Duration::from_millis(u64::from(rounded)))
}

#[cfg(test)]
mod tests;
