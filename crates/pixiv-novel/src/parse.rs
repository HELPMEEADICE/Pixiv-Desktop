//! The Pixiv novel body grammar, implemented as a total function.
//!
//! # The rule this module enforces
//!
//! Parsing never fails and never deletes content. Every input maps to a
//! [`Document`]: a marker the grammar knows becomes structure, a marker it does
//! not know stays literal text. There is no error type, no [`Result`], and no
//! input path that drops a character from the visible output.
//!
//! # The grammar
//!
//! Everything below was observed in a live `/ajax/novel/{id}` body (see
//! `.local-secrets/probe/novel-detail.json` for the capture this crate was
//! written against); nothing here is guessed from documentation.
//!
//! | marker                                  | result                                   |
//! |-----------------------------------------|------------------------------------------|
//! | `[newpage]`                             | [`BlockKind::PageBreak`], its own block   |
//! | `[chapter:title]` / `[chapter:]`        | [`BlockKind::Heading`]                    |
//! | `[[rb:base > reading]]`                  | [`Span::Ruby`]                            |
//! | `[pixivimage:12345-2]`, ``[[...]]``     | [`Span::WorkRef`], page suffix dropped    |
//! | `[[jumpuri:label > url]]`, single form  | [`Span::Link`]                            |
//! | `[[jump:12345]]`                        | [`Span::Link`] to `jump:12345`            |
//! | `[uploadedimage:26247899]`              | [`BlockKind::Image`]                      |
//! | a line of `***` or `---`                 | [`BlockKind::Separator`]                  |
//! | blank line                              | paragraph boundary                       |
//! | anything else in `[...]`                | literal text, never interpreted          |
//!
//! Degradation rules, all of them observable in the tests:
//!
//! * An unclosed `[[rb:` or `[[jumpuri:` stays literal. A marker's inner text
//!   may not contain `[` or a newline, so an unclosed marker can never reach
//!   forward and swallow the tail of the document.
//! * `[uploadedimage:id]` becomes an image block *anywhere*, including mid-line;
//!   the paragraph is split around it. The block always carries a
//!   `[图片 id]` label in its spans, present or missing, so `plain` does not
//!   change length when the image map arrives later — stored character offsets
//!   would otherwise shift under the reader.
//! * `[chapter:...]` is only recognised when it is the whole line. Mid-line it
//!   is literal text, like any other unknown marker.

use std::collections::{BTreeMap, BTreeSet};

/// Where one uploaded image can be fetched from.
///
/// The two sizes are pre-resolved by the caller from the API's
/// `urls` map (`240mw` for `small`, `1200x1200` for `large`); this crate never
/// sees the raw API shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageInfo {
    pub small: String,
    pub large: String,
}

/// One inline run of text inside a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    /// Plain text, carried byte for byte.
    Text(String),
    /// `[[rb:base > reading]]` — furigana over a base word.
    Ruby { base: String, reading: String },
    /// A link. `href` is a real URL, or `jump:<id>` for an in-site jump.
    Link { label: String, href: String },
    /// An embedded pixiv work (`[pixivimage:`). `id` is the work id with the
    /// page suffix removed; `label` is what a text-only surface shows.
    WorkRef { id: String, label: String },
}

/// What a block *is*, which is all the reader needs to pick a template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading,
    Separator,
    PageBreak,
    Image,
}

/// One row of the reader.
///
/// `image_id` is empty unless `kind` is [`BlockKind::Image`]. `missing` is true
/// only for an image block whose id was absent from the map handed to
/// [`parse`]; the reader then draws a placeholder instead of a broken picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    pub spans: Vec<Span>,
    pub image: Option<ImageInfo>,
    pub image_id: String,
    pub missing: bool,
}

/// A parsed novel body.
///
/// `plain` is the whole body as readable text — the same string the reader would
/// produce with ruby shown inline — used for search, for copy, and as the space
/// that `char_offset` indexes. `characters` is `plain.chars().count()`: scalar
/// values, not bytes, because that is what a text cursor counts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    pub blocks: Vec<Block>,
    pub images: Vec<ImageInfo>,
    pub plain: String,
    pub characters: usize,
}

/// How `[[rb:base > reading]]` is drawn.
///
/// Serialisable so the value can live inside [`crate::prefs::ReaderPrefs`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RubyMode {
    /// For non-Japanese readers: the reading is dropped.
    Hidden,
    /// `base（reading）` — the default, because losing the reading loses meaning
    /// for a reader who does not know the kanji.
    #[default]
    Inline,
}

const NEW_PAGE: &str = "[newpage]";
const RB_OPEN: &str = "[[rb:";
const CHAPTER_OPEN: &str = "[chapter:";
const PIXIV_IMAGE: &str = "[pixivimage:";
const PIXIV_IMAGE_NESTED: &str = "[[pixivimage:";
const JUMPURI: &str = "[jumpuri:";
const JUMPURI_NESTED: &str = "[[jumpuri:";
const JUMP: &str = "[jump:";
const JUMP_NESTED: &str = "[[jump:";
const UPLOADED_IMAGE: &str = "[uploadedimage:";
const CLOSE_NESTED: &str = "]]";
const CLOSE_FLAT: &str = "]";

