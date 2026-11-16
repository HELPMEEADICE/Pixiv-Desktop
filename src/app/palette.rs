use super::*;

/// What the palette is showing right now.
#[derive(Default, Debug)]
pub(crate) struct PaletteState {
    pub open: bool,
    pub query: String,
    /// Index into `rows` that Enter would run, `-1` for none.
    pub active: i32,
    pub rows: Vec<PaletteRow>,
    /// The last few works this machine recorded, read once when the palette opens.
    ///
    /// Cached rather than re-read per keystroke: `Store::view_history` takes the
    /// store mutex and prepares a statement, and the palette is typed into one
    /// character at a time - the same reason `recent_words` is cached (plan §2.3 R16).
    pub recent: Vec<RecentView>,
}

/// One work from the local history, as the palette names it.
#[derive(Clone, Debug)]
pub(crate) struct RecentView {
    pub id: u64,
    pub title: String,
}

/// One row: what it says, and what choosing it means.
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
    /// Extra words a query may match that the label does not show ("home",
    /// "cache"). Never drawn.
    pub keywords: String,
    /// What choosing this row runs.
    pub plan: RowPlan,
}

/// The verb behind a row.
#[derive(Clone, Debug)]
pub(crate) enum RowPlan {
    Page(Destination),
    Action(PaletteAction),
    Recent(u64),
    Target(Target),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaletteAction {
    Refresh,
    ToggleTheme,
    OpenSettings,
    ClearCache,
    SignIn,
}

pub(crate) const KIND_PAGE: u8 = 0;
pub(crate) const KIND_ACTION: u8 = 1;
pub(crate) const KIND_RECENT: u8 = 2;
pub(crate) const KIND_TARGET: u8 = 3;

/// The palette never draws more than this: eight rows is what Windows Terminal
/// shows above its key-hint footer, and a longer list would want a scroll bar the
/// panel's own height was never given.
pub(crate) const PALETTE_ROWS: usize = 8;

// ===========================================================================
// Direct jump: what the typed text names
// ===========================================================================

/// What a palette query points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Illust(u64),
    User(u64),
    Novel(u64),
    /// Not a link and not an id: search for it.
    Search(String),
    /// Nothing to do at all (an empty query).
    None,
}

const ARTWORK_KEYS: &[&str] = &["artworks", "artwork", "illust", "illusts", "i"];
const USER_KEYS: &[&str] = &["users", "user", "u"];
const NOVEL_KEYS: &[&str] = &["novel", "novels"];

/// A positive decimal id, and nothing else.
fn positive(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>().ok().filter(|number| *number > 0)
}

/// `id=1234`, the shape a share sheet hands over.
fn after_equals(text: &str) -> Option<u64> {
    let (key, value) = text.split_once('=')?;
    if !key.trim().eq_ignore_ascii_case("id") {
        return None;
    }
    positive(value.trim())
}

/// The `id` of a query string, when one of its pairs is `id=<number>`.
///
/// The pairs are split on both separators: a link pasted out of a share sheet can
/// carry a fragment after the id, and an id followed by `&foo=1` is still the id.
fn query_id(query: &str) -> Option<u64> {
    query
        .split(['&', '#'])
        .find_map(|pair| after_equals(pair.trim()))
}

/// The work or artist a URL path names, if it names one.
fn path_target(lower: &str) -> Option<Target> {
    let segments: Vec<&str> = lower.split('/').filter(|part| !part.is_empty()).collect();
    for (index, segment) in segments.iter().enumerate() {
        let Some(number) = segments.get(index + 1).and_then(|next| positive(next)) else {
            continue;
        };
        if ARTWORK_KEYS.contains(segment) {
            return Some(Target::Illust(number));
        }
        if USER_KEYS.contains(segment) {
            return Some(Target::User(number));
        }
        if NOVEL_KEYS.contains(segment) {
            return Some(Target::Novel(number));
        }
    }
    None
}

