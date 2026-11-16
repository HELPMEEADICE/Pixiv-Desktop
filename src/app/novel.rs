//! The novel surfaces: search, the novel page, and the reader.
//!
//! # What this module owns
//!
//! * three page models - novels (search + recent reading), novel (one work) and
//!   reader (the body) - as one NovelState;
//! * the bridge from Pixiv's markup to Slint rows, which is the only place that
//!   knows both, via pixiv_novel::parse;
//! * the reading preferences, their persistence and the one mapping from
//!   ReaderPrefs to the ReaderView the page draws.
//!
//! # Why the preference mapping lives here and not in Slint
//!
//! The settings panel writes ratios (0..1). The clamps live in
//! pixiv_novel::prefs, the colours here, and the persistence in pixiv_store.
//! Rust is the only layer that can see all three, so it is the only place the
//! three can be kept in step: the page receives a fully resolved ReaderView and
//! never computes a value it would have to clamp again.
//!
//! # Progress
//!
//! The anchor is the scroll ratio, not a block index and not a pixel offset.
//! Slint cannot report which block is at the top of the viewport - a row has no
//! position until it is laid out, and there is no query for it - but it can report
//! the scroll offset and the content height, and their quotient survives a font
//! change, which a pixel offset does not. The stored block_index and char_offset
//! are derived from the same quotient so a half-read chapter reads "42%" in the
//! history list, and so a caller that later gains real geometry has a place to put
//! it.

use super::*;

use pixiv_novel::parse::{self, BlockKind, Document, Span};
use pixiv_novel::prefs::{self as novel_prefs, ReaderPrefs};
use pixiv_store::{NovelHistoryEntry, NovelProgress};
use pixiv_types::domain::{Novel, NovelSeries, NovelSummary, SeriesChapter};

use crate::route::Route;

/// Setting key the reader's preferences are stored under.
const PREFS_KEY: &str = "novel.reader.prefs";
/// How many recent reads the novels page shows.
const RECENT_KEEP: usize = 12;
/// A progress write rewrites a row, so the reader writes at most this often while
/// the user scrolls. Leaving the page and closing the app force one.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(2_000);
/// Preference writes are cheap but frequent (a slider drag is ~60 events/s).
const PREFS_INTERVAL: Duration = Duration::from_millis(400);
/// Reader block kinds, mirroring ui/novel-model.slint.
const K_PARA: i32 = 0;
const K_HEAD: i32 = 1;
const K_RULE: i32 = 2;
const K_BREAK: i32 = 3;
const K_IMAGE: i32 = 4;
const K_WORK: i32 = 5;
const K_LINK: i32 = 6;

/// Everything the three novel surfaces need.
pub(crate) struct NovelState {
    /// Search results, as rows for the page.
    pub(crate) rows: Rc<VecModel<NovelRow>>,
    /// Recent reads, newest first.
    pub(crate) recent: Rc<VecModel<NovelRow>>,
    /// Series chapters of the novel on the landing page.
    pub(crate) chapters: Rc<VecModel<NovelChapter>>,
    /// The reader's body.
    pub(crate) blocks: Rc<VecModel<NovelBlock>>,
    /// Search results in domain form, for paging and for opening by id.
    pub(crate) summaries: Vec<NovelSummary>,
    /// 0 content, 1 loading, 2 empty, 3 error.
    pub(crate) state: i32,
    pub(crate) message: String,
    pub(crate) word: String,
    pub(crate) page: u32,
    pub(crate) last_page: u32,
    /// One page at a time (B8): a second "load more" while one is in flight used to
    /// bump the cursor again, so two answers raced and the page that arrived second
    /// replaced the page that arrived first.
    pub(crate) loading_more: bool,
    /// Bumped whenever the list is replaced, so a slow search cannot patch a newer one.
    pub(crate) epoch: u64,

    /// The landing page's value.
    pub(crate) view: NovelView,
    /// The work the landing page shows, once loaded.
    pub(crate) novel: Option<Novel>,
    /// Its series, when it belongs to one.
    pub(crate) series: Option<NovelSeries>,
    /// Chapter ids in order, so reader-chapter(-1) means something.
    pub(crate) chapter_ids: Vec<u64>,
    pub(crate) chapter_index: usize,

    /// The parsed body, kept so a preference edit re-renders without a refetch.
    pub(crate) document: Option<Document>,
    pub(crate) prefs: ReaderPrefs,
    /// The reader page's value, derived from prefs + the document.
    pub(crate) reader: ReaderView,
    /// Last ratio reported by the page.
    pub(crate) ratio: f32,
    pub(crate) saved_at: Option<std::time::Instant>,
    pub(crate) prefs_saved_at: Option<std::time::Instant>,
    /// Decoded novel images by URL (covers and embedded illustrations).
    pub(crate) images: HashMap<String, slint::Image>,
    /// Which body rows show which image URL, built with the body (B9).
    ///
    /// An arrival used to walk all 800 rows, clone each one and search the image
    /// table linearly for the row's id; the index turns that into one row per
    /// reference, which for a body with one illustration is one row.
    pub(crate) block_images: HashMap<String, Vec<usize>>,
    /// The id -> URL table the body was built from, kept so a reflow rebuilds the
    /// index without asking the network for anything.
    pub(crate) image_sources: std::collections::BTreeMap<String, parse::ImageInfo>,
    pub(crate) loading: HashSet<String>,
    /// The novel the reader currently holds.
    pub(crate) reader_novel: Option<u64>,
}

impl NovelState {
    pub(crate) fn new() -> Self {
        let prefs = ReaderPrefs::default();
        Self {
            rows: Rc::new(VecModel::from(Vec::new())),
            recent: Rc::new(VecModel::from(Vec::new())),
            chapters: Rc::new(VecModel::from(Vec::new())),
            blocks: Rc::new(VecModel::from(Vec::new())),
            summaries: Vec::new(),
            state: 2,
            message: String::new(),
            word: String::new(),
            page: 1,
            last_page: 1,
            loading_more: false,
            epoch: 0,
            view: empty_view(),
            novel: None,
            series: None,
            chapter_ids: Vec::new(),
            chapter_index: 0,
            document: None,
            reader: reader_view(
                &prefs,
                1,
                "",
                &ReaderDoc {
                    title: "",
                    chapter_meta: "",
                    prev: "",
                    next: "",
                },
                false,
                false,
            ),
            prefs,
            ratio: 0.0,
            saved_at: None,
            prefs_saved_at: None,
            images: HashMap::new(),
            block_images: HashMap::new(),
            image_sources: std::collections::BTreeMap::new(),
            loading: HashSet::new(),
            reader_novel: None,
        }
    }
}

/// What one settings-panel edit did to the reader.
///
/// The three answers are the whole O5 split (plan R10 §2.3): most opcodes change
/// what the `ReaderView` resolves to and nothing else, so the block model must be
/// left alone; two of them are flattened into the rows' own text, so the model has
/// to be replaced. `Reflowed` is the *only* path that touches `blocks` inside
/// `NovelState::apply_edit`, which is what makes counting it the same thing as
/// counting `rows_from` + `set_vec`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReaderEdit {
    /// An opcode this build does not have: no preference, no row and no store
    /// write changed (O4).
    Ignored,
    /// The resolved view changed; the body rows did not.
    Restyled,
    /// The rows themselves changed, so the block model was rebuilt and replaced.
    Reflowed,
}

/// Whether one settings opcode changes the rows' own text.
///
/// Exactly two do, because `rows_from` bakes them into the string it hands
/// Slint: the ruby mode ("base（reading）" vs "base") and the first-line indent
/// (a run of full-width spaces in front of each paragraph). Everything else —
/// size, line height, tracking, weight, family, italic, alignment, separator,
/// measure, margins, colours, paging, chrome, progress — is drawn by Slint from
/// the resolved `ReaderView`, so rebuilding the model for it re-creates text that
/// did not change. That was O5: changing the theme colour reset an 800-block
/// model.
fn reader_op_reflows(op: i32) -> bool {
    matches!(op, 8 | 15)
}

impl NovelState {
    /// Build the body rows from the document this state already holds.
    ///
    /// The one place rows are produced: the mount (`App::adopt_document`) and the
    /// two reflowing opcodes, and nothing else. A colour edit therefore has nothing
    /// to call.
    /// Take the "load more" slot and move the cursor, or refuse this click (B8).
    ///
    /// Returns false when a page is already in flight or the list has ended: the
    /// cursor used to be bumped unconditionally, so two clicks asked for page 3
    /// twice and the two answers raced into the same model.
    pub(crate) fn take_more(&mut self) -> bool {
        if self.loading_more || self.page >= self.last_page {
            return false;
        }
        self.loading_more = true;
        self.page = self.page.saturating_add(1);
        true
    }

    /// Free the slot. A page that did not arrive puts the cursor back, because the
    /// next attempt has to ask for the page that failed rather than skip it.
    pub(crate) fn release_more(&mut self, landed: bool) {
        self.loading_more = false;
        if !landed {
            self.page = self.page.saturating_sub(1).max(1);
        }
    }

