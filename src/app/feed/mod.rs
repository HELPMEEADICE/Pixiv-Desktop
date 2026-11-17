use super::*;

mod avatars;
mod batch;
mod discovery;
pub(crate) mod fixture;
mod home;
mod reconcile;
mod search;
mod selection;

pub(crate) use self::{discovery::*, reconcile::start_reconcile, search::*};

impl App {
    // ------------------------------------------------------------- feed

    pub(crate) fn render_feed(&mut self, items: Vec<Illust>, append: bool) {
        // The count has to be taken *before* the take: read from `self.items` after
        // `mem::take` had emptied it, `already` was 0 by construction, so
        // `already > 0` was never true and every "load more" fell through to
        // `rebuild_columns` (plan §2.2 R2).
        let (already, mut all) = if append {
            take_existing(&mut self.items)
        } else {
            (0, Vec::new())
        };
        if items.is_empty() && all.is_empty() {
            let ui = self.ui();
            ui.set_grid_state(2);
            // The headline names what came back empty; the message says what to do
            // about it. One generic sentence for every page read as filler.
            let (headline, glyph, hint) = match self.page {
                Destination::Search => {
                    ("没有匹配的作品", "search", "换个关键词，或用标签再试一次。")
                }
                Destination::Bookmarks => (
                    "收藏夹是空的",
                    "star",
                    "在作品详情页点击「收藏」后，会出现在这里。",
                ),
                Destination::History => (
                    "还没有浏览记录",
                    "history",
                    "本机看过的作品会记录在这里，仅保存在本地。",
                ),
                _ => ("这里还没有内容", "", "试试刷新，或换一个标签。"),
            };
            ui.set_grid_state_title(headline.into());
            ui.set_grid_state_glyph(glyph.into());
            ui.set_grid_message(hint.into());
            ui.set_grid_action_label("".into());
            ui.set_grid_count_label("".into());
            ui.set_grid_columns(ModelRc::default());
            self.columns.clear();
            self.index.clear();
            self.reached_end = true;
            return;
        }
        // A page can hand back a row this feed already shows: discovery
        // re-recommends, and a load-more retried after a failure asks for the page
        // that failed. `index` holds one cell per id, so a second copy would claim
        // the first card's cell - leaving that card on its placeholder for good -
        // draw a duplicate card, and count the work twice (T3). Same rule the
        // discovery and profile pages already merge by.
        if append {
            append_feed_page(&mut all, items);
        } else {
            all.extend(items);
        }
        self.items = all;
        // Both paths prune. Pruning only inside `if !append` meant an endlessly
        // appended feed kept one decoded texture per work ever shown, outside the
        // 192 MiB cache budget, because `prune_covers` had exactly this one call
        // site and a feed that is extended is never replaced (plan §2.3 R13).
        // `keep` is counted from the top of the feed, so what a column rebuild can
        // paint without waiting is exactly what survives.
        // One id buffer for the whole render (B11): refilled in place, pruned from
        // here, and planned from by the column builder that runs below. A render used
        // to materialise every id twice and drop both - a 512-entry set of owned ids
        // and a 500-element Vec of (id, url) pairs, about 1 500 strings.
        {
            let App {
                items,
                covers,
                cover_ids,
                ..
            } = self;
            let mut ids = cover_ids.borrow_mut();
            refill_ids(&mut ids, items.as_slice());
            prune_covers(covers, ids.as_slice(), COVERS_KEEP);
        }
        // A new page extends the columns it finds. Rebuilding them re-ran the
        // balancer over the whole feed, so every card above silently moved to a
        // different column - the opposite of what `layout::distribute` is documented
        // to preserve, and visible as a jump while scrolling.
        match plan_feed_update(
            append,
            already,
            self.columns.len(),
            self.geometry.columns,
            self.column_heights.len(),
        ) {
            FeedUpdate::Extend { from } => self.append_columns(from),
            FeedUpdate::Rebuild => self.rebuild_columns(),
        }
        // The capture fixtures are start-up state and are applied here, once the
        // grid actually holds rows: a selection needs cards to select, and the
        // suggestion panel would be closed again by the navigation that reaches
        // this page.
        self.apply_fixtures();
    }
}

/// Move the page that is already on screen out of `items`, remembering how many
/// rows it held.
///
/// Both halves are one operation on purpose. `render_feed` used to take the page
/// first and read `self.items.len()` afterwards, which measured the emptied
/// vector (plan §2.2 R2); there is no longer an ordering to get wrong.
fn take_existing<T>(items: &mut Vec<T>) -> (usize, Vec<T>) {
    let already = items.len();
    (already, std::mem::take(items))
}

/// What `render_feed` does with the page it was handed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FeedUpdate {
    /// Rebuild every column model from `self.items`.
    Rebuild,
    /// Extend the models that are already there, starting at item `from`.
    Extend { from: usize },
}

/// Extend the columns, or rebuild them?
///
/// `existing` is the item count *before* the new page is merged in, and
/// `heights`/`columns` must describe the models currently set on the grid: a
/// column count that changed under the feed invalidates both the buckets and the
/// accumulated heights, so that case rebuilds.
pub(crate) fn plan_feed_update(
    append: bool,
    existing: usize,
    columns: usize,
    geometry_columns: usize,
    heights: usize,
) -> FeedUpdate {
    if append && existing > 0 && columns == geometry_columns && heights == columns {
        FeedUpdate::Extend { from: existing }
    } else {
        FeedUpdate::Rebuild
    }
}