/// What the user typed into the palette.
///
/// One parser for the four things a person actually pastes: a full link, a path
/// without a host, a bare id, and `id=`. Everything else is a search - which is
/// the honest answer, because "find me a work called that" is what the user meant
/// by typing it.
pub(crate) fn parse_target(input: &str) -> Target {
    let text = input.trim();
    if text.is_empty() {
        return Target::None;
    }
    // A leading '@' is a person's handle, not a work.
    if let Some(handle) = text.strip_prefix('@') {
        let word = handle.trim();
        return Target::Search(if word.is_empty() {
            text.to_string()
        } else {
            word.to_string()
        });
    }
    // The fragment and the query string describe the page the link came from,
    // never the id: they have to fall away before the path is read.
    let head = text.split(['?', '#']).next().unwrap_or(text).trim();
    // A novel page names its id in the query string, not in the path:
    // `novel/show.php?id=1234`. The path is read first because `after_equals` on
    // the whole text would answer "a work" for a novel link.
    let lower = head.to_ascii_lowercase();
    if lower.ends_with("novel/show.php") {
        if let Some(number) = query_id(text.split_once('?').map_or("", |(_, query)| query)) {
            return Target::Novel(number);
        }
    }
    if let Some(number) = after_equals(head) {
        return Target::Illust(number);
    }
    if let Some(number) = positive(head) {
        return Target::Illust(number);
    }
    path_target(&lower).unwrap_or_else(|| Target::Search(text.to_string()))
}

// ===========================================================================
// Rows and ranking
// ===========================================================================

impl PaletteRow {
    fn new(
        label: &str,
        detail: &str,
        shortcut: &str,
        kind: u8,
        keywords: &str,
        plan: RowPlan,
    ) -> Self {
        Self {
            label: label.to_string(),
            detail: detail.to_string(),
            shortcut: shortcut.to_string(),
            kind,
            keywords: keywords.to_string(),
            plan,
        }
    }
}

/// The eight pages, in the rail's own order.
fn page_rows() -> Vec<PaletteRow> {
    let pages = [
        ("首页", "home", "Ctrl+1", Destination::Home),
        (
            "发现",
            "discover discovery",
            "Ctrl+2",
            Destination::Discovery,
        ),
        ("排行榜", "ranking", "Ctrl+3", Destination::Ranking),
        ("搜索", "search", "Ctrl+K", Destination::Search),
        ("收藏", "bookmarks", "Ctrl+4", Destination::Bookmarks),
        ("浏览历史", "history", "Ctrl+5", Destination::History),
        ("下载", "downloads queue", "Ctrl+6", Destination::Downloads),
        (
            "设置",
            "settings preferences",
            "Ctrl+,",
            Destination::Settings,
        ),
    ];
    pages
        .into_iter()
        .map(|(label, keywords, shortcut, page)| {
            PaletteRow::new(
                label,
                "页面",
                shortcut,
                KIND_PAGE,
                keywords,
                RowPlan::Page(page),
            )
        })
        .collect()
}

/// The five things the palette can do that are not a navigation.
fn action_rows() -> Vec<PaletteRow> {
    [
        (
            "刷新当前页",
            "refresh reload",
            "Ctrl+R",
            PaletteAction::Refresh,
        ),
        (
            "切换主题",
            "theme dark light",
            "",
            PaletteAction::ToggleTheme,
        ),
        (
            "打开设置",
            "settings preferences",
            "Ctrl+,",
            PaletteAction::OpenSettings,
        ),
        (
            "清理缓存",
            "cache clear disk",
            "",
            PaletteAction::ClearCache,
        ),
        (
            "重新登录",
            "login session cookie",
            "",
            PaletteAction::SignIn,
        ),
    ]
    .into_iter()
    .map(|(label, keywords, shortcut, action)| {
        PaletteRow::new(
            label,
            "动作",
            shortcut,
            KIND_ACTION,
            keywords,
            RowPlan::Action(action),
        )
    })
    .collect()
}

fn recent_rows(recent: &[RecentView]) -> Vec<PaletteRow> {
    recent
        .iter()
        .map(|view| {
            PaletteRow::new(
                &view.title,
                &format!("最近浏览 · {}", view.id),
                "",
                KIND_RECENT,
                "",
                RowPlan::Recent(view.id),
            )
        })
        .collect()
}

/// How well one row answers the query: 0 prefix, 1 subsequence, 2 fuzzy.
fn match_rank(query: &str, row: &PaletteRow) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let label = row.label.to_lowercase();
    let keywords = row.keywords.to_lowercase();
    if label.starts_with(query)
        || keywords
            .split_whitespace()
            .any(|word| word.starts_with(query))
    {
        return Some(0);
    }
    let haystack = format!("{label} {} {keywords}", row.detail.to_lowercase());
    if is_subsequence(query, &haystack) {
        return Some(1);
    }
    // Fuzzy last: every character of the query appears somewhere, in any order.
    if query.chars().all(|wanted| haystack.contains(wanted)) {
        return Some(2);
    }
    None
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut rest = haystack.chars();
    needle.chars().all(|wanted| rest.any(|have| have == wanted))
}

