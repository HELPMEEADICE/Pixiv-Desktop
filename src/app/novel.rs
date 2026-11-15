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
            loading: HashSet::new(),
            reader_novel: None,
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
    }
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
            let rows = store.novel_history(RECENT_KEEP).unwrap_or_default();
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                let recent = rows
                    .iter()
                    .map(|entry| NovelRow {
                        id: entry.novel_id.clone().into(),
                        title: entry.title.clone().into(),
                        author: entry.author.clone().into(),
                        meta: "最近阅读".into(),
                        cover: slint::Image::default(),
                        cover_state: 2,
                        series: entry.series_id.clone().into(),
                    })
                    .collect::<Vec<_>>();
                app.novel.recent.set_vec(recent);
                let ui = app.ui();
                ui.set_novel_recent(ModelRc::from(app.novel.recent.clone()));
                let urls: Vec<String> = rows
                    .iter()
                    .map(|entry| entry.cover.clone())
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
                app.publish_novels_state(1, "");
                app.novel.page = app.novel.page.saturating_add(1);
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
                    return;
                }
                match result {
                    Ok(found) => {
                        app.novel.last_page = found.last_page.max(1);
                        let mut rows: Vec<NovelRow> = Vec::with_capacity(found.novels.len());
                        let mut covers: Vec<String> = Vec::new();
                        for summary in &found.novels {
                            let mut row = summary_row(summary, 2);
                            if let Some(cover) = summary.cover.as_deref() {
                                covers.push(cover.to_string());
                                if let Some(image) = app.novel.images.get(cover) {
                                    row.cover = image.clone();
                                    row.cover_state = 0;
                                }
                            }
                            rows.push(row);
                        }
                        if app.novel.summaries.is_empty() {
                            app.novel.summaries = found.novels;
                        } else {
                            app.novel.summaries.extend(found.novels);
                        }
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
                        let message = novel_error_text(&error);
                        app.publish_novels_state(3, &message);
                    }
                }
            });
        });
    }
}

/// One sentence a user can act on, per failure family.
pub(crate) fn novel_error_text(error: &PixivError) -> String {
    match error {
        PixivError::Unauthorized => "登录已失效：请在设置中重新登录后再试".to_string(),
        PixivError::Network(_) => "无法连接 Pixiv：请检查网络或代理设置".to_string(),
        PixivError::Decode(_) => "Pixiv 返回了无法解析的小说数据（接口可能已变更）".to_string(),
        PixivError::Http { status, .. } if *status == 404 => {
            "这本小说不存在，或作者已删除/设为私密".to_string()
        }
        PixivError::Api(message) if !message.is_empty() => message.clone(),
        other => other.to_string(),
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
            let progress = store.novel_progress(&id.to_string()).ok().flatten();
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
        let series_meta = nav
            .as_ref()
            .map(|nav| {
                format!(
                    "第 {} 章 · {}",
                    nav.order,
                    if nav.concluded {
                        "已完结"
                    } else {
                        "连载中"
                    }
                )
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
            let progress = store.novel_progress(&id.to_string()).ok().flatten();
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
        let rows = rows_from(&document, &self.novel.prefs);
        let urls: Vec<String> = rows
            .iter()
            .filter(|row| row.kind == K_IMAGE)
            .filter_map(|row| {
                let info = images.get(row.id.as_str())?;
                if info.large.is_empty() {
                    Some(info.small.clone())
                } else {
                    Some(info.large.clone())
                }
            })
            .filter(|url| !url.is_empty())
            .collect();
        self.novel.blocks.set_vec(rows);
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
        self.novel.document = Some(document);
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
        let mut prefs = self.novel.prefs.clone();
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
            _ => prefs = ReaderPrefs::default(),
        }
        self.novel.prefs = prefs.clamped();
        self.save_prefs(false);
        self.rerender_reader();
    }

    /// Re-render the body after a preference change and keep the place.
    ///
    /// The restore is what makes a font-size change feel like a reflow instead of a
    /// jump: the ratio the page last reported is applied again once the new layout
    /// exists, so the paragraph the user was reading stays under their eyes.
    fn rerender_reader(&mut self) {
        let ratio = self.novel.ratio;
        if let Some(document) = self.novel.document.clone() {
            let rows = rows_from(&document, &self.novel.prefs);
            self.novel.blocks.set_vec(rows);
        }
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

        // Reader body: only the image rows can change, so only they are rewritten.
        let mut patched = false;
        for row_index in 0..self.novel.blocks.row_count() {
            let Some(row) = self.novel.blocks.row_data(row_index) else {
                continue;
            };
            if row.kind != K_IMAGE || row.state == 0 {
                continue;
            }
            let Some(info) = self
                .novel
                .novel
                .as_ref()
                .and_then(|novel| novel.images.iter().find(|info| info.id == row.id.as_str()))
            else {
                continue;
            };
            if info.large == url || info.small == url {
                let mut next = row.clone();
                next.image = image.clone();
                next.state = 0;
                self.novel.blocks.set_row_data(row_index, next);
                patched = true;
            }
        }
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
