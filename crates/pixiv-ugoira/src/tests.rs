//! Unit tests for the ugoira crate.
//!
//! No test touches the network: every archive is built in memory with the `zip`
//! crate and every frame is synthesised with the `image` crate. The metadata
//! *shape* is pinned against the live-captured `fixtures/ugoira_meta.json`.

use std::io::{Cursor, Write};
use std::path::PathBuf;

use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, DynamicImage, ImageFormat, Rgba, RgbaImage};
use pixiv_types::wire::{UgoiraFrameWire, UgoiraMetaWire};
use pixiv_types::{UgoiraFrame, UgoiraMeta};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use super::{MAX_DECODED_BYTES, UgoiraError, UgoiraFilm};

const RED: [u8; 4] = [220, 20, 20, 255];
const GREEN: [u8; 4] = [20, 200, 60, 255];
const BLUE: [u8; 4] = [30, 60, 220, 255];

// ---------------------------------------------------------------- fixtures ---

/// A tiny JSON reader: `pixiv-ugoira` has no `serde_json` dependency, and the
/// point of the fixture test is the *shape*, so the bytes are parsed here and
/// mapped onto the real `pixiv_types` wire structs.
#[allow(dead_code)]
#[derive(Debug, Clone)]
enum Json {
    Null,
    Bool(bool),
    Num(i64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Json::Str(s) => s,
            _ => "",
        }
    }

    fn as_u32(&self) -> u32 {
        match self {
            Json::Num(n) => u32::try_from(*n).unwrap_or(0),
            _ => 0,
        }
    }

    fn as_array(&self) -> &[Json] {
        match self {
            Json::Arr(items) => items,
            _ => &[],
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek();
        if byte.is_some() {
            self.pos += 1;
        }
        byte
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, want: u8) {
        let got = self.bump();
        assert_eq!(
            got,
            Some(want),
            "expected {:?} at byte {}",
            want as char,
            self.pos
        );
    }

    fn literal(&mut self, word: &str) {
        for byte in word.as_bytes() {
            self.expect(*byte);
        }
    }

    fn value(&mut self) -> Json {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Json::Str(self.string()),
            Some(b't') => {
                self.literal("true");
                Json::Bool(true)
            }
            Some(b'f') => {
                self.literal("false");
                Json::Bool(false)
            }
            Some(b'n') => {
                self.literal("null");
                Json::Null
            }
            other => self.number(other),
        }
    }

    fn object(&mut self) -> Json {
        self.expect(b'{');
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.bump();
            return Json::Obj(fields);
        }
        loop {
            self.skip_ws();
            let key = self.string();
            self.skip_ws();
            self.expect(b':');
            let value = self.value();
            fields.push((key, value));
            self.skip_ws();
            match self.bump() {
                Some(b',') => {}
                Some(b'}') => return Json::Obj(fields),
                other => panic!("bad object separator {other:?} at byte {}", self.pos),
            }
        }
    }

    fn array(&mut self) -> Json {
        self.expect(b'[');
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.bump();
            return Json::Arr(items);
        }
        loop {
            items.push(self.value());
            self.skip_ws();
            match self.bump() {
                Some(b',') => {}
                Some(b']') => return Json::Arr(items),
                other => panic!("bad array separator {other:?} at byte {}", self.pos),
            }
        }
    }

    fn string(&mut self) -> String {
        self.expect(b'"');
        let mut out: Vec<u8> = Vec::new();
        loop {
            match self.bump() {
                Some(b'"') => break,
                Some(b'\\') => match self.bump() {
                    Some(b'/') => out.push(b'/'),
                    Some(b'"') => out.push(b'"'),
                    Some(b'\\') => out.push(b'\\'),
                    Some(b'n') => out.push(b'\n'),
                    Some(b't') => out.push(b'\t'),
                    Some(b'r') => out.push(b'\r'),
                    Some(b'b') => out.push(0x08),
                    Some(b'f') => out.push(0x0c),
                    Some(b'u') => {
                        let mut code = 0_u32;
                        for _ in 0..4 {
                            let digit = self.bump().expect("hex digit");
                            let value = (digit as char).to_digit(16).expect("hex digit");
                            code = code * 16 + value;
                        }
                        out.extend(
                            char::from_u32(code)
                                .unwrap_or('\u{fffd}')
                                .to_string()
                                .bytes(),
                        );
                    }
                    other => panic!("bad escape {other:?} at byte {}", self.pos),
                },
                Some(byte) => out.push(byte),
                None => panic!("unterminated string"),
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn number(&mut self, first: Option<u8>) -> Json {
        assert!(
            matches!(first, Some(b'-' | b'0'..=b'9')),
            "unexpected json value start {first:?}"
        );
        let start = self.pos;
        while matches!(
            self.peek(),
            Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        ) {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).expect("ascii number");
        // The ugoira body only carries integers (`delay`); fractions are unused.
        let value = text.parse::<i64>().unwrap_or(0);
        Json::Num(value)
    }
}

/// Read `fixtures/ugoira_meta.json` and normalise it through `pixiv_types`
/// exactly as the API client does.
fn fixture_meta() -> UgoiraMeta {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("fixtures")
        .join("ugoira_meta.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let doc = Parser::new(&raw).value();
    let body = doc.get("body").expect("ugoira envelope body");

    let wire = UgoiraMetaWire {
        src: body
            .get("src")
            .map(Json::as_str)
            .unwrap_or_default()
            .to_owned(),
        original_src: body
            .get("originalSrc")
            .map(Json::as_str)
            .unwrap_or_default()
            .to_owned(),
        mime_type: body
            .get("mime_type")
            .map(Json::as_str)
            .unwrap_or_default()
            .to_owned(),
        frames: body
            .get("frames")
            .map(Json::as_array)
            .unwrap_or_default()
            .iter()
            .map(|frame| UgoiraFrameWire {
                file: frame
                    .get("file")
                    .map(Json::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                delay: frame.get("delay").map(Json::as_u32).unwrap_or(0),
            })
            .collect(),
    };
    UgoiraMeta::from(wire)
}

// ----------------------------------------------------------------- helpers ---

fn solid(width: u32, height: u32, colour: [u8; 4]) -> RgbaImage {
    RgbaImage::from_pixel(width, height, Rgba(colour))
}

fn png_bytes(image: &RgbaImage) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut cursor, ImageFormat::Png)
        .expect("png encode");
    cursor.into_inner()
}

fn jpeg_bytes(image: &RgbaImage) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    // The JPEG encoder flattens alpha itself.
    DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut cursor, ImageFormat::Jpeg)
        .expect("jpeg encode");
    cursor.into_inner()
}