    pub(crate) fn build_body(&mut self) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let rows = rows_from(document, &self.prefs);
        // One pass, once per body: the arrival path reads this instead of the body (B9).
        self.block_images = block_image_index(&rows, &self.image_sources);
        self.blocks.set_vec(rows);
    }

    /// The URLs the built body needs decoded, in row order.
    ///
    /// Derived from the model rather than from the rows before they are pushed, so
    /// there is one row list and one order (O5 moved the rebuild inside the state).
    fn image_urls(
        &self,
        images: &std::collections::BTreeMap<String, parse::ImageInfo>,
    ) -> Vec<String> {
        self.blocks
            .iter()
            .filter(|row| row.kind == K_IMAGE)
            .filter_map(|row| {
                let info = images.get(row.id.as_str())?;
                let url = if info.large.is_empty() {
                    info.small.clone()
                } else {
                    info.large.clone()
                };
                if url.is_empty() { None } else { Some(url) }
            })
            .collect()
    }

    /// One settings-panel edit, applied to the preference model.
    ///
    /// The opcodes are the contract with ui/pages/novel-reader.slint's
    /// ReaderSettings component, and values arrive as 0..1 ratios; the clamp lives
    /// in the preference model, so an extreme slider position can only produce a
    /// value it already allows.
    ///
    /// The answer says what the edit cost (see [`ReaderEdit`]), which is what keeps
    /// a colour change from rebuilding the body.
    ///
    /// An unknown opcode changes **nothing** and answers `Ignored`, so the caller
    /// writes no row and rebuilds no rows (plan R10 §2.3 O4). Guessing
    /// `ReaderPrefs::default()` here — which is what this used to do — reset all
    /// nineteen settings and persisted the reset over the user's row, so a panel
    /// from a newer build talking to an older one would silently wipe the
    /// reader's appearance. Being inert is the only answer that cannot lose data.
    pub(crate) fn apply_edit(&mut self, op: i32, value: f32, text: &str) -> ReaderEdit {
        let mut prefs = self.prefs.clone();
        match op {
            0 => prefs.theme = theme_from(value.round() as i32),
            1 => prefs.custom_background = text.trim().to_string(),
            2 => prefs.custom_ink = text.trim().to_string(),
            3 => prefs.font_family = text.trim().to_string(),
            4 => {
                prefs.font_size = value_of(
                    value,
                    novel_prefs::MIN_FONT_SIZE,
                    novel_prefs::MAX_FONT_SIZE,
                )
            }
            5 => {
                prefs.line_height = value_of(
                    value,
                    novel_prefs::MIN_LINE_HEIGHT,
                    novel_prefs::MAX_LINE_HEIGHT,
                )
            }
            6 => {
                prefs.letter_spacing = value_of(
                    value,
                    novel_prefs::MIN_LETTER_SPACING,
                    novel_prefs::MAX_LETTER_SPACING,
                )
            }
            7 => {
                prefs.paragraph_spacing = value_of(
                    value,
                    novel_prefs::MIN_PARAGRAPH_SPACING,
                    novel_prefs::MAX_PARAGRAPH_SPACING,
                )
            }
            8 => {
                prefs.first_line_indent = value_of(
                    value,
                    novel_prefs::MIN_FIRST_LINE_INDENT,
                    novel_prefs::MAX_FIRST_LINE_INDENT,
                )
            }
            9 => {
                prefs.margin_x =
                    value_of(value, novel_prefs::MIN_MARGIN_X, novel_prefs::MAX_MARGIN_X)
            }
            10 => {
                prefs.max_width = value_of(
                    value,
                    novel_prefs::MIN_MAX_WIDTH,
                    novel_prefs::MAX_MAX_WIDTH,
                )
            }
            11 => {
                let index = value.round() as i32;
                prefs.font_weight = WEIGHTS[index.clamp(0, 2) as usize];
            }
            12 => prefs.italic = value > 0.5,
            13 => prefs.align = align_from(value.round() as i32),
            14 => prefs.paragraph_rule = rule_from(value.round() as i32),
            15 => {
                prefs.ruby = if value > 0.5 {
                    parse::RubyMode::Inline
                } else {
                    parse::RubyMode::Hidden
                }
            }
            16 => prefs.paged = value > 0.5,
            17 => prefs.show_progress = value > 0.5,
            18 => prefs.hide_chrome = value > 0.5,
            _ => return ReaderEdit::Ignored,
        }
        self.prefs = prefs.clamped();
        if reader_op_reflows(op) {
            self.build_body();
            ReaderEdit::Reflowed
        } else {
            ReaderEdit::Restyled
        }
    }
}

/// A landing-page value with every field present and nothing loaded.
fn empty_view() -> NovelView {
    NovelView {
        state: 1,
        message: "".into(),
        title: "".into(),
        author: "".into(),
        author_id: "".into(),
        account: "".into(),
        author_avatar: slint::Image::default(),
        author_avatar_state: 2,
        cover: slint::Image::default(),
        cover_state: 2,
        caption: "".into(),
        posted: "".into(),
        stats: "".into(),
        read_label: "开始阅读".into(),
        bookmarked: false,
        busy: true,
        series_title: "".into(),
        series_meta: "".into(),
        chapters_state: 2,
        chapters_message: "".into(),
    }
}

// ---------------------------------------------------------------------
// formatting
// ---------------------------------------------------------------------

/// 4025 -> "4,025". Grouping happens here, once, so a count cannot appear with
/// separators on one row and without on the next.
fn thousands(value: u32) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// "4,025 字".
fn characters_label(characters: u32) -> String {
    if characters == 0 {
        String::new()
    } else {
        format!("{} 字", thousands(characters))
    }
}

/// "约 8 分钟", from Pixiv's own reading time in seconds.
fn reading_label(seconds: u32) -> String {
    if seconds == 0 {
        return String::new();
    }
    let minutes = seconds.div_ceil(60);
    format!("约 {minutes} 分钟")
}

/// Offline novel fixture, for captures and for the reader gate.
///
/// PIXIV_DESKTOP_NOVEL_FIXTURE=<path> makes the novel surfaces read one captured
/// /ajax/novel/{id} document instead of the network. The document goes through the
/// same wire -> domain mapping the live path uses, so the fixture proves the
/// renderer and the parser, not a second code path. It is a test mode and says so
/// in the status bar.
pub(crate) fn fixture_novel() -> Option<Novel> {
    let path = std::env::var("PIXIV_DESKTOP_NOVEL_FIXTURE").ok()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let body = value.get("body").cloned().unwrap_or(value);
    let wire: pixiv_types::wire::NovelDetailWire = serde_json::from_value(body).ok()?;
    Some(Novel::from_detail(wire))
}

// ---------------------------------------------------------------------
// domain -> rows
// ---------------------------------------------------------------------

/// One search row / recent-read row.
/// The URL -> body row index for the block model that was just built (B9).
///
/// A body row names an image id, and an image id names up to two URLs (the thumbnail
/// and the large one). Both are indexed against the row that shows them, so an
/// arrival can be addressed by URL. Built once per body instead of searched once per
/// arrival: 800 blocks is 800 clones and 800 comparisons per image the old way.
pub(crate) fn block_image_index(
    rows: &[NovelBlock],
    sources: &std::collections::BTreeMap<String, parse::ImageInfo>,
) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (row_index, row) in rows.iter().enumerate() {
        if row.kind != K_IMAGE {
            continue;
        }
        let Some(info) = sources.get(row.id.as_str()) else {
            continue;
        };
        // One entry per URL: a row whose two URLs agree is still one row.
        let mut seen: Vec<&String> = Vec::with_capacity(2);
        for url in [&info.small, &info.large] {
            if url.is_empty() || seen.contains(&url) {
                continue;
            }
            seen.push(url);
            index.entry(url.clone()).or_default().push(row_index);
        }
    }
    index
}

/// Write one decoded image into the rows that show it. Returns how many rows it wrote.
///
/// Only the rows the index named are read, and a row that already holds a picture is
/// left alone (a second arrival for the same URL is not a third paint).
pub(crate) fn patch_block_image<M: Model<Data = NovelBlock>>(
    model: &M,
    rows: &[usize],
    image: &slint::Image,
) -> usize {
    let mut written = 0;
    for index in rows {
        let Some(row) = model.row_data(*index) else {
            continue;
        };
        if row.state == 0 {
            continue;
        }
        let mut next = row;
        next.image = image.clone();
        next.state = 0;
        model.set_row_data(*index, next);
        written += 1;
    }
    written
}

/// Adopt one search page: merge it into the list and rebuild the rows from the whole
/// list (B8).
///
/// The rows used to be replaced by whichever page arrived last while the summaries
/// accumulated, so the second "load more" erased the first: page 1 disappeared and
/// the count said 48 while the model held the 24 rows that start at work 25. Deriving
/// the rows from the merged list makes the model the whole answer by construction, so
/// there is no ordering left to get wrong. Returns the rows and the covers still to
/// decode, in load order.
pub(crate) fn adopt_novel_page(
    summaries: &mut Vec<NovelSummary>,
    page: Vec<NovelSummary>,
    images: &HashMap<String, slint::Image>,
) -> (Vec<NovelRow>, Vec<String>) {
    if summaries.is_empty() {
        *summaries = page;
    } else {
        summaries.extend(page);
    }
    // Derived from the merged list, which is what makes the model the whole answer.
    let mut rows: Vec<NovelRow> = Vec::with_capacity(summaries.len());
    let mut covers: Vec<String> = Vec::new();
    for summary in summaries.iter() {
        let mut row = summary_row(summary, 2);
        if let Some(cover) = summary.cover.as_deref() {
            covers.push(cover.to_string());
            if let Some(image) = images.get(cover) {
                row.cover = image.clone();
                row.cover_state = 0;
            }
        }
        rows.push(row);
    }
    (rows, covers)
}

