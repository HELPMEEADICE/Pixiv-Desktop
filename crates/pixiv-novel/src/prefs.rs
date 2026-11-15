//! Reader preferences: the whole appearance surface as one serialisable value.
//!
//! # The rule this module enforces
//!
//! A stored preference can never make the reader unusable. Everything that comes
//! out of SQLite is untrusted: a row can be corrupt, hand-edited, or written by a
//! build that had a field this one does not. So every value passes through
//! [`ReaderPrefs::clamped`], which pins each number into the range the layout can
//! actually draw and replaces any string that is not a colour with the built-in
//! one. The failure mode is a slightly different page, never a blank window and
//! never a panic.
//!
//! # Why the clamps live here and not in the UI
//!
//! The UI writes the value, the store persists it, and a future UI reads it back
//! on a different DPI. Putting the range in one place means a slider cannot
//! produce a value that the reader refuses, and the parser's ruby mode, the
//! theme the window paints and the fonts the text run uses all agree on the same
//! limits.

use serde::{Deserialize, Serialize};

use crate::parse::RubyMode;

/// Font size in logical pixels: the range the reader draws, and the default.
pub const MIN_FONT_SIZE: f32 = 14.0;
pub const MAX_FONT_SIZE: f32 = 32.0;
pub const DEFAULT_FONT_SIZE: f32 = 19.0;

/// Weight is only offered at the four steps a variable font actually has.
pub const MIN_FONT_WEIGHT: u16 = 300;
pub const MAX_FONT_WEIGHT: u16 = 700;
pub const DEFAULT_FONT_WEIGHT: u16 = 400;

/// Multiplier on the font size.
pub const MIN_LINE_HEIGHT: f32 = 1.2;
pub const MAX_LINE_HEIGHT: f32 = 3.0;
pub const DEFAULT_LINE_HEIGHT: f32 = 1.8;

/// Extra tracking, in pixels.
pub const MIN_LETTER_SPACING: f32 = 0.0;
pub const MAX_LETTER_SPACING: f32 = 4.0;
pub const DEFAULT_LETTER_SPACING: f32 = 0.0;

/// Gap between blocks, in pixels.
pub const MIN_PARAGRAPH_SPACING: f32 = 0.0;
pub const MAX_PARAGRAPH_SPACING: f32 = 40.0;
pub const DEFAULT_PARAGRAPH_SPACING: f32 = 12.0;

/// First-line indent, in em (a fraction of the font size).
pub const MIN_FIRST_LINE_INDENT: f32 = 0.0;
pub const MAX_FIRST_LINE_INDENT: f32 = 3.0;
pub const DEFAULT_FIRST_LINE_INDENT: f32 = 0.0;

/// Page margin on each side, in pixels.
pub const MIN_MARGIN_X: f32 = 12.0;
pub const MAX_MARGIN_X: f32 = 200.0;
pub const DEFAULT_MARGIN_X: f32 = 48.0;

/// Measure: the widest the text column may grow, so a 4K window does not turn a
/// paragraph into a single 300-character line.
pub const MIN_MAX_WIDTH: f32 = 420.0;
pub const MAX_MAX_WIDTH: f32 = 1400.0;
pub const DEFAULT_MAX_WIDTH: f32 = 720.0;

/// Shown to the reader when the custom theme is selected and no valid colour was
/// stored. Warm paper rather than pure white: it is the setting the reader
/// starts from, and a full-white page is the one thing readers complain about.
pub const DEFAULT_BACKGROUND: &str = "#f7f3e9";
pub const DEFAULT_INK: &str = "#2b2b2b";

/// Horizontal alignment of a block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

/// How two blocks are separated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParagraphRule {
    /// Blank space, the default: least ink for the reader to skip.
    #[default]
    Space,
    /// A hairline between blocks, for dense web-style novels.
    Rule,
    /// Nothing at all, so the indent carries the break.
    None,
}

/// Page colours.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReaderTheme {
    /// Follow the OS. Slint reports the scheme; this crate only records the
    /// intent, because the decision needs the window.
    #[default]
    System,
    Light,
    Dark,
    Sepia,
    /// Uses [`ReaderPrefs::custom_background`] and [`ReaderPrefs::custom_ink`].
    Custom,
}