/// Merge one fetched page into the list a page keeps, and return only the rows
/// that were new.
///
/// Two paged feeds keep their own copy of what they have shown - the home grid
/// (`home.works`) and a user's page (`profile.works`) - and both used to hand the
/// whole accumulated list to `render_feed(.., false)`: an extra full clone, a
/// fresh `distribute`, and `already = 0`, so `plan_feed_update` chose Rebuild
/// every time even after round seven made Extend reachable (plan §2.3 R10/R11).
/// The caller passes what this returns with `append = true`, which is one `Vec`
/// of the new rows and no rebuild.
pub(crate) fn merge_page(all: &mut Vec<Illust>, page: Vec<Illust>) -> Vec<Illust> {
    let before = all.len();
    pixiv_types::domain::append_unique(all, page);
    all[before..].to_vec()
}

/// Merge one page into the feed that is already showing, dropping repeated ids.
///
/// `index` holds one cell per id, so the second copy of a row that a paged feed
/// handed back would claim the first card's cell: that card keeps its placeholder
/// for good, a duplicate card is drawn, and the count says one work more than the
/// feed holds (T3). Returns how many rows were actually added.
pub(crate) fn append_feed_page(all: &mut Vec<Illust>, items: Vec<Illust>) -> usize {
    pixiv_types::domain::append_unique(all, items)
}

/// Push one card into the shortest column, returning the row it landed on.
///
/// The models are extended in place - never replaced - which is the whole point
/// of the incremental path: a model that is swapped for a new one makes Slint
/// re-create every card above it.
pub(crate) fn place_in_columns(
    columns: &[Rc<VecModel<IllustItem>>],
    heights: &mut [f32],
    item: IllustItem,
    height: f32,
) -> Option<(usize, usize)> {
    let target = shortest_column(heights);
    let model = columns.get(target)?;
    let row = model.row_count();
    model.push(item);
    heights[target] += height;
    Some((target, row))
}

impl App {
    /// Recompute the column buckets from `self.items` for the current geometry.
    ///
    /// This is the expensive path (it rebuilds every row model), so it is only
    /// called when the *column count* changes or when the feed itself changes.
    /// A pure width change keeps the buckets and only updates `card-width`, which
    /// is one property assignment — that is what stops a window drag from
    /// rebuilding the grid on every frame.
    pub(crate) fn rebuild_columns(&mut self) {
        let geometry = self.geometry;
        let heights: Vec<f32> = self
            .items
            .iter()
            .map(|illust| {
                layout::card_height(
                    geometry.card_width,
                    layout::clamped_aspect(illust.width, illust.height),
                )
            })
            .collect();
        let assignment = layout::distribute(&heights, geometry.columns);
        let buckets = layout::bucket(&assignment, geometry.columns);
        // Sum what each column actually received, so an appended page can be placed
        // without re-reading the models or recomputing the feed.
        let heights_by_column: Vec<f32> = buckets
            .iter()
            .map(|rows| rows.iter().map(|index| heights[*index]).sum())
            .collect();

        // One target for the whole pass: the tier the settled geometry asks for.
        // Every card drawn below it is either requested at it or, if the cache
        // already has pixels for the URL, handed over and upgraded once.
        let target = self.cover_target();
        // One pass over the feed, not one per card (T5). On the ranking page this
        // is the only place a card's number comes from, so the table is built here
        // and read by every row below.
        let ranks = rank_table(self.page, &self.items);
        let mut adopted: Vec<(String, slint::Image, bool)> = Vec::new();
        let mut upgrades: Vec<(String, String)> = Vec::new();
        let mut models = Vec::with_capacity(buckets.len());
        let mut index = HashMap::new();
        for (column, rows) in buckets.iter().enumerate() {
            let rows: Vec<IllustItem> = rows
                .iter()
                .enumerate()
                .map(|(row, item_index)| {
                    let illust = &self.items[*item_index];
                    let mut item = self.card_item(illust, rank_in(&ranks, illust));
                    self.mark_selected(&mut item);
                    let key = illust.id.to_string();
                    if let Some(image) = self.covers.get(&key) {
                        item.cover = image.clone();
                        item.state = 0;
                    } else if let Some(pick) = self.cache.pick(illust.urls.grid(), target) {
                        // The cache knows something the app-side map does not: it
                        // may already hold these pixels (a pruned entry, or a
                        // re-plan that dropped a map entry on purpose). Drawing
                        // them now beats a placeholder while the decode runs.
                        let image = cached_texture(&pick.image);
                        item.cover = image.clone();
                        item.state = 0;
                        adopted.push((key.clone(), image, !pick.needs_upgrade));
                        if pick.needs_upgrade {
                            upgrades.push((key.clone(), illust.urls.grid().to_string()));
                        }
                    }
                    index.insert(key, (column, row));
                    item
                })
                .collect();
            models.push(Rc::new(VecModel::from(rows)));
        }

        let outer: Vec<ModelRc<IllustItem>> =
            models.iter().map(|m| ModelRc::from(m.clone())).collect();

        let ui = self.ui();
        ui.set_grid_columns(ModelRc::from(Rc::new(VecModel::from(outer))));
        ui.set_grid_card_width(geometry.card_width);
        ui.set_grid_state(0);
        ui.set_grid_action_label("".into());
        ui.set_grid_count_label(format!("{} 件作品", self.items.len()).into());
        ui.set_grid_message("".into());
        ui.set_status_text(self.status_line().into());

        self.columns = models;
        self.column_heights = heights_by_column;
        self.index = index;
        // Requested before the adoption below, because `plan` skips whatever the
        // app-side map already holds. A cover the cache handed back below this
        // tier is thereby drawn **and** re-decoded once at the full target - that
        // is the half of the defect that let a stale small entry serve the card
        // for the life of the process.
        self.request_covers(upgrades, true);
        for (id, image, at_target) in adopted {
            if at_target {
                self.loader.adopt(&id, target);
            }
            self.covers.insert(id, image);
        }
        self.spawn_covers();
        self.backfill_author_avatars();
    }
}