/// Actions first, then pages, then what was already seen.
fn kind_order(kind: u8) -> u8 {
    match kind {
        KIND_ACTION => 0,
        KIND_PAGE => 1,
        KIND_RECENT => 2,
        _ => 3,
    }
}

/// Rank `candidates` against `query`, leading with the direct target.
///
/// Prefix beats subsequence beats fuzzy; within one quality the kind decides (an
/// action the user named beats a page whose title merely contains the letters),
/// and the builders' own order breaks the tie, so one query always produces the
/// same list.
pub(crate) fn rank_rows(
    candidates: Vec<PaletteRow>,
    direct: Option<PaletteRow>,
    query: &str,
) -> Vec<PaletteRow> {
    let wanted = query.trim().to_lowercase();
    let mut scored: Vec<(u8, u8, usize, PaletteRow)> = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(index, row)| {
            match_rank(&wanted, &row).map(|quality| (quality, kind_order(row.kind), index, row))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut rows: Vec<PaletteRow> = Vec::with_capacity(PALETTE_ROWS);
    if let Some(direct) = direct {
        rows.push(direct);
    }
    rows.extend(scored.into_iter().map(|(_, _, _, row)| row));
    rows.truncate(PALETTE_ROWS);
    rows
}
impl App {
    // ------------------------------------------------------------- palette

    /// Every row the query can produce, before ranking.
    fn palette_candidates(&self, query: &str) -> (Vec<PaletteRow>, Option<PaletteRow>) {
        let mut candidates = page_rows();
        candidates.extend(action_rows());
        candidates.extend(recent_rows(&self.palette.recent));
        let direct = match parse_target(query) {
            Target::Illust(id) => Some(PaletteRow::new(
                &format!("作品 {id}"),
                "直接打开",
                "",
                KIND_TARGET,
                "artworks illust",
                RowPlan::Target(Target::Illust(id)),
            )),
            Target::User(id) => Some(PaletteRow::new(
                &format!("画师 {id}"),
                "直接打开",
                "",
                KIND_TARGET,
                "users artist",
                RowPlan::Target(Target::User(id)),
            )),
            Target::Novel(id) => Some(PaletteRow::new(
                &format!("小说 {id}"),
                "直接打开",
                "",
                KIND_TARGET,
                "novels novel text",
                RowPlan::Target(Target::Novel(id)),
            )),
            // A search is only offered when nothing else answers the query: the row
            // exists so Enter never does nothing, not to outrank a page the user named.
            Target::Search(word) => {
                let answered = candidates
                    .iter()
                    .any(|row| match_rank(&word.to_lowercase(), row).is_some());
                (!answered).then(|| {
                    PaletteRow::new(
                        &format!("搜索「{word}」"),
                        "在 Pixiv 搜索",
                        "",
                        KIND_TARGET,
                        "search",
                        RowPlan::Target(Target::Search(word)),
                    )
                })
            }
            Target::None => None,
        };
        (candidates, direct)
    }

    pub(crate) fn palette_rows_for(&self, query: &str) -> Vec<PaletteRow> {
        let (candidates, direct) = self.palette_candidates(query);
        rank_rows(candidates, direct, query)
    }

    fn publish_palette(&self) {
        let rows: Vec<crate::PaletteRow> = self
            .palette
            .rows
            .iter()
            .map(|row| crate::PaletteRow {
                label: row.label.clone().into(),
                detail: row.detail.clone().into(),
                shortcut: row.shortcut.clone().into(),
                kind: i32::from(row.kind),
            })
            .collect();
        let ui = self.ui();
        ui.set_palette_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
        ui.set_palette_open(self.palette.open);
        ui.set_palette_active(self.palette.active);
    }

    /// Open the panel: read the recent views once, then rank the query the field
    /// is already showing.
    ///
    /// The query comes from the field rather than from a cleared string, because
    /// Ctrl+P only flips a property: a panel that reopened empty while its own
    /// input still held the last query would be two states in one window.
    pub(crate) fn palette_open(&mut self) {
        self.palette.recent = self.recent_views();
        self.palette.open = true;
        let query = self.ui().get_palette_text().to_string();
        self.palette.query = query.clone();
        self.palette.rows = self.palette_rows_for(&query);
        self.palette.active = if self.palette.rows.is_empty() { -1 } else { 0 };
        self.publish_palette();
    }

    pub(crate) fn palette_close(&mut self) {
        self.palette.open = false;
        self.palette.query.clear();
        self.palette.rows.clear();
        self.palette.active = -1;
        self.publish_palette();
        // The field is in-out, so closing empties what the panel showed instead of
        // leaving a query behind for the next open to inherit.
        self.ui().set_palette_text("".into());
    }

    pub(crate) fn palette_query(&mut self, text: &str) {
        self.palette.query = text.to_string();
        self.palette.rows = self.palette_rows_for(text);
        self.palette.active = if self.palette.rows.is_empty() { -1 } else { 0 };
        self.publish_palette();
    }

    /// Move the cursor by `delta`, clamped to the rows that exist.
    ///
    /// Only the cursor is published, never the rows: the Up/Down keys belong to
    /// the field in `ui/main.slint`, and a cursor move that rebuilt the row
    /// model would recreate every row under the one it was moving to.
    ///
    /// Also the clamp the panel relies on after a query shortens the list: the
    /// field's own arithmetic caps at `rows.length - 1`, and this is what keeps
    /// Rust's copy of the cursor inside the same bound.
    pub(crate) fn palette_move(&mut self, delta: i32) {
        let last = self.palette.rows.len() as i32 - 1;
        let next = self.palette.active + delta;
        self.palette.active = next.clamp(-1, last.max(-1));
        self.ui().set_palette_active(self.palette.active);
    }

    /// The five recent views, as the palette needs them.
    fn recent_views(&self) -> Vec<RecentView> {
        // A forced-demo capture must not carry this machine's real history into a
        // screenshot: the same rule the history page already follows.
        if forced_demo() {
            return Vec::new();
        }
        self.store
            .view_history(5)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| {
                let id = entry.illust_id.parse::<u64>().ok()?;
                (id > 0).then_some(RecentView {
                    id,
                    title: if entry.title.trim().is_empty() {
                        format!("作品 {id}")
                    } else {
                        entry.title
                    },
                })
            })
            .collect()
    }
}