/// Parse one novel body.
///
/// `images` maps an uploaded-image id to its URLs; an id that is absent becomes
/// an image block with [`Block::missing`] set rather than an error, because the
/// body must remain readable while the image map is still loading.
#[must_use]
pub fn parse(body: &str, images: &BTreeMap<String, ImageInfo>) -> Document {
    let normalised = normalise(body);
    let mut blocks: Vec<Block> = Vec::new();
    let mut paragraph: Vec<Span> = Vec::new();

    for line in normalised.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush_paragraph(&mut paragraph, &mut blocks);
            continue;
        }
        if let Some(title) = chapter_title(trimmed) {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block {
                kind: BlockKind::Heading,
                spans: vec![Span::Text(title)],
                image: None,
                image_id: String::new(),
                missing: false,
            });
            continue;
        }
        if is_separator(trimmed) {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block {
                kind: BlockKind::Separator,
                spans: Vec::new(),
                image: None,
                image_id: String::new(),
                missing: false,
            });
            continue;
        }

        let mut parts: Vec<Part> = Vec::new();
        scan(line, &mut parts);
        for part in parts {
            match part {
                Part::Spans(spans) => append_line(&mut paragraph, spans),
                Part::Image(id) => {
                    flush_paragraph(&mut paragraph, &mut blocks);
                    blocks.push(image_block(&id, images));
                }
                Part::PageBreak => {
                    flush_paragraph(&mut paragraph, &mut blocks);
                    blocks.push(Block {
                        kind: BlockKind::PageBreak,
                        spans: Vec::new(),
                        image: None,
                        image_id: String::new(),
                        missing: false,
                    });
                }
            }
        }
    }
    flush_paragraph(&mut paragraph, &mut blocks);

    let plain = blocks
        .iter()
        .map(|block| render(block, RubyMode::Inline))
        .collect::<Vec<_>>()
        .join("\n");

    // Resolved images in document order, each id once: the reader prefetches
    // this list up front, and an id repeated in the body must not be fetched
    // twice.
    let mut resolved: Vec<ImageInfo> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for block in &blocks {
        if let Some(info) = &block.image {
            if seen.insert(block.image_id.as_str()) {
                resolved.push(info.clone());
            }
        }
    }

    Document {
        characters: plain.chars().count(),
        blocks,
        images: resolved,
        plain,
    }
}

/// Flatten one block into the single string a reader row shows.
///
/// `ruby` only changes how [`Span::Ruby`] flattens; every other span renders the
/// same in both modes.
#[must_use]
pub fn render(block: &Block, ruby: RubyMode) -> String {
    let mut out = String::new();
    for span in &block.spans {
        match span {
            Span::Text(text) => out.push_str(text),
            Span::Ruby { base, reading } => {
                out.push_str(base);
                if ruby == RubyMode::Inline && !reading.is_empty() {
                    out.push('（');
                    out.push_str(reading);
                    out.push('）');
                }
            }
            // A link and a work reference both read as their label in flat text;
            // the href is carried separately for the UI's click handler.
            Span::Link { label, .. } | Span::WorkRef { label, .. } => out.push_str(label),
        }
    }
    out
}

/// Turn the caption HTML the API returns into readable text.
///
/// `<br />` becomes a newline, an anchor becomes `label (href)` so the URL
/// survives in plain text, any other tag is dropped, and entities are decoded
/// last — decoding after stripping means an escaped `&lt;b&gt;` stays literal
/// text instead of being mistaken for a tag.
#[must_use]
pub fn clean_caption(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        let rest = &html[i..];
        if !rest.starts_with('<') {
            if let Some(ch) = rest.chars().next() {
                out.push(ch);
                i += ch.len_utf8();
            }
            continue;
        }
        let Some(gt) = rest.find('>') else {
            // A stray '<' with no '>' is text, not a tag.
            out.push_str(rest);
            break;
        };
        let tag = &rest[..=gt];
        let lower = tag.to_ascii_lowercase();
        if lower.starts_with("<br") {
            out.push('\n');
            i += gt + 1;
            continue;
        }
        if lower.starts_with("<a ") || lower.starts_with("<a>") {
            let after_open = i + gt + 1;
            let following = &html[after_open..];
            let close = following.to_ascii_lowercase().find("</a>");
            if let Some(close) = close {
                let label = &following[..close];
                let href = attribute(tag, "href");
                out.push_str(label.trim());
                if !href.is_empty() {
                    out.push_str(" (");
                    out.push_str(&href);
                    out.push(')');
                }
                i = after_open + close + 4;
                continue;
            }
            // No closing tag: the label is the rest of the caption.
            let href = attribute(tag, "href");
            out.push_str(following.trim());
            if !href.is_empty() {
                out.push_str(" (");
                out.push_str(&href);
                out.push(')');
            }
            break;
        }
        // Any other tag is markup the reader cannot show; drop the tag, keep the
        // text between tags.
        i += gt + 1;
    }
    decode_entities(&out)
}

// ---------------------------------------------------------------------------
// line scanning
// ---------------------------------------------------------------------------

/// One contiguous piece of a line, after markers have been recognised.
enum Part {
    Spans(Vec<Span>),
    Image(String),
    PageBreak,
}