impl App {
    /// Place the items that arrived after row `from` into the existing columns.
    ///
    /// Uses the same greedy rule as [`layout::distribute`] - shortest column, ties
    /// to the left - applied one item at a time against the accumulated heights.
    /// Nothing above the new page is touched, so the feed does not reshuffle while
    /// the user is reading it.
    pub(crate) fn append_columns(&mut self, from: usize) {
        let geometry = self.geometry;
        // Named apart from the `target` this loop already uses for the column the
        // next card goes in.
        let cover_target = self.cover_target();
        // The same one-pass table a rebuild uses (T5).
        let ranks = rank_table(self.page, &self.items);
        let mut adopted: Vec<(String, slint::Image, bool)> = Vec::new();
        let mut upgrades: Vec<(String, String)> = Vec::new();
        for index in from..self.items.len() {
            let illust = &self.items[index];
            let height = layout::card_height(
                geometry.card_width,
                layout::clamped_aspect(illust.width, illust.height),
            );
            let mut item = self.card_item(illust, rank_in(&ranks, illust));
            self.mark_selected(&mut item);
            let key = illust.id.to_string();
            if let Some(image) = self.covers.get(&key) {
                item.cover = image.clone();
                item.state = 0;
            } else if let Some(pick) = self.cache.pick(illust.urls.grid(), cover_target) {
                // Same rule as `rebuild_columns`: the cache's pixels are drawn now
                // and re-decoded once if they are below this tier.
                let image = cached_texture(&pick.image);
                item.cover = image.clone();
                item.state = 0;
                adopted.push((key.clone(), image, !pick.needs_upgrade));
                if pick.needs_upgrade {
                    upgrades.push((key.clone(), illust.urls.grid().to_string()));
                }
            }
            let Some((target, row)) =
                place_in_columns(&self.columns, &mut self.column_heights, item, height)
            else {
                continue;
            };
            self.index.insert(key, (target, row));
        }

        let ui = self.ui();
        ui.set_grid_card_width(geometry.card_width);
        ui.set_grid_count_label(format!("{} 件作品", self.items.len()).into());
        ui.set_status_text(self.status_line().into());
        self.request_covers(upgrades, true);
        for (id, image, at_target) in adopted {
            if at_target {
                self.loader.adopt(&id, cover_target);
            }
            self.covers.insert(id, image);
        }
        self.spawn_covers();
        self.backfill_author_avatars();
    }
}

/// The rank every work has on the ranking page, by id, built once per render.
///
/// `rank_of` used to scan `items` for every card, so a 500-row page cost about
/// 125 000 comparisons per render (T5). The numbers are identical: position in
/// load order, one-based, and nothing at all on a page that does not rank.
pub(crate) fn rank_table(page: Destination, items: &[Illust]) -> HashMap<u64, i32> {
    let mut ranks: HashMap<u64, i32> = HashMap::new();
    if page != Destination::Ranking {
        return ranks;
    }
    for (index, illust) in items.iter().enumerate() {
        // First occurrence wins, the way `position` did.
        ranks.entry(illust.id.get()).or_insert(index as i32 + 1);
    }
    ranks
}

/// One card's number, out of the table built for this render.
pub(crate) fn rank_in(ranks: &HashMap<u64, i32>, illust: &Illust) -> i32 {
    ranks.get(&illust.id.get()).copied().unwrap_or(0)
}

/// The page count a card's badge draws.
///
/// `is_multi_page` is the domain's predicate, and it is asked here rather than
/// re-derived: the badge's rule and the payload's rule are the same sentence, and a
/// card that compared `page_count > 1` itself was the second copy of it.
pub(crate) fn card_page_count(illust: &Illust) -> i32 {
    if illust.is_multi_page() {
        i32::try_from(illust.page_count).unwrap_or(i32::MAX)
    } else {
        1
    }
}

/// The date a card is labelled with, or the empty string.
///
/// Only the ranking board labels a card with a date: its rows carry a per-entry
/// stamp (`create_date` from `ranking.php`'s own `date`), while the work endpoints
/// carry an upload timestamp the caption band has no room for and the grid never
/// showed. An empty value is the answer that draws nothing — no slot, no separator.
pub(crate) fn card_date(page: Destination, illust: &Illust) -> String {
    if page != Destination::Ranking {
        return String::new();
    }
    let raw = illust.create_date.as_deref().unwrap_or_default().trim();
    let date = pixiv_types::domain::iso_date(raw);
    // A board stamps its rows with a date *and* the slot it was taken in
    // ("2024-03-12 12:33:00", "2026年11月09日 00:00"). Every row of one board shares
    // the time, and the caption band is 44 px: the card keeps the date.
    match date.split_once(' ') {
        Some((head, tail)) if !head.is_empty() && tail.contains(':') => head.to_string(),
        _ => date,
    }
}