impl App {
    /// Run the row the user chose. The panel closes first: a command that
    /// navigates must not leave itself drawn over the page it opened.
    pub(crate) fn palette_choose(shared: &Shared, index: i32) {
        let plan = {
            let mut app = shared.borrow_mut();
            let plan = usize::try_from(index)
                .ok()
                .and_then(|position| app.palette.rows.get(position))
                .map(|row| row.plan.clone());
            app.palette_close();
            plan
        };
        let Some(plan) = plan else {
            return;
        };
        match plan {
            RowPlan::Page(page) => App::navigate(shared, page),
            RowPlan::Action(action) => App::palette_action(shared, action),
            RowPlan::Recent(id) => App::open_detail(shared, &id.to_string()),
            RowPlan::Target(Target::Illust(id)) => App::open_detail(shared, &id.to_string()),
            RowPlan::Target(Target::User(id)) => App::open_profile(shared, &id.to_string()),
            RowPlan::Target(Target::Novel(id)) => App::open_novel_from(shared, &id.to_string()),
            RowPlan::Target(Target::Search(word)) => App::submit_search(shared, &word),
            RowPlan::Target(Target::None) => {}
        }
    }

    fn palette_action(shared: &Shared, action: PaletteAction) {
        match action {
            // The same path Ctrl+R takes: reload whatever is in front of me.
            PaletteAction::Refresh => App::load_page(shared, false),
            PaletteAction::ToggleTheme => {
                let app = shared.borrow();
                let next = if app.ui().get_dark() {
                    ThemeMode::Light
                } else {
                    ThemeMode::Dark
                };
                set_theme_mode(&app, next);
            }
            PaletteAction::OpenSettings => App::navigate(shared, Destination::Settings),
            PaletteAction::ClearCache => App::sweep_cache(shared),
            PaletteAction::SignIn => {
                App::navigate(shared, Destination::Settings);
                let app = shared.borrow();
                app.notify(0, "在设置页粘贴新的 PHPSESSID 即可重新登录");
            }
        }
    }