fn summary_row(summary: &NovelSummary, cover_state: i32) -> NovelRow {
    let meta = [
        characters_label(summary.character_count),
        reading_label(summary.reading_time),
        summary.upload_date.clone(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    NovelRow {
        id: summary.id.get().to_string().into(),
        title: summary.title.clone().into(),
        author: summary.author.name.clone().into(),
        meta: meta.into(),
        cover: slint::Image::default(),
        cover_state,
        series: summary.series_title.clone().into(),
        // A search result is not a reading position: -1 draws no rule.
        progress: -1.0,
    }
}

/// The percentage a recent row prints, from the three numbers the store keeps
/// (F4).
///
/// `block_index` and `char_offset` are the position `save_progress` wrote, and
/// `ratio` is the same position as the fraction the store also keeps. `None`
/// means "no progress at all", which is what makes a row draw no rule instead of
/// a 0 % one: a novel that was opened and closed on its first screen has a row and
/// a zero, and a novel that was never opened has neither.
///
/// Every other answer is clamped into 0..=100. The clamp is not defensive
/// padding: `ratio` was measured against the document that was open, so a
/// document that changed underneath the saved position (a longer or shorter
/// revision of the same novel id) can report a fraction above 1, and a progress
/// rule that drew 134 % would be describing a length that does not exist.
pub(crate) fn progress_percent(block_index: u32, char_offset: u32, ratio: f32) -> Option<f32> {
    if !ratio.is_finite() {
        return None;
    }
    if ratio <= 0.0 && block_index == 0 && char_offset == 0 {
        return None;
    }
    Some((ratio * 100.0).clamp(0.0, 100.0))
}

/// One chapter row of a series.
fn chapter_row(chapter: &SeriesChapter, current: Option<u64>) -> NovelChapter {
    NovelChapter {
        id: chapter.id.get().to_string().into(),
        title: chapter.title.clone().into(),
        order: i32::try_from(chapter.order).unwrap_or(i32::MAX),
        meta: characters_label(chapter.character_count).into(),
        current: current == Some(chapter.id.get()),
    }
}

/// The one-line metadata row of the landing page.
fn stats_line(novel: &Novel) -> String {
    let mut parts = vec![
        characters_label(novel.character_count),
        reading_label(novel.reading_time),
    ];
    if novel.views > 0 {
        parts.push(format!("{} 浏览", thousands(novel.views)));
    }
    if novel.bookmarks > 0 {
        parts.push(format!("{} 收藏", thousands(novel.bookmarks)));
    }
    if novel.x_restrict > 0 {
        parts.push("R-18".to_string());
    }
    parts.retain(|part| !part.is_empty());
    parts.join(" · ")
}

// ---------------------------------------------------------------------
// prefs -> reader value
// ---------------------------------------------------------------------

/// Paper and ink for the chosen reader theme.
///
/// The reader has its own palette on purpose: a sepia page is not "the app in
/// light mode", and the reading surface must stay legible against an app theme the
/// user may have picked for a different reason. System resolves against the app's
/// own resolved darkness, so the two can never disagree.
fn paper_ink(prefs: &ReaderPrefs, app_dark: bool) -> (slint::Color, slint::Color) {
    let (paper, ink) = match prefs.theme {
        novel_prefs::ReaderTheme::System => {
            if app_dark {
                ("#1b1b1b", "#e8e8e8")
            } else {
                ("#fbfbfb", "#1a1a1a")
            }
        }
        novel_prefs::ReaderTheme::Light => ("#fbfbfb", "#1a1a1a"),
        novel_prefs::ReaderTheme::Dark => ("#1b1b1b", "#e8e8e8"),
        novel_prefs::ReaderTheme::Sepia => {
            (novel_prefs::DEFAULT_BACKGROUND, novel_prefs::DEFAULT_INK)
        }
        novel_prefs::ReaderTheme::Custom => {
            let bg = prefs.custom_background.trim();
            let fg = prefs.custom_ink.trim();
            if novel_prefs::is_colour(bg) && novel_prefs::is_colour(fg) {
                (bg, fg)
            } else {
                (novel_prefs::DEFAULT_BACKGROUND, novel_prefs::DEFAULT_INK)
            }
        }
    };
    (colour(paper, "#fbfbfb"), colour(ink, "#1a1a1a"))
}

/// #RRGGBB / #AARRGGBB to a Slint colour, falling back when the value is not one.
fn colour(hex: &str, fallback: &str) -> slint::Color {
    let parse = |text: &str| u8::from_str_radix(text, 16).ok();
    let hex = hex.trim().trim_start_matches('#');
    let bytes = |text: &str| match (parse(&text[0..2]), parse(&text[2..4]), parse(&text[4..6])) {
        (Some(r), Some(g), Some(b)) => Some((r, g, b)),
        _ => None,
    };
    match hex.len() {
        6 => match bytes(hex) {
            Some((r, g, b)) => slint::Color::from_rgb_u8(r, g, b),
            None => colour(fallback, "#1a1a1a"),
        },
        8 => {
            let alpha = parse(&hex[0..2]);
            match (alpha, bytes(&hex[2..8])) {
                (Some(a), Some((r, g, b))) => slint::Color::from_argb_u8(a, r, g, b),
                _ => colour(fallback, "#1a1a1a"),
            }
        }
        _ => slint::Color::from_rgb_u8(0x1a, 0x1a, 0x1a),
    }
}

/// 0..1 position of value inside [min, max], clamped.
fn ratio_of(value: f32, min: f32, max: f32) -> f32 {
    if (max - min).abs() < f32::EPSILON {
        0.0
    } else {
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    }
}

/// The inverse of ratio_of.
fn value_of(ratio: f32, min: f32, max: f32) -> f32 {
    min + (max - min) * ratio.clamp(0.0, 1.0)
}

/// Weights the panel offers, matching its three segments.
const WEIGHTS: [u16; 3] = [300, 400, 600];

fn weight_index(weight: u16) -> i32 {
    match weight {
        0..=349 => 0,
        350..=499 => 1,
        _ => 2,
    }
}

fn align_index(align: novel_prefs::Align) -> i32 {
    match align {
        novel_prefs::Align::Start => 0,
        novel_prefs::Align::Center => 1,
        novel_prefs::Align::End => 2,
    }
}

fn align_from(index: i32) -> novel_prefs::Align {
    match index {
        1 => novel_prefs::Align::Center,
        2 => novel_prefs::Align::End,
        _ => novel_prefs::Align::Start,
    }
}

fn rule_index(rule: novel_prefs::ParagraphRule) -> i32 {
    match rule {
        novel_prefs::ParagraphRule::Space => 0,
        novel_prefs::ParagraphRule::Rule => 1,
        novel_prefs::ParagraphRule::None => 2,
    }
}

fn rule_from(index: i32) -> novel_prefs::ParagraphRule {
    match index {
        1 => novel_prefs::ParagraphRule::Rule,
        2 => novel_prefs::ParagraphRule::None,
        _ => novel_prefs::ParagraphRule::Space,
    }
}

fn theme_index(theme: novel_prefs::ReaderTheme) -> i32 {
    match theme {
        novel_prefs::ReaderTheme::System => 0,
        novel_prefs::ReaderTheme::Light => 1,
        novel_prefs::ReaderTheme::Dark => 2,
        novel_prefs::ReaderTheme::Sepia => 3,
        novel_prefs::ReaderTheme::Custom => 4,
    }
}

fn theme_from(index: i32) -> novel_prefs::ReaderTheme {
    match index {
        1 => novel_prefs::ReaderTheme::Light,
        2 => novel_prefs::ReaderTheme::Dark,
        3 => novel_prefs::ReaderTheme::Sepia,
        4 => novel_prefs::ReaderTheme::Custom,
        _ => novel_prefs::ReaderTheme::System,
    }
}

/// What is being read, and what its neighbours are called.
///
/// Bundled rather than passed as four more arguments: the view builder already
/// takes the preferences, the state, the message and two flags, and clippy's
/// too_many_arguments is right that eight is one too many for a function whose
/// arguments are all "the same document".
struct ReaderDoc<'a> {
    title: &'a str,
    chapter_meta: &'a str,
    prev: &'a str,
    next: &'a str,
}

/// Build the whole reader value from the preferences.
///
/// Every field is derived, none is carried over: a stale field in this struct is a
/// settings panel that lies about what the body is doing.
fn reader_view(
    prefs: &ReaderPrefs,
    state: i32,
    message: &str,
    doc: &ReaderDoc<'_>,
    settings_open: bool,
    app_dark: bool,
) -> ReaderView {
    let (paper, ink) = paper_ink(prefs, app_dark);
    ReaderView {
        state,
        message: message.into(),
        title: doc.title.into(),
        chapter_meta: doc.chapter_meta.into(),
        has_prev: !doc.prev.is_empty(),
        has_next: !doc.next.is_empty(),
        prev_label: doc.prev.into(),
        next_label: doc.next.into(),
        settings_open,
        chrome_visible: !prefs.hide_chrome,
        show_progress: prefs.show_progress,
        paged: prefs.paged,
        ruby: matches!(prefs.ruby, parse::RubyMode::Inline),
        font_family: prefs.font_family.clone().into(),
        font_size: prefs.font_size,
        font_weight: i32::from(prefs.font_weight),
        italic: prefs.italic,
        line_height: prefs.line_height,
        letter_spacing: prefs.letter_spacing,
        paragraph_gap: prefs.paragraph_spacing,
        margin_x: prefs.margin_x,
        measure: prefs.max_width,
        align: align_index(prefs.align),
        rule: rule_index(prefs.paragraph_rule),
        ink,
        paper,
        font_size_ratio: ratio_of(
            prefs.font_size,
            novel_prefs::MIN_FONT_SIZE,
            novel_prefs::MAX_FONT_SIZE,
        ),
        line_height_ratio: ratio_of(
            prefs.line_height,
            novel_prefs::MIN_LINE_HEIGHT,
            novel_prefs::MAX_LINE_HEIGHT,
        ),
        letter_spacing_ratio: ratio_of(
            prefs.letter_spacing,
            novel_prefs::MIN_LETTER_SPACING,
            novel_prefs::MAX_LETTER_SPACING,
        ),
        paragraph_gap_ratio: ratio_of(
            prefs.paragraph_spacing,
            novel_prefs::MIN_PARAGRAPH_SPACING,
            novel_prefs::MAX_PARAGRAPH_SPACING,
        ),
        indent_ratio: ratio_of(
            prefs.first_line_indent,
            novel_prefs::MIN_FIRST_LINE_INDENT,
            novel_prefs::MAX_FIRST_LINE_INDENT,
        ),
        margin_ratio: ratio_of(
            prefs.margin_x,
            novel_prefs::MIN_MARGIN_X,
            novel_prefs::MAX_MARGIN_X,
        ),
        measure_ratio: ratio_of(
            prefs.max_width,
            novel_prefs::MIN_MAX_WIDTH,
            novel_prefs::MAX_MAX_WIDTH,
        ),
        weight_index: weight_index(prefs.font_weight),
        theme_index: theme_index(prefs.theme),
        custom_background: prefs.custom_background.clone().into(),
        custom_ink: prefs.custom_ink.clone().into(),
    }
}

// ---------------------------------------------------------------------
// document -> rows
// ---------------------------------------------------------------------

/// Ideographic-space indent for the first line of a paragraph.
///
/// Slint has no text-indent: a spacer element cannot wrap with the paragraph, so
/// the choice is between an indent the text carries and no indent at all. U+3000
/// is full-width by definition, which makes the indent exactly N characters wide
/// at every font size, and it collapses correctly at a line break.
fn indent_prefix(prefs: &ReaderPrefs) -> String {
    let count = prefs.first_line_indent.round().clamp(0.0, 3.0) as usize;
    "\u{3000}".repeat(count)
}

/// Flatten one span into prose, according to the ruby preference.
fn span_text(span: &Span, prefs: &ReaderPrefs) -> String {
    match span {
        Span::Text(text) => text.clone(),
        Span::Ruby { base, reading } => {
            if matches!(prefs.ruby, parse::RubyMode::Inline) {
                format!("{base}（{reading}）")
            } else {
                base.clone()
            }
        }
        // A link inside a paragraph keeps its caption in the prose; the clickable
        // row is emitted separately by rows_from.
        Span::Link { label, .. } => label.clone(),
        Span::WorkRef { label, .. } => label.clone(),
    }
}

/// Turn a parsed document into the reader's rows.
///
/// Link and work spans become their own rows: Slint has no inline link inside a
/// Text, so a paragraph carrying a link would otherwise be one unclickable block.
/// The paragraph is split at its links in body order, which keeps the reading order
/// of the page identical to the reading order of the body.
fn rows_from(document: &Document, prefs: &ReaderPrefs) -> Vec<NovelBlock> {
    let mut rows: Vec<NovelBlock> = Vec::with_capacity(document.blocks.len());
    for block in &document.blocks {
        if matches!(block.kind, BlockKind::Image) {
            rows.push(NovelBlock {
                kind: K_IMAGE,
                text: "".into(),
                label: if block.missing {
                    format!("图片 {}（原图不可用）", block.image_id).into()
                } else {
                    String::new().into()
                },
                image: slint::Image::default(),
                state: if block.missing { 2 } else { 1 },
                id: block.image_id.clone().into(),
            });
            continue;
        }

        let kind = match block.kind {
            BlockKind::Paragraph => K_PARA,
            BlockKind::Heading => K_HEAD,
            BlockKind::Separator => K_RULE,
            BlockKind::PageBreak => K_BREAK,
            BlockKind::Image => unreachable!("handled above"),
        };

        let mut prose = String::new();
        for span in &block.spans {
            match span {
                Span::Link { label, href } => {
                    if !prose.trim().is_empty() {
                        rows.push(text_row(kind, std::mem::take(&mut prose), prefs));
                    }
                    rows.push(link_row(label.clone(), href.clone()));
                }
                Span::WorkRef { id, label } => {
                    if !prose.trim().is_empty() {
                        rows.push(text_row(kind, std::mem::take(&mut prose), prefs));
                    }
                    rows.push(NovelBlock {
                        kind: K_WORK,
                        text: label.clone().into(),
                        label: "".into(),
                        image: slint::Image::default(),
                        state: 0,
                        id: format!("work:{id}").into(),
                    });
                }
                other => prose.push_str(&span_text(other, prefs)),
            }
        }
        if !prose.trim().is_empty() {
            rows.push(text_row(kind, prose, prefs));
        }
    }
    rows
}

fn text_row(kind: i32, text: String, prefs: &ReaderPrefs) -> NovelBlock {
    let mut text = text.trim().to_string();
    if kind == K_PARA {
        text = format!("{}{}", indent_prefix(prefs), text);
    }
    NovelBlock {
        kind,
        text: text.into(),
        label: "".into(),
        image: slint::Image::default(),
        state: 0,
        id: "".into(),
    }
}

fn link_row(label: String, href: String) -> NovelBlock {
    let kind = if href.starts_with("jump:") {
        K_WORK
    } else {
        K_LINK
    };
    NovelBlock {
        kind,
        text: label.into(),
        label: "".into(),
        image: slint::Image::default(),
        state: 0,
        id: href.into(),
    }
}

impl App {
    /// Load the novels page: recent reads from SQLite, then the last search if any.
    ///
    /// The recent list is read on the runtime and published from there: it is a
    /// SQLite query and this method runs on the frame thread (plan section 2.3 R16).
    pub(crate) fn load_novels_page(shared: &Shared) {
        {
            let mut app = shared.borrow_mut();
            if app.novel.state != 0 && app.novel.rows.row_count() == 0 {
                app.publish_novels_state(1, "");
            }
        }
        let (store, rt) = {
            let app = shared.borrow();
            (app.store.clone(), app.rt.clone())
        };
        rt.spawn(async move {
            // One query, not one per row: the position is joined to the entry it
            // belongs to (F4), so the list and the progress rule under it come from
            // the same statement and cannot disagree about which novel is first.
            let rows = store
                .recent_rows_with_progress(RECENT_KEEP)
                .unwrap_or_default();
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                let recent = rows
                    .iter()
                    .map(|row| {
                        let entry = &row.entry;
                        NovelRow {
                            id: entry.novel_id.clone().into(),
                            title: entry.title.clone().into(),
                            author: entry.author.clone().into(),
                            meta: "最近阅读".into(),
                            cover: slint::Image::default(),
                            cover_state: 2,
                            series: entry.series_id.clone().into(),
                            // -1 is the row's "no progress": it draws no rule.
                            progress: row
                                .progress
                                .as_ref()
                                .and_then(|stored| {
                                    progress_percent(
                                        stored.block_index,
                                        stored.char_offset,
                                        stored.percent,
                                    )
                                })
                                .unwrap_or(-1.0),
                        }
                    })
                    .collect::<Vec<_>>();
                app.novel.recent.set_vec(recent);
                let ui = app.ui();
                ui.set_novel_recent(ModelRc::from(app.novel.recent.clone()));
                let urls: Vec<String> = rows
                    .iter()
                    .map(|row| row.entry.cover.clone())
                    .filter(|url| !url.is_empty())
                    .collect();
                app.spawn_novel_images(urls);
            });
        });
    }

    fn publish_novels_state(&mut self, state: i32, message: &str) {
        self.novel.state = state;
        self.novel.message = message.to_string();
        let ui = self.ui();
        ui.set_novels_state(state);
        ui.set_novels_message(message.into());
    }
}