impl App {
    /// One card, with the two values that belong to the page rather than to the row.
    pub(crate) fn card_item(&self, illust: &Illust, rank: i32) -> IllustItem {
        let mut item = illust_to_item(illust, rank);
        item.page_count = card_page_count(illust);
        item.date = card_date(self.page, illust).into();
        item
    }
}

impl App {
    /// Start cover loads for every item of the current feed.
    pub(crate) fn spawn_covers(&mut self) {
        let target = self.cover_target();
        let plan = {
            let App {
                items,
                covers,
                loader,
                cover_ids,
                ..
            } = self;
            // The buffer the prune filled, refilled only by what changed (B11): the
            // ids are borrowed, and the loader copies one only for a request it starts.
            let mut ids = cover_ids.borrow_mut();
            refill_ids(&mut ids, items.as_slice());
            loader.plan_borrowed(
                ids.iter()
                    .zip(items.iter())
                    .map(|(id, illust)| (id.as_str(), illust.urls.grid())),
                target,
                |id| covers.contains_key(id),
            )
        };
        self.spawn_cover_requests(plan);
    }

    /// Decode these covers at the tier the current geometry asks for.
    ///
    /// The target is stamped onto every request by `plan` and travels with it, so
    /// the decode that lands is recorded at the tier it was **planned** for rather
    /// than at whatever the geometry says by then - those differ on every resize
    /// that crosses a tier, and the record is what a later settle compares against.
    ///
    /// `refresh` drops the memory tier for each URL before requesting it, and every
    /// *upgrade* needs that: `ImageCache::load_sized` counts an entry no wider than
    /// the request as a hit, so a 320px entry would be handed straight back for a
    /// 352px request and the cover would stay soft for the life of the entry - the
    /// exact silence §12i describes. The disk tier keeps the encoded bytes and is
    /// keyed by URL alone, so a refresh costs a decode, never a download.
    pub(crate) fn request_covers(&mut self, urls: Vec<(String, String)>, refresh: bool) {
        if refresh {
            for (_, url) in &urls {
                self.cache.forget(url);
            }
        }
        let target = self.cover_target();
        let plan = {
            let App { covers, loader, .. } = self;
            // The owned form: this caller's pairs were built anyway (an upgrade
            // list, a re-plan's stale ids), and it is the one place that owns them.
            loader.plan(urls, target, |id| covers.contains_key(id))
        };
        self.spawn_cover_requests(plan);
    }

    /// Start the decodes a plan asked for.
    ///
    /// Split out of [`App::request_covers`] so the render path, which plans from a
    /// borrowed buffer (B11), shares one spawn half with every other caller.
    fn spawn_cover_requests(&mut self, plan: Vec<crate::images::CoverRequest>) {
        if plan.is_empty() {
            return;
        }

        let epoch = self.epoch;
        let demo = self.mode == Mode::Demo;
        let cache = self.cache.clone();
        let rt = self.rt.clone();

        rt.spawn(async move {
            let cache_for = cache.clone();
            let mut stream = futures::stream::iter(plan)
                .map(move |request| {
                    let cache = cache_for.clone();
                    async move {
                        let target = request.target;
                        let raw = if demo {
                            demo_raw(
                                Id(request.id.parse::<u64>().unwrap_or(0)),
                                target,
                                target,
                                0,
                            )
                            .await
                        } else {
                            fetch_sized(&cache, &request.url, target).await
                        };
                        (request.id, raw, target)
                    }
                })
                .buffer_unordered(COVER_CONCURRENCY);

            while let Some((id, raw, target)) = stream.next().await {
                let ok = raw.is_some();
                on_ui(move |shared| {
                    let Ok(mut app) = shared.try_borrow_mut() else {
                        return;
                    };
                    app.loader.complete(&id, target, ok);
                    // A result for a feed the user already left must not touch
                    // the current grid.
                    if app.epoch != epoch {
                        return;
                    }
                    match raw {
                        Some(raw) => app.apply_cover(&id, &raw, target),
                        None => app.mark_cover_failed(&id),
                    }
                });
            }
        });
    }
}

impl App {
    pub(crate) fn apply_cover(&mut self, id: &str, raw: &RawImage, target: u32) {
        // Two decodes of one cover can be in the air at different tiers, because a
        // re-plan frees the slot a smaller request is still holding. If the older,
        // smaller one finishes last it must not repaint a card that already holds
        // the wider bitmap.
        if self.loader.is_superseded(id, target) {
            return;
        }
        let image = raw.to_slint();
        self.covers.insert(id.to_string(), image.clone());
        let Some((column, row)) = self.index.get(id).copied() else {
            return;
        };
        let Some(model) = self.columns.get(column) else {
            return;
        };
        if let Some(mut item) = model.row_data(row) {
            item.cover = image;
            item.state = 0;
            model.set_row_data(row, item);
        }
    }
}

impl App {
    pub(crate) fn mark_cover_failed(&mut self, id: &str) {
        let Some((column, row)) = self.index.get(id).copied() else {
            return;
        };
        let Some(model) = self.columns.get(column) else {
            return;
        };
        if let Some(mut item) = model.row_data(row) {
            item.state = 2;
            model.set_row_data(row, item);
        }
    }
}