/// What a recognised marker turns into.
enum Marker {
    Span(Span),
    Image(String),
    PageBreak,
    /// A recognised marker that carries nothing (an empty ruby pair). Dropped so
    /// it cannot leave an empty span behind.
    Nothing,
}

/// Split one line into spans, image blocks and page breaks.
fn scan(line: &str, out: &mut Vec<Part>) {
    let mut spans: Vec<Span> = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < line.len() {
        let rest = &line[i..];
        if rest.starts_with('[') {
            if let Some((marker, used)) = marker(rest) {
                push_text(&mut spans, &mut text);
                match marker {
                    Marker::Span(span) => spans.push(span),
                    Marker::Image(id) => {
                        if !spans.is_empty() {
                            out.push(Part::Spans(std::mem::take(&mut spans)));
                        }
                        out.push(Part::Image(id));
                    }
                    Marker::PageBreak => {
                        if !spans.is_empty() {
                            out.push(Part::Spans(std::mem::take(&mut spans)));
                        }
                        out.push(Part::PageBreak);
                    }
                    Marker::Nothing => {}
                }
                i += used;
                continue;
            }
        }
        match rest.chars().next() {
            Some(ch) => {
                text.push(ch);
                i += ch.len_utf8();
            }
            None => break,
        }
    }
    push_text(&mut spans, &mut text);
    if !spans.is_empty() {
        out.push(Part::Spans(spans));
    }
}

fn push_text(spans: &mut Vec<Span>, text: &mut String) {
    if !text.is_empty() {
        spans.push(Span::Text(std::mem::take(text)));
    }
}

/// Recognise the marker at the start of `rest`.
///
/// Returns the marker and how many bytes it occupies, or [`None`] when `rest`
/// does not start with a marker the grammar knows — the caller then keeps the
/// `[` as literal text, which is what makes an unknown marker degrade instead of
/// disappearing.
fn marker(rest: &str) -> Option<(Marker, usize)> {
    if rest.starts_with(NEW_PAGE) {
        return Some((Marker::PageBreak, NEW_PAGE.len()));
    }
    if rest.starts_with(RB_OPEN) {
        return closed(rest, RB_OPEN.len(), CLOSE_NESTED).map(|end| {
            let inner = &rest[RB_OPEN.len()..end - CLOSE_NESTED.len()];
            (ruby(inner), end)
        });
    }
    if rest.starts_with(PIXIV_IMAGE_NESTED) {
        return nested(rest, PIXIV_IMAGE_NESTED, work_ref);
    }
    if rest.starts_with(PIXIV_IMAGE) {
        return flat(rest, PIXIV_IMAGE, work_ref);
    }
    if rest.starts_with(JUMPURI_NESTED) {
        return nested(rest, JUMPURI_NESTED, link);
    }
    if rest.starts_with(JUMPURI) {
        return flat(rest, JUMPURI, link);
    }
    if rest.starts_with(JUMP_NESTED) {
        return nested(rest, JUMP_NESTED, jump);
    }
    if rest.starts_with(JUMP) {
        return flat(rest, JUMP, jump);
    }
    if rest.starts_with(UPLOADED_IMAGE) {
        return flat(rest, UPLOADED_IMAGE, |inner| {
            let id = inner.trim();
            if id.is_empty() {
                None
            } else {
                Some(Marker::Image(id.to_string()))
            }
        });
    }
    None
}

/// A `[[name:...]]` marker.
fn nested(rest: &str, open: &str, build: fn(&str) -> Option<Marker>) -> Option<(Marker, usize)> {
    let end = closed(rest, open.len(), CLOSE_NESTED)?;
    let inner = &rest[open.len()..end - CLOSE_NESTED.len()];
    build(inner).map(|marker| (marker, end))
}

/// A `[name:...]` marker, which Pixiv also accepts.
fn flat(rest: &str, open: &str, build: fn(&str) -> Option<Marker>) -> Option<(Marker, usize)> {
    let end = closed(rest, open.len(), CLOSE_FLAT)?;
    let inner = &rest[open.len()..end - CLOSE_FLAT.len()];
    build(inner).map(|marker| (marker, end))
}

/// Byte length of the marker at the start of `s`, or [`None`] when it is not
/// closed before a `[` or a line break.
///
/// That bound is the whole safety story: without it an unclosed `[[rb:` would
/// search forward to the *next* marker's closing brackets and swallow everything
/// between them.
fn closed(s: &str, open_len: usize, close: &str) -> Option<usize> {
    let mut end = open_len;
    while end < s.len() {
        let tail = &s[end..];
        if tail.starts_with(close) {
            return Some(end + close.len());
        }
        // The loop condition makes this a formality, but clippy is right that the
        // fallible step belongs on the function boundary rather than in a let-else
        // that can never fire.
        let ch = tail.chars().next()?;
        if ch == '[' || ch == '\n' {
            return None;
        }
        end += ch.len_utf8();
    }
    None
}