impl App {
    /// Search Pixiv novels. next_page appends; otherwise it replaces the list.
    pub(crate) fn search_novels(shared: &Shared, word: &str, next_page: bool) {
        let word = word.trim().to_string();
        let client = shared.borrow().client.clone();
        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.publish_novels_state(3, "未登录或网络不可用：小说搜索需要访问 Pixiv");
            return;
        };
        let (page, epoch) = {
            let mut app = shared.borrow_mut();
            if next_page {
                // One page at a time, and never past the last one (B8).
                if !app.novel.take_more() {
                    return;
                }
                app.publish_novels_state(1, "");
            } else {
                app.novel.epoch = app.novel.epoch.wrapping_add(1);
                app.novel.page = 1;
                app.novel.summaries.clear();
                app.novel.rows.set_vec(Vec::new());
                app.novel.word = word.clone();
                app.publish_novels_state(1, "");
                let ui = app.ui();
                ui.set_novel_query(word.clone().into());
                ui.set_novel_rows(ModelRc::from(app.novel.rows.clone()));
            }
            (app.novel.page, app.novel.epoch)
        };
        let rt = shared.borrow().rt.clone();
        rt.spawn(async move {
            let result = client.novel_search(&word, page).await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                if app.novel.epoch != epoch {
                    // A newer list replaced this one; the slot must not stay taken.
                    app.novel.release_more(true);
                    return;
                }
                match result {
                    Ok(found) => {
                        app.novel.release_more(true);
                        app.novel.last_page = found.last_page.max(1);
                        // The rows are the merged list, not the page that arrived (B8).
                        let (rows, covers) = {
                            // Two fields of one state, borrowed apart: the summaries
                            // grow while the decoded covers are read.
                            let NovelState {
                                summaries, images, ..
                            } = &mut app.novel;
                            adopt_novel_page(summaries, found.novels, images)
                        };
                        app.novel.rows.set_vec(rows);
                        let count = app.novel.rows.row_count();
                        let empty = count == 0;
                        let title = if app.novel.word.is_empty() {
                            "小说".to_string()
                        } else {
                            format!("小说：{}", app.novel.word)
                        };
                        let ui = app.ui();
                        ui.set_novel_rows(ModelRc::from(app.novel.rows.clone()));
                        ui.set_novels_title(title.into());
                        ui.set_novels_subtitle(
                            format!("{count} 本 · 来自 /ajax/search/novels").into(),
                        );
                        if empty {
                            app.publish_novels_state(2, "Pixiv 没有返回结果，换个关键词试试");
                        } else {
                            app.publish_novels_state(0, "");
                        }
                        app.spawn_novel_images(covers);
                    }
                    Err(error) => {
                        // The cursor goes back: the next attempt asks for this page.
                        app.novel.release_more(false);
                        let message = novel_error_text(&error);
                        app.publish_novels_state(3, &message);
                    }
                }
            });
        });
    }
}

/// Read one novel's stored position, saying so when the read itself failed.
///
/// "Nothing stored" and "the database would not answer" both mean the reader opens
/// at the beginning, which is fine - but only one of them is a failure, and a
/// failure that reaches neither a state nor a log is what G41 exists to stop.
fn stored_novel_progress(
    store: &pixiv_store::Store,
    id: u64,
) -> Option<pixiv_store::NovelProgress> {
    match store.novel_progress(&id.to_string()) {
        Ok(progress) => progress,
        Err(error) => {
            tracing::debug!(%error, novel = id, "reading position could not be read");
            None
        }
    }
}

/// One sentence a user can act on, per failure family.
pub(crate) fn novel_error_text(error: &PixivError) -> String {
    match error {
        // The novel-specific readings first: a 404 here is about the novel, and a
        // changed payload is about the reader's data.
        PixivError::Http { status, .. } if *status == 404 => {
            "这本小说不存在，或作者已删除/设为私密".to_string()
        }
        PixivError::Decode(_) => "Pixiv 返回了无法解析的小说数据（接口可能已变更）".to_string(),
        // C6: a flagged non-auth 4xx now arrives as `Http { status, message }` -
        // the crate layer stopped flattening it into `Api` - so the arm below no
        // longer catches it and Pixiv's own sentence was being replaced by the
        // generic one. It is the sentence the user needs: keep it verbatim.
        PixivError::Http { message, .. } if !message.trim().is_empty() => message.clone(),
        PixivError::Api(message) if !message.trim().is_empty() => message.clone(),
        // Everything else is the app-wide sentence, so an expired cookie says
        // 重新登录 here as it does on every other page (T2).
        other => describe_session_error(other),
    }
}

impl App {
    /// Open one novel's landing page.
    pub(crate) fn open_novel_from(shared: &Shared, id: &str) {
        let Ok(raw) = id.trim().parse::<u64>() else {
            return;
        };
        App::go(shared, Route::novel(raw));
    }