impl App {
    /// Slint reported a new window width. This is the only place the shell's
    /// narrow/compact decision is made, and it is event-driven: the previous
    /// implementation polled `Window::size()` every 250ms *and* reported the width
    /// from a Slint callback, so the two could disagree for a quarter of a second
    /// on every resize.
    pub(crate) fn on_window_width(&mut self, width: f32) {
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        const NARROW_BREAKPOINT: f32 = 1000.0;

        let narrow = width < NARROW_BREAKPOINT;
        // Only the narrow/compact decision is made here. This function also used to
        // *report* a content box, computed from a rail width it read out of the theme:
        // a second source of truth for a number Slint can derive from its own layout,
        // and the reason collapsing the rail left the detail page 180px too narrow
        // until the window was resized. `ui/main.slint` now states
        // `root.width - rail-pane.width` where that number is needed.
        self.ui().set_narrow(narrow);
    }
}

impl App {
    /// Slint reported the content width it actually resolved. Recompute geometry.
    pub(crate) fn on_measured(&mut self, width: f32) {
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        if (width - self.measured_width).abs() < 0.5 {
            return;
        }
        self.measured_width = width;
        // The profile header is the one page that changes *shape* rather than just
        // density, so it needs the same measured number the grid is built from.
        self.ui().set_profile_wide(width >= PROFILE_WIDE_MIN);
        let geometry = GridGeometry::for_width(width);
        let columns_changed = geometry.columns != self.geometry.columns;
        let width_changed = (geometry.card_width - self.geometry.card_width).abs() >= 0.5;
        self.geometry = geometry;
        if !width_changed && !columns_changed {
            return;
        }
        if columns_changed && !self.items.is_empty() {
            self.rebuild_columns();
        } else {
            self.ui().set_grid_card_width(geometry.card_width);
        }
        // A resize reports a new body-width per frame. Re-planning here would start
        // a decode per frame of a drag; the timer is *restarted* by every
        // measurement, so the re-plan runs once, after the width stopped moving.
        // `replan_covers` then decides whether that settle crossed a tier.
        arm_cover_replan();
    }
}

/// How long the measured width has to be still before covers are re-planned.
const COVER_SETTLE_MS: u64 = 180;

thread_local! {
    /// Restarted by every measurement and never read: the restart *is* the
    /// debounce and Slint owns the clock. It lives here rather than on `App`
    /// because it owns nothing any other module may read, and because a `Timer`
    /// is a handle onto a native timer that only the UI thread can own.
    static SETTLE: slint::Timer = slint::Timer::default();
}

/// Arm the settle that re-plans covers, restarting any window already running.
fn arm_cover_replan() {
    SETTLE.with(|timer| {
        timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(COVER_SETTLE_MS),
            || {
                on_ui(|shared| {
                    if let Ok(mut app) = shared.try_borrow_mut() {
                        app.replan_covers();
                    }
                });
            },
        );
    });
}

impl App {
    /// The decode tier the *settled* geometry asks for, in physical pixels.
    ///
    /// Never the seed: `geometry` has been overwritten by `on_measured` as soon as
    /// Slint reported `body-width`, and this only ever reads it afterwards.
    pub(crate) fn cover_target(&self) -> u32 {
        crate::images::target_width(self.geometry.card_width, self.ui().window().scale_factor())
    }

    /// Re-plan every cover the settled width has outgrown.
    ///
    /// Called once per settle by [`arm_cover_replan`], never once per measurement.
    /// Drops the app-side entries whose decode tier is below the tier the current
    /// geometry asks for, frees any in-flight slot planned below it (which would
    /// otherwise suppress the new request) and requests those covers again at the
    /// new target. `spawn_covers` walks the feed in order, so the first screen is
    /// still the first to arrive. A settle that did not cross a tier returns
    /// without touching anything.
    pub(crate) fn replan_covers(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let target = self.cover_target();
        let replan = self
            .loader
            .replan(self.covers.keys().map(String::as_str), target);
        if replan.is_empty() {
            return;
        }
        let stale: HashSet<&str> = replan.stale.iter().map(String::as_str).collect();
        let mut urls = Vec::with_capacity(stale.len());
        for illust in &self.items {
            let id = illust.id.to_string();
            if stale.contains(id.as_str()) {
                self.covers.remove(&id);
                urls.push((id, illust.urls.grid().to_string()));
            }
        }
        // In feed order, and refreshed: the stale pixels are still in the memory
        // tier and would be handed back for the bigger request otherwise.
        self.request_covers(urls, true);
        // The restarted slots have no drawn cover, so they are not in `stale`; the
        // normal plan picks them up now that their bookkeeping is gone.
        self.spawn_covers();
    }
}

/// Copy cache pixels into a Slint texture.
///
/// The same copy `RawImage::to_slint` performs, for the same reason: the buffer
/// length is the only honest statement of how many pixels exist, so a declared
/// height the data cannot fill is shortened rather than trusted. It cannot call
/// that function - `RawImage`'s fields are private to `helpers.rs` - and this path
/// has to be synchronous, because it runs while a column is being built.
fn cached_texture(image: &pixiv_image::DecodedImage) -> slint::Image {
    // One implementation for both draw paths: a cache-served cover and a freshly
    // decoded one must not disagree about what a short buffer means (UI-NOTES
    // section 12f). Keep this a delegation; the clamp lives in helpers.
    crate::app::helpers::rgba_to_slint(image.width, image.height, &image.rgba)
}