/// Build a ZIP in memory from `(entry name, bytes)` pairs, in the given order.
fn build_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        writer
            .start_file(name.as_str(), SimpleFileOptions::default())
            .expect("start entry");
        writer.write_all(data).expect("write entry");
    }
    writer.finish().expect("zip trailer").into_inner()
}

fn frame(name: &str, delay_ms: u32) -> UgoiraFrame {
    UgoiraFrame {
        file: name.to_owned(),
        delay_ms,
    }
}

/// `count` frames named `000000.png`… with the given delays.
fn meta_for(delays: &[u32]) -> Vec<UgoiraFrame> {
    delays
        .iter()
        .enumerate()
        .map(|(index, delay)| frame(&format!("{index:06}.png"), *delay))
        .collect()
}

/// A decodable film whose frames are distinct solid colours, built through
/// `decode_zip` so the tests exercise the real entry point.
fn film(delays: &[u32]) -> UgoiraFilm {
    let meta = meta_for(delays);
    let colours = [RED, GREEN, BLUE];
    let entries: Vec<(String, Vec<u8>)> = meta
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            let colour = colours[index % colours.len()];
            (frame.file.clone(), png_bytes(&solid(8, 6, colour)))
        })
        .collect();
    let zip = build_zip(&entries);
    UgoiraFilm::decode_zip(&zip, &meta).expect("synthetic film decodes")
}

// ------------------------------------------------------------------- tests ---