/// Everything the reader's appearance depends on.
///
/// `#[serde(default)]` is deliberate: a value written by an older build is
/// missing the fields added later, and a missing field must take its default
/// rather than reject the whole struct and reset the reader's settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReaderPrefs {
    /// Empty means "follow the application default font".
    pub font_family: String,
    pub font_size: f32,
    pub font_weight: u16,
    /// Italic is a genuine reader preference for long-form Japanese text.
    pub italic: bool,
    pub line_height: f32,
    pub letter_spacing: f32,
    pub paragraph_spacing: f32,
    /// In em, so it scales with `font_size` instead of being a fixed pixel gap.
    pub first_line_indent: f32,
    pub margin_x: f32,
    pub max_width: f32,
    pub align: Align,
    pub paragraph_rule: ParagraphRule,
    pub theme: ReaderTheme,
    /// `#RRGGBB` or `#AARRGGBB`; anything else falls back to
    /// [`DEFAULT_BACKGROUND`].
    pub custom_background: String,
    pub custom_ink: String,
    pub ruby: RubyMode,
    /// Hide the reader's toolbar so the page is the only thing on screen.
    pub hide_chrome: bool,
    pub show_progress: bool,
    /// One page at a time instead of a continuous scroll.
    pub paged: bool,
}

impl ReaderPrefs {
    /// The built-in default, usable in `const` position.
    ///
    /// The two colour fields are left empty on purpose. Rust 1.85 — the version
    /// this workspace declares — cannot build a [`String`] from a literal inside
    /// a `const fn`, and an empty colour already means "use the built-in one"
    /// here, the same rule `font_family` uses for "follow the app default". So
    /// [`clamped`](Self::clamped) fills them in and [`Default::default`] is this
    /// value with the colours already resolved; every other field is final.
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            font_family: String::new(),
            font_size: DEFAULT_FONT_SIZE,
            font_weight: DEFAULT_FONT_WEIGHT,
            italic: false,
            line_height: DEFAULT_LINE_HEIGHT,
            letter_spacing: DEFAULT_LETTER_SPACING,
            paragraph_spacing: DEFAULT_PARAGRAPH_SPACING,
            first_line_indent: DEFAULT_FIRST_LINE_INDENT,
            margin_x: DEFAULT_MARGIN_X,
            max_width: DEFAULT_MAX_WIDTH,
            align: Align::Start,
            paragraph_rule: ParagraphRule::Space,
            theme: ReaderTheme::System,
            custom_background: String::new(),
            custom_ink: String::new(),
            ruby: RubyMode::Inline,
            hide_chrome: false,
            show_progress: true,
            paged: false,
        }
    }

    /// The same value with every field pinned into its usable range.
    ///
    /// Called on every load, on every save, and after every UI edit. It is cheap
    /// (one pass over twenty fields) and it is the only thing standing between a
    /// corrupt row and an unreadable page.
    #[must_use]
    pub fn clamped(&self) -> Self {
        Self {
            font_family: self.font_family.trim().to_string(),
            font_size: bound(
                self.font_size,
                MIN_FONT_SIZE,
                MAX_FONT_SIZE,
                DEFAULT_FONT_SIZE,
            ),
            font_weight: snap_weight(self.font_weight),
            italic: self.italic,
            line_height: bound(
                self.line_height,
                MIN_LINE_HEIGHT,
                MAX_LINE_HEIGHT,
                DEFAULT_LINE_HEIGHT,
            ),
            letter_spacing: bound(
                self.letter_spacing,
                MIN_LETTER_SPACING,
                MAX_LETTER_SPACING,
                DEFAULT_LETTER_SPACING,
            ),
            paragraph_spacing: bound(
                self.paragraph_spacing,
                MIN_PARAGRAPH_SPACING,
                MAX_PARAGRAPH_SPACING,
                DEFAULT_PARAGRAPH_SPACING,
            ),
            first_line_indent: bound(
                self.first_line_indent,
                MIN_FIRST_LINE_INDENT,
                MAX_FIRST_LINE_INDENT,
                DEFAULT_FIRST_LINE_INDENT,
            ),
            margin_x: bound(self.margin_x, MIN_MARGIN_X, MAX_MARGIN_X, DEFAULT_MARGIN_X),
            max_width: bound(
                self.max_width,
                MIN_MAX_WIDTH,
                MAX_MAX_WIDTH,
                DEFAULT_MAX_WIDTH,
            ),
            align: self.align,
            paragraph_rule: self.paragraph_rule,
            theme: self.theme,
            custom_background: colour_or(&self.custom_background, DEFAULT_BACKGROUND),
            custom_ink: colour_or(&self.custom_ink, DEFAULT_INK),
            ruby: self.ruby,
            hide_chrome: self.hide_chrome,
            show_progress: self.show_progress,
            paged: self.paged,
        }
    }

    /// Serialise for storage, clamped first so an out-of-range value can never
    /// reach the database.
    ///
    /// Serialising this struct cannot fail (no maps with non-string keys, no
    /// floats that JSON cannot represent). If it somehow did, an empty string is
    /// returned, which [`from_json`](Self::from_json) reads back as "nothing
    /// stored" — the reader keeps working.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.clamped()).unwrap_or_default()
    }

    /// Read a stored value, clamped, falling back to the defaults on any error.
    ///
    /// This is the whole point of the module: a truncated row, a row written by
    /// a newer build, or a value someone edited by hand produces a working
    /// reader rather than an error the UI has to handle.
    #[must_use]
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str::<Self>(s)
            .map(|prefs| prefs.clamped())
            .unwrap_or_else(|_| Self::default())
    }
}