    /// Load the novel the current route names into the landing page.
    pub(crate) fn load_novel_page(shared: &Shared) {
        let Some(id) = shared.borrow().route.novel_id() else {
            return;
        };
        let already = shared
            .borrow()
            .novel
            .novel
            .as_ref()
            .is_some_and(|novel| novel.id.get() == id);
        if already {
            shared.borrow_mut().publish_novel_view();
            return;
        }
        {
            let mut app = shared.borrow_mut();
            app.novel.novel = None;
            app.novel.series = None;
            app.novel.chapter_ids.clear();
            app.novel.chapter_index = 0;
            app.novel.chapters.set_vec(Vec::new());
            app.novel.view = empty_view();
            app.novel.view.state = 1;
            app.publish_novel_view();
        }
        if let Some(novel) = fixture_novel() {
            let mut app = shared.borrow_mut();
            let ratio = app.novel.ratio;
            app.adopt_novel(novel, ratio);
            // The fixture carries one document, not a series listing: say so instead
            // of leaving the chapter block on a spinner that can never finish.
            app.novel.view.chapters_state = 2;
            app.novel.view.chapters_message =
                "样本模式：章节列表需要联网，正文与排版不受影响".into();
            app.publish_novel_view();
            return;
        }
        let client = shared.borrow().client.clone();
        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.novel.view.state = 3;
            app.novel.view.message = "未登录或网络不可用：小说正文需要访问 Pixiv".into();
            app.publish_novel_view();
            return;
        };
        let (store, rt) = {
            let app = shared.borrow();
            (app.store.clone(), app.rt.clone())
        };
        rt.spawn(async move {
            let result = client.novel(Id(id)).await;
            let progress = stored_novel_progress(&store, id);
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                match result {
                    Ok(novel) => {
                        let percent = progress.as_ref().map_or(0.0, |p| p.percent);
                        app.novel.ratio = percent;
                        let series_id = novel.series.as_ref().map(|nav| nav.id);
                        app.adopt_novel(novel, percent);
                        if let Some(series_id) = series_id {
                            app.load_novel_series(series_id);
                        }
                    }
                    Err(error) => {
                        let missing = matches!(error, PixivError::Http { status: 404, .. });
                        app.novel.view.state = if missing { 2 } else { 3 };
                        app.novel.view.message = novel_error_text(&error).into();
                        app.publish_novel_view();
                    }
                }
            });
        });
    }

    /// Adopt a loaded novel into the landing page state.
    fn adopt_novel(&mut self, novel: Novel, percent: f32) {
        let cover_url = novel.cover.clone();
        let avatar_url = novel.author.avatar.clone();
        let nav = novel.series.clone();
        let read_label = if percent > 0.001 {
            format!("继续阅读 · {}%", (percent * 100.0).round() as i32)
        } else {
            "开始阅读".to_string()
        };
        // F4: the chapter band says which chapter to continue, not only which one
        // this is. The two sentences are one string because they are one slot
        // (`SectionHeader.subtitle`): a band that drew the chapter number twice,
        // once as "第 4 章" and once as "继续阅读 第 4 章", would be saying one thing
        // in two places and inviting them to disagree.
        let series_meta = nav
            .as_ref()
            .map(|nav| {
                let status = if nav.concluded {
                    "已完结"
                } else {
                    "连载中"
                };
                if percent > 0.001 {
                    format!("继续阅读 第 {} 章 · {status}", nav.order)
                } else {
                    format!("第 {} 章 · {status}", nav.order)
                }
            })
            .unwrap_or_default();
        let view = NovelView {
            state: 0,
            message: "".into(),
            title: novel.title.clone().into(),
            author: novel.author.name.clone().into(),
            author_id: novel.author.id.get().to_string().into(),
            account: novel.author.account.clone().into(),
            author_avatar: avatar_url
                .as_deref()
                .and_then(|url| self.novel.images.get(url).cloned())
                .unwrap_or_default(),
            author_avatar_state: if avatar_url.is_some() { 1 } else { 2 },
            cover: cover_url
                .as_deref()
                .and_then(|url| self.novel.images.get(url).cloned())
                .unwrap_or_default(),
            cover_state: if cover_url.is_some() { 1 } else { 2 },
            caption: novel.caption.clone().into(),
            posted: novel.upload_date.clone().into(),
            stats: stats_line(&novel).into(),
            read_label: read_label.into(),
            bookmarked: novel.bookmarked,
            busy: true,
            series_title: nav
                .as_ref()
                .map(|nav| nav.title.clone())
                .unwrap_or_default()
                .into(),
            series_meta: series_meta.into(),
            chapters_state: if nav.is_some() { 1 } else { 2 },
            chapters_message: "".into(),
        };
        let tags: Vec<slint::SharedString> = novel
            .tags
            .iter()
            .map(|tag| slint::SharedString::from(tag.display().to_string()))
            .collect();
        self.novel.view = view;
        self.novel.novel = Some(novel);
        self.publish_novel_view();
        let ui = self.ui();
        ui.set_novel_tags(ModelRc::from(Rc::new(VecModel::from(tags))));
        let mut urls = Vec::new();
        if let Some(url) = cover_url {
            urls.push(url);
        }
        if let Some(url) = avatar_url {
            urls.push(url);
        }
        self.spawn_novel_images(urls);
    }

    /// Load a series' chapter list.
    fn load_novel_series(&mut self, series: Id) {
        let client = self.client.clone();
        let Some(client) = client else {
            self.novel.view.chapters_state = 3;
            self.novel.view.chapters_message = "未登录或网络不可用".into();
            self.publish_novel_view();
            return;
        };
        let rt = self.rt.clone();
        rt.spawn(async move {
            let info = client.novel_series(series).await;
            let chapters = client.novel_series_chapters(series).await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                if let Ok(info) = info {
                    app.novel.series = Some(info);
                }
                match chapters {
                    Ok(list) => {
                        let current = app.novel.novel.as_ref().map(|novel| novel.id.get());
                        app.novel.chapter_ids =
                            list.iter().map(|chapter| chapter.id.get()).collect();
                        app.novel.chapter_index = current
                            .and_then(|id| {
                                app.novel.chapter_ids.iter().position(|known| *known == id)
                            })
                            .unwrap_or(0);
                        let rows: Vec<NovelChapter> = list
                            .iter()
                            .map(|chapter| chapter_row(chapter, current))
                            .collect();
                        app.novel.chapters.set_vec(rows);
                        app.novel.view.chapters_state = 0;
                        app.novel.view.chapters_message = "".into();
                        if let Some(info) = app.novel.series.clone() {
                            app.novel.view.series_title = info.title.clone().into();
                            app.novel.view.series_meta = format!(
                                "共 {} 章 · {} · {}",
                                info.total,
                                characters_label(info.total_characters),
                                reading_label(info.total_reading_time)
                            )
                            .into();
                        }
                        app.publish_novel_view();
                    }
                    Err(error) => {
                        app.novel.view.chapters_state = 3;
                        app.novel.view.chapters_message = novel_error_text(&error).into();
                        app.publish_novel_view();
                    }
                }
            });
        });
    }

    /// Push the landing-page value to Slint.
    fn publish_novel_view(&mut self) {
        let mut view = self.novel.view.clone();
        if let Some(url) = self
            .novel
            .novel
            .as_ref()
            .and_then(|novel| novel.cover.clone())
        {
            match self.novel.images.get(&url) {
                Some(image) => {
                    view.cover = image.clone();
                    view.cover_state = 0;
                }
                None => {
                    if self.novel.loading.contains(&url) {
                        view.cover_state = 1;
                    }
                }
            }
        }
        if let Some(url) = self
            .novel
            .novel
            .as_ref()
            .and_then(|novel| novel.author.avatar.clone())
        {
            if let Some(image) = self.novel.images.get(&url) {
                view.author_avatar = image.clone();
                view.author_avatar_state = 0;
            }
        }
        self.novel.view = view.clone();
        let ui = self.ui();
        ui.set_novel_view(view);
        ui.set_novel_chapters(ModelRc::from(self.novel.chapters.clone()));
    }
}

impl App {
    /// Enter the reader for the novel the route names.
    pub(crate) fn open_reader_route(shared: &Shared, id: u64) {
        App::go(shared, Route::novel_reader(id));
    }

    /// Load the body into the reader page.
    ///
    /// Reuses the landing page's document when the route came from it: the body
    /// arrives with the metadata, so opening a novel and then reading it is one
    /// request, not two.
    pub(crate) fn load_reader_page(shared: &Shared) {
        let Some(id) = shared.borrow().route.novel_id() else {
            return;
        };
        let cached = {
            let app = shared.borrow();
            app.novel
                .novel
                .as_ref()
                .filter(|novel| novel.id.get() == id)
                .cloned()
        };
        if let Some(novel) = cached {
            let mut app = shared.borrow_mut();
            app.novel.reader_novel = Some(id);
            app.adopt_document(&novel);
            return;
        }
        if let Some(novel) = fixture_novel() {
            let mut app = shared.borrow_mut();
            app.novel.reader_novel = Some(id);
            let ratio = app.novel.ratio;
            app.adopt_novel(novel.clone(), ratio);
            app.adopt_document(&novel);
            return;
        }
        let client = shared.borrow().client.clone();
        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.novel.reader.state = 3;
            app.novel.reader.message = "未登录或网络不可用：正文需要访问 Pixiv".into();
            app.publish_reader();
            return;
        };
        {
            let mut app = shared.borrow_mut();
            app.novel.reader.state = 1;
            app.novel.reader.message = "".into();
            app.publish_reader();
        }
        let (store, rt) = {
            let app = shared.borrow();
            (app.store.clone(), app.rt.clone())
        };
        rt.spawn(async move {
            let result = client.novel(Id(id)).await;
            let progress = stored_novel_progress(&store, id);
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                match result {
                    Ok(novel) => {
                        let percent = progress.as_ref().map_or(0.0, |p| p.percent);
                        app.novel.ratio = percent;
                        app.novel.reader_novel = Some(id);
                        app.adopt_novel(novel.clone(), percent);
                        app.adopt_document(&novel);
                    }
                    Err(error) => {
                        app.novel.reader.state = 3;
                        app.novel.reader.message = novel_error_text(&error).into();
                        app.publish_reader();
                    }
                }
            });
        });
    }

    /// Parse the body and publish the reader's rows.
    fn adopt_document(&mut self, novel: &Novel) {
        let images: std::collections::BTreeMap<String, parse::ImageInfo> = novel
            .images
            .iter()
            .map(|image| {
                (
                    image.id.clone(),
                    parse::ImageInfo {
                        small: image.small.clone(),
                        large: image.large.clone(),
                    },
                )
            })
            .collect();
        let document = parse::parse(&novel.body, &images);
        // One row-build site (O5): the mount builds the body here, and the only
        // other caller is the two opcodes whose text changes. The URLs come from
        // the model that was just built, so there is one row list in one order.
        self.novel.document = Some(document);
        // The index the arrival path reads is built from these URLs, so the body's
        // builder has to see them (B9).
        self.novel.image_sources = images.clone();
        self.novel.build_body();
        let urls = self.novel.image_urls(&images);
        let title = novel.title.clone();
        let chapter_meta = novel
            .series
            .as_ref()
            .map(|nav| format!("{} · 第 {} 章", nav.title, nav.order))
            .unwrap_or_else(|| characters_label(novel.character_count));
        let prev = novel
            .series
            .as_ref()
            .map(|nav| nav.prev_title.clone())
            .unwrap_or_default();
        let next = novel
            .series
            .as_ref()
            .map(|nav| nav.next_title.clone())
            .unwrap_or_default();
        let app_dark = self.dark_now();
        let settings_open = self.novel.reader.settings_open;
        self.novel.reader = reader_view(
            &self.novel.prefs,
            0,
            "",
            &ReaderDoc {
                title: &title,
                chapter_meta: &chapter_meta,
                prev: &prev,
                next: &next,
            },
            settings_open,
            app_dark,
        );
        // A capture (and a manual test) can ask the reader to open at a position:
        // PIXIV_DESKTOP_READER_SCROLL=0.42 exercises the restore path without input.
        if let Ok(value) = std::env::var("PIXIV_DESKTOP_READER_SCROLL") {
            if let Ok(ratio) = value.trim().parse::<f32>() {
                self.novel.ratio = ratio.clamp(0.0, 1.0);
            }
        }
        if std::env::var_os("PIXIV_DESKTOP_READER_SETTINGS").is_some() {
            self.novel.reader.settings_open = true;
        }
        if std::env::var_os("PIXIV_DESKTOP_READER_IMMERSIVE").is_some() {
            self.novel.prefs.hide_chrome = true;
            self.novel.reader.chrome_visible = false;
        }
        self.publish_reader();
        let entry = NovelHistoryEntry {
            novel_id: novel.id.get().to_string(),
            title: novel.title.clone(),
            author: novel.author.name.clone(),
            cover: novel.cover.clone().unwrap_or_default(),
            series_id: novel
                .series
                .as_ref()
                .map(|nav| nav.id.get().to_string())
                .unwrap_or_default(),
            read_at: 0,
        };
        let store = self.store.clone();
        self.rt.spawn(async move {
            let _ = store.record_novel_read(&entry);
        });
        let ratio = self.novel.ratio.clamp(0.0, 1.0);
        let ui = self.ui();
        ui.set_reader_restore(ratio);
        self.spawn_novel_images(urls);
    }

    /// Push the reader value and the body to Slint.
    fn publish_reader(&mut self) {
        let view = self.novel.reader.clone();
        let ui = self.ui();
        ui.set_reader_view(view);
        ui.set_novel_blocks(ModelRc::from(self.novel.blocks.clone()));
    }

    /// Whether the app is currently dark, for the reader's follow-system theme.
    fn dark_now(&self) -> bool {
        match self.theme_mode {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            // The window's own resolved value: it already folds the OS app-mode
            // preference in, so the reader cannot disagree with the shell.
            ThemeMode::System => self.ui().get_dark(),
        }
    }
}