#[test]
fn decodes_frames_in_metadata_order_despite_archive_order() {
    let meta = meta_for(&[200, 100, 300]);
    // Entry order is deliberately scrambled: 2, 0, 1.
    let entries = vec![
        (meta[2].file.clone(), png_bytes(&solid(8, 6, BLUE))),
        (meta[0].file.clone(), png_bytes(&solid(8, 6, RED))),
        (meta[1].file.clone(), png_bytes(&solid(8, 6, GREEN))),
    ];
    let zip = build_zip(&entries);

    let film = UgoiraFilm::decode_zip(&zip, &meta).expect("decode");

    assert_eq!(film.frames.len(), 3);
    assert_eq!((film.width, film.height), (8, 6));
    let names: Vec<&str> = film.frames.iter().map(|f| f.file.as_str()).collect();
    assert_eq!(names, ["000000.png", "000001.png", "000002.png"]);
    // Colour proves the entry looked up by name really is that frame, not just
    // that some entry was emitted in the right slot.
    assert_eq!(&film.frames[0].rgba[..4], &RED);
    assert_eq!(&film.frames[1].rgba[..4], &GREEN);
    assert_eq!(&film.frames[2].rgba[..4], &BLUE);
    // Delays come from the metadata, never from the archive.
    let delays: Vec<u32> = film.frames.iter().map(|f| f.delay_ms).collect();
    assert_eq!(delays, [200, 100, 300]);
    for frame in &film.frames {
        assert_eq!(frame.rgba.len(), 8 * 6 * 4);
    }
}

#[test]
fn frame_index_at_covers_boundaries_and_wraps() {
    // Delay shape taken from the fixture (millisecond integers per frame).
    let film = film(&[200, 100, 300]);
    assert_eq!(film.total_duration_ms(), 600);

    assert_eq!(film.frame_index_at(0), 0, "start of the loop");
    assert_eq!(film.frame_index_at(199), 0, "last ms of frame 0");
    assert_eq!(film.frame_index_at(200), 1, "first delay is exclusive end");
    assert_eq!(film.frame_index_at(299), 1, "last ms of frame 1");
    assert_eq!(film.frame_index_at(300), 2, "mid film");
    assert_eq!(film.frame_index_at(599), 2, "last ms of frame 2");
    assert_eq!(
        film.frame_index_at(600),
        0,
        "exactly total wraps to frame 0"
    );
    assert_eq!(film.frame_index_at(601), 0, "total + 1 wraps");
    assert_eq!(film.frame_index_at(900), 2, "1.5 loops lands in frame 2");
    assert_eq!(film.frame_index_at(1200), 0, "two full loops");
    assert!(
        film.frame_index_at(u64::MAX) < 3,
        "huge elapsed stays in range"
    );
}

#[test]
fn frame_start_ms_is_cumulative_and_clamped() {
    let film = film(&[200, 100, 300]);
    assert_eq!(film.frame_start_ms(0), 0);
    assert_eq!(film.frame_start_ms(1), 200);
    assert_eq!(film.frame_start_ms(2), 300);
    assert_eq!(
        film.frame_start_ms(3),
        300,
        "out-of-range clamps to last frame"
    );
    assert_eq!(
        film.frame_start_ms(99),
        300,
        "far out-of-range still clamps"
    );
    // The last start plus its own delay is the total: the timeline is closed.
    assert_eq!(
        film.frame_start_ms(2) + u64::from(film.frames[2].delay_ms),
        600
    );
}

#[test]
fn total_duration_sums_metadata_delays() {
    assert_eq!(film(&[16]).total_duration_ms(), 16);
    assert_eq!(film(&[200, 200, 200, 200]).total_duration_ms(), 800);
    assert_eq!(film(&[0, 0]).total_duration_ms(), 0);
    assert_eq!(film(&[u32::MAX, 1]).total_duration_ms(), 4_294_967_296);
}

#[test]
fn single_frame_film_always_returns_zero() {
    let film = film(&[500]);
    assert_eq!(film.frames.len(), 1);
    assert_eq!(film.total_duration_ms(), 500);
    for elapsed in [0, 1, 499, 500, 501, 100_000, u64::MAX] {
        assert_eq!(film.frame_index_at(elapsed), 0, "elapsed {elapsed}");
        assert_eq!(film.frame_start_ms(elapsed as usize), 0);
    }
}

#[test]
fn zero_delay_film_returns_first_frame() {
    let film = film(&[0, 0, 0]);
    assert_eq!(film.total_duration_ms(), 0);
    assert_eq!(film.frame_index_at(0), 0);
    assert_eq!(film.frame_index_at(123_456), 0);
    assert_eq!(film.frame_start_ms(2), 0);
}

