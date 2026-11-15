//! Pixiv novel body markup and reading preferences.
//!
//! # Scope
//!
//! This crate is pure: it turns a novel body into a document model, and it turns
//! the reader's appearance settings into a value that is safe to persist. No
//! network, no disk, no SQLite, no UI types. The HTTP layer hands over
//! `body.content` as a `&str`; the UI consumes [`parse::Document`] verbatim.
//!
//! # Contract
//!
//! * [`parse::parse`] is total. It has no error type and no `Result`: every input
//!   string, including an empty one, a body of only blank lines or a body full
//!   of half-written markers, produces a [`parse::Document`]. A marker the
//!   grammar does not recognise stays *literal text*, so a future Pixiv marker
//!   degrades to visible characters instead of vanishing.
//! * Text is never rewritten. The only substitutions are the ones that lose
//!   nothing the model can carry: `\r\n` and `\r` become `\n`, a tab becomes
//!   two spaces, and an image marker becomes a `[图片 id]` label so its position
//!   stays readable in the copyable text.
//! * [`parse::render`] is the single place that turns a block into the string a
//!   row shows, and the ruby preference is the only thing that changes its
//!   output.
//! * Every preference read from storage passes through
//!   [`prefs::ReaderPrefs::clamped`]; a corrupt or hand-edited value can never
//!   make the reader unusable, which is the only reason that method exists.
//!
//! # Why the document model is flat
//!
//! Reading progress is persisted as `(novel_id, block_index, char_offset)` and is
//! compared across sessions, so an index has to mean the same thing forever. A
//! nested model (sections containing paragraphs containing spans) would make two
//! numbers per position ambiguous the moment the nesting changes — and Pixiv's
//! markup offers no reliable nesting to begin with: `[chapter:...]` is a marker
//! on a line, not a closing-tag construct, so any tree would be invented rather
//! than parsed. A flat `Vec<Block>` also maps 1:1 onto a Slint `ListView` row
//! model, which is what the reader renders; a tree would need flattening every
//! frame and would defeat row recycling.
//!
//! # Layout
//!
//! * [`parse`] — the body grammar, the block model and the ruby rendering rule.
//! * [`prefs`] — the reader's appearance settings and their clamps.

pub mod parse;
pub mod prefs;

pub use parse::{
    Block, BlockKind, Document, ImageInfo, RubyMode, Span, clean_caption, parse, render,
};
pub use prefs::{Align, ParagraphRule, ReaderPrefs, ReaderTheme};