/// The four independent bands of the home page.
pub(crate) struct HomeBands {
    pub(crate) works: Vec<Illust>,
    pub(crate) latest: Vec<Illust>,
    pub(crate) latest_title: String,
    pub(crate) tags: Vec<RecommendTag>,
    pub(crate) artists: Vec<RecommendUser>,
    pub(crate) works_state: i32,
    pub(crate) latest_state: i32,
    pub(crate) tags_state: i32,
    pub(crate) artists_state: i32,
    pub(crate) works_message: String,
    pub(crate) latest_message: String,
    pub(crate) tags_message: String,
    pub(crate) artists_message: String,
    /// Extra "load more" batches appended to the for-you grid.
    pub(crate) batches: u32,
    pub(crate) empty_batches: u32,
    pub(crate) exhausted: bool,
}

impl HomeBands {
    pub(crate) fn loading() -> Self {
        Self {
            works: Vec::new(),
            latest: Vec::new(),
            latest_title: String::new(),
            tags: Vec::new(),
            artists: Vec::new(),
            works_state: BAND_LOADING,
            latest_state: BAND_LOADING,
            tags_state: BAND_LOADING,
            artists_state: BAND_LOADING,
            works_message: String::new(),
            latest_message: String::new(),
            tags_message: String::new(),
            artists_message: String::new(),
            batches: 0,
            empty_batches: 0,
            exhausted: false,
        }
    }
}

impl App {
    /// One card, with whatever avatar is already decoded.
    pub(crate) fn illust_item_avatar(&self, illust: &Illust, rank: i32) -> IllustItem {
        let mut item = illust_to_item(illust, rank);
        if let Some(url) = illust.author.avatar.as_ref()
            && let Some(image) = self.avatars.get(url)
        {
            item.author_avatar = image.clone();
            item.author_avatar_state = BAND_OK;
        }
        item
    }
}

impl App {
    /// Every image one recommended-user card needs: their avatar and the cover
    /// of the work used as a preview. Without this the artist row is drawn from
    /// placeholders, which is exactly what "no avatar" looked like.
    pub(crate) fn user_image_urls(users: &[RecommendUser]) -> Vec<String> {
        let mut urls = Vec::new();
        for user in users {
            urls.extend(user.avatar.clone());
            if let Some(preview) = user.previews.first() {
                let cover = preview.urls.grid();
                if !cover.is_empty() {
                    urls.push(cover.to_string());
                }
            }
        }
        urls
    }
}