#[test]
fn truncated_archive_is_an_error() {
    let meta = meta_for(&[100, 100]);
    let entries = vec![
        (meta[0].file.clone(), png_bytes(&solid(8, 6, RED))),
        (meta[1].file.clone(), png_bytes(&solid(8, 6, GREEN))),
    ];
    let zip = build_zip(&entries);

    for cut in [0, 1, 4, zip.len() / 2, zip.len() - 8] {
        let truncated = &zip[..cut];
        let error =
            UgoiraFilm::decode_zip(truncated, &meta).expect_err("truncated archive must fail");
        assert!(
            matches!(error, UgoiraError::Zip(_)),
            "cut {cut} gave {error:?}"
        );
    }
}

#[test]
fn corrupt_entry_is_a_decode_error() {
    let meta = meta_for(&[100, 100]);
    let entries = vec![
        (meta[0].file.clone(), png_bytes(&solid(8, 6, RED))),
        // Valid ZIP member, garbage payload.
        (meta[1].file.clone(), b"definitely not an image".to_vec()),
    ];
    let zip = build_zip(&entries);

    match UgoiraFilm::decode_zip(&zip, &meta) {
        Err(UgoiraError::Decode(message)) => {
            assert!(
                message.contains("000001.png"),
                "message names the frame: {message}"
            );
        }
        other => panic!("expected a decode error, got {other:?}"),
    }
}

#[test]
fn missing_entry_error_names_the_file() {
    // Same entry count as the metadata, so the count check cannot fire: the
    // failure has to come from the name lookup.
    let meta = vec![
        frame("000000.png", 100),
        frame("000001.png", 100),
        frame("000003.png", 100),
    ];
    let entries = vec![
        (meta[0].file.clone(), png_bytes(&solid(8, 6, RED))),
        (meta[1].file.clone(), png_bytes(&solid(8, 6, GREEN))),
        ("000009.png".to_owned(), png_bytes(&solid(8, 6, BLUE))),
    ];
    let zip = build_zip(&entries);

    match UgoiraFilm::decode_zip(&zip, &meta) {
        Err(UgoiraError::Zip(message)) => {
            assert!(
                message.contains("000003.png"),
                "message names the file: {message}"
            );
        }
        other => panic!("expected a zip error, got {other:?}"),
    }
}

#[test]
fn fewer_entries_than_metadata_is_frame_mismatch() {
    let zip = build_zip(&[
        ("000000.png".to_owned(), png_bytes(&solid(8, 6, RED))),
        ("000001.png".to_owned(), png_bytes(&solid(8, 6, GREEN))),
    ]);
    let meta = meta_for(&[100, 100, 100]);

    match UgoiraFilm::decode_zip(&zip, &meta) {
        Err(UgoiraError::FrameMismatch { in_zip, in_meta }) => {
            assert_eq!((in_zip, in_meta), (2, 3));
        }
        other => panic!("expected a frame mismatch, got {other:?}"),
    }
}

#[test]
fn resolution_mismatch_is_a_decode_error() {
    let meta = meta_for(&[100, 100]);
    let entries = vec![
        (meta[0].file.clone(), png_bytes(&solid(8, 6, RED))),
        (meta[1].file.clone(), png_bytes(&solid(4, 4, GREEN))),
    ];
    let zip = build_zip(&entries);

    match UgoiraFilm::decode_zip(&zip, &meta) {
        Err(UgoiraError::Decode(message)) => {
            assert!(message.contains("分辨率"), "message explains it: {message}");
            assert!(
                message.contains("8x6") && message.contains("4x4"),
                "{message}"
            );
        }
        other => panic!("expected a decode error, got {other:?}"),
    }
}

#[test]
fn empty_metadata_is_an_error() {
    let zip = build_zip(&[("000000.png".to_owned(), png_bytes(&solid(8, 6, RED)))]);
    assert!(matches!(
        UgoiraFilm::decode_zip(&zip, &[]),
        Err(UgoiraError::Zip(_))
    ));
}

#[test]
fn footprint_ceiling_rejects_huge_films_without_overflow() {
    // A full-HD 12-frame film is ~95 MiB: comfortably inside the ceiling.
    assert!(super::footprint_fits(1920, 1080, 12));
    // The 600x600 tier at 370 frames is ~508 MiB (inside); 400 is ~549 MiB.
    assert!(super::footprint_fits(600, 600, 370));
    assert!(!super::footprint_fits(600, 600, 400));
    // 4096x4096 RGBA8 is exactly 64 MiB per frame: 8 frames sit on the ceiling,
    // 9 frames (576 MiB) is refused.
    assert!(super::footprint_fits(4096, 4096, 8));
    assert!(!super::footprint_fits(4096, 4096, 9));
    // Hostile headers saturate instead of overflowing.
    assert!(!super::footprint_fits(u32::MAX, u32::MAX, u64::MAX));
    assert!(!super::footprint_fits(u32::MAX, u32::MAX, 2));
    assert_eq!(MAX_DECODED_BYTES, 512 * 1024 * 1024);
}