    /// The disk sweep the settings page runs, reachable from the palette.
    ///
    /// Both entry points funnel through `ImageCache::clear_disk` and
    /// `prune_memory`; what is repeated here is the choreography around them (the
    /// busy flag, a notice before and after), and the settings copy lives in
    /// `callbacks/settings.rs`, which this track may not edit.
    fn sweep_cache(shared: &Shared) {
        let (cache, rt) = {
            let app = shared.borrow();
            app.ui().set_busy(true);
            app.notify(0, "正在清理磁盘缓存…");
            (app.cache.clone(), app.rt.clone())
        };
        rt.spawn(async move {
            let sweep = cache.clone();
            let reclaimed = tokio::task::spawn_blocking(move || sweep.clear_disk().unwrap_or(0))
                .await
                .unwrap_or(0);
            cache.prune_memory();
            on_ui(move |shared| {
                let Ok(app) = shared.try_borrow() else {
                    return;
                };
                app.ui().set_busy(false);
                app.refresh_chrome();
                app.notify(1, format!("已清理磁盘缓存 {}", human_bytes(reclaimed)));
            });
        });
    }
}

impl App {
    /// PIXIV_DESKTOP_FIXTURE=palette: the panel open, with four literal rows.
    ///
    /// The capture has to show the 36 px rows, the layer surface, the key-hint
    /// footer and the active-row wash; the demo data source has no pages to
    /// produce rows from a query, so the four are written out here. Each carries
    /// a real plan, so the rows a screenshot shows are rows that work.
    pub(crate) fn palette_fixture(&mut self) {
        self.palette.recent.clear();
        self.palette.query.clear();
        self.palette.rows = vec![
            PaletteRow::new(
                "首页",
                "页面",
                "Ctrl+1",
                KIND_PAGE,
                "home",
                RowPlan::Page(Destination::Home),
            ),
            PaletteRow::new(
                "下载",
                "页面",
                "Ctrl+6",
                KIND_PAGE,
                "downloads",
                RowPlan::Page(Destination::Downloads),
            ),
            PaletteRow::new(
                "刷新当前页",
                "动作",
                "Ctrl+R",
                KIND_ACTION,
                "refresh",
                RowPlan::Action(PaletteAction::Refresh),
            ),
            PaletteRow::new(
                "切换主题",
                "动作",
                "",
                KIND_ACTION,
                "theme",
                RowPlan::Action(PaletteAction::ToggleTheme),
            ),
        ];
        self.palette.active = 0;
        self.palette.open = true;
        self.ui().set_palette_text("".into());
        self.publish_palette();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fifteen shapes a person actually pastes, plus the two that must stay
    /// searches. Each row of the table is a bug that would ship: a trailing slash
    /// that broke the path, a query string that turned the id into text, and a
    /// host without a scheme that read as a search for the whole URL.
    #[test]
    fn the_parser_table() {
        let table: [(&str, Target); 22] = [
            ("https://www.pixiv.net/artworks/1234", Target::Illust(1234)),
            // A link copied with its trailing slash.
            ("https://www.pixiv.net/artworks/1234/", Target::Illust(1234)),
            // The language segment the site inserts.
            ("/en/artworks/1234", Target::Illust(1234)),
            // No scheme at all.
            ("pixiv.net/artworks/1234", Target::Illust(1234)),
            // Case is not part of a path.
            ("PIXIV.NET/ARTWORKS/1234", Target::Illust(1234)),
            // A doubled slash, and the empty segment between them.
            (
                "https://www.pixiv.net//artworks//1234",
                Target::Illust(1234),
            ),
            // The share-sheet form.
            ("id=1234", Target::Illust(1234)),
            // A bare id, which the search box used to swallow.
            ("1234", Target::Illust(1234)),
            // A query string and a fragment name the page they came from, not the id.
            (
                "https://www.pixiv.net/artworks/1234?foo=bar#top",
                Target::Illust(1234),
            ),
            ("/users/5678", Target::User(5678)),
            (
                "https://www.pixiv.net/novel/show.php?id=1234",
                Target::Novel(1234),
            ),
            ("novel/show.php?id=1234&lang=zh", Target::Novel(1234)),
            (
                "https://www.pixiv.net/novel/show.php?id=1234#top",
                Target::Novel(1234),
            ),
            ("https://www.pixiv.net/novel/1234", Target::Novel(1234)),
            ("/novel/1234", Target::Novel(1234)),
            ("novels/1234", Target::Novel(1234)),
            ("https://www.pixiv.net/novels/1234/", Target::Novel(1234)),
            ("users/5678", Target::User(5678)),
            ("https://www.pixiv.net/en/users/5678/", Target::User(5678)),
            // A handle is not an id: search for the person.
            ("@someone", Target::Search("someone".into())),
            // Anything else is a search for what was typed, not for its lower form.
            ("Landscape", Target::Search("Landscape".into())),
            ("   ", Target::None),
        ];
        for (input, expected) in table {
            assert_eq!(parse_target(input), expected, "input {input:?}");
        }
        // A refused or truncated id is not a work.
        assert_eq!(parse_target("id=0"), Target::Search("id=0".into()));
        assert_eq!(parse_target("1234abc"), Target::Search("1234abc".into()));
        assert_eq!(parse_target("@"), Target::Search("@".into()));
    }

    fn row(label: &str, kind: u8) -> PaletteRow {
        PaletteRow::new(
            label,
            "页面",
            "",
            kind,
            "",
            RowPlan::Page(Destination::Home),
        )
    }

    /// Prefix beats subsequence beats fuzzy, and equal qualities keep their order.
    #[test]
    fn ranking_is_prefix_then_subsequence_then_fuzzy() {
        let candidates = vec![
            // fuzzy: every letter is there, in no order
            row("cba", KIND_PAGE),
            // subsequence: a, b, c appear in order
            row("xabcx", KIND_PAGE),
            // prefix
            row("abcde", KIND_PAGE),
        ];
        let ranked = rank_rows(candidates, None, "abc");
        let labels: Vec<&str> = ranked.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, vec!["abcde", "xabcx", "cba"]);
    }