impl App {
    /// Apply one settings-panel edit.
    ///
    /// The opcodes are the contract with ui/pages/novel-reader.slint's
    /// ReaderSettings component. Values arrive as 0..1 ratios; the clamp lives in
    /// the preference model, so an extreme slider position can only ever produce a
    /// value the model already allows.
    pub(crate) fn apply_reader_edit(&mut self, op: i32, value: f32, text: &str) {
        // O4 (plan R10 §2.3): an opcode this build does not know is a no-op and
        // stops here, before the write and before the reflow. The fallback used to
        // be `ReaderPrefs::default()`, which wiped all nineteen settings and then
        // persisted the wipe over the user's row.
        //
        // O5: the body is rebuilt by `apply_edit` itself, and only for the two
        // opcodes whose text changes. Everything below therefore republishes the
        // resolved view and nothing else.
        if self.novel.apply_edit(op, value, text) == ReaderEdit::Ignored {
            return;
        }
        self.save_prefs(false);
        self.rerender_reader();
    }

    /// Re-render after a preference change and keep the place.
    ///
    /// O5: this used to rebuild the body rows for *every* edit, including the ones
    /// that only change a colour, so a theme switch reset an 800-block model. The
    /// rows are now rebuilt by `NovelState::apply_edit` for the two opcodes that
    /// change their text and by nobody else; this function only republishes the
    /// value the page draws from.
    ///
    /// The restore is what makes a font-size change feel like a reflow instead of a
    /// jump: the ratio the page last reported is applied again once the new layout
    /// exists, so the paragraph the user was reading stays under their eyes.
    fn rerender_reader(&mut self) {
        let ratio = self.novel.ratio;
        let title = self.novel.reader.title.clone();
        let chapter_meta = self.novel.reader.chapter_meta.clone();
        let prev = self.novel.reader.prev_label.clone();
        let next = self.novel.reader.next_label.clone();
        let state = self.novel.reader.state;
        let message = self.novel.reader.message.clone();
        let settings_open = self.novel.reader.settings_open;
        let app_dark = self.dark_now();
        self.novel.reader = reader_view(
            &self.novel.prefs,
            state,
            &message,
            &ReaderDoc {
                title: &title,
                chapter_meta: &chapter_meta,
                prev: &prev,
                next: &next,
            },
            settings_open,
            app_dark,
        );
        self.publish_reader();
        if ratio > 0.0 {
            let ui = self.ui();
            ui.set_reader_restore(ratio);
        }
    }

    /// Remember where the reader is, and write it out if it is due.
    pub(crate) fn reader_progress(&mut self, ratio: f32) {
        self.novel.ratio = ratio.clamp(0.0, 1.0);
        self.save_progress(false);
    }

    /// Flush the reading position. Called when leaving the page and on exit.
    pub(crate) fn flush_novel_state(&mut self) {
        self.save_progress(true);
        self.save_prefs(true);
    }

    fn save_progress(&mut self, force: bool) {
        let Some(id) = self.novel.reader_novel else {
            return;
        };
        let now = std::time::Instant::now();
        if !force
            && self
                .novel
                .saved_at
                .is_some_and(|at| now.duration_since(at) < PROGRESS_INTERVAL)
        {
            return;
        }
        self.novel.saved_at = Some(now);
        let ratio = self.novel.ratio;
        let (blocks, characters) = self
            .novel
            .document
            .as_ref()
            .map(|document| (document.blocks.len(), document.characters))
            .unwrap_or((0, 0));
        let block_index = if blocks == 0 {
            0
        } else {
            ((ratio * blocks as f32) as usize).min(blocks - 1)
        };
        let progress = NovelProgress {
            novel_id: id.to_string(),
            series_id: self
                .novel
                .novel
                .as_ref()
                .and_then(|novel| novel.series.as_ref())
                .map(|nav| nav.id.get().to_string())
                .unwrap_or_default(),
            block_index: u32::try_from(block_index).unwrap_or(0),
            char_offset: (ratio * characters as f32) as u32,
            percent: ratio,
            finished: ratio >= 0.985,
            updated_at: 0,
        };
        let store = self.store.clone();
        self.rt.spawn(async move {
            let _ = store.save_novel_progress(&progress);
        });
    }

    fn save_prefs(&mut self, force: bool) {
        let now = std::time::Instant::now();
        if !force
            && self
                .novel
                .prefs_saved_at
                .is_some_and(|at| now.duration_since(at) < PREFS_INTERVAL)
        {
            return;
        }
        self.novel.prefs_saved_at = Some(now);
        let json = self.novel.prefs.to_json();
        let store = self.store.clone();
        self.rt.spawn(async move {
            let _ = store.set_novel_setting(PREFS_KEY, &json);
        });
    }
}

impl App {
    /// Decode novel images: covers, author faces and embedded illustrations.
    ///
    /// One pipeline for all three, and the same cache the feed uses, so a cover
    /// already seen in a search result is a hit when the landing page opens.
    pub(crate) fn spawn_novel_images(&mut self, urls: Vec<String>) {
        let mut plan: Vec<String> = Vec::new();
        for url in urls {
            if url.trim().is_empty()
                || self.novel.images.contains_key(&url)
                || self.novel.loading.contains(&url)
            {
                continue;
            }
            self.novel.loading.insert(url.clone());
            plan.push(url);
        }
        if plan.is_empty() {
            return;
        }
        let cache = self.cache.clone();
        let rt = self.rt.clone();
        let scale = self.ui().window().scale_factor().max(1.0);
        let target =
            crate::images::target_width_up_to(640.0, scale, crate::images::BANNER_MAX_WIDTH);
        rt.spawn(async move {
            let mut stream = futures::stream::iter(plan)
                .map(move |url| {
                    let cache = cache.clone();
                    async move {
                        let raw = fetch_sized(&cache, &url, target).await;
                        (url, raw)
                    }
                })
                .buffer_unordered(4);
            while let Some((url, raw)) = stream.next().await {
                on_ui(move |shared| {
                    let Ok(mut app) = shared.try_borrow_mut() else {
                        return;
                    };
                    app.novel.loading.remove(&url);
                    let Some(raw) = raw else {
                        return;
                    };
                    let image = raw.to_slint();
                    app.novel.images.insert(url.clone(), image);
                    app.repaint_novel_images(&url);
                });
            }
        });
    }

    /// Patch every novel surface that shows the image that just arrived.
    fn repaint_novel_images(&mut self, url: &str) {
        let Some(image) = self.novel.images.get(url).cloned() else {
            return;
        };
        let is_cover = self
            .novel
            .novel
            .as_ref()
            .and_then(|novel| novel.cover.clone())
            .as_deref()
            == Some(url);
        let is_avatar = self
            .novel
            .novel
            .as_ref()
            .and_then(|novel| novel.author.avatar.clone())
            .as_deref()
            == Some(url);
        if is_cover || is_avatar {
            self.publish_novel_view();
        }

        // Reader body: the index built with the body names the rows that show this
        // URL, so an arrival reads those rows and no others (B9).
        let shown_at = self.novel.block_images.get(url).cloned().unwrap_or_default();
        let patched = patch_block_image(&self.novel.blocks, &shown_at, &image) > 0;
        if patched {
            let ui = self.ui();
            ui.set_novel_blocks(ModelRc::from(self.novel.blocks.clone()));
        }

        // Search and recent rows.
        let mut rows_changed = false;
        for index in 0..self.novel.rows.row_count() {
            let Some(mut row) = self.novel.rows.row_data(index) else {
                continue;
            };
            let cover = self
                .novel
                .summaries
                .get(index)
                .and_then(|summary| summary.cover.clone());
            if cover.as_deref() == Some(url) {
                row.cover = image.clone();
                row.cover_state = 0;
                self.novel.rows.set_row_data(index, row);
                rows_changed = true;
            }
        }
        if rows_changed {
            let ui = self.ui();
            ui.set_novel_rows(ModelRc::from(self.novel.rows.clone()));
        }
    }