/// `[[rb:base > reading]]`.
fn ruby(inner: &str) -> Marker {
    let (base, reading) = match inner.split_once('>') {
        Some((base, reading)) => (base.trim(), reading.trim()),
        // No separator: the whole inner text is the base. Content is kept.
        None => (inner.trim(), ""),
    };
    if base.is_empty() && reading.is_empty() {
        return Marker::Nothing;
    }
    if reading.is_empty() {
        // An empty reading would flatten to a stray pair of parentheses, so a
        // bare base is plain text.
        return Marker::Span(Span::Text(base.to_string()));
    }
    Marker::Span(Span::Ruby {
        base: base.to_string(),
        reading: reading.to_string(),
    })
}

/// `[pixivimage:12345-2]` — the `-2` is the page inside that work, not part of
/// the id, so it is dropped.
fn work_ref(inner: &str) -> Option<Marker> {
    let raw = inner.trim();
    let id = raw.split('-').next().unwrap_or_default().trim();
    if id.is_empty() {
        return None;
    }
    Some(Marker::Span(Span::WorkRef {
        id: id.to_string(),
        label: format!("作品 {id}"),
    }))
}

/// `[[jumpuri:label > url]]`.
fn link(inner: &str) -> Option<Marker> {
    let (label, href) = inner.split_once('>')?;
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let label = label.trim();
    Some(Marker::Span(Span::Link {
        // A link with no label still has to be visible, so the URL stands in.
        label: if label.is_empty() {
            href.to_string()
        } else {
            label.to_string()
        },
        href: href.to_string(),
    }))
}

/// `[[jump:12345]]` — a jump inside the site, kept as a link with a `jump:` url
/// so the model has one way to express "this is clickable".
fn jump(inner: &str) -> Option<Marker> {
    let id = inner.trim();
    if id.is_empty() {
        return None;
    }
    Some(Marker::Span(Span::Link {
        label: format!("跳转 {id}"),
        href: format!("jump:{id}"),
    }))
}

fn chapter_title(line: &str) -> Option<String> {
    if line.len() <= CHAPTER_OPEN.len() {
        return None;
    }
    if !line.starts_with(CHAPTER_OPEN) || !line.ends_with(']') {
        return None;
    }
    Some(line[CHAPTER_OPEN.len()..line.len() - 1].trim().to_string())
}

/// A line of three or more identical `*` or `-` is a horizontal rule.
///
/// The capture uses `***`; `---` is the other form Pixiv's editor emits. Longer
/// runs of the same character are the same rule, not a bullet list.
fn is_separator(line: &str) -> bool {
    let mut chars = line.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first != '*' && first != '-' {
        return false;
    }
    let mut count = 1usize;
    for ch in chars {
        if ch != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

fn image_block(id: &str, images: &BTreeMap<String, ImageInfo>) -> Block {
    let info = images.get(id).cloned();
    Block {
        missing: info.is_none(),
        image: info,
        kind: BlockKind::Image,
        // Always labelled, resolved or not: see the module header on why plain
        // must not change length when the image map arrives.
        spans: vec![Span::Text(format!("[图片 {id}]"))],
        image_id: id.to_string(),
    }
}

/// Append one source line to the open paragraph.
///
/// A paragraph keeps the author's hard line breaks: Japanese novels use them as
/// beats, and dropping one would join two lines into a sentence that never
/// existed.
fn append_line(paragraph: &mut Vec<Span>, spans: Vec<Span>) {
    if !paragraph.is_empty() {
        push_text_span(paragraph, "\n");
    }
    for span in spans {
        match span {
            Span::Text(text) => push_text_span(paragraph, &text),
            other => paragraph.push(other),
        }
    }
}

/// Push text, merging into a trailing [~Span::Text~].
///
/// Without the merge a paragraph wrapped over five lines would carry nine spans
/// (line, break, line, break ...) and the reader would allocate nine strings per
/// row instead of one.
fn push_text_span(spans: &mut Vec<Span>, text: &str) {
    match spans.last_mut() {
        Some(Span::Text(last)) => last.push_str(text),
        _ => spans.push(Span::Text(text.to_string())),
    }
}

fn flush_paragraph(paragraph: &mut Vec<Span>, blocks: &mut Vec<Block>) {
    if paragraph.is_empty() {
        return;
    }
    blocks.push(Block {
        kind: BlockKind::Paragraph,
        spans: std::mem::take(paragraph),
        image: None,
        image_id: String::new(),
        missing: false,
    });
}

/// Line endings and tabs, normalised before anything else looks at the text.
fn normalise(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                // CRLF is one line break, not two.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\t' => out.push_str("  "),
            _ => out.push(ch),
        }
    }
    out
}

/// The value of `name="..."` inside one tag, or an empty string.
///
/// Only an attribute *name* can match: `<a title="href=x" href="y">` carries the
/// text `href=` inside another attribute's value, and a plain substring search
/// handed `title`'s value back as the link - which then surfaced in the caption
/// as a URL the anchor never had. So the scan keeps its own state: skip quoted
/// regions, and require the match to start where an attribute name may start.
fn attribute(tag: &str, name: &str) -> String {
    let lower = tag.to_ascii_lowercase();
    let Some(at) = find_attribute(&lower, &name.to_ascii_lowercase()) else {
        return String::new();
    };
    let needle = format!("{name}=");
    let mut rest = &tag[at + needle.len()..];
    let quote = rest.chars().next();
    match quote {
        Some(q @ ('"' | '\'')) => {
            rest = &rest[q.len_utf8()..];
            match rest.find(q) {
                Some(end) => rest[..end].to_string(),
                None => String::new(),
            }
        }
        _ => {
            let end = rest.find([' ', '>']).unwrap_or(rest.len());
            rest[..end].to_string()
        }
    }
}