impl Default for ReaderPrefs {
    fn default() -> Self {
        Self {
            custom_background: DEFAULT_BACKGROUND.to_string(),
            custom_ink: DEFAULT_INK.to_string(),
            ..Self::defaults()
        }
    }
}

/// `#RRGGBB` or `#AARRGGBB`, and nothing else.
///
/// Three-digit shorthand is rejected on purpose: the reader also accepts those in
/// its colour picker's output, and a value that round-trips through this check
/// must mean exactly one colour.
#[must_use]
pub fn is_colour(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 7 && bytes.len() != 9 {
        return false;
    }
    if bytes[0] != b'#' {
        return false;
    }
    bytes[1..].iter().all(u8::is_ascii_hexdigit)
}

/// A finite number inside its range, or the documented default when it is NaN —
/// `f32::clamp` passes NaN straight through, and NaN scrolling offsets poison
/// every layout computation downstream.
fn bound(value: f32, min: f32, max: f32, default: f32) -> f32 {
    if value.is_nan() {
        return default;
    }
    value.clamp(min, max)
}

/// Round to the nearest hundred and pin into 300..=700.
fn snap_weight(weight: u16) -> u16 {
    let snapped = (i32::from(weight) + 50) / 100 * 100;
    let clamped = snapped.clamp(i32::from(MIN_FONT_WEIGHT), i32::from(MAX_FONT_WEIGHT));
    u16::try_from(clamped).unwrap_or(DEFAULT_FONT_WEIGHT)
}