impl App {
    pub(crate) fn user_item(&self, user: &RecommendUser) -> RecommendUserItem {
        let avatar_url = user.avatar.clone().unwrap_or_default();
        let preview = user
            .previews
            .first()
            .and_then(|illust| self.avatars.get(illust.urls.grid()))
            .cloned()
            .unwrap_or_default();
        RecommendUserItem {
            id: user.id.to_string().into(),
            name: user.name.clone().into(),
            account: user.account.clone().into(),
            comment: user.comment.clone().into(),
            avatar: self.avatars.get(&avatar_url).cloned().unwrap_or_default(),
            avatar_url: avatar_url.clone().into(),
            preview,
            followed: user.is_followed,
            premium: user.is_premium,
            avatar_state: if avatar_url.is_empty() {
                BAND_EMPTY
            } else if self.avatars.contains_key(&avatar_url) {
                BAND_OK
            } else {
                BAND_LOADING
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str) -> IllustItem {
        IllustItem {
            id: id.into(),
            title: "".into(),
            author: "".into(),
            author_id: "".into(),
            cover: slint::Image::default(),
            kind: 0,
            page_count: 1,
            bookmarked: false,
            rank: 0,
            width: 0,
            height: 0,
            state: 0,
            author_avatar: slint::Image::default(),
            author_avatar_url: "".into(),
            author_avatar_state: 0,
            ai: false,
            date: "".into(),
            selected: false,
            selectable: false,
        }
    }

    fn columns(count: usize) -> Vec<Rc<VecModel<IllustItem>>> {
        (0..count)
            .map(|_| Rc::new(VecModel::from(Vec::<IllustItem>::new())))
            .collect()
    }

    /// "Load more" must extend the column models that are already on screen.
    ///
    /// This could not hold before the fix, and not because of a layout rule:
    /// `render_feed` took `self.items` (plan §2.2 R2, `feed/mod.rs:14`) and then
    /// read `self.items.len()` (`:51`), so `already` measured the emptied vector,
    /// was 0 by construction, `already > 0` was never true and `append_columns`
    /// was unreachable. Every append therefore rebuilt every row model: a fresh
    /// `distribute`, five `SharedString` allocations per card and a full card
    /// re-creation in Slint, growing with what the user had already scrolled.
    #[test]
    fn a_second_page_extends_the_column_models_instead_of_rebuilding_them() {
        let mut heights = vec![0.0_f32; 4];
        let columns = columns(4);

        // First page: 24 cards, laid out by the same greedy rule the feed uses.
        for index in 0..24 {
            let height = 100.0 + (index % 5) as f32 * 40.0;
            place_in_columns(&columns, &mut heights, card(&format!("p{index}")), height);
        }
        let first_page: Vec<Rc<VecModel<IllustItem>>> = columns.clone();
        let rows_before: Vec<usize> = columns.iter().map(|model| model.row_count()).collect();
        assert_eq!(rows_before.iter().sum::<usize>(), 24);

        // Second page arrives while the first is on screen. The measurement and
        // the take are one operation, so the decision sees 24 and not 0.
        let mut on_screen: Vec<String> = (0..24).map(|index| format!("p{index}")).collect();
        let (already, taken) = take_existing(&mut on_screen);
        assert_eq!(already, 24);
        assert!(on_screen.is_empty() && taken.len() == 24);
        assert_eq!(
            plan_feed_update(true, already, columns.len(), 4, heights.len()),
            FeedUpdate::Extend { from: 24 }
        );
        // 0 is the value master measured, and it always chose the rebuild.
        assert_eq!(plan_feed_update(true, 0, 4, 4, 4), FeedUpdate::Rebuild);

        // The extend itself: the same models, more rows in each.
        for index in 24..48 {
            let height = 100.0 + (index % 5) as f32 * 40.0;
            place_in_columns(&columns, &mut heights, card(&format!("p{index}")), height);
        }
        for (index, before) in first_page.iter().enumerate() {
            assert!(
                Rc::ptr_eq(before, &columns[index]),
                "column {index} was rebuilt instead of extended"
            );
            assert!(
                columns[index].row_count() > rows_before[index],
                "column {index} did not grow"
            );
        }
        assert_eq!(
            columns.iter().map(|model| model.row_count()).sum::<usize>(),
            48
        );
    }

    /// B11 - one id buffer serves the prune and the plan, and a second render of the
    /// same feed allocates nothing.
    ///
    /// A render materialised every id twice - a 512-entry set of owned ids for the
    /// prune and a Vec of (id, url) pairs for the plan - and dropped both, about
    /// 1 500 strings per page of covers. The plant that makes this red is
    /// [refill_ids] rebuilding the buffer each time (500 new strings per render).
    #[test]
    fn b11_a_second_render_allocates_no_ids() {
        let works: Vec<Illust> = (1..=500).map(work).collect();
        let mut ids: Vec<String> = Vec::new();

        assert_eq!(
            refill_ids(&mut ids, &works),
            500,
            "the first render materialises each id once"
        );
        let pointers: Vec<*const u8> = ids.iter().map(|id| id.as_ptr()).collect();
        let capacity = ids.capacity();

        // The same feed again: the buffer and the strings in it are reused.
        assert_eq!(
            refill_ids(&mut ids, &works),
            0,
            "a second render allocates nothing"
        );
        assert_eq!(
            ids.iter().map(|id| id.as_ptr()).collect::<Vec<_>>(),
            pointers,
            "the same 500 strings, not 500 new ones"
        );
        assert_eq!(ids.capacity(), capacity, "the buffer did not grow");

        // A shorter feed truncates it without freeing anything.
        assert_eq!(refill_ids(&mut ids, &works[..10]), 0);
        assert_eq!(ids.len(), 10);
        assert_eq!(ids.capacity(), capacity);

        // A changed id is rewritten in place, so that row costs no allocation either.
        let mut changed = works.clone();
        changed[3].id = Id(99_999);
        assert_eq!(refill_ids(&mut ids, &changed[..10]), 0);
        assert_eq!(ids[3], "99999");

        // The prune this buffer feeds keeps borrowed keys: the plan it also feeds
        // starts nothing for a feed whose covers are all held.
        let mut covers: HashMap<String, slint::Image> = HashMap::new();
        for id in &ids {
            covers.insert(id.clone(), slint::Image::default());
        }
        let before = covers.len();
        assert_eq!(
            prune_covers(&mut covers, ids.as_slice(), COVERS_KEEP),
            0,
            "every cover the feed names stays"
        );
        assert_eq!(covers.len(), before);
    }

    /// Pruning the cover map is not a column operation: the models keep their
    /// identity, their rows and their order (plan §2.3 R13).
    #[test]
    fn pruning_covers_leaves_the_column_models_alone() {
        let columns = columns(2);
        let mut heights = vec![0.0_f32; 2];
        place_in_columns(&columns, &mut heights, card("a"), 100.0);
        place_in_columns(&columns, &mut heights, card("b"), 100.0);
        let published: Vec<Rc<VecModel<IllustItem>>> = columns.clone();

        let mut covers = HashMap::new();
        covers.insert("a".to_string(), slint::Image::default());
        covers.insert("pruned".to_string(), slint::Image::default());
        // The append path prunes too now; the model above must survive it.
        assert_eq!(
            prune_covers(&mut covers, &["a".to_string()], COVERS_KEEP),
            1
        );

        for (index, before) in published.iter().enumerate() {
            assert!(
                Rc::ptr_eq(before, &columns[index]),
                "column {index} was replaced by a maintenance pass"
            );
        }
        assert_eq!(columns[0].row_data(0).unwrap().id.to_string(), "a");
        assert_eq!(columns[1].row_data(0).unwrap().id.to_string(), "b");
    }
    fn work(id: u64) -> Illust {
        history_illust(&pixiv_store::HistoryEntry {
            illust_id: id.to_string(),
            title: format!("w{id}"),
            author: "a".into(),
            thumb_url: format!("https://i.pximg.net/{id}_p0_master1200.jpg"),
            viewed_at: 0,
        })
    }

    /// Both "load more" paths must reach the append branch.
    ///
    /// The defect was not in `plan_feed_update` but in its callers: the profile
    /// page handed `render_feed` the *whole* accumulated list with `append =
    /// false`, and the home grid did the same after an extra full clone. `already`
    /// was therefore 0 by construction and every batch rebuilt every column (plan
    /// §2.3 R10/R11). This walks the real chain - merge the fetched page, take what
    /// is on screen, plan the update - and asserts the shape the call sites now use.
    #[test]
    fn a_second_page_of_works_reaches_the_append_branch() {
        // What the first page left: 24 rows in `profile.works`, the same 24 in
        // `self.items` (render_feed set it), and 4 column models on screen.
        let mut kept: Vec<Illust> = (1..=24).map(work).collect();
        let mut on_screen: Vec<Illust> = (1..=24).map(work).collect();
        let heights = [100.0_f32; 4];
        let columns = columns(4);

        // The second page arrives: one row is a duplicate the feed already shows.
        let mut page: Vec<Illust> = (24..=47).map(work).collect();
        page.push(work(1));
        let batch = merge_page(&mut kept, page);

        // Only the new rows are handed over, never the accumulated list: the
        // second page repeats two ids the feed already shows.
        assert_eq!(batch.len(), 23, "the batch is what the page added");
        assert_eq!(kept.len(), 47);
        assert_eq!(batch.first().map(|illust| illust.id.get()), Some(25));

        // The call the sites make: render_feed(batch, true).
        let (already, taken) = take_existing(&mut on_screen);
        assert_eq!(already, 24, "the page that is already on screen is counted");
        assert!(taken.len() == 24 && on_screen.is_empty());
        assert_eq!(
            plan_feed_update(true, already, columns.len(), 4, heights.len()),
            FeedUpdate::Extend { from: 24 }
        );
        // What the two callers used to do: the whole list, with append = false.
        // `already` is 0 on that branch by construction, so the decision was
        // Rebuild no matter what the page already showed.
        assert_eq!(
            plan_feed_update(false, 0, columns.len(), 4, heights.len()),
            FeedUpdate::Rebuild
        );
    }

    /// Appending is the one path that can see the same work twice, and the feed
    /// must not grow a duplicate row for it.
    #[test]
    fn a_page_that_adds_nothing_hands_over_no_rows() {
        let mut kept: Vec<Illust> = (1..=3).map(work).collect();
        let batch = merge_page(&mut kept, vec![work(2), work(3)]);
        assert!(batch.is_empty());
        assert_eq!(kept.len(), 3);
    }

    /// A column count that changed under the feed invalidates the buckets and
    /// the accumulated heights, so that is the case that still rebuilds.
    #[test]
    fn a_changed_column_count_still_rebuilds() {
        assert_eq!(plan_feed_update(true, 24, 3, 4, 3), FeedUpdate::Rebuild);
        assert_eq!(plan_feed_update(true, 24, 4, 4, 3), FeedUpdate::Rebuild);
        assert_eq!(plan_feed_update(false, 24, 4, 4, 4), FeedUpdate::Rebuild);
        // Nothing on screen yet: an empty feed has no columns to extend.
        assert_eq!(plan_feed_update(true, 0, 0, 4, 0), FeedUpdate::Rebuild);
    }

    /// F3: the badge's count is the domain's predicate, not a second comparison.
    #[test]
    fn f3_a_badge_counts_only_a_multi_page_work() {
        let mut illust = work(1);
        illust.page_count = 1;
        assert_eq!(card_page_count(&illust), 1);
        illust.page_count = 7;
        assert_eq!(card_page_count(&illust), 7);
        // A payload that claims zero pages is one picture, which is what
        // `Illust::base` already clamps to; the badge must not print "共0张".
        illust.page_count = 0;
        assert_eq!(card_page_count(&illust), 1);
        assert!(!illust.is_multi_page(), "the predicate the badge asks");
    }

    /// F3: the ranking board labels its cards; no other grid does.
    #[test]
    fn f3_only_the_ranking_card_carries_its_date() {
        let mut illust = work(1);
        illust.create_date = Some("2026年11月09日 00:00".to_string());
        illust.create_date = Some("2026年11月09日 00:00".to_string());
        assert_eq!(
            card_date(Destination::Ranking, &illust),
            "2026年11月09日",
            "the board's own stamp is shown as Pixiv wrote it, without the slot time"
        );
        illust.create_date = Some("2024-03-12 12:33:00".to_string());
        assert_eq!(
            card_date(Destination::Ranking, &illust),
            "2024-03-12",
            "every row of one board shares the time, so the card keeps the date"
        );
        for page in [
            Destination::Home,
            Destination::Search,
            Destination::Bookmarks,
            Destination::History,
        ] {
            assert_eq!(
                card_date(page, &illust),
                "",
                "{page:?} draws no date slot at all"
            );
        }
        // The eight-digit form is the one shape the domain reformats.
        illust.create_date = Some("20261116".to_string());
        assert_eq!(card_date(Destination::Ranking, &illust), "2026-11-16");
    }

    /// F3: an empty date is the empty string, which is what draws no separator.
    #[test]
    fn f3_an_empty_date_draws_no_slot_and_no_separator() {
        let mut illust = work(1);
        assert_eq!(card_date(Destination::Ranking, &illust), "");
        illust.create_date = Some("   ".to_string());
        assert_eq!(
            card_date(Destination::Ranking, &illust),
            "",
            "whitespace is not a date"
        );
        assert!(
            !card_date(Destination::Ranking, &illust).starts_with('·'),
            "the separator belongs to the non-empty branch only"
        );
    }
}
