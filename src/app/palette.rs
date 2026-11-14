//! Command palette state (round nine, F3).
//!
//! Declared in phase 0 so `App` owns the field before the feed track fills in
//! the parser and the row ranking; the `allow` goes away with that work.

/// What the palette is showing right now.
#[allow(dead_code)]
#[derive(Default, Debug)]
pub(crate) struct PaletteState {
    pub open: bool,
    pub query: String,
    /// Index into `rows` that Enter would run, `-1` for none.
    pub active: i32,
    pub rows: Vec<PaletteRow>,
}

/// One row: what it says, and what choosing it means.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct PaletteRow {
    pub label: String,
    /// The right-hand explanatory line ("页面", "作品 1234", "最近浏览").
    pub detail: String,
    /// The key chord that does the same thing from the shell, when one exists.
    pub shortcut: String,
    /// 0 page, 1 action, 2 recent view, 3 direct target. The kind decides what
    /// choosing the row does, so the label never has to be parsed back.
    pub kind: u8,
}