    /// One step in the series, from the reader.
    pub(crate) fn reader_chapter_target(&self, delta: i32) -> Option<u64> {
        if self.novel.chapter_ids.is_empty() {
            let series = self.novel.novel.as_ref()?.series.as_ref()?;
            return if delta < 0 {
                series.prev.map(|id| id.get())
            } else {
                series.next.map(|id| id.get())
            };
        }
        let next = self.novel.chapter_index as i32 + delta;
        if next < 0 {
            return None;
        }
        self.novel.chapter_ids.get(next as usize).copied()
    }

    /// Escape inside the reader: settings first, then the hidden toolbar.
    ///
    /// Returns true when the key was consumed, so the shell falls through to its
    /// own behaviour (leaving the page) only when the reader has nothing open.
    pub(crate) fn reader_escape(&mut self) -> bool {
        if self.novel.reader.settings_open {
            self.novel.reader.settings_open = false;
            self.publish_reader();
            return true;
        }
        if self.novel.prefs.hide_chrome {
            self.novel.prefs.hide_chrome = false;
            self.save_prefs(false);
            self.rerender_reader();
            return true;
        }
        false
    }

    /// The reader's chrome toggles, driven by the page's own buttons.
    pub(crate) fn reader_toggle_settings(&mut self) {
        let open = !self.novel.reader.settings_open;
        self.novel.reader.settings_open = open;
        self.publish_reader();
    }

    pub(crate) fn reader_toggle_chrome(&mut self) {
        self.novel.prefs.hide_chrome = !self.novel.prefs.hide_chrome;
        self.save_prefs(false);
        self.rerender_reader();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B9 - an arriving illustration reads the row that shows it, not the body.
    ///
    /// Every embedded image that arrived walked 0..blocks.row_count(), cloned each row
    /// and linearly searched the image table: an 800-block chapter paid 800 clones and
    /// 800 comparisons per picture. The plant that makes this red is
    /// [patch_block_image] walking the model, which is the shape it had.
    #[test]
    fn b9_an_arriving_image_patches_only_the_row_that_shows_it() {
        use crate::app::helpers::testkit::CountingModel;

        const IMAGE_ROW: usize = 500;
        let rows: Vec<NovelBlock> = (0..800)
            .map(|index| NovelBlock {
                kind: if index == IMAGE_ROW { K_IMAGE } else { K_PARA },
                text: "段".into(),
                label: "".into(),
                image: slint::Image::default(),
                state: if index == IMAGE_ROW { 1 } else { 0 },
                id: if index == IMAGE_ROW {
                    "img1".into()
                } else {
                    "".into()
                },
            })
            .collect();

        let mut sources: std::collections::BTreeMap<String, parse::ImageInfo> =
            std::collections::BTreeMap::new();
        sources.insert(
            "img1".to_string(),
            parse::ImageInfo {
                small: "https://i.pximg.net/novel/small.jpg".into(),
                large: "https://i.pximg.net/novel/large.jpg".into(),
            },
        );
        let index = block_image_index(&rows, &sources);
        assert_eq!(
            index.get("https://i.pximg.net/novel/large.jpg").map(Vec::as_slice),
            Some([IMAGE_ROW].as_slice()),
            "the big URL names the row that shows it"
        );
        assert_eq!(
            index.get("https://i.pximg.net/novel/small.jpg").map(Vec::as_slice),
            Some([IMAGE_ROW].as_slice())
        );

        let model = CountingModel::new(rows);
        let shown_at = index
            .get("https://i.pximg.net/novel/large.jpg")
            .cloned()
            .unwrap_or_default();
        let image = slint::Image::default();
        assert_eq!(patch_block_image(&model, &shown_at, &image), 1);
        assert_eq!(model.reads(), 1, "one row read, not eight hundred");
        assert_eq!(model.writes(), 1);

        // The shape this replaced, measured on the same model: the whole body, per
        // arriving image.
        model.reset();
        for row_index in 0..model.row_count() {
            let _ = model.row_data(row_index);
        }
        assert_eq!(model.reads(), 800, "the old walk cloned every block");
    }

    /// B8 - the second page extends the list instead of replacing it.
    ///
    /// The rows were rebuilt from the page that just arrived while the summaries
    /// accumulated them, so "load more" erased what was on screen. The plant that
    /// makes this red is [adopt_novel_page] deriving the rows from the arriving page,
    /// which is the shape it had.
    #[test]
    fn b8_a_second_page_keeps_the_first_on_screen() {
        let images: HashMap<String, slint::Image> = HashMap::new();
        let mut summaries: Vec<NovelSummary> = (1..=24).map(novel).collect();

        let second_page: Vec<NovelSummary> = (25..=48).map(novel).collect();
        let (rows, _) = adopt_novel_page(&mut summaries, second_page, &images);

        assert_eq!(rows.len(), 48, "both pages are on screen");
        assert_eq!(rows[0].id.as_str(), "1", "page one is still the top of the list");
        assert_eq!(rows[47].id.as_str(), "48", "and page two is below it");
        assert_eq!(summaries.len(), 48, "the list the model is built from agrees");
    }

    /// B8 - one page in flight, and a page that failed asks for itself again.
    ///
    /// next_page had no slot and no rollback: two clicks asked the server for the
    /// same page twice, and a failure left the cursor one page ahead, so the page
    /// that never arrived could not be requested again.
    #[test]
    fn b8_the_more_slot_takes_one_page_at_a_time() {
        let mut state = NovelState::new();
        state.page = 1;
        state.last_page = 3;

        assert!(state.take_more(), "the first click asks for page 2");
        assert_eq!(state.page, 2);
        assert!(
            !state.take_more(),
            "the second click is dropped while one is in flight"
        );
        assert_eq!(state.page, 2, "the cursor did not move twice");

        // The page failed: the cursor goes back so the retry asks for page 2 again.
        state.release_more(false);
        assert!(!state.loading_more);
        assert_eq!(state.page, 1);

        // The page landed: the cursor stays where the answer put it.
        assert!(state.take_more());
        state.release_more(true);
        assert_eq!(state.page, 2);

        // The end of the list is not a page to ask for.
        state.page = 3;
        assert!(!state.take_more());
    }

    /// One search-result summary whose title names its position.
    fn novel(id: u64) -> NovelSummary {
        NovelSummary {
            id: Id(id),
            title: format!("n{id}"),
            ..NovelSummary::default()
        }
    }

    #[test]
    fn thousands_groups_every_three_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(4_025), "4,025");
        assert_eq!(thousands(82_847), "82,847");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    /// F4: the row's percentage is `progress_percent(block, char)`, a row with no
    /// position has -1 (and therefore draws no rule), and a document that changed
    /// underneath the saved position clamps into 0..=100.
    #[test]
    fn a_recent_row_prints_the_percentage_of_its_stored_position() {
        // The stored position: block 7 of the document, 120 characters in.
        assert_eq!(progress_percent(7, 120, 0.42), Some(42.0));
        assert_eq!(progress_percent(1, 0, 0.5), Some(50.0));
        // Nothing recorded: a zero fraction with the position still at the start of
        // the first block draws no rule rather than a 0 % one.
        assert_eq!(progress_percent(0, 0, 0.0), None);
        assert_eq!(progress_percent(0, 0, -1.0), None, "-1 is not a position");
        assert_eq!(progress_percent(0, 0, f32::NAN), None, "nor is NaN");
        // A real position that rounds to zero is still a position, and the two
        // coordinates are what say so.
        assert_eq!(progress_percent(0, 12, 0.004), Some(0.4));

        // The document changed underneath: the stored fraction can exceed 1.
        assert_eq!(progress_percent(99, 9_000, 1.34), Some(100.0));
        assert_eq!(progress_percent(0, 4, -0.25), Some(0.0));

        // The shape `load_novels_page` publishes: -1 when the store has no row.
        // Written through a function so the literal-`None` lint cannot rewrite the
        // shape being asserted (clippy::unnecessary_literal_unwrap).
        fn published(stored: Option<f32>) -> f32 {
            stored.unwrap_or(-1.0)
        }
        assert!(published(None) < 0.0, "a row at -1 draws no rule");
    }

    #[test]
    fn labels_never_print_a_zero_they_cannot_support() {
        // Pixiv reports 0 for "unknown" on several counters; a reader that shows
        // "0 字" is stating something it does not know.
        assert_eq!(characters_label(0), "");
        assert_eq!(characters_label(4_025), "4,025 字");
        assert_eq!(reading_label(0), "");
        assert_eq!(reading_label(1), "约 1 分钟");
        assert_eq!(reading_label(60), "约 1 分钟");
        assert_eq!(reading_label(61), "约 2 分钟");
    }

    #[test]
    fn stats_line_drops_the_fields_the_payload_left_empty() {
        let mut novel = Novel {
            character_count: 4_025,
            reading_time: 483,
            views: 4,
            ..Novel::default()
        };
        assert_eq!(stats_line(&novel), "4,025 字 · 约 9 分钟 · 4 浏览");
        novel.views = 0;
        novel.bookmarks = 12;
        assert_eq!(stats_line(&novel), "4,025 字 · 约 9 分钟 · 12 收藏");
        novel.character_count = 0;
        novel.reading_time = 0;
        novel.bookmarks = 0;
        assert_eq!(stats_line(&novel), "");
    }

    #[test]
    fn ratios_clamp_at_both_ends_and_round_trip() {
        assert_eq!(ratio_of(5.0, 0.0, 10.0), 0.5);
        assert_eq!(ratio_of(-1.0, 0.0, 10.0), 0.0);
        assert_eq!(ratio_of(99.0, 0.0, 10.0), 1.0);
        // A degenerate range must not divide by zero.
        assert_eq!(ratio_of(3.0, 4.0, 4.0), 0.0);
        assert!((value_of(0.5, 0.0, 10.0) - 5.0).abs() < f32::EPSILON);
        assert!((value_of(2.0, 0.0, 10.0) - 10.0).abs() < f32::EPSILON);
    }