#[test]
fn export_gif_roundtrips_frame_count() {
    let film = film(&[200, 100, 300]);
    let gif = film.export_gif().expect("gif export");

    assert!(
        gif.starts_with(b"GIF89a") || gif.starts_with(b"GIF87a"),
        "unexpected header {:?}",
        &gif[..6.min(gif.len())]
    );

    let decoder = GifDecoder::new(Cursor::new(gif.as_slice())).expect("gif header decodes");
    let frames = decoder.into_frames().collect_frames().expect("gif frames");
    assert_eq!(frames.len(), 3, "every film frame survives the round trip");
    assert_eq!(frames[0].buffer().dimensions(), (8, 6));
}

#[test]
fn export_gif_rounds_delays_to_centisecond_grid() {
    // 3 ms must not become instantaneous; 15 ms rounds down; 250 ms is exact.
    let film = film(&[3, 15, 250]);
    let gif = film.export_gif().expect("gif export");
    let decoder = GifDecoder::new(Cursor::new(gif.as_slice())).expect("gif header decodes");
    let frames = decoder.into_frames().collect_frames().expect("gif frames");

    let delays: Vec<(u32, u32)> = frames.iter().map(|f| f.delay().numer_denom_ms()).collect();
    assert_eq!(delays, [(10, 1), (10, 1), (250, 1)]);
}

#[test]
fn export_gif_rejects_an_empty_film() {
    let empty = UgoiraFilm {
        width: 8,
        height: 6,
        frames: Vec::new(),
    };
    assert!(matches!(empty.export_gif(), Err(UgoiraError::Encode(_))));
}

#[test]
fn export_gif_rejects_a_frame_with_the_wrong_buffer_length() {
    let broken = UgoiraFilm {
        width: 8,
        height: 6,
        frames: vec![super::FilmFrame {
            file: "000000.png".to_owned(),
            delay_ms: 100,
            rgba: vec![0; 7],
        }],
    };
    assert!(matches!(broken.export_gif(), Err(UgoiraError::Encode(_))));
}

#[test]
fn live_fixture_shape_drives_a_real_decode() {
    let meta = fixture_meta();

    // The shape the endpoint produces: 12 JPEG frames of 200 ms each.
    assert_eq!(meta.frames.len(), 12);
    assert_eq!(meta.frames[0].file, "000000.jpg");
    assert_eq!(meta.frames[0].delay_ms, 200);
    assert_eq!(meta.frames[11].file, "000011.jpg");
    assert_eq!(meta.total_duration_ms(), 2400);
    assert!(meta.mime_type.contains("jpeg"), "mime {}", meta.mime_type);
    assert!(meta.best_zip().ends_with("151939293_ugoira1920x1080.zip"));

    // Same names, same delays, real JPEG payloads: the declared shape is exactly
    // what decode_zip consumes.
    let entries: Vec<(String, Vec<u8>)> = meta
        .frames
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            let colour = [RED, GREEN, BLUE][index % 3];
            (frame.file.clone(), jpeg_bytes(&solid(32, 32, colour)))
        })
        .collect();
    let zip = build_zip(&entries);

    let film = UgoiraFilm::decode_zip(&zip, &meta.frames).expect("fixture-shaped archive decodes");
    assert_eq!(film.frames.len(), 12);
    assert_eq!((film.width, film.height), (32, 32));
    assert_eq!(film.total_duration_ms(), 2400);
    let names: Vec<&str> = film.frames.iter().map(|f| f.file.as_str()).collect();
    let expected: Vec<&str> = meta.frames.iter().map(|f| f.file.as_str()).collect();
    assert_eq!(names, expected);
    assert!(film.frames.iter().all(|f| f.delay_ms == 200));
    // 2400 ms loop: last frame owns 2200..2400, then wraps.
    assert_eq!(film.frame_index_at(2199), 10);
    assert_eq!(film.frame_index_at(2399), 11);
    assert_eq!(film.frame_index_at(2400), 0);
}