fn colour_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if is_colour(trimmed) {
        trimmed.to_string()
    } else {
        fallback.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_the_documented_values() {
        let prefs = ReaderPrefs::default();
        assert_eq!(prefs.font_family, "");
        assert_eq!(prefs.font_size, 19.0);
        assert_eq!(prefs.font_weight, 400);
        assert!(!prefs.italic);
        assert_eq!(prefs.line_height, 1.8);
        assert_eq!(prefs.letter_spacing, 0.0);
        assert_eq!(prefs.paragraph_spacing, 12.0);
        assert_eq!(prefs.first_line_indent, 0.0);
        assert_eq!(prefs.margin_x, 48.0);
        assert_eq!(prefs.max_width, 720.0);
        assert_eq!(prefs.align, Align::Start);
        assert_eq!(prefs.paragraph_rule, ParagraphRule::Space);
        assert_eq!(prefs.theme, ReaderTheme::System);
        assert_eq!(prefs.custom_background, "#f7f3e9");
        assert_eq!(prefs.custom_ink, "#2b2b2b");
        assert_eq!(prefs.ruby, RubyMode::Inline);
        assert!(!prefs.hide_chrome);
        assert!(prefs.show_progress);
        assert!(!prefs.paged);

        // The default is already inside every range, so clamping is a no-op.
        assert_eq!(prefs.clamped(), prefs);
    }

    #[test]
    fn the_const_default_leaves_the_colours_to_clamped() {
        // Documented in `defaults`: a const fn cannot build a String from a
        // literal at the workspace MSRV, so the const form carries the marker for
        // "use the built-in colour" and clamped() resolves it.
        let skeleton = ReaderPrefs::defaults();
        assert!(skeleton.custom_background.is_empty());
        assert!(skeleton.custom_ink.is_empty());
        assert_eq!(skeleton.clamped().custom_background, DEFAULT_BACKGROUND);
        assert_eq!(skeleton.clamped().custom_ink, DEFAULT_INK);
        // Everything other than the two colours is already final.
        assert_eq!(
            ReaderPrefs {
                custom_background: DEFAULT_BACKGROUND.to_string(),
                custom_ink: DEFAULT_INK.to_string(),
                ..skeleton
            },
            ReaderPrefs::default()
        );
    }

    #[test]
    fn clamped_pins_every_number_into_range() {
        let high = ReaderPrefs {
            font_size: 1_000.0,
            font_weight: 50,
            line_height: 99.0,
            letter_spacing: -5.0,
            paragraph_spacing: 900.0,
            first_line_indent: -1.0,
            margin_x: 0.0,
            max_width: 10_000.0,
            ..ReaderPrefs::default()
        }
        .clamped();
        assert_eq!(high.font_size, MAX_FONT_SIZE);
        assert_eq!(high.font_weight, MIN_FONT_WEIGHT);
        assert_eq!(high.line_height, MAX_LINE_HEIGHT);
        assert_eq!(high.letter_spacing, MIN_LETTER_SPACING);
        assert_eq!(high.paragraph_spacing, MAX_PARAGRAPH_SPACING);
        assert_eq!(high.first_line_indent, MIN_FIRST_LINE_INDENT);
        assert_eq!(high.margin_x, MIN_MARGIN_X);
        assert_eq!(high.max_width, MAX_MAX_WIDTH);

        let low = ReaderPrefs {
            font_size: 1.0,
            line_height: 0.1,
            margin_x: -100.0,
            max_width: 10.0,
            ..ReaderPrefs::default()
        }
        .clamped();
        assert_eq!(low.font_size, MIN_FONT_SIZE);
        assert_eq!(low.line_height, MIN_LINE_HEIGHT);
        assert_eq!(low.margin_x, MIN_MARGIN_X);
        assert_eq!(low.max_width, MIN_MAX_WIDTH);
    }

    #[test]
    fn nan_and_infinity_never_survive_clamping() {
        let prefs = ReaderPrefs {
            font_size: f32::NAN,
            line_height: f32::INFINITY,
            margin_x: f32::NEG_INFINITY,
            letter_spacing: f32::NAN,
            ..ReaderPrefs::default()
        }
        .clamped();
        // NaN would poison every layout computation downstream, so it becomes the
        // documented default instead of being clamped.
        assert_eq!(prefs.font_size, DEFAULT_FONT_SIZE);
        assert_eq!(prefs.letter_spacing, DEFAULT_LETTER_SPACING);
        assert_eq!(prefs.line_height, MAX_LINE_HEIGHT);
        assert_eq!(prefs.margin_x, MIN_MARGIN_X);
        assert!(prefs.font_size.is_finite());
    }

    #[test]
    fn weight_snaps_to_the_nearest_hundred() {
        let cases = [
            (400u16, 400u16),
            (449, 400),
            (450, 500),
            (300, 300),
            (301, 300),
            (699, 700),
            (700, 700),
            (1_000, 700),
            (0, 300),
            (u16::MAX, 700),
        ];
        for (stored, expected) in cases {
            let prefs = ReaderPrefs {
                font_weight: stored,
                ..ReaderPrefs::default()
            }
            .clamped();
            assert_eq!(prefs.font_weight, expected, "stored {stored}");
        }
    }

    #[test]
    fn colour_validation_accepts_rrggbb_and_aarrggbb_only() {
        assert!(is_colour("#f7f3e9"));
        assert!(is_colour("#AABBCCDD"));
        assert!(!is_colour("#fff"));
        assert!(!is_colour("#f7f3e"));
        assert!(!is_colour(" #f7f3e9"));
        assert!(!is_colour("#f7f3eg"));
        assert!(!is_colour("red"));
        assert!(!is_colour(""));
    }

    #[test]
    fn an_invalid_colour_falls_back_to_the_built_in_one() {
        for bad in [
            "",
            " ",
            "red",
            "#fff",
            "#f7f3e",
            "#f7f3e99",
            "f7f3e9",
            "#GGGGGG",
            "rgb(1,2,3)",
        ] {
            let prefs = ReaderPrefs {
                custom_background: bad.to_string(),
                custom_ink: bad.to_string(),
                ..ReaderPrefs::default()
            }
            .clamped();
            assert_eq!(
                prefs.custom_background, DEFAULT_BACKGROUND,
                "colour {bad:?}"
            );
            assert_eq!(prefs.custom_ink, DEFAULT_INK, "colour {bad:?}");
        }

        for good in ["#000000", "#ffffff", "#123456", "#80FF00AA", "#ff000080"] {
            let prefs = ReaderPrefs {
                custom_background: good.to_string(),
                custom_ink: good.to_string(),
                ..ReaderPrefs::default()
            }
            .clamped();
            assert_eq!(prefs.custom_background, good);
            assert_eq!(prefs.custom_ink, good);
        }

        // Whitespace around a valid colour is trimmed, not treated as invalid.
        let padded = ReaderPrefs {
            custom_background: "  #123456 ".to_string(),
            ..ReaderPrefs::default()
        }
        .clamped();
        assert_eq!(padded.custom_background, "#123456");
    }

    #[test]
    fn the_font_family_is_trimmed_so_a_blank_one_means_default() {
        let prefs = ReaderPrefs {
            font_family: "  Noto Serif JP  ".to_string(),
            ..ReaderPrefs::default()
        }
        .clamped();
        assert_eq!(prefs.font_family, "Noto Serif JP");

        let blank = ReaderPrefs {
            font_family: "   ".to_string(),
            ..ReaderPrefs::default()
        }
        .clamped();
        assert_eq!(blank.font_family, "");
    }

    #[test]
    fn json_roundtrips_every_field() {
        let prefs = ReaderPrefs {
            font_family: "Noto Serif JP".to_string(),
            font_size: 22.5,
            font_weight: 500,
            italic: true,
            line_height: 2.1,
            letter_spacing: 0.5,
            paragraph_spacing: 20.0,
            first_line_indent: 1.0,
            margin_x: 64.0,
            max_width: 900.0,
            align: Align::Center,
            paragraph_rule: ParagraphRule::Rule,
            theme: ReaderTheme::Sepia,
            custom_background: "#101010".to_string(),
            custom_ink: "#f0f0f0".to_string(),
            ruby: RubyMode::Hidden,
            hide_chrome: true,
            show_progress: false,
            paged: true,
        };
        assert_eq!(ReaderPrefs::from_json(&prefs.to_json()), prefs);
    }

    #[test]
    fn corrupt_or_foreign_json_falls_back_instead_of_failing() {
        for bad in [
            "",
            "not json",
            "{",
            "[]",
            "null",
            "17",
            "{}",
            r#"{"font_size":"big"}"#,
            r#"{"theme":"neon"}"#,
            r#"{"ruby":null}"#,
        ] {
            let prefs = ReaderPrefs::from_json(bad);
            assert_eq!(prefs, ReaderPrefs::default(), "input {bad:?}");
            assert_eq!(prefs.font_size, DEFAULT_FONT_SIZE);
        }
    }

    #[test]
    fn an_out_of_range_stored_value_is_clamped_on_the_way_in() {
        let stored = r#"{"font_size":900.0,"font_weight":900,"custom_background":"blue"}"#;
        let prefs = ReaderPrefs::from_json(stored);
        assert_eq!(prefs.font_size, MAX_FONT_SIZE);
        assert_eq!(prefs.font_weight, MAX_FONT_WEIGHT);
        assert_eq!(prefs.custom_background, DEFAULT_BACKGROUND);

        // And nothing out of range is ever written back out.
        let wild = ReaderPrefs {
            font_size: 900.0,
            ..ReaderPrefs::default()
        };
        let json = wild.to_json();
        assert!(json.contains("32.0"), "{json}");
        assert_eq!(ReaderPrefs::from_json(&json).font_size, MAX_FONT_SIZE);
    }

    #[test]
    fn a_value_from_an_older_build_keeps_its_known_fields() {
        // #[serde(default)] on the struct: absent fields take their default
        // instead of failing the whole row.
        let prefs = ReaderPrefs::from_json(r#"{"font_size":24.0,"theme":"dark"}"#);
        assert_eq!(prefs.font_size, 24.0);
        assert_eq!(prefs.theme, ReaderTheme::Dark);
        assert_eq!(prefs.line_height, DEFAULT_LINE_HEIGHT);
        assert_eq!(prefs.custom_background, DEFAULT_BACKGROUND);
        assert!(prefs.show_progress);
    }

    #[test]
    fn enums_use_lowercase_on_the_wire() {
        assert_eq!(serde_json::to_string(&Align::End).unwrap(), r#""end""#);
        assert_eq!(
            serde_json::to_string(&ParagraphRule::None).unwrap(),
            r#""none""#
        );
        assert_eq!(
            serde_json::to_string(&ReaderTheme::Sepia).unwrap(),
            r#""sepia""#
        );
        assert_eq!(
            serde_json::to_string(&RubyMode::Inline).unwrap(),
            r#""inline""#
        );

        assert_eq!(Align::default(), Align::Start);
        assert_eq!(ParagraphRule::default(), ParagraphRule::Space);
        assert_eq!(ReaderTheme::default(), ReaderTheme::System);
    }
}