    /// A preference set that differs from the default in **every** field, so a
    /// reset is visible field by field instead of being masked by a coincidence.
    fn exotic_prefs() -> ReaderPrefs {
        ReaderPrefs {
            font_family: "Source Han Serif".into(),
            font_size: 26.0,
            font_weight: 600,
            italic: true,
            line_height: 2.4,
            letter_spacing: 1.5,
            paragraph_spacing: 20.0,
            first_line_indent: 2.0,
            margin_x: 120.0,
            max_width: 900.0,
            align: novel_prefs::Align::Center,
            paragraph_rule: novel_prefs::ParagraphRule::Rule,
            theme: novel_prefs::ReaderTheme::Sepia,
            custom_background: "#fffff0".into(),
            custom_ink: "#101010".into(),
            ruby: parse::RubyMode::Hidden,
            hide_chrome: true,
            show_progress: false,
            paged: true,
        }
        .clamped()
    }

    #[test]
    fn an_unknown_reader_opcode_is_inert_and_writes_nothing() {
        // Plan R10 §2.3 O4, the whole acceptance: an opcode this build does not
        // know must change no field and must not reach set_novel_setting. The
        // fallback used to be ReaderPrefs::default(), which reset all nineteen
        // settings and saved the reset over the user's row.
        let mut state = NovelState::new();
        state.prefs = exotic_prefs();
        let before = state.prefs.clone();

        // Exactly the shape of App::apply_reader_edit: one persistence write per
        // opcode the state says it understood.
        let mut writes = 0;
        for op in [-1, 19, 23, 42, 99, i32::MAX] {
            if state.apply_edit(op, 0.75, "wiped") != ReaderEdit::Ignored {
                writes += 1;
            }
        }
        assert_eq!(
            writes, 0,
            "an unknown opcode must not reach set_novel_setting"
        );

        // Nineteen fields, one assertion each, so a failure names what moved.
        let after = &state.prefs;
        assert_eq!(after.font_family, before.font_family, "font_family");
        assert_eq!(after.font_size, before.font_size, "font_size");
        assert_eq!(after.font_weight, before.font_weight, "font_weight");
        assert_eq!(after.italic, before.italic, "italic");
        assert_eq!(after.line_height, before.line_height, "line_height");
        assert_eq!(
            after.letter_spacing, before.letter_spacing,
            "letter_spacing"
        );
        assert_eq!(
            after.paragraph_spacing, before.paragraph_spacing,
            "paragraph_spacing"
        );
        assert_eq!(
            after.first_line_indent, before.first_line_indent,
            "first_line_indent"
        );
        assert_eq!(after.margin_x, before.margin_x, "margin_x");
        assert_eq!(after.max_width, before.max_width, "max_width");
        assert_eq!(after.align, before.align, "align");
        assert_eq!(
            after.paragraph_rule, before.paragraph_rule,
            "paragraph_rule"
        );
        assert_eq!(after.theme, before.theme, "theme");
        assert_eq!(
            after.custom_background, before.custom_background,
            "custom_background"
        );
        assert_eq!(after.custom_ink, before.custom_ink, "custom_ink");
        assert_eq!(after.ruby, before.ruby, "ruby");
        assert_eq!(after.hide_chrome, before.hide_chrome, "hide_chrome");
        assert_eq!(after.show_progress, before.show_progress, "show_progress");
        assert_eq!(after.paged, before.paged, "paged");
        assert_eq!(state.prefs, before, "the whole value, field for field");
    }

    #[test]
    fn every_opcode_the_panel_sends_is_one_this_build_understands() {
        // The other half of O4: the no-op must be reserved for opcodes that do not
        // exist. If an arm were lost, the panel would go silent instead of wiping
        // the settings, and this is the assertion that says so.
        let mut state = NovelState::new();
        state.prefs = exotic_prefs();
        for op in 0..=18 {
            assert!(
                state.apply_edit(op, 0.5, "#123456") != ReaderEdit::Ignored,
                "opcode {op} is one ReaderSettings sends"
            );
        }
    }

    /// A body with 800 blocks, one of them carrying ruby, so the two reflowing
    /// opcodes have something to change.
    fn eight_hundred_blocks() -> Document {
        let mut body = String::new();
        for i in 0..799 {
            body.push_str(&format!("第{i}段。[[rb:漢字 > かんじ]]の行。\n\n"));
        }
        body.push_str("最后一段。");
        parse::parse(&body, &std::collections::BTreeMap::new())
    }

    #[test]
    fn a_restyle_keeps_the_body_model_and_a_reflow_rebuilds_it() {
        // Plan R10 §2.3 O5, the acceptance as a count: on an 800-block document a
        // colour edit must not rebuild the rows, ten consecutive drags must not
        // either, and only the opcodes whose *text* changes may replace the model.
        // `ReaderEdit::Reflowed` is the only answer that touches `blocks` inside
        // `NovelState::apply_edit`, so counting it counts `rows_from` + `set_vec`.
        let mut state = NovelState::new();
        state.prefs = exotic_prefs();
        state.document = Some(eight_hundred_blocks());
        state.build_body();

        let mut rebuilds = 0;
        let count = |edit: ReaderEdit, rebuilds: &mut u32| {
            if edit == ReaderEdit::Reflowed {
                *rebuilds += 1;
            }
        };

        // One theme change: the paper changes, the paragraphs do not.
        let paper_before = reader_view(
            &state.prefs,
            0,
            "",
            &ReaderDoc {
                title: "",
                chapter_meta: "",
                prev: "",
                next: "",
            },
            false,
            false,
        )
        .paper;
        count(state.apply_edit(0, 2.0, ""), &mut rebuilds);
        let paper_after = reader_view(
            &state.prefs,
            0,
            "",
            &ReaderDoc {
                title: "",
                chapter_meta: "",
                prev: "",
                next: "",
            },
            false,
            false,
        )
        .paper;
        assert_ne!(paper_after, paper_before, "the theme edit reached the view");
        assert_eq!(rebuilds, 0, "a theme change rebuilt the body");

        // Ten font-size events, i.e. one slider drag. None may rebuild.
        for _ in 0..10 {
            count(state.apply_edit(4, 0.62, ""), &mut rebuilds);
        }
        assert_eq!(rebuilds, 0, "ten drag events rebuilt the body");

        // The indent is flattened into each paragraph's text: it must rebuild.
        count(state.apply_edit(8, 0.5, ""), &mut rebuilds);
        assert_eq!(rebuilds, 1, "the indent is baked into the rows");
        let indented = state.blocks.row_data(0).expect("a first row");
        assert!(
            indented.text.starts_with('\u{3000}'),
            "the rebuilt paragraph carries its indent: {:?}",
            indented.text
        );

        // Ruby flattening is the other one.
        count(state.apply_edit(15, 0.0, ""), &mut rebuilds);
        assert_eq!(rebuilds, 2, "the ruby mode is baked into the rows");
        let hidden = state.blocks.row_data(0).expect("a first row");
        assert!(
            !hidden.text.contains('（'),
            "ruby hidden: {:?}",
            hidden.text
        );

        // And a colour edit after all of that still does not.
        count(state.apply_edit(1, 0.0, "#102030"), &mut rebuilds);
        assert_eq!(rebuilds, 2, "a custom background rebuilt the body");
    }

    #[test]
    fn every_preference_opcode_round_trips_through_the_view() {
        // The panel writes ratios; the view publishes them back as ratios. If the
        // two disagreed, a slider would jump when the settings panel was reopened.
        let prefs = ReaderPrefs::default();
        let view = reader_view(
            &prefs,
            0,
            "",
            &ReaderDoc {
                title: "",
                chapter_meta: "",
                prev: "",
                next: "",
            },
            false,
            false,
        );
        assert_eq!(view.weight_index, weight_index(prefs.font_weight));
        assert_eq!(view.theme_index, theme_index(prefs.theme));
        assert_eq!(view.align, align_index(prefs.align));
        assert_eq!(view.rule, rule_index(prefs.paragraph_rule));
        assert!(
            (value_of(
                view.font_size_ratio,
                novel_prefs::MIN_FONT_SIZE,
                novel_prefs::MAX_FONT_SIZE
            ) - prefs.font_size)
                .abs()
                < 0.01
        );
        assert!(
            (value_of(
                view.line_height_ratio,
                novel_prefs::MIN_LINE_HEIGHT,
                novel_prefs::MAX_LINE_HEIGHT
            ) - prefs.line_height)
                .abs()
                < 0.01
        );
        assert!(
            (value_of(
                view.measure_ratio,
                novel_prefs::MIN_MAX_WIDTH,
                novel_prefs::MAX_MAX_WIDTH
            ) - prefs.max_width)
                .abs()
                < 0.5
        );
    }

    #[test]
    fn the_reader_has_a_page_for_every_block_kind() {
        // A kind the page does not know draws nothing; the mapping therefore has to
        // cover the parser's whole vocabulary.
        use pixiv_novel::parse::BlockKind;
        let kinds = [
            BlockKind::Paragraph,
            BlockKind::Heading,
            BlockKind::Separator,
            BlockKind::PageBreak,
            BlockKind::Image,
        ];
        let mapped: Vec<i32> = kinds
            .iter()
            .map(|kind| match kind {
                BlockKind::Paragraph => K_PARA,
                BlockKind::Heading => K_HEAD,
                BlockKind::Separator => K_RULE,
                BlockKind::PageBreak => K_BREAK,
                BlockKind::Image => K_IMAGE,
            })
            .collect();
        assert_eq!(mapped, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_captured_body_renders_rows_without_its_markup() {
        // The fixture is a trimmed live capture: it must produce read rows, and no
        // markup may survive into the text a reader sees.
        let raw = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/novel_detail.json"),
        )
        .expect("the novel fixture is committed");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("fixture is JSON");
        let body = value.get("body").cloned().unwrap_or(value);
        let wire: pixiv_types::wire::NovelDetailWire =
            serde_json::from_value(body).expect("fixture decodes as a detail document");
        let novel = Novel::from_detail(wire);
        let images: std::collections::BTreeMap<String, parse::ImageInfo> = novel
            .images
            .iter()
            .map(|image| {
                (
                    image.id.clone(),
                    parse::ImageInfo {
                        small: image.small.clone(),
                        large: image.large.clone(),
                    },
                )
            })
            .collect();
        let document = parse::parse(&novel.body, &images);
        let rows = rows_from(&document, &ReaderPrefs::default());
        assert!(!rows.is_empty(), "a captured body must produce rows");
        assert!(document.characters > 100, "the body has text");
        for row in &rows {
            assert!(
                !row.text.contains("[[rb:"),
                "ruby markup leaked: {}",
                row.text
            );
            assert!(!row.text.contains("[uploadedimage:"), "image markup leaked");
            assert!(!row.text.contains("[newpage]"), "page-break markup leaked");
        }
        assert!(
            rows.iter().any(|row| row.kind == K_IMAGE),
            "the capture embeds two images"
        );
    }
}