/// Where `name=` starts as an attribute name, not as text inside a value.
///
/// Walks the tag once, tracking the quote it is inside: an unquoted stretch may
/// hold a match only when it starts at a word boundary and the character before
/// the name is not the one that ends another attribute's name (`-` in
/// `data-href=`, `.` or `:` in a namespaced name). Comparison runs on the
/// lowercased tag, so the caller passes a lowercased name and gets a byte offset
/// that indexes either form.
fn find_attribute(lower_tag: &str, lower_name: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (at, ch) in lower_tag.char_indices() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None => {
                if !lower_tag[at..].starts_with(lower_name) {
                    continue;
                }
                let after = lower_tag[at + lower_name.len()..].starts_with('=');
                let before_is_name = !matches!(
                    lower_tag[..at].chars().next_back(),
                    Some('-' | '.' | ':' | '_')
                );
                if after && before_is_name {
                    return Some(at);
                }
            }
        }
    }
    None
}

/// Named and numeric entities. An unknown entity stays as written.
fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        if !rest.starts_with('&') {
            if let Some(ch) = rest.chars().next() {
                out.push(ch);
                i += ch.len_utf8();
            }
            continue;
        }
        let mut end = 1;
        let mut body = String::new();
        while end < rest.len() {
            let Some(ch) = rest[end..].chars().next() else {
                break;
            };
            if ch == ';' {
                break;
            }
            // No entity name is longer than "&#x10FFFF;"; stopping early keeps a
            // stray '&' in prose from scanning the rest of the caption.
            if end > 10 {
                body.clear();
                break;
            }
            body.push(ch);
            end += ch.len_utf8();
        }
        if !rest[end..].starts_with(';') || body.is_empty() {
            out.push('&');
            i += 1;
            continue;
        }
        let decoded = match body.as_str() {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            other => numeric_entity(other),
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                i += end + 1;
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

fn numeric_entity(body: &str) -> Option<char> {
    let digits = body.strip_prefix('#')?;
    let value = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    // `&#0;` (and `&#x0;`) is not a character the reader may hold: U+0000
    // truncates C strings, breaks a Slint text model, and reads as nothing at
    // all - so it stays literal text like any other entity this module does not
    // know. `char::from_u32(0)` is `Some`, which is why the filter is explicit.
    char::from_u32(value).filter(|ch| *ch != '\0')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(body: &str) -> Document {
        parse(body, &BTreeMap::new())
    }

    fn image_map(ids: &[(&str, &str, &str)]) -> BTreeMap<String, ImageInfo> {
        ids.iter()
            .map(|(id, small, large)| {
                (
                    (*id).to_string(),
                    ImageInfo {
                        small: (*small).to_string(),
                        large: (*large).to_string(),
                    },
                )
            })
            .collect()
    }

    fn kinds(document: &Document) -> Vec<BlockKind> {
        document.blocks.iter().map(|block| block.kind).collect()
    }

    #[test]
    fn crlf_cr_and_tabs_are_normalised() {
        let d = doc("一行目\r\n二行目\r三行目\tタブ");
        // One paragraph: only a blank line ends it, and a normalised line break
        // is not a blank line.
        assert_eq!(kinds(&d), vec![BlockKind::Paragraph]);
        assert_eq!(
            d.blocks[0].spans,
            vec![Span::Text("一行目\n二行目\n三行目  タブ".to_string())]
        );
        assert!(!d.plain.contains('\r'));
        assert!(!d.plain.contains('\t'));
        assert_eq!(d.plain, "一行目\n二行目\n三行目  タブ");
        assert_eq!(d.characters, d.plain.chars().count());
    }

    #[test]
    fn newpage_becomes_its_own_block() {
        let d = doc("前のページ\n[newpage]\n次のページ");
        assert_eq!(
            kinds(&d),
            vec![
                BlockKind::Paragraph,
                BlockKind::PageBreak,
                BlockKind::Paragraph
            ]
        );
        assert_eq!(render(&d.blocks[1], RubyMode::Inline), "");
        // The break itself is not text, so it adds no characters.
        assert_eq!(d.plain, "前のページ\n\n次のページ");
    }

    #[test]
    fn chapter_markers_become_headings() {
        let d = doc("[chapter:三・趙、未だ先王の功と罪の在ること]\n本文");
        assert_eq!(kinds(&d), vec![BlockKind::Heading, BlockKind::Paragraph]);
        assert_eq!(
            d.blocks[0].spans,
            vec![Span::Text("三・趙、未だ先王の功と罪の在ること".to_string())]
        );

        // An empty title still closes the previous paragraph and opens a heading.
        let empty = doc("本文\n[chapter:]\n次の本文");
        assert_eq!(
            kinds(&empty),
            vec![
                BlockKind::Paragraph,
                BlockKind::Heading,
                BlockKind::Paragraph
            ]
        );
        assert_eq!(render(&empty.blocks[1], RubyMode::Inline), "");

        // Only a whole line is a heading; mid-line it stays literal.
        let inline = doc("これは[chapter:章]です");
        assert_eq!(kinds(&inline), vec![BlockKind::Paragraph]);
        assert_eq!(inline.plain, "これは[chapter:章]です");
    }

    #[test]
    fn ruby_becomes_a_ruby_span() {
        let d = doc("龍台と呼ばれる約[[rb:七丈 > 16m]]もの高さ");
        assert_eq!(
            d.blocks[0].spans,
            vec![
                Span::Text("龍台と呼ばれる約".to_string()),
                Span::Ruby {
                    base: "七丈".to_string(),
                    reading: "16m".to_string(),
                },
                Span::Text("もの高さ".to_string()),
            ]
        );
        assert_eq!(
            render(&d.blocks[0], RubyMode::Inline),
            "龍台と呼ばれる約七丈（16m）もの高さ"
        );
        assert_eq!(
            render(&d.blocks[0], RubyMode::Hidden),
            "龍台と呼ばれる約七丈もの高さ"
        );
        // `plain` always carries the reading so search can match it.
        assert_eq!(d.plain, "龍台と呼ばれる約七丈（16m）もの高さ");

        // A base with no reading is plain text, not a stray pair of brackets.
        let bare = doc("[[rb:七丈]]");
        assert_eq!(bare.blocks[0].spans, vec![Span::Text("七丈".to_string())]);
    }

    #[test]
    fn pixivimage_markers_become_work_refs() {
        for body in ["[pixivimage:12345-2]", "[[pixivimage:12345-2]]"] {
            let d = doc(body);
            assert_eq!(
                d.blocks[0].spans,
                vec![Span::WorkRef {
                    id: "12345".to_string(),
                    label: "作品 12345".to_string(),
                }],
                "body: {body}"
            );
            assert_eq!(render(&d.blocks[0], RubyMode::Inline), "作品 12345");
        }

        // Surrounding text is preserved on both sides.
        let d = doc("前[[pixivimage:12345-1]]後");
        assert_eq!(
            d.blocks[0].spans,
            vec![
                Span::Text("前".to_string()),
                Span::WorkRef {
                    id: "12345".to_string(),
                    label: "作品 12345".to_string(),
                },
                Span::Text("後".to_string()),
            ]
        );
    }

    #[test]
    fn jumpuri_markers_become_links() {
        for body in [
            "[[jumpuri:作者ページ > https://www.pixiv.net/users/1]]",
            "[jumpuri:作者ページ > https://www.pixiv.net/users/1]",
        ] {
            let d = doc(body);
            assert_eq!(
                d.blocks[0].spans,
                vec![Span::Link {
                    label: "作者ページ".to_string(),
                    href: "https://www.pixiv.net/users/1".to_string(),
                }],
                "body: {body}"
            );
        }
        assert_eq!(
            doc("[[jumpuri:作者ページ > https://x.example/]]").plain,
            "作者ページ"
        );
    }

    #[test]
    fn jump_markers_become_jump_links() {
        let d = doc("[[jump:12345]]");
        assert_eq!(
            d.blocks[0].spans,
            vec![Span::Link {
                label: "跳转 12345".to_string(),
                href: "jump:12345".to_string(),
            }]
        );
        assert_eq!(d.plain, "跳转 12345");
    }

    #[test]
    fn uploaded_image_markers_become_image_blocks() {
        let map = image_map(&[(
            "26247899",
            "https://i.pximg.net/240mw.jpg",
            "https://i.pximg.net/1200x1200.jpg",
        )]);
        let d = parse("[uploadedimage:26247899]\n本文", &map);
        assert_eq!(kinds(&d), vec![BlockKind::Image, BlockKind::Paragraph]);
        assert_eq!(d.blocks[0].image_id, "26247899");
        assert!(!d.blocks[0].missing);
        assert_eq!(
            d.blocks[0].image.as_ref().map(|info| info.large.as_str()),
            Some("https://i.pximg.net/1200x1200.jpg")
        );
        assert_eq!(d.images.len(), 1);
        assert_eq!(d.images[0].small, "https://i.pximg.net/240mw.jpg");
    }

    #[test]
    fn a_missing_image_stays_a_block_with_a_readable_fallback() {
        let d = doc("[uploadedimage:26247899]");
        assert_eq!(kinds(&d), vec![BlockKind::Image]);
        assert!(d.blocks[0].missing);
        assert!(d.blocks[0].image.is_none());
        assert!(d.images.is_empty());
        // Nothing is lost: the position is still readable and searchable.
        assert_eq!(d.plain, "[图片 26247899]");
        assert_eq!(render(&d.blocks[0], RubyMode::Inline), "[图片 26247899]");

        // And the resolved version has the same length, so stored character
        // offsets do not shift when the image map arrives.
        let map = image_map(&[("26247899", "s", "l")]);
        let resolved = parse("[uploadedimage:26247899]", &map);
        assert_eq!(resolved.plain, d.plain);
        assert_eq!(resolved.characters, d.characters);
    }

    #[test]
    fn an_image_marker_splits_the_paragraph_around_it() {
        let d = doc("前の段落\n[uploadedimage:1]\n後の段落");
        assert_eq!(
            kinds(&d),
            vec![BlockKind::Paragraph, BlockKind::Image, BlockKind::Paragraph]
        );
        assert_eq!(render(&d.blocks[0], RubyMode::Inline), "前の段落");
        assert_eq!(render(&d.blocks[2], RubyMode::Inline), "後の段落");
    }

    #[test]
    fn images_are_listed_once_even_when_repeated() {
        let map = image_map(&[("1", "s1", "l1"), ("2", "s2", "l2")]);
        let d = parse(
            "[uploadedimage:1]\n[uploadedimage:2]\n[uploadedimage:1]",
            &map,
        );
        assert_eq!(d.blocks.len(), 3);
        assert_eq!(d.images.len(), 2, "a repeated id is fetched once");
        assert_eq!(d.images[0].large, "l1");
        assert_eq!(d.images[1].large, "l2");
    }

    #[test]
    fn rules_become_separators() {
        let d = doc("上\n***\n下");
        assert_eq!(
            kinds(&d),
            vec![
                BlockKind::Paragraph,
                BlockKind::Separator,
                BlockKind::Paragraph
            ]
        );
        assert_eq!(render(&d.blocks[1], RubyMode::Inline), "");

        for rule in ["***", "---", "*****", "-----"] {
            let d = doc(rule);
            assert_eq!(kinds(&d), vec![BlockKind::Separator], "rule: {rule}");
        }
        // Two dashes are prose, and mixed characters are not a rule.
        for prose in ["--", "**", "*-*", "-*-"] {
            let d = doc(prose);
            assert_eq!(kinds(&d), vec![BlockKind::Paragraph], "prose: {prose}");
        }
    }

    #[test]
    fn blank_lines_bound_paragraphs() {
        let d = doc("一\n\n二\n\n\n三");
        assert_eq!(kinds(&d), vec![BlockKind::Paragraph; 3]);
        assert_eq!(d.plain, "一\n二\n三");

        // A line of spaces is blank too.
        let spaced = doc("一\n   \n二");
        assert_eq!(
            kinds(&spaced),
            vec![BlockKind::Paragraph, BlockKind::Paragraph]
        );
    }

    #[test]
    fn an_unknown_marker_stays_literal() {
        let d = doc("[kana:よみがな]は残る");
        assert_eq!(kinds(&d), vec![BlockKind::Paragraph]);
        assert_eq!(
            d.blocks[0].spans,
            vec![Span::Text("[kana:よみがな]は残る".to_string())]
        );
        assert_eq!(d.plain, "[kana:よみがな]は残る");
        // No control character ever leaves the parser.
        assert!(!d.plain.chars().any(|ch| ch.is_control()));

        // A partially-known prefix is not a marker either.
        let partial = doc("[rb:七丈]");
        assert_eq!(partial.plain, "[rb:七丈]");
    }

    #[test]
    fn an_unclosed_ruby_marker_stays_literal() {
        let d = doc("[[rb:七丈 > 16m");
        assert_eq!(kinds(&d), vec![BlockKind::Paragraph]);
        assert_eq!(d.plain, "[[rb:七丈 > 16m");
        assert_eq!(d.characters, "[[rb:七丈 > 16m".chars().count());
    }

    #[test]
    fn an_unclosed_marker_never_swallows_what_follows() {
        // The unclosed jumpuri stops at the '[' of the next marker instead of
        // searching forward for that marker's closing brackets.
        let d = doc("[[jumpuri:壊れた > https://a.example/ [[rb:七丈 > 16m]]");
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(
            d.blocks[0].spans,
            vec![
                // The two brackets that started the marker are still there: the
                // literal text reconstructs the input exactly.
                Span::Text("[[jumpuri:壊れた > https://a.example/ ".to_string()),
                Span::Ruby {
                    base: "七丈".to_string(),
                    reading: "16m".to_string(),
                },
            ]
        );
        assert!(d.plain.starts_with("[[jumpuri:壊れた"), "{}", d.plain);
        assert!(d.plain.ends_with("七丈（16m）"), "{}", d.plain);
    }

    #[test]
    fn a_body_of_only_blank_lines_is_empty() {
        for body in ["", "\n", "\n\n\n", "   \n\t\n \r\n"] {
            let d = doc(body);
            assert!(d.blocks.is_empty(), "body: {body:?}");
            assert!(d.plain.is_empty(), "body: {body:?}");
            assert_eq!(d.characters, 0, "body: {body:?}");
            assert!(d.images.is_empty());
        }
    }

    #[test]
    fn a_two_hundred_kilobyte_body_parses() {
        let mut body = String::new();
        while body.len() < 200_000 {
            body.push_str("[chapter:第三章]\n");
            body.push_str("これは[[rb:試験 > しけん]]の本文です。\n\n");
            body.push_str("[uploadedimage:26247899]\n");
            body.push_str("***\n");
            body.push_str("[[jumpuri:リンク > https://www.pixiv.net/novel/show.php?id=1]]\n\n");
        }
        assert!(body.len() >= 200_000, "fixture must exceed 200 KB");

        let d = parse(&body, &image_map(&[("26247899", "s", "l")]));
        assert_eq!(d.characters, d.plain.chars().count());
        assert_eq!(d.characters, d.plain.chars().count());
        assert!(d.characters > 10_000, "characters: {}", d.characters);
        assert!(d.blocks.len() > 1_000, "blocks: {}", d.blocks.len());
        assert_eq!(d.images.len(), 1);
        assert!(d.plain.contains("これは試験（しけん）の本文です。"));
    }

    #[test]
    fn plain_matches_the_reader_text_and_counts_scalars() {
        let body = "[chapter:章]\nあ[[rb:漢字 > かんじ]]い\n[newpage]\n***\n[uploadedimage:9]";
        let d = doc(body);
        assert_eq!(d.plain, "章\nあ漢字（かんじ）い\n\n\n[图片 9]");
        // Byte count and character count differ for Japanese; the cursor counts
        // scalars.
        assert_eq!(d.characters, d.plain.chars().count());
        assert!(d.characters < d.plain.len());
    }

    #[test]
    fn clean_caption_decodes_the_api_html() {
        assert_eq!(clean_caption("一行目<br />二行目"), "一行目\n二行目");
        assert_eq!(clean_caption("一行目<br/>二行目"), "一行目\n二行目");
        assert_eq!(clean_caption("一行目<BR>二行目"), "一行目\n二行目");
        assert_eq!(
            clean_caption("<a href=\"https://www.pixiv.net/users/1\">作者</a>の小説"),
            "作者 (https://www.pixiv.net/users/1)の小説"
        );
        assert_eq!(
            clean_caption("<a href='https://x.example/'>link</a>"),
            "link (https://x.example/)"
        );
        assert_eq!(clean_caption("<p>段落</p>"), "段落");
        assert_eq!(clean_caption("&amp;&lt;&gt;&quot;&#39;&nbsp;"), "&<>\"' ");
        assert_eq!(clean_caption("&#x3042;"), "あ");
        assert_eq!(clean_caption("&unknown;"), "&unknown;");
        // A stray '<' is text; a '&' that starts no entity is text.
        assert_eq!(clean_caption("a < b & c"), "a < b & c");
        // Escaped markup decodes to visible text instead of being stripped.
        assert_eq!(clean_caption("&lt;b&gt;"), "<b>");
        assert_eq!(clean_caption(""), "");
    }

    /// `&#0;` must stay literal text: `char::from_u32(0)` is `Some`, so the old
    /// entity decoder accepted it and put a NUL into a caption, where a Slint text
    /// model truncates and the reader loses every character after it.
    #[test]
    fn a_nul_entity_stays_literal_text() {
        assert_eq!(clean_caption("あ&#0;い"), "あ&#0;い");
        assert_eq!(clean_caption("&#x0;"), "&#x0;");
        assert_eq!(clean_caption("&#00;"), "&#00;");
        // The entities around it still decode, and no NUL reaches the output.
        let decoded = clean_caption("&#65;&#0;&#66;");
        assert_eq!(decoded, "A&#0;B");
        assert!(!decoded.contains('\u{0}'));
    }

    /// An `href=` that lives inside another attribute's value is not this
    /// anchor's href.
    ///
    /// The old search was a substring find over the whole tag, so
    /// `<a title="href=x" href="y">` reported `x` - a URL the anchor never
    /// carried, printed as if it did.
    #[test]
    fn an_attribute_is_not_found_inside_another_attributes_value() {
        assert_eq!(
            clean_caption(r#"<a title="href=x" href="y">label</a>"#),
            "label (y)"
        );
        assert_eq!(clean_caption(r#"<a title='href=x'>label</a>"#), "label");
        assert_eq!(
            clean_caption(r#"<a data-href="z" href="y">label</a>"#),
            "label (y)"
        );
        // A value may hold the other quote character, and unquoted values still
        // resolve: the rule is about attribute boundaries, not about quoting.
        assert_eq!(clean_caption("<a href=\"a'b\">label</a>"), "label (a'b)");
        assert_eq!(
            clean_caption("<a href=https://x.example/>l</a>"),
            "l (https://x.example/)"
        );
    }

    #[test]
    fn render_is_the_only_place_ruby_changes() {
        let d = doc("[[rb:七丈 > 16m]]");
        assert_eq!(render(&d.blocks[0], RubyMode::Inline), "七丈（16m）");
        assert_eq!(render(&d.blocks[0], RubyMode::Hidden), "七丈");

        let plain_block = doc("本文").blocks.remove(0);
        assert_eq!(render(&plain_block, RubyMode::Hidden), "本文");
        assert_eq!(render(&plain_block, RubyMode::Inline), "本文");
    }

    #[test]
    fn ruby_mode_serialises_lowercase_and_defaults_to_inline() {
        assert_eq!(RubyMode::default(), RubyMode::Inline);
        assert_eq!(
            serde_json::to_string(&RubyMode::Inline).unwrap(),
            "\"inline\""
        );
        assert_eq!(
            serde_json::to_string(&RubyMode::Hidden).unwrap(),
            "\"hidden\""
        );
        assert_eq!(
            serde_json::from_str::<RubyMode>("\"hidden\"").unwrap(),
            RubyMode::Hidden
        );
    }
}