    /// An empty query is a menu, not a search: the five actions come first so the
    /// panel's first frame is something to do, and the pages follow in rail order.
    #[test]
    fn an_empty_query_puts_the_actions_first() {
        let mut candidates = page_rows();
        candidates.extend(action_rows());
        candidates.extend(recent_rows(&[RecentView {
            id: 9,
            title: "昨天的作品".into(),
        }]));
        let ranked = rank_rows(candidates, None, "");
        assert_eq!(ranked.len(), PALETTE_ROWS);
        assert!(
            ranked.iter().take(5).all(|row| row.kind == KIND_ACTION),
            "the five actions lead: {:?}",
            ranked
                .iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(ranked[5].label, "首页");
    }

    /// A link leads whatever else the query could match, and a typed word that
    /// nothing answers becomes a search row instead of an empty panel.
    #[test]
    fn a_pasted_link_leads_and_a_dead_end_offers_a_search() {
        let mut candidates = page_rows();
        candidates.extend(action_rows());
        let direct = Some(PaletteRow::new(
            "作品 1234",
            "直接打开",
            "",
            KIND_TARGET,
            "",
            RowPlan::Target(Target::Illust(1234)),
        ));
        let ranked = rank_rows(
            candidates.clone(),
            direct,
            "https://www.pixiv.net/artworks/1234",
        );
        assert_eq!(ranked[0].kind, KIND_TARGET);
        assert_eq!(ranked[0].label, "作品 1234");

        // Nothing in the built-in list answers this one.
        assert!(rank_rows(candidates.clone(), None, "非常具体的搜索词").is_empty());

        // A page the user named still wins over the search fallback.
        let settings = rank_rows(candidates, None, "设置");
        assert_eq!(settings[0].label, "设置");
    }

    /// The kind numbers are a contract with `ui/main.slint`'s `PaletteRow`: a
    /// row that says 1 is drawn as an action, and one that says 2 says what it was.
    #[test]
    fn kinds_and_details_match_the_ui_contract() {
        assert_eq!(KIND_PAGE, 0);
        assert_eq!(KIND_ACTION, 1);
        assert_eq!(KIND_RECENT, 2);
        assert_eq!(KIND_TARGET, 3);
        assert_eq!(page_rows().len(), 8);
        assert_eq!(action_rows().len(), 5);
        assert!(page_rows().iter().all(|row| row.detail == "页面"));
        assert!(action_rows().iter().all(|row| row.detail == "动作"));
        let recents = recent_rows(&[RecentView {
            id: 7,
            title: "t".into(),
        }]);
        assert_eq!(recents[0].kind, KIND_RECENT);
        assert!(recents[0].detail.contains("最近浏览"));
    }
}
